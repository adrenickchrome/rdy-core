//! Briefings (FW-E1, Q6a, Q6b): what a writer must know before changing a set of blocks, and a
//! token that names exactly the versions it was computed from. A briefing covers blocks (by id,
//! or every block under a doc or folder path); its token changes when, and only when, a covered
//! block or one of their dependencies changes version. Registered briefings live in memory
//! (`BriefRegistry`), one registry per open project.
use crate::consequence_literals::{self, LiteralSet};
use crate::coverage::{
    Confidence, CoverageIssue, Dimension, PAGES_CAP, PageScore, confidence_in, coverage_error_text,
    page_blocks, rollup, sort_issue_refs,
};
use crate::error::*;
use crate::inflight::Inflight;
use crate::keys;
use crate::lexicon::Lexicon;
use crate::query::{is_entity_in, summary_in};
use crate::related::{RELATED_CAP, RelatedDecision};
use crate::suggest::MentionedBy;
use crate::suspect_reply::SuspectReply;
use crate::tensions::info::{InfoCache, SideInfo, TensionInfo, row_bytes, rows_bytes};
use crate::tensions::{Tension, TensionSeverity};
use crate::write::OpInput;
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{
    DerivedStatus, EdgeKind, GoverningCache, GoverningStatuses, Graph, IMPACT_LINKS, IMPACT_SHOWN,
    SUSPECT_LINKS,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, decision};
use rdy_schema::{Issue, Pack, Severity, supporting};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
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

thread_local! {
    /// Test switch: `page` builds every tension row before it measures (see
    /// `build_every_row_for_tests`).
    static EVERY_ROW: Cell<bool> = const { Cell::new(false) };
}

/// Test hook: while `on`, a briefing on this thread builds every tension row before its page
/// measures, as before rows were built on demand. A test compares the two to check that
/// building rows on demand changes no page.
#[doc(hidden)]
pub fn build_every_row_for_tests(on: bool) {
    EVERY_ROW.with(|c| c.set(on));
}

/// What to brief: blocks by id (or key or path), and a doc or folder path whose whole subtree is
/// covered. The coverage is their union.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Coverage {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// What a registered briefing vouches for: the blocks it covers, the version of every
/// recorded block (covered blocks and their dependencies; `None` for a block that is missing),
/// and the governing state it showed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefRecord {
    pub token: Hash,
    pub covers: BTreeSet<BlockId>,
    pub recorded: BTreeMap<BlockId, Option<Hash>>,
    pub governance: Governance,
}

/// rf6 (D-VPATZ): one live or proposed governing route of a covered block, as a briefing showed
/// it: the decision, the route (`via`) and its entity, and the decision's derived status tag.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteLine {
    pub block: BlockId,
    pub decision: BlockId,
    pub via: &'static str,
    pub entity: Option<BlockId>,
    pub status: &'static str,
}

/// rf6: the governing state a briefing vouches for beyond block versions. A decision's derived
/// status, a route that another block adds or removes, and an entity's `defines` trait can all
/// change while every recorded version stays: [`BriefRecord::stale`] compares this capture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Governance {
    /// Each covered block's governing routes, in (block, decision) order.
    pub routes: Vec<RouteLine>,
    /// Each recorded decision that a later decision retires (wholly or in part) or revokes,
    /// with its derived status tag.
    pub retired: BTreeMap<BlockId, &'static str>,
}

/// rf6: the live and proposed governing routes of each block of `ids` by the graph's four
/// routes, in (block, decision) order. `cache` belongs to `g` and this read.
fn route_lines(
    g: &Graph,
    pack: &Pack,
    m: &Materialized,
    ids: &BTreeSet<BlockId>,
    cache: &mut GoverningCache,
) -> Vec<RouteLine> {
    let mut out = vec![];
    for id in ids {
        for r in g.governing_routes(
            id,
            GoverningStatuses::LiveOrProposed,
            |e| is_entity_in(pack, m, e),
            cache,
        ) {
            let status = g
                .status(&r.decision)
                .as_ref()
                .map_or("", DerivedStatus::tag);
            out.push(RouteLine {
                block: id.clone(),
                decision: r.decision,
                via: r.via,
                entity: r.entity,
                status,
            });
        }
    }
    out
}

/// rf6: the blocks of `ids` that are decisions a later decision retires or revokes, with their
/// derived status tag.
fn retired_of<'a>(
    g: &Graph,
    ids: impl IntoIterator<Item = &'a BlockId>,
) -> BTreeMap<BlockId, &'static str> {
    ids.into_iter()
        .filter_map(|id| {
            let tag = g.status(id)?.tag();
            matches!(tag, "partially_superseded" | "superseded" | "revoked")
                .then(|| (id.clone(), tag))
        })
        .collect()
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

    /// rf6: what moved since the briefing in `m` and its graph `g` under `pack`, in id order:
    /// each recorded block whose version changed ([`BriefRecord::moved`]), each decision whose
    /// governing route of a covered block appeared, went or changed its route, entity or status
    /// (a new superseder, another decision on an entity, an entity that no longer defines), and
    /// each recorded decision whose retired status changed.
    pub fn stale(&self, pack: &Pack, m: &Materialized, g: &Graph) -> Vec<BlockId> {
        let mut out: BTreeSet<BlockId> = self.moved(m).into_iter().collect();
        let now = route_lines(g, pack, m, &self.covers, &mut GoverningCache::default());
        let then: BTreeSet<&RouteLine> = self.governance.routes.iter().collect();
        let now: BTreeSet<&RouteLine> = now.iter().collect();
        out.extend(then.symmetric_difference(&now).map(|r| r.decision.clone()));
        let retired = retired_of(g, self.recorded.keys());
        out.extend(
            self.recorded
                .keys()
                .filter(|id| retired.get(*id) != self.governance.retired.get(*id))
                .cloned(),
        );
        out.into_iter().collect()
    }

    pub fn token_hex(&self) -> String {
        self.token.to_string()
    }

    /// What this record weighs against a registry's entry budget.
    fn entries(&self) -> usize {
        self.covers.len()
            + self.recorded.len()
            + self.governance.routes.len()
            + self.governance.retired.len()
    }
}

/// The briefing token (exact): BLAKE3 of `rdy-brief-1\n`; for each recorded block in id order,
/// `<id>\t<version hex or "-">\t<c|d>\n` (`c` covered, `d` dependency only); rf6: then for each
/// governing route through an entity in (block, decision) order, `<block>\t<decision>\t<entity>\tr\n`;
/// then for each retired recorded decision in id order, `<id>\t<status tag>\ts\n`. A briefing
/// with no entity route and no retired decision keeps the token of its recorded blocks alone.
pub fn token_of(
    covers: &BTreeSet<BlockId>,
    recorded: &BTreeMap<BlockId, Option<Hash>>,
    governance: &Governance,
) -> Hash {
    let mut bytes = b"rdy-brief-1\n".to_vec();
    for (id, version) in recorded {
        let version = version.map_or_else(|| "-".to_string(), |h| h.to_string());
        let role = if covers.contains(id) { 'c' } else { 'd' };
        bytes.extend_from_slice(format!("{id}\t{version}\t{role}\n").as_bytes());
    }
    for r in &governance.routes {
        if let Some(entity) = &r.entity {
            bytes.extend_from_slice(
                format!("{}\t{}\t{entity}\tr\n", r.block, r.decision).as_bytes(),
            );
        }
    }
    for (id, tag) in &governance.retired {
        bytes.extend_from_slice(format!("{id}\t{tag}\ts\n").as_bytes());
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

/// The paged sections of a briefing, in cursor order: a cursor holds one offset per section, how
/// many of its entries earlier pages showed.
pub const PAGE_SECTIONS: [&str; 11] = [
    "governs",
    "dependents",
    "lineage",
    "will_go_stale",
    "suspects",
    "sources",
    "inflight.views",
    "inflight.proposals",
    "inflight.intents",
    "inflight.tensions",
    "base.blocks",
];

/// What a briefing holds for one doc: how many covered blocks are in it, and how many entries of
/// each list the whole briefing (not the cut one) has there.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocCounts {
    pub doc: String,
    pub title: String,
    pub blocks: usize,
    /// Per decision entry, once for each doc of the blocks it governs.
    pub governs: usize,
    /// By the doc of `on`.
    pub dependents: usize,
    /// By the doc of `on`.
    pub will_go_stale: usize,
    /// By the doc of `block`.
    pub suspects: usize,
    /// rf9: once for each source that a covered block of the doc reflects.
    #[serde(skip_serializing_if = "is_zero")]
    pub sources: usize,
    /// rf9: once for each finding row of a covered decision of the doc.
    #[serde(skip_serializing_if = "is_zero")]
    pub findings: usize,
}

/// What a dependent should do when the block it depends on changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowUp {
    Update,
    Confirm,
    Reaffirm,
    /// For conflicts (M2c3) and tensions (M2d2).
    Resolve,
}

