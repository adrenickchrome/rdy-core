//! What a write or a view op leaves for its author to do: the blocks that depend on what it
//! changed, read from the state it published, and the conflicts it recorded. Each entry has a
//! stable id (`cq-<16 hex>`).
use crate::brief::{BRIEF_DEFAULT_TOKENS, FollowUp, expand_error, follow_up};
use crate::consequence_literals::{
    LiteralMatch, LiteralSet, changed as changed_literals, changed_from, changed_many, evidence,
};
use crate::coverage::{coverage_change, coverage_error_text};
use crate::engine::{Engine, write_typed};
use crate::error::{E_CONFLICT_UNRESOLVED, E_INTERNAL, RdyError};
use crate::gate::{LandInfo, ProposalInfo, applied_ops, changes_of, landed_as};
use crate::intent::IntentNote;
use crate::lexicon::Lexicon;
use crate::tensions::delta::on_change;
use crate::tensions::{SideRef, Tension, tension_label};
use crate::tensions::{TensionDelta, tension_delta};
use crate::views::{ReplayResult, UndoResult, ViewOpResult};
use crate::{Materialized, Rdy, ReadState, Scope, now_ms};
use rdy_graph::{
    DerivedStatus, Edge, EdgeKind, Graph, IMPACT_LINKS, SUSPECT_LINKS, Suspect, edges_of,
};
use rdy_model::Tracking;
use rdy_model::decision::{AFFECTS, DECISION, REFINES, REVOKES, SUPERSEDES};
use rdy_model::{Block, BlockId, Hash, Link, Op, Proposal};
use rdy_store::StoreError;
use serde::Serialize;
use std::cell::{OnceCell, RefCell};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// Why a block is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsequenceKind {
    /// The block is suspect on a changed block in the state the write published.
    Suspect,
    /// The block depends on a changed block but cannot be suspect (yet).
    Dependent,
    /// A view op recorded a new conflict on the block.
    Conflict,
    /// The change opened a tension (or, for a view op, the view has a new one).
    Tension,
}

impl ConsequenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConsequenceKind::Suspect => "suspect",
            ConsequenceKind::Dependent => "dependent",
            ConsequenceKind::Conflict => "conflict",
            ConsequenceKind::Tension => "tension",
        }
    }
}

/// The edge and the changed block behind a consequence.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Because {
    /// The representative edge.
    pub edge: String,
    pub changed: String,
    /// For a dependent, suspect or kept suspect entry: the representative edge and every link
    /// kind impact follows from the block to `changed`, sorted, each once (D-Q69VB, O13). A
    /// reference is listed only as the representative.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edges: Option<Vec<String>>,
    /// For a block that is not a decision, on a target that exists: the first eight literals
    /// the change removed (D-CFC6V), as first written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    /// How many removed literals past the eight `values` shows were inspected.
    #[serde(skip_serializing_if = "is_zero")]
    pub values_omitted: usize,
}

impl Because {
    /// A cause with no edge set and no literals (a conflict, a tension).
    fn bare(edge: impl Into<String>, changed: impl Into<String>) -> Because {
        Because {
            edge: edge.into(),
            changed: changed.into(),
            edges: None,
            values: None,
            values_omitted: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Consequence {
    pub id: String,
    pub block: String,
    pub kind: ConsequenceKind,
    pub because: Because,
    pub follow_up: FollowUp,
    /// Where the block states a removed literal: at most eight rows, in (field, line, span)
    /// order. On an `update` the match causes, and on a reach entry as evidence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matched: Vec<LiteralMatch>,
    /// How many more rows were found.
    #[serde(skip_serializing_if = "is_zero")]
    pub matched_omitted: usize,
    /// For a `conflict` entry: the conflict's id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    /// For a `tension` entry: the tension's display id (`tn-…`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// The view's own included changes already wrote this block (FW-LP2).
    #[serde(skip_serializing_if = "is_false")]
    pub already_written: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Consequences {
    pub items: Vec<Consequence>,
    /// How many entries a cut dropped, by kind.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, usize>,
    /// Why the tension entries could not be read (the other entries stand); a reply carries it
    /// as `consequences_tension_error`.
    #[serde(skip)]
    pub tensions_error: Option<String>,
}

/// D-01QX5 (p-EC3VW): a write's reply row for a block it rewrote with its glob set unchanged
/// while its reflected code still differs from the recorded snapshot. A read projection made at
/// report time: never in a TRANSITION, the store's suspect table or a change plan. It has no
/// target, so it is its own row shape beside [`Consequence`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CodeKept {
    pub id: String,
    pub block: String,
    pub kind: ConsequenceKind,
    pub because: CodeBecause,
    pub follow_up: FollowUp,
    pub reason: &'static str,
    /// Always `None`: the cause is the code, not a block.
    pub target: Option<String>,
    /// The changed matched files, as `<repo>:<path>`, sorted.
    pub paths: Vec<String>,
    pub already_written: bool,
    pub fix: &'static str,
}

/// The cause of a [`CodeKept`] row: the `reflects` edge, with no changed block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CodeBecause {
    pub edge: &'static str,
    pub changed: Option<String>,
}

/// The fix of a [`CodeKept`] row.
pub const CODE_KEPT_FIX: &str = "check the block against its code, then repin it with code: true";

/// One row of a write's `consequences`: a structural entry, or a code-kept row after them all.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ReplyRow {
    Consequence(Consequence),
    CodeKept(CodeKept),
}

impl ReplyRow {
    fn kind(&self) -> &'static str {
        match self {
            ReplyRow::Consequence(c) => c.kind.as_str(),
            ReplyRow::CodeKept(c) => c.kind.as_str(),
        }
    }
}

/// The hint on a `reaffirm` entry.
pub const REAFFIRM_FIX: &str =
    "if it still holds, re-affirm it with a reaffirm op; otherwise supersede it";

/// The hint on a kept `suspect` entry (D-W7KAK) whose target changed, for a block that is not a
/// live decision: the write kept the old pin.
pub const KEPT_CHANGED_FIX: &str = "this write kept its pin on the old version of the target. Check that it still holds, then repin it with a repin op.";

/// The hint on a kept `suspect` entry whose target was deleted: there is no version to pin.
pub const KEPT_DELETED_FIX: &str =
    "its target is gone. Remove the reference or the link, or point it at another block.";

/// The hint on a kept `suspect` entry whose target is superseded or revoked: a repin does not
/// clear it, only a new citation does.
pub const KEPT_REPLACED_FIX: &str =
    "its target is superseded or revoked. Point it at the decision in force, or remove the link.";

/// The hint on a `conflict` entry.
pub const CONFLICT_FIX: &str = "write the block from the version you want (rdy_context lists the sides), or exclude one side's change";

/// The hint on a `tension` entry.
pub const TENSION_FIX: &str = "resolve it with a change (rdy_tensions shows both sides), or ask a reviewer to dismiss it with rdy_resolve";

/// D-CFC6V's fix texts, after the decision and kept-suspect precedence. A full supersede whose
/// dependent states no removed value.
pub const SUPERSEDE_CONFIRM_FIX: &str = "this block states no removed value. Check that it still holds, then point each listed link at the decision in force.";
/// A revoke whose dependent states no value of the decision.
pub const REVOKE_CONFIRM_FIX: &str = "this block states no value of the decision. Check that it still holds, then remove each listed link or point it elsewhere.";
/// A full supersede whose dependent states a removed value.
pub const SUPERSEDE_UPDATE_FIX: &str =
    "update the block for the removed values. Point each listed link at the decision in force.";
/// A revoke whose dependent states a removed value.
pub const REVOKE_UPDATE_FIX: &str =
    "update the block for the removed values. Remove each listed link or point it elsewhere.";
/// A full supersede whose dependent was read only in part, with no match.
pub const SUPERSEDE_CUT_FIX: &str = "rdy checked only the first 16384 bytes. Check the rest, then point each listed link at the decision in force.";
/// A revoke whose dependent was read only in part, with no match.
pub const REVOKE_CUT_FIX: &str = "rdy checked only the first 16384 bytes. Check the rest, then remove each listed link or point it elsewhere.";
/// An ordinary change whose dependent was read only in part, with no match.
pub const EDIT_CUT_FIX: &str =
    "rdy checked only the first 16384 bytes. Check the rest of the block and update it if needed.";

/// The hint on an entry for a block that a refined or part-superseded decision governs (FW-LP6).
/// The governing decision did not change, so the block already pins it: a repin is refused, and
/// the hint never asks for one (D-M19GV).
pub const LINEAGE_FIX: &str = "a decision that governs it was refined or partly superseded: check that it still holds, and update it if not; no repin is needed";

/// D-ATBP6: the hint on a `confirm` entry for an accepted decision in
/// a refine's or part supersede's reach. An accepted decision is frozen, so it is never asked to
/// be updated, and the reach moved no pin, so it is never asked to repin.
pub const LINEAGE_DECISION_FIX: &str = "a decision that governs it was refined or partly superseded: check that it still holds, and supersede it if not; no repin is needed";

/// D-ATBP6: the hint on a `confirm` entry for an accepted decision that
/// any other route reaches without a suspect on the cause (a `describes` link, a premise edge
/// with no pin): a `reaffirm` op would be refused, as there is no suspect to re-affirm.
pub const REACH_CONFIRM_FIX: &str = "a block it relies on changed: check that it still holds, and supersede it if not; no repin is needed";

/// FW-LP6: for each decision `ops` create, each `refines` link and each `supersedes` link with a
/// part: (the new decision, the target, the link's name). In op order, then link order. The
/// refines links a full supersede copied are dropped later, with the graph ([`refines_copied`]).
pub(crate) fn reached_targets(ops: &[Op]) -> Vec<(BlockId, BlockId, &'static str)> {
    let mut out = vec![];
    for op in ops {
        let Op::Create { block } = op else { continue };
        if block.block_type != DECISION {
            continue;
        }
        for l in block.links.get(REFINES).into_iter().flatten() {
            out.push((block.id.clone(), l.target.clone(), REFINES));
        }
        for l in block.links.get(SUPERSEDES).into_iter().flatten() {
            if l.part.is_some() {
                out.push((block.id.clone(), l.target.clone(), SUPERSEDES));
            }
        }
    }
    out
}

/// Whether decision `id`'s refines links are the ones a full supersede copied (sup1): it fully
/// supersedes a decision whose refines targets are exactly its own. Those do not reach (FW-LP6a
/// is M3); refines given to rdy_supersede differ from the old ones, and reach as any refine
/// does (final review 2, finding 10).
pub(crate) fn refines_copied(g: &Graph, id: &BlockId) -> bool {
    let refines = |b: &BlockId| -> BTreeSet<BlockId> {
        g.outgoing(b)
            .iter()
            .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == REFINES))
            .map(|e| e.to.clone())
            .collect()
    };
    let mine = refines(id);
    !mine.is_empty()
        && g.outgoing(id).iter().any(|e| {
            matches!(&e.kind, EdgeKind::Link(n) if n == SUPERSEDES)
                && e.part.is_none()
                && refines(&e.to) == mine
        })
}

