//! Briefings (FW-E1, Q6a, Q6b): what a writer must know before changing a set of blocks, and a
//! token that names exactly the versions it was computed from. A briefing covers blocks (by id,
//! or every block under a doc or folder path); its token changes when, and only when, a covered
//! block or one of their dependencies changes version. Registered briefings live in memory
//! (`BriefRegistry`), one registry per open project.
use crate::error::*;
use crate::keys;
use crate::query::summary_in;
use crate::write::OpInput;
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{DerivedStatus, EdgeKind, IMPACT_LINKS, SUSPECT_LINKS, Suspect};
use rdy_model::{Block, BlockId, FieldValue, Hash};
use rdy_schema::{Issue, Pack, Severity, Trait};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

/// A write to stable content without a briefing, or with one that does not cover a subject.
pub const E_BRIEF_REQUIRED: &str = "E_BRIEF_REQUIRED";
/// A write citing a briefing the registry does not know, or one whose recorded blocks moved.
pub const E_BRIEF_STALE: &str = "E_BRIEF_STALE";
/// A draft-only write citing an unknown or stale briefing (no refusal).
pub const W_BRIEF_STALE: &str = "W_BRIEF_STALE";
/// The most briefings one registry holds; the least recently used goes first.
pub const BRIEF_REGISTRY_CAP: usize = 10_000;
/// The most entries (covered plus recorded blocks, summed over its briefings) one registry
/// holds; the least recently used briefing goes first. An entry costs on the order of 100 bytes,
/// so this bounds a registry near 100 MB however large its briefings are.
pub const BRIEF_REGISTRY_ENTRIES: usize = 1_000_000;
/// The most blocks one briefing covers.
pub const MAX_BRIEF_BLOCKS: usize = 5_000;
/// A briefing's size budget when none is given, in tokens (4 bytes of JSON each).
pub const BRIEF_DEFAULT_TOKENS: usize = 4_000;

/// What to brief: blocks by id (or key or path), and a doc or folder path whose whole subtree is
/// covered. The coverage is their union.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Coverage {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// What a registered briefing vouches for: the blocks it covers and the version of every
/// recorded block (covered blocks and their dependencies; `None` for a block that is missing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefRecord {
    pub token: Hash,
    pub covers: BTreeSet<BlockId>,
    pub recorded: BTreeMap<BlockId, Option<Hash>>,
}

impl BriefRecord {
    pub fn covers(&self, id: &BlockId) -> bool {
        self.covers.contains(id)
    }

    /// The recorded blocks whose version in `m` is not the recorded one, in id order.
    pub fn moved(&self, m: &Materialized) -> Vec<BlockId> {
        self.recorded
            .iter()
            .filter(|(id, v)| m.version_of(id) != **v)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn token_hex(&self) -> String {
        self.token.to_string()
    }

    /// What this record weighs against a registry's entry budget.
    fn entries(&self) -> usize {
        self.covers.len() + self.recorded.len()
    }
}

/// The briefing token (exact): BLAKE3 of `rdy-brief-1\n` and, for each recorded block in id
/// order, `<id>\t<version hex or "-">\t<c|d>\n` (`c` covered, `d` dependency only).
pub fn token_of(covers: &BTreeSet<BlockId>, recorded: &BTreeMap<BlockId, Option<Hash>>) -> Hash {
    let mut bytes = b"rdy-brief-1\n".to_vec();
    for (id, version) in recorded {
        let version = version.map_or_else(|| "-".to_string(), |h| h.to_string());
        let role = if covers.contains(id) { 'c' } else { 'd' };
        bytes.extend_from_slice(format!("{id}\t{version}\t{role}\n").as_bytes());
    }
    Hash::of(&bytes)
}

/// The registered briefings, by token: at most `cap` of them and at most `entry_budget` entries
/// (`covers` plus `recorded`, summed), the least recently inserted or read dropped first. The
/// briefing inserted last is always kept. In memory only: gone when the project closes.
pub struct BriefRegistry {
    cap: usize,
    entry_budget: usize,
    inner: Mutex<Lru>,
}

#[derive(Default)]
struct Lru {
    /// The next recency stamp.
    clock: u64,
    by_token: HashMap<Hash, (u64, Arc<BriefRecord>)>,
    /// Stamp → token, oldest first.
    order: BTreeMap<u64, Hash>,
    /// The entries of every record held.
    entries: usize,
}

impl Lru {
    /// Stores `r` as the most recently used, replacing any entry for its token.
    fn put(&mut self, r: Arc<BriefRecord>) {
        let (stamp, token) = (self.clock, r.token);
        self.clock += 1;
        self.entries += r.entries();
        if let Some((old, prev)) = self.by_token.insert(token, (stamp, r)) {
            self.order.remove(&old);
            self.entries -= prev.entries();
        }
        self.order.insert(stamp, token);
    }

