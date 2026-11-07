//! Release archives: one native `.tar.gz` per platform, holding the `rdy`
//! binary, both license files, the third-party notices (`THIRD-PARTY-NOTICES.txt`) and, for a
//! preview build, the preview note as `PREVIEW.txt`. The
//! binary's own `--version` output decides whether it is a preview, so the archive's name and
//! note follow the build, with no second switch. Packaging stops at the unsigned archive: the
//! signed `SHA256SUMS` is made later, offline.
//!
//! `qa-release-package` is the command; [`package`] does the work.
use crate::platform::{self, RELEASE_CHILD_DEADLINE, RELEASE_CHILD_OUTPUT_MAX};
use crate::release_keys::ReleaseKeys;
use crate::release_notices;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The preview note: `PREVIEW.txt` in a preview archive, the README's "About this preview"
/// block, and each preview's release notes.
pub const PREVIEW_NOTE: &str = "This is a preview of rdy.
It comes before Release 1, the first release with a compatibility promise.

Until Release 1, any command, flag, tool, code or file format may change in the next preview, with no deprecation period.
One thing stays: every later rdy, Release 1 included, reads every dump that this rdy writes.

To move a project to a newer preview, run `rdy dump --out <file>` with this rdy.
Then run `rdy load <file> <dir>` with the new rdy, into a directory with no store.
A project made with a preview moves to Release 1 the same way.

Report problems at https://github.com/adrenickchrome/rdy-core/issues.
";

/// The two Unix release targets: macOS arm64 and Linux amd64. Their archive lines are the whole
/// signed manifest; the Windows target joins the manifest with its install checker.
pub const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"];
/// The Windows release target: its archive holds `rdy.exe` where the others hold `rdy`.
pub const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
/// The MIT license file, at the repository root and in each archive.
pub const MIT_FILE: &str = "LICENSE-MIT";
/// The Apache 2.0 license file, at the repository root and in each archive.
pub const APACHE_FILE: &str = "LICENSE-APACHE";
/// The preview note's file name in a preview archive.
pub const NOTE_FILE: &str = "PREVIEW.txt";
/// The third-party notices' file name in each archive.
pub const NOTICES_FILE: &str = "THIRD-PARTY-NOTICES.txt";
/// The largest binary packaged: 512 MiB. A fixed bound by judgement: the debug binary that the
/// package tests stage passed 256 MiB in M3b (320 MB); a release binary is far below either.
pub const BINARY_MAX: u64 = 536_870_912;
/// The largest license file packaged: 64 KiB.
pub const LICENSE_MAX: u64 = 65_536;
/// The largest third-party notices file packaged: 16 MiB.
pub const NOTICES_MAX: u64 = 16_777_216;
/// The largest archive accepted: the binary cap plus the notices cap plus 1 MiB.
pub const ARCHIVE_MAX: u64 = BINARY_MAX + NOTICES_MAX + 1_048_576;
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
pub const MSG_NOTICES: &str = "release package: third-party notices are incomplete";

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
    /// The third-party notices could not be made, or are over their cap.
    Notices,
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
            Failure::Notices => MSG_NOTICES,
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
    /// This machine's release target, the only one the real release packages for: the version
    /// probe runs the binary here, so a binary it accepts is native. `None` (stand-ins only)
    /// accepts either target.
    native_target: Option<&'a dyn Fn() -> io::Result<&'static str>>,
    /// The notices of a target, called once the inputs are open and before the OUT check.
    notices: &'a dyn Fn(&str) -> Result<release_notices::Notices, String>,
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
            native_target: Some(&platform::release_target),
            notices: &release_notices::for_target,
        }
    }
}

/// The repository root, from this crate's manifest directory (`crates/rdy-qa`).
pub(crate) fn repository_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .unwrap_or(manifest)
        .to_path_buf()
}

/// `tar -czf - <stem>`: the archive goes to stdout. AppleDouble files stay out of the archive.
fn tar_command(stem: &str) -> Command {
    let mut cmd = Command::new(platform::tar_program());
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

/// Whether `target` is one of the [`TARGETS`] or the [`WINDOWS_TARGET`].
pub fn valid_target(target: &str) -> bool {
    TARGETS.contains(&target) || target == WINDOWS_TARGET
}

/// The executable's file name in the archive of `target`, chosen by the triple alone.
fn binary_name(target: &str) -> &'static str {
    if target == WINDOWS_TARGET {
        "rdy.exe"
    } else {
        "rdy"
    }
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
    let mut cmd = Command::new(platform::tar_program());
    cmd.arg("--version");
    platform::run_bounded(cmd, None, RELEASE_CHILD_OUTPUT_MAX, RELEASE_CHILD_DEADLINE)
        .is_ok_and(|o| o.status.success())
}

/// Packages `binary` (a release build of rdy) for `target` into the new directory `out`, which
/// then holds only `<stem>.tar.gz`; returns that archive's path. The stem is
/// `rdy-<version>-preview-<target>` when the binary's `--version` prints `rdy <version>
/// (preview)`, and `rdy-<version>-<target>` when it prints `rdy <version>`. The archive holds
/// `<stem>/` with `rdy` (0755), `LICENSE-MIT` and `LICENSE-APACHE` (0644) from the repository
/// root, `THIRD-PARTY-NOTICES.txt` (0644) with the target's notices
/// ([`release_notices::for_target`]) and, for a preview, `PREVIEW.txt` (0644) with
/// [`PREVIEW_NOTE`]. Packaging never opens a `.crate` file.
///
/// The error is one of the `MSG_` texts. Checks run in order: the grammar of the arguments and
/// that `target` is this machine's release target (the version probe runs the binary here, so
/// the binary is native), the binary, `LICENSE-MIT`, `LICENSE-APACHE`, the notices (at most
/// [`NOTICES_MAX`] bytes), then that `out` does not exist. On failure every path this call
/// created is removed again, and nothing that was there before is touched.
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
    if let Some(native) = setup.native_target
        && native().ok() != Some(target)
    {
        return Err(Failure::Arguments);
    }
    let binary = open_input(binary, BINARY_MAX)?;
    let mit = open_input(&setup.licenses.join(MIT_FILE), LICENSE_MAX)?;
    let apache = open_input(&setup.licenses.join(APACHE_FILE), LICENSE_MAX)?;
    let notices = (setup.notices)(target).map_err(|_| Failure::Notices)?.bytes;
    if notices.len() as u64 > NOTICES_MAX {
        return Err(Failure::Notices);
    }
    let inputs = Inputs {
        binary,
        mit,
        apache,
        notices,
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

/// The three input files, open: the handles their checks read, which the copies read too.
/// And the notices this run owns, already within their cap.
struct Inputs {
    binary: File,
    mit: File,
    apache: File,
    notices: Vec<u8>,
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
        let exe = binary_name(target);
        stage.add(exe, 0o755, |to| copy_from(&inputs.binary, to, BINARY_MAX))?;
        stage.add(MIT_FILE, 0o644, |to| {
            copy_from(&inputs.mit, to, LICENSE_MAX)
        })?;
        stage.add(APACHE_FILE, 0o644, |to| {
            copy_from(&inputs.apache, to, LICENSE_MAX)
        })?;
        stage.add(NOTICES_FILE, 0o644, |to| {
            use std::io::Write;
            to.write_all(&inputs.notices)
                .map_err(|_| Failure::FileOperation)
        })?;
        let preview = probe_version(&stage.dir, exe, version, setup)?;
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
        let start =
            platform::start_in_dir(&mut tar, &self.out_dir).map_err(|_| Failure::FileOperation)?;
        let into = reserved.try_clone().map_err(|_| Failure::FileOperation)?;
        let tarred = platform::run_bounded_to(
            tar,
            None,
            into,
            RELEASE_CHILD_OUTPUT_MAX,
            RELEASE_CHILD_DEADLINE,
        );
        drop(start);
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
/// as `./<exe>` (`rdy`, or `rdy.exe` for Windows), and reads whether it is a preview.
fn probe_version(stage: &File, exe: &str, version: &str, setup: &Setup) -> Result<bool, Failure> {
    let mut tries = 1;
    loop {
        let program =
            platform::program_in_dir(stage, exe.as_ref()).map_err(|_| Failure::FileOperation)?;
        let mut probe = (setup.probe)(program.path());
        let start =
            platform::start_in_dir(&mut probe, stage).map_err(|_| Failure::FileOperation)?;
        let run = platform::run_bounded(
            probe,
            None,
            RELEASE_CHILD_OUTPUT_MAX,
            RELEASE_CHILD_DEADLINE,
        );
        drop((start, program));
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

// ---------------------------------------------------------------------------------------------
// Installing a signed release: `qa-release-install` and [`check`].
//
// The checker takes the trusted signer line as input. It verifies the signed `SHA256SUMS` with
// OpenSSH, then the archive's hash with the native checksum tool, and only then extracts,
// installs and runs the binary. One command source ([`commands`]) gives both the guide a person
// follows and the commands the checker runs.
// ---------------------------------------------------------------------------------------------

/// The release signer's principal in the allowed-signers file.
pub const RELEASE_PRINCIPAL: &str = "rdy-release";
/// The SSHSIG namespace of the signed `SHA256SUMS`.
pub const RELEASE_NAMESPACE: &str = "rdy-release-v1";
/// The signed checksum manifest.
pub const MANIFEST_FILE: &str = "SHA256SUMS";
/// The manifest's signature.
pub const SIGNATURE_FILE: &str = "SHA256SUMS.sig";
/// The allowed-signers file the check writes in the download directory.
pub const SIGNERS_FILE: &str = "release-allowed-signers";
/// The largest manifest accepted.
pub const MANIFEST_MAX: u64 = 1024;
/// The largest signature accepted.
pub const SIGNATURE_MAX: u64 = 16_384;
/// The longest signer line accepted, in bytes.
pub const SIGNER_LINE_MAX: usize = 8192;
/// The macOS attribute a browser download carries.
pub const QUARANTINE_ATTRIBUTE: &str = "com.apple.quarantine";

/// `qa-release-install`'s refusal of its arguments (stderr, with an LF; exit 2).
pub const MSG_INSTALL_INVALID_ARGUMENTS: &str = "release install: invalid arguments";
/// [`render_commands`]'s refusal of a token it cannot render.
pub const MSG_INVALID_COMMAND: &str = "invalid release command";
/// Why a release install test skips when a tool is missing.
pub const INSTALL_SKIP_REASON: &str =
    "release install tests require ssh-keygen, tar, install and the native checksum tool";
/// The rendered browser block's note before downloading.
pub const SAFARI_NOTE: &str = "In Safari, turn off “Open safe files after downloading” before downloading. Keep the original .tar.gz file.";
/// The rendered browser block's note about the quarantine attribute.
pub const QUARANTINE_NOTE: &str = "After both checks pass, remove com.apple.quarantine from the archive before extraction. Skip this command if the archive has no com.apple.quarantine attribute.";

/// The signer line's fixed start: the principal and the namespace option.
const SIGNER_PREFIX: &str = "rdy-release namespaces=\"rdy-release-v1\" ";
/// The two key types a release key may have.
const SIGNER_KEY_TYPES: [&str; 2] = ["ssh-ed25519", "sk-ssh-ed25519@openssh.com"];
/// The macOS target, the only one with a browser path.
const MAC_TARGET: &str = TARGETS[0];
/// The installed check's human actor and the agent session it opens.
const SMOKE_CONFIG: &str = "actor: human:release-check\n";
/// The targets a signed manifest may list, in the manifest's order: sorted by archive name.
const MANIFEST_TARGETS: [&str; 3] = [TARGETS[0], WINDOWS_TARGET, TARGETS[1]];
/// The pipe of the Windows OpenSSH agent. rdy needs `SSH_AUTH_SOCK` to name it, even when ssh
/// finds the agent without it.
pub const WINDOWS_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
/// The installed report's checks, in order.
pub const CHECK_NAMES: [&str; 6] = [
    "signature",
    "checksums",
    "install",
    "version",
    "daemon",
    "mcp",
];

/// What `qa-release-install` checks and installs. Paths are absolute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallInput {
    /// The directory holding the downloaded archive, `SHA256SUMS` and `SHA256SUMS.sig`.
    pub download: PathBuf,
    /// The trusted allowed-signers line, from the README and the release notes.
    pub signer_line: String,
    /// The plain version V.
    pub version: String,
    /// The 40-hex commit S from the build receipt (its syntax only is checked).
    pub commit: String,
    /// The target triple of the archive to install.
    pub target: String,
    /// A new directory to extract into; removed afterwards.
    pub unpack: PathBuf,
    /// The new version prefix, `.../rdy-V`; it holds `bin/rdy` after a success.
    pub prefix: PathBuf,
    /// A macOS browser download: remove the quarantine attribute after both checks.
    pub browser: bool,
}

/// One check of the installed report.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ReleaseCheck {
    pub name: String,
    pub passed: bool,
}

/// The report of a successful install.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct InstalledRelease {
    /// The installed binary, `PREFIX/bin/rdy`.
    pub binary: PathBuf,
    pub version: String,
    pub commit: String,
    pub target: String,
    /// The OS, kernel release and hardware name of this machine.
    pub os: String,
    pub checks: Vec<ReleaseCheck>,
    pub skipped: u32,
}

/// Why a check failed: the stage, or a global limit, or the cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallFailure {
    Input,
    Signature,
    Checksums,
    Install,
    Version,
    Daemon,
    Mcp,
    OutputLimit,
    Deadline,
    Cleanup,
}

impl InstallFailure {
    /// The stage's name in `release install failed: STAGE`.
    pub fn stage(self) -> &'static str {
        match self {
            InstallFailure::Input => "input",
            InstallFailure::Signature => "signature",
            InstallFailure::Checksums => "checksums",
            InstallFailure::Install => "install",
            InstallFailure::Version => "version",
            InstallFailure::Daemon => "daemon",
            InstallFailure::Mcp => "mcp",
            InstallFailure::OutputLimit => "output_limit",
            InstallFailure::Deadline => "deadline",
            InstallFailure::Cleanup => "cleanup",
        }
    }

    /// The failure's line, without its LF.
    pub fn message(self) -> String {
        format!("release install failed: {}", self.stage())
    }
}

/// One command of the install guide: its argv, the directory it runs in and its redirections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuideCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub stdin: Option<PathBuf>,
    pub stdout: Option<PathBuf>,
    /// The Windows guide's PowerShell step with no native program: its exact text. The guide
    /// prints it, and the checker runs the same text, so `program` and `args` are not used.
    pub ps: Option<String>,
}

impl GuideCommand {
    fn new(program: &str, args: &[&OsStr], cwd: &Path) -> Self {
        GuideCommand {
            program: program.into(),
            args: args.iter().map(|a| a.to_os_string()).collect(),
            cwd: cwd.to_path_buf(),
            stdin: None,
            stdout: None,
            ps: None,
        }
    }

    /// A PowerShell step: `text` is the whole step.
    fn script(text: String, cwd: &Path) -> Self {
        GuideCommand {
            ps: Some(text),
            ..GuideCommand::new("powershell.exe", &[], cwd)
        }
    }
}

