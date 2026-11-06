//! The store's platform calls (FW-XP1 D-1RXEA), one seam for the platform-specific file calls
//! that store and engine code make:
//! - positional reads and writes: at an offset, without moving the file's cursor;
//! - no-follow opens (`open_no_follow`): a file that is a symbolic link is refused, never
//!   followed;
//! - privacy: private files and directories (`private_file`, `make_private_file`,
//!   `create_private_dir`, `make_private_dir`), and who may use an open file or directory
//!   ([`privacy`], one query of its handle, as a [`Privacy`]);
//! - durability: a directory flush ([`sync_dir`], [`sync_open_dir`]) and a durable rename
//!   ([`rename_durable`]); a failure of either is an error, never a success. On Windows a flush is
//!   `FlushFileBuffers` on a directory handle and a rename is a write-through `MoveFileExW`
//!   (D-FPKPS). A test backend ([`on_durable`]) sees each flush and rename, `rename_at`'s too,
//!   before and after its platform call, and can fail it;
//! - calls in an open directory (`open_dir`, `open_dir_at`, `open_read_at`,
//!   `create_private_at`, `create_private_dir_at`, `umask_keeps_0700_at`, `remove_file_at`,
//!   `remove_dir_at`, `rename_at`, `is_link_refusal`, and on Unix only `set_mode_at`): each names
//!   one entry of a directory handle, so a path that is changed after the handle was opened
//!   cannot redirect it, and none follows a link at that entry; `is_empty_dir` and
//!   `for_each_entry` list a directory handle itself, and `entry_kind_at` reads an entry's kind
//!   without following or opening it. Windows has no permission modes, so `set_mode_at` has no
//!   Windows form: a private Windows create gives the object its private descriptor;
//! - a child process that starts in an open directory (`start_in_dir`), and the path of a program
//!   in that directory (`program_in_dir`), for the same reason; each gives a guard (`StartPin`,
//!   `ProgramInDir`) that the caller holds until the child is waited;
//! - hole punching ([`punch_hole`]) and a file's allocated bytes (`allocated_bytes`): a file
//!   system's own best effort, which never changes a byte that a read returns;
//! - Unix only, for Unix mechanism tests and the Unix seams of the daemon and QA: this process's
//!   uid (`own_uid`), a file's owner (`owner`), the mode tests `is_private_file`,
//!   `is_private_dir`, `is_shared_writable`, `is_world_writable_unsticky` and `is_owner_only`,
//!   and the test fixtures: a file's mode bits and inode (`mode_of`, `inode`), any mode
//!   (`permissions`, `set_mode`, `dir_mode`, `file_mode`), symbolic links (`symlink`), a Unix
//!   socket (`bind_socket`) and a child process under another umask (`under_umask`), so the
//!   store's and the engine's tests need no Unix-only call of their own;
//! - child windows: `no_console_window` makes a console program that rdy starts run without a
//!   console window of its own (Windows `CREATE_NO_WINDOW`; nothing on Unix), so a detached
//!   daemon never flashes a window when it runs git;
//! - on every platform, for tests that kill or abort a child: how the child ended (`ended`); and
//!   for tests of text that is not Unicode: such text (`not_unicode`).
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

/// One durable step, as a test backend of [`on_durable`] sees it.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DurableStep {
    /// A flush of the directory at the path ([`sync_dir`], [`sync_open_dir`]).
    Flush(std::path::PathBuf),
    /// A rename of `from` to `to` ([`rename_durable`], [`rename_at`]), with the flags of its
    /// platform call: MoveFileExW's on Windows, none (0) on Unix. In a [`rename_at`] on Unix, the
    /// two paths are the entry names.
    Rename {
        from: std::path::PathBuf,
        to: std::path::PathBuf,
        flags: u32,
    },
}

/// Where a test backend of [`on_durable`] runs: just before a step's platform call, or just after
/// the call succeeded.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Before,
    After,
}

