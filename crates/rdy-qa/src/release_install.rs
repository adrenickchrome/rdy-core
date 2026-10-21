//! Release archives: one native `.tar.gz` per platform, holding the `rdy`
//! binary, both license files and, for a preview build, the preview note as `PREVIEW.txt`. The
//! binary's own `--version` output decides whether it is a preview, so the archive's name and
//! note follow the build, with no second switch. Packaging stops at the unsigned archive: the
//! signed `SHA256SUMS` is made later, offline.
//!
//! `qa-release-package` is the command; [`package`] does the work.
use crate::platform::{self, RELEASE_CHILD_DEADLINE, RELEASE_CHILD_OUTPUT_MAX};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The preview note: `PREVIEW.txt` in a preview archive, and the start of the README section
/// and of each preview's release notes.
pub const PREVIEW_NOTE: &str = "This is a preview of rdy.
It comes before Release 1, the first release with a compatibility promise.

Until Release 1, any command, flag, tool, code or file format may change in the next preview, with no deprecation period.
One thing stays: every later rdy, Release 1 included, reads every dump that this rdy writes.

To move a project to a newer preview, run `rdy dump --out <file>` with this rdy.
Then run `rdy load <file> <dir>` with the new rdy, into a directory with no store.
A project made with a preview moves to Release 1 the same way.

Report problems at https://github.com/adrenickchrome/rdy-core/issues.
";

/// The two release targets: macOS arm64 and Linux amd64.
pub const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"];
/// The MIT license file, at the repository root and in each archive.
pub const MIT_FILE: &str = "LICENSE-MIT";
/// The Apache 2.0 license file, at the repository root and in each archive.
pub const APACHE_FILE: &str = "LICENSE-APACHE";
/// The preview note's file name in a preview archive.
pub const NOTE_FILE: &str = "PREVIEW.txt";
/// The largest binary packaged: 256 MiB.
pub const BINARY_MAX: u64 = 268_435_456;
/// The largest license file packaged: 64 KiB.
pub const LICENSE_MAX: u64 = 65_536;
/// The largest archive accepted: the binary cap plus 1 MiB.
pub const ARCHIVE_MAX: u64 = 269_484_032;
/// The longest path argument, in bytes.
pub const PATH_MAX: usize = 4096;

/// `qa-release-package`'s refusal texts (stderr, with an LF). Invalid arguments exit 2; the
/// others exit 1.
pub const MSG_INVALID_ARGUMENTS: &str = "release package: invalid arguments";
pub const MSG_INVALID_INPUT: &str = "release package: invalid input file";
pub const MSG_OUTPUT_EXISTS: &str = "release package: output exists";
pub const MSG_BINARY_VERSION: &str = "release package: invalid binary version";
pub const MSG_SIZE_LIMIT: &str = "release package: size limit exceeded";
pub const MSG_FILE_OPERATION: &str = "release package: file operation failed";

/// Why a release archive test skips when `tar` does not run.
pub const TAR_SKIP_REASON: &str = "tar is required for release archive tests";

/// Why packaging failed; each has one public text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    /// A version, target or path outside its grammar.
    Arguments,
    /// A missing, empty, symlinked or not regular input file.
    InputFile,
    /// OUT is already there.
    OutputExists,
    /// The binary's `--version` failed, wrote to stderr, or printed another text.
    BinaryVersion,
    /// A file over its cap, or child output over the runner's cap.
    SizeLimit,
    /// Any other I/O failure: a timeout, tar, or the cleanup.
    FileOperation,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            Failure::Arguments => MSG_INVALID_ARGUMENTS,
            Failure::InputFile => MSG_INVALID_INPUT,
            Failure::OutputExists => MSG_OUTPUT_EXISTS,
            Failure::BinaryVersion => MSG_BINARY_VERSION,
            Failure::SizeLimit => MSG_SIZE_LIMIT,
            Failure::FileOperation => MSG_FILE_OPERATION,
        }
    }
}

/// The command that writes the archive of the directory `stem` to its stdout. It starts in
/// OUT, and its stdout is the archive file, reserved through OUT's handle.
type TarCommand<'a> = &'a dyn Fn(&str) -> Command;

/// What packaging reads besides its arguments; tests swap parts of it.
struct Setup<'a> {
    /// The directory holding `LICENSE-MIT` and `LICENSE-APACHE`.
    licenses: PathBuf,
    archive_max: u64,
    tar: TarCommand<'a>,
    /// The version probe of the staged binary, given as `./rdy`: it starts in the stage.
    probe: &'a dyn Fn(&Path) -> Command,
    /// Runs once every input is checked, before OUT is made (tests change input paths here).
    checked: &'a dyn Fn(),
}

/// The real release's `checked`: nothing.
fn nothing() {}

impl Setup<'static> {
    /// The real release: the repository's license files, native tar, the binary's own probe.
    fn release() -> Self {
        Setup {
            licenses: repository_root(),
            archive_max: ARCHIVE_MAX,
            tar: &tar_command,
            probe: &version_command,
            checked: &nothing,
        }
    }
}

/// The repository root, from this crate's manifest directory (`crates/rdy-qa`).
fn repository_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .unwrap_or(manifest)
        .to_path_buf()
}

/// `tar -czf - <stem>`: the archive goes to stdout. AppleDouble files stay out of the archive.
fn tar_command(stem: &str) -> Command {
    let mut cmd = Command::new("tar");
    cmd.args(["-czf", "-", stem]).env("COPYFILE_DISABLE", "1");
    cmd
}

/// `<binary> --version`.
fn version_command(binary: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.arg("--version");
    cmd
}

/// Whether `version` is a preview version: `^0\.[1-9][0-9]{0,9}\.0$`.
pub fn valid_version(version: &str) -> bool {
    let Some(minor) = version
        .strip_prefix("0.")
        .and_then(|v| v.strip_suffix(".0"))
    else {
        return false;
    };
    (1..=10).contains(&minor.len())
        && minor.bytes().all(|b| b.is_ascii_digit())
        && !minor.starts_with('0')
}

/// Whether `target` is one of the two [`TARGETS`].
pub fn valid_target(target: &str) -> bool {
    TARGETS.contains(&target)
}

/// Whether `path` is an absolute UTF-8 path of at most [`PATH_MAX`] bytes with no NUL, CR or LF.
pub fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .to_str()
            .is_some_and(|p| p.len() <= PATH_MAX && !p.contains(['\0', '\r', '\n']))
}

/// Whether `tar --version` runs and succeeds.
pub fn tar_available() -> bool {
    let mut cmd = Command::new("tar");
    cmd.arg("--version");
    platform::run_bounded(cmd, None, RELEASE_CHILD_OUTPUT_MAX, RELEASE_CHILD_DEADLINE)
        .is_ok_and(|o| o.status.success())
}

