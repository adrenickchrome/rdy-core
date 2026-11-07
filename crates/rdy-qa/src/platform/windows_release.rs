//! The release packaging calls on Windows (FW-XP1 D-1RXEA): this machine's release target, the
//! handle-based file calls that [`crate::release_install`] makes, and the archive tool.
//!
//! - **The release target is MSVC amd64.** Any other Windows machine has none, as any other
//!   Unix machine has none.
//! - **Every call goes through a handle.** An input is opened without following a reparse point
//!   at its last entry, and an entry is the same file only if its identity (volume and file ID)
//!   is the open file's.
//! - **There are no permission bits.** A file a call creates is private to this account from
//!   the moment it exists (an ACL, set by the store's directory calls), so setting a "mode" has
//!   nothing to change.
//! - **The archive tool is the system's.** `tar.exe` of the Windows directory is bsdtar; a
//!   `tar` that a PATH entry names (Git for Windows ships GNU tar) is never used.

use rdy_store::fileid::FileId;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

/// The release target triple of this machine: Windows (MSVC) amd64. Any other machine is
/// `Unsupported`.
pub fn release_target() -> io::Result<&'static str> {
    crate::platform::release_target_of(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "gnu"),
    )
}

/// This machine's OS: `Windows`, and the version that the system reports.
pub fn release_os() -> io::Result<String> {
    let mut cmd = std::process::Command::new(system_file("cmd.exe"));
    cmd.args(["/d", "/c", "ver"]);
    let out = super::run_bounded(
        cmd,
        None,
        super::RELEASE_CHILD_OUTPUT_MAX,
        super::RELEASE_CHILD_DEADLINE,
    )?;
    if !out.status.success() {
        return Err(io::Error::other(format!("ver: {}", out.status)));
    }
    let text = std::str::from_utf8(&out.stdout)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ver printed non-UTF-8 text"))?;
    let line = text.trim_matches(['\r', '\n']);
    if line.is_empty() || line.contains(['\r', '\n']) || line.len() > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ver did not print exactly one line",
        ));
    }
    Ok(line.to_string())
}

/// The system's own copy of `name`: `%SystemRoot%\System32\<name>`.
fn system_file(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join("System32").join(name)
}

/// An absolute path to a device that is not a regular file and that a package input must refuse:
/// the null device, which has no directory entry to open (`ErrorKind::InvalidInput`).
#[cfg(test)]
pub(crate) fn device_input() -> &'static Path {
    Path::new(r"\\.\NUL")
}

/// A command that writes exactly `text` to stdout and exits 0: a stand-in version probe.
/// Windows PowerShell writes the argument's characters, with no added line ending.
#[cfg(test)]
pub(crate) fn print_command(text: &str) -> Command {
    let mut cmd = Command::new(system_file(r"WindowsPowerShell\v1.0\powershell.exe"));
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "[Console]::Out.Write($args[0])",
        text,
    ]);
    cmd
}

/// The archive tool: the system's `tar.exe` (bsdtar).
pub fn tar_program() -> PathBuf {
    system_file("tar.exe")
}

/// Opens the directory `path` for the `*_at` calls; links in `path` are followed.
pub fn open_dir(path: &Path) -> io::Result<File> {
    rdy_store::pio::open_dir(path)
}

/// Creates the new directory `name` in the open directory `parent`, private to this account,
/// and returns a handle to it. An existing `name` is `ErrorKind::AlreadyExists`. The directory
/// is opened through `parent`, never through a reparse point; if that fails, the empty directory
/// is removed again by name.
pub fn create_private_dir_at(parent: &File, name: &OsStr) -> io::Result<File> {
    use rdy_store::pio;
    pio::create_private_dir_at(parent, name)?;
    match pio::open_dir_at(parent, name) {
        Ok(dir) => Ok(dir),
        Err(e) => {
            let _ = pio::remove_dir_at(parent, name);
            Err(e)
        }
    }
}

/// Opens the file `path` for reading, never through a reparse point at its last entry. A link
/// there is `ErrorKind::InvalidInput`, and so is an entry that is not a regular file and cannot
/// be opened. The caller judges the file's kind and size from the handle.
pub fn open_input(path: &Path) -> io::Result<File> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no directory entry", path.display()),
        ));
    };
    let dir = rdy_store::pio::open_dir(parent)?;
    rdy_store::pio::open_read_at(&dir, name).map_err(|e| {
        if rdy_store::pio::is_link_refusal(&e) {
            return io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a link or not a regular file", path.display()),
            );
        }
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.file_type().is_file() => io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ),
            _ => e,
        }
    })
}

/// Creates the new file `name` in the open directory `dir`, for reading and writing, private to
/// this account, never through a link: anything already at `name` is `ErrorKind::AlreadyExists`.
pub fn create_new_at(dir: &File, name: &OsStr) -> io::Result<File> {
    rdy_store::pio::create_private_at(dir, name)
}

/// Whether the entry `name` in the open directory `dir` is still the file or directory `file`
/// (the same volume and file ID). A missing entry, or a reparse point in its place, is not.
pub fn holds_file(dir: &File, name: &OsStr, file: &File) -> io::Result<bool> {
    match rdy_store::pio::open_read_at(dir, name) {
        Ok(found) => Ok(FileId::of_file(&found)? == FileId::of_file(file)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound || rdy_store::pio::is_link_refusal(&e) => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Whether `path`, its links followed, opens the open directory `dir` now. A path that cannot
/// be opened as a directory does not.
pub fn opens_dir(path: &Path, dir: &File) -> bool {
    match (open_dir(path), FileId::of_file(dir)) {
        (Ok(a), Ok(b)) => FileId::of_file(&a).is_ok_and(|a| a == b),
        _ => false,
    }
}

/// Whether the open directory `dir` holds no entry, listed through the handle.
pub fn is_empty_dir(dir: &File) -> io::Result<bool> {
    rdy_store::pio::is_empty_dir(dir)
}

/// Removes the entry `name` (not a directory) from the open directory `dir`.
pub fn remove_file_at(dir: &File, name: &OsStr) -> io::Result<()> {
    rdy_store::pio::remove_file_at(dir, name)
}

/// Removes the empty directory `name` from the open directory `dir`.
pub fn remove_dir_at(dir: &File, name: &OsStr) -> io::Result<()> {
    rdy_store::pio::remove_dir_at(dir, name)
}

/// Renames the entry `from` to `to`, both in the open directory `dir`.
pub fn rename_at(dir: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
    rdy_store::pio::rename_at(dir, from, to)
}

/// Sets the permission bits of the open file `file` to `mode`: nothing to set here. Access is
/// an ACL that the creating call made private, and a file is executable by its extension.
pub fn set_file_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}
