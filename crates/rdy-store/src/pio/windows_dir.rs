//! The Windows calls of [`crate::pio`] in an open directory (FW-XP1 Phase 4): opens, entry kinds,
//! listings, creates, removals, renames and child processes anchored to a directory handle.
//!
//! - The directory handle is the verified ancestor. Every entry is opened relative to it
//!   (`NtOpenFile` with the handle as the root directory and one entry name), never through a
//!   path made from the directory's path: a directory moved, or another put at its path, after its
//!   handle was opened cannot redirect a call. The handle shares delete, as std's opens do, so it
//!   never stops the directory from being moved; what it names stays the same object.
//! - No reparse point is followed. The entry is opened itself (`FILE_OPEN_REPARSE_POINT`, with
//!   `OBJ_DONT_REPARSE`), and one of any tag is refused, `{path} is a reparse point; rdy will not
//!   follow it`, from the handle that was opened, never from a name read again.
//! - A listing goes through a new open of the directory itself (its handle and an empty name), so
//!   it resolves no path and has its own position, and the new handle must name the same
//!   directory.
//! - A create makes the entry relative to the handle (`NtCreateFile`, `FILE_CREATE`) with this
//!   account's private descriptor (`windows_security`'s `PrivateDescriptor`), so it is private
//!   from the moment it exists; anything at the name, a reparse point included, is
//!   `AlreadyExists` and never followed. The new object's privacy is then read from its handle.
//! - A removal opens the entry itself for `DELETE` and removes the object it opened, through that
//!   handle (`FileDispositionInfoEx`): what another process puts at the name after the open is
//!   never removed, and a reparse point is removed as itself, never followed. A create or a probe
//!   that cleans up after itself removes the object it made through its own handle, never by name.
//! - A rename is a path call (a write-through `MoveFileExW`, as D-FPKPS says, made by the sibling
//!   `windows_durable`), made while the directory is pinned:
//!   a new handle of the directory itself that reads it and does not share delete, so nobody can
//!   move or remove the directory while it is open, and NTFS refuses to move any directory above
//!   an open handle. Both names are entries of the pinned directory. Its path is its final path,
//!   which holds no reparse point, and it is checked before and after the move: resolved again, it
//!   must name the pinned directory (`the directory holding {path} changed during the operation`).
//!   The path is used verbatim (`\\?\`), so no name is changed on the way (a trailing dot stays).
//! - A child's start directory and its program are paths that the child resolves when it starts,
//!   after the spawn has returned. So `start_in_dir` and `program_in_dir` pin the directory as a
//!   rename does, check its path, and give those verbatim paths, and the caller holds the pin
//!   until the child is waited.
//! - Bounds: an entry name longer than 32,767 UTF-16 code units, or with a NUL, is refused before
//!   anything is made of it. Each listed record is checked against the listing buffer, and its
//!   name against the same bound, before the name is copied.
//!
//! This file depends only on `std`, `windows-sys`, [`crate::fileid`], [`EntryKind`] and its
//! siblings `windows_security` and `windows_durable` (whose move it makes), so an isolated probe
//! can compile and test it with those files.

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NtCreateFile, NtOpenFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_BAD_LENGTH, ERROR_DIRECTORY, ERROR_DIRECTORY_NOT_SUPPORTED, ERROR_FILENAME_EXCED_RANGE,
    ERROR_INVALID_FUNCTION, ERROR_INVALID_NAME, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES,
    ERROR_NOT_SUPPORTED, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, OBJ_CASE_INSENSITIVE,
    OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_REPARSE_POINT_ENCOUNTERED, UNICODE_STRING,
};
use windows_sys::Win32::Security::SECURITY_DESCRIPTOR;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileDispositionInfo, FileDispositionInfoEx, FileFullDirectoryInfo,
    FileFullDirectoryRestartInfo, GetFileInformationByHandleEx, OPEN_EXISTING, READ_CONTROL,
    SYNCHRONIZE, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::windows_durable::move_entry;
use super::windows_security::{
    NATIVE_PATH_MAX, ObjectKind, PrivateDescriptor, attribute_tag, final_path, is_reparse_refusal,
    native, not_made, not_private, os_error, parent_changed, privacy, reparse_point,
};
use crate::fileid::FileId;
use crate::pio::EntryKind;

/// The rights of a directory handle: list it, read its attributes (a `File::metadata`) and its
/// security ([`super::privacy`]), and wait on it (synchronous I/O).
const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;

/// Every handle here shares reading, writing and delete, as std's opens do: an entry stays
/// movable and removable, as on Unix, and the handle keeps naming the object it opened.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// The bytes of one listing call's buffer: it holds many records, and any one name (at most 255
/// units on NTFS) with room to spare.
const LIST_BUFFER_BYTES: usize = 64 * 1024;

/// Where a `FILE_FULL_DIR_INFO` record's fields are: its next-entry offset, its attributes, its
/// name's length in bytes, and its name.
const NEXT_AT: usize = 0;
const NAME_BYTES_AT: usize = 60;
const NAME_AT: usize = 68;
const _: () = assert!(std::mem::offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset) == NEXT_AT);
const _: () = assert!(std::mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength) == NAME_BYTES_AT);
const _: () = assert!(std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName) == NAME_AT);

/// A reparse tag whose reparse point names another object: a symbolic link or a junction
/// (`IsReparseTagNameSurrogate`).
const NAME_SURROGATE: u32 = 0x2000_0000;

/// The rights of a new file's handle: read and write it, read its attributes and security, flush
/// it, and remove it again (`DELETE`) if it is not private.
const NEW_FILE_ACCESS: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE;

/// The rights of a handle that removes an entry: remove it, and read its attributes.
const REMOVE_ACCESS: u32 = DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// The sharing of a pin ([`PinnedDir`]): reading and writing, never delete. The pin reads the
/// directory (`FILE_LIST_DIRECTORY`): a handle with only attribute rights takes no part in
/// sharing, and its sharing would refuse nothing.
const SHARE_NO_DELETE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;

#[cfg(test)]
thread_local! {
    /// Test hook: runs after an entry is opened or made and before the object is checked.
    static AFTER_OPEN: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs the test hook [`AFTER_OPEN`], if one is set; does nothing outside tests.
fn after_open() {
    #[cfg(test)]
    AFTER_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook()
        }
    });
}

/// Opens the directory `path` for the `*_at` calls below. Links in `path` are followed: the
/// caller trusts this path. Anything that is not a directory is refused (`ERROR_DIRECTORY`, the
/// `ENOTDIR` of Windows). A NUL in `path`, or a native path longer than 32,767 units, is refused
/// before the native path is made.
#[allow(unsafe_code)]
pub fn open_dir(path: &Path) -> io::Result<File> {
    let name = native(path)?;
    // SAFETY: `name` is NUL-terminated and lives for the call; no security attributes or
    // template handle are passed.
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            DIR_ACCESS,
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
    if attribute_tag(dir.as_raw_handle())?.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(os_error(ERROR_DIRECTORY));
    }
    Ok(dir)
}

/// Opens the directory `name` in the open directory `dir`, never through a reparse point: a
/// reparse point at `name` (`{path} is a reparse point; rdy will not follow it`) or a
/// non-directory (`ERROR_DIRECTORY`) fails with an error that [`is_link_refusal`] accepts.
pub fn open_dir_at(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(dir, name, DIR_ACCESS, FILE_DIRECTORY_FILE)
}

/// Opens `name` in the open directory `dir` for reading, never through a reparse point, which is
/// refused as in [`open_dir_at`]. A directory opens too: the caller judges the kind of file from
/// the handle's metadata.
pub fn open_read_at(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(dir, name, FILE_GENERIC_READ, 0)
}

/// The kind of the entry `name` of the open directory `dir`, read from the entry itself opened
/// for its attributes alone, never followed and with no access to its content. A reparse point
/// that names another object (a symbolic link, a junction) is a link; one of any other tag (a
/// socket, a cloud file) is another kind of file.
pub fn entry_kind_at(dir: &File, name: &OsStr) -> io::Result<EntryKind> {
    let units = entry_name(dir, name)?;
    let entry = open_entry(dir, &units, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)
        .map_err(|status| status_error(dir, name, status))?;
    let info = attribute_tag(entry.as_raw_handle())?;
    Ok(kind_from(info.FileAttributes, info.ReparseTag))
}

/// Whether `e`, from [`open_dir_at`], [`open_read_at`] or [`super::open_no_follow`], means that
/// a reparse point stands at the name, or something that is not a directory where a directory
/// was opened.
pub fn is_link_refusal(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_DIRECTORY as i32) || is_reparse_refusal(e)
}

/// Whether the open directory `dir` holds no entry but `.` and `..`. It is listed through a new
/// open of `dir` itself, as in [`for_each_entry`]. A failed listing is an error, never "empty".
pub fn is_empty_dir(dir: &File) -> io::Result<bool> {
    let mut empty = true;
    each_name::<io::Error>(dir, |_| {
        empty = false;
        Ok(false)
    })?;
    Ok(empty)
}