/// A test backend of the durable steps: it sees each step at each [`Boundary`], and an error it
/// gives fails the step. Before the call, the call is then not made; after it, the call was made.
/// It can only fail a step: a call's own error is always the step's error.
#[doc(hidden)]
pub type DurableBackend = Box<dyn FnMut(&DurableStep, Boundary) -> io::Result<()>>;

thread_local! {
    static DURABLE_BACKEND: std::cell::RefCell<Option<DurableBackend>> =
        const { std::cell::RefCell::new(None) };
}

/// Test hook: every durable step on this thread calls `backend`. None removes it.
#[doc(hidden)]
pub fn on_durable(backend: Option<DurableBackend>) {
    DURABLE_BACKEND.with(|b| *b.borrow_mut() = backend);
}

/// Runs `call`, the platform call of the durable step that `step` describes, with the test
/// backend of [`on_durable`], if one is set, before and after it. `step` is made only for a
/// backend.
pub(crate) fn durable(
    step: impl FnOnce() -> DurableStep,
    call: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let step = DURABLE_BACKEND.with(|b| b.borrow().is_some()).then(step);
    let at = |boundary| match &step {
        Some(step) => DURABLE_BACKEND.with(|b| match b.borrow_mut().as_mut() {
            Some(backend) => backend(step, boundary),
            None => Ok(()),
        }),
        None => Ok(()),
    };
    at(Boundary::Before)?;
    call()?;
    at(Boundary::After)
}

/// How a test's child process ended, as [`ended`] reads it from its exit status.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// `Child::kill` ended it: `SIGKILL` on Unix; on Windows `TerminateProcess`, which
    /// `Child::kill` calls with exit code 1.
    Killed,
    /// `std::process::abort` ended it: `SIGABRT` on Unix; on Windows the fail-fast status
    /// `STATUS_STACK_BUFFER_OVERRUN` (0xC0000409).
    Aborted,
    /// It ended any other way.
    Other,
}

/// How the child process whose exit status is `status` ended: killed, aborted or otherwise. On
/// Windows a child that exits with code 1 by itself also reads as killed: the test that kills a
/// child knows that it did. For test fixtures.
#[doc(hidden)]
pub fn ended(status: &std::process::ExitStatus) -> Ended {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        match status.signal() {
            Some(9) => Ended::Killed,
            Some(6) => Ended::Aborted,
            _ => Ended::Other,
        }
    }
    #[cfg(windows)]
    {
        const STATUS_STACK_BUFFER_OVERRUN: u32 = 0xC000_0409;
        match status.code().map(|c| c as u32) {
            Some(1) => Ended::Killed,
            Some(STATUS_STACK_BUFFER_OVERRUN) => Ended::Aborted,
            _ => Ended::Other,
        }
    }
}

