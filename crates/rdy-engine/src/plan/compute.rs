//! The complete items of a change plan (D-4JMPH, tb-2ZED7) and the closes the version itself
//! makes: one pure read of a version's selected changes on its canon baseline. Nothing here
//! writes, reads a clock or cuts the set to a budget.
//!
//! The document items come from the consequence path's own candidate visitor and judges
//! (`ReadState::plan_pairs`), over the net state (D-NJ2SP): intermediate edits leave nothing.
//! The code items come from the code index at the HEADs the caller captured (D-ERKCP): one per
//! cause and anchored file, closed by a later commit, where "later" is Git reachability (O5).
use crate::brief::FollowUp;
use crate::code_evidence::{self, anchored_files, later_touch};
use crate::code_index::{CodeRepo, code_repos};
use crate::consequence::{CONFLICT_FIX, NetState, PlanPair, conflict_message, refines_copied};
use crate::error::{E_CONFLICT_UNRESOLVED, E_LIMIT};
use crate::gate::{applied_ops, changes_of, prospective};
use crate::lexicon::Lexicon;
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{DerivedStatus, Graph, edges_of};
use rdy_model::decision::{DECISION, REFINES, REVOKES, SUPERSEDES};
use rdy_model::engine_fields::{REAFFIRMED, reaffirmed_causes};
use rdy_model::plan::{MAX_SEALED_ITEMS, PlanAct, PlanHead, PlanKind, PlanOutcome, item_hash};
use rdy_model::{Block, BlockId, Change, FieldValue, Hash, Tracking};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most items one plan holds: every computed item is sealed, so the seal's bound.
pub const MAX_PLAN_ITEMS: usize = MAX_SEALED_ITEMS;

/// The domain prefix of [`deleted_identity`].
pub const DELETED_DOMAIN: &[u8] = b"rdy-plan-deleted\0";

/// The edge of every code item.
const CODE_EDGE: &str = "implements";

/// The fix of [`limit_error`].
const LIMIT_FIX: &str = "split the change into smaller proposals";

/// One computed plan item. `subject` is a block id, or `<configured repo>:<relative path>` for
/// a code item. The hashes are the content hashes an answer keeps (O4): a deleted block's is
/// [`deleted_identity`], and a code subject has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanCandidate {
    /// [`item_hash`] of the cause, the subject and the kind.
    pub id: Hash,
    pub kind: PlanKind,
    pub subject: String,
    pub cause: BlockId,
    pub edge: String,
    pub subject_hash: Option<Hash>,
    pub cause_hash: Hash,
    /// The close the version itself makes (checked before any answer), or `None` while open.
    pub close: Option<PlanOutcome>,
}

/// Every item of a version, sorted by full item hash, with the distinct code files they name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComputedPlan {
    pub items: Vec<PlanCandidate>,
    /// The distinct (repo, path) pairs of the code items.
    pub code_files: usize,
    /// No linked code repo was captured: the plan has no code item.
    pub code_unavailable: bool,
    /// The decisions a landing of the version names in its seal (D-4JMPH), sorted: the ones it
    /// creates, the targets it supersedes (whole or in part), revokes or refines, and the ones
    /// it re-affirms. Not every governing decision.
    pub decisions: Vec<BlockId>,
    /// The version re-affirms a decision: its `reaffirmed` metadata changes, or it closes a
    /// re-affirm item as re-affirmed. A landing that does seals, also with no item.
    pub reaffirms: bool,
}

/// The acts that may answer an item of `kind` (tb-2ZED7); a computed close needs none.
pub fn allowed_acts(kind: PlanKind) -> &'static [PlanAct] {
    match kind {
        PlanKind::Update | PlanKind::Rejustify | PlanKind::Reaffirm => &[PlanAct::Defer],
        PlanKind::Confirm => &[PlanAct::Confirm, PlanAct::Defer],
        PlanKind::UpdateImplementation => &[PlanAct::Done, PlanAct::Defer],
    }
}

/// The identity of a deleted block (O4): BLAKE3 over [`DELETED_DOMAIN`] and its id. An identity,
/// never a stored object.
pub fn deleted_identity(id: &BlockId) -> Hash {
    let mut bytes = DELETED_DOMAIN.to_vec();
    bytes.extend_from_slice(id.as_str().as_bytes());
    Hash::of(&bytes)
}

/// The refusal of a plan past `n` items: the whole plan, never a cut one.
pub(crate) fn limit_error(n: usize) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("change plan exceeds {n} plan items"),
        LIMIT_FIX,
    )
}

/// The two states a plan compares: the canon baseline, the version's final state on it, and the
/// blocks the version's applied ops write.
pub(crate) struct Sides {
    pub(crate) base: NetState,
    pub(crate) fin: NetState,
    pub(crate) written: BTreeSet<BlockId>,
}

/// One linked checkout: the version's HEAD and the chain's first HEAD, resolved (`None` is an
/// unborn HEAD).
pub(crate) struct CodeSide {
    pub(crate) repo: CodeRepo,
    pub(crate) head: Option<String>,
    pub(crate) first: Option<String>,
}

/// The complete plan items of `changes` (the selected changes of a version of `view`) on canon
/// as of landing `canon_at` (`None`: the empty state), with code evidence at `heads` against the
/// chain's `first_heads`. A repo `first_heads` does not name uses its own HEAD as both ends. A
/// pure read: no plan write and no clock; the caller captures the HEADs first. A version that
/// does not apply on its baseline is the merge's refusal; past [`MAX_PLAN_ITEMS`] items,
/// [`limit_error`].
pub(crate) fn compute(
    st: &ReadState,
    _view: &str,
    changes: &[Hash],
    canon_at: Option<Hash>,
    heads: &[PlanHead],
    first_heads: &[PlanHead],
) -> Result<ComputedPlan, RdyError> {
    let changes = changes_of(st, changes)?;
    compute_changes(st, &changes, canon_at, heads, first_heads)
}

/// [`compute`] of change values, read or not yet written: the propose binding computes the
/// candidate with the key change rdy adds before anything is appended.
pub(crate) fn compute_changes(
    st: &ReadState,
    changes: &[Change],
    canon_at: Option<Hash>,
    heads: &[PlanHead],
    first_heads: &[PlanHead],
) -> Result<ComputedPlan, RdyError> {
    let sides = sides_of(st, changes, canon_at)?;
    let code = code_sides(st, heads, first_heads)?;
    compute_in(st, &sides, code.as_deref(), MAX_PLAN_ITEMS)
}

/// The baseline of `canon_at` and `changes` applied on it. On canon's head the gate's own
/// prospective state; on an earlier landing the same merge, onto that landing's state.
fn sides_of(st: &ReadState, changes: &[Change], canon_at: Option<Hash>) -> Result<Sides, RdyError> {
    let base = match canon_at {
        None => {
            let m = Arc::new(Materialized::default());
            NetState {
                g: Arc::new(Graph::build_with(
                    m.iter(),
                    Arc::clone(st.reader().tracking()),
                )),
                names: Arc::new(Lexicon::build(st.pack(), m.iter())),
                m,
            }
        }
        Some(h) => {
            let scope = if Some(h) == st.canon_head() {
                Scope::Canon
            } else {
                Scope::At(h)
            };
            NetState {
                m: st.materialized(&scope)?,
                g: st.graph(&scope)?,
                names: st.lexicon(&scope)?,
            }
        }
    };
    let (m, ops) = if canon_at == st.canon_head() {
        let (next, applied) = prospective(st, changes, None)?;
        (next, applied_ops(changes, &applied))
    } else {
        let reader = st.reader();
        let merged = reader.merge_onto(canon_at, &reader.state_of(canon_at)?, changes)?;
        if !merged.conflicts.is_empty() {
            let ids: Vec<String> = merged
                .conflicts
                .iter()
                .map(|c| c.block.to_string())
                .collect();
            return Err(RdyError::new(
                E_CONFLICT_UNRESOLVED,
                conflict_message(&ids),
                CONFLICT_FIX,
            ));
        }
        let applied: Vec<usize> = merged
            .order
            .iter()
            .filter_map(|h| changes.iter().position(|c| c.id() == *h))
            .collect();
        let ops = applied_ops(changes, &applied);
        (base.m.with(&ops), ops)
    };
    let written: BTreeSet<BlockId> = ops.iter().map(|o| o.block_id().clone()).collect();
    let old = || written.iter().filter_map(|id| base.m.get(id));
    let new = || written.iter().filter_map(|id| m.get(id));
    let (g, names) = (
        base.g.with_changes(old(), new()),
        base.names.with_changes(old(), new()),
    );
    let fin = NetState {
        m: Arc::new(m),
        g: Arc::new(g),
        names: Arc::new(names),
    };
    Ok(Sides { base, fin, written })
}

