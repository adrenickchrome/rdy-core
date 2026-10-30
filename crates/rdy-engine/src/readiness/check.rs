//! Readiness check 1 (D-ME2HD item 1, O3): each change of a version that touches stable content
//! records a briefing that was current when it was written.
//!
//! A change is judged on the state it was written on (ruling task-8 F2), as the write funnel
//! judged it: the canon of the version's `canon_at` chain that the log holds before the change
//! (a doc that canon makes stable after the write does not count), plus the version's earlier
//! changes, in order (a block an earlier change made draft, or moved out of a stable doc, is a
//! draft for the later one). So a change the funnel accepted with no briefing is never refused.
//!
//! A change needs no briefing when (closed): (a) it touches no stable content, by the subjects a
//! write's briefing check reads (`brief::stable_subjects`), on that state; (b) `rdy import`
//! wrote it (`links.imported_by`); (c) the log holds it before the store's first change that
//! records a briefing (none yet: every change is before it); (d) it is a replay
//! (`links.replayed_from`) whose chain of sources, followed for at most [`MAX_REPLAY_STEPS`]
//! steps, reaches a change that (b) or (c) exempts (ruling task-8 F3; a longer chain, a missing
//! source or a cycle is not exempt). Every other change with no briefing is one
//! `E_BRIEF_REQUIRED` issue, on its first stable subject. It stays in the version until it is
//! excluded, so the fix says so ([`BRIEFING_FIX`]).
//!
//! rdy's own writers record their briefing: the propose's key change and a resolve to the landed
//! side brief themselves, as a repin does, and a replay copies its source's.

use crate::brief::{E_BRIEF_REQUIRED, stable_subjects};
use crate::error::RdyError;
use crate::write::{MoveTo, OpInput};
use crate::{Location, Materialized, ReadState};
use rdy_model::{Block, Change, FieldValue, Hash, Op, Proposal};
use rdy_schema::{Issue, Severity};
use rdy_store::StoreError;
use std::collections::{BTreeMap, BTreeSet};

/// The fix of a stable change with no briefing (O3, ruling task-8 F2): the change stays in the
/// version until it is excluded.
pub const BRIEFING_FIX: &str =
    "exclude the change from the view, then brief the block and write it again";

/// The most `replayed_from` steps exemption (d) follows (ruling task-8 F3; a fixed bound).
pub const MAX_REPLAY_STEPS: usize = 64;

/// The check 1 issues of version `p`, whose changes are `changes` (see the module docs).
pub(crate) fn briefing_issues(
    st: &ReadState,
    p: &Proposal,
    changes: &[Change],
) -> Result<Vec<Issue>, RdyError> {
    let reader = st.reader();
    // (c): no change records a briefing yet, so every change is before the first one
    let Some(first) = reader.first_briefed_change()? else {
        return Ok(vec![]);
    };
    let exempt = |c: &Change| -> Result<bool, RdyError> {
        Ok(c.links.imported_by.is_some() || reader.change_location(&c.id())? < first)
    };
    let mut canons = CanonChain::new(p.canon_at);
    let mut written: Option<Written> = None;
    let mut issues = vec![];
    for (i, c) in changes.iter().enumerate() {
        if c.links.briefing.is_some() || exempt(c)? || replay_exempt(st, c, &exempt)? {
            continue;
        }
        let canon = canons.before(st, reader.change_location(&c.id())?)?;
        let m = state_before(st, &mut written, canon, changes, i)?;
        let ops: Vec<OpInput> = c.ops.iter().map(|op| op_input(m, op)).collect();
        let stable = stable_subjects(st.pack(), m, &ops);
        let Some(first) = stable.first() else {
            continue;
        };
        let ids: Vec<String> = stable.iter().map(ToString::to_string).collect();
        issues.push(Issue {
            severity: Severity::Error,
            code: E_BRIEF_REQUIRED,
            block: Some(first.clone()),
            message: format!(
                "change {} touches stable content ({}) and records no briefing",
                c.id().short(),
                ids.join(", ")
            ),
            fix: BRIEFING_FIX.into(),
        });
    }
    Ok(issues)
}

