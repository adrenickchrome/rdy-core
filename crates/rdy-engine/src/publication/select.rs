//! Selection (D-48NYM, D-0VCQ5, D-KDFZ3): one profile, read in one snapshot, resolved to its
//! whole docs and folders, their ancestor folders' own records, the selected state and induced
//! graph, the digests, the attachments and the reference check. Nothing here is stored.

use super::references::{self, ReferenceReport};
use crate::error::*;
use crate::materialize::Materialized;
use crate::{ReadState, Scope};
use rdy_graph::Graph;
use rdy_model::{BlockId, Hash, StateSnapshot, tree};
use rdy_schema::publication::{self as schema, Audience};
use rdy_schema::{Issue, Pack, Severity};
use rdy_store::StoreReader;
use rdy_store::kinds::{self, BlobManifest};
use serde::Serialize;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The fixed bounds of one resolution and its asset load. Each is checked before the row,
/// block, edge or byte that would pass it is kept; a bound never truncates silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// UTF-8 bytes of the profile id or key a caller gives.
    pub profile_name: usize,
    /// Retained blocks, ancestor folders included.
    pub selection: usize,
    /// Candidate graph edges of the selected blocks, before induction.
    pub graph: usize,
    /// UTF-8 bytes of one selected body or string field value.
    pub text_value: usize,
    /// UTF-8 bytes of every selected body and string field value together.
    pub text: usize,
    /// Reference and link occurrences in the selected blocks.
    pub references: usize,
    /// Attachment rows.
    pub attachments: usize,
    /// Levels of a field value (the top-level value is level 1).
    pub field_depth: usize,
    /// UTF-8 bytes of one reference row's location.
    pub location: usize,
    /// UTF-8 bytes of one reference row's target token.
    pub token: usize,
    /// UTF-8 bytes of one selected doc's or folder's path.
    pub path: usize,
    /// Bytes of one attachment blob.
    pub blob: u64,
    /// Bytes of every distinct attachment blob together.
    pub assets: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            profile_name: 128,
            selection: 100_000,
            graph: 1_000_000,
            text_value: 1_048_576,
            text: 67_108_864,
            references: 1_000_000,
            attachments: 100_000,
            field_depth: 64,
            location: 1024,
            token: 128,
            path: 4096,
            blob: 67_108_864,
            assets: 268_435_456,
        }
    }
}

#[cfg(feature = "test-support")]
thread_local! {
    static LIMITS: Cell<Option<Limits>> = const { Cell::new(None) };
}

thread_local! {
    static BLOB_READS: Cell<u64> = const { Cell::new(0) };
}

/// The bounds this thread resolves under: the fixed defaults (a build with the `test-support`
/// feature lets a test replace them for its thread).
pub fn limits() -> Limits {
    #[cfg(feature = "test-support")]
    if let Some(l) = LIMITS.with(Cell::get) {
        return l;
    }
    Limits::default()
}

/// Tests only (the `test-support` feature, which no normal build enables): resolves and loads
/// assets on this thread under `l` until the guard drops.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn override_limits_for_tests(l: Limits) -> LimitsGuard {
    LimitsGuard(LIMITS.with(|c| c.replace(Some(l))))
}

/// Restores the bounds [`override_limits_for_tests`] replaced when dropped.
#[cfg(feature = "test-support")]
#[doc(hidden)]
#[must_use = "the bounds are lowered only while the guard lives"]
pub struct LimitsGuard(Option<Limits>);

#[cfg(feature = "test-support")]
impl Drop for LimitsGuard {
    fn drop(&mut self) {
        LIMITS.with(|c| c.set(self.0));
    }
}

/// Tests only: how many blobs [`load_assets`] has read on this thread, so a test can check that
/// a bound refuses before any byte is read.
#[doc(hidden)]
pub fn blob_reads_for_tests() -> u64 {
    BLOB_READS.with(Cell::get)
}

/// `E_LIMIT` for one of the closed (part, limit, unit) rows.
pub(crate) fn limit(part: &str, limit: impl std::fmt::Display, unit: &str) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("Publication {part} exceeds the limit of {limit} {unit}."),
        "Reduce the selection or split the source project.",
    )
}

/// The profile metadata output and report code read. `id`, `version` and `key` are `None` only
/// for the implicit profile; a profile block fills all three.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub id: Option<BlockId>,
    pub version: Option<Hash>,
    pub key: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub audience: Audience,
    pub trail: bool,
}

/// One retained block: its source version and, for a removed version's stub, the request that
/// removed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionEntry {
    pub id: BlockId,
    pub version: Hash,
    pub redaction: Option<Hash>,
}

/// One selected doc: its path in the selected tree and the digest of it and its descendants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocEntry {
    pub id: BlockId,
    pub path: String,
    pub digest: Hash,
}