    /// Drops the least recently used record; false when none is left.
    fn evict(&mut self) -> bool {
        let Some((_, oldest)) = self.order.pop_first() else {
            return false;
        };
        if let Some((_, r)) = self.by_token.remove(&oldest) {
            self.entries -= r.entries();
        }
        true
    }

    /// `token`'s record, marked most recently used.
    fn touch(&mut self, token: &Hash) -> Option<Arc<BriefRecord>> {
        let stamp = self.clock;
        let (at, rec) = self.by_token.get_mut(token)?;
        self.clock += 1;
        let old = std::mem::replace(at, stamp);
        let rec = Arc::clone(rec);
        self.order.remove(&old);
        self.order.insert(stamp, *token);
        Some(rec)
    }
}

impl BriefRegistry {
    /// A registry of at most `cap` briefings and `BRIEF_REGISTRY_ENTRIES` entries.
    pub fn new(cap: usize) -> BriefRegistry {
        BriefRegistry::with_entry_budget(cap, BRIEF_REGISTRY_ENTRIES)
    }

    /// A registry of at most `cap` briefings and `entry_budget` entries.
    pub fn with_entry_budget(cap: usize, entry_budget: usize) -> BriefRegistry {
        BriefRegistry {
            cap,
            entry_budget,
            inner: Mutex::new(Lru::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Lru> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers `r` as the most recently used; an existing token is refreshed.
    pub fn insert(&self, r: Arc<BriefRecord>) {
        let mut lru = self.lock();
        lru.put(r);
        while lru.by_token.len() > 1
            && (lru.by_token.len() > self.cap || lru.entries > self.entry_budget)
        {
            if !lru.evict() {
                break;
            }
        }
    }

    /// The briefing whose token is `token` (64 lowercase hex), marked most recently used.
    pub fn get(&self, token: &str) -> Option<Arc<BriefRecord>> {
        let token: Hash = token.parse().ok()?;
        self.lock().touch(&token)
    }

    pub fn len(&self) -> usize {
        self.lock().by_token.len()
    }

    /// The entries (`covers` plus `recorded`) of every briefing held.
    pub fn entries(&self) -> usize {
        self.lock().entries
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a dependent should do when the block it depends on changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowUp {
    Update,
    Confirm,
    Reaffirm,
    /// Declared for M2c3 and M2d2; never produced here.
    Resolve,
}

/// The follow-up a dependent block owes when a block it depends on changed. `status` is the
/// dependent's derived decision status (`None` for a block that is not a decision);
/// `target_deleted` says the changed block no longer exists. `None` means no follow-up at all.
///
/// Exact, in order:
/// 1. a decision that asserts nothing live (rejected, revoked or fully superseded): `None`;
/// 2. a live or partially superseded decision: `Reaffirm`, whatever the target did;
/// 3. a proposed decision (mutable): `Update`;
/// 4. any other block while the target is deleted (a repin cannot pin to a block that is gone):
///    `Update`;
/// 5. a `normative` or `defines` type, or a `table` marked `normative: true`: `Update`;
/// 6. anything else (informative, structural, no classifying trait, asset): `Confirm`.
pub fn follow_up(
    pack: &Pack,
    dependent: &Block,
    status: Option<&DerivedStatus>,
    target_deleted: bool,
) -> Option<FollowUp> {
    match status {
        Some(
            DerivedStatus::Rejected
            | DerivedStatus::Superseded { .. }
            | DerivedStatus::Revoked { .. },
        ) => return None,
        Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. }) => {
            return Some(FollowUp::Reaffirm);
        }
        Some(DerivedStatus::Proposed) => return Some(FollowUp::Update),
        None => {}
    }
    if target_deleted {
        return Some(FollowUp::Update);
    }
    let classifying = pack
        .block_types
        .get(&dependent.block_type)
        .is_some_and(|t| {
            t.traits.contains(&Trait::Normative) || t.traits.contains(&Trait::Defines)
        });
    let normative_table = dependent.block_type == "table"
        && dependent.fields.get("normative") == Some(&FieldValue::Bool(true));
    Some(if classifying || normative_table {
        FollowUp::Update
    } else {
        FollowUp::Confirm
    })
}

/// A live decision governing covered blocks.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Governs {
    pub decision: String,
    pub status: String,
    pub statement: String,
    /// The covered blocks it governs, in id order.
    pub blocks: Vec<String>,
}

/// An uncovered block that depends on a covered one (`on`) through `kind`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Dependent {
    pub block: String,
    pub kind: String,
    pub on: String,
    pub summary: String,
}

/// An uncovered block that pins a covered one (`on`) and goes suspect when it changes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WillGoStale {
    pub block: String,
    pub via: String,
    pub on: String,
    pub follow_up: FollowUp,
}

/// The token and the versions it vouches for.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Base {
    pub token: String,
    /// Every covered block (unless cut), in id order.
    pub blocks: Vec<BaseBlock>,
    /// How many recorded blocks are dependencies only.
    pub depends: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BaseBlock {
    pub id: String,
    pub version: String,
}

/// One briefing over a set of blocks. Registering it (`register`) makes its token citable.
#[derive(Clone, Debug, Serialize)]
pub struct Briefing {
    pub covers: usize,
    pub governs: Vec<Governs>,
    pub dependents: Vec<Dependent>,
    pub will_go_stale: Vec<WillGoStale>,
    pub suspects: Vec<Suspect>,
    pub base: Base,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, usize>,
    #[serde(skip)]
    pub record: Arc<BriefRecord>,
}

impl Briefing {
    /// Makes this briefing's token known to `reg`.
    pub fn register(&self, reg: &BriefRegistry) {
        reg.insert(Arc::clone(&self.record));
    }

