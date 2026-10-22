//! Re-affirmation (D-EJBPD, p-0H3JT, p-0SH30, p-QMGB4, p-HF5W0): the `reaffirm` op records that
//! a suspect accepted decision still holds, without changing its ruling, body, links or content
//! hash.
//!
//! The op is one ordinary metadata-only update per decision (no review kind of its own):
//! - for a changed target, the decision's pin moves to the target's current content and the
//!   store-only `reaffirmed` field records `against: []` and the reason (O18);
//! - for a superseded or revoked target, it records the sorted set of decisions that retire the
//!   target now ([`Graph::retiring_decisions`]), and the pin moves to the target's current
//!   content, so the premise's own drift is re-affirmed too;
//! - for a deleted target, it records `deleted`.
//!
//! The selection runs once, on the write's starting view, before the briefing check and before
//! any op applies ([`select`]); the briefing check and the op builder read the same result.
use crate::error::*;
use crate::write::OpInput;
use crate::{Materialized, Rdy, ReadState, Scope, Session, keys, views};
use rdy_graph::{DerivedStatus, Graph};
use rdy_model::engine_fields::{
    self, Against, MAX_REASON_CHARS, Reaffirmation, decode_reaffirmed, encode_reaffirmed,
};
use rdy_model::{Block, BlockId, Hash, decision};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most decisions one write re-affirms (the briefing bound, [`crate::brief::MAX_BRIEF_BLOCKS`]).
pub const MAX_REAFFIRM_DECISIONS: usize = crate::brief::MAX_BRIEF_BLOCKS;

/// The most (decision, target) pairs one write re-affirms.
pub const MAX_REAFFIRM_PAIRS: usize = 100_000;

/// The most bytes of target text a bulk reaffirm may repeat across its ops: the bytes of the
/// shared targets, times the number of decisions.
pub const MAX_REAFFIRM_SHARED_BYTES: usize = 16 * 1024 * 1024;

/// Checks the inputs that `count` reaffirm ops would share (one reason and one target list)
/// before any op is built, so a bulk request cannot multiply them past the bounds. Returns the
/// reason, trimmed. The reason check is the engine's own; the pair and byte totals are checked
/// products, never wrapped.
pub fn check_shared_inputs(
    count: usize,
    reason: &str,
    targets: Option<&[String]>,
) -> Result<String, RdyError> {
    let reason = Reason::parse(reason)?;
    if count > MAX_REAFFIRM_DECISIONS {
        return Err(bound_error());
    }
    if let Some(ts) = targets {
        let pairs = count.checked_mul(ts.len());
        let bytes = ts
            .iter()
            .try_fold(0usize, |n, t| n.checked_add(t.len()))
            .and_then(|n| n.checked_mul(count));
        if pairs.is_none_or(|n| n > MAX_REAFFIRM_PAIRS)
            || bytes.is_none_or(|n| n > MAX_REAFFIRM_SHARED_BYTES)
        {
            return Err(bound_error());
        }
    }
    Ok(reason.as_str().to_string())
}

/// The sentence an `E_FROZEN` fix ends with when the frozen decision is a suspect one that a
/// `reaffirm` op could re-affirm.
pub(crate) const FROZEN_REAFFIRM_HINT: &str =
    "For a suspect accepted decision that still holds, re-affirm it with a reaffirm op instead.";

/// One row of a write's `reaffirmed` reply: a decision the write re-affirmed, and the causes
/// (suspect targets) it re-affirmed it against, sorted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reaffirmed {
    pub id: String,
    pub causes: Vec<String>,
}

/// A re-affirmation reason, trimmed, of 1 to [`MAX_REASON_CHARS`] Unicode scalar values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reason(String);

