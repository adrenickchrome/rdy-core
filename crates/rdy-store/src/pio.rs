//! Positional reads and writes: at an offset, without moving the file's cursor (FW-XP1 D-1RXEA).
//!
//! This is the only store module that uses the Unix positional-I/O calls. Every other module
//! calls these two functions.

use std::fs::File;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::FileExt;

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
