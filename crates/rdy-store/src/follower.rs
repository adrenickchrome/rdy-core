//! The follower mark (D-AA4FD: "The mark: a file `.rdy/store/FOLLOWER`, outside the log, written
//! as a private replace with a directory sync, holds the source's repo id and the chain's last
//! trailer digest."). A store with a mark is a follower: a byte-identical prefix of its source's
//! log, and no normal open or write reaches it.
//!
//! The file is at most [`MAX_MARK_BYTES`] bytes: the line `rdy follower 1`, then one line of JSON
//! `{chain,files}`. `chain` is the existing `ChainEnd`, and it includes the marked end, which
//! tells a replay after a crash from a store at its mark. `files` is the source's project files
//! as sorted, unique `{path,hash}` rows, the hash over the file's content bytes. A holder has no
//! rows. Nothing else is accepted: this is a private machine file, not an editable config.
//!
//! The mark is checked against its log at the saved boundary: the repo, the end, the batch marker
//! and the manifest there. A later manifest may differ after an interrupted follow.
use crate::dump::{MAX_FILES, is_project_file};
use crate::dump_chain::{BatchMarker, ChainEnd};
use crate::error::{FollowerMarkFailure as Why, StoreError};
use crate::format::{self, DumpStore, MAX_MANIFEST_BYTES};
use crate::kinds::{self, BatchCommit, RefUpdate};
use crate::meta::{REF_REPO_ID, decode_repo_id};
use crate::segment::{Location, SegmentLog};
use crate::store::StoreLock;
use crate::{fsutil, pio};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

/// The mark's file name in the store directory.
pub const FOLLOWER_FILE: &str = "FOLLOWER";
/// The mark's first line, with its newline.
const HEADER: &[u8] = b"rdy follower 1\n";
/// The most bytes a mark holds (inclusive).
pub const MAX_MARK_BYTES: u64 = 262_144;
/// The most log frames the saved-boundary check reads (inclusive).
pub const MAX_CHECK_FRAMES: u64 = 100_000_000;

/// One project file of the source: its dump path and the hash of its content bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowerFile {
    pub path: String,
    #[serde(with = "crate::transition::hex::one")]
    pub hash: Hash,
}

/// A follower's mark: where its chain ends, and the source's project files there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowerMark {
    pub chain: ChainEnd,
    pub files: Vec<FollowerFile>,
}

fn refused(why: Why) -> StoreError {
    StoreError::FollowerMark(why)
}

/// `Follower` when the store at `dir` holds a valid mark, the mark's own refusal when it holds a
/// bad one; nothing for a store with none. Reads only. Every ordinary create and open runs it
/// before it plans an upgrade, opens an index or creates a sentinel.
pub fn refuse_follower(dir: &Path) -> Result<(), StoreError> {
    match read_mark(dir)? {
        Some(mark) => Err(StoreError::Follower(mark.chain.repo_id)),
        None => Ok(()),
    }
}

/// The mark of the store at `dir`, or the refusal of an explicit follower open with none.
pub(crate) fn require_mark(dir: &Path) -> Result<FollowerMark, StoreError> {
    read_mark(dir)?.ok_or(refused(Why::Missing))
}

/// The mark of the store at `dir`; None when it has none (a path that is no store has none). The
/// file is opened without following a link, and at most [`MAX_MARK_BYTES`] of it are read.
pub fn read_mark(dir: &Path) -> Result<Option<FollowerMark>, StoreError> {
    let path = dir.join(FOLLOWER_FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(_) => return Err(refused(Why::Unreadable)),
    };
    if !private_regular(&meta) {
        return Err(refused(Why::NotPrivate));
    }
    if meta.len() > MAX_MARK_BYTES {
        return Err(refused(Why::TooLarge));
    }
    let file = pio::open_no_follow(std::fs::OpenOptions::new().read(true), &path).map_err(|e| {
        refused(if pio::is_link_refusal(&e) {
            Why::NotPrivate
        } else {
            Why::Unreadable
        })
    })?;
    match file.metadata() {
        Ok(m) if private_regular(&m) => {}
        Ok(_) => return Err(refused(Why::NotPrivate)),
        Err(_) => return Err(refused(Why::Unreadable)),
    }
    let mut bytes = Vec::new();
    file.take(MAX_MARK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused(Why::Unreadable))?;
    if bytes.len() as u64 > MAX_MARK_BYTES {
        return Err(refused(Why::TooLarge));
    }
    parse(&bytes).map(Some)
}