/// Packages `binary` (a release build of rdy) for `target` into the new directory `out`, which
/// then holds only `<stem>.tar.gz`; returns that archive's path. The stem is
/// `rdy-<version>-preview-<target>` when the binary's `--version` prints `rdy <version>
/// (preview)`, and `rdy-<version>-<target>` when it prints `rdy <version>`. The archive holds
/// `<stem>/` with `rdy` (0755), `LICENSE-MIT` and `LICENSE-APACHE` (0644) from the repository
/// root and, for a preview, `PREVIEW.txt` (0644) with [`PREVIEW_NOTE`].
///
/// The error is one of the `MSG_` texts. Checks run in order: the grammar of the arguments, the
/// binary, `LICENSE-MIT`, `LICENSE-APACHE`, then that `out` does not exist. On failure every
/// path this call created is removed again, and nothing that was there before is touched.
///
/// `out` is made private to this user (mode 0700, whatever the umask), and the stage inside it
/// too, so no other user can change a staged file between its probe and the archive. Every
/// step after `out` is made goes through handles to `out` and the stage, never through the
/// path `out`: the probe and tar start in those directories, and tar writes the archive to a
/// file this run reserved through `out`'s handle. So if someone who can write `out`'s parent
/// moves `out` away, nothing is done in what then stands at that path; the run fails, as the
/// archive's path would no longer name its archive.
pub fn package(binary: &Path, version: &str, target: &str, out: &Path) -> Result<PathBuf, String> {
    package_with(binary, version, target, out, &Setup::release())
        .map_err(|f| f.message().to_string())
}

/// The private staging directory's name in OUT.
const STAGE_DIR: &str = ".rdy-stage";

fn package_with(
    binary: &Path,
    version: &str,
    target: &str,
    out: &Path,
    setup: &Setup,
) -> Result<PathBuf, Failure> {
    if !(valid_version(version) && valid_target(target) && valid_path(binary) && valid_path(out)) {
        return Err(Failure::Arguments);
    }
    let inputs = Inputs {
        binary: open_input(binary, BINARY_MAX)?,
        mit: open_input(&setup.licenses.join(MIT_FILE), LICENSE_MAX)?,
        apache: open_input(&setup.licenses.join(APACHE_FILE), LICENSE_MAX)?,
    };
    match fs::symlink_metadata(out) {
        Ok(_) => return Err(Failure::OutputExists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(Failure::FileOperation),
    }
    let (Some(parent), Some(out_name)) = (out.parent(), out.file_name()) else {
        return Err(Failure::FileOperation);
    };
    let parent = platform::open_dir(parent).map_err(|_| Failure::FileOperation)?;
    (setup.checked)();
    let out_dir = match platform::create_private_dir_at(&parent, out_name) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(Failure::OutputExists),
        Err(_) => return Err(Failure::FileOperation),
    };
    let mut work = Work {
        out,
        parent,
        out_name: out_name.to_os_string(),
        out_dir,
        stage: None,
        archive: None,
    };
    let made = work.build(&inputs, version, target, setup);
    work.finish(made)
}

/// The three inputs, open: the handles their checks read, which the copies read too.
struct Inputs {
    binary: File,
    mit: File,
    apache: File,
}

/// What one packaging run made in OUT, which only it may remove. Every step reaches OUT and
/// the stage through these handles.
struct Work<'a> {
    out: &'a Path,
    /// OUT's parent, open, and OUT's name in it.
    parent: File,
    out_name: std::ffi::OsString,
    /// OUT, open: the stage and the archive are made, checked and removed through it.
    out_dir: File,
    stage: Option<Stage>,
    /// The archive's name and the file this run reserved there.
    archive: Option<(String, File)>,
}

/// The private stage in OUT: its name there (first [`STAGE_DIR`], then the stem), its handle,
/// and the files this run made in it.
struct Stage {
    name: String,
    dir: File,
    files: Vec<&'static str>,
}

impl Stage {
    /// Makes the new file `name` in the stage with `mode`, holding what `write` writes.
    fn add(
        &mut self,
        name: &'static str,
        mode: u32,
        write: impl FnOnce(&mut File) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let mut file = platform::create_new_at(&self.dir, name.as_ref())
            .map_err(|_| Failure::FileOperation)?;
        self.files.push(name);
        write(&mut file)?;
        file.sync_all().map_err(|_| Failure::FileOperation)?;
        // the write handle closes when this returns, before any child runs the file
        platform::set_file_mode(&file, mode).map_err(|_| Failure::FileOperation)
    }
}

