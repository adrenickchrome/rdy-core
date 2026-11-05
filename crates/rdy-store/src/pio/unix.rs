//! The Unix side of [`crate::pio`] (FW-XP1 D-1RXEA): the calls that module documents, made
//! with libc and `std::os::unix`.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use super::{EntryKind, Privacy, Punch, bad_punch_range};

/// Fills `buf` from `f` at `offset`. A file that ends first is `ErrorKind::UnexpectedEof`.
pub fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    f.read_exact_at(buf, offset)
}

/// Writes all of `buf` to `f` at `offset`. A write past the end extends the file.
pub fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    f.write_all_at(buf, offset)
}

/// This process's effective uid.
#[allow(unsafe_code)]
pub fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Opens `path` with `opts` and `O_NOFOLLOW`: a file that is a symlink is refused, never
/// followed, so a checkout cannot point `.rdy/run/<file>` or a config file at a file elsewhere for
/// rdy to write, chmod or read.
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
pub fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
    opts.mode(0o600)
}

/// Sets the open file `f` to mode 0o600: a file left by an older run keeps its mode through
/// open.
pub fn make_private_file(f: &File) -> io::Result<()> {
    f.set_permissions(fs::Permissions::from_mode(0o600))
}

/// Creates the directory `path` with mode 0o700; an existing `path` is an error, as with
/// `DirBuilder::create`.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// Sets the directory `path` to mode 0o700.
pub fn make_private_dir(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Whether `meta`'s mode is exactly 0o700.
pub fn is_private_dir(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o777 == 0o700
}

/// Whether `meta` is a regular file that this user owns, with mode exactly 0o600.
pub fn is_private_file(meta: &Metadata) -> bool {
    meta.is_file() && owner(meta) == own_uid() && meta.permissions().mode() & 0o777 == 0o600
}

/// The uid that owns the file `meta` describes.
pub fn owner(meta: &Metadata) -> u32 {
    meta.uid()
}

/// Whether group or others may write the file or directory `meta` describes (mode bit 0o020
/// or 0o002). In such a directory another user can remove or replace an entry.
pub fn is_shared_writable(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o022 != 0
}

/// Whether others (all users, not only the group) may write the directory `meta` describes and
/// its sticky bit is unset (mode bit 0o002 without 0o1000). In such a directory any user can
/// remove or replace an entry; the sticky bit limits that to the entry's owner.
pub fn is_world_writable_unsticky(meta: &Metadata) -> bool {
    let mode = meta.permissions().mode();
    mode & 0o002 != 0 && mode & 0o1000 == 0
}

/// Whether group and others have no permission bit on `meta` (mode & 0o077 is 0).
pub fn is_owner_only(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o077 == 0
}

/// Who may use the open file or directory `f`: one `fstat` of its handle, read as
/// [`Privacy`] describes. A failed `fstat` is an error.
pub fn privacy(f: &File) -> io::Result<Privacy> {
    let meta = f.metadata()?;
    let mode = meta.permissions().mode();
    let private = if meta.is_file() {
        mode & 0o777 == 0o600
    } else if meta.is_dir() {
        mode & 0o777 == 0o700
    } else {
        false
    };
    Ok(Privacy {
        mine: owner(&meta) == own_uid(),
        private,
        owner_only: is_owner_only(&meta),
        shared_writable: is_shared_writable(&meta),
        open_to_all: is_world_writable_unsticky(&meta),
    })
}

/// Flushes the directory `path`, so entries created, renamed or removed in it survive a power
/// loss: it opens the directory and calls [`sync_open_dir`].
pub fn sync_dir(path: &Path) -> io::Result<()> {
    let dir = File::open(path).map_err(|e| sync_failed(path, e))?;
    sync_open_dir(&dir, path)
}

/// Flushes the open directory `dir` (at `path`, which names it in an error). This uses
/// `File::sync_all`, never `sync_data`: std implements `sync_all` on Apple targets as
/// `fcntl(F_FULLFSYNC)` (a drive-cache flush, also on a directory descriptor), and on Linux as
/// `fsync`. A failed flush is an error, `could not sync directory {path}: {error}`, of the same
/// kind as the failed call's.
pub fn sync_open_dir(dir: &File, path: &Path) -> io::Result<()> {
    dir.sync_all().map_err(|e| sync_failed(path, e))
}

fn sync_failed(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(
        e.kind(),
        format!("could not sync directory {}: {e}", path.display()),
    )
}

/// Renames `from` to `to`, replacing a file at `to`, as one atomic step (`rename(2)`). The entry
/// survives a power loss once the directory that holds it is flushed ([`sync_dir`]). A failed
/// rename is an error, `could not rename {from} to {to}: {error}`, of the same kind as the
/// failed call's; nothing is ever copied instead.
pub fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to).map_err(|e| {
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

/// Opens the directory `path` for the `*_at` calls below. Links in `path` are followed: the
/// caller trusts this path. Anything that is not a directory is refused (`ENOTDIR`).
pub fn open_dir(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
}

/// Opens the directory `name` in the open directory `dir`, never through a link: a link or a
/// non-directory at `name` fails with an error that [`is_link_refusal`] accepts.
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
#[allow(unsafe_code)]
pub fn remove_file_at(dir: &File, name: &OsStr) -> io::Result<()> {
    let name = entry_name(name)?;
    // SAFETY: as in `create_private_dir_at`: an open descriptor and a live C string.
    cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) }).map(|_| ())
}

