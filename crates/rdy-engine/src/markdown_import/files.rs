//! The capture of a local Markdown source (D-19BZD, D-FSWQE, D-H5MKZ): one bounded inventory of
//! a source directory and the bytes of its included Markdown files and root `.rdyignore`, and
//! the source digest over them. Preview and import call the same [`capture`].
//!
//! The walk lists the source through directory handles (`rdy_store::pio`): each entry is named in
//! its open parent, so a path changed after its parent was opened cannot redirect it, and a
//! symbolic link, a junction or any other reparse point is refused, never followed. `.rdy` and
//! `.git` are always left out, then the root `.rdyignore` decides (the working copy's matcher and
//! limits). Only Markdown files (`.md`, `.markdown`, any case) are opened; every other regular
//! file is reported without being opened. A second walk reads each entry's identity and size
//! again, so a source that changes while it is captured refuses. Every bound is checked before
//! the collection it limits grows past it.
use super::parse::{check_path, collision, unsafe_path};
use super::{
    BLOCKS, FILE_BYTES, FOLDER_DEPTH, MAX_BLOCKS, MAX_FILE_BYTES, MAX_SOURCE_FILES,
    MAX_TOTAL_BYTES, MarkdownFile, SOURCE_FILES, TOTAL_BYTES, limit_error,
};
use crate::error::{E_BAD_INPUT, E_IO, RdyError};
use crate::wc::ignore::{self, IGNORE_FILE, Ignore, IgnoreAt, MAX_IGNORE_BYTES};
use rdy_model::tree::MAX_FOLDER_DEPTH;
use rdy_store::fileid::FileId;
use rdy_store::pio::{self, EntryKind};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// The digest's kind byte of an included Markdown file.
pub const KIND_MARKDOWN: u8 = 0;
/// The digest's kind byte of a reported (non-Markdown) regular file.
pub const KIND_REPORTED: u8 = 1;
/// The digest's kind byte of the root `.rdyignore` (an empty entry when there is none).
pub const KIND_IGNORE: u8 = 2;
/// The digest's kind byte of an included directory.
pub const KIND_DIR: u8 = 3;

/// The start of the digest's input: this tag and one zero byte.
const DIGEST_TAG: &[u8] = b"rdy-md-import-1\0";

/// A captured source. Every path is a logical path under the source root (`/` between
/// segments), and every list is sorted by its paths' UTF-8 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// The included directories, empty ones too.
    pub dirs: Vec<String>,
    /// The included Markdown files and their captured bytes.
    pub files: Vec<MarkdownFile>,
    /// The other included regular files: reported, never opened.
    pub reported: Vec<String>,
    /// The root `.rdyignore`'s bytes; empty when there is none.
    pub ignore: Vec<u8>,
    /// The BLAKE3 digest of the inventory, 64 lowercase hexadecimal characters.
    pub source_digest: String,
}

/// `E_BAD_INPUT` for a source entry that changed while it was captured.
pub(crate) fn changed(path: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("Markdown source changed while reading {path}"),
        "copy the source to a private directory and retry",
    )
}

/// One included entry as a walk saw it: its path and kind, and for a directory or a Markdown file
/// its identity and size (a directory's size is 0). A reported file is never opened, so it has
/// neither.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    path: String,
    kind: u8,
    id: Option<FileId>,
    len: u64,
}

/// How much a walk has taken: files (Markdown and reported), directories, and captured bytes.
#[derive(Default)]
struct Budget {
    files: usize,
    dirs: usize,
    bytes: usize,
}

/// Why a directory listing stopped.
enum Stop {
    Io(io::Error),
    Refused(RdyError),
}

impl From<io::Error> for Stop {
    fn from(e: io::Error) -> Stop {
        Stop::Io(e)
    }
}

