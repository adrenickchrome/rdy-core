//! Release archives: one native `.tar.gz` per platform, holding the `rdy`
//! binary, both license files and, for a preview build, the preview note as `PREVIEW.txt`. The
//! binary's own `--version` output decides whether it is a preview, so the archive's name and
//! note follow the build, with no second switch. Packaging stops at the unsigned archive: the
//! signed `SHA256SUMS` is made later, offline.
//!
//! `qa-release-package` is the command; [`package`] does the work.
use crate::platform::{self, RELEASE_CHILD_DEADLINE, RELEASE_CHILD_OUTPUT_MAX};
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
    /// This machine's release target, the only one the real release packages for: the version
    /// probe runs the binary here, so a binary it accepts is native. `None` (stand-ins only)
    /// accepts either target.
    native_target: Option<&'a dyn Fn() -> io::Result<&'static str>>,
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
/// The error is one of the `MSG_` texts. Checks run in order: the grammar of the arguments and
/// that `target` is this machine's release target (the version probe runs the binary here, so
/// the binary is native), the binary, `LICENSE-MIT`, `LICENSE-APACHE`, then that `out` does not
/// exist. On failure every path this call created is removed again, and nothing that was there
/// before is touched.
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
/// The installed check's PATH after the version's bin directory.
const SMOKE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
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
}