/// Removes the empty directory `name` from the open directory `dir`. A link at `name` is
/// refused (`ENOTDIR`), never followed; a directory that is not empty stays (`ENOTEMPTY`).
#[allow(unsafe_code)]
pub fn remove_dir_at(dir: &File, name: &OsStr) -> io::Result<()> {
    let name = entry_name(name)?;
    // SAFETY: as in `create_private_dir_at`: an open descriptor and a live C string.
    cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }).map(|_| ())
}

/// Sets the permission bits of the entry `name` in the open directory `dir` to `mode` (at most
/// 0o7777), never through a link at `name`: for an entry that cannot be opened to set its mode
/// through a handle, such as a directory the umask left without its owner's bits.
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

/// Calls `f` with the name of each entry of the open directory `dir` but `.` and `..`, in the
/// order the directory lists them. The listing goes through a new open of `dir` itself, as in
/// [`is_empty_dir`], so no path is resolved again. A failed listing is an error, and so is the
/// first error of `f`, which ends the listing.
#[allow(unsafe_code)]
pub fn for_each_entry<E: From<io::Error>>(
    dir: &File,
    mut f: impl FnMut(&OsStr) -> Result<(), E>,
) -> Result<(), E> {
    use std::os::unix::io::IntoRawFd;
    // SAFETY: as in `is_empty_dir`.
    let fd = cvt(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: `fd` is a new descriptor that nothing else owns.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: as in `is_empty_dir`.
    let stream = unsafe { libc::fdopendir(owned.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let _ = owned.into_raw_fd();
    let listed = loop {
        // SAFETY: as in `is_empty_dir`.
        let mut entry: libc::dirent = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::dirent = std::ptr::null_mut();
        // SAFETY: as in `is_empty_dir`.
        let r = unsafe { libc::readdir_r(stream, &mut entry, &mut found) };
        if r != 0 {
            break Err(io::Error::from_raw_os_error(r).into());
        }
        if found.is_null() {
            break Ok(());
        }
        // SAFETY: as in `is_empty_dir`.
        let name = unsafe { std::ffi::CStr::from_ptr((*found).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if let Err(e) = f(OsStr::from_bytes(name)) {
            break Err(e);
        }
    };
    // SAFETY: the stream is open and is closed once, here.
    unsafe { libc::closedir(stream) };
    listed
}

/// The kind of the entry `name` of the open directory `dir`, from `fstatat` with
/// `AT_SYMLINK_NOFOLLOW`: a link is reported as a link, and nothing is opened, so a FIFO or a
/// device is never touched.
#[allow(unsafe_code)]
pub fn entry_kind_at(dir: &File, name: &OsStr) -> io::Result<EntryKind> {
    let name = entry_name(name)?;
    // SAFETY: a `stat` is plain data; all zeros is a valid value for `fstatat` to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as in `create_private_dir_at`: an open descriptor and a live C string, and a
    // whole `stat` to fill.
    cvt(unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => EntryKind::Dir,
        libc::S_IFREG => EntryKind::File,
        libc::S_IFLNK => EntryKind::Link,
        _ => EntryKind::Other,
    })
}

/// Whether `e`, from [`open_dir_at`] or [`open_read_at`], means that a link stands at the name,
/// or something that is not a directory where a directory was opened (`ELOOP`, `ENOTDIR`).
pub fn is_link_refusal(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
}

/// One entry name as a C string: not empty, not `.` or `..`, with no `/` and no NUL.
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
#[allow(unsafe_code)]
fn set_blocking(f: &File) -> io::Result<()> {
    let fd = f.as_raw_fd();
    // SAFETY: `fd` is open for the whole call; F_GETFL and F_SETFL take no pointer.
    let flags = cvt(unsafe { libc::fcntl(fd, libc::F_GETFL) })?;
    // SAFETY: as above.
    cvt(unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) }).map(|_| ())
}

/// A libc return value: -1 is the thread's `errno`.
fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// The permission bits of the file `meta` describes, with the set-id and sticky bits
/// (mode & 0o7777). For test fixtures and the upgrade backup's file modes.
pub fn mode_of(meta: &Metadata) -> u32 {
    meta.permissions().mode() & 0o7777
}

/// Permissions of exactly `mode` (for example 0o644), for `fs::set_permissions` or a
/// `tempfile::Builder`. For test fixtures and the upgrade backup's file modes.
pub fn permissions(mode: u32) -> fs::Permissions {
    fs::Permissions::from_mode(mode)
}

/// Sets the file or directory `path` to `mode`, following a link at `path`. For test fixtures.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, permissions(mode))
}

/// Makes the directories that `builder` creates `mode` less the umask. For test fixtures.
pub fn dir_mode(builder: &mut fs::DirBuilder, mode: u32) -> &mut fs::DirBuilder {
    builder.mode(mode)
}

/// Makes a file that `opts` creates `mode` less the umask. For test fixtures.
pub fn file_mode(opts: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    opts.mode(mode)
}

/// The inode number of the file `meta` describes: equal numbers on one device are one file.
/// For test fixtures.
pub fn inode(meta: &Metadata) -> u64 {
    meta.ino()
}

/// Makes `link` a symbolic link to `target`. For test fixtures.
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// A command that runs `program` under the process umask `umask` (octal text, for example
/// "002"); the caller adds the program's arguments. The umask is process-wide, so a test that
/// needs another one runs a child under it. For test fixtures.
pub fn under_umask(umask: &str, program: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", r#"umask "$1" && shift && exec "$@""#, "sh", umask])
        .arg(program);
    cmd
}

/// The answer to a failed platform punch: a file system that cannot is `Unsupported`, and every
/// other failure, an invalid range (`EINVAL`) included, stays an error.
fn punch_outcome(e: io::Error) -> io::Result<Punch> {
    match e.raw_os_error() {
        // on Linux EOPNOTSUPP and ENOTSUP are one value, so these are not match patterns
        Some(code) if [libc::ENOSYS, libc::EOPNOTSUPP, libc::ENOTSUP].contains(&code) => {
            Ok(Punch::Unsupported)
        }
        _ => Err(e),
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(super) fn platform_punch(file: &File, offset: u64, len: u64) -> io::Result<Punch> {
    let as_off = |v: u64| libc::off_t::try_from(v).map_err(|_| bad_punch_range("overflows"));
    let (offset, len) = (as_off(offset)?, as_off(len)?);
    // SAFETY: the call takes only an open descriptor and integers.
    let r = unsafe {
        libc::fallocate(
            file.as_raw_fd(),
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
            offset,
            len,
        )
    };
    match cvt(r) {
        Ok(_) => Ok(Punch::Supported),
        Err(e) => punch_outcome(e),
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(super) fn platform_punch(file: &File, offset: u64, len: u64) -> io::Result<Punch> {
    // SAFETY: an all-zero statfs is a valid value of this plain C struct.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is valid for writes for the call.
    cvt(unsafe { libc::fstatfs(file.as_raw_fd(), &mut st) })?;
    let Some((start, length)) = aligned_interior(offset, len, u64::from(st.f_bsize))? else {
        return Ok(Punch::Supported);
    };
    let as_off = |v: u64| libc::off_t::try_from(v).map_err(|_| bad_punch_range("overflows"));
    let args = libc::fpunchhole_t {
        fp_flags: 0,
        reserved: 0,
        fp_offset: as_off(start)?,
        fp_length: as_off(length)?,
    };
    // SAFETY: `args` is valid for reads for the call, and F_PUNCHHOLE reads this struct.
    let r = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &args) };
    match cvt(r) {
        Ok(_) => Ok(Punch::Supported),
        Err(e) => punch_outcome(e),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn platform_punch(_file: &File, _offset: u64, _len: u64) -> io::Result<Punch> {
    Ok(Punch::Unsupported)
}

/// The largest range inside `offset..offset + len` that starts and ends on a multiple of `block`,
/// as (start, length); None if it holds no whole block. It rounds inward only. A zero block size
/// or an overflow is an error.
#[cfg(any(target_os = "macos", test))]
fn aligned_interior(offset: u64, len: u64, block: u64) -> io::Result<Option<(u64, u64)>> {
    if block == 0 {
        return Err(io::Error::other("the file system reports no block size"));
    }
    let end = offset
        .checked_add(len)
        .ok_or_else(|| bad_punch_range("overflows"))?;
    let start = offset
        .checked_next_multiple_of(block)
        .ok_or_else(|| bad_punch_range("overflows"))?;
    let end = end - end % block;
    Ok((start < end).then(|| (start, end - start)))
}

/// The bytes the file system has allocated to `file`: its allocated block count times 512. This
/// is not its length.
pub fn allocated_bytes(file: &File) -> io::Result<u64> {
    blocks_to_bytes(file.metadata()?.blocks())
}

fn blocks_to_bytes(blocks: u64) -> io::Result<u64> {
    blocks
        .checked_mul(512)
        .ok_or_else(|| io::Error::other("the file's allocated size overflows"))
}

/// Unix only: the tests read modes, owners, error numbers and inodes, make symbolic links, sockets
/// and shell scripts, and run children under a umask.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pio::{PunchBackend, on_punch, punch_hole};

    #[test]
    fn mh1_ranges_and_allocation_are_checked() {
        // the platform call never sees a range these refuse
        let calls = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let seen = std::rc::Rc::clone(&calls);
        on_punch(Some(Box::new(move |offset, len| {
            seen.borrow_mut().push((offset, len));
            Some(Ok(Punch::Supported))
        })));
        let f = tempfile::tempfile().unwrap();
        write_all_at(&f, &[0u8; 8192], 0).unwrap();
        assert_eq!(punch_hole(&f, 100, 0).unwrap(), Punch::Supported);
        assert_eq!(
            punch_hole(&f, u64::MAX, 0).unwrap(),
            Punch::Supported,
            "zero length succeeds without a call, wherever it is"
        );
        for (offset, len) in [
            (u64::MAX, 1),
            (1, u64::MAX),
            (8192, 1),
            (8191, 2),
            (i64::MAX as u64, 1),
            (i64::MAX as u64 + 1, 1),
        ] {
            let e = punch_hole(&f, offset, len).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{offset} {len}");
        }
        assert!(calls.borrow().is_empty());
        // the last valid byte
        assert_eq!(punch_hole(&f, 8191, 1).unwrap(), Punch::Supported);
        assert_eq!(*calls.borrow(), [(8191, 1)]);
        on_punch(None);

        // the real call over a span with no aligned interior or a whole one is bounded
        assert!(punch_hole(&f, 8191, 1).is_ok());
        assert!(punch_hole(&f, 0, 8192).is_ok());
        let mut back = [1u8; 8192];
        read_exact_at(&f, &mut back, 0).unwrap();
        assert_eq!(back, [0u8; 8192]);

        // alignment rounds inward only
        for (offset, len, want) in [
            (0, 4096, Some((0, 4096))),
            (0, 4095, None),
            (1, 4095, None),
            (4095, 2, None),
            (1, 8191, Some((4096, 4096))),
            (5000, 10_000, Some((8192, 4096))),
            (4096, 8192, Some((4096, 8192))),
        ] {
            assert_eq!(aligned_interior(offset, len, 4096).unwrap(), want);
            if let Some((start, n)) = want {
                assert!(start >= offset && start + n <= offset + len);
            }
        }
        assert!(aligned_interior(0, 4096, 0).is_err(), "a zero block size");
        assert!(aligned_interior(u64::MAX, 1, 4096).is_err(), "end overflow");
        assert!(
            aligned_interior(u64::MAX - 1, 1, 4096).is_err(),
            "rounding up overflows"
        );

        // the errors that mean the file system cannot, and the rest
        for code in [libc::ENOSYS, libc::EOPNOTSUPP, libc::ENOTSUP] {
            let got = punch_outcome(io::Error::from_raw_os_error(code)).unwrap();
            assert_eq!(got, Punch::Unsupported, "{code}");
        }
        for code in [libc::EINVAL, libc::EPERM, libc::EACCES, libc::EIO] {
            assert!(punch_outcome(io::Error::from_raw_os_error(code)).is_err());
        }

        // allocation is checked and is not the file's length
        assert_eq!(blocks_to_bytes(0).unwrap(), 0);
        assert_eq!(blocks_to_bytes(3).unwrap(), 1536);
        assert_eq!(
            blocks_to_bytes(u64::MAX / 512).unwrap(),
            u64::MAX / 512 * 512
        );
        assert!(blocks_to_bytes(u64::MAX / 512 + 1).is_err());
        let sparse = tempfile::tempfile().unwrap();
        sparse.set_len(1 << 30).unwrap();
        assert!(allocated_bytes(&sparse).unwrap() < 1 << 30);
    }

    #[test]
    fn wx1_unix_seam_exports_keep_their_contract() {
        use crate::fileid::{self, ChangeStamp, FileId};
        // every export keeps its signature
        let _: fn(&File, &mut [u8], u64) -> io::Result<()> = read_exact_at;
        let _: fn(&File, &[u8], u64) -> io::Result<()> = write_all_at;
        let _: fn(&mut OpenOptions, &Path) -> io::Result<File> = open_no_follow;
        let _: fn(&mut OpenOptions) -> &mut OpenOptions = private_file;
        let _: fn(&File) -> io::Result<()> = make_private_file;
        let _: fn(&Path) -> io::Result<()> = create_private_dir;
        let _: fn(&Path) -> io::Result<()> = make_private_dir;
        let _: fn(&Path) -> io::Result<File> = open_dir;
        let _: fn(&File, &OsStr) -> io::Result<File> = open_dir_at;
        let _: fn(&File, &OsStr) -> io::Result<File> = open_read_at;
        let _: fn(&File, &OsStr) -> io::Result<File> = create_private_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = create_private_dir_at;
        let _: fn(&File, &OsStr) -> io::Result<bool> = umask_keeps_0700_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = remove_file_at;
        let _: fn(&File, &OsStr) -> io::Result<()> = remove_dir_at;
        let _: fn(&File, &OsStr, u32) -> io::Result<()> = set_mode_at;
        let _: fn(&mut std::process::Command, &File) -> io::Result<()> = start_in_dir;
        let _: fn(&File, &OsStr, &OsStr) -> io::Result<()> = rename_at;
        let _: fn(&File) -> io::Result<bool> = is_empty_dir;
        #[allow(clippy::type_complexity)]
        let _: fn(&File, &mut dyn FnMut(&OsStr) -> io::Result<()>) -> io::Result<()> =
            |d, f| for_each_entry(d, f);
        let _: fn(&File, &OsStr) -> io::Result<EntryKind> = entry_kind_at;
        let _: fn(&io::Error) -> bool = is_link_refusal;
        let _: fn(&File, u64, u64) -> io::Result<Punch> = punch_hole;
        let _: fn(Option<PunchBackend>) = on_punch;
        let _: fn(&File) -> io::Result<u64> = allocated_bytes;
        // the new handle queries: privacy, directory flush and durable rename
        let _: fn(&File) -> io::Result<Privacy> = privacy;
        let _: fn(&Path) -> io::Result<()> = sync_dir;
        let _: fn(&File, &Path) -> io::Result<()> = sync_open_dir;
        let _: fn(&Path, &Path) -> io::Result<()> = rename_durable;
        let _: fn(&File, &Path) -> io::Result<FileId> = FileId::of_open;
        let _: fn(&File, &Path) -> io::Result<u64> = fileid::links_of;
        let _: fn(&File, &Path) -> io::Result<ChangeStamp> = fileid::change_stamp_of;
        // the Unix owner and mode helpers stay for Unix mechanism tests
        let _: fn() -> u32 = own_uid;
        let _: fn(&Metadata) -> u32 = owner;
        let _: fn(&Metadata) -> bool = is_private_file;
        let _: fn(&Metadata) -> bool = is_private_dir;
        let _: fn(&Metadata) -> bool = is_shared_writable;
        let _: fn(&Metadata) -> bool = is_world_writable_unsticky;
        let _: fn(&Metadata) -> bool = is_owner_only;
        let _: fn(&Metadata) -> u32 = mode_of;
        let _: fn(u32) -> fs::Permissions = permissions;
        let _: fn(&Path, u32) -> io::Result<()> = set_mode;
        let _: fn(&Metadata) -> u64 = inode;

        // privacy is read from the handle, and it is what the mode helpers say
        let d = tempfile::tempdir().unwrap();
        let mine = Privacy {
            mine: true,
            private: true,
            owner_only: true,
            shared_writable: false,
            open_to_all: false,
        };
        let f = d.path().join("f");
        let file = private_file(OpenOptions::new().write(true).create_new(true))
            .open(&f)
            .unwrap();
        make_private_file(&file).unwrap();
        assert_eq!(privacy(&file).unwrap(), mine);
        for (mode, want) in [
            (0o600, mine),
            (
                0o400,
                Privacy {
                    private: false,
                    ..mine
                },
            ),
            (
                0o640,
                Privacy {
                    private: false,
                    owner_only: false,
                    ..mine
                },
            ),
            (
                0o620,
                Privacy {
                    private: false,
                    owner_only: false,
                    shared_writable: true,
                    ..mine
                },
            ),
            (
                0o602,
                Privacy {
                    private: false,
                    owner_only: false,
                    shared_writable: true,
                    open_to_all: true,
                    ..mine
                },
            ),
            (
                0o700,
                Privacy {
                    private: false,
                    ..mine
                },
            ),
        ] {
            set_mode(&f, mode).unwrap();
            let handle = File::open(&f).unwrap();
            assert_eq!(privacy(&handle).unwrap(), want, "{mode:o}");
            let m = handle.metadata().unwrap();
            assert_eq!(want.private, is_private_file(&m), "{mode:o}");
            assert_eq!(want.owner_only, is_owner_only(&m), "{mode:o}");
            assert_eq!(want.shared_writable, is_shared_writable(&m), "{mode:o}");
            assert_eq!(want.mine, owner(&m) == own_uid(), "{mode:o}");
        }
        let sub = d.path().join("sub");
        create_private_dir(&sub).unwrap();
        make_private_dir(&sub).unwrap();
        assert_eq!(privacy(&open_dir(&sub).unwrap()).unwrap(), mine);
        for (mode, private, open_to_all) in [
            (0o700, true, false),
            (0o600, false, false),
            (0o777, false, true),
            (0o1777, false, false),
        ] {
            set_mode(&sub, mode).unwrap();
            let p = privacy(&open_dir(&sub).unwrap()).unwrap();
            let m = fs::symlink_metadata(&sub).unwrap();
            assert_eq!(p.private, private, "{mode:o}");
            assert_eq!(p.private, is_private_dir(&m), "{mode:o}");
            assert_eq!(p.open_to_all, open_to_all, "{mode:o}");
            assert_eq!(p.open_to_all, is_world_writable_unsticky(&m), "{mode:o}");
        }
        set_mode(&sub, 0o700).unwrap();

        // a directory flush and a durable rename succeed, or say what failed and keep the kind
        sync_dir(d.path()).unwrap();
        sync_open_dir(&open_dir(d.path()).unwrap(), d.path()).unwrap();
        let missing = d.path().join("missing");
        let e = sync_dir(&missing).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(
            e.to_string()
                .starts_with(&format!("could not sync directory {}: ", missing.display())),
            "{e}"
        );
        let to = d.path().join("g");
        rename_durable(&f, &to).unwrap();
        assert!(!f.exists() && to.is_file());
        let e = rename_durable(&missing, &f).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(
            e.to_string().starts_with(&format!(
                "could not rename {} to {}: ",
                missing.display(),
                f.display()
            )),
            "{e}"
        );

        // the handle identity queries agree with the Metadata reads, and name the path
        let handle = File::open(&to).unwrap();
        let m = handle.metadata().unwrap();
        assert_eq!(FileId::of_open(&handle, &to).unwrap(), FileId::of(&m));
        assert_eq!(fileid::links_of(&handle, &to).unwrap(), fileid::links(&m));
        assert_eq!(
            fileid::change_stamp_of(&handle, &to).unwrap(),
            fileid::change_stamp(&m)
        );
        fs::hard_link(&to, d.path().join("h")).unwrap();
        assert_eq!(fileid::links_of(&handle, &to).unwrap(), 2);
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

    #[test]
    fn ok5_pio_lists_a_directory_handle_and_reads_entry_kinds_without_following() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("file"), b"x").unwrap();
        fs::create_dir(d.path().join("dir")).unwrap();
        symlink(d.path().join("file"), d.path().join("link")).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(d.path().join("sock")).unwrap();
        let dir = open_dir(d.path()).unwrap();
        let mut names = vec![];
        for_each_entry::<io::Error>(&dir, |n| {
            names.push(n.to_os_string());
            Ok(())
        })
        .unwrap();
        names.sort();
        assert_eq!(names, ["dir", "file", "link", "sock"]);
        for (name, kind) in [
            ("dir", EntryKind::Dir),
            ("file", EntryKind::File),
            ("link", EntryKind::Link),
            ("sock", EntryKind::Other),
        ] {
            assert_eq!(entry_kind_at(&dir, name.as_ref()).unwrap(), kind, "{name}");
        }
        assert_eq!(
            entry_kind_at(&dir, "gone".as_ref()).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        // the first error of the callback ends the listing and is returned
        let mut seen = 0;
        let e = for_each_entry(&dir, |_| {
            seen += 1;
            Err(io::Error::other("stop"))
        })
        .unwrap_err();
        assert_eq!((seen, e.to_string()), (1, "stop".to_string()));
        // an empty directory lists nothing
        let empty = open_dir_at(&dir, "dir".as_ref()).unwrap();
        for_each_entry::<io::Error>(&empty, |_| panic!("no entry")).unwrap();
    }
}
