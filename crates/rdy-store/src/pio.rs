//! The store's platform calls (FW-XP1 D-1RXEA), one seam for the Unix-only file calls that store
//! and engine code make:
//! - positional reads and writes: at an offset, without moving the file's cursor;
//! - no-follow opens (`open_no_follow`): a file that is a symbolic link is refused, never
//!   followed;
//! - privacy: this process's uid (`own_uid`), a file's owner (`owner`), private files of mode
//!   0o600 (`private_file`, `make_private_file`, `is_private_file`), private directories of
//!   mode 0o700 (`create_private_dir`, `make_private_dir`, `is_private_dir`), the mode
//!   tests `is_shared_writable`, `is_world_writable_unsticky` and `is_owner_only`, and the umask
//!   check `umask_keeps_0700_at`;
//! - calls in an open directory (`open_dir`, `open_dir_at`, `open_read_at`,
//!   `create_private_at`, `create_private_dir_at`, `umask_keeps_0700_at`, `remove_file_at`,
//!   `remove_dir_at`, `set_mode_at`, `rename_at`, `is_link_refusal`): each names one entry of a
//!   directory handle, so a path that is changed after the handle was opened cannot redirect
//!   it, and none follows a link at that entry; `is_empty_dir` lists a directory handle itself;
//! - a child process that starts in an open directory (`start_in_dir`), for the same reason;
//! - test fixtures: a file's mode bits and inode (`mode_of`, `inode`), any mode
//!   (`permissions`, `set_mode`, `dir_mode`, `file_mode`), symbolic links (`symlink`) and a
//!   child process under another umask (`under_umask`), so the store's and the engine's tests
//!   need no Unix-only call of their own.
//!
//! Every other store module, and the engine, calls these functions. The daemon's `sys` seam
//! re-exports the no-follow and privacy calls under the same names.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

/// Fills `buf` from `f` at `offset`. A file that ends first is `ErrorKind::UnexpectedEof`.
#[cfg(unix)]
pub fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    f.read_exact_at(buf, offset)
}

/// Writes all of `buf` to `f` at `offset`. A write past the end extends the file.
#[cfg(unix)]
pub fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    f.write_all_at(buf, offset)
}

/// This process's effective uid.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Opens `path` with `opts` and `O_NOFOLLOW`: a file that is a symlink is refused, never
/// followed, so a checkout cannot point `.rdy/run/<file>` or a config file at a file elsewhere for
/// rdy to write, chmod or read.
#[cfg(unix)]
pub fn open_no_follow(opts: &mut OpenOptions, path: &Path) -> io::Result<File> {
    match opts.custom_flags(libc::O_NOFOLLOW).open(path) {
        Ok(f) => Ok(f),
        Err(e)
            if e.raw_os_error() == Some(libc::ELOOP)
                && fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) =>
        {
            Err(io::Error::other(format!(
                "{} is a symbolic link, which rdy does not follow",
                path.display()
            )))
        }
        Err(e) => Err(e),
    }
}

/// Makes a file that `opts` creates private to this user (mode 0o600).
#[cfg(unix)]
pub fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
    opts.mode(0o600)
}

/// Sets the open file `f` to mode 0o600: a file left by an older run keeps its mode through
/// open.
#[cfg(unix)]
pub fn make_private_file(f: &File) -> io::Result<()> {
    f.set_permissions(fs::Permissions::from_mode(0o600))
}

