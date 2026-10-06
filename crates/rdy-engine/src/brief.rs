//! Briefings (FW-E1, Q6a, Q6b): what a writer must know before changing a set of blocks, and a
//! token that names exactly the versions it was computed from. A briefing covers blocks (by id,
//! or every block under a doc or folder path); its token changes when, and only when, a covered
//! block or one of their dependencies changes version. Registered briefings live in memory
//! (`BriefRegistry`), one registry per open project.
use crate::error::*;
use crate::query::summary_in;
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{DerivedStatus, EdgeKind, IMPACT_LINKS, SUSPECT_LINKS, Suspect};
use rdy_model::decision::is_frozen;
use rdy_model::{Block, BlockId, FieldValue, Hash};
use rdy_schema::{Pack, Trait};
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

/// The registered briefings, by token: at most `cap`, the least recently inserted or read
/// dropped first. In memory only: gone when the project closes.
pub struct BriefRegistry {
    cap: usize,
    inner: Mutex<Lru>,
}

#[derive(Default)]
struct Lru {
    /// The next recency stamp.
    clock: u64,
    by_token: HashMap<Hash, (u64, Arc<BriefRecord>)>,
    /// Stamp → token, oldest first.
    order: BTreeMap<u64, Hash>,
}

impl Lru {
    /// Stores `r` as the most recently used, replacing any entry for its token.
    fn put(&mut self, r: Arc<BriefRecord>) {
        let (stamp, token) = (self.clock, r.token);
        self.clock += 1;
        if let Some((old, _)) = self.by_token.insert(token, (stamp, r)) {
            self.order.remove(&old);
        }
        self.order.insert(stamp, token);
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
    pub fn new(cap: usize) -> BriefRegistry {
        BriefRegistry {
            cap,
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
        while lru.by_token.len() > self.cap {
            let Some((_, oldest)) = lru.order.pop_first() else {
                break;
            };
            lru.by_token.remove(&oldest);
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

/// The follow-up a dependent block owes, by its kind (exact, in order): a frozen decision
/// reaffirms; a `normative` or `defines` type, or a `table` marked `normative: true`, updates;
/// anything else confirms.
pub fn follow_up(pack: &Pack, dependent: &Block) -> FollowUp {
    if is_frozen(dependent) {
        return FollowUp::Reaffirm;
    }
    let classifying = pack
        .block_types
        .get(&dependent.block_type)
        .is_some_and(|t| {
            t.traits.contains(&Trait::Normative) || t.traits.contains(&Trait::Defines)
        });
    let normative_table = dependent.block_type == "table"
        && dependent.fields.get("normative") == Some(&FieldValue::Bool(true));
    if classifying || normative_table {
        FollowUp::Update
    } else {
        FollowUp::Confirm
    }
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
        serde_json::to_string(self).map_or(0, |s| s.len()) / 4
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

/// A derived status's tag, as `DerivedStatus` serializes it.
fn status_tag(s: &DerivedStatus) -> &'static str {
    match s {
        DerivedStatus::Proposed => "proposed",
        DerivedStatus::Rejected => "rejected",
        DerivedStatus::Live => "live",
        DerivedStatus::PartiallySuperseded { .. } => "partially_superseded",
        DerivedStatus::Superseded { .. } => "superseded",
        DerivedStatus::Revoked { .. } => "revoked",
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
        let mut ids = BTreeSet::new();
        for text in &cov.ids {
            ids.insert(known(text)?);
        }
        if let Some(path) = &cov.path {
            let root = known(path)?;
            // the walk keeps its own visited set: a block also named in `ids` is still walked
            let mut seen = BTreeSet::from([root.clone()]);
            let mut todo = vec![root];
            while let Some(cur) = todo.pop() {
                for c in m.children(Some(&cur)) {
                    // a block already seen is not walked again (a damaged store may loop)
                    if seen.insert(c.id.clone()) {
                        todo.push(c.id.clone());
                    }
                }
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
        if ids.len() > MAX_BRIEF_BLOCKS {
            return Err(RdyError::new(
                E_LIMIT,
                format!(
                    "{} blocks to brief; the limit is {MAX_BRIEF_BLOCKS}",
                    ids.len()
                ),
                "brief a smaller part: a doc, or the blocks you will change",
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
                        .map(status_tag)
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
                {
                    stale.insert(key, follow_up(pack, from));
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