impl Reason {
    /// The reason `text`, trimmed and checked. The error never quotes the text.
    pub(crate) fn parse(text: &str) -> Result<Reason, RdyError> {
        let t = text.trim();
        let n = t.chars().count();
        if n == 0 || n > MAX_REASON_CHARS {
            return Err(bad(
                "a re-affirmation needs a reason of 1 to 2000 characters",
                "give a reason of 1 to 2000 characters",
            ));
        }
        Ok(Reason(t.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// One decision a reaffirm op selected, with the causes it is re-affirmed against.
#[derive(Clone, Debug)]
pub(crate) struct Selected {
    pub id: BlockId,
    pub causes: BTreeSet<BlockId>,
}

/// What one reaffirm op selected: its validated reason and its decisions, sorted by id.
#[derive(Clone, Debug)]
pub(crate) struct OpSelection {
    pub reason: Reason,
    pub decisions: Vec<Selected>,
}

/// The selection of every reaffirm op of a write, by op index, and the starting view's graph
/// it was made on (`None` when the write has no reaffirm op).
#[derive(Clone, Debug, Default)]
pub(crate) struct Selection {
    pub ops: BTreeMap<usize, OpSelection>,
    pub graph: Option<Arc<Graph>>,
}

impl Selection {
    /// The decisions every reaffirm op selected.
    pub(crate) fn ids(&self) -> BTreeSet<BlockId> {
        self.ops
            .values()
            .flat_map(|o| o.decisions.iter().map(|s| s.id.clone()))
            .collect()
    }

    /// The decisions op `i` selected (none for an op that is no reaffirm).
    pub(crate) fn of(&self, i: usize) -> &[Selected] {
        self.ops.get(&i).map_or(&[], |o| o.decisions.as_slice())
    }
}

fn bad(message: impl Into<String>, fix: impl Into<String>) -> RdyError {
    RdyError::new(E_BAD_INPUT, message, fix)
}

fn bound_error() -> RdyError {
    bad(
        "reaffirm exceeds 5000 selected decisions or 100000 targets",
        "split the re-affirmation into smaller writes",
    )
}

fn not_a_selected_suspect(target: &BlockId, id: &BlockId) -> RdyError {
    bad(
        format!("{target} is not a selected suspect of {id}"),
        "pass targets from the decision's current suspects",
    )
}

/// Whether `s`'s status lets it be re-affirmed: accepted, and live or partly superseded.
fn is_live(s: Option<&DerivedStatus>) -> bool {
    matches!(
        s,
        Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. })
    )
}

/// The suspects of `id` in the view, as `rdy_context` lists them: a presentational target
/// makes nothing suspect. By target, with the reason.
fn suspects_of(
    st: &ReadState,
    m: &Materialized,
    g: &Graph,
    id: &BlockId,
) -> BTreeMap<BlockId, String> {
    g.suspects_of(id)
        .into_iter()
        .filter(|s| !st.is_presentational_source(m, s))
        .map(|s| (s.target, s.reason))
        .collect()
}

/// Whether `id` is an accepted decision that a reaffirm op could re-affirm in the view: live
/// or partly superseded, with a suspect. An `E_FROZEN` fix names the op only for one.
pub(crate) fn reaffirmable(st: &ReadState, scope: &Scope, id: &BlockId) -> bool {
    let (Ok(m), Ok(g)) = (st.materialized(scope), st.graph(scope)) else {
        return false;
    };
    m.get(id)
        .is_some_and(|b| b.block_type == decision::DECISION)
        && is_live(g.status(id).as_ref())
        && !suspects_of(st, &m, &g, id).is_empty()
}

/// An `E_FROZEN` fix, with [`FROZEN_REAFFIRM_HINT`] appended when `reaffirmable`.
pub(crate) fn frozen_fix(fix: String, reaffirmable: bool) -> String {
    if reaffirmable {
        format!("{fix}. {FROZEN_REAFFIRM_HINT}")
    } else {
        fix
    }
}

/// The selection of every reaffirm op in `ops`, against the view `scope` as `st` holds it (the
/// write's starting view). Ops of other kinds are skipped. Each op is checked in order: its
/// selector, its reason, its targets, then the decisions it names; the bounds hold for the
/// whole write.
pub(crate) fn select(
    st: &ReadState,
    scope: &Scope,
    ops: &[OpInput],
) -> Result<Selection, RdyError> {
    let mut out = Selection::default();
    if !ops.iter().any(|o| matches!(o, OpInput::Reaffirm { .. })) {
        return Ok(out);
    }
    let m = st.materialized(scope)?;
    let g = st.graph(scope)?;
    let (mut decisions, mut pairs) = (0usize, 0usize);
    for (i, op) in ops.iter().enumerate() {
        let OpInput::Reaffirm {
            id,
            because,
            base,
            targets,
            reason,
        } = op
        else {
            continue;
        };
        let sel = select_op(
            st,
            &m,
            &g,
            id.as_deref(),
            because.as_deref(),
            base.is_some(),
            targets.as_deref(),
            reason,
            MAX_REAFFIRM_DECISIONS - decisions,
        )?;
        decisions += sel.decisions.len();
        pairs += sel.decisions.iter().map(|s| s.causes.len()).sum::<usize>();
        if decisions > MAX_REAFFIRM_DECISIONS || pairs > MAX_REAFFIRM_PAIRS {
            return Err(bound_error());
        }
        out.ops.insert(i, sel);
    }
    out.graph = Some(g);
    Ok(out)
}

/// The selection of one reaffirm op. `room` is how many decisions the write may still select.
#[allow(clippy::too_many_arguments)]
fn select_op(
    st: &ReadState,
    m: &Materialized,
    g: &Graph,
    id: Option<&str>,
    because: Option<&str>,
    has_base: bool,
    targets: Option<&[String]>,
    reason: &str,
    room: usize,
) -> Result<OpSelection, RdyError> {
    if id.is_some() == because.is_some() {
        return Err(bad(
            "reaffirm takes id or because, not both",
            "pass id for one decision, or because for its suspect decisions",
        ));
    }
    if because.is_some() && has_base {
        return Err(bad(
            "base goes with id only",
            "drop base, or pass id instead of because",
        ));
    }
    let reason = Reason::parse(reason)?;
    let wanted: Option<BTreeSet<BlockId>> = match targets {
        None => None,
        Some([]) => {
            return Err(bad(
                "reaffirm targets is empty",
                "omit targets, or pass a current suspect target",
            ));
        }
        Some(ts) => Some(
            ts.iter()
                .map(|t| keys::resolve_text(m, t))
                .collect::<Result<_, _>>()?,
        ),
    };
    let decisions = match (id, because) {
        (Some(text), _) => vec![select_id(st, m, g, text, wanted)?],
        (None, Some(text)) => select_because(st, m, g, text, wanted, room)?,
        (None, None) => unreachable!("checked above"),
    };
    Ok(OpSelection { reason, decisions })
}

/// The decision `text` names, checked, with its causes.
fn select_id(
    st: &ReadState,
    m: &Materialized,
    g: &Graph,
    text: &str,
    wanted: Option<BTreeSet<BlockId>>,
) -> Result<Selected, RdyError> {
    let id = keys::resolve_text(m, text)?;
    let b = m.get(&id).ok_or_else(|| {
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no block {id} in this view"),
            "check the id with rdy_read",
        )
    })?;
    if b.block_type != decision::DECISION {
        return Err(bad(
            format!("{id} is not a decision"),
            "pass a live or partly superseded decision",
        ));
    }
    match g.status(&id) {
        Some(
            s @ (DerivedStatus::Rejected
            | DerivedStatus::Superseded { .. }
            | DerivedStatus::Revoked { .. }),
        ) => {
            return Err(bad(
                format!("{id} is {}; it asserts nothing to re-affirm", s.tag()),
                "pass a live or partly superseded decision",
            ));
        }
        Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. }) => {}
        Some(DerivedStatus::Proposed) | None => {
            return Err(bad(
                format!("{id} is proposed; update it instead of re-affirming it"),
                "update the proposed decision",
            ));
        }
    }
    let suspects = suspects_of(st, m, g, &id);
    if suspects.is_empty() {
        return Err(bad(
            format!("{id} has no suspect; there is nothing to re-affirm"),
            "read its context and pass a suspect decision",
        ));
    }
    let causes = match wanted {
        None => suspects.into_keys().collect(),
        Some(w) => {
            if let Some(t) = w.iter().find(|t| !suspects.contains_key(*t)) {
                return Err(not_a_selected_suspect(t, &id));
            }
            w
        }
    };
    Ok(Selected { id, causes })
}

