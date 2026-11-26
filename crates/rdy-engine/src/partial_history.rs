//! A project whose history is still arriving (D-GGTYA, D-4PV25): a clone that took the canon
//! state of its root first. The store's history ref (`meta/sync/<remote>/history`,
//! `rdy_store::sync_state::SyncHistory`) names the frontier and the exact record count `<m>`;
//! this module computes how many of those records are held (`<n>`) and gives every surface the
//! one refusal and the one warning while the history is incomplete.
//!
//! - [`ReadState::history_progress`] reads the progress once per state: the listed keys the
//!   store holds plus the records each staged `.batch` file's trailer states, read with no
//!   payload hashed. Nothing is stored: a read appends no ref.
//! - [`ReadState::require_complete_history`] is E_HISTORY_PENDING while the history is
//!   incomplete: every call that needs records behind the frontier asks it first.
//! - [`ReadState::history_warning`] is the one W_HISTORY_PENDING a read that omits older
//!   history carries.
//!
//! The checked key file (`.rdy/sync/<remote>/history/keys`, [`HistoryKeys`]) lists the
//! canon history's keys at the frontier, in export order. It is a closed JSON value, at most
//! [`MAX_KEY_FILE_BYTES`] bytes and [`MAX_KEYS`] keys, read without following a link.
use crate::error::*;
use crate::{RdyError, ReadState};
use rdy_model::{Hash, RepoId};
use rdy_store::StoreReader;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{MAX_HISTORY_RECORDS, SyncHistory};
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The most bytes of a history key file.
pub const MAX_KEY_FILE_BYTES: u64 = 2_147_483_648;
/// The most keys a history key file lists: a history ref's record bound.
pub const MAX_KEYS: u64 = MAX_HISTORY_RECORDS;
/// The fix of E_HISTORY_PENDING and W_HISTORY_PENDING.
pub const HISTORY_PENDING_FIX: &str = "wait for the clone to finish, or run rdy pull to resume it";

/// The message of E_HISTORY_PENDING with `held` of `total` records held.
pub fn history_pending_message(held: u64, total: u64) -> String {
    format!("the project's history is still arriving ({held} of {total} records)")
}

/// The message of W_HISTORY_PENDING with `held` of `total` records held.
pub fn history_warning_message(held: u64, total: u64) -> String {
    format!("history is still arriving ({held} of {total} records)")
}

/// How much of a project's history it holds: its frontier, whether it is complete, and how many
/// of its `total` records are held. `total` is the history ref's `records`; a complete history
/// holds all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HistoryProgress {
    #[serde(serialize_with = "hex_hash")]
    pub frontier: Hash,
    pub complete: bool,
    pub held: u64,
    pub total: u64,
}

fn hex_hash<S: serde::Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

impl HistoryProgress {
    /// E_HISTORY_PENDING of this progress.
    pub fn refusal(&self) -> RdyError {
        RdyError::new(
            E_HISTORY_PENDING,
            history_pending_message(self.held, self.total),
            HISTORY_PENDING_FIX,
        )
    }

    /// W_HISTORY_PENDING of this progress.
    pub fn warning(&self) -> rdy_schema::Issue {
        rdy_schema::Issue {
            severity: rdy_schema::Severity::Warning,
            code: W_HISTORY_PENDING,
            block: None,
            message: history_warning_message(self.held, self.total),
            fix: HISTORY_PENDING_FIX.into(),
        }
    }
}

/// The history a state captured with its log end: the history ref of the store (the incomplete
/// one, else a complete one, by remote), and its progress, computed on first read.
pub(crate) struct HistoryCell {
    captured: Result<Option<(String, SyncHistory)>, RdyError>,
    progress: OnceLock<Result<Option<HistoryProgress>, RdyError>>,
    /// Whether the state reads the full history (a full check's state, over
    /// `StoreReader::full_history_reader`): its walks have no frontier, while the incomplete
    /// ref and its progress stay as captured.
    full: bool,
}

impl Default for HistoryCell {
    fn default() -> Self {
        HistoryCell {
            captured: Ok(None),
            progress: OnceLock::new(),
            full: false,
        }
    }
}

