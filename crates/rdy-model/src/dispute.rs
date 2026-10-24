//! A dispute of a landing, and the record that resolves it: pure stored values.
//!
//! Declaration order is the wire format (postcard's field order and variant index). Unknown keys
//! are refused, at every level, including the actor records. Actor text stays as it was written: no input grammar applies here.

use crate::{Hash, Provenance};
use serde::{Deserialize, Serialize};

/// The most Unicode scalar values in a reason, after trimming.
pub const MAX_REASON_CHARS: usize = 2_000;
/// The most bytes in a reason.
pub const MAX_REASON_BYTES: usize = 8_000;

/// Whether `reason` is 1 to 2,000 Unicode scalar values after trim, and at most 8,000 bytes.
/// The record keeps the reason as written.
pub(crate) fn reason_in_bounds(reason: &str) -> bool {
    let chars = reason.trim().chars().count();
    (1..=MAX_REASON_CHARS).contains(&chars) && reason.len() <= MAX_REASON_BYTES
}

/// A person's dispute of one landing. `at_ms` need not equal `opened_by.at_ms`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dispute {
    pub landing: Hash,
    #[serde(deserialize_with = "crate::strict::provenance")]
    pub opened_by: Provenance,
    pub reason: String,
    pub at_ms: i64,
}

impl Dispute {
    /// Checks the reason bound. It never normalizes the record.
    pub fn validate(&self) -> Result<(), &'static str> {
        if reason_in_bounds(&self.reason) {
            Ok(())
        } else {
            Err("invalid dispute reason")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisputeOutcome {
    Upheld,
    Dismissed,
    Withdrawn,
}

/// How a dispute ended. `correction` names the object that fixed the landing, when one did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisputeResolution {
    pub dispute: Hash,
    pub outcome: DisputeOutcome,
    #[serde(deserialize_with = "crate::strict::provenance")]
    pub by: Provenance,
    pub correction: Option<Hash>,
}

impl DisputeResolution {
    /// A resolution has no free text and no structural rule beyond its types.
    pub fn validate(&self) -> Result<(), &'static str> {
        Ok(())
    }
}
