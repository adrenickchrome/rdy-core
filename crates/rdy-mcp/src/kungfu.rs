//! The kungfu check (FW-MH.kungfu, D-Y18E1): whether a call holds the value of the ruleset that
//! owns its tool. It warns and counts. It never changes what a call does.
//!
//! [`Check::of`] removes the value from a call's arguments once, as the call enters, and keeps only
//! what it was. [`evaluate`] runs once per request, after the request's claim succeeds and before
//! the action: it counts a miss and decides what the reply owes. [`finish`] adds that to the
//! complete reply, after the action has used its budget, and marks a warning as sent only when a
//! reply carried it.
use crate::claims::{Key, Registry};
use crate::params::{Kungfu, take_kungfu};
use crate::tools::{McpState, W_KUNGFU, add_reminder, error_json};
use rdy_engine::RdyError;
use rdy_engine::embedded_guide;
use rdy_engine::instructions::{REMINDER, REMINDER_KEY};
use rdy_engine::kungfu::{self as table, Owner};
use rdy_engine::kungfu_counts::Case;
use rmcp::model::JsonObject;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

/// The owner that a due reminder speaks for: while it is due, its calls carry that and no warning.
const BOOTSTRAP: &str = "bootstrap";

/// The most bytes the advisory adds to a reply's compact JSON body, with its key and separators.
pub const ADVISORY_BYTES: usize = 512;

/// The bit of the owner named `name`, with no `rdy:` prefix, in the two sets of
/// [`crate::claims::Settings`]. A ruleset that owns no tool has none.
pub fn bit_of(name: &str) -> Option<u16> {
    table::owners()
        .iter()
        .position(|o| o.name == name)
        .map(|i| 1 << i)
}

/// The `W_KUNGFU` warning of a miss on `owner`'s tool.
pub fn warning(owner: &Owner, case: Case) -> Value {
    let message = match case {
        Case::NotLoaded => "This call has no kungfu value: you have not loaded this ruleset.",
        Case::Lost => "This call has no kungfu value: you may have lost your instructions.",
        Case::Wrong => {
            "This call's kungfu value is not current: your instructions may be out of date."
        }
    };
    let ruleset = format!("rdy:{}", owner.name);
    let fix = if owner.name == BOOTSTRAP {
        "Call rdy_instructions and re-read it".to_string()
    } else {
        format!("Call rdy_instructions {{load: [\"{ruleset}\"]}} and re-read it before you go on.")
    };
    json!({"code": W_KUNGFU, "message": message, "fix": fix, "ruleset": ruleset})
}

/// What a reply owes after a miss.
enum Advice {
    /// `W_KUNGFU`, marked as sent for `key` when a complete reply carries it.
    Warn {
        key: Key,
        owner: &'static Owner,
        bit: u16,
        case: Case,
    },
    /// The due reminder, in place of the warning. It marks nothing.
    Remind,
}

/// One evaluation per request, however many claimed steps the request takes.
#[derive(Default)]
struct Latch {
    evaluated: bool,
    advice: Option<Advice>,
}

/// One request's value, as its call entered.
#[derive(Clone)]
pub(crate) struct Check {
    /// The tool's owner and its bit; `None` for an exempt or unknown tool.
    owned: Option<(&'static Owner, u16)>,
    value: Kungfu,
    latch: Arc<Mutex<Latch>>,
}

impl Check {
    /// Removes the value from `args`, for `tool`.
    pub(crate) fn of(tool: &str, args: &mut Option<JsonObject>) -> Check {
        let owned = table::policy(tool)
            .and_then(|p| p.ruleset)
            .and_then(|handle| handle.strip_prefix("rdy:"))
            .and_then(|name| {
                let i = table::owners().iter().position(|o| o.name == name)?;
                Some((&table::owners()[i], 1 << i))
            });
        let expected = owned.map(|(o, _)| o.value.as_str());
        let value = args
            .as_mut()
            .map_or(Kungfu::Missing, |a| take_kungfu(a, expected));
        Check {
            owned,
            value,
            latch: Default::default(),
        }
    }

