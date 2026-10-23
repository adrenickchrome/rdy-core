//! Trace (D-1876T): why a block says what it says, read on demand from the recorded decisions
//! and landings of one captured scope. Nothing here writes: no plan, briefing, report or cursor.
//!
//! The start set is the block itself when it is a decision, else the decisions that govern it by
//! the graph's routes (D-VPATZ), live or proposed, beside the sources it reflects. Lineage
//! (supersedes, revokes, refines, both directions) is walked for at most `depth` steps; each
//! lineage decision adds its one-step premises (depends_on, conditional_on) and every decision
//! shown adds the questions it answers. `ungrounded` follows the coverage rule (D-X4RYX). The
//! landing that introduced the block and the landing that accepted each decision are found by
//! streaming the scope's own landing chain, and only the seals of that chain count.
//!
//! Every read is bounded before it allocates (O3): depth 1 to 8, 5000 distinct ids, 20000
//! distinct edges, 16777216 assembled JSON bytes and 100000 landings. A reply over its token
//! budget drops whole decision rows, deepest first, and names each in `frontier` (TASK-507 (2a)).
use crate::brief::{budget_bytes, effective_stable};
use crate::error::{E_BAD_INPUT, E_LIMIT, E_UNKNOWN_BLOCK, RdyError};
use crate::gate::{ProposalRef, version_number};
use crate::history::{Author, Carried, ChainWrites, chain_writes};
use crate::query::{is_entity_in, summary_in};
use crate::tensions::index::is_normative;
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{DerivedStatus, Edge, EdgeKind, GoverningCache, GoverningStatuses, Graph};
use rdy_model::engine_fields::REAFFIRMED;
use rdy_model::plan::{Plan, PlanOutcome};
use rdy_model::{ActorRun, Block, BlockId, FieldValue, Hash, decision};
use rdy_schema::supporting::{REFLECTS_LINK, SOURCE};
use rdy_schema::{Glob, REFLECTS_FIELD, TypeClass};
use serde::{Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The lineage depth a trace walks when none is given.
pub const TRACE_DEFAULT_DEPTH: u8 = 3;
/// The deepest lineage a trace walks (O3).
pub const TRACE_MAX_DEPTH: u8 = 8;
/// The token budget of a trace when none is given (four bytes each).
pub const TRACE_DEFAULT_TOKENS: usize = 1000;
/// The largest token budget a trace takes.
pub const TRACE_MAX_TOKENS: usize = 100_000;
/// The most distinct block ids one trace visits: decisions, premises, questions, sources and the
/// block itself.
pub const MAX_TRACE_BLOCKS: usize = 5000;
/// The most distinct edges one trace keeps.
pub const MAX_TRACE_EDGES: usize = 20_000;
/// The most JSON bytes one trace assembles, before its token cut.
pub const MAX_TRACE_BYTES: usize = 16_777_216;

/// The fix of every walk cap.
const WALK_FIX: &str = "trace a governing decision with a smaller depth";

/// What a trace reads: its lineage depth (1 to 8), whether to add the recorded detail, and its
/// token budget (at most 100000).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceOptions {
    pub depth: u8,
    pub detail: bool,
    pub max_tokens: usize,
}

impl Default for TraceOptions {
    fn default() -> Self {
        TraceOptions {
            depth: TRACE_DEFAULT_DEPTH,
            detail: false,
            max_tokens: TRACE_DEFAULT_TOKENS,
        }
    }
}

/// The refusal of a depth outside 1 to 8.
pub fn depth_error(depth: impl std::fmt::Display) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("depth {depth} is outside 1 to {TRACE_MAX_DEPTH}"),
        format!("pass a depth from 1 to {TRACE_MAX_DEPTH}"),
    )
}

/// A boundary's signed depth, checked: 1 to 8, else [`depth_error`] naming it as given.
pub fn check_depth(depth: i64) -> Result<u8, RdyError> {
    match u8::try_from(depth) {
        Ok(d) if (1..=TRACE_MAX_DEPTH).contains(&d) => Ok(d),
        _ => Err(depth_error(depth)),
    }
}

/// The refusal of a token budget over [`TRACE_MAX_TOKENS`] (the read boundaries' text).
fn tokens_error(n: usize) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("max_tokens {n} is over the limit of {TRACE_MAX_TOKENS}"),
        format!("ask for at most {TRACE_MAX_TOKENS} tokens and page through the rest"),
    )
}

fn blocks_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("trace exceeds {MAX_TRACE_BLOCKS} blocks"),
        WALK_FIX,
    )
}

fn edges_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("trace exceeds {MAX_TRACE_EDGES} edges"),
        WALK_FIX,
    )
}

fn bytes_limit() -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("trace exceeds {MAX_TRACE_BYTES} bytes"),
        WALK_FIX,
    )
}

fn unknown_root(id: &BlockId) -> RdyError {
    RdyError::new(
        E_UNKNOWN_BLOCK,
        format!("unknown block {id}"),
        "check the id with rdy_search",
    )
}

/// A stored field value as JSON (what [`crate::query::field_json`] gives), serialized from the
/// value itself: a borrowed one is measured without a copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Json<F = FieldValue>(pub F);

impl<F: Borrow<FieldValue>> Serialize for Json<F> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        AsJson(self.0.borrow()).serialize(s)
    }
}

struct AsJson<'a>(&'a FieldValue);

impl Serialize for AsJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self.0 {
            FieldValue::Null => s.serialize_unit(),
            FieldValue::Bool(b) => s.serialize_bool(*b),
            FieldValue::Int(i) => s.serialize_i64(*i),
            FieldValue::Str(t) => s.serialize_str(t),
            FieldValue::List(l) => {
                let mut seq = s.serialize_seq(Some(l.len()))?;
                for v in l {
                    seq.serialize_element(&AsJson(v))?;
                }
                seq.end()
            }
            FieldValue::Map(m) => {
                let mut map = s.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    map.serialize_entry(k, &AsJson(v))?;
                }
                map.end()
            }
        }
    }
}

