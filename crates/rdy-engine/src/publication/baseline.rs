//! Portable publication baselines (D-CDQF7, D-6WD17, D-W4YAE, D-MCH70, D-AMF3T): one profile
//! resolved at one tag, kept as a JSON value that another store can compare a candidate with.
//!
//! A baseline holds what the disclosure comparison reads of a resolution: ids, versions, hashes,
//! doc and folder paths kept as their parts (block id and name), the attachment rows (D-TBDA1),
//! the reference rows and the hidden ids (D-B2AC3). It also holds what the review reads of one
//! side (D-W4YAE): doc and folder titles, doc pages and places, and for each shown block its doc,
//! type, label, section, parent, order key, place and a hash of each compared part (D-MCH70). A
//! shown decision adds its derived status tag and its whole title (D-AMF3T). It holds no
//! excerpt, body or field value, and no timestamp, actor or rdy version.
//!
//! The bytes (D-6WD17): one JSON object, with `format` set to [`BASELINE_FORMAT`], encoded in RFC
//! 8785 form with safe integers. `baseline_hash` is BLAKE3 over the format name, a zero byte and
//! the canonical bytes of the object without `baseline_hash`. Each array has a closed order. A
//! reader ([`Baseline::read`]) checks the input bound before it parses, parses at a depth of at
//! most [`BASELINE_DEPTH_MAX`], refuses a duplicate key, an unknown or missing member and a
//! number or string outside the rules, encodes the object again, checks the canonical cap and
//! then the hash. Every reader failure is the one `E_PUBLICATION_BASELINE` text.
//!
//! A comparison against a baseline ([`compare`]) reads the candidate in its own store and the
//! baseline as a value. It takes no record from the baseline's store, so the baseline stays
//! usable when its source is gone. Its side view ([`BaselineReviewSide`]) gives labels, counts
//! and part hashes, and no reader text: a removed row has no excerpt and an updated row no
//! word diff there. Nothing here enters a selection hash, a selection identity or a doc digest.

use super::diff::{DisclosureDiff, Names, baseline_diff, removed_entries};
use super::references::{CheckStatus, Classification, ReferenceReport, ReferenceRow};
use super::review_json::PublicationReview;
use super::review_labels::figure_label;
use super::review_side::{
    BlockParts, DecisionFacts, PartName, PublicationReviewSide, ReaderReviewSide, ReviewRemovals,
    SAFE_INTEGER_MAX, SelectionFacts, ShownBlock, TreeFacts, rfc8785_safe_integers, text_fields,
};
use super::select::{
    AttachmentEntry, AttachmentVariant, DocEntry, Limits, Resolved, SelectionEntry, invalid_tree,
    limit, limits, resolve,
};
use super::source::{PublicationSource, ReadPublicationSource};
use crate::error::*;
use crate::materialize::Materialized;
use crate::reader::labels;
use crate::{ReadState, Scope};
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, Hash, RepoId, decision, tree};
use rdy_schema::publication::Audience;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The format name of a publication baseline, and the prefix of its hash input (D-6WD17).
pub const BASELINE_FORMAT: &str = "rdy-publication-baseline-1";

/// The most input bytes a baseline reader takes, checked before it parses: twice the canonical
/// cap, so stored JSON with spacing fits (D-6WD17).
pub const BASELINE_INPUT_MAX: usize = 268_435_456;

/// The most bytes the canonical encoding of a baseline holds (D-6WD17). It is the baseline's own
/// cap, apart from the selection text budget.
pub const BASELINE_CANONICAL_MAX: usize = 134_217_728;

/// The deepest nesting a baseline reader parses (D-6WD17): the top-level object is level 1.
pub const BASELINE_DEPTH_MAX: usize = 32;

/// The largest number a baseline holds: 2^53 - 1 (D-6WD17).
pub const BASELINE_INTEGER_MAX: u64 = SAFE_INTEGER_MAX;

// ---- caps ---------------------------------------------------------------------------------

/// The two byte caps one read or make applies: the input bound and the canonical cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaselineCaps {
    pub input: usize,
    pub canonical: usize,
}

impl Default for BaselineCaps {
    fn default() -> BaselineCaps {
        BaselineCaps {
            input: BASELINE_INPUT_MAX,
            canonical: BASELINE_CANONICAL_MAX,
        }
    }
}

#[cfg(feature = "test-support")]
thread_local! {
    static CAPS: std::cell::Cell<Option<BaselineCaps>> = const { std::cell::Cell::new(None) };
}

/// The caps this thread reads and makes baselines under: the fixed ones (a build with the
/// `test-support` feature lets a test replace them for its thread).
fn caps() -> BaselineCaps {
    #[cfg(feature = "test-support")]
    if let Some(c) = CAPS.with(std::cell::Cell::get) {
        return c;
    }
    BaselineCaps::default()
}

/// Tests only (the `test-support` feature): reads and makes baselines on this thread under `c`
/// until the guard drops, so a test can reach both byte caps with small values.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn override_baseline_caps_for_tests(c: BaselineCaps) -> BaselineCapsGuard {
    BaselineCapsGuard(CAPS.with(|x| x.replace(Some(c))))
}

/// Restores the caps [`override_baseline_caps_for_tests`] replaced when dropped.
#[cfg(feature = "test-support")]
#[doc(hidden)]
#[must_use = "the caps are lowered only while the guard lives"]
pub struct BaselineCapsGuard(Option<BaselineCaps>);

#[cfg(feature = "test-support")]
impl Drop for BaselineCapsGuard {
    fn drop(&mut self) {
        CAPS.with(|x| x.set(self.0));
    }
}

// ---- refusals -----------------------------------------------------------------------------

/// The refusal of every reader failure (D-CDQF7, D-6WD17).
fn not_verified() -> RdyError {
    RdyError::new(
        E_PUBLICATION_BASELINE,
        BASELINE_VERIFY_MESSAGE,
        BASELINE_VERIFY_FIX,
    )
}

/// The refusal of a binding that names another repo id, tag, profile version or selection
/// identity: it names the binding's tag and repo id.
fn binding_mismatch(tag: &str, repo_id: &RepoId) -> RdyError {
    RdyError::new(
        E_PUBLICATION_BASELINE,
        BASELINE_BINDING_MESSAGE
            .replace("<tag>", tag)
            .replace("<repo_id>", &repo_id.to_string()),
        BASELINE_BINDING_FIX,
    )
}

/// The refusal of a baseline from outside the candidate's lineage.
fn outside_lineage() -> RdyError {
    RdyError::new(
        E_PUBLICATION_BASELINE,
        BASELINE_LINEAGE_MESSAGE,
        BASELINE_LINEAGE_FIX,
    )
}

/// The refusal of a make whose scope is not a tag.
fn needs_tag() -> RdyError {
    RdyError::new(E_BAD_INPUT, BASELINE_TAG_MESSAGE, BASELINE_TAG_FIX)
}

// ---- the value ----------------------------------------------------------------------------

/// One part of a doc's or folder's path, root first: the block's id and its name as the
/// baseline's side showed it (a removed version's stub is named by its id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathPart {
    pub id: BlockId,
    pub name: String,
}

/// Where a baseline came from: its source's repo id, the fork origins that source's log
/// records, sorted, the tag and the tag's landing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineSource {
    pub repo_id: RepoId,
    pub fork_origins: Vec<RepoId>,
    pub tag: String,
    pub landing: Hash,
}

/// What a shown decision's entry adds (D-W4YAE, D-AMF3T): its whole title, null when a removal
/// removed it, and its derived status tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineDecision {
    pub title: Option<String>,
    pub status_tag: Option<String>,
}