impl HistoryCell {
    /// The history refs of `reader` as they stand: its incomplete history, else its first
    /// complete one. A damaged ref is kept as the error every history read returns.
    pub(crate) fn capture(reader: &StoreReader) -> HistoryCell {
        let captured = reader
            .histories()
            .map(|all| {
                let partial = all.iter().find(|(_, h)| !h.complete).cloned();
                partial.or_else(|| all.into_iter().next())
            })
            .map_err(RdyError::from);
        HistoryCell {
            captured,
            progress: OnceLock::new(),
            full: reader.is_full_history(),
        }
    }
}

thread_local! {
    static KEY_FILE_OPENS: Cell<u64> = const { Cell::new(0) };
    static STAGED_PAYLOAD_HASHES: Cell<u64> = const { Cell::new(0) };
}

/// For tests: how many history key files this thread opened so far.
#[doc(hidden)]
pub fn key_file_opens_for_tests() -> u64 {
    KEY_FILE_OPENS.with(Cell::get)
}

/// For tests: how many staged batch payloads this thread hashed so far (a full recheck hashes
/// each one; a progress read hashes none).
#[doc(hidden)]
pub fn staged_payload_hashes_for_tests() -> u64 {
    STAGED_PAYLOAD_HASHES.with(Cell::get)
}

/// The directory of remote `remote`'s history files: `.rdy/sync/<remote>/history`.
pub fn history_dir(root: &Path, remote: &str) -> PathBuf {
    root.join(".rdy").join("sync").join(remote).join("history")
}

/// The checked key file of remote `remote`: `.rdy/sync/<remote>/history/keys`.
pub fn key_file(root: &Path, remote: &str) -> PathBuf {
    history_dir(root, remote).join("keys")
}

/// The checked key file: the canon history's keys at `frontier` of repo `repo_id`, in the
/// history's export order. Closed: an unknown member is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryKeys {
    #[serde(with = "repo_hex")]
    pub repo_id: RepoId,
    #[serde(with = "rdy_store::transition::hex::one")]
    pub frontier: Hash,
    #[serde(deserialize_with = "bounded_keys")]
    pub keys: Vec<ObjectKey>,
}