    fn latch(&self) -> std::sync::MutexGuard<'_, Latch> {
        self.latch.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The check of one request, under its claim: the state's session is the call's. A later step of
/// the same request does nothing. A correct value clears the owner's mark and delivers nothing.
/// A miss counts, always; a failed count changes nothing.
pub(crate) fn evaluate(st: &McpState, check: &Check) {
    let Some(session) = st.session.as_ref() else {
        return;
    };
    let key = (session.actor.clone(), session.run.clone());
    evaluate_for(st.hub.claims(), &key, || st.kungfu_counts.clone(), check);
}

/// [`evaluate`] for a caller that has no state: the claims registry, the call's `key`, and the
/// recorder of the project that owns the count, made only when a miss needs it. A server with no
/// project passes the meta project's recorder; making it opens no store.
pub(crate) fn evaluate_for(
    claims: &Registry,
    key: &Key,
    recorder: impl FnOnce() -> Option<rdy_engine::kungfu_counts::Recorder>,
    check: &Check,
) {
    let Some((owner, bit)) = check.owned else {
        return;
    };
    let mut latch = check.latch();
    if std::mem::replace(&mut latch.evaluated, true) {
        return;
    }
    let marks = claims.kungfu_marks(key);
    let case = match check.value {
        Kungfu::Correct => {
            if marks.warned & bit != 0 {
                claims.clear_kungfu_warned(key, bit);
            }
            return;
        }
        Kungfu::Wrong => Case::Wrong,
        Kungfu::Missing if marks.received & bit != 0 => Case::Lost,
        Kungfu::Missing => Case::NotLoaded,
    };
    if let Some(recorder) = recorder() {
        recorder.record(&key.0, key.1.as_deref(), owner.name, case);
    }
    latch.advice = if owner.name == BOOTSTRAP && !marks.read {
        Some(Advice::Remind)
    } else if marks.warned & bit != 0 {
        None
    } else {
        Some(Advice::Warn {
            key: key.clone(),
            owner,
            bit,
            case,
        })
    };
}

/// The complete reply `out`, with what the request owes: the value, or a refusal's JSON. The
/// action's result, code, fix and warnings stay as they are, and the advisory goes last. A reply
/// that is not an object cannot carry a warning: it stays as it is, the mark stays clear, and the
/// next object reply of that owner carries the warning.
pub(crate) fn finish(
    claims: &Registry,
    check: Option<&Check>,
    out: Result<Value, RdyError>,
) -> Result<Value, Value> {
    let advice = check.and_then(|c| c.latch().advice.take());
    match (advice, out) {
        (None, Ok(v)) => Ok(v),
        (None, Err(e)) => Err(error_json(&e)),
        (Some(Advice::Remind), Ok(mut v)) => {
            add_reminder(&mut v);
            Ok(v)
        }
        (Some(Advice::Remind), Err(e)) => {
            let mut v = error_json(&e);
            if let Some(m) = v.as_object_mut() {
                m.insert(REMINDER_KEY.into(), REMINDER.into());
            }
            Err(v)
        }
        (
            Some(Advice::Warn {
                key,
                owner,
                bit,
                case,
            }),
            Ok(mut v),
        ) => {
            if v.is_object() && claims.mark_kungfu_warned(&key, bit) {
                crate::claims::append_warning(&mut v, warning(owner, case));
            }
            Ok(v)
        }
        (
            Some(Advice::Warn {
                key,
                owner,
                bit,
                case,
            }),
            Err(mut e),
        ) => {
            if claims.mark_kungfu_warned(&key, bit) {
                e.warnings
                    .get_or_insert_default()
                    .push(warning(owner, case));
            }
            Err(error_json(&e))
        }
    }
}

/// The owners whose every built-in row a complete instructions reply holds. A menu entry is no
/// delivery, and a stored row is no built-in row.
pub(crate) fn received_bits(reply: &Value) -> u16 {
    let rows: BTreeSet<(&str, &str)> = reply["instructions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["layer"] == "rdy")
        .filter_map(|r| Some((r["ruleset"].as_str()?, r["id"].as_str()?)))
        .collect();
    let mut bits = 0;
    for (i, owner) in table::owners().iter().enumerate() {
        let handle = format!("rdy:{}", owner.name);
        let whole = embedded_guide::RULESETS
            .iter()
            .find(|set| set.name == owner.name)
            .is_some_and(|set| {
                set.rows
                    .iter()
                    .all(|r| rows.contains(&(handle.as_str(), r.id)))
            });
        if whole {
            bits |= 1 << i;
        }
    }
    bits
}

/// A complete instructions reply `reply` went out for `key`: the reminder is paid, and the rulesets
/// it holds are received, with their warning marks clear.
pub(crate) fn note_delivered(claims: &Registry, key: &Key, reply: &Value) {
    claims.mark_instructions_read(key);
    claims.mark_kungfu_received(key, received_bits(reply));
}