/// One retained block (D-CDQF7) as its side showed it (D-W4YAE): its id, version and removal
/// request, its home doc, type, change label, nearest section, parent, order key, page place and
/// the hash of each compared part; a decision's title and status tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineEntry {
    pub id: BlockId,
    pub version: Hash,
    pub redaction: Option<Hash>,
    pub doc: Option<BlockId>,
    pub block_type: String,
    pub label: String,
    pub section: Option<BlockId>,
    pub parent: Option<BlockId>,
    pub order: String,
    pub position: Option<u64>,
    pub parts: BTreeMap<PartName, String>,
    pub decision: Option<BaselineDecision>,
}

/// One selected doc: its path parts, digest, title (null when a removal removed it), page and
/// place in the site's reading order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineDoc {
    pub id: BlockId,
    pub path: Vec<PathPart>,
    pub digest: Hash,
    pub title: Option<String>,
    pub page: Option<String>,
    pub position: Option<u64>,
}

/// One selected folder: its version, path parts and title (null when a removal removed it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineFolder {
    pub id: BlockId,
    pub version: Hash,
    pub path: Vec<PathPart>,
    pub title: Option<String>,
}

/// One attachment row (D-TBDA1): the comparison's row, the request that removed its blob, and
/// its figure's label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineAttachment {
    pub row: AttachmentEntry,
    pub redaction: Option<Hash>,
    pub label: String,
}

/// A verified publication baseline. Only [`Baseline::read`] and a make build one, so its hash
/// always covers its members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Baseline {
    source: BaselineSource,
    profile_id: Option<BlockId>,
    profile_version: Option<Hash>,
    audience: Audience,
    selection_hash: Hash,
    selection_identity: Hash,
    redactions: Vec<Hash>,
    entries: Vec<BaselineEntry>,
    docs: Vec<BaselineDoc>,
    folders: Vec<BaselineFolder>,
    hidden: Vec<BlockId>,
    attachments: Vec<BaselineAttachment>,
    references: Vec<ReferenceRow>,
    baseline_hash: Hash,
    /// Each entry's id, version and removal request, in id order: what the comparison reads.
    selection: Vec<SelectionEntry>,
    /// Each attachment row as the comparison reads it, in the baseline's order.
    rows: Vec<AttachmentEntry>,
}

/// What a host recorded about the previous publication a baseline must match (D-CDQF7): the
/// source's repo id, the tag, the profile version and the selection identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineBinding {
    pub repo_id: RepoId,
    pub tag: String,
    pub profile_version: Option<Hash>,
    pub selection_identity: Hash,
}

impl Baseline {
    /// The source facts.
    pub fn source(&self) -> &BaselineSource {
        &self.source
    }

    /// The source's repo id.
    pub fn repo_id(&self) -> RepoId {
        self.source.repo_id
    }

    /// The tag the baseline was made from.
    pub fn tag(&self) -> &str {
        &self.source.tag
    }

    /// The tag's landing.
    pub fn landing(&self) -> Hash {
        self.source.landing
    }

    /// The profile id; `None` for the implicit profile.
    pub fn profile_id(&self) -> Option<&BlockId> {
        self.profile_id.as_ref()
    }

    /// The profile version; `None` for the implicit profile.
    pub fn profile_version(&self) -> Option<Hash> {
        self.profile_version
    }

    /// The profile's audience.
    pub fn audience(&self) -> Audience {
        self.audience
    }

    pub fn selection_hash(&self) -> Hash {
        self.selection_hash
    }

    pub fn selection_identity(&self) -> Hash {
        self.selection_identity
    }

    /// The removal requests the baseline reflects, sorted.
    pub fn redactions(&self) -> &[Hash] {
        &self.redactions
    }

    /// Every retained block, in id order.
    pub fn entries(&self) -> &[BaselineEntry] {
        &self.entries
    }

    /// Every selected doc, in id order.
    pub fn docs(&self) -> &[BaselineDoc] {
        &self.docs
    }

    /// Every selected folder, in id order.
    pub fn folders(&self) -> &[BaselineFolder] {
        &self.folders
    }

    /// The ids of the blocks the profile's planned filter hid, in id order.
    pub fn hidden(&self) -> &[BlockId] {
        &self.hidden
    }

    /// Every attachment row, by home doc, block and variant, light first.
    pub fn attachments(&self) -> &[BaselineAttachment] {
        &self.attachments
    }

    /// Every reference row and link edge outside the selection, sorted as the comparison sorts
    /// them.
    pub fn references(&self) -> &[ReferenceRow] {
        &self.references
    }

    /// The baseline hash.
    pub fn baseline_hash(&self) -> Hash {
        self.baseline_hash
    }

    /// Each entry's id, version and removal request, in id order.
    pub(super) fn selection_entries(&self) -> &[SelectionEntry] {
        &self.selection
    }

    /// Each attachment row as the comparison reads it.
    pub(super) fn attachment_entries(&self) -> &[AttachmentEntry] {
        &self.rows
    }

    /// The canonical bytes of the whole baseline, `baseline_hash` included: what a file holds.
    pub fn to_bytes(&self) -> Vec<u8> {
        rfc8785_safe_integers(&self.to_value(true))
            .expect("a baseline holds only safe integers")
            .into_bytes()
    }

    /// The whole baseline as a JSON value, `baseline_hash` included.
    pub fn to_json(&self) -> Value {
        self.to_value(true)
    }

    /// Reads a baseline from `input` (D-6WD17): the input bound, a strict parse at a depth of
    /// at most [`BASELINE_DEPTH_MAX`], the closed members, orders and limits, the canonical cap
    /// and the hash, in that order. Any failure is `E_PUBLICATION_BASELINE`.
    pub fn read(input: &[u8]) -> Result<Baseline, RdyError> {
        let caps = caps();
        if input.len() > caps.input {
            return Err(not_verified());
        }
        let value = parse_strict(input).map_err(|_| not_verified())?;
        let mut baseline = from_value(&value, &limits()).map_err(|_| not_verified())?;
        let Value::Object(mut map) = value else {
            return Err(not_verified());
        };
        map.remove("baseline_hash");
        let canonical = rfc8785_safe_integers(&Value::Object(map)).map_err(|_| not_verified())?;
        if canonical.len() > caps.canonical {
            return Err(not_verified());
        }
        if hash_of(canonical.as_bytes()) != baseline.baseline_hash {
            return Err(not_verified());
        }
        baseline.derive();
        Ok(baseline)
    }

