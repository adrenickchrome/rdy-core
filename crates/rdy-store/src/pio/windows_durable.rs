//! The Windows durability calls of [`crate::pio`] (FW-XP1 Phase 4, D-FPKPS): directory flushes
//! and write-through renames. A failure of either is an error, never a success.
//!
//! - A flush opens a handle of the directory with `FILE_FLAG_BACKUP_SEMANTICS` and calls
//!   `FlushFileBuffers` on it. The handle has the right to add a file to the directory
//!   (`FILE_ADD_FILE`, which is `FILE_WRITE_DATA` for a directory): `FlushFileBuffers` needs write
//!   access. Measured on NTFS as an ordinary account, on its own and on private directories: a
//!   handle with read rights alone fails with `ERROR_ACCESS_DENIED`, and this one flushes. A
//!   directory that the account may not add a file to cannot be flushed: the open fails with
//!   `ERROR_ACCESS_DENIED`. A file system that cannot flush gives its own error
//!   (`ERROR_INVALID_FUNCTION`). Each is an error.
//! - [`sync_open_dir`] opens that handle relative to the open directory (`NtOpenFile` with its
//!   handle as the root and an empty name), so no path is resolved again; the new handle must
//!   name the same directory. (`ReOpenFile` of a directory handle is refused.)
//! - A rename is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`, never
//!   `MOVEFILE_COPY_ALLOWED`: a move to another volume fails (`ERROR_NOT_SAME_DEVICE`) and nothing
//!   is copied. [`rename_durable`] makes it while both parent directories are pinned, as
//!   `rename_at` does for one: nobody can move or remove a pinned directory or a directory above
//!   it, and each path is checked before and after the move to name its pinned directory.
//!
//! A flush's failure is `could not sync directory {path}: {error}`, and a durable rename's is
//! `could not rename {from} to {to}: {error}`, each of the same kind as the failed call's, with
//! its OS error and number.
//!
//! This file depends only on `std`, `windows-sys`, [`crate::fileid`], the durable-step hook of
//! [`crate::pio`] and its siblings `windows_security` and `windows_dir`.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use windows_sys::Wdk::Storage::FileSystem::FILE_DIRECTORY_FILE;
use windows_sys::Win32::Foundation::{ERROR_INVALID_NAME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FlushFileBuffers,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING, SYNCHRONIZE,
};

use super::windows_dir::{PinnedDir, dos_error, entry_name, open_dir, open_entry};
use super::windows_security::{native, os_error, parent_changed};
use crate::fileid::FileId;
use crate::pio::{DurableStep, durable};

/// The rights of a flush handle: add a file (`FILE_WRITE_DATA` for a directory), which
/// `FlushFileBuffers` needs; read its attributes, for its identity; and wait on it.
pub(super) const FLUSH_ACCESS: u32 = FILE_ADD_FILE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// The flags of every rename: replace a file at the target, and return only once the move is on
/// the disk. Never `MOVEFILE_COPY_ALLOWED`.
pub(super) const MOVE_FLAGS: u32 = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;

/// A flush handle shares everything, as the other handles here do: it stops nobody.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// Flushes the directory `path`, so entries created, renamed or removed in it survive a power
/// loss: it opens a flush handle of the directory (links in `path` are followed: the caller trusts
/// this path) and calls `FlushFileBuffers`. A failure is an error, `could not sync directory
/// {path}: {error}`.
#[allow(unsafe_code)]
pub fn sync_dir(path: &Path) -> io::Result<()> {
    let flushed = || {
        let name = native(path)?;
        // SAFETY: `name` is NUL-terminated and lives for the call; no security attributes or
        // template handle are passed.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                FLUSH_ACCESS,
                SHARE_ALL,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so `h` is a new handle that nothing else owns.
        let dir = File::from(unsafe { OwnedHandle::from_raw_handle(h) });
        flush(&dir, path)
    };
    flushed().map_err(|e| sync_failed(path, e))
}

