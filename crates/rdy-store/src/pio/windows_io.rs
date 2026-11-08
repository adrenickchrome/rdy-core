//! The Windows positioned reads and writes, hole punching and allocated bytes of [`crate::pio`].
//!
//! - A positioned call reads or writes at its own offset (`seek_read`, `seek_write`: `ReadFile`
//!   and `WriteFile` with the offset in their `OVERLAPPED`). On a synchronous handle Windows also
//!   moves the file's cursor, so each call puts the cursor back where it was. One lock covers the
//!   calls of every handle, because two handles of one opened file (`File::try_clone`) share
//!   their cursor. A cursor call that does not come through this module can still see the cursor
//!   moved while a positioned call runs.
//! - A short read or write goes on from where it stopped. A read that the file ends before is
//!   `UnexpectedEof`, a write that writes nothing is `WriteZero`, and every other failure is the
//!   OS error. A range past `i64::MAX` is `InvalidInput`, before any call.
//! - A hole punch marks the file sparse (`FSCTL_SET_SPARSE`), then asks the file system to free
//!   the range (`FSCTL_SET_ZERO_DATA`). A file system that has neither call is `Unsupported`.
//! - Allocated bytes are `FILE_STANDARD_INFO.AllocationSize`, read from the open handle.
//!
//! This file depends only on `std`, `windows-sys` and the portable `Punch`, so an isolated probe
//! can compile and test it on its own. No value is made up for a failed call.

use std::fs::File;
use std::io::{self, Seek, SeekFrom};
use std::mem::{MaybeUninit, size_of};
use std::os::windows::fs::FileExt;
use std::os::windows::io::AsRawHandle;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FILE_ZERO_DATA_INFORMATION, FSCTL_SET_SPARSE, FSCTL_SET_ZERO_DATA,
};

use crate::pio::Punch;

/// Held for each positioned call, from reading the cursor until it is put back.
static CURSOR: Mutex<()> = Mutex::new(());

/// Fills `buf` from `f` at `offset`. A file that ends first is `ErrorKind::UnexpectedEof`.
pub fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    check_range(offset, buf.len())?;
    keeping_cursor(f, || fill_at(buf, offset, |b, at| f.seek_read(b, at)))
}

/// Writes all of `buf` to `f` at `offset`. A write past the end extends the file.
pub fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    check_range(offset, buf.len())?;
    keeping_cursor(f, || put_at(buf, offset, |b, at| f.seek_write(b, at)))
}

/// Refuses a range of `len` bytes at `offset` that ends past `i64::MAX`, the last offset a
/// Windows file call takes.
fn check_range(offset: u64, len: usize) -> io::Result<()> {
    let end = u64::try_from(len)
        .ok()
        .and_then(|len| offset.checked_add(len));
    match end {
        Some(end) if i64::try_from(end).is_ok() => Ok(()),
        _ => Err(io::ErrorKind::InvalidInput.into()),
    }
}

/// Runs `call` and then puts `f`'s cursor back where it was before. The call's own failure wins
/// over a failure to put the cursor back.
fn keeping_cursor<T>(f: &File, call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    // the lock guards no data, so a panic while it was held leaves nothing to repair
    let _held = CURSOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut handle = f;
    let cursor = handle.stream_position()?;
    let out = call();
    let back = handle.seek(SeekFrom::Start(cursor));
    let out = out?;
    back?;
    Ok(out)
}