/// Whether `meta` is one private regular file: not a link's and not shared by a second name.
fn private_regular(meta: &std::fs::Metadata) -> bool {
    pio::is_private_file(meta) && crate::fileid::links(meta) == 1
}

/// The mark in `bytes`, which is within the size bound.
fn parse(bytes: &[u8]) -> Result<FollowerMark, StoreError> {
    let Some(rest) = bytes.strip_prefix(HEADER) else {
        return Err(refused(Why::BadHeader));
    };
    // one line: the newline ends it, and there is no third line
    let Some(line) = rest.strip_suffix(b"\n").filter(|l| !l.contains(&b'\n')) else {
        return Err(refused(Why::BadJson));
    };
    let value: Value = serde_json::from_slice(line).map_err(|_| refused(Why::BadJson))?;
    let Some(top) = value.as_object() else {
        return Err(refused(Why::BadJson));
    };
    if top.keys().any(|k| k != "chain" && k != "files") {
        return Err(refused(Why::UnknownField));
    }
    let (Some(chain), Some(files)) = (top.get("chain"), top.get("files")) else {
        return Err(refused(Why::BadJson));
    };
    let chain = parse_chain(chain)?;
    let files = parse_files(files)?;
    Ok(FollowerMark { chain, files })
}

/// The chain end in `value`: the existing codec, with no field it would ignore.
fn parse_chain(value: &Value) -> Result<ChainEnd, StoreError> {
    let chain: ChainEnd = serde_json::from_value(value.clone()).map_err(|e| {
        refused(if e.to_string().contains("unknown field") {
            Why::UnknownField
        } else {
            Why::BadJson
        })
    })?;
    let again = serde_json::to_value(&chain).map_err(|_| refused(Why::BadJson))?;
    if !same_keys(value, &again) {
        return Err(refused(Why::UnknownField));
    }
    if let DumpStore::Features(m) = &chain.store_format {
        let json = m.to_json();
        if json.len() > MAX_MANIFEST_BYTES || format::StoreFeatures::parse(&json).is_err() {
            return Err(refused(Why::BadJson));
        }
    }
    Ok(chain)
}

/// Whether `a` holds no object key that `b` lacks, at any depth.
fn same_keys(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => a
            .iter()
            .all(|(k, v)| b.get(k).is_some_and(|w| same_keys(v, w))),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(v, w)| same_keys(v, w))
        }
        _ => true,
    }
}

/// The project file list in `value`: at most [`MAX_FILES`] rows of the dump's path grammar,
/// sorted by the path's bytes, each path once.
fn parse_files(value: &Value) -> Result<Vec<FollowerFile>, StoreError> {
    let Some(rows) = value.as_array() else {
        return Err(refused(Why::BadFiles));
    };
    if rows.len() > MAX_FILES as usize {
        return Err(refused(Why::BadFiles));
    }
    let mut out: Vec<FollowerFile> = Vec::with_capacity(rows.len());
    for row in rows {
        let file: FollowerFile = serde_json::from_value(row.clone()).map_err(|e| {
            refused(if e.to_string().contains("unknown field") {
                Why::UnknownField
            } else {
                Why::BadFiles
            })
        })?;
        if !is_project_file(&file.path)
            || out
                .last()
                .is_some_and(|p| p.path.as_bytes() >= file.path.as_bytes())
        {
            return Err(refused(Why::BadFiles));
        }
        out.push(file);
    }
    Ok(out)
}

/// The mark's bytes, as the writer makes them: the header, then one compact line of JSON.
fn encode(mark: &FollowerMark) -> Result<Vec<u8>, StoreError> {
    let mut bytes = HEADER.to_vec();
    serde_json::to_writer(&mut bytes, mark).map_err(|_| refused(Why::BadJson))?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_MARK_BYTES {
        return Err(refused(Why::TooLarge));
    }
    Ok(bytes)
}

