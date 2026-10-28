//! Selection (D-48NYM, D-0VCQ5, D-KDFZ3): one profile, read in one snapshot, resolved to its
//! whole docs and folders, their ancestor folders' own records, the selected state and induced
//! graph, the digests, the attachments and the reference check. Nothing here is stored.

use super::references::{self, CheckStatus, ReferenceReport};
use super::source::{PublicationSource, ReadPublicationSource};
use crate::error::*;
use crate::materialize::Materialized;
use crate::{ReadState, Scope};
use rdy_graph::Graph;
use rdy_model::{Block, BlockId, Hash, StateSnapshot, tree};
use rdy_schema::publication::{self as schema, Audience};
use rdy_schema::{Issue, Pack, Severity};
use rdy_store::kinds::BlobManifest;
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
    static SELECTION_RESOLUTIONS: Cell<u64> = const { Cell::new(0) };
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

/// Tests only: how many selections [`resolve_in`] has expanded on this thread, the one place
/// every resolution (`resolve`, `resolve_inspection`, `resolve_revision`, `resolve_source`) goes
/// through. A metadata read ([`profile_at`]) is not one. A test can check that a caller resolves
/// only the selections it needs.
#[doc(hidden)]
pub fn selection_resolutions_for_tests() -> u64 {
    SELECTION_RESOLUTIONS.with(Cell::get)
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
/// `redacted` is true when a removal replaced the doc's own name or an ancestor folder's name in
/// that path with the block's id (D-3M81W).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocEntry {
    pub id: BlockId,
    pub path: String,
    pub digest: Hash,
    pub redacted: bool,
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
///
/// `selection_identity` (D-Z8Q4N, D-075QD) hashes each entry's id and version and nothing else,
/// so a removal, which keeps a block's recorded version, keeps it, while `selection_hash` moves.
/// `redactions` lists, sorted and once each, the approved removal requests this resolution
/// applies: those of the selected blocks and of the selected attachments' manifests and chunks.
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
    pub selection_identity: Hash,
    pub redactions: Vec<Hash>,
}

impl Resolved {
    /// What an output caller asks first: `E_PUBLICATION_REFERENCE` when the reference check of
    /// a public profile is blocked. An inspection reads `references` instead.
    pub fn require_output(&self) -> Result<(), RdyError> {
        self.references.require_output()
    }
}

/// The facts of one resolved profile that every surface shows the same way (D-91DZW): its stable
/// selector, its metadata, the complete id-sorted selection with its hash and identity, and the
/// status of its reference check. `selector` is the full profile id, or [`IMPLICIT_ORG`], whose
/// id, version and key are None.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileFacts {
    pub selector: String,
    pub id: Option<BlockId>,
    pub version: Option<Hash>,
    pub key: Option<String>,
    pub title: String,
    pub audience: Audience,
    pub trail: bool,
    pub selection: Vec<SelectionEntry>,
    pub selection_hash: Hash,
    pub selection_identity: Hash,
    pub references: CheckStatus,
}

/// The [`ProfileFacts`] of `r`, copied from its resolution: no title, selector or hash of its own.
pub fn profile_facts(r: &Resolved) -> ProfileFacts {
    ProfileFacts {
        selector: r
            .profile
            .id
            .as_ref()
            .map_or_else(|| IMPLICIT_ORG.to_string(), BlockId::to_string),
        id: r.profile.id.clone(),
        version: r.profile.version,
        key: r.profile.key.clone(),
        title: r.profile.title.clone(),
        audience: r.profile.audience,
        trail: r.profile.trail,
        selection: r.entries.clone(),
        selection_hash: r.selection_hash,
        selection_identity: r.selection_identity,
        references: r.references.status,
    }
}

/// The most distinct removal requests one resolution, or one comparison, lists.
pub(crate) const REDACTION_REQUESTS: usize = 1_000_000;

/// A sorted set of removal requests, bounded by [`REDACTION_REQUESTS`] before each new request
/// is kept; a request already held costs nothing.
#[derive(Default)]
pub(crate) struct RequestSet(BTreeSet<Hash>);

impl RequestSet {
    pub(crate) fn insert(&mut self, request: Hash) -> Result<(), RdyError> {
        if self.0.contains(&request) {
            return Ok(());
        }
        if self.0.len() >= REDACTION_REQUESTS {
            return Err(limit(
                "redaction request list",
                REDACTION_REQUESTS,
                "requests",
            ));
        }
        self.0.insert(request);
        Ok(())
    }