/// Exemption (d): whether `c` is a replay whose chain of sources reaches, within
/// [`MAX_REPLAY_STEPS`] steps, a change `exempt` exempts by (b) or (c). A missing source or a
/// cycle ends the chain: not exempt.
fn replay_exempt(
    st: &ReadState,
    c: &Change,
    exempt: &impl Fn(&Change) -> Result<bool, RdyError>,
) -> Result<bool, RdyError> {
    let reader = st.reader();
    let mut seen: BTreeSet<Hash> = BTreeSet::new();
    let mut next = c.links.replayed_from;
    for _ in 0..MAX_REPLAY_STEPS {
        let Some(src) = next else {
            return Ok(false);
        };
        if !seen.insert(src) {
            return Ok(false);
        }
        let source = match reader.change(&src) {
            Ok(s) => s,
            Err(StoreError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if exempt(&source)? {
            return Ok(true);
        }
        next = source.links.replayed_from;
    }
    Ok(false)
}

/// Canon's landings from a version's `canon_at` back, read as far as a check needs them.
struct CanonChain {
    /// Each landing read so far with its place in the log, newest first; the last one's `prev`
    /// is `rest`.
    read: Vec<(Hash, Location)>,
    /// The landing before the last one read: `None` once the chain reaches the empty canon.
    rest: Option<Hash>,
}

impl CanonChain {
    fn new(canon_at: Option<Hash>) -> CanonChain {
        CanonChain {
            read: vec![],
            rest: canon_at,
        }
    }

    /// The newest landing of the chain that the log holds before `at` (`None`: the empty canon).
    fn before(&mut self, st: &ReadState, at: Location) -> Result<Option<Hash>, RdyError> {
        let reader = st.reader();
        loop {
            if let Some((h, _)) = self.read.iter().find(|(_, l)| *l < at) {
                return Ok(Some(*h));
            }
            let Some(h) = self.rest else {
                return Ok(None);
            };
            let l = reader.landing_location(&h)?;
            self.rest = reader.landing(&h)?.prev;
            self.read.push((h, l));
        }
    }
}

/// The state a change was last judged on: canon at `canon` with the version's first `next`
/// changes applied.
struct Written {
    canon: Option<Hash>,
    next: usize,
    m: Materialized,
}

/// The state change `i` of `changes` was written on: canon at landing `canon` (`None`: the empty
/// canon) plus `changes[..i]`, in order. `written` carries it from one change to the next, so a
/// run on one canon applies each change once.
fn state_before<'w>(
    st: &ReadState,
    written: &'w mut Option<Written>,
    canon: Option<Hash>,
    changes: &[Change],
    i: usize,
) -> Result<&'w Materialized, RdyError> {
    let reuse = written
        .as_ref()
        .is_some_and(|w| w.canon == canon && w.next <= i);
    if !reuse {
        let m = match super::canon_scope(st, canon) {
            Some(scope) => (*st.materialized(&scope)?).clone(),
            None => Materialized::default(),
        };
        *written = Some(Written { canon, next: 0, m });
    }
    let w = written.as_mut().expect("set above");
    for c in &changes[w.next..i] {
        w.m = w.m.with(&c.ops);
    }
    w.next = i;
    Ok(&w.m)
}

/// Stored op `op` as the write op whose subjects are its subjects on `m` (the canon before it):
/// a create under its parent, an update with the lifecycle and the parent it changes, a delete.
fn op_input(m: &Materialized, op: &Op) -> OpInput {
    match op {
        Op::Create { block } => OpInput::Create {
            block_type: block.block_type.clone(),
            id: Some(block.id.to_string()),
            parent: block.parent.as_ref().map(ToString::to_string),
            after: None,
            before: None,
            fields: BTreeMap::new(),
            body: String::new(),
            links: BTreeMap::new(),
        },
        Op::Update { block, .. } => {
            let old = m.get(&block.id);
            let (mut fields, mut unset) = (BTreeMap::new(), vec![]);
            if old.and_then(|o| lifecycle(o)) != lifecycle(block) {
                match lifecycle(block) {
                    Some(l) => {
                        fields.insert("lifecycle".to_string(), json_of(l));
                    }
                    None => unset.push("lifecycle".to_string()),
                }
            }
            let move_to = old.filter(|o| o.parent != block.parent).map(|_| MoveTo {
                parent: block.parent.as_ref().map(ToString::to_string),
                after: None,
                before: None,
            });
            OpInput::Update {
                id: block.id.to_string(),
                base: None,
                fields,
                unset,
                body: None,
                links: None,
                block_type: None,
                move_to,
            }
        }
        Op::Delete { id, .. } => OpInput::Delete {
            id: id.to_string(),
            base: None,
        },
    }
}

/// A block's own `lifecycle`, if it has one (a null value counts as none).
fn lifecycle(b: &Block) -> Option<&FieldValue> {
    b.fields
        .get("lifecycle")
        .filter(|v| **v != FieldValue::Null)
}

/// A lifecycle value as a write op gives it.
fn json_of(v: &FieldValue) -> serde_json::Value {
    match v.as_str() {
        Some(s) => serde_json::Value::String(s.to_string()),
        None => serde_json::Value::Null,
    }
}