/// Writes `mark` at `dir` through the private durable replace: a failure keeps the old valid mark
/// or the new valid mark, never a part of one and never none.
pub(crate) fn write_mark(dir: &Path, mark: &FollowerMark) -> Result<(), StoreError> {
    let bytes = encode(mark)?;
    fsutil::write_private_durable(&dir.join(FOLLOWER_FILE), &bytes).map_err(StoreError::Io)
}

/// A loader's write of the first mark of its staging store at `dir`, after `Ingest::finish` and
/// before any store opens over it. `lock` is the caller's lock of that store (`Locked` otherwise),
/// the store has no mark yet, and the chain's facts must hold against the sealed log.
pub fn write_seed_mark(
    dir: &Path,
    lock: &StoreLock,
    mark: &FollowerMark,
) -> Result<(), StoreError> {
    if !lock.guards(dir) {
        return Err(StoreError::Locked(dir.to_path_buf()));
    }
    refuse_follower(dir)?;
    let (log, _) =
        SegmentLog::open_read_only(&dir.join("segments"), crate::store::SEGMENT_MAX, dir, None)?;
    check_against_log(&log, mark)?;
    write_mark(dir, mark)
}

/// The refusal of a mark that ends past the log's end `log`.
pub(crate) fn past_end(mark: &FollowerMark, log: Location) -> StoreError {
    refused(Why::PastEnd {
        mark: mark.chain.end,
        log,
    })
}

/// One frame more of the saved-boundary check, of `n` so far: over the bound is its `E_LIMIT`.
fn count_frame(n: u64) -> Result<u64, StoreError> {
    let n = n + 1;
    if n > MAX_CHECK_FRAMES {
        return Err(StoreError::FollowerMarkCheckLimit);
    }
    Ok(n)
}

/// Test hook: the saved-boundary check's bound on `n` frames.
#[doc(hidden)]
pub fn check_frame_count_for_tests(n: u64) -> Result<(), StoreError> {
    count_frame(n - 1).map(drop)
}

/// Test hook: the follow path's replacement of a store's mark, with no check.
#[doc(hidden)]
pub fn replace_mark_for_tests(dir: &Path, mark: &FollowerMark) -> Result<(), StoreError> {
    write_mark(dir, mark)
}

/// Checks `mark` against `log`: the log's repo id, a saved end within the log's, the batch marker
/// that ends there, and the manifest in force there. Damage in the log stays the walk's own
/// error. A suffix past the saved end passes, whatever it holds.
pub(crate) fn check_against_log(log: &SegmentLog, mark: &FollowerMark) -> Result<(), StoreError> {
    let saved = mark.chain.end;
    let mut frames = 0u64;
    let mut repo = None;
    let mut manifest: Option<Vec<u8>> = None;
    let mut marker: Option<BatchMarker> = None;
    log.for_each_view::<StoreError>(
        Location {
            segment: 0,
            offset: 0,
        },
        |_, v, next| {
            frames = count_frame(frames)?;
            match (v.kind, v.payload) {
                (kinds::BATCH_COMMIT, Some(p)) if next == saved => {
                    marker = postcard::from_bytes::<BatchCommit>(p)
                        .ok()
                        .map(|c| BatchMarker::of_commit(&c));
                }
                (kinds::REF_UPDATE, Some(p)) => {
                    if let Ok(u) = postcard::from_bytes::<RefUpdate>(p) {
                        if u.name == REF_REPO_ID {
                            repo = decode_repo_id(&u.value);
                        } else if u.name == format::REF_STORE_FEATURES && next <= saved {
                            manifest = Some(u.value);
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        },
    )?;
    let Some(repo) = repo else {
        return Err(StoreError::Decode("the log holds no meta/repo_id".into()));
    };
    if repo != mark.chain.repo_id {
        return Err(refused(Why::Repo {
            mark: mark.chain.repo_id,
            log: repo,
        }));
    }
    if saved > log.end() {
        return Err(past_end(mark, log.end()));
    }
    if marker != Some(mark.chain.end_batch) {
        return Err(refused(Why::Marker));
    }
    let same = match &mark.chain.store_format {
        DumpStore::Features(m) => manifest.is_some_and(|v| v == m.to_json()),
        DumpStore::Legacy(_) => false,
    };
    if !same {
        return Err(refused(Why::StoreFormat));
    }
    Ok(())
}