impl GuideCommand {
    fn new(program: &str, args: &[&OsStr], cwd: &Path) -> Self {
        GuideCommand {
            program: program.into(),
            args: args.iter().map(|a| a.to_os_string()).collect(),
            cwd: cwd.to_path_buf(),
            stdin: None,
            stdout: None,
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

/// Checks the grammar of every argument (no filesystem access) and builds the guide.
fn guide(input: &InstallInput) -> Result<Guide, InstallFailure> {
    let (v, target) = (input.version.as_str(), input.target.as_str());
    let paths = [&input.download, &input.unpack, &input.prefix];
    let grammar = valid_version(v)
        && valid_target(target)
        && valid_commit(&input.commit)
        && valid_signer_line(&input.signer_line)
        && paths.iter().all(|p| valid_path(p))
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
    let archive = download.join(archive_name(v, target));
    let (unpack, prefix) = (input.unpack.as_os_str(), input.prefix.as_os_str());
    let mut signers = GuideCommand::new(
        "printf",
        &[r"%s\n".as_ref(), input.signer_line.as_ref()],
        download,
    );
    signers.stdout = Some(download.join(SIGNERS_FILE));
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
    verify.stdin = Some(download.join(MANIFEST_FILE));
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
    let bin = input.prefix.join("bin");
    let installed = bin.join("rdy");
    let extracted = input.unpack.join(&stem).join("rdy");
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
    let mut cmd = Command::new(&c.program);
    cmd.args(&c.args);
    cmd
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
                platform::holds_file(bin, OsStr::new("rdy"), rdy).unwrap_or(false)
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
            Some(rewound(&mut dl.manifest.file).map_err(|_| stage)?)
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
    platform::start_in_dir(&mut cmd, &dl.dir).map_err(|_| stage)?;
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
        (meta.is_dir()
            && rdy_store::pio::owner(&meta) == rdy_store::pio::own_uid()
            && platform::is_empty_dir(&dir).unwrap_or(false))
        .then_some(())?;
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

    /// Removes what is in the directory, through its own handle (`find . -mindepth 1 -delete`
    /// started in it, which never follows a link), then the directory from its parent, only
    /// while the parent's entry is still this directory. False when a step fails or the entry
    /// names another file now: the directory is then kept.
    fn remove(self) -> bool {
        if !self.in_place() {
            return false;
        }
        let mut find = Command::new("find");
        find.args([".", "-mindepth", "1", "-delete"]);
        let emptied = platform::start_in_dir(&mut find, &self.dir).is_ok()
            && platform::run_bounded(find, None, RELEASE_CHILD_OUTPUT_MAX, RELEASE_CHILD_DEADLINE)
                .is_ok_and(|o| o.status.success());
        // rmdir removes only an empty directory: a directory swapped in since is not removed
        emptied && self.in_place() && platform::remove_dir_at(&self.parent, &self.name).is_ok()
    }
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

/// A handle to `file` that reads from its start (it shares the file's offset).
fn rewound(file: &mut File) -> io::Result<File> {
    use std::io::Seek;
    file.seek(io::SeekFrom::Start(0))?;
    file.try_clone()
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
    if !meta.is_dir()
        || rdy_store::pio::owner(&meta) != rdy_store::pio::own_uid()
        || rdy_store::pio::is_shared_writable(&meta)
    {
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
    for target in TARGETS {
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

/// Checks the verified manifest: exactly the two archive lines for `version`, in target order,
/// each 64 lowercase hexadecimal digits, two spaces, the file name and LF.
fn check_manifest(bytes: &[u8], version: &str) -> Result<(), InstallFailure> {
    let bad = InstallFailure::Checksums;
    let text = std::str::from_utf8(bytes).map_err(|_| bad)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.len() != TARGETS.len() {
        return Err(bad);
    }
    for (line, target) in lines.iter().zip(TARGETS) {
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
            binary: input.prefix.join("bin").join("rdy"),
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
    if printed.stdout != format!("{}\n", input.signer_line).as_bytes() {
        return Err(Signature);
    }
    let signers = owned.signers.as_mut().ok_or(Signature)?;
    signers
        .write_all(&printed.stdout)
        .and_then(|()| signers.sync_all())
        .map_err(|_| Signature)?;
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
    let verified = unchanged(&dl.dir, SIGNERS_FILE, signers, &printed.stdout)
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
    check_manifest(&dl.manifest.bytes, &input.version)?;
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
    let stem =
        rdy_store::pio::open_dir_at(&unpack.dir, guide.stem.as_ref()).map_err(|_| Install)?;
    let extracted = open_regular_at(&stem, "rdy", BINARY_MAX).ok_or(Install)?;
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
        open_regular_at(bin, "rdy", BINARY_MAX)
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
    let mut path = bin.as_os_str().to_os_string();
    path.push(":");
    path.push(SMOKE_PATH);
    let start = smoke.dir.dir.try_clone().map_err(|_| Daemon)?;
    let product = |args: &[&OsStr], stage: InstallFailure| -> Result<Command, InstallFailure> {
        let mut cmd = Command::new(rdy);
        cmd.args(args)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .env("PATH", &path)
            .env("XDG_CONFIG_HOME", &config);
        platform::start_in_dir(&mut cmd, &start).map_err(|_| stage)?;
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
/// and the checksum tool of `target`.
pub fn install_tools_available(target: &str) -> bool {
    let sums = if target == MAC_TARGET {
        "shasum"
    } else {
        "sha256sum"
    };
    ["ssh-keygen", "tar", "install", sums].iter().all(|tool| {
        let mut cmd = Command::new(tool);
        cmd.arg("--version");
        // a tool that runs at all is present, whatever it thinks of the flag
        platform::run_bounded(cmd, None, RELEASE_CHILD_OUTPUT_MAX, RELEASE_CHILD_DEADLINE).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::test_support::{fifo, fifo_line, unix_socket};
    use crate::platform::{Expiry, mode_of, run_bounded, run_bounded_inner, shell_script};
    use crate::skip::{self, Why};
    use std::sync::Mutex;

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
            native_target: None,
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
        if preview {
            assert!(
                fs::read(into.join(&stem).join(NOTE_FILE)).expect("PREVIEW.txt")
                    == PREVIEW_NOTE.as_bytes(),
                "PREVIEW.txt is the note"
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
    fn pre1_package_calls_unopenable_nonregular_inputs_invalid() {
        let probe = printing("rdy 0.14.0\n");
        // /dev/tty opens only with a controlling terminal; missing or opened, it is not regular
        let f = Fixture::new();
        let before = f.state();
        assert_eq!(
            package_with(
                Path::new("/dev/tty"),
                V,
                TARGET,
                &f.out(),
                &setup(&f, &probe)
            ),
            Err(Failure::InputFile),
            "/dev/tty"
        );
        assert_eq!(f.state(), before, "/dev/tty: nothing changed");
        assert!(!f.out().exists(), "/dev/tty: no OUT");
        // a socket never opens; a FIFO without read access does not open for this user
        let socket: Spoil = |p| {
            fs::remove_file(p).expect("rm");
            drop(unix_socket(p));
        };
        let locked_fifo: Spoil = |p| {
            fs::remove_file(p).expect("rm");
            fifo(p);
            platform::set_mode(p, 0o000).expect("chmod");
        };
        for (what, spoil) in [("a socket", socket), ("a locked fifo", locked_fifo)] {
            for input in ["binary", MIT_FILE, APACHE_FILE] {
                let f = Fixture::new();
                let path = if input == "binary" {
                    f.binary.clone()
                } else {
                    f.licenses.join(input)
                };
                spoil(&path);
                let before = f.state();
                let got = package_with(&f.binary, V, TARGET, &f.out(), &setup(&f, &probe));
                assert_eq!(got, Err(Failure::InputFile), "{input} as {what}");
                assert_eq!(f.state(), before, "{input} as {what}: nothing changed");
                assert!(!f.out().exists(), "{input} as {what}: no OUT");
            }
        }
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
    fn pre1_package_refuses_a_target_that_is_not_this_machines() {
        // The version probe runs the binary here, so the real release packages only for this
        // machine's target: the other triple is an argument error, before any file work. On a
        // machine with no release target, both are.
        let native = crate::platform::release_target().ok();
        let mut refused = 0;
        for target in TARGETS {
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
        assert_eq!(refused, if native.is_some() { 1 } else { 2 });
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

    // ----- the install check -----

    const IV: &str = "0.14.0";
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    /// The hang guard of a test helper's own tool runs.
    const GUARD: std::time::Duration = std::time::Duration::from_secs(600);
    const MCP_INIT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"rdy","version":"0.14.0"}}}"#;
    const MCP_SESSION: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"actor\":\"agent:release-check\"}"}]}}"#;
    const MCP_OVERVIEW: &str =
        r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"docs\":[]}"}]}}"#;

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

    /// The SHA-256 of `file` in lowercase hex, from the host's checksum tool.
    fn sha256(file: &Path) -> String {
        let out = if host_target() == MAC_TARGET {
            tool("shasum", &["-a".as_ref(), "256".as_ref(), file.as_os_str()])
        } else {
            tool("sha256sum", &[file.as_os_str()])
        };
        String::from_utf8(out).expect("utf-8")[..64].to_string()
    }

    /// A fake `rdy` (a shell script): it logs each call's arguments to `marker`, prints
    /// `version` for `--version`, answers the smoke commands and the MCP session, and stops its
    /// "daemon" with exit `stop`.
    fn fake_rdy(marker: &Path, version: &str, stop: i32) -> String {
        fake_rdy_with(marker, version, stop, "")
    }

    /// [`fake_rdy`] whose MCP server runs `mcp_end` once its input closes.
    fn fake_rdy_with(marker: &Path, version: &str, stop: i32, mcp_end: &str) -> String {
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{marker}'
case "$1" in
--version) printf '%s\n' '{version}' ;;
init) mkdir "$2" ;;
mcp)
  read -r l
  printf '%s\n' '{init}'
  read -r l
  read -r l
  printf '%s\n' '{session}'
  read -r l
  printf '%s\n' '{overview}'
  cat > /dev/null
  {mcp_end} ;;
--project)
  case "$3" in
  --json) printf '%s\n' '{{"views":[]}}' ;;
  serve) exit {stop} ;;
  esac ;;
esac
"#,
            marker = marker.display(),
            init = MCP_INIT,
            session = MCP_SESSION,
            overview = MCP_OVERVIEW,
        )
    }

    /// A download kit: a private DOWNLOAD holding a test archive for `target` with a fake
    /// `rdy`, its two-line manifest and the manifest's signature by a disposable key.
    struct Kit {
        dir: tempfile::TempDir,
        download: PathBuf,
        key: PathBuf,
        signer: String,
        target: &'static str,
        marker: PathBuf,
        scratch: PathBuf,
    }

    impl Kit {
        fn new(target: &'static str) -> Kit {
            Kit::with_rdy(target, "rdy 0.14.0 (preview)", 0)
        }

        fn with_rdy(target: &'static str, version: &str, stop: i32) -> Kit {
            Kit::with_fake(target, &|marker| fake_rdy(marker, version, stop))
        }

        /// A kit whose archive's `rdy` is `fake(marker)`.
        fn with_fake(target: &'static str, fake: &dyn Fn(&Path) -> String) -> Kit {
            let dir = tempfile::tempdir().expect("tempdir");
            let download = dir.path().join("down load");
            let scratch = dir.path().join("scratch");
            for d in [&download, &scratch, &dir.path().join("pre fix")] {
                fs::create_dir(d).expect("mkdir");
            }
            platform::set_mode(&download, 0o700).expect("private download");
            fs::write(download.join("keep"), b"sentinel\n").expect("sentinel");
            let key = dir.path().join("key");
            let signer = test_key(&key);
            let marker = dir.path().join("ran");
            let kit = Kit {
                download,
                key,
                signer,
                target,
                marker,
                scratch,
                dir,
            };
            kit.build_archive(target, &fake(&kit.marker));
            kit.write_manifest(kit.lines().concat().as_bytes());
            kit
        }

        fn archive(&self, target: &str) -> PathBuf {
            self.download.join(archive_name(IV, target))
        }

        /// Writes the archive for `target` holding `rdy` and the other three files.
        fn build_archive(&self, target: &str, rdy: &str) {
            let stem = archive_stem(IV, target);
            let src = self.dir.path().join(format!("src-{target}"));
            let root = src.join(&stem);
            fs::create_dir_all(&root).expect("mkdir");
            fs::write(root.join("rdy"), rdy).expect("rdy");
            platform::set_mode(&root.join("rdy"), 0o755).expect("chmod");
            fs::write(root.join(MIT_FILE), MIT).expect("mit");
            fs::write(root.join(APACHE_FILE), APACHE).expect("apache");
            fs::write(root.join(NOTE_FILE), PREVIEW_NOTE).expect("note");
            self.retar(target);
        }

        /// The archive's source directory for `target` (`<stem>` holds its files).
        fn source(&self, target: &str) -> PathBuf {
            self.dir
                .path()
                .join(format!("src-{target}"))
                .join(archive_stem(IV, target))
        }

        /// Writes the archive for `target` again from its source directory, and the manifest
        /// for it.
        fn retar(&self, target: &str) {
            let src = self.dir.path().join(format!("src-{target}"));
            let _ = fs::remove_file(self.archive(target));
            let mut cmd = Command::new("tar");
            cmd.arg("-czf")
                .arg(self.archive(target))
                .arg("-C")
                .arg(&src)
                .arg(archive_stem(IV, target))
                .env("COPYFILE_DISABLE", "1");
            let out = run_bounded(cmd, None, 1 << 20, GUARD).expect("tar");
            assert!(out.status.success(), "tar");
            if self.download.join(MANIFEST_FILE).exists() {
                self.write_manifest(self.lines().concat().as_bytes());
            }
        }

        /// The two manifest lines: each present archive's hash, else a stand-in hash.
        fn lines(&self) -> Vec<String> {
            TARGETS
                .iter()
                .map(|t| {
                    let a = self.archive(t);
                    let hash = if a.exists() {
                        sha256(&a)
                    } else {
                        "ab".repeat(32)
                    };
                    format!("{hash}  {}\n", archive_name(IV, t))
                })
                .collect()
        }

        /// Writes `bytes` as the manifest and signs it with the kit's key.
        fn write_manifest(&self, bytes: &[u8]) {
            let manifest = self.download.join(MANIFEST_FILE);
            fs::write(&manifest, bytes).expect("manifest");
            sign(&self.key, &manifest, RELEASE_NAMESPACE);
        }

        fn input(&self) -> InstallInput {
            InstallInput {
                download: self.download.clone(),
                signer_line: self.signer.clone(),
                version: IV.into(),
                commit: COMMIT.into(),
                target: self.target.into(),
                unpack: self.dir.path().join("un pack"),
                prefix: self.dir.path().join("pre fix").join("rdy-0.14.0"),
                browser: false,
            }
        }

        /// Every file in DOWNLOAD with its bytes, sorted.
        fn download_state(&self) -> Vec<(String, Vec<u8>)> {
            let mut v: Vec<(String, Vec<u8>)> = names(&self.download)
                .into_iter()
                .map(|n| {
                    let bytes = fs::read(self.download.join(&n)).unwrap_or_default();
                    (n, bytes)
                })
                .collect();
            v.sort();
            v
        }

        /// The fake rdy's call log.
        fn ran(&self) -> Vec<String> {
            fs::read_to_string(&self.marker)
                .map(|t| t.lines().map(str::to_string).collect())
                .unwrap_or_default()
        }

        /// Asserts that nothing was extracted, installed or run, and that the signer file is
        /// gone again.
        fn assert_untouched(&self, input: &InstallInput, what: &str) {
            assert!(absent(&input.unpack), "{what}: no UNPACK");
            assert!(absent(&input.prefix), "{what}: no PREFIX");
            assert!(!self.marker.exists(), "{what}: the binary never ran");
            assert!(
                absent(&self.download.join(SIGNERS_FILE)),
                "{what}: the signer file is removed"
            );
            assert!(
                names(&self.scratch).is_empty(),
                "{what}: no scratch project"
            );
        }
    }

    /// The test's stand-in for each guide command: the real tool, except the checksum command
    /// of the other target (the host's native tool instead) and `xattr`, which reports
    /// `attributes` (a printf text) and removes nothing.
    fn stand_in(c: &GuideCommand, attributes: &str) -> Command {
        if c.program == "shasum" && host_target() != MAC_TARGET {
            let mut cmd = Command::new("sha256sum");
            cmd.args(["-c", "--ignore-missing", MANIFEST_FILE]);
            return cmd;
        }
        if c.program == "sha256sum" && host_target() == MAC_TARGET {
            let mut cmd = Command::new("shasum");
            cmd.args(["-a", "256", "-c", "--ignore-missing", MANIFEST_FILE]);
            return cmd;
        }
        if c.program == "xattr" {
            return if c.args.first().is_some_and(|a| a == "-d") {
                shell_script("exit 0")
            } else {
                shell_script(&format!("printf '{attributes}'"))
            };
        }
        guide_process(c)
    }

    /// A guide command's argv as text.
    fn argv(c: &GuideCommand) -> Vec<String> {
        std::iter::once(&c.program)
            .chain(&c.args)
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Runs the check of `input` against `kit`, as a machine of the kit's target. `process`
    /// stands in for the tools; `edit` changes the setup.
    fn check_kit<'a>(
        kit: &Kit,
        input: &InstallInput,
        process: &'a dyn Fn(&GuideCommand) -> Command,
        edit: impl FnOnce(&mut InstallSetup<'a>),
    ) -> Result<InstalledRelease, InstallFailure> {
        let native: &'static dyn Fn() -> io::Result<&'static str> = if kit.target == MAC_TARGET {
            &as_mac
        } else {
            &as_linux
        };
        let mut setup = InstallSetup {
            native_target: native,
            os: &platform::release_os,
            process,
            ran: &nothing_ran,
            scratch: kit.scratch.clone(),
            max_output: RELEASE_CHILD_OUTPUT_MAX,
            deadline: RELEASE_CHILD_DEADLINE,
        };
        edit(&mut setup);
        check_with(input, &setup)
    }

    /// A macOS arm64 machine.
    fn as_mac() -> io::Result<&'static str> {
        Ok(TARGETS[0])
    }

    /// A Linux amd64 machine.
    fn as_linux() -> io::Result<&'static str> {
        Ok(TARGETS[1])
    }

    /// [`check_kit`] with the plain stand-ins and the setup unchanged.
    fn check_plain(kit: &Kit, input: &InstallInput) -> Result<InstalledRelease, InstallFailure> {
        check_kit(kit, input, &|c| stand_in(c, ""), |_| {})
    }

    /// Runs the check of `input` against `kit` with the plain stand-ins (`attributes` for
    /// `xattr`): its result and every guide command its runner ran, as the runner's hook saw
    /// it. From the first guide command to the last, the runner starts no process but these
    /// commands: the count of every process it starts goes up by exactly one per command.
    fn check_recorded(
        kit: &Kit,
        input: &InstallInput,
        attributes: &str,
    ) -> (Result<InstalledRelease, InstallFailure>, Vec<Shown>) {
        let ran = Mutex::new(Vec::new());
        let record = |c: &GuideCommand| {
            let started = platform::runner_starts();
            ran.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((shown(c), started));
        };
        let process = |c: &GuideCommand| stand_in(c, attributes);
        let got = check_kit(kit, input, &process, |s| s.ran = &record);
        let ran = ran.into_inner().unwrap_or_else(|e| e.into_inner());
        let first = ran.first().map_or(0, |r| r.1);
        let between: Vec<(String, Option<usize>)> = ran
            .iter()
            .enumerate()
            .map(|(i, r)| (r.0.0.clone(), r.1.checked_sub(first + i)))
            .collect();
        assert!(
            between.iter().all(|b| b.1 == Some(0)),
            "processes started before each guide command that are not guide commands: {between:?}"
        );
        (got, ran.into_iter().map(|r| r.0).collect())
    }

    /// The argv of every guide command the check ran, in order: a log kept outside every
    /// directory the check owns, so its cleanup cannot remove the evidence.
    struct Log(Mutex<Vec<Vec<String>>>);

    impl Log {
        fn new() -> Log {
            Log(Mutex::new(Vec::new()))
        }

        fn push(&self, c: &GuideCommand) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(argv(c));
        }

        fn all(&self) -> Vec<Vec<String>> {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        /// Whether a command whose program is `program` ran.
        fn ran(&self, program: &str) -> bool {
            self.all().iter().any(|a| a[0] == program)
        }

        /// Whether anything was extracted, installed or executed from the archive: tar,
        /// install, or a run of an installed `rdy`.
        fn touched_the_archive(&self) -> bool {
            self.all()
                .iter()
                .any(|a| a[0] == "tar" || a[0] == "install" || a[0].ends_with("rdy"))
        }
    }

    /// [`check_kit`] with the plain stand-ins, each command logged to `log`.
    fn check_logged(
        kit: &Kit,
        input: &InstallInput,
        log: &Log,
    ) -> Result<InstalledRelease, InstallFailure> {
        check_kit(
            kit,
            input,
            &|c| {
                log.push(c);
                stand_in(c, "")
            },
            |_| {},
        )
    }

    /// Points the symbolic link `link` at `target` in one step: a new link renamed over it.
    fn relink(link: &Path, target: &Path) {
        let tmp = link.with_extension("relink");
        platform::symlink(target, &tmp).expect("symlink");
        fs::rename(&tmp, link).expect("rename the link");
    }

    #[test]
    fn pre3_retargeted_download_cannot_substitute_checked_files() {
        if !install_tools_or_skip(&crate::test_name!(
            "pre3_retargeted_download_cannot_substitute_checked_files"
        )) {
            return;
        }
        let target = host_target();
        // The kit's key A signs the manifest, but the trusted key is B. DOWNLOAD is reached
        // through a link, which is pointed, right before OpenSSH runs, at a directory with the
        // same manifest, archive and signature and an allowed-signers file that names A.
        let kit = Kit::new(target);
        let impostor = kit.dir.path().join("impostor");
        fs::create_dir(&impostor).expect("mkdir");
        platform::set_mode(&impostor, 0o700).expect("private");
        for name in [
            MANIFEST_FILE.to_string(),
            SIGNATURE_FILE.to_string(),
            archive_name(IV, target),
        ] {
            fs::copy(kit.download.join(&name), impostor.join(&name)).expect("copy");
        }
        fs::write(impostor.join(SIGNERS_FILE), format!("{}\n", kit.signer)).expect("signers");
        let link = kit.dir.path().join("download link");
        platform::symlink(&kit.download, &link).expect("symlink");
        let mut input = kit.input();
        input.download = link.clone();
        input.signer_line = test_key(&kit.dir.path().join("trusted-key"));
        let log = Log::new();
        let process = |c: &GuideCommand| {
            log.push(c);
            if c.program == "ssh-keygen" {
                relink(&link, &impostor);
            }
            stand_in(c, "")
        };
        assert_eq!(
            check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
            Err(InstallFailure::Signature),
            "the trusted key B did not sign the manifest"
        );
        assert!(!log.touched_the_archive(), "{:?}", log.all());
        kit.assert_untouched(&input, "a retargeted download");

        // The link is pointed at another kit's download, whose archive of the same name holds
        // another binary: while the checksum tool runs, or right before tar runs. tar reads
        // the archive through DOWNLOAD's path, as the guide says, so the check confirms after
        // each command that DOWNLOAD's path still names the checked directory and its checked
        // files. A moved link fails the stage that was running, and neither binary runs or is
        // installed.
        for (when, want) in [
            ("sha256sum", InstallFailure::Checksums),
            ("tar", InstallFailure::Install),
        ] {
            let kit = Kit::new(target);
            let other = Kit::new(target);
            let link = kit.dir.path().join("download link");
            platform::symlink(&kit.download, &link).expect("symlink");
            let mut input = kit.input();
            input.download = link.clone();
            let log = Log::new();
            let process = |c: &GuideCommand| {
                log.push(c);
                let checksum = c.program == "shasum" || c.program == "sha256sum";
                if (when == "sha256sum" && checksum) || c.program == when {
                    relink(&link, &other.download);
                }
                stand_in(c, "")
            };
            assert_eq!(
                check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
                Err(want),
                "the link moved at {when}"
            );
            assert!(
                !log.ran("install") && !log.all().iter().any(|a| a[0].ends_with("/rdy")),
                "{when}: nothing installed or run: {:?}",
                log.all()
            );
            assert_eq!(log.ran("tar"), when == "tar", "{when}: {:?}", log.all());
            assert!(other.ran().is_empty(), "{when}: the other binary never ran");
            assert!(kit.ran().is_empty(), "{when}: the checked binary never ran");
            assert!(absent(&input.unpack) && absent(&input.prefix), "{when}");
            assert!(
                absent(&kit.download.join(SIGNERS_FILE))
                    && absent(&other.download.join(SIGNERS_FILE)),
                "{when}: the signer file is removed, and none is left in the other download"
            );
        }
    }

    #[test]
    fn pre3_openssh_accepts_only_the_trusted_manifest() {
        if !install_tools_or_skip(&crate::test_name!(
            "pre3_openssh_accepts_only_the_trusted_manifest"
        )) {
            return;
        }
        // The trusted key verifies the two-line manifest offline, signed in rdy-release-v1 with
        // sha512, and the release installs.
        let kit = Kit::new(host_target());
        let input = kit.input();
        assert_eq!(kit.lines().len(), 2);
        let before = kit.download_state();
        check_plain(&kit, &input).expect("the trusted manifest");
        assert_eq!(kit.download_state(), before, "the download is unchanged");

        // Each other key, principal, namespace or manifest byte fails at the signature, before
        // any line of the manifest is read.
        type Spoil = fn(&Kit, &mut InstallInput);
        let cases: [(&str, Spoil); 5] = [
            ("another key", |kit, input| {
                input.signer_line = test_key(&kit.dir.path().join("other-key"));
            }),
            ("another namespace", |kit, _| {
                sign(
                    &kit.key,
                    &kit.download.join(MANIFEST_FILE),
                    "rdy-release-v2",
                );
            }),
            ("a signature of other bytes", |kit, _| {
                let other = kit.dir.path().join("other");
                fs::write(&other, b"other bytes\n").expect("write");
                sign(&kit.key, &other, RELEASE_NAMESPACE);
                fs::copy(
                    kit.dir.path().join("other.sig"),
                    kit.download.join(SIGNATURE_FILE),
                )
                .expect("copy");
            }),
            ("a one-byte change of the manifest", |kit, _| {
                let manifest = kit.download.join(MANIFEST_FILE);
                let mut bytes = fs::read(&manifest).expect("read");
                bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
                fs::write(&manifest, bytes).expect("write");
            }),
            ("a signature that is not SSHSIG", |kit, _| {
                fs::write(kit.download.join(SIGNATURE_FILE), b"not a signature\n").expect("write");
            }),
        ];
        for (what, spoil) in cases {
            let kit = Kit::new(host_target());
            let mut input = kit.input();
            spoil(&kit, &mut input);
            let before = kit.download_state();
            let log = Log::new();
            assert_eq!(
                check_logged(&kit, &input, &log).map(|_| ()),
                Err(InstallFailure::Signature),
                "{what}"
            );
            assert!(!log.touched_the_archive(), "{what}: {:?}", log.all());
            kit.assert_untouched(&input, what);
            assert_eq!(
                kit.download_state(),
                before,
                "{what}: the download is unchanged"
            );
        }

        // The principal: OpenSSH matches the identity to the allowed signer's principal.
        let kit = Kit::new(host_target());
        let input = kit.input();
        let other_principal = |c: &GuideCommand| {
            let mut c = c.clone();
            if c.program == "ssh-keygen" {
                for a in &mut c.args {
                    if a == RELEASE_PRINCIPAL {
                        *a = "someone-else".into();
                    }
                }
            }
            stand_in(&c, "")
        };
        assert_eq!(
            check_kit(&kit, &input, &other_principal, |_| {}).map(|_| ()),
            Err(InstallFailure::Signature),
            "another principal"
        );
        kit.assert_untouched(&input, "another principal");
        // ... and a signer line naming another principal or namespace is not a release line.
        for line in [
            kit.signer.replacen("rdy-release ", "someone-else ", 1),
            kit.signer.replacen("rdy-release-v1", "rdy-release-v2", 1),
        ] {
            let mut input = kit.input();
            input.signer_line = line;
            assert_eq!(
                check_plain(&kit, &input).map(|_| ()),
                Err(InstallFailure::Input)
            );
        }

        // A missing or empty signature is an input failure.
        for (what, spoil) in [
            (
                "missing",
                (|p: &Path| fs::remove_file(p).expect("rm")) as fn(&Path),
            ),
            ("empty", |p: &Path| fs::write(p, b"").expect("write")),
        ] {
            let kit = Kit::new(host_target());
            let input = kit.input();
            spoil(&kit.download.join(SIGNATURE_FILE));
            assert_eq!(
                check_plain(&kit, &input).map(|_| ()),
                Err(InstallFailure::Input),
                "a {what} signature"
            );
            kit.assert_untouched(&input, what);
        }
    }

    #[test]
    fn pre3_hash_failures_never_extract_or_execute() {
        if !install_tools_or_skip(&crate::test_name!(
            "pre3_hash_failures_never_extract_or_execute"
        )) {
            return;
        }
        let target = host_target();
        let selected = TARGETS.iter().position(|t| *t == target).expect("a target");
        type Spoil = fn(&Kit, usize);
        let cases: [(&str, Spoil, InstallFailure); 10] = [
            (
                "an altered selected archive",
                |kit, _| {
                    let a = kit.archive(kit.target);
                    let mut bytes = fs::read(&a).expect("read");
                    bytes.push(0);
                    fs::write(&a, bytes).expect("write");
                },
                InstallFailure::Checksums,
            ),
            (
                "the selected line omitted",
                |kit, sel| {
                    let lines = kit.lines();
                    kit.write_manifest(lines[1 - sel].as_bytes());
                },
                InstallFailure::Checksums,
            ),
            (
                "a line duplicated",
                |kit, sel| {
                    let lines = kit.lines();
                    kit.write_manifest(format!("{0}{0}", lines[sel]).as_bytes());
                },
                InstallFailure::Checksums,
            ),
            (
                "the lines reordered",
                |kit, _| {
                    let lines = kit.lines();
                    kit.write_manifest(format!("{}{}", lines[1], lines[0]).as_bytes());
                },
                InstallFailure::Checksums,
            ),
            (
                "a third line",
                |kit, _| {
                    let mut text = kit.lines().concat();
                    text.push_str(&format!("{}  extra.tar.gz\n", "cd".repeat(32)));
                    kit.write_manifest(text.as_bytes());
                },
                InstallFailure::Checksums,
            ),
            (
                "CRLF line ends",
                |kit, _| {
                    kit.write_manifest(kit.lines().concat().replace('\n', "\r\n").as_bytes());
                },
                InstallFailure::Checksums,
            ),
            (
                "no final LF",
                |kit, _| {
                    let text = kit.lines().concat();
                    kit.write_manifest(text.trim_end_matches('\n').as_bytes());
                },
                InstallFailure::Checksums,
            ),
            (
                "the selected archive missing",
                |kit, _| fs::remove_file(kit.archive(kit.target)).expect("rm"),
                InstallFailure::Input,
            ),
            (
                "a failed signature",
                |kit, _| {
                    let lines = kit.lines();
                    fs::write(
                        kit.download.join(MANIFEST_FILE),
                        lines.concat().replace("  ", "   "),
                    )
                    .expect("write");
                },
                InstallFailure::Signature,
            ),
            (
                "a present nonselected archive that does not match",
                |kit, sel| {
                    let lines = kit.lines();
                    fs::write(kit.archive(TARGETS[1 - sel]), b"not the archive\n").expect("write");
                    kit.write_manifest(lines.concat().as_bytes());
                },
                InstallFailure::Checksums,
            ),
        ];
        for (what, spoil, want) in cases {
            let kit = Kit::new(target);
            let input = kit.input();
            spoil(&kit, selected);
            let before = kit.download_state();
            // the command log is kept outside what the cleanup removes: neither tar nor
            // install nor the binary ever ran
            let log = Log::new();
            assert_eq!(
                check_logged(&kit, &input, &log).map(|_| ()),
                Err(want),
                "{what}"
            );
            assert!(!log.touched_the_archive(), "{what}: {:?}", log.all());
            kit.assert_untouched(&input, what);
            assert_eq!(
                kit.download_state(),
                before,
                "{what}: the download is unchanged"
            );
        }

        // The nonselected archive may be absent (the kit's default) or present and right.
        for present in [false, true] {
            let kit = Kit::new(target);
            if present {
                kit.build_archive(TARGETS[1 - selected], "the other target's binary\n");
                kit.write_manifest(kit.lines().concat().as_bytes());
            }
            let input = kit.input();
            let log = Log::new();
            let report = check_logged(&kit, &input, &log)
                .unwrap_or_else(|f| panic!("present={present}: {}", f.message()));
            assert_eq!(report.skipped, 0);
            assert!(
                log.ran("tar"),
                "the log sees an extraction: {:?}",
                log.all()
            );
            assert_eq!(kit.ran().first().map(String::as_str), Some("--version"));
        }
    }

    /// A guide command as (program, args, cwd, stdin, stdout) text.
    type Shown = (String, Vec<String>, String, Option<String>, Option<String>);

    fn shown(c: &GuideCommand) -> Shown {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        (
            c.program.to_string_lossy().into_owned(),
            c.args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            text(&c.cwd),
            c.stdin.as_deref().map(text),
            c.stdout.as_deref().map(text),
        )
    }

    /// A signer line from fixed key bytes.
    fn sample_signer() -> String {
        let key = ssh_key::PublicKey::new(
            ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([7u8; 32])),
            "",
        );
        format!("{SIGNER_PREFIX}{}", key.to_openssh().expect("openssh"))
    }

    /// What is wrong with the README's install blocks: in the pending state there are none; in
    /// the ready state the README holds the generated section for its V and signer line.
    fn readme_install_problem(readme: &str) -> Result<(), String> {
        let generated_marks = [
            "'ssh-keygen' '-Y' 'verify'",
            "### Install\n",
            "export PATH=\"<version-prefix>/bin:$PATH\"",
            "'install' '-m' '755'",
        ];
        if readme.contains("Release signing key: pending founder confirmation.") {
            return match generated_marks.iter().find(|m| readme.contains(**m)) {
                Some(m) => Err(format!("the pending state holds an install block: {m}")),
                None => Ok(()),
            };
        }
        let signer = readme
            .split("### Release signing key\n")
            .nth(1)
            .and_then(|rest| rest.split("```text\n").nth(1))
            .and_then(|rest| rest.lines().next())
            .ok_or("no signer line in the ready state")?;
        let v = readme
            .split("/releases/download/v")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .ok_or("no V in the README")?;
        let section = readme_install_section(v, signer)?;
        if readme.contains(&section) {
            Ok(())
        } else {
            Err("the README's install blocks are not the renderer's".into())
        }
    }

    #[test]
    fn pre3_guide_commands_match_execution() {
        let signer = sample_signer();
        assert!(valid_signer_line(&signer), "{signer}");
        let input = |target: &str, browser: bool| InstallInput {
            download: PathBuf::from("/nowhere/dl dir/it's"),
            signer_line: signer.clone(),
            version: IV.into(),
            commit: COMMIT.into(),
            target: target.into(),
            unpack: PathBuf::from("/nowhere/ü unpack"),
            prefix: PathBuf::from("/nowhere/p p/rdy-0.14.0"),
            browser,
        };
        let dl = "/nowhere/dl dir/it's";
        let stem = |t: &str| format!("rdy-0.14.0-preview-{t}");
        let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let want = |target: &str, browser: bool| -> Vec<Shown> {
            let archive = format!("{dl}/{}.tar.gz", stem(target));
            let at = |program: &str, args: Vec<String>| -> Shown {
                (program.into(), args, dl.into(), None, None)
            };
            let mut v = vec![
                (
                    "printf".into(),
                    s(&[r"%s\n", &signer]),
                    dl.into(),
                    None,
                    Some(format!("{dl}/release-allowed-signers")),
                ),
                (
                    "ssh-keygen".into(),
                    s(&[
                        "-Y",
                        "verify",
                        "-f",
                        "release-allowed-signers",
                        "-I",
                        "rdy-release",
                        "-n",
                        "rdy-release-v1",
                        "-s",
                        "SHA256SUMS.sig",
                    ]),
                    dl.into(),
                    Some(format!("{dl}/SHA256SUMS")),
                    None,
                ),
                if target == MAC_TARGET {
                    at(
                        "shasum",
                        s(&["-a", "256", "-c", "--ignore-missing", "SHA256SUMS"]),
                    )
                } else {
                    at("sha256sum", s(&["-c", "--ignore-missing", "SHA256SUMS"]))
                },
            ];
            if browser {
                v.push(at("xattr", s(&["-d", "com.apple.quarantine", &archive])));
            }
            v.extend([
                at(
                    "mkdir",
                    s(&["/nowhere/ü unpack", "/nowhere/p p/rdy-0.14.0"]),
                ),
                at("tar", s(&["-xzf", &archive, "-C", "/nowhere/ü unpack"])),
                at("mkdir", s(&["-p", "/nowhere/p p/rdy-0.14.0/bin"])),
                at(
                    "install",
                    s(&[
                        "-m",
                        "755",
                        &format!("/nowhere/ü unpack/{}/rdy", stem(target)),
                        "/nowhere/p p/rdy-0.14.0/bin/rdy",
                    ]),
                ),
                at("/nowhere/p p/rdy-0.14.0/bin/rdy", s(&["--version"])),
            ]);
            v
        };
        for (target, browser) in [(TARGETS[0], false), (TARGETS[1], false), (TARGETS[0], true)] {
            let got = commands(&input(target, browser)).expect("commands");
            assert_eq!(
                got.iter().map(shown).collect::<Vec<_>>(),
                want(target, browser),
                "{target} browser={browser}"
            );
        }

        // The rendered block: a cd, then one quoted line per command, each after `&&`.
        let mac = render_commands(&commands(&input(TARGETS[0], false)).expect("commands"))
            .expect("render");
        let q = r"'/nowhere/dl dir/it'\''s'";
        let qd = r"/nowhere/dl dir/it'\''s";
        assert_eq!(
            mac,
            format!(
                "cd {q} &&\n\
                 'printf' '%s\\n' '{signer}' > '{qd}/release-allowed-signers' &&\n\
                 'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '{qd}/SHA256SUMS' &&\n\
                 'shasum' '-a' '256' '-c' '--ignore-missing' 'SHA256SUMS' &&\n\
                 'mkdir' '/nowhere/ü unpack' '/nowhere/p p/rdy-0.14.0' &&\n\
                 'tar' '-xzf' '{qd}/rdy-0.14.0-preview-aarch64-apple-darwin.tar.gz' '-C' '/nowhere/ü unpack' &&\n\
                 'mkdir' '-p' '/nowhere/p p/rdy-0.14.0/bin' &&\n\
                 'install' '-m' '755' '/nowhere/ü unpack/rdy-0.14.0-preview-aarch64-apple-darwin/rdy' '/nowhere/p p/rdy-0.14.0/bin/rdy' &&\n\
                 '/nowhere/p p/rdy-0.14.0/bin/rdy' '--version'"
            )
        );
        for (target, browser) in [(TARGETS[0], false), (TARGETS[1], false), (TARGETS[0], true)] {
            let cmds = commands(&input(target, browser)).expect("commands");
            let block = render_commands(&cmds).expect("render");
            let lines: Vec<&str> = block.lines().filter(|l| !l.starts_with("# ")).collect();
            assert_eq!(lines.len(), cmds.len() + 1, "a cd and one line per command");
            assert!(lines[0].starts_with("cd "));
            let (last, rest) = lines.split_last().expect("lines");
            assert!(
                rest.iter().all(|l| l.ends_with(" &&")),
                "each command runs only after its predecessor succeeded: {block}"
            );
            assert!(!last.ends_with("&&"));
            // verification comes before any extraction, and the quarantine removal before mkdir
            let at = |word: &str| lines.iter().position(|l| l.starts_with(word)).expect(word);
            assert!(at("'ssh-keygen'") < at("'tar'"));
            assert!(at_or(&lines, "'shasum'").min(at_or(&lines, "'sha256sum'")) < at("'tar'"));
            let notes = format!("# {SAFARI_NOTE}\n# {QUARANTINE_NOTE}\n");
            if browser {
                assert!(block.starts_with(&notes), "{block}");
                assert_eq!(at("'xattr'") + 1, at("'mkdir'"));
            } else {
                assert!(!block.contains("xattr") && !block.contains("Safari"));
            }
        }
        // Refused tokens.
        let mut bad = commands(&input(TARGETS[1], false)).expect("commands");
        for token in ["a\nb", "a\rb", "a\0b"] {
            bad[0].args[1] = token.into();
            assert_eq!(
                render_commands(&bad),
                Err(MSG_INVALID_COMMAND.to_string()),
                "{token:?}"
            );
        }

        // Print mode is pure: nonexistent paths are accepted and nothing is made.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut pure = input(TARGETS[1], false);
        pure.download = dir.path().join("no download");
        pure.unpack = dir.path().join("no unpack");
        pure.prefix = dir.path().join("no parent").join("rdy-0.14.0");
        render_commands(&commands(&pure).expect("commands")).expect("render");
        assert!(names(dir.path()).is_empty(), "print mode makes nothing");
        assert_eq!(
            commands(&InstallInput {
                version: "0.14".into(),
                ..pure.clone()
            }),
            Err(MSG_INSTALL_INVALID_ARGUMENTS.to_string())
        );

        // printf's bytes are the signer line and one LF.
        let printf = &commands(&input(TARGETS[1], false)).expect("commands")[0];
        let mut cmd = guide_process(printf);
        cmd.current_dir(dir.path());
        let out = run_bounded(cmd, None, 1 << 20, GUARD).expect("printf");
        assert_eq!(out.stdout, format!("{signer}\n").into_bytes());

        // The README: no install block while the key is pending; the generated section when
        // ready.
        let readme = fs::read_to_string(repository_root().join("README.md")).expect("README");
        assert_eq!(readme_install_problem(&readme), Ok(()), "the real README");
        let pending = "Release signing key: pending founder confirmation.\n\
                       Preview downloads are not ready.\n\
                       Do not install a preview until this README and its GitHub release notes show the same key and fingerprint.\n";
        let synthetic = format!(
            "# rdy\n\nhttps://github.com/x/releases/download/v{IV}/SHA256SUMS\n\n{pending}"
        );
        assert_eq!(readme_install_problem(&synthetic), Ok(()));
        let section = readme_install_section(IV, &signer).expect("section");
        assert!(readme_install_problem(&format!("{synthetic}\n{section}")).is_err());
        let ready = synthetic.replace(
            pending,
            &format!("### Release signing key\n\n```text\n{signer}\n```\n\n{section}"),
        );
        assert_eq!(readme_install_problem(&ready), Ok(()));
        // the section holds the three blocks for the example paths, in order, each after its
        // label; a browser download on macOS keeps its quarantine mark until the third block
        let mut from = 0;
        for (label, target, browser) in [
            (
                "On macOS arm64, for an archive downloaded with curl:\n\n",
                TARGETS[0],
                false,
            ),
            ("On Linux amd64:\n\n", TARGETS[1], false),
            (
                "On macOS arm64, for an archive downloaded with a web browser:\n\n",
                TARGETS[0],
                true,
            ),
        ] {
            let example = InstallInput {
                download: "/path/to/download".into(),
                unpack: "/path/to/unpack".into(),
                prefix: "/path/to/rdy-0.14.0".into(),
                ..input(target, browser)
            };
            let block = render_commands(&commands(&example).expect("commands")).expect("render");
            let fenced = format!("{label}```sh\n{block}\n```\n");
            let at = section[from..]
                .find(&fenced)
                .unwrap_or_else(|| panic!("{target} browser={browser}: {section}"));
            from += at + fenced.len();
        }
        assert!(section.contains("Add the version's bin directory to PATH in this shell.\n"));
        let edited = ready.replacen("'--version'", "'-V'", 1);
        assert!(
            readme_install_problem(&edited).is_err(),
            "a hand edit is caught"
        );

        // The check runs these same command objects: the argv, cwd and redirections that its
        // runner starts, recorded by the runner's hook, are the guide's, element for element,
        // on macOS and on Linux. Its other steps (the version receipt, the daemon and MCP
        // checks and the cleanup) come after the guide's last command and are not guide
        // commands.
        if !install_tools_or_skip(&crate::test_name!("pre3_guide_commands_match_execution")) {
            return;
        }
        for target in TARGETS {
            let kit = Kit::new(target);
            let input = kit.input();
            let (got, ran) = check_recorded(&kit, &input, "");
            got.unwrap_or_else(|f| panic!("{target}: {}", f.message()));
            let guide: Vec<Shown> = commands(&input)
                .expect("commands")
                .iter()
                .map(shown)
                .collect();
            assert_eq!(ran, guide, "{target}: the check runs the guide's commands");
            assert!(
                ran.iter().all(|c| c.2 == input.download.to_string_lossy()),
                "{target}: every command starts in DOWNLOAD"
            );
            assert_eq!(
                ran.last().map(|c| c.0.clone()),
                Some(
                    input
                        .prefix
                        .join("bin")
                        .join("rdy")
                        .to_string_lossy()
                        .into_owned()
                ),
                "{target}: the product runs by its absolute installed path"
            );
        }
    }

    /// The position of the first of `lines` that starts with `word`, else past the end.
    fn at_or(lines: &[&str], word: &str) -> usize {
        lines
            .iter()
            .position(|l| l.starts_with(word))
            .unwrap_or(usize::MAX)
    }

    #[test]
    fn pre3_browser_quarantine_removal_follows_both_checks() {
        if !install_tools_or_skip(&crate::test_name!(
            "pre3_browser_quarantine_removal_follows_both_checks"
        )) {
            return;
        }
        // Both attribute branches, on the macOS target, with xattr stood in for.
        for (attributes, removes) in [
            (r"com.apple.provenance\ncom.apple.quarantine\n", true),
            (r"com.apple.provenance\n", false),
        ] {
            let kit = Kit::new(MAC_TARGET);
            let mut input = kit.input();
            input.browser = true;
            let (got, ran) = check_recorded(&kit, &input, attributes);
            got.unwrap_or_else(|f| panic!("{}", f.message()));
            // the guide's commands, with the probe of the verified archive right after the
            // checksum command, and the removal only when the probe names the attribute
            let guide = commands(&input).expect("commands");
            let archive = input.download.join(archive_name(IV, MAC_TARGET));
            let probe = GuideCommand::new("xattr", &[archive.as_os_str()], &input.download);
            let mut want: Vec<Shown> = guide.iter().map(shown).collect();
            assert_eq!(want[3].0, "xattr", "the guide's removal");
            assert_eq!(
                want[3].1,
                ["-d", QUARANTINE_ATTRIBUTE, &archive.to_string_lossy()]
            );
            if !removes {
                want.remove(3);
            }
            want.insert(3, shown(&probe));
            assert_eq!(ran, want, "attributes {attributes}");
        }
        // No xattr call before both checks pass.
        for spoil in [
            (|kit: &Kit| {
                let a = kit.archive(MAC_TARGET);
                let mut bytes = fs::read(&a).expect("read");
                bytes.push(0);
                fs::write(&a, bytes).expect("write");
            }) as fn(&Kit),
            |kit: &Kit| fs::write(kit.download.join(SIGNATURE_FILE), b"bad\n").expect("write"),
        ] {
            let kit = Kit::new(MAC_TARGET);
            let mut input = kit.input();
            input.browser = true;
            spoil(&kit);
            let log = Mutex::new(Vec::new());
            let process = |c: &GuideCommand| {
                log.lock().unwrap_or_else(|e| e.into_inner()).push(argv(c));
                stand_in(c, r"com.apple.quarantine\n")
            };
            assert!(check_kit(&kit, &input, &process, |_| {}).is_err());
            let ran = log.into_inner().unwrap_or_else(|e| e.into_inner());
            assert!(ran.iter().all(|a| a[0] != "xattr"), "{ran:?}");
            kit.assert_untouched(&input, "a failed check");
        }
        // A failed probe or removal is an install failure.
        for failing in ["probe", "removal"] {
            let kit = Kit::new(MAC_TARGET);
            let mut input = kit.input();
            input.browser = true;
            let process = |c: &GuideCommand| {
                let removal = c.args.first().is_some_and(|a| a == "-d");
                if c.program == "xattr" && (removal == (failing == "removal")) {
                    return shell_script("exit 1");
                }
                stand_in(c, r"com.apple.quarantine\n")
            };
            assert_eq!(
                check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
                Err(InstallFailure::Install),
                "a failed {failing}"
            );
            kit.assert_untouched(&input, failing);
        }
    }

    #[test]
    fn pre3_bounds_and_owned_cleanup() {
        if !install_tools_or_skip(&crate::test_name!("pre3_bounds_and_owned_cleanup")) {
            return;
        }
        use InstallFailure::*;
        let target = host_target();

        // Grammar, each at N and N+1 where it has a bound.
        let kit = Kit::new(target);
        let grammar = |edit: &dyn Fn(&mut InstallInput)| {
            let mut input = kit.input();
            edit(&mut input);
            guide(&input).map(|_| ())
        };
        let long = |len: usize| {
            let tail = "/rdy-0.14.0";
            PathBuf::from(format!("/{}{tail}", "a".repeat(len - 1 - tail.len())))
        };
        assert_eq!(grammar(&|i| i.prefix = long(4096)), Ok(()));
        assert_eq!(grammar(&|i| i.prefix = long(4097)), Err(Input));
        let over = format!(
            "{SIGNER_PREFIX}ssh-ed25519 {}",
            "A".repeat(8193 - SIGNER_PREFIX.len() - 12)
        );
        assert_eq!(over.len(), 8193);
        assert!(!valid_signer_line(&over));
        type Edit = fn(&mut InstallInput);
        let refused: [(&str, Edit); 17] = [
            ("a signer line one byte over", |i| {
                i.signer_line = format!(
                    "{SIGNER_PREFIX}ssh-ed25519 {}",
                    "A".repeat(8193 - SIGNER_PREFIX.len() - 12)
                );
            }),
            ("a signer comment", |i| i.signer_line.push_str(" me@host")),
            ("a signer of another key type", |i| {
                i.signer_line = i.signer_line.replacen("ssh-ed25519", "ssh-rsa", 1);
            }),
            ("a signer type that disagrees with its key", |i| {
                i.signer_line =
                    i.signer_line
                        .replacen("ssh-ed25519", "sk-ssh-ed25519@openssh.com", 1);
            }),
            ("a signer with a CR", |i| i.signer_line.push('\r')),
            ("a version of eleven minor digits", |i| {
                i.version = "0.12345678901.0".into()
            }),
            ("a commit of 39 digits", |i| i.commit.truncate(39)),
            ("a commit of 41 digits", |i| i.commit.push('0')),
            ("an uppercase commit", |i| {
                i.commit = i.commit.to_uppercase()
            }),
            ("another target", |i| {
                i.target = "aarch64-unknown-linux-gnu".into()
            }),
            ("a relative download", |i| i.download = "down load".into()),
            ("a prefix not named rdy-V", |i| {
                i.prefix.set_file_name("rdy-0.15.0")
            }),
            ("UNPACK inside DOWNLOAD", |i| {
                i.unpack = i.download.join("x")
            }),
            ("PREFIX inside UNPACK", |i| {
                i.prefix = i.unpack.join("rdy-0.14.0")
            }),
            ("UNPACK as DOWNLOAD, spelled with ..", |i| {
                i.unpack = i
                    .download
                    .join("..")
                    .join(i.download.file_name().expect("name"));
            }),
            ("UNPACK with a NUL", |i| {
                i.unpack = PathBuf::from("/tmp/a\0b")
            }),
            ("the browser path on Linux", |i| {
                i.target = TARGETS[1].into();
                i.browser = true;
            }),
        ];
        let before = kit.download_state();
        for (what, edit) in refused {
            let mut input = kit.input();
            edit(&mut input);
            assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Input), "{what}");
            kit.assert_untouched(&kit.input(), what);
            assert_eq!(
                kit.download_state(),
                before,
                "{what}: the download is unchanged"
            );
        }

        // A valid signer line at the cap. Its length is fixed by the key type, so the FIDO key's
        // application string sets it: an sk-ssh-ed25519 line is 3 mod 4 bytes long, and 8191
        // and 8195 are the valid lengths on each side of the 8192-byte cap. At the cap the line
        // passes the grammar (and fails the signature: it is not the kit's key); past it, the
        // line is still a well-formed key that only the length refuses.
        let sk_line = |len: usize| -> String {
            let fixed = SIGNER_PREFIX.len() + "sk-ssh-ed25519@openssh.com ".len();
            let base64 = len - fixed;
            assert_eq!(base64 % 4, 0, "{len}");
            // the blob: the type (4 + 26), the key (4 + 32) and the application (4 + its bytes)
            let application = base64 / 4 * 3 - (4 + 26) - (4 + 32) - 4;
            let key = ssh_key::public::SkEd25519::new(
                ssh_key::public::Ed25519PublicKey([9u8; 32]),
                format!("ssh:{}", "a".repeat(application - 4)),
            );
            let key = ssh_key::PublicKey::new(ssh_key::public::KeyData::SkEd25519(key), "");
            let line = format!("{SIGNER_PREFIX}{}", key.to_openssh().expect("openssh"));
            assert_eq!(line.len(), len);
            line
        };
        let (at_cap, past_cap) = (sk_line(SIGNER_LINE_MAX - 1), sk_line(SIGNER_LINE_MAX + 3));
        assert!(valid_signer_line(&at_cap));
        let past_key = past_cap.strip_prefix(SIGNER_PREFIX).expect("prefix");
        assert!(
            ssh_key::PublicKey::from_openssh(past_key)
                .is_ok_and(|k| k.to_openssh().is_ok_and(|again| again == past_key)),
            "past the cap, the key itself is well formed"
        );
        for (line, want) in [(at_cap, Signature), (past_cap, Input)] {
            let kit = Kit::new(target);
            let mut input = kit.input();
            let len = line.len();
            input.signer_line = line;
            assert_eq!(
                check_plain(&kit, &input).map(|_| ()),
                Err(want),
                "a signer line of {len} bytes"
            );
            kit.assert_untouched(&input, "a long signer line");
        }

        // Files in DOWNLOAD: N passes the input checks (and fails the signature), N+1 does not.
        for (name, max) in [
            (MANIFEST_FILE, MANIFEST_MAX),
            (SIGNATURE_FILE, SIGNATURE_MAX),
        ] {
            for (len, want) in [(max, Signature), (max + 1, Input)] {
                let kit = Kit::new(target);
                fs::write(kit.download.join(name), vec![b'x'; len as usize]).expect("write");
                let input = kit.input();
                assert_eq!(
                    check_plain(&kit, &input).map(|_| ()),
                    Err(want),
                    "{name} of {len}"
                );
                kit.assert_untouched(&input, name);
            }
        }
        for (len, want) in [(ARCHIVE_MAX, Signature), (ARCHIVE_MAX + 1, Input)] {
            let kit = Kit::new(target);
            fs::write(kit.download.join(SIGNATURE_FILE), b"bad\n").expect("write");
            let file = File::options()
                .write(true)
                .open(kit.archive(target))
                .expect("open");
            file.set_len(len).expect("sparse archive");
            let input = kit.input();
            assert_eq!(
                check_plain(&kit, &input).map(|_| ()),
                Err(want),
                "an archive of {len}"
            );
            kit.assert_untouched(&input, "archive");
        }

        // Symlinks, a directory, a non-directory DOWNLOAD and a shared one.
        type Fs = fn(&Kit, &InstallInput);
        let fs_cases: [(&str, Fs); 11] = [
            ("a symlinked manifest", |k, _| {
                link_in_place(&k.download.join(MANIFEST_FILE))
            }),
            ("a symlinked signature", |k, _| {
                link_in_place(&k.download.join(SIGNATURE_FILE))
            }),
            ("a symlinked archive", |k, _| {
                link_in_place(&k.archive(k.target))
            }),
            ("a symlinked other archive", |k, _| {
                let other = TARGETS.iter().find(|t| **t != k.target).expect("other");
                platform::symlink(&k.archive(k.target), &k.archive(other)).expect("symlink");
            }),
            ("an archive that is a directory", |k, _| {
                let a = k.archive(k.target);
                fs::remove_file(&a).expect("rm");
                fs::create_dir(&a).expect("mkdir");
            }),
            ("a shared DOWNLOAD", |k, _| {
                platform::set_mode(&k.download, 0o770).expect("chmod")
            }),
            ("an occupied signer file", |k, _| {
                fs::write(k.download.join(SIGNERS_FILE), b"sentinel\n").expect("write");
            }),
            ("an occupied UNPACK", |_, i| {
                fs::create_dir(&i.unpack).expect("mkdir");
                fs::write(i.unpack.join("keep"), b"sentinel\n").expect("write");
            }),
            ("an occupied PREFIX", |_, i| {
                fs::create_dir(&i.prefix).expect("mkdir");
                fs::write(i.prefix.join("keep"), b"sentinel\n").expect("write");
            }),
            ("a dangling link at PREFIX", |k, i| {
                platform::symlink(&k.dir.path().join("nowhere"), &i.prefix).expect("symlink");
            }),
            ("UNPACK under a link to DOWNLOAD", |k, _| {
                platform::symlink(&k.download, &k.dir.path().join("link")).expect("symlink");
            }),
        ];
        for (what, prepare) in fs_cases {
            let kit = Kit::new(target);
            let mut input = kit.input();
            if what == "UNPACK under a link to DOWNLOAD" {
                input.unpack = kit.dir.path().join("link").join("x");
            }
            prepare(&kit, &input);
            let before = kit.download_state();
            let unpack_before = state_of(&input.unpack);
            let prefix_before = state_of(&input.prefix);
            assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Input), "{what}");
            assert_eq!(
                kit.download_state(),
                before,
                "{what}: the download is unchanged"
            );
            assert_eq!(
                state_of(&input.unpack),
                unpack_before,
                "{what}: UNPACK is unchanged"
            );
            assert_eq!(
                state_of(&input.prefix),
                prefix_before,
                "{what}: PREFIX is unchanged"
            );
            assert!(!kit.marker.exists(), "{what}: nothing ran");
        }
        let kit = Kit::new(target);
        let mut input = kit.input();
        input.unpack = kit.dir.path().join("no parent").join("x");
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(Input),
            "UNPACK without a parent"
        );
        input = kit.input();
        input.download = kit.download.join(MANIFEST_FILE);
        assert_eq!(
            check_plain(&kit, &input).map(|_| ()),
            Err(Input),
            "DOWNLOAD is a file"
        );

        // The machine: another target, an unknown one, or a failed OS probe.
        let kit = Kit::new(target);
        let input = kit.input();
        let other = TARGETS
            .iter()
            .copied()
            .find(|t| *t != target)
            .expect("other");
        let wrong = move || Ok(other);
        let unsupported = || -> io::Result<&'static str> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no release"))
        };
        let no_os = || -> io::Result<String> { Err(io::Error::other("uname failed")) };
        let late_os =
            || -> io::Result<String> { Err(io::Error::new(io::ErrorKind::TimedOut, "late")) };
        let process = |c: &GuideCommand| stand_in(c, "");
        let results = [
            (
                "another native target",
                Input,
                check_kit(&kit, &input, &process, |s| s.native_target = &wrong),
            ),
            (
                "an unsupported machine",
                Input,
                check_kit(&kit, &input, &process, |s| s.native_target = &unsupported),
            ),
            (
                "a failed OS probe",
                Input,
                check_kit(&kit, &input, &process, |s| s.os = &no_os),
            ),
            (
                "an OS probe past its deadline",
                Deadline,
                check_kit(&kit, &input, &process, |s| s.os = &late_os),
            ),
        ];
        for (what, want, got) in results {
            assert_eq!(got.map(|_| ()), Err(want), "{what}");
            kit.assert_untouched(&input, what);
        }

        // Bounded children: output past the cap, and a child past its deadline (injected).
        let noisy = |c: &GuideCommand| {
            if c.program == "ssh-keygen" {
                return shell_script("head -c 1048577 /dev/zero");
            }
            stand_in(c, "")
        };
        let hanging = |c: &GuideCommand| {
            if c.program == "ssh-keygen" {
                return shell_script("exec sleep 1000");
            }
            stand_in(c, "")
        };
        for (what, want, process, deadline) in [
            (
                "output past the cap",
                OutputLimit,
                &noisy as &dyn Fn(&GuideCommand) -> Command,
                RELEASE_CHILD_DEADLINE,
            ),
            (
                "a child past its deadline",
                Deadline,
                &hanging,
                std::time::Duration::ZERO,
            ),
        ] {
            let kit = Kit::new(target);
            let input = kit.input();
            let before = kit.download_state();
            assert_eq!(
                check_kit(&kit, &input, process, |s| s.deadline = deadline).map(|_| ()),
                Err(want),
                "{what}"
            );
            kit.assert_untouched(&input, what);
            assert_eq!(
                kit.download_state(),
                before,
                "{what}: the download is unchanged"
            );
        }

        // An extracted rdy that is missing, a link, a directory or empty is never installed.
        type Spoil = fn(&Path);
        let extracted: [(&str, Spoil); 4] = [
            ("missing", |rdy| fs::remove_file(rdy).expect("rm")),
            ("a symlink", |rdy| {
                fs::remove_file(rdy).expect("rm");
                platform::symlink(Path::new("/bin/sh"), rdy).expect("symlink");
            }),
            ("a directory", |rdy| {
                fs::remove_file(rdy).expect("rm");
                fs::create_dir(rdy).expect("mkdir");
            }),
            ("empty", |rdy| fs::write(rdy, b"").expect("write")),
        ];
        for (what, spoil) in extracted {
            let kit = Kit::new(target);
            spoil(&kit.source(target).join("rdy"));
            kit.retar(target);
            let input = kit.input();
            assert_eq!(
                check_plain(&kit, &input).map(|_| ()),
                Err(Install),
                "an extracted rdy that is {what}"
            );
            kit.assert_untouched(&input, what);
        }

        // An extracted rdy of exactly the cap is installed and runs (its version is wrong); one
        // byte more is refused before it is installed. Each is the fake `rdy`, ended by `exit
        // 0` and padded with zeros (a sparse file) to its length.
        for (len, want) in [(BINARY_MAX, Version), (BINARY_MAX + 1, Install)] {
            let kit = Kit::new(target);
            let rdy = kit.source(target).join("rdy");
            let script = format!(
                "{}exit 0\n",
                fake_rdy(&kit.marker, "rdy 0.15.0 (preview)", 0)
            );
            fs::write(&rdy, script).expect("rdy");
            File::options()
                .write(true)
                .open(&rdy)
                .and_then(|f| f.set_len(len))
                .expect("pad rdy");
            kit.retar(target);
            let input = kit.input();
            let log = Log::new();
            assert_eq!(
                check_logged(&kit, &input, &log).map(|_| ()),
                Err(want),
                "an extracted rdy of {len} bytes"
            );
            if want == Version {
                assert!(log.ran("install"), "the binary at the cap is installed");
                assert_eq!(kit.ran(), ["--version"], "the binary at the cap ran");
            } else {
                assert!(log.ran("tar"), "{:?}", log.all());
                assert!(
                    !log.ran("install"),
                    "the binary past the cap is refused before install: {:?}",
                    log.all()
                );
                assert!(!kit.marker.exists(), "the binary past the cap never ran");
            }
            assert!(absent(&input.unpack) && absent(&input.prefix), "{len}");
            assert!(absent(&kit.download.join(SIGNERS_FILE)));
            assert!(names(&kit.scratch).is_empty());
            fs::remove_file(&rdy).expect("rm the padded source");
        }

        // UNPACK and PREFIX are reached through links, which the installed binary points
        // elsewhere while its version check runs (and fails). The cleanup removes this run's
        // directories through their handles and nothing at the links' new targets.
        let kit = Kit::with_fake(target, &|marker| {
            let dir = marker.parent().expect("the kit's directory");
            let (elsewhere, unpack_alias, prefix_alias) = (
                dir.join("elsewhere"),
                dir.join("unpack alias"),
                dir.join("prefix alias"),
            );
            fake_rdy(marker, "rdy 0.15.0 (preview)", 0).replacen(
                "#!/bin/sh\n",
                &format!(
                    "#!/bin/sh\nif [ \"$1\" = --version ]; then ln -sfn '{e}' '{u}'; ln -sfn '{e}' '{p}'; fi\n",
                    e = elsewhere.display(),
                    u = unpack_alias.display(),
                    p = prefix_alias.display(),
                ),
                1,
            )
        });
        let mut input = kit.input();
        let unpack_parent = kit.dir.path().join("unpack parent");
        let prefix_parent = kit.dir.path().join("pre fix");
        fs::create_dir(&unpack_parent).expect("mkdir");
        let elsewhere = kit.dir.path().join("elsewhere");
        for d in ["un pack", "rdy-0.14.0"] {
            fs::create_dir_all(elsewhere.join(d)).expect("mkdir");
            fs::write(elsewhere.join(d).join("user-content"), b"sentinel\n").expect("sentinel");
        }
        let unpack_alias = kit.dir.path().join("unpack alias");
        let prefix_alias = kit.dir.path().join("prefix alias");
        platform::symlink(&unpack_parent, &unpack_alias).expect("symlink");
        platform::symlink(&prefix_parent, &prefix_alias).expect("symlink");
        input.unpack = unpack_alias.join("un pack");
        input.prefix = prefix_alias.join("rdy-0.14.0");
        assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Version));
        assert_eq!(
            fs::read_link(&prefix_alias).ok(),
            Some(elsewhere.clone()),
            "the links were moved"
        );
        for d in ["un pack", "rdy-0.14.0"] {
            assert_eq!(
                fs::read(elsewhere.join(d).join("user-content")).ok(),
                Some(b"sentinel\n".to_vec()),
                "{d} at the link's new target is kept"
            );
        }
        assert!(
            absent(&unpack_parent.join("un pack")) && absent(&prefix_parent.join("rdy-0.14.0")),
            "this run's UNPACK and PREFIX are removed"
        );
        assert_eq!(kit.ran(), ["--version"], "the installed binary ran");

        // Another process makes UNPACK or PREFIX after the input checks found them absent:
        // this run's mkdir of it fails, and that directory is never treated as this run's.
        type Pick = fn(&InstallInput) -> PathBuf;
        let competitors: [(&str, Pick); 2] = [
            ("UNPACK", |i| i.unpack.clone()),
            ("PREFIX", |i| i.prefix.clone()),
        ];
        for (what, pick) in competitors {
            let kit = Kit::new(target);
            let input = kit.input();
            let theirs = pick(&input);
            let log = Log::new();
            let process = |c: &GuideCommand| {
                log.push(c);
                if c.program == "shasum" || c.program == "sha256sum" {
                    fs::create_dir(&theirs).expect("the other process's mkdir");
                }
                stand_in(c, "")
            };
            assert_eq!(
                check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
                Err(Install),
                "{what} made by another process"
            );
            assert!(
                theirs.is_dir() && names(&theirs).is_empty(),
                "{what}: the other process's directory stays"
            );
            for ours in [&input.unpack, &input.prefix] {
                if *ours != theirs {
                    assert!(absent(ours), "{what}: {} is removed", ours.display());
                }
            }
            assert!(!kit.marker.exists(), "{what}: nothing ran");
            assert!(
                !log.ran("mkdir"),
                "{what}: the guide's mkdir does not run: {:?}",
                log.all()
            );
            assert!(absent(&kit.download.join(SIGNERS_FILE)));
        }
        // ... or right as the guide's `mkdir UNPACK PREFIX` starts. That mkdir fails, so this
        // run claims neither directory: the other process's stays, and so does the one this
        // mkdir may have made (empty), since a failed mkdir does not say which it made.
        for (what, pick) in competitors {
            let kit = Kit::new(target);
            let input = kit.input();
            let theirs = pick(&input);
            let process = |c: &GuideCommand| {
                if c.program == "mkdir" && c.args.first().is_some_and(|a| a != "-p") {
                    fs::create_dir(&theirs).expect("the other process's mkdir");
                    fs::write(theirs.join("theirs"), b"sentinel\n").expect("their file");
                }
                stand_in(c, "")
            };
            assert_eq!(
                check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
                Err(Install),
                "{what} made as mkdir starts"
            );
            assert_eq!(
                fs::read(theirs.join("theirs")).ok(),
                Some(b"sentinel\n".to_vec()),
                "{what}: the other process's directory stays"
            );
            for ours in [&input.unpack, &input.prefix] {
                if *ours != theirs {
                    assert!(
                        absent(ours) || (ours.is_dir() && names(ours).is_empty()),
                        "{what}: {} is absent or empty",
                        ours.display()
                    );
                }
            }
            assert!(!kit.marker.exists(), "{what}: nothing ran");
            assert!(absent(&kit.download.join(SIGNERS_FILE)));
        }
        // ... or the link to UNPACK's (or PREFIX's) parent is moved to another directory as
        // that mkdir starts, and another process makes the output in the parent the input
        // checks found. The mkdir succeeds in the new parent. The output's parent path no longer
        // names the checked parent, so this run claims neither directory: the other process's
        // directory (with a file in it or empty) stays, and so do the (empty) directories the
        // mkdir made.
        for (what, their_file) in [
            ("UNPACK", true),
            ("UNPACK", false),
            ("PREFIX", true),
            ("PREFIX", false),
        ] {
            let kit = Kit::new(target);
            let mut input = kit.input();
            let (checked, moved, alias) = (
                kit.dir.path().join("checked parent"),
                kit.dir.path().join("moved parent"),
                kit.dir.path().join("parent alias"),
            );
            for d in [&checked, &moved] {
                fs::create_dir(d).expect("mkdir");
            }
            platform::symlink(&checked, &alias).expect("symlink");
            let output = if what == "UNPACK" {
                &mut input.unpack
            } else {
                &mut input.prefix
            };
            let name = output.file_name().expect("a name").to_os_string();
            *output = alias.join(&name);
            let theirs = checked.join(&name);
            let process = |c: &GuideCommand| {
                if c.program == "mkdir" && c.args.first().is_some_and(|a| a != "-p") {
                    fs::remove_file(&alias).expect("rm the link");
                    platform::symlink(&moved, &alias).expect("move the link");
                    fs::create_dir(&theirs).expect("the other process's mkdir");
                    if their_file {
                        fs::write(theirs.join("theirs"), b"sentinel\n").expect("their file");
                    }
                }
                stand_in(c, "")
            };
            assert_eq!(
                check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
                Err(Install),
                "{what} ({their_file}): its parent's link moved as mkdir starts"
            );
            assert!(
                theirs.is_dir(),
                "{what} ({their_file}): the other process's directory in the checked parent stays"
            );
            if their_file {
                assert_eq!(
                    fs::read(theirs.join("theirs")).ok(),
                    Some(b"sentinel\n".to_vec()),
                    "{what}: their file stays"
                );
                assert_eq!(names(&theirs), ["theirs"], "{what}: and only their file");
            } else {
                assert!(names(&theirs).is_empty(), "{what}: theirs stays empty");
            }
            for ours in [&input.unpack, &input.prefix] {
                assert!(
                    absent(ours) || (ours.is_dir() && names(ours).is_empty()),
                    "{what}: {} is absent or empty",
                    ours.display()
                );
            }
            assert!(!kit.marker.exists(), "{what}: nothing ran");
            assert!(absent(&kit.download.join(SIGNERS_FILE)));
        }
        // ... or another process removes the directory the mkdir made right after it, and
        // makes its own at that name, with a file in it. The parents are unchanged, but a
        // directory the guide's mkdir just made is empty: this one is not this run's, and
        // neither directory is claimed.
        for what in ["UNPACK", "PREFIX"] {
            let kit = Kit::new(target);
            let input = kit.input();
            let theirs = if what == "UNPACK" {
                input.unpack.clone()
            } else {
                input.prefix.clone()
            };
            let process = |c: &GuideCommand| {
                if c.program == "mkdir" && c.args.first().is_some_and(|a| a != "-p") {
                    let q = |p: &Path| format!("'{}'", p.display());
                    return shell_script(&format!(
                        "mkdir {u} {p} && rmdir {t} && mkdir {t} && echo sentinel > {f}",
                        u = q(&input.unpack),
                        p = q(&input.prefix),
                        t = q(&theirs),
                        f = q(&theirs.join("theirs")),
                    ));
                }
                stand_in(c, "")
            };
            assert_eq!(
                check_kit(&kit, &input, &process, |_| {}).map(|_| ()),
                Err(Install),
                "{what} replaced after mkdir"
            );
            assert_eq!(
                fs::read(theirs.join("theirs")).ok(),
                Some(b"sentinel\n".to_vec()),
                "{what}: the other process's directory stays"
            );
            for ours in [&input.unpack, &input.prefix] {
                if *ours != theirs {
                    assert!(
                        ours.is_dir() && names(ours).is_empty(),
                        "{what}: {} the mkdir made stays, empty",
                        ours.display()
                    );
                }
            }
            assert!(!kit.marker.exists(), "{what}: nothing ran");
            assert!(absent(&kit.download.join(SIGNERS_FILE)));
        }

        // A failure after the install removes what this run made, and nothing else.
        let kit = Kit::with_rdy(target, "rdy 0.15.0 (preview)", 0);
        let input = kit.input();
        fs::write(kit.dir.path().join("pre fix").join("keep"), b"sentinel\n").expect("sentinel");
        assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Version));
        assert_eq!(kit.ran(), ["--version"], "only the version check ran");
        assert!(
            absent(&input.unpack) && absent(&input.prefix),
            "UNPACK and PREFIX are removed"
        );
        assert_eq!(
            names(&kit.dir.path().join("pre fix")),
            ["keep"],
            "PREFIX's parent is kept"
        );
        assert!(absent(&kit.download.join(SIGNERS_FILE)));
        assert!(names(&kit.scratch).is_empty());

        // A failed daemon stop: the scratch project and PREFIX stay for a later stop.
        let kit = Kit::with_rdy(target, "rdy 0.14.0 (preview)", 1);
        let input = kit.input();
        assert_eq!(check_plain(&kit, &input).map(|_| ()), Err(Cleanup));
        let kept = names(&kit.scratch);
        assert_eq!(kept.len(), 1, "one scratch directory is kept: {kept:?}");
        let project = kit.scratch.join(&kept[0]).join("project");
        assert!(project.is_dir(), "the owned project is kept");
        assert!(
            input.prefix.join("bin").join("rdy").is_file(),
            "PREFIX is kept"
        );
        assert!(absent(&input.unpack), "UNPACK is removed");
        assert!(
            absent(&kit.download.join(SIGNERS_FILE)),
            "the signer file is removed"
        );
        let ran = kit.ran();
        assert_eq!(
            ran.last().map(String::as_str),
            Some(format!("--project {} serve --stop", project.display()).as_str())
        );
    }

    /// What stands at `p`: nothing, a link and its target, a file's bytes or a directory's tree.
    fn state_of(p: &Path) -> String {
        match fs::symlink_metadata(p) {
            Err(_) => "absent".into(),
            Ok(m) if m.file_type().is_symlink() => format!("a link to {:?}", fs::read_link(p)),
            Ok(m) if m.is_dir() => format!("a directory {:?}", tree_of(p)),
            Ok(_) => format!("a file {:?}", fs::read(p)),
        }
    }

    /// Replaces the file `p` with a symlink to its moved copy.
    fn link_in_place(p: &Path) {
        let real = p.with_extension("real");
        fs::rename(p, &real).expect("mv");
        platform::symlink(&real, p).expect("symlink");
    }

    #[test]
    fn pre3_check_installs_and_runs_the_smoke_path() {
        if !install_tools_or_skip(&crate::test_name!(
            "pre3_check_installs_and_runs_the_smoke_path"
        )) {
            return;
        }
        let kit = Kit::new(host_target());
        let input = kit.input();
        let report = check_plain(&kit, &input).unwrap_or_else(|f| panic!("{}", f.message()));
        assert_eq!(report.binary, input.prefix.join("bin").join("rdy"));
        assert_eq!(
            (report.version.as_str(), report.commit.as_str()),
            (IV, COMMIT)
        );
        assert_eq!(report.target, host_target());
        assert_eq!(report.os, platform::release_os().expect("os"));
        assert_eq!(
            report
                .checks
                .iter()
                .map(|c| (c.name.as_str(), c.passed))
                .collect::<Vec<_>>(),
            CHECK_NAMES.iter().map(|n| (*n, true)).collect::<Vec<_>>()
        );
        assert_eq!(report.skipped, 0);
        let json = serde_json::to_string(&report).expect("json");
        assert!(json.starts_with("{\"binary\":"), "{json}");
        let keys: Vec<String> = serde_json::from_str::<serde_json::Value>(&json)
            .expect("json")
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        let mut want = [
            "binary", "version", "commit", "target", "os", "checks", "skipped",
        ];
        let in_order = json.match_indices("\":").count();
        assert!(in_order >= want.len());
        want.sort();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(sorted, want);
        // PREFIX holds only bin/rdy, the archive's binary; UNPACK, the signer file and the
        // scratch project are gone.
        assert_eq!(
            tree_of(&input.prefix)
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["bin", "bin/rdy"]
        );
        assert_eq!(
            fs::read(input.prefix.join("bin").join("rdy")).expect("installed"),
            fake_rdy(&kit.marker, "rdy 0.14.0 (preview)", 0).into_bytes()
        );
        assert_eq!(
            mode_of(&input.prefix.join("bin").join("rdy")).expect("mode"),
            0o755
        );
        assert!(absent(&input.unpack));
        assert!(absent(&kit.download.join(SIGNERS_FILE)));
        assert!(
            names(&kit.scratch).is_empty(),
            "the scratch project is removed"
        );
        // The installed binary ran the version check, then the CLI and MCP paths on one
        // project, then the stop.
        let ran = kit.ran();
        assert_eq!(ran.len(), 5, "{ran:?}");
        assert_eq!(ran[0], "--version");
        let project = ran[1].strip_prefix("init ").expect("init").to_string();
        assert!(
            project.starts_with(&kit.scratch.to_string_lossy().into_owned()),
            "{project}"
        );
        assert_eq!(ran[2], format!("--project {project} --json status"));
        assert_eq!(ran[3], format!("mcp --project {project}"));
        assert_eq!(ran[4], format!("--project {project} serve --stop"));

        // An MCP server that answers every call but then exits nonzero, or by a signal, fails
        // the mcp stage once the daemon is stopped.
        for (what, end) in [("exit 17", "exit 17"), ("a signal", "kill -9 $$")] {
            let kit = Kit::with_fake(host_target(), &|marker| {
                fake_rdy_with(marker, "rdy 0.14.0 (preview)", 0, end)
            });
            let input = kit.input();
            assert_eq!(
                check_plain(&kit, &input).map(|_| ()),
                Err(InstallFailure::Mcp),
                "{what}"
            );
            let ran = kit.ran();
            assert!(
                ran.last().is_some_and(|l| l.ends_with("serve --stop")),
                "{what}: the daemon is stopped: {ran:?}"
            );
            assert!(absent(&input.unpack) && absent(&input.prefix), "{what}");
            assert!(names(&kit.scratch).is_empty(), "{what}: no scratch project");
        }
    }
}