/// The stem of the archive for `version` and `target`: the preview family, the only one PRE's
/// manifest lists.
fn archive_stem(version: &str, target: &str) -> String {
    format!("rdy-{version}-preview-{target}")
}

/// The archive's file name for `version` and `target`.
pub fn archive_name(version: &str, target: &str) -> String {
    format!("{}.tar.gz", archive_stem(version, target))
}

/// The exact `--version` output of a binary from an archive with `stem`: `rdy V (preview)` for
/// a preview stem, `rdy V` for a plain one; `None` for any other stem.
fn version_text_of(stem: &str, version: &str, target: &str) -> Option<String> {
    if stem == format!("rdy-{version}-preview-{target}") {
        Some(format!("rdy {version} (preview)\n"))
    } else if stem == format!("rdy-{version}-{target}") {
        Some(format!("rdy {version}\n"))
    } else {
        None
    }
}

/// Whether `line` is exactly `rdy-release namespaces="rdy-release-v1" <key-type> <base64>`: at
/// most [`SIGNER_LINE_MAX`] bytes with no NUL, CR or LF, an Ed25519 or FIDO Ed25519 key that
/// parses as its stated type, and no comment.
pub fn valid_signer_line(line: &str) -> bool {
    if line.len() > SIGNER_LINE_MAX || line.contains(['\0', '\r', '\n']) {
        return false;
    }
    let Some(key) = line.strip_prefix(SIGNER_PREFIX) else {
        return false;
    };
    let Some((kind, base64)) = key.split_once(' ') else {
        return false;
    };
    if !SIGNER_KEY_TYPES.contains(&kind) || base64.is_empty() || base64.contains(' ') {
        return false;
    }
    match ssh_key::PublicKey::from_openssh(key) {
        Ok(parsed) => {
            parsed.algorithm().as_str() == kind
                && parsed.comment().is_empty()
                && parsed.to_openssh().is_ok_and(|again| again == key)
        }
        Err(_) => false,
    }
}

/// Whether `commit` is 40 lowercase hexadecimal digits.
fn valid_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `path` with `.` removed and each `..` applied to the components before it.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether two of `paths` are the same, or one contains another.
fn overlapping(paths: &[PathBuf]) -> bool {
    paths.iter().enumerate().any(|(i, a)| {
        paths[i + 1..]
            .iter()
            .any(|b| a == b || a.starts_with(b) || b.starts_with(a))
    })
}

/// The guide's commands, by role, for one valid input.
struct Guide {
    stem: String,
    archive: PathBuf,
    signers: GuideCommand,
    verify: GuideCommand,
    checksums: GuideCommand,
    unquarantine: Option<GuideCommand>,
    make_dirs: GuideCommand,
    extract: GuideCommand,
    make_bin: GuideCommand,
    install: GuideCommand,
    version: GuideCommand,
}

impl Guide {
    /// The commands in the order a person runs them.
    fn commands(&self) -> Vec<GuideCommand> {
        let mut all = vec![
            self.signers.clone(),
            self.verify.clone(),
            self.checksums.clone(),
        ];
        all.extend(self.unquarantine.clone());
        all.extend([
            self.make_dirs.clone(),
            self.extract.clone(),
            self.make_bin.clone(),
            self.install.clone(),
            self.version.clone(),
        ]);
        all
    }
}

// ---- the Windows guide ----
//
// The guide for the Windows target is a PowerShell block. Windows paths are text here, on every
// host, so the block can be printed anywhere; only the check runs on Windows. Each token is a
// PowerShell single-quoted literal, which expands nothing. A step with no native program is a
// PowerShell statement ([`GuideCommand::ps`]); the checker runs that same text.

/// Whether `path` is an absolute Windows path in the guide's grammar: a drive letter and a
/// backslash, or `\\server\share`, then named components. At most [`PATH_MAX`] bytes. No `/`,
/// no empty, `.` or `..` component, and none of the characters Windows refuses in a name.
fn valid_windows_path(path: &str) -> bool {
    let rest = if let Some(rest) = path.strip_prefix(r"\\") {
        // the server and the share are components too, and a name follows them
        if rest.split('\\').count() < 3 {
            return false;
        }
        rest
    } else {
        let b = path.as_bytes();
        if !(b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\') {
            return false;
        }
        &path[3..]
    };
    path.len() <= PATH_MAX
        && rest.split('\\').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with([' ', '.'])
                && !part.contains(['<', '>', ':', '"', '/', '|', '?', '*', '\0', '\r', '\n'])
                && !part.chars().any(|c| c.is_control())
        })
}

/// The components of a valid Windows path, lowercase (Windows compares names without case).
fn windows_parts(path: &str) -> Vec<String> {
    path.split('\\')
        .filter(|p| !p.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `name` in the directory `base`.
fn windows_join(base: &str, name: &str) -> String {
    format!("{base}\\{name}")
}

/// The last component of a valid Windows path.
fn windows_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

/// `text` as a PowerShell single-quoted literal. The quote characters that PowerShell reads as
/// one (the four single curly quotes included) are doubled. NUL, CR and LF are refused.
fn ps_literal(text: &str) -> Result<String, String> {
    if text.contains(['\0', '\r', '\n']) {
        return Err(MSG_INVALID_COMMAND.to_string());
    }
    let mut out = String::from("'");
    for c in text.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            out.push(c);
        }
    }
    out.push('\'');
    Ok(out)
}

/// [`ps_literal`] of a token that is text; one that is not UTF-8 (on Windows, one with an
/// unpaired surrogate) is refused.
fn ps_token(token: &OsStr) -> Result<String, String> {
    ps_literal(
        token
            .to_str()
            .ok_or_else(|| MSG_INVALID_COMMAND.to_string())?,
    )
}

/// A PowerShell string expression for text the guide built itself.
fn ps_text(text: &str) -> String {
    ps_literal(text).unwrap_or_else(|_| "''".to_string())
}

/// Checks the grammar of the Windows target's arguments (no filesystem access) and builds its
/// guide.
fn guide_windows(input: &InstallInput) -> Result<Guide, InstallFailure> {
    let v = input.version.as_str();
    let (Some(dl), Some(un), Some(pre)) = (
        input.download.to_str(),
        input.unpack.to_str(),
        input.prefix.to_str(),
    ) else {
        return Err(InstallFailure::Input);
    };
    let parts = [dl, un, pre].map(windows_parts);
    let overlap = parts.iter().enumerate().any(|(i, a)| {
        parts[i + 1..]
            .iter()
            .any(|b| a.starts_with(b) || b.starts_with(a))
    });
    let grammar = valid_version(v)
        && valid_commit(&input.commit)
        && valid_signer_line(&input.signer_line)
        && [dl, un, pre].iter().all(|p| valid_windows_path(p))
        && windows_name(pre) == format!("rdy-{v}")
        && !input.browser
        && !overlap;
    if !grammar {
        return Err(InstallFailure::Input);
    }
    let download = input.download.as_path();
    let stem = archive_stem(v, WINDOWS_TARGET);
    let archive = windows_join(dl, &archive_name(v, WINDOWS_TARGET));
    let (manifest, signers_file) = (
        windows_join(dl, MANIFEST_FILE),
        windows_join(dl, SIGNERS_FILE),
    );
    let bin = windows_join(pre, "bin");
    let installed = windows_join(&bin, binary_name(WINDOWS_TARGET));
    let extracted = windows_join(&windows_join(un, &stem), binary_name(WINDOWS_TARGET));
    let (q_dl, q_manifest) = (ps_text(dl), ps_text(&manifest));
    let signers = GuideCommand::script(
        // opened for shared writing and deleting: the check holds its own handle to this file
        format!(
            "$s = [IO.File]::Open({}, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::Write, [IO.FileShare]'ReadWrite, Delete'); $s.SetLength(0); $b = [Text.Encoding]::UTF8.GetBytes({} + [char]10); $s.Write($b, 0, $b.Length); $s.Close()",
            ps_text(&signers_file),
            ps_text(&input.signer_line)
        ),
        download,
    );
    let mut verify = GuideCommand::new(
        "ssh-keygen.exe",
        &[
            "-Y",
            "verify",
            "-f",
            SIGNERS_FILE,
            "-I",
            RELEASE_PRINCIPAL,
            "-n",
            RELEASE_NAMESPACE,
            "-s",
            SIGNATURE_FILE,
        ]
        .map(OsStr::new),
        download,
    );
    verify.stdin = Some(PathBuf::from(&manifest));
    // `sha256sum -c --ignore-missing` in PowerShell: one `name: OK` line (LF) per archive that
    // is there; a hash that differs, or no archive at all, throws
    let checksums = GuideCommand::script(
        format!(
            "$n = 0; foreach ($line in [IO.File]::ReadAllLines({q_manifest})) {{ $want = $line.Substring(0, 64); $name = $line.Substring(66); $file = [IO.Path]::Combine({q_dl}, $name); if (Test-Path -LiteralPath $file) {{ $got = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLower(); if ($got -cne $want) {{ throw ($name + ': FAILED') }}; [Console]::Out.Write($name + ': OK' + [char]10); $n++ }} }}; if ($n -eq 0) {{ throw 'no archive was checked' }}"
        ),
        download,
    );
    let make_dirs = GuideCommand::script(
        format!(
            "foreach ($dir in @({}, {})) {{ if (Test-Path -LiteralPath $dir) {{ throw ($dir + ' exists') }}; [void][IO.Directory]::CreateDirectory($dir) }}",
            ps_text(un),
            ps_text(pre)
        ),
        download,
    );
    let extract = GuideCommand::new(
        "tar.exe",
        &[
            OsStr::new("-xzf"),
            archive.as_ref(),
            OsStr::new("-C"),
            un.as_ref(),
        ],
        download,
    );
    let make_bin = GuideCommand::script(
        format!("[void][IO.Directory]::CreateDirectory({})", ps_text(&bin)),
        download,
    );
    let install = GuideCommand::script(
        format!(
            "[IO.File]::Copy({}, {})",
            ps_text(&extracted),
            ps_text(&installed)
        ),
        download,
    );
    let version = GuideCommand::new(&installed, &["--version"].map(OsStr::new), download);
    Ok(Guide {
        verify,
        checksums,
        unquarantine: None,
        make_dirs,
        extract,
        make_bin,
        install,
        version,
        signers,
        stem,
        archive: PathBuf::from(archive),
    })
}

/// Checks the grammar of every argument (no filesystem access) and builds the guide.
/// Whether `path` is an absolute POSIX path in the guide's grammar: it starts with `/`, is UTF-8
/// and at most [`PATH_MAX`] bytes, and has no NUL, CR or LF. The guide for a POSIX target is
/// text that a person runs elsewhere, so this holds on every host (on a POSIX host it is
/// [`valid_path`]).
fn valid_posix_path(path: &Path) -> bool {
    path.to_str().is_some_and(|p| {
        p.starts_with('/') && p.len() <= PATH_MAX && !p.contains(['\0', '\r', '\n'])
    })
}

/// `base` and `name` joined with `/`, whatever the host's separator is.
fn posix_join(base: &Path, name: &str) -> PathBuf {
    let base = base.to_string_lossy();
    PathBuf::from(format!("{}/{name}", base.trim_end_matches('/')))
}

fn guide(input: &InstallInput) -> Result<Guide, InstallFailure> {
    if input.target == WINDOWS_TARGET {
        return guide_windows(input);
    }
    let (v, target) = (input.version.as_str(), input.target.as_str());
    let paths = [&input.download, &input.unpack, &input.prefix];
    let grammar = valid_version(v)
        && valid_target(target)
        && valid_commit(&input.commit)
        && valid_signer_line(&input.signer_line)
        && paths.iter().all(|p| valid_posix_path(p))
        && input
            .prefix
            .file_name()
            .is_some_and(|n| n == format!("rdy-{v}").as_str())
        && input.unpack.file_name().is_some()
        && input.download.file_name().is_some()
        && !(input.browser && target != MAC_TARGET)
        && !overlapping(&paths.map(|p| lexical(p)));
    if !grammar {
        return Err(InstallFailure::Input);
    }
    let download = input.download.as_path();
    let stem = archive_stem(v, target);
    let archive = posix_join(download, &archive_name(v, target));
    let (unpack, prefix) = (input.unpack.as_os_str(), input.prefix.as_os_str());
    let mut signers = GuideCommand::new(
        "printf",
        &[r"%s\n".as_ref(), input.signer_line.as_ref()],
        download,
    );
    signers.stdout = Some(posix_join(download, SIGNERS_FILE));
    let mut verify = GuideCommand::new(
        "ssh-keygen",
        &[
            "-Y",
            "verify",
            "-f",
            SIGNERS_FILE,
            "-I",
            RELEASE_PRINCIPAL,
            "-n",
            RELEASE_NAMESPACE,
            "-s",
            SIGNATURE_FILE,
        ]
        .map(OsStr::new),
        download,
    );
    verify.stdin = Some(posix_join(download, MANIFEST_FILE));
    let checksums = if target == MAC_TARGET {
        GuideCommand::new(
            "shasum",
            &["-a", "256", "-c", "--ignore-missing", MANIFEST_FILE].map(OsStr::new),
            download,
        )
    } else {
        GuideCommand::new(
            "sha256sum",
            &["-c", "--ignore-missing", MANIFEST_FILE].map(OsStr::new),
            download,
        )
    };
    let unquarantine = input.browser.then(|| {
        GuideCommand::new(
            "xattr",
            &[
                "-d".as_ref(),
                QUARANTINE_ATTRIBUTE.as_ref(),
                archive.as_os_str(),
            ],
            download,
        )
    });
    let bin = posix_join(&input.prefix, "bin");
    let installed = posix_join(&bin, "rdy");
    let extracted = posix_join(&posix_join(&input.unpack, &stem), "rdy");
    Ok(Guide {
        verify,
        checksums,
        unquarantine,
        make_dirs: GuideCommand::new("mkdir", &[unpack, prefix], download),
        extract: GuideCommand::new(
            "tar",
            &["-xzf".as_ref(), archive.as_os_str(), "-C".as_ref(), unpack],
            download,
        ),
        make_bin: GuideCommand::new("mkdir", &["-p".as_ref(), bin.as_os_str()], download),
        install: GuideCommand::new(
            "install",
            &[
                "-m".as_ref(),
                "755".as_ref(),
                extracted.as_os_str(),
                installed.as_os_str(),
            ],
            download,
        ),
        version: GuideCommand {
            program: installed.clone().into_os_string(),
            args: vec!["--version".into()],
            cwd: download.to_path_buf(),
            stdin: None,
            stdout: None,
            ps: None,
        },
        signers,
        stem,
        archive,
    })
}

/// The guide's commands for `input`, in order, after a grammar check of every argument. It
/// touches no path and starts no process. With `browser`, the quarantine removal comes right
/// before `mkdir`; [`check`] skips it when the archive has no such attribute.
pub fn commands(input: &InstallInput) -> Result<Vec<GuideCommand>, String> {
    guide(input)
        .map(|g| g.commands())
        .map_err(|_| MSG_INSTALL_INVALID_ARGUMENTS.to_string())
}