/// Creates the directory `path` with mode 0o700; an existing `path` is an error, as with
/// `DirBuilder::create`.
#[cfg(unix)]
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// Sets the directory `path` to mode 0o700.
#[cfg(unix)]
pub fn make_private_dir(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Whether `meta`'s mode is exactly 0o700.
#[cfg(unix)]
pub fn is_private_dir(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o777 == 0o700
}

/// Whether `meta` is a regular file that this user owns, with mode exactly 0o600.
#[cfg(unix)]
pub fn is_private_file(meta: &Metadata) -> bool {
    meta.is_file() && owner(meta) == own_uid() && meta.permissions().mode() & 0o777 == 0o600
}

/// The uid that owns the file `meta` describes.
#[cfg(unix)]
pub fn owner(meta: &Metadata) -> u32 {
    meta.uid()
}

/// Whether group or others may write the file or directory `meta` describes (mode bit 0o020
/// or 0o002). In such a directory another user can remove or replace an entry.
#[cfg(unix)]
pub fn is_shared_writable(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o022 != 0
}

/// Whether others (all users, not only the group) may write the directory `meta` describes and
/// its sticky bit is unset (mode bit 0o002 without 0o1000). In such a directory any user can
/// remove or replace an entry; the sticky bit limits that to the entry's owner.
#[cfg(unix)]
pub fn is_world_writable_unsticky(meta: &Metadata) -> bool {
    let mode = meta.permissions().mode();
    mode & 0o002 != 0 && mode & 0o1000 == 0
}

/// Whether group and others have no permission bit on `meta` (mode & 0o077 is 0).
#[cfg(unix)]
pub fn is_owner_only(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o077 == 0
}

/// Opens the directory `path` for the `*_at` calls below. Links in `path` are followed: the
/// caller trusts this path. Anything that is not a directory is refused (`ENOTDIR`).
#[cfg(unix)]
pub fn open_dir(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
}

/// Opens the directory `name` in the open directory `dir`, never through a link: a link or a
/// non-directory at `name` fails with an error that [`is_link_refusal`] accepts.
#[cfg(unix)]
pub fn open_dir_at(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(
        dir,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
}

/// Opens `name` in the open directory `dir` for reading, never through a link and without
/// waiting on a FIFO. The caller judges the kind of file from the handle's metadata. The
/// handle blocks as usual for later calls, such as `File::lock`.
#[cfg(unix)]
pub fn open_read_at(dir: &File, name: &OsStr) -> io::Result<File> {
    let f = open_at(
        dir,
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )?;
    set_blocking(&f)?;
    Ok(f)
}

/// Creates the new file `name` in the open directory `dir`, for reading and writing, with mode
/// 0o600 less the umask (set it exactly with [`make_private_file`]). Anything at `name`,
/// a link included, fails with `ErrorKind::AlreadyExists` and is never followed.
#[cfg(unix)]
pub fn create_private_at(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(
        dir,
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
}

/// Creates the directory `name` in the open directory `dir`, with mode 0o700 less the umask
/// ([`umask_keeps_0700_at`] tells beforehand whether that is 0o700). An existing `name` is
/// `ErrorKind::AlreadyExists`. mkdir returns no handle: nothing proves that the directory a
/// later open finds at `name` is the one made here.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn create_private_dir_at(dir: &File, name: &OsStr) -> io::Result<()> {
    let name = entry_name(name)?;
    // SAFETY: `dir` is an open descriptor for the whole call and `name` is a NUL-terminated
    // string that lives until the call returns.
    cvt(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) }).map(|_| ())
}

/// Whether this process's umask keeps mode 0o700 whole, so that [`create_private_dir_at`] makes
/// a 0o700 directory: it creates the new file `name` in the open directory `dir` with mode 0o700
/// (anything at `name`, a link included, fails with `ErrorKind::AlreadyExists` and is never
/// followed), reads the mode the umask left from the file's handle, and removes the file again.
#[cfg(unix)]
pub fn umask_keeps_0700_at(dir: &File, name: &OsStr) -> io::Result<bool> {
    let f = open_at(
        dir,
        name,
        libc::O_RDONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o700,
    )?;
    let mode = f.metadata().map(|m| m.permissions().mode() & 0o777);
    drop(f);
    let removed = remove_file_at(dir, name);
    let mode = mode?;
    removed?;
    Ok(mode == 0o700)
}