/// `cq-` and the first 16 hex characters of the hash of the batch, kind and target.
/// For a write, `batch` is the change hash; for a view op, the view op's hash.
pub fn consequence_id(batch: &Hash, kind: &str, target: &BlockId) -> String {
    let mut bytes = b"rdy-consequence\0".to_vec();
    bytes.extend_from_slice(batch.to_string().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(target.to_string().as_bytes());
    format!("cq-{}", &Hash::of(&bytes).to_string()[..16])
}

/// The first cause found for a dependent block, with its edge set ([`edge_set`]).
struct Cause {
    suspect: bool,
    edge: String,
    edges: Vec<String>,
    changed: BlockId,
}

/// A premise edge: one that can make its source suspect.
fn premise(e: &Edge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => SUSPECT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

/// An edge impact follows.
fn impact(e: &Edge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => IMPACT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

/// The blocks `ops` write (`touched`), and those plus the `supersedes` / `revokes` targets of
/// the decisions they create, whose status changed (`changed`).
pub(crate) fn changed_by_ops(ops: &[Op]) -> (BTreeSet<BlockId>, BTreeSet<BlockId>) {
    let touched: BTreeSet<BlockId> = ops.iter().map(|o| o.block_id().clone()).collect();
    let mut changed = touched.clone();
    for op in ops {
        if let Op::Create { block } = op
            && block.block_type == DECISION
        {
            for name in [SUPERSEDES, REVOKES] {
                for l in block.links.get(name).into_iter().flatten() {
                    changed.insert(l.target.clone());
                }
            }
        }
    }
    (touched, changed)
}

/// The blocks `ops` create or update (`Op::Create`, `Op::Update`): those a kept `suspect` entry
/// may name (D-W7KAK).
fn rewritten_by(ops: &[Op]) -> BTreeSet<BlockId> {
    ops.iter()
        .filter(|o| matches!(o, Op::Create { .. } | Op::Update { .. }))
        .map(|o| o.block_id().clone())
        .collect()
}

/// Whether a block `ops` create cites (by a reference or a link) a block that `ops` do not write:
/// only then can a block a change set only creates be suspect after it (on a superseded or
/// revoked target, or, merged onto canon, on a target canon changed since).
fn creates_cite_outside(ops: &[Op]) -> bool {
    let written: BTreeSet<&BlockId> = ops.iter().map(Op::block_id).collect();
    ops.iter().any(|o| match o {
        Op::Create { block } => edges_of(block).iter().any(|e| !written.contains(&e.to)),
        _ => false,
    })
}

/// Whether `ops` create a decision that fully supersedes or revokes a block: then a block they
/// create can keep a link to a target a pending decision replaces (O9), even when every block
/// they cite is one they write.
fn creates_retirement(ops: &[Op]) -> bool {
    ops.iter().any(|o| match o {
        Op::Create { block } if block.block_type == DECISION => {
            block.links.get(REVOKES).is_some_and(|l| !l.is_empty())
                || block
                    .links
                    .get(SUPERSEDES)
                    .into_iter()
                    .flatten()
                    .any(|l| l.part.is_none())
        }
        _ => false,
    })
}

/// D-W7KAK: the follow-up and the fix of a kept `suspect` entry, by the suspect's reason and
/// whether the block is a live or partly superseded decision; `None` for any other reason. A
/// live decision is frozen, so it gets `reaffirm` for every reason, as `follow_up` gives it.
fn kept_follow_up(reason: &str, live_decision: bool) -> Option<(FollowUp, String)> {
    let (follow, fix) = match reason {
        "changed" | "deleted" | "superseded" | "revoked" if live_decision => {
            (FollowUp::Reaffirm, REAFFIRM_FIX)
        }
        "changed" => (FollowUp::Confirm, KEPT_CHANGED_FIX),
        "deleted" => (FollowUp::Update, KEPT_DELETED_FIX),
        "superseded" | "revoked" => (FollowUp::Update, KEPT_REPLACED_FIX),
        _ => return None,
    };
    Some((follow, fix.to_string()))
}

/// The words after the code of a version's `consequences_error` while `ids` are in conflict
/// (sorted, each once).
pub(crate) fn conflict_message(ids: &[String]) -> String {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    match ids.as_slice() {
        [one] => format!(
            "no consequences while {one} is in conflict. Write it from the version you want (rdy_context lists the sides), then propose again."
        ),
        _ => format!(
            "no consequences while {} are in conflict. Write each from the version you want (rdy_context lists the sides), then propose again.",
            ids.join(", ")
        ),
    }
}

/// A version's `consequences_error` while `ids` are in conflict: it has no consequences until
/// the conflicts are resolved and it is proposed again (ledger O4).
fn conflict_error(ids: &[String]) -> String {
    format!("{E_CONFLICT_UNRESOLVED}: {}", conflict_message(ids))
}

/// A changed target's lineage event in this change (M10): a full supersede or a revoke of the
/// target (`Retired`), a refine or part supersede of it (`Partial`), or neither.
#[derive(Clone, Copy, Debug)]
enum Lineage<'r> {
    Ordinary,
    Retired(&'r Retired),
    Partial,
}

/// The decisions that fully supersede a target, by id, and whether one revokes it (a revoke
/// wins: the target's literals are all removed).
#[derive(Clone, Debug, Default)]
struct Retired {
    revoke: bool,
    by: BTreeSet<BlockId>,
}

/// The lineage events of decisions: the targets each fully supersedes or revokes, and the
/// targets each refines or part-supersedes.
#[derive(Debug, Default)]
struct Events {
    retired: BTreeMap<BlockId, Retired>,
    partial: BTreeMap<BlockId, BTreeSet<BlockId>>,
}

impl Events {
    /// The events of decision `b`'s own lineage links (none for another type).
    fn add_decision(&mut self, b: &Block) {
        if b.block_type != DECISION {
            return;
        }
        for l in b.links.get(SUPERSEDES).into_iter().flatten() {
            match l.part {
                None => {
                    self.retired
                        .entry(l.target.clone())
                        .or_default()
                        .by
                        .insert(b.id.clone());
                }
                Some(_) => {
                    self.partial
                        .entry(l.target.clone())
                        .or_default()
                        .insert(b.id.clone());
                }
            }
        }
        for l in b.links.get(REVOKES).into_iter().flatten() {
            self.retired.entry(l.target.clone()).or_default().revoke = true;
        }
    }

    /// The events of the decisions `ops` create; the refines and part supersedes as `reach`
    /// lists them (copied refines already dropped).
    fn of_ops(ops: &[Op], reach: &[(BlockId, BlockId, &'static str)]) -> Events {
        let created = ops.iter().filter_map(|op| match op {
            Op::Create { block } => Some(block),
            _ => None,
        });
        Events::of_blocks(created, reach)
    }

    /// The events of the decisions among `blocks`; the refines and part supersedes as `reach`
    /// lists them (copied refines already dropped).
    fn of_blocks<'b>(
        blocks: impl IntoIterator<Item = &'b Block>,
        reach: &[(BlockId, BlockId, &'static str)],
    ) -> Events {
        let mut ev = Events::default();
        for block in blocks {
            ev.add_decision(block);
        }
        ev.partial.clear();
        for (decision, target, _) in reach {
            ev.partial
                .entry(target.clone())
                .or_default()
                .insert(decision.clone());
        }
        ev
    }
}

/// One baseline a target's literals are compared with (O8, D-EGXAS): a scope of a state (read
/// on first use), the empty state (a view on no landing, a first landing), or a write's start.
enum Baseline<'a> {
    Scope {
        st: &'a ReadState,
        scope: Scope,
        read: OnceCell<(Arc<Materialized>, Arc<Lexicon>)>,
    },
    Empty,
    /// A state already read: its blocks and their names (a change plan's canon baseline).
    Ready {
        m: Arc<Materialized>,
        names: Arc<Lexicon>,
    },
    /// Each block the changes write and the version its first update or delete replaced (`None`
    /// for one they create); read on first use into those versions and their lexicon.
    Start {
        first: BTreeMap<BlockId, Option<Hash>>,
        read: OnceCell<(BTreeMap<BlockId, Block>, Arc<Lexicon>)>,
    },
}

impl<'a> Baseline<'a> {
    fn scope(st: &'a ReadState, scope: Scope) -> Baseline<'a> {
        Baseline::Scope {
            st,
            scope,
            read: OnceCell::new(),
        }
    }

    /// The write's start of the blocks `ops` write, in op order: the first op on a block gives
    /// its start.
    fn start<'o>(ops: impl IntoIterator<Item = &'o Op>) -> Baseline<'a> {
        let mut first = BTreeMap::new();
        for op in ops {
            first.entry(op.block_id().clone()).or_insert(op.base());
        }
        Baseline::Start {
            first,
            read: OnceCell::new(),
        }
    }
}

/// The names the state after the change is read with: a lexicon given, or a scope's, read on
/// first use. Most changes have no dependent to judge, and a lexicon that is not built yet reads
/// every block of the scope.
enum Names<'a> {
    Ready(Arc<Lexicon>),
    Scope {
        st: &'a ReadState,
        scope: Scope,
        read: OnceCell<Arc<Lexicon>>,
    },
}

impl<'a> Names<'a> {
    fn scope(st: &'a ReadState, scope: Scope) -> Names<'a> {
        Names::Scope {
            st,
            scope,
            read: OnceCell::new(),
        }
    }

    fn get(&self) -> Result<&Arc<Lexicon>, RdyError> {
        match self {
            Names::Ready(l) => Ok(l),
            Names::Scope { st, scope, read } => {
                if read.get().is_none() {
                    let _ = read.set(st.lexicon(scope)?);
                }
                Ok(read.get().expect("set above"))
            }
        }
    }
}

/// What a consequence compares (O8): the state after the change (`m`, read with `names`), its
/// ordinary baselines in order, and the lineage events of the change. Nothing is stored; each
/// target's changed literals are worked out once.
struct Compare<'a> {
    st: &'a ReadState,
    m: &'a Materialized,
    names: Names<'a>,
    baselines: Vec<Baseline<'a>>,
    events: Events,
    cache: RefCell<BTreeMap<(BlockId, LiteralsKey), LiteralSet>>,
}

/// What a target's changed literals depend on besides the target: its lineage event and, for
/// a retirement, whether a revoke is among them and which decisions replace it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LiteralsKey {
    Ordinary,
    Retired(bool, BTreeSet<BlockId>),
    Partial,
}

/// The follow-up of one entry, its fix and its evidence.
struct Judged {
    follow: FollowUp,
    /// An accepted decision reached with no suspect on the cause (D-ATBP6): its `confirm` is a
    /// confirm item, whatever the cause's lineage.
    reached_decision: bool,
    fix: Option<String>,
    values: Option<Vec<String>>,
    values_omitted: usize,
    matched: Vec<LiteralMatch>,
    matched_omitted: usize,
}

impl<'a> Compare<'a> {
    fn new(
        st: &'a ReadState,
        m: &'a Materialized,
        names: Names<'a>,
        baselines: Vec<Baseline<'a>>,
        events: Events,
    ) -> Compare<'a> {
        Compare {
            st,
            m,
            names,
            baselines,
            events,
            cache: RefCell::default(),
        }
    }

    /// Target `x`'s lineage event in this change: a revoke or full supersede first.
    fn lineage_of(&self, x: &BlockId) -> Lineage<'_> {
        match self.events.retired.get(x) {
            Some(r) => Lineage::Retired(r),
            None if self.events.partial.contains_key(x) => Lineage::Partial,
            None => Lineage::Ordinary,
        }
    }

    /// Block `x` in baseline `b`, with the names to read it under.
    fn in_baseline(
        &self,
        b: &Baseline<'a>,
        x: &BlockId,
    ) -> Result<Option<(Block, Arc<Lexicon>)>, RdyError> {
        match b {
            Baseline::Empty => Ok(None),
            Baseline::Ready { m, names } => {
                Ok(m.get(x).map(|blk| (blk.clone(), Arc::clone(names))))
            }
            Baseline::Scope { st, scope, read } => {
                if read.get().is_none() {
                    let _ = read.set((st.materialized(scope)?, st.lexicon(scope)?));
                }
                let (m, names) = read.get().expect("set above");
                Ok(m.get(x).map(|blk| (blk.clone(), Arc::clone(names))))
            }
            Baseline::Start { first, read } => {
                if read.get().is_none() {
                    let mut blocks = BTreeMap::new();
                    for (id, base) in first {
                        if let Some(h) = base {
                            blocks.insert(id.clone(), self.st.reader().get_block(h)?);
                        }
                    }
                    // the final lexicon with the written blocks put back as they were
                    let names = self.names.get()?.with_changes(
                        first.keys().filter_map(|id| self.m.get(id)),
                        blocks.values(),
                    );
                    let _ = read.set((blocks, Arc::new(names)));
                }
                let (blocks, names) = read.get().expect("set above");
                let blk = match first.get(x) {
                    Some(_) => blocks.get(x),
                    None => self.m.get(x),
                };
                Ok(blk.map(|b| (b.clone(), Arc::clone(names))))
            }
        }
    }

    /// The literals the change removed from target `x` (D-EGXAS): for a revoke all of the old
    /// decision's, for a full supersede those that some replacement lacks, for a refine or part
    /// supersede those the new decisions lack (each a decision's title, then statement); else
    /// those that a baseline holds and the final text lacks, each once, under one cap.
    fn literals(&self, x: &BlockId, lineage: Lineage<'_>) -> Result<LiteralSet, RdyError> {
        // a retired target is compared with the replacements given: the change's own (a normal
        // entry) and every active one (a retained link) can differ in one reply
        let key = match lineage {
            Lineage::Ordinary => LiteralsKey::Ordinary,
            Lineage::Retired(r) => LiteralsKey::Retired(r.revoke, r.by.clone()),
            Lineage::Partial => LiteralsKey::Partial,
        };
        let key = (x.clone(), key);
        if let Some(l) = self.cache.borrow().get(&key) {
            return Ok(l.clone());
        }
        let pack = self.st.pack();
        let names = &**self.names.get()?;
        let set = match lineage {
            Lineage::Retired(r) => match self.m.get(x) {
                None => LiteralSet::default(),
                Some(old) if r.revoke => changed_literals(pack, old, None, names, names, true),
                Some(old) => {
                    let news: Vec<&Block> = r.by.iter().filter_map(|b| self.m.get(b)).collect();
                    changed_many(pack, old, &news, names, names, true)
                }
            },
            Lineage::Partial => match self.m.get(x) {
                None => LiteralSet::default(),
                Some(old) => {
                    let news: Vec<&Block> = self
                        .events
                        .partial
                        .get(x)
                        .into_iter()
                        .flatten()
                        .filter_map(|b| self.m.get(b))
                        .collect();
                    changed_many(pack, old, &news, names, names, true)
                }
            },
            Lineage::Ordinary => {
                // an equal block read under other names can hold other entities (D-CFC6V), so
                // a baseline is skipped only when both its block and its names are the same;
                // `changed_from` keeps each identity once
                let mut olds: Vec<(Block, Arc<Lexicon>)> = vec![];
                for b in &self.baselines {
                    if let Some((blk, lex)) = self.in_baseline(b, x)?
                        && !olds.iter().any(|(o, l)| *o == blk && Arc::ptr_eq(l, &lex))
                    {
                        olds.push((blk, lex));
                    }
                }
                let new = self.m.get(x);
                let pair = new
                    .or(olds.first().map(|(b, _)| b))
                    .is_some_and(|b| b.block_type == DECISION);
                let refs: Vec<(&Block, &Lexicon)> = olds.iter().map(|(b, l)| (b, &**l)).collect();
                changed_from(pack, &refs, new, names, pair)
            }
        };
        self.cache.borrow_mut().insert(key, set.clone());
        Ok(set)
    }

    /// The follow-up of `block` (as `src`) on target `x` (D-CFC6V): a decision's by its status
    /// and, for an accepted one, whether it is `suspect` on `x` ([`reached_follow_up`]), update
    /// for a deleted target, else from the literals it states. `None` for a decision that
    /// asserts nothing live.
    fn judge(
        &self,
        g: &Graph,
        block: &BlockId,
        src: &Block,
        x: &BlockId,
        lineage: Lineage<'_>,
        suspect: bool,
    ) -> Result<Option<Judged>, RdyError> {
        let status = g.status(block);
        let deleted = self.m.get(x).is_none();
        if status.is_some() || deleted {
            let Some(follow) = follow_up(status.as_ref(), deleted, false, false) else {
                return Ok(None);
            };
            let (follow, fix, reached_decision) = reached_follow_up(follow, suspect, lineage);
            return Ok(Some(Judged {
                follow,
                reached_decision,
                fix: fix.map(str::to_string),
                values: None,
                values_omitted: 0,
                matched: vec![],
                matched_omitted: 0,
            }));
        }
        let lits = self.literals(x, lineage)?;
        let ev = evidence(self.st.pack(), &lits, src, self.names.get()?);
        let hit = !ev.matched.is_empty();
        let (follow, fix) = match lineage {
            // D-M19GV: a refine or part supersede keeps confirm, whatever it matched
            Lineage::Partial => (FollowUp::Confirm, Some(LINEAGE_FIX)),
            _ => {
                let follow = if hit || ev.cut {
                    FollowUp::Update
                } else {
                    FollowUp::Confirm
                };
                let fix = match (lineage, follow, hit) {
                    (Lineage::Retired(r), FollowUp::Confirm, _) => Some(if r.revoke {
                        REVOKE_CONFIRM_FIX
                    } else {
                        SUPERSEDE_CONFIRM_FIX
                    }),
                    (Lineage::Retired(r), _, true) => Some(if r.revoke {
                        REVOKE_UPDATE_FIX
                    } else {
                        SUPERSEDE_UPDATE_FIX
                    }),
                    (Lineage::Retired(r), _, false) => Some(if r.revoke {
                        REVOKE_CUT_FIX
                    } else {
                        SUPERSEDE_CUT_FIX
                    }),
                    (_, FollowUp::Update, false) => Some(EDIT_CUT_FIX),
                    _ => None,
                };
                (follow, fix)
            }
        };
        let (matched, matched_omitted) = if hit {
            (ev.matched, ev.matched_omitted)
        } else {
            (vec![], 0)
        };
        Ok(Some(Judged {
            follow,
            reached_decision: false,
            fix: fix.map(str::to_string),
            values: Some(ev.values),
            values_omitted: ev.values_omitted,
            matched,
            matched_omitted,
        }))
    }
}