    /// Reads a baseline from the file at `path`: the file's length is checked against the input
    /// bound before a byte is read, and at most one byte more than the bound is ever read, so a
    /// file that grows during the read still refuses (D-6WD17). A file that cannot be opened or
    /// read is `E_IO`; the rest is [`Baseline::read`].
    pub fn read_file(path: &std::path::Path) -> Result<Baseline, RdyError> {
        use std::io::Read;
        let io = |e: std::io::Error| {
            RdyError::new(
                E_IO,
                format!("{}: {e}", path.display()),
                "check the path and its permissions",
            )
        };
        let cap = caps().input;
        let file = std::fs::File::open(path).map_err(io)?;
        let meta = file.metadata().map_err(io)?;
        if !meta.is_file() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{} is not a file", path.display()),
                "pass the file that rdy publication baseline wrote",
            ));
        }
        if meta.len() > cap as u64 {
            return Err(not_verified());
        }
        let mut bytes = Vec::new();
        file.take(cap as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        Baseline::read(&bytes)
    }

    /// Checks the hash again: the members still give the baseline hash.
    fn verify(&self) -> Result<(), RdyError> {
        let canonical = rfc8785_safe_integers(&self.to_value(false)).map_err(|_| not_verified())?;
        if canonical.len() > caps().canonical || hash_of(canonical.as_bytes()) != self.baseline_hash
        {
            return Err(not_verified());
        }
        Ok(())
    }

    /// Checks the host's record of the previous publication (D-CDQF7): its repo id, tag,
    /// profile version and selection identity must be the baseline's. A tag matches by its
    /// exact name or by version precedence (`1.0.0` names `v1.0.0`).
    pub fn check_binding(&self, binding: &BaselineBinding) -> Result<(), RdyError> {
        let same_tag = binding.tag == self.source.tag
            || matches!(
                (
                    crate::parse_version(&binding.tag),
                    crate::parse_version(&self.source.tag)
                ),
                (Ok(a), Ok(b)) if a.cmp_precedence(&b) == std::cmp::Ordering::Equal
            );
        if binding.repo_id != self.source.repo_id
            || !same_tag
            || binding.profile_version != self.profile_version
            || binding.selection_identity != self.selection_identity
        {
            return Err(binding_mismatch(&binding.tag, &binding.repo_id));
        }
        Ok(())
    }

    /// Fills the rows the comparison reads from the members.
    fn derive(&mut self) {
        self.selection = self
            .entries
            .iter()
            .map(|e| SelectionEntry {
                id: e.id.clone(),
                version: e.version,
                redaction: e.redaction,
            })
            .collect();
        self.rows = self.attachments.iter().map(|a| a.row.clone()).collect();
    }

    /// The folders as the comparison reads them: each folder's own entry.
    pub(super) fn folder_entries(&self) -> Vec<&SelectionEntry> {
        let by_id: BTreeMap<&BlockId, &SelectionEntry> =
            self.selection.iter().map(|e| (&e.id, e)).collect();
        self.folders
            .iter()
            .filter_map(|f| by_id.get(&f.id).copied())
            .collect()
    }

    /// The docs as the comparison reads them: each doc's id and digest.
    pub(super) fn doc_digests(&self) -> Vec<(&BlockId, Hash)> {
        self.docs.iter().map(|d| (&d.id, d.digest)).collect()
    }

    /// The retained blocks outside a doc: each entry that is no doc or folder and has no home
    /// doc, with its version and whether it is a removed version.
    pub(super) fn outside_docs(&self) -> BTreeMap<&BlockId, (Hash, bool)> {
        self.entries
            .iter()
            .filter(|e| !tree::is_tree_type(&e.block_type) && e.doc.is_none())
            .map(|e| (&e.id, (e.version, e.redaction.is_some())))
            .collect()
    }

    /// Each doc's and folder's path as the comparison shows it (D-Z8Q4N, D-3M81W): a part whose
    /// version is in `removed` is named by its id, and the flag is set when a part was.
    pub(super) fn shown_paths(&self, removed: &BTreeMap<Hash, Hash>) -> Names {
        let version: BTreeMap<&BlockId, Hash> =
            self.entries.iter().map(|e| (&e.id, e.version)).collect();
        let shown = |path: &[PathPart]| {
            let mut replaced = false;
            let names: Vec<&str> = path
                .iter()
                .map(
                    |p| match version.get(&p.id).is_some_and(|v| removed.contains_key(v)) {
                        true => {
                            replaced = true;
                            p.id.as_str()
                        }
                        false => p.name.as_str(),
                    },
                )
                .collect();
            (names.join("/"), replaced)
        };
        self.docs
            .iter()
            .map(|d| (d.id.clone(), shown(&d.path)))
            .chain(self.folders.iter().map(|f| (f.id.clone(), shown(&f.path))))
            .collect()
    }

    /// The baseline as JSON: every member, with `baseline_hash` when `with_hash`.
    fn to_value(&self, with_hash: bool) -> Value {
        let opt = |x: Option<String>| x.map_or(Value::Null, Value::String);
        let id = |x: &Option<BlockId>| opt(x.as_ref().map(ToString::to_string));
        let hash = |x: Option<Hash>| opt(x.map(|h| h.to_string()));
        let num = |x: Option<u64>| x.map_or(Value::Null, |n| json!(n));
        let path = |p: &[PathPart]| -> Value {
            p.iter()
                .map(|x| json!({"id": x.id.to_string(), "name": x.name}))
                .collect()
        };
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                let parts: Map<String, Value> = e
                    .parts
                    .iter()
                    .map(|(k, v)| (k.to_string(), Value::String(v.clone())))
                    .collect();
                let mut row = json!({
                    "id": e.id.to_string(),
                    "version": e.version.to_string(),
                    "redaction": hash(e.redaction),
                    "doc": id(&e.doc),
                    "type": e.block_type,
                    "label": e.label,
                    "section": id(&e.section),
                    "parent": id(&e.parent),
                    "order": e.order,
                    "position": num(e.position),
                    "parts": parts,
                });
                if let Some(d) = &e.decision {
                    row["title"] = opt(d.title.clone());
                    row["status_tag"] = opt(d.status_tag.clone());
                }
                row
            })
            .collect();
        let docs: Vec<Value> = self
            .docs
            .iter()
            .map(|d| {
                json!({
                    "id": d.id.to_string(),
                    "path": path(&d.path),
                    "digest": d.digest.to_string(),
                    "title": opt(d.title.clone()),
                    "page": opt(d.page.clone()),
                    "position": num(d.position),
                })
            })
            .collect();
        let folders: Vec<Value> = self
            .folders
            .iter()
            .map(|f| {
                json!({
                    "id": f.id.to_string(),
                    "version": f.version.to_string(),
                    "path": path(&f.path),
                    "title": opt(f.title.clone()),
                })
            })
            .collect();
        let attachments: Vec<Value> = self
            .attachments
            .iter()
            .map(|a| {
                let mut row = json!({
                    "block": a.row.block.to_string(),
                    "home_doc": a.row.home_doc.to_string(),
                    "blob": a.row.blob.to_string(),
                    "available": a.row.available,
                    "redaction": hash(a.redaction),
                    "label": a.label,
                });
                if let Some(v) = a.row.variant {
                    row["variant"] = json!(v.as_str());
                }
                row
            })
            .collect();
        let references: Vec<Value> = self
            .references
            .iter()
            .map(|r| {
                json!({
                    "source": r.source.to_string(),
                    "location": r.location,
                    "target": r.target,
                    "classification": r.classification.as_str(),
                    "occurrences": r.occurrences,
                })
            })
            .collect();
        let mut v = json!({
            "format": BASELINE_FORMAT,
            "source": {
                "repo_id": self.source.repo_id.to_string(),
                "fork_origins": self.source.fork_origins.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "tag": self.source.tag,
                "landing": self.source.landing.to_string(),
            },
            "profile": {
                "id": id(&self.profile_id),
                "version": hash(self.profile_version),
                "audience": self.audience.as_str(),
            },
            "selection_hash": self.selection_hash.to_string(),
            "selection_identity": self.selection_identity.to_string(),
            "redactions": self.redactions.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "entries": entries,
            "docs": docs,
            "folders": folders,
            "hidden": self.hidden.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "attachments": attachments,
            "references": references,
        });
        if with_hash {
            v["baseline_hash"] = json!(self.baseline_hash.to_string());
        }
        v
    }
}

/// BLAKE3 over the format name, a zero byte and `canonical` (D-6WD17).
fn hash_of(canonical: &[u8]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(BASELINE_FORMAT.as_bytes());
    h.update(&[0]);
    h.update(canonical);
    Hash(*h.finalize().as_bytes())
}

/// The canonical bytes of `input` read under the reader's rules (D-6WD17): the input bound, a
/// strict parse at a depth of at most [`BASELINE_DEPTH_MAX`], no duplicate key, safe integer
/// tokens and no lone surrogate, encoded in RFC 8785 form. No member is checked. Any failure is
/// `E_PUBLICATION_BASELINE`.
pub fn canonical_json(input: &[u8]) -> Result<Vec<u8>, RdyError> {
    if input.len() > caps().input {
        return Err(not_verified());
    }
    let v = parse_strict(input).map_err(|_| not_verified())?;
    rfc8785_safe_integers(&v)
        .map(String::into_bytes)
        .map_err(|_| not_verified())
}