/// One decision that governs the traced block and its route (D-VPATZ): `via` is `decided_by`,
/// `implements`, `affects` or `describes/affects`; `entity` names the entity of the last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GoverningTrace<S = String> {
    pub decision: S,
    /// [`DerivedStatus::tag`].
    pub status: &'static str,
    pub via: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<S>,
}

/// A held source the traced block reflects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceTrace<S = String> {
    pub source: S,
    pub kind: S,
    pub title: S,
}

/// A premise that is no decision (TASK-507 (1)): shown, never expanded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PremiseTrace<S = String> {
    pub id: S,
    pub block_type: S,
    pub summary: String,
    pub depth: u8,
}

/// A question a shown decision answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QuestionTrace<S = String> {
    pub id: S,
    pub prompt: S,
    pub depth: u8,
}

/// An edge the trace walked, as `rdy_context` serializes one, without its pin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TraceEdge<S = String> {
    pub from: S,
    pub to: S,
    pub kind: EdgeKind,
    pub part: Option<S>,
}

/// What a relayed approval quoted, as `rdy log`'s `relay_evidence` gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RelayEvidenceTrace {
    pub quoted: String,
    pub session_ref: String,
    pub question: Option<String>,
}

/// How an approval was obtained, as `rdy log`'s `evidence` gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CardEvidenceTrace {
    pub kind: String,
    pub options: Option<Vec<String>>,
    pub choice: Option<String>,
    pub card_hash: Option<String>,
}

/// A landing that carried a write: the block's first create, or a decision's oldest accepted
/// version. `author` and `imported_by` are the writing change's; the rest is the landing's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LandingTrace {
    pub id: String,
    /// `P-<stable short>@<n>` of the version the approver approved.
    pub proposal: String,
    pub title: String,
    pub approved_by: String,
    pub assurance: String,
    pub author: Author,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<ActorRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<ActorRun>,
    /// Detail only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<RelayEvidenceTrace>,
    /// Detail only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_evidence: Option<CardEvidenceTrace>,
}

/// A decision's `reaffirmed` record: its target ids, or with detail its whole engine field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged, bound(serialize = "S: Serialize, F: Borrow<FieldValue>"))]
pub enum ReaffirmedTrace<S = String, F = FieldValue> {
    Targets(Vec<S>),
    Full(Json<F>),
}

/// One seal of the scope's chain that names a decision (detail only), as `rdy_plan` lists one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SealedPlanTrace {
    pub landing: String,
    pub version: String,
    pub items: usize,
    pub outcomes: BTreeMap<&'static str, usize>,
}

/// One decision of a trace. Authored scalars appear only when the decision holds them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(bound(serialize = "S: Serialize, F: Borrow<FieldValue>"))]
pub struct DecisionTrace<S = String, F = FieldValue> {
    pub id: S,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<S>,
    pub depth: u8,
    /// [`DerivedStatus::tag`].
    pub status: &'static str,
    pub statement: S,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority_basis: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attested_by: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reversibility: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<S>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_in: Option<LandingTrace>,
    /// The distinct seals of the chain that name the decision, per outcome present in them; all
    /// ten outcomes named.
    pub plans: BTreeMap<&'static str, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reaffirmed: Option<ReaffirmedTrace<S, F>>,
    /// Detail only: the authored options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Json<F>>,
    /// Detail only: the seals, oldest landing first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sealed_plans: Option<Vec<SealedPlanTrace>>,
}

/// Why a block says what it says (D-1876T).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TraceReply {
    pub id: String,
    pub block_type: String,
    pub summary: String,
    pub governing: Vec<GoverningTrace>,
    pub sources: Vec<SourceTrace>,
    pub ungrounded: bool,
    pub introduced: Option<LandingTrace>,
    pub decisions: Vec<DecisionTrace>,
    pub premises: Vec<PremiseTrace>,
    pub edges: Vec<TraceEdge>,
    pub questions: Vec<QuestionTrace>,
    pub frontier: Vec<String>,
    /// Set only by a token-budget cut.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// The decision rows a token-budget cut removed; present only with `truncated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted: Option<usize>,
}

// ---- conversion of measured rows ----

fn own(s: &str) -> String {
    s.to_string()
}

impl GoverningTrace<&str> {
    fn owned(&self) -> GoverningTrace {
        GoverningTrace {
            decision: own(self.decision),
            status: self.status,
            via: self.via,
            entity: self.entity.map(own),
        }
    }
}

impl SourceTrace<&str> {
    fn owned(&self) -> SourceTrace {
        SourceTrace {
            source: own(self.source),
            kind: own(self.kind),
            title: own(self.title),
        }
    }
}

impl PremiseTrace<&str> {
    fn owned(self) -> PremiseTrace {
        PremiseTrace {
            id: own(self.id),
            block_type: own(self.block_type),
            summary: self.summary,
            depth: self.depth,
        }
    }
}

impl QuestionTrace<&str> {
    fn owned(&self) -> QuestionTrace {
        QuestionTrace {
            id: own(self.id),
            prompt: own(self.prompt),
            depth: self.depth,
        }
    }
}

impl TraceEdge<&str> {
    fn owned(&self) -> TraceEdge {
        TraceEdge {
            from: own(self.from),
            to: own(self.to),
            kind: self.kind.clone(),
            part: self.part.map(own),
        }
    }
}