/// The linked checkouts `heads` names, each with its first HEAD; `None` when none is linked.
fn code_sides(
    st: &ReadState,
    heads: &[PlanHead],
    first_heads: &[PlanHead],
) -> Result<Option<Vec<CodeSide>>, RdyError> {
    if heads.is_empty() {
        return Ok(None);
    }
    let repos = code_repos(st.root())?;
    let sides: Vec<CodeSide> = heads
        .iter()
        .filter_map(|h| {
            let repo = repos.iter().find(|r| r.configured == h.repo)?;
            let first = match first_heads.iter().find(|f| f.repo == h.repo) {
                Some(f) => f.head.clone(),
                None => h.head.clone(),
            };
            Some(CodeSide {
                repo: repo.clone(),
                head: h.head.clone(),
                first,
            })
        })
        .collect();
    Ok((!sides.is_empty()).then_some(sides))
}

/// What the version's decisions do (their final versions among the changed blocks).
#[derive(Default)]
struct Lineage {
    /// The targets fully superseded or revoked.
    retired: BTreeSet<BlockId>,
    /// The targets superseded in part.
    partial: BTreeSet<BlockId>,
    /// The targets refined (not by refines a full supersede copied).
    refined: BTreeSet<BlockId>,
    /// The decisions that supersede, revoke or refine.
    deciders: BTreeSet<BlockId>,
    /// (decision, target, link) for each refine and part supersede, sorted (FW-LP6).
    reach: Vec<(BlockId, BlockId, &'static str)>,
}

impl Lineage {
    fn of(fin: &NetState, touched: &BTreeSet<BlockId>) -> Lineage {
        let mut out = Lineage::default();
        for b in touched.iter().filter_map(|id| fin.m.get(id)) {
            if b.block_type != DECISION {
                continue;
            }
            let links = |name: &str| b.links.get(name).into_iter().flatten();
            for l in links(SUPERSEDES) {
                out.deciders.insert(b.id.clone());
                match l.part {
                    None => {
                        out.retired.insert(l.target.clone());
                    }
                    Some(_) => {
                        out.partial.insert(l.target.clone());
                        out.reach.push((b.id.clone(), l.target.clone(), SUPERSEDES));
                    }
                }
            }
            for l in links(REVOKES) {
                out.deciders.insert(b.id.clone());
                out.retired.insert(l.target.clone());
            }
            if !refines_copied(&fin.g, &b.id) {
                for l in links(REFINES) {
                    out.deciders.insert(b.id.clone());
                    out.refined.insert(l.target.clone());
                    out.reach.push((b.id.clone(), l.target.clone(), REFINES));
                }
            }
        }
        out.reach.sort();
        out
    }
}

/// `b`'s content as a stale-code cause reads it: its content hash under `t` with its place
/// (parent and order) left out, so a move alone changes nothing (p-E9MA3).
fn code_content(b: &Block, t: &Tracking) -> Hash {
    let mut c = b.clone();
    c.parent = None;
    c.order = String::new();
    c.content_hash_with(t)
}

/// `b`'s `reaffirmed` value for one target, as stored (`None` without one).
fn reaffirmed_for<'b>(b: &'b Block, target: &BlockId) -> Option<&'b FieldValue> {
    match b.fields.get(REAFFIRMED)? {
        FieldValue::Map(m) => m.get(target.as_str()),
        _ => None,
    }
}

/// Adds `c` to `items` once per item id: a repeat keeps the least edge name, and is closed only
/// when both are (a reach and an edge to the same cause agree). A new item past `limit` is the
/// whole plan's refusal.
fn admit(
    items: &mut BTreeMap<Hash, PlanCandidate>,
    c: PlanCandidate,
    limit: usize,
) -> Result<(), RdyError> {
    if let Some(had) = items.get_mut(&c.id) {
        if c.edge < had.edge {
            had.edge = c.edge;
        }
        had.close = had.close.and(c.close);
        return Ok(());
    }
    if items.len() >= limit {
        return Err(limit_error(limit));
    }
    items.insert(c.id, c);
    Ok(())
}

/// What the closes of one plan read: both states, the changed blocks and, built on first use,
/// the final graph as the landing leaves it (the version's proposed decisions accepted), which
/// decides a retirement and a re-affirmation against one.
struct Closes<'a> {
    sides: &'a Sides,
    t: Arc<Tracking>,
    touched: &'a BTreeSet<BlockId>,
    landed: OnceCell<Graph>,
}

impl Closes<'_> {
    fn hash(&self, b: &Block) -> Hash {
        b.content_hash_with(&self.t)
    }

    /// The final graph with each proposed decision the version changes accepted, as its landing
    /// accepts it.
    fn landed(&self) -> &Graph {
        self.landed.get_or_init(|| {
            let fin = &self.sides.fin;
            let proposed: Vec<&Block> = self
                .touched
                .iter()
                .filter_map(|id| fin.m.get(id))
                .filter(|b| b.block_type == DECISION && b.field_str("status") == Some("proposed"))
                .collect();
            let accepted: Vec<Block> = proposed
                .iter()
                .map(|b| {
                    let mut a = (*b).clone();
                    a.fields
                        .insert("status".into(), FieldValue::Str("accepted".into()));
                    a
                })
                .collect();
            fin.g.with_changes(proposed, accepted.iter())
        })
    }

    /// The close of an item of `kind` on pair `p`, before any answer (tb-2ZED7). A subject the
    /// version deletes closes first, as updated, whatever the kind: deleting it is the
    /// version's own change to it, so it needs no answer (D-4JMPH, O4).
    fn close(&self, kind: PlanKind, p: &PlanPair) -> Option<PlanOutcome> {
        let before = self.sides.base.m.get(&p.subject);
        let Some(after) = self.sides.fin.m.get(&p.subject) else {
            return Some(PlanOutcome::Updated);
        };
        match kind {
            // the subject's content changed, or it is new
            PlanKind::Update => match before {
                None => Some(PlanOutcome::Updated),
                Some(a) => (self.hash(a) != self.hash(after)).then_some(PlanOutcome::Updated),
            },
            // no link and no reference to the retired cause is left
            PlanKind::Rejustify => {
                (!edges_of(after).iter().any(|e| e.to == p.cause)).then_some(PlanOutcome::Relinked)
            }
            // a retirement of the subject wins; else its metadata or pin cleared the cause
            PlanKind::Reaffirm => {
                let landed = self.landed();
                if matches!(
                    landed.status(&p.subject),
                    Some(DerivedStatus::Superseded { .. } | DerivedStatus::Revoked { .. })
                ) {
                    return Some(PlanOutcome::Retired);
                }
                let moved = before.map(|b| b.pins.get(&p.cause)) != Some(after.pins.get(&p.cause))
                    || before.and_then(|b| reaffirmed_for(b, &p.cause))
                        != reaffirmed_for(after, &p.cause);
                let cleared = !landed
                    .suspects_of(&p.subject)
                    .iter()
                    .any(|s| s.target == p.cause);
                (moved && cleared).then_some(PlanOutcome::Reaffirmed)
            }
            // a suspect on the cause that a repin alone cleared: never for a reach or a
            // neighbour, which has no suspect, nor for a rewrite
            PlanKind::Confirm => {
                let before = before?;
                let repinned = !p.reach
                    && p.prior_suspect
                    && !p.suspect
                    && self.hash(before) == self.hash(after)
                    && before.pins.get(&p.cause) != after.pins.get(&p.cause);
                repinned.then_some(PlanOutcome::Repinned)
            }
            PlanKind::UpdateImplementation => None,
        }
    }
}

/// The kind of a judged pair: its follow-up, and a confirm on a retired cause is a rejustify. An
/// accepted decision's confirm (D-ATBP6: reached with no suspect on the cause) is a confirm item,
/// as a frozen decision cannot relink; a `reaffirm` comes only from a suspect, so the reaffirm
/// op closes every reaffirm item.
fn kind_of(p: &PlanPair) -> Option<PlanKind> {
    Some(match p.follow {
        FollowUp::Update => PlanKind::Update,
        FollowUp::Reaffirm => PlanKind::Reaffirm,
        FollowUp::Confirm if p.retired && !p.reached_decision => PlanKind::Rejustify,
        FollowUp::Confirm => PlanKind::Confirm,
        FollowUp::Resolve => return None,
    })
}