/// Every live or partly superseded decision in the view that is suspect on the block `text`
/// names, sorted by id. The block may be deleted: an id names it.
fn select_because(
    st: &ReadState,
    m: &Materialized,
    g: &Graph,
    text: &str,
    wanted: Option<BTreeSet<BlockId>>,
    room: usize,
) -> Result<Vec<Selected>, RdyError> {
    let cause = keys::resolve_text(m, text)?;
    let sources: BTreeSet<&BlockId> = g.incoming(&cause).iter().map(|e| &e.from).collect();
    let mut out = vec![];
    for d in sources {
        if !m.get(d).is_some_and(|b| b.block_type == decision::DECISION)
            || !is_live(g.status(d).as_ref())
            || !suspects_of(st, m, g, d).contains_key(&cause)
        {
            continue;
        }
        if out.len() == room {
            return Err(bound_error());
        }
        out.push(Selected {
            id: d.clone(),
            causes: BTreeSet::from([cause.clone()]),
        });
    }
    let Some(first) = out.first() else {
        return Err(bad(
            format!("no decision in this view is suspect on {cause}"),
            "read the suspects in this view",
        ));
    };
    if let Some(w) = wanted
        && let Some(t) = w.iter().find(|t| **t != cause)
    {
        return Err(not_a_selected_suspect(t, &first.id));
    }
    Ok(out)
}