mod repo_hex {
    use rdy_model::RepoId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(r: &RepoId, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(r)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RepoId, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The keys of a key file, at most [`MAX_KEYS`]: the count is checked before each key is kept,
/// so no space is reserved for more.
fn bounded_keys<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ObjectKey>, D::Error> {
    struct Keys;
    impl<'de> Visitor<'de> for Keys {
        type Value = Vec<ObjectKey>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "at most {MAX_KEYS} object keys")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out: Vec<ObjectKey> = Vec::new();
            while let Some(k) = seq.next_element::<ObjectKey>()? {
                if out.len() as u64 >= MAX_KEYS {
                    return Err(de::Error::custom(format!(
                        "the history key file lists more than {MAX_KEYS} keys"
                    )));
                }
                out.try_reserve(1)
                    .map_err(|_| de::Error::custom("the history keys do not fit in memory"))?;
                out.push(k);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Keys)
}

/// The refusal of a key file that breaks a rule.
fn bad_keys(path: &Path, why: impl std::fmt::Display) -> RdyError {
    RdyError::new(
        E_BAD_RECORD,
        format!("{}: {why}", path.display()),
        HISTORY_PENDING_FIX,
    )
}

fn io_at(path: &Path, e: &std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check the path, its permissions and disk space",
    )
}

/// Reads and checks the key file at `path` against `history`: a regular file (a link is never
/// followed) of at most [`MAX_KEY_FILE_BYTES`] bytes, the closed value with at most
/// [`MAX_KEYS`] keys, each a portable kind, all distinct, naming the history's repo and
/// frontier, and exactly `history.records` of them.
pub fn read_key_file(path: &Path, history: &SyncHistory) -> Result<HistoryKeys, RdyError> {
    KEY_FILE_OPENS.with(|c| c.set(c.get() + 1));
    let f = rdy_store::pio::open_no_follow(std::fs::OpenOptions::new().read(true), path)
        .map_err(|e| io_at(path, &e))?;
    let meta = f.metadata().map_err(|e| io_at(path, &e))?;
    if !meta.is_file() {
        return Err(bad_keys(path, "is not a file"));
    }
    if meta.len() > MAX_KEY_FILE_BYTES {
        return Err(RdyError::new(
            E_LIMIT,
            format!(
                "{}: the history key file exceeds {MAX_KEY_FILE_BYTES} bytes",
                path.display()
            ),
            HISTORY_PENDING_FIX,
        ));
    }
    let keys: HistoryKeys = serde_json::from_reader(std::io::BufReader::new(f)).map_err(|e| {
        if e.is_io() {
            RdyError::new(
                E_IO,
                format!("{}: {e}", path.display()),
                "check the path, its permissions and disk space",
            )
        } else if e.to_string().contains("more than") {
            RdyError::new(
                E_LIMIT,
                format!("{}: {e}", path.display()),
                HISTORY_PENDING_FIX,
            )
        } else {
            bad_keys(path, e)
        }
    })?;
    check_keys(path, &keys, history)?;
    Ok(keys)
}

/// The rules of a key file's value against `history` (see [`read_key_file`]).
fn check_keys(path: &Path, keys: &HistoryKeys, history: &SyncHistory) -> Result<(), RdyError> {
    if keys.repo_id != history.repo_id {
        return Err(bad_keys(
            path,
            format_args!(
                "names repo {}, but the history is of repo {}",
                keys.repo_id, history.repo_id
            ),
        ));
    }
    if keys.frontier != history.frontier {
        return Err(bad_keys(
            path,
            format_args!(
                "names frontier {}, but the history's frontier is {}",
                keys.frontier, history.frontier
            ),
        ));
    }
    let mut seen: HashSet<ObjectKey> = HashSet::new();
    seen.try_reserve(keys.keys.len())
        .map_err(|_| bad_keys(path, "the history keys do not fit in memory"))?;
    for (i, k) in keys.keys.iter().enumerate() {
        if !rdy_store::object_refs::is_portable(k.kind) {
            return Err(bad_keys(
                path,
                format_args!("key {i} names kind {}, which is not portable", k.kind),
            ));
        }
        if !seen.insert(*k) {
            return Err(bad_keys(path, format_args!("key {i} ({k}) repeats")));
        }
    }
    if keys.keys.len() as u64 != history.records {
        return Err(bad_keys(
            path,
            format_args!(
                "lists {} keys, but the history counts {} records",
                keys.keys.len(),
                history.records
            ),
        ));
    }
    Ok(())
}

/// The staged batch files of `dir`, in name order: each regular `*.batch` file, never through a
/// link. An absent directory holds none.
pub fn staged_files(dir: &Path) -> Result<Vec<PathBuf>, RdyError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_at(dir, &e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io_at(dir, &e))?;
        let path = entry.path();
        let regular = entry.file_type().map_err(|e| io_at(&path, &e))?.is_file();
        if regular && path.extension().is_some_and(|x| x == "batch") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// The progress of `history` (of remote `remote`) in the project at `root`, over `reader`: for a
/// complete history, all of its records; else the listed keys the store holds plus the object
/// records each staged batch's trailer states, at most the total. No payload is read or hashed.
pub fn progress_of(
    reader: &StoreReader,
    root: &Path,
    remote: &str,
    history: &SyncHistory,
) -> Result<HistoryProgress, RdyError> {
    if history.complete {
        return Ok(HistoryProgress {
            frontier: history.frontier,
            complete: true,
            held: history.records,
            total: history.records,
        });
    }
    let keys = read_key_file(&key_file(root, remote), history)?;
    let mut held = 0u64;
    for k in &keys.keys {
        if reader.has_kind(k.kind, &k.hash)? {
            held += 1;
        }
    }
    for path in staged_files(&history_dir(root, remote))? {
        let mut f = rdy_store::pio::open_no_follow(std::fs::OpenOptions::new().read(true), &path)
            .map_err(|e| io_at(&path, &e))?;
        let totals = rdy_store::sync_stream::read_trailer_totals(&mut f).map_err(|e| {
            RdyError::new(e.code, format!("{}: {}", path.display(), e.message), e.fix)
        })?;
        held = held.saturating_add(totals.records);
    }
    Ok(HistoryProgress {
        frontier: history.frontier,
        complete: false,
        held: held.min(history.records),
        total: history.records,
    })
}

/// How many of the records `history`'s checked key file lists the store itself holds (staged
/// batches not counted): a full check of an incomplete history needs all of them.
pub(crate) fn store_held(
    reader: &StoreReader,
    root: &Path,
    remote: &str,
    history: &SyncHistory,
) -> Result<u64, RdyError> {
    let keys = read_key_file(&key_file(root, remote), history)?;
    let mut held = 0u64;
    for k in &keys.keys {
        if reader.has_kind(k.kind, &k.hash)? {
            held += 1;
        }
    }
    Ok(held)
}

/// The exact progress a full check counts: every listed key the store holds or a staged batch
/// holds, each once, after each staged batch passes its whole check (`check_batch`, which hashes
/// every payload). Partial fsck, a resume and the completion use it.
pub fn checked_progress_of(
    reader: &StoreReader,
    root: &Path,
    remote: &str,
    history: &SyncHistory,
) -> Result<HistoryProgress, RdyError> {
    if history.complete {
        return progress_of(reader, root, remote, history);
    }
    let keys = read_key_file(&key_file(root, remote), history)?;
    let listed: HashSet<ObjectKey> = keys.keys.iter().copied().collect();
    let mut found: HashSet<ObjectKey> = HashSet::new();
    for k in &keys.keys {
        if reader.has_kind(k.kind, &k.hash)? {
            found.insert(*k);
        }
    }
    for path in staged_files(&history_dir(root, remote))? {
        let f = rdy_store::pio::open_no_follow(std::fs::OpenOptions::new().read(true), &path)
            .map_err(|e| io_at(&path, &e))?;
        let report =
            rdy_store::sync_stream::check_batch(std::io::BufReader::new(f)).map_err(|e| {
                RdyError::new(e.code, format!("{}: {}", path.display(), e.message), e.fix)
            })?;
        STAGED_PAYLOAD_HASHES.with(|c| c.set(c.get() + report.records));
        if report.header.repo_id != history.repo_id {
            return Err(bad_keys(&path, "is a batch of another repo"));
        }
        for k in report.keys {
            if !listed.contains(&k) {
                return Err(bad_keys(
                    &path,
                    format_args!("holds {k}, which the history key file does not list"),
                ));
            }
            found.insert(k);
        }
    }
    Ok(HistoryProgress {
        frontier: history.frontier,
        complete: false,
        held: found.len() as u64,
        total: history.records,
    })
}

impl ReadState {
    /// The incomplete history this state captured with its log end, by remote: None for a
    /// project whose history is complete or that has no history ref.
    pub(crate) fn partial_history(&self) -> Result<Option<(&str, &SyncHistory)>, RdyError> {
        match &self.history.captured {
            Ok(Some((remote, h))) if !h.complete => Ok(Some((remote.as_str(), h))),
            Ok(_) => Ok(None),
            Err(e) => Err(e.clone()),
        }
    }