/// The follow-up a dependent block owes when a block it depends on changed (D-CFC6V). `status`
/// is the dependent's derived decision status (`None` for a block that is not a decision);
/// `target_deleted` says the changed block no longer exists; `states_changed_literal` says the
/// dependent states a literal the change removed (`consequence_literals`); `dependent_cut` says
/// the scan of the dependent stopped at its byte budget. `None` means no follow-up at all.
///
/// Exact, in order:
/// 1. a decision that asserts nothing live (rejected, revoked or fully superseded): `None`;
/// 2. a live or partially superseded decision: `Reaffirm`, whatever the target did;
/// 3. a proposed decision (mutable): `Update`;
/// 4. any other block while the target is deleted (a repin cannot pin to a block that is gone):
///    `Update`;
/// 5. a block that states a removed literal: `Update`;
/// 6. a block whose scan was cut with no match (rdy cannot say the rest is clean): `Update`;
/// 7. anything else, after a complete scan with no match: `Confirm`.
pub fn follow_up(
    status: Option<&DerivedStatus>,
    target_deleted: bool,
    states_changed_literal: bool,
    dependent_cut: bool,
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
    Some(
        if target_deleted || states_changed_literal || dependent_cut {
            FollowUp::Update
        } else {
            FollowUp::Confirm
        },
    )
}

/// A live or proposed decision governing covered blocks (rf6, D-VPATZ: by the graph's four
/// routes).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Governs {
    pub decision: String,
    pub status: String,
    pub statement: String,
    /// The covered blocks it governs, in id order.
    pub blocks: Vec<String>,
    /// rf6: each covered block the decision governs through an entity it affects and the block
    /// describes (its selected route), with that entity; by block, then entity. Omitted when
    /// every route is direct.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub through: Vec<Through>,
}

/// rf6: a covered block and the entity through which a decision governs it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Through {
    pub block: String,
    pub entity: String,
}

/// An uncovered block that depends on a covered one (`on`) through `kind`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Dependent {
    pub block: String,
    pub kind: String,
    pub on: String,
    pub summary: String,
}

/// A refines, supersedes or revokes edge of a covered block (`on`) to a block outside the
/// briefing. `direction` is `in` when `block` points at `on`, `out` when `on` points at `block`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Lineage {
    pub block: String,
    pub kind: String,
    pub direction: String,
    pub on: String,
    pub summary: String,
}

/// FW-LP6: a covered decision (`decision`) that refines or part-supersedes `target` (`via` is
/// `refines` or `supersedes`) reaches `blocks`, the uncovered blocks `target` governs: never a
/// deleted block or a decision that asserts nothing live. The list is sorted by target, then
/// decision, then via; `blocks` is sorted by id. A decision that fully supersedes
/// another reaches nothing through the `refines` links it copied from it (a refines list equal
/// to the old decision's counts as copied).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reach {
    pub decision: String,
    pub via: String,
    pub target: String,
    pub blocks: Vec<String>,
}

/// The most blocks `reaches` lists in all; `reaches_omitted` counts the rest.
pub const REACH_CAP: usize = 50;

/// rf9 (D-4WC6R): a source that covered blocks reflect: the source, its kind and title, and the
/// covered blocks whose `reflects` link names it, in id order. One level: the source's own links
/// are not followed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BriefSource {
    pub source: String,
    pub kind: String,
    pub title: String,
    pub blocks: Vec<String>,
}

/// rf9 (D-Z8P09): a finding that a covered decision (`for`) depends on directly, with its
/// statement. A `conditional_on` link and a premise of a premise add no row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BriefFinding {
    pub r#for: String,
    pub finding: String,
    pub statement: String,
}

/// The most finding rows a briefing lists in all; `findings_omitted` counts the rest.
pub const FINDINGS_CAP: usize = 20;

/// An uncovered block that pins a covered one (`on`) and goes suspect when it changes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WillGoStale {
    pub block: String,
    pub via: String,
    pub on: String,
    pub follow_up: FollowUp,
    /// For a block that is not a decision, while `on` exists: up to eight of the literals `on`
    /// states that `block` states too (O3). Empty after a complete scan means `confirm`; a cut
    /// scan with no match keeps it empty and asks `update`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<String>>,
}

/// A will-go-stale row's follow-up and states, keyed by (on, block, via) in `assemble`.
type StaleRow = (FollowUp, Option<Vec<String>>);

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

/// The most coverage issues a briefing lists; the rest are counted in `issues_omitted`.
pub const BRIEF_ISSUES_CAP: usize = 20;

/// The coverage section of a briefing (D-KE84J, D-C6HHV, D-XZXCE): the score and dimensions of the
/// covered blocks, the score of each page that holds one, their issues in the closed order, and
/// the confidence of the briefing with its reasons. The confidence follows the covered blocks,
/// the decisions that govern them and the blocks that depend on them or will go stale (D-EEN7A),
/// taken before the page cut.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BriefCoverage {
    pub score: u32,
    pub dimensions: BTreeMap<Dimension, u32>,
    /// The pages that hold a covered block, lowest score first, then by path; at most
    /// `PAGES_CAP` (the cap of the coverage report), the rest counted in `pages_omitted`.
    pub pages: Vec<PageScore>,
    #[serde(skip_serializing_if = "is_zero")]
    pub pages_omitted: usize,
    pub issues: Vec<CoverageIssue>,
    #[serde(skip_serializing_if = "is_zero")]
    pub issues_omitted: usize,
    #[serde(flatten)]
    pub confidence: Confidence,
}

/// One briefing over a set of blocks. Registering it (`register`) makes its token citable.
#[derive(Clone, Debug, Serialize)]
pub struct Briefing {
    pub covers: usize,
    pub governs: Vec<Governs>,
    pub dependents: Vec<Dependent>,
    /// The refines, supersedes and revokes edges of the covered blocks, in both directions.
    pub lineage: Vec<Lineage>,
    pub will_go_stale: Vec<WillGoStale>,
    pub suspects: Vec<SuspectReply>,
    /// rf9: the sources the covered blocks reflect (a paged section), by source id.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<BriefSource>,
    /// Other views and open proposals that change a covered block (not part of the token).
    pub inflight: Inflight,
    /// The coverage of the covered blocks and the confidence of this briefing (FW-CV1, FW-CV2).
    /// Not paged or cut: every page carries it. Left out at a past landing: coverage describes
    /// the current state only (FW-H7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<BriefCoverage>,
    /// Why the coverage could not be read, as `<code>: <message>` (the rest of the briefing
    /// stands, and `coverage` is left out).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_error: Option<String>,
    /// Blocks in other docs that name a covered new doc (FW-EN3). Not paged or cut.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mentioned_by: Vec<MentionedBy>,
    #[serde(skip_serializing_if = "is_zero")]
    pub mentioned_by_omitted: usize,
    /// Decisions related to the covered decisions (FW-EN5), at most `RELATED_CAP` in all. Not
    /// paged or cut.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_decisions: Vec<RelatedDecision>,
    #[serde(skip_serializing_if = "is_zero")]
    pub related_decisions_omitted: usize,
    /// rf9: the findings the covered decisions depend on directly, by (for, finding), at most
    /// `FINDINGS_CAP` in all. Not paged or cut: every page repeats them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<BriefFinding>,
    #[serde(skip_serializing_if = "is_zero")]
    pub findings_omitted: usize,
    /// What the covered decisions reach (FW-LP6), at most `REACH_CAP` blocks in all. Not paged
    /// or cut, and not in the digest.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reaches: Vec<Reach>,
    #[serde(skip_serializing_if = "is_zero")]
    pub reaches_omitted: usize,
    pub base: Base,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, usize>,
    /// Pass this as `page` to see the entries after this page; absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Per-doc counts over the whole briefing, on a truncated briefing only; never cut.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<DocCounts>,
    /// How many entries of each `PAGE_SECTIONS` section this page and the pages before it showed.
    #[serde(skip)]
    pub offsets: [usize; PAGE_SECTIONS.len()],
    /// A digest of every paged section of the whole, uncut briefing except the advisory
    /// `inflight` ones: what a cursor vouches for beyond the token (which covers only the covered
    /// blocks and their outgoing suspect-link targets, not dependents, governs or lineage).
    #[serde(skip)]
    pub digest: Hash,
    /// The scope the briefing was made in, as scope text (`cursor_scope`); a cursor carries it.
    #[serde(skip)]
    pub scope: String,
    #[serde(skip)]
    pub record: Arc<BriefRecord>,
    /// Every tension row of the briefing, in row order (blocking first). `inflight.tensions`
    /// holds the rows built so far, always a prefix of this: `page` builds a row only when the
    /// page can need it (`fill`), so a cut briefing costs the rows it shows, not every row on
    /// its blocks.
    #[serde(skip)]
    rows: Vec<Arc<Tension>>,
    /// The JSON size of each built row (`inflight.tensions`), row for row, once measured
    /// (`bytes` measures a row once, not once per measure of the briefing).
    #[serde(skip)]
    row_sizes: Vec<Option<usize>>,
    /// Whether the built rows' sides are settled: read (`read_sides`), or dropped for good by
    /// the detail cut (`cut_tensions`).
    #[serde(skip)]
    sides_read: bool,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The most bytes a briefing of `max` tokens (4 bytes each, rounded down) can take. It
/// saturates: an engine caller may pass any `usize`, and a budget past the byte range is no cut.
pub(crate) fn budget_bytes(max: usize) -> usize {
    max.saturating_mul(4).saturating_add(3)
}

impl Briefing {
    /// Makes this briefing's token known to `reg`.
    pub fn register(&self, reg: &BriefRegistry) {
        reg.insert(Arc::clone(&self.record));
    }