/// Captures the source directory `root` (the caller resolved it; `root` itself is opened as
/// given): its inventory, the bytes of its Markdown files and `.rdyignore`, and its digest.
///
/// Refuses: an ignore file that is not a regular file, cannot be read, is not UTF-8, or is over
/// 65,536 bytes or 1,024 patterns (`E_CONFIG`, the working copy's refusals); an entry that is a
/// link or reparse point, not a regular file or directory, or not one plain UTF-8 name
/// (`E_BAD_INPUT`, the unsafe source path); a bound of the Markdown import (`E_LIMIT`); two paths
/// that are one under case folding (`E_BAD_INPUT`); and a source that changed between the walks
/// (`E_BAD_INPUT`).
pub fn capture(root: &Path) -> Result<Capture, RdyError> {
    let dir = pio::open_dir(root).map_err(|e| {
        RdyError::new(
            E_IO,
            format!("{}: {e}", root.display()),
            "check the path and permissions",
        )
    })?;
    let (ignore_bytes, ignore_seen) = read_ignore(&dir, true)?;
    let rules = Ignore::from_bytes(ignore_bytes.clone())?;
    let mut budget = Budget {
        bytes: ignore_bytes.len(),
        ..Budget::default()
    };
    let mut texts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let seen = walk(&dir, &rules, &mut budget, Some(&mut texts))?;
    between_passes();
    // the second walk: every entry again, with the same identity and size
    let (_, ignore_again) = read_ignore(&dir, false)?;
    if ignore_again != ignore_seen {
        return Err(changed(IGNORE_FILE));
    }
    let again = walk(&dir, &rules, &mut Budget::default(), None)?;
    if let Some(path) = first_difference(&seen, &again) {
        return Err(changed(path));
    }
    refuse_collisions(&seen)?;
    let source_digest = digest(&seen, &texts, &ignore_bytes);
    let mut dirs = vec![];
    let mut reported = vec![];
    for s in seen {
        match s.kind {
            KIND_DIR => dirs.push(s.path),
            KIND_REPORTED => reported.push(s.path),
            _ => {}
        }
    }
    let files = texts
        .into_iter()
        .map(|(path, bytes)| MarkdownFile { path, bytes })
        .collect();
    Ok(Capture {
        dirs,
        files,
        reported,
        ignore: ignore_bytes,
        source_digest,
    })
}

/// The root `.rdyignore`'s identity and size, when there is one.
type IgnoreSeen = Option<(FileId, u64)>;

/// The root `.rdyignore` of the open directory `dir`: its bytes (when `read`) and its identity
/// and size; no bytes and `None` when there is none. A link, a directory or any other
/// non-regular file there is the working copy's `E_CONFIG` refusal, as are a failed read and a
/// file over 65,536 bytes.
fn read_ignore(dir: &File, read: bool) -> Result<(Vec<u8>, IgnoreSeen), RdyError> {
    let name = OsStr::new(IGNORE_FILE);
    match pio::entry_kind_at(dir, name) {
        Ok(EntryKind::File) => {}
        Ok(_) => return Err(ignore::not_regular()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((vec![], None)),
        Err(e) => return Err(ignore::read_error(&e)),
    }
    let f = pio::open_read_at(dir, name).map_err(|e| {
        if pio::is_link_refusal(&e) {
            ignore::not_regular()
        } else if e.kind() == io::ErrorKind::NotFound {
            changed(IGNORE_FILE)
        } else {
            ignore::read_error(&e)
        }
    })?;
    let meta = f.metadata().map_err(|e| ignore::read_error(&e))?;
    if !meta.is_file() {
        return Err(ignore::not_regular());
    }
    if meta.len() > MAX_IGNORE_BYTES as u64 {
        return Err(ignore::over_bytes());
    }
    let id = FileId::of_file(&f).map_err(|e| ignore::read_error(&e))?;
    if !read {
        return Ok((vec![], Some((id, meta.len()))));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    (&f).take(MAX_IGNORE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ignore::read_error(&e))?;
    if bytes.len() > MAX_IGNORE_BYTES {
        return Err(ignore::over_bytes());
    }
    let after = f.metadata().map_err(|e| ignore::read_error(&e))?.len();
    if bytes.len() as u64 != meta.len() || after != meta.len() {
        return Err(changed(IGNORE_FILE));
    }
    Ok((bytes, Some((id, meta.len()))))
}

/// `path/name`, or `name` at the root.
fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}/{name}")
    }
}