    /// The JSON size in tokens (4 bytes each).
    fn tokens(&self) -> usize {
        serde_json::to_string(self)
            .expect("a briefing serializes: strings, numbers and string-keyed maps only")
            .len()
            / 4
    }

    /// Halves the longest detail list (ties: governs, dependents, will_go_stale, suspects,
    /// base.blocks) until the briefing fits `max` tokens or every list is empty. Coverage, the
    /// token and the record are never cut.
    fn cut(&mut self, max: usize) {
        while self.tokens() > max {
            let lens = [
                self.governs.len(),
                self.dependents.len(),
                self.will_go_stale.len(),
                self.suspects.len(),
                self.base.blocks.len(),
            ];
            let longest = lens.iter().copied().max().unwrap_or(0);
            if longest == 0 {
                break;
            }
            let at = lens
                .iter()
                .position(|&n| n == longest)
                .expect("the max is in the list");
            let keep = longest / 2;
            let name = match at {
                0 => {
                    self.governs.truncate(keep);
                    "governs"
                }
                1 => {
                    self.dependents.truncate(keep);
                    "dependents"
                }
                2 => {
                    self.will_go_stale.truncate(keep);
                    "will_go_stale"
                }
                3 => {
                    self.suspects.truncate(keep);
                    "suspects"
                }
                _ => {
                    self.base.blocks.truncate(keep);
                    "base.blocks"
                }
            };
            *self.omitted.entry(name.to_string()).or_default() += longest - keep;
            self.truncated = true;
        }
    }
}

/// How a scope is named in a briefing's errors.
fn scope_label(scope: &Scope) -> String {
    match scope {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => format!("view {v}"),
        Scope::At(h) => format!("landing:{h}"),
    }
}

/// A references edge, or a link named in `names`.
fn edge_in(kind: &EdgeKind, names: &[&str]) -> bool {
    match kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => names.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

impl ReadState {
    /// Briefs the blocks `cov` names in `scope`: each id, and the block at `path` with every
    /// descendant. Nothing is registered (see `Briefing::register`).
    pub fn brief(
        &self,
        scope: &Scope,
        cov: &Coverage,
        max_tokens: Option<usize>,
    ) -> Result<Briefing, RdyError> {
        let m = self.materialized(scope)?;
        let known = |text: &str| -> Result<BlockId, RdyError> {
            let id = self.resolve_id(scope, text)?;
            if m.get(&id).is_none() {
                return Err(RdyError::new(
                    E_UNKNOWN_BLOCK,
                    format!("no block {id} in {}", scope_label(scope)),
                    "check the id with rdy_read",
                ));
            }
            Ok(id)
        };
        // refused as soon as the coverage passes the limit: the walk stops there
        let too_many = || {
            RdyError::new(
                E_LIMIT,
                format!(
                    "more than {MAX_BRIEF_BLOCKS} blocks to brief; the limit is {MAX_BRIEF_BLOCKS}"
                ),
                "brief a smaller part: a doc, or the blocks you will change",
            )
        };
        let mut ids = BTreeSet::new();
        for text in &cov.ids {
            ids.insert(known(text)?);
            if ids.len() > MAX_BRIEF_BLOCKS {
                return Err(too_many());
            }
        }
        if let Some(path) = &cov.path {
            let root = known(path)?;
            // the walk keeps its own visited set: a block also named in `ids` is still walked,
            // and counts once (`extra` counts the walked blocks not in `ids`)
            let mut extra = usize::from(!ids.contains(&root));
            let mut seen = BTreeSet::from([root.clone()]);
            let mut todo = vec![root];
            while let Some(cur) = todo.pop() {
                if ids.len() + extra > MAX_BRIEF_BLOCKS {
                    return Err(too_many());
                }
                for c in m.children(Some(&cur)) {
                    // a block already seen is not walked again (a damaged store may loop)
                    if seen.insert(c.id.clone()) {
                        extra += usize::from(!ids.contains(&c.id));
                        todo.push(c.id.clone());
                    }
                }
            }
            if ids.len() + extra > MAX_BRIEF_BLOCKS {
                return Err(too_many());
            }
            ids.extend(seen);
        }
        if ids.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "give ids or path",
                "rdy_brief takes ids (blocks) or path (a doc or folder)",
            ));
        }
        self.brief_ids(scope, &ids, max_tokens)
    }

    /// Briefs exactly `ids` in `scope` and cuts the result to `max_tokens` (default
    /// `BRIEF_DEFAULT_TOKENS`). Nothing is registered (see `Briefing::register`).
    pub fn brief_ids(
        &self,
        scope: &Scope,
        ids: &BTreeSet<BlockId>,
        max_tokens: Option<usize>,
    ) -> Result<Briefing, RdyError> {
        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        let pack = self.pack();
        let summary_of = |id: &BlockId| m.get(id).map(|b| summary_in(b, &m)).unwrap_or_default();

        let mut recorded = BTreeMap::new();
        for id in ids {
            recorded.insert(id.clone(), m.version_of(id));
            for e in g.outgoing(id) {
                if edge_in(&e.kind, &SUSPECT_LINKS) {
                    recorded.insert(e.to.clone(), m.version_of(&e.to));
                }
            }
        }
        let token = token_of(ids, &recorded);

        let mut governed: BTreeMap<BlockId, Vec<String>> = BTreeMap::new();
        for id in ids {
            for d in g.governing(id) {
                governed.entry(d).or_default().push(id.to_string());
            }
        }
        let governs = governed
            .into_iter()
            .map(|(d, blocks)| {
                let statement = m
                    .get(&d)
                    .map(
                        |b| match b.field_str("statement").filter(|s| !s.is_empty()) {
                            Some(s) => s.to_string(),
                            None => summary_in(b, &m),
                        },
                    )
                    .unwrap_or_default();
                Governs {
                    decision: d.to_string(),
                    status: g
                        .status(&d)
                        .as_ref()
                        .map(DerivedStatus::tag)
                        .unwrap_or_default()
                        .into(),
                    statement,
                    blocks,
                }
            })
            .collect();

        // keyed by (on, block, kind): deduplicated and in order
        let mut dependents: BTreeMap<(BlockId, BlockId, String), ()> = BTreeMap::new();
        let mut stale: BTreeMap<(BlockId, BlockId, String), FollowUp> = BTreeMap::new();
        for id in ids {
            for e in g.incoming(id) {
                if ids.contains(&e.from) {
                    continue;
                }
                let key = (id.clone(), e.from.clone(), e.kind.name());
                if edge_in(&e.kind, &IMPACT_LINKS) {
                    dependents.insert(key.clone(), ());
                }
                if edge_in(&e.kind, &SUSPECT_LINKS)
                    && e.pin.is_some()
                    && let Some(from) = m.get(&e.from)
                    && let Some(f) =
                        follow_up(pack, from, g.status(&e.from).as_ref(), m.get(id).is_none())
                {
                    stale.insert(key, f);
                }
            }
        }
        let dependents = dependents
            .into_keys()
            .map(|(on, block, kind)| Dependent {
                summary: summary_of(&block),
                block: block.to_string(),
                kind,
                on: on.to_string(),
            })
            .collect();
        let will_go_stale = stale
            .into_iter()
            .map(|((on, block, via), follow_up)| WillGoStale {
                block: block.to_string(),
                via,
                on: on.to_string(),
                follow_up,
            })
            .collect();

        let mut suspects = vec![];
        for id in ids {
            let mut found = g.suspects_of(id, &*m, &|h| self.reader().get_block(h).ok());
            found.retain(|s| !self.is_presentational_source(&m, s));
            suspects.extend(found);
        }
        suspects.sort_by(|a, b| (&a.block, &a.target).cmp(&(&b.block, &b.target)));

        let base = Base {
            token: token.to_string(),
            blocks: ids
                .iter()
                .map(|id| BaseBlock {
                    id: id.to_string(),
                    version: m
                        .version_of(id)
                        .map_or_else(|| "-".to_string(), |h| h.to_string()),
                })
                .collect(),
            depends: recorded.len() - ids.len(),
        };
        let mut briefing = Briefing {
            covers: ids.len(),
            governs,
            dependents,
            will_go_stale,
            suspects,
            base,
            truncated: false,
            omitted: BTreeMap::new(),
            record: Arc::new(BriefRecord {
                token,
                covers: ids.clone(),
                recorded,
            }),
        };
        briefing.cut(max_tokens.unwrap_or(BRIEF_DEFAULT_TOKENS));
        Ok(briefing)
    }
}