/// D-ATBP6 (refines D-4XTRC and D-7M06Q): the follow-up `follow_up` gave a decision subject,
/// with its fix, and whether it is an accepted decision's `confirm`. An accepted decision keeps
/// `reaffirm` only when it is `suspect` on the cause (the suspects detector's own test of the
/// pair, so a `reaffirm` op accepts it); any other reach gives `confirm`: in a refine's or part
/// supersede's reach ([`Lineage::Partial`]) with [`LINEAGE_DECISION_FIX`], else with
/// [`REACH_CONFIRM_FIX`]. Its own `affects` link gives nothing: [`visit_pairs`] skips it. Every
/// other follow-up is kept, with no fix.
fn reached_follow_up(
    follow: FollowUp,
    suspect: bool,
    lineage: Lineage<'_>,
) -> (FollowUp, Option<&'static str>, bool) {
    match follow {
        FollowUp::Reaffirm if suspect => (FollowUp::Reaffirm, Some(REAFFIRM_FIX), false),
        FollowUp::Reaffirm => {
            let fix = match lineage {
                Lineage::Partial => LINEAGE_DECISION_FIX,
                Lineage::Ordinary | Lineage::Retired(_) => REACH_CONFIRM_FIX,
            };
            (FollowUp::Confirm, Some(fix), true)
        }
        other => (other, None, false),
    }
}

/// The edge set of a cause (O13): the representative `rep` and every link kind impact follows
/// among `edges`, sorted, each once. A reference is in it only as `rep`; `contains` never.
fn edge_set<'e>(rep: &str, edges: impl IntoIterator<Item = &'e Edge>) -> Vec<String> {
    let mut out: Vec<String> = edges
        .into_iter()
        .filter_map(|e| match &e.kind {
            EdgeKind::Link(n) if IMPACT_LINKS.contains(&n.as_str()) => Some(n.clone()),
            _ => None,
        })
        .collect();
    out.push(rep.to_string());
    out.sort();
    out.dedup();
    out
}

/// One subject/cause pair the candidate visitor found: block `from` points at the changed block
/// `changed` through `edges` (by name; never `contains`), and `edge` is the representative.
struct Pair<'e> {
    from: BlockId,
    changed: BlockId,
    suspect: bool,
    edge: &'e Edge,
    edges: Vec<&'e Edge>,
}