// ---- the strict parser --------------------------------------------------------------------

/// A failed parse or member check: the reader turns each into its one refusal.
#[derive(Debug)]
struct Bad;

type Parsed<T> = Result<T, Bad>;

/// `input` as one JSON value under the reader's rules: UTF-8, depth at most
/// [`BASELINE_DEPTH_MAX`], no duplicate key, every number a token `0|[1-9][0-9]*` of at most
/// [`BASELINE_INTEGER_MAX`], no lone surrogate, nothing after the value but white space.
fn parse_strict(input: &[u8]) -> Parsed<Value> {
    let text = std::str::from_utf8(input).map_err(|_| Bad)?;
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    match p.i == p.s.len() {
        true => Ok(v),
        false => Err(Bad),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> Parsed<()> {
        match self.peek() == Some(b) {
            true => {
                self.i += 1;
                Ok(())
            }
            false => Err(Bad),
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Parsed<Value> {
        match self.s[self.i..].starts_with(word.as_bytes()) {
            true => {
                self.i += word.len();
                Ok(v)
            }
            false => Err(Bad),
        }
    }

    /// One value at `depth` levels of nesting above it.
    fn value(&mut self, depth: usize) -> Parsed<Value> {
        match self.peek().ok_or(Bad)? {
            b'{' => self.object(depth + 1),
            b'[' => self.array(depth + 1),
            b'"' => self.string().map(Value::String),
            b't' => self.literal("true", Value::Bool(true)),
            b'f' => self.literal("false", Value::Bool(false)),
            b'n' => self.literal("null", Value::Null),
            b'0'..=b'9' => self.number(),
            _ => Err(Bad),
        }
    }

    fn object(&mut self, depth: usize) -> Parsed<Value> {
        if depth > BASELINE_DEPTH_MAX {
            return Err(Bad);
        }
        self.eat(b'{')?;
        let mut map = Map::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let v = self.value(depth)?;
            if map.insert(key, v).is_some() {
                return Err(Bad);
            }
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(map));
                }
                _ => return Err(Bad),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Parsed<Value> {
        if depth > BASELINE_DEPTH_MAX {
            return Err(Bad);
        }
        self.eat(b'[')?;
        let mut items = vec![];
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth)?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(Bad),
            }
        }
    }

    /// A number token `0|[1-9][0-9]*` of at most [`BASELINE_INTEGER_MAX`]: no sign, fraction or
    /// exponent.
    fn number(&mut self) -> Parsed<Value> {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        let digits = &self.s[start..self.i];
        if (digits.len() > 1 && digits[0] == b'0')
            || matches!(self.peek(), Some(b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            return Err(Bad);
        }
        let mut n: u64 = 0;
        for d in digits {
            n = n
                .checked_mul(10)
                .and_then(|n| n.checked_add(u64::from(d - b'0')))
                .filter(|n| *n <= BASELINE_INTEGER_MAX)
                .ok_or(Bad)?;
        }
        Ok(Value::Number(n.into()))
    }

    fn hex4(&mut self) -> Parsed<u32> {
        let h = self.s.get(self.i..self.i + 4).ok_or(Bad)?;
        let h = std::str::from_utf8(h).map_err(|_| Bad)?;
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Bad);
        }
        self.i += 4;
        u32::from_str_radix(h, 16).map_err(|_| Bad)
    }

    /// A string: its escapes read, a control character refused, and a surrogate escape taken
    /// only as one half of a pair.
    fn string(&mut self) -> Parsed<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.i += 1;
            }
            // the input is UTF-8 and the run stops at an ASCII byte, so the run is UTF-8
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).map_err(|_| Bad)?);
            match self.peek().ok_or(Bad)? {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let e = self.peek().ok_or(Bad)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let c = match hi {
                                0xD800..=0xDBFF => {
                                    self.eat(b'\\')?;
                                    self.eat(b'u')?;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&lo) {
                                        return Err(Bad);
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                }
                                0xDC00..=0xDFFF => return Err(Bad),
                                c => c,
                            };
                            out.push(char::from_u32(c).ok_or(Bad)?);
                        }
                        _ => return Err(Bad),
                    }
                }
                _ => return Err(Bad),
            }
        }
    }
}

// ---- the members --------------------------------------------------------------------------

/// The members of `v`, an object that holds every name of `required`, and no name outside
/// `required` and `optional`.
fn members<'v>(
    v: &'v Value,
    required: &[&str],
    optional: &[&str],
) -> Parsed<&'v Map<String, Value>> {
    let Value::Object(m) = v else { return Err(Bad) };
    if !required.iter().all(|k| m.contains_key(*k))
        || !m
            .keys()
            .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    {
        return Err(Bad);
    }
    Ok(m)
}

fn get<'v>(m: &'v Map<String, Value>, k: &str) -> Parsed<&'v Value> {
    m.get(k).ok_or(Bad)
}

fn string(v: &Value) -> Parsed<&str> {
    v.as_str().ok_or(Bad)
}

fn opt_string(v: &Value) -> Parsed<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(Bad),
    }
}

fn block_id(v: &Value) -> Parsed<BlockId> {
    let s = string(v)?;
    let id = BlockId::parse(s).map_err(|_| Bad)?;
    match id.as_str() == s {
        true => Ok(id),
        false => Err(Bad),
    }
}

fn opt_block_id(v: &Value) -> Parsed<Option<BlockId>> {
    match v {
        Value::Null => Ok(None),
        v => block_id(v).map(Some),
    }
}

/// 64 lower-case hex digits.
fn hash(v: &Value) -> Parsed<Hash> {
    string(v)?.parse().map_err(|_| Bad)
}

fn opt_hash(v: &Value) -> Parsed<Option<Hash>> {
    match v {
        Value::Null => Ok(None),
        v => hash(v).map(Some),
    }
}

/// 32 lower-case hex digits.
fn repo_id(v: &Value) -> Parsed<RepoId> {
    let s = string(v)?;
    if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Bad);
    }
    s.parse().map_err(|_| Bad)
}

fn integer(v: &Value) -> Parsed<u64> {
    v.as_u64().filter(|n| *n <= BASELINE_INTEGER_MAX).ok_or(Bad)
}

fn opt_integer(v: &Value) -> Parsed<Option<u64>> {
    match v {
        Value::Null => Ok(None),
        v => integer(v).map(Some),
    }
}

/// The items of `v`, an array of at most `max` items.
fn array(v: &Value, max: usize) -> Parsed<&Vec<Value>> {
    match v {
        Value::Array(a) if a.len() <= max => Ok(a),
        _ => Err(Bad),
    }
}

/// Ok when `keys` rise strictly: each row comes after the one before it and none is there
/// twice.
fn strictly_rising<K: Ord>(keys: impl Iterator<Item = K>) -> Parsed<()> {
    let mut last: Option<K> = None;
    for k in keys {
        if last.as_ref().is_some_and(|l| *l >= k) {
            return Err(Bad);
        }
        last = Some(k);
    }
    Ok(())
}

/// A path: its parts, root first, at most [`Limits::path`] bytes as the comparison joins them,
/// ending with `id`.
fn path(v: &Value, id: &BlockId, l: &Limits) -> Parsed<Vec<PathPart>> {
    let parts = array(v, l.path)?
        .iter()
        .map(|p| {
            let m = members(p, &["id", "name"], &[])?;
            Ok(PathPart {
                id: block_id(get(m, "id")?)?,
                name: string(get(m, "name")?)?.to_string(),
            })
        })
        .collect::<Parsed<Vec<PathPart>>>()?;
    let bytes = parts.iter().map(|p| p.name.len()).sum::<usize>() + parts.len().saturating_sub(1);
    if parts.last().map(|p| &p.id) != Some(id) || bytes > l.path {
        return Err(Bad);
    }
    Ok(parts)
}

