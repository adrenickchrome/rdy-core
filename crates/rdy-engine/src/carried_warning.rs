//! The carried-removal warning (D-QF2HQ: "rdy status and rdy fsck repeat it while a span with
//! held: true still holds its payload"; D-0VG72: a holder's warning ends at a taken removal).
//!
//! A carried record (kind REDACTION_CARRIED) says a removal that another store approved covers
//! frames this store holds again. The warning is derived from the log and the current mask on
//! every read: no acknowledgement and no reminder is stored, and nothing here requests, approves
//! or applies a removal. In a Project the warning ends at a new local apply that a person
//! approves; in a holder it ends when takes have removed every held payload of the record.

use crate::{E_LIMIT, RdyError, ReadState};
use rdy_model::Hash;
use rdy_model::redaction::RedactionCarried;
use rdy_store::closure::END_OF_LOG;
use rdy_store::holder::StoreMode;
use rdy_store::redaction_carried::{HOLDER_FIX, PROJECT_FIX};
use rdy_store::transition::hex;

/// The code of the warning.
pub const W_STOP_BEFORE_REDACTION: &str = "W_STOP_BEFORE_REDACTION";

/// The fix of `E_REDACTION_BUDGET` when the overview cannot fit the carried warnings.
pub const CARRIED_WARNING_BUDGET_FIX: &str =
    "Increase max_tokens, or run rdy fsck for the complete warning list.";

/// One target of the source request that still has a held copy here, in the redact selector
/// spelling: a block id, `id@full-version-hash`, or `blob:full-manifest-hash`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CarriedTargetIssue {
    pub target: String,
}

/// A carried record whose removal this store does not repeat. Not stored.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CarriedWarning {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    /// The source's removal request.
    #[serde(with = "hex::one")]
    pub request: Hash,
    /// The source's apply record.
    #[serde(with = "hex::one")]
    pub apply: Hash,
    /// The source's landing of that apply.
    #[serde(with = "hex::one")]
    pub landing: Hash,
    /// The carried record in this store.
    #[serde(with = "hex::one")]
    pub carried: Hash,
    /// The source request's targets, in the source's order.
    pub issues: Vec<CarriedTargetIssue>,
}

fn too_many() -> RdyError {
    RdyError::new(
        E_LIMIT,
        "The carried warnings do not fit in memory.",
        "Free memory, then run the command again.",
    )
}

fn fix_of(mode: StoreMode) -> &'static str {
    match mode {
        StoreMode::Project => PROJECT_FIX,
        StoreMode::Holder => HOLDER_FIX,
    }
}

/// The warning of `record` (stored as `carried`) with `held` payloads intact and `removed`
/// spans removed. Message and issues do not depend on the mode.
fn warning(
    mode: StoreMode,
    carried: Hash,
    record: &RedactionCarried,
    held: u64,
    removed: u64,
) -> Result<CarriedWarning, RdyError> {
    let mut issues = Vec::new();
    issues
        .try_reserve_exact(record.targets.len())
        .map_err(|_| too_many())?;
    for t in &record.targets {
        issues.push(CarriedTargetIssue {
            target: crate::redaction::targets::selector_text(t),
        });
    }
    Ok(CarriedWarning {
        code: W_STOP_BEFORE_REDACTION,
        message: format!(
            "This store stops before redaction apply {} of its source, approved by {} at landing {}. It holds {held} removed payloads again, and {removed} stay removed.",
            record.apply, record.approved_by.actor, record.landing
        ),
        fix: fix_of(mode).to_string(),
        request: record.request,
        apply: record.apply,
        landing: record.landing,
        carried,
        issues,
    })
}

/// The load-time report: one warning per newly appended carried record, in the order given,
/// including a record whose spans are all `held: false`. Counts come from the record's own
/// flags. This reports a load; it is not another approval.
pub fn for_load(
    mode: StoreMode,
    records: &[(Hash, RedactionCarried)],
) -> Result<Vec<CarriedWarning>, RdyError> {
    let mut out = Vec::new();
    out.try_reserve_exact(records.len())
        .map_err(|_| too_many())?;
    for (hash, record) in records {
        let held = record.spans.iter().filter(|s| s.held).count() as u64;
        let removed = record.spans.len() as u64 - held;
        out.push(warning(mode, *hash, record, held, removed)?);
    }
    Ok(out)
}

impl ReadState {
    /// One warning per carried record, in physical order, while a span of it with
    /// `held: true` still holds its payload. A record repeated for one source apply warns once.
    /// The counts are physical spans read from the current mask: a partial take or apply lowers
    /// the held count, and the warning ends when it reaches zero.
    pub fn carried_warnings(&self) -> Result<Vec<CarriedWarning>, RdyError> {
        let reader = self.reader();
        let mode = reader.mode()?;
        let mut out: Vec<CarriedWarning> = Vec::new();
        // the source applies met so far, sorted; growth is fallible like the output's
        let mut seen: Vec<Hash> = Vec::new();
        let mut failed: Option<RdyError> = None;
        let visited = reader.visit_carried(END_OF_LOG, &mut |c| {
            let step = (|| {
                match seen.binary_search(&c.record.apply) {
                    Ok(_) => return Ok(()),
                    Err(at) => {
                        seen.try_reserve(1).map_err(|_| too_many())?;
                        seen.insert(at, c.record.apply);
                    }
                }
                let (held, removed) = reader.carried_counts(&c.record)?;
                if held == 0 {
                    return Ok(());
                }
                out.try_reserve(1).map_err(|_| too_many())?;
                out.push(warning(mode, c.hash, &c.record, held, removed)?);
                Ok::<(), RdyError>(())
            })();
            match step {
                Ok(()) => Ok(()),
                Err(e) => {
                    failed = Some(e);
                    Err(rdy_store::StoreError::Refused(
                        "carried warnings stopped".into(),
                    ))
                }
            }
        });
        if let Some(e) = failed {
            return Err(e);
        }
        visited?;
        Ok(out)
    }
}