/// The candidate visitor the report and the change plan share: for each block of `changed`, in
/// order, each block that `incoming` lists with an edge to it and `keep` admits, in id order. The
/// representative edge of a block `suspect_on` calls suspect on the changed block is its least
/// premise edge by name (else its least edge); of any other block, its least impact edge. A block
/// with no such edge is no pair. D-4XTRC: a change does not reach a decision (`is_decision`)
/// through that decision's own `affects` link, so a decision whose every edge to the changed
/// block is an `affects` link is no pair; one with another edge to it keeps its pair and every
/// edge.
fn visit_pairs<'e>(
    changed: &BTreeSet<BlockId>,
    incoming: impl Fn(&BlockId) -> Vec<&'e Edge>,
    keep: impl Fn(&BlockId) -> bool,
    is_decision: impl Fn(&BlockId) -> bool,
    mut suspect_on: impl FnMut(&BlockId, &BlockId) -> bool,
    mut visit: impl FnMut(Pair<'e>) -> Result<(), RdyError>,
) -> Result<(), RdyError> {
    for x in changed {
        let mut by_from: BTreeMap<&BlockId, Vec<&'e Edge>> = BTreeMap::new();
        for e in incoming(x) {
            if e.kind != EdgeKind::Contains {
                by_from.entry(&e.from).or_default().push(e);
            }
        }
        for (from, mut edges) in by_from {
            if !keep(from) {
                continue;
            }
            let own_affects = edges
                .iter()
                .all(|e| matches!(&e.kind, EdgeKind::Link(n) if n == AFFECTS));
            if own_affects && is_decision(from) {
                continue;
            }
            edges.sort_by_key(|e| e.kind.name());
            let suspect = suspect_on(from, x);
            let pick = if suspect {
                edges.iter().find(|e| premise(e)).or_else(|| edges.first())
            } else {
                edges.iter().find(|e| impact(e))
            };
            let Some(&edge) = pick else { continue };
            visit(Pair {
                from: from.clone(),
                changed: x.clone(),
                suspect,
                edge,
                edges,
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Test counter: the subject/cause pairs `plan_pairs` has judged on this thread, so a test
    /// can see where a plan stops.
    pub(crate) static PLAN_PAIRS_JUDGED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Counts one judged plan pair (tests only).
fn count_plan_pair() {
    #[cfg(test)]
    PLAN_PAIRS_JUDGED.with(|c| c.set(c.get() + 1));
}

/// One state a change plan reads (D-4JMPH): its blocks, its graph and its names.
pub(crate) struct NetState {
    pub(crate) m: Arc<Materialized>,
    pub(crate) g: Arc<Graph>,
    pub(crate) names: Arc<Lexicon>,
}

/// One subject/cause pair of the change plan, judged as the consequence path judges it (O3).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlanPair {
    pub(crate) subject: BlockId,
    pub(crate) cause: BlockId,
    /// The representative edge (for a reach, the reaching link's name).
    pub(crate) edge: String,
    pub(crate) follow: FollowUp,
    /// An accepted decision reached with no suspect on the cause: its `confirm` is a confirm
    /// item, never a rejustify (D-ATBP6).
    pub(crate) reached_decision: bool,
    /// The version fully supersedes or revokes the cause.
    pub(crate) retired: bool,
    /// A refine or part supersede reached the subject (FW-LP6); it has no edge to a changed
    /// block.
    pub(crate) reach: bool,
    /// The baseline subject had an edge (or, for a reach, a governing link) to the cause.
    pub(crate) in_base: bool,
    /// The subject is suspect on the cause in the final state.
    pub(crate) suspect: bool,
    /// The baseline subject held a pinned premise edge to the cause that the final cause no
    /// longer matches: the suspect a repin clears.
    pub(crate) prior_suspect: bool,
}

/// Whether `block` is suspect on `target` in `g` (over `m`), as the suspects detector reads the
/// pair: a presentational source is no suspect.
fn suspect_on(
    st: &ReadState,
    g: &Graph,
    m: &Materialized,
    block: &BlockId,
    target: &BlockId,
) -> bool {
    g.suspects_of(block)
        .iter()
        .any(|s| s.target == *target && !st.is_presentational_source(m, s))
}

/// Whether `before`, a subject's baseline version, holds a premise edge to `x` with a pin that
/// the final `x` (`after`, read under `t`) no longer matches; a deleted `x` matches no pin. A
/// removed (redacted) `x` has no content to compare, so it gives none. The caller applies the
/// suspects detector's presentational exclusion (D-ATBP6: no reaffirm item without a suspect).
fn prior_suspect(before: Option<&Block>, after: Option<&Block>, x: &BlockId, t: &Tracking) -> bool {
    let Some(before) = before else {
        return false;
    };
    edges_of(before).iter().any(|e| {
        e.to == *x
            && premise(e)
            && match (e.pin, after) {
                (Some(pin), Some(a)) => !a.is_redacted() && pin != a.content_hash_with(t),
                (Some(_), None) => true,
                (None, _) => false,
            }
    })
}

impl ReadState {
    /// The change plan's projection of the candidate visitor (O3): every subject/cause pair of
    /// the version's net state `fin` against its canon baseline `base`, with `touched` the
    /// blocks whose content the version changes or deletes and `reach` its refine and part
    /// supersede links (decision, target, name). The changed blocks are `touched` and the
    /// `supersedes` / `revokes` targets of its decisions, in either state; their dependents are
    /// read through the edges of both states, the version's own written blocks included, by
    /// `Compare::judge` with the baseline as the one ordinary baseline. A subject is judged as
    /// its baseline version under the baseline's statuses (work the version did), or, when it is
    /// new, as its final version. No pair for a dead decision subject or a block and itself. A
    /// `supersedes`, `revokes` or `refines` link the version writes or removes is the change, not
    /// a dependency, so it gives no pair; the subject's other edges, and a lineage link it
    /// already held, still do. Then each reached target's governed blocks, judged as the reach
    /// entries are. A fully superseded decision's `affects` targets (in either state) are
    /// judged the same way, with the old decision as the cause and `supersedes` as the edge
    /// (D-B55PS); a revoke has none. Each pair goes to `visit` in visit order as it is judged, and the first
    /// error `visit` returns stops the walk: nothing is cut, and nothing is judged past a
    /// refusal.
    pub(crate) fn plan_pairs(
        &self,
        base: &NetState,
        fin: &NetState,
        touched: &BTreeSet<BlockId>,
        reach: &[(BlockId, BlockId, &'static str)],
        mut visit: impl FnMut(PlanPair) -> Result<(), RdyError>,
    ) -> Result<(), RdyError> {
        let mut changed = touched.clone();
        for id in touched {
            for b in [base.m.get(id), fin.m.get(id)].into_iter().flatten() {
                if b.block_type != DECISION {
                    continue;
                }
                for name in [SUPERSEDES, REVOKES] {
                    for l in b.links.get(name).into_iter().flatten() {
                        changed.insert(l.target.clone());
                    }
                }
            }
        }
        let events = Events::of_blocks(touched.iter().filter_map(|id| fin.m.get(id)), reach);
        let cmp = Compare::new(
            self,
            &fin.m,
            Names::Ready(Arc::clone(&fin.names)),
            vec![Baseline::Ready {
                m: Arc::clone(&base.m),
                names: Arc::clone(&base.names),
            }],
            events,
        );
        let t = Arc::clone(fin.g.tracking());
        let suspect = |s: &BlockId, x: &BlockId| {
            fin.g
                .suspects_of(s)
                .iter()
                .any(|v| v.target == *x && !self.is_presentational_source(&fin.m, v))
        };
        // the suspect a repin or reaffirm cleared, judged as the suspects detector judges it: a
        // presentational cause is never a suspect, so it gives no reaffirm item (D-ATBP6)
        let prior = |s: &BlockId, x: &BlockId| {
            prior_suspect(base.m.get(s), fin.m.get(x), x, &t)
                && !self.is_presentational_target(&fin.m, x)
        };
        // a lineage link the version itself writes or removes is the change, never a
        // dependency; a lineage link the subject already held, and every other edge, stay
        let own_lineage = |e: &Edge| {
            let EdgeKind::Link(n) = &e.kind else {
                return false;
            };
            if ![SUPERSEDES, REVOKES, REFINES].contains(&n.as_str()) {
                return false;
            }
            fn links<'b>(b: Option<&'b Block>, name: &str, to: &BlockId) -> Vec<&'b Link> {
                b.filter(|b| b.block_type == DECISION)
                    .and_then(|b| b.links.get(name))
                    .into_iter()
                    .flatten()
                    .filter(|l| l.target == *to)
                    .collect()
            }
            links(base.m.get(&e.from), n, &e.to) != links(fin.m.get(&e.from), n, &e.to)
        };
        // the version's own work is classified as it stood before (the baseline subject under
        // the baseline's statuses); a new subject as it stands after
        let judged_as = |s: &BlockId| -> Option<(&Graph, &Block)> {
            match base.m.get(s) {
                Some(b) => Some((&*base.g, b)),
                None => fin.m.get(s).map(|b| (&*fin.g, b)),
            }
        };
        visit_pairs(
            &changed,
            |x| {
                base.g
                    .incoming(x)
                    .iter()
                    .chain(fin.g.incoming(x))
                    .filter(|e| !own_lineage(e))
                    .collect()
            },
            |s| base.m.get(s).is_some() || fin.m.get(s).is_some(),
            |s| {
                base.m
                    .get(s)
                    .or_else(|| fin.m.get(s))
                    .is_some_and(|b| b.block_type == DECISION)
            },
            |s, x| suspect(s, x) || prior(s, x),
            |p| {
                if p.from == p.changed {
                    return Ok(());
                }
                let Some((g, src)) = judged_as(&p.from) else {
                    return Ok(());
                };
                let lineage = cmp.lineage_of(&p.changed);
                count_plan_pair();
                let Some(j) = cmp.judge(g, &p.from, src, &p.changed, lineage, p.suspect)? else {
                    return Ok(());
                };
                let in_base = base
                    .g
                    .outgoing(&p.from)
                    .iter()
                    .any(|e| e.to == p.changed && e.kind != EdgeKind::Contains);
                visit(PlanPair {
                    suspect: suspect(&p.from, &p.changed),
                    prior_suspect: prior(&p.from, &p.changed),
                    subject: p.from,
                    cause: p.changed,
                    edge: p.edge.kind.name(),
                    follow: j.follow,
                    reached_decision: j.reached_decision,
                    retired: matches!(lineage, Lineage::Retired(_)),
                    reach: false,
                    in_base,
                })
            },
        )?;
        for (decision, target, via) in reach {
            if matches!(
                fin.g.status(target),
                Some(
                    DerivedStatus::Rejected
                        | DerivedStatus::Superseded { .. }
                        | DerivedStatus::Revoked { .. }
                )
            ) {
                continue;
            }
            let before: BTreeSet<BlockId> = base.g.governed_by(target).into_iter().collect();
            let mut governed = before.clone();
            governed.extend(fin.g.governed_by(target));
            for block in governed {
                if block == *decision || block == *target {
                    continue;
                }
                let Some((g, src)) = judged_as(&block) else {
                    continue;
                };
                count_plan_pair();
                // D-ATBP6: an accepted decision is confirm here (LINEAGE_DECISION_FIX), unless it
                // is suspect on the target; a proposed one update; a dead one nothing
                let on = suspect(&block, target);
                let Some(j) = cmp.judge(g, &block, src, target, Lineage::Partial, on)? else {
                    continue;
                };
                visit(PlanPair {
                    suspect: on,
                    prior_suspect: false,
                    in_base: before.contains(&block),
                    subject: block,
                    cause: target.clone(),
                    edge: (*via).to_string(),
                    follow: j.follow,
                    reached_decision: j.reached_decision,
                    retired: false,
                    reach: true,
                })?;
            }
        }
        // D-B55PS: a full supersede also reaches the old decision's `affects` targets, each as
        // a reach of the old decision (the cause) judged as a part supersede's. A revoke does
        // not. A target that is already one of the old decision's dependents is judged by the
        // walk above, so it gets no second item
        let mut affected: BTreeSet<(BlockId, BlockId, BlockId)> = BTreeSet::new();
        for id in touched {
            let Some(b) = fin.m.get(id).filter(|b| b.block_type == DECISION) else {
                continue;
            };
            for l in b.links.get(SUPERSEDES).into_iter().flatten() {
                if l.part.is_some() {
                    continue;
                }
                for g in [&base.g, &fin.g] {
                    for e in g.outgoing(&l.target) {
                        if matches!(&e.kind, EdgeKind::Link(n) if n == AFFECTS) {
                            affected.insert((b.id.clone(), l.target.clone(), e.to.clone()));
                        }
                    }
                }
            }
        }
        for (decision, old, block) in affected {
            if block == decision || block == old {
                continue;
            }
            if matches!(
                fin.g.status(&block),
                Some(
                    DerivedStatus::Rejected
                        | DerivedStatus::Superseded { .. }
                        | DerivedStatus::Revoked { .. }
                )
            ) {
                continue;
            }
            if base
                .g
                .incoming(&old)
                .iter()
                .chain(fin.g.incoming(&old))
                .any(|e| e.from == block && e.kind != EdgeKind::Contains && !own_lineage(e))
            {
                continue;
            }
            let Some((g, src)) = judged_as(&block) else {
                continue;
            };
            count_plan_pair();
            let on = suspect(&block, &old);
            let Some(j) = cmp.judge(g, &block, src, &old, Lineage::Partial, on)? else {
                continue;
            };
            visit(PlanPair {
                suspect: on,
                prior_suspect: false,
                in_base: base.g.governed_by(&old).contains(&block),
                subject: block,
                cause: old,
                edge: SUPERSEDES.to_string(),
                follow: j.follow,
                reached_decision: j.reached_decision,
                retired: false,
                reach: true,
            })?;
        }
        Ok(())
    }

    /// The consequences of `change` in this state (the one the write published): for each block
    /// the change touched, each block outside the change that points at it. See
    /// `consequences_in` for the order and the cut.
    pub fn consequences(
        &self,
        scope: &Scope,
        change: &Hash,
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        self.consequences_of(scope, std::slice::from_ref(change), max_tokens)
    }

    /// The consequences of `changes` written together (an import writes one change per author):
    /// the blocks any of them touched are the touched blocks, the tensions that involve any of
    /// them are listed once each, in change order, and each entry's id takes the last change
    /// (the write's `change`) as its batch. For one change, this is [`ReadState::consequences`].
    pub fn consequences_of(
        &self,
        scope: &Scope,
        changes: &[Hash],
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let Some(change) = changes.last() else {
            return Ok(Consequences::default());
        };
        let chs = changes
            .iter()
            .map(|h| self.reader().change(h))
            .collect::<Result<Vec<_>, _>>()?;
        let (mut touched, mut changed) = (BTreeSet::new(), BTreeSet::new());
        let mut rewritten = BTreeSet::new();
        for ch in &chs {
            let (t, c) = changed_by_ops(&ch.ops);
            touched.extend(t);
            changed.extend(c);
            rewritten.extend(rewritten_by(&ch.ops));
        }
        // FW-LP6: the reaching links, by new decision, then target
        let mut reach: Vec<_> = chs.iter().flat_map(|ch| reached_targets(&ch.ops)).collect();
        reach.sort();
        let mine = |s: &SideRef| matches!(s, SideRef::Change(c) if changes.contains(c));
        // a tension set that cannot be built never fails the other entries
        let (tensions, tensions_error): (Vec<Consequence>, _) = match self.tensions() {
            Ok(set) => {
                let mut seen = BTreeSet::new();
                let tensions = changes
                    .iter()
                    .flat_map(|h| set.involving_change(h))
                    .filter(|t| seen.insert(t.id))
                    .map(|t| tension_entry(change, &t, &mine))
                    .collect();
                (tensions, None)
            }
            Err(e) => (vec![], Some(format!("{}: {}", e.code, e.message))),
        };
        // nothing can point at a block that did not exist, and a new block that cites nothing
        // outside the write cannot be suspect: no graph to read, unless the write also retires
        // a block it creates (a retained link to it is reported, O9)
        let only_creates = chs
            .iter()
            .flat_map(|ch| &ch.ops)
            .all(|o| matches!(o, Op::Create { .. }));
        if only_creates
            && changed == touched
            && reach.is_empty()
            && !chs.iter().any(|ch| creates_cite_outside(&ch.ops))
            && !chs.iter().any(|ch| creates_retirement(&ch.ops))
        {
            return Ok(Consequences {
                tensions_error,
                ..cut(tensions, max_tokens)
            });
        }
        let g = self.graph(scope)?;
        // a full supersede does not reach through the refines links it copied (Q5, Q6)
        reach.retain(|(new, _, via)| *via != REFINES || !refines_copied(&g, new));
        let m = self.materialized(scope)?;
        let view = match scope {
            Scope::View(v) => Some(v.as_str()),
            Scope::Canon | Scope::At(_) | Scope::Inflight | Scope::Proposal(_) => None,
        };
        let written = self.written_in(view);
        // O8 (D-EGXAS): in a view, the view's base, then the write's start; else the start only
        let mut baselines = vec![];
        if let Some(v) = view {
            baselines.push(self.view_base(v));
        }
        baselines.push(Baseline::start(chs.iter().flat_map(|ch| &ch.ops)));
        let ops: Vec<Op> = chs.iter().flat_map(|ch| ch.ops.iter().cloned()).collect();
        let cmp = Compare::new(
            self,
            &m,
            Names::scope(self, scope.clone()),
            baselines,
            Events::of_ops(&ops, &reach),
        );
        Ok(Consequences {
            tensions_error,
            ..self.consequences_in(
                &g,
                &cmp,
                change,
                &touched,
                &rewritten,
                &changed,
                &written,
                vec![],
                tensions,
                &reach,
                max_tokens,
            )?
        })
    }

    /// The baseline of view `v`'s base landing: canon's head read as canon, an earlier landing
    /// as of that landing, and no landing as the empty state (never current canon).
    fn view_base(&self, v: &str) -> Baseline<'_> {
        match self.effective_base(v) {
            None => Baseline::Empty,
            Some(h) if Some(h) == self.canon_head() => Baseline::scope(self, Scope::Canon),
            Some(h) => Baseline::scope(self, Scope::At(h)),
        }
    }

    /// Whether `view`'s own included changes wrote a block (always false for no view), read
    /// once, on the first call: most writes and view ops list nothing to mark.
    fn written_in<'a>(&'a self, view: Option<&'a str>) -> impl Fn(&BlockId) -> bool + 'a {
        let cell: OnceCell<Option<Arc<BTreeSet<BlockId>>>> = OnceCell::new();
        move |b: &BlockId| {
            cell.get_or_init(|| view.and_then(|v| self.view_touched(v).ok()))
                .as_ref()
                .is_some_and(|w| w.contains(b))
        }
    }

    /// The consequences of `view`'s op `batch` (this state is the one after it, `before` the
    /// one before it): the blocks whose version differs between the view before and after,
    /// among those the ops of `changes` write, or among every block when `changes` is `None`
    /// (a rebase, or a moved base); the changed decisions' `supersedes` / `revokes` targets;
    /// a `conflict` entry per conflict the view did not have before.
    pub fn view_consequences(
        &self,
        before: &ReadState,
        view: &str,
        batch: &Hash,
        changes: Option<&[Hash]>,
        delta: &TensionDelta,
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let scope = Scope::View(view.to_string());
        let m = self.materialized(&scope)?;
        let old = before.materialized(&scope)?;
        let differs = |id: &BlockId| old.version_of(id) != m.version_of(id);
        let touched: BTreeSet<BlockId> = match changes {
            Some(changes) => {
                let mut ids = BTreeSet::new();
                for h in changes {
                    let ch = self.reader().change(h)?;
                    ids.extend(ch.ops.iter().map(|o| o.block_id().clone()));
                }
                ids.into_iter().filter(|id| differs(id)).collect()
            }
            None if Arc::ptr_eq(&old, &m) => BTreeSet::new(),
            None => old
                .versions()
                .filter(|(id, v)| m.version_of(id) != Some(**v))
                .chain(m.versions().filter(|(id, _)| old.version_of(id).is_none()))
                .map(|(id, _)| id.clone())
                .collect(),
        };
        // a changed decision's targets changed status, before or after
        let mut changed = touched.clone();
        for id in &touched {
            for b in [old.get(id), m.get(id)].into_iter().flatten() {
                if b.block_type != DECISION {
                    continue;
                }
                for name in [SUPERSEDES, REVOKES] {
                    for l in b.links.get(name).into_iter().flatten() {
                        changed.insert(l.target.clone());
                    }
                }
            }
        }
        let had: BTreeSet<Hash> = before.conflicts(&scope)?.iter().map(|c| c.id).collect();
        let conflicts: Vec<Consequence> = self
            .conflicts(&scope)?
            .iter()
            .filter(|c| !had.contains(&c.id))
            .map(|c| Consequence {
                id: consequence_id(batch, ConsequenceKind::Conflict.as_str(), &c.block),
                block: c.block.to_string(),
                kind: ConsequenceKind::Conflict,
                because: Because::bare("conflict", c.block.to_string()),
                follow_up: FollowUp::Resolve,
                matched: vec![],
                matched_omitted: 0,
                conflict: Some(c.id.to_string()),
                tension: None,
                fix: Some(CONFLICT_FIX.to_string()),
                already_written: false,
            })
            .collect();
        // the tensions this state has and the one before had not, with a change side among the
        // op's changes (all the view's, for a full diff)
        let ours: BTreeSet<Hash> = match changes {
            Some(changes) => changes.iter().copied().collect(),
            None => self.included(view).iter().copied().collect(),
        };
        let mine = |s: &SideRef| matches!(s, SideRef::Change(c) if ours.contains(c));
        let tensions: Vec<Consequence> = delta
            .opened
            .iter()
            .filter(|t| t.sides.iter().any(mine))
            .map(|t| tension_entry(batch, t, &mine))
            .collect();
        if changed.is_empty() {
            let mut lead = conflicts;
            lead.sort_by(|a, b| a.block.cmp(&b.block));
            lead.extend(tensions);
            return Ok(cut(lead, max_tokens));
        }
        let g = self.graph(&scope)?;
        let written = self.written_in(Some(view));
        // the op's actual transition: the view before it is the one baseline; the lineage links
        // the touched decisions have after it are the events (one found only before compares
        // as an ordinary transition)
        let mut events = Events::default();
        for id in &touched {
            if let Some(b) = m.get(id) {
                events.add_decision(b);
                for l in b.links.get(REFINES).into_iter().flatten() {
                    events
                        .partial
                        .entry(l.target.clone())
                        .or_default()
                        .insert(b.id.clone());
                }
            }
        }
        let cmp = Compare::new(
            self,
            &m,
            Names::scope(self, scope.clone()),
            vec![Baseline::scope(before, scope.clone())],
            events,
        );
        self.consequences_in(
            &g,
            &cmp,
            batch,
            &touched,
            &BTreeSet::new(),
            &changed,
            &written,
            conflicts,
            tensions,
            &[],
            max_tokens,
        )
    }

    /// The consequences of landing proposal version `version` on canon (Q2b): what would turn
    /// suspect or dependent in canon. When one of the version's changes has landed, canon as it is
    /// (the landing is in it); otherwise the version's changes merged onto canon, applied to
    /// canon's materialization, with canon's graph carried across the applied ops. The batch is
    /// the version hash, so the propose and the approval give each entry the same id. A version
    /// that does not merge onto canon is an error (the caller reports it): on a conflict,
    /// `E_CONFLICT_UNRESOLVED` with the propose's text (ledger O4).
    pub fn landing_consequences(
        &self,
        version: &Hash,
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let p = self.reader().proposal(version)?;
        self.landing_consequences_of(version, &p, max_tokens)
    }

    /// [`ReadState::landing_consequences`] of version `version` whose record is `p`, which the
    /// store need not hold yet: a propose reads them before it writes the version (D-ME2HD).
    pub(crate) fn landing_consequences_of(
        &self,
        version: &Hash,
        p: &Proposal,
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let changes = changes_of(self, &p.changes)?;
        let ops: Vec<Op> = changes.iter().flat_map(|c| c.ops.iter().cloned()).collect();
        let (touched, changed) = changed_by_ops(&ops);
        let only_creates = ops.iter().all(|o| matches!(o, Op::Create { .. }));
        if only_creates
            && changed == touched
            && !creates_cite_outside(&ops)
            && !creates_retirement(&ops)
        {
            return Ok(Consequences::default());
        }
        // O8: a landed version compares its landing's prev with the state it recorded, and reads
        // the graph, the dependents and their names as of that landing (a later landing changes
        // none of them); an unlanded one compares canon with canon and its changes
        let (m, g, names, before) = if let Some(landing) = landed_as(self, p)? {
            let at = Scope::At(landing);
            let prev = self.reader().landing(&landing)?.prev;
            let before = match prev {
                Some(prev) => Baseline::scope(self, Scope::At(prev)),
                None => Baseline::Empty,
            };
            (
                self.materialized(&at)?,
                self.graph(&at)?,
                Names::scope(self, at),
                before,
            )
        } else {
            // build canon's graph and lexicon first, so that `after` carries them instead of
            // building others
            self.graph(&Scope::Canon)?;
            self.lexicon(&Scope::Canon)?;
            let reader = self.reader();
            // a conflict the version carries: it has no consequences until it is resolved
            let (_, applied) =
                match reader.merge_checked(&reader.state_of(self.canon_head())?, &changes) {
                    Ok(merged) => merged,
                    Err(StoreError::StaleBase { id, .. }) => {
                        return Err(RdyError::new(
                            E_CONFLICT_UNRESOLVED,
                            conflict_message(&[id.to_string()]),
                            CONFLICT_FIX,
                        ));
                    }
                    Err(e) => return Err(e.into()),
                };
            let next = self
                .built(&Scope::Canon)?
                .after(&applied_ops(&changes, &applied), self.pack());
            let g = Arc::clone(next.graph.get_or_init(|| {
                Arc::new(Graph::build_with(
                    next.m.iter(),
                    Arc::clone(reader.tracking()),
                ))
            }));
            let names = Arc::clone(
                next.lexicon
                    .get_or_init(|| Arc::new(Lexicon::build(self.pack(), next.m.iter()))),
            );
            (
                Arc::clone(&next.m),
                g,
                Names::Ready(names),
                Baseline::scope(self, Scope::Canon),
            )
        };
        let mut reach = reached_targets(&ops);
        reach.retain(|(new, _, via)| *via != REFINES || !refines_copied(&g, new));
        let cmp = Compare::new(self, &m, names, vec![before], Events::of_ops(&ops, &reach));
        self.consequences_in(
            &g,
            &cmp,
            version,
            &touched,
            &rewritten_by(&ops),
            &changed,
            &|_| false,
            vec![],
            vec![],
            &[],
            max_tokens,
        )
    }

    /// The entries for `changed` in graph `g` over `cmp`'s state: for each changed block, each
    /// block outside `touched` that points at it, the kept suspects of `rewritten` (see
    /// `kept_suspects`) and their retained links to a target a pending decision replaces (see
    /// `pending_replacements`). `conflicts` (by block id) come first, then `tensions` (as
    /// given), then suspect entries, then dependent ones, each by block id, then the pending
    /// entries (by block, then target), then the FW-LP6 entries of `reach` (a `dependent` entry
    /// for each block a reached target governs, in the order of `reach`, never a block
    /// `touched` holds, the new decision itself, or a block the list already holds; none for a
    /// target that is rejected, superseded or revoked; follow-up `confirm` for a block that is
    /// not a decision, and for a decision what `follow_up` gives it with D-ATBP6's rule
    /// ([`reached_follow_up`]): none for one that asserts nothing live); `written` marks
    /// `already_written`. No entry for a decision whose only edges to the changed block are its
    /// own `affects` links (D-4XTRC, [`visit_pairs`]). Each entry's follow-up, fix and
    /// evidence are `Compare::judge`'s, worked out before the cut. The list is cut from the end
    /// to `max_tokens` (default `BRIEF_DEFAULT_TOKENS`), counting what it drops in `omitted`.
    #[allow(clippy::too_many_arguments)]
    fn consequences_in(
        &self,
        g: &Graph,
        cmp: &Compare<'_>,
        batch: &Hash,
        touched: &BTreeSet<BlockId>,
        rewritten: &BTreeSet<BlockId>,
        changed: &BTreeSet<BlockId>,
        written: &dyn Fn(&BlockId) -> bool,
        mut conflicts: Vec<Consequence>,
        tensions: Vec<Consequence>,
        reach: &[(BlockId, BlockId, &'static str)],
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let m = cmp.m;
        let mut suspects_of: BTreeMap<BlockId, Vec<Suspect>> = BTreeMap::new();
        let mut causes: BTreeMap<BlockId, Cause> = BTreeMap::new();
        // the report keeps one cause per block outside the change: the first, or the first it
        // is suspect on
        visit_pairs(
            changed,
            |x| {
                g.incoming(x)
                    .iter()
                    .filter(|e| !touched.contains(&e.from))
                    .collect()
            },
            |from| m.get(from).is_some(),
            |from| m.get(from).is_some_and(|b| b.block_type == DECISION),
            |from, x| {
                suspects_of
                    .entry(from.clone())
                    .or_insert_with(|| {
                        let mut s = g.suspects_of(from);
                        s.retain(|s| !self.is_presentational_source(m, s));
                        s
                    })
                    .iter()
                    .any(|s| s.target == *x)
            },
            |p| {
                let better = causes.get(&p.from).is_none_or(|c| p.suspect && !c.suspect);
                if better {
                    let edge = p.edge.kind.name();
                    causes.insert(
                        p.from,
                        Cause {
                            suspect: p.suspect,
                            edges: edge_set(&edge, p.edges.iter().copied()),
                            edge,
                            changed: p.changed,
                        },
                    );
                }
                Ok(())
            },
        )?;

        let mut suspect_items = vec![];
        let mut dependent_items = vec![];
        for (block, cause) in causes {
            let Some(src) = m.get(&block) else { continue };
            // no entry for a decision that asserts nothing live
            let lineage = cmp.lineage_of(&cause.changed);
            let Some(j) = cmp.judge(g, &block, src, &cause.changed, lineage, cause.suspect)? else {
                continue;
            };
            let kind = if cause.suspect {
                ConsequenceKind::Suspect
            } else {
                ConsequenceKind::Dependent
            };
            let item = Consequence {
                id: consequence_id(batch, kind.as_str(), &block),
                block: block.to_string(),
                kind,
                because: Because {
                    edge: cause.edge,
                    changed: cause.changed.to_string(),
                    edges: Some(cause.edges),
                    values: j.values,
                    values_omitted: j.values_omitted,
                },
                follow_up: j.follow,
                matched: j.matched,
                matched_omitted: j.matched_omitted,
                conflict: None,
                tension: None,
                fix: j.fix,
                already_written: written(&block),
            };
            match kind {
                ConsequenceKind::Suspect => suspect_items.push(item),
                _ => dependent_items.push(item),
            }
        }
        // D-W7KAK: with the other suspect entries, by block id (a kept entry never names a block
        // an outside entry names: those are outside `touched`)
        let kept = self.kept_suspects(g, m, batch, rewritten);
        let pending = self.pending_replacements(g, cmp, batch, rewritten, &kept)?;
        suspect_items.extend(kept);
        suspect_items.sort_by(|a, b| a.block.cmp(&b.block));
        conflicts.sort_by(|a, b| a.block.cmp(&b.block));
        conflicts.extend(tensions);
        conflicts.extend(suspect_items);
        conflicts.extend(dependent_items);
        conflicts.extend(pending);
        // FW-LP6: what a refined or part-superseded target governs, once per block, never a
        // block this change touched or one the list already holds
        let mut listed: BTreeSet<String> = conflicts.iter().map(|c| c.block.clone()).collect();
        for (decision, target, via) in reach {
            if matches!(
                g.status(target),
                Some(
                    DerivedStatus::Rejected
                        | DerivedStatus::Superseded { .. }
                        | DerivedStatus::Revoked { .. }
                )
            ) {
                continue;
            }
            // by id, whatever the iteration order of the graph
            let mut governed = g.governed_by(target);
            governed.sort();
            for block in governed {
                if touched.contains(&block) || block == *decision {
                    continue;
                }
                if listed.contains(&block.to_string()) {
                    continue;
                }
                // the walk's rule for a governed decision: none for one that asserts nothing
                // live, update for a proposed one, and for an accepted one confirm with
                // LINEAGE_DECISION_FIX (D-7M06Q, D-ATBP6), or reaffirm only when it is suspect on
                // the target; a block that is not a decision gets confirm, with what it states of
                // the change as evidence
                let Some(src) = m.get(&block) else { continue };
                let on = suspect_on(self, g, m, &block, target);
                let Some(j) = cmp.judge(g, &block, src, target, Lineage::Partial, on)? else {
                    continue;
                };
                listed.insert(block.to_string());
                conflicts.push(Consequence {
                    id: consequence_id(batch, ConsequenceKind::Dependent.as_str(), &block),
                    block: block.to_string(),
                    kind: ConsequenceKind::Dependent,
                    because: Because {
                        edge: (*via).to_string(),
                        changed: target.to_string(),
                        edges: None,
                        values: j.values,
                        values_omitted: j.values_omitted,
                    },
                    follow_up: j.follow,
                    matched: j.matched,
                    matched_omitted: j.matched_omitted,
                    conflict: None,
                    tension: None,
                    fix: j.fix,
                    already_written: written(&block),
                });
            }
        }
        Ok(cut(conflicts, max_tokens))
    }

    /// D-W7KAK: a `suspect` entry for each block of `rewritten` that is suspect in `g`, once per
    /// target, marked `already_written`: the write kept its pin, so the agent answers it before it
    /// proposes. `because.edge` is the edge to the target that makes the block suspect (a premise
    /// edge first, the least by name; else the least link by name), `because.edges` it and the
    /// impact links to the same target; the id covers the target too. Follow-up and fix by
    /// [`kept_follow_up`]. None for a block that is gone, a decision that asserts nothing live,
    /// or a presentational source. By block, then target.
    fn kept_suspects(
        &self,
        g: &Graph,
        m: &Materialized,
        batch: &Hash,
        rewritten: &BTreeSet<BlockId>,
    ) -> Vec<Consequence> {
        let mut out = vec![];
        for block in rewritten {
            if m.get(block).is_none() {
                continue;
            }
            let live_decision = match g.status(block) {
                Some(
                    DerivedStatus::Rejected
                    | DerivedStatus::Superseded { .. }
                    | DerivedStatus::Revoked { .. },
                ) => continue,
                Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. }) => true,
                Some(DerivedStatus::Proposed) | None => false,
            };
            for s in g.suspects_of(block) {
                if self.is_presentational_source(m, &s) {
                    continue;
                }
                let Some((follow, fix)) = kept_follow_up(&s.reason, live_decision) else {
                    continue;
                };
                // the edge that makes it suspect, by the graph's own rule (`edge_suspect`), so
                // this reply and rdy_context never disagree: a premise edge first, then a link
                // that is not one (a `refines` or an `affects` to a superseded decision)
                let edge = g
                    .outgoing(block)
                    .iter()
                    .filter(|e| {
                        e.to == s.target && g.edge_suspect(e).is_some_and(|x| x.reason == s.reason)
                    })
                    .map(|e| (!premise(e), e.kind.name()))
                    .min();
                let Some((_, edge)) = edge else { continue };
                let edges = edge_set(&edge, g.outgoing(block).iter().filter(|e| e.to == s.target));
                out.push(Consequence {
                    // one id per (block, target), as a tension entry names its tension
                    id: consequence_id(batch, &format!("suspect {}", s.target), block),
                    block: block.to_string(),
                    kind: ConsequenceKind::Suspect,
                    because: Because {
                        edge,
                        changed: s.target.to_string(),
                        edges: Some(edges),
                        values: None,
                        values_omitted: 0,
                    },
                    follow_up: follow,
                    matched: vec![],
                    matched_omitted: 0,
                    conflict: None,
                    tension: None,
                    fix: Some(fix),
                    already_written: true,
                });
            }
        }
        out
    }

    /// O9: the links a rewritten block keeps to a target that a pending decision replaces. For
    /// each block of `rewritten` and each target of its remaining impact links, the target's
    /// incoming full `supersedes` and `revokes` links in this scope; a proposed, live or partly
    /// superseded replacer (never the block itself) makes the link reportable before the target
    /// turns suspect. One `dependent` entry per (block, target), marked `already_written`, with
    /// the id `consequence_id(batch, "dependent <target>", block)`; none where `kept` names the
    /// same block and target. Representative edge, edge set and follow-up as a normal entry's,
    /// the literals compared with every active replacement (a revoke removes them all). By
    /// block, then target.
    fn pending_replacements(
        &self,
        g: &Graph,
        cmp: &Compare<'_>,
        batch: &Hash,
        rewritten: &BTreeSet<BlockId>,
        kept: &[Consequence],
    ) -> Result<Vec<Consequence>, RdyError> {
        let kept: BTreeSet<(&str, &str)> = kept
            .iter()
            .map(|c| (c.block.as_str(), c.because.changed.as_str()))
            .collect();
        let mut out = vec![];
        for block in rewritten {
            let Some(src) = cmp.m.get(block) else {
                continue;
            };
            let mut by_target: BTreeMap<&BlockId, Vec<&Edge>> = BTreeMap::new();
            for e in g.outgoing(block) {
                if impact(e) {
                    by_target.entry(&e.to).or_default().push(e);
                }
            }
            for (target, mut edges) in by_target {
                let (b, t) = (block.to_string(), target.to_string());
                if kept.contains(&(b.as_str(), t.as_str())) {
                    continue;
                }
                let mut retired = Retired::default();
                for e in g.incoming(target) {
                    let (supersedes, revokes) = match &e.kind {
                        EdgeKind::Link(n) => (n == SUPERSEDES && e.part.is_none(), n == REVOKES),
                        _ => (false, false),
                    };
                    let active = matches!(
                        g.status(&e.from),
                        Some(
                            DerivedStatus::Proposed
                                | DerivedStatus::Live
                                | DerivedStatus::PartiallySuperseded { .. }
                        )
                    );
                    if !(supersedes || revokes) || !active || e.from == *block {
                        continue;
                    }
                    if revokes {
                        retired.revoke = true;
                    } else {
                        retired.by.insert(e.from.clone());
                    }
                }
                if !retired.revoke && retired.by.is_empty() {
                    continue;
                }
                edges.sort_by_key(|e| e.kind.name());
                let rep = edges
                    .iter()
                    .find(|e| premise(e))
                    .or_else(|| edges.first())
                    .expect("a target has an edge")
                    .kind
                    .name();
                let on = suspect_on(self, g, cmp.m, block, target);
                let Some(j) = cmp.judge(g, block, src, target, Lineage::Retired(&retired), on)?
                else {
                    continue;
                };
                out.push(Consequence {
                    id: consequence_id(batch, &format!("dependent {target}"), block),
                    block: b,
                    kind: ConsequenceKind::Dependent,
                    because: Because {
                        edges: Some(edge_set(&rep, edges.iter().copied())),
                        edge: rep,
                        changed: t,
                        values: j.values,
                        values_omitted: j.values_omitted,
                    },
                    follow_up: j.follow,
                    matched: j.matched,
                    matched_omitted: j.matched_omitted,
                    conflict: None,
                    tension: None,
                    fix: j.fix,
                    already_written: true,
                });
            }
        }
        Ok(out)
    }
}

/// `items` cut from the end to `max_tokens` (default `BRIEF_DEFAULT_TOKENS`; four bytes a
/// token), counting what it drops in `omitted`, by kind.
fn cut(items: Vec<Consequence>, max_tokens: Option<usize>) -> Consequences {
    let (items, omitted) = cut_rows(items, |c| c.kind.as_str(), max_tokens);
    Consequences {
        items,
        omitted,
        ..Consequences::default()
    }
}

/// The report cutter over any row shape: `items` cut from the end until `{"items", "omitted"}`
/// fits `max_tokens` (see [`cut`]), with the dropped rows counted by `kind`.
fn cut_rows<R: Serialize>(
    mut items: Vec<R>,
    kind: fn(&R) -> &'static str,
    max_tokens: Option<usize>,
) -> (Vec<R>, BTreeMap<String, usize>) {
    #[derive(Serialize)]
    struct Sized<'a, R> {
        items: &'a [R],
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        omitted: &'a BTreeMap<String, usize>,
    }
    let mut omitted = BTreeMap::new();
    let budget = max_tokens.unwrap_or(BRIEF_DEFAULT_TOKENS).saturating_mul(4);
    let size = |items: &[R], omitted: &BTreeMap<String, usize>| {
        serde_json::to_string(&Sized { items, omitted })
            .expect("consequences serialize: strings and string-keyed maps only")
            .len()
    };
    while !items.is_empty() && size(&items, &omitted) > budget {
        let drop = items.len().div_ceil(2);
        for dropped in items.split_off(items.len() - drop) {
            *omitted.entry(kind(&dropped).to_string()).or_default() += 1;
        }
    }
    (items, omitted)
}

/// A reply's rows, the counts a cut dropped by kind, the tension error, and the code error.
type Rows<R> = (
    Vec<R>,
    BTreeMap<String, usize>,
    Option<String>,
    Option<String>,
);

/// The rows of a write's reply: its structural consequences (`c`, read uncut) in their order,
/// then a [`CodeKept`] row for each block the write rewrote whose code differs from the recorded
/// snapshot, by block id; the combined list goes through the report cutter. When the code rows
/// cannot be read, the structural rows stand alone and the code error is reported beside them.
fn with_code_rows(
    st: &ReadState,
    scope: &Scope,
    changes: &[Hash],
    c: Consequences,
    max_tokens: Option<usize>,
) -> Rows<ReplyRow> {
    let mut rows: Vec<ReplyRow> = c.items.into_iter().map(ReplyRow::Consequence).collect();
    let code_error = match code_kept_rows(st, scope, changes) {
        Ok(code) => {
            rows.extend(code.into_iter().map(ReplyRow::CodeKept));
            None
        }
        Err(e) => Some(format!("{}: {}", e.code, e.message)),
    };
    let (rows, omitted) = cut_rows(rows, ReplyRow::kind, max_tokens);
    (rows, omitted, c.tensions_error, code_error)
}

/// The [`CodeKept`] rows of the write `changes` made (its last change names the batch): one for
/// each block it rewrote whose code differs from the recorded snapshot, by block id.
fn code_kept_rows(
    st: &ReadState,
    scope: &Scope,
    changes: &[Hash],
) -> Result<Vec<CodeKept>, RdyError> {
    let Some(batch) = changes.last() else {
        return Ok(vec![]);
    };
    let mut rewritten = BTreeSet::new();
    for h in changes {
        rewritten.extend(rewritten_by(&st.reader().change(h)?.ops));
    }
    let ids: Vec<BlockId> = rewritten.into_iter().collect();
    Ok(crate::reflects::code_suspects(st, scope, Some(&ids))?
        .into_iter()
        .map(|s| CodeKept {
            id: consequence_id(batch, "suspect code_changed", &s.block),
            block: s.block.to_string(),
            kind: ConsequenceKind::Suspect,
            because: CodeBecause {
                edge: rdy_schema::REFLECTS_FIELD,
                changed: None,
            },
            follow_up: FollowUp::Confirm,
            reason: "code_changed",
            target: None,
            paths: s.paths,
            already_written: true,
            fix: CODE_KEPT_FIX,
        })
        .collect())
}

/// The entry for tension `t`: its `block` is the first involved block of an item whose side is
/// not `mine` (else the first involved block), `changed` the first involved block of an item
/// that is. The id covers the batch, the tension and the block.
fn tension_entry(batch: &Hash, t: &Tension, mine: &dyn Fn(&SideRef) -> bool) -> Consequence {
    let first = |own: bool| {
        t.involved
            .iter()
            .filter(|i| mine(&i.side) == own)
            .map(|i| &i.block)
            .min()
    };
    let block = first(false)
        .or_else(|| t.involved.iter().map(|i| &i.block).min())
        .expect("a tension involves a block")
        .clone();
    let changed = first(true).unwrap_or(&block).clone();
    Consequence {
        id: consequence_id(batch, &format!("tension {}", t.id), &block),
        block: block.to_string(),
        kind: ConsequenceKind::Tension,
        because: Because::bare(t.kind.name(), changed.to_string()),
        follow_up: FollowUp::Resolve,
        matched: vec![],
        matched_omitted: 0,
        conflict: None,
        tension: Some(tension_label(&t.id)),
        fix: Some(TENSION_FIX.to_string()),
        already_written: false,
    }
}

/// Writes `c` into a write's or a view op's reply: `consequences` always, with
/// `consequences_omitted` when a cut dropped some, or `consequences: []` and
/// `consequences_error` when they could not be read (the write stands).
pub(crate) fn attach(v: &mut serde_json::Value, c: Result<Consequences, RdyError>) {
    attach_rows(v, c.map(|c| (c.items, c.omitted, c.tensions_error, None)));
}

/// [`attach`] over any row shape. A write's reply also carries `consequences_code_error` when
/// its code-kept rows could not be read (its structural rows stand).
fn attach_rows<R: Serialize>(v: &mut serde_json::Value, c: Result<Rows<R>, RdyError>) {
    match c {
        Ok((items, omitted, tensions_error, code_error)) => {
            v["consequences"] = serde_json::to_value(&items)
                .expect("consequences serialize: strings and string-keyed maps only");
            if !omitted.is_empty() {
                v["consequences_omitted"] =
                    serde_json::to_value(&omitted).expect("a map of counts serializes");
            }
            if let Some(e) = tensions_error {
                v["consequences_tension_error"] = e.into();
            }
            // D-01QX5: the checkout could not be read; the structural rows still stand
            if let Some(e) = code_error {
                v["consequences_code_error"] = e.into();
            }
        }
        Err(e) => {
            v["consequences"] = serde_json::json!([]);
            v["consequences_error"] = format!("{}: {}", e.code, e.message).into();
        }
    }
}

/// Writes `o` into a write's or a view op's reply as `on_change`, or an empty one and
/// `on_change_error` when it could not be read (the write stands).
fn attach_on_change(
    v: &mut serde_json::Value,
    o: Result<crate::tensions::delta::OnChange, RdyError>,
) {
    match o {
        Ok(o) => {
            v["on_change"] = serde_json::to_value(&o)
                .expect("on_change serializes: strings and string-keyed maps only");
        }
        Err(e) => {
            v["on_change"] = serde_json::json!({"tensions": [], "resolved": []});
            v["on_change_error"] = format!("{}: {}", e.code, e.message).into();
        }
    }
}

/// D-JQD5X, the change stage: writes the pack rule rows of a write or view op into its reply's
/// `on_change`, after the tension reply is built, as `checks` (always, `[]` included) and, when
/// a cut left rows out, `checks_omitted`. A report that cannot be read gives `checks: []` and
/// `checks_error`; the write stands, and a tension read error does not hide the rows. No row
/// refuses anything.
fn attach_checks(v: &mut serde_json::Value, r: Result<crate::check::RuleScan, RdyError>) {
    let Some(on) = v.get_mut("on_change").filter(|o| o.is_object()) else {
        return;
    };
    match r {
        Ok(scan) => {
            on["checks"] =
                serde_json::to_value(scan.rows()).expect("rule rows serialize: strings only");
            if scan.omitted() > 0 {
                on["checks_omitted"] = scan.omitted().into();
            }
        }
        Err(e) => {
            on["checks"] = serde_json::json!([]);
            on["checks_error"] = format!("{}: {}", e.code, e.message).into();
        }
    }
}

/// The change stage's rule checks of `changes` (their written blocks, read in `after`) in
/// `view`, from `before` to `after` ([`crate::check::rules::change_checks`]).
fn change_rule_checks(
    before: &ReadState,
    after: &ReadState,
    view: &str,
    changes: &[Hash],
    full: bool,
) -> Result<crate::check::RuleScan, RdyError> {
    // a pack with no rules reads nothing
    if after.pack().rules.is_empty() {
        return crate::check::rules::change_checks(before, after, view, &BTreeSet::new(), false);
    }
    let mut written = BTreeSet::new();
    if !full {
        for h in changes {
            written.extend(
                after
                    .reader()
                    .change(h)?
                    .ops
                    .iter()
                    .map(|o| o.block_id().clone()),
            );
        }
    }
    crate::check::rules::change_checks(before, after, view, &written, full)
}

/// `write_typed`, then the write's consequences read from the state it published: on success
/// `consequences` is always present (with `consequences_omitted` when a cut dropped some, or
/// `consequences_error` when they could not be read; the write stands), then `intents` when other
/// sessions' open intents cover blocks the change wrote (iwr1), then `on_change` (the tensions
/// the write opened or resolved in the view; `on_change_error` when they could not be read),
/// then `coverage` (the change in the view's coverage and its pages, FW-CH3; `coverage_error`
/// when it could not be read), and the write's suggestions (FW-EN3), which never fail the write; on a briefing refusal
/// the error carries a fresh briefing for exactly the blocks the write touches. A write that
/// lists its `changes` (an import: one change per author) is reported over all of them.
pub fn write_and_report<T: Serialize + 'static>(
    engine: &dyn Engine,
    view: &str,
    max_tokens: Option<usize>,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    report(engine, None, view, max_tokens, f)
}

/// [`write_and_report`], with every change of the write read as `reader`'s session: an import's
/// reply is the importer's, whoever wrote the blocks (final review 2, finding 11). An intent of
/// `reader`'s own session is not listed; an author's intent is.
pub fn write_and_report_as<T: Serialize + 'static>(
    engine: &dyn Engine,
    reader: &crate::Session,
    view: &str,
    max_tokens: Option<usize>,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    report(engine, Some(reader), view, max_tokens, f)
}

