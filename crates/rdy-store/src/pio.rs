//! The store's platform calls (FW-XP1 D-1RXEA), one seam for the platform-specific file calls
//! that store and engine code make:
//! - positional reads and writes: at an offset, without moving the file's cursor;
//! - no-follow opens (`open_no_follow`): a file that is a symbolic link is refused, never
//!   followed;
//! - privacy: private files and directories (`private_file`, `make_private_file`,
//!   `create_private_dir`, `make_private_dir`), and who may use an open file or directory
//!   ([`privacy`], one query of its handle, as a [`Privacy`]);
//! - durability: a directory flush ([`sync_dir`], [`sync_open_dir`]) and a durable rename
//!   ([`rename_durable`]); a failure of either is an error, never a success;
//! - calls in an open directory (`open_dir`, `open_dir_at`, `open_read_at`,
//!   `create_private_at`, `create_private_dir_at`, `umask_keeps_0700_at`, `remove_file_at`,
//!   `remove_dir_at`, `set_mode_at`, `rename_at`, `is_link_refusal`): each names one entry of a
//!   directory handle, so a path that is changed after the handle was opened cannot redirect
//!   it, and none follows a link at that entry; `is_empty_dir` and `for_each_entry` list a
//!   directory handle itself, and `entry_kind_at` reads an entry's kind without following or
//!   opening it;
//! - a child process that starts in an open directory (`start_in_dir`), for the same reason;
//! - hole punching ([`punch_hole`]) and a file's allocated bytes (`allocated_bytes`): a file
//!   system's own best effort, which never changes a byte that a read returns;
//! - Unix only, for Unix mechanism tests and the Unix seams of the daemon and QA: this process's
//!   uid (`own_uid`), a file's owner (`owner`), the mode tests `is_private_file`,
//!   `is_private_dir`, `is_shared_writable`, `is_world_writable_unsticky` and `is_owner_only`,
//!   and the test fixtures: a file's mode bits and inode (`mode_of`, `inode`), any mode
//!   (`permissions`, `set_mode`, `dir_mode`, `file_mode`), symbolic links (`symlink`) and a
//!   child process under another umask (`under_umask`), so the store's and the engine's tests
//!   need no Unix-only call of their own.
//!
//! Each platform's calls live in a child module: `unix` and `windows`. Every other store module,
//! and the engine, calls these functions. The daemon's `sys` seam re-exports the no-follow and
//! privacy calls under the same names.

use std::fs::File;
use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

// Stops the build in this module itself, so that each call into it is not one more error: the
// placeholders in pio/windows.rs name each part that is missing.
#[cfg(windows)]
compile_error!(
    "rdy_store::pio is not built on Windows yet; each part that is missing has its own error in pio/windows.rs (FW-XP1 Phase 4)"
);

#[cfg(not(any(unix, windows)))]
compile_error!("rdy_store::pio has no implementation for this platform (FW-XP1 D-1RXEA)");

/// Who may use an open file or directory, as [`privacy`] reads it from the handle in one query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Privacy {
    /// This account owns it.
    pub mine: bool,
    /// It is exactly as private as rdy makes a private file or directory: only its owner may use
    /// it. On Unix, a regular file of mode 0o600 or a directory of mode 0o700; any other kind
    /// of file is never private.
    pub private: bool,
    /// No other account may use it (on Unix, no permission bit for its group or others).
    pub owner_only: bool,
    /// Another account may write it; in a directory, it may remove or replace an entry (on Unix,
    /// mode bit 0o020 or 0o002).
    pub shared_writable: bool,
    /// Every account may write it, and nothing limits the removal or replacement of an entry to
    /// the entry's owner (on Unix, mode bit 0o002 without the sticky bit 0o1000).
    pub open_to_all: bool,
}

/// The kind of a directory entry, as [`entry_kind_at`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// A symbolic link, never followed.
    Link,
    /// A socket, FIFO, device or any other kind of file.
    Other,
}

/// What a file system did with a hole punch. `Supported` means that it accepted the call, not that
/// the disk allocation fell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Punch {
    Supported,
    Unsupported,
}