    pub(crate) fn into_sorted(self) -> Vec<Hash> {
        self.0.into_iter().collect()
    }
}

/// The selection identity of `entries` (D-075QD), which are in id order: ASCII
/// `rdy-publication-selection-identity`, one NUL, u32 LE 1, u32 LE entry count, then per entry
/// its u32 LE id length, its UTF-8 id bytes and its 32 raw version bytes. Nothing else enters:
/// no attachment row, removal mark, availability, title, path, selector or audience. A transient
/// read identity, not a stored format.
fn selection_identity(entries: &[SelectionEntry], l: &Limits) -> Result<Hash, RdyError> {
    let over = || limit("selection", l.selection, "blocks");
    let count = u32::try_from(entries.len()).map_err(|_| over())?;
    let mut h = blake3::Hasher::new();
    h.update(b"rdy-publication-selection-identity\0");
    h.update(&1u32.to_le_bytes());
    h.update(&count.to_le_bytes());
    for e in entries {
        let id = e.id.as_str().as_bytes();
        let len = u32::try_from(id.len()).map_err(|_| over())?;
        h.update(&len.to_le_bytes());
        h.update(id);
        h.update(&e.version.0);
    }
    Ok(Hash(*h.finalize().as_bytes()))
}

/// Whether a removal replaced the name of the doc or folder `id`, or of a folder above it, in
/// `m`: a removed version's stub names itself by its id (D-3M81W). Iterative and bounded by the
/// state's size, so a damaged chain cannot loop.
pub(crate) fn name_replaced(m: &Materialized, id: &BlockId) -> bool {
    let mut cur = m.get(id);
    for _ in 0..=m.len() {
        let Some(b) = cur else { break };
        if b.is_redacted() {
            return true;
        }
        cur = b.parent.as_ref().and_then(|p| m.get(p));
    }
    false
}

/// `profile` (a full profile id or an existing key) resolved in `scope`, which must be canon or
/// a landing (a tag resolves to one). The profile is the version that scope holds: a tag selects
/// what its own profile revision selected. A blocked check is returned, not refused. This is the
/// stable entry durable output resolves through; [`resolve_inspection`] also reads review scopes.
///
/// The exact value [`IMPLICIT_ORG`] names the implicit profile instead (D-48NYM, RULING F5): no
/// profile block, every block of the snapshot, a null id, version and key, the project's title
/// and description, and audience org (FW-PB9a). No key or id holds a colon, so it never names a
/// block; any other `implicit:` value is refused.
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
    resolve_scoped(read, scope, profile)
}

/// `E_BAD_INPUT` for a scope an inspection does not read: the in-flight scope, an empty scope
/// text or an empty view name (D-CWC91).
pub(crate) fn bad_inspection_scope() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "Publication check scope must be canon, tag:<name>, landing:<hash>, a view, or proposal:P-x@n.",
        "Use canon, tag:<name>, landing:<hash>, view:<name>, or proposal:P-x@n.",
    )
}

/// The scopes an inspection reads: canon, a landing, a view and a proposal version; the
/// in-flight scope merges every open view and selects no one state, so it is refused.
fn check_inspection_scope(scope: &Scope) -> Result<(), RdyError> {
    match scope {
        Scope::Canon | Scope::At(_) | Scope::View(_) | Scope::Proposal(_) => Ok(()),
        Scope::Inflight => Err(bad_inspection_scope()),
    }
}

/// [`resolve`] for an inspection (D-CWC91): the live site and the publication check also read a
/// view or a proposal version, with the same selection, bounds and errors. A blocked check is
/// returned, not refused; output still asks [`Resolved::require_output`]. The caller owns
/// whether a view exists ([`ReadState::resolve_read_scope`]).
pub fn resolve_inspection(
    read: &ReadState,
    scope: &Scope,
    profile: &str,
) -> Result<Resolved, RdyError> {
    check_inspection_scope(scope)?;
    resolve_scoped(read, scope, profile)
}