/// One selected figure's blob: its nearest selected doc, and whether the store can read the
/// blob (`false` for a blob the store lacks or a redaction removed, with no error).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentEntry {
    pub block: BlockId,
    pub home_doc: BlockId,
    pub blob: Hash,
    pub available: bool,
}

/// A warning on an output: `W_PUBLICATION_REFERENCE` for an org profile whose selection
/// references blocks outside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublicationWarning {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
}

/// One profile resolved in one snapshot. `m` and `graph` hold the selected blocks alone;
/// `entries` (every retained block), `docs` and `attachments` are sorted, and with
/// `selection_hash` they identify the selection. None of it is stored or written beside output.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub profile: ResolvedProfile,
    pub m: Materialized,
    pub graph: Graph,
    pub entries: Vec<SelectionEntry>,
    pub docs: Vec<DocEntry>,
    pub attachments: Vec<AttachmentEntry>,
    pub references: ReferenceReport,
    pub selection_hash: Hash,
}

impl Resolved {
    /// What an output caller asks first: `E_PUBLICATION_REFERENCE` when the reference check of
    /// a public profile is blocked. An inspection reads `references` instead.
    pub fn require_output(&self) -> Result<(), RdyError> {
        self.references.require_output()
    }
}

/// `profile` (a full profile id or an existing key) resolved in `scope`, which must be canon or
/// a landing (a tag resolves to one). The profile is the version that scope holds: a tag selects
/// what its own profile revision selected. A blocked check is returned, not refused.
pub fn resolve(read: &ReadState, scope: &Scope, profile: &str) -> Result<Resolved, RdyError> {
    match scope {
        Scope::Canon | Scope::At(_) => {}
        Scope::View(_) | Scope::Inflight | Scope::Proposal(_) => {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "A publication profile requires canon, a tag, or a landing.",
                "Use scope canon, tag:<name>, or landing:<hash>.",
            ));
        }
    }
    check_name(profile, &limits())?;
    let m = read.materialized(scope)?;
    resolve_in(read.pack(), read.reader(), &m, profile, &limits())
}

/// Tests only: [`resolve`] over the state `m`, which may be built in memory (for trees and
/// values a store refuses). The pack and the store reader are `read`'s.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn resolve_in_for_tests(
    read: &ReadState,
    m: &Materialized,
    profile: &str,
) -> Result<Resolved, RdyError> {
    resolve_in(read.pack(), read.reader(), m, profile, &limits())
}

fn check_name(profile: &str, l: &Limits) -> Result<(), RdyError> {
    if profile.len() > l.profile_name {
        return Err(limit("profile name", l.profile_name, "bytes"));
    }
    Ok(())
}

fn not_found(name: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("Publication profile \"{name}\" was not found."),
        "Use a publication profile id or key from this scope.",
    )
}

fn invalid_tree() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "Publication selection has an invalid document tree.",
        "Run rdy fsck and fix the document tree.",
    )
}

/// The schema's issues as the existing validation error: the first error's text, every issue.
fn issues_error(issues: Vec<Issue>) -> RdyError {
    let first = issues
        .iter()
        .find(|i| i.severity == Severity::Error)
        .or(issues.first());
    let (message, fix) = first
        .map(|i| (i.message.clone(), i.fix.clone()))
        .unwrap_or_default();
    let mut e = RdyError::new(E_VALIDATION, message, fix);
    e.issues = issues;
    e
}

/// Step 1: the profile block `name` names in `m`, read and parsed.
fn find_profile(
    pack: &Pack,
    m: &Materialized,
    name: &str,
) -> Result<(BlockId, schema::ProfileFields), RdyError> {
    let id = match crate::keys::resolve_text(m, name) {
        Ok(id) => id,
        Err(e) if e.code == E_UNKNOWN_BLOCK => return Err(not_found(name)),
        Err(e) => return Err(e),
    };
    let block = m.get(&id).ok_or_else(|| not_found(name))?;
    if block.block_type != schema::PROFILE_TYPE {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("Block {id} is not a publication profile."),
            "Use a publication profile id or key.",
        ));
    }
    if block.is_redacted() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("Publication profile {id} has been redacted."),
            "Choose a profile whose content is available.",
        ));
    }
    let parent = block.parent.as_ref().and_then(|p| m.get(p));
    let fields = schema::parse(pack, block, parent).map_err(issues_error)?;
    Ok((id, fields))
}

/// Adds `id` to `keep`, unless that passes the selection bound.
fn retain(keep: &mut BTreeSet<BlockId>, id: &BlockId, l: &Limits) -> Result<bool, RdyError> {
    if keep.contains(id) {
        return Ok(false);
    }
    if keep.len() >= l.selection {
        return Err(limit("selection", l.selection, "blocks"));
    }
    keep.insert(id.clone());
    Ok(true)
}