/// A test backend of [`punch_hole`]: it sees each range that passed the checks, and its answer
/// replaces the platform call. None lets the platform call run.
#[doc(hidden)]
pub type PunchBackend = Box<dyn FnMut(u64, u64) -> Option<io::Result<Punch>>>;

thread_local! {
    static PUNCH_BACKEND: std::cell::RefCell<Option<PunchBackend>> =
        const { std::cell::RefCell::new(None) };
}

/// Test hook: every [`punch_hole`] on this thread calls `backend`. None removes it.
#[doc(hidden)]
pub fn on_punch(backend: Option<PunchBackend>) {
    PUNCH_BACKEND.with(|b| *b.borrow_mut() = backend);
}

/// Gives the bytes at `offset..offset + len` of `file`, which must be open for writing and hold
/// zeros there, back to the file system. The file's length and every byte that a read returns
/// stay as they are. A range that overflows or ends past the file is `InvalidInput` and makes no
/// call; a zero length makes no call and succeeds. On macOS only the range's aligned interior is
/// punched, so a span with no aligned interior makes no call. A file system that cannot punch is
/// `Unsupported`; any other failed call is an error.
pub fn punch_hole(file: &File, offset: u64, len: u64) -> io::Result<Punch> {
    if len == 0 {
        return Ok(Punch::Supported);
    }
    let end = offset
        .checked_add(len)
        .filter(|end| i64::try_from(*end).is_ok())
        .ok_or_else(|| bad_punch_range("overflows"))?;
    if end > file.metadata()?.len() {
        return Err(bad_punch_range("ends past the file"));
    }
    let injected = PUNCH_BACKEND.with(|b| b.borrow_mut().as_mut().and_then(|f| f(offset, len)));
    match injected {
        Some(answer) => answer,
        #[cfg(unix)]
        None => unix::platform_punch(file, offset, len),
        #[cfg(windows)]
        None => windows::platform_punch(file, offset, len),
    }
}