/// The revision of profile `id` that `scope` holds, resolved for an inspection: None only when
/// `scope`'s state holds no block `id`. A block of another type, a removed profile, invalid
/// fields or a missing selected doc are the errors [`resolve_inspection`] gives.
pub fn resolve_revision(
    read: &ReadState,
    scope: &Scope,
    id: &BlockId,
) -> Result<Option<Resolved>, RdyError> {
    check_inspection_scope(scope)?;
    let l = limits();
    check_name(id.as_str(), &l)?;
    let m = read.materialized(scope)?;
    if m.get(id).is_none() {
        return Ok(None);
    }
    with_read_source(read, |source| resolve_in(source, &m, id.as_str(), &l)).map(Some)
}

/// The metadata of `profile` in `scope`, an inspection scope, by the lookup every resolution
/// makes: the same name bound, key, type, parent, removal and field checks and errors, but no
/// selection is expanded and no blob is read. A valid result does not stand for the selection
/// and reference checks a build still makes.
pub fn profile_at(
    read: &ReadState,
    scope: &Scope,
    profile: &str,
) -> Result<ResolvedProfile, RdyError> {
    check_inspection_scope(scope)?;
    let l = limits();
    check_name(profile, &l)?;
    let m = read.materialized(scope)?;
    with_read_source(read, |source| {
        lookup(source, &m, profile, &l).map(|(p, _)| p)
    })
}

/// `profile` resolved in `scope`'s state through `read`'s store, with no scope rule.
fn resolve_scoped(read: &ReadState, scope: &Scope, profile: &str) -> Result<Resolved, RdyError> {
    check_name(profile, &limits())?;
    let m = read.materialized(scope)?;
    with_read_source(read, |source| resolve_in(source, &m, profile, &limits()))
}

/// `profile` (a full profile id or an existing key, or [`IMPLICIT_ORG`]) resolved in the
/// snapshot `m` through `source`, with no store open: the selection [`resolve`] makes, by the
/// same rules and with the same errors. `m` is checked whole by the snapshot limits a take keeps
/// before any of it is scanned; the caller owns the scope rule, which `m` alone cannot show.
pub fn resolve_source(
    source: &dyn PublicationSource,
    m: &Materialized,
    profile: &str,
) -> Result<Resolved, RdyError> {
    let l = limits();
    check_name(profile, &l)?;
    check_snapshot_limits(m.iter(), |h| source.manifest(h))?;
    resolve_in(source, m, profile, &l)
}

/// `f` over the store source of `read`, with the settings `read` captured when it was published.
fn with_read_source<T>(
    read: &ReadState,
    f: impl FnOnce(&dyn PublicationSource) -> Result<T, RdyError>,
) -> Result<T, RdyError> {
    let (title, description) = (read.project_name(), read.project_description());
    f(&ReadPublicationSource::with_settings(
        read,
        read.pack(),
        &title,
        description.as_deref(),
    ))
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
    with_read_source(read, |source| resolve_in(source, m, profile, &limits()))
}

fn check_name(profile: &str, l: &Limits) -> Result<(), RdyError> {
    if profile.len() > l.profile_name {
        return Err(limit("profile name", l.profile_name, "bytes"));
    }
    Ok(())
}

/// The request value that names the implicit profile, compared byte for byte (O10, FW-PB9a).
pub const IMPLICIT_ORG: &str = "implicit:org";

/// What a profile request names: the implicit profile with its audience, or a profile block.
enum Request {
    Implicit(Audience),
    Named,
}

/// The closed set of `implicit:` values: [`IMPLICIT_ORG`] alone; `implicit:public` and every
/// other value with the prefix are refused. Any other text names a profile block.
fn request(name: &str) -> Result<Request, RdyError> {
    match name {
        IMPLICIT_ORG => Ok(Request::Implicit(Audience::Org)),
        "implicit:public" => Err(RdyError::new(
            E_BAD_INPUT,
            "the implicit profile is always org (FW-PB9a)",
            "name an explicit publication profile whose audience is public",
        )),
        _ if name.starts_with("implicit:") => Err(RdyError::new(
            E_BAD_INPUT,
            format!("Implicit profile \"{name}\" is not known."),
            "Use implicit:org.",
        )),
        _ => Ok(Request::Named),
    }
}

/// The longest stable selector a take reads, in UTF-8 bytes; a longer one is refused before it
/// is parsed.
const STABLE_SELECTOR_BYTES: usize = 128;