/// The part keys an entry of `block_type` holds (D-MCH70): a doc or folder `title` with
/// `field:name` and `field:description` when it has them; a decision exactly `title`,
/// `statement` and `status`; any other block `title`, `text`, `type`, `links` and `status`,
/// with `field:<name>` for each other field.
fn check_parts(block_type: &str, parts: &BTreeMap<PartName, String>) -> Parsed<()> {
    use PartName::*;
    let fixed: &[PartName] = if tree::is_tree_type(block_type) {
        &[Title]
    } else if block_type == decision::DECISION {
        &[Title, Statement, Status]
    } else {
        &[Title, Text, Type, Links, Status]
    };
    if !fixed.iter().all(|p| parts.contains_key(p)) {
        return Err(Bad);
    }
    let covered = text_fields(block_type);
    for p in parts.keys() {
        let ok = fixed.contains(p)
            || match p {
                Field(name) if tree::is_tree_type(block_type) => {
                    name == "name" || name == "description"
                }
                Field(name) if block_type != decision::DECISION => {
                    name != "title" && !covered.contains(&name.as_str())
                }
                _ => false,
            };
        if !ok {
            return Err(Bad);
        }
    }
    Ok(())
}

/// A part hash: 64 lower-case hex digits.
fn part_hash(v: &Value) -> Parsed<String> {
    let s = string(v)?;
    match s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        true => Ok(s.to_string()),
        false => Err(Bad),
    }
}

/// The members of a baseline, all required.
const TOP: [&str; 13] = [
    "format",
    "source",
    "profile",
    "selection_hash",
    "selection_identity",
    "redactions",
    "entries",
    "docs",
    "folders",
    "hidden",
    "attachments",
    "references",
    "baseline_hash",
];

/// The typed baseline of `v`, with every member, order and limit checked. The hash is not.
fn from_value(v: &Value, l: &Limits) -> Parsed<Baseline> {
    let m = members(v, &TOP, &[])?;
    if string(get(m, "format")?)? != BASELINE_FORMAT {
        return Err(Bad);
    }
    // source
    let s = members(
        get(m, "source")?,
        &["repo_id", "fork_origins", "tag", "landing"],
        &[],
    )?;
    let fork_origins = array(get(s, "fork_origins")?, l.selection)?
        .iter()
        .map(repo_id)
        .collect::<Parsed<Vec<RepoId>>>()?;
    strictly_rising(fork_origins.iter().map(ToString::to_string))?;
    let source = BaselineSource {
        repo_id: repo_id(get(s, "repo_id")?)?,
        fork_origins,
        tag: string(get(s, "tag")?)?.to_string(),
        landing: hash(get(s, "landing")?)?,
    };
    // profile
    let p = members(get(m, "profile")?, &["id", "version", "audience"], &[])?;
    let audience = match string(get(p, "audience")?)? {
        "org" => Audience::Org,
        "public" => Audience::Public,
        _ => return Err(Bad),
    };
    let profile_id = opt_block_id(get(p, "id")?)?;
    let profile_version = opt_hash(get(p, "version")?)?;
    if profile_id.is_some() != profile_version.is_some() {
        return Err(Bad);
    }
    // redactions
    let redactions = array(get(m, "redactions")?, super::select::REDACTION_REQUESTS)?
        .iter()
        .map(hash)
        .collect::<Parsed<Vec<Hash>>>()?;
    strictly_rising(redactions.iter().map(ToString::to_string))?;
    // entries
    let entries = array(get(m, "entries")?, l.selection)?
        .iter()
        .map(entry)
        .collect::<Parsed<Vec<BaselineEntry>>>()?;
    strictly_rising(entries.iter().map(|e| e.id.as_str()))?;
    let types: BTreeMap<&BlockId, (&str, Hash)> = entries
        .iter()
        .map(|e| (&e.id, (e.block_type.as_str(), e.version)))
        .collect();
    // docs and folders: each is an entry of its type
    let docs = array(get(m, "docs")?, l.selection)?
        .iter()
        .map(|d| {
            let r = members(
                d,
                &["id", "path", "digest", "title", "page", "position"],
                &[],
            )?;
            let id = block_id(get(r, "id")?)?;
            if types.get(&id).map(|t| t.0) != Some(tree::DOC) {
                return Err(Bad);
            }
            Ok(BaselineDoc {
                path: path(get(r, "path")?, &id, l)?,
                digest: hash(get(r, "digest")?)?,
                title: opt_string(get(r, "title")?)?,
                page: opt_string(get(r, "page")?)?,
                position: opt_integer(get(r, "position")?)?,
                id,
            })
        })
        .collect::<Parsed<Vec<BaselineDoc>>>()?;
    strictly_rising(docs.iter().map(|d| d.id.as_str()))?;
    let folders = array(get(m, "folders")?, l.selection)?
        .iter()
        .map(|f| {
            let r = members(f, &["id", "version", "path", "title"], &[])?;
            let id = block_id(get(r, "id")?)?;
            let version = hash(get(r, "version")?)?;
            if types.get(&id) != Some(&(tree::FOLDER, version)) {
                return Err(Bad);
            }
            Ok(BaselineFolder {
                path: path(get(r, "path")?, &id, l)?,
                title: opt_string(get(r, "title")?)?,
                version,
                id,
            })
        })
        .collect::<Parsed<Vec<BaselineFolder>>>()?;
    strictly_rising(folders.iter().map(|f| f.id.as_str()))?;
    // hidden ids
    let hidden = array(get(m, "hidden")?, l.selection)?
        .iter()
        .map(block_id)
        .collect::<Parsed<Vec<BlockId>>>()?;
    strictly_rising(hidden.iter().map(BlockId::as_str))?;
    // attachments: by home doc, block and variant, light first
    let attachments = array(get(m, "attachments")?, l.attachments)?
        .iter()
        .map(attachment)
        .collect::<Parsed<Vec<BaselineAttachment>>>()?;
    strictly_rising(
        attachments
            .iter()
            .map(|a| (a.row.home_doc.as_str(), a.row.block.as_str(), a.row.variant)),
    )?;
    // references: by source, location, target and classification
    let references = array(get(m, "references")?, l.references)?
        .iter()
        .map(|r| reference(r, l))
        .collect::<Parsed<Vec<ReferenceRow>>>()?;
    strictly_rising(references.iter().map(|r| {
        (
            r.source.as_str(),
            r.location.as_str(),
            r.target.as_str(),
            r.classification,
        )
    }))?;
    let occurrences = references
        .iter()
        .try_fold(0u64, |n, r| n.checked_add(r.occurrences))
        .ok_or(Bad)?;
    if occurrences > l.references as u64 {
        return Err(Bad);
    }
    Ok(Baseline {
        source,
        profile_id,
        profile_version,
        audience,
        selection_hash: hash(get(m, "selection_hash")?)?,
        selection_identity: hash(get(m, "selection_identity")?)?,
        redactions,
        entries,
        docs,
        folders,
        hidden,
        attachments,
        references,
        baseline_hash: hash(get(m, "baseline_hash")?)?,
        selection: vec![],
        rows: vec![],
    })
}

/// The members of every entry row; a decision's row adds `title` and `status_tag`.
const ENTRY: [&str; 11] = [
    "id",
    "version",
    "redaction",
    "doc",
    "type",
    "label",
    "section",
    "parent",
    "order",
    "position",
    "parts",
];

