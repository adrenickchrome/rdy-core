//! The store's platform calls (FW-XP1 D-1RXEA), one seam for the Unix-only file calls that store
//! and engine code make:
//! - positional reads and writes: at an offset, without moving the file's cursor;
//! - no-follow opens (`open_no_follow`): a file that is a symbolic link is refused, never
//!   followed;
//! - privacy: this process's uid (`own_uid`), a file's owner (`owner`), private files of mode
//!   0o600 (`private_file`, `make_private_file`, `is_private_file`), private directories of
//!   mode 0o700 (`create_private_dir`, `make_private_dir`, `is_private_dir`), the mode
//!   tests `is_shared_writable` and `is_owner_only`, and the umask check `umask_keeps_0700_at`;
//! - calls in an open directory (`open_dir`, `open_dir_at`, `open_read_at`,
//!   `create_private_at`, `create_private_dir_at`, `umask_keeps_0700_at`, `remove_file_at`,
//!   `rename_at`, `is_link_refusal`): each names one entry of a directory handle, so a path that
//!   is changed after the handle was opened cannot redirect it, and none follows a link at that
//!   entry.
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

/// Renames the entry `from` over the entry `to`, both in the open directory `dir`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn rename_at(dir: &File, from: &OsStr, to: &OsStr) -> io::Result<()> {
    let (from, to) = (entry_name(from)?, entry_name(to)?);
    let fd = dir.as_raw_fd();
    // SAFETY: as in `create_private_dir_at`: an open descriptor and two live C strings.
    cvt(unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) }).map(|_| ())
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
}