/// `text` followed by one code unit that is not Unicode: the byte 0xE9 on Unix, an unpaired
/// surrogate on Windows. For test fixtures of paths and account names that are not Unicode.
#[doc(hidden)]
pub fn not_unicode(text: &str) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0xE9);
        std::ffi::OsString::from_vec(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0xD800);
        std::ffi::OsString::from_wide(&wide)
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
    #[test]
    fn wx12d_not_unicode_text_keeps_its_start_and_has_no_str() {
        let odd = super::not_unicode("zo");
        assert_eq!(odd.to_str(), None);
        assert!(odd.to_string_lossy().starts_with("zo"), "{odd:?}");
    }

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
        // only the registrations of its built children, and no placeholder is left (Task 6): no
        // body that compiles, and no answer made up without the platform call.
        let (found, rest) = placeholders(&code_lines(&source("src/pio/windows.rs")));
        assert_eq!(
            rest.trim(),
            "pub use windows_io::*;\n#[path = \"windows_io.rs\"]\nmod windows_io;\n\
             pub use windows_security::*;\n#[path = \"windows_security.rs\"]\n\
             mod windows_security;\n\
             pub use windows_dir::*;\n#[path = \"windows_dir.rs\"]\nmod windows_dir;\n\
             pub use windows_durable::*;\n#[path = \"windows_durable.rs\"]\n\
             mod windows_durable;",
            "code outside the placeholders"
        );
        assert!(found.is_empty(), "{found:?}");
        // This module no longer stops the build on Windows: only a platform that is neither Unix
        // nor Windows stops it.
        let pio = source("src/pio.rs");
        let (found, _) = placeholders(&code_lines(pio.split("#[cfg(test)]").next().unwrap()));
        assert_eq!(
            found,
            ["rdy_store::pio has no implementation for this platform (FW-XP1 D-1RXEA)"]
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
        // Opens, entry kinds and listings in an open directory are built (Task 5a), and so are
        // creates and removals (Task 5b) and renames (Task 5c): pio/windows_dir.rs opens and makes
        // each entry relative to the directory handle, never following a reparse point, makes it
        // with the private descriptor, removes the object it opened through that handle, lists
        // the directory through a new open of its handle, and renames by path while a pin that
        // does not share delete holds the directory, checking its path before and after, and
        // starts a child in the directory's checked verbatim path while that pin is held (Task 5c,
        // ruling R1); it holds no stub body or placeholder before its tests.
        let dir = source("src/pio/windows_dir.rs");
        // its test hook is under #[cfg(test)] too: the code ends where its test module starts
        let dir = code_lines(dir.split("#[cfg(test)]\nmod tests {").next().unwrap());
        for word in [
            "unreachable!",
            "unimplemented!",
            "todo!",
            "Default::default",
        ] {
            assert!(!dir.contains(word), "pio/windows_dir.rs holds {word}");
        }
        assert!(placeholders(&dir).0.is_empty());
        for call in [
            "NtOpenFile",
            "RootDirectory: dir.as_raw_handle()",
            "OBJ_DONT_REPARSE",
            "FILE_OPEN_REPARSE_POINT",
            "FILE_ATTRIBUTE_REPARSE_POINT",
            "FileFullDirectoryRestartInfo",
            "FileFullDirectoryInfo",
            "CreateFileW",
            "NtCreateFile",
            "FILE_CREATE",
            "PrivateDescriptor::of_this_account",
            "SecurityDescriptor: sd",
            "FileDispositionInfoEx",
            "FILE_DISPOSITION_FLAG_POSIX_SEMANTICS",
            "move_entry(",
            "FILE_SHARE_READ | FILE_SHARE_WRITE;",
            "PinnedDir::of(dir)",
            "cmd.current_dir(OsString::from_wide(&pinned.verbatim))",
            "Ok(StartPin { pinned })",
        ] {
            assert!(dir.contains(call), "pio/windows_dir.rs calls {call}");
        }
        // Directory flushes and write-through renames are built (Task 6): pio/windows_durable.rs
        // flushes a directory handle opened with backup semantics and the right to add a file,
        // renames with MoveFileExW's write-through flag and never its copy flag, and holds no
        // stub body or placeholder before its tests.
        let durable = source("src/pio/windows_durable.rs");
        let durable = code_lines(durable.split("#[cfg(test)]").next().unwrap());
        for word in [
            "unreachable!",
            "unimplemented!",
            "todo!",
            "Default::default",
        ] {
            assert!(
                !durable.contains(word),
                "pio/windows_durable.rs holds {word}"
            );
        }
        assert!(placeholders(&durable).0.is_empty());
        for call in [
            "FlushFileBuffers",
            "FILE_FLAG_BACKUP_SEMANTICS",
            "FILE_ADD_FILE | FILE_READ_ATTRIBUTES | SYNCHRONIZE",
            "open_entry(dir, &[], FLUSH_ACCESS, FILE_DIRECTORY_FILE)",
            "MoveFileExW(source.as_ptr(), target.as_ptr(), MOVE_FLAGS)",
            "MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH",
            "PinnedDir::of(&from_dir)",
            "PinnedDir::of(&to_dir)",
        ] {
            assert!(
                durable.contains(call),
                "pio/windows_durable.rs calls {call}"
            );
        }
        assert!(!durable.contains("MOVEFILE_COPY_ALLOWED"));
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