/// One entry row: the closed members, and a decision's title and status tag.
fn entry(v: &Value) -> Parsed<BaselineEntry> {
    let Value::Object(raw) = v else {
        return Err(Bad);
    };
    let block_type = string(raw.get("type").ok_or(Bad)?)?.to_string();
    let is_decision = block_type == decision::DECISION;
    let m = match is_decision {
        true => members(v, &[&ENTRY[..], &["title", "status_tag"]].concat(), &[])?,
        false => members(v, &ENTRY, &[])?,
    };
    if block_type.is_empty() {
        return Err(Bad);
    }
    let parts = match get(m, "parts")? {
        Value::Object(p) => p
            .iter()
            .map(|(k, h)| Ok((PartName::parse(k).ok_or(Bad)?, part_hash(h)?)))
            .collect::<Parsed<BTreeMap<PartName, String>>>()?,
        _ => return Err(Bad),
    };
    check_parts(&block_type, &parts)?;
    let decision = match is_decision {
        true => Some(BaselineDecision {
            title: opt_string(get(m, "title")?)?,
            status_tag: opt_string(get(m, "status_tag")?)?,
        }),
        false => None,
    };
    Ok(BaselineEntry {
        id: block_id(get(m, "id")?)?,
        version: hash(get(m, "version")?)?,
        redaction: opt_hash(get(m, "redaction")?)?,
        doc: opt_block_id(get(m, "doc")?)?,
        label: string(get(m, "label")?)?.to_string(),
        section: opt_block_id(get(m, "section")?)?,
        parent: opt_block_id(get(m, "parent")?)?,
        order: string(get(m, "order")?)?.to_string(),
        position: opt_integer(get(m, "position")?)?,
        block_type,
        parts,
        decision,
    })
}

/// One attachment row: a dark row holds `variant: "dark"`, a light row no `variant`.
fn attachment(v: &Value) -> Parsed<BaselineAttachment> {
    let m = members(
        v,
        &[
            "block",
            "home_doc",
            "blob",
            "available",
            "redaction",
            "label",
        ],
        &["variant"],
    )?;
    let variant = match m.get("variant") {
        None => None,
        Some(x) if string(x)? == AttachmentVariant::Dark.as_str() => Some(AttachmentVariant::Dark),
        Some(_) => return Err(Bad),
    };
    Ok(BaselineAttachment {
        row: AttachmentEntry {
            block: block_id(get(m, "block")?)?,
            home_doc: block_id(get(m, "home_doc")?)?,
            blob: hash(get(m, "blob")?)?,
            available: get(m, "available")?.as_bool().ok_or(Bad)?,
            variant,
        },
        redaction: opt_hash(get(m, "redaction")?)?,
        label: string(get(m, "label")?)?.to_string(),
    })
}

/// One reference row, its location and target token within their byte bounds.
fn reference(v: &Value, l: &Limits) -> Parsed<ReferenceRow> {
    let m = members(
        v,
        &[
            "source",
            "location",
            "target",
            "classification",
            "occurrences",
        ],
        &[],
    )?;
    let location = string(get(m, "location")?)?.to_string();
    let target = string(get(m, "target")?)?.to_string();
    if location.len() > l.location || target.len() > l.token {
        return Err(Bad);
    }
    let occurrences = integer(get(m, "occurrences")?)?;
    if occurrences == 0 {
        return Err(Bad);
    }
    Ok(ReferenceRow {
        source: block_id(get(m, "source")?)?,
        location,
        target,
        classification: match string(get(m, "classification")?)? {
            "excluded" => Classification::Excluded,
            "missing" => Classification::Missing,
            _ => return Err(Bad),
        },
        occurrences,
    })
}

// ---- make ---------------------------------------------------------------------------------

/// The baseline of `resolved` from `facts`, read with the removals `source` knows (D-W4YAE):
/// titles and labels follow them, a removed block's label reads "Content removed." and a title
/// a removal removed is null. `E_LIMIT`, naming the canonical cap, when its canonical encoding
/// would pass it; then no baseline is given.
pub fn build(
    facts: BaselineSource,
    resolved: &Resolved,
    source: &dyn PublicationSource,
) -> Result<Baseline, RdyError> {
    let removals = ReviewRemovals::of_sides(source, &[resolved])?;
    let side = ReaderReviewSide::new(resolved, &removals);
    let mut entries = Vec::with_capacity(resolved.entries.len());
    for e in &resolved.entries {
        let shown = side.shown(&e.id).ok_or_else(invalid_tree)?;
        let parts = side.parts(&e.id).ok_or_else(invalid_tree)?.hashes();
        let decision = side.decision(&e.id).map(|d| BaselineDecision {
            title: (!d.redacted).then(|| d.title.clone()),
            status_tag: d.status.clone(),
        });
        entries.push(BaselineEntry {
            id: e.id.clone(),
            version: e.version,
            redaction: e.redaction,
            doc: shown.home_doc.clone(),
            block_type: shown.block_type.clone(),
            label: shown.label.clone(),
            section: shown.section.clone(),
            parent: shown.parent.clone(),
            order: shown.order.clone(),
            position: shown.position.map(|p| p as u64),
            parts,
            decision,
        });
    }
    let path = |id: &BlockId| path_parts(&resolved.m, id).ok_or_else(invalid_tree);
    let title = |id: &BlockId| side.tree_entry(id).and_then(|t| t.title.clone());
    let mut docs = vec![];
    for d in &resolved.docs {
        let t = side.tree_entry(&d.id).ok_or_else(invalid_tree)?;
        docs.push(BaselineDoc {
            id: d.id.clone(),
            path: path(&d.id)?,
            digest: d.digest,
            title: t.title.clone(),
            page: t.page.clone(),
            position: t.position.map(|p| p as u64),
        });
    }
    let mut folders = vec![];
    for e in resolved.entries.iter().filter(|e| {
        resolved
            .m
            .get(&e.id)
            .is_some_and(|b| b.block_type == tree::FOLDER)
    }) {
        folders.push(BaselineFolder {
            id: e.id.clone(),
            version: e.version,
            path: path(&e.id)?,
            title: title(&e.id),
        });
    }
    let attachments = resolved
        .attachments
        .iter()
        .map(|a| BaselineAttachment {
            row: a.clone(),
            redaction: removals.blobs.get(&a.blob).copied(),
            label: figure_label(&side, &a.block),
        })
        .collect();
    let mut references: Vec<ReferenceRow> = resolved
        .references
        .rows
        .iter()
        .chain(&resolved.references.links_outside)
        .cloned()
        .collect();
    references.sort_by(|x, y| {
        (&x.source, &x.location, &x.target, x.classification).cmp(&(
            &y.source,
            &y.location,
            &y.target,
            y.classification,
        ))
    });
    let mut fork_origins = facts.fork_origins.clone();
    fork_origins.sort_by_key(ToString::to_string);
    fork_origins.dedup();
    let mut b = Baseline {
        source: BaselineSource {
            fork_origins,
            ..facts
        },
        profile_id: resolved.profile.id.clone(),
        profile_version: resolved.profile.version,
        audience: resolved.profile.audience,
        selection_hash: resolved.selection_hash,
        selection_identity: resolved.selection_identity,
        redactions: resolved.redactions.clone(),
        entries,
        docs,
        folders,
        hidden: resolved.hidden_planned.ids().cloned().collect(),
        attachments,
        references,
        baseline_hash: Hash([0; 32]),
        selection: vec![],
        rows: vec![],
    };
    let cap = caps().canonical;
    let canonical =
        rfc8785_safe_integers(&b.to_value(false)).map_err(|_| limit("baseline", cap, "bytes"))?;
    if canonical.len() > cap {
        return Err(limit("baseline", cap, "bytes"));
    }
    b.baseline_hash = hash_of(canonical.as_bytes());
    b.derive();
    Ok(b)
}

/// The path parts of doc or folder `id` in `m`, root first: each tree block's id and name.
fn path_parts(m: &Materialized, id: &BlockId) -> Option<Vec<PathPart>> {
    let mut parts = vec![];
    let mut cur = m.get(id);
    for _ in 0..=m.len() {
        let Some(b) = cur else { break };
        if !tree::is_tree_type(&b.block_type) {
            return None;
        }
        parts.push(PathPart {
            id: b.id.clone(),
            name: m.name_of(&b.id)?,
        });
        cur = b.parent.as_ref().and_then(|p| m.get(p));
    }
    parts.reverse();
    Some(parts)
}