/// [`compute`] on states already read: the plan of `sides` with code evidence from `code`
/// (`None`: no linked repo), refused past `limit` items.
pub(crate) fn compute_in(
    st: &ReadState,
    sides: &Sides,
    code: Option<&[CodeSide]>,
    limit: usize,
) -> Result<ComputedPlan, RdyError> {
    let (base, fin) = (&sides.base, &sides.fin);
    let t = Arc::clone(fin.g.tracking());
    // D-NJ2SP: the net change, by content (pins, derived fields and engine metadata excluded)
    let touched: BTreeSet<BlockId> = sides
        .written
        .iter()
        .filter(|id| match (base.m.get(id), fin.m.get(id)) {
            (None, None) => false,
            (Some(a), Some(b)) => a.content_hash_with(&t) != b.content_hash_with(&t),
            _ => true,
        })
        .cloned()
        .collect();
    let lineage = Lineage::of(fin, &touched);
    let closes = Closes {
        sides,
        t: Arc::clone(&t),
        touched: &touched,
        landed: OnceCell::new(),
    };
    let hash_or_deleted = |id: &BlockId| {
        fin.m
            .get(id)
            .map(|b| b.content_hash_with(&t))
            .unwrap_or_else(|| deleted_identity(id))
    };
    let mut items: BTreeMap<Hash, PlanCandidate> = BTreeMap::new();
    // the decisions whose re-affirm item the version closes as re-affirmed: they re-affirm, for
    // the code closes as for the document
    let mut reaffirmed: BTreeSet<BlockId> = BTreeSet::new();
    // each pair is filtered, deduplicated and admitted as it is judged, so the first one past
    // the bound stops the walk
    st.plan_pairs(base, fin, &touched, &lineage.reach, |p| {
        let Some(kind) = kind_of(&p) else {
            return Ok(());
        };
        // a link the version wrote to a block it changed, with no suspect: written against the
        // final state, it owes nothing (D-NJ2SP). A reach is no such link: it always needs a
        // reasoned confirm (p-0HXT0)
        if kind == PlanKind::Confirm && !p.reach && !p.in_base && !p.suspect {
            return Ok(());
        }
        let close = closes.close(kind, &p);
        if kind == PlanKind::Reaffirm && close == Some(PlanOutcome::Reaffirmed) {
            reaffirmed.insert(p.subject.clone());
        }
        let subject = p.subject.to_string();
        admit(
            &mut items,
            PlanCandidate {
                id: item_hash(&p.cause, &subject, kind),
                kind,
                close,
                subject_hash: Some(hash_or_deleted(&p.subject)),
                cause_hash: hash_or_deleted(&p.cause),
                subject,
                cause: p.cause,
                edge: p.edge,
            },
            limit,
        )
    })?;
    let (code_files, code_unavailable) = match code {
        None => (0, true),
        Some(code) => (
            code_items(
                sides,
                &t,
                &touched,
                &lineage,
                &reaffirmed,
                code,
                &mut items,
                limit,
            )?,
            false,
        ),
    };
    let acknowledged = acknowledged(sides);
    let decisions: BTreeSet<BlockId> = created_decisions(sides)
        .chain(lineage.retired.iter().cloned())
        .chain(lineage.partial.iter().cloned())
        .chain(lineage.refined.iter().cloned())
        .chain(acknowledged.iter().cloned())
        .chain(reaffirmed.iter().cloned())
        .collect();
    Ok(ComputedPlan {
        items: items.into_values().collect(),
        code_files,
        code_unavailable,
        decisions: decisions.into_iter().collect(),
        reaffirms: !acknowledged.is_empty() || !reaffirmed.is_empty(),
    })
}

/// The decisions the version creates: written, absent from the baseline, a decision after.
fn created_decisions(sides: &Sides) -> impl Iterator<Item = BlockId> + '_ {
    let (base, fin) = (&sides.base, &sides.fin);
    sides
        .written
        .iter()
        .filter(|id| {
            base.m.get(id).is_none() && fin.m.get(id).is_some_and(|b| b.block_type == DECISION)
        })
        .cloned()
}

/// The decisions the version re-affirms by metadata: their `reaffirmed` field changes, or the
/// pin of a cause it records moves (a repeated re-affirmation with the same reason writes the
/// same entry and moves only the pin; [`reaffirmed_causes`], the queue's rule too).
fn acknowledged(sides: &Sides) -> BTreeSet<BlockId> {
    let (base, fin) = (&sides.base, &sides.fin);
    sides
        .written
        .iter()
        .filter(|id| match (base.m.get(id), fin.m.get(id)) {
            (Some(a), Some(b)) => {
                b.block_type == DECISION
                    && (a.fields.get(REAFFIRMED) != b.fields.get(REAFFIRMED)
                        || !reaffirmed_causes(Some(a), b).is_empty())
            }
            _ => false,
        })
        .cloned()
        .collect()
}