// ---- bwr1: briefings on the write path ----

/// The lifecycle that counts as stable.
const STABLE: &str = "stable";

/// The block of `m` an op's parent text names, as the pre-pass sees it.
#[derive(Clone, Debug)]
enum Anchor {
    /// The top level: no parent.
    Top,
    /// The nearest existing ancestor.
    At(BlockId),
    /// The text names nothing in `m` (or this batch): `build_ops` reports it.
    Unknown,
}

/// What one op is about: the block a briefing must cover, and whether the op touches stable
/// content.
#[derive(Clone, Debug)]
struct Subject {
    target: BlockId,
    stable: bool,
}

/// A block's own `lifecycle`, if it has one (a null value counts as none).
fn own_lifecycle(b: &Block) -> Option<&FieldValue> {
    b.fields
        .get("lifecycle")
        .filter(|v| **v != FieldValue::Null)
}

/// The `lifecycle` of `id` or of its nearest ancestor that has one is `stable`.
fn effective_stable(m: &Materialized, id: Option<&BlockId>) -> bool {
    let mut cur = id.and_then(|i| m.get(i));
    // a damaged store may loop: never walk more steps than there are blocks
    let mut steps = 0;
    while let Some(b) = cur {
        if let Some(l) = own_lifecycle(b) {
            return l.as_str() == Some(STABLE);
        }
        steps += 1;
        if steps > m.len() {
            break;
        }
        cur = b.parent.as_ref().and_then(|p| m.get(p));
    }
    false
}