/// The baseline of `profile` (a profile id, a key or `implicit:org`) at tag `tag`, whose
/// landing is `landing`, in the store `read` reads (D-CDQF7). It writes nothing. The read epoch
/// is checked when it ends: a removal during the make refuses it with `E_REDACTED_STATE`.
pub fn make_at(
    read: &ReadState,
    tag: &str,
    landing: Hash,
    profile: &str,
) -> Result<Baseline, RdyError> {
    let resolved = resolve(read, &Scope::At(landing), profile)?;
    let (repo_id, fork_origins) = crate::signature_check::repo_ids(read)?;
    let source = ReadPublicationSource::with_settings(read, read.pack(), "", None);
    let baseline = build(
        BaselineSource {
            repo_id,
            fork_origins,
            tag: tag.to_string(),
            landing,
        },
        &resolved,
        &source,
    )?;
    read.ensure_current_epoch()?;
    Ok(baseline)
}

/// The baseline of `profile` in `scope`, which must be `tag:<name>` (D-CDQF7); any other scope
/// is `E_BAD_INPUT`.
pub fn make(read: &ReadState, scope: &str, profile: &str) -> Result<Baseline, RdyError> {
    let Some(name) = scope.strip_prefix("tag:") else {
        return Err(needs_tag());
    };
    let Scope::At(landing) = read.resolve_scope(scope)? else {
        return Err(needs_tag());
    };
    // the tag's own name: `tag:1.0.0` names the tag `v1.0.0`
    let want = crate::parse_version(name).ok();
    let tag = read
        .version_tags()?
        .into_iter()
        .filter(|(_, t)| t.landing == landing)
        .find(|(v, t)| t.name == name || want.as_ref().is_some_and(|w| v.cmp_precedence(w).is_eq()))
        .map_or_else(|| name.to_string(), |(_, t)| t.name);
    make_at(read, &tag, landing, profile)
}

/// The command the baseline's file output names in its texts.
const BASELINE_CMD: &str = "rdy publication baseline";

/// The reply of a baseline make (D-CDQF7, D-6WD17), the one form `rdy_publication_baseline` and
/// `rdy publication baseline` share: the baseline of `profile` at `scope`, which must be
/// `tag:<name>` (an absent scope is refused as any other scope is). With `out` the canonical
/// bytes go to that new file, through the output guards of a dump, and the reply holds no
/// baseline; without it the reply holds the baseline as `content`. `baseline` names the
/// baseline as a comparison does: its source's repo id, its tag and landing, and its hash. It
/// writes nothing else.
pub fn make_reply(
    read: &ReadState,
    profile: &str,
    scope: Option<&str>,
    out: Option<&std::path::Path>,
) -> Result<Value, RdyError> {
    let baseline = make(read, scope.unwrap_or("canon"), profile)?;
    let bytes = baseline.to_bytes();
    let src = baseline.source();
    let mut reply = json!({
        "baseline": {
            "repo_id": src.repo_id.to_string(),
            "tag": src.tag,
            "landing": src.landing.to_string(),
            "baseline_hash": baseline.baseline_hash().to_string(),
        },
        "profile": {
            "id": baseline.profile_id().map(ToString::to_string),
            "audience": baseline.audience().as_str(),
        },
        "bytes": bytes.len(),
        "out": null,
    });
    match out {
        Some(out) => {
            read.write_new_output(out, &bytes, BASELINE_CMD)?;
            reply["out"] = json!(out.display().to_string());
        }
        None => reply["content"] = baseline.to_json(),
    }
    Ok(reply)
}

// ---- compare ------------------------------------------------------------------------------

/// Whether `baseline` shares a lineage with the project `read` reads (D-CDQF7): the repo ids
/// and logged fork origins of the two sources have one repo id in common. The candidate's fork
/// origins are the ones its log records (D-1ZTBP).
pub fn check_lineage(read: &ReadState, baseline: &Baseline) -> Result<(), RdyError> {
    let (current, origins) = crate::signature_check::repo_ids(read)?;
    let ours: BTreeSet<RepoId> = std::iter::once(current).chain(origins).collect();
    let shared = std::iter::once(&baseline.source.repo_id)
        .chain(&baseline.source.fork_origins)
        .any(|r| ours.contains(r));
    match shared {
        true => Ok(()),
        false => Err(outside_lineage()),
    }
}

/// The comparison of the candidate `after`, resolved in the store `read` reads from
/// `candidate_source`, with `baseline` (D-CDQF7): its comparison and its review. In order it
/// checks the baseline hash, `binding` when the caller passes one, the profile id and audience
/// (the texts of [`super::disclosure_diff`]) and the lineage. It reads no record of the
/// baseline's store.
pub fn compare(
    read: &ReadState,
    source: &dyn PublicationSource,
    candidate_source: &Materialized,
    after: &Resolved,
    baseline: &Baseline,
    binding: Option<&BaselineBinding>,
) -> Result<(DisclosureDiff, PublicationReview), RdyError> {
    baseline.verify()?;
    if let Some(b) = binding {
        baseline.check_binding(b)?;
    }
    super::diff::same_profile(baseline.profile_id(), baseline.audience(), after)?;
    check_lineage(read, baseline)?;
    let diff = baseline_diff(baseline, after)?;
    let review = PublicationReview::build_against_baseline(
        source,
        candidate_source,
        after,
        baseline,
        &diff,
    )?;
    Ok((diff, review))
}

// ---- the side view ------------------------------------------------------------------------

impl ReviewRemovals {
    /// These removals with the ones `baseline` reflects: each removed version its entries hold,
    /// and each blob a removal removed from its attachment rows.
    pub fn with_baseline(mut self, baseline: &Baseline) -> ReviewRemovals {
        for (version, request) in removed_entries(baseline.selection.iter()) {
            self.versions.entry(version).or_insert(request);
        }
        for a in &baseline.attachments {
            if let Some(request) = a.redaction {
                self.blobs.entry(a.row.blob).or_insert(request);
            }
        }
        self
    }
}

/// The side view of a baseline (D-W4YAE): its labels, sections, places and part hashes, with
/// the removals of both sides applied by version and by blob. A block a removal of the other
/// side removed reads as its stub: "Content removed.", no title, and the part hashes of the
/// stub. It has no reader text, so it gives no excerpt and no word diff, and no label is turned
/// back into text.
#[derive(Clone, Debug)]
pub struct BaselineReviewSide {
    selection: SelectionFacts,
    reading: Vec<BlockId>,
    tree: BTreeMap<BlockId, TreeFacts>,
    shown: BTreeMap<BlockId, ShownBlock>,
    children: BTreeMap<Option<BlockId>, Vec<BlockId>>,
    parts: BTreeMap<BlockId, BlockParts>,
    decisions: BTreeMap<BlockId, DecisionFacts>,
    attachment_labels: BTreeMap<BlockId, String>,
}

