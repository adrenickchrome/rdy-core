//! The Windows calls of [`crate::pio`] in an open directory (FW-XP1 Phase 4): opens, entry kinds
//! and listings anchored to a directory handle.
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
//! - Bounds: an entry name longer than 32,767 UTF-16 code units, or with a NUL, is refused before
//!   anything is made of it. Each listed record is checked against the listing buffer, and its
//!   name against the same bound, before the name is copied.
//!
//! This file depends only on `std`, `windows-sys`, [`crate::fileid`], [`EntryKind`] and its
//! sibling `windows_security`, so an isolated probe can compile and test it with those files.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtOpenFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_BAD_LENGTH, ERROR_DIRECTORY, ERROR_FILENAME_EXCED_RANGE, ERROR_INVALID_NAME,
    ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, OBJ_CASE_INSENSITIVE,
    OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_REPARSE_POINT_ENCOUNTERED, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO, FILE_GENERIC_READ, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetFileInformationByHandleEx,
    OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::windows_security::{
    NATIVE_PATH_MAX, attribute_tag, final_path, is_reparse_refusal, native, os_error,
    parent_changed, reparse_point,
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

#[cfg(test)]
thread_local! {
    /// Test hook: runs after an entry is opened and before the opened object is checked.
    static AFTER_OPEN: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
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

const SLASH: u16 = b'/' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const COLON: u16 = b':' as u16;

/// [`open_dir_at`] and [`open_read_at`]: `name` in `dir`, opened with `access` and `options`,
/// itself and never followed; then a reparse point is refused, from the handle.
fn open_at(dir: &File, name: &OsStr, access: u32, options: u32) -> io::Result<File> {
    let units = entry_name(dir, name)?;
    let f = open_entry(dir, &units, access, options)
        .map_err(|status| status_error(dir, name, status))?;
    #[cfg(test)]
    AFTER_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook()
        }
    });
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
fn open_entry(dir: &File, units: &[u16], access: u32, options: u32) -> Result<File, NTSTATUS> {
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
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: `dir` is open for the call; `name` points at `units`, which lives for the call and
    // which NtOpenFile only reads; `attributes`, `handle` and `status` are valid for the call.
    let r = unsafe {
        NtOpenFile(
            &mut handle,
            access,
            &attributes,
            &mut status,
            SHARE_ALL,
            options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
        )
    };
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
fn dos_error(status: NTSTATUS) -> i32 {
    // SAFETY: the call takes a plain integer and has no preconditions.
    unsafe { RtlNtStatusToDosError(status) as i32 }
}

/// One entry name as UTF-16 units: at most 32,767 units and with no NUL (both checked before
/// the units are made: `the native path for {path} exceeds 32767 UTF-16 code units`, `the
/// native path for {path} contains a NUL`), then not empty, `.` or `..`, and with no `/`, `\` or
/// `:` (a stream).
fn entry_name(dir: &File, name: &OsStr) -> io::Result<Vec<u16>> {
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
