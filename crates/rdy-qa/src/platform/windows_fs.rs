//! Filesystem fixtures on Windows: read-only directories, links, file identities and names that
//! only this platform has. They are for tests, and each one changes real access, so a test
//! observes the change by what the system allows, never by reading a descriptor back.
//!
//! - **A read-only directory has no write right for any entry.** Its DACL is protected and holds
//!   one entry: this account may read and list. The owner keeps the right to change the DACL,
//!   so the directory can be restored. The refusal is for a standard account: an elevated
//!   token was seen to write there, so the test asserts the refusal only when not elevated.
//! - **A junction needs no privilege, a symlink does.** A missing symlink privilege is an error
//!   the caller sees (`ERROR_PRIVILEGE_NOT_HELD`). It is a machine prerequisite, so a test that
//!   needs it fails under `--require-all` and never skips.
//! - **A fixture removes only what it made.** A junction is removed as the directory entry it
//!   is: its target is never entered.
//! - **Names are UTF-16 units.** No name here comes from Unix bytes.

use rdy_store::fileid::{FileId, links_of};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Networking::WinSock::{
    AF_UNIX, INVALID_SOCKET, SOCK_STREAM, SOCKADDR_UN, WSADATA, WSAGetLastError, WSAStartup, bind,
    closesocket, socket,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FindClose, FindFirstFileW, GetShortPathNameW, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY, UnmapViewOfFile,
};
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
};

/// `text` followed by one UTF-16 unit that is not Unicode: an unpaired surrogate. NTFS keeps
/// such a name as it is, and `to_str` refuses it.
pub fn not_unicode(prefix: &str) -> OsString {
    rdy_store::pio::not_unicode(prefix)
}

/// Names that Win32 refuses to create (`ERROR_INVALID_NAME`): each holds a character that a
/// file name cannot hold. A plain `a` is the only text around the character.
pub fn native_invalid_names() -> Vec<OsString> {
    ["a<b", "a>b", "a|b", "a?b", "a*b", "a\"b", "a\u{1}b"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// Pairs of a name Win32 accepts and the different name it stores: a trailing dot and a
/// trailing space are dropped.
pub fn normalized_names() -> Vec<(OsString, OsString)> {
    vec![
        (OsString::from("trail."), OsString::from("trail")),
        (OsString::from("space "), OsString::from("space")),
    ]
}

/// The name of a device, which makes no directory entry when it is created in a directory.
pub fn device_name() -> &'static str {
    "NUL"
}

/// A path as NUL-terminated UTF-16. A NUL inside it is an error.
pub(super) fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a path holds a NUL",
        ));
    }
    units.push(0);
    Ok(units)
}

/// Replaces the DACL of `path` with the one that `sddl` describes, protected from inheritance.
/// Only the DACL changes.
#[allow(unsafe_code)]
pub(super) fn set_dacl(path: &Path, sddl: &str) -> io::Result<()> {
    let name = wide(path)?;
    let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut sd = null_mut();
    // SAFETY: `text` is NUL-terminated; `sd` is a writable pointer slot that receives a
    // descriptor this function frees.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `name` is NUL-terminated and `sd` is a valid descriptor until it is freed.
    let done = unsafe {
        SetFileSecurityW(
            name.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: `sd` came from the conversion call and is freed once.
    unsafe { LocalFree(sd) };
    if done == 0 { Err(error) } else { Ok(()) }
}

/// The standard string of this account's SID.
pub(super) fn own_sid() -> io::Result<String> {
    Ok(rdy_store::pio::current_account()?.to_string())
}

/// FILE_GENERIC_READ and FILE_GENERIC_EXECUTE: list, read and traverse, nothing that writes.
const READ_AND_LIST: &str = "0x1200a9";

/// The protected DACL that `mode` gives its owner, in Windows terms. No owner bit (`0o000`) is an
/// empty DACL: no account may open the file, and the owner can still change the DACL. An owner
/// bit without the write bit (`0o400`, `0o500`, `0o555`) is read and list only, so nothing can be
/// written or created there, and the owner can still change the DACL. A write bit gives full
/// control. A directory's entry is inherited.
fn mode_sddl(mode: u32, is_dir: bool, sid: &str) -> String {
    let flags = if is_dir { "OICI" } else { "" };
    if mode & 0o700 == 0 {
        "D:P".to_string()
    } else if mode & 0o200 == 0 {
        format!("D:P(A;{flags};{READ_AND_LIST};;;{sid})")
    } else {
        format!("D:P(A;{flags};FA;;;{sid})")
    }
}

/// Sets the access of `path` to what `mode` says for its owner. See `mode_sddl`.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    if mode & 0o700 == 0 {
        return set_dacl(path, &mode_sddl(mode, false, ""));
    }
    set_dacl(path, &mode_sddl(mode, path.is_dir(), &own_sid()?))
}