    /// The JSON size in tokens (4 bytes each).
    fn tokens(&mut self) -> usize {
        self.bytes() / 4
    }

    /// The JSON size in bytes: the briefing without its tension rows, and each built row
    /// (measured once while it does not change) with the comma between rows. A row not built
    /// yet counts nothing.
    fn bytes(&mut self) -> usize {
        self.frame_bytes() + self.built_rows_bytes()
    }

    /// The JSON size in bytes of the briefing without its tension rows.
    fn frame_bytes(&mut self) -> usize {
        let rows = std::mem::take(&mut self.inflight.tensions);
        let frame = serde_json::to_string(self)
            .expect("a briefing serializes: strings, numbers and string-keyed maps only")
            .len();
        self.inflight.tensions = rows;
        frame
    }

    /// The JSON size in bytes of the built rows as array elements, each row measured once.
    fn built_rows_bytes(&mut self) -> usize {
        let rows = &self.inflight.tensions;
        self.row_sizes.resize(rows.len(), None);
        let total: usize = rows
            .iter()
            .zip(&mut self.row_sizes)
            .map(|(row, size)| *size.get_or_insert_with(|| row_bytes(row)))
            .sum();
        rows_bytes(total, rows.len())
    }

    /// Builds the next unbuilt tension row (without its sides). `false` when every row is built.
    fn build_next(&mut self, bare: &dyn Fn(&Tension) -> TensionInfo) -> bool {
        let Some(t) = self.rows.get(self.inflight.tensions.len()) else {
            return false;
        };
        let row = bare(t);
        self.inflight.tensions.push(row);
        true
    }

    /// Builds the tension rows this page can need: every blocking row (the cut never collapses
    /// one), then the next advisory row while the briefing is within `max` tokens and rows
    /// remain. After it, either every row is built, or the built rows alone put the briefing
    /// over `max`, so every unbuilt row is collapsed by the cut (rows are a prefix: the cut
    /// collapses advisory rows from the end, and halving keeps a prefix).
    fn fill(&mut self, max: usize, bare: &dyn Fn(&Tension) -> TensionInfo) {
        while self
            .rows
            .get(self.inflight.tensions.len())
            .is_some_and(|t| t.severity != TensionSeverity::Advisory)
        {
            self.build_next(bare);
        }
        if self.inflight.tensions.len() == self.rows.len() {
            return;
        }
        // the frame does not change while rows are built: measure it once
        let frame = self.frame_bytes();
        while frame + self.built_rows_bytes() <= budget_bytes(max) && self.build_next(bare) {}
    }

    /// Builds every tension row left unbuilt.
    fn build_all(&mut self, bare: &dyn Fn(&Tension) -> TensionInfo) {
        while self.build_next(bare) {}
    }

    /// The first `DIGEST_HEX` hex digits of `digest`, as a cursor carries them.
    fn digest_hex(&self) -> String {
        self.digest.to_string()[..DIGEST_HEX].to_string()
    }

    /// The length of each `PAGE_SECTIONS` section.
    fn lens(&self) -> [usize; PAGE_SECTIONS.len()] {
        [
            self.governs.len(),
            self.dependents.len(),
            self.lineage.len(),
            self.will_go_stale.len(),
            self.suspects.len(),
            self.sources.len(),
            self.inflight.views.len(),
            self.inflight.proposals.len(),
            self.inflight.intents.len(),
            self.rows.len(),
            self.base.blocks.len(),
        ]
    }

    /// Drops the first `skip[i]` entries of section `i` (each at most its length).
    fn drop_first(&mut self, skip: &[usize; PAGE_SECTIONS.len()]) {
        fn drop<T>(v: &mut Vec<T>, n: usize) {
            v.drain(..n.min(v.len()));
        }
        drop(&mut self.governs, skip[0]);
        drop(&mut self.dependents, skip[1]);
        drop(&mut self.lineage, skip[2]);
        drop(&mut self.will_go_stale, skip[3]);
        drop(&mut self.suspects, skip[4]);
        drop(&mut self.sources, skip[5]);
        drop(&mut self.inflight.views, skip[6]);
        drop(&mut self.inflight.proposals, skip[7]);
        drop(&mut self.inflight.intents, skip[8]);
        drop(&mut self.rows, skip[9]);
        drop(&mut self.inflight.tensions, skip[9]);
        drop(&mut self.row_sizes, skip[9]);
        drop(&mut self.base.blocks, skip[10]);
    }

    /// Shows the entries after the first `skip[i]` of each section, cut to `max` tokens, and
    /// sets `next`, `truncated` and `docs`. A briefing is cut when it is a later page or its
    /// whole does not fit `max`; only then does `docs` compute the per-doc counts (over the
    /// whole briefing, before the earlier pages' entries are dropped). With `floor`, the cut
    /// keeps at least one entry (a page always moves on). An offset past the end of an advisory
    /// section (`inflight`, which may shrink between pages) counts as all of it shown. `Err`
    /// (`E_PAGE_STALE`) when an offset is past the end of any other section: the sections are no
    /// longer the ones the cursor counted in.
    ///
    /// The tension rows (`bare` builds one without its sides) are built only when the page
    /// can need them (`fill`), and their sides (`sides` reads them) are read only for the rows
    /// this page can show with them: a row's sides only add to its size, so a briefing whose
    /// rows are over `max` without them loses them to the detail cut (Q15) anyway. The outcome
    /// is the one of a briefing assembled with every row built and every side read.
    fn page(
        &mut self,
        mut skip: [usize; PAGE_SECTIONS.len()],
        max: usize,
        floor: bool,
        docs: impl FnOnce(&Briefing) -> Vec<DocCounts>,
        sides: &mut dyn FnMut(&Tension) -> Vec<SideInfo>,
        bare: &dyn Fn(&Tension) -> TensionInfo,
    ) -> Result<(), RdyError> {
        if EVERY_ROW.with(Cell::get) {
            self.build_all(bare);
        }
        let totals = self.lens();
        for (i, s) in skip.iter_mut().enumerate() {
            if is_advisory(i) {
                *s = (*s).min(totals[i]);
            } else if *s > totals[i] {
                return Err(e_page_moved());
            }
        }
        let skips = skip.iter().any(|s| *s > 0);
        // a later page is cut whatever its size
        let cut = skips || {
            self.fill(max, bare);
            let mut tokens = self.tokens();
            // uncut without the sides: whether it is cut with them depends on them
            if tokens <= max && self.read_sides(sides) {
                tokens = self.tokens();
            }
            tokens > max
        };
        if cut {
            self.docs = docs(self);
        }
        self.drop_first(&skip);
        if skips {
            self.fill(max, bare);
        }
        self.settle(&skip, &totals);
        if cut {
            // `settle` only grows the frame (the cursor, `truncated`), so the `fill` above
            // already built every row this page can need: a second one would build nothing
            #[cfg(debug_assertions)]
            {
                let built = self.inflight.tensions.len();
                self.fill(max, bare);
                debug_assert_eq!(self.inflight.tensions.len(), built, "settle built no room");
            }
            let mut tokens = self.tokens();
            // within `max` without the sides: whether the detail cut drops them depends on them
            if tokens <= max && self.read_sides(sides) {
                tokens = self.tokens();
            }
            if tokens > max && self.cut_tensions(max, floor) {
                self.settle(&skip, &totals);
            }
            while self.tokens() > max && self.halve(floor) {
                self.settle(&skip, &totals);
            }
            // a row the cut could not collapse (a page shows at least one entry) is shown
            self.build_all(bare);
        }
        Ok(())
    }