/// The subjects of `ops` against `m` (the target view before the write), in op order. Ops whose
/// block does not resolve are skipped; a subject of a presentational type is exempt (Q6d), and
/// for `create` and `rebalance` the subject is the parent.
fn subjects(pack: &Pack, m: &Materialized, ops: &[OpInput]) -> Vec<Subject> {
    // the anchor each create of this batch hangs from, by the id it was given ($name or an id)
    let mut created: HashMap<&str, Anchor> = HashMap::new();
    let existing = |text: &str| -> Option<BlockId> {
        keys::resolve_text(m, text)
            .ok()
            .filter(|id| m.get(id).is_some())
    };
    let anchor = |created: &HashMap<&str, Anchor>, text: Option<&str>| -> Anchor {
        match text {
            None => Anchor::Top,
            Some(t) => match created.get(t) {
                Some(a) => a.clone(),
                None => existing(t).map_or(Anchor::Unknown, Anchor::At),
            },
        }
    };
    let exempt = |b: &Block| pack.is_presentational(&b.block_type);
    let mut out = vec![];
    for op in ops {
        match op {
            OpInput::Create { id, parent, .. } => {
                // the subject is the parent's nearest existing ancestor (the coverage target),
                // so its type decides the exemption; the new block's own type does not count
                let a = anchor(&created, parent.as_deref());
                if let Some(id) = id {
                    created.insert(id.as_str(), a.clone());
                }
                if let Anchor::At(p) = a
                    && m.get(&p).is_some_and(|b| !exempt(b))
                {
                    out.push(Subject {
                        stable: effective_stable(m, Some(&p)),
                        target: p,
                    });
                }
            }
            OpInput::Update {
                id, fields, unset, ..
            } => {
                let Some(b) = existing(id).and_then(|i| m.get(&i)) else {
                    continue;
                };
                if exempt(b) {
                    continue;
                }
                let before = effective_stable(m, Some(&b.id));
                let after = if let Some(l) = fields.get("lifecycle") {
                    l.as_str() == Some(STABLE)
                } else if unset.iter().any(|u| u == "lifecycle") {
                    effective_stable(m, b.parent.as_ref())
                } else {
                    before
                };
                out.push(Subject {
                    target: b.id.clone(),
                    stable: before || after,
                });
            }
            OpInput::Move { id, parent, .. } => {
                let Some(b) = existing(id).and_then(|i| m.get(&i)) else {
                    continue;
                };
                if exempt(b) {
                    continue;
                }
                let before = effective_stable(m, Some(&b.id));
                let after = match own_lifecycle(b) {
                    Some(l) => l.as_str() == Some(STABLE),
                    None => match anchor(&created, parent.as_deref()) {
                        Anchor::At(p) => effective_stable(m, Some(&p)),
                        Anchor::Top | Anchor::Unknown => false,
                    },
                };
                out.push(Subject {
                    target: b.id.clone(),
                    stable: before || after,
                });
            }
            OpInput::Delete { id, .. } | OpInput::Repin { id, .. } => {
                let Some(b) = existing(id).and_then(|i| m.get(&i)) else {
                    continue;
                };
                if exempt(b) {
                    continue;
                }
                out.push(Subject {
                    target: b.id.clone(),
                    stable: effective_stable(m, Some(&b.id)),
                });
            }
            OpInput::Rebalance { parent } => match anchor(&created, parent.as_deref()) {
                Anchor::At(p) => {
                    if m.get(&p).is_some_and(|b| !exempt(b)) {
                        out.push(Subject {
                            stable: effective_stable(m, Some(&p)),
                            target: p,
                        });
                    }
                }
                Anchor::Top => {
                    // a top-level rebalance touches every stable top-level block
                    for b in m.children(None) {
                        if !exempt(b)
                            && own_lifecycle(b).and_then(FieldValue::as_str) == Some(STABLE)
                        {
                            out.push(Subject {
                                target: b.id.clone(),
                                stable: true,
                            });
                        }
                    }
                }
                Anchor::Unknown => {}
            },
        }
    }
    out
}