fn report<T: Serialize + 'static>(
    engine: &dyn Engine,
    reader: Option<&crate::Session>,
    view: &str,
    max_tokens: Option<usize>,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    // the states before and after the write, captured on the writer: a deleted or moved block's
    // old areas, and what the write did to the tensions
    type States = (Arc<ReadState>, Arc<ReadState>);
    let slot: Arc<Mutex<Option<States>>> = Arc::default();
    let inner = Arc::clone(&slot);
    let f = move |r: &mut Rdy| {
        let before = r.read();
        let t = f(r)?;
        *inner.lock().unwrap_or_else(|p| p.into_inner()) = Some((before, r.read()));
        Ok(t)
    };
    let mut v = match write_typed(engine, f) {
        Ok(v) => v,
        Err(e) => {
            return Err(expand_error(
                e,
                &engine.read(),
                view,
                &engine.briefs(),
                max_tokens,
            ));
        }
    };
    let states = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
    let before = states.as_ref().map(|s| &*s.0);
    // FW-PC1: a nudge, never a refusal; absent unless the count is reached under cadence `changes`
    if v.is_object() && propose_due(&engine.read(), view) {
        v["propose_due"] = true.into();
    }
    // the write's changes: an import lists one per author in `changes` (oldest first, the last
    // is `change`); every other write has the one `change`
    let change = v
        .get("change")
        .and_then(|c| c.as_str())
        .and_then(|c| c.parse::<Hash>().ok());
    let changes: Vec<Hash> = match v.get("changes").and_then(|c| c.as_array()) {
        Some(list) if !list.is_empty() => list
            .iter()
            .filter_map(|c| c.as_str()?.parse::<Hash>().ok())
            .collect(),
        _ => change.into_iter().collect(),
    };
    let scope = Scope::View(view.into());
    if changes.is_empty() {
        v["consequences"] = serde_json::json!([]);
        v["suggestions"] = serde_json::json!([]);
        return Ok(v);
    }
    let st = engine.read();
    // the state the write published, not a later one: a write that lands before this read
    // changes nothing in this reply (O8); the view base is that state's too
    let published = states.as_ref().map_or(&st, |s| &s.1);
    // read uncut, then cut once with the code-kept rows appended (D-01QX5)
    let c = published
        .consequences_of(&scope, &changes, Some(usize::MAX))
        .map(|c| with_code_rows(published, &scope, &changes, c, max_tokens));
    attach_rows(&mut v, c);
    if let Some(notes) = intent_notes_of(&st, before, view, &changes, reader)
        && !notes.is_empty()
    {
        v["intents"] = serde_json::to_value(&notes).expect("intent notes serialize");
    }
    if let Some((b, a)) = &states {
        let r =
            tension_delta(b, a, Some(&changes)).and_then(|d| on_change(b, a, view, max_tokens, &d));
        attach_on_change(&mut v, r);
        attach_checks(&mut v, change_rule_checks(b, a, view, &changes, false));
        attach_coverage(&mut v, b, a, &scope, &changes);
    }
    match engine.read().suggestions_of(&scope, &changes) {
        Ok((s, n)) => {
            v["suggestions"] =
                serde_json::to_value(&s).expect("suggestions serialize: strings only");
            if n > 0 {
                v["suggestions_omitted"] = n.into();
            }
        }
        Err(e) => {
            v["suggestions"] = serde_json::json!([]);
            v["suggestions_error"] = format!("{}: {}", e.code, e.message).into();
        }
    }
    attach_related(&mut v, engine, view, &changes);
    Ok(v)
}