/// Whether a name is always left out: `.rdy` and `.git`, in any case.
fn excluded(name: &str) -> bool {
    name.eq_ignore_ascii_case(".rdy") || name.eq_ignore_ascii_case(".git")
}

/// Whether a file name is a Markdown file's: `.md` or `.markdown` after a stem, in any case.
fn is_markdown(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".md", ".markdown"]
        .iter()
        .any(|ext| lower.len() > ext.len() && lower.ends_with(ext))
}

/// The refusal of a failed call at the entry `path`: a vanished entry changed during the
/// capture; any other failure is `E_IO`.
fn entry_error(path: &str, e: &io::Error) -> RdyError {
    if e.kind() == io::ErrorKind::NotFound {
        return changed(path);
    }
    RdyError::new(
        E_IO,
        format!("{path}: {e}"),
        "check the path and permissions",
    )
}

/// The refusal of a failed open at the entry `path`: a link, a reparse point or a changed kind
/// there is an unsafe source path.
fn open_error(path: &str, e: &io::Error) -> RdyError {
    if pio::is_link_refusal(e) {
        return unsafe_path(path);
    }
    entry_error(path, e)
}

/// Walks the source under the open root `root`, depth first through directory handles, and
/// returns every included entry sorted by path. With `texts`, each Markdown file's bytes are
/// read into it (the capture); without, each is only opened for its identity and size (the
/// second walk).
fn walk(
    root: &File,
    rules: &Ignore,
    budget: &mut Budget,
    mut texts: Option<&mut BTreeMap<String, Vec<u8>>>,
) -> Result<Vec<Seen>, RdyError> {
    let mut seen = vec![];
    let top = root.try_clone().map_err(|e| entry_error(".", &e))?;
    let mut stack: Vec<(File, String, IgnoreAt)> = vec![(top, String::new(), rules.root())];
    while let Some((dir, path, at)) = stack.pop() {
        let listed = list(&dir, &path, &at, rules, budget)?;
        for (name, kind) in listed {
            let rel = join(&path, &name);
            match kind {
                EntryKind::Dir => {
                    let sub = pio::open_dir_at(&dir, OsStr::new(&name))
                        .map_err(|e| open_error(&rel, &e))?;
                    let id = FileId::of_file(&sub).map_err(|e| entry_error(&rel, &e))?;
                    seen.push(Seen {
                        path: rel.clone(),
                        kind: KIND_DIR,
                        id: Some(id),
                        len: 0,
                    });
                    let inner = rules.enter(&at, &name);
                    stack.push((sub, rel, inner));
                }
                _ if is_markdown(&name) => {
                    let read = texts.is_some();
                    let (id, len, bytes) = markdown(&dir, &name, &rel, budget, read)?;
                    seen.push(Seen {
                        path: rel.clone(),
                        kind: KIND_MARKDOWN,
                        id: Some(id),
                        len,
                    });
                    if let Some(t) = texts.as_deref_mut() {
                        t.insert(rel, bytes);
                    }
                }
                _ => seen.push(Seen {
                    path: rel,
                    kind: KIND_REPORTED,
                    id: None,
                    len: 0,
                }),
            }
        }
    }
    seen.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(seen)
}