/// The selector a publication take names its profile by (D-91DZW): a full profile block id or
/// [`IMPLICIT_ORG`], never a key, which a later landing may move. The implicit values keep the
/// request parser's refusals; any other text, or one over 128 bytes, is refused with one text the
/// render process shares. Only the text is checked: a well-formed id need name no block.
pub fn check_stable_selector(selector: &str) -> Result<(), RdyError> {
    let unstable = || {
        RdyError::new(
            E_BAD_INPUT,
            "A publication take names its profile by a full profile id or implicit:org.",
            "Pass the site's profile id, or implicit:org.",
        )
    };
    if selector.len() > STABLE_SELECTOR_BYTES {
        return Err(unstable());
    }
    match request(selector)? {
        Request::Implicit(_) => Ok(()),
        Request::Named if BlockId::parse(selector).is_ok() => Ok(()),
        Request::Named => Err(unstable()),
    }
}

fn not_found(name: &str) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("Publication profile \"{name}\" was not found."),
        "Use a publication profile id or key from this scope.",
    )
}

pub(crate) fn invalid_tree() -> RdyError {
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
/// the tree checked on the way up and then by [`check_tree`], all before any text is scanned.
/// Iterative: a damaged store may nest deep or loop.
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
    check_tree(&|id| m.get(id), &keep)?;
    Ok(keep)
}

/// The tree check every retained set passes before any text is scanned, for a named profile and
/// the implicit one alike. Every retained node's chain of parents reaches the top through
/// retained nodes, with no missing parent and no loop; then every retained node sits where the
/// tree allows: a doc or folder at the top or in a folder; a decision, a question, a finding or a
/// source anywhere its chain is rooted, at the top and in a folder too (D-P3YD5,
/// [`tree::may_sit_outside_a_doc`]); and any other block in a doc or in another such block. With
/// every chain rooted, the first tree node above other content is its home doc or, for content
/// under a block outside a doc, its folder or the top. Iterative, and each chain is walked once.
///
/// `get` reads a block of the state; a selection reads its source state, a snapshot check its
/// own blocks.
fn check_tree<'m>(
    get: &dyn Fn(&BlockId) -> Option<&'m Block>,
    keep: &BTreeSet<BlockId>,
) -> Result<(), RdyError> {
    let mut rooted: BTreeSet<&BlockId> = BTreeSet::new();
    for id in keep {
        let mut chain: BTreeSet<&BlockId> = BTreeSet::new();
        let mut cur = id;
        while !rooted.contains(cur) {
            if !chain.insert(cur) {
                return Err(invalid_tree());
            }
            match &get(cur).ok_or_else(invalid_tree)?.parent {
                Some(p) => cur = keep.get(p).ok_or_else(invalid_tree)?,
                None => break,
            }
        }
        rooted.extend(chain);
    }
    for id in keep {
        let b = get(id).ok_or_else(invalid_tree)?;
        let parent = match &b.parent {
            Some(p) => Some(get(p).ok_or_else(invalid_tree)?),
            None => None,
        };
        let placed = if tree::is_tree_type(&b.block_type) {
            parent.is_none_or(|p| p.block_type == tree::FOLDER)
        } else if tree::may_sit_outside_a_doc(&b.block_type) {
            true
        } else {
            parent.is_some_and(|p| p.block_type != tree::FOLDER)
        };
        if !placed {
            return Err(invalid_tree());
        }
    }
    Ok(())
}

/// Step 6: one row per selected figure whose `blob` field is a hash, sorted by (home doc, block,
/// blob). A removed figure's stub has no fields, so no row. The rows are bounded and sorted
/// first, then each distinct blob is checked once in that order, so the first damaged manifest
/// reported is the first row's. Availability is the source's: no blob byte is read.
fn attachments(
    source: &dyn PublicationSource,
    sel: &Materialized,
    l: &Limits,
) -> Result<Vec<AttachmentEntry>, RdyError> {
    let mut rows = vec![];
    for b in sel.iter() {
        let Some(blob) = attachment_blob(b) else {
            continue;
        };
        check_attachment_row(rows.len(), l)?;
        let home_doc = sel.doc_of(&b.id).ok_or_else(invalid_tree)?.id.clone();
        rows.push(AttachmentEntry {
            block: b.id.clone(),
            home_doc,
            blob,
            available: false,
        });
    }
    rows.sort_by(|a, b| (&a.home_doc, &a.block, &a.blob).cmp(&(&b.home_doc, &b.block, &b.blob)));
    let mut seen: BTreeMap<Hash, bool> = BTreeMap::new();
    for row in &mut rows {
        row.available = match seen.get(&row.blob) {
            Some(ok) => *ok,
            None => {
                let ok = source.blob_available(&row.blob)?;
                seen.insert(row.blob, ok);
                ok
            }
        };
    }
    Ok(rows)
}