    /// The Q15 rule for `inflight.tensions` (ruling on item 54), before any list is halved:
    /// every listed tension's sides go (`tensions_detail_cut`); then, while the briefing is
    /// over `max` tokens, advisory tensions go from the end of the list (they come after the
    /// blocking ones), each counted in `tensions_collapsed` under its kind and in `omitted`. A
    /// blocking tension is never collapsed: a page that cannot hold it leaves it to a later
    /// page. With `floor`, one entry is left. Each tension is measured once, and the whole
    /// briefing once more per count the cut adds. `true` if it cut.
    fn cut_tensions(&mut self, max: usize, floor: bool) -> bool {
        let mut changed = false;
        // rows whose sides are unread have sides (every tension has one), which the cut drops
        let unsided = !self.sides_read && !self.rows.is_empty();
        self.sides_read = true;
        let sided = self.inflight.tensions.iter().any(|t| !t.sides.is_empty());
        if unsided || sided {
            if sided {
                for t in &mut self.inflight.tensions {
                    t.sides.clear();
                }
                self.row_sizes.clear();
            }
            self.inflight.tensions_detail_cut = true;
            changed = true;
        }
        let mut bytes = self.bytes();
        while bytes > budget_bytes(max) {
            match self.collapse_last(floor) {
                Some(n) => bytes = bytes.saturating_sub(n),
                None => break,
            }
            changed = true;
        }
        // the counts the cut added, measured exactly
        while self.tokens() > max && self.collapse_last(floor).is_some() {
            changed = true;
        }
        changed
    }

    /// Reads the sides of every built tension row when they are not settled yet; `true` if
    /// there was a row. It runs only when the briefing fits without the sides, and then every
    /// row is built (`fill`).
    fn read_sides(&mut self, sides: &mut dyn FnMut(&Tension) -> Vec<SideInfo>) -> bool {
        if self.sides_read {
            return false;
        }
        self.sides_read = true;
        debug_assert_eq!(self.inflight.tensions.len(), self.rows.len());
        let any = !self.inflight.tensions.is_empty();
        for (row, t) in self.inflight.tensions.iter_mut().zip(&self.rows) {
            row.sides = sides(t);
        }
        if any {
            self.row_sizes.clear();
        }
        any
    }

    /// Collapses the last listed tension when it is advisory (and, with `floor`, more than one
    /// entry is shown): it leaves the list and is counted in `tensions_collapsed` and
    /// `omitted`. Its JSON size with its comma (0 for a row not built: `bytes` never counted
    /// it), or `None`.
    fn collapse_last(&mut self, floor: bool) -> Option<usize> {
        if floor && self.lens().iter().sum::<usize>() <= 1 {
            return None;
        }
        let t = self
            .rows
            .pop_if(|t| t.severity == TensionSeverity::Advisory)?;
        let n = if self.inflight.tensions.len() > self.rows.len() {
            let row = self
                .inflight
                .tensions
                .pop()
                .expect("a built row is in the list");
            let size = if self.row_sizes.len() == self.inflight.tensions.len() + 1 {
                self.row_sizes.pop().flatten()
            } else {
                self.row_sizes.clear();
                None
            };
            size.unwrap_or_else(|| row_bytes(&row)) + 1
        } else {
            0
        };
        *self
            .inflight
            .tensions_collapsed
            .entry(t.kind.name().to_string())
            .or_default() += 1;
        *self
            .omitted
            .entry("inflight.tensions".to_string())
            .or_default() += 1;
        self.truncated = true;
        Some(n)
    }

    /// Sets `offsets`, `next` and `truncated` from what this page shows now, so that the cut
    /// measures the briefing with its cursor: the cursor counts against the budget.
    fn settle(
        &mut self,
        skip: &[usize; PAGE_SECTIONS.len()],
        totals: &[usize; PAGE_SECTIONS.len()],
    ) {
        let shown = self.lens();
        let after: [usize; PAGE_SECTIONS.len()] = std::array::from_fn(|i| skip[i] + shown[i]);
        self.offsets = after;
        self.next = (after != *totals).then(|| {
            let offsets: Vec<String> = after.iter().map(ToString::to_string).collect();
            format!(
                "{}:{}:{}:{}",
                self.record.token_hex(),
                offsets.join(","),
                self.digest_hex(),
                self.scope
            )
        });
        self.truncated |= self.next.is_some() || skip.iter().any(|s| *s > 0);
    }

    /// Halves the longest detail list (ties: `PAGE_SECTIONS` order). `false` when nothing is
    /// left to cut: every list is empty (with `floor`: one entry is left). Coverage, the token
    /// and the record are never cut.
    fn halve(&mut self, floor: bool) -> bool {
        let lens = self.lens();
        if floor && lens.iter().sum::<usize>() <= 1 {
            return false;
        }
        let longest = lens.iter().copied().max().unwrap_or(0);
        if longest == 0 {
            return false;
        }
        let at = lens
            .iter()
            .position(|&n| n == longest)
            .expect("the max is in the list");
        let keep = longest / 2;
        match at {
            0 => self.governs.truncate(keep),
            1 => self.dependents.truncate(keep),
            2 => self.lineage.truncate(keep),
            3 => self.will_go_stale.truncate(keep),
            4 => self.suspects.truncate(keep),
            5 => self.sources.truncate(keep),
            6 => self.inflight.views.truncate(keep),
            7 => self.inflight.proposals.truncate(keep),
            8 => self.inflight.intents.truncate(keep),
            9 => {
                self.rows.truncate(keep);
                self.inflight.tensions.truncate(keep);
                self.row_sizes.truncate(keep);
            }
            _ => self.base.blocks.truncate(keep),
        }
        *self
            .omitted
            .entry(PAGE_SECTIONS[at].to_string())
            .or_default() += longest - keep;
        self.truncated = true;
        true
    }
}

/// `page` given with `ids`, `path` or `code_path`.
pub fn e_page_alone() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "page continues a briefing: pass it alone",
        "call rdy_brief with page (and scope or max_tokens) only, or without page to brief again",
    )
}

/// A page that is not a cursor `rdy_brief` gave.
fn bad_cursor() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "page is not a cursor from rdy_brief's next",
        "pass the next of a cut rdy_brief, alone",
    )
}

/// A page whose briefing is not the one its cursor counted in.
fn e_page_moved() -> RdyError {
    RdyError::new(
        E_PAGE_STALE,
        "the briefing moved since its first page: brief again",
        PAGE_STALE_FIX,
    )
}

/// A page of a briefing made in another scope than this call's.
fn e_page_scope(cursor: &str, given: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("page continues a briefing in scope {cursor}, not in {given}"),
        format!("pass scope {cursor:?} with page, or brief again without page to brief in {given}"),
    )
}

/// Whether `PAGE_SECTIONS[i]` is advisory: an `inflight` list, which a cursor does not vouch
/// for (it is not in the digest), so a page may show it shifted.
fn is_advisory(i: usize) -> bool {
    PAGE_SECTIONS[i].starts_with("inflight.")
}

/// A scope as a cursor carries it, in the scope text `resolve_scope` reads: `canon`,
/// `view:<name>`, `landing:<id>` or `proposal:<version hash>`.
fn cursor_scope(scope: &Scope) -> String {
    match scope {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => format!("view:{v}"),
        Scope::At(h) => format!("landing:{h}"),
        Scope::Proposal(v) => format!("proposal:{v}"),
        Scope::Inflight => crate::INFLIGHT_SCOPE.to_string(),
    }
}

