//! The store's platform calls (FW-XP1 D-1RXEA), one seam for the Unix-only file calls that store
//! and engine code make:
//! - positional reads and writes: at an offset, without moving the file's cursor;
//! - no-follow opens (`open_no_follow`): a file that is a symbolic link is refused, never
//!   followed;
//! - privacy: this process's uid (`own_uid`), a file's owner (`owner`), private files of mode
//!   0o600 (`private_file`, `make_private_file`, `is_private_file`) and private directories of
//!   mode 0o700 (`create_private_dir`, `make_private_dir`, `is_private_dir`).
//!
//! Every other store module, and the engine, calls these functions. The daemon's `sys` seam
//! re-exports the no-follow and privacy calls under the same names.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
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