/// The blob of `b`'s attachment row: a figure whose `blob` field is a hash. A removed figure's
/// stub has no fields, so no row.
fn attachment_blob(b: &Block) -> Option<Hash> {
    if b.block_type != "figure" || b.is_redacted() {
        return None;
    }
    b.field_str("blob").and_then(|t| t.parse::<Hash>().ok())
}

/// The bound on one more attachment row after `rows`.
fn check_attachment_row(rows: usize, l: &Limits) -> Result<(), RdyError> {
    if rows >= l.attachments {
        return Err(limit("attachments", l.attachments, "rows"));
    }
    Ok(())
}

/// The graph bound of `blocks`: each block's edges and its parent edge, counted before
/// induction.
fn check_graph<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    l: &Limits,
) -> Result<(), RdyError> {
    let mut edges = 0usize;
    for b in blocks {
        edges += rdy_graph::edges_of(b).len() + usize::from(b.parent.is_some());
        if edges > l.graph {
            return Err(limit("graph", l.graph, "edges"));
        }
    }
    Ok(())
}

/// The bound on one selected doc's or folder's path.
fn check_path(path: &str, l: &Limits) -> Result<(), RdyError> {
    if path.len() > l.path {
        return Err(limit("selected path", l.path, "bytes"));
    }
    Ok(())
}