/// The re-affirmed version of the decision `cur` against `causes`, with `reason`: the internal
/// transformation every re-affirmation uses (a reaffirm op, or an answered card). `g` is the
/// graph the causes were selected on; `current` gives a target's current content hash, `None`
/// for a target that is gone. Each cause must be a suspect of `cur` in `g`. Only the `reaffirmed`
/// field and the causes' pins change: unselected pins and records are kept.
pub(crate) fn reaffirmed_block(
    g: &Graph,
    cur: &Block,
    causes: &BTreeSet<BlockId>,
    reason: &Reason,
    current: &dyn Fn(&BlockId) -> Option<Hash>,
) -> Result<Block, RdyError> {
    let suspects: BTreeMap<BlockId, String> = g
        .suspects_of(&cur.id)
        .into_iter()
        .map(|s| (s.target, s.reason))
        .collect();
    let mut block = cur.clone();
    let mut record = block
        .fields
        .get(engine_fields::REAFFIRMED)
        .and_then(|v| decode_reaffirmed(v).ok())
        .unwrap_or_default();
    for t in causes {
        let Some(why) = suspects.get(t) else {
            return Err(not_a_selected_suspect(t, &cur.id));
        };
        let against = match why.as_str() {
            "deleted" => Against::Deleted,
            "superseded" | "revoked" => {
                Against::Retiring(g.retiring_decisions(t).into_iter().collect())
            }
            _ => Against::Retiring(vec![]),
        };
        // a changed target is pinned (its pin differs); a retired one is when it is a premise
        if !matches!(against, Against::Deleted)
            && block.pins.contains_key(t)
            && let Some(h) = current(t)
        {
            block.pins.insert(t.clone(), h);
        }
        record.insert(
            t.clone(),
            Reaffirmation {
                against,
                reason: reason.as_str().to_string(),
            },
        );
    }
    block
        .fields
        .insert(engine_fields::REAFFIRMED.into(), encode_reaffirmed(&record));
    Ok(block)
}

impl Rdy {
    /// The decisions the reaffirm ops of `ops` select in `view` as it is now: the same
    /// selection pass the write runs, so a caller can brief exactly them. Ops of other kinds are
    /// ignored. Refused as the write would refuse the selection.
    pub fn reaffirm_selection(
        &self,
        session: &Session,
        view: &str,
        ops: &[OpInput],
    ) -> Result<BTreeSet<BlockId>, RdyError> {
        let st = self.read();
        crate::write::check_view_access(&st, view, session, views::Action::Commit)?;
        Ok(select(&st, &Scope::View(view.to_string()), ops)?.ids())
    }
}