/// Flushes the open directory `dir` (at `path`, which names it in an error): a flush handle is
/// opened relative to `dir` itself, must name the same directory, and is flushed. A failure is an
/// error, `could not sync directory {path}: {error}`.
pub fn sync_open_dir(dir: &File, path: &Path) -> io::Result<()> {
    let flushed = || {
        let handle = open_entry(dir, &[], FLUSH_ACCESS, FILE_DIRECTORY_FILE)
            .map_err(|status| io::Error::from_raw_os_error(dos_error(status)))?;
        if FileId::of_open(&handle, path)? != FileId::of_open(dir, path)? {
            return Err(parent_changed(path));
        }
        flush(&handle, path)
    };
    flushed().map_err(|e| sync_failed(path, e))
}

/// `FlushFileBuffers` on the flush handle `dir` of the directory `path`, as one durable step.
#[allow(unsafe_code)]
fn flush(dir: &File, path: &Path) -> io::Result<()> {
    durable(
        || DurableStep::Flush(path.to_path_buf()),
        || {
            // SAFETY: `dir` is an open handle for the call.
            if unsafe { FlushFileBuffers(dir.as_raw_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        },
    )
}

/// `could not sync directory {path}: {error}`, of the same kind as `e`.
fn sync_failed(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!("could not sync directory {}: {e}", path.display()),
    )
}

/// Renames `from` to `to`, replacing a file at `to`, as one write-through move (`MoveFileExW`).
/// Each parent directory is opened (links in its path are followed: the caller trusts these
/// paths) and pinned while the move runs, and both are checked again after it; both names are
/// checked first, as for the opens. The entry survives a power loss once the directory that
/// holds it is flushed ([`sync_dir`]). Nothing is ever copied: a move to another volume fails. A
/// failure is an error, `could not rename {from} to {to}: {error}`, of the same kind as the
/// failed call's.
pub fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    let renamed = || {
        let (from_dir, from_name) = parent(from)?;
        let (to_dir, to_name) = parent(to)?;
        let from_units = entry_name(&from_dir, from_name)?;
        let to_units = entry_name(&to_dir, to_name)?;
        let from_pin = PinnedDir::of(&from_dir)?;
        let to_pin = PinnedDir::of(&to_dir)?;
        let source = from_pin.entry(from_name, &from_units)?;
        let target = to_pin.entry(to_name, &to_units)?;
        move_entry(
            &source,
            &target,
            &from_pin.shown().join(from_name),
            &to_pin.shown().join(to_name),
        )?;
        from_pin.check()?;
        to_pin.check()
    };
    renamed().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "could not rename {} to {}: {e}",
                from.display(),
                to.display()
            ),
        )
    })
}

/// The open parent directory of `path` (`.` for a bare name), and `path`'s file name. A path that
/// names no entry (a root, or one that ends in `..`) is `ERROR_INVALID_NAME`.
fn parent(path: &Path) -> io::Result<(File, &OsStr)> {
    let name = path
        .file_name()
        .ok_or_else(|| os_error(ERROR_INVALID_NAME))?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((open_dir(&dir)?, name))
}

/// Moves `source` over `target`, two NUL-terminated verbatim paths, with [`MOVE_FLAGS`], as one
/// durable step (`shown_from` and `shown_to` name them). A failed move is the OS error.
#[allow(unsafe_code)]
pub(super) fn move_entry(
    source: &[u16],
    target: &[u16],
    shown_from: &Path,
    shown_to: &Path,
) -> io::Result<()> {
    let step = || DurableStep::Rename {
        from: shown_from.to_path_buf(),
        to: shown_to.to_path_buf(),
        flags: MOVE_FLAGS,
    };
    durable(step, || {
        // SAFETY: both paths are NUL-terminated and live for the call.
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVE_FLAGS) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })
}