/// Calls `f` with the name of each entry of the open directory `dir` but `.` and `..`, in the
/// order the directory lists them. The listing goes through a new open of `dir` itself (its
/// handle and an empty name), which must name the same directory, so no path is resolved again
/// and the listing has its own position. A failed listing is an error, and so is the first error
/// of `f`, which ends the listing.
pub fn for_each_entry<E: From<io::Error>>(
    dir: &File,
    mut f: impl FnMut(&OsStr) -> Result<(), E>,
) -> Result<(), E> {
    each_name(dir, |name| f(name).map(|()| true))
}

/// Lists `dir` as [`for_each_entry`] says; `visit` returns false to end the listing.
#[allow(unsafe_code)]
fn each_name<E: From<io::Error>>(
    dir: &File,
    mut visit: impl FnMut(&OsStr) -> Result<bool, E>,
) -> Result<(), E> {
    let listing = open_entry(
        dir,
        &[],
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_DIRECTORY_FILE,
    )
    .map_err(|status| io::Error::from_raw_os_error(dos_error(status)))?;
    // the directory's final path names it in an error; a path that cannot be read is empty
    let shown = final_path(dir.as_raw_handle()).unwrap_or_default();
    if FileId::of_open(&listing, &shown)? != FileId::of_open(dir, &shown)? {
        return Err(parent_changed(&shown).into());
    }
    let mut buf = vec![0u64; LIST_BUFFER_BYTES / 8];
    let mut class = FileFullDirectoryRestartInfo;
    loop {
        // SAFETY: `listing` is open for the call; the buffer is writable, 8-byte aligned (as the
        // records need) and of the size passed.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                listing.as_raw_handle(),
                class,
                buf.as_mut_ptr().cast(),
                LIST_BUFFER_BYTES as u32,
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                return Ok(());
            }
            return Err(e.into());
        }
        class = FileFullDirectoryInfo;
        // SAFETY: the bytes of the u64 buffer, which lives and is not written while this view
        // does; any byte value is a valid u8.
        let bytes =
            unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), LIST_BUFFER_BYTES) };
        if !records(bytes, &mut visit)? {
            return Ok(());
        }
    }
}

/// Calls `visit` with the name of each record of one listing buffer `buf`, but `.` and `..`,
/// until it returns false; returns whether the listing goes on. Each record's header, name length
/// and next offset are checked against `buf` before its name is copied: a malformed record is
/// `ERROR_BAD_LENGTH`, a name longer than 32,767 units is `ERROR_FILENAME_EXCED_RANGE`, and an
/// empty name, or one with a NUL or a separator, is `ERROR_INVALID_NAME`.
fn records<E: From<io::Error>>(
    buf: &[u8],
    visit: &mut impl FnMut(&OsStr) -> Result<bool, E>,
) -> Result<bool, E> {
    let u32_at = |at: usize| u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
    let bad = || os_error(ERROR_BAD_LENGTH);
    let mut at = 0usize;
    loop {
        let name_at = at
            .checked_add(NAME_AT)
            .filter(|end| *end <= buf.len())
            .ok_or_else(bad)?;
        let next = u32_at(at + NEXT_AT) as usize;
        let name_bytes = u32_at(at + NAME_BYTES_AT) as usize;
        if name_bytes / 2 > NATIVE_PATH_MAX {
            return Err(os_error(ERROR_FILENAME_EXCED_RANGE).into());
        }
        if !name_bytes.is_multiple_of(2) {
            return Err(bad().into());
        }
        let name_end = name_at
            .checked_add(name_bytes)
            .filter(|end| *end <= buf.len())
            .ok_or_else(bad)?;
        if next != 0 && next < NAME_AT + name_bytes {
            return Err(bad().into());
        }
        let units: Vec<u16> = buf[name_at..name_end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let is = |s: &str| units.iter().copied().eq(s.encode_utf16());
        if units.is_empty()
            || units
                .iter()
                .any(|&u| u == 0 || u == SLASH || u == BACKSLASH)
        {
            return Err(os_error(ERROR_INVALID_NAME).into());
        }
        if !is(".") && !is("..") && !visit(&OsString::from_wide(&units))? {
            return Ok(false);
        }
        if next == 0 {
            return Ok(true);
        }
        at = at.checked_add(next).ok_or_else(bad)?;
    }
}

/// Creates the new file `name` in the open directory `dir`, for reading and writing, private to
/// this account from the moment it exists: it is made relative to `dir` with this account's
/// private descriptor (on Unix, mode 0o600 less the umask). Anything at `name`, a reparse point
/// included, fails with `ErrorKind::AlreadyExists` and is never followed. The new file's privacy
/// is then read from its handle; a file that is not private (a file system that keeps no
/// security) is removed through that handle, never by name, and the create fails, `{path} is not
/// private to this account`.
pub fn create_private_at(dir: &File, name: &OsStr) -> io::Result<File> {
    create_private(dir, name, ObjectKind::File)
}

/// Creates the directory `name` in the open directory `dir`, private to this account from the
/// moment it exists, as [`create_private_at`] makes a file: an existing `name` is
/// `ErrorKind::AlreadyExists`, and a directory that is not private is removed through the handle
/// that made it. The handle is then closed (on Unix, mkdir returns none).
pub fn create_private_dir_at(dir: &File, name: &OsStr) -> io::Result<()> {
    create_private(dir, name, ObjectKind::Dir).map(drop)
}

/// Whether [`create_private_dir_at`] makes a private object here (the question that the umask
/// asks on Unix, where it can take bits from mode 0o700; Windows has no umask): it creates the
/// new file `name` in the open directory `dir` with this account's private descriptor (anything
/// at `name`, a reparse point included, fails with `ErrorKind::AlreadyExists` and is never
/// followed), reads whether it is private from the file's handle, and removes the file again
/// through that handle, so what another process puts at `name` meanwhile stays.
pub fn umask_keeps_0700_at(dir: &File, name: &OsStr) -> io::Result<bool> {
    let units = entry_name(dir, name)?;
    let sd = PrivateDescriptor::of_this_account(ObjectKind::File)?;
    let probe = create_entry(
        dir,
        &units,
        FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
        // no FILE_NON_DIRECTORY_FILE: with it, a directory at `name` is ACCESS_DENIED, not a
        // collision; FILE_CREATE without FILE_DIRECTORY_FILE makes a file
        0,
        &sd,
    )
    .map_err(|status| status_error(dir, name, status))?;
    after_open();
    let private = privacy(&probe).map(|p| p.mine && p.private);
    let removed = delete_open(&probe);
    drop(probe);
    let private = private?;
    removed?;
    Ok(private)
}

/// Removes the entry `name` (not a directory) from the open directory `dir`. The entry itself is
/// opened, never followed, and the object opened is removed through that handle: a reparse point
/// (a link, a junction) is removed as itself, and what another process puts at `name` after the
/// open stays. A directory that is not a link is refused (`ERROR_DIRECTORY_NOT_SUPPORTED`).
pub fn remove_file_at(dir: &File, name: &OsStr) -> io::Result<()> {
    let units = entry_name(dir, name)?;
    let entry = open_entry(dir, &units, REMOVE_ACCESS, 0)
        .map_err(|status| status_error(dir, name, status))?;
    after_open();
    let info = attribute_tag(entry.as_raw_handle())?;
    if info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
        && kind_from(info.FileAttributes, info.ReparseTag) != EntryKind::Link
    {
        return Err(os_error(ERROR_DIRECTORY_NOT_SUPPORTED));
    }
    delete_open(&entry)
}

/// Removes the empty directory `name` from the open directory `dir`. The entry itself is opened as
/// a directory, never followed, and removed through that handle, as in [`remove_file_at`]. A
/// reparse point at `name` is refused (`{path} is a reparse point; rdy will not follow it`, or
/// `ERROR_DIRECTORY` for one that is not a directory), with an error that [`is_link_refusal`]
/// accepts; a directory that is not empty stays (`ERROR_DIR_NOT_EMPTY`).
pub fn remove_dir_at(dir: &File, name: &OsStr) -> io::Result<()> {
    let units = entry_name(dir, name)?;
    let entry = open_entry(dir, &units, REMOVE_ACCESS, FILE_DIRECTORY_FILE)
        .map_err(|status| status_error(dir, name, status))?;
    after_open();
    if attribute_tag(entry.as_raw_handle())?.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(reparse_point(&entry_path(dir, name)));
    }
    delete_open(&entry)
}

/// Renames the entry `from` over the entry `to`, both in the open directory `dir`, replacing a
/// file at `to`. Both names are checked first, as for the opens. The directory is then pinned
/// ([`PinnedDir`]): while the call runs, nobody can move or remove it or a directory above it,
/// and its path is checked to name it before and after the move. The move is `MoveFileExW` on the
/// two verbatim paths in the pinned directory, so `from` and `to` stay entries of the directory
/// that `dir` opened, whatever its old path names now. It writes through (`MOVEFILE_WRITE_THROUGH`,
/// `windows_durable`'s move). A reparse point at `from` is moved itself, and one at `to` is
/// replaced itself, never followed. Nothing is copied: a move that would need a copy fails. As on
/// Windows generally, an existing directory at `to` is never replaced (`ERROR_ACCESS_DENIED`). A
/// failed move is the OS error.
pub fn rename_at(dir: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
    let from_units = entry_name(dir, from)?;
    let to_units = entry_name(dir, to)?;
    let pinned = PinnedDir::of(dir)?;
    let source = pinned.entry(from, &from_units)?;
    let target = pinned.entry(to, &to_units)?;
    after_open();
    let moved = move_entry(
        &source,
        &target,
        &pinned.shown.join(from),
        &pinned.shown.join(to),
    );
    after_open();
    moved?;
    pinned.check()
}