impl DecisionTrace<&str, &FieldValue> {
    fn owned(self) -> DecisionTrace {
        DecisionTrace {
            id: own(self.id),
            key: self.key.map(own),
            depth: self.depth,
            status: self.status,
            statement: own(self.statement),
            authority_basis: self.authority_basis.map(own),
            attested_by: self.attested_by.map(own),
            reversibility: self.reversibility.map(own),
            decided_at: self.decided_at.map(own),
            recorded_at: self.recorded_at.map(own),
            effective_from: self.effective_from.map(own),
            accepted_in: self.accepted_in,
            plans: self.plans,
            reaffirmed: self.reaffirmed.map(|r| match r {
                ReaffirmedTrace::Targets(ids) => {
                    ReaffirmedTrace::Targets(ids.into_iter().map(own).collect())
                }
                ReaffirmedTrace::Full(Json(v)) => ReaffirmedTrace::Full(Json(v.clone())),
            }),
            options: self.options.map(|Json(v)| Json(v.clone())),
            sealed_plans: self.sealed_plans,
        }
    }
}

// ---- measuring ----

/// An `io::Write` that only counts: a row's JSON length without building it.
struct Count(usize);

impl std::io::Write for Count {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `v`'s compact JSON length in bytes.
fn json_len<T: Serialize + ?Sized>(v: &T) -> usize {
    let mut c = Count(0);
    serde_json::to_writer(&mut c, v).expect("a trace row serializes");
    c.0
}

/// The running size of the assembled reply, refused past [`MAX_TRACE_BYTES`] before the row
/// that would pass it is copied.
struct ByteBudget(usize);

impl ByteBudget {
    /// Adds one array element of `len` bytes to an array that holds `before` elements.
    fn element(&mut self, before: usize, len: usize) -> Result<(), RdyError> {
        self.add(len + usize::from(before > 0))
    }

    fn add(&mut self, n: usize) -> Result<(), RdyError> {
        self.0 = self.0.saturating_add(n);
        if self.0 > MAX_TRACE_BYTES {
            return Err(bytes_limit());
        }
        Ok(())
    }
}

// ---- the walk ----

/// The ids a trace has visited, capped at [`MAX_TRACE_BLOCKS`] before each new one.
struct Visited(BTreeSet<BlockId>);

impl Visited {
    fn add(&mut self, id: &BlockId) -> Result<(), RdyError> {
        if self.0.contains(id) {
            return Ok(());
        }
        if self.0.len() == MAX_TRACE_BLOCKS {
            return Err(blocks_limit());
        }
        self.0.insert(id.clone());
        Ok(())
    }
}

/// An edge's identity: endpoints, kind name and part, borrowed from the graph.
type EdgeKey<'g> = (&'g BlockId, &'g BlockId, &'g str, Option<&'g str>);

/// The distinct edges kept, capped at [`MAX_TRACE_EDGES`] before each new one.
#[derive(Default)]
struct Edges<'g>(BTreeMap<EdgeKey<'g>, &'g Edge>);

impl<'g> Edges<'g> {
    fn add(&mut self, e: &'g Edge) -> Result<(), RdyError> {
        let EdgeKind::Link(kind) = &e.kind else {
            return Ok(());
        };
        let key = (&e.from, &e.to, kind.as_str(), e.part.as_deref());
        if self.0.contains_key(&key) {
            return Ok(());
        }
        if self.0.len() == MAX_TRACE_EDGES {
            return Err(edges_limit());
        }
        self.0.insert(key, e);
        Ok(())
    }
}

fn link_is(e: &Edge, names: &[&str]) -> bool {
    matches!(&e.kind, EdgeKind::Link(n) if names.contains(&n.as_str()))
}

const LINEAGE: [&str; 3] = [decision::SUPERSEDES, decision::REVOKES, decision::REFINES];
const PREMISES: [&str; 2] = [decision::DEPENDS_ON, decision::CONDITIONAL_ON];

/// The decision whose status the graph knows: a decision block the scope holds.
fn is_decision(g: &Graph, id: &BlockId) -> bool {
    g.status(id).is_some()
}

/// The lineage neighbours of `id` that are decisions, with the edge that joins each.
fn lineage_of<'g>(g: &'g Graph, id: &BlockId) -> impl Iterator<Item = (&'g BlockId, &'g Edge)> {
    let out = g
        .outgoing(id)
        .iter()
        .filter(|e| link_is(e, &LINEAGE))
        .map(|e| (&e.to, e));
    let inc = g
        .incoming(id)
        .iter()
        .filter(|e| link_is(e, &LINEAGE))
        .map(|e| (&e.from, e));
    out.chain(inc).filter(move |(n, _)| is_decision(g, n))
}

/// What the graph walk found: every decision shown at its least depth, the non-decision premises and questions with their least depth and owners, the edges,
/// and the frontier the depth boundary and the unexpanded premises give.
struct Walk<'g> {
    decisions: BTreeMap<BlockId, u8>,
    premises: BTreeMap<BlockId, (u8, BTreeSet<BlockId>)>,
    questions: BTreeMap<BlockId, (u8, BTreeSet<BlockId>)>,
    edges: Edges<'g>,
    frontier: BTreeSet<BlockId>,
}