/// Native NTFS cases of the provider itself: every export of `pio` on Windows, and which handle
/// of a directory can be flushed.
#[cfg(test)]
mod tests {
    use super::{FLUSH_ACCESS, MOVE_FLAGS};
    use crate::pio::*;
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ADD_FILE, FlushFileBuffers, MOVEFILE_COPY_ALLOWED, MOVEFILE_WRITE_THROUGH,
    };

    #[test]
    fn wx6_native_every_pio_export_is_built() {
        use crate::fileid::{self, ChangeStamp, FileId};
        // every portable export of pio, with its Windows signature: nothing is left unbuilt
        let _: fn(&File, &mut [u8], u64) -> io::Result<()> = read_exact_at;
        let _: fn(&File, &[u8], u64) -> io::Result<()> = write_all_at;
        let _: fn(&mut OpenOptions, &Path) -> io::Result<File> = |o, p| open_no_follow(o, p);
        let _: fn(&mut OpenOptions) -> NoFollow<'_> = private_file;
        let _: fn(&File) -> io::Result<()> = make_private_file;
        let _: fn(&Path) -> io::Result<()> = create_private_dir;
        let _: fn(&Path) -> io::Result<()> = make_private_dir;
        let _: fn(&File) -> io::Result<Privacy> = privacy;
        let _: fn(&Path) -> io::Result<()> = sync_dir;
        let _: fn(&File, &Path) -> io::Result<()> = sync_open_dir;
        let _: fn(&Path, &Path) -> io::Result<()> = rename_durable;
        let _: fn(&Path) -> io::Result<File> = open_dir;
        let _: fn(&File, &OsStr) -> io::Result<File> = open_dir_at;
        let _: fn(&File, &OsStr) -> io::Result<File> = open_read_at;
        let _: fn(&File, &OsStr) -> io::Result<File> = create_private_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = create_private_dir_at;
        let _: fn(&File, &OsStr) -> io::Result<bool> = umask_keeps_0700_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = remove_file_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = remove_dir_at;
        let _: fn(&File, &OsStr, &OsStr) -> io::Result<()> = rename_at;
        let _: fn(&mut std::process::Command, &File) -> io::Result<StartPin> = start_in_dir;
        let _: fn(&File, &OsStr) -> io::Result<ProgramInDir> = program_in_dir;
        let _: fn(&File) -> io::Result<bool> = is_empty_dir;
        #[allow(clippy::type_complexity)]
        let _: fn(&File, &mut dyn FnMut(&OsStr) -> io::Result<()>) -> io::Result<()> =
            |d, f| for_each_entry(d, f);
        let _: fn(&File, &OsStr) -> io::Result<EntryKind> = entry_kind_at;
        let _: fn(&io::Error) -> bool = is_link_refusal;
        let _: fn(&File, u64, u64) -> io::Result<Punch> = punch_hole;
        let _: fn(Option<PunchBackend>) = on_punch;
        let _: fn(Option<DurableBackend>) = on_durable;
        let _: fn(&File) -> io::Result<u64> = allocated_bytes;
        let _: fn(&File, &Path) -> io::Result<FileId> = FileId::of_open;
        let _: fn(&File, &Path) -> io::Result<u64> = fileid::links_of;
        let _: fn(&File, &Path) -> io::Result<ChangeStamp> = fileid::change_stamp_of;
        // a rename writes through and never copies
        assert_ne!(MOVE_FLAGS & MOVEFILE_WRITE_THROUGH, 0);
        assert_eq!(MOVE_FLAGS & MOVEFILE_COPY_ALLOWED, 0);
    }

    #[test]
    #[allow(unsafe_code)]
    fn wx6_native_only_a_handle_that_may_add_a_file_flushes_a_directory() {
        let root = tempfile::tempdir().unwrap();
        // pio's directory handle reads the directory: FlushFileBuffers refuses it
        let read = open_dir(root.path()).unwrap();
        // SAFETY: `read` is open for the call.
        assert_eq!(unsafe { FlushFileBuffers(read.as_raw_handle()) }, 0);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(5));
        // the flush handle may add a file, and flushes
        assert_ne!(FLUSH_ACCESS & FILE_ADD_FILE, 0);
        sync_open_dir(&read, root.path()).unwrap();
        sync_dir(root.path()).unwrap();
        // a directory moved after its handle was opened is still the one flushed
        let moved = root.path().join("moved");
        let dir = root.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        let h = open_dir(&dir).unwrap();
        std::fs::rename(&dir, &moved).unwrap();
        sync_open_dir(&h, &dir).unwrap();
        // a path that names nothing is the open's error, in the flush text
        let e = sync_dir(&dir).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
        assert!(
            e.to_string()
                .starts_with(&format!("could not sync directory {}: ", dir.display())),
            "{e}"
        );
    }
}
