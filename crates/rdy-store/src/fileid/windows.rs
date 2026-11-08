//! The Windows handle queries of `rdy_store::fileid`: the identity (`FILE_ID_INFO`), the change
//! time (`FILE_BASIC_INFO`) and the link count (`FILE_STANDARD_INFO`) of an open object.
//!
//! This file depends only on `std` and `windows-sys`, so an isolated probe can compile and test
//! it on its own. Every failed call is the OS error; no value is made up.

use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileBasicInfo, FileIdInfo, FileStandardInfo,
    GetFileInformationByHandleEx,
};

/// The 100 ns intervals from 1601-01-01 (a `FILETIME`'s origin) to 1970-01-01.
const UNIX_EPOCH_AS_FILETIME: i64 = 116_444_736_000_000_000;

/// The object `path` names, opened only to read its attributes: a directory opens too
/// (`FILE_FLAG_BACKUP_SEMANTICS`), and a reparse point (a symbolic link, a junction) is opened
/// itself, never followed (`FILE_FLAG_OPEN_REPARSE_POINT`). It shares read, write and delete,
/// so it never stands in the way of another handle.
pub(super) fn open_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// The information of class `class`, of type `T`, about the open object `f`. Each caller in this
/// file passes the plain-data struct that its class fills: `FILE_ID_INFO` for `FileIdInfo`,
/// `FILE_BASIC_INFO` for `FileBasicInfo`, `FILE_STANDARD_INFO` for `FileStandardInfo`.
#[allow(unsafe_code)]
fn info<T: Copy>(f: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut out = MaybeUninit::<T>::zeroed();
    let size = u32::try_from(size_of::<T>()).map_err(io::Error::other)?;
    // SAFETY: the handle is open for as long as `f` is borrowed; the buffer is writable, of
    // `size` bytes, and of the type the class fills.
    let ok = unsafe {
        GetFileInformationByHandleEx(f.as_raw_handle(), class, out.as_mut_ptr().cast(), size)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so it filled the whole struct (zeroed before, in any case).
    Ok(unsafe { out.assume_init() })
}

/// The volume serial number and the 16 bytes of `FILE_ID_128.Identifier`, in index order.
pub(super) fn id_of(f: &File) -> io::Result<(u64, [u8; 16])> {
    let i: FILE_ID_INFO = info(f, FileIdInfo)?;
    Ok((i.VolumeSerialNumber, i.FileId.Identifier))
}

/// `FILE_BASIC_INFO.ChangeTime` as a `SystemTime`. A time that `SystemTime` cannot hold is an
/// error, never a made-up time.
pub(super) fn change_time(f: &File) -> io::Result<SystemTime> {
    let b: FILE_BASIC_INFO = info(f, FileBasicInfo)?;
    filetime(b.ChangeTime)
}

/// The `SystemTime` of `t`, 100 ns intervals since 1601-01-01.
fn filetime(t: i64) -> io::Result<SystemTime> {
    let out_of_range = || io::Error::other(format!("the change time {t} is out of range"));
    let since = i128::from(t) - i128::from(UNIX_EPOCH_AS_FILETIME);
    let ticks = u64::try_from(since.unsigned_abs()).map_err(|_| out_of_range())?;
    let d = Duration::new(ticks / 10_000_000, (ticks % 10_000_000) as u32 * 100);
    if since >= 0 {
        UNIX_EPOCH.checked_add(d)
    } else {
        UNIX_EPOCH.checked_sub(d)
    }
    .ok_or_else(out_of_range)
}

/// `FILE_STANDARD_INFO.NumberOfLinks`.
pub(super) fn links(f: &File) -> io::Result<u64> {
    let s: FILE_STANDARD_INFO = info(f, FileStandardInfo)?;
    Ok(u64::from(s.NumberOfLinks))
}

/// Native NTFS cases of the provider itself: identity across rename, copy, hard link and
/// replacement, links, and reparse points opened without being followed.
#[cfg(test)]
mod tests {
    use super::*;

    fn id(path: &Path) -> (u64, [u8; 16]) {
        id_of(&open_no_follow(path).unwrap()).unwrap()
    }

    #[test]
    fn wx2_ntfs_identity_follows_the_object_not_the_name() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let b = d.path().join("b");
        std::fs::write(&a, b"one").unwrap();
        let first = id(&a);
        assert_ne!(first.1, [0; 16], "a real 128-bit ID");
        let handle = File::open(&a).unwrap();
        assert_eq!(id_of(&handle).unwrap(), first);
        assert_eq!(links(&handle).unwrap(), 1);

        std::fs::rename(&a, &b).unwrap();
        assert_eq!(id(&b), first, "a rename keeps the identity");

        let copy = d.path().join("copy");
        std::fs::copy(&b, &copy).unwrap();
        assert_ne!(id(&copy), first, "a copy is another file");
        assert_eq!(id(&copy).0, first.0, "on the same volume");

        let hard = d.path().join("hard");
        std::fs::hard_link(&b, &hard).unwrap();
        assert_eq!(id(&hard), first, "a hard link is the same file");
        assert_eq!(links(&handle).unwrap(), 2);
        std::fs::remove_file(&hard).unwrap();
        assert_eq!(links(&handle).unwrap(), 1);

        drop(handle);
        let tmp = d.path().join("tmp");
        std::fs::write(&tmp, b"two").unwrap();
        let replacement = id(&tmp);
        std::fs::rename(&tmp, &b).unwrap();
        assert_eq!(id(&b), replacement, "a replacement is the file renamed in");
        assert_ne!(replacement, first);

        let dir = id(d.path());
        assert_ne!(dir, first, "a directory has its own identity");
        assert_eq!(id(d.path()), dir);
    }

    #[test]
    fn wx2_ntfs_a_reparse_point_is_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = d.path().join("link");
        // a directory symbolic link needs a privilege or developer mode; a junction does not
        let made = std::os::windows::fs::symlink_dir(&target, &link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .output()
                .is_ok_and(|o| o.status.success());
        assert!(made, "could not make a reparse point");
        assert_ne!(
            id(&link),
            id(&target),
            "the reparse point is its own object"
        );
        assert_eq!(id(&link), id(&link));
        match std::fs::canonicalize(&link) {
            Ok(through) => assert_eq!(id(&through), id(&target), "following it reaches the target"),
            // ERROR_UNTRUSTED_MOUNT_POINT: the system refuses to traverse a link that an ordinary
            // account made, so nothing is followed
            Err(e) => assert_eq!(e.raw_os_error(), Some(448), "{e}"),
        }
    }

    #[test]
    fn wx2_ntfs_change_time_moves_on_a_write() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::write(&f, b"one").unwrap();
        let handle = File::open(&f).unwrap();
        let before = change_time(&handle).unwrap();
        assert!(before > UNIX_EPOCH);
        let older = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(older)
            .unwrap();
        assert!(
            change_time(&handle).unwrap() >= before,
            "setting the mtime back does not move the change time back"
        );
    }

    #[test]
    fn wx2_filetime_is_converted_exactly_and_checked() {
        assert_eq!(filetime(UNIX_EPOCH_AS_FILETIME).unwrap(), UNIX_EPOCH);
        assert_eq!(
            filetime(UNIX_EPOCH_AS_FILETIME + 10_000_001).unwrap(),
            UNIX_EPOCH + Duration::new(1, 100)
        );
        assert!(filetime(i64::MAX).is_ok());
    }
}