/// Fills `buf` with calls of `once(rest, at)`, each of which reads some bytes of `rest` at `at`.
/// A call that reads nothing is the end of the file; an interrupted call is made again.
fn fill_at(
    mut buf: &mut [u8],
    mut at: u64,
    mut once: impl FnMut(&mut [u8], u64) -> io::Result<usize>,
) -> io::Result<()> {
    while !buf.is_empty() {
        match once(buf, at) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ));
            }
            Ok(n) => {
                buf = &mut buf[n..];
                at += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes all of `buf` with calls of `once(rest, at)`, each of which writes some bytes of `rest`
/// at `at`. A call that writes nothing is `WriteZero`; an interrupted call is made again.
fn put_at(
    mut buf: &[u8],
    mut at: u64,
    mut once: impl FnMut(&[u8], u64) -> io::Result<usize>,
) -> io::Result<()> {
    while !buf.is_empty() {
        match once(buf, at) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write whole buffer",
                ));
            }
            Ok(n) => {
                buf = &buf[n..];
                at += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Marks `file` sparse, then frees `offset..offset + len`, which `pio::punch_hole` has checked
/// against the file and `i64::MAX`.
pub(in crate::pio) fn platform_punch(file: &File, offset: u64, len: u64) -> io::Result<Punch> {
    let as_i64 =
        |v: u64| i64::try_from(v).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
    let end = offset
        .checked_add(len)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let zero = FILE_ZERO_DATA_INFORMATION {
        FileOffset: as_i64(offset)?,
        BeyondFinalZero: as_i64(end)?,
    };
    punch_with(
        || control(file, FSCTL_SET_SPARSE, None),
        || control(file, FSCTL_SET_ZERO_DATA, Some(&zero)),
    )
}

/// A punch from its two steps: no zero-data request before the file is sparse, since on a file
/// that is not sparse that request writes zeros and frees nothing.
fn punch_with(
    set_sparse: impl FnOnce() -> io::Result<()>,
    zero_data: impl FnOnce() -> io::Result<()>,
) -> io::Result<Punch> {
    if let Err(e) = set_sparse() {
        return punch_outcome(e);
    }
    match zero_data() {
        Ok(()) => Ok(Punch::Supported),
        Err(e) => punch_outcome(e),
    }
}

/// The answer to a failed step: a file system without the call (`ERROR_INVALID_FUNCTION`,
/// `ERROR_NOT_SUPPORTED`) is `Unsupported`, and every other failure stays an error.
fn punch_outcome(e: io::Error) -> io::Result<Punch> {
    match e.raw_os_error().and_then(|code| u32::try_from(code).ok()) {
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => Ok(Punch::Unsupported),
        _ => Err(e),
    }
}

/// One file-system control call on `file` with an optional zero-data range and no output.
#[allow(unsafe_code)]
fn control(file: &File, code: u32, range: Option<&FILE_ZERO_DATA_INFORMATION>) -> io::Result<()> {
    let (input, size) = match range {
        Some(r) => (
            std::ptr::from_ref(r).cast(),
            u32::try_from(size_of::<FILE_ZERO_DATA_INFORMATION>()).map_err(io::Error::other)?,
        ),
        None => (std::ptr::null(), 0),
    };
    let mut returned = 0u32;
    // SAFETY: the handle is open for as long as `file` is borrowed; the input is null with size
    // 0, or a live FILE_ZERO_DATA_INFORMATION of `size` bytes; there is no output buffer; the
    // returned count is a live u32; no OVERLAPPED, so the call completes before it returns.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            input,
            size,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The bytes the file system has allocated to `file`: `FILE_STANDARD_INFO.AllocationSize`. This
/// is not its length; a sparse file's freed ranges are not in it.
#[allow(unsafe_code)]
pub fn allocated_bytes(file: &File) -> io::Result<u64> {
    let mut out = MaybeUninit::<FILE_STANDARD_INFO>::zeroed();
    let size = u32::try_from(size_of::<FILE_STANDARD_INFO>()).map_err(io::Error::other)?;
    // SAFETY: the handle is open for as long as `file` is borrowed; the buffer is writable, of
    // `size` bytes, and of the type FileStandardInfo fills.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            out.as_mut_ptr().cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so it filled the whole struct (zeroed before, in any case).
    allocation(unsafe { out.assume_init() }.AllocationSize)
}

/// A reported allocation as a byte count; a negative one is an error, never a made-up count.
fn allocation(size: i64) -> io::Result<u64> {
    u64::try_from(size).map_err(|_| io::Error::other("the file's allocated size overflows"))
}

/// Native cases: fault-injected short and failed calls, the punch outcome of each failure, and
/// real NTFS punches, cursors and allocation.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{ErrorKind, Read, Write};
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_DISK_FULL, ERROR_INVALID_PARAMETER,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SPARSE_FILE;

    fn os(code: u32) -> io::Error {
        io::Error::from_raw_os_error(code as i32)
    }

    #[test]
    fn wx3_native_short_reads_and_writes_go_on_where_they_stopped() {
        // one byte per call: every offset once, in order, and the whole buffer
        let source: Vec<u8> = (0u8..=200).collect();
        let mut seen = vec![];
        let mut buf = [0u8; 50];
        fill_at(&mut buf, 100, |b, at| {
            seen.push((at, b.len()));
            b[0] = source[at as usize];
            Ok(1)
        })
        .unwrap();
        assert_eq!(buf[..], source[100..150]);
        assert_eq!(
            seen,
            (0..50)
                .map(|i| (100 + i, 50 - i as usize))
                .collect::<Vec<_>>()
        );

        // a read that ends early is UnexpectedEof, with std's text; an interruption is retried
        let mut calls = 0;
        let mut buf = [0u8; 8];
        let e = fill_at(&mut buf, 0, |b, _| {
            calls += 1;
            match calls {
                1 => Err(ErrorKind::Interrupted.into()),
                2 => {
                    b[..3].copy_from_slice(b"abc");
                    Ok(3)
                }
                _ => Ok(0),
            }
        })
        .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(e.to_string(), "failed to fill whole buffer");
        assert_eq!((calls, &buf[..3]), (3, &b"abc"[..]));

        // any other failure is the OS error, and no further call is made
        let mut calls = 0;
        let e = fill_at(&mut [0u8; 8], 0, |_, _| {
            calls += 1;
            Err(os(ERROR_ACCESS_DENIED))
        })
        .unwrap_err();
        assert_eq!(
            (calls, e.raw_os_error()),
            (1, Some(ERROR_ACCESS_DENIED as i32))
        );

        // writes: short writes go on, a write of nothing is WriteZero, a failure is its error
        let mut sink = [0u8; 64];
        let mut seen = vec![];
        put_at(b"0123456789", 20, |b, at| {
            seen.push(at);
            let n = b.len().min(3);
            sink[at as usize..at as usize + n].copy_from_slice(&b[..n]);
            Ok(n)
        })
        .unwrap();
        assert_eq!(&sink[20..30], b"0123456789");
        assert_eq!(seen, [20, 23, 26, 29]);
        let mut calls = 0;
        let e = put_at(b"xy", 0, |_, _| {
            calls += 1;
            if calls == 1 {
                Err(ErrorKind::Interrupted.into())
            } else {
                Ok(0)
            }
        })
        .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::WriteZero);
        assert_eq!(e.to_string(), "failed to write whole buffer");
        assert_eq!(calls, 2);
        let e = put_at(b"xy", 0, |_, _| Err(os(ERROR_DISK_FULL))).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERROR_DISK_FULL as i32));

        // the range check comes before any call
        for (offset, len) in [
            (u64::MAX, 1usize),
            (i64::MAX as u64, 1),
            (i64::MAX as u64 + 1, 1),
            (i64::MAX as u64 - 3, 5),
            (1, usize::MAX),
        ] {
            let e = check_range(offset, len).unwrap_err();
            assert_eq!(e.kind(), ErrorKind::InvalidInput, "{offset} {len}");
        }
        check_range(i64::MAX as u64 - 4, 4).unwrap();
        check_range(0, 0).unwrap();
        let f = tempfile::tempfile().unwrap();
        let e = read_exact_at(&f, &mut [0u8; 4], u64::MAX - 1).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        let e = write_all_at(&f, b"abcd", i64::MAX as u64 - 1).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
        assert_eq!(f.metadata().unwrap().len(), 0, "no write was made");
    }

    #[test]
    fn wx3_native_the_cursor_is_kept_through_success_and_failure() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        std::fs::write(&path, b"0123456789").unwrap();
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.seek(SeekFrom::Start(4)).unwrap();
        write_all_at(&f, b"zz", 20).unwrap();
        let mut two = [0u8; 2];
        read_exact_at(&f, &mut two, 20).unwrap();
        assert_eq!(&two, b"zz");
        assert_eq!(f.stream_position().unwrap(), 4);
        // a read the file ends before, after it has read some bytes
        let e = read_exact_at(&f, &mut [0u8; 8], 18).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(f.stream_position().unwrap(), 4);
        f.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"45");

        // a clone shares the cursor: neither handle's calls move it
        let clone = f.try_clone().unwrap();
        read_exact_at(&clone, &mut two, 0).unwrap();
        assert_eq!(f.stream_position().unwrap(), 6);

        // a write through a read-only handle is the OS error, and the cursor stays
        let mut ro = std::fs::File::open(&path).unwrap();
        ro.seek(SeekFrom::Start(3)).unwrap();
        let e = write_all_at(&ro, b"no", 0).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        assert_eq!(ro.stream_position().unwrap(), 3);
        drop((f, clone, ro));
        let mut want = b"0123456789".to_vec();
        want.resize(20, 0);
        want.extend(b"zz");
        assert_eq!(std::fs::read(&path).unwrap(), want);

        // a cursor write after positioned calls lands at the cursor
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open(&path)
            .unwrap();
        f.seek(SeekFrom::Start(1)).unwrap();
        write_all_at(&f, b"Q", 0).unwrap();
        f.write_all(b"W").unwrap();
        assert_eq!(&std::fs::read(&path).unwrap()[..3], b"QW2");
    }

    #[test]
    fn wx3_native_punch_failures_are_classified() {
        for code in [ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED] {
            assert_eq!(
                punch_outcome(os(code)).unwrap(),
                Punch::Unsupported,
                "{code}"
            );
        }
        for code in [
            ERROR_ACCESS_DENIED,
            ERROR_INVALID_PARAMETER,
            ERROR_DISK_FULL,
        ] {
            let e = punch_outcome(os(code)).unwrap_err();
            assert_eq!(e.raw_os_error(), Some(code as i32));
        }
        assert!(punch_outcome(io::Error::other("no code")).is_err());

        // each step's failure, and the order of the steps
        let steps = std::cell::RefCell::new(vec![]);
        let step = |name: &'static str, answer: Option<u32>| {
            let steps = &steps;
            move || {
                steps.borrow_mut().push(name);
                answer.map_or(Ok(()), |code| Err(os(code)))
            }
        };
        // (sparse step's failure, zero step's failure, outcome or error code, steps made)
        type Case = (
            Option<u32>,
            Option<u32>,
            Result<Punch, u32>,
            &'static [&'static str],
        );
        let cases: [Case; 5] = [
            (None, None, Ok(Punch::Supported), &["sparse", "zero"]),
            (
                Some(ERROR_INVALID_FUNCTION),
                None,
                Ok(Punch::Unsupported),
                &["sparse"],
            ),
            (
                Some(ERROR_ACCESS_DENIED),
                None,
                Err(ERROR_ACCESS_DENIED),
                &["sparse"],
            ),
            (
                None,
                Some(ERROR_NOT_SUPPORTED),
                Ok(Punch::Unsupported),
                &["sparse", "zero"],
            ),
            (
                None,
                Some(ERROR_DISK_FULL),
                Err(ERROR_DISK_FULL),
                &["sparse", "zero"],
            ),
        ];
        for (sparse, zero, want, order) in cases {
            steps.borrow_mut().clear();
            let got = punch_with(step("sparse", sparse), step("zero", zero));
            match want {
                Ok(p) => assert_eq!(got.unwrap(), p),
                Err(code) => assert_eq!(got.unwrap_err().raw_os_error(), Some(code as i32)),
            }
            assert_eq!(*steps.borrow(), order);
        }
    }

    #[test]
    fn wx3_ntfs_sparse_punch_frees_allocation_and_keeps_bytes() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("seg");
        let len = 8 * 1024 * 1024;
        let mut content = vec![0xA5u8; len];
        content[1024 * 1024 + 7..7 * 1024 * 1024 - 3].fill(0);
        std::fs::write(&path, &content).unwrap();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let before = allocated_bytes(&f).unwrap();
        assert!(
            before >= len as u64,
            "a written file is allocated: {before}"
        );
        assert_eq!(
            f.metadata().unwrap().file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE,
            0
        );

        let (start, n) = (1024 * 1024 + 7, 6 * 1024 * 1024 - 10);
        for round in 0..2 {
            assert_eq!(
                platform_punch(&f, start, n).unwrap(),
                Punch::Supported,
                "{round}"
            );
            assert_ne!(
                std::fs::metadata(&path).unwrap().file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE,
                0,
                "marked sparse"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                content,
                "{round}: every byte"
            );
            assert_eq!(f.metadata().unwrap().len(), len as u64);
        }
        f.sync_all().unwrap();
        let after = allocated_bytes(&f).unwrap();
        assert!(
            after < before && after <= (len as u64) - 5 * 1024 * 1024,
            "allocation fell: {before} -> {after}"
        );
        let mut back = vec![1u8; len];
        read_exact_at(&f, &mut back, 0).unwrap();
        assert_eq!(back, content);

        // a handle without write access is refused by the file system: an error, not Unsupported
        let ro = std::fs::File::open(&path).unwrap();
        let e = platform_punch(&ro, start, n).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        assert_eq!(std::fs::read(&path).unwrap(), content);
    }

    #[test]
    fn wx3_allocation_is_checked() {
        assert_eq!(allocation(0).unwrap(), 0);
        assert_eq!(allocation(4096).unwrap(), 4096);
        assert_eq!(allocation(i64::MAX).unwrap(), i64::MAX as u64);
        let e = allocation(-1).unwrap_err();
        assert_eq!(e.to_string(), "the file's allocated size overflows");
        let f = tempfile::tempfile().unwrap();
        assert_eq!(
            allocated_bytes(&f).unwrap(),
            0,
            "an empty file has no allocation"
        );
    }
}