/// The asset bounds of the distinct blobs `hashes`, in their order: each manifest's length, then
/// the running total, before any blob byte is read. `manifest` reads a blob's manifest; None
/// (a manifest that is missing) is skipped, and the caller owns completeness.
fn check_assets<'h>(
    hashes: impl IntoIterator<Item = &'h Hash>,
    manifest: &mut dyn FnMut(&Hash) -> Result<Option<BlobManifest>, RdyError>,
    l: &Limits,
) -> Result<(), RdyError> {
    let mut total = 0u64;
    for h in hashes {
        let Some(manifest) = manifest(h)? else {
            continue;
        };
        if manifest.len > l.blob {
            return Err(limit("blob", l.blob, "bytes"));
        }
        total = total.saturating_add(manifest.len);
        if total > l.assets {
            return Err(limit("assets", l.assets, "bytes"));
        }
    }
    Ok(())
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

/// Step 1: the profile `name` names in `m`, as metadata, with the selectors of a profile block
/// (None for the implicit profile, which selects every block). Reads no blob and selects nothing.
fn lookup(
    source: &dyn PublicationSource,
    m: &Materialized,
    name: &str,
    l: &Limits,
) -> Result<(ResolvedProfile, Option<Vec<BlockId>>), RdyError> {
    check_name(name, l)?;
    match request(name)? {
        Request::Implicit(audience) => Ok((
            ResolvedProfile {
                id: None,
                version: None,
                key: None,
                title: source.title().to_string(),
                description: source.description().map(str::to_string),
                audience,
                trail: audience == Audience::Org,
            },
            None,
        )),
        Request::Named => {
            // the profile, in this snapshot only
            let (pid, fields) = find_profile(source.pack(), m, name)?;
            Ok((
                ResolvedProfile {
                    version: m.version_of(&pid),
                    id: Some(pid),
                    key: Some(fields.key),
                    title: fields.title,
                    description: fields.description,
                    audience: fields.audience,
                    trail: fields.trail,
                },
                Some(fields.select),
            ))
        }
    }
}

fn resolve_in(
    source: &dyn PublicationSource,
    m: &Materialized,
    name: &str,
    l: &Limits,
) -> Result<Resolved, RdyError> {
    SELECTION_RESOLUTIONS.with(|c| c.set(c.get() + 1));
    let pack = source.pack();
    // 1. the profile, in this snapshot only
    let (profile, selectors) = lookup(source, m, name, l)?;
    let keep = match selectors {
        // the implicit profile reads no profile block and selects every block of the snapshot,
        // profile blocks included (O12); every ancestor is already selected, so the selector
        // and ancestor steps do not apply. The selection bound (O14) and the named profile's
        // tree check still do, before any text is scanned.
        None => {
            let mut keep = BTreeSet::new();
            for b in m.iter() {
                retain(&mut keep, &b.id, l)?;
            }
            check_tree(&|id| m.get(id), &keep)?;
            keep
        }
        // 2, 3. whole docs and folders, then their ancestor folders' own records
        Some(mut selectors) => {
            selectors.sort();
            selectors.dedup();
            select(m, &selectors, l)?
        }
    };

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
    let chosen = profile.id.as_ref().and_then(|id| m.get(id));
    let references = references::scan(sel.iter(), chosen, m, &keep, profile.audience, l)?;
    check_graph(sel.iter(), l)?;
    let graph = Graph::build_with(sel.iter(), Arc::new(pack.tracking())).induced(&keep);

    // 6. attachments, then the removal requests of the selected blocks and of the selected
    // attachments' manifests and chunks, each bounded before it is kept; no payload is read
    let attachments = attachments(source, &sel, l)?;
    let mut requests = RequestSet::default();
    for b in sel.iter() {
        if let Some(request) = b.redaction() {
            requests.insert(request)?;
        }
    }
    let blobs: BTreeSet<Hash> = attachments.iter().map(|a| a.blob).collect();
    for blob in &blobs {
        source.visit_blob_redactions(blob, &mut |request| requests.insert(request))?;
    }

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
        check_path(&path, l)?;
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
            redacted: name_replaced(&sel, &b.id),
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
    let selection_identity = selection_identity(&entries, l)?;
    Ok(Resolved {
        profile,
        m: sel,
        graph,
        entries,
        docs,
        attachments,
        references,
        selection_hash: Hash::of(&bytes),
        selection_identity,
        redactions: requests.into_sorted(),
    })
}

/// The bytes of every distinct available attachment blob of `resolved`, by hash, read through
/// the store's own blob reader: [`load_source_assets`] over `read`'s store.
pub fn load_assets(
    read: &ReadState,
    resolved: &Resolved,
) -> Result<BTreeMap<Hash, Vec<u8>>, RdyError> {
    with_read_source(read, |source| load_source_assets(source, resolved))
}

/// The bytes of every distinct available attachment blob of `resolved`, by hash, read through
/// `source`: the gated loader every durable output uses. A public profile whose reference check
/// is blocked is refused first ([`Resolved::require_output`]), before any manifest or byte is
/// read; then [`load_inspection_assets`].
pub fn load_source_assets(
    source: &dyn PublicationSource,
    resolved: &Resolved,
) -> Result<BTreeMap<Hash, Vec<u8>>, RdyError> {
    resolved.require_output()?;
    load_inspection_assets(source, resolved)
}

/// The bytes of every distinct available attachment blob of `resolved`, by hash, read through
/// `source`, with no output gate: the live inspection reads a blocked public selection's figures
/// and shows its warning (D-CWC91). Every manifest length is checked against the per-blob and
/// total bounds before any blob byte is read, and each blob's bytes against the same bounds
/// once read; an unavailable blob (one the store lacks or a removal masks) is skipped.
pub fn load_inspection_assets(
    source: &dyn PublicationSource,
    resolved: &Resolved,
) -> Result<BTreeMap<Hash, Vec<u8>>, RdyError> {
    let l = limits();
    let hashes: BTreeSet<Hash> = resolved
        .attachments
        .iter()
        .filter(|a| a.available)
        .map(|a| a.blob)
        .collect();
    check_assets(&hashes, &mut |h| source.manifest(h), &l)?;
    let mut out = BTreeMap::new();
    let mut total = 0u64;
    for h in hashes {
        BLOB_READS.with(|c| c.set(c.get().wrapping_add(1)));
        let bytes = source.blob(&h)?;
        // a source's bytes keep the bounds its manifest was checked against
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if len > l.blob {
            return Err(limit("blob", l.blob, "bytes"));
        }
        total = total.saturating_add(len);
        if total > l.assets {
            return Err(limit("assets", l.assets, "bytes"));
        }
        out.insert(h, bytes);
    }
    Ok(out)
}

/// The resource limits of one complete snapshot, `blocks`, by the rules a publication of the
/// whole snapshot keeps, in the same order and with the same errors: the selection bound, the
/// document tree, the text and reference bounds, the graph, the attachment rows, the doc and
/// folder paths, then each distinct attachment blob's length and their total. `manifest` reads a
/// blob's manifest; None means the manifest is missing, and the caller owns completeness.
///
/// The references are checked as the implicit profile checks them, with the snapshot as both
/// source and selection: every occurrence counts, and only one whose target lies outside the
/// snapshot is a row, with its location and token bounded. So a snapshot Publications
/// publishes whole passes, and one it refuses is refused with its error. It reads no store,
/// pack or project path.
#[allow(
    dead_code,
    reason = "the holder take calls it for every landing snapshot it takes"
)]
pub(crate) fn check_snapshot_limits<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    mut manifest: impl FnMut(&Hash) -> Result<Option<BlobManifest>, RdyError>,
) -> Result<(), RdyError> {
    let l = limits();
    let mut keep = BTreeSet::new();
    let mut by_id: BTreeMap<&BlockId, &Block> = BTreeMap::new();
    for b in blocks {
        // two records of one block in one snapshot are no tree
        if !retain(&mut keep, &b.id, &l)? {
            return Err(invalid_tree());
        }
        by_id.insert(&b.id, b);
    }
    check_tree(&|id| by_id.get(id).copied(), &keep)?;
    // the reference check of the implicit profile: the snapshot is the source and the selection
    references::scan(
        by_id.values().copied(),
        None,
        &by_id,
        &keep,
        Audience::Org,
        &l,
    )?;
    check_graph(by_id.values().copied(), &l)?;
    let mut rows = 0usize;
    let mut blobs = BTreeSet::new();
    for b in by_id.values() {
        if let Some(blob) = attachment_blob(b) {
            check_attachment_row(rows, &l)?;
            rows += 1;
            blobs.insert(blob);
        }
    }
    // paths name docs and folders only, and with the tree checked a doc's or folder's ancestors
    // are folders: the docs and folders alone give every path
    let tree_blocks: BTreeMap<BlockId, Block> = by_id
        .values()
        .filter(|b| tree::is_tree_type(&b.block_type))
        .map(|b| (b.id.clone(), (*b).clone()))
        .collect();
    let sel = Materialized::build(StateSnapshot::default(), tree_blocks);
    for b in sel.iter() {
        check_path(&sel.path_of(&b.id).ok_or_else(invalid_tree)?, &l)?;
    }
    check_assets(&blobs, &mut manifest, &l)
}

