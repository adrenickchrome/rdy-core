//! Removals through sync (D-FNRXT, D-CC90E, D-3M81W): the uncovered-carrier warning and the
//! removed content a pull replay must not copy.
//!
//! A fetch takes each removal of the history it brings when a person's approval proves it, and
//! erases what that removal covers. A local change that holds a removed block version, and that
//! no removal covers, keeps its bytes. [`ReadState::uncovered_warnings`] warns once per such
//! change, on every read: it is derived from the store's mask and producers, so no
//! acknowledgement is stored, and it repeats after a reopen or a reindex. The warning ends when
//! a person approves a removal here that covers the change.
//!
//! [`removed_in`] lists the removals that hold content of a landing's changes. A pull replay
//! stops at such a landing (`StopReason::Removal`), and the `pull-<n>` view never gets those
//! changes: their text is never copied into a new change or a conflict.
use crate::error::{SYNC_UNCOVERED_FIX, SYNC_UNCOVERED_MESSAGE, W_REDACTION_UNCOVERED};
use crate::{RdyError, ReadState};
use rdy_model::{Hash, LandingRecord, Op};
use rdy_store::StoreReader;
use rdy_store::kinds;
use rdy_store::transition::hex;
use std::collections::BTreeSet;

/// A local change that holds content a removal removed, and that no removal covers. Not stored.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct UncoveredWarning {
    pub code: &'static str,
    pub message: String,
    pub fix: String,
    /// The local change that keeps its bytes.
    #[serde(with = "hex::one")]
    pub change: Hash,
    /// The removal request that removed the content.
    #[serde(with = "hex::one")]
    pub removal: Hash,
}

/// The warning of change `change`, whose content removal `removal` removed.
pub fn uncovered_warning(change: Hash, removal: Hash) -> UncoveredWarning {
    UncoveredWarning {
        code: W_REDACTION_UNCOVERED,
        message: SYNC_UNCOVERED_MESSAGE
            .replace("<change>", &change.to_string())
            .replace("<removal>", &removal.to_string()),
        fix: SYNC_UNCOVERED_FIX.to_string(),
        change,
        removal,
    }
}

impl ReadState {
    /// One warning per uncovered local carrier, in ascending change order (see the module docs).
    pub fn uncovered_warnings(&self) -> Result<Vec<UncoveredWarning>, RdyError> {
        Ok(self
            .reader()
            .uncovered_carriers()?
            .into_iter()
            .map(|(change, removal)| uncovered_warning(change, removal))
            .collect())
    }
}

/// The removals that hold content of landing `l`'s changes, in ascending order: the request
/// that removed a change, or a block version one of its ops produces. Empty when none does.
pub fn removed_in(reader: &StoreReader, l: &LandingRecord) -> Result<Vec<Hash>, RdyError> {
    let mut out: BTreeSet<Hash> = BTreeSet::new();
    for c in &l.changes {
        if let Some(r) = reader.redaction_of(kinds::CHANGE, c)? {
            out.insert(r);
            continue;
        }
        // a carrier holds no removed text: its request is the landing's own removal
        if reader.redaction_for_carrier(c)?.is_some() {
            continue;
        }
        for op in &reader.change(c)?.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op
                && let Some(r) = reader.redaction_of(kinds::BLOCK, &block.version_hash())?
            {
                out.insert(r);
            }
        }
    }
    Ok(out.into_iter().collect())
}