/// FW-PC1 (D-0GGW9): whether a proposal is due for `view`. Only under cadence `changes`: the
/// view's included changes that its open proposal does not carry (its latest version, read from
/// the view's own open ref; every change when it has none) number at least `propose.changes`. A
/// state that cannot be read says no: the nudge never fails a write. No other proposal is read
/// and no version is counted.
fn propose_due(st: &ReadState, view: &str) -> bool {
    if st.propose_cadence() != crate::ProposeCadence::Changes {
        return false;
    }
    let Ok(open) = crate::gate::open_proposal_of_view(st, view) else {
        return false;
    };
    let carried: BTreeSet<Hash> = open
        .map(|p| p.changes.into_iter().collect())
        .unwrap_or_default();
    let waiting = st
        .included(view)
        .iter()
        .filter(|c| !carried.contains(*c))
        .count();
    waiting >= st.propose_changes() as usize
}

/// cc16 (FW-CH3): `coverage: {before, after, pages}`, the change `changes` made to the coverage
/// of `scope`, from the two states the write was captured between; or `coverage_error` (the
/// error text) when it could not be read. The write stands either way. A `coverage` key the
/// write already carries is kept.
fn attach_coverage(
    v: &mut serde_json::Value,
    before: &ReadState,
    after: &ReadState,
    scope: &Scope,
    changes: &[Hash],
) {
    if v.get("coverage").is_some() {
        return;
    }
    let change = changes_of(after, changes).and_then(|cs| {
        let touched: BTreeSet<BlockId> = cs
            .iter()
            .flat_map(|c| &c.ops)
            .map(|o| o.block_id().clone())
            .collect();
        coverage_change(before, scope, after, scope, &touched)
    });
    match change {
        Ok(c) => {
            v["coverage"] = serde_json::to_value(&c)
                .expect("coverage change serializes: numbers and strings only");
        }
        Err(e) => v["coverage_error"] = coverage_error_text(&e).into(),
    }
}