/// The resources of one snapshot counted block by block while its blocks are read, before each
/// one is kept: the selection bound (one block per entry), the text and reference bounds, and
/// the graph, each refused with the error [`check_snapshot_limits`] gives for it. A reader that
/// charges every block before it keeps it never holds a snapshot past these bounds, whatever
/// the snapshot's size. A snapshot over two bounds may be refused for another one first than
/// the whole check would name; either way it is refused.
pub(crate) struct SnapshotBudget {
    l: Limits,
    blocks: usize,
    meter: references::ResourceMeter,
    edges: usize,
}

impl SnapshotBudget {
    pub(crate) fn new() -> SnapshotBudget {
        let l = limits();
        SnapshotBudget {
            l,
            blocks: 0,
            meter: references::ResourceMeter::new(&l),
            edges: 0,
        }
    }

    /// Counts block `b`, one more entry of the snapshot.
    pub(crate) fn charge(&mut self, b: &Block) -> Result<(), RdyError> {
        if self.blocks >= self.l.selection {
            return Err(limit("selection", self.l.selection, "blocks"));
        }
        self.blocks += 1;
        self.meter.charge(b)?;
        self.edges = self
            .edges
            .saturating_add(rdy_graph::edges_of(b).len() + usize::from(b.parent.is_some()));
        if self.edges > self.l.graph {
            return Err(limit("graph", self.l.graph, "edges"));
        }
        Ok(())
    }
}

/// The limits block `b` keeps on its own, whatever snapshot holds it: its text bounds, its
/// reference occurrences, its field depth and its graph edges. A block over one of them puts
/// every snapshot that holds it over the same limit, so a receiver can refuse it at once. A
/// reference row's location and token bounds depend on whether the snapshot holds the target,
/// so [`check_snapshot_limits`] checks them.
pub(crate) fn check_block_limits(b: &Block) -> Result<(), RdyError> {
    let l = limits();
    references::check_resources(std::iter::once(b), &l)?;
    check_graph(std::iter::once(b), &l)
}

/// Tests only: [`check_snapshot_limits`].
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn check_snapshot_limits_for_tests<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    manifest: impl FnMut(&Hash) -> Result<Option<BlobManifest>, RdyError>,
) -> Result<(), RdyError> {
    check_snapshot_limits(blocks, manifest)
}