/// What [`start_in_dir`] and [`program_in_dir`] give the caller to hold until the child is
/// waited: a pin of the directory ([`PinnedDir`]). A child resolves its start directory and its
/// program from their paths when it starts, after the spawn has returned; while the pin is held,
/// nobody can move or remove the directory or a directory above it, so those paths still name it.
#[must_use = "hold it until the child is waited: on Windows it keeps the directory where it is"]
pub struct StartPin {
    /// Held: while it is open, the directory and those above it stay where they are.
    pinned: PinnedDir,
}

impl std::fmt::Debug for StartPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StartPin").field(&self.pinned.shown).finish()
    }
}

/// The path of a program in an open directory, from [`program_in_dir`], and the pin that keeps it
/// naming that directory's entry while the caller holds it.
#[must_use = "hold it until the child is waited: on Windows it keeps the directory where it is"]
#[derive(Debug)]
pub struct ProgramInDir {
    path: PathBuf,
    _pin: StartPin,
}

impl ProgramInDir {
    /// The program's absolute path for `Command::new`: the verbatim path of the entry in the
    /// pinned directory, so it is never looked up from this process's directory or the search
    /// path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Makes the child that `cmd` spawns start in the open directory `dir`. A child's start directory
/// is a path that the child resolves when it starts, after the spawn has returned. So the
/// directory is pinned ([`PinnedDir`]), its final path is checked to name it, and the child's
/// start directory is that verbatim path. The caller holds the returned [`StartPin`] until the
/// child is waited, and may close `dir`. A program in `dir` is named by [`program_in_dir`]: a
/// relative program (`.\name`) would be looked up from this process's directory.
pub fn start_in_dir(cmd: &mut std::process::Command, dir: &File) -> io::Result<StartPin> {
    let pinned = PinnedDir::of(dir)?;
    cmd.current_dir(OsString::from_wide(&pinned.verbatim));
    Ok(StartPin { pinned })
}

/// The absolute path of the program `name`, one entry of the open directory `dir`, for a child
/// that [`start_in_dir`] starts in `dir`. The name is checked first, as for the opens, and the
/// directory is pinned while the caller holds the result, so the path keeps naming the entry
/// of the directory that `dir` opened. A path longer than 32,767 units is refused before it is
/// made.
pub fn program_in_dir(dir: &File, name: &OsStr) -> io::Result<ProgramInDir> {
    let units = entry_name(dir, name)?;
    let pinned = PinnedDir::of(dir)?;
    let mut path = pinned.entry(name, &units)?;
    path.pop();
    Ok(ProgramInDir {
        path: PathBuf::from(OsString::from_wide(&path)),
        _pin: StartPin { pinned },
    })
}

/// A directory held so that its path keeps naming it, for a call that needs a path: a new handle
/// of the directory itself (its handle and an empty name), which reads it and shares reading and
/// writing but not delete. While the pin is open, nobody can move or remove the directory, and
/// NTFS refuses to move any directory above an open handle; entries in it can still be made,
/// moved and removed. The path is the pin's final path, which holds no reparse point, and
/// [`PinnedDir::check`] resolves it again and compares the identity it reaches with the pin's.
pub(super) struct PinnedDir {
    /// Held, never read: while it is open, the directory and those above it stay where they are.
    _pin: File,
    /// The final path, as messages show it.
    shown: PathBuf,
    /// The final path, verbatim (`\\?\`, or `\\?\UNC\`), without a NUL.
    verbatim: Vec<u16>,
    id: FileId,
}

impl PinnedDir {
    /// Pins the open directory `dir` (the pin must name it) and checks that its final path names
    /// it. A directory that another handle holds open to be moved or removed (`DELETE`) cannot be
    /// pinned: the OS's sharing violation.
    #[allow(unsafe_code)]
    pub(super) fn of(dir: &File) -> io::Result<PinnedDir> {
        let pin = in_dir(dir, &[], std::ptr::null(), |attributes, handle, status| {
            // SAFETY: as in `open_entry`.
            unsafe {
                NtOpenFile(
                    handle,
                    DIR_ACCESS,
                    attributes,
                    status,
                    SHARE_NO_DELETE,
                    FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                )
            }
        })
        .map_err(|status| io::Error::from_raw_os_error(dos_error(status)))?;
        after_open();
        let shown = final_path(pin.as_raw_handle())?;
        let verbatim = verbatim(&shown);
        let id = FileId::of_open(&pin, &shown)?;
        if FileId::of_open(dir, &shown)? != id {
            return Err(parent_changed(&shown));
        }
        let pinned = PinnedDir {
            _pin: pin,
            shown,
            verbatim,
            id,
        };
        pinned.check()?;
        Ok(pinned)
    }

    /// The pinned directory's path resolved again from its start, now: it must reach the pinned
    /// directory (`the directory holding {path} changed during the operation`). A path that cannot
    /// be opened is that open's error.
    #[allow(unsafe_code)]
    pub(super) fn check(&self) -> io::Result<()> {
        let mut path = self.verbatim.clone();
        path.push(0);
        // SAFETY: `path` is NUL-terminated and lives for the call; no security attributes or
        // template handle are passed.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                SHARE_ALL,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so `h` is a new handle that nothing else owns.
        let again = File::from(unsafe { OwnedHandle::from_raw_handle(h) });
        if FileId::of_open(&again, &self.shown)? != self.id {
            return Err(parent_changed(&self.shown));
        }
        Ok(())
    }

    /// The final path of the pinned directory, as messages show it.
    pub(super) fn shown(&self) -> &Path {
        &self.shown
    }

    /// The verbatim path of the entry `name` (its checked units `units`) in the pinned directory,
    /// NUL-terminated. A path longer than 32,767 units is refused before it is made (`the native
    /// path for {path} exceeds 32767 UTF-16 code units`).
    pub(super) fn entry(&self, name: &OsStr, units: &[u16]) -> io::Result<Vec<u16>> {
        if self.verbatim.len() + 1 + units.len() > NATIVE_PATH_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the native path for {} exceeds 32767 UTF-16 code units",
                    self.shown.join(name).display()
                ),
            ));
        }
        let mut path = Vec::with_capacity(self.verbatim.len() + units.len() + 2);
        path.extend_from_slice(&self.verbatim);
        if path.last() != Some(&BACKSLASH) {
            path.push(BACKSLASH);
        }
        path.extend_from_slice(units);
        path.push(0);
        Ok(path)
    }
}

/// The verbatim form of `shown`, a final path as [`final_path`] shows it: `\\server\share\...`
/// becomes `\\?\UNC\server\share\...`, any other `\\?\` and the path. A final path is already
/// whole and normalized, so nothing in it is changed.
fn verbatim(shown: &Path) -> Vec<u16> {
    let units: Vec<u16> = shown.as_os_str().encode_wide().collect();
    let unc: Vec<u16> = r"\\".encode_utf16().collect();
    match units.strip_prefix(&unc[..]) {
        Some(rest) => r"\\?\UNC\"
            .encode_utf16()
            .chain(rest.iter().copied())
            .collect(),
        None => r"\\?\"
            .encode_utf16()
            .chain(units.iter().copied())
            .collect(),
    }
}

/// [`create_private_at`] and [`create_private_dir_at`]: makes `name` in `dir` as a new object of
/// `kind` with this account's private descriptor, then checks from its handle that it is private,
/// or removes it through that handle.
fn create_private(dir: &File, name: &OsStr, kind: ObjectKind) -> io::Result<File> {
    let units = entry_name(dir, name)?;
    let sd = PrivateDescriptor::of_this_account(kind)
        .map_err(|e| not_made(&entry_path(dir, name), e))?;
    let (access, options) = match kind {
        // options 0, as in umask_keeps_0700_at: a directory at `name` is a collision
        ObjectKind::File => (NEW_FILE_ACCESS, 0),
        ObjectKind::Dir => (DIR_ACCESS | DELETE, FILE_DIRECTORY_FILE),
    };
    let made = create_entry(dir, &units, access, options, &sd)
        .map_err(|status| status_error(dir, name, status))?;
    after_open();
    match privacy(&made) {
        Ok(p) if p.mine && p.private => Ok(made),
        checked => {
            // the object this call made, through its own handle: never what is at `name` now
            let _ = delete_open(&made);
            let path = entry_path(dir, name);
            Err(match checked {
                Err(e) => not_made(&path, e),
                Ok(_) => not_private(&path),
            })
        }
    }
}