fn bad_punch_range(why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("the punch range {why}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::Path;

    /// The text of `rel`, a path under this crate.
    fn source(rel: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The message of each `compile_error!` in `code`, in order, and the code left outside them.
    fn placeholders(code: &str) -> (Vec<String>, String) {
        let mut found = vec![];
        let mut rest = String::new();
        let mut text = code;
        while let Some(at) = text.find("compile_error!(") {
            rest.push_str(&text[..at]);
            let after = &text[at..];
            let end = after.find(");").expect("a compile_error! ends with );") + 2;
            let call = &after[..end];
            let (open, close) = (call.find('"').unwrap(), call.rfind('"').unwrap());
            found.push(call[open + 1..close].to_string());
            text = &after[end..];
        }
        rest.push_str(text);
        (found, rest)
    }

    /// The lines of `text` that are neither blank nor comments, joined by newlines.
    fn code_lines(text: &str) -> String {
        text.lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What a body that compiles and answers without the platform call would hold.
    const STUB_WORDS: &[&str] = &[
        "fn ",
        "impl ",
        "Ok(",
        "unreachable!",
        "unimplemented!",
        "todo!",
        "Default::default",
    ];

    #[test]
    fn wx1_isolated_interface_probe_has_no_success_stub() {
        // The Windows provider source, which an isolated native probe compiles on its own, holds
        // only the registrations of its built children and the placeholders of the parts not
        // built yet: no body that compiles, and no answer made up without the platform call.
        let (found, rest) = placeholders(&code_lines(&source("src/pio/windows.rs")));
        assert_eq!(
            rest.trim(),
            "pub use windows_io::*;\n#[path = \"windows_io.rs\"]\nmod windows_io;\n\
             pub use windows_security::*;\n#[path = \"windows_security.rs\"]\n\
             mod windows_security;",
            "code outside the placeholders"
        );
        assert_eq!(
            found,
            [
                "rdy_store::pio has no Windows implementation yet of opens, entry kinds and \
                 listings anchored to an open directory (FW-XP1 Phase 4)",
                "rdy_store::pio has no Windows implementation yet of creates and removals \
                 anchored to an open directory (FW-XP1 Phase 4)",
                "rdy_store::pio has no Windows implementation yet of renames anchored to an \
                 open directory or of a child process that starts in one (FW-XP1 Phase 4)",
                "rdy_store::pio has no Windows implementation yet of directory flushes \
                 (FlushFileBuffers) or write-through renames (MoveFileExW) (FW-XP1 Phase 4)",
            ]
        );
        // This module stops the build itself on Windows too, so that each call into it is not one
        // more error, and holds nothing else for Windows.
        let pio = source("src/pio.rs");
        let (found, _) = placeholders(&code_lines(pio.split("#[cfg(test)]").next().unwrap()));
        assert_eq!(
            found,
            [
                "rdy_store::pio is not built on Windows yet; each part that is missing has its \
                 own error in pio/windows.rs (FW-XP1 Phase 4)",
                "rdy_store::pio has no implementation for this platform (FW-XP1 D-1RXEA)",
            ]
        );
        // The file identity seam is built on Windows (Task 2): fileid/windows.rs makes each
        // platform call, and neither file holds a stub body before its tests or a Windows
        // placeholder.
        let fileid = source("src/fileid.rs");
        let product = code_lines(fileid.split("#[cfg(all(test").next().unwrap());
        let provider = source("src/fileid/windows.rs");
        let provider = code_lines(provider.split("#[cfg(test)]").next().unwrap());
        for (file, code) in [("fileid.rs", &product), ("fileid/windows.rs", &provider)] {
            for word in [
                "unreachable!",
                "unimplemented!",
                "todo!",
                "Default::default",
            ] {
                assert!(!code.contains(word), "{file} holds {word}");
            }
        }
        assert_eq!(
            placeholders(&product).0,
            ["rdy_store::fileid has no implementation for this platform (FW-XP1 D-1RXEA)"]
        );
        assert!(placeholders(&provider).0.is_empty());
        for call in [
            "GetFileInformationByHandleEx",
            "FileIdInfo",
            "FileBasicInfo",
            "FileStandardInfo",
        ] {
            assert!(provider.contains(call), "fileid/windows.rs calls {call}");
        }
        let windows_pio = code_lines(&source("src/pio/windows.rs"));
        for word in STUB_WORDS {
            assert!(!windows_pio.contains(word), "pio/windows.rs holds {word}");
        }
        // Positioned I/O, punching and allocation are built (Task 3): pio/windows_io.rs makes each
        // platform call, and holds no stub body or placeholder before its tests.
        let io = source("src/pio/windows_io.rs");
        let io = code_lines(io.split("#[cfg(test)]").next().unwrap());
        for word in [
            "unreachable!",
            "unimplemented!",
            "todo!",
            "Default::default",
        ] {
            assert!(!io.contains(word), "pio/windows_io.rs holds {word}");
        }
        assert!(placeholders(&io).0.is_empty());
        for call in [
            "seek_read",
            "seek_write",
            "DeviceIoControl",
            "FSCTL_SET_SPARSE",
            "FSCTL_SET_ZERO_DATA",
            "GetFileInformationByHandleEx",
            "FileStandardInfo",
        ] {
            assert!(io.contains(call), "pio/windows_io.rs calls {call}");
        }
        // Privacy and no-follow opens are built (Task 4): pio/windows_security.rs reads the
        // token's user and each object's descriptor, makes objects with the private descriptor,
        // sets it, and refuses reparse points; it holds no stub body or placeholder before its
        // tests.
        let security = source("src/pio/windows_security.rs");
        let security = code_lines(security.split("#[cfg(test)]").next().unwrap());
        for word in [
            "unreachable!",
            "unimplemented!",
            "todo!",
            "Default::default",
        ] {
            assert!(
                !security.contains(word),
                "pio/windows_security.rs holds {word}"
            );
        }
        assert!(placeholders(&security).0.is_empty());
        for call in [
            "GetTokenInformation",
            "GetKernelObjectSecurity",
            "SetKernelObjectSecurity",
            "ReOpenFile",
            "CreateFileW",
            "CreateDirectoryW",
            "FileAttributeTagInfo",
            "FILE_FLAG_OPEN_REPARSE_POINT",
            "FILE_ATTRIBUTE_REPARSE_POINT",
            "SE_DACL_PROTECTED",
        ] {
            assert!(
                security.contains(call),
                "pio/windows_security.rs calls {call}"
            );
        }
    }

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
}