/// One token, single-quoted for a POSIX shell; [`MSG_INVALID_COMMAND`] for a token that is not
/// UTF-8 or holds NUL, CR or LF.
fn shell_quote(token: &OsStr) -> Result<String, String> {
    match token.to_str() {
        Some(t) if !t.contains(['\0', '\r', '\n']) => Ok(format!("'{}'", t.replace('\'', "'\\''"))),
        _ => Err(MSG_INVALID_COMMAND.to_string()),
    }
}

/// The commands as one shell block: a `cd` to each new working directory, then one line per
/// command with every token single-quoted and its redirections, the lines joined by ` &&` and
/// LF, so a command runs only after every one before it succeeded. A block that removes the
/// quarantine attribute starts with the two browser notes as comments. No final LF.
pub fn render_commands(commands: &[GuideCommand]) -> Result<String, String> {
    let mut lines = Vec::new();
    let mut cwd: Option<&Path> = None;
    let mut browser = false;
    for c in commands {
        browser |= c.program == "xattr" && c.args.first().is_some_and(|a| a == "-d");
        if cwd != Some(c.cwd.as_path()) {
            lines.push(format!("cd {}", shell_quote(c.cwd.as_os_str())?));
            cwd = Some(&c.cwd);
        }
        let mut line = shell_quote(&c.program)?;
        for a in &c.args {
            line.push(' ');
            line.push_str(&shell_quote(a)?);
        }
        if let Some(from) = &c.stdin {
            line.push_str(" < ");
            line.push_str(&shell_quote(from.as_os_str())?);
        }
        if let Some(to) = &c.stdout {
            line.push_str(" > ");
            line.push_str(&shell_quote(to.as_os_str())?);
        }
        lines.push(line);
    }
    let mut out = String::new();
    if browser {
        out.push_str(&format!("# {SAFARI_NOTE}\n# {QUARANTINE_NOTE}\n"));
    }
    out.push_str(&lines.join(" &&\n"));
    Ok(out)
}

/// A token of a command that `cmd.exe` runs for the input redirection: letters, digits, `.`,
/// `_` and `-` only, so no `cmd.exe` quoting is needed.
fn cmd_safe(token: &OsStr) -> Option<&str> {
    token.to_str().filter(|t| {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    })
}

/// The Windows commands as one PowerShell block: `& {` and `}` around lines that run in order.
/// The block stops at the first error: `$ErrorActionPreference` is `Stop`, and a native
/// command that exits with a nonzero code throws. Every token is a single-quoted literal
/// ([`ps_token`]), so a path with `$`, a backtick or a space is not expanded. PowerShell has no
/// input redirection, so a command that reads a file runs under `cmd.exe`, which reads the file by
/// its name in the working directory. The archive tool is the system's `tar.exe`, never one
/// that PATH finds first. [`MSG_INVALID_COMMAND`] for a token that is not UTF-8 (on Windows, one
/// with an unpaired surrogate), or holds NUL, CR or LF, or a command this block cannot render.
pub fn render_powershell(commands: &[GuideCommand]) -> Result<String, String> {
    let bad = || MSG_INVALID_COMMAND.to_string();
    let mut lines = vec!["$ErrorActionPreference = 'Stop'".to_string()];
    let mut cwd: Option<&Path> = None;
    for c in commands {
        if cwd != Some(c.cwd.as_path()) {
            lines.push(format!(
                "Set-Location -LiteralPath {}",
                ps_token(c.cwd.as_os_str())?
            ));
            cwd = Some(&c.cwd);
        }
        if let Some(text) = &c.ps {
            if text.contains(['\r', '\n']) {
                return Err(bad());
            }
            lines.push(text.clone());
            continue;
        }
        if c.stdout.is_some() {
            return Err(bad());
        }
        let line = match &c.stdin {
            None => {
                let mut line = if c.program == "tar.exe" {
                    "& (Join-Path $env:SystemRoot 'System32\\tar.exe')".to_string()
                } else {
                    format!("& {}", ps_token(&c.program)?)
                };
                for a in &c.args {
                    line.push(' ');
                    line.push_str(&ps_token(a)?);
                }
                line
            }
            Some(from) => {
                // the file is in the working directory: `cmd.exe` names it without a path
                let from = from.to_str().ok_or_else(bad)?;
                let here = c.cwd.to_str().ok_or_else(bad)?;
                let name = windows_name(from);
                if from != windows_join(here, name) {
                    return Err(bad());
                }
                let mut words = vec![cmd_safe(&c.program).ok_or_else(bad)?];
                for a in &c.args {
                    words.push(cmd_safe(a).ok_or_else(bad)?);
                }
                let redirected = format!(
                    "{} < {}",
                    words.join(" "),
                    cmd_safe(name.as_ref()).ok_or_else(bad)?
                );
                format!("& 'cmd.exe' '/d' '/c' {}", ps_literal(&redirected)?)
            }
        };
        lines.push(line);
        lines.push("if ($LASTEXITCODE -ne 0) { throw 'a step failed' }".to_string());
    }
    Ok(format!("& {{\n{}\n}}", lines.join("\n")))
}

/// The Windows guide's notes for the version prefix `prefix`, as PowerShell comment lines:
/// PATH, and the agent pipe that rdy needs.
fn windows_notes(prefix: &str) -> Result<String, String> {
    Ok(format!(
        "# Add the version's bin directory to PATH in this session:\n# $env:Path = {} + $env:Path\n# rdy needs SSH_AUTH_SOCK, even when ssh works without it. Set it to the agent pipe:\n# $env:SSH_AUTH_SOCK = {}",
        ps_literal(&format!("{prefix}\\bin;"))?,
        ps_literal(WINDOWS_AGENT_PIPE)?
    ))
}

/// The guide for `input`, as `qa-release-install --print-commands` prints it (without the final
/// LF): the POSIX block of [`render_commands`] for a Unix target; for the Windows target the
/// PowerShell block of [`render_powershell`] and then [`windows_notes`].
pub fn render_guide(input: &InstallInput) -> Result<String, String> {
    let commands = commands(input)?;
    if input.target != WINDOWS_TARGET {
        return render_commands(&commands);
    }
    let prefix = input
        .prefix
        .to_str()
        .ok_or_else(|| MSG_INVALID_COMMAND.to_string())?;
    Ok(format!(
        "{}\n{}",
        render_powershell(&commands)?,
        windows_notes(prefix)?
    ))
}

/// The README's install section for `version` and the public `signer_line`: the macOS, Linux
/// and macOS browser blocks for the example paths, each rendered by [`render_commands`], then
/// the PATH instruction. For the README's ready state only.
pub fn readme_install_section(version: &str, signer_line: &str) -> Result<String, String> {
    let input = |target: &str, browser: bool| InstallInput {
        download: PathBuf::from("/path/to/download"),
        signer_line: signer_line.to_string(),
        version: version.to_string(),
        commit: "0".repeat(40),
        target: target.to_string(),
        unpack: PathBuf::from("/path/to/unpack"),
        prefix: PathBuf::from(format!("/path/to/rdy-{version}")),
        browser,
    };
    let block = |target: &str, browser: bool| -> Result<String, String> {
        Ok(format!(
            "```sh\n{}\n```\n",
            render_commands(&commands(&input(target, browser))?)?
        ))
    };
    Ok(format!(
        "### Install\n\nOn macOS arm64, for an archive downloaded with curl:\n\n{}\nOn Linux amd64:\n\n{}\nOn macOS arm64, for an archive downloaded with a web browser:\n\n{}\nAdd the version's bin directory to PATH in this shell.\nReplace `<version-prefix>` with the version prefix, such as `/path/to/rdy-{version}`:\n\n```sh\nexport PATH=\"<version-prefix>/bin:$PATH\"\n```\n",
        block(TARGETS[0], false)?,
        block(TARGETS[1], false)?,
        block(TARGETS[0], true)?,
    ))
}

/// The README's install section for `version` under the one release key that signs it: the
/// key comes from the authenticated records, never from the README. A thin wrapper around
/// [`readme_install_section`], which the isolated rehearsal keeps using with a bare signer line.
pub fn readme_version_section(keys: &ReleaseKeys, version: &str) -> Result<String, String> {
    let signer = keys.signer_for(version).map_err(|e| e.to_string())?;
    readme_install_section(version, &signer.signer_line)
}

/// The README's ready state for `version`: the selected release key with its fingerprint, the
/// trust list of every key and range (historical and machine epochs, their statements and the
/// root verification commands), then the install blocks for the selected key. All of it comes
/// from the authenticated records. `notes_url` is the release notes link.
pub fn readme_ready_section(
    keys: &ReleaseKeys,
    version: &str,
    notes_url: &str,
) -> Result<String, String> {
    let signer = keys.signer_for(version).map_err(|e| e.to_string())?;
    Ok(format!(
        "### Release signing key\n\n```text\n{}\n```\n\nFingerprint: {}.\n\nCompare this line and fingerprint with the ones in the [GitHub release notes]({notes_url}). Use the key only when both match.\n\n{}\n{}",
        signer.signer_line,
        signer.fingerprint,
        keys.trust_section().map_err(|e| e.to_string())?,
        readme_version_section(keys, version)?,
    ))
}

/// What [`check`] reads besides its input; tests swap parts of it.
struct InstallSetup<'a> {
    /// This machine's release target.
    native_target: &'a dyn Fn() -> io::Result<&'static str>,
    /// This machine's OS line for the report.
    os: &'a dyn Fn() -> io::Result<String>,
    /// The process that runs one guide command (tests stand in for a tool here).
    process: &'a dyn Fn(&GuideCommand) -> Command,
    /// Told each guide command the runner runs, right before it starts (a test hook).
    ran: &'a dyn Fn(&GuideCommand),
    /// Where the installed check's scratch directory is made.
    scratch: PathBuf,
    /// Each child's output cap, and the MCP session's.
    max_output: usize,
    /// Each child's hang guard.
    deadline: std::time::Duration,
}

impl InstallSetup<'static> {
    /// The real check: this machine's target and OS, the real tools, the system temp dir.
    fn release() -> Self {
        InstallSetup {
            native_target: &platform::release_target,
            os: &platform::release_os,
            process: &guide_process,
            ran: &nothing_ran,
            scratch: std::env::temp_dir(),
            max_output: RELEASE_CHILD_OUTPUT_MAX,
            deadline: RELEASE_CHILD_DEADLINE,
        }
    }
}

/// The release check's [`InstallSetup::ran`]: nothing is told.
fn nothing_ran(_: &GuideCommand) {}

/// The process of a guide command: its program and arguments. Where it starts and its
/// redirections are the runner's to give ([`run_guide`]).
fn guide_process(c: &GuideCommand) -> Command {
    if let Some(text) = &c.ps {
        // the same text the block prints, as the encoded command that PowerShell runs: it needs
        // no argument quoting
        let mut cmd = Command::new(platform::powershell_program());
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded_command(&format!("$ErrorActionPreference = 'Stop'\n{text}")),
        ]);
        return cmd;
    }
    let program = if c.program == "tar.exe" {
        platform::tar_program().into_os_string()
    } else {
        c.program.clone()
    };
    let mut cmd = Command::new(program);
    cmd.args(&c.args);
    cmd
}