/// Removes the object that `f` is open on (with `DELETE`), whatever name it has now: its entry is
/// unlinked when the last handle with the delete set is closed, at once with POSIX semantics,
/// even while other handles keep the object open. The owner's read-only attribute does not stop
/// it, as a file's own mode does not stop unlink on Unix. A file system without the POSIX form
/// (`ERROR_INVALID_PARAMETER`, `ERROR_NOT_SUPPORTED` or `ERROR_INVALID_FUNCTION`) gets the
/// classic form, whose failure is the error. A directory that is not empty is
/// `ERROR_DIR_NOT_EMPTY`.
#[allow(unsafe_code)]
fn delete_open(f: &File) -> io::Result<()> {
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: `f` is open for the call; `ex` is a valid FILE_DISPOSITION_INFO_EX of the size
    // passed, which the call only reads.
    let ok = unsafe {
        SetFileInformationByHandle(
            f.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw const ex).cast::<c_void>(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    let no_posix_form = [
        ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED,
        ERROR_INVALID_FUNCTION,
    ]
    .iter()
    .any(|&code| e.raw_os_error() == Some(code as i32));
    if !no_posix_form {
        return Err(e);
    }
    let classic = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: as above, with a valid FILE_DISPOSITION_INFO.
    let ok = unsafe {
        SetFileInformationByHandle(
            f.as_raw_handle(),
            FileDispositionInfo,
            (&raw const classic).cast::<c_void>(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

const SLASH: u16 = b'/' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const COLON: u16 = b':' as u16;

/// [`open_dir_at`] and [`open_read_at`]: `name` in `dir`, opened with `access` and `options`,
/// itself and never followed; then a reparse point is refused, from the handle.
fn open_at(dir: &File, name: &OsStr, access: u32, options: u32) -> io::Result<File> {
    let units = entry_name(dir, name)?;
    let f = open_entry(dir, &units, access, options)
        .map_err(|status| status_error(dir, name, status))?;
    after_open();
    if attribute_tag(f.as_raw_handle())?.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(reparse_point(&entry_path(dir, name)));
    }
    Ok(f)
}

/// The entry `units` (one checked entry name, or empty for `dir` itself) of the open directory
/// `dir`, opened relative to `dir` with `access`, sharing everything, for synchronous I/O, the
/// entry itself and never a reparse point's target, and no reparse point followed on the way. A
/// failed open is its NTSTATUS.
#[allow(unsafe_code)]
pub(super) fn open_entry(
    dir: &File,
    units: &[u16],
    access: u32,
    options: u32,
) -> Result<File, NTSTATUS> {
    in_dir(
        dir,
        units,
        std::ptr::null(),
        |attributes, handle, status| {
            // SAFETY: `attributes`, `handle` and `status` are valid for the call, and what
            // `attributes` points to lives for it (see `in_dir`); NtOpenFile only reads the name.
            unsafe {
                NtOpenFile(
                    handle,
                    access,
                    attributes,
                    status,
                    SHARE_ALL,
                    options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                )
            }
        },
    )
}

/// The new entry `units` (one checked entry name) of the open directory `dir`, made relative to
/// `dir` (`FILE_CREATE`: anything at the name, a reparse point included, is
/// `STATUS_OBJECT_NAME_COLLISION` and is never opened) with the security descriptor `sd`, and
/// opened with `access`, sharing everything, for synchronous I/O. A failed create is its
/// NTSTATUS.
#[allow(unsafe_code)]
fn create_entry(
    dir: &File,
    units: &[u16],
    access: u32,
    options: u32,
    sd: &PrivateDescriptor,
) -> Result<File, NTSTATUS> {
    in_dir(
        dir,
        units,
        sd.as_ptr().cast_const().cast(),
        |attributes, handle, status| {
            // SAFETY: as in `open_entry`; no allocation size or extended attributes are passed, and
            // the descriptor lives as long as `sd`, for the call.
            unsafe {
                NtCreateFile(
                    handle,
                    access,
                    attributes,
                    status,
                    std::ptr::null(),
                    FILE_ATTRIBUTE_NORMAL,
                    SHARE_ALL,
                    FILE_CREATE,
                    options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                    std::ptr::null(),
                    0,
                )
            }
        },
    )
}

/// Calls `call` with the object attributes of the entry `units` of `dir` (no reparse point
/// followed on the way, case-insensitive, with the security descriptor `sd` or none), a handle
/// slot and a status block, all of which live for the call; the handle it fills is the result.
#[allow(unsafe_code)]
fn in_dir(
    dir: &File,
    units: &[u16],
    sd: *const SECURITY_DESCRIPTOR,
    call: impl FnOnce(&OBJECT_ATTRIBUTES, &mut HANDLE, &mut IO_STATUS_BLOCK) -> NTSTATUS,
) -> Result<File, NTSTATUS> {
    // an entry name is at most NATIVE_PATH_MAX units, so its length in bytes fits in a u16
    let bytes = (units.len() * 2) as u16;
    let name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: units.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: dir.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: sd,
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    let r = call(&attributes, &mut handle, &mut status);
    if r < 0 || handle.is_null() {
        return Err(r);
    }
    // SAFETY: the call succeeded, so `handle` is a new handle that nothing else owns.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

/// The error of a failed open of `name` in `dir`: a reparse point met on the way is the reparse
/// refusal; any other status is its Win32 error.
fn status_error(dir: &File, name: &OsStr, status: NTSTATUS) -> io::Error {
    if status == STATUS_REPARSE_POINT_ENCOUNTERED {
        return reparse_point(&entry_path(dir, name));
    }
    io::Error::from_raw_os_error(dos_error(status))
}

/// The Win32 error of the NTSTATUS `status`.
#[allow(unsafe_code)]
pub(super) fn dos_error(status: NTSTATUS) -> i32 {
    // SAFETY: the call takes a plain integer and has no preconditions.
    unsafe { RtlNtStatusToDosError(status) as i32 }
}

/// One entry name as UTF-16 units: at most 32,767 units and with no NUL (both checked before
/// the units are made: `the native path for {path} exceeds 32767 UTF-16 code units`, `the
/// native path for {path} contains a NUL`), then not empty, `.` or `..`, and with no `/`, `\` or
/// `:` (a stream).
pub(super) fn entry_name(dir: &File, name: &OsStr) -> io::Result<Vec<u16>> {
    let invalid = |text: String| io::Error::new(io::ErrorKind::InvalidInput, text);
    if name.encode_wide().count() > NATIVE_PATH_MAX {
        return Err(invalid(format!(
            "the native path for {} exceeds 32767 UTF-16 code units",
            entry_path(dir, name).display()
        )));
    }
    if name.encode_wide().any(|u| u == 0) {
        return Err(invalid(format!(
            "the native path for {} contains a NUL",
            entry_path(dir, name).display()
        )));
    }
    let units: Vec<u16> = name.encode_wide().collect();
    let is = |s: &str| units.iter().copied().eq(s.encode_utf16());
    if units.is_empty()
        || is(".")
        || is("..")
        || units
            .iter()
            .any(|&u| u == SLASH || u == BACKSLASH || u == COLON)
    {
        return Err(invalid("not one directory entry name".to_string()));
    }
    Ok(units)
}

/// `name` in `dir`, for messages: the directory's final path and the name, or the name alone if
/// that path cannot be read.
fn entry_path(dir: &File, name: &OsStr) -> PathBuf {
    final_path(dir.as_raw_handle())
        .map(|p| p.join(name))
        .unwrap_or_else(|_| PathBuf::from(name))
}

/// The kind of an entry with the attributes `attributes` and the reparse tag `tag`.
fn kind_from(attributes: u32, tag: u32) -> EntryKind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if tag & NAME_SURROGATE != 0 {
            EntryKind::Link
        } else {
            EntryKind::Other
        }
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Dir
    } else if attributes & FILE_ATTRIBUTE_DEVICE != 0 {
        EntryKind::Other
    } else {
        EntryKind::File
    }
}

/// Makes `link` a symbolic link to `target`: a directory link when `target` is a directory, a
/// file link otherwise (also when `target` is missing). Needs the symbolic-link privilege. For
/// test fixtures.
pub fn symlink(
    target: impl AsRef<std::path::Path>,
    link: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    if target.as_ref().is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Native NTFS cases of the provider itself: the exported signatures, a swap at the boundary
/// between an open and its check, every reparse tag at an entry, and hostile listing records.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::{OpenOptionsExt, symlink_dir, symlink_file};
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
    use windows_sys::Win32::System::SystemServices::{
        IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
    };

    fn id(path: &Path) -> FileId {
        let f = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        FileId::of_open(&f, path).unwrap()
    }

    fn id_of(f: &File) -> FileId {
        FileId::of_open(f, Path::new("the handle")).unwrap()
    }

    fn junction(target: &Path, link: &Path) {
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    /// Makes `path` (an empty file or directory) a reparse point of the non-Microsoft tag `tag`.
    #[allow(unsafe_code)]
    fn set_custom_tag(path: &Path, tag: u32) {
        let f = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        let mut buf = Vec::new();
        buf.extend(tag.to_le_bytes());
        buf.extend(4u16.to_le_bytes());
        buf.extend(0u16.to_le_bytes());
        buf.extend(*b"rdy-wx5a-reparse");
        buf.extend(*b"data");
        let mut returned = 0u32;
        // SAFETY: the handle is open for the call; the input buffer is readable for its length;
        // no output buffer or overlapped struct is passed.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr().cast(),
                buf.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "{}: {}", path.display(), io::Error::last_os_error());
    }

    /// A listing record: the name, its next offset, and a name length that may lie.
    fn record(name: &[u16], next: u32, name_bytes: u32) -> Vec<u8> {
        let mut r = vec![0u8; NAME_AT];
        r[NEXT_AT..NEXT_AT + 4].copy_from_slice(&next.to_le_bytes());
        r[NAME_BYTES_AT..NAME_BYTES_AT + 4].copy_from_slice(&name_bytes.to_le_bytes());
        for u in name {
            r.extend(u.to_le_bytes());
        }
        r
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// The names `records` gives for `buf`, or its error.
    fn parse(buf: &[u8]) -> Result<Vec<String>, io::Error> {
        let mut names = vec![];
        records::<io::Error>(buf, &mut |n| {
            names.push(n.to_string_lossy().into_owned());
            Ok(true)
        })?;
        Ok(names)
    }

    /// Runs `hook` after the next entry is opened or made on this thread, before it is checked,
    /// then removes it.
    fn on_next_open(hook: impl FnMut() + 'static) {
        AFTER_OPEN.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    fn no_hook() {
        AFTER_OPEN.with(|h| *h.borrow_mut() = None);
    }

    /// Grants every account full access to `path`.
    fn grant_everyone(path: &Path) {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .args(["/grant", "*S-1-1-0:(F)"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    /// The owner and DACL of what `path` names, read without following it.
    fn security_at(path: &Path) -> super::super::Security {
        let f = OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        super::super::security_of(&f).unwrap()
    }

    /// The security of a private object of `kind` of this account.
    fn private_security(kind: ObjectKind) -> super::super::Security {
        let me = super::super::current_account().unwrap();
        super::super::Security {
            owner: Some(me),
            protected: true,
            dacl: Some(vec![super::super::Ace::private(me, kind)]),
        }
    }

    fn names(dir: &File) -> Vec<String> {
        let mut names = vec![];
        for_each_entry::<io::Error>(dir, |n| {
            names.push(n.to_string_lossy().into_owned());
            Ok(())
        })
        .unwrap();
        names.sort();
        names
    }

    #[test]
    fn wx5c_native_the_pin_holds_at_every_boundary_of_a_rename() {
        let _: fn(&File, &OsStr, &OsStr) -> io::Result<()> = rename_at;
        let d = tempfile::tempdir().unwrap();
        let top = d.path().join("top");
        let p = top.join("p");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("a"), b"mine").unwrap();
        let dir = open_dir(&p).unwrap();
        let a_id = id(&p.join("a"));
        // without a pin, the directory moves while `dir` is open (its handle shares delete)
        fs::rename(&p, top.join("p-aside")).unwrap();
        fs::rename(top.join("p-aside"), &p).unwrap();
        // at each boundary of the call, nothing can move the directory or the one above it
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(i32, i32)>::new()));
        {
            let (seen, p, top) = (seen.clone(), p.clone(), top.clone());
            on_next_open(move || {
                let moved = fs::rename(&p, p.with_file_name("p-aside")).unwrap_err();
                let above = fs::rename(&top, top.with_file_name("top-aside")).unwrap_err();
                let removed = fs::remove_dir(&p).unwrap_err();
                assert_eq!(moved.raw_os_error(), removed.raw_os_error());
                seen.borrow_mut()
                    .push((moved.raw_os_error().unwrap(), above.raw_os_error().unwrap()));
            });
        }
        rename_at(&dir, OsStr::new("a"), OsStr::new("b")).unwrap();
        no_hook();
        // the pin's sharing violation (32) and the open handle's refusal (5), at the three
        // boundaries: after the pin, before the move and after the move
        assert_eq!(*seen.borrow(), [(32, 5), (32, 5), (32, 5)]);
        assert_eq!(names(&dir), ["b"]);
        assert_eq!(id(&p.join("b")), a_id);
        // and the pin ends with the call
        fs::rename(&p, top.join("p-aside")).unwrap();
        fs::rename(top.join("p-aside"), &p).unwrap();

        // a racer that replaces the old name before the move: the entry at the name moves, in
        // the pinned directory, as renameat moves what is at the name on Unix
        let decoy = tempfile::tempdir().unwrap();
        fs::write(decoy.path().join("b"), b"theirs").unwrap();
        {
            let p = p.clone();
            let mut boundary = 0;
            on_next_open(move || {
                boundary += 1;
                if boundary == 2 {
                    fs::rename(p.join("b"), p.join("b-aside")).unwrap();
                    fs::write(p.join("b"), b"racer").unwrap();
                }
            });
        }
        rename_at(&dir, OsStr::new("b"), OsStr::new("c")).unwrap();
        no_hook();
        assert_eq!(names(&dir), ["b-aside", "c"]);
        assert_eq!(fs::read(p.join("c")).unwrap(), b"racer");
        assert_eq!(id(&p.join("b-aside")), a_id);
        assert_eq!(fs::read(decoy.path().join("b")).unwrap(), b"theirs");

        // a directory that another handle holds open to be moved cannot be pinned: the call
        // fails with the sharing violation, and nothing moves
        let held = OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&p)
            .unwrap();
        let e = rename_at(&dir, OsStr::new("c"), OsStr::new("d")).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(32), "{e}");
        drop(held);
        assert_eq!(names(&dir), ["b-aside", "c"]);
    }

    #[test]
    fn wx5c_native_a_child_start_holds_the_pin_until_the_guard_drops() {
        let _: fn(&mut std::process::Command, &File) -> io::Result<StartPin> = start_in_dir;
        let _: fn(&File, &OsStr) -> io::Result<ProgramInDir> = program_in_dir;
        let d = tempfile::tempdir().unwrap();
        let top = d.path().join("top");
        let p = top.join("p");
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("marker"), b"mine").unwrap();
        fs::write(p.join("show.cmd"), "@type marker\r\n").unwrap();
        let dir = open_dir(&p).unwrap();
        // nothing can move the directory or the one above it, or remove it: the pin's sharing
        // violation (32) and the open handle's refusal (5)
        let refused = || {
            let moved = fs::rename(&p, top.join("p-aside")).unwrap_err();
            let above = fs::rename(&top, d.path().join("top-aside")).unwrap_err();
            let removed = fs::remove_dir(&p).unwrap_err();
            (
                moved.raw_os_error(),
                above.raw_os_error(),
                removed.raw_os_error(),
            )
        };
        let moves = || {
            fs::rename(&p, top.join("p-aside")).unwrap();
            fs::rename(top.join("p-aside"), &p).unwrap();
        };
        let shown = |out: std::process::Output| {
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };

        // the start directory and the program are verbatim paths of the pinned directory
        let program = program_in_dir(&dir, OsStr::new("show.cmd")).unwrap();
        let mut cmd = std::process::Command::new(program.path());
        cmd.stdout(std::process::Stdio::piped());
        let start = start_in_dir(&mut cmd, &dir).unwrap();
        let want = OsString::from_wide(&verbatim(&final_path(dir.as_raw_handle()).unwrap()));
        assert_eq!(cmd.get_current_dir().unwrap().as_os_str(), want);
        assert_eq!(program.path(), Path::new(&want).join("show.cmd"));
        // a child resolves both paths when it starts, after the spawn has returned: a move right
        // after the spawn is refused, and the child reads its own marker
        let child = cmd.spawn().unwrap();
        assert_eq!(refused(), (Some(32), Some(5), Some(32)));
        assert_eq!(shown(child.wait_with_output().unwrap()), "mine");
        // each guard pins the directory until it is dropped
        drop(start);
        assert_eq!(refused(), (Some(32), Some(5), Some(32)));
        drop(program);
        moves();
        let mut cmd = std::process::Command::new("cmd");
        let start = start_in_dir(&mut cmd, &dir).unwrap();
        assert_eq!(refused(), (Some(32), Some(5), Some(32)));
        drop(start);
        moves();

        // a directory that another handle holds open to be moved cannot be pinned: each call
        // fails with the sharing violation, and the command's start directory is not set
        let held = OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&p)
            .unwrap();
        let mut cmd = std::process::Command::new("cmd");
        let e = start_in_dir(&mut cmd, &dir).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(32), "{e}");
        assert_eq!(cmd.get_current_dir(), None);
        let e = program_in_dir(&dir, OsStr::new("show.cmd")).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(32), "{e}");
        drop(held);

        // a name is checked before the pin, with the opens' texts; so is the native bound
        let e = program_in_dir(&dir, OsStr::new("a\0b")).unwrap_err();
        assert!(e.to_string().ends_with("contains a NUL"), "{e}");
        let long = "n".repeat(32_767 - p.as_os_str().len());
        let e = program_in_dir(&dir, OsStr::new(&long)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        assert!(
            e.to_string().ends_with("exceeds 32767 UTF-16 code units"),
            "{e}"
        );
    }

    #[test]
    fn wx5c_native_a_pinned_path_that_names_another_directory_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        let dir = open_dir(&a).unwrap();
        let pinned = PinnedDir::of(&dir).unwrap();
        assert_eq!(pinned.shown, final_path(dir.as_raw_handle()).unwrap());
        pinned.check().unwrap();
        // the path that the pin checks resolves to another directory
        let other = final_path(open_dir(&b).unwrap().as_raw_handle()).unwrap();
        let wrong = PinnedDir {
            _pin: PinnedDir::of(&dir).unwrap()._pin,
            verbatim: verbatim(&other),
            shown: other.clone(),
            id: pinned.id,
        };
        let e = wrong.check().unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "the directory holding {} changed during the operation",
                other.display()
            )
        );
        assert_eq!(e.kind(), io::ErrorKind::Other);
        // a path that no longer opens is that open's error
        let gone = d.path().join("gone");
        let missing = PinnedDir {
            _pin: PinnedDir::of(&dir).unwrap()._pin,
            verbatim: verbatim(&gone),
            shown: gone,
            id: pinned.id,
        };
        assert_eq!(missing.check().unwrap_err().kind(), io::ErrorKind::NotFound);
        // the verbatim forms
        let w = |s: &str| String::from_utf16(&verbatim(Path::new(s))).unwrap();
        assert_eq!(w(r"C:\x\y"), r"\\?\C:\x\y");
        assert_eq!(w(r"\\server\share\x"), r"\\?\UNC\server\share\x");
    }

    #[test]
    fn wx5c_native_a_rename_keeps_every_name_exact() {
        let d = tempfile::tempdir().unwrap();
        let dir = open_dir(d.path()).unwrap();
        // names that a path call would change (a trailing dot or space, a lone surrogate) are
        // renamed as they are: the paths are verbatim
        let lone = OsString::from_wide(&[0x61, 0xD800]);
        let lone2 = OsString::from_wide(&[0x62, 0xD800]);
        for (from, to) in [
            (OsStr::new("dot."), OsStr::new("dot2.")),
            (OsStr::new("space "), OsStr::new("space2 ")),
            (lone.as_os_str(), lone2.as_os_str()),
        ] {
            drop(create_private_at(&dir, from).unwrap());
            rename_at(&dir, from, to).unwrap();
            let listed: Vec<OsString> = {
                let mut v = vec![];
                for_each_entry::<io::Error>(&dir, |n| {
                    v.push(n.to_os_string());
                    Ok(())
                })
                .unwrap();
                v
            };
            assert_eq!(listed, [to.to_os_string()], "{from:?}");
            remove_file_at(&dir, to).unwrap();
        }
        // the whole path is bounded before it is made: a name that passes its own bound, in a
        // directory whose path makes the total longer than 32,767 units
        let shown = final_path(dir.as_raw_handle()).unwrap();
        drop(create_private_at(&dir, OsStr::new("a")).unwrap());
        let long = "n".repeat(NATIVE_PATH_MAX - 4);
        for (from, to) in [("a", long.as_str()), (long.as_str(), "a")] {
            let e = rename_at(&dir, OsStr::new(from), OsStr::new(to)).unwrap_err();
            let name = if from == "a" { to } else { from };
            assert_eq!(
                (e.kind(), e.to_string()),
                (
                    io::ErrorKind::InvalidInput,
                    format!(
                        "the native path for {} exceeds 32767 UTF-16 code units",
                        shown.join(name).display()
                    )
                )
            );
        }
        assert_eq!(names(&dir), ["a"]);
        // Windows replaces a file with a directory (Unix refuses, ENOTDIR), and never replaces
        // a directory
        create_private_dir_at(&dir, OsStr::new("dir")).unwrap();
        create_private_dir_at(&dir, OsStr::new("empty")).unwrap();
        let e = rename_at(&dir, OsStr::new("a"), OsStr::new("empty")).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(5), "{e}");
        let e = rename_at(&dir, OsStr::new("dir"), OsStr::new("empty")).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(5), "{e}");
        rename_at(&dir, OsStr::new("dir"), OsStr::new("a")).unwrap();
        assert_eq!(
            entry_kind_at(&dir, OsStr::new("a")).unwrap(),
            EntryKind::Dir
        );
        assert_eq!(names(&dir), ["a", "empty"]);
    }

    #[test]
    fn wx5b_native_seam_exports_keep_their_contract() {
        let _: fn(&File, &OsStr) -> io::Result<File> = create_private_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = create_private_dir_at;
        let _: fn(&File, &OsStr) -> io::Result<bool> = umask_keeps_0700_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = remove_file_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = remove_dir_at;
        // a new file's handle reads, writes, flushes, reads its security and takes WRITE_DAC
        let d = tempfile::tempdir().unwrap();
        let dir = open_dir(d.path()).unwrap();
        let mut f = create_private_at(&dir, OsStr::new("f")).unwrap();
        io::Write::write_all(&mut f, b"bytes").unwrap();
        f.sync_all().unwrap();
        let mut back = String::new();
        io::Seek::rewind(&mut f).unwrap();
        io::Read::read_to_string(&mut f, &mut back).unwrap();
        assert_eq!(back, "bytes");
        super::super::make_private_file(&f).unwrap();
        assert!(super::super::privacy(&f).unwrap().private);
        // an ordinary existing name is AlreadyExists, the OS's error 183
        let e = create_private_at(&dir, OsStr::new("f")).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(183), "{e}");
        // a directory is not removed as a file
        create_private_dir_at(&dir, OsStr::new("sub")).unwrap();
        let e = remove_file_at(&dir, OsStr::new("sub")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::IsADirectory, "{e}");
        assert_eq!(names(&dir), ["f", "sub"]);
    }

    #[test]
    fn wx5b_native_a_swap_before_cleanup_keeps_the_replacement() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_path_buf();
        let dir = open_dir(&p).unwrap();

        // a file is opened for removal, then moved aside and replaced before it is removed: the
        // object opened is removed, and the replacement stays
        fs::write(p.join("f"), b"mine").unwrap();
        let mine = id(&p.join("f"));
        let at = p.clone();
        on_next_open(move || {
            fs::rename(at.join("f"), at.join("f-aside")).unwrap();
            fs::write(at.join("f"), b"theirs").unwrap();
        });
        remove_file_at(&dir, OsStr::new("f")).unwrap();
        no_hook();
        assert_eq!(names(&dir), ["f"]);
        assert_eq!(fs::read(p.join("f")).unwrap(), b"theirs");
        assert_ne!(id(&p.join("f")), mine);
        fs::remove_file(p.join("f")).unwrap();

        // the same for a directory
        fs::create_dir(p.join("s")).unwrap();
        let at = p.clone();
        on_next_open(move || {
            fs::rename(at.join("s"), at.join("s-aside")).unwrap();
            fs::create_dir(at.join("s")).unwrap();
            fs::write(at.join("s").join("theirs"), b"theirs").unwrap();
        });
        remove_dir_at(&dir, OsStr::new("s")).unwrap();
        no_hook();
        assert_eq!(names(&dir), ["s"]);
        assert_eq!(fs::read(p.join("s").join("theirs")).unwrap(), b"theirs");
        fs::remove_dir_all(p.join("s")).unwrap();

        // a directory is opened for removal, then swapped for a junction: the junction's target
        // is never emptied or removed
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("keep"), b"keep").unwrap();
        fs::create_dir(p.join("s")).unwrap();
        let (at, target) = (p.clone(), elsewhere.path().to_path_buf());
        on_next_open(move || {
            fs::rename(at.join("s"), at.join("s-aside")).unwrap();
            junction(&target, &at.join("s"));
        });
        remove_dir_at(&dir, OsStr::new("s")).unwrap();
        no_hook();
        assert_eq!(names(&dir), ["s"]);
        assert_eq!(
            entry_kind_at(&dir, OsStr::new("s")).unwrap(),
            EntryKind::Link
        );
        assert_eq!(fs::read(elsewhere.path().join("keep")).unwrap(), b"keep");
        remove_file_at(&dir, OsStr::new("s")).unwrap();
        assert!(names(&dir).is_empty());

        // a new file has the private descriptor at the moment it exists; it is then opened to
        // every account and replaced at its name before the create checks it: the create fails,
        // removes the file it made, and leaves the replacement
        let at = p.clone();
        on_next_open(move || {
            assert_eq!(
                security_at(&at.join("t")),
                private_security(ObjectKind::File),
                "at creation"
            );
            fs::rename(at.join("t"), at.join("t-aside")).unwrap();
            grant_everyone(&at.join("t-aside"));
            fs::write(at.join("t"), b"theirs").unwrap();
        });
        let e = create_private_at(&dir, OsStr::new("t")).unwrap_err();
        no_hook();
        let shown = final_path(dir.as_raw_handle()).unwrap().join("t");
        assert_eq!(
            (e.kind(), e.to_string()),
            (
                io::ErrorKind::PermissionDenied,
                format!("{} is not private to this account", shown.display())
            )
        );
        assert_eq!(names(&dir), ["t"]);
        assert_eq!(fs::read(p.join("t")).unwrap(), b"theirs");
        fs::remove_file(p.join("t")).unwrap();

        // the same for a new directory
        let at = p.clone();
        on_next_open(move || {
            assert_eq!(
                security_at(&at.join("t")),
                private_security(ObjectKind::Dir),
                "at creation"
            );
            fs::rename(at.join("t"), at.join("t-aside")).unwrap();
            grant_everyone(&at.join("t-aside"));
            fs::create_dir(at.join("t")).unwrap();
        });
        let e = create_private_dir_at(&dir, OsStr::new("t")).unwrap_err();
        no_hook();
        assert_eq!(
            e.to_string(),
            format!("{} is not private to this account", shown.display())
        );
        assert_eq!(names(&dir), ["t"]);
        fs::remove_dir(p.join("t")).unwrap();

        // the probe file is replaced at its name before it is removed: it is removed through its
        // own handle, and the replacement stays
        let at = p.clone();
        on_next_open(move || {
            assert_eq!(
                security_at(&at.join(".probe")),
                private_security(ObjectKind::File)
            );
            fs::rename(at.join(".probe"), at.join(".probe-aside")).unwrap();
            fs::write(at.join(".probe"), b"theirs").unwrap();
        });
        assert!(umask_keeps_0700_at(&dir, OsStr::new(".probe")).unwrap());
        no_hook();
        assert_eq!(names(&dir), [".probe"]);
        assert_eq!(fs::read(p.join(".probe")).unwrap(), b"theirs");

        // the probe answers no when what it made is not private, and still removes only it
        fs::remove_file(p.join(".probe")).unwrap();
        let at = p.clone();
        on_next_open(move || grant_everyone(&at.join(".probe")));
        assert!(!umask_keeps_0700_at(&dir, OsStr::new(".probe")).unwrap());
        no_hook();
        assert!(names(&dir).is_empty());
    }

    #[test]
    fn wx5b_native_every_reparse_tag_at_cleanup_is_never_followed() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("victim"), b"keep").unwrap();
        fs::create_dir(elsewhere.path().join("vdir")).unwrap();
        fs::write(elsewhere.path().join("vdir").join("inside"), b"keep").unwrap();
        let p = d.path();
        let make = |p: &Path| {
            junction(&elsewhere.path().join("vdir"), &p.join("junction"));
            symlink_dir(elsewhere.path().join("vdir"), p.join("dirlink")).unwrap();
            symlink_file(elsewhere.path().join("victim"), p.join("filelink")).unwrap();
            symlink_file(p.join("nowhere"), p.join("dangling")).unwrap();
            fs::write(p.join("customfile"), b"").unwrap();
            set_custom_tag(&p.join("customfile"), 0x0000_1234);
            fs::create_dir(p.join("customdir")).unwrap();
            set_custom_tag(&p.join("customdir"), 0x0000_1234);
        };
        make(p);
        let dir = open_dir(p).unwrap();
        let intact = || {
            assert_eq!(fs::read(elsewhere.path().join("victim")).unwrap(), b"keep");
            assert_eq!(
                fs::read(elsewhere.path().join("vdir").join("inside")).unwrap(),
                b"keep"
            );
            assert!(!p.join("nowhere").exists());
        };
        let all = [
            "customdir",
            "customfile",
            "dangling",
            "dirlink",
            "filelink",
            "junction",
        ];
        for name in all {
            let name = OsStr::new(name);
            // a create at the entry is AlreadyExists and never reaches what it names
            for e in [
                create_private_at(&dir, name).unwrap_err(),
                create_private_dir_at(&dir, name).unwrap_err(),
                umask_keeps_0700_at(&dir, name).unwrap_err(),
            ] {
                assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{name:?}: {e}");
            }
            // a directory removal refuses every reparse point, with the exact text where it
            // opened one as a directory
            let e = remove_dir_at(&dir, name).unwrap_err();
            assert!(is_link_refusal(&e), "{name:?}: {e}");
            if e.raw_os_error() != Some(ERROR_DIRECTORY as i32) {
                assert_eq!(
                    e.to_string(),
                    format!(
                        "{} is a reparse point; rdy will not follow it",
                        final_path(dir.as_raw_handle())
                            .unwrap()
                            .join(name)
                            .display()
                    )
                );
            }
            intact();
        }
        assert_eq!(names(&dir), all);
        // a file removal removes each link and the reparse file itself; a directory that is not
        // a link stays
        let e = remove_file_at(&dir, OsStr::new("customdir")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::IsADirectory, "{e}");
        for name in ["customfile", "dangling", "dirlink", "filelink", "junction"] {
            remove_file_at(&dir, OsStr::new(name)).unwrap();
            intact();
        }
        assert_eq!(names(&dir), ["customdir"]);
        let vdir = open_dir(&elsewhere.path().join("vdir")).unwrap();
        assert_eq!(names(&vdir), ["inside"]);
        assert_eq!(
            names(&open_dir(elsewhere.path()).unwrap()),
            ["vdir", "victim"]
        );
    }

    #[test]
    fn wx5a_native_seam_exports_keep_their_contract() {
        let _: fn(&Path) -> io::Result<File> = open_dir;
        let _: fn(&File, &OsStr) -> io::Result<File> = open_dir_at;
        let _: fn(&File, &OsStr) -> io::Result<File> = open_read_at;
        let _: fn(&File, &OsStr) -> io::Result<EntryKind> = entry_kind_at;
        let _: fn(&File) -> io::Result<bool> = is_empty_dir;
        #[allow(clippy::type_complexity)]
        let _: fn(&File, &mut dyn FnMut(&OsStr) -> io::Result<()>) -> io::Result<()> =
            |d, f| for_each_entry(d, f);
        let _: fn(&io::Error) -> bool = is_link_refusal;
        // a directory handle reads its security and attributes: privacy and metadata work on it
        let d = tempfile::tempdir().unwrap();
        let dir = open_dir(d.path()).unwrap();
        assert!(dir.metadata().unwrap().is_dir());
        super::super::privacy(&dir).unwrap();
        fs::create_dir(d.path().join("s")).unwrap();
        let s = open_dir_at(&dir, OsStr::new("s")).unwrap();
        assert!(s.metadata().unwrap().is_dir());
        super::super::privacy(&s).unwrap();
        // open_no_follow's reparse refusal is a link refusal too
        junction(&d.path().join("s"), &d.path().join("j"));
        let e = super::super::open_no_follow(OpenOptions::new().read(true), &d.path().join("j"))
            .unwrap_err();
        assert!(is_link_refusal(&e), "{e}");
        assert!(!is_link_refusal(&os_error(ERROR_BAD_LENGTH)));
        assert!(!is_link_refusal(&io::Error::other(
            "x is a reparse point; rdy will not follow it"
        )));
    }

    #[test]
    fn wx5a_native_a_swap_after_the_open_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let evil = tempfile::tempdir().unwrap();
        fs::write(evil.path().join("f"), b"theirs").unwrap();
        fs::create_dir(d.path().join("s")).unwrap();
        fs::write(d.path().join("s").join("f"), b"mine").unwrap();
        let dir = open_dir(d.path()).unwrap();
        let s_id = id(&d.path().join("s"));

        // the real directory is opened, then swapped for a junction before the check: the
        // handle is still the real directory
        let (at, aside, target) = (
            d.path().join("s"),
            d.path().join("s-aside"),
            evil.path().to_path_buf(),
        );
        AFTER_OPEN.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                fs::rename(&at, &aside).unwrap();
                junction(&target, &at);
            }))
        });
        let s = open_dir_at(&dir, OsStr::new("s")).unwrap();
        AFTER_OPEN.with(|h| *h.borrow_mut() = None);
        assert_eq!(id_of(&s), s_id);
        let f = open_read_at(&s, OsStr::new("f")).unwrap();
        assert_eq!(
            fs::read(d.path().join("s-aside").join("f")).unwrap(),
            b"mine"
        );
        drop(f);

        // a junction is opened, then swapped for a real directory before the check: the handle
        // is still the junction, and it is refused
        let (at, target) = (d.path().join("s"), d.path().join("s-aside"));
        AFTER_OPEN.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                // the junction is moved aside (its open handle shares delete), not removed
                fs::rename(&at, at.with_file_name("junction-aside")).unwrap();
                fs::rename(&target, &at).unwrap();
            }))
        });
        let e = open_dir_at(&dir, OsStr::new("s")).unwrap_err();
        AFTER_OPEN.with(|h| *h.borrow_mut() = None);
        assert!(is_link_refusal(&e), "{e}");
        assert_eq!(
            e.to_string(),
            format!(
                "{} is a reparse point; rdy will not follow it",
                final_path(dir.as_raw_handle()).unwrap().join("s").display()
            )
        );
        // the real directory is back at the entry and opens
        assert_eq!(id_of(&open_dir_at(&dir, OsStr::new("s")).unwrap()), s_id);
        assert_eq!(fs::read(evil.path().join("f")).unwrap(), b"theirs");
    }

    #[test]
    fn wx5a_native_every_reparse_tag_at_an_entry_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("victim"), b"keep").unwrap();
        fs::create_dir(elsewhere.path().join("vdir")).unwrap();
        let p = d.path();
        junction(&elsewhere.path().join("vdir"), &p.join("junction"));
        symlink_dir(elsewhere.path().join("vdir"), p.join("dirlink")).unwrap();
        symlink_file(elsewhere.path().join("victim"), p.join("filelink")).unwrap();
        symlink_file(p.join("nowhere"), p.join("dangling")).unwrap();
        fs::write(p.join("customfile"), b"").unwrap();
        set_custom_tag(&p.join("customfile"), 0x0000_1234);
        fs::create_dir(p.join("customdir")).unwrap();
        set_custom_tag(&p.join("customdir"), 0x0000_1234);
        let dir = open_dir(p).unwrap();
        for (name, kind, tag) in [
            (
                "junction",
                EntryKind::Link,
                Some(IO_REPARSE_TAG_MOUNT_POINT),
            ),
            ("dirlink", EntryKind::Link, Some(IO_REPARSE_TAG_SYMLINK)),
            ("filelink", EntryKind::Link, Some(IO_REPARSE_TAG_SYMLINK)),
            ("dangling", EntryKind::Link, Some(IO_REPARSE_TAG_SYMLINK)),
            ("customfile", EntryKind::Other, Some(0x0000_1234)),
            ("customdir", EntryKind::Other, Some(0x0000_1234)),
        ] {
            let name = OsStr::new(name);
            assert_eq!(entry_kind_at(&dir, name).unwrap(), kind, "{name:?}");
            let entry = open_entry(
                &dir,
                &wide(&name.to_string_lossy()),
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                0,
            )
            .unwrap();
            assert_eq!(
                Some(attribute_tag(entry.as_raw_handle()).unwrap().ReparseTag),
                tag
            );
            let shown = final_path(dir.as_raw_handle()).unwrap().join(name);
            let e = open_read_at(&dir, name).unwrap_err();
            assert!(is_link_refusal(&e), "{name:?}: {e}");
            assert_eq!(
                e.to_string(),
                format!(
                    "{} is a reparse point; rdy will not follow it",
                    shown.display()
                )
            );
            let e = open_dir_at(&dir, name).unwrap_err();
            assert!(is_link_refusal(&e), "{name:?}: {e}");
            assert!(
                e.raw_os_error() == Some(ERROR_DIRECTORY as i32) || is_reparse_refusal(&e),
                "{name:?}: {e}"
            );
        }
        // the listing names every entry, and lists none of what they point to
        let mut names = vec![];
        for_each_entry::<io::Error>(&dir, |n| {
            names.push(n.to_string_lossy().into_owned());
            Ok(())
        })
        .unwrap();
        names.sort();
        assert_eq!(
            names,
            [
                "customdir",
                "customfile",
                "dangling",
                "dirlink",
                "filelink",
                "junction"
            ]
        );
        assert!(!is_empty_dir(&dir).unwrap());
        assert_eq!(fs::read(elsewhere.path().join("victim")).unwrap(), b"keep");
        assert!(
            fs::read_dir(elsewhere.path().join("vdir"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!p.join("nowhere").exists());
        // a plain file and a plain directory are their kinds
        fs::write(p.join("file"), b"x").unwrap();
        fs::create_dir(p.join("sub")).unwrap();
        assert_eq!(
            entry_kind_at(&dir, OsStr::new("file")).unwrap(),
            EntryKind::File
        );
        assert_eq!(
            entry_kind_at(&dir, OsStr::new("sub")).unwrap(),
            EntryKind::Dir
        );
        assert_eq!(kind_from(FILE_ATTRIBUTE_DEVICE, 0), EntryKind::Other);
    }

    #[test]
    fn wx5a_native_hostile_listing_records_are_bounded_errors() {
        // two good records, and `.` and `..`, which are skipped
        let mut buf = record(&wide("."), 72, 2);
        buf.resize(72, 0);
        let second = record(&wide(".."), 72, 4);
        buf.extend(&second);
        buf.resize(144, 0);
        let mut third = record(&wide("name"), 80, 8);
        third.resize(80, 0);
        buf.extend(&third);
        buf.extend(record(&wide("last"), 0, 8));
        assert_eq!(parse(&buf).unwrap(), ["name", "last"]);
        // a callback that stops ends the listing
        let mut seen = 0;
        let more = records::<io::Error>(&buf, &mut |_| {
            seen += 1;
            Ok(false)
        })
        .unwrap();
        assert_eq!((more, seen), (false, 1));

        let code = |buf: &[u8]| {
            let e = parse(buf).unwrap_err();
            assert_ne!(e.kind(), io::ErrorKind::InvalidData, "{e}");
            e.raw_os_error().unwrap() as u32
        };
        let good = record(&wide("a"), 0, 2);
        // every truncation of a record is a bounded error
        for cut in 0..good.len() {
            assert_eq!(code(&good[..cut]), ERROR_BAD_LENGTH, "cut at {cut}");
        }
        // an odd name length, a name past the buffer, a next offset into the record or past
        // the buffer
        assert_eq!(code(&record(&wide("ab"), 0, 3)), ERROR_BAD_LENGTH);
        assert_eq!(code(&record(&wide("a"), 0, 4)), ERROR_BAD_LENGTH);
        assert_eq!(code(&record(&wide("a"), 0, 65_534)), ERROR_BAD_LENGTH);
        assert_eq!(code(&record(&wide("a"), 4, 2)), ERROR_BAD_LENGTH);
        assert_eq!(code(&record(&wide("a"), 69, 2)), ERROR_BAD_LENGTH);
        assert_eq!(code(&record(&wide("a"), 70, 2)), ERROR_BAD_LENGTH);
        assert_eq!(code(&record(&wide("a"), u32::MAX, 2)), ERROR_BAD_LENGTH);
        // a name longer than any native path is refused before anything is copied, even when
        // the buffer holds it
        let mut called = false;
        let huge = record(&vec![u16::from(b'x'); 32_768], 0, 65_536);
        let e = records::<io::Error>(&huge, &mut |_| {
            called = true;
            Ok(true)
        })
        .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERROR_FILENAME_EXCED_RANGE as i32));
        assert!(!called);
        assert_eq!(code(&record(&[], 0, u32::MAX)), ERROR_FILENAME_EXCED_RANGE);
        assert_eq!(code(&record(&[], 0, 65_536)), ERROR_FILENAME_EXCED_RANGE);
        // the longest name is copied whole
        let longest = record(&vec![u16::from(b'y'); 32_767], 0, 65_534);
        assert_eq!(parse(&longest).unwrap(), ["y".repeat(32_767)]);
        // an empty name, a NUL or a separator is not one entry name
        for bad in [&[][..], &[0x61, 0][..], &wide("a\\b")[..], &wide("a/b")[..]] {
            let r = record(bad, 0, (bad.len() * 2) as u32);
            assert_eq!(code(&r), ERROR_INVALID_NAME, "{bad:?}");
        }
        // an error after a good record ends the listing at the bad one
        let mut buf = record(&wide("ok"), 72, 4);
        buf.resize(72, 0);
        buf.extend(record(&wide("x"), 0, 3));
        let mut names = vec![];
        let e = records::<io::Error>(&buf, &mut |n| {
            names.push(n.to_os_string());
            Ok(true)
        })
        .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERROR_BAD_LENGTH as i32));
        assert_eq!(names, ["ok"]);
    }

    #[test]
    fn wx5a_native_entry_names_are_bounded_before_use() {
        let d = tempfile::tempdir().unwrap();
        let dir = open_dir(d.path()).unwrap();
        let shown = final_path(dir.as_raw_handle()).unwrap();
        let long = "n".repeat(NATIVE_PATH_MAX + 1);
        let e = entry_name(&dir, OsStr::new(&long)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            e.to_string(),
            format!(
                "the native path for {} exceeds 32767 UTF-16 code units",
                shown.join(&long).display()
            )
        );
        // the bound is the length of a UNICODE_STRING: the longest name passes it, and the open
        // is the file system's to refuse
        let longest = "n".repeat(NATIVE_PATH_MAX);
        assert_eq!(
            entry_name(&dir, OsStr::new(&longest)).unwrap().len(),
            NATIVE_PATH_MAX
        );
        let e = open_read_at(&dir, OsStr::new(&longest)).unwrap_err();
        let shown_e: String = e.to_string().chars().take(80).collect();
        assert!(!is_link_refusal(&e), "{shown_e}");
        let e = entry_name(&dir, OsStr::new("a\0")).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "the native path for {} contains a NUL",
                shown.join("a\0").display()
            )
        );
        for bad in ["", ".", "..", "a/b", "a\\b", "a:b", ":"] {
            let e = entry_name(&dir, OsStr::new(bad)).unwrap_err();
            assert_eq!(
                (e.kind(), e.to_string()),
                (
                    io::ErrorKind::InvalidInput,
                    "not one directory entry name".into()
                ),
                "{bad:?}"
            );
        }
        // a name that is not valid UTF-16 is one entry name
        let lone = OsString::from_wide(&[0x61, 0xD800]);
        assert_eq!(entry_name(&dir, &lone).unwrap(), [0x61, 0xD800]);
        // open_dir bounds its path before it is made absolute
        let e = open_dir(&d.path().join("p".repeat(40_000))).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(e.to_string().ends_with(" exceeds 32767 UTF-16 code units"));
    }
}
