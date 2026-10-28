//! A request for a check: a pure stored value. No writer exists yet.
//!
//! Declaration order is the wire format (postcard's field order and variant index). Unknown keys
//! are refused at every level, including the actor and relay records. The carrier parses no git
//! revision, no check name and no actor text.

use crate::assessment::{MAX_ROWS, strictly_increasing};
use crate::dispute::reason_in_bounds;
use crate::{ActorRun, BlockId, Hash, Provenance, RelayEvidence};
use serde::{Deserialize, Serialize};

/// The most bytes in a check name, a scope or a code repo.
pub const MAX_NAME_BYTES: usize = 256;
/// The most bytes in a code revision.
pub const MAX_REVISION_BYTES: usize = 64;

/// A span of code: a repo and two revisions, kept as written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckCode {
    pub repo: String,
    pub from: String,
    pub to: String,
}

/// What the check looks at. Every part is optional, and the parts may coexist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSubject {
    pub blocks: Vec<BlockId>,
    pub change: Option<Hash>,
    pub version: Option<Hash>,
    pub code: Option<CheckCode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMode {
    Author,
    Independent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRequest {
    pub check: String,
    pub subject: CheckSubject,
    pub scope: String,
    pub case_hash: Hash,
    pub mode: CheckMode,
    pub reason: Option<String>,
    #[serde(deserialize_with = "crate::strict::provenance")]
    pub requested_by: Provenance,
    #[serde(default, deserialize_with = "crate::strict::actor_run_opt")]
    pub relayed_by: Option<ActorRun>,
    #[serde(default, deserialize_with = "crate::strict::relay_opt")]
    pub relay: Option<RelayEvidence>,
}

impl CheckRequest {
    /// Checks the structure of a decoded request, in wire order. It never normalizes the record.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=MAX_NAME_BYTES).contains(&self.check.len()) {
            return Err("invalid check name");
        }
        let blocks = &self.subject.blocks;
        if blocks.len() > MAX_ROWS || !strictly_increasing(blocks, |b| b) {
            return Err("check blocks are not sorted and distinct");
        }
        if let Some(code) = &self.subject.code
            && (!(1..=MAX_NAME_BYTES).contains(&code.repo.len())
                || !(1..=MAX_REVISION_BYTES).contains(&code.from.len())
                || !(1..=MAX_REVISION_BYTES).contains(&code.to.len()))
        {
            return Err("invalid check code");
        }
        if !(1..=MAX_NAME_BYTES).contains(&self.scope.len()) {
            return Err("invalid check scope");
        }
        if let Some(reason) = &self.reason
            && !reason_in_bounds(reason)
        {
            return Err("invalid check reason");
        }
        if self.relayed_by.is_some() != self.relay.is_some() {
            return Err("check relay fields differ");
        }
        Ok(())
    }
}