/// D-ERKCP: one `update_implementation` item per stale-code cause and file anchored to it at
/// each checkout's HEAD; the distinct (repo, path) pairs they name. A cause is a baseline block
/// whose content (place left out) the version changes or that it deletes, and each decision it
/// supersedes (whole or in part) or revokes. An item closes on a commit after the first HEAD
/// that touched the file and cites the cause, or a decision the version creates, refines,
/// supersedes or revokes with, or re-affirms. A decision re-affirms when its `reaffirmed`
/// metadata changes, or when the version closes one of its re-affirm items as re-affirmed
/// (`reaffirmed`: a repeated re-affirmation moves only the pin).
#[allow(clippy::too_many_arguments)]
fn code_items(
    sides: &Sides,
    t: &Tracking,
    touched: &BTreeSet<BlockId>,
    lineage: &Lineage,
    reaffirmed: &BTreeSet<BlockId>,
    code: &[CodeSide],
    items: &mut BTreeMap<Hash, PlanCandidate>,
    limit: usize,
) -> Result<usize, RdyError> {
    let (base, fin) = (&sides.base, &sides.fin);
    let mut causes: BTreeSet<BlockId> = touched
        .iter()
        .filter(|id| match (base.m.get(id), fin.m.get(id)) {
            (Some(a), Some(b)) => code_content(a, t) != code_content(b, t),
            (Some(_), None) => true,
            (None, _) => false,
        })
        .cloned()
        .collect();
    causes.extend(
        lineage
            .retired
            .iter()
            .chain(&lineage.partial)
            .filter(|id| base.m.get(id).is_some())
            .cloned(),
    );
    let version_decisions: BTreeSet<BlockId> = created_decisions(sides)
        .chain(lineage.refined.iter().cloned())
        .chain(lineage.deciders.iter().cloned())
        .chain(acknowledged(sides))
        .chain(reaffirmed.iter().cloned())
        .collect();
    let cause_hash = |id: &BlockId| {
        fin.m
            .get(id)
            .map(|b| b.content_hash_with(t))
            .unwrap_or_else(|| deleted_identity(id))
    };
    let mut files: BTreeSet<(String, String)> = BTreeSet::new();
    for side in code {
        let evidence = code_evidence::at(&side.repo, side.head.as_deref())?;
        for cause in &causes {
            let only = BTreeSet::from([cause.clone()]);
            for path in anchored_files(&base.m, cause, &evidence) {
                // the cause read as it stood (a deleted one is gone after), the version's
                // decisions as they stand after
                let closed = later_touch(&evidence, side.first.as_deref(), &path, &only, &base.m)?
                    || later_touch(
                        &evidence,
                        side.first.as_deref(),
                        &path,
                        &version_decisions,
                        &fin.m,
                    )?;
                let subject = format!("{}:{}", evidence.repo, path);
                files.insert((evidence.repo.clone(), path));
                admit(
                    items,
                    PlanCandidate {
                        id: item_hash(cause, &subject, PlanKind::UpdateImplementation),
                        kind: PlanKind::UpdateImplementation,
                        subject,
                        cause: cause.clone(),
                        edge: CODE_EDGE.to_string(),
                        subject_hash: None,
                        cause_hash: cause_hash(cause),
                        close: closed.then_some(PlanOutcome::Committed),
                    },
                    limit,
                )?;
            }
        }
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consequence::REAFFIRM_FIX;
    use crate::{E_LIMIT, Rdy, Session, WriteRequest, WriteResult, parse_ops};
    use rdy_model::engine_fields::{self, Against, Reaffirmation};
    use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome, item_hash};
    use rdy_model::{FieldValue, Link, Op, Role, Tracking};
    use serde_json::{Value, json};
    use std::path::Path;
    use std::process::Command;

    const DOC: &str = "d-AAAAA";
    const SEC: &str = "s-AAAAA";
    const A: &str = "c-AAAAA";
    const B: &str = "c-BBBBB";
    const C: &str = "c-CCCCC";
    const P: &str = "c-PPPPP";
    const Q: &str = "c-QQQQQ";
    const R: &str = "c-RRRRR";
    const G: &str = "c-GGGGG";
    const N: &str = "c-NNNNN";
    const D1: &str = "D-11111";
    const D2: &str = "D-22222";
    const D3: &str = "D-33333";
    const S: &str = "D-SSSSS";
    const S2: &str = "D-SSSS2";
    const T: &str = "D-TTTTT";
    const T2: &str = "D-TTTT2";

    fn id(s: &str) -> BlockId {
        BlockId::parse(s).unwrap()
    }

    fn block(i: &str, ty: &str, parent: Option<&str>, body: &str) -> Block {
        Block {
            id: id(i),
            block_type: ty.to_string(),
            parent: parent.map(id),
            order: "a0".to_string(),
            fields: BTreeMap::new(),
            body: body.to_string(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: None,
        }
    }

    fn field(mut b: Block, name: &str, v: FieldValue) -> Block {
        b.fields.insert(name.to_string(), v);
        b
    }

    fn text(b: Block, name: &str, v: &str) -> Block {
        field(b, name, FieldValue::Str(v.to_string()))
    }

    /// The doc and the section every fixture block sits in.
    fn frame() -> Vec<Block> {
        vec![
            text(
                text(block(DOC, "doc", None, ""), "title", "Spec"),
                "doc_type",
                "overview",
            ),
            text(block(SEC, "section", Some(DOC), ""), "title", "Rules"),
        ]
    }

    fn claim(i: &str, body: &str) -> Block {
        block(i, "claim", Some(SEC), body)
    }

    fn decision(i: &str, statement: &str, status: &str) -> Block {
        text(
            text(block(i, "decision", Some(SEC), ""), "statement", statement),
            "status",
            status,
        )
    }

    fn link(mut b: Block, name: &str, target: &str, part: Option<&str>) -> Block {
        b.links.entry(name.to_string()).or_default().push(Link {
            target: id(target),
            part: part.map(str::to_string),
        });
        b
    }

    fn unlink(mut b: Block, name: &str) -> Block {
        b.links.remove(name);
        b
    }

    /// `b` pinned to `target`'s content (no tracking fields).
    fn pin(mut b: Block, target: &Block) -> Block {
        b.pins.insert(
            target.id.clone(),
            target.content_hash_with(&Tracking::default()),
        );
        b
    }

    fn reaffirmed(b: Block, target: &str, against: Against) -> Block {
        let mut m = BTreeMap::new();
        m.insert(
            id(target),
            Reaffirmation {
                against,
                reason: "it still holds".to_string(),
            },
        );
        field(
            b,
            engine_fields::REAFFIRMED,
            engine_fields::encode_reaffirmed(&m),
        )
    }

    fn project() -> (tempfile::TempDir, Rdy) {
        let dir = tempfile::tempdir().unwrap();
        let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        (dir, r)
    }

    fn state(st: &ReadState, t: &Arc<Tracking>, blocks: &[Block]) -> NetState {
        let ops: Vec<Op> = blocks
            .iter()
            .cloned()
            .map(|block| Op::Create { block })
            .collect();
        let m = Materialized::default().with(&ops);
        NetState {
            g: Arc::new(Graph::build_with(m.iter(), Arc::clone(t))),
            names: Arc::new(Lexicon::build(st.pack(), m.iter())),
            m: Arc::new(m),
        }
    }

    /// The two states of a hand-built version: `base` and `fin` hold the frame too; the written
    /// blocks are those whose version differs.
    fn sides_with(st: &ReadState, t: Tracking, base: &[Block], fin: &[Block]) -> Sides {
        let t = Arc::new(t);
        let (mut b, mut f) = (frame(), frame());
        b.extend(base.iter().cloned());
        f.extend(fin.iter().cloned());
        let (base, fin) = (state(st, &t, &b), state(st, &t, &f));
        let written = base
            .m
            .iter()
            .chain(fin.m.iter())
            .map(|x| x.id.clone())
            .filter(|i| base.m.version_of(i) != fin.m.version_of(i))
            .collect();
        Sides { base, fin, written }
    }

    fn sides(st: &ReadState, base: &[Block], fin: &[Block]) -> Sides {
        sides_with(st, Tracking::default(), base, fin)
    }

    fn plan(st: &ReadState, base: &[Block], fin: &[Block]) -> ComputedPlan {
        compute_in(st, &sides(st, base, fin), None, MAX_PLAN_ITEMS).unwrap()
    }

    /// The items as (cause, subject, kind, close), in item order.
    fn rows(p: &ComputedPlan) -> Vec<(String, String, PlanKind, Option<PlanOutcome>)> {
        p.items
            .iter()
            .map(|i| (i.cause.to_string(), i.subject.clone(), i.kind, i.close))
            .collect()
    }

    fn find<'a>(
        p: &'a ComputedPlan,
        cause: &str,
        subject: &str,
        kind: PlanKind,
    ) -> &'a PlanCandidate {
        p.items
            .iter()
            .find(|i| i.cause.as_str() == cause && i.subject == subject && i.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} item ({cause}, {subject}): {:#?}", rows(p)))
    }

    fn close_of(
        p: &ComputedPlan,
        cause: &str,
        subject: &str,
        kind: PlanKind,
    ) -> Option<PlanOutcome> {
        find(p, cause, subject, kind).close
    }

    fn doc_items(p: &ComputedPlan) -> Vec<(String, String, PlanKind, Option<PlanOutcome>)> {
        rows(p)
            .into_iter()
            .filter(|r| r.2 != PlanKind::UpdateImplementation)
            .collect()
    }

    // ---- git ----

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=qa",
                "-c",
                "user.email=qa",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A fresh checkout at `dir`.
    fn git_init(dir: &Path) {
        git(dir, &["init", "-q", "--initial-branch=main"]);
    }

    /// Writes `path` (its content names the message, so each commit changes it), commits with
    /// `message`, and returns the commit.
    fn commit(dir: &Path, path: &str, message: &str) -> String {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(dir.join(parent)).unwrap();
        }
        std::fs::write(dir.join(path), format!("{message}\n")).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--no-verify", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn repo_at(dir: &Path) -> CodeRepo {
        CodeRepo {
            configured: "app".to_string(),
            root: std::fs::canonicalize(dir).unwrap(),
        }
    }

    fn code_side(dir: &Path, head: &str, first: Option<&str>) -> CodeSide {
        CodeSide {
            repo: repo_at(dir),
            head: Some(head.to_string()),
            first: first.map(str::to_string),
        }
    }

    // ---- real projects ----

    fn writer() -> Session {
        Session::human("human:qa".into(), Role::Writer).unwrap()
    }

    fn approver() -> Session {
        Session::human("human:qa".into(), Role::Approver).unwrap()
    }

    fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
        r.write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
    }

    fn land(r: &mut Rdy, view: &str) {
        let p = r.propose(&writer(), view, view, None).unwrap();
        r.land(&approver(), &p.id).unwrap();
    }

    fn change(w: &WriteResult) -> Hash {
        w.change.parse().unwrap()
    }

    /// Canon: doc `spec`, section `Rules` and claims a ("A holds 30 days."), c ("C stands.")
    /// and b, which references both (pinned to each as created). Returns (a, b, c, section).
    fn landed_spec(r: &mut Rdy) -> (BlockId, BlockId, BlockId, BlockId) {
        let w = put(
            r,
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
                {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C stands."},
                {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}} and {{$c}}."}
            ]),
        );
        land(r, "seed");
        let get = |k: &str| id(&w.created[k]);
        (get("$a"), get("$b"), get("$c"), get("$s"))
    }

    fn update(r: &mut Rdy, view: &str, target: &BlockId, body: &str) -> Hash {
        change(&put(
            r,
            view,
            json!([{"op": "update", "id": target.to_string(), "body": body}]),
        ))
    }

    // ---- tests ----

    /// tb-2ZED7: each kind's computed close, checked before any answer, and the acts that may
    /// answer it.
    #[test]
    fn cp6_each_kind_has_its_exact_close_and_allowed_acts() {
        assert_eq!(allowed_acts(PlanKind::Update), [PlanAct::Defer]);
        assert_eq!(allowed_acts(PlanKind::Rejustify), [PlanAct::Defer]);
        assert_eq!(allowed_acts(PlanKind::Reaffirm), [PlanAct::Defer]);
        assert_eq!(
            allowed_acts(PlanKind::Confirm),
            [PlanAct::Confirm, PlanAct::Defer]
        );
        assert_eq!(
            allowed_acts(PlanKind::UpdateImplementation),
            [PlanAct::Done, PlanAct::Defer]
        );
        let (_d, r) = project();
        let st = r.read();

        // update: the subject states a removed literal; a content change or a delete closes it
        let a = claim(A, "Wait 30 days.");
        let a2 = claim(A, "Wait 60 days.");
        let p = pin(claim(P, "Wait 30 days per {{c-AAAAA}}."), &a);
        let base = [a.clone(), p.clone()];
        let open = plan(&st, &base, &[a2.clone(), p.clone()]);
        assert_eq!(close_of(&open, A, P, PlanKind::Update), None);
        let item = find(&open, A, P, PlanKind::Update);
        assert_eq!(item.edge, "references");
        assert_eq!(item.id, item_hash(&id(A), P, PlanKind::Update));
        assert_eq!(item.subject_hash, Some(p.content_hash()));
        assert_eq!(item.cause_hash, a2.content_hash());
        let p2 = pin(claim(P, "Wait 60 days per {{c-AAAAA}}."), &a2);
        let done = plan(&st, &base, &[a2.clone(), p2]);
        assert_eq!(
            close_of(&done, A, P, PlanKind::Update),
            Some(PlanOutcome::Updated)
        );
        let gone = plan(&st, &base, std::slice::from_ref(&a2));
        assert_eq!(
            close_of(&gone, A, P, PlanKind::Update),
            Some(PlanOutcome::Updated)
        );
        assert_eq!(
            find(&gone, A, P, PlanKind::Update).subject_hash,
            Some(deleted_identity(&id(P)))
        );

        // rejustify: a confirm on a fully superseded cause; removing every link and reference
        // to it, or deleting the subject, closes it
        let d1 = decision(D1, "Keep 30 days", "accepted");
        let d2 = link(
            decision(D2, "Keep 60 days", "proposed"),
            "supersedes",
            D1,
            None,
        );
        let rq = pin(
            link(claim(R, "Teams follow it."), "decided_by", D1, None),
            &d1,
        );
        let base = [d1.clone(), rq.clone()];
        let open = plan(&st, &base, &[d1.clone(), d2.clone(), rq.clone()]);
        assert_eq!(close_of(&open, D1, R, PlanKind::Rejustify), None);
        assert_eq!(find(&open, D1, R, PlanKind::Rejustify).edge, "decided_by");
        let relinked = link(unlink(rq.clone(), "decided_by"), "decided_by", D2, None);
        let done = plan(&st, &base, &[d1.clone(), d2.clone(), relinked]);
        assert_eq!(
            close_of(&done, D1, R, PlanKind::Rejustify),
            Some(PlanOutcome::Relinked)
        );
        // a deleted subject closes first, as updated (task-6 R1; D-4JMPH, O4)
        let gone = plan(&st, &base, &[d1.clone(), d2.clone()]);
        assert_eq!(
            close_of(&gone, D1, R, PlanKind::Rejustify),
            Some(PlanOutcome::Updated)
        );
        // still citing it in the text keeps it open
        let cited = link(
            unlink(claim(R, "Teams follow {{D-11111}}."), "decided_by"),
            "decided_by",
            D2,
            None,
        );
        let still = plan(&st, &base, &[d1.clone(), d2.clone(), cited]);
        assert_eq!(close_of(&still, D1, R, PlanKind::Rejustify), None);

        // reaffirm: a live decision depending on a changed block; its metadata and pin clear
        // it, a retirement of the subject retires it, and the retirement wins
        let a = claim(A, "A holds.");
        let a2 = claim(A, "A holds firmly.");
        let s = pin(
            link(decision(S, "Build on A", "accepted"), "depends_on", A, None),
            &a,
        );
        let base = [a.clone(), s.clone()];
        let open = plan(&st, &base, &[a2.clone(), s.clone()]);
        assert_eq!(close_of(&open, A, S, PlanKind::Reaffirm), None);
        let s_ok = reaffirmed(pin(s.clone(), &a2), A, Against::Retiring(vec![]));
        let done = plan(&st, &base, &[a2.clone(), s_ok.clone()]);
        assert_eq!(
            close_of(&done, A, S, PlanKind::Reaffirm),
            Some(PlanOutcome::Reaffirmed)
        );
        let s2 = link(
            decision(S2, "Build on A, firmly", "proposed"),
            "supersedes",
            S,
            None,
        );
        let retired = plan(&st, &base, &[a2.clone(), s.clone(), s2.clone()]);
        assert_eq!(
            close_of(&retired, A, S, PlanKind::Reaffirm),
            Some(PlanOutcome::Retired)
        );
        let both = plan(&st, &base, &[a2.clone(), s_ok, s2]);
        assert_eq!(
            close_of(&both, A, S, PlanKind::Reaffirm),
            Some(PlanOutcome::Retired),
            "retirement wins over re-affirmation"
        );

        // confirm: its suspect cleared by a repin closes it; a rewrite does not
        let b = pin(claim(B, "B builds on {{c-AAAAA}}."), &a);
        let base = [a.clone(), b.clone()];
        let open = plan(&st, &base, &[a2.clone(), b.clone()]);
        assert_eq!(close_of(&open, A, B, PlanKind::Confirm), None);
        let done = plan(&st, &base, &[a2.clone(), pin(b.clone(), &a2)]);
        assert_eq!(
            close_of(&done, A, B, PlanKind::Confirm),
            Some(PlanOutcome::Repinned)
        );
        let rewritten = pin(claim(B, "B builds on {{c-AAAAA}}, firmly."), &a2);
        let still = plan(&st, &base, &[a2.clone(), rewritten]);
        assert_eq!(
            close_of(&still, A, B, PlanKind::Confirm),
            None,
            "a content rewrite is no repin"
        );

        // update_implementation: a later commit that touched the file and cites the cause
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git_init(dir);
        let first = commit(dir, "f.rs", "start\n\nImplements: c-AAAAA");
        let sides = sides(&st, std::slice::from_ref(&a), std::slice::from_ref(&a2));
        let open = compute_in(
            &st,
            &sides,
            Some(&[code_side(dir, &first, Some(&first))]),
            MAX_PLAN_ITEMS,
        )
        .unwrap();
        assert_eq!(
            close_of(&open, A, "app:f.rs", PlanKind::UpdateImplementation),
            None
        );
        let later = commit(dir, "f.rs", "follow A\n\nImplements: c-AAAAA");
        let done = compute_in(
            &st,
            &sides,
            Some(&[code_side(dir, &later, Some(&first))]),
            MAX_PLAN_ITEMS,
        )
        .unwrap();
        assert_eq!(
            close_of(&done, A, "app:f.rs", PlanKind::UpdateImplementation),
            Some(PlanOutcome::Committed)
        );
        let code = find(&done, A, "app:f.rs", PlanKind::UpdateImplementation);
        assert_eq!(code.edge, "implements");
        assert_eq!(code.subject_hash, None);
        assert_eq!(code.cause_hash, a2.content_hash());
    }

    /// O3: work the version already did stays in the plan, closed: an updated, a deleted, a
    /// relinked, a repinned, a re-affirmed and a retired subject.
    #[test]
    fn cp6_closed_work_stays_in_the_plan() {
        let (_d, r) = project();
        let st = r.read();
        let a = claim(A, "Wait 30 days.");
        let a2 = claim(A, "Wait 60 days.");
        let d1 = decision(D1, "Keep 10 hours", "accepted");
        let base = vec![
            a.clone(),
            pin(claim(P, "Wait 30 days per {{c-AAAAA}}."), &a),
            pin(claim(Q, "Also 30 days per {{c-AAAAA}}."), &a),
            pin(claim(B, "B builds on {{c-AAAAA}}."), &a),
            d1.clone(),
            pin(
                link(claim(R, "Teams follow it."), "decided_by", D1, None),
                &d1,
            ),
            pin(
                link(decision(S, "Build on A", "accepted"), "depends_on", A, None),
                &a,
            ),
            pin(
                link(decision(T, "Hold A", "accepted"), "depends_on", A, None),
                &a,
            ),
        ];
        let fin = vec![
            a2.clone(),
            pin(claim(P, "Wait 60 days per {{c-AAAAA}}."), &a2),
            pin(claim(B, "B builds on {{c-AAAAA}}."), &a2),
            d1.clone(),
            link(
                decision(D2, "Keep 12 hours", "proposed"),
                "supersedes",
                D1,
                None,
            ),
            link(claim(R, "Teams follow it."), "decided_by", D2, None),
            reaffirmed(
                pin(
                    link(decision(S, "Build on A", "accepted"), "depends_on", A, None),
                    &a2,
                ),
                A,
                Against::Retiring(vec![]),
            ),
            pin(
                link(decision(T, "Hold A", "accepted"), "depends_on", A, None),
                &a,
            ),
            link(
                decision(T2, "Hold A, firmly", "proposed"),
                "supersedes",
                T,
                None,
            ),
        ];
        let p = plan(&st, &base, &fin);
        let mut want = vec![
            (
                A.to_string(),
                P.to_string(),
                PlanKind::Update,
                Some(PlanOutcome::Updated),
            ),
            (
                A.to_string(),
                Q.to_string(),
                PlanKind::Update,
                Some(PlanOutcome::Updated),
            ),
            (
                A.to_string(),
                B.to_string(),
                PlanKind::Confirm,
                Some(PlanOutcome::Repinned),
            ),
            (
                D1.to_string(),
                R.to_string(),
                PlanKind::Rejustify,
                Some(PlanOutcome::Relinked),
            ),
            (
                A.to_string(),
                S.to_string(),
                PlanKind::Reaffirm,
                Some(PlanOutcome::Reaffirmed),
            ),
            (
                A.to_string(),
                T.to_string(),
                PlanKind::Reaffirm,
                Some(PlanOutcome::Retired),
            ),
        ];
        // in item order: by full item hash
        want.sort_by_key(|(c, s, k, _)| item_hash(&id(c), s, *k));
        assert_eq!(doc_items(&p), want);
        let ids: Vec<Hash> = p.items.iter().map(|i| i.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    /// p-9ZENG: one subject with two causes has two items, and each id is the same in a later
    /// version of the proposal.
    #[test]
    fn cp6_two_causes_keep_two_stable_item_ids() {
        let (_d, mut r) = project();
        let (a, b, c, _s) = landed_spec(&mut r);
        update(&mut r, "w", &a, "A holds now.");
        update(&mut r, "w", &c, "C stands now.");
        let v1 = r.propose(&writer(), "w", "w", None).unwrap();
        let of = |r: &Rdy, v: &str| {
            let st = r.read();
            let p = st.reader().proposal(&v.parse().unwrap()).unwrap();
            compute(&st, "w", &p.changes, p.canon_at, &[], &[]).unwrap()
        };
        let first = of(&r, &v1.id);
        let want = vec![
            (a.to_string(), b.to_string(), PlanKind::Confirm, None),
            (c.to_string(), b.to_string(), PlanKind::Confirm, None),
        ];
        let mut want_sorted = want.clone();
        want_sorted.sort_by_key(|(x, s, k, _)| item_hash(&id(x), s, *k));
        assert_eq!(doc_items(&first), want_sorted);
        assert_eq!(
            find(&first, a.as_str(), b.as_str(), PlanKind::Confirm).id,
            item_hash(&a, b.as_str(), PlanKind::Confirm)
        );
        assert!(first.code_unavailable);
        // a later version with one more change keeps both ids
        put(
            &mut r,
            "w",
            json!([{"op": "create", "type": "doc", "id": "$x", "fields": {"name": "more", "title": "More", "doc_type": "page"}}]),
        );
        let v2 = r.propose(&writer(), "w", "w", None).unwrap();
        assert_ne!(v1.id, v2.id);
        let second = of(&r, &v2.id);
        let ids = |p: &ComputedPlan| p.items.iter().map(|i| i.id).collect::<Vec<_>>();
        assert_eq!(ids(&first), ids(&second));
    }

    /// D-NJ2SP: only the net state counts: an edit undone before the proposal and a block
    /// created and then deleted leave no item.
    #[test]
    fn cp6_net_undo_and_create_delete_leave_no_debt() {
        let (_d, mut r) = project();
        let (a, _b, _c, s) = landed_spec(&mut r);
        let mut changes = vec![update(&mut r, "u", &a, "A holds 60 days.")];
        changes.push(update(&mut r, "u", &a, "A holds 30 days."));
        let made = put(
            &mut r,
            "u",
            json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.to_string(), "body": format!("X cites {{{{{a}}}}}.")}]),
        );
        changes.push(change(&made));
        changes.push(change(&put(
            &mut r,
            "u",
            json!([{"op": "delete", "id": made.created["$x"]}]),
        )));
        let st = r.read();
        assert_eq!(*st.included("u"), changes);
        let p = compute(&st, "u", &changes, st.canon_head(), &[], &[]).unwrap();
        assert_eq!(rows(&p), vec![]);
        assert_eq!(p.code_files, 0);
        // the first edit alone owes the confirm
        let p = compute(&st, "u", &changes[..1], st.canon_head(), &[], &[]).unwrap();
        assert_eq!(doc_items(&p).len(), 1, "{:#?}", rows(&p));
    }

    /// A version recorded on an earlier canon is read on that canon (its `canon_at`), not on
    /// the canon that landed since.
    #[test]
    fn cp6_an_earlier_canon_baseline_is_read_as_recorded() {
        let (_d, mut r) = project();
        let (a, b, _c, _s) = landed_spec(&mut r);
        update(&mut r, "w", &a, "A holds now.");
        let v = r.propose(&writer(), "w", "w", None).unwrap();
        let of = |r: &Rdy| {
            let st = r.read();
            let p = st.reader().proposal(&v.id.parse().unwrap()).unwrap();
            (
                p.canon_at,
                compute(&st, "w", &p.changes, p.canon_at, &[], &[]).unwrap(),
            )
        };
        let (at, before) = of(&r);
        assert_eq!(
            rows(&before),
            vec![(a.to_string(), b.to_string(), PlanKind::Confirm, None)]
        );
        // canon moves on: another view lands its own edit of a
        put(
            &mut r,
            "x",
            json!([{"op": "update", "id": a.to_string(), "body": "A holds, landed."}]),
        );
        land(&mut r, "x");
        assert_ne!(r.read().canon_head(), at);
        let (_, after) = of(&r);
        // on today's canon the version no longer applies; on its own it reads as it did
        let st = r.read();
        let p = st.reader().proposal(&v.id.parse().unwrap()).unwrap();
        assert!(compute(&st, "w", &p.changes, st.canon_head(), &[], &[]).is_err());
        assert_eq!(after, before);
    }

    /// A subset proposal reads only its selected changes: a repin in a change it leaves out does
    /// not close its item.
    #[test]
    fn cp6_subset_uses_only_its_selected_changes() {
        let (_d, mut r) = project();
        let (a, b, _c, _s) = landed_spec(&mut r);
        let edit = update(&mut r, "w", &a, "A holds now.");
        let repin = change(&put(
            &mut r,
            "w",
            json!([{"op": "repin", "id": b.to_string(), "targets": [a.to_string()]}]),
        ));
        let st = r.read();
        let all = compute(&st, "w", &[edit, repin], st.canon_head(), &[], &[]).unwrap();
        assert_eq!(
            close_of(&all, a.as_str(), b.as_str(), PlanKind::Confirm),
            Some(PlanOutcome::Repinned)
        );
        let subset = compute(&st, "w", &[edit], st.canon_head(), &[], &[]).unwrap();
        assert_eq!(
            close_of(&subset, a.as_str(), b.as_str(), PlanKind::Confirm),
            None
        );
        assert_eq!(
            find(&subset, a.as_str(), b.as_str(), PlanKind::Confirm).id,
            find(&all, a.as_str(), b.as_str(), PlanKind::Confirm).id
        );
    }

    /// p-0HXT0: a confirm from a reach (a refine) or a neighbour (a link that cannot make the
    /// subject suspect) has no suspect to clear: a pin never closes it.
    #[test]
    fn cp6_reach_without_suspect_requires_a_reasoned_confirm() {
        let (_d, r) = project();
        let st = r.read();
        let d1 = decision(D1, "Keep A", "accepted");
        let a = claim(A, "A holds.");
        let a2 = claim(A, "A holds now.");
        let g = pin(
            link(claim(G, "G follows the rule."), "decided_by", D1, None),
            &d1,
        );
        let n = pin(link(claim(N, "N notes it."), "describes", A, None), &a);
        let d3 = link(
            decision(D3, "Keep A, in short", "proposed"),
            "refines",
            D1,
            None,
        );
        let base = [d1.clone(), a.clone(), g.clone(), n.clone()];
        let p = plan(
            &st,
            &base,
            &[d1.clone(), a2.clone(), g.clone(), n.clone(), d3.clone()],
        );
        assert_eq!(close_of(&p, D1, G, PlanKind::Confirm), None);
        assert_eq!(find(&p, D1, G, PlanKind::Confirm).edge, "refines");
        assert_eq!(close_of(&p, A, N, PlanKind::Confirm), None);
        assert_eq!(find(&p, A, N, PlanKind::Confirm).edge, "describes");
        assert!(
            p.items.iter().all(|i| i.subject != D3 && i.subject != D1),
            "{:#?}",
            rows(&p)
        );
        // pins written again change nothing: both still need a confirm with a reason
        let p = plan(
            &st,
            &base,
            &[
                d1.clone(),
                a2.clone(),
                pin(g.clone(), &d1),
                pin(n.clone(), &a2),
                d3,
            ],
        );
        assert_eq!(close_of(&p, D1, G, PlanKind::Confirm), None);
        assert_eq!(close_of(&p, A, N, PlanKind::Confirm), None);
    }

    /// D-ERKCP: a changed or deleted block of any type is a cause of code items, and so is a
    /// decision the version supersedes (in whole or in part) or revokes.
    #[test]
    fn cp6_all_block_types_can_cause_code_items() {
        let (_d, r) = project();
        let st = r.read();
        const PR: &str = "p-PPPPP";
        const SX: &str = "s-XXXXX";
        const D9: &str = "D-99999";
        const D10: &str = "D-10000";
        const RV: &str = "D-RRRRR";
        const RK: &str = "D-RRRR2";
        let sx = text(block(SX, "section", Some(DOC), ""), "title", "Other");
        let base = vec![
            claim(A, "A holds."),
            block(PR, "prose", Some(SEC), "Some prose."),
            sx.clone(),
            decision(D1, "Keep it", "accepted"),
            claim(Q, "Q goes."),
            decision(D9, "Keep both", "accepted"),
            decision(RV, "Keep this", "accepted"),
        ];
        let fin = vec![
            claim(A, "A holds now."),
            block(PR, "prose", Some(SEC), "Other prose."),
            text(sx, "title", "Renamed"),
            decision(D1, "Keep it", "accepted"),
            link(
                decision(D2, "Keep the other", "proposed"),
                "supersedes",
                D1,
                None,
            ),
            decision(D9, "Keep both", "accepted"),
            link(
                decision(D10, "Keep one", "proposed"),
                "supersedes",
                D9,
                Some("the second"),
            ),
            decision(RV, "Keep this", "accepted"),
            link(decision(RK, "Drop this", "proposed"), "revokes", RV, None),
        ];
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git_init(dir);
        for (file, cause) in [
            ("a.rs", A),
            ("p.rs", PR),
            ("s.rs", SX),
            ("d1.rs", D1),
            ("q.rs", Q),
            ("d9.rs", D9),
            ("rv.rs", RV),
        ] {
            commit(dir, file, &format!("{file}\n\nImplements: {cause}"));
        }
        let head = git(dir, &["rev-parse", "HEAD"]);
        let p = compute_in(
            &st,
            &sides(&st, &base, &fin),
            Some(&[code_side(dir, &head, Some(&head))]),
            MAX_PLAN_ITEMS,
        )
        .unwrap();
        let code: Vec<(String, String)> = p
            .items
            .iter()
            .filter(|i| i.kind == PlanKind::UpdateImplementation)
            .map(|i| (i.cause.to_string(), i.subject.clone()))
            .collect();
        let mut want: Vec<(String, String)> = [
            (A, "a.rs"),
            (PR, "p.rs"),
            (SX, "s.rs"),
            (D1, "d1.rs"),
            (Q, "q.rs"),
            (D9, "d9.rs"),
            (RV, "rv.rs"),
        ]
        .iter()
        .map(|(c, f)| (c.to_string(), format!("app:{f}")))
        .collect();
        want.sort_by_key(|(c, s)| item_hash(&id(c), s, PlanKind::UpdateImplementation));
        assert_eq!(code, want);
        assert_eq!(p.code_files, 7);
        assert!(!p.code_unavailable);
        assert_eq!(
            find(&p, Q, "app:q.rs", PlanKind::UpdateImplementation).cause_hash,
            deleted_identity(&id(Q))
        );
        assert_eq!(
            deleted_identity(&id(Q)),
            Hash::of(&[b"rdy-plan-deleted\0".as_slice(), Q.as_bytes()].concat())
        );
        assert!(p.items.iter().all(|i| i.close.is_none()));
        // no linked repo: no code item, and the plan says so
        let none = plan(&st, &base, &fin);
        assert!(none.code_unavailable);
        assert_eq!(none.code_files, 0);
        assert!(
            none.items
                .iter()
                .all(|i| i.kind != PlanKind::UpdateImplementation)
        );
    }

    /// p-E9MA3: a tracking edit, a move, a repin, a re-affirmation and a refine change no
    /// content, and a new block has no baseline: none causes a code item; nor does a `reflects`
    /// glob.
    #[test]
    fn cp6_tracking_move_repin_reaffirm_and_refine_do_not_cause_code_items() {
        let (_d, r) = project();
        let st = r.read();
        const M: &str = "c-MMMMM";
        const SX: &str = "s-XXXXX";
        const RF: &str = "c-FFFFF";
        const NEW: &str = "c-WWWWW";
        const K: &str = "c-KKKKK";
        let tracking = Tracking::new(BTreeMap::from([(
            "claim".to_string(),
            BTreeSet::from(["owner".to_string()]),
        )]));
        let a = text(claim(A, "A holds."), "owner", "ann");
        let d1 = decision(D1, "Keep A", "accepted");
        let s = link(decision(S, "Build on A", "accepted"), "depends_on", A, None);
        let mut b = claim(B, "B builds on {{c-AAAAA}}.");
        b.pins.insert(id(A), Hash::of(b"an old version"));
        let reflects = FieldValue::List(vec![FieldValue::Str("src/*.rs".into())]);
        let rf = field(
            claim(RF, "R reflects the code."),
            "reflects",
            reflects.clone(),
        );
        let sx = text(block(SX, "section", Some(DOC), ""), "title", "Other");
        let base = vec![
            a.clone(),
            claim(M, "M moves."),
            sx.clone(),
            b.clone(),
            s.clone(),
            d1.clone(),
            rf,
            claim(K, "K changes."),
        ];
        let mut moved = claim(M, "M moves.");
        moved.parent = Some(id(SX));
        moved.order = "b0".into();
        let fin = vec![
            text(claim(A, "A holds."), "owner", "bob"),
            moved,
            sx,
            pin(b.clone(), &claim(A, "A holds.")),
            reaffirmed(pin(s, &a), A, Against::Retiring(vec![])),
            d1,
            link(
                decision(D3, "Keep A, in short", "proposed"),
                "refines",
                D1,
                None,
            ),
            field(claim(RF, "R reflects the code, now."), "reflects", reflects),
            claim(NEW, "W is new."),
            claim(K, "K changed."),
        ];
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git_init(dir);
        for (file, cause) in [
            ("a.rs", A),
            ("m.rs", M),
            ("b.rs", B),
            ("s.rs", S),
            ("d1.rs", D1),
            ("w.rs", NEW),
            ("k.rs", K),
        ] {
            commit(dir, file, &format!("{file}\n\nImplements: {cause}"));
        }
        let head = commit(dir, "src/x.rs", "reflected, no trailer");
        let sides = sides_with(&st, tracking, &base, &fin);
        assert!(sides.written.contains(&id(A)) && sides.written.contains(&id(M)));
        let p = compute_in(
            &st,
            &sides,
            Some(&[code_side(dir, &head, Some(&head))]),
            MAX_PLAN_ITEMS,
        )
        .unwrap();
        let code: Vec<_> = rows(&p)
            .into_iter()
            .filter(|r| r.2 == PlanKind::UpdateImplementation)
            .collect();
        // only the content change is a cause (the control)
        assert_eq!(
            code,
            vec![(
                K.to_string(),
                "app:k.rs".to_string(),
                PlanKind::UpdateImplementation,
                None
            )]
        );
        assert_eq!(p.code_files, 1);
        assert!(!p.code_unavailable);
    }

    /// D-ERKCP, O5: a code item closes when a commit after the chain's first HEAD, reached from
    /// the version's HEAD, touched the file and cites the cause or a decision of the version.
    #[test]
    fn cp6_code_close_uses_first_heads_and_matching_touch() {
        let (d, mut r) = project();
        let (a, _b, _c, section) = landed_spec(&mut r);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git_init(dir);
        let h1 = commit(dir, "f.rs", &format!("start\n\nImplements: {a}"));
        let cfg = d.path().join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str("code_repos: [app]\n");
        std::fs::write(&cfg, text).unwrap();
        std::fs::create_dir_all(d.path().join(".rdy")).unwrap();
        std::fs::write(
            d.path().join(".rdy/local.yaml"),
            format!(
                "code_paths:\n  app: {}\n",
                serde_json::to_string(&dir.display().to_string()).unwrap()
            ),
        )
        .unwrap();
        let edit = update(&mut r, "w", &a, "A holds now.");
        let made = r
            .decide(
                &writer(),
                "w",
                crate::DecisionInput {
                    parent: section.to_string(),
                    statement: "Hold A".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let dn = made.created["$decision"].clone();
        let changes = r.read().included("w").to_vec();
        assert!(changes.contains(&edit));
        let head = |h: &str| {
            vec![PlanHead {
                repo: "app".into(),
                head: Some(h.to_string()),
            }]
        };
        let at = |r: &Rdy, now: &str, first: &[PlanHead]| {
            let st = r.read();
            compute(&st, "w", &changes, st.canon_head(), &head(now), first).unwrap()
        };
        let f =
            |p: &ComputedPlan| close_of(p, a.as_str(), "app:f.rs", PlanKind::UpdateImplementation);
        // a later touch that cites another block closes nothing
        let h2 = commit(dir, "f.rs", "touch\n\nImplements: c-ZZZZZ");
        assert_eq!(f(&at(&r, &h2, &head(&h1))), None);
        // a later commit that cites the version's decision but misses the file closes nothing
        commit(dir, "g.rs", &format!("other\n\nImplements: {dn}"));
        let p = at(&r, &git(dir, &["rev-parse", "HEAD"]), &head(&h1));
        assert_eq!(f(&p), None);
        assert_eq!(p.code_files, 1, "{:#?}", rows(&p));
        // a later touch citing a decision of the version closes it
        let h4 = commit(dir, "f.rs", &format!("follow\n\nImplements: {dn}"));
        assert_eq!(f(&at(&r, &h4, &head(&h1))), Some(PlanOutcome::Committed));
        // from a first HEAD at the version's HEAD nothing is later
        assert_eq!(f(&at(&r, &h4, &head(&h4))), None);
        // no chain: the captured HEAD is both ends
        assert_eq!(f(&at(&r, &h4, &[])), None);
        // the cause itself closes it too
        let h5 = commit(dir, "f.rs", &format!("again\n\nImplements: {a}"));
        assert_eq!(f(&at(&r, &h5, &head(&h4))), Some(PlanOutcome::Committed));
        // no linked repo captured: code unavailable, no code item
        let st = r.read();
        let p = compute(&st, "w", &changes, st.canon_head(), &[], &[]).unwrap();
        assert!(p.code_unavailable);
        assert!(
            p.items
                .iter()
                .all(|i| i.kind != PlanKind::UpdateImplementation)
        );
    }

    /// The plan is refused whole past its item bound; nothing is cut to fit.
    #[test]
    fn cp6_limit_is_refused_before_a_partial_plan() {
        assert_eq!(MAX_PLAN_ITEMS, 100_000);
        let e = limit_error(MAX_PLAN_ITEMS);
        assert_eq!(e.code, E_LIMIT);
        assert_eq!(e.message, "change plan exceeds 100000 plan items");
        assert_eq!(e.fix, "split the change into smaller proposals");
        let (_d, r) = project();
        let st = r.read();
        let a = claim(A, "A holds.");
        let base = [
            a.clone(),
            pin(claim(B, "B builds on {{c-AAAAA}}."), &a),
            pin(claim(C, "C builds on {{c-AAAAA}}."), &a),
        ];
        let fin = [claim(A, "A holds now."), base[1].clone(), base[2].clone()];
        let sides = sides(&st, &base, &fin);
        assert_eq!(compute_in(&st, &sides, None, 2).unwrap().items.len(), 2);
        let e = compute_in(&st, &sides, None, 1).unwrap_err();
        assert_eq!(e.code, E_LIMIT);
        assert_eq!(e.message, "change plan exceeds 1 plan items");
        assert_eq!(e.fix, "split the change into smaller proposals");
    }

    /// The re-affirm fix names the act that answers it (FW-R6 shipped).
    #[test]
    fn cp6_reaffirm_fix_is_actionable() {
        assert_eq!(
            REAFFIRM_FIX,
            "if it still holds, re-affirm it with a reaffirm op; otherwise supersede it"
        );
    }

    /// An existing refiner that also depends on its target keeps that dependency: only the
    /// version's own lineage links are the change. Accepted S refines and depends on D1; the
    /// version supersedes D1 with D2, so S owes an answer on D1. D2 is only proposed, so S is not
    /// suspect on D1 and the item is a confirm, not a reaffirm the reaffirm op would refuse
    /// (D-ATBP6).
    #[test]
    fn cp6_an_existing_refiner_keeps_its_own_dependency() {
        let (_d, r) = project();
        let st = r.read();
        let d1 = decision(D1, "Keep 30 days", "accepted");
        let s = pin(
            link(
                link(
                    decision(S, "Keep 30 days for teams", "accepted"),
                    "refines",
                    D1,
                    None,
                ),
                "depends_on",
                D1,
                None,
            ),
            &d1,
        );
        let d2 = link(
            decision(D2, "Keep 60 days", "proposed"),
            "supersedes",
            D1,
            None,
        );
        let p = plan(&st, &[d1.clone(), s.clone()], &[d1, s, d2]);
        assert_eq!(
            doc_items(&p),
            vec![(D1.to_string(), S.to_string(), PlanKind::Confirm, None)]
        );
        assert_eq!(find(&p, D1, S, PlanKind::Confirm).edge, "depends_on");
    }

    /// p-0HXT0: a reach on a link the version itself writes still needs a reasoned confirm.
    /// G is ungoverned; the version links it to D1 and refines D1 with D3.
    #[test]
    fn cp6_a_reach_on_new_governance_still_needs_a_confirm() {
        let (_d, r) = project();
        let st = r.read();
        let d1 = decision(D1, "Keep A", "accepted");
        let g = claim(G, "G follows the rule.");
        let governed = pin(link(g.clone(), "decided_by", D1, None), &d1);
        let d3 = link(
            decision(D3, "Keep A, in short", "proposed"),
            "refines",
            D1,
            None,
        );
        let p = plan(&st, &[d1.clone(), g], &[d1, governed, d3]);
        assert_eq!(
            doc_items(&p),
            vec![(D1.to_string(), G.to_string(), PlanKind::Confirm, None)]
        );
        assert_eq!(find(&p, D1, G, PlanKind::Confirm).edge, "refines");
    }

    /// Final review F5: every outcome an answer gives an item of a kind is one the store's
    /// shared matrix lets that kind seal.
    #[test]
    fn cp6_fix1_answer_outcomes_are_in_the_stores_matrix() {
        use rdy_model::plan::outcome_allowed;
        for kind in [
            PlanKind::Update,
            PlanKind::Rejustify,
            PlanKind::Reaffirm,
            PlanKind::Confirm,
            PlanKind::UpdateImplementation,
        ] {
            for act in allowed_acts(kind) {
                let outcome = match act {
                    PlanAct::Confirm => PlanOutcome::Confirmed,
                    PlanAct::Done => PlanOutcome::Done,
                    PlanAct::Defer => PlanOutcome::Deferred,
                };
                assert!(outcome_allowed(kind, outcome), "{kind:?}/{outcome:?}");
            }
            assert!(outcome_allowed(kind, PlanOutcome::Open), "{kind:?}");
        }
    }

    /// A repeated re-affirmation (the same reason and `against`, only the pin moves) is a
    /// re-affirmation for code closes too, as it is for the document item.
    #[test]
    fn cp6_a_repeated_reaffirmation_closes_its_code_item() {
        let (_d, r) = project();
        let st = r.read();
        let a = claim(A, "A holds.");
        let a2 = claim(A, "A holds firmly.");
        let s = link(decision(S, "Build on A", "accepted"), "depends_on", A, None);
        let before = reaffirmed(pin(s.clone(), &a), A, Against::Retiring(vec![]));
        let after = reaffirmed(pin(s, &a2), A, Against::Retiring(vec![]));
        assert_eq!(
            before.fields.get(engine_fields::REAFFIRMED),
            after.fields.get(engine_fields::REAFFIRMED)
        );
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git_init(dir);
        let first = commit(dir, "f.rs", "start\n\nImplements: c-AAAAA");
        let later = commit(dir, "f.rs", "follow S\n\nImplements: D-SSSSS");
        let sides = sides(&st, &[a, before], &[a2, after]);
        let p = compute_in(
            &st,
            &sides,
            Some(&[code_side(dir, &later, Some(&first))]),
            MAX_PLAN_ITEMS,
        )
        .unwrap();
        assert_eq!(
            close_of(&p, A, S, PlanKind::Reaffirm),
            Some(PlanOutcome::Reaffirmed)
        );
        assert_eq!(
            close_of(&p, A, "app:f.rs", PlanKind::UpdateImplementation),
            Some(PlanOutcome::Committed)
        );
    }

    /// The bound stops the plan at the first candidate past it: no later pair is judged.
    #[test]
    fn cp6_the_limit_stops_at_the_first_excess_candidate() {
        use crate::consequence::PLAN_PAIRS_JUDGED;
        let (_d, r) = project();
        let st = r.read();
        let a = claim(A, "A holds.");
        let deps = [
            pin(claim(B, "B builds on {{c-AAAAA}}."), &a),
            pin(claim(C, "C builds on {{c-AAAAA}}."), &a),
            pin(claim(P, "P builds on {{c-AAAAA}}."), &a),
        ];
        let mut base = vec![a];
        base.extend(deps.iter().cloned());
        let mut fin = vec![claim(A, "A holds now.")];
        fin.extend(deps.iter().cloned());
        let sides = sides(&st, &base, &fin);
        let judged = |limit: usize| {
            PLAN_PAIRS_JUDGED.with(|c| c.set(0));
            let out = compute_in(&st, &sides, None, limit);
            (out, PLAN_PAIRS_JUDGED.with(|c| c.get()))
        };
        let (all, n) = judged(3);
        assert_eq!(all.unwrap().items.len(), 3);
        assert_eq!(n, 3);
        let (refused, n) = judged(1);
        assert_eq!(
            refused.unwrap_err().message,
            "change plan exceeds 1 plan items"
        );
        assert_eq!(n, 2, "the plan stops at the second candidate");
    }

    /// task-6 R1 (D-4JMPH, O4): a deleted subject closes first, as updated, whatever its kind:
    /// a confirm, a reach and a neighbour too. The item stays, with the deleted identity.
    #[test]
    fn cp6_a_deleted_subject_closes_as_updated() {
        // the review's case through the public write path: b references a without its
        // literals; the version changes a and deletes b
        let (_d, mut r) = project();
        let (a, b, _c, _s) = landed_spec(&mut r);
        let mut changes = vec![update(&mut r, "w", &a, "A holds now.")];
        changes.push(change(&put(
            &mut r,
            "w",
            json!([{"op": "delete", "id": b.to_string()}]),
        )));
        let st = r.read();
        let p = compute(&st, "w", &changes, st.canon_head(), &[], &[]).unwrap();
        assert_eq!(
            rows(&p),
            vec![(
                a.to_string(),
                b.to_string(),
                PlanKind::Confirm,
                Some(PlanOutcome::Updated)
            )]
        );
        assert_eq!(p.items[0].subject_hash, Some(deleted_identity(&b)));

        // a reach and a neighbour with a deleted subject
        let (_d, r) = project();
        let st = r.read();
        let d1 = decision(D1, "Keep A", "accepted");
        let a = claim(A, "A holds.");
        let g = pin(
            link(claim(G, "G follows the rule."), "decided_by", D1, None),
            &d1,
        );
        let n = pin(link(claim(N, "N notes it."), "describes", A, None), &a);
        let d3 = link(
            decision(D3, "Keep A, in short", "proposed"),
            "refines",
            D1,
            None,
        );
        let base = [d1.clone(), a, g, n];
        let p = plan(&st, &base, &[d1.clone(), claim(A, "A holds now."), d3]);
        let mut want = vec![
            (
                D1.to_string(),
                G.to_string(),
                PlanKind::Confirm,
                Some(PlanOutcome::Updated),
            ),
            (
                A.to_string(),
                N.to_string(),
                PlanKind::Confirm,
                Some(PlanOutcome::Updated),
            ),
        ];
        want.sort_by_key(|(c, s, k, _)| item_hash(&id(c), s, *k));
        assert_eq!(doc_items(&p), want);
        assert_eq!(find(&p, D1, G, PlanKind::Confirm).edge, "refines");
        assert_eq!(find(&p, A, N, PlanKind::Confirm).edge, "describes");

        // a re-affirm subject deleted closes as updated too, not retired
        let a = claim(A, "A holds.");
        let s = pin(
            link(decision(S, "Build on A", "accepted"), "depends_on", A, None),
            &a,
        );
        let p = plan(&st, &[a, s], &[claim(A, "A holds now.")]);
        assert_eq!(
            close_of(&p, A, S, PlanKind::Reaffirm),
            Some(PlanOutcome::Updated)
        );
    }
}