/// insc1: a briefing is made in the scope its reader will write in, never in `inflight`.
pub(crate) fn refuse_inflight(scope: &Scope) -> Result<(), RdyError> {
    if *scope == Scope::Inflight {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "inflight is an analysis scope: a briefing reads the view you will write in",
            "brief with the scope of your view (the default)",
        ));
    }
    Ok(())
}

/// How many hex digits of a briefing's digest a cursor carries (64 bits). A change plan's cursor
/// carries as many.
pub(crate) const DIGEST_HEX: usize = 16;

/// A cursor `<token>:<o1>,…,<o11>:<digest>:<scope>` split at its first colon: the token (64
/// lowercase hex) and the rest. The rest is read by `parse_tail` once the token is known.
fn parse_cursor(cursor: &str) -> Result<(&str, &str), RdyError> {
    let (token, rest) = cursor.split_once(':').ok_or_else(bad_cursor)?;
    if token.len() != 64 || !token.bytes().all(is_lower_hex) {
        return Err(bad_cursor());
    }
    Ok((token, rest))
}

pub(crate) fn is_lower_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

/// The offsets, the digest and the scope of a cursor's rest. The scope is last: everything
/// after the digest's colon (a view name may hold a colon).
fn parse_tail(rest: &str) -> Result<([usize; PAGE_SECTIONS.len()], &str, &str), RdyError> {
    parse_page_tail::<{ PAGE_SECTIONS.len() }>(rest).ok_or_else(bad_cursor)
}

/// A cursor tail `<o1>,…,<oN>:<digest>:<scope>`: exactly `N` decimal offsets, a digest of
/// [`DIGEST_HEX`] lowercase hex digits and a non-empty scope, which is last (everything after
/// the digest's colon, so it may hold colons). `None` when the tail is not one. The offsets are
/// read into the array as they are split: nothing is allocated from their count.
pub(crate) fn parse_page_tail<const N: usize>(rest: &str) -> Option<([usize; N], &str, &str)> {
    let (offsets, tail) = rest.split_once(':')?;
    let (digest, scope) = tail.split_once(':')?;
    if digest.len() != DIGEST_HEX || !digest.bytes().all(is_lower_hex) || scope.is_empty() {
        return None;
    }
    let mut out = [0usize; N];
    let mut parts = offsets.split(',');
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((out, digest, scope))
}