/// Removes the entry `name` (not a directory) from the open directory `dir`. A link is
/// removed, never followed.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn remove_file_at(dir: &File, name: &OsStr) -> io::Result<()> {
    let name = entry_name(name)?;
    // SAFETY: as in `create_private_dir_at`: an open descriptor and a live C string.
    cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) }).map(|_| ())
}

/// Removes the empty directory `name` from the open directory `dir`. A link at `name` is
/// refused (`ENOTDIR`), never followed; a directory that is not empty stays (`ENOTEMPTY`).
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn remove_dir_at(dir: &File, name: &OsStr) -> io::Result<()> {
    let name = entry_name(name)?;
    // SAFETY: as in `create_private_dir_at`: an open descriptor and a live C string.
    cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }).map(|_| ())
}

/// Sets the permission bits of the entry `name` in the open directory `dir` to `mode` (at most
/// 0o7777), never through a link at `name`: for an entry that cannot be opened to set its mode
/// through a handle, such as a directory the umask left without its owner's bits.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn set_mode_at(dir: &File, name: &OsStr, mode: u32) -> io::Result<()> {
    if mode > 0o7777 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a permission mode",
        ));
    }
    let name = entry_name(name)?;
    // the check above keeps `mode` within `mode_t` (16 bits on macOS)
    #[allow(clippy::unnecessary_cast)]
    let mode = mode as libc::mode_t;
    // SAFETY: as in `create_private_dir_at`: an open descriptor and a live C string.
    cvt(unsafe {
        libc::fchmodat(
            dir.as_raw_fd(),
            name.as_ptr(),
            mode,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })
    .map(|_| ())
}

/// Makes the child that `cmd` spawns start in the open directory `dir`: the child changes into
/// it (`fchdir`) before its exec, so a path to `dir` that changes after `dir` was opened cannot
/// move the child elsewhere. A relative program path (`./name`) is found in `dir`. `cmd` keeps
/// its own copy of the handle, so the caller may close `dir`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn start_in_dir(cmd: &mut std::process::Command, dir: &File) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let dir = dir.try_clone()?;
    // SAFETY: the closure runs in the child between fork and exec. It calls only `fchdir`,
    // which is async-signal-safe, on a descriptor the closure owns, and allocates nothing.
    unsafe {
        cmd.pre_exec(move || cvt(libc::fchdir(dir.as_raw_fd())).map(|_| ()));
    }
    Ok(())
}

/// Renames the entry `from` over the entry `to`, both in the open directory `dir`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn rename_at(dir: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
    let (from, to) = (entry_name(from)?, entry_name(to)?);
    let fd = dir.as_raw_fd();
    // SAFETY: as in `create_private_dir_at`: an open descriptor and two live C strings.
    cvt(unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) }).map(|_| ())
}