fn walk<'g>(
    g: &'g Graph,
    m: &Materialized,
    starts: &[BlockId],
    depth: u8,
    visited: &mut Visited,
) -> Result<Walk<'g>, RdyError> {
    // lineage, layer by layer in id order: a breadth-first walk gives each its least depth
    let mut lineage: BTreeMap<BlockId, u8> = BTreeMap::new();
    let mut layer: BTreeSet<BlockId> = BTreeSet::new();
    for s in starts {
        visited.add(s)?;
        lineage.insert(s.clone(), 0);
        layer.insert(s.clone());
    }
    for d in 0..depth {
        let mut next = BTreeSet::new();
        for id in &layer {
            for (n, _) in lineage_of(g, id) {
                if !lineage.contains_key(n) && !next.contains(n) {
                    visited.add(n)?;
                    next.insert(n.clone());
                }
            }
        }
        for n in &next {
            lineage.insert(n.clone(), d + 1);
        }
        if next.is_empty() {
            break;
        }
        layer = next;
    }
    let mut edges = Edges::default();
    let mut frontier = BTreeSet::new();
    for (id, d) in &lineage {
        for (n, e) in lineage_of(g, id) {
            if lineage.contains_key(n) {
                edges.add(e)?;
            } else if *d == depth {
                // TASK-507 (2b): a boundary decision with an unvisited lineage neighbour
                frontier.insert(id.clone());
            }
        }
    }
    // one-step premises of every lineage decision, in (depth, id) order
    let mut by_depth: Vec<(u8, &BlockId)> = lineage.iter().map(|(i, d)| (*d, i)).collect();
    by_depth.sort();
    let mut decisions = lineage.clone();
    let mut premises: BTreeMap<BlockId, (u8, BTreeSet<BlockId>)> = BTreeMap::new();
    for (d, id) in &by_depth {
        for e in g.outgoing(id).iter().filter(|e| link_is(e, &PREMISES)) {
            if m.get(&e.to).is_none() || e.to == **id {
                continue;
            }
            if is_decision(g, &e.to) {
                if !lineage.contains_key(&e.to) {
                    visited.add(&e.to)?;
                    let at = decisions.entry(e.to.clone()).or_insert(d + 1);
                    *at = (*at).min(d + 1);
                    // TASK-507 (2c): an unexpanded decision premise
                    frontier.insert(e.to.clone());
                }
            } else {
                visited.add(&e.to)?;
                let row = premises
                    .entry(e.to.clone())
                    .or_insert((d + 1, BTreeSet::new()));
                row.0 = row.0.min(d + 1);
                row.1.insert((*id).clone());
            }
            edges.add(e)?;
        }
    }
    // the questions every decision shown answers
    let mut questions: BTreeMap<BlockId, (u8, BTreeSet<BlockId>)> = BTreeMap::new();
    for (id, d) in &decisions {
        for e in g
            .outgoing(id)
            .iter()
            .filter(|e| link_is(e, &[decision::ANSWERS]))
        {
            if !m
                .get(&e.to)
                .is_some_and(|q| q.block_type == decision::QUESTION)
            {
                continue;
            }
            visited.add(&e.to)?;
            let row = questions
                .entry(e.to.clone())
                .or_insert((d + 1, BTreeSet::new()));
            row.0 = row.0.min(d + 1);
            row.1.insert(id.clone());
            edges.add(e)?;
        }
    }
    Ok(Walk {
        decisions,
        premises,
        questions,
        edges,
        frontier,
    })
}

/// D-X4RYX: a stable normative content block with no live governing route, no `reflects` link to
/// a held source and no nonempty list of valid `reflects` globs. A decision or a supporting block
/// (a source, a finding) is never ungrounded.
fn is_ungrounded(
    st: &ReadState,
    m: &Materialized,
    g: &Graph,
    b: &Block,
    has_source: bool,
    cache: &mut GoverningCache,
) -> bool {
    let pack = st.pack();
    if b.block_type == decision::DECISION
        || pack.class_of(&b.block_type) == TypeClass::Supporting
        || !is_normative(pack, b)
        || !effective_stable(m, Some(&b.id))
    {
        return false;
    }
    let live = !g
        .governing_routes(
            &b.id,
            GoverningStatuses::Live,
            |e| is_entity_in(pack, m, e),
            cache,
        )
        .is_empty();
    let globs = match b.fields.get(REFLECTS_FIELD) {
        Some(FieldValue::List(items)) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|i| i.as_str().is_some_and(|g| Glob::compile(g).is_ok()))
        }
        _ => false,
    };
    !(live || has_source || globs)
}

/// The landing chain a scope reads (its head): canon's head for canon and the in-flight scope, a
/// view's base landing, a proposal version's recorded base, a tag's or landing's own landing.
/// Unlanded changes of a view or proposal carry no landing.
fn chain_head(st: &ReadState, scope: &Scope) -> Result<Option<Hash>, RdyError> {
    Ok(match scope {
        Scope::Canon | Scope::Inflight => st.canon_head(),
        Scope::View(name) => st.effective_base(name),
        Scope::At(h) => Some(*h),
        Scope::Proposal(v) => st.proposal_version(v)?.canon_at,
    })
}

/// The landing `c` names, as a trace shows it; with `detail`, its recorded relay and approval
/// evidence as `rdy log` gives them.
fn landing_trace(st: &ReadState, c: &Carried, detail: bool) -> Result<LandingTrace, RdyError> {
    let reader = st.reader();
    let rec = reader.landing(&c.landing)?;
    let approved = rec.approved_version.unwrap_or(rec.proposal);
    let p = reader.proposal(&approved)?;
    let proposal = ProposalRef {
        version: approved,
        stable: p.stable.unwrap_or(approved),
        n: version_number(st, &approved)?,
    }
    .to_string();
    let title = if approved == rec.proposal {
        p.title
    } else {
        reader.proposal(&rec.proposal)?.title
    };
    let (evidence, card_evidence) = if detail {
        (
            rec.relay_evidence.map(|e| RelayEvidenceTrace {
                quoted: e.quoted,
                session_ref: e.session_ref,
                question: e.question,
            }),
            rec.evidence.map(|e| CardEvidenceTrace {
                kind: e.kind.as_str().into(),
                options: (!e.options.is_empty()).then_some(e.options),
                choice: e.choice,
                card_hash: e.card_hash.map(|h| h.to_string()),
            }),
        )
    } else {
        (None, None)
    };
    Ok(LandingTrace {
        id: c.landing.to_string(),
        proposal,
        title,
        approved_by: rec.approved_by.actor,
        assurance: rec.assurance.as_str().into(),
        author: c.author.clone(),
        relayed_by: rec.relayed_by,
        imported_by: c.imported_by.clone(),
        evidence,
        card_evidence,
    })
}

