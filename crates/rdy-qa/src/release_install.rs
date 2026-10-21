//! Release archives (D-NFJ3D, D-XTWM9): one native `.tar.gz` per platform, holding the `rdy`
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
/// and of each preview's release notes (D-XTWM9).
pub const PREVIEW_NOTE: &str = "This is a preview of rdy.
It comes before Release 1, the first release with a compatibility promise.

Until Release 1, any command, flag, tool, code or file format may change in the next preview, with no deprecation period.
One thing stays: every later rdy, Release 1 included, reads every dump that this rdy writes.

To move a project to a newer preview, run `rdy dump --out <file>` with this rdy.
Then run `rdy load <file> <dir>` with the new rdy, into a directory with no store.
A project made with a preview moves to Release 1 the same way.

Report problems at https://github.com/adrenickchrome/rdy-core/issues.
";

/// The two release targets (D-NFJ3D): macOS arm64 and Linux amd64.
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

/// The command that writes the archive (path, stage, stem).
type TarCommand<'a> = &'a dyn Fn(&Path, &Path, &str) -> Command;

/// What packaging reads besides its arguments; tests swap parts of it.
struct Setup<'a> {
    /// The directory holding `LICENSE-MIT` and `LICENSE-APACHE`.
    licenses: PathBuf,
    archive_max: u64,
    tar: TarCommand<'a>,
    /// The version probe of the staged binary.
    probe: &'a dyn Fn(&Path) -> Command,
}

impl Setup<'static> {
    /// The real release: the repository's license files, native tar, the binary's own probe.
    fn release() -> Self {
        Setup {
            licenses: repository_root(),
            archive_max: ARCHIVE_MAX,
            tar: &tar_command,
            probe: &version_command,
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

/// `tar -czf <archive> -C <stage> <stem>`. AppleDouble files stay out of the archive.
fn tar_command(archive: &Path, stage: &Path, stem: &str) -> Command {
    let mut cmd = Command::new("tar");
    cmd.arg("-czf")
        .arg(archive)
        .arg("-C")
        .arg(stage)
        .arg(stem)
        .env("COPYFILE_DISABLE", "1");
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
pub fn package(binary: &Path, version: &str, target: &str, out: &Path) -> Result<PathBuf, String> {
    package_with(binary, version, target, out, &Setup::release())
        .map_err(|f| f.message().to_string())
}

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
    check_input(binary, BINARY_MAX)?;
    let mit = setup.licenses.join(MIT_FILE);
    check_input(&mit, LICENSE_MAX)?;
    let apache = setup.licenses.join(APACHE_FILE);
    check_input(&apache, LICENSE_MAX)?;
    match fs::symlink_metadata(out) {
        Ok(_) => return Err(Failure::OutputExists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(Failure::FileOperation),
    }
    let parent_is_dir = out
        .parent()
        .is_some_and(|p| fs::metadata(p).is_ok_and(|m| m.is_dir()));
    if !parent_is_dir {
        return Err(Failure::FileOperation);
    }
    match fs::create_dir(out) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(Failure::OutputExists),
        Err(_) => return Err(Failure::FileOperation),
    }
    let made = (|| {
        let stage = tempfile::Builder::new()
            .prefix(".rdy-stage-")
            .tempdir_in(out)
            .map_err(|_| Failure::FileOperation)?;
        let staged = assemble(stage.path(), binary, &mit, &apache, version, target, setup);
        // the stage goes on every path; failing to remove it fails the run
        let closed = stage.close().map_err(|_| Failure::FileOperation);
        let archive = out.join(format!("{}.tar.gz", staged?));
        let checked = archive_written(&archive, setup.archive_max).and(closed);
        if checked.is_err() {
            let _ = fs::remove_file(&archive);
        }
        checked.map(|()| archive)
    })();
    if made.is_err() {
        // OUT itself, which this call made; it is empty again unless a cleanup failed
        let _ = fs::remove_dir(out);
    }
    made
}

/// Stages the release in `stage` and writes `<out>/<stem>.tar.gz` beside it; returns the stem.
/// A partial archive is removed on failure.
fn assemble(
    stage: &Path,
    binary: &Path,
    mit: &Path,
    apache: &Path,
    version: &str,
    target: &str,
    setup: &Setup,
) -> Result<String, Failure> {
    let rdy = stage.join("rdy");
    copy_bounded(binary, &rdy, BINARY_MAX)?;
    let licenses = [(MIT_FILE, mit), (APACHE_FILE, apache)];
    for (name, from) in licenses {
        copy_bounded(from, &stage.join(name), LICENSE_MAX)?;
        set_mode(&stage.join(name), 0o644)?;
    }
    set_mode(&rdy, 0o755)?;
    let preview = probe_version(&rdy, version, setup)?;
    let stem = if preview {
        format!("rdy-{version}-preview-{target}")
    } else {
        format!("rdy-{version}-{target}")
    };
    let dir = stage.join(&stem);
    fs::create_dir(&dir).map_err(|_| Failure::FileOperation)?;
    set_mode(&dir, 0o755)?;
    for name in ["rdy", MIT_FILE, APACHE_FILE] {
        fs::rename(stage.join(name), dir.join(name)).map_err(|_| Failure::FileOperation)?;
    }
    if preview {
        let note = dir.join(NOTE_FILE);
        write_new(&note, PREVIEW_NOTE.as_bytes())?;
        set_mode(&note, 0o644)?;
    }
    let out = stage.parent().ok_or(Failure::FileOperation)?;
    let archive = out.join(format!("{stem}.tar.gz"));
    let tarred = platform::run_bounded(
        (setup.tar)(&archive, stage, &stem),
        None,
        RELEASE_CHILD_OUTPUT_MAX,
        RELEASE_CHILD_DEADLINE,
    );
    let tarred = match tarred {
        Ok(o) if o.status.success() => Ok(stem),
        Ok(_) => Err(Failure::FileOperation),
        Err(e) => Err(child_failure(&e)),
    };
    if tarred.is_err() {
        let _ = fs::remove_file(&archive);
    }
    tarred
}

/// The archive tar wrote: a regular file of 1..=`max` bytes.
fn archive_written(archive: &Path, max: u64) -> Result<(), Failure> {
    let meta = fs::symlink_metadata(archive).map_err(|_| Failure::FileOperation)?;
    if !meta.file_type().is_file() || meta.len() == 0 {
        return Err(Failure::FileOperation);
    }
    if meta.len() > max {
        return Err(Failure::SizeLimit);
    }
    Ok(())
}

/// An input file: a regular file (not a symlink) of 1..=`max` bytes.
fn check_input(path: &Path, max: u64) -> Result<(), Failure> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::NotADirectory
                    | io::ErrorKind::InvalidFilename
            ) =>
        {
            return Err(Failure::InputFile);
        }
        Err(_) => return Err(Failure::FileOperation),
    };
    if !meta.file_type().is_file() || meta.len() == 0 {
        return Err(Failure::InputFile);
    }
    if meta.len() > max {
        return Err(Failure::SizeLimit);
    }
    Ok(())
}