/// Whether the open directory `dir` holds no entry but `.` and `..`. It is listed in this
/// process through a new open of `dir` itself (`openat(dir, ".")`), so no path is resolved
/// again and the listing has its own position. A failed listing is an error, never "empty".
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn is_empty_dir(dir: &File) -> io::Result<bool> {
    use std::os::unix::io::IntoRawFd;
    // SAFETY: an open descriptor and a static C string; without O_CREAT no mode is read.
    let fd = cvt(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: `fd` is a new descriptor that nothing else owns.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: `fd` is open; on success the stream owns it, and `closedir` closes it.
    let stream = unsafe { libc::fdopendir(owned.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    let _ = owned.into_raw_fd();
    let empty = loop {
        // SAFETY: a `dirent` is plain data; all zeros is a valid value for `readdir_r` to fill.
        let mut entry: libc::dirent = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::dirent = std::ptr::null_mut();
        // SAFETY: an open stream, a whole `dirent` and a valid out pointer. `readdir_r` returns
        // its error, so an end of the listing is never confused with a failure.
        let r = unsafe { libc::readdir_r(stream, &mut entry, &mut found) };
        if r != 0 {
            break Err(io::Error::from_raw_os_error(r));
        }
        if found.is_null() {
            break Ok(true);
        }
        // SAFETY: `found` points at `entry`, whose name `readdir_r` ended with a NUL.
        let name = unsafe { std::ffi::CStr::from_ptr((*found).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            break Ok(false);
        }
    };
    // SAFETY: the stream is open and is closed once, here.
    unsafe { libc::closedir(stream) };
    empty
}

/// Whether `e`, from [`open_dir_at`] or [`open_read_at`], means that a link stands at the name,
/// or something that is not a directory where a directory was opened (`ELOOP`, `ENOTDIR`).
#[cfg(unix)]
pub fn is_link_refusal(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
}

/// One entry name as a C string: not empty, not `.` or `..`, with no `/` and no NUL.
#[cfg(unix)]
fn entry_name(name: &OsStr) -> io::Result<std::ffi::CString> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not one directory entry name",
        ));
    }
    std::ffi::CString::new(b).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// `openat(dir, name, flags | O_CLOEXEC, mode)`, as a `File`.
#[cfg(unix)]
#[allow(unsafe_code)]
fn open_at(dir: &File, name: &OsStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<File> {
    let name = entry_name(name)?;
    // SAFETY: as in `create_private_dir_at`: an open descriptor and a live C string. The mode
    // is passed as the variadic argument `openat` reads only with O_CREAT.
    let fd = cvt(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            libc::c_uint::from(mode),
        )
    })?;
    // SAFETY: `fd` is a new descriptor that nothing else owns.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Clears `O_NONBLOCK` on `f`.
#[cfg(unix)]
#[allow(unsafe_code)]
fn set_blocking(f: &File) -> io::Result<()> {
    let fd = f.as_raw_fd();
    // SAFETY: `fd` is open for the whole call; F_GETFL and F_SETFL take no pointer.
    let flags = cvt(unsafe { libc::fcntl(fd, libc::F_GETFL) })?;
    // SAFETY: as above.
    cvt(unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) }).map(|_| ())
}

/// A libc return value: -1 is the thread's `errno`.
#[cfg(unix)]
fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// The permission bits of the file `meta` describes, with the set-id and sticky bits
/// (mode & 0o7777). For test fixtures.
#[cfg(unix)]
pub fn mode_of(meta: &Metadata) -> u32 {
    meta.permissions().mode() & 0o7777
}

/// Permissions of exactly `mode` (for example 0o644), for `fs::set_permissions` or a
/// `tempfile::Builder`. For test fixtures.
#[cfg(unix)]
pub fn permissions(mode: u32) -> fs::Permissions {
    fs::Permissions::from_mode(mode)
}

/// Sets the file or directory `path` to `mode`, following a link at `path`. For test fixtures.
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, permissions(mode))
}

/// Makes the directories that `builder` creates `mode` less the umask. For test fixtures.
#[cfg(unix)]
pub fn dir_mode(builder: &mut fs::DirBuilder, mode: u32) -> &mut fs::DirBuilder {
    builder.mode(mode)
}

/// Makes a file that `opts` creates `mode` less the umask. For test fixtures.
#[cfg(unix)]
pub fn file_mode(opts: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    opts.mode(mode)
}

/// The inode number of the file `meta` describes: equal numbers on one device are one file.
/// For test fixtures.
#[cfg(unix)]
pub fn inode(meta: &Metadata) -> u64 {
    meta.ino()
}

/// Makes `link` a symbolic link to `target`. For test fixtures.
#[cfg(unix)]
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// A command that runs `program` under the process umask `umask` (octal text, for example
/// "002"); the caller adds the program's arguments. The umask is process-wide, so a test that
/// needs another one runs a child under it. For test fixtures.
#[cfg(unix)]
pub fn under_umask(umask: &str, program: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", r#"umask "$1" && shift && exec "$@""#, "sh", umask])
        .arg(program);
    cmd
}