/// The ten outcomes, in their wire order.
const OUTCOMES: [PlanOutcome; 10] = [
    PlanOutcome::Updated,
    PlanOutcome::Relinked,
    PlanOutcome::Repinned,
    PlanOutcome::Reaffirmed,
    PlanOutcome::Retired,
    PlanOutcome::Committed,
    PlanOutcome::Confirmed,
    PlanOutcome::Done,
    PlanOutcome::Deferred,
    PlanOutcome::Open,
];

fn zero_outcomes() -> BTreeMap<&'static str, usize> {
    OUTCOMES.iter().map(|o| (o.as_str(), 0)).collect()
}

/// The seals of `id` whose landing lies in the chain, counted per outcome present (each seal
/// once per outcome), and with `detail` one row per seal, oldest landing first (log position),
/// ties by plan hash.
#[allow(clippy::type_complexity)]
fn plans_of(
    st: &ReadState,
    id: &BlockId,
    chain: &ChainWrites,
    detail: bool,
) -> Result<(BTreeMap<&'static str, usize>, Option<Vec<SealedPlanTrace>>), RdyError> {
    let reader = st.reader();
    let mut counts = zero_outcomes();
    let mut rows = Vec::new();
    for h in reader.plans_of_decision(id, st.seq())? {
        let plan: Plan = reader.plan(&h)?;
        let Some(seal) = plan.seal else {
            return Err(rdy_store::StoreError::Decode(format!(
                "plan {h} is indexed as a seal but has none"
            ))
            .into());
        };
        if !chain.landings.contains(&seal.landing) {
            continue;
        }
        let mut outcomes = zero_outcomes();
        for i in &seal.items {
            *outcomes.entry(i.outcome.as_str()).or_default() += 1;
        }
        for (o, n) in &outcomes {
            if *n > 0 {
                *counts.entry(o).or_default() += 1;
            }
        }
        if detail {
            let at = reader.landing_location(&seal.landing)?;
            rows.push((
                (at, h),
                SealedPlanTrace {
                    landing: seal.landing.to_string(),
                    version: seal.version.to_string(),
                    items: seal.items.len(),
                    outcomes,
                },
            ));
        }
    }
    if !detail {
        return Ok((counts, None));
    }
    rows.sort_by_key(|r| r.0);
    Ok((counts, Some(rows.into_iter().map(|(_, r)| r).collect())))
}

/// The borrowed row of decision `b` at `depth`, ready to measure.
fn decision_row<'b>(
    b: &'b Block,
    g: &Graph,
    depth: u8,
    accepted_in: Option<LandingTrace>,
    plans: BTreeMap<&'static str, usize>,
    sealed_plans: Option<Vec<SealedPlanTrace>>,
    detail: bool,
) -> DecisionTrace<&'b str, &'b FieldValue> {
    let f = |k: &str| b.field_str(k);
    let reaffirmed = b.fields.get(REAFFIRMED).map(|v| match (detail, v) {
        (true, _) => ReaffirmedTrace::Full(Json(v)),
        (false, FieldValue::Map(m)) => {
            ReaffirmedTrace::Targets(m.keys().map(String::as_str).collect())
        }
        (false, _) => ReaffirmedTrace::Targets(vec![]),
    });
    DecisionTrace {
        id: b.id.as_str(),
        key: f("key"),
        depth,
        status: g.status(&b.id).as_ref().map_or("", DerivedStatus::tag),
        statement: f("statement").unwrap_or_default(),
        authority_basis: f("authority_basis"),
        attested_by: f("attested_by"),
        reversibility: f("reversibility"),
        decided_at: f("decided_at"),
        recorded_at: f("recorded_at"),
        effective_from: f("effective_from"),
        accepted_in,
        plans,
        reaffirmed,
        options: if detail {
            b.fields.get("options").map(Json)
        } else {
            None
        },
        sealed_plans,
    }
}

/// A row of the cut and its measured JSON length.
struct Measured<T> {
    row: T,
    len: usize,
    kept: bool,
}

/// One list of the reply the cut may shorten, with its count of kept rows.
struct Rows<T> {
    rows: Vec<Measured<T>>,
    kept: usize,
}

impl<T> Rows<T> {
    fn with_capacity(n: usize) -> Self {
        Rows {
            rows: Vec::with_capacity(n),
            kept: 0,
        }
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn push(&mut self, row: T, len: usize) {
        self.rows.push(Measured {
            row,
            len,
            kept: true,
        });
        self.kept += 1;
    }

    /// Drops row `i` (once), taking its bytes and its comma from `total`.
    fn drop_row(&mut self, i: usize, total: &mut usize) -> bool {
        if !self.rows[i].kept {
            return false;
        }
        self.rows[i].kept = false;
        *total -= self.rows[i].len + usize::from(self.kept > 1);
        self.kept -= 1;
        true
    }

    fn into_kept(self) -> Vec<T> {
        self.rows
            .into_iter()
            .filter(|r| r.kept)
            .map(|r| r.row)
            .collect()
    }
}

impl ReadState {
    /// D-1876T: why `id` says what it says in `scope` (see the module). Depth 1 to 8 and at most
    /// [`TRACE_MAX_TOKENS`] are checked here as at every boundary. The captured state is read
    /// once, and its redaction epoch is checked before and after, so a removal that lands
    /// during the read is `E_REDACTED_STATE`.
    pub fn trace(
        &self,
        scope: &Scope,
        id: &BlockId,
        options: TraceOptions,
    ) -> Result<TraceReply, RdyError> {
        if !(1..=TRACE_MAX_DEPTH).contains(&options.depth) {
            return Err(depth_error(options.depth));
        }
        if options.max_tokens > TRACE_MAX_TOKENS {
            return Err(tokens_error(options.max_tokens));
        }
        self.ensure_current_epoch()?;
        let out = self.trace_in(scope, id, options)?;
        self.ensure_current_epoch()?;
        Ok(out)
    }