/// FW-EN5: when the changes create decisions, `related_decisions` lists the decisions related
/// to them (with `related_decisions_omitted` when a cap dropped some), or `[]` and
/// `related_decisions_error` when they could not be read; the write stands. Changes that
/// create no decision add no key.
fn attach_related(v: &mut serde_json::Value, engine: &dyn Engine, view: &str, changes: &[Hash]) {
    let st = engine.read();
    let ids: Result<Vec<BlockId>, RdyError> = changes
        .iter()
        .map(|h| st.reader().change(h).map_err(RdyError::from))
        .collect::<Result<Vec<_>, _>>()
        .map(|chs| {
            chs.iter()
                .flat_map(|ch| &ch.ops)
                .filter_map(|o| match o {
                    Op::Create { block } if block.block_type == DECISION => Some(block.id.clone()),
                    _ => None,
                })
                .collect()
        });
    match ids {
        Ok(ids) if ids.is_empty() => {}
        Ok(ids) => match st.related_decisions(&Scope::View(view.into()), &ids) {
            Ok((rel, n)) => {
                v["related_decisions"] =
                    serde_json::to_value(&rel).expect("related decisions serialize: strings only");
                if n > 0 {
                    v["related_decisions_omitted"] = n.into();
                }
            }
            Err(e) => {
                v["related_decisions"] = serde_json::json!([]);
                v["related_decisions_error"] = format!("{}: {}", e.code, e.message).into();
            }
        },
        Err(e) => {
            v["related_decisions"] = serde_json::json!([]);
            v["related_decisions_error"] = format!("{}: {}", e.code, e.message).into();
        }
    }
}

/// iwr1: other sessions' open intents on the blocks `changes` wrote, in `view`, with each block's
/// areas taken after the write and, when given, `before` it; `None` when they cannot be read (the
/// write stands and the reply has no `intents`). Each change is read as `reader`'s session when
/// given (an import: the importer's), else as its author's: an intent of that session is not
/// listed for the change's blocks. An intent several changes hear of is listed once, with the
/// blocks of all of them; by id.
fn intent_notes_of(
    st: &ReadState,
    before: Option<&ReadState>,
    view: &str,
    changes: &[Hash],
    reader: Option<&crate::Session>,
) -> Option<Vec<IntentNote>> {
    if st.coord_heads().open_intents.is_empty() {
        return Some(vec![]);
    }
    let scope = Scope::View(view.into());
    let now = now_ms();
    let mut by_id: BTreeMap<String, IntentNote> = BTreeMap::new();
    for h in changes {
        let change = st.reader().change(h).ok()?;
        let touched: BTreeSet<BlockId> = change.ops.iter().map(|o| o.block_id().clone()).collect();
        let (actor, run) = match reader {
            Some(s) => (&s.actor, s.run.as_deref()),
            None => (&change.provenance.actor, change.provenance.run.as_deref()),
        };
        let notes = match before {
            Some(b) => st.intent_notes_since(b, &scope, actor, run, &touched, now),
            None => st.intent_notes(&scope, actor, run, &touched, now),
        }
        .ok()?;
        for n in notes {
            match by_id.entry(n.id.clone()) {
                Entry::Vacant(e) => {
                    e.insert(n);
                }
                Entry::Occupied(mut e) => {
                    let blocks = &mut e.get_mut().blocks;
                    blocks.extend(n.blocks);
                    blocks.sort();
                    blocks.dedup();
                }
            }
        }
    }
    Some(by_id.into_values().collect())
}