impl Work<'_> {
    /// Stages the release in a private directory in OUT and writes `<stem>.tar.gz` beside it;
    /// returns the archive's path.
    fn build(
        &mut self,
        inputs: &Inputs,
        version: &str,
        target: &str,
        setup: &Setup,
    ) -> Result<PathBuf, Failure> {
        let dir = platform::create_private_dir_at(&self.out_dir, STAGE_DIR.as_ref())
            .map_err(|_| Failure::FileOperation)?;
        let stage = self.stage.insert(Stage {
            name: STAGE_DIR.to_string(),
            dir,
            files: Vec::new(),
        });
        stage.add("rdy", 0o755, |to| copy_from(&inputs.binary, to, BINARY_MAX))?;
        stage.add(MIT_FILE, 0o644, |to| {
            copy_from(&inputs.mit, to, LICENSE_MAX)
        })?;
        stage.add(APACHE_FILE, 0o644, |to| {
            copy_from(&inputs.apache, to, LICENSE_MAX)
        })?;
        let preview = probe_version(&stage.dir, version, setup)?;
        let stem = if preview {
            format!("rdy-{version}-preview-{target}")
        } else {
            format!("rdy-{version}-{target}")
        };
        if preview {
            stage.add(NOTE_FILE, 0o644, |to| {
                use std::io::Write;
                to.write_all(PREVIEW_NOTE.as_bytes())
                    .map_err(|_| Failure::FileOperation)
            })?;
        }
        platform::set_file_mode(&stage.dir, 0o755).map_err(|_| Failure::FileOperation)?;
        platform::rename_at(&self.out_dir, stage.name.as_ref(), stem.as_ref())
            .map_err(|_| Failure::FileOperation)?;
        stage.name.clone_from(&stem);

        // Reserve the archive's name, then let tar write into that file through its handle:
        // a file that someone else put at the name is refused and never touched.
        let name = format!("{stem}.tar.gz");
        let reserved = platform::create_new_at(&self.out_dir, name.as_ref())
            .map_err(|_| Failure::FileOperation)?;
        let (name, reserved) = self.archive.insert((name, reserved));
        let mut tar = (setup.tar)(&stem);
        platform::start_in_dir(&mut tar, &self.out_dir).map_err(|_| Failure::FileOperation)?;
        let into = reserved.try_clone().map_err(|_| Failure::FileOperation)?;
        let tarred = platform::run_bounded_to(
            tar,
            None,
            into,
            RELEASE_CHILD_OUTPUT_MAX,
            RELEASE_CHILD_DEADLINE,
        );
        match tarred {
            Ok(o) if o.status.success() => {}
            Ok(_) => return Err(Failure::FileOperation),
            Err(e) => return Err(child_failure(&e)),
        }
        // the archive's name must still hold the file tar wrote
        if !platform::holds_file(&self.out_dir, name.as_ref(), reserved)
            .map_err(|_| Failure::FileOperation)?
        {
            return Err(Failure::FileOperation);
        }
        let len = reserved
            .metadata()
            .map_err(|_| Failure::FileOperation)?
            .len();
        if len == 0 {
            return Err(Failure::FileOperation);
        }
        if len > setup.archive_max {
            return Err(Failure::SizeLimit);
        }
        platform::set_file_mode(reserved, 0o644).map_err(|_| Failure::FileOperation)?;
        Ok(self.out.join(&*name))
    }

    /// Removes the stage on every path, through its handle. A success stands only while OUT's
    /// path still names this run's OUT. On failure it removes the reserved archive (only while
    /// its name still holds the reserved file) and OUT (only while OUT's path still names it).
    /// A failed removal is the result, whatever failed before it.
    fn finish(self, made: Result<PathBuf, Failure>) -> Result<PathBuf, Failure> {
        let mut cleaned = true;
        if let Some(stage) = &self.stage {
            for name in &stage.files {
                cleaned &= platform::remove_file_at(&stage.dir, name.as_ref()).is_ok();
            }
            cleaned &= platform::holds_file(&self.out_dir, stage.name.as_ref(), &stage.dir)
                .unwrap_or(false)
                && platform::remove_dir_at(&self.out_dir, stage.name.as_ref()).is_ok();
        }
        // someone who can write OUT's parent may have moved OUT away
        let in_place =
            platform::holds_file(&self.parent, &self.out_name, &self.out_dir).unwrap_or(false);
        let made = match made {
            Ok(archive) if cleaned && in_place => Ok(archive),
            Ok(_) => Err(Failure::FileOperation),
            failed => failed,
        };
        if made.is_err() {
            if let Some((name, reserved)) = &self.archive {
                cleaned &= match platform::holds_file(&self.out_dir, name.as_ref(), reserved) {
                    Ok(true) => platform::remove_file_at(&self.out_dir, name.as_ref()).is_ok(),
                    // gone, or another's: never removed
                    Ok(false) => true,
                    Err(_) => false,
                };
            }
            // OUT is empty again unless a removal failed or someone else put a file there; what
            // stands at OUT's path once OUT was moved away is never this run's to remove
            if in_place {
                cleaned &= platform::remove_dir_at(&self.parent, &self.out_name).is_ok();
            }
        }
        if cleaned {
            made
        } else {
            Err(Failure::FileOperation)
        }
    }
}

/// Opens an input file and checks it through the open handle: a regular file (not a symlink, a
/// FIFO or a directory) of 1..=`max` bytes.
fn open_input(path: &Path, max: u64) -> Result<File, Failure> {
    let file = platform::open_input(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound
        | io::ErrorKind::NotADirectory
        | io::ErrorKind::InvalidFilename
        | io::ErrorKind::InvalidInput => Failure::InputFile,
        _ => Failure::FileOperation,
    })?;
    let meta = file.metadata().map_err(|_| Failure::FileOperation)?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(Failure::InputFile);
    }
    if meta.len() > max {
        return Err(Failure::SizeLimit);
    }
    Ok(file)
}

/// Copies the checked input `from` into the open file `to`, reading at most `max + 1` bytes: a
/// file that grew past `max` since its check fails, and so does one that is now empty.
fn copy_from(from: &File, to: &mut File, max: u64) -> Result<(), Failure> {
    use std::io::Read;
    let copied =
        io::copy(&mut from.take(max.saturating_add(1)), to).map_err(|_| Failure::FileOperation)?;
    match copied {
        0 => Err(Failure::InputFile),
        n if n > max => Err(Failure::SizeLimit),
        _ => Ok(()),
    }
}

/// How often the probe is retried while the kernel still sees the staged copy as open for
/// writing (`ETXTBSY`: another thread's child may hold the descriptor until it execs).
const PROBE_BUSY_TRIES: u32 = 50;

/// Runs the staged binary's `--version` (the setup's probe), started in the open stage `stage`
/// as `./rdy`, and reads whether it is a preview.
fn probe_version(stage: &File, version: &str, setup: &Setup) -> Result<bool, Failure> {
    let mut tries = 1;
    loop {
        let mut probe = (setup.probe)(Path::new("./rdy"));
        platform::start_in_dir(&mut probe, stage).map_err(|_| Failure::FileOperation)?;
        let run = platform::run_bounded(
            probe,
            None,
            RELEASE_CHILD_OUTPUT_MAX,
            RELEASE_CHILD_DEADLINE,
        );
        match run {
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy && tries < PROBE_BUSY_TRIES => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            run => return classify_probe(run, version),
        }
    }
}

/// The probe's result: the runner's own failures first (overflow is the size limit, any other
/// the file-operation failure), then exit 0 with empty stderr, then the stdout text.
fn classify_probe(run: io::Result<Output>, version: &str) -> Result<bool, Failure> {
    let out = run.map_err(|e| child_failure(&e))?;
    if !out.status.success() || !out.stderr.is_empty() {
        return Err(Failure::BinaryVersion);
    }
    preview_from_version(&out.stdout, version).map_err(|_| Failure::BinaryVersion)
}

/// A runner failure's class: output overflow is the size limit, anything else (a deadline, a
/// start or cleanup failure) a file operation.
fn child_failure(e: &io::Error) -> Failure {
    if e.kind() == io::ErrorKind::InvalidData {
        Failure::SizeLimit
    } else {
        Failure::FileOperation
    }
}