    fn trace_in(
        &self,
        scope: &Scope,
        id: &BlockId,
        options: TraceOptions,
    ) -> Result<TraceReply, RdyError> {
        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        let pack = self.pack();
        let root = m.get(id).ok_or_else(|| unknown_root(id))?;
        let mut cache = GoverningCache::default();
        let mut visited = Visited(BTreeSet::new());
        visited.add(id)?;
        let root_decision = root.block_type == decision::DECISION && is_decision(&g, id);

        // the start set: the decision itself, else its governing routes (live or proposed)
        let routes = if root_decision {
            vec![]
        } else {
            g.governing_routes(
                id,
                GoverningStatuses::LiveOrProposed,
                |e| is_entity_in(pack, &m, e),
                &mut cache,
            )
        };
        let starts: Vec<BlockId> = if root_decision {
            vec![id.clone()]
        } else {
            routes.iter().map(|r| r.decision.clone()).collect()
        };
        // the held sources the block reflects, by id
        let sources: BTreeMap<&BlockId, &Block> = g
            .outgoing(id)
            .iter()
            .filter(|e| link_is(e, &[REFLECTS_LINK]))
            .filter_map(|e| m.get(&e.to).filter(|t| t.block_type == SOURCE))
            .map(|t| (&t.id, t))
            .collect();
        for s in sources.keys() {
            visited.add(s)?;
        }
        let ungrounded = is_ungrounded(self, &m, &g, root, !sources.is_empty(), &mut cache);
        let w = walk(&g, &m, &starts, options.depth, &mut visited)?;

        // one stream of the scope's own landing chain for every introduction and acceptance
        let shown: BTreeSet<BlockId> = w.decisions.keys().cloned().collect();
        let chain = chain_writes(
            self.reader(),
            chain_head(self, scope)?,
            &BTreeSet::from([id.clone()]),
            &shown,
        )?;

        // assemble, measuring each row before its strings are copied
        let summary = summary_in(root, &m);
        let introduced = match chain.created.get(id) {
            Some(c) => Some(landing_trace(self, c, options.detail)?),
            None => None,
        };
        let mut reply = TraceReply {
            id: id.to_string(),
            block_type: root.block_type.clone(),
            summary,
            governing: vec![],
            sources: vec![],
            ungrounded,
            introduced: None,
            decisions: vec![],
            premises: vec![],
            edges: vec![],
            questions: vec![],
            frontier: vec![],
            truncated: false,
            omitted: None,
        };
        let mut bytes = ByteBudget(0);
        bytes.add(json_len(&reply))?;
        if let Some(l) = &introduced {
            bytes.add(json_len(l).saturating_sub("null".len()))?;
        }
        reply.introduced = introduced;
        for r in &routes {
            let row = GoverningTrace {
                decision: r.decision.as_str(),
                status: g
                    .status(&r.decision)
                    .as_ref()
                    .map_or("", DerivedStatus::tag),
                via: r.via,
                entity: r.entity.as_ref().map(BlockId::as_str),
            };
            bytes.element(reply.governing.len(), json_len(&row))?;
            reply.governing.push(row.owned());
        }
        for b in sources.values() {
            let row = SourceTrace {
                source: b.id.as_str(),
                kind: b.field_str("kind").unwrap_or_default(),
                title: b.field_str("title").unwrap_or_default(),
            };
            bytes.element(reply.sources.len(), json_len(&row))?;
            reply.sources.push(row.owned());
        }

        // the rows the cut may remove, each sorted by (depth, id)
        let mut order: Vec<(u8, &BlockId)> = w.decisions.iter().map(|(i, d)| (*d, i)).collect();
        order.sort();
        let mut decisions: Rows<DecisionTrace> = Rows::with_capacity(order.len());
        let mut decision_ids: Vec<(u8, BlockId)> = Vec::with_capacity(order.len());
        for (depth, did) in &order {
            let Some(b) = m.get(did) else { continue };
            let accepted_in = match chain.accepted.get(*did) {
                Some(c) => Some(landing_trace(self, c, options.detail)?),
                None => None,
            };
            let (plans, sealed) = plans_of(self, did, &chain, options.detail)?;
            let row = decision_row(b, &g, *depth, accepted_in, plans, sealed, options.detail);
            let len = json_len(&row);
            bytes.element(decisions.len(), len)?;
            decisions.push(row.owned(), len);
            decision_ids.push((*depth, (*did).clone()));
        }
        let mut premise_order: Vec<(u8, &BlockId)> =
            w.premises.iter().map(|(i, (d, _))| (*d, i)).collect();
        premise_order.sort();
        let mut premises: Rows<PremiseTrace> = Rows::with_capacity(premise_order.len());
        let mut premise_ids: Vec<BlockId> = Vec::with_capacity(premise_order.len());
        let mut premise_owners: Vec<BTreeSet<BlockId>> = Vec::with_capacity(premise_order.len());
        for (depth, pid) in &premise_order {
            let Some(b) = m.get(pid) else { continue };
            let row = PremiseTrace {
                id: b.id.as_str(),
                block_type: b.block_type.as_str(),
                summary: summary_in(b, &m),
                depth: *depth,
            };
            let len = json_len(&row);
            bytes.element(premises.len(), len)?;
            premises.push(row.owned(), len);
            premise_ids.push((*pid).clone());
            premise_owners.push(w.premises[*pid].1.clone());
        }
        let mut question_order: Vec<(u8, &BlockId)> =
            w.questions.iter().map(|(i, (d, _))| (*d, i)).collect();
        question_order.sort();
        let mut questions: Rows<QuestionTrace> = Rows::with_capacity(question_order.len());
        let mut question_ids: Vec<BlockId> = Vec::with_capacity(question_order.len());
        let mut question_owners: Vec<BTreeSet<BlockId>> = Vec::with_capacity(question_order.len());
        for (depth, qid) in &question_order {
            let Some(b) = m.get(qid) else { continue };
            let row = QuestionTrace {
                id: b.id.as_str(),
                prompt: b.field_str("prompt").unwrap_or_default(),
                depth: *depth,
            };
            let len = json_len(&row);
            bytes.element(questions.len(), len)?;
            questions.push(row.owned(), len);
            question_ids.push((*qid).clone());
            question_owners.push(w.questions[*qid].1.clone());
        }
        // each node's least row depth, for the edge order
        let mut node_depth: HashMap<&BlockId, u8> = HashMap::new();
        let rows = w
            .decisions
            .iter()
            .chain(w.premises.iter().map(|(i, (d, _))| (i, d)))
            .chain(w.questions.iter().map(|(i, (d, _))| (i, d)));
        for (i, d) in rows {
            let at = node_depth.entry(i).or_insert(*d);
            *at = (*at).min(*d);
        }
        let depth_of = |i: &BlockId| node_depth.get(i).copied().unwrap_or(0);
        let mut edge_order: Vec<(u8, EdgeKey<'_>, &Edge)> = w
            .edges
            .0
            .iter()
            .map(|(k, e)| (depth_of(k.0).min(depth_of(k.1)), *k, *e))
            .collect();
        edge_order.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let mut edges: Rows<TraceEdge> = Rows::with_capacity(edge_order.len());
        let mut edge_ends: Vec<(BlockId, BlockId)> = Vec::with_capacity(edge_order.len());
        for (_, _, e) in &edge_order {
            let row = TraceEdge {
                from: e.from.as_str(),
                to: e.to.as_str(),
                kind: e.kind.clone(),
                part: e.part.as_deref(),
            };
            let len = json_len(&row);
            bytes.element(edges.len(), len)?;
            edges.push(row.owned(), len);
            edge_ends.push((e.from.clone(), e.to.clone()));
        }
        let mut frontier: BTreeSet<(u8, BlockId)> = w
            .frontier
            .iter()
            .map(|f| (w.decisions.get(f).copied().unwrap_or(0), f.clone()))
            .collect();
        for (i, (_, f)) in frontier.iter().enumerate() {
            bytes.element(i, json_len(f))?;
        }

        let cut = Cut {
            budget: budget_bytes(options.max_tokens),
            total: bytes.0,
            decisions,
            decision_ids,
            premises,
            premise_ids,
            questions,
            question_ids,
            edges,
            edge_ends,
            premise_owners,
            question_owners,
        };
        let done = cut.run(&mut frontier);
        reply.decisions = done.decisions;
        reply.premises = done.premises;
        reply.questions = done.questions;
        reply.edges = done.edges;
        reply.frontier = frontier.into_iter().map(|(_, f)| f.to_string()).collect();
        if done.truncated {
            reply.truncated = true;
            reply.omitted = Some(done.omitted);
        }
        Ok(reply)
    }
}

/// The token cut (TASK-507 (2a)): over budget, decision rows deeper than zero go first,
/// greatest (depth, id) first, each with the premise and question rows only removed decisions
/// owned and the edges no kept row still needs; each removed decision joins the frontier. Past
/// them the remaining premise and question rows go, greatest (depth, id) first, then the
/// remaining edges, last first. The depth-zero decisions and the envelope always stay.
struct Cut {
    budget: usize,
    /// The reply's exact JSON length so far, without `truncated` and `omitted`.
    total: usize,
    decisions: Rows<DecisionTrace>,
    decision_ids: Vec<(u8, BlockId)>,
    premises: Rows<PremiseTrace>,
    premise_ids: Vec<BlockId>,
    questions: Rows<QuestionTrace>,
    question_ids: Vec<BlockId>,
    edges: Rows<TraceEdge>,
    edge_ends: Vec<(BlockId, BlockId)>,
    /// Each premise and question row's owners (the decisions with an edge to it).
    premise_owners: Vec<BTreeSet<BlockId>>,
    question_owners: Vec<BTreeSet<BlockId>>,
}

struct CutRows {
    decisions: Vec<DecisionTrace>,
    premises: Vec<PremiseTrace>,
    questions: Vec<QuestionTrace>,
    edges: Vec<TraceEdge>,
    truncated: bool,
    omitted: usize,
}

/// The bytes `truncated` and `omitted` add to a cut reply.
fn flag_bytes(omitted: usize) -> usize {
    ",\"truncated\":true".len() + ",\"omitted\":".len() + omitted.to_string().len()
}

/// The state of one node during the cut: its kept rows and whether it is a removed decision.
#[derive(Default)]
struct Node {
    kept_rows: usize,
    decision: bool,
    removed: bool,
}

impl Cut {
    fn run(mut self, frontier: &mut BTreeSet<(u8, BlockId)>) -> CutRows {
        let mut omitted = 0usize;
        let truncated = self.total > self.budget;
        if truncated {
            omitted = self.shorten(frontier);
        }
        CutRows {
            decisions: self.decisions.into_kept(),
            premises: self.premises.into_kept(),
            questions: self.questions.into_kept(),
            edges: self.edges.into_kept(),
            truncated,
            omitted,
        }
    }