    /// The frontier where this state's canon walks stop: the incomplete history's, else None.
    /// A full check's state has none: it reads the whole history.
    pub(crate) fn history_frontier(&self) -> Result<Option<Hash>, RdyError> {
        if self.history.full {
            return Ok(None);
        }
        Ok(self.partial_history()?.map(|(_, h)| h.frontier))
    }

    /// How much of the project's history this state holds: None for a project with no history
    /// ref. Computed once per state (the key file read once, no staged payload hashed); a read
    /// appends nothing.
    pub fn history_progress(&self) -> Result<Option<HistoryProgress>, RdyError> {
        self.history
            .progress
            .get_or_init(|| match &self.history.captured {
                Ok(Some((remote, h))) => progress_of(&self.reader, &self.root, remote, h).map(Some),
                Ok(None) => Ok(None),
                Err(e) => Err(e.clone()),
            })
            .clone()
    }

    /// E_HISTORY_PENDING while the project's history is incomplete; else nothing. Every call
    /// that needs records behind the frontier asks this first, before it reads, writes or
    /// creates anything.
    pub fn require_complete_history(&self) -> Result<(), RdyError> {
        if self.partial_history()?.is_none() {
            return Ok(());
        }
        match self.history_progress()? {
            Some(p) if !p.complete => Err(p.refusal()),
            _ => Ok(()),
        }
    }

    /// The one W_HISTORY_PENDING of a reply that omits history behind the frontier: None while
    /// the history is complete.
    pub fn history_warning(&self) -> Result<Option<rdy_schema::Issue>, RdyError> {
        if self.partial_history()?.is_none() {
            return Ok(None);
        }
        Ok(self
            .history_progress()?
            .filter(|p| !p.complete)
            .map(|p| p.warning()))
    }
}