fn joined(ids: &BTreeSet<BlockId>) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first 8 characters of a cited token, as given.
fn short_token(t: &str) -> String {
    t.chars().take(8).collect()
}

/// bwr1: checks that `ops`, about to be written to the view `m` materializes, cite a current
/// briefing where they touch stable content. Writes that touch only drafts are never refused;
/// an unknown or stale cited briefing is a `W_BRIEF_STALE` warning on them. A refusal names, in
/// `briefing.ids`, every block a fresh briefing must cover (`expand_error` computes it).
pub(crate) fn check(
    pack: &Pack,
    reg: &BriefRegistry,
    m: &Materialized,
    ops: &[OpInput],
    brief: Option<&str>,
) -> Result<Vec<Issue>, RdyError> {
    let subjects = subjects(pack, m, ops);
    let stable: BTreeSet<BlockId> = subjects
        .iter()
        .filter(|s| s.stable)
        .map(|s| s.target.clone())
        .collect();
    if stable.is_empty() {
        let Some(t) = brief else {
            return Ok(vec![]);
        };
        let message = match reg.get(t) {
            None => Some(format!(
                "the cited briefing {} is unknown; this write touches only drafts, so it went ahead",
                short_token(t)
            )),
            Some(rec) => {
                let moved: BTreeSet<BlockId> = rec.moved(m).into_iter().collect();
                (!moved.is_empty()).then(|| {
                    format!(
                        "the cited briefing is stale: {} moved since it was made; this write touches only drafts, so it went ahead",
                        joined(&moved)
                    )
                })
            }
        };
        return Ok(message
            .map(|message| Issue {
                severity: Severity::Warning,
                code: W_BRIEF_STALE,
                block: None,
                message,
                fix: "brief again before the next write if the moved blocks matter".into(),
            })
            .into_iter()
            .collect());
    }
    let ids: BTreeSet<String> = subjects.iter().map(|s| s.target.to_string()).collect();
    let refuse = |code: &'static str, message: String, issues: Vec<Issue>| {
        let mut e = RdyError::new(
            code,
            message,
            "brief these blocks (rdy_brief) and retry with the token",
        );
        e.issues = issues;
        e.briefing = Some(serde_json::json!({ "ids": ids }));
        e
    };
    let block_issue = |id: &BlockId, message: &str, fix: &str| Issue {
        severity: Severity::Error,
        code: E_BRIEF_REQUIRED,
        block: Some(id.clone()),
        message: message.into(),
        fix: fix.into(),
    };
    let Some(t) = brief else {
        return Err(refuse(
            E_BRIEF_REQUIRED,
            format!(
                "this write touches stable content ({}) and cites no briefing",
                joined(&stable)
            ),
            vec![],
        ));
    };
    let Some(rec) = reg.get(t) else {
        return Err(refuse(
            E_BRIEF_STALE,
            format!(
                "the cited briefing {} is unknown: the daemon restarted, or the briefing was evicted",
                short_token(t)
            ),
            vec![],
        ));
    };
    let missing: BTreeSet<BlockId> = stable.into_iter().filter(|id| !rec.covers(id)).collect();
    if !missing.is_empty() {
        let issues = missing
            .iter()
            .map(|id| {
                block_issue(
                    id,
                    "not covered by the cited briefing",
                    "brief it (or, to create under it, its parent)",
                )
            })
            .collect();
        return Err(refuse(
            E_BRIEF_REQUIRED,
            format!(
                "the cited briefing does not cover {}, which this write touches on stable content",
                joined(&missing)
            ),
            issues,
        ));
    }
    let moved: BTreeSet<BlockId> = rec.moved(m).into_iter().collect();
    if !moved.is_empty() {
        let issues = moved
            .iter()
            .map(|id| {
                let mut i = block_issue(
                    id,
                    "moved since the briefing",
                    "read it (rdy_context) before you retry",
                );
                i.code = E_BRIEF_STALE;
                i
            })
            .collect();
        return Err(refuse(
            E_BRIEF_STALE,
            format!(
                "the cited briefing is stale: {} moved since it was made",
                joined(&moved)
            ),
            issues,
        ));
    }
    Ok(vec![])
}