    fn fits(&self, omitted: usize) -> bool {
        self.total + flag_bytes(omitted) <= self.budget
    }

    /// The cut itself; returns how many decision rows it removed.
    fn shorten(&mut self, frontier: &mut BTreeSet<(u8, BlockId)>) -> usize {
        // the indexes: each node's kept rows, each decision's owned rows, each node's edges
        let mut nodes: HashMap<BlockId, Node> = HashMap::new();
        for (_, d) in &self.decision_ids {
            let n = nodes.entry(d.clone()).or_default();
            n.kept_rows += 1;
            n.decision = true;
        }
        for p in self.premise_ids.iter().chain(&self.question_ids) {
            nodes.entry(p.clone()).or_default().kept_rows += 1;
        }
        let mut owned_premises: HashMap<&BlockId, Vec<usize>> = HashMap::new();
        for (i, owners) in self.premise_owners.iter().enumerate() {
            for o in owners {
                owned_premises.entry(o).or_default().push(i);
            }
        }
        let mut owned_questions: HashMap<&BlockId, Vec<usize>> = HashMap::new();
        for (i, owners) in self.question_owners.iter().enumerate() {
            for o in owners {
                owned_questions.entry(o).or_default().push(i);
            }
        }
        let mut edges_of: HashMap<BlockId, Vec<usize>> = HashMap::new();
        for (i, (a, b)) in self.edge_ends.iter().enumerate() {
            edges_of.entry(a.clone()).or_default().push(i);
            if a != b {
                edges_of.entry(b.clone()).or_default().push(i);
            }
        }
        let mut omitted = 0usize;
        let mut touched: Vec<BlockId> = Vec::new();
        // decisions deeper than zero, greatest (depth, id) first
        for i in (0..self.decisions.len()).rev() {
            if self.fits(omitted) {
                break;
            }
            let (depth, did) = self.decision_ids[i].clone();
            if depth == 0 {
                continue;
            }
            self.decisions.drop_row(i, &mut self.total);
            omitted += 1;
            if let Some(n) = nodes.get_mut(&did) {
                n.kept_rows -= 1;
                n.removed = true;
            }
            let key = (depth, did.clone());
            if !frontier.contains(&key) {
                self.total += did.as_str().len() + 2 + usize::from(!frontier.is_empty());
                frontier.insert(key);
            }
            touched.clear();
            touched.push(did.clone());
            for &p in owned_premises.get(&did).into_iter().flatten() {
                let all_gone = self.premise_owners[p]
                    .iter()
                    .all(|o| nodes.get(o).is_some_and(|n| n.removed));
                if all_gone && self.premises.drop_row(p, &mut self.total) {
                    let pid = self.premise_ids[p].clone();
                    if let Some(n) = nodes.get_mut(&pid) {
                        n.kept_rows -= 1;
                    }
                    touched.push(pid);
                }
            }
            for &q in owned_questions.get(&did).into_iter().flatten() {
                let all_gone = self.question_owners[q]
                    .iter()
                    .all(|o| nodes.get(o).is_some_and(|n| n.removed));
                if all_gone && self.questions.drop_row(q, &mut self.total) {
                    let qid = self.question_ids[q].clone();
                    if let Some(n) = nodes.get_mut(&qid) {
                        n.kept_rows -= 1;
                    }
                    touched.push(qid);
                }
            }
            for t in &touched {
                for &e in edges_of.get(t).into_iter().flatten() {
                    if !self.edge_needed(e, &nodes) {
                        self.edges.drop_row(e, &mut self.total);
                    }
                }
            }
        }
        // then the remaining premise and question rows, greatest (depth, id) first
        let mut rest: Vec<(u8, &BlockId, u8, usize)> = Vec::new();
        for (i, p) in self
            .premises
            .rows
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kept)
        {
            rest.push((p.row.depth, &self.premise_ids[i], 0, i));
        }
        for (i, q) in self
            .questions
            .rows
            .iter()
            .enumerate()
            .filter(|(_, q)| q.kept)
        {
            rest.push((q.row.depth, &self.question_ids[i], 1, i));
        }
        rest.sort();
        let rest: Vec<(BlockId, u8, usize)> = rest
            .into_iter()
            .rev()
            .map(|(_, id, k, i)| (id.clone(), k, i))
            .collect();
        for (rid, kind, i) in rest {
            if self.fits(omitted) {
                break;
            }
            let dropped = if kind == 0 {
                self.premises.drop_row(i, &mut self.total)
            } else {
                self.questions.drop_row(i, &mut self.total)
            };
            if dropped && let Some(n) = nodes.get_mut(&rid) {
                n.kept_rows -= 1;
            }
            for &e in edges_of.get(&rid).into_iter().flatten() {
                if !self.edge_needed(e, &nodes) {
                    self.edges.drop_row(e, &mut self.total);
                }
            }
        }
        // then the remaining edges, last first
        for e in (0..self.edges.len()).rev() {
            if self.fits(omitted) {
                break;
            }
            self.edges.drop_row(e, &mut self.total);
        }
        omitted
    }

    /// Whether edge `e` still joins two kept rows, or a kept decision to a removed one (a
    /// frontier relation).
    fn edge_needed(&self, e: usize, nodes: &HashMap<BlockId, Node>) -> bool {
        let (a, b) = &self.edge_ends[e];
        let state = |x: &BlockId| {
            nodes.get(x).map_or((false, false, false), |n| {
                (n.kept_rows > 0, n.decision && n.kept_rows > 0, n.removed)
            })
        };
        let (ka, da, ra) = state(a);
        let (kb, db, rb) = state(b);
        (ka && kb) || (da && rb) || (db && ra)
    }
}
