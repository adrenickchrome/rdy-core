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

/// The dispute state of a landing (O22): derived from its disputes and their resolutions, never
/// stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisputeState {
    None,
    Open,
    Upheld,
    Dismissed,
}

impl DisputeState {
    pub fn as_str(self) -> &'static str {
        match self {
            DisputeState::None => "none",
            DisputeState::Open => "open",
            DisputeState::Upheld => "upheld",
            DisputeState::Dismissed => "dismissed",
        }
    }
}

/// O22: each dispute's resolution outcome, None while it is open. `open` when a dispute has no
/// resolution; else `upheld` when any resolution is upheld; else `dismissed` when any is
/// dismissed; else `none` (no dispute, or every one withdrawn).
pub fn landing_state(outcomes: impl IntoIterator<Item = Option<DisputeOutcome>>) -> DisputeState {
    let (mut open, mut upheld, mut dismissed) = (false, false, false);
    for o in outcomes {
        match o {
            None => open = true,
            Some(DisputeOutcome::Upheld) => upheld = true,
            Some(DisputeOutcome::Dismissed) => dismissed = true,
            Some(DisputeOutcome::Withdrawn) => {}
        }
    }
    if open {
        DisputeState::Open
    } else if upheld {
        DisputeState::Upheld
    } else if dismissed {
        DisputeState::Dismissed
    } else {
        DisputeState::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DisputeOutcome::*;

    #[test]
    fn ap2_landing_state_follows_o22() {
        let state = |o: &[Option<DisputeOutcome>]| landing_state(o.iter().copied());
        // no dispute, or every one withdrawn
        assert_eq!(state(&[]), DisputeState::None);
        assert_eq!(
            state(&[Some(Withdrawn), Some(Withdrawn)]),
            DisputeState::None
        );
        // open over upheld over dismissed, in any order
        assert_eq!(state(&[None]), DisputeState::Open);
        assert_eq!(
            state(&[Some(Upheld), Some(Dismissed), None, Some(Withdrawn)]),
            DisputeState::Open
        );
        assert_eq!(
            state(&[Some(Dismissed), Some(Upheld), Some(Withdrawn)]),
            DisputeState::Upheld
        );
        assert_eq!(
            state(&[Some(Withdrawn), Some(Dismissed)]),
            DisputeState::Dismissed
        );
        // the wire names
        let names: Vec<&str> = [
            DisputeState::None,
            DisputeState::Open,
            DisputeState::Upheld,
            DisputeState::Dismissed,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect();
        assert_eq!(names, ["none", "open", "upheld", "dismissed"]);
        assert_eq!(
            serde_yaml_ng::to_string(&DisputeState::Dismissed).unwrap(),
            "dismissed\n"
        );
    }
}