/// The included entries of the open directory `dir` at `path`: their names and kinds (a
/// directory or a regular file), each bound counted as it is listed. An excluded or ignored entry
/// is skipped before anything else is checked; the root's `.rdyignore` is read on its own.
fn list(
    dir: &File,
    path: &str,
    at: &IgnoreAt,
    rules: &Ignore,
    budget: &mut Budget,
) -> Result<Vec<(String, EntryKind)>, RdyError> {
    let mut listed = vec![];
    let r = pio::for_each_entry::<Stop>(dir, |native| {
        let Some(name) = native.to_str() else {
            let shown = join(path, &native.to_string_lossy());
            return Err(Stop::Refused(unsafe_path(&shown)));
        };
        if (path.is_empty() && name == IGNORE_FILE) || excluded(name) {
            return Ok(());
        }
        let rel = join(path, name);
        let kind =
            pio::entry_kind_at(dir, native).map_err(|e| Stop::Refused(entry_error(&rel, &e)))?;
        if rules.ignores(at, name, kind == EntryKind::Dir) {
            return Ok(());
        }
        let segments = check_path(&rel).map_err(Stop::Refused)?;
        match kind {
            EntryKind::Dir => {
                if segments > MAX_FOLDER_DEPTH {
                    return Err(Stop::Refused(limit_error(FOLDER_DEPTH)));
                }
                budget.dirs += 1;
                // each directory becomes a folder block
                if budget.dirs > MAX_BLOCKS {
                    return Err(Stop::Refused(limit_error(BLOCKS)));
                }
            }
            EntryKind::File => {
                budget.files += 1;
                if budget.files > MAX_SOURCE_FILES {
                    return Err(Stop::Refused(limit_error(SOURCE_FILES)));
                }
            }
            EntryKind::Link | EntryKind::Other => return Err(Stop::Refused(unsafe_path(&rel))),
        }
        listed.push((name.to_string(), kind));
        Ok(())
    });
    match r {
        Ok(()) => Ok(listed),
        Err(Stop::Refused(e)) => Err(e),
        Err(Stop::Io(e)) => Err(entry_error(if path.is_empty() { "." } else { path }, &e)),
    }
}

/// Opens the Markdown file `name` of the open directory `dir` (never through a link): its
/// identity, its size and, when `read`, its bytes. Its size is bounded before it is read, and
/// the read itself stops past the bound; a size that moves while it is read is a change.
fn markdown(
    dir: &File,
    name: &str,
    rel: &str,
    budget: &mut Budget,
    read: bool,
) -> Result<(FileId, u64, Vec<u8>), RdyError> {
    let f = pio::open_read_at(dir, OsStr::new(name)).map_err(|e| open_error(rel, &e))?;
    let meta = f.metadata().map_err(|e| entry_error(rel, &e))?;
    if !meta.is_file() {
        return Err(changed(rel));
    }
    let len = meta.len();
    if len > MAX_FILE_BYTES as u64 {
        return Err(limit_error(FILE_BYTES));
    }
    let id = FileId::of_file(&f).map_err(|e| entry_error(rel, &e))?;
    if !read {
        return Ok((id, len, vec![]));
    }
    if budget.bytes as u64 + len > MAX_TOTAL_BYTES as u64 {
        return Err(limit_error(TOTAL_BYTES));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    (&f).take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| entry_error(rel, &e))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(limit_error(FILE_BYTES));
    }
    let after = f.metadata().map_err(|e| entry_error(rel, &e))?.len();
    if bytes.len() as u64 != len || after != len {
        return Err(changed(rel));
    }
    budget.bytes += bytes.len();
    Ok((id, len, bytes))
}

/// The first path at which two walks differ (an entry only one holds, or another kind, identity
/// or size), or `None` when they agree. Both are sorted by path.
fn first_difference<'a>(a: &'a [Seen], b: &'a [Seen]) -> Option<&'a str> {
    for i in 0..a.len().max(b.len()) {
        match (a.get(i), b.get(i)) {
            (Some(x), Some(y)) if x == y => {}
            (Some(x), Some(y)) => {
                return Some(if x.path.as_bytes() <= y.path.as_bytes() {
                    &x.path
                } else {
                    &y.path
                });
            }
            (Some(x), None) | (None, Some(x)) => return Some(&x.path),
            (None, None) => return None,
        }
    }
    None
}

/// Refuses two included paths that are one under case folding: the first and second in path
/// order.
fn refuse_collisions(seen: &[Seen]) -> Result<(), RdyError> {
    let mut folded: BTreeMap<String, &str> = BTreeMap::new();
    for s in seen {
        if let Some(first) = folded.insert(s.path.to_lowercase(), &s.path) {
            return Err(collision(first, &s.path));
        }
    }
    Ok(())
}

