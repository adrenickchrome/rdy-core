//! ap4 (D-ZWP7Q, D-TF78H): the live authority policy, its owners and the policy role an item
//! needs. Pure reads over the state a caller passes. The policy itself is always canon's at its
//! head; the blocks an item covers read in canon and, for a version's `approve` item, in its
//! prospective canon ([`roles::required_role`]).
pub mod init;
pub mod owners;
pub mod roles;

use crate::{Materialized, ReadState};
use rdy_model::assessment::PolicyRef;
use rdy_model::{BlockId, Hash};
use rdy_schema::policy::{POLICY_TYPE, PolicyFields, policy_fields};

/// The live policy: the authority_policy block of canon at its head, the version of it there,
/// and its fields.
///
/// `fields` is None when the block no longer validates (`policy_fields` None), for example after a
/// pack change removed an area it names. Routing and roles then treat it as no policy; the verdict
/// needs a person with the reason `the authority policy <ap-id> does not validate` (O8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivePolicy {
    pub block: BlockId,
    pub version: Hash,
    pub fields: Option<PolicyFields>,
}

impl LivePolicy {
    /// The block and version, as a landing and an assessment record them.
    pub fn reference(&self) -> PolicyRef {
        PolicyRef {
            block: self.block.clone(),
            version: self.version,
        }
    }

    /// `<ap-id>@<short>`: the block id and the 12-hex short of its version.
    pub fn label(&self) -> String {
        format!("{}@{}", self.block, self.version.short())
    }
}

/// The live policy: the block of type authority_policy in canon at canon's head. Every caller passes
/// canon's head as `canon`: at propose, at the fresh card, at landing, in routing and at approval.
/// Readiness never reads the policy from the candidate or a view (Plan additions, "One verdict").
/// None when canon holds no such block.
///
/// Keys are unique, so canon holds at most one (O1); were a damaged store to hold more, the first
/// in block id order is read.
pub fn live_policy(st: &ReadState, canon: &Materialized) -> Option<LivePolicy> {
    let block = canon.iter().find(|b| b.block_type == POLICY_TYPE)?;
    let version = canon.version_of(&block.id)?;
    Some(LivePolicy {
        block: block.id.clone(),
        version,
        fields: policy_fields(st.pack(), block),
    })
}