/// `script` as the base64 of its UTF-16LE bytes: PowerShell's `-EncodedCommand`.
fn encoded_command(script: &str) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(DIGITS[(word >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A failed child run's class: output overflow and the deadline are global; any other failure
/// belongs to `stage`.
fn run_failure(e: &io::Error, stage: InstallFailure) -> InstallFailure {
    match e.kind() {
        io::ErrorKind::InvalidData => InstallFailure::OutputLimit,
        io::ErrorKind::TimedOut => InstallFailure::Deadline,
        _ => stage,
    }
}

/// Runs `cmd` through the bounded runner; a nonzero exit is a failure of `stage`.
fn run_ok(
    setup: &InstallSetup,
    cmd: Command,
    stdin: Option<File>,
    stage: InstallFailure,
) -> Result<Output, InstallFailure> {
    let out = platform::run_bounded(cmd, stdin, setup.max_output, setup.deadline)
        .map_err(|e| run_failure(&e, stage))?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(stage)
    }
}

/// Whether `path`, its links followed, names the open directory `dir` now (the same device
/// and inode).
fn names_dir(path: &Path, dir: &File) -> bool {
    let Ok(now) = fs::canonicalize(path) else {
        return false;
    };
    match (now.parent(), now.file_name()) {
        (Some(parent), Some(name)) => platform::open_dir(parent)
            .and_then(|parent| platform::holds_file(&parent, name, dir))
            .unwrap_or(false),
        _ => false,
    }
}

/// The directory and file the install made in PREFIX, open once the check has found them.
#[derive(Default)]
struct Installed {
    /// `PREFIX/bin`.
    bin: Option<File>,
    /// `PREFIX/bin/rdy`.
    rdy: Option<File>,
}

/// Whether the paths the guide's commands name still name what the check checked and made:
/// DOWNLOAD the checked directory, with the manifest, the signature, every present archive and
/// the signer file the same entries; the parents of UNPACK and PREFIX the checked parents;
/// UNPACK and PREFIX, once made, this run's directories; and `PREFIX/bin` and
/// `PREFIX/bin/rdy`, once found, the install's.
fn intact(input: &InstallInput, dl: &Download, owned: &Owned, installed: &Installed) -> bool {
    let holds = |name: &str, file: &File| {
        platform::holds_file(&dl.dir, name.as_ref(), file).unwrap_or(false)
    };
    let in_parent = |output: &Path, parent: &File| {
        output
            .parent()
            .is_some_and(|p| platform::opens_dir(p, parent))
    };
    names_dir(&input.download, &dl.dir)
        && in_parent(&input.unpack, &dl.unpack_parent)
        && in_parent(&input.prefix, &dl.prefix_parent)
        && holds(MANIFEST_FILE, &dl.manifest.file)
        && holds(SIGNATURE_FILE, &dl.signature.file)
        && dl.archives.iter().all(|(name, file)| holds(name, file))
        && owned
            .signers
            .as_ref()
            .is_none_or(|f| holds(SIGNERS_FILE, f))
        && owned
            .unpack
            .as_ref()
            .is_none_or(|u| names_dir(&input.unpack, &u.dir))
        && owned
            .prefix
            .as_ref()
            .is_none_or(|p| names_dir(&input.prefix, &p.dir))
        && installed
            .bin
            .as_ref()
            .is_none_or(|bin| names_dir(&input.prefix.join("bin"), bin))
        && installed.rdy.as_ref().is_none_or(|rdy| {
            installed.bin.as_ref().is_some_and(|bin| {
                platform::holds_file(bin, OsStr::new(binary_name(&input.target)), rdy)
                    .unwrap_or(false)
            })
        })
}

/// Runs the guide command `c` as it is, with `env` set: its program and arguments, started in
/// its working directory, DOWNLOAD, through DOWNLOAD's checked handle. Its stdin, the manifest,
/// is the checked manifest's handle; its stdout, the signer file, is captured for the caller to
/// write to the signer file this check created. Right before it starts, and again after it
/// ends, every path the guide names must still name what was checked ([`intact`]); otherwise
/// it fails as `stage`, like a nonzero exit (see [`run_ok`]). [`InstallSetup::ran`] is told
/// `c` right before it starts.
#[allow(clippy::too_many_arguments)]
fn run_guide(
    setup: &InstallSetup,
    input: &InstallInput,
    c: &GuideCommand,
    dl: &mut Download,
    owned: &Owned,
    installed: &Installed,
    env: &[(&str, &str)],
    stage: InstallFailure,
) -> Result<Output, InstallFailure> {
    let stdin = match &c.stdin {
        None => None,
        Some(from) if *from == input.download.join(MANIFEST_FILE) => {
            // a new handle of the checked manifest, read from its start: `intact` below
            // confirms that the path still names the checked file. (A handle read and rewound
            // here makes the Windows ssh-keygen hang when it reads it as its input.)
            Some(
                rdy_store::pio::open_read_at(&dl.dir, OsStr::new(MANIFEST_FILE))
                    .map_err(|_| stage)?,
            )
        }
        Some(_) => return Err(stage),
    };
    let stdout_ok = c
        .stdout
        .as_ref()
        .is_none_or(|to| *to == input.download.join(SIGNERS_FILE));
    if c.cwd != input.download || !stdout_ok || !intact(input, dl, owned, installed) {
        return Err(stage);
    }
    let mut cmd = (setup.process)(c);
    cmd.envs(env.iter().copied());
    let _start = platform::start_in_dir(&mut cmd, &dl.dir).map_err(|_| stage)?;
    (setup.ran)(c);
    let out = run_ok(setup, cmd, stdin, stage)?;
    if intact(input, dl, owned, installed) {
        Ok(out)
    } else {
        Err(stage)
    }
}

/// A file in DOWNLOAD, open, and the bytes read through that handle when it was checked.
struct Checked {
    file: File,
    bytes: Vec<u8>,
}

/// The checked download directory and the parents of the outputs, each open. The guide's
/// commands name these files by their paths; before and after each of them the check confirms
/// that the paths still name these handles ([`intact`]), and it reads, validates and cleans up
/// through the handles.
struct Download {
    /// DOWNLOAD itself.
    dir: File,
    manifest: Checked,
    signature: Checked,
    /// The expected archives present in DOWNLOAD, in manifest order: each name and its handle.
    archives: Vec<(String, File)>,
    /// The directories UNPACK and PREFIX are made in.
    unpack_parent: File,
    prefix_parent: File,
}

/// A directory this check made in an open parent: the scratch directory, made exclusively, or
/// UNPACK or PREFIX, which the guide's `mkdir` made after the check found both absent, claimed
/// only when that mkdir succeeded. It holds its parent's handle, its name there and its own
/// handle. Only such a directory is ever removed, and only through these handles; no path is
/// resolved again to choose what to delete.
struct MadeDir {
    parent: File,
    name: OsString,
    dir: File,
}

impl MadeDir {
    /// Makes the new directory `name` in `parent` (private to this user). A directory already
    /// at `name`, which another process may have made, fails with `AlreadyExists` and is never
    /// this check's.
    fn make(parent: &File, name: &OsStr) -> io::Result<MadeDir> {
        let parent = parent.try_clone()?;
        let dir = platform::create_private_dir_at(&parent, name)?;
        Ok(MadeDir {
            parent,
            name: name.to_os_string(),
            dir,
        })
    }

    /// The directory `name` in `parent`, which a guide command has just made: a directory (not
    /// a link) that this user owns, and empty, as a directory that mkdir has just made is. `None`
    /// for anything else: a directory with something in it is another process's. The check
    /// starts no process: it reads the directory in this process, through its handle, so the
    /// guide's commands stay the only processes between its mkdir and tar, and a listing that
    /// fails is "not empty".
    fn claim(parent: &File, name: &OsStr) -> Option<MadeDir> {
        let dir = rdy_store::pio::open_dir_at(parent, name).ok()?;
        let meta = dir.metadata().ok()?;
        let who = rdy_store::pio::privacy(&dir).ok()?;
        // On Windows an elevated account owns what it makes as the Administrators group, so a
        // directory that no other account may use counts as this user's, as it does on Unix.
        let mine = who.mine || (cfg!(windows) && who.owner_only);
        (meta.is_dir() && mine && platform::is_empty_dir(&dir).unwrap_or(false)).then_some(())?;
        Some(MadeDir {
            parent: parent.try_clone().ok()?,
            name: name.to_os_string(),
            dir,
        })
    }

    /// Whether the parent's entry is still this directory.
    fn in_place(&self) -> bool {
        platform::holds_file(&self.parent, &self.name, &self.dir).unwrap_or(false)
    }

    /// Removes what is in the directory, in this process and through its own handle (every
    /// entry is opened and removed relative to its parent's handle, and a link is removed,
    /// never followed), then the directory from its parent, only while the parent's entry is
    /// still this directory. False when a step fails or the entry names another file now: the
    /// directory is then kept.
    fn remove(self) -> bool {
        if !self.in_place() {
            return false;
        }
        let emptied = empty_dir(&self.dir, 0).is_ok();
        // rmdir removes only an empty directory: a directory swapped in since is not removed
        emptied && self.in_place() && platform::remove_dir_at(&self.parent, &self.name).is_ok()
    }
}

/// The deepest directory [`empty_dir`] enters.
const REMOVE_DEPTH_MAX: usize = 64;

/// Removes every entry of the open directory `dir`, subdirectories first. Each entry is read
/// by kind without following it: a directory is opened relative to `dir` and emptied, then
/// removed; any other entry, a link included, is removed itself.
fn empty_dir(dir: &File, depth: usize) -> io::Result<()> {
    use rdy_store::pio::EntryKind;
    if depth > REMOVE_DEPTH_MAX {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut names: Vec<OsString> = Vec::new();
    rdy_store::pio::for_each_entry::<io::Error>(dir, |name| {
        names.push(name.to_os_string());
        Ok(())
    })?;
    for name in names {
        match rdy_store::pio::entry_kind_at(dir, &name)? {
            EntryKind::Dir => {
                let inner = rdy_store::pio::open_dir_at(dir, &name)?;
                empty_dir(&inner, depth + 1)?;
                drop(inner);
                platform::remove_dir_at(dir, &name)?;
            }
            _ => platform::remove_file_at(dir, &name)?,
        }
    }
    Ok(())
}

/// The installed check's scratch directory.
struct Smoke {
    dir: MadeDir,
    /// False while a daemon may run for its project: it is kept then.
    stopped: bool,
}

/// What one check made, which only it may remove.
struct Owned {
    /// DOWNLOAD, open, and the signer file this check created in it.
    download: File,
    signers: Option<File>,
    unpack: Option<MadeDir>,
    prefix: Option<MadeDir>,
    smoke: Option<Smoke>,
}

impl Owned {
    /// Removes the signer file on every exit. After a successful (or no) daemon stop, removes
    /// UNPACK and the scratch directory, and PREFIX too on failure. When the daemon stop
    /// failed, the scratch directory and PREFIX stay for a later stop. A failed removal, or a
    /// kept directory, is the result.
    fn finish(
        self,
        made: Result<InstalledRelease, InstallFailure>,
    ) -> Result<InstalledRelease, InstallFailure> {
        let mut clean = true;
        if let Some(file) = &self.signers {
            let name = OsStr::new(SIGNERS_FILE);
            clean &= platform::holds_file(&self.download, name, file).unwrap_or(false)
                && platform::remove_file_at(&self.download, name).is_ok();
        }
        let live = self.smoke.as_ref().is_some_and(|s| !s.stopped);
        if let Some(unpack) = self.unpack {
            clean &= unpack.remove();
        }
        if let Some(smoke) = self.smoke {
            if live {
                // kept: the project's daemon may still run (handles never remove on drop)
                clean = false;
            } else {
                clean &= smoke.dir.remove();
            }
        }
        if let Some(prefix) = self.prefix
            && made.is_err()
            && !live
        {
            clean &= prefix.remove();
        }
        if clean {
            made
        } else {
            Err(InstallFailure::Cleanup)
        }
    }
}

/// Opens the entry `name` of the open directory `dir`, never through a link, as a regular file
/// of 1..=`max` bytes; `None` for anything else.
fn open_regular_at(dir: &File, name: &str, max: u64) -> Option<File> {
    let file = rdy_store::pio::open_read_at(dir, name.as_ref()).ok()?;
    let meta = file.metadata().ok()?;
    (meta.is_file() && (1..=max).contains(&meta.len())).then_some(file)
}

/// Opens and reads the entry `name` of DOWNLOAD (see [`open_regular_at`]): all of its bytes,
/// read through the handle.
fn checked_at(dir: &File, name: &str, max: u64) -> Result<Checked, InstallFailure> {
    let mut file = open_regular_at(dir, name, max).ok_or(InstallFailure::Input)?;
    let len = file.metadata().map_err(|_| InstallFailure::Input)?.len();
    let bytes = read_from_start(&mut file, max).map_err(|_| InstallFailure::Input)?;
    if bytes.len() as u64 != len {
        return Err(InstallFailure::Input);
    }
    Ok(Checked { file, bytes })
}

/// Reads `file` from its start, at most `max + 1` bytes.
fn read_from_start(file: &mut File, max: u64) -> io::Result<Vec<u8>> {
    use std::io::{Read, Seek};
    file.seek(io::SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    (&*file)
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Whether the entry `name` of the open directory `dir` is still `file` and holds exactly
/// `bytes`, read through the handle.
fn unchanged(dir: &File, name: &str, file: &mut File, bytes: &[u8]) -> bool {
    platform::holds_file(dir, name.as_ref(), file).unwrap_or(false)
        && read_from_start(file, bytes.len() as u64).is_ok_and(|now| now == bytes)
}

/// Whether nothing stands at `path`, not even a dangling link.
fn absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

/// Whether nothing stands at the entry `name` of the open directory `dir`, not even a link.
fn absent_at(dir: &File, name: &OsStr) -> bool {
    matches!(rdy_store::pio::open_read_at(dir, name), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

/// `path` with its existing parent resolved; `None` when the parent is not a directory.
fn canonical_entry(path: &Path) -> Option<PathBuf> {
    let (parent, name) = (fs::canonicalize(path.parent()?).ok()?, path.file_name()?);
    fs::metadata(&parent)
        .ok()?
        .is_dir()
        .then(|| parent.join(name))
}

/// The filesystem checks, in order: DOWNLOAD (opened once) is a directory of this user's that
/// no one else may write; UNPACK and PREFIX are absent with directories as parents; the three
/// do not overlap once resolved; the signer file is absent; the manifest, the signature and the
/// selected archive (and the other archive, when present) are regular files within their caps,
/// each opened through DOWNLOAD's handle. Then it creates the signer file there. Every failure
/// is [`InstallFailure::Input`].
fn inspect(input: &InstallInput) -> Result<(Download, Owned), InstallFailure> {
    use InstallFailure::Input;
    let dir = platform::open_dir(&input.download).map_err(|_| Input)?;
    let meta = dir.metadata().map_err(|_| Input)?;
    let who = rdy_store::pio::privacy(&dir).map_err(|_| Input)?;
    if !meta.is_dir() || !who.mine || who.shared_writable {
        return Err(Input);
    }
    if !(absent(&input.unpack) && absent(&input.prefix)) {
        return Err(Input);
    }
    let resolved = [
        fs::canonicalize(&input.download).map_err(|_| Input)?,
        canonical_entry(&input.unpack).ok_or(Input)?,
        canonical_entry(&input.prefix).ok_or(Input)?,
    ];
    if overlapping(&resolved) {
        return Err(Input);
    }
    // the resolved parents, open, with nothing at the outputs' names in them
    let open_parent = |p: &Path| -> Result<File, InstallFailure> {
        let (parent, name) = (p.parent().ok_or(Input)?, p.file_name().ok_or(Input)?);
        let parent = platform::open_dir(parent).map_err(|_| Input)?;
        if absent_at(&parent, name) {
            Ok(parent)
        } else {
            Err(Input)
        }
    };
    let unpack_parent = open_parent(&resolved[1])?;
    let prefix_parent = open_parent(&resolved[2])?;
    if !absent_at(&dir, OsStr::new(SIGNERS_FILE)) {
        return Err(Input);
    }
    let manifest = checked_at(&dir, MANIFEST_FILE, MANIFEST_MAX)?;
    let signature = checked_at(&dir, SIGNATURE_FILE, SIGNATURE_MAX)?;
    // the selected archive is never skipped: its absence fails here
    let mut archives = Vec::new();
    for target in MANIFEST_TARGETS {
        let name = archive_name(&input.version, target);
        if target != input.target && absent_at(&dir, name.as_ref()) {
            continue;
        }
        let file = open_regular_at(&dir, &name, ARCHIVE_MAX).ok_or(Input)?;
        archives.push((name, file));
    }
    let signers = platform::create_new_at(&dir, OsStr::new(SIGNERS_FILE)).map_err(|_| Input)?;
    let owned = Owned {
        download: dir.try_clone().map_err(|_| Input)?,
        signers: Some(signers),
        unpack: None,
        prefix: None,
        smoke: None,
    };
    Ok((
        Download {
            dir,
            manifest,
            signature,
            archives,
            unpack_parent,
            prefix_parent,
        },
        owned,
    ))
}

/// Checks the verified manifest: exactly the archive lines for `version`, sorted by name: the
/// macOS and Linux lines, or those two and the Windows line between them. Each line is 64
/// lowercase hexadecimal digits, two spaces, the file name and LF. The Windows `target` needs
/// the three-line manifest, as its own archive's hash must be in it.
fn check_manifest(bytes: &[u8], version: &str, target: &str) -> Result<(), InstallFailure> {
    let bad = InstallFailure::Checksums;
    let text = std::str::from_utf8(bytes).map_err(|_| bad)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let listed: &[&str] = match lines.len() {
        2 if target != WINDOWS_TARGET => &TARGETS,
        3 => &MANIFEST_TARGETS,
        _ => return Err(bad),
    };
    for (line, target) in lines.iter().zip(listed) {
        let body = line.strip_suffix('\n').ok_or(bad)?;
        let (hash, rest) = body.split_at_checked(64).ok_or(bad)?;
        let hex = hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !hex || rest != format!("  {}", archive_name(version, target)) {
            return Err(bad);
        }
    }
    Ok(())
}

/// Whether `out` (LF-separated names, as `xattr <file>` prints them) names the quarantine
/// attribute.
fn names_quarantine(out: &[u8]) -> bool {
    out.split(|&b| b == b'\n')
        .any(|name| name == QUARANTINE_ATTRIBUTE.as_bytes())
}

/// Checks `input` and installs its release: signature, checksums, install, version, then the
/// installed CLI and MCP paths in a scratch project. On success PREFIX holds `bin/rdy` and the
/// report has six passed checks; see the module's guide for the order and the failures.
pub fn check(input: &InstallInput) -> Result<InstalledRelease, InstallFailure> {
    check_with(input, &InstallSetup::release())
}

fn check_with(
    input: &InstallInput,
    setup: &InstallSetup,
) -> Result<InstalledRelease, InstallFailure> {
    let guide = guide(input)?;
    match (setup.native_target)() {
        Ok(native) if native == input.target => {}
        _ => return Err(InstallFailure::Input),
    }
    let os = (setup.os)().map_err(|e| run_failure(&e, InstallFailure::Input))?;
    let (mut download, mut owned) = inspect(input)?;
    let made = install_stages(input, &guide, &mut download, setup, &mut owned).map(|()| {
        InstalledRelease {
            binary: input.prefix.join("bin").join(binary_name(&input.target)),
            version: input.version.clone(),
            commit: input.commit.clone(),
            target: input.target.clone(),
            os,
            checks: CHECK_NAMES
                .iter()
                .map(|name| ReleaseCheck {
                    name: name.to_string(),
                    passed: true,
                })
                .collect(),
            skipped: 0,
        }
    });
    owned.finish(made)
}

/// The stages after the input checks, in order.
///
/// The guide's steps are the guide's commands, run in order by [`run_guide`], each as it is:
/// the same argv, DOWNLOAD as the working directory and the product by its absolute installed
/// path. Between them the check validates what each command left: the signer file's bytes,
/// the manifest's grammar, the checksum report, the extracted binary and the version text.
/// The browser path probes the verified archive for the quarantine attribute right before the
/// guide's removal command, and skips that command when the attribute is absent. After the
/// guide's last command come the installed check's own steps ([`smoke`]), then the cleanup.
fn install_stages(
    input: &InstallInput,
    guide: &Guide,
    dl: &mut Download,
    setup: &InstallSetup,
    owned: &mut Owned,
) -> Result<(), InstallFailure> {
    use InstallFailure::*;
    use std::io::Write;
    let mut installed = Installed::default();

    // Signature: the signer file holds printf's bytes; OpenSSH verifies the raw manifest.
    let printed = run_guide(
        setup,
        input,
        &guide.signers,
        dl,
        owned,
        &installed,
        &[],
        Signature,
    )?;
    let line = format!("{}\n", input.signer_line);
    let signers = owned.signers.as_mut().ok_or(Signature)?;
    if guide.signers.ps.is_some() {
        // the step wrote the signer file itself, as the Windows guide does: its bytes are the
        // signer line and LF, read back through the handle this check made
        let wrote = read_from_start(signers, SIGNER_LINE_MAX as u64 + 2).map_err(|_| Signature)?;
        if wrote != line.as_bytes() {
            return Err(Signature);
        }
    } else {
        if printed.stdout != line.as_bytes() {
            return Err(Signature);
        }
        signers
            .write_all(&printed.stdout)
            .and_then(|()| signers.sync_all())
            .map_err(|_| Signature)?;
    }
    // From here the check holds a read handle of the signer file, the same file: a handle open
    // for writing makes the Windows ssh-keygen hang when it reads the file.
    let same = platform::holds_file(&dl.dir, OsStr::new(SIGNERS_FILE), signers).unwrap_or(false);
    let reader = rdy_store::pio::open_read_at(&dl.dir, OsStr::new(SIGNERS_FILE));
    match (same, reader) {
        (true, Ok(reader)) => owned.signers = Some(reader),
        _ => return Err(Signature),
    }
    run_guide(
        setup,
        input,
        &guide.verify,
        dl,
        owned,
        &installed,
        &[],
        Signature,
    )?;
    // what OpenSSH read is what was checked: the same bytes
    let signers = owned.signers.as_mut().ok_or(Signature)?;
    let verified = unchanged(&dl.dir, SIGNERS_FILE, signers, line.as_bytes())
        && unchanged(
            &dl.dir,
            MANIFEST_FILE,
            &mut dl.manifest.file,
            &dl.manifest.bytes,
        )
        && unchanged(
            &dl.dir,
            SIGNATURE_FILE,
            &mut dl.signature.file,
            &dl.signature.bytes,
        );
    if !verified {
        return Err(Signature);
    }

    // Checksums: the manifest's grammar, then the native tool, one OK line per present archive.
    check_manifest(&dl.manifest.bytes, &input.version, &input.target)?;
    let out = run_guide(
        setup,
        input,
        &guide.checksums,
        dl,
        owned,
        &installed,
        &[("LC_ALL", "C")],
        Checksums,
    )?;
    let checked = dl.archives.iter().all(|(name, _)| {
        out.stdout
            .split(|&b| b == b'\n')
            .any(|line| line == format!("{name}: OK").as_bytes())
    });
    let hashed = unchanged(
        &dl.dir,
        MANIFEST_FILE,
        &mut dl.manifest.file,
        &dl.manifest.bytes,
    );
    if dl.archives.is_empty() || !checked || !hashed {
        return Err(Checksums);
    }

    // Install: the quarantine attribute (browser only), then the guide's directories,
    // extraction, the extracted binary's check and the install.
    if let Some(remove) = &guide.unquarantine {
        let probe = GuideCommand::new("xattr", &[guide.archive.as_os_str()], &input.download);
        let names = run_guide(setup, input, &probe, dl, owned, &installed, &[], Install)?;
        if names_quarantine(&names.stdout) {
            run_guide(setup, input, remove, dl, owned, &installed, &[], Install)?;
        }
    }
    let name_of = |p: &Path| p.file_name().map(OsStr::to_os_string).ok_or(Install);
    let (unpack_name, prefix_name) = (name_of(&input.unpack)?, name_of(&input.prefix)?);
    // a directory that stands there now was made by another process: the guide's mkdir would
    // fail, and it is never this run's
    if !(absent_at(&dl.unpack_parent, &unpack_name) && absent_at(&dl.prefix_parent, &prefix_name)) {
        return Err(Install);
    }
    // a failed mkdir does not say which directory it made: neither is claimed then
    run_guide(
        setup,
        input,
        &guide.make_dirs,
        dl,
        owned,
        &installed,
        &[],
        Install,
    )?;
    // the mkdir's own check found both parents still the checked ones (`run_guide`); each
    // directory is claimed only when both are found as that mkdir leaves them, else neither is
    let claimed = (
        MadeDir::claim(&dl.unpack_parent, &unpack_name),
        MadeDir::claim(&dl.prefix_parent, &prefix_name),
    );
    let (Some(unpack), Some(prefix)) = claimed else {
        return Err(Install);
    };
    (owned.unpack, owned.prefix) = (Some(unpack), Some(prefix));
    if !intact(input, dl, owned, &installed) {
        return Err(Install);
    }
    run_guide(
        setup,
        input,
        &guide.extract,
        dl,
        owned,
        &installed,
        &[],
        Install,
    )?;
    run_guide(
        setup,
        input,
        &guide.make_bin,
        dl,
        owned,
        &installed,
        &[],
        Install,
    )?;
    let prefix = owned.prefix.as_ref().ok_or(Install)?;
    installed.bin =
        Some(rdy_store::pio::open_dir_at(&prefix.dir, OsStr::new("bin")).map_err(|_| Install)?);
    // the extracted binary: a regular file, not a link, within its bound, before install
    let unpack = owned.unpack.as_ref().ok_or(Install)?;
    let exe = binary_name(&input.target);
    let stem =
        rdy_store::pio::open_dir_at(&unpack.dir, guide.stem.as_ref()).map_err(|_| Install)?;
    let extracted = open_regular_at(&stem, exe, BINARY_MAX).ok_or(Install)?;
    let len = extracted.metadata().map_err(|_| Install)?.len();
    if !intact(input, dl, owned, &installed) {
        return Err(Install);
    }
    run_guide(
        setup,
        input,
        &guide.install,
        dl,
        owned,
        &installed,
        &[],
        Install,
    )?;
    let bin = installed.bin.as_ref().ok_or(Install)?;
    installed.rdy = Some(
        open_regular_at(bin, exe, BINARY_MAX)
            .filter(|f| f.metadata().is_ok_and(|m| m.len() == len))
            .ok_or(Install)?,
    );

    // Version: the exact text the archive's stem calls for, from the absolute installed path.
    let want = version_text_of(&guide.stem, &input.version, &input.target).ok_or(Version)?;
    let out = run_guide(
        setup,
        input,
        &guide.version,
        dl,
        owned,
        &installed,
        &[],
        Version,
    )?;
    if out.stdout != want.as_bytes() || !out.stderr.is_empty() {
        return Err(Version);
    }

    // The installed check's own steps, after the guide's last command.
    smoke(Path::new(&guide.version.program), setup, owned)
}

/// The class of a failed bounded MCP session.
fn mcp_failure(f: crate::mcp::McpFailure) -> InstallFailure {
    use crate::mcp::McpFailure;
    match f {
        McpFailure::Io | McpFailure::Protocol => InstallFailure::Mcp,
        McpFailure::OutputLimit => InstallFailure::OutputLimit,
        McpFailure::Deadline => InstallFailure::Deadline,
        McpFailure::Cleanup => InstallFailure::Cleanup,
    }
}

/// Makes the installed check's new scratch directory in the open directory `parent`, under a
/// name of this process's that nothing else holds: the name and the directory.
fn scratch_dir(parent: &File) -> io::Result<(String, MadeDir)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..100 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("rdy-release-check-{}-{n}", std::process::id());
        match MadeDir::make(parent, name.as_ref()) {
            Ok(dir) => return Ok((name, dir)),
            // a name left by an earlier process with this ID: try the next
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// Writes the isolated config `config/rdy/config.yaml` in the open directory `root`, through
/// handles.
fn write_config(root: &File) -> io::Result<()> {
    use std::io::Write;
    let config = platform::create_private_dir_at(root, "config".as_ref())?;
    let rdy = platform::create_private_dir_at(&config, "rdy".as_ref())?;
    let mut file = platform::create_new_at(&rdy, "config.yaml".as_ref())?;
    file.write_all(SMOKE_CONFIG.as_bytes())?;
    file.sync_all()
}

/// The installed CLI and MCP paths: a new project in a scratch directory, its daemon-backed
/// status, then an MCP session through the same daemon, then the daemon stop. Every product
/// command is the installed binary `rdy`, as a human with an isolated config, started in the
/// scratch directory's handle. The MCP server must exit 0 once its input closes.
fn smoke(rdy: &Path, setup: &InstallSetup, owned: &mut Owned) -> Result<(), InstallFailure> {
    use crate::mcp::{McpClient, Reply};
    use InstallFailure::*;
    let scratch = platform::open_dir(&setup.scratch).map_err(|_| Daemon)?;
    let (name, dir) = scratch_dir(&scratch).map_err(|_| Daemon)?;
    let smoke = owned.smoke.insert(Smoke { dir, stopped: true });
    write_config(&smoke.dir.dir).map_err(|_| Daemon)?;
    let root = setup.scratch.join(name);
    let config = root.join("config");
    let project = root.join("project");
    let bin = rdy.parent().ok_or(Daemon)?;
    let path =
        std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(platform::smoke_path_dirs()))
            .map_err(|_| Daemon)?;
    let start = smoke.dir.dir.try_clone().map_err(|_| Daemon)?;
    // the guard of each product command, held until the smoke ends: each child is waited by then
    let started = std::cell::RefCell::new(Vec::new());
    let product = |args: &[&OsStr], stage: InstallFailure| -> Result<Command, InstallFailure> {
        let mut cmd = Command::new(rdy);
        cmd.args(args)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .env("PATH", &path)
            .env("XDG_CONFIG_HOME", &config)
            // ain: the scratch root is the home, so no real meta project is read
            .env("HOME", &root)
            .env("USERPROFILE", &root)
            .env("APPDATA", &config)
            .env("LOCALAPPDATA", &root);
        started
            .borrow_mut()
            .push(platform::start_in_dir(&mut cmd, &start).map_err(|_| stage)?);
        Ok(cmd)
    };
    let p = project.as_os_str();
    run_ok(setup, product(&["init".as_ref(), p], Daemon)?, None, Daemon)?;
    smoke.stopped = false;

    let checked = (|| {
        let status = run_ok(
            setup,
            product(
                &[
                    "--project".as_ref(),
                    p,
                    "--json".as_ref(),
                    "status".as_ref(),
                ],
                Daemon,
            )?,
            None,
            Daemon,
        )?;
        serde_json::from_slice::<serde_json::Value>(&status.stdout).map_err(|_| Daemon)?;
        let mut client = McpClient::try_launch_bounded(
            product(&["mcp".as_ref(), "--project".as_ref(), p], Mcp)?,
            setup.max_output,
        )
        .map_err(mcp_failure)?;
        let session = client
            .try_call(
                "rdy_session",
                serde_json::json!({"actor": "agent:release-check", "run": "preview-check", "role": "reader"}),
            )
            .map_err(mcp_failure)?;
        let overview = match session {
            Reply::Ok(_) => client
                .try_call("rdy_overview", serde_json::json!({}))
                .map_err(mcp_failure)?,
            other => other,
        };
        let answered = matches!(&overview, Reply::Ok(v) if v.is_object());
        let finished = client
            .try_finish(crate::mcp::DEADLINE)
            .map_err(mcp_failure)?;
        // a server that answered but then exited nonzero, or by a signal, failed
        if answered && finished.code == Some(0) {
            Ok(())
        } else {
            Err(Mcp)
        }
    })();

    let stop = product(
        &["--project".as_ref(), p, "serve".as_ref(), "--stop".as_ref()],
        Cleanup,
    )
    .and_then(|cmd| run_ok(setup, cmd, None, Cleanup));
    if stop.is_err() {
        return Err(Cleanup);
    }
    smoke.stopped = true;
    checked
}

/// Whether the tools a release install test needs start here: `ssh-keygen`, `tar`, `install`
/// and the checksum tool of a Unix `target`; `ssh-keygen`, the system `tar.exe` and PowerShell
/// for the Windows target.
pub fn install_tools_available(target: &str) -> bool {
    let tools: Vec<OsString> = if target == WINDOWS_TARGET {
        vec![
            "ssh-keygen".into(),
            platform::tar_program().into_os_string(),
            platform::powershell_program().into_os_string(),
        ]
    } else {
        let sums = if target == MAC_TARGET {
            "shasum"
        } else {
            "sha256sum"
        };
        vec![
            "ssh-keygen".into(),
            platform::tar_program().into_os_string(),
            "install".into(),
            sums.into(),
        ]
    };
    tools.iter().all(|tool| {
        if cfg!(windows) && tool == "ssh-keygen" {
            // The Windows ssh-keygen has no flag that only prints: it asks for a key file, and
            // it hangs when its output is a pipe. It is there if PATH holds it.
            return std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|dir| dir.join("ssh-keygen.exe").is_file())
            });
        }
        let mut cmd = Command::new(tool);
        if tool == platform::powershell_program().as_os_str() {
            cmd.args(["-NoProfile", "-Command", "exit 0"]);
        } else {
            cmd.arg("--version");
        }
        // a tool that runs at all is present, whatever it thinks of the flag
        // an empty file as input: a tool that asks for something reads the end of it
        let Ok(empty) = tempfile::tempfile() else {
            return false;
        };
        platform::run_bounded(
            cmd,
            Some(empty),
            RELEASE_CHILD_OUTPUT_MAX,
            RELEASE_CHILD_DEADLINE,
        )
        .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::run_bounded;

    /// wx33g: the README's POSIX install blocks are text for a person on macOS or Linux, so a
    /// Windows host renders them with `/` and the same words.
    #[test]
    fn wx33g_the_readme_install_section_is_posix_text_on_every_host() {
        let signer = format!(
            "{SIGNER_PREFIX}ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA11UHVOCACl0jfu9YJgNXZrmz5aFYaKlAqyiZWHiOOw"
        );
        let section = readme_install_section("0.14.0", &signer).expect("install section");
        assert!(!section.contains("/path/to/download\\"), "{section}");
        assert!(!section.contains("/path/to/rdy-0.14.0\\"), "{section}");
        assert!(
            section.contains("'/path/to/download/SHA256SUMS'"),
            "{section}"
        );
        assert!(
            section.contains("'/path/to/rdy-0.14.0/bin/rdy'"),
            "{section}"
        );
    }

    // The Unix-only fixtures (shell scripts, modes) live in the two whole-file Unix modules.
    #[cfg(unix)]
    use crate::platform::{mode_of, shell_script};

    #[cfg(unix)]
    mod unix_install_tests;
    #[cfg(unix)]
    mod unix_tests;
    use crate::skip::{self, Why};
    #[cfg(unix)]
    use std::sync::Mutex;

    const V: &str = "0.14.0";
    const TARGET: &str = "aarch64-apple-darwin";
    const BINARY: &[u8] = b"\x7fELF a small binary fixture\n";
    const MIT: &[u8] = b"the MIT license fixture\n";
    const APACHE: &[u8] = b"the Apache license fixture\n";
    /// The stand-ins' notices: fixed synthetic bytes, from no Cargo run.
    const NOTICES: &[u8] = b"Third-party notices for rdy\n\nsynthetic 1.0.0\nLicense: MIT\n\n--- LICENSE ---\nthe synthetic license\n\n";

    /// The stand-ins' notices provider.
    fn synthetic_notices(_: &str) -> Result<crate::release_notices::Notices, String> {
        Ok(crate::release_notices::Notices {
            bytes: NOTICES.to_vec(),
            mpl: Vec::new(),
        })
    }

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

    /// Asserts a Unix permission mode; Windows has no mode bits, so it asserts nothing there.
    #[cfg(unix)]
    fn assert_mode(path: &Path, want: u32) {
        assert_eq!(mode_of(path).expect("mode"), want, "{}", path.display());
    }
    #[cfg(not(unix))]
    fn assert_mode(_: &Path, _: u32) {}

    /// A probe that prints `text` (a printf format) instead of running the staged copy.
    fn printing(text: &'static str) -> impl Fn(&Path) -> Command {
        move |_| platform::print_command(&text.replace("\\n", "\n"))
    }

    fn setup<'a>(f: &Fixture, probe: &'a dyn Fn(&Path) -> Command) -> Setup<'a> {
        Setup {
            licenses: f.licenses.clone(),
            probe,
            native_target: None,
            notices: &synthetic_notices,
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
        let mut cmd = Command::new(platform::tar_program());
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
            format!("{stem}/{NOTICES_FILE}"),
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
            assert_mode(&root, 0o755);
            assert_mode(&root.join("rdy"), 0o755);
            assert_mode(&root.join(MIT_FILE), 0o644);
            assert_mode(&root.join(APACHE_FILE), 0o644);
            assert_eq!(fs::read(root.join(NOTICES_FILE)).expect("notices"), NOTICES);
            assert_mode(&root.join(NOTICES_FILE), 0o644);
            let note = root.join("PREVIEW.txt");
            if preview {
                assert_eq!(fs::read_to_string(&note).expect("note"), PREVIEW_NOTE);
                assert_mode(&note, 0o644);
            } else {
                assert!(!note.exists(), "a plain build has no preview note");
            }
            assert_eq!(names(&root).len(), if preview { 5 } else { 4 });
            let mut after = f.state();
            after[2].1.retain(|n| n != "out");
            assert_eq!(after, before, "the inputs are unchanged");
        }

        // The notices provider: building a setup calls it never (and runs no process);
        // packaging calls it once the three inputs are open and before the OUT check. A
        // failed or oversized notice is the notices refusal, before OUT is made.
        let starts = crate::platform::runner_starts();
        let release = Setup::release();
        assert_eq!(
            crate::platform::runner_starts(),
            starts,
            "Setup::release runs nothing"
        );
        drop(release);
        let calls = std::cell::Cell::new(0);
        let counting = |target: &str| {
            calls.set(calls.get() + 1);
            synthetic_notices(target)
        };
        let failing = |_: &str| -> Result<crate::release_notices::Notices, String> {
            Err("release sources: third-party notices are incomplete".into())
        };
        let oversized = |_: &str| {
            Ok(crate::release_notices::Notices {
                bytes: vec![b'n'; NOTICES_MAX as usize + 1],
                mpl: Vec::new(),
            })
        };
        let probe = printing("rdy 0.14.0\\n");
        let f = Fixture::new();
        let counted = Setup {
            notices: &counting,
            ..setup(&f, &probe)
        };
        assert_eq!(calls.get(), 0, "construction calls no provider");
        fs::rename(f.licenses.join(APACHE_FILE), f.dir.path().join("apache")).expect("mv");
        assert_eq!(
            package_with(&f.binary, V, TARGET, &f.out(), &counted),
            Err(Failure::InputFile)
        );
        assert_eq!(calls.get(), 0, "the inputs open first");
        fs::rename(f.dir.path().join("apache"), f.licenses.join(APACHE_FILE)).expect("mv");
        fs::create_dir(f.out()).expect("occupy OUT");
        assert_eq!(
            package_with(&f.binary, V, TARGET, &f.out(), &counted),
            Err(Failure::OutputExists)
        );
        assert_eq!(calls.get(), 1, "then the notices, before the OUT check");
        type Provider<'a> = &'a dyn Fn(&str) -> Result<crate::release_notices::Notices, String>;
        let refusing: [(&str, Provider); 2] =
            [("a failed", &failing), ("an oversized", &oversized)];
        for (what, provider) in refusing {
            let s = Setup {
                notices: provider,
                ..setup(&f, &probe)
            };
            assert_eq!(
                package_with(&f.binary, V, TARGET, &f.out(), &s).map_err(Failure::message),
                Err(MSG_NOTICES),
                "{what} notice, before an occupied OUT"
            );
            let g = Fixture::new();
            let before = g.state();
            let s = Setup {
                notices: provider,
                ..setup(&g, &probe)
            };
            assert_eq!(
                package_with(&g.binary, V, TARGET, &g.out(), &s).map_err(Failure::message),
                Err(MSG_NOTICES),
                "{what} notice"
            );
            assert_eq!(g.state(), before, "{what} notice: nothing created");
            assert!(!g.out().exists(), "{what} notice: no OUT");
        }
        assert_eq!(
            MSG_NOTICES,
            "release package: third-party notices are incomplete"
        );
        assert_eq!(NOTICES_FILE, "THIRD-PARTY-NOTICES.txt");
        assert_eq!(NOTICES_MAX, 16_777_216);
        assert_eq!(ARCHIVE_MAX, BINARY_MAX + NOTICES_MAX + 1_048_576);
        assert_eq!(ARCHIVE_MAX, 554_696_704);

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
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("out");
        // the real release packages only for this machine's target; on a machine with none,
        // the same release setup without that check stands in
        let (target, archive) = match crate::platform::release_target() {
            Ok(target) => (target, package(&rdy, version, target, &out)),
            Err(_) => {
                let any = Setup {
                    native_target: None,
                    ..Setup::release()
                };
                let made = package_with(&rdy, version, TARGETS[0], &out, &any)
                    .map_err(|f| f.message().to_string());
                (TARGETS[0], made)
            }
        };
        let archive = archive.expect("package the built CLI");
        let stem = if preview {
            format!("rdy-{version}-preview-{target}")
        } else {
            format!("rdy-{version}-{target}")
        };
        assert_eq!(archive, out.join(format!("{stem}.tar.gz")));
        let mut want = members(&stem, preview);
        if stem.ends_with(WINDOWS_TARGET) {
            // the Windows archive holds rdy.exe where the others hold rdy
            want.retain(|m| !m.ends_with("/rdy"));
            want.push(format!("{stem}/rdy.exe"));
            want.sort();
        }
        assert_eq!(listing(&archive), want);
        let into = dir.path().join("x");
        extract(&archive, &into);
        assert!(
            fs::read(into.join(&stem).join(crate::fixtures::rdy_exe_name())).expect("rdy")
                == fs::read(&rdy).expect("rdy"),
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
        if preview {
            assert!(
                fs::read(into.join(&stem).join(NOTE_FILE)).expect("PREVIEW.txt")
                    == PREVIEW_NOTE.as_bytes(),
                "PREVIEW.txt is the note"
            );
        }
        let notices = into.join(&stem).join(NOTICES_FILE);
        let want = crate::release_notices::for_target(target).expect("the target's notices");
        assert!(
            fs::read(&notices).expect("THIRD-PARTY-NOTICES.txt") == want.bytes,
            "THIRD-PARTY-NOTICES.txt is the target's notices"
        );
        assert_mode(&notices, 0o644);
        assert_eq!(
            names(&into.join(&stem)).len(),
            if preview { 5 } else { 4 },
            "five files in a preview, four in a plain build"
        );
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

        // Real children through the runner (the private fixture child, the same on every
        // platform): its stdout starts with libtest's header, so a clean exit is still not a
        // version line, and the runner's own failures take precedence.
        let probe = |mode: &str| {
            classify_probe(
                run_bounded(
                    platform::fixture_command(mode),
                    None,
                    crate::platform::RELEASE_CHILD_OUTPUT_MAX,
                    std::time::Duration::from_secs(600),
                ),
                V,
            )
        };
        assert_eq!(probe("exit:0"), Err(Failure::BinaryVersion));
        assert_eq!(probe("exit:1"), Err(Failure::BinaryVersion));
        assert_eq!(
            probe("output:stderr:10"),
            Err(Failure::BinaryVersion),
            "anything on stderr is wrong"
        );
        assert_eq!(probe("output:stdout:1048577"), Err(Failure::SizeLimit));
        assert_eq!(probe("output:stderr:1048577"), Err(Failure::SizeLimit));

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
    fn pre1_package_calls_unopenable_nonregular_inputs_invalid() {
        let probe = printing("rdy 0.14.0\n");
        // a device is never a regular file (Unix: /dev/tty, which opens only with a controlling
        // terminal; Windows: the null device); missing or opened, it is refused as invalid input
        let f = Fixture::new();
        let before = f.state();
        let device = platform::device_input();
        assert_eq!(
            package_with(device, V, TARGET, &f.out(), &setup(&f, &probe)),
            Err(Failure::InputFile),
            "{device:?}"
        );
        assert_eq!(f.state(), before, "{device:?}: nothing changed");
        assert!(!f.out().exists(), "{device:?}: no OUT");
    }

    #[test]
    fn pre1_package_failure_removes_only_its_own_paths() {
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_failure_removes_only_its_own_paths"
        )) {
            return;
        }
        let probe = printing("rdy 0.14.0 (preview)\\n");
        // the private fixture child (the same on every platform): it writes libtest's header to
        // stdout, which is a partial archive, and exits 3
        let partial = |_: &str| platform::fixture_command("exit:3");
        let missing = |_: &str| Command::new("/nonexistent/rdy-qa-tar");
        let noisy = |_: &str| platform::fixture_command("output:stderr:1048577");
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

    /// A rendezvous that needs no named pipe or shell, so it runs on every platform. The first
    /// time packaging calls the returned `reach` (it does so while it builds a command), a thread
    /// of the test runs `act` and then releases it. The channel reports that `act` ran.
    fn rendezvous(
        act: impl FnOnce() + Send + 'static,
    ) -> (impl Fn(), std::sync::mpsc::Receiver<()>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if ready_rx.recv_timeout(Duration::from_secs(60)).is_err() {
                return;
            }
            act();
            let _ = done_tx.send(());
            let _ = go_tx.send(());
        });
        let first = AtomicBool::new(true);
        let reach = move || {
            if first.swap(false, Ordering::SeqCst) {
                let _ = ready_tx.send(());
                let _ = go_rx.recv_timeout(Duration::from_secs(60));
            }
        };
        (reach, done_rx)
    }

    #[test]
    fn wx35a_package_never_takes_an_archive_path_it_did_not_create() {
        const STEM: &str = "rdy-0.14.0-preview-aarch64-apple-darwin";
        if !tar_or_skip(&crate::test_name!(
            "wx35a_package_never_takes_an_archive_path_it_did_not_create"
        )) {
            return;
        }
        let missing = |_: &str| Command::new("/nonexistent/rdy-qa-tar");
        for (case, what) in [
            "a newcomer at the archive path before tar, with tar",
            "a newcomer at the archive path before tar, with a tar that cannot start",
            "a newcomer in place of the archive when tar starts, then tar succeeds",
            "a newcomer in place of the archive when tar starts, then tar fails",
            "a newcomer in place of the archive before the real tar starts",
        ]
        .into_iter()
        .enumerate()
        {
            let f = Fixture::new();
            let before = f.state();
            let archive = f.out().join(format!("{STEM}.tar.gz"));
            let (reach, placed) = rendezvous({
                let archive = archive.clone();
                move || {
                    let _ = fs::remove_file(&archive);
                    fs::write(&archive, b"newcomer\n").expect("place the newcomer");
                }
            });
            let plain = printing("rdy 0.14.0 (preview)\\n");
            let paused_probe = |p: &Path| {
                reach();
                plain(p)
            };
            let code = if case == 2 { 0 } else { 3 };
            let paused_tar = |_: &str| {
                reach();
                platform::fixture_command(&format!("exit:{code}"))
            };
            let paused_real_tar = |stem: &str| {
                reach();
                tar_command(stem)
            };
            let mut s = setup(&f, if case < 2 { &paused_probe } else { &plain });
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
    #[cfg(unix)]
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
        // Once checked, an input path that changes does not change what is packaged.
        if !tar_or_skip(&crate::test_name!(
            "pre1_package_copies_the_inputs_it_checked"
        )) {
            return;
        }
        let cases: [(&str, Change); 3] = [
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

    #[test]
    fn pre1_package_refuses_a_target_that_is_not_this_machines() {
        // The version probe runs the binary here, so the real release packages only for this
        // machine's target: the other triple is an argument error, before any file work. On a
        // machine with no release target, both are.
        let native = crate::platform::release_target().ok();
        let mut refused = 0;
        for target in [TARGETS[0], TARGETS[1], WINDOWS_TARGET] {
            if native == Some(target) {
                continue;
            }
            let f = Fixture::new();
            let before = f.state();
            assert_eq!(
                package(&f.binary, V, target, &f.out()),
                Err(MSG_INVALID_ARGUMENTS.to_string()),
                "{target} on a {native:?} machine"
            );
            assert_eq!(f.state(), before, "{target}: nothing made or touched");
            refused += 1;
        }
        assert_eq!(refused, if native.is_some() { 2 } else { 3 });
    }

    /// The Windows packaging branch, driven by the target triple on any host: a stand-in
    /// probe replaces the staged executable's own run.
    fn windows_package(f: &Fixture, stdout: &'static str) -> PathBuf {
        let probe = printing(stdout);
        package_with(&f.binary, V, WINDOWS_TARGET, &f.out(), &setup(f, &probe))
            .unwrap_or_else(|e| panic!("package for Windows: {}", e.message()))
    }

    #[test]
    fn wx35a_windows_target_packages_rdy_exe() {
        if !tar_or_skip(&crate::test_name!("wx35a_windows_target_packages_rdy_exe")) {
            return;
        }
        assert_eq!(WINDOWS_TARGET, "x86_64-pc-windows-msvc");
        assert!(valid_target(WINDOWS_TARGET));
        for (stdout, preview, stem) in [
            (
                "rdy 0.14.0 (preview)\\n",
                true,
                "rdy-0.14.0-preview-x86_64-pc-windows-msvc",
            ),
            ("rdy 0.14.0\\n", false, "rdy-0.14.0-x86_64-pc-windows-msvc"),
        ] {
            let f = Fixture::new();
            let archive = windows_package(&f, stdout);
            assert_eq!(archive, f.out().join(format!("{stem}.tar.gz")));
            let mut want = members(stem, preview);
            want.retain(|m| m != &format!("{stem}/rdy"));
            want.push(format!("{stem}/rdy.exe"));
            want.sort();
            assert_eq!(
                listing(&archive),
                want,
                "rdy.exe, licenses, notices, marker"
            );
            let into = f.dir.path().join("x");
            extract(&archive, &into);
            let root = into.join(stem);
            assert_eq!(fs::read(root.join("rdy.exe")).expect("rdy.exe"), BINARY);
            assert!(!root.join("rdy").exists(), "no extensionless executable");
            assert_eq!(fs::read(root.join(NOTICES_FILE)).expect("notices"), NOTICES);
            assert_eq!(root.join("PREVIEW.txt").exists(), preview);
        }
        // the Unix targets keep `rdy`
        let f = Fixture::new();
        let probe = printing("rdy 0.14.0\\n");
        let archive = package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe))
            .map_err(Failure::message)
            .expect("package for macOS");
        assert!(
            listing(&archive).contains(&"rdy-0.14.0-aarch64-apple-darwin/rdy".to_string()),
            "{:?}",
            listing(&archive)
        );
    }

    #[test]
    fn wx35a_archive_bytes_and_manifest_names_are_stable() {
        // the archive names of all three targets, byte for byte
        assert_eq!(
            [TARGETS[0], TARGETS[1], WINDOWS_TARGET].map(|t| archive_name("0.14.0", t)),
            [
                "rdy-0.14.0-preview-aarch64-apple-darwin.tar.gz",
                "rdy-0.14.0-preview-x86_64-unknown-linux-gnu.tar.gz",
                "rdy-0.14.0-preview-x86_64-pc-windows-msvc.tar.gz",
            ]
        );
        // the two Unix archive lines of the manifest stay; the Windows line joins them in name
        // order (Task 36a: the checker takes the three-line manifest, and a Windows install
        // needs it); no zip, MSI or installer name exists
        assert_eq!(
            TARGETS,
            ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
        );
        let line = |t: &str| format!("{}  {}\n", "a".repeat(64), archive_name("0.14.0", t));
        let manifest = format!("{}{}", line(TARGETS[0]), line(TARGETS[1]));
        for target in TARGETS {
            assert_eq!(
                check_manifest(manifest.as_bytes(), "0.14.0", target),
                Ok(())
            );
        }
        assert_eq!(
            check_manifest(manifest.as_bytes(), "0.14.0", WINDOWS_TARGET),
            Err(InstallFailure::Checksums),
            "the Windows archive's hash must be in the manifest"
        );
        let sorted = format!(
            "{}{}{}",
            line(TARGETS[0]),
            line(WINDOWS_TARGET),
            line(TARGETS[1])
        );
        for target in [TARGETS[0], TARGETS[1], WINDOWS_TARGET] {
            assert_eq!(check_manifest(sorted.as_bytes(), "0.14.0", target), Ok(()));
        }
        let unsorted = format!("{manifest}{}", line(WINDOWS_TARGET));
        assert!(check_manifest(unsorted.as_bytes(), "0.14.0", WINDOWS_TARGET).is_err());
        assert!(!archive_name("0.14.0", WINDOWS_TARGET).ends_with(".zip"));
        if !tar_or_skip(&crate::test_name!(
            "wx35a_archive_bytes_and_manifest_names_are_stable"
        )) {
            return;
        }
        // the member bytes are the inputs' bytes and the members are the sorted closed set
        let f = Fixture::new();
        let archive = windows_package(&f, "rdy 0.14.0 (preview)\\n");
        let stem = "rdy-0.14.0-preview-x86_64-pc-windows-msvc";
        let into = f.dir.path().join("x");
        extract(&archive, &into);
        let root = into.join(stem);
        assert_eq!(names(&root), {
            let mut v = vec![
                "LICENSE-APACHE",
                "LICENSE-MIT",
                "PREVIEW.txt",
                "THIRD-PARTY-NOTICES.txt",
                "rdy.exe",
            ];
            v.sort();
            v.into_iter().map(str::to_string).collect::<Vec<_>>()
        });
        assert_eq!(fs::read(root.join("rdy.exe")).expect("exe"), BINARY);
        assert_eq!(fs::read(root.join(MIT_FILE)).expect("mit"), MIT);
        assert_eq!(fs::read(root.join(APACHE_FILE)).expect("apache"), APACHE);
        assert_eq!(
            fs::read(root.join("PREVIEW.txt")).expect("note"),
            PREVIEW_NOTE.as_bytes()
        );
        assert!(!PREVIEW_NOTE.contains('\r'), "LF note");
    }

    #[test]
    fn wx35a_windows_archive_contains_no_host_path() {
        if !tar_or_skip(&crate::test_name!(
            "wx35a_windows_archive_contains_no_host_path"
        )) {
            return;
        }
        let f = Fixture::new();
        let archive = windows_package(&f, "rdy 0.14.0 (preview)\\n");
        let verbose = String::from_utf8(tar(&["-tvzf".as_ref(), archive.as_os_str()]))
            .expect("utf-8 listing");
        let host = f.dir.path().to_string_lossy().into_owned();
        assert!(!verbose.contains(&host), "{verbose}");
        for member in listing(&archive) {
            assert!(
                member.starts_with("rdy-0.14.0-preview-x86_64-pc-windows-msvc"),
                "{member}"
            );
            assert!(!member.contains(['\\', ':']), "{member}");
            assert!(!member.contains(STAGE_DIR), "{member}");
        }
        // no file holds the temporary directory either
        let into = f.dir.path().join("x");
        extract(&archive, &into);
        for entry in
            fs::read_dir(into.join("rdy-0.14.0-preview-x86_64-pc-windows-msvc")).expect("read_dir")
        {
            let bytes = fs::read(entry.expect("entry").path()).expect("read");
            assert!(
                !String::from_utf8_lossy(&bytes).contains(&host),
                "a member names the host directory"
            );
        }
    }

    #[test]
    fn wx35a_native_windows_never_takes_unsupported_target_branch() {
        // the shared mapping: Windows on x86-64 with the MSVC environment is a release target
        assert_eq!(
            platform::release_target_of("windows", "x86_64", false).expect("Windows amd64"),
            WINDOWS_TARGET
        );
        for (os, arch, gnu) in [
            ("windows", "x86_64", true),
            ("windows", "aarch64", false),
            ("windows", "x86", false),
        ] {
            let err = platform::release_target_of(os, arch, gnu).expect_err("no release target");
            assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{os}/{arch}/{gnu}");
        }
        // a machine whose target is Windows packages for it, and is not refused as invalid
        let f = Fixture::new();
        let native = || Ok(WINDOWS_TARGET);
        let probe = printing("rdy 0.14.0\\n");
        let on_windows = Setup {
            native_target: Some(&native),
            ..setup(&f, &probe)
        };
        if tar_available() {
            let made = package_with(&f.binary, V, WINDOWS_TARGET, &f.out(), &on_windows);
            assert!(made.is_ok(), "{made:?}");
        }
        // and a Unix target is still another machine's target there
        let g = Fixture::new();
        let on_windows = Setup {
            native_target: Some(&native),
            ..setup(&g, &probe)
        };
        assert_eq!(
            package_with(&g.binary, V, TARGET, &g.out(), &on_windows),
            Err(Failure::Arguments)
        );
        assert!(!g.out().exists());
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
        // an absolute path of the platform: `C:\` or `/`, then the name
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        assert!(valid_path(Path::new(&format!("{root}a"))));
        assert!(valid_path(Path::new(&format!(
            "{root}{}",
            "a".repeat(4095 - root.len() + 1)
        ))));
        for bad in [
            "a".to_string(),
            format!("{root}{}", "a".repeat(4096)),
            format!("{root}a\0b"),
            format!("{root}a\rb"),
            format!("{root}a\nb"),
        ] {
            assert!(!valid_path(Path::new(&bad)), "{bad:?}");
        }
    }

    // ----- the install check -----

    const IV: &str = "0.14.0";
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    /// The hang guard of a test helper's own tool runs.
    const GUARD: std::time::Duration = std::time::Duration::from_secs(600);

    /// The target these tests install: this machine's release target, else Linux amd64, whose
    /// checksum tool every Linux has (Linux arm64 runs these tests too).
    fn host_target() -> &'static str {
        platform::release_target().unwrap_or(TARGETS[1])
    }

    /// Whether the install tools run here; a skip line when they do not.
    fn install_tools_or_skip(test: &str) -> bool {
        if install_tools_available(host_target()) {
            return true;
        }
        skip::skip(test, Why::Prerequisite, INSTALL_SKIP_REASON);
        false
    }

    /// Runs `program` with `args`, which must succeed: a test helper's own tool run. Its stdout.
    fn tool(program: &str, args: &[&OsStr]) -> Vec<u8> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        let out =
            run_bounded(cmd, None, 1 << 20, GUARD).unwrap_or_else(|e| panic!("{program}: {e}"));
        assert!(
            out.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Makes a disposable Ed25519 test key at `key`, with no passphrase; its signer line. It is
    /// never a release key.
    fn test_key(key: &Path) -> String {
        tool(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"]
                .map(OsStr::new)
                .into_iter()
                .chain([key.as_os_str()])
                .collect::<Vec<_>>(),
        );
        let public = fs::read_to_string(format!("{}.pub", key.display())).expect("public key");
        let mut words = public.split_whitespace();
        let (kind, base64) = (words.next().expect("type"), words.next().expect("key"));
        format!("{SIGNER_PREFIX}{kind} {base64}")
    }

    /// Signs `file` with `key` in `namespace` (sha512), replacing `<file>.sig`.
    fn sign(key: &Path, file: &Path, namespace: &str) {
        let _ = fs::remove_file(format!("{}.sig", file.display()));
        tool(
            "ssh-keygen",
            &[
                "-Y".as_ref(),
                "sign".as_ref(),
                "-f".as_ref(),
                key.as_os_str(),
                "-n".as_ref(),
                namespace.as_ref(),
                "-O".as_ref(),
                "hashalg=sha512".as_ref(),
                file.as_os_str(),
            ],
        );
    }

    // ----- Task 36a: the portable install checker -----

    /// The SHA-256 of `bytes` in lowercase hex.
    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A new directory `name` in `parent`, private to this user, made through handles.
    fn private_dir(parent: &Path, name: &str) -> PathBuf {
        let handle = platform::open_dir(parent).expect("open the parent");
        platform::create_private_dir_at(&handle, name.as_ref()).expect("a private directory");
        parent.join(name)
    }

    /// The manifest of `target`'s kit: the two Unix lines, or all three for Windows, with the
    /// real hash for `target`'s archive and a placeholder for the others.
    fn manifest_of(target: &str, archive: &[u8]) -> String {
        let listed: &[&str] = if target == WINDOWS_TARGET {
            &MANIFEST_TARGETS
        } else {
            &TARGETS
        };
        listed
            .iter()
            .map(|t| {
                let hash = if *t == target {
                    sha256_hex(archive)
                } else {
                    "ab".repeat(32)
                };
                format!("{hash}  {}\n", archive_name(IV, t))
            })
            .collect()
    }

    /// A private DOWNLOAD with a stand-in archive for the host's target, a manifest and its
    /// signature by a disposable key (never a release key), and the paths the check makes.
    struct Download {
        dir: tempfile::TempDir,
        download: PathBuf,
        key: PathBuf,
        signer: String,
        target: &'static str,
        archive: Vec<u8>,
    }

    impl Download {
        fn new() -> Download {
            let dir = tempfile::tempdir().expect("tempdir");
            let download = private_dir(dir.path(), "down load");
            private_dir(dir.path(), "pre fix");
            private_dir(dir.path(), "scratch");
            let target = host_target();
            let archive = b"a stand-in for an archive: no check extracts it".to_vec();
            fs::write(download.join(archive_name(IV, target)), &archive).expect("archive");
            let key = dir.path().join("key");
            let signer = test_key(&key);
            let kit = Download {
                dir,
                download,
                key,
                signer,
                target,
                archive,
            };
            kit.write_manifest(&manifest_of(target, &kit.archive), RELEASE_NAMESPACE);
            kit
        }

        /// Replaces the manifest with `text`, signed in `namespace` by the kit's key.
        fn write_manifest(&self, text: &str, namespace: &str) {
            let manifest = self.download.join(MANIFEST_FILE);
            fs::write(&manifest, text).expect("manifest");
            sign(&self.key, &manifest, namespace);
        }

        fn input(&self, signer_line: &str) -> InstallInput {
            InstallInput {
                download: self.download.clone(),
                signer_line: signer_line.to_string(),
                version: IV.to_string(),
                commit: COMMIT.to_string(),
                target: self.target.to_string(),
                unpack: self.dir.path().join("un pack"),
                prefix: self.dir.path().join("pre fix").join(format!("rdy-{IV}")),
                browser: false,
            }
        }

        /// The sorted names in DOWNLOAD.
        fn names(&self) -> Vec<String> {
            names(&self.download)
        }
    }

    /// What each guide command looked like when the check started it: its program, or the start
    /// of its PowerShell text.
    fn started(c: &GuideCommand) -> String {
        match &c.ps {
            Some(text) => text.chars().take(24).collect(),
            None => c.program.to_string_lossy().into_owned(),
        }
    }

    /// Runs the check of `input` for `kit` with the real tools, and the guide commands it
    /// started, in order.
    fn check_logged(
        kit: &Download,
        input: &InstallInput,
    ) -> (Result<InstalledRelease, InstallFailure>, Vec<String>) {
        let log = std::cell::RefCell::new(Vec::new());
        let note = |c: &GuideCommand| log.borrow_mut().push(started(c));
        let host = kit.target;
        let native = move || Ok(host);
        let os = || Ok("a test machine".to_string());
        let setup = InstallSetup {
            native_target: &native,
            os: &os,
            ran: &note,
            scratch: kit.dir.path().join("scratch"),
            ..InstallSetup::release()
        };
        let result = check_with(input, &setup);
        (result, log.into_inner())
    }

    /// Nothing was extracted, installed or run: UNPACK and PREFIX are absent, and DOWNLOAD
    /// holds what it held (the signer file the check made is gone).
    fn nothing_made(kit: &Download, input: &InstallInput, before: &[String]) {
        assert!(absent(&input.unpack), "UNPACK was made");
        assert!(absent(&input.prefix), "PREFIX was made");
        assert_eq!(kit.names(), before, "DOWNLOAD changed");
        assert_eq!(
            names(&kit.dir.path().join("scratch")),
            Vec::<String>::new(),
            "the smoke scratch directory was made"
        );
    }

    #[test]
    fn wx36a_signature_passes_before_manifest_and_extraction() {
        if !install_tools_or_skip(&crate::test_name!(
            "wx36a_signature_passes_before_manifest_and_extraction"
        )) {
            return;
        }
        // A manifest that OpenSSH accepts but whose grammar is wrong: the signature stage passes
        // (both of its commands ran), the manifest check stops the run, and nothing is extracted.
        let kit = Download::new();
        let one_line = manifest_of(kit.target, &kit.archive)
            .lines()
            .next()
            .map(|l| format!("{l}\n"))
            .expect("a line");
        kit.write_manifest(&one_line, RELEASE_NAMESPACE);
        let before = kit.names();
        let input = kit.input(&kit.signer);
        let (result, ran) = check_logged(&kit, &input);
        assert_eq!(result, Err(InstallFailure::Checksums));
        assert_eq!(ran.len(), 2, "{ran:?}");
        assert!(ran[1].starts_with("ssh-keygen"), "{ran:?}");
        nothing_made(&kit, &input, &before);

        // The same manifest, signed in the namespace of a key transition: not a release
        // signature, so the signature stage fails and the manifest is never read as one.
        // (The Windows ssh-keygen does not return when its output is a pipe and the signature's
        // namespace differs: the check then fails as `deadline`, after its 30 s guard, with
        // nothing extracted. This case runs on the Unix machines only.)
        let namespaces: &[&str] = if cfg!(windows) {
            &[]
        } else {
            &[
                crate::release_keys::TRANSITION_NAMESPACE,
                crate::release_keys::REVOCATION_NAMESPACE,
            ]
        };
        for namespace in namespaces.iter().copied() {
            kit.write_manifest(&manifest_of(kit.target, &kit.archive), namespace);
            let before = kit.names();
            let (result, ran) = check_logged(&kit, &input);
            assert_eq!(result, Err(InstallFailure::Signature), "{namespace}");
            assert_eq!(ran.len(), 2, "{namespace}: {ran:?}");
            nothing_made(&kit, &input, &before);
        }

        // Signed correctly, the run goes on past the manifest to the hash and the extraction.
        kit.write_manifest(&manifest_of(kit.target, &kit.archive), RELEASE_NAMESPACE);
        let (result, ran) = check_logged(&kit, &input);
        // the stand-in is not an archive: the extraction fails, after signature and hash
        assert_eq!(result, Err(InstallFailure::Install), "{ran:?}");
        assert!(ran.len() >= 4, "{ran:?}");
    }

    #[test]
    fn wx36a_wrong_version_key_extracts_and_runs_nothing() {
        if !install_tools_or_skip(&crate::test_name!(
            "wx36a_wrong_version_key_extracts_and_runs_nothing"
        )) {
            return;
        }
        let kit = Download::new();
        let before = kit.names();
        // another key of the trust list (the historical key of 0.20.0), another disposable key:
        // neither signed this manifest
        let historical = ReleaseKeys::historical()
            .signer_for("0.20.0")
            .expect("the historical key")
            .signer_line;
        let other = test_key(&kit.dir.path().join("other key"));
        for wrong in [historical, other] {
            let input = kit.input(&wrong);
            let (result, ran) = check_logged(&kit, &input);
            assert_eq!(result, Err(InstallFailure::Signature));
            // the signer file is written and OpenSSH refuses: nothing else starts
            assert_eq!(ran.len(), 2, "{ran:?}");
            nothing_made(&kit, &input, &before);
        }
        // a version that no key signs has no signer line at all: the trust data refuses it
        assert!(ReleaseKeys::historical().signer_for("0.1.0").is_err());
    }

    /// A Windows input with fixed paths; `signer` is the trusted line.
    fn windows_input(signer: &str) -> InstallInput {
        InstallInput {
            download: PathBuf::from(r"C:\Users\person\down load"),
            signer_line: signer.to_string(),
            version: IV.to_string(),
            commit: COMMIT.to_string(),
            target: WINDOWS_TARGET.to_string(),
            unpack: PathBuf::from(r"C:\Users\person\un pack"),
            prefix: PathBuf::from(format!(r"C:\Users\person\rdy-{IV}")),
            browser: false,
        }
    }

    #[test]
    fn wx36a_windows_guide_runs_its_own_commands() {
        // The text of the guide, on every host: literal PowerShell, one source for the guide and
        // the check.
        let signer = {
            let key = ssh_key::PublicKey::new(
                ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([7u8; 32])),
                "",
            );
            format!("{SIGNER_PREFIX}{}", key.to_openssh().expect("openssh"))
        };
        let input = windows_input(&signer);
        let steps = commands(&input).expect("the Windows commands");
        assert_eq!(steps.len(), 8, "the Unix guide's steps but the browser one");
        let block = render_guide(&input).expect("the Windows guide");
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines[0], "& {");
        assert_eq!(lines[1], "$ErrorActionPreference = 'Stop'");
        assert_eq!(
            lines[2],
            r"Set-Location -LiteralPath 'C:\Users\person\down load'"
        );
        assert!(lines[3].starts_with("$s = [IO.File]::Open('C:\\Users\\person\\down load\\release-allowed-signers', [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::Write, [IO.FileShare]'ReadWrite, Delete'); $s.SetLength(0); $b = [Text.Encoding]::UTF8.GetBytes('rdy-release namespaces=\"rdy-release-v1\" ssh-ed25519 "));
        assert_eq!(
            lines[4],
            "& 'cmd.exe' '/d' '/c' 'ssh-keygen.exe -Y verify -f release-allowed-signers -I rdy-release -n rdy-release-v1 -s SHA256SUMS.sig < SHA256SUMS'"
        );
        assert_eq!(
            lines[5],
            "if ($LASTEXITCODE -ne 0) { throw 'a step failed' }"
        );
        assert!(lines[6].starts_with("$n = 0; foreach ($line in [IO.File]::ReadAllLines('C:\\Users\\person\\down load\\SHA256SUMS'))"));
        assert_eq!(
            lines[8],
            r"& (Join-Path $env:SystemRoot 'System32\tar.exe') '-xzf' 'C:\Users\person\down load\rdy-0.14.0-preview-x86_64-pc-windows-msvc.tar.gz' '-C' 'C:\Users\person\un pack'"
        );
        assert!(!block.contains("&&"), "PowerShell 5.1 has no &&");
        assert!(!block.contains("printf") && !block.contains("sha256sum"));
        let native = lines.iter().filter(|l| l.starts_with("& ")).count();
        let checks = lines.iter().filter(|l| l.starts_with("if ($LAST")).count();
        assert_eq!(
            native,
            checks + 1,
            "every native command but the block is checked"
        );
        let tail: Vec<&str> = lines.iter().rev().take(4).rev().copied().collect();
        assert_eq!(
            tail,
            [
                r"# Add the version's bin directory to PATH in this session:",
                r"# $env:Path = 'C:\Users\person\rdy-0.14.0\bin;' + $env:Path",
                r"# rdy needs SSH_AUTH_SOCK, even when ssh works without it. Set it to the agent pipe:",
                r"# $env:SSH_AUTH_SOCK = '\\.\pipe\openssh-ssh-agent'",
            ]
        );

        // A token is a literal: the quote characters PowerShell reads as one are doubled, and
        // `$` and a backtick expand nothing. Text it cannot render is refused.
        assert_eq!(ps_literal("a'b").as_deref(), Ok("'a''b'"));
        assert_eq!(
            ps_literal("a\u{2019}b").as_deref(),
            Ok("'a\u{2019}\u{2019}b'")
        );
        assert_eq!(ps_literal("$x `y").as_deref(), Ok("'$x `y'"));
        for bad in ["a\nb", "a\rb", "a\0b"] {
            assert_eq!(ps_literal(bad), Err(MSG_INVALID_COMMAND.to_string()));
        }
        let mut quoted = windows_input(&signer);
        quoted.unpack = PathBuf::from("C:\\Users\\it's\\un\u{2018}pack");
        let block = render_guide(&quoted).expect("quoted paths");
        assert!(block.contains(r"'C:\Users\it''s\un‘‘pack'"), "{block}");

        // Paths outside the guide's grammar are argument errors.
        for (what, edit) in [
            (
                "a relative path",
                (|i: &mut InstallInput| i.unpack = PathBuf::from(r"un pack"))
                    as fn(&mut InstallInput),
            ),
            ("a slash", |i| {
                i.unpack = PathBuf::from("C:/Users/person/unpack")
            }),
            ("a dot component", |i| {
                i.unpack = PathBuf::from(r"C:\Users\..\unpack")
            }),
            ("a name Windows refuses", |i| {
                i.unpack = PathBuf::from(r"C:\Users\un*pack")
            }),
            ("a stream name", |i| {
                i.unpack = PathBuf::from(r"C:\Users\unpack:s")
            }),
            ("a drive root", |i| i.unpack = PathBuf::from(r"C:\")),
            ("a prefix with no version", |i| {
                i.prefix = PathBuf::from(r"C:\Users\person\rdy")
            }),
            ("overlapping paths", |i| {
                i.unpack = PathBuf::from(r"c:\users\PERSON\down load\unpack")
            }),
            ("the browser path", |i| i.browser = true),
            ("a bad commit", |i| i.commit = "abc".to_string()),
        ] {
            let mut bad = windows_input(&signer);
            edit(&mut bad);
            assert!(commands(&bad).is_err(), "{what}");
        }

        // A native Windows machine runs the printed block, and the block installs the exe.
        #[cfg(windows)]
        windows_guide_runs();
    }

    /// Runs `block` in Windows PowerShell as the encoded command: no quoting between the test
    /// and the block.
    #[cfg(windows)]
    fn run_block(block: &str) -> Output {
        let mut cmd = Command::new(platform::powershell_program());
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded_command(block),
        ]);
        run_bounded(cmd, None, 1 << 20, GUARD).expect("run the block")
    }

    /// A signed Windows DOWNLOAD named `name` in `work`, holding an archive of the built `rdy`
    /// as `rdy.exe`: the download, the signer line of its key and the archive's path.
    #[cfg(windows)]
    fn windows_download(work: &Path, name: &str) -> (PathBuf, String, PathBuf) {
        let download = private_dir(work, name);
        let stem = archive_stem(IV, WINDOWS_TARGET);
        let src = work.join(format!("src {name}"));
        fs::create_dir_all(src.join(&stem)).expect("mkdir");
        fs::copy(crate::fixtures::rdy_bin(), src.join(&stem).join("rdy.exe"))
            .expect("the built rdy");
        let archive = download.join(archive_name(IV, WINDOWS_TARGET));
        tool(
            platform::tar_program().to_str().expect("utf-8"),
            &[
                OsStr::new("-czf"),
                archive.as_os_str(),
                OsStr::new("-C"),
                src.as_os_str(),
                stem.as_ref(),
            ],
        );
        let key = work.join(format!("key {name}"));
        let signer = test_key(&key);
        let manifest = download.join(MANIFEST_FILE);
        fs::write(
            &manifest,
            manifest_of(WINDOWS_TARGET, &fs::read(&archive).expect("archive")),
        )
        .expect("manifest");
        sign(&key, &manifest, RELEASE_NAMESPACE);
        (download, signer, archive)
    }

    /// The Windows guide, rendered and run in PowerShell: it verifies, extracts and installs
    /// the exe; a wrong key and a changed archive stop it before anything is extracted.
    #[cfg(windows)]
    fn windows_guide_runs() {
        if !install_tools_or_skip(&crate::test_name!(
            "wx36a_windows_guide_runs_its_own_commands"
        )) {
            return;
        }
        let work = tempfile::tempdir().expect("tempdir");
        // names that break a careless script: a quote, curly quotes, a dollar sign, spaces
        let (download, signer, archive) = windows_download(work.path(), "d'l \u{2018}x\u{2019} $x");
        let input = |tag: &str, download: &Path, signer: &str| InstallInput {
            download: download.to_path_buf(),
            signer_line: signer.to_string(),
            version: IV.to_string(),
            commit: COMMIT.to_string(),
            target: WINDOWS_TARGET.to_string(),
            unpack: work.path().join(format!("un pack {tag}")),
            prefix: work
                .path()
                .join(format!("pre fix {tag}"))
                .join(format!("rdy-{IV}")),
            browser: false,
        };
        let good = input("good", &download, &signer);
        let out = run_block(&render_guide(&good).expect("the guide"));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let name = archive_name(IV, WINDOWS_TARGET);
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&format!("{name}: OK\n")),
            "only the archive that is there is checked, with LF"
        );
        let installed = good.prefix.join("bin").join("rdy.exe");
        assert_eq!(
            fs::read(&installed).expect("installed"),
            fs::read(crate::fixtures::rdy_bin()).expect("built"),
            "the installed exe is the archive's"
        );
        let version = |exe: &Path| {
            let mut cmd = Command::new(exe);
            cmd.arg("--version");
            run_bounded(cmd, None, 1 << 20, GUARD)
                .expect("--version")
                .stdout
        };
        assert_eq!(version(&installed), version(&crate::fixtures::rdy_bin()));

        // another key signed nothing here: OpenSSH refuses, and nothing is made
        let wrong = test_key(&work.path().join("wrong key"));
        let (download, _, _) = windows_download(work.path(), "wrong");
        let bad = input("wrong", &download, &wrong);
        let out = run_block(&render_guide(&bad).expect("the guide"));
        assert!(!out.status.success());
        assert!(absent(&bad.unpack) && absent(&bad.prefix));

        // a changed archive: the signature holds, the hash does not, and nothing is extracted
        let (download, signer, archive_2) = windows_download(work.path(), "changed");
        let mut bytes = fs::read(&archive_2).expect("archive");
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&archive_2, bytes).expect("change it");
        let changed = input("changed", &download, &signer);
        let out = run_block(&render_guide(&changed).expect("the guide"));
        assert!(!out.status.success());
        assert!(absent(&changed.unpack) && absent(&changed.prefix));
        let _ = archive;
    }
}