impl BaselineReviewSide {
    /// The view of `baseline` with `removals` applied.
    pub fn new(baseline: &Baseline, removals: &ReviewRemovals) -> BaselineReviewSide {
        let removed_figures: BTreeSet<&BlockId> = baseline
            .attachments
            .iter()
            .filter(|a| removals.blobs.contains_key(&a.row.blob))
            .map(|a| &a.row.block)
            .collect();
        let masked = |e: &BaselineEntry| {
            e.redaction.is_some()
                || removals.versions.contains_key(&e.version)
                || removed_figures.contains(&e.id)
        };
        let removed_label = labels::plain_label(CONTENT_REMOVED);
        // a path part whose version either side removed is named by its id (D-Z8Q4N)
        let versions: BTreeMap<&BlockId, Hash> = baseline
            .entries
            .iter()
            .map(|e| (&e.id, e.version))
            .collect();
        let part_removed = |p: &PathPart| {
            versions
                .get(&p.id)
                .is_some_and(|v| removals.versions.contains_key(v))
                || p.name == p.id.as_str()
        };
        let shown_path = |path: &[PathPart]| {
            path.iter()
                .map(|p| match part_removed(p) {
                    true => p.id.as_str(),
                    false => p.name.as_str(),
                })
                .collect::<Vec<_>>()
                .join("/")
        };
        // a page names the doc's path; when a part of it is removed the page shows the id
        // in its place, and a page of another form is not shown
        let shown_page = |d: &BaselineDoc| -> Option<String> {
            let page = d.page.as_ref()?;
            if !d.path.iter().any(part_removed) {
                return Some(page.clone());
            }
            let named = d
                .path
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            (*page == format!("{named}.html")).then(|| format!("{}.html", shown_path(&d.path)))
        };
        let docs: BTreeMap<&BlockId, &BaselineDoc> =
            baseline.docs.iter().map(|d| (&d.id, d)).collect();
        let folders: BTreeMap<&BlockId, &BaselineFolder> =
            baseline.folders.iter().map(|f| (&f.id, f)).collect();

        let mut tree_facts = BTreeMap::new();
        let mut shown = BTreeMap::new();
        let mut parts = BTreeMap::new();
        let mut decisions = BTreeMap::new();
        let mut kids: BTreeMap<Option<BlockId>, Vec<(&str, &BlockId)>> = BTreeMap::new();
        for e in &baseline.entries {
            let redacted = masked(e);
            kids.entry(e.parent.clone())
                .or_default()
                .push((e.order.as_str(), &e.id));
            if tree::is_tree_type(&e.block_type) {
                let (title, page, position) = match (docs.get(&e.id), folders.get(&e.id)) {
                    (Some(d), _) => (d.title.clone(), shown_page(d), d.position),
                    (None, Some(f)) => (f.title.clone(), None, None),
                    (None, None) => (None, None, None),
                };
                let title = title.filter(|_| !redacted);
                tree_facts.insert(
                    e.id.clone(),
                    TreeFacts {
                        id: e.id.clone(),
                        block_type: e.block_type.clone(),
                        parent: e.parent.clone(),
                        redacted: redacted || title.is_none(),
                        title,
                        page,
                        position: position.map(|p| p as usize),
                        lede: None,
                    },
                );
            }
            shown.insert(
                e.id.clone(),
                ShownBlock {
                    id: e.id.clone(),
                    block_type: e.block_type.clone(),
                    home_doc: e.doc.clone(),
                    parent: e.parent.clone(),
                    order: e.order.clone(),
                    label: match redacted {
                        true => removed_label.clone(),
                        false => e.label.clone(),
                    },
                    section: e.section.clone(),
                    position: e.position.map(|p| p as usize),
                    redacted,
                },
            );
            let hashes = match redacted && e.redaction.is_none() {
                true => stub_parts(e),
                false => e.parts.clone(),
            };
            parts.insert(e.id.clone(), BlockParts::hashed(hashes));
            if let Some(d) = &e.decision {
                decisions.insert(
                    e.id.clone(),
                    DecisionFacts {
                        title: match redacted {
                            true => CONTENT_REMOVED.to_string(),
                            false => d.title.clone().unwrap_or_else(|| CONTENT_REMOVED.into()),
                        },
                        status: d.status_tag.clone().filter(|_| !redacted),
                        text: None,
                        redacted,
                    },
                );
            }
        }
        let children = kids
            .into_iter()
            .map(|(parent, mut ids)| {
                ids.sort();
                (parent, ids.into_iter().map(|(_, id)| id.clone()).collect())
            })
            .collect();
        // the docs and folders in reading order: each entry's place in the reading order of
        // every block, which keeps the order of the tree blocks
        let mut reading: Vec<&BaselineEntry> = baseline
            .entries
            .iter()
            .filter(|e| tree_facts.contains_key(&e.id))
            .collect();
        reading.sort_by_key(|e| (e.position.is_none(), e.position, &e.id));
        let attachment_labels = baseline
            .attachments
            .iter()
            .map(|a| {
                let label = match removed_figures.contains(&a.row.block)
                    || shown
                        .get(&a.row.block)
                        .is_some_and(|s: &ShownBlock| s.redacted)
                {
                    true => removed_label.clone(),
                    false => a.label.clone(),
                };
                (a.row.block.clone(), label)
            })
            .collect();
        BaselineReviewSide {
            selection: SelectionFacts {
                profile_id: baseline.profile_id.clone(),
                profile_version: baseline.profile_version,
                audience: baseline.audience,
                selection_hash: baseline.selection_hash,
                selection_identity: baseline.selection_identity,
                entries: baseline.selection.clone(),
                docs: baseline
                    .docs
                    .iter()
                    .map(|d| DocEntry {
                        id: d.id.clone(),
                        path: shown_path(&d.path),
                        digest: d.digest,
                        redacted: d.path.iter().any(part_removed),
                    })
                    .collect(),
                attachments: baseline.rows.clone(),
                // the baseline keeps the rows alone: no status or count of its check
                references: ReferenceReport {
                    status: CheckStatus::Clear,
                    excluded_count: 0,
                    missing_count: 0,
                    blocked_count: 0,
                    rows: baseline.references.clone(),
                    links_outside_count: 0,
                    links_outside: vec![],
                    dropped_decisions: BTreeSet::new(),
                },
                redactions: baseline.redactions.clone(),
            },
            reading: reading.into_iter().map(|e| e.id.clone()).collect(),
            tree: tree_facts,
            shown,
            children,
            parts,
            decisions,
            attachment_labels,
        }
    }
}

/// The part hashes of `e`'s stub: what a block reads as when a removal of the other side
/// removed its version. An ordinary block keeps its status, which no removal changes; a
/// decision's status is null.
fn stub_parts(e: &BaselineEntry) -> BTreeMap<PartName, String> {
    let stub = Block::redacted(
        e.id.clone(),
        e.block_type.clone(),
        e.parent.clone(),
        e.order.clone(),
        RedactedBlockIdentity {
            version: e.version,
            content_hash: Hash([0; 32]),
            frozen: false,
            request: Hash([0; 32]),
        },
    );
    let mut hashes = BlockParts::of(&stub, None).hashes();
    if e.decision.is_none()
        && !tree::is_tree_type(&e.block_type)
        && let Some(status) = e.parts.get(&PartName::Status)
    {
        hashes.insert(PartName::Status, status.clone());
    }
    hashes
}

impl PublicationReviewSide for BaselineReviewSide {
    fn selection(&self) -> &SelectionFacts {
        &self.selection
    }

    fn reading(&self) -> &[BlockId] {
        &self.reading
    }

    fn tree_entry(&self, id: &BlockId) -> Option<&TreeFacts> {
        self.tree.get(id)
    }

    fn shown(&self, id: &BlockId) -> Option<&ShownBlock> {
        self.shown.get(id)
    }

    fn children(&self, parent: Option<&BlockId>) -> &[BlockId] {
        self.children
            .get(&parent.cloned())
            .map_or(&[], Vec::as_slice)
    }

    fn parts(&self, id: &BlockId) -> Option<&BlockParts> {
        self.parts.get(id)
    }

    fn decision(&self, id: &BlockId) -> Option<&DecisionFacts> {
        self.decisions.get(id)
    }

    fn reader_text(&self, _: &BlockId) -> Option<&str> {
        None
    }

    fn attachment_label(&self, block: &BlockId) -> Option<&str> {
        self.attachment_labels.get(block).map(String::as_str)
    }
}