/// Steps 2 and 3: each selector's whole subtree, then each selector's ancestor folders, with
/// the tree checked on the way up. Iterative: a damaged store may nest deep or loop.
fn select(
    m: &Materialized,
    selectors: &[BlockId],
    l: &Limits,
) -> Result<BTreeSet<BlockId>, RdyError> {
    let mut keep = BTreeSet::new();
    for sel in selectors {
        let b = m.get(sel).ok_or_else(|| {
            RdyError::new(
                E_NOT_FOUND,
                format!("Publication selector {sel} was not found in this scope."),
                "Remove this selector or choose a scope that contains it.",
            )
        })?;
        if !tree::is_tree_type(&b.block_type) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("Publication selector {sel} must name a doc or folder."),
                "Select the containing doc or folder.",
            ));
        }
        let mut stack = vec![sel.clone()];
        while let Some(id) = stack.pop() {
            if !retain(&mut keep, &id, l)? {
                continue;
            }
            stack.extend(m.children(Some(&id)).iter().map(|c| c.id.clone()));
        }
    }
    for sel in selectors {
        let mut chain = BTreeSet::from([sel.clone()]);
        let mut cur = m.get(sel).ok_or_else(invalid_tree)?;
        while let Some(pid) = &cur.parent {
            let p = m.get(pid).ok_or_else(invalid_tree)?;
            if p.block_type != tree::FOLDER || !chain.insert(pid.clone()) {
                return Err(invalid_tree());
            }
            retain(&mut keep, pid, l)?;
            cur = p;
        }
    }
    Ok(keep)
}