/// What a view op did, for its consequences: the op (`batch`), the changes it put in or took
/// out of the view, and whether it moved the view so that only a full diff tells what changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewOpEffect {
    pub batch: Hash,
    pub changes: Vec<Hash>,
    pub full: bool,
}

fn hashes<'a>(lists: impl IntoIterator<Item = &'a Vec<String>>) -> Option<Vec<Hash>> {
    lists
        .into_iter()
        .flatten()
        .map(|c| c.parse().ok())
        .collect()
}

/// An include's or an exclude's effect: the batch is the view's new head op.
pub(crate) fn op_effect(r: &ViewOpResult) -> Option<ViewOpEffect> {
    Some(ViewOpEffect {
        batch: r.head.parse().ok()?,
        changes: hashes([&r.added, &r.added_deps, &r.removed, &r.cascaded])?,
        full: false,
    })
}

/// A replay's effect: the batch is the view's new head op (the one `Include`); the changes are
/// every new change the replay stored.
pub(crate) fn replay_effect(r: &ReplayResult) -> Option<ViewOpEffect> {
    Some(ViewOpEffect {
        batch: r.head.parse().ok()?,
        changes: hashes([&r.added, &r.added_deps, &r.removed, &r.cascaded])?,
        full: false,
    })
}

/// An undo's effect: the batch is the `Undo` op; a rebase needs the full diff.
pub(crate) fn undo_effect(u: &UndoResult) -> Option<ViewOpEffect> {
    Some(ViewOpEffect {
        batch: u.op.parse().ok()?,
        changes: hashes([&u.included, &u.excluded, &u.cascaded])?,
        full: u.rebased,
    })
}

/// The states before and after a view op, and its effect, captured on the writer.
type Captured = (Arc<ReadState>, Option<ViewOpEffect>, Arc<ReadState>);

/// A view op (`f`) on the writer, then its consequences in `view`: the states before and after
/// are captured inside the write, so no other write interleaves. When `effect` gives one, the
/// reply gains `consequences` (see `attach`); a moved effective base reads the full diff.
/// Errors come back unchanged (view ops take no briefing).
pub fn view_op_and_report<T: Serialize + 'static>(
    engine: &dyn Engine,
    view: &str,
    max_tokens: Option<usize>,
    effect: fn(&T) -> Option<ViewOpEffect>,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    let slot: Arc<Mutex<Option<Captured>>> = Arc::default();
    let inner = Arc::clone(&slot);
    let mut v = write_typed(engine, move |r| {
        let before = r.read();
        let t = f(r)?;
        let after = r.read();
        let e = effect(&t);
        *inner.lock().unwrap_or_else(|p| p.into_inner()) = Some((before, e, after));
        Ok(t)
    })?;
    let captured = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
    let Some((before, Some(e), after)) = captured else {
        return Ok(v);
    };
    let full = e.full || before.effective_base(view) != after.effective_base(view);
    let changes = (!full).then_some(&e.changes[..]);
    // one delta for both: the consequences' tension entries and `on_change` (a view op's state
    // builds its tension set again, so `tension_delta` diffs the two sets whole)
    match tension_delta(&before, &after, changes) {
        Ok(d) => {
            let c = after.view_consequences(&before, view, &e.batch, changes, &d, max_tokens);
            attach(&mut v, c);
            attach_on_change(&mut v, on_change(&before, &after, view, max_tokens, &d));
        }
        Err(err) => {
            attach(&mut v, Err(err.clone()));
            attach_on_change(&mut v, Err(err));
        }
    }
    attach_checks(
        &mut v,
        change_rule_checks(&before, &after, view, &e.changes, full),
    );
    Ok(v)
}

/// `write_typed` for a proposal (`f` returns its `ProposalInfo`), then the consequences of
/// landing it on canon (see `ReadState::landing_consequences`): `consequences` is always present
/// in the reply, with `consequences_omitted` when a cut dropped some, or `consequences: []` and
/// `consequences_error` when the version has a blocking conflict (`E_CONFLICT_UNRESOLVED`, no
/// merge tried) or does not merge onto canon (the proposal stands). The reply's `plan` is the
/// summary the propose built with the plan it bound in the version's batch (D-4JMPH): it is not
/// read or computed again after the write.
pub fn propose_and_report(
    engine: &dyn Engine,
    max_tokens: Option<usize>,
    f: impl FnOnce(&mut Rdy) -> Result<ProposalInfo, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    let mut v = write_typed(engine, f)?;
    // ledger O4: a version with a blocking conflict has no consequences (FW-LP3 reads them from
    // its final state, which it does not have yet); no merge is tried
    let blocking: Vec<String> = v
        .get("blocking")
        .and_then(|b| b.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("block")?.as_str().map(str::to_string))
        .collect();
    if blocking.is_empty() {
        report_landing(engine, &mut v, "id", max_tokens);
    } else {
        v["consequences"] = serde_json::json!([]);
        v["consequences_error"] = conflict_error(&blocking).into();
    }
    attach_proposal_coverage(engine, &mut v);
    Ok(v)
}

/// cc16 (FW-CH3): the proposal's `coverage: {before, after, pages}`, canon's score against the
/// proposal candidate's (canon with exactly the version's changes; ruling final, FW-CV2
/// D-XZXCE) and the pages its changes touch, read from the stored version after the write, off
/// the writer (as a write's is). The batch card's Coverage fact is worked out by the same
/// function (`ReadState::proposal_coverage`), so a subset proposal's reply and card agree. A read error leaves `coverage` out and sets `coverage_error`
/// (an earlier text is kept, also when the read works: the unapplied-decision pass sets one
/// that does not stop coverage): the proposal stands. When the propose could not read the
/// tensions (`tensions_error`), coverage cannot be worked out either: the reply is left as is.
fn attach_proposal_coverage(engine: &dyn Engine, v: &mut serde_json::Value) {
    if v.get("tensions_error").is_some() {
        return;
    }
    let st = engine.read();
    let read = v
        .get("id")
        .and_then(|h| h.as_str())
        .and_then(|h| h.parse::<Hash>().ok())
        .ok_or_else(|| {
            RdyError::new(
                E_INTERNAL,
                "the propose reply has no proposal id",
                "report this",
            )
        })
        .and_then(|id| Ok(st.reader().proposal(&id)?))
        .and_then(|p| st.proposal_coverage(&p));
    match read {
        Ok(c) => {
            v["coverage"] = serde_json::to_value(&c)
                .expect("coverage change serializes: numbers and strings only");
        }
        Err(e) => {
            if v.get("coverage_error").is_none() {
                v["coverage_error"] = coverage_error_text(&e).into();
            }
        }
    }
}

/// `write_typed` for an approval (`f` returns its `LandInfo`), then the consequences of the
/// version it landed, read from canon (with the default budget). The ids match those the propose
/// reported.
pub fn land_and_report(
    engine: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<LandInfo, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    let mut v = write_typed(engine, f)?;
    report_landing(engine, &mut v, "version", None);
    Ok(v)
}

/// Attaches the consequences of the version an approval's reply landed (its `version`), as
/// `land_and_report` does: for a reply built after its write.
pub(crate) fn report_approval(engine: &dyn Engine, v: &mut serde_json::Value) {
    report_landing(engine, v, "version", None);
}

/// Attaches the landing consequences to the landing of an answer's reply, which is an
/// `AnswerInfo` or a list of them: for each one with a `landing`, the consequences of the
/// version it landed, as `land_and_report` attaches them to an approval. Other answers stay as
/// they are.
pub fn report_answer_landings(engine: &dyn Engine, v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Array(done) => {
            for one in done {
                report_answer_landings(engine, one);
            }
        }
        _ => {
            if let Some(landing) = v.get_mut("landing").filter(|l| l.is_object()) {
                report_landing(engine, landing, "version", None);
            }
        }
    }
}

/// Attaches the landing consequences of the version hash in `v[key]`.
fn report_landing(
    engine: &dyn Engine,
    v: &mut serde_json::Value,
    key: &str,
    max_tokens: Option<usize>,
) {
    match v
        .get(key)
        .and_then(|h| h.as_str())
        .and_then(|h| h.parse::<Hash>().ok())
    {
        Some(h) => {
            let c = engine.read().landing_consequences(&h, max_tokens);
            attach(v, c);
        }
        None => v["consequences"] = serde_json::json!([]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Local, Session, SessionKind, WriteRequest};
    use rdy_model::Role;
    use serde_json::json;

    /// A reply for a stored proposal of view `work`, as `propose` builds it.
    fn proposed() -> (tempfile::TempDir, Local, serde_json::Value) {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let s = Session {
            actor: "agent:designer".into(),
            run: Some("run-1".into()),
            role: Role::Writer,
            kind: SessionKind::Human,
        };
        r.write(
            &s,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Ops", "doc_type": "overview"}}
                ]))
                .unwrap(),
                justification: None,
                message: "doc".into(),
            },
        )
        .unwrap();
        let info = r.propose(&s, "work", "work", None).unwrap();
        let v = serde_json::to_value(&info).unwrap();
        (dir, Local::new(r), v)
    }

    /// cc16 fix 1: a coverage_error the unapplied-decision pass left does not stop the coverage
    /// read: the reply gains `coverage` and keeps the error text.
    #[test]
    fn cc16_a_warning_pass_error_still_yields_the_coverage() {
        let (_d, e, mut v) = proposed();
        v["coverage_error"] = "E_INTERNAL: warning pass".into();
        attach_proposal_coverage(&e, &mut v);
        assert!(v["coverage"]["before"].is_u64(), "{v:#}");
        assert_eq!(v["coverage_error"], "E_INTERNAL: warning pass", "{v:#}");
    }

    /// df4 (D-W7KAK): a kept suspect's follow-up and fix, by the suspect's reason and whether the
    /// block is a live (or partly superseded) decision.
    #[test]
    fn df4_kept_rows_follow_the_table() {
        let row = |f: FollowUp, fix: &str| Some((f, fix.to_string()));
        // fix round 1 (ruling 1): a live decision is frozen, so it always gets reaffirm, whatever
        // the reason; an update would send the agent to an edit rdy refuses (E_FROZEN)
        for reason in ["changed", "deleted", "superseded", "revoked"] {
            assert_eq!(
                kept_follow_up(reason, true),
                row(FollowUp::Reaffirm, REAFFIRM_FIX),
                "{reason}"
            );
        }
        assert_eq!(
            kept_follow_up("changed", false),
            row(FollowUp::Confirm, KEPT_CHANGED_FIX)
        );
        assert_eq!(
            kept_follow_up("deleted", false),
            row(FollowUp::Update, KEPT_DELETED_FIX)
        );
        assert_eq!(
            kept_follow_up("superseded", false),
            row(FollowUp::Update, KEPT_REPLACED_FIX)
        );
        assert_eq!(
            kept_follow_up("revoked", false),
            row(FollowUp::Update, KEPT_REPLACED_FIX)
        );
        for live in [true, false] {
            assert_eq!(kept_follow_up("moved", live), None);
            assert_eq!(kept_follow_up("", live), None);
        }
    }

    /// df4 (ledger O4): the conflict text names each id once, sorted, and says "each" for more.
    #[test]
    fn df4_conflict_error_names_each_id_once_sorted() {
        assert_eq!(
            conflict_error(&["c-0PEN1".into()]),
            "E_CONFLICT_UNRESOLVED: no consequences while c-0PEN1 is in conflict. Write it from the version you want (rdy_context lists the sides), then propose again."
        );
        assert_eq!(
            conflict_error(&["c-2".into(), "c-1".into(), "c-2".into()]),
            "E_CONFLICT_UNRESOLVED: no consequences while c-1, c-2 are in conflict. Write each from the version you want (rdy_context lists the sides), then propose again."
        );
    }

    /// A tension read error means the coverage cannot be worked out: the reply is left as it is.
    #[test]
    fn cc16_a_tension_read_error_leaves_the_coverage_out() {
        let (_d, e, mut v) = proposed();
        v["tensions_error"] = "E_INTERNAL: boom".into();
        v["coverage_error"] = "E_INTERNAL: boom".into();
        attach_proposal_coverage(&e, &mut v);
        assert!(v.get("coverage").is_none(), "{v:#}");
    }
}