/// Whether `rdy --version` output marks a preview of `version`: exactly `rdy <version>
/// (preview)` and LF (true) or `rdy <version>` and LF (false); anything else is an error.
fn preview_from_version(stdout: &[u8], version: &str) -> Result<bool, String> {
    if stdout == format!("rdy {version} (preview)\n").as_bytes() {
        Ok(true)
    } else if stdout == format!("rdy {version}\n").as_bytes() {
        Ok(false)
    } else {
        Err(MSG_BINARY_VERSION.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::test_support::{fifo, fifo_line};
    use crate::platform::{Expiry, mode_of, run_bounded, run_bounded_inner, shell_script};
    use crate::skip::{self, Why};

    const V: &str = "0.14.0";
    const TARGET: &str = "aarch64-apple-darwin";
    const BINARY: &[u8] = b"\x7fELF a small binary fixture\n";
    const MIT: &[u8] = b"the MIT license fixture\n";
    const APACHE: &[u8] = b"the Apache license fixture\n";

    /// A binary, a license root and an empty parent for OUT, plus a sentinel beside each.
    struct Fixture {
        dir: tempfile::TempDir,
        binary: PathBuf,
        licenses: PathBuf,
        parent: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let src = dir.path().join("src");
            let licenses = dir.path().join("licenses");
            let parent = dir.path().join("parent");
            for d in [&src, &licenses, &parent] {
                fs::create_dir(d).expect("mkdir");
                fs::write(d.join("keep"), b"sentinel\n").expect("sentinel");
            }
            let binary = src.join("rdy");
            fs::write(&binary, BINARY).expect("binary");
            fs::write(licenses.join(MIT_FILE), MIT).expect("mit");
            fs::write(licenses.join(APACHE_FILE), APACHE).expect("apache");
            Self {
                dir,
                binary,
                licenses,
                parent,
            }
        }

        fn out(&self) -> PathBuf {
            self.parent.join("out")
        }

        /// The sorted names in each fixture directory, with each sentinel's bytes.
        fn state(&self) -> Vec<(String, Vec<String>, Vec<u8>)> {
            ["src", "licenses", "parent"]
                .iter()
                .map(|d| {
                    let p = self.dir.path().join(d);
                    (
                        d.to_string(),
                        names(&p),
                        fs::read(p.join("keep")).unwrap_or_default(),
                    )
                })
                .collect()
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// A probe that prints `text` (a printf format) instead of running the staged copy.
    fn printing(text: &'static str) -> impl Fn(&Path) -> Command {
        move |_| shell_script(&format!("printf '{text}'"))
    }

    fn setup<'a>(f: &Fixture, probe: &'a dyn Fn(&Path) -> Command) -> Setup<'a> {
        Setup {
            licenses: f.licenses.clone(),
            probe,
            ..Setup::release()
        }
    }

    /// Whether tar runs here; a skip line when it does not.
    fn tar_or_skip(test: &str) -> bool {
        if tar_available() {
            return true;
        }
        skip::skip(test, Why::Prerequisite, TAR_SKIP_REASON);
        false
    }

    /// `tar` with `args`, which must succeed; its stdout.
    fn tar(args: &[&std::ffi::OsStr]) -> Vec<u8> {
        let mut cmd = Command::new("tar");
        cmd.args(args);
        let out =
            run_bounded(cmd, None, 1 << 20, std::time::Duration::from_secs(600)).expect("run tar");
        assert!(
            out.status.success(),
            "tar {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// The archive's member names, sorted.
    fn listing(archive: &Path) -> Vec<String> {
        let out = tar(&["-tzf".as_ref(), archive.as_os_str()]);
        let mut v: Vec<String> = String::from_utf8(out)
            .expect("utf-8 listing")
            .lines()
            .map(str::to_string)
            .collect();
        v.sort();
        v
    }

    /// Extracts `archive` into the new directory `into`, keeping modes.
    fn extract(archive: &Path, into: &Path) {
        fs::create_dir(into).expect("mkdir");
        tar(&[
            "-xpzf".as_ref(),
            archive.as_os_str(),
            "-C".as_ref(),
            into.as_os_str(),
        ]);
    }

    fn members(stem: &str, preview: bool) -> Vec<String> {
        let mut want = vec![
            format!("{stem}/"),
            format!("{stem}/{APACHE_FILE}"),
            format!("{stem}/{MIT_FILE}"),
            format!("{stem}/rdy"),
        ];
        if preview {
            want.push(format!("{stem}/PREVIEW.txt"));
        }
        want.sort();
        want
    }

    #[test]
    fn pre1_package_contains_binary_license_and_preview_note() {
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_contains_binary_license_and_preview_note"
        )) {
            return;
        }
        // The exact note, line by line, with its final LF.
        assert_eq!(
            PREVIEW_NOTE,
            concat!(
                "This is a preview of rdy.\n",
                "It comes before Release 1, the first release with a compatibility promise.\n",
                "\n",
                "Until Release 1, any command, flag, tool, code or file format may change in the ",
                "next preview, with no deprecation period.\n",
                "One thing stays: every later rdy, Release 1 included, reads every dump that this ",
                "rdy writes.\n",
                "\n",
                "To move a project to a newer preview, run `rdy dump --out <file>` with this rdy.\n",
                "Then run `rdy load <file> <dir>` with the new rdy, into a directory with no store.\n",
                "A project made with a preview moves to Release 1 the same way.\n",
                "\n",
                "Report problems at https://github.com/adrenickchrome/rdy-core/issues.\n",
            )
        );
        for (stdout, preview, stem) in [
            (
                "rdy 0.14.0 (preview)\\n",
                true,
                "rdy-0.14.0-preview-aarch64-apple-darwin",
            ),
            ("rdy 0.14.0\\n", false, "rdy-0.14.0-aarch64-apple-darwin"),
        ] {
            let f = Fixture::new();
            let before = f.state();
            let probe = printing(stdout);
            let archive = package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe))
                .unwrap_or_else(|e| panic!("package ({stdout}): {}", e.message()));
            assert_eq!(archive, f.out().join(format!("{stem}.tar.gz")));
            assert_eq!(
                names(&f.out()),
                vec![format!("{stem}.tar.gz")],
                "OUT holds only the archive"
            );
            assert_eq!(listing(&archive), members(stem, preview));
            let into = f.dir.path().join("x");
            extract(&archive, &into);
            let root = into.join(stem);
            assert_eq!(fs::read(root.join("rdy")).expect("rdy"), BINARY);
            assert_eq!(fs::read(root.join(MIT_FILE)).expect("mit"), MIT);
            assert_eq!(fs::read(root.join(APACHE_FILE)).expect("apache"), APACHE);
            assert_eq!(mode_of(&root).expect("mode"), 0o755);
            assert_eq!(mode_of(&root.join("rdy")).expect("mode"), 0o755);
            assert_eq!(mode_of(&root.join(MIT_FILE)).expect("mode"), 0o644);
            assert_eq!(mode_of(&root.join(APACHE_FILE)).expect("mode"), 0o644);
            let note = root.join("PREVIEW.txt");
            if preview {
                assert_eq!(fs::read_to_string(&note).expect("note"), PREVIEW_NOTE);
                assert_eq!(mode_of(&note).expect("mode"), 0o644);
            } else {
                assert!(!note.exists(), "a plain build has no preview note");
            }
            assert_eq!(names(&root).len(), if preview { 4 } else { 3 });
            let mut after = f.state();
            after[2].1.retain(|n| n != "out");
            assert_eq!(after, before, "the inputs are unchanged");
        }

        // The built CLI: its own `--version` output picks the archive family.
        let rdy = crate::fixtures::rdy_bin();
        let mut cmd = Command::new(&rdy);
        cmd.arg("--version");
        let out = run_bounded(cmd, None, 1 << 20, std::time::Duration::from_secs(600))
            .expect("rdy --version");
        let text = String::from_utf8(out.stdout).expect("utf-8");
        let line = text
            .strip_prefix("rdy ")
            .and_then(|t| t.strip_suffix('\n'))
            .unwrap_or_else(|| panic!("rdy --version printed {text:?}"));
        let (version, preview) = match line.strip_suffix(" (preview)") {
            Some(v) => (v, true),
            None => (line, false),
        };
        let target = crate::platform::release_target().unwrap_or(TARGETS[0]);
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("out");
        let archive = package(&rdy, version, target, &out).expect("package the built CLI");
        let stem = if preview {
            format!("rdy-{version}-preview-{target}")
        } else {
            format!("rdy-{version}-{target}")
        };
        assert_eq!(archive, out.join(format!("{stem}.tar.gz")));
        assert_eq!(listing(&archive), members(&stem, preview));
        let into = dir.path().join("x");
        extract(&archive, &into);
        assert!(
            fs::read(into.join(&stem).join("rdy")).expect("rdy") == fs::read(&rdy).expect("rdy"),
            "the archive holds the built binary's bytes"
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in [MIT_FILE, APACHE_FILE] {
            assert_eq!(
                fs::read(into.join(&stem).join(name)).expect("license"),
                fs::read(root.join(name)).expect("license"),
                "{name}"
            );
        }
    }

    #[test]
    fn pre1_package_reads_the_binary_version() {
        assert_eq!(preview_from_version(b"rdy 0.14.0 (preview)\n", V), Ok(true));
        assert_eq!(preview_from_version(b"rdy 0.14.0\n", V), Ok(false));
        for bad in [
            &b"rdy 0.13.0\n"[..],
            b"rdy 0.13.0 (preview)\n",
            b"rdy 0.14.0 (preview)",
            b"rdy 0.14.0",
            b"rdy 0.14.0-rc1\n",
            b"rdy 0.14.0 (beta)\n",
            b"rdy 0.14.0 (Preview)\n",
            b"rdy 0.14.0\nextra\n",
            b"rdy 0.14.0\n\n",
            b"rdy 0.14.0 (preview)\nrdy 0.14.0\n",
            b"Rdy 0.14.0\n",
            b"rdy  0.14.0\n",
            b" rdy 0.14.0\n",
            b"rdy 0.14.0\r\n",
            b"rdy-cli 0.14.0\n",
            b"",
            b"\n",
        ] {
            assert_eq!(
                preview_from_version(bad, V),
                Err(MSG_BINARY_VERSION.to_string()),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }

        // Real children through the runner: status and stderr count, and the runner's own
        // failures take precedence over the version check.
        let probe = |script: &str| {
            classify_probe(
                run_bounded(
                    shell_script(script),
                    None,
                    crate::platform::RELEASE_CHILD_OUTPUT_MAX,
                    std::time::Duration::from_secs(600),
                ),
                V,
            )
        };
        assert_eq!(probe("printf 'rdy 0.14.0 (preview)\\n'"), Ok(true));
        assert_eq!(probe("printf 'rdy 0.14.0\\n'"), Ok(false));
        assert_eq!(probe("printf 'rdy 0.15.0\\n'"), Err(Failure::BinaryVersion));
        assert_eq!(
            probe("printf 'rdy 0.14.0\\n'; printf 'warning\\n' >&2"),
            Err(Failure::BinaryVersion)
        );
        assert_eq!(
            probe("printf 'rdy 0.14.0\\n'; exit 1"),
            Err(Failure::BinaryVersion)
        );
        assert_eq!(probe("head -c 1048577 /dev/zero"), Err(Failure::SizeLimit));
        assert_eq!(
            probe("printf 'rdy 0.14.0\\n'; head -c 1048576 /dev/zero >&2"),
            Err(Failure::SizeLimit)
        );
        let run = run_bounded_inner(
            shell_script("printf 'rdy 0.14.0\\n'; exec sleep 1000"),
            None,
            None,
            crate::platform::RELEASE_CHILD_OUTPUT_MAX,
            None,
            Expiry::expire,
        );
        assert_eq!(classify_probe(run.result, V), Err(Failure::FileOperation));

        // Through packaging: the exact public text, and OUT is gone again.
        let f = Fixture::new();
        let before = f.state();
        for (stdout, want) in [
            ("rdy 0.13.0\\n", MSG_BINARY_VERSION),
            ("rdy 0.14.0 (preview)", MSG_BINARY_VERSION),
        ] {
            let probe = printing(stdout);
            let got = package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe))
                .map_err(Failure::message);
            assert_eq!(got, Err(want), "{stdout}");
            assert_eq!(f.state(), before, "no path is left behind");
        }
        assert_eq!(
            Failure::BinaryVersion.message(),
            "release package: invalid binary version"
        );
    }

    /// Spoils one license file in place.
    type Spoil = fn(&Path);

    #[test]
    fn pre1_package_refuses_inputs_and_never_overwrites_license_files() {
        let probe = printing("rdy 0.14.0\\n");
        for name in [MIT_FILE, APACHE_FILE] {
            let cases: [(&str, Spoil, Failure); 5] = [
                (
                    "missing",
                    |p| fs::remove_file(p).expect("rm"),
                    Failure::InputFile,
                ),
                (
                    "empty",
                    |p| fs::write(p, b"").expect("write"),
                    Failure::InputFile,
                ),
                (
                    "a directory",
                    |p| {
                        fs::remove_file(p).expect("rm");
                        fs::create_dir(p).expect("mkdir");
                    },
                    Failure::InputFile,
                ),
                (
                    "a symlink",
                    |p| {
                        let real = p.with_extension("real");
                        fs::rename(p, &real).expect("mv");
                        crate::platform::symlink(&real, p).expect("symlink");
                    },
                    Failure::InputFile,
                ),
                (
                    "one byte over",
                    |p| fs::write(p, vec![b'x'; 65_537]).expect("write"),
                    Failure::SizeLimit,
                ),
            ];
            for (what, spoil, want) in cases {
                let f = Fixture::new();
                spoil(&f.licenses.join(name));
                let before = f.state();
                let got = package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe));
                assert_eq!(got, Err(want), "{name} {what}");
                assert_eq!(f.state(), before, "{name} {what}: nothing changed");
                assert!(!f.out().exists(), "{name} {what}: no OUT");
            }
        }

        // Order: grammar, then binary, LICENSE-MIT, LICENSE-APACHE, then OUT.
        let f = Fixture::new();
        fs::write(f.licenses.join(MIT_FILE), vec![b'x'; 65_537]).expect("write");
        fs::remove_file(f.licenses.join(APACHE_FILE)).expect("rm");
        let s = setup(&f, &probe);
        assert_eq!(
            package_with(&f.binary, V, TARGET, &f.out(), &s),
            Err(Failure::SizeLimit),
            "LICENSE-MIT before LICENSE-APACHE"
        );
        assert_eq!(
            package_with(&f.licenses.join("nothing"), V, TARGET, &f.out(), &s),
            Err(Failure::InputFile),
            "the binary before the licenses"
        );
        assert_eq!(
            package_with(&f.binary, "0.14", TARGET, &f.out(), &s),
            Err(Failure::Arguments),
            "grammar first"
        );
        let f = Fixture::new();
        fs::remove_file(f.licenses.join(APACHE_FILE)).expect("rm");
        fs::create_dir(f.out()).expect("occupy OUT");
        assert_eq!(
            package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe)),
            Err(Failure::InputFile),
            "LICENSE-APACHE before OUT"
        );

        // A license of exactly the cap is packaged.
        if tar_or_skip(&crate::test_name!(
            "pre1_package_refuses_inputs_and_never_overwrites_license_files"
        )) {
            let f = Fixture::new();
            let full = vec![b'm'; 65_536];
            fs::write(f.licenses.join(MIT_FILE), &full).expect("write");
            fs::write(f.licenses.join(APACHE_FILE), &full).expect("write");
            let archive = package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe))
                .map_err(Failure::message)
                .expect("licenses at the cap");
            let into = f.dir.path().join("x");
            extract(&archive, &into);
            let root = into.join("rdy-0.14.0-aarch64-apple-darwin");
            assert_eq!(fs::read(root.join(MIT_FILE)).expect("mit"), full);
            assert_eq!(fs::read(root.join(APACHE_FILE)).expect("apache"), full);
        }

        // A file that grows past its cap between the check and the copy fails the copy.
        let f = Fixture::new();
        let mut to = File::create_new(f.dir.path().join("copy")).expect("create");
        let from = open_input(&f.binary, BINARY_MAX).expect("open the binary");
        assert_eq!(
            copy_from(&from, &mut to, BINARY.len() as u64 - 1),
            Err(Failure::SizeLimit)
        );
        let mut to = File::create_new(f.dir.path().join("copy.b")).expect("create");
        let from = open_input(&f.binary, BINARY_MAX).expect("open the binary");
        assert_eq!(copy_from(&from, &mut to, BINARY.len() as u64), Ok(()));
    }

    #[test]
    fn pre1_package_failure_removes_only_its_own_paths() {
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_failure_removes_only_its_own_paths"
        )) {
            return;
        }
        let probe = printing("rdy 0.14.0 (preview)\\n");
        let partial = |_: &str| shell_script("printf partial; exit 3");
        let missing = |_: &str| Command::new("/nonexistent/rdy-qa-tar");
        let noisy = |_: &str| shell_script("head -c 1048577 /dev/zero >&2");
        type Tar<'a> = &'a dyn Fn(&str) -> Command;
        let cases: [(&str, Option<Tar>, u64, Failure); 4] = [
            (
                "a failed tar that left a partial archive",
                Some(&partial),
                ARCHIVE_MAX,
                Failure::FileOperation,
            ),
            (
                "a tar that cannot start",
                Some(&missing),
                ARCHIVE_MAX,
                Failure::FileOperation,
            ),
            (
                "a tar over the output cap",
                Some(&noisy),
                ARCHIVE_MAX,
                Failure::SizeLimit,
            ),
            ("an archive over its cap", None, 1, Failure::SizeLimit),
        ];
        for (what, tar, archive_max, want) in cases {
            let f = Fixture::new();
            let before = f.state();
            let mut s = setup(&f, &probe);
            s.archive_max = archive_max;
            if let Some(tar) = tar {
                s.tar = tar;
            }
            assert_eq!(
                package_with(&f.binary, V, TARGET, &f.out(), &s),
                Err(want),
                "{what}"
            );
            assert_eq!(f.state(), before, "{what}: only new paths were removed");
            assert!(!f.out().exists(), "{what}: OUT and its stage are gone");
        }
    }

    /// A paused child: it writes `ready` to the named pipe `ready`, waits for a line on the named
    /// pipe `go`, then runs the shell text `then`.
    fn paused(ready: &Path, go: &Path, then: &str) -> Command {
        let mut c = shell_script(&format!("echo ready > \"$0\"; read go < \"$1\"; {then}"));
        c.arg(ready).arg(go);
        c
    }

    /// On a thread of its own: waits for the ready line, runs `act`, then sends the go line. The
    /// returned channel reports that `act` ran.
    fn on_ready(
        ready: PathBuf,
        go: PathBuf,
        act: impl FnOnce() + Send + 'static,
    ) -> std::sync::mpsc::Receiver<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if fifo_line(ready)
                .recv_timeout(std::time::Duration::from_secs(60))
                .is_err()
            {
                return;
            }
            act();
            use std::io::Write;
            let sent = File::options()
                .write(true)
                .open(&go)
                .and_then(|mut g| g.write_all(b"go\n"));
            if sent.is_ok() {
                let _ = tx.send(());
            }
        });
        rx
    }

    /// A paused wrapper of `real`: it writes `ready` to the named pipe `ready`, waits for a line
    /// on the named pipe `go`, then execs `real`'s program with its arguments and environment.
    fn paused_exec(ready: &Path, go: &Path, real: &Command) -> Command {
        let mut c = shell_script("echo ready > \"$0\"; read go < \"$1\"; shift; exec \"$@\"");
        c.arg(ready)
            .arg(go)
            .arg(real.get_program())
            .args(real.get_args());
        for (key, value) in real.get_envs() {
            match value {
                Some(value) => c.env(key, value),
                None => c.env_remove(key),
            };
        }
        c
    }

    #[test]
    fn pre1_package_never_takes_an_archive_path_it_did_not_create() {
        const STEM: &str = "rdy-0.14.0-preview-aarch64-apple-darwin";
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_never_takes_an_archive_path_it_did_not_create"
        )) {
            return;
        }
        let missing = |_: &str| Command::new("/nonexistent/rdy-qa-tar");
        for (case, what) in [
            "a newcomer at the archive path before tar, with tar",
            "a newcomer at the archive path before tar, with a tar that cannot start",
            "a newcomer in place of the archive while tar runs, then tar succeeds",
            "a newcomer in place of the archive while tar runs, then tar fails",
            "a newcomer in place of the archive before the real tar starts",
        ]
        .into_iter()
        .enumerate()
        {
            let f = Fixture::new();
            let before = f.state();
            let (ready, go) = (f.dir.path().join("ready"), f.dir.path().join("go"));
            fifo(&ready);
            fifo(&go);
            let archive = f.out().join(format!("{STEM}.tar.gz"));
            let placed = on_ready(ready.clone(), go.clone(), {
                let archive = archive.clone();
                move || {
                    let _ = fs::remove_file(&archive);
                    fs::write(&archive, b"newcomer\n").expect("place the newcomer");
                }
            });
            let paused_probe = {
                let (r, g) = (ready.clone(), go.clone());
                move |_: &Path| paused(&r, &g, "printf 'rdy 0.14.0 (preview)\\n'")
            };
            let plain_probe = printing("rdy 0.14.0 (preview)\\n");
            let paused_tar = {
                let (r, g) = (ready.clone(), go.clone());
                let code = if case == 2 { 0 } else { 3 };
                move |_: &str| paused(&r, &g, &format!("exit {code}"))
            };
            let paused_real_tar = {
                let (r, g) = (ready.clone(), go.clone());
                move |stem: &str| paused_exec(&r, &g, &tar_command(stem))
            };
            let mut s = setup(
                &f,
                if case < 2 {
                    &paused_probe
                } else {
                    &plain_probe
                },
            );
            s.tar = match case {
                0 => &tar_command,
                1 => &missing,
                2 | 3 => &paused_tar,
                _ => &paused_real_tar,
            };
            let got = package_with(&f.binary, V, TARGET, &f.out(), &s);
            placed
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("{what}: the newcomer was placed"));
            assert_eq!(got, Err(Failure::FileOperation), "{what}");
            assert_eq!(
                fs::read(&archive).expect("the newcomer"),
                b"newcomer\n",
                "{what}: the newcomer is untouched"
            );
            assert_eq!(
                names(&f.out()),
                vec![format!("{STEM}.tar.gz")],
                "{what}: OUT holds only the newcomer"
            );
            let mut after = f.state();
            after[2].1.retain(|n| n != "out");
            assert_eq!(after, before, "{what}: the inputs are unchanged");
        }
    }

    /// Every entry under `dir`, sorted, with each file's bytes (`None` for a directory); none
    /// when `dir` is gone.
    fn tree_of(dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        if fs::symlink_metadata(dir).is_err() {
            return Vec::new();
        }
        fn walk(root: &Path, dir: &Path, into: &mut Vec<(String, Option<Vec<u8>>)>) {
            for e in fs::read_dir(dir).expect("read_dir") {
                let p = e.expect("entry").path();
                let name = p
                    .strip_prefix(root)
                    .expect("under the root")
                    .to_string_lossy()
                    .into_owned();
                if fs::symlink_metadata(&p).expect("stat").is_dir() {
                    into.push((name, None));
                    walk(root, &p, into);
                } else {
                    into.push((name, Some(fs::read(&p).expect("read"))));
                }
            }
        }
        let mut v = Vec::new();
        walk(dir, dir, &mut v);
        v.sort();
        v
    }

    /// A decoy OUT at `out`: a stage of its own, as staged inputs and as a staged release, with
    /// an unprobed binary and other licenses.
    fn make_decoy(out: &Path, stem: &str) {
        for d in [out.join(STAGE_DIR), out.join(STAGE_DIR).join(stem)] {
            fs::create_dir_all(&d).expect("mkdir");
            for (name, bytes) in [
                ("rdy", &b"an unprobed binary\n"[..]),
                (MIT_FILE, b"a decoy license\n"),
                (APACHE_FILE, b"a decoy license\n"),
            ] {
                fs::write(d.join(name), bytes).expect("write");
            }
        }
    }

    #[test]
    fn pre1_package_keeps_to_its_own_out_when_out_is_replaced() {
        const STEM: &str = "rdy-0.14.0-preview-aarch64-apple-darwin";
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_keeps_to_its_own_out_when_out_is_replaced"
        )) {
            return;
        }
        let reference = tempfile::tempdir().expect("tempdir");
        make_decoy(reference.path(), STEM);
        let decoy = tree_of(reference.path());
        // Someone who can write OUT's parent moves OUT aside and puts a decoy in its place: while
        // the probe runs, and after it, before the real tar starts.
        for (case, what) in [
            "OUT replaced while the probe runs",
            "OUT replaced before tar runs",
        ]
        .into_iter()
        .enumerate()
        {
            let f = Fixture::new();
            let before = f.state();
            let (ready, go) = (f.dir.path().join("ready"), f.dir.path().join("go"));
            fifo(&ready);
            fifo(&go);
            let moved = f.parent.join("moved");
            let replaced = on_ready(ready.clone(), go.clone(), {
                let (out, moved) = (f.out(), moved.clone());
                move || {
                    fs::rename(&out, &moved).expect("move OUT aside");
                    make_decoy(&out, STEM);
                }
            });
            let paused_probe = {
                let (r, g) = (ready.clone(), go.clone());
                move |_: &Path| paused(&r, &g, "printf 'rdy 0.14.0 (preview)\\n'")
            };
            let plain_probe = printing("rdy 0.14.0 (preview)\\n");
            let paused_real_tar = {
                let (r, g) = (ready.clone(), go.clone());
                move |stem: &str| paused_exec(&r, &g, &tar_command(stem))
            };
            let mut s = setup(
                &f,
                if case == 0 {
                    &paused_probe
                } else {
                    &plain_probe
                },
            );
            if case == 1 {
                s.tar = &paused_real_tar;
            }
            let got = package_with(&f.binary, V, TARGET, &f.out(), &s);
            replaced
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("{what}: OUT was replaced"));
            unlock(&moved);
            assert_eq!(tree_of(&f.out()), decoy, "{what}: the decoy is untouched");
            assert_eq!(
                tree_of(&moved),
                Vec::new(),
                "{what}: nothing of this run is left in its own OUT"
            );
            assert_eq!(got, Err(Failure::FileOperation), "{what}");
            let mut after = f.state();
            after[2].1.retain(|n| n != "out" && n != "moved");
            assert_eq!(after, before, "{what}: the inputs are unchanged");
        }
    }

    /// A change to a fixture's input paths.
    type Change = fn(&Fixture);

    /// Packages a fixture on a thread of its own: `prepare` runs before packaging, `change`
    /// once the inputs are checked. A hang guard: packaging must end within a minute.
    fn package_changed(prepare: Change, change: Change) -> (Result<PathBuf, Failure>, Fixture) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let f = Fixture::new();
            prepare(&f);
            let probe = printing("rdy 0.14.0\\n");
            let hook = || change(&f);
            let got = {
                let mut s = setup(&f, &probe);
                s.checked = &hook;
                package_with(&f.binary, V, TARGET, &f.out(), &s)
            };
            let _ = tx.send((got, f));
        });
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .expect("packaging did not wait on a changed input")
    }

    #[test]
    fn pre1_package_copies_the_inputs_it_checked() {
        fn unchanged(_: &Fixture) {}
        // A FIFO is refused at the check, without waiting for a writer.
        let cases: [(&str, Change); 2] = [
            ("a FIFO binary", |f| {
                fs::remove_file(&f.binary).expect("rm");
                fifo(&f.binary);
            }),
            ("a FIFO LICENSE-MIT", |f| {
                let p = f.licenses.join(MIT_FILE);
                fs::remove_file(&p).expect("rm");
                fifo(&p);
            }),
        ];
        for (what, prepare) in cases {
            let (got, f) = package_changed(prepare, unchanged);
            assert_eq!(got, Err(Failure::InputFile), "{what}");
            assert!(!f.out().exists(), "{what}: no OUT");
        }

        // Once checked, an input path that changes does not change what is packaged.
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_copies_the_inputs_it_checked"
        )) {
            return;
        }
        let cases: [(&str, Change); 4] = [
            ("a symlink in place of LICENSE-MIT", |f| {
                let p = f.licenses.join(MIT_FILE);
                let other = f.dir.path().join("other");
                fs::write(&other, b"other bytes\n").expect("write");
                fs::remove_file(&p).expect("rm");
                crate::platform::symlink(&other, &p).expect("symlink");
            }),
            ("another file in place of LICENSE-APACHE", |f| {
                let p = f.licenses.join(APACHE_FILE);
                fs::remove_file(&p).expect("rm");
                fs::write(&p, b"other bytes\n").expect("write");
            }),
            ("a FIFO in place of the binary", |f| {
                fs::remove_file(&f.binary).expect("rm");
                fifo(&f.binary);
            }),
            ("a symlink in place of the binary", |f| {
                let other = f.dir.path().join("other");
                fs::write(&other, b"other bytes\n").expect("write");
                fs::remove_file(&f.binary).expect("rm");
                crate::platform::symlink(&other, &f.binary).expect("symlink");
            }),
        ];
        for (what, change) in cases {
            let (got, f) = package_changed(unchanged, change);
            let archive = got
                .map_err(Failure::message)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            let into = f.dir.path().join("x");
            extract(&archive, &into);
            let root = into.join("rdy-0.14.0-aarch64-apple-darwin");
            assert_eq!(fs::read(root.join("rdy")).expect("rdy"), BINARY, "{what}");
            assert_eq!(fs::read(root.join(MIT_FILE)).expect("mit"), MIT, "{what}");
            assert_eq!(
                fs::read(root.join(APACHE_FILE)).expect("apache"),
                APACHE,
                "{what}"
            );
        }
    }

    /// Sets OUT and each directory in it back to 0700, so the fixture can be removed.
    fn unlock(out: &Path) {
        if fs::symlink_metadata(out).is_err() {
            return;
        }
        crate::platform::set_mode(out, 0o700).expect("unlock OUT");
        for e in fs::read_dir(out).expect("read_dir") {
            let p = e.expect("entry").path();
            if fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
                crate::platform::set_mode(&p, 0o700).expect("unlock the stage");
            }
        }
    }

    #[test]
    fn pre1_package_cleanup_failure_takes_precedence() {
        let name = crate::test_name!("pre1_package_cleanup_failure_takes_precedence");
        let probe_dir = tempfile::tempdir().expect("tempdir");
        crate::platform::set_mode(probe_dir.path(), 0o500).expect("chmod");
        let writable = fs::write(probe_dir.path().join("x"), b"x").is_ok();
        crate::platform::set_mode(probe_dir.path(), 0o700).expect("chmod");
        if writable {
            skip::skip(
                &name,
                Why::Prerequisite,
                "a read-only directory takes writes here (running as root?)",
            );
            return;
        }
        // Each probe or tar makes a directory that the cleanup must empty read-only.
        let locking = |script: &'static str| {
            move |bin: &Path| {
                let mut c = shell_script(script);
                c.arg(bin);
                c
            }
        };
        let rejected = locking("chmod 500 \"${0%/*}\"; printf 'rdy 0.13.0\\n'");
        let overflow = locking("chmod 500 \"${0%/*}\"; head -c 1048577 /dev/zero");
        let fine = printing("rdy 0.14.0\\n");
        // tar starts in OUT
        let locking_tar = |stem: &str| {
            let mut c = shell_script("tar -czf - \"$0\" && chmod 500 .");
            c.arg(stem).env("COPYFILE_DISABLE", "1");
            c
        };
        type Probe<'a> = &'a dyn Fn(&Path) -> Command;
        let cases: [(&str, Probe, Option<TarCommand>, u64); 3] = [
            ("a version rejection", &rejected, None, ARCHIVE_MAX),
            ("an output overflow", &overflow, None, ARCHIVE_MAX),
            ("an archive over its cap", &fine, Some(&locking_tar), 1),
        ];
        for (what, probe, tar, archive_max) in cases {
            if tar.is_some() && !tar_or_skip(&name) {
                continue;
            }
            let f = Fixture::new();
            let before = f.state();
            let mut s = setup(&f, probe);
            s.archive_max = archive_max;
            if let Some(tar) = tar {
                s.tar = tar;
            }
            let got = package_with(&f.binary, V, TARGET, &f.out(), &s);
            unlock(&f.out());
            assert_eq!(
                got,
                Err(Failure::FileOperation),
                "{what}: the cleanup failure is the result"
            );
            let mut after = f.state();
            after[2].1.retain(|n| n != "out");
            assert_eq!(after, before, "{what}: the inputs are unchanged");
        }
    }

    #[test]
    fn pre1_valid_version_and_target_grammar() {
        for good in ["0.1.0", "0.14.0", "0.1234567890.0"] {
            assert!(valid_version(good), "{good}");
        }
        for bad in [
            "0.0.0",
            "0.01.0",
            "0.12345678901.0",
            "1.0.0",
            "0.14.1",
            "0.14",
            "0.14.0-preview",
            "v0.14.0",
            " 0.14.0",
            "0.14.0\n",
            "",
        ] {
            assert!(!valid_version(bad), "{bad:?}");
        }
        assert!(valid_target("aarch64-apple-darwin"));
        assert!(valid_target("x86_64-unknown-linux-gnu"));
        for bad in [
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "",
            "aarch64-apple-darwin\n",
        ] {
            assert!(!valid_target(bad), "{bad:?}");
        }
        assert!(valid_path(Path::new("/a")));
        assert!(valid_path(Path::new(&format!("/{}", "a".repeat(4095)))));
        for bad in [
            "a".to_string(),
            format!("/{}", "a".repeat(4096)),
            "/a\0b".to_string(),
            "/a\rb".to_string(),
            "/a\nb".to_string(),
        ] {
            assert!(!valid_path(Path::new(&bad)), "{bad:?}");
        }
    }
}
