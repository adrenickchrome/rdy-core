//! What a write or a view op leaves for its author to do: the blocks that depend on what it
//! changed, read from the state it published, and the conflicts it recorded. Each entry has a
//! stable id (`cq-<16 hex>`).
use crate::brief::{BRIEF_DEFAULT_TOKENS, FollowUp, expand_error, follow_up};
use crate::engine::{Engine, write_typed};
use crate::error::RdyError;
use crate::gate::{LandInfo, ProposalInfo, applied_ops, changes_of, landed_as};
use crate::intent::IntentNote;
use crate::tensions::delta::on_change;
use crate::tensions::{SideRef, Tension, tension_label};
use crate::tensions::{TensionDelta, tension_delta};
use crate::views::{UndoResult, ViewOpResult};
use crate::{Materialized, Rdy, ReadState, Scope, now_ms};
use rdy_graph::{DerivedStatus, Edge, EdgeKind, Graph, IMPACT_LINKS, SUSPECT_LINKS, Suspect};
use rdy_model::decision::{DECISION, REFINES, REVOKES, SUPERSEDES};
use rdy_model::{BlockId, Hash, Op};
use serde::Serialize;
use std::cell::OnceCell;
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
    pub edge: String,
    pub changed: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Consequence {
    pub id: String,
    pub block: String,
    pub kind: ConsequenceKind,
    pub because: Because,
    pub follow_up: FollowUp,
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

/// The hint on a `reaffirm` entry.
pub const REAFFIRM_FIX: &str =
    "supersede it, or leave it suspect; re-affirmation arrives with FW-R6";

/// The hint on a `conflict` entry.
pub const CONFLICT_FIX: &str = "write the block from the version you want (rdy_context lists the sides), or exclude one side's change";

/// The hint on a `tension` entry.
pub const TENSION_FIX: &str = "resolve it with a change (rdy_tensions shows both sides), or ask a reviewer to dismiss it with rdy_resolve";

/// The hint on an entry for a block that a refined or part-superseded decision governs (FW-LP6).
pub const LINEAGE_FIX: &str = "a decision that governs it is refined: check that it still holds and answer confirm, or update it";

/// FW-LP6: for each decision `ops` create, each `refines` link and each `supersedes` link with a
/// part: (the new decision, the target, the link's name). In op order, then link order.
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

/// The first cause found for a dependent block.
struct Cause {
    suspect: bool,
    edge: String,
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

impl ReadState {
    /// The consequences of `change` in this state (the one the write published): for each block
    /// the change touched, each block outside the change that points at it. See
    /// `consequences_in` for the order and the cut.
    pub fn consequences(
        &self,
        scope: &Scope,
        change: &Hash,
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let ch = self.reader().change(change)?;
        let (touched, changed) = changed_by_ops(&ch.ops);
        // FW-LP6: the reaching links, by new decision, then target
        let mut reach = reached_targets(&ch.ops);
        reach.sort();
        let mine = |s: &SideRef| matches!(s, SideRef::Change(c) if c == change);
        // a tension set that cannot be built never fails the other entries
        let (tensions, tensions_error): (Vec<Consequence>, _) = match self.tensions() {
            Ok(set) => (
                set.involving_change(change)
                    .iter()
                    .map(|t| tension_entry(change, t, &mine))
                    .collect(),
                None,
            ),
            Err(e) => (vec![], Some(format!("{}: {}", e.code, e.message))),
        };
        // nothing can point at a block that did not exist: no graph to read
        let only_creates = ch.ops.iter().all(|o| matches!(o, Op::Create { .. }));
        if only_creates && changed == touched && reach.is_empty() {
            return Ok(Consequences {
                tensions_error,
                ..cut(tensions, max_tokens)
            });
        }
        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        let view = match scope {
            Scope::View(v) => Some(v.as_str()),
            Scope::Canon | Scope::At(_) | Scope::Inflight => None,
        };
        let written = self.written_in(view);
        Ok(Consequences {
            tensions_error,
            ..self.consequences_in(
                &g,
                &m,
                change,
                &touched,
                &changed,
                &written,
                vec![],
                tensions,
                &reach,
                max_tokens,
            )
        })
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
                because: Because {
                    edge: "conflict".into(),
                    changed: c.block.to_string(),
                },
                follow_up: FollowUp::Resolve,
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
        Ok(self.consequences_in(
            &g,
            &m,
            batch,
            &touched,
            &changed,
            &written,
            conflicts,
            tensions,
            &[],
            max_tokens,
        ))
    }

    /// The consequences of landing proposal version `version` on canon (Q2b): what would turn
    /// suspect or dependent in canon. When one of the version's changes has landed, canon as it is
    /// (the landing is in it); otherwise the version's changes merged onto canon, applied to
    /// canon's materialization, with canon's graph carried across the applied ops. The batch is
    /// the version hash, so the propose and the approval give each entry the same id. A version
    /// that does not merge onto canon is an error (the caller reports it).
    pub fn landing_consequences(
        &self,
        version: &Hash,
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let p = self.reader().proposal(version)?;
        let changes = changes_of(self, &p.changes)?;
        let ops: Vec<Op> = changes.iter().flat_map(|c| c.ops.iter().cloned()).collect();
        let (touched, changed) = changed_by_ops(&ops);
        let only_creates = ops.iter().all(|o| matches!(o, Op::Create { .. }));
        if only_creates && changed == touched {
            return Ok(Consequences::default());
        }
        // build canon's graph first, so that `after` carries it instead of building another
        self.graph(&Scope::Canon)?;
        let (m, g) = if landed_as(self, &p)?.is_some() {
            (
                self.materialized(&Scope::Canon)?,
                self.graph(&Scope::Canon)?,
            )
        } else {
            let reader = self.reader();
            let (_, applied) =
                reader.merge_checked(&reader.state_of(self.canon_head())?, &changes)?;
            let next = self
                .built(&Scope::Canon)?
                .after(&applied_ops(&changes, &applied));
            let g = Arc::clone(next.graph.get_or_init(|| {
                Arc::new(Graph::build_with(
                    next.m.iter(),
                    Arc::clone(reader.tracking()),
                ))
            }));
            (Arc::clone(&next.m), g)
        };
        Ok(self.consequences_in(
            &g,
            &m,
            version,
            &touched,
            &changed,
            &|_| false,
            vec![],
            vec![],
            &[],
            max_tokens,
        ))
    }

    /// The entries for `changed` in graph `g` over `m`: for each changed block, each block
    /// outside `touched` that points at it. `conflicts` (by block id) come first, then
    /// `tensions` (as given), then suspect entries, then dependent ones, each by block id, then
    /// the FW-LP6 entries of `reach` (a `dependent` entry with follow-up `confirm` for each block
    /// a reached target governs, in the order of `reach`, never a block `touched` holds, the new
    /// decision itself, or a block the list already holds; none for a target that is rejected,
    /// superseded or revoked); `written` marks `already_written`. The list is cut
    /// from the end to `max_tokens` (default `BRIEF_DEFAULT_TOKENS`), counting what it drops in
    /// `omitted`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn consequences_in(
        &self,
        g: &Graph,
        m: &Materialized,
        batch: &Hash,
        touched: &BTreeSet<BlockId>,
        changed: &BTreeSet<BlockId>,
        written: &dyn Fn(&BlockId) -> bool,
        mut conflicts: Vec<Consequence>,
        tensions: Vec<Consequence>,
        reach: &[(BlockId, BlockId, &'static str)],
        max_tokens: Option<usize>,
    ) -> Consequences {
        let mut suspects_of: BTreeMap<BlockId, Vec<Suspect>> = BTreeMap::new();
        let mut causes: BTreeMap<BlockId, Cause> = BTreeMap::new();
        for x in changed {
            let mut by_from: BTreeMap<&BlockId, Vec<&Edge>> = BTreeMap::new();
            for e in g.incoming(x) {
                if e.kind != EdgeKind::Contains && !touched.contains(&e.from) {
                    by_from.entry(&e.from).or_default().push(e);
                }
            }
            for (from, mut edges) in by_from {
                if m.get(from).is_none() {
                    continue;
                }
                edges.sort_by_key(|e| e.kind.name());
                let found = suspects_of.entry(from.clone()).or_insert_with(|| {
                    let mut s = g.suspects_of(from);
                    s.retain(|s| !self.is_presentational_source(m, s));
                    s
                });
                let suspect = found.iter().any(|s| s.target == *x);
                let pick = if suspect {
                    edges.iter().find(|e| premise(e)).or_else(|| edges.first())
                } else {
                    edges.iter().find(|e| impact(e))
                };
                let Some(e) = pick else { continue };
                let better = causes.get(from).is_none_or(|c| suspect && !c.suspect);
                if better {
                    causes.insert(
                        from.clone(),
                        Cause {
                            suspect,
                            edge: e.kind.name(),
                            changed: x.clone(),
                        },
                    );
                }
            }
        }

        let mut suspect_items = vec![];
        let mut dependent_items = vec![];
        for (block, cause) in causes {
            let Some(src) = m.get(&block) else { continue };
            let deleted = m.get(&cause.changed).is_none();
            // no entry for a decision that asserts nothing live
            let value_reference = cause.edge == "references"
                && m.get(&cause.changed)
                    .is_some_and(|b| b.block_type == rdy_model::entity::VALUE);
            let Some(follow) = follow_up(
                self.pack(),
                src,
                g.status(&block).as_ref(),
                deleted,
                value_reference,
            ) else {
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
                },
                follow_up: follow,
                conflict: None,
                tension: None,
                fix: (follow == FollowUp::Reaffirm).then(|| REAFFIRM_FIX.to_string()),
                already_written: written(&block),
            };
            match kind {
                ConsequenceKind::Suspect => suspect_items.push(item),
                _ => dependent_items.push(item),
            }
        }
        conflicts.sort_by(|a, b| a.block.cmp(&b.block));
        conflicts.extend(tensions);
        conflicts.extend(suspect_items);
        conflicts.extend(dependent_items);
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
            for block in g.governed_by(target) {
                if touched.contains(&block) || block == *decision || m.get(&block).is_none() {
                    continue;
                }
                if !listed.insert(block.to_string()) {
                    continue;
                }
                conflicts.push(Consequence {
                    id: consequence_id(batch, ConsequenceKind::Dependent.as_str(), &block),
                    block: block.to_string(),
                    kind: ConsequenceKind::Dependent,
                    because: Because {
                        edge: (*via).to_string(),
                        changed: target.to_string(),
                    },
                    follow_up: FollowUp::Confirm,
                    conflict: None,
                    tension: None,
                    fix: Some(LINEAGE_FIX.to_string()),
                    already_written: written(&block),
                });
            }
        }
        cut(conflicts, max_tokens)
    }
}

/// `items` cut from the end to `max_tokens` (default `BRIEF_DEFAULT_TOKENS`; four bytes a
/// token), counting what it drops in `omitted`, by kind.
fn cut(items: Vec<Consequence>, max_tokens: Option<usize>) -> Consequences {
    let mut out = Consequences {
        items,
        ..Consequences::default()
    };
    let budget = max_tokens.unwrap_or(BRIEF_DEFAULT_TOKENS).saturating_mul(4);
    let size = |c: &Consequences| {
        serde_json::to_string(c)
            .expect("consequences serialize: strings and string-keyed maps only")
            .len()
    };
    while !out.items.is_empty() && size(&out) > budget {
        let drop = out.items.len().div_ceil(2);
        for dropped in out.items.split_off(out.items.len() - drop) {
            *out.omitted
                .entry(dropped.kind.as_str().to_string())
                .or_default() += 1;
        }
    }
    out
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
        because: Because {
            edge: t.kind.name().to_string(),
            changed: changed.to_string(),
        },
        follow_up: FollowUp::Resolve,
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
    match c {
        Ok(c) => {
            v["consequences"] = serde_json::to_value(&c.items)
                .expect("consequences serialize: strings and string-keyed maps only");
            if !c.omitted.is_empty() {
                v["consequences_omitted"] =
                    serde_json::to_value(&c.omitted).expect("a map of counts serializes");
            }
            if let Some(e) = c.tensions_error {
                v["consequences_tension_error"] = e.into();
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

/// `write_typed`, then the write's consequences read from the state it published: on success
/// `consequences` is always present (with `consequences_omitted` when a cut dropped some, or
/// `consequences_error` when they could not be read; the write stands), then `intents` when other
/// sessions' open intents cover blocks the change wrote (iwr1), then `on_change` (the tensions
/// the write opened or resolved in the view; `on_change_error` when they could not be read),
/// and the write's suggestions (FW-EN3), which never fail the write; on a briefing refusal
/// the error carries a fresh briefing for exactly the blocks the write touches.
pub fn write_and_report<T: Serialize + 'static>(
    engine: &dyn Engine,
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
    let change = v
        .get("change")
        .and_then(|c| c.as_str())
        .and_then(|c| c.parse::<Hash>().ok());
    match change {
        Some(h) => {
            let st = engine.read();
            let c = st.consequences(&Scope::View(view.into()), &h, max_tokens);
            attach(&mut v, c);
            if let Some(notes) = intent_notes_of(&st, before, view, &h)
                && !notes.is_empty()
            {
                v["intents"] = serde_json::to_value(&notes).expect("intent notes serialize");
            }
            if let Some((b, a)) = &states {
                let r = tension_delta(b, a, Some(&[h]))
                    .and_then(|d| on_change(b, a, view, max_tokens, &d));
                attach_on_change(&mut v, r);
            }
        }
        None => v["consequences"] = serde_json::json!([]),
    }
    match change {
        Some(h) => match engine.read().suggestions(&Scope::View(view.into()), &h) {
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
        },
        None => v["suggestions"] = serde_json::json!([]),
    }
    if let Some(h) = change {
        attach_related(&mut v, engine, view, &h);
    }
    Ok(v)
}

/// FW-EN5: when the change creates decisions, `related_decisions` lists the decisions related
/// to them (with `related_decisions_omitted` when a cap dropped some), or `[]` and
/// `related_decisions_error` when they could not be read; the write stands. A change that
/// creates no decision adds no key.
fn attach_related(v: &mut serde_json::Value, engine: &dyn Engine, view: &str, change: &Hash) {
    let st = engine.read();
    let ids: Result<Vec<BlockId>, RdyError> = st
        .reader()
        .change(change)
        .map_err(RdyError::from)
        .map(|ch| {
            ch.ops
                .iter()
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

/// iwr1: other sessions' open intents on the blocks change `h` wrote, in `view`, with each block's
/// areas taken after the write and, when given, `before` it; `None` when they cannot be read (the
/// write stands and the reply has no `intents`).
fn intent_notes_of(
    st: &ReadState,
    before: Option<&ReadState>,
    view: &str,
    h: &Hash,
) -> Option<Vec<IntentNote>> {
    if st.coord_heads().open_intents.is_empty() {
        return Some(vec![]);
    }
    let change = st.reader().change(h).ok()?;
    let touched: BTreeSet<BlockId> = change.ops.iter().map(|o| o.block_id().clone()).collect();
    let prov = &change.provenance;
    let scope = Scope::View(view.into());
    let (actor, run, now) = (&prov.actor, prov.run.as_deref(), now_ms());
    match before {
        Some(b) => st.intent_notes_since(b, &scope, actor, run, &touched, now),
        None => st.intent_notes(&scope, actor, run, &touched, now),
    }
    .ok()
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
    Ok(v)
}

/// `write_typed` for a proposal (`f` returns its `ProposalInfo`), then the consequences of
/// landing it on canon (see `ReadState::landing_consequences`): `consequences` is always present
/// in the reply, with `consequences_omitted` when a cut dropped some, or `consequences_error`
/// when the version does not merge onto canon (the proposal stands).
pub fn propose_and_report(
    engine: &dyn Engine,
    max_tokens: Option<usize>,
    f: impl FnOnce(&mut Rdy) -> Result<ProposalInfo, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    let mut v = write_typed(engine, f)?;
    report_landing(engine, &mut v, "id", max_tokens);
    Ok(v)
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