/// How a scope is named in a briefing's errors.
fn scope_label(scope: &Scope) -> String {
    match scope {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => format!("view {v}"),
        Scope::At(h) => format!("landing:{h}"),
        Scope::Proposal(v) => format!("proposal:{}", v.short()),
        Scope::Inflight => crate::INFLIGHT_SCOPE.to_string(),
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

/// The link kinds a briefing lists as lineage: how a decision refines, supersedes or revokes
/// another. `dependents` keeps the ones `rdy impact` shows at depth 1 (`IMPACT_SHOWN`).
const LINEAGE_LINKS: [&str; 3] = [decision::REFINES, decision::SUPERSEDES, decision::REVOKES];

/// A link (not a reference) named in `names`.
fn link_in(kind: &EdgeKind, names: &[&str]) -> bool {
    matches!(kind, EdgeKind::Link(n) if names.contains(&n.as_str()))
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
        refuse_inflight(scope)?;
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
        // df8: an address is a block, or `<doc>#<heading-slug>` (the section, walked like a path)
        let address = |text: &str| -> Result<(BlockId, bool), RdyError> {
            let (target, slug) = crate::section::split_address(text)?;
            let id = known(target)?;
            let Some(slug) = slug else {
                return Ok((id, false));
            };
            let doc = m.get(&id).expect("known returns a block of m");
            Ok((crate::section::section_by_slug(&m, doc, slug)?, true))
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
        // the block at `root` with every descendant joins `ids`. The walk keeps its own visited
        // set: a block already in `ids` is still walked, and counts once (`extra` counts the
        // walked blocks not in `ids`)
        let walk = |ids: &mut BTreeSet<BlockId>, root: BlockId| -> Result<(), RdyError> {
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
            Ok(())
        };
        let mut ids = BTreeSet::new();
        for text in &cov.ids {
            match address(text)? {
                (id, false) => {
                    ids.insert(id);
                }
                (section, true) => walk(&mut ids, section)?,
            }
            if ids.len() > MAX_BRIEF_BLOCKS {
                return Err(too_many());
            }
        }
        if let Some(path) = &cov.path {
            walk(&mut ids, address(path)?.0)?;
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
        refuse_inflight(scope)?;
        let mut briefing = self.assemble(scope, ids)?;
        let m = self.materialized(scope)?;
        let mut cache = InfoCache::default();
        briefing.page(
            [0; PAGE_SECTIONS.len()],
            max_tokens.unwrap_or(BRIEF_DEFAULT_TOKENS),
            false,
            |b| doc_counts(&m, b),
            &mut |t| self.sides_info(t, &mut cache),
            &|t| self.tension_info_bare(t),
        )?;
        Ok(briefing)
    }

    /// The page of a cut briefing that `cursor` (a `next`) names: the briefing registered under
    /// its token is made again in `scope` and must still have that token; the entries earlier
    /// pages showed are dropped and the rest cut to `max_tokens`. A page shows at least one
    /// entry, so paging ends. `E_BAD_INPUT` when `scope` is not the scope the cursor's briefing
    /// was made in. `E_PAGE_STALE` when the token or any non-advisory paged entry of the whole
    /// briefing differs from what the cursor counted (its digest), or an offset is out of range.
    pub fn brief_page(
        &self,
        scope: &Scope,
        reg: &BriefRegistry,
        cursor: &str,
        max_tokens: Option<usize>,
    ) -> Result<Briefing, RdyError> {
        refuse_inflight(scope)?;
        let (token, rest) = parse_cursor(cursor)?;
        let Some(rec) = reg.get(token) else {
            return Err(RdyError::new(
                E_PAGE_STALE,
                "the briefing behind this page is unknown (the daemon restarted, or it was evicted): brief again",
                PAGE_STALE_FIX,
            ));
        };
        let (skip, digest, made_in) = parse_tail(rest)?;
        let given = cursor_scope(scope);
        if made_in != given {
            return Err(e_page_scope(made_in, &given));
        }
        let mut briefing = self.assemble(scope, &rec.covers)?;
        // the token covers the covered blocks only; the digest covers every non-advisory paged
        // entry, so a dependent, a governing decision or a lineage edge that came or went is
        // caught too. inflight is advice: another view's write does not refuse the page.
        if briefing.record.token != rec.token || briefing.digest_hex() != digest {
            return Err(e_page_moved());
        }
        let m = self.materialized(scope)?;
        let mut cache = InfoCache::default();
        briefing.page(
            skip,
            max_tokens.unwrap_or(BRIEF_DEFAULT_TOKENS),
            true,
            |b| doc_counts(&m, b),
            &mut |t| self.sides_info(t, &mut cache),
            &|t| self.tension_info_bare(t),
        )?;
        Ok(briefing)
    }

    /// The coverage section of the briefing of `ids`: the rollup of the covered blocks, one score
    /// per page that holds one, their issues (at most [`BRIEF_ISSUES_CAP`]) and the confidence
    /// of the neighbourhood (D-EEN7A). The lists are those before any page cut. It reads the
    /// scope's memoised coverage when that is cheap to have, else works out the entries of the
    /// blocks it reads (the covered blocks, the neighbourhood and the covered pages); it never
    /// builds the coverage of the whole scope (perf4).
    fn brief_coverage(
        &self,
        scope: &Scope,
        ids: &BTreeSet<BlockId>,
        governs: &[Governs],
        dependents: &[Dependent],
        will_go_stale: &[WillGoStale],
    ) -> Result<BriefCoverage, RdyError> {
        let mut cov = self.scoped_entries(scope)?;
        let m = self.materialized(scope)?;
        self.fill_entries(scope, &mut cov, ids)?;
        let rolled = rollup(self.pack(), ids.iter().filter_map(|id| cov.get(id)));

        let mut neighbourhood = ids.clone();
        let named = governs
            .iter()
            .map(|g| &g.decision)
            .chain(dependents.iter().map(|d| &d.block))
            .chain(will_go_stale.iter().map(|w| &w.block));
        neighbourhood.extend(named.filter_map(|b| BlockId::parse(b).ok()));
        self.fill_entries(scope, &mut cov, &neighbourhood)?;

        // the doc path of every block with an issue, for the closed order; one path per doc
        let mut doc_paths: BTreeMap<&BlockId, Option<String>> = BTreeMap::new();
        let mut paths: BTreeMap<String, String> = BTreeMap::new();
        for id in &neighbourhood {
            if let Some(e) = cov.get(id)
                && !e.issues.is_empty()
                && let Some(d) = e.doc.as_ref()
                && let Some(p) = doc_paths.entry(d).or_insert_with(|| m.path_of(d))
            {
                paths.insert(id.to_string(), p.clone());
            }
        }

        let covered_docs: BTreeSet<BlockId> = ids
            .iter()
            .filter_map(|id| cov.get(id)?.doc.clone())
            .collect();
        let mut pages: Vec<PageScore> = vec![];
        for d in &covered_docs {
            let blocks = page_blocks(&m, d);
            self.fill_entries(scope, &mut cov, &blocks)?;
            let mut entries = blocks.iter().filter_map(|id| cov.get(id)).peekable();
            if entries.peek().is_none() {
                continue;
            }
            let score = rollup(self.pack(), entries).score;
            if let Some(path) = m.path_of(d) {
                pages.push(PageScore {
                    doc: d.to_string(),
                    path,
                    score,
                });
            }
        }
        // lowest score first, then by path, as the coverage report lists its pages
        pages.sort_by(|a, b| (a.score, &a.path, &a.doc).cmp(&(b.score, &b.path, &b.doc)));
        let pages_omitted = pages.len().saturating_sub(PAGES_CAP);
        pages.truncate(PAGES_CAP);

        let mut listed: Vec<&CoverageIssue> = ids
            .iter()
            .filter_map(|id| cov.get(id))
            .flat_map(|e| e.issues.iter())
            .collect();
        sort_issue_refs(&mut listed, &paths);
        let issues_omitted = listed.len().saturating_sub(BRIEF_ISSUES_CAP);
        let listed: Vec<CoverageIssue> =
            listed.into_iter().take(BRIEF_ISSUES_CAP).cloned().collect();

        Ok(BriefCoverage {
            score: rolled.score,
            dimensions: rolled.dimensions,
            pages,
            pages_omitted,
            issues: listed,
            issues_omitted,
            confidence: confidence_in(|b| cov.get(b), &neighbourhood, None, &paths),
        })
    }

    /// The whole briefing of `ids` in `scope`, uncut, without its per-doc counts (`page`
    /// computes them for a cut briefing only).
    fn assemble(&self, scope: &Scope, ids: &BTreeSet<BlockId>) -> Result<Briefing, RdyError> {
        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        let pack = self.pack();
        let summary_of = |id: &BlockId| m.get(id).map(|b| summary_in(b, &m)).unwrap_or_default();

        // rf9: every source a covered block reflects and every finding a covered decision depends
        // on is a suspect-link target (`reflects` and `depends_on`), so it is recorded here:
        // however many finding rows the cap leaves out, an edit of any of them moves the token
        let mut recorded = BTreeMap::new();
        for id in ids {
            recorded.insert(id.clone(), m.version_of(id));
            for e in g.outgoing(id) {
                if edge_in(&e.kind, &SUSPECT_LINKS) {
                    recorded.insert(e.to.clone(), m.version_of(&e.to));
                }
            }
        }

        // rf6 (D-VPATZ): live and proposed decisions by the graph's four routes, one cache for
        // the briefing. Every governing decision is a recorded dependency (its status and
        // affects list), and so is each entity a route runs through. The routes and the retired
        // statuses of the recorded decisions are captured too, so a change no recorded version
        // shows (a new superseder or decision on an entity, an entity that stops defining)
        // moves the briefing as well (`BriefRecord::stale`).
        let routes = route_lines(&g, pack, &m, ids, &mut GoverningCache::default());
        let mut governed: BTreeMap<BlockId, (Vec<String>, Vec<Through>)> = BTreeMap::new();
        for r in &routes {
            recorded.insert(r.decision.clone(), m.version_of(&r.decision));
            let row = governed.entry(r.decision.clone()).or_default();
            row.0.push(r.block.to_string());
            if let Some(entity) = &r.entity {
                recorded.insert(entity.clone(), m.version_of(entity));
                row.1.push(Through {
                    block: r.block.to_string(),
                    entity: entity.to_string(),
                });
            }
        }
        let governance = Governance {
            retired: retired_of(&g, recorded.keys()),
            routes,
        };
        let token = token_of(ids, &recorded, &governance);
        let governs: Vec<Governs> = governed
            .into_iter()
            .map(|(d, (blocks, mut through))| {
                // one route per (block, decision): already by block; sorted and once each
                through.sort();
                through.dedup();
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
                    through,
                }
            })
            .collect();

        // keyed by (on, block, kind): deduplicated and in order
        let mut dependents: BTreeMap<(BlockId, BlockId, String), ()> = BTreeMap::new();
        let mut stale: BTreeMap<(BlockId, BlockId, String), StaleRow> = BTreeMap::new();
        // O3: what a nondecision dependent states of the covered block's literals, once per
        // (block, dependent) whatever the number of edges; the lexicon only when one is read
        let mut names: Option<Arc<Lexicon>> = None;
        let mut covered: BTreeMap<BlockId, LiteralSet> = BTreeMap::new();
        let mut stated: BTreeMap<(BlockId, BlockId), (Vec<String>, bool)> = BTreeMap::new();
        for id in ids {
            for e in g.incoming(id) {
                if ids.contains(&e.from) {
                    continue;
                }
                let key = (id.clone(), e.from.clone(), e.kind.name());
                if edge_in(&e.kind, &IMPACT_LINKS) || link_in(&e.kind, &IMPACT_SHOWN) {
                    dependents.insert(key.clone(), ());
                }
                if !edge_in(&e.kind, &SUSPECT_LINKS) || e.pin.is_none() {
                    continue;
                }
                let Some(from) = m.get(&e.from) else { continue };
                let status = g.status(&e.from);
                let target = m.get(id);
                let states = match (&status, target) {
                    (None, Some(target)) => {
                        if names.is_none() {
                            names = Some(self.lexicon(scope)?);
                        }
                        let names = names.as_deref().expect("set above");
                        let pair = (id.clone(), e.from.clone());
                        let found = stated.entry(pair).or_insert_with(|| {
                            let lits = covered.entry(id.clone()).or_insert_with(|| {
                                consequence_literals::covered(pack, target, names)
                            });
                            consequence_literals::states(pack, lits, from, names)
                        });
                        Some(found.clone())
                    }
                    _ => None,
                };
                let (said, cut) = states
                    .as_ref()
                    .map_or((false, false), |(s, cut)| (!s.is_empty(), *cut));
                if let Some(f) = follow_up(status.as_ref(), target.is_none(), said, cut) {
                    stale.insert(key, (f, states.map(|(s, _)| s)));
                }
            }
        }
        // keyed by (on, direction, block, kind): deduplicated, and in before out
        let mut lineage: BTreeMap<(BlockId, &'static str, BlockId, String), ()> = BTreeMap::new();
        for id in ids {
            for e in g.incoming(id) {
                if !ids.contains(&e.from) && link_in(&e.kind, &LINEAGE_LINKS) {
                    lineage.insert((id.clone(), "in", e.from.clone(), e.kind.name()), ());
                }
            }
            for e in g.outgoing(id) {
                if !ids.contains(&e.to) && link_in(&e.kind, &LINEAGE_LINKS) {
                    lineage.insert((id.clone(), "out", e.to.clone(), e.kind.name()), ());
                }
            }
        }
        let lineage: Vec<Lineage> = lineage
            .into_keys()
            .map(|(on, direction, block, kind)| Lineage {
                summary: summary_of(&block),
                block: block.to_string(),
                kind,
                direction: direction.to_string(),
                on: on.to_string(),
            })
            .collect();
        // FW-LP6: what each covered decision reaches through a refines link or a part
        // supersedes link, at most `REACH_CAP` blocks in all
        let mut found: Vec<(String, String, &str, Vec<String>)> = vec![];
        for id in ids {
            // a full supersede does not reach through the refines links it copied
            let copied = crate::consequence::refines_copied(&g, id);
            for e in g.outgoing(id) {
                let via = match &e.kind {
                    EdgeKind::Link(n) if n == decision::REFINES && !copied => decision::REFINES,
                    EdgeKind::Link(n) if n == decision::SUPERSEDES && e.part.is_some() => {
                        decision::SUPERSEDES
                    }
                    _ => continue,
                };
                if matches!(
                    g.status(&e.to),
                    Some(
                        DerivedStatus::Rejected
                            | DerivedStatus::Superseded { .. }
                            | DerivedStatus::Revoked { .. }
                    )
                ) {
                    continue;
                }
                let mut all: Vec<String> = g
                    .governed_by(&e.to)
                    .into_iter()
                    // as the consequences do: never a block deleted here, or a decision that
                    // asserts nothing live
                    .filter(|b| {
                        !ids.contains(b)
                            && m.get(b).is_some()
                            && follow_up(g.status(b).as_ref(), false, false, false).is_some()
                    })
                    .map(|b| b.to_string())
                    .collect();
                if all.is_empty() {
                    continue;
                }
                all.sort();
                found.push((e.to.to_string(), id.to_string(), via, all));
            }
        }
        // a stable order, whatever the iteration order of the graph: target, decision, via; the
        // cap then cuts the same entries every time
        found.sort();
        let mut reaches = vec![];
        let mut reaches_omitted = 0;
        let mut room = REACH_CAP;
        for (target, decision, via, all) in found {
            let shown = all.len().min(room);
            reaches_omitted += all.len() - shown;
            room -= shown;
            // past the cap: counted, never listed with no blocks
            if shown == 0 {
                continue;
            }
            reaches.push(Reach {
                decision,
                via: via.to_string(),
                target,
                blocks: all.into_iter().take(shown).collect(),
            });
        }
        let dependents: Vec<Dependent> = dependents
            .into_keys()
            .map(|(on, block, kind)| Dependent {
                summary: summary_of(&block),
                block: block.to_string(),
                kind,
                on: on.to_string(),
            })
            .collect();
        let will_go_stale: Vec<WillGoStale> = stale
            .into_iter()
            .map(|((on, block, via), (follow_up, states))| WillGoStale {
                block: block.to_string(),
                via,
                on: on.to_string(),
                follow_up,
                states,
            })
            .collect();

        let mut structural = vec![];
        for id in ids {
            let mut found = g.suspects_of(id);
            found.retain(|s| !self.is_presentational_source(&m, s));
            structural.extend(found);
        }
        let covered: Vec<BlockId> = ids.iter().cloned().collect();
        let suspects = self.suspect_rows(scope, structural, Some(&covered))?;
        let (sources, mut findings) = supporting_rows(&g, &m, ids);
        debug_assert!(
            sources
                .iter()
                .map(|s| &s.source)
                .chain(findings.iter().map(|f| &f.finding))
                .all(|t| BlockId::parse(t).is_ok_and(|t| recorded.contains_key(&t))),
            "every listed source and finding is a recorded dependency"
        );
        let findings_omitted = findings.len().saturating_sub(FINDINGS_CAP);
        findings.truncate(FINDINGS_CAP);
        let (inflight, rows) = self.inflight_unsided(scope, ids)?;
        // a coverage read error never fails a briefing (a briefing gates writes): it is reported
        let (coverage, coverage_error) = if self.coverage_left_out(scope) {
            (None, None)
        } else {
            match self.brief_coverage(scope, ids, &governs, &dependents, &will_go_stale) {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(coverage_error_text(&e))),
            }
        };
        let (mentioned_by, mentioned_by_omitted) = self.mentioned_by(scope, ids)?;
        let decisions: Vec<BlockId> = ids.iter().cloned().collect();
        let (mut related_decisions, mut related_decisions_omitted) =
            self.related_decisions(scope, &decisions)?;
        // a briefing over many decisions would carry the per-decision cap for each: cut it
        // to one cap in all, and count what this cut drops
        if related_decisions.len() > RELATED_CAP {
            related_decisions_omitted += related_decisions.len() - RELATED_CAP;
            related_decisions.truncate(RELATED_CAP);
        }

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
        // inflight is advice and not vouched for: a listed view's next write moves its
        // last_write, and a page must not go stale over that
        let mut digested = serde_json::to_vec(&(
            &governs,
            &dependents,
            &lineage,
            &will_go_stale,
            &suspects,
            &sources,
            &base.blocks,
        ))
        .expect("a briefing's sections serialize: strings, numbers and string-keyed maps only");
        // rf9: the finding rows every page repeats are vouched for too, when there are any
        if !findings.is_empty() || findings_omitted > 0 {
            digested.extend(
                serde_json::to_vec(&(&findings, findings_omitted))
                    .expect("finding rows serialize: strings and numbers only"),
            );
        }
        let digest = Hash::of(&digested);
        Ok(Briefing {
            covers: ids.len(),
            governs,
            dependents,
            lineage,
            will_go_stale,
            suspects,
            sources,
            inflight,
            coverage,
            coverage_error,
            mentioned_by,
            mentioned_by_omitted,
            related_decisions,
            related_decisions_omitted,
            findings,
            findings_omitted,
            reaches,
            reaches_omitted,
            base,
            truncated: false,
            omitted: BTreeMap::new(),
            next: None,
            docs: vec![],
            offsets: [0; PAGE_SECTIONS.len()],
            digest,
            scope: cursor_scope(scope),
            record: Arc::new(BriefRecord {
                token,
                covers: ids.clone(),
                recorded,
                governance,
            }),
            rows,
            row_sizes: vec![],
            sides_read: false,
        })
    }
}

/// rf9: the source and finding parts of a briefing of `ids` in `m` (graph `g`), one level deep.
/// A source is a held `source` block that a covered block names with a `reflects` link, listed
/// once with the covered blocks that name it; a finding row is a held `finding` that a covered
/// decision names with a `depends_on` link. Sources are in source-id order, finding rows in
/// (decision, finding) order, none cut. A `conditional_on` link and a premise of a premise add
/// nothing.
fn supporting_rows(
    g: &Graph,
    m: &Materialized,
    ids: &BTreeSet<BlockId>,
) -> (Vec<BriefSource>, Vec<BriefFinding>) {
    let mut by_source: BTreeMap<BlockId, BTreeSet<BlockId>> = BTreeMap::new();
    let mut rows: BTreeSet<(BlockId, BlockId)> = BTreeSet::new();
    for id in ids {
        let Some(from) = m.get(id) else { continue };
        for e in g.outgoing(id) {
            let EdgeKind::Link(kind) = &e.kind else {
                continue;
            };
            let Some(to) = m.get(&e.to) else { continue };
            if kind == supporting::REFLECTS_LINK && to.block_type == supporting::SOURCE {
                by_source
                    .entry(e.to.clone())
                    .or_default()
                    .insert(id.clone());
            } else if kind == decision::DEPENDS_ON
                && from.block_type == decision::DECISION
                && to.block_type == supporting::FINDING
            {
                rows.insert((id.clone(), e.to.clone()));
            }
        }
    }
    let text = |b: &Block, field: &str| b.field_str(field).unwrap_or_default().to_string();
    let sources = by_source
        .into_iter()
        .filter_map(|(source, blocks)| {
            let b = m.get(&source)?;
            Some(BriefSource {
                kind: text(b, "kind"),
                title: text(b, "title"),
                source: source.to_string(),
                blocks: blocks.iter().map(ToString::to_string).collect(),
            })
        })
        .collect();
    let findings = rows
        .into_iter()
        .filter_map(|(decision, finding)| {
            let b = m.get(&finding)?;
            Some(BriefFinding {
                r#for: decision.to_string(),
                statement: text(b, "statement"),
                finding: finding.to_string(),
            })
        })
        .collect();
    (sources, findings)
}

/// The per-doc counts of a whole briefing `b` (before any entry is dropped), by doc id. A
/// covered block outside any doc (a folder) counts nowhere.
fn doc_counts(m: &Materialized, b: &Briefing) -> Vec<DocCounts> {
    let (ids, governs, dependents, will_go_stale, suspects) = (
        &b.record.covers,
        &b.governs,
        &b.dependents,
        &b.will_go_stale,
        &b.suspects,
    );
    let (sources, findings) = (&b.sources, &b.findings);
    let mut by_doc: BTreeMap<BlockId, DocCounts> = BTreeMap::new();
    let mut bump = |id: &BlockId, add: fn(&mut DocCounts)| {
        if let Some(d) = m.doc_of(id) {
            add(by_doc.entry(d.id.clone()).or_insert_with(|| DocCounts {
                doc: d.id.to_string(),
                title: d
                    .field_str("title")
                    .or_else(|| d.field_str("name"))
                    .unwrap_or_default()
                    .to_string(),
                blocks: 0,
                governs: 0,
                dependents: 0,
                will_go_stale: 0,
                suspects: 0,
                sources: 0,
                findings: 0,
            }));
        }
    };
    let of = |text: &str| BlockId::parse(text).ok();
    for id in ids {
        bump(id, |c| c.blocks += 1);
    }
    for g in governs {
        let docs: BTreeSet<BlockId> = g
            .blocks
            .iter()
            .filter_map(|b| of(b))
            .filter_map(|b| m.doc_of(&b).map(|d| d.id.clone()))
            .collect();
        for d in docs {
            bump(&d, |c| c.governs += 1);
        }
    }
    for d in dependents {
        if let Some(on) = of(&d.on) {
            bump(&on, |c| c.dependents += 1);
        }
    }
    for w in will_go_stale {
        if let Some(on) = of(&w.on) {
            bump(&on, |c| c.will_go_stale += 1);
        }
    }
    for s in suspects {
        bump(&s.block, |c| c.suspects += 1);
    }
    // rf9: a source counts once for each doc of the covered blocks that reflect it; a finding
    // row counts in the doc of its decision
    for s in sources {
        let docs: BTreeSet<BlockId> = s
            .blocks
            .iter()
            .filter_map(|b| of(b))
            .filter_map(|b| m.doc_of(&b).map(|d| d.id.clone()))
            .collect();
        for d in docs {
            bump(&d, |c| c.sources += 1);
        }
    }
    for f in findings {
        if let Some(d) = of(&f.r#for) {
            bump(&d, |c| c.findings += 1);
        }
    }
    by_doc.into_values().collect()
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

/// What one op is about: the block a briefing must cover, whether the op touches stable
/// content, and, for a create, the type of the block it creates under the target.
#[derive(Clone, Debug)]
struct Subject<'a> {
    target: BlockId,
    stable: bool,
    created_type: Option<&'a str>,
}

/// A block's own `lifecycle`, if it has one (a null value counts as none).
fn own_lifecycle(b: &Block) -> Option<&FieldValue> {
    b.fields
        .get("lifecycle")
        .filter(|v| **v != FieldValue::Null)
}

/// The `lifecycle` of `id` or of its nearest ancestor that has one is `stable`.
pub(crate) fn effective_stable(m: &Materialized, id: Option<&BlockId>) -> bool {
    effective_lifecycle(m, id).as_deref() == Some(STABLE)
}

/// The effective lifecycle of `id`: its own `lifecycle`, else that of its nearest ancestor that
/// has one. `None` when no block on the way has one, `id` is absent, or the nearest value is not
/// a string.
pub(crate) fn effective_lifecycle(m: &Materialized, id: Option<&BlockId>) -> Option<String> {
    let mut cur = id.and_then(|i| m.get(i));
    // a damaged store may loop: never walk more steps than there are blocks
    let mut steps = 0;
    while let Some(b) = cur {
        if let Some(l) = own_lifecycle(b) {
            return l.as_str().map(str::to_string);
        }
        steps += 1;
        if steps > m.len() {
            break;
        }
        cur = b.parent.as_ref().and_then(|p| m.get(p));
    }
    None
}

/// The subjects of `ops` against `m` (the target view before the write), in op order. Ops whose
/// block does not resolve are skipped; a subject of a presentational type is exempt (Q6d), and
/// for `create` and `rebalance` the subject is the parent. A `reaffirm` op's subjects are the
/// decisions `reaffirmed` says it selected (p-0SH30), each once, never a parent.
fn subjects<'a>(
    pack: &Pack,
    m: &Materialized,
    ops: &'a [OpInput],
    reaffirmed: &crate::reaffirm::Selection,
) -> Vec<Subject<'a>> {
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
    for (i, op) in ops.iter().enumerate() {
        match op {
            OpInput::Reaffirm { .. } => {
                for s in reaffirmed.of(i) {
                    let Some(b) = m.get(&s.id) else {
                        continue;
                    };
                    if exempt(b) {
                        continue;
                    }
                    out.push(Subject {
                        target: b.id.clone(),
                        stable: effective_stable(m, Some(&b.id)),
                        created_type: None,
                    });
                }
            }
            OpInput::Create {
                id,
                parent,
                block_type,
                ..
            } => {
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
                        created_type: Some(block_type.as_str()),
                    });
                }
            }
            OpInput::Update {
                id,
                fields,
                unset,
                move_to,
                ..
            } => {
                let Some(b) = existing(id).and_then(|i| m.get(&i)) else {
                    continue;
                };
                if exempt(b) {
                    continue;
                }
                let before = effective_stable(m, Some(&b.id));
                // wcmv1: a move_to counts as a move to that place does
                let moved_to =
                    move_to
                        .as_ref()
                        .map(|to| match anchor(&created, to.parent.as_deref()) {
                            Anchor::At(p) => effective_stable(m, Some(&p)),
                            Anchor::Top | Anchor::Unknown => false,
                        });
                let inherited =
                    || moved_to.unwrap_or_else(|| effective_stable(m, b.parent.as_ref()));
                let after = if let Some(l) = fields.get("lifecycle") {
                    l.as_str() == Some(STABLE)
                } else if unset.iter().any(|u| u == "lifecycle") {
                    inherited()
                } else if moved_to.is_some() {
                    own_lifecycle(b).map_or_else(inherited, |l| l.as_str() == Some(STABLE))
                } else {
                    before
                };
                out.push(Subject {
                    target: b.id.clone(),
                    stable: before || after,
                    created_type: None,
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
                    created_type: None,
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
                    created_type: None,
                });
            }
            OpInput::Rebalance { parent } => match anchor(&created, parent.as_deref()) {
                Anchor::At(p) => {
                    if m.get(&p).is_some_and(|b| !exempt(b)) {
                        out.push(Subject {
                            stable: effective_stable(m, Some(&p)),
                            target: p,
                            created_type: None,
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
                                created_type: None,
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
/// `reaffirmed` is what the write's reaffirm ops selected on `m` (`reaffirm::select`). `graph`
/// gives `m`'s graph, read only when a cited briefing is held: its governing state is checked
/// against it (rf6).
pub(crate) fn check(
    pack: &Pack,
    reg: &BriefRegistry,
    m: &Materialized,
    graph: impl FnOnce() -> Result<Arc<Graph>, RdyError>,
    ops: &[OpInput],
    reaffirmed: &crate::reaffirm::Selection,
    brief: Option<&str>,
) -> Result<Vec<Issue>, RdyError> {
    let subjects = subjects(pack, m, ops, reaffirmed);
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
                let moved: BTreeSet<BlockId> = rec.stale(pack, m, &*graph()?).into_iter().collect();
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
        // the type of each target's first stable create, in op order: a create's parent needs
        // the briefing, so the refusal names it as one
        let mut created: HashMap<&BlockId, &str> = HashMap::new();
        for s in subjects.iter().filter(|s| s.stable) {
            if let Some(t) = s.created_type {
                created.entry(&s.target).or_insert(t);
            }
        }
        let mut entries = Vec::with_capacity(missing.len());
        let mut issues = Vec::with_capacity(missing.len());
        for id in &missing {
            if let Some(t) = created.get(id) {
                entries.push(format!("{id} (the parent of the new {t})"));
                issues.push(block_issue(
                    id,
                    &format!("not covered by the cited briefing: the write creates a {t} under it"),
                    "brief this parent; a briefing of a parent covers its new children",
                ));
            } else {
                entries.push(id.to_string());
                issues.push(block_issue(
                    id,
                    "not covered by the cited briefing",
                    "brief it (or, to create under it, its parent)",
                ));
            }
        }
        return Err(refuse(
            E_BRIEF_REQUIRED,
            format!(
                "the cited briefing does not cover {}, which this write touches on stable content",
                entries.join(", ")
            ),
            issues,
        ));
    }
    let moved: BTreeSet<BlockId> = rec.stale(pack, m, &*graph()?).into_iter().collect();
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