/// Turns a briefing refusal's ids into a fresh, registered briefing of exactly those blocks
/// (in `view`, or canon while the view has no head), computed from the published state `st`,
/// and rewrites the fix to cite its token. Any other error, or one whose briefing cannot be
/// computed, comes back unchanged.
pub fn expand_error(
    mut e: RdyError,
    st: &ReadState,
    view: &str,
    reg: &BriefRegistry,
    max_tokens: Option<usize>,
) -> RdyError {
    if e.code != E_BRIEF_REQUIRED && e.code != E_BRIEF_STALE {
        return e;
    }
    let Some(texts) = e
        .briefing
        .as_ref()
        .and_then(|b| b.get("ids"))
        .and_then(|ids| ids.as_array())
    else {
        return e;
    };
    let Some(ids) = texts
        .iter()
        .map(|t| t.as_str().and_then(|s| BlockId::parse(s).ok()))
        .collect::<Option<BTreeSet<BlockId>>>()
    else {
        return e;
    };
    let scope = if st.has_view(view) {
        Scope::View(view.to_string())
    } else {
        Scope::Canon
    };
    let Ok(b) = st.brief_ids(&scope, &ids, max_tokens) else {
        return e;
    };
    let Ok(json) = serde_json::to_value(&b) else {
        return e;
    };
    b.register(reg);
    e.fix = format!(
        "retry with brief: \"{}\"; this error's briefing covers exactly the blocks the write touches",
        b.base.token
    );
    e.briefing = Some(json);
    e
}
