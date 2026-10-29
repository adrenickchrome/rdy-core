//! Readiness check 1 (D-ME2HD item 1, O3): each change of a version that touches stable content
//! records a briefing that was current when it was written.
//!
//! A change needs no briefing when (closed): (a) it touches no stable content, by the subjects a
//! write's briefing check reads (`brief::stable_subjects`), on canon at the version's
//! `canon_at`; (b) `rdy import` wrote it (`links.imported_by`); (c) the log holds it before the
//! store's first change that records a briefing (none yet: every change is before it); (d) it
//! is a replay (`links.replayed_from`) of a change that (b) or (c) exempts. Every other change
//! with no briefing is one `E_BRIEF_REQUIRED` issue, on its first stable subject.
//!
//! rdy's own writers record their briefing: the propose's key change and a resolve to the landed
//! side brief themselves, as a repin does, and a replay copies its source's.

use crate::brief::{E_BRIEF_REQUIRED, stable_subjects};
use crate::error::RdyError;
use crate::write::{MoveTo, OpInput};
use crate::{Materialized, ReadState};
use rdy_model::{Block, Change, FieldValue, Op, Proposal};
use rdy_schema::{Issue, Severity};
use std::collections::BTreeMap;

/// The fix of a stable change with no briefing (O3).
pub const BRIEFING_FIX: &str =
    "write it again with a current briefing: rdy_brief, then rdy_write / rdy edit";

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
    let before_first =
        |c: &Change| -> Result<bool, RdyError> { Ok(reader.change_location(&c.id())? < first) };
    let exempt = |c: &Change| -> Result<bool, RdyError> {
        Ok(c.links.imported_by.is_some() || before_first(c)?)
    };
    // the canon the version recorded, read only for a change that needs a briefing
    let mut canon: Option<std::sync::Arc<Materialized>> = None;
    let mut issues = vec![];
    for c in changes {
        if c.links.briefing.is_some() || exempt(c)? {
            continue;
        }
        if let Some(src) = c.links.replayed_from
            && exempt(&reader.change(&src)?)?
        {
            continue;
        }
        let m = match &canon {
            Some(m) => m,
            None => {
                let m = match super::canon_scope(st, p.canon_at) {
                    Some(scope) => st.materialized(&scope)?,
                    // the empty canon holds no stable content
                    None => return Ok(issues),
                };
                canon.insert(m)
            }
        };
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
