//! The project's pipe name and the files of its run directory, on Windows (D-FPKPS).
//!
//! A pipe is no file. Nothing here ever creates, unlinks or changes a filesystem entry at its
//! name: its descriptor is set when it is made, and it goes when its last handle closes. The exact
//! grammars of the run files are in [`records`].

use super::super::DirState;
use super::super::records::{self, MAX_PID_RECORD as PID_BOUND, MAX_PIPE_RECORD};
use super::identity::creation_time;
use super::pipe::{Listener, bind};
use rdy_model::Hash;
use rdy_store::fileid::FileId;
use rdy_store::pio;
pub use rdy_store::pio::{
    create_private_dir, make_private_dir, make_private_file, open_no_follow, private_file,
};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// The most of `daemon.sock` that is read.
pub const MAX_ENDPOINT_RECORD: usize = MAX_PIPE_RECORD;
/// The most of `daemon.pid` that is read.
pub const MAX_PID_RECORD: usize = PID_BOUND;

/// The hash that names the project at the canonical path `root`, its pipe and its stop event: of
/// the path's own UTF-16LE code units.
pub fn project_hash(root: &Path) -> Hash {
    records::hash_of_units(root.as_os_str().encode_wide())
}

/// The pipe of the canonical project root `root`. It is never the short socket.
pub fn project_endpoint(root: &Path) -> (PathBuf, bool) {
    (records::pipe_name(&project_hash(root)), false)
}

/// Refuses a project whose canonical path cannot be read: its pipe's name would depend on the
/// spelling. `could not resolve the canonical project path {path}: {error}`.
pub fn check_project_root(root: &Path) -> io::Result<()> {
    fs::canonicalize(root).map(drop).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "could not resolve the canonical project path {}: {e}",
                root.display()
            ),
        )
    })
}

/// A pipe needs no directory.
pub fn prepare_endpoint_place(_root: &Path) -> io::Result<()> {
    Ok(())
}

/// Nothing to clear: a dead daemon's pipe has gone with its handles, and a live one's name is
/// never taken from it ([`bind`] refuses).
pub fn clear_endpoint(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// [`bind`], whose error names the pipe.
pub fn bind_endpoint(path: &Path) -> io::Result<Listener> {
    bind(path)
}

/// Nothing to do: the pipe has been private since it was made.
pub fn seal_endpoint(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Nothing to remove: the pipe goes when the listener is dropped.
pub fn remove_endpoint(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// See [`records::parse_pipe_record`].
pub fn parse_endpoint_record(bytes: &[u8]) -> Option<(PathBuf, FileId)> {
    records::parse_pipe_record(bytes)
}

/// What `daemon.pid` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PidRecord {
    pid: u32,
    created: u64,
}

impl PidRecord {
    /// The process the record names.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// That process's creation time, as a `FILETIME` in one number. A reused pid has another.
    pub fn created(&self) -> u64 {
        self.created
    }
}

/// This process's `daemon.pid`: its pid and its creation time.
#[allow(unsafe_code)]
pub fn own_pid_record() -> io::Result<Vec<u8>> {
    // SAFETY: the call takes nothing, and returns this process's pseudo handle, which is always
    // valid and needs no closing.
    let me = unsafe { GetCurrentProcess() };
    Ok(records::pipe_pid_record(
        std::process::id(),
        creation_time(me)?,
    ))
}

/// See [`records::parse_pipe_pid_record`].
pub fn parse_pid_record(bytes: &[u8]) -> Option<PidRecord> {
    let (pid, created) = records::parse_pipe_pid_record(bytes)?;
    Some(PidRecord { pid, created })
}

/// What stands at `path`, read from the object itself and never through a reparse point, which is
/// refused: `{path} is a reparse point; rdy will not follow it`. A directory that this account may
/// not even read belongs to another: `{path} is not private to this account`.
pub fn dir_state(path: &Path) -> io::Result<DirState> {
    let opened = open_no_follow(
        OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE),
        path,
    );
    let dir = match opened {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return Err(io::Error::new(
                e.kind(),
                format!("{} is not private to this account", path.display()),
            ));
        }
        Err(e) => return Err(e),
    };
    if !dir.metadata()?.is_dir() {
        return Ok(DirState::NotDir);
    }
    Ok(if pio::privacy(&dir)?.private {
        DirState::Private
    } else {
        DirState::NotPrivate
    })
}

/// Whether the open run file `f` may be believed: this account owns it, and only this account may
/// use it.
pub fn run_file_is_private(f: &File) -> io::Result<bool> {
    Ok(pio::privacy(f)?.private)
}

/// Whether `e` says the file system has no hard links: `CreateHardLinkW` fails so on FAT and exFAT,
/// and on a share that does not offer them.
pub fn no_hard_links(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|n| n as u32),
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)
    )
}