/// The source digest: BLAKE3 over the tag, then each entry in path order (the `.rdyignore` entry
/// among them, empty when there is none) as its kind byte, its path's length and bytes, and its
/// content's length and bytes; each length an unsigned 64-bit little-endian integer. Only a
/// Markdown file and the ignore file have content.
fn digest(seen: &[Seen], texts: &BTreeMap<String, Vec<u8>>, ignore: &[u8]) -> String {
    let mut entries: Vec<(u8, &str, &[u8])> = seen
        .iter()
        .map(|s| {
            let content: &[u8] = match s.kind {
                KIND_MARKDOWN => texts.get(&s.path).map_or(&[], Vec::as_slice),
                _ => &[],
            };
            (s.kind, s.path.as_str(), content)
        })
        .collect();
    entries.push((KIND_IGNORE, IGNORE_FILE, ignore));
    entries.sort_by(|a, b| a.1.as_bytes().cmp(b.1.as_bytes()));
    let mut h = blake3::Hasher::new();
    h.update(DIGEST_TAG);
    for (kind, path, content) in entries {
        h.update(&[kind]);
        h.update(&(path.len() as u64).to_le_bytes());
        h.update(path.as_bytes());
        h.update(&(content.len() as u64).to_le_bytes());
        h.update(content);
    }
    h.finalize().to_hex().to_string()
}

#[cfg(feature = "test-support")]
thread_local! {
    static BETWEEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Tests only: the next capture on this thread runs `f` after its first walk and before its
/// second, so a test can change the source in between. It runs once.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn between_passes_for_tests(f: Box<dyn FnOnce()>) {
    BETWEEN.with(|c| *c.borrow_mut() = Some(f));
}

/// Runs the hook a test armed, if any.
fn between_passes() {
    #[cfg(feature = "test-support")]
    {
        let f = BETWEEN.with(|c| c.borrow_mut().take());
        if let Some(f) = f {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(path: &str, kind: u8) -> Seen {
        Seen {
            path: path.to_string(),
            kind,
            id: None,
            len: 0,
        }
    }

    /// Any file system: two paths that fold to one refuse, named in path order; Unicode folds.
    #[test]
    fn m4bc4_case_collisions_refuse_in_path_order() {
        let e = refuse_collisions(&[
            seen("Docs", KIND_DIR),
            seen("a.md", KIND_MARKDOWN),
            seen("docs", KIND_DIR),
        ])
        .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "Markdown source paths collide: Docs, docs",
                "rename one source path and update its links"
            )
        );
        let e = refuse_collisions(&[
            seen("ÜBER.md", KIND_MARKDOWN),
            seen("über.md", KIND_REPORTED),
        ])
        .unwrap_err();
        assert_eq!(e.message, "Markdown source paths collide: ÜBER.md, über.md");
        refuse_collisions(&[seen("a.md", KIND_MARKDOWN), seen("a.md.txt", KIND_REPORTED)]).unwrap();
    }

    /// The first differing path of two walks: an added, removed or changed entry.
    #[test]
    fn m4bc4_the_first_difference_names_its_path() {
        let a = [seen("a.md", 0), seen("d", 3), seen("n.txt", 1)];
        let added = [
            seen("a.md", 0),
            seen("c.md", 0),
            seen("d", 3),
            seen("n.txt", 1),
        ];
        let removed = [seen("a.md", 0), seen("n.txt", 1)];
        let mut grown = a.clone();
        grown[0].len = 9;
        assert_eq!(first_difference(&a, &a), None);
        assert_eq!(first_difference(&a, &added), Some("c.md"));
        assert_eq!(first_difference(&a, &removed), Some("d"));
        assert_eq!(first_difference(&a, &a[..2]), Some("n.txt"));
        assert_eq!(first_difference(&a, &grown), Some("a.md"));
    }
}
