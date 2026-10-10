//! The working-tree membership and reader (spec §12.1, `D-7BKK5`): which files under the root are
//! the working copy's, which are untracked, and the blocks they hold. A new entry needs no `id`,
//! `order` or `parent`: the reader gives it a fresh id and the file's doc as its parent.

use crate::io::KeyedLink;
use crate::tree_io::{self, Diagnostic};
use crate::wc::ignore::Ignore;
use rdy_model::{Block, BlockId, Hash};
use rdy_schema::Pack;
use std::collections::BTreeMap;
use std::path::Path;

/// The directory at the root that holds figures; nothing in it is a doc.
pub const BLOBS_DIR: &str = "_blobs";
/// The suffix of the other side of a conflicted file.
pub const THEIRS_SUFFIX: &str = ".theirs.yaml";
/// The other side of a conflicted `_folder.yaml`.
pub const FOLDER_THEIRS: &str = "_folder.theirs.yaml";
/// The largest file rdy reads as a member.
pub const MAX_MEMBER_BYTES: usize = 16 * 1024 * 1024;

/// What the working tree holds. Every path is relative to the root and `/`-separated; every list
/// is sorted by path, and `blocks` is in tree pre-order (files by path, entries in file order).
pub struct WcTree {
    pub blocks: Vec<WcBlock>,
    pub files: BTreeMap<String, WcFile>,
    pub untracked: Vec<Untracked>,
    pub theirs: Vec<String>,
    pub blobs: Vec<String>,
    pub diags: Vec<Diagnostic>,
}

/// One entry of a member file.
pub struct WcBlock {
    pub block: Block,
    /// Link targets written as keys, resolved later against the view.
    pub keyed: Vec<KeyedLink>,
    /// The member file that holds it.
    pub file: String,
    /// Where the entry starts (line, column).
    pub at: (usize, usize),
    /// Where each key of the entry starts (`fields.<name>` for a field).
    pub keys: BTreeMap<String, (usize, usize)>,
    /// The file gave no id: the reader assigned a fresh one.
    pub new: bool,
    /// The file gave an `order`; when false, `block.order` is empty.
    pub order_given: bool,
    /// The entry's index in its file.
    pub seq: usize,
}

/// A member file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WcFile {
    pub kind: FileKind,
    /// The doc or folder the file describes.
    pub head: BlockId,
    /// The head entry gave no id.
    pub new_head: bool,
    /// BLAKE3 of the file's bytes.
    pub hash: Hash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Doc,
    Folder,
}

/// A path the scan met but does not read, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Untracked {
    pub path: String,
    pub reason: String,
    /// Where in the file the reason is (line, column); `None` when it is the whole file.
    pub at: Option<(usize, usize)>,
    /// What to do about it: the fix of the diagnostic that untracked the file (a did-you-mean,
    /// say), else `rdy check <path>`.
    pub fix: String,
}

/// Reads the working tree under `root`. Never fails: what cannot be read is untracked or a
/// diagnostic.
pub fn read_tree(root: &Path, ignore: &Ignore, pack: &Pack) -> WcTree {
    let r = tree_io::read_wc(root, ignore, pack);
    let blocks: Vec<WcBlock> = r
        .loaded
        .into_iter()
        .map(|l| WcBlock {
            block: l.block,
            keyed: l.keyed,
            file: l.file,
            at: l.at,
            keys: l.keys,
            new: l.new,
            order_given: l.order_given,
            seq: l.seq,
        })
        .collect();
    let mut untracked = r.untracked;
    untracked.sort_by(|a, b| a.path.cmp(&b.path));
    let mut theirs = r.theirs;
    theirs.sort();
    let mut blobs = r.blobs;
    blobs.sort();
    WcTree {
        blocks,
        files: r.files,
        untracked,
        theirs,
        blobs,
        diags: r.diags,
    }
}