/// Whether the store can read blob `h`, judged from its manifest and the redaction marks on the
/// manifest and its chunks; no blob byte is read.
fn available(reader: &StoreReader, h: &Hash) -> Result<bool, RdyError> {
    if !reader.has_blob(h)? || reader.redaction_of(kinds::BLOB_MANIFEST, h)?.is_some() {
        return Ok(false);
    }
    let manifest: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, h)?;
    for c in &manifest.chunks {
        if reader.redaction_of(kinds::BLOB_CHUNK, c)?.is_some() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Step 6: one row per selected figure whose `blob` field is a hash, sorted by (home doc, block,
/// blob). A removed figure's stub has no fields, so no row.
fn attachments(
    reader: &StoreReader,
    sel: &Materialized,
    l: &Limits,
) -> Result<Vec<AttachmentEntry>, RdyError> {
    let mut rows = vec![];
    let mut seen: BTreeMap<Hash, bool> = BTreeMap::new();
    for b in sel.iter() {
        if b.block_type != "figure" || b.is_redacted() {
            continue;
        }
        let Some(blob) = b.field_str("blob").and_then(|t| t.parse::<Hash>().ok()) else {
            continue;
        };
        if rows.len() >= l.attachments {
            return Err(limit("attachments", l.attachments, "rows"));
        }
        let home_doc = sel.doc_of(&b.id).ok_or_else(invalid_tree)?.id.clone();
        let ok = match seen.get(&blob) {
            Some(ok) => *ok,
            None => {
                let ok = available(reader, &blob)?;
                seen.insert(blob, ok);
                ok
            }
        };
        rows.push(AttachmentEntry {
            block: b.id.clone(),
            home_doc,
            blob,
            available: ok,
        });
    }
    rows.sort_by(|a, b| (&a.home_doc, &a.block, &a.blob).cmp(&(&b.home_doc, &b.block, &b.blob)));
    Ok(rows)
}

fn put(out: &mut Vec<u8>, part: &str) {
    out.extend_from_slice(part.as_bytes());
    out.push(0);
}

fn put_entry(out: &mut Vec<u8>, e: &SelectionEntry) {
    put(out, e.id.as_str());
    put(out, &e.version.to_string());
    put(
        out,
        &e.redaction
            .map_or_else(|| "-".to_string(), |h| h.to_string()),
    );
}

fn put_attachment(out: &mut Vec<u8>, a: &AttachmentEntry) {
    put(out, a.block.as_str());
    put(out, &a.blob.to_string());
    put(out, if a.available { "1" } else { "0" });
}

/// The ids of `id` and every block below it in `sel`, in id order.
fn subtree(sel: &Materialized, id: &BlockId) -> BTreeSet<BlockId> {
    let mut out = BTreeSet::new();
    let mut stack = vec![id.clone()];
    while let Some(cur) = stack.pop() {
        if out.insert(cur.clone()) {
            stack.extend(sel.children(Some(&cur)).iter().map(|c| c.id.clone()));
        }
    }
    out
}

fn resolve_in(
    pack: &Pack,
    reader: &StoreReader,
    m: &Materialized,
    name: &str,
    l: &Limits,
) -> Result<Resolved, RdyError> {
    // 1. the profile, in this snapshot only
    check_name(name, l)?;
    let (pid, fields) = find_profile(pack, m, name)?;
    let profile = ResolvedProfile {
        version: m.version_of(&pid),
        id: Some(pid),
        key: Some(fields.key),
        title: fields.title,
        description: fields.description,
        audience: fields.audience,
        trail: fields.trail,
    };

    // 2, 3. whole docs and folders, then their ancestor folders' own records
    let mut selectors = fields.select;
    selectors.sort();
    selectors.dedup();
    let keep = select(m, &selectors, l)?;

    // 5. the selected state: cloned records, their source versions
    let versions = keep
        .iter()
        .filter_map(|id| Some((id.clone(), m.version_of(id)?)))
        .collect();
    let blocks = keep
        .iter()
        .filter_map(|id| Some((id.clone(), m.get(id)?.clone())))
        .collect();
    let sel = Materialized::build(StateSnapshot { blocks: versions }, blocks);
    let references = references::scan(sel.iter(), m, &keep, profile.audience, l)?;
    let mut edges = 0usize;
    for b in sel.iter() {
        edges += rdy_graph::edges_of(b).len() + usize::from(b.parent.is_some());
        if edges > l.graph {
            return Err(limit("graph", l.graph, "edges"));
        }
    }
    let graph = Graph::build_with(sel.iter(), Arc::new(pack.tracking())).induced(&keep);

    // 6. attachments
    let attachments = attachments(reader, &sel, l)?;

    // 7. entries, paths and digests
    let entries: Vec<SelectionEntry> = sel
        .iter()
        .map(|b| SelectionEntry {
            id: b.id.clone(),
            version: m.version_of(&b.id).unwrap_or_else(|| b.version_hash()),
            redaction: b.redaction(),
        })
        .collect();
    let mut paths = BTreeMap::new();
    for b in sel.iter().filter(|b| tree::is_tree_type(&b.block_type)) {
        let path = sel.path_of(&b.id).ok_or_else(invalid_tree)?;
        if path.len() > l.path {
            return Err(limit("selected path", l.path, "bytes"));
        }
        paths.insert(b.id.clone(), path);
    }
    let by_id: BTreeMap<&BlockId, &SelectionEntry> = entries.iter().map(|e| (&e.id, e)).collect();
    let mut docs = vec![];
    for b in sel.iter().filter(|b| b.block_type == tree::DOC) {
        let path = paths.remove(&b.id).ok_or_else(invalid_tree)?;
        let mut bytes = b"rdy-publication-doc-1\0".to_vec();
        put(&mut bytes, &path);
        for id in subtree(&sel, &b.id) {
            put_entry(&mut bytes, by_id[&id]);
        }
        for a in attachments.iter().filter(|a| a.home_doc == b.id) {
            put_attachment(&mut bytes, a);
        }
        docs.push(DocEntry {
            id: b.id.clone(),
            path,
            digest: Hash::of(&bytes),
        });
    }
    let mut bytes = b"rdy-publication-selection-1\0".to_vec();
    put(
        &mut bytes,
        &profile
            .id
            .as_ref()
            .map_or_else(|| "-".to_string(), BlockId::to_string),
    );
    put(
        &mut bytes,
        &profile
            .version
            .map_or_else(|| "-".to_string(), |h| h.to_string()),
    );
    for e in &entries {
        put_entry(&mut bytes, e);
    }
    for a in &attachments {
        put_attachment(&mut bytes, a);
    }
    Ok(Resolved {
        profile,
        m: sel,
        graph,
        entries,
        docs,
        attachments,
        references,
        selection_hash: Hash::of(&bytes),
    })
}

/// The bytes of every distinct available attachment blob of `resolved`, by hash, read through
/// the store's own blob reader. Every manifest length is checked against the per-blob and total
/// bounds before any blob byte is read; an unavailable blob is skipped.
pub fn load_assets(
    read: &ReadState,
    resolved: &Resolved,
) -> Result<BTreeMap<Hash, Vec<u8>>, RdyError> {
    let l = limits();
    let reader = read.reader();
    let hashes: BTreeSet<Hash> = resolved
        .attachments
        .iter()
        .filter(|a| a.available)
        .map(|a| a.blob)
        .collect();
    let mut total = 0u64;
    for h in &hashes {
        let manifest: BlobManifest = reader.get_typed(kinds::BLOB_MANIFEST, h)?;
        if manifest.len > l.blob {
            return Err(limit("blob", l.blob, "bytes"));
        }
        total = total.saturating_add(manifest.len);
        if total > l.assets {
            return Err(limit("assets", l.assets, "bytes"));
        }
    }
    let mut out = BTreeMap::new();
    for h in hashes {
        BLOB_READS.with(|c| c.set(c.get().wrapping_add(1)));
        out.insert(h, reader.get_blob(&h)?);
    }
    Ok(out)
}