#[cfg(not(unix))]
compile_error!("rdy_store::pio has no implementation for this platform yet (FW-XP1 Phase 4)");

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    #[test]
    fn ps1_a_write_at_an_offset_reads_back_at_that_offset() {
        let f = tempfile::tempfile().unwrap();
        write_all_at(&f, b"0123456789", 0).unwrap();
        write_all_at(&f, b"abc", 4).unwrap();
        let mut buf = [0u8; 3];
        read_exact_at(&f, &mut buf, 4).unwrap();
        assert_eq!(&buf, b"abc");
        let mut all = [0u8; 10];
        read_exact_at(&f, &mut all, 0).unwrap();
        assert_eq!(&all, b"0123abc789");
    }

    #[test]
    fn ps1_a_read_past_the_end_is_unexpected_eof() {
        let f = tempfile::tempfile().unwrap();
        write_all_at(&f, b"short", 0).unwrap();
        let mut buf = [0u8; 4];
        let e = read_exact_at(&f, &mut buf, 3).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
        let e = read_exact_at(&f, &mut buf, 100).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn ps1_positional_io_leaves_the_file_cursor_alone() {
        let mut f = tempfile::tempfile().unwrap();
        write_all_at(&f, b"first", 0).unwrap();
        write_all_at(&f, b"later", 100).unwrap();
        let mut back = [0u8; 5];
        read_exact_at(&f, &mut back, 100).unwrap();
        assert_eq!(&back, b"later");
        assert_eq!(f.stream_position().unwrap(), 0);
        let mut buf = [0u8; 5];
        f.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"first");
        f.seek(SeekFrom::Start(0)).unwrap();
        let mut one = [0u8; 1];
        f.read_exact(&mut one).unwrap();
        assert_eq!(one[0], b'f');
    }

    #[test]
    fn pre3_is_empty_dir_lists_the_directory_handle() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        fs::create_dir(&a).unwrap();
        let dir = open_dir(&a).unwrap();
        assert!(is_empty_dir(&dir).unwrap(), "a new directory");
        assert!(is_empty_dir(&dir).unwrap(), "the same answer again");
        // the handle is listed, not the path: moved, with a full directory at its old path
        let b = root.path().join("b");
        fs::rename(&a, &b).unwrap();
        fs::create_dir(&a).unwrap();
        fs::write(a.join("theirs"), b"x").unwrap();
        assert!(is_empty_dir(&dir).unwrap(), "moved, still empty");
        fs::write(b.join(".hidden"), b"x").unwrap();
        assert!(!is_empty_dir(&dir).unwrap(), "a hidden file");
        fs::remove_file(b.join(".hidden")).unwrap();
        fs::create_dir(b.join("sub")).unwrap();
        assert!(!is_empty_dir(&dir).unwrap(), "a directory");
        fs::remove_dir(b.join("sub")).unwrap();
        assert!(is_empty_dir(&dir).unwrap(), "empty again");
    }

    #[test]
    fn ps1_a_write_past_the_end_extends_the_file() {
        let f = tempfile::tempfile().unwrap();
        write_all_at(&f, b"ab", 0).unwrap();
        write_all_at(&f, b"xyz", 10).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 13);
        let mut buf = [0u8; 13];
        read_exact_at(&f, &mut buf, 0).unwrap();
        assert_eq!(&buf[..2], b"ab");
        assert_eq!(&buf[2..10], &[0u8; 8]);
        assert_eq!(&buf[10..], b"xyz");
    }

    #[test]
    fn pre1_pio_removes_dirs_sets_modes_and_starts_children_in_a_dir_handle() {
        let d = tempfile::tempdir().unwrap();
        let root = open_dir(d.path()).unwrap();
        // remove_dir_at: an empty directory goes; a full one, a file and a link stay.
        fs::create_dir(d.path().join("empty")).unwrap();
        remove_dir_at(&root, OsStr::new("empty")).unwrap();
        assert!(!d.path().join("empty").exists());
        fs::create_dir(d.path().join("full")).unwrap();
        fs::write(d.path().join("full/f"), b"x").unwrap();
        assert!(remove_dir_at(&root, OsStr::new("full")).is_err());
        assert!(d.path().join("full/f").exists());
        fs::write(d.path().join("file"), b"x").unwrap();
        assert!(remove_dir_at(&root, OsStr::new("file")).is_err());
        fs::create_dir(d.path().join("target")).unwrap();
        symlink(d.path().join("target"), d.path().join("link")).unwrap();
        assert!(remove_dir_at(&root, OsStr::new("link")).is_err());
        assert!(d.path().join("target").is_dir());
        assert!(remove_dir_at(&root, OsStr::new("a/b")).is_err());

        // set_mode_at: a directory without its owner's bits opens again; a link's target keeps
        // its mode.
        set_mode(&d.path().join("target"), 0o000).unwrap();
        assert!(open_dir_at(&root, OsStr::new("target")).is_err() || own_uid() == 0);
        set_mode_at(&root, OsStr::new("target"), 0o700).unwrap();
        assert_eq!(
            mode_of(&fs::metadata(d.path().join("target")).unwrap()),
            0o700
        );
        open_dir_at(&root, OsStr::new("target")).unwrap();
        let _ = set_mode_at(&root, OsStr::new("link"), 0o777);
        assert_eq!(
            mode_of(&fs::metadata(d.path().join("target")).unwrap()),
            0o700
        );
        assert!(set_mode_at(&root, OsStr::new("target"), 0o10000).is_err());

        // start_in_dir: the child starts in the directory the handle opened, even after that
        // directory was moved, and finds `./name` there.
        let here = open_dir_at(&root, OsStr::new("target")).unwrap();
        fs::rename(d.path().join("target"), d.path().join("moved")).unwrap();
        fs::create_dir(d.path().join("target")).unwrap();
        fs::write(d.path().join("moved/name"), b"#!/bin/sh\npwd -P\n").unwrap();
        set_mode(&d.path().join("moved/name"), 0o700).unwrap();
        let mut cmd = std::process::Command::new("./name");
        start_in_dir(&mut cmd, &here).unwrap();
        drop(here);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            fs::canonicalize(d.path().join("moved"))
                .unwrap()
                .to_string_lossy()
        );
    }

    #[test]
    fn ha1_pio_fixture_helpers_set_and_read_modes_links_and_umasks() {
        let d = tempfile::Builder::new()
            .permissions(permissions(0o700))
            .tempdir()
            .unwrap();
        assert_eq!(mode_of(&fs::metadata(d.path()).unwrap()), 0o700);
        let f = d.path().join("f");
        file_mode(OpenOptions::new().write(true).create_new(true), 0o600)
            .open(&f)
            .unwrap();
        assert_eq!(mode_of(&fs::metadata(&f).unwrap()), 0o600);
        set_mode(&f, 0o644).unwrap();
        assert_eq!(mode_of(&fs::metadata(&f).unwrap()), 0o644);
        let sub = d.path().join("sub");
        dir_mode(&mut fs::DirBuilder::new(), 0o700)
            .create(&sub)
            .unwrap();
        assert_eq!(mode_of(&fs::metadata(&sub).unwrap()), 0o700);
        set_mode(&sub, 0o1777).unwrap();
        assert_eq!(mode_of(&fs::metadata(&sub).unwrap()), 0o1777);
        let link = d.path().join("link");
        symlink(&f, &link).unwrap();
        let lm = fs::symlink_metadata(&link).unwrap();
        assert!(lm.file_type().is_symlink());
        assert_ne!(inode(&lm), inode(&fs::metadata(&f).unwrap()));
        assert_eq!(
            inode(&fs::metadata(&link).unwrap()),
            inode(&fs::metadata(&f).unwrap())
        );
        let out = under_umask("077", Path::new("/bin/sh"))
            .args(["-c", "umask"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0077");
    }
}