/// A directory that a standard account cannot write in while this value lives. No entry gives
/// a write right; the owner can still change the DACL, and does when the value is dropped. Only the directory's own DACL changes, never what it holds.
pub struct ReadOnlyDir {
    path: PathBuf,
}

impl ReadOnlyDir {
    /// Makes the directory `path` read-only for every account.
    pub fn new(path: &Path) -> io::Result<ReadOnlyDir> {
        set_dacl(
            path,
            &format!("D:P(A;OICI;{READ_AND_LIST};;;{})", own_sid()?),
        )?;
        Ok(ReadOnlyDir {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        if let Ok(sid) = own_sid() {
            let _ = set_dacl(&self.path, &format!("D:P(A;OICI;FA;;;{sid})"));
        }
    }
}

/// The bytes of a file that a live store may hold, as a kill of its process would leave them. A
/// live index file holds a byte-range lock that refuses every read of another handle, and a
/// mapped view is the one way past it that needs no help from the holder.
#[allow(unsafe_code)]
pub fn read_live_file(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let len = usize::try_from(file.metadata()?.len())
        .map_err(|_| io::Error::other("the file does not fit in memory"))?;
    if len == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: the handle is open for reading, and no name or security attributes are given.
    let map =
        unsafe { CreateFileMappingW(file.as_raw_handle(), null(), PAGE_READONLY, 0, 0, null()) };
    if map.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `map` is a read-only mapping of the whole file, and the handle is closed once below.
    let map = unsafe { OwnedHandle::from_raw_handle(map) };
    // SAFETY: the same mapping. A length of 0 maps the whole file, which holds `len` bytes or more.
    let view = unsafe { MapViewOfFile(map.as_raw_handle(), FILE_MAP_READ, 0, 0, 0) };
    if view.Value.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the view is readable for `len` bytes, and it stays mapped until the call below.
    let bytes = unsafe { std::slice::from_raw_parts(view.Value.cast::<u8>(), len) }.to_vec();
    // SAFETY: `view` came from MapViewOfFile above and is unmapped once.
    unsafe { UnmapViewOfFile(view) };
    Ok(bytes)
}

/// What a link in the file system is, by its reparse tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparseKind {
    Symlink,
    Junction,
    Other(u32),
}

/// The reparse kind of the entry `path` itself, never of its target. `None` is an entry that is
/// not a reparse point.
#[allow(unsafe_code)]
pub fn reparse_kind(path: &Path) -> io::Result<Option<ReparseKind>> {
    let name = wide(path)?;
    // SAFETY: an all-zero find record is valid.
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is NUL-terminated and `data` is a writable find record.
    let handle = unsafe { FindFirstFileW(name.as_ptr(), &mut data) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` came from the find call above and is closed once.
    unsafe { FindClose(handle) };
    if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(None);
    }
    Ok(Some(match data.dwReserved0 {
        IO_REPARSE_TAG_SYMLINK => ReparseKind::Symlink,
        IO_REPARSE_TAG_MOUNT_POINT => ReparseKind::Junction,
        other => ReparseKind::Other(other),
    }))
}

/// Makes `link` a symbolic link to `target`: a directory link when `target` is a directory (a
/// relative `target` is read from `link`'s parent), else a file link. Without the privilege
/// this fails with `ERROR_PRIVILEGE_NOT_HELD`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Makes `link` a second name for the existing directory `target`: a junction here, which needs
/// no privilege. The other platform's counterpart is a symbolic link.
pub fn dir_alias(target: &Path, link: &Path) -> io::Result<()> {
    junction(target, link)
}

/// Makes `link` a junction to the existing directory `target`. A new empty directory takes the
/// reparse data, and it is removed again if that fails. No privilege is needed.
#[allow(unsafe_code)]
pub fn junction(target: &Path, link: &Path) -> io::Result<()> {
    let canonical = std::fs::canonicalize(target)?;
    let text = canonical.as_os_str().to_string_lossy().into_owned();
    let plain = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let substitute: Vec<u16> = format!(r"\??\{plain}").encode_utf16().collect();
    let print: Vec<u16> = plain.encode_utf16().collect();
    let path_bytes = (substitute.len() + 1 + print.len() + 1) * 2;
    // header (tag, data length, reserved), four offsets and lengths, then the two names
    let data_len = 8 + path_bytes;
    if data_len > 16 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the junction target is too long",
        ));
    }
    let mut buffer = Vec::with_capacity(8 + data_len);
    buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend((data_len as u16).to_le_bytes());
    buffer.extend(0u16.to_le_bytes());
    buffer.extend(0u16.to_le_bytes());
    buffer.extend(((substitute.len() * 2) as u16).to_le_bytes());
    buffer.extend((((substitute.len() + 1) * 2) as u16).to_le_bytes());
    buffer.extend(((print.len() * 2) as u16).to_le_bytes());
    for unit in substitute.iter().chain(&[0]).chain(&print).chain(&[0]) {
        buffer.extend(unit.to_le_bytes());
    }
    std::fs::create_dir(link)?;
    let set = || -> io::Result<()> {
        let dir = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(link)?;
        let mut returned = 0u32;
        // SAFETY: the handle is open for the call; `buffer` is readable for its length.
        let done = unsafe {
            DeviceIoControl(
                dir.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                null_mut(),
                0,
                &mut returned,
                null_mut(),
            )
        };
        if done == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    let result = set();
    if result.is_err() {
        let _ = std::fs::remove_dir(link);
    }
    result
}

/// Removes the junction or directory symlink `link` as an entry; its target is never entered.
pub fn remove_dir_link(link: &Path) -> io::Result<()> {
    std::fs::remove_dir(link)
}

/// Makes `path` a directory entry that is a reparse point and no link: a bound `AF_UNIX` socket.
/// Its tag names no other object, so the system reports a plain file, and any open of it is
/// refused (`ERROR_CANT_ACCESS_FILE`, 1920). It is the nearest thing here to a Unix FIFO: an entry
/// that holds no data. `path` is a plain path of at most 107 bytes.
#[allow(unsafe_code)]
pub fn socket_entry(path: &Path) -> io::Result<()> {
    let wsa_error = || {
        io::Error::from_raw_os_error(
            // SAFETY: this call takes nothing and reads this thread's last socket error.
            unsafe { WSAGetLastError() },
        )
    };
    // SAFETY: an all-zero record is valid for both; each field below is plain data.
    let (mut wsa, mut address): (WSADATA, SOCKADDR_UN) = unsafe { std::mem::zeroed() };
    let bytes = path.to_str().map(str::as_bytes).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "a socket path is not Unicode")
    })?;
    if bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a socket path of {} bytes is too long", bytes.len()),
        ));
    }
    address.sun_family = AF_UNIX;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as i8;
    }
    // SAFETY: `wsa` is a writable record. A repeated start is counted and needs no matching call
    // here: the process ends with the test.
    let started = unsafe { WSAStartup(0x0202, &mut wsa) };
    if started != 0 {
        return Err(io::Error::from_raw_os_error(started));
    }
    // SAFETY: the arguments are plain values.
    let handle = unsafe { socket(i32::from(AF_UNIX), SOCK_STREAM, 0) };
    if handle == INVALID_SOCKET {
        return Err(wsa_error());
    }
    // SAFETY: `address` is a valid record of the size given, alive for the call.
    let bound = unsafe {
        bind(
            handle,
            (&raw const address).cast(),
            std::mem::size_of::<SOCKADDR_UN>() as i32,
        )
    };
    let result = if bound == 0 { Ok(()) } else { Err(wsa_error()) };
    // SAFETY: `handle` came from the call above and is closed once. The entry stays.
    unsafe { closesocket(handle) };
    result
}

/// Makes `new` a link with the target of the link `link`, as written there. Whether it is a
/// directory link comes from the entry itself: following it can be refused (error 448), and the
/// target may not exist.
pub fn copy_link(link: &Path, new: &Path) -> io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    let target = std::fs::read_link(link)?;
    // a junction stays a junction: it needs no privilege, a symbolic link does
    if reparse_kind(link)? == Some(ReparseKind::Junction) {
        return junction(&target, new);
    }
    if std::fs::symlink_metadata(link)?
        .file_type()
        .is_symlink_dir()
    {
        std::os::windows::fs::symlink_dir(target, new)
    } else {
        std::os::windows::fs::symlink_file(target, new)
    }
}

/// Makes `new` a second name of the file `existing`.
pub fn hard_link(existing: &Path, new: &Path) -> io::Result<()> {
    std::fs::hard_link(existing, new)
}

/// How many names the file `path` has, read from an open handle.
pub fn link_count(path: &Path) -> io::Result<u64> {
    links_of(&File::open(path)?, path)
}

/// The identity of the entry `path` itself (a link is not followed).
pub fn file_id(path: &Path) -> io::Result<FileId> {
    FileId::lstat(path)
}

/// The 8.3 alias of the existing `path`. The alias is the same file under a second name. A
/// volume without short names gives back `path` unchanged, which the caller can see.
#[allow(unsafe_code)]
pub fn short_alias(path: &Path) -> io::Result<PathBuf> {
    let name = wide(path)?;
    // SAFETY: `name` is NUL-terminated; a null buffer of length 0 asks for the needed length.
    let needed = unsafe { GetShortPathNameW(name.as_ptr(), null_mut(), 0) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = vec![0u16; needed as usize];
    // SAFETY: `out` holds `needed` units.
    let written = unsafe { GetShortPathNameW(name.as_ptr(), out.as_mut_ptr(), needed) };
    if written == 0 || written >= needed {
        return Err(io::Error::last_os_error());
    }
    out.truncate(written as usize);
    Ok(PathBuf::from(OsString::from_wide(&out)))
}

/// The names in the directory `dir`, as the system lists them.
pub fn entry_names(dir: &Path) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        names.push(entry?.file_name());
    }
    names.sort();
    Ok(names)
}

/// True when `name` holds a unit that is not Unicode.
pub fn is_not_unicode(name: &OsStr) -> bool {
    name.to_str().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn wx24_read_only_fixture_refuses_a_normal_user_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("ro");
        fs::create_dir(&dir).expect("mkdir");
        fs::write(dir.join("before"), b"1").expect("a write works before the fixture");
        {
            let _ro = ReadOnlyDir::new(&dir).expect("read-only");
            // The fixture is for a normal user. An elevated token is judged by the system
            // differently, so the refusal is asserted for a standard account only.
            if !super::super::windows_accounts::is_elevated().expect("elevation") {
                let err = fs::write(dir.join("after"), b"2").expect_err("the write is refused");
                assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
                assert_eq!(err.raw_os_error(), Some(5));
                assert!(!dir.join("after").exists());
                fs::create_dir(dir.join("sub")).expect_err("a new directory is refused");
            }
            // what it holds can still be read and listed
            assert_eq!(fs::read(dir.join("before")).expect("read"), b"1");
            assert_eq!(entry_names(&dir).expect("list"), [OsString::from("before")]);
        }
        fs::write(dir.join("after"), b"2").expect("a write works again");
    }

    #[test]
    fn wx24_reparse_fixture_is_detected_as_a_reparse_point() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let real = root.join("real");
        fs::create_dir(&real).expect("mkdir");
        fs::write(real.join("f"), b"data").expect("write");

        let junction_path = root.join("junction");
        junction(&real, &junction_path).expect("a junction needs no privilege");
        let dir_link = root.join("dirlink");
        symlink(&real, &dir_link).expect("the symlink privilege is a machine prerequisite");
        let file_link = root.join("filelink");
        symlink(&real.join("f"), &file_link)
            .expect("the symlink privilege is a machine prerequisite");

        assert_eq!(reparse_kind(&real).expect("kind"), None);
        assert_eq!(reparse_kind(&real.join("f")).expect("kind"), None);
        assert_eq!(
            reparse_kind(&junction_path).expect("kind"),
            Some(ReparseKind::Junction)
        );
        assert_eq!(
            reparse_kind(&dir_link).expect("kind"),
            Some(ReparseKind::Symlink)
        );
        assert_eq!(
            reparse_kind(&file_link).expect("kind"),
            Some(ReparseKind::Symlink)
        );
        // the links reach the data, and each is an identity of its own
        // A link that a standard user made can be refused at traversal by the system's
        // redirection guard (`ERROR_UNTRUSTED_MOUNT_POINT`, 448), as on the Windows 11 build
        // box. Either answer is the system's, and the detection above does not depend on it.
        for through in [
            junction_path.join("f"),
            dir_link.join("f"),
            file_link.clone(),
        ] {
            match fs::read(&through) {
                Ok(bytes) => assert_eq!(bytes, b"data"),
                Err(e) => assert_eq!(e.raw_os_error(), Some(448), "{}: {e}", through.display()),
            }
        }
        // each link is an identity of its own
        let real_id = file_id(&real).expect("id");
        assert_ne!(file_id(&junction_path).expect("id"), real_id);
        assert_ne!(file_id(&dir_link).expect("id"), real_id);
        assert_ne!(
            file_id(&file_link).expect("id"),
            file_id(&real.join("f")).expect("id")
        );

        // a hard link is one file under two names, and no reparse point
        let twin = real.join("twin");
        assert_eq!(link_count(&real.join("f")).expect("count"), 1);
        hard_link(&real.join("f"), &twin).expect("link");
        assert_eq!(link_count(&real.join("f")).expect("count"), 2);
        assert_eq!(link_count(&twin).expect("count"), 2);
        assert_eq!(
            file_id(&twin).expect("id"),
            file_id(&real.join("f")).expect("id")
        );
        assert_eq!(reparse_kind(&twin).expect("kind"), None);

        // a short alias is the same file under another name
        let long = real.join("a-long-file-name-for-wx24.txt");
        fs::write(&long, b"x").expect("write");
        let alias = short_alias(&long).expect("alias");
        assert_ne!(alias, long, "8.3 names are on for this volume");
        assert_eq!(file_id(&alias).expect("id"), file_id(&long).expect("id"));

        // removing the links leaves the target alone
        remove_dir_link(&junction_path).expect("remove the junction");
        remove_dir_link(&dir_link).expect("remove the directory link");
        fs::remove_file(&file_link).expect("remove the file link");
        assert_eq!(fs::read(real.join("f")).expect("target kept"), b"data");
        assert!(reparse_kind(&junction_path).is_err());
    }

    #[test]
    fn wx24_native_filename_fixtures_do_not_use_unix_bytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();

        // a name that is not Unicode is an unpaired surrogate, kept as it is
        let odd = not_unicode("odd");
        assert!(is_not_unicode(&odd));
        let units: Vec<u16> = odd.encode_wide().collect();
        assert_eq!(units.last(), Some(&0xD800));
        assert!(units.iter().all(|&u| u != 0xFF && u != 0xE9));
        fs::write(dir.join(&odd), b"x").expect("NTFS keeps a lone surrogate");
        assert_eq!(entry_names(dir).expect("list"), std::slice::from_ref(&odd));
        fs::remove_file(dir.join(&odd)).expect("remove");

        // names the system refuses
        for name in native_invalid_names() {
            let err = File::create(dir.join(&name)).expect_err("refused");
            assert_eq!(err.raw_os_error(), Some(123), "{name:?}: {err}");
        }
        assert!(entry_names(dir).expect("list").is_empty());

        // names the system stores under another name
        for (given, stored) in normalized_names() {
            fs::write(dir.join(&given), b"x").expect("accepted");
            assert_eq!(
                entry_names(dir).expect("list"),
                std::slice::from_ref(&stored)
            );
            fs::remove_file(dir.join(&stored)).expect("remove");
        }

        // a device name makes no entry
        let _ = fs::write(dir.join(device_name()), b"x");
        assert!(entry_names(dir).expect("list").is_empty());
    }
}

#[cfg(test)]
mod set_mode_tests {
    use super::*;

    #[test]
    fn read_only_owner_modes_give_no_write_right() {
        for mode in [0o400, 0o500, 0o555] {
            let sddl = mode_sddl(mode, true, "S-1-5-X");
            assert!(!sddl.contains(";FA;"), "{mode:o}: {sddl}");
            assert!(sddl.contains(READ_AND_LIST), "{mode:o}: {sddl}");
        }
        assert!(mode_sddl(0o700, true, "S").contains(";FA;"));
        assert!(mode_sddl(0o600, false, "S").contains(";FA;"));
        assert_eq!(mode_sddl(0o000, true, "S"), "D:P");
    }

    #[test]
    fn a_read_only_mode_refuses_a_write_unless_elevated() {
        let dir = tempfile::tempdir().expect("tempdir");
        set_mode(dir.path(), 0o500).expect("chmod");
        let wrote = std::fs::write(dir.path().join("x"), b"x").is_ok();
        set_mode(dir.path(), 0o700).expect("restore");
        if !super::super::windows_accounts::is_elevated().unwrap_or(false) {
            assert!(!wrote, "0o500 must refuse a write");
        }
    }
}