/// Copies `from` into the new file `to`, reading at most `max + 1` bytes: a file that grew past
/// `max` since its check fails, and so does one that is now empty or not a regular file.
fn copy_bounded(from: &Path, to: &Path, max: u64) -> Result<(), Failure> {
    use std::io::Read;
    let src = File::open(from).map_err(|_| Failure::FileOperation)?;
    if !src
        .metadata()
        .map_err(|_| Failure::FileOperation)?
        .is_file()
    {
        return Err(Failure::InputFile);
    }
    let mut dst = File::create_new(to).map_err(|_| Failure::FileOperation)?;
    let copied = io::copy(&mut src.take(max.saturating_add(1)), &mut dst)
        .map_err(|_| Failure::FileOperation)?;
    dst.sync_all().map_err(|_| Failure::FileOperation)?;
    match copied {
        0 => Err(Failure::InputFile),
        n if n > max => Err(Failure::SizeLimit),
        _ => Ok(()),
    }
}

/// Writes `bytes` to the new file `path`.
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    use std::io::Write;
    let mut f = File::create_new(path).map_err(|_| Failure::FileOperation)?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|_| Failure::FileOperation)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Failure> {
    platform::set_mode(path, mode).map_err(|_| Failure::FileOperation)
}

/// How often the probe is retried while the kernel still sees the staged copy as open for
/// writing (`ETXTBSY`: another thread's child may hold the descriptor until it execs).
const PROBE_BUSY_TRIES: u32 = 50;

/// Runs the staged binary's `--version` (the setup's probe) and reads whether it is a preview.
fn probe_version(binary: &Path, version: &str, setup: &Setup) -> Result<bool, Failure> {
    let mut tries = 1;
    loop {
        let run = platform::run_bounded(
            (setup.probe)(binary),
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

    #[test]
    fn pre1_package_refuses_inputs_and_never_overwrites_license_files() {
        let probe = printing("rdy 0.14.0\\n");
        for name in [MIT_FILE, APACHE_FILE] {
            let cases: [(&str, fn(&Path), Failure); 5] = [
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
        let to = f.dir.path().join("copy");
        assert_eq!(
            copy_bounded(&f.binary, &to, BINARY.len() as u64 - 1),
            Err(Failure::SizeLimit)
        );
        assert_eq!(
            copy_bounded(&f.binary, &to.with_extension("b"), BINARY.len() as u64),
            Ok(())
        );
    }

    #[test]
    fn pre1_package_failure_removes_only_its_own_paths() {
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_failure_removes_only_its_own_paths"
        )) {
            return;
        }
        let probe = printing("rdy 0.14.0 (preview)\\n");
        let partial = |archive: &Path, _: &Path, _: &str| {
            let mut c = shell_script("printf partial > \"$0\"; exit 3");
            c.arg(archive);
            c
        };
        let missing = |_: &Path, _: &Path, _: &str| Command::new("/nonexistent/rdy-qa-tar");
        let noisy = |_: &Path, _: &Path, _: &str| shell_script("head -c 1048577 /dev/zero");
        type Tar<'a> = &'a dyn Fn(&Path, &Path, &str) -> Command;
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
