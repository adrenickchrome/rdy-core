//! Semantic tensions (plan M2e1): a pair of blocks at given contents, and the versions of a tension a reviewer's verdict opened.
use crate::{ActorRun, BlockId, Hash, Provenance, RelayEvidence};
use serde::{Deserialize, Serialize};

/// One side of a pair: a block at a given content. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PairItem {
    pub block: BlockId,
    pub content: Hash,
}

/// The key of a pair: the same for its items in any order, and different when a content differs.
pub fn pair_key(items: &[PairItem]) -> Hash {
    let mut sorted: Vec<&PairItem> = items.iter().collect();
    sorted.sort();
    let mut bytes = b"rdy-pair-1\0".to_vec();
    for i in sorted {
        bytes.extend_from_slice(i.block.to_string().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(i.content.to_string().as_bytes());
        bytes.push(0);
    }
    Hash::of(&bytes)
}

/// A semantic tension version (kind TENSION). The first version comes with the verdict that opened it, with the reviewer's advice if any; the only later version is a person's acceptance of that advice (D-WMCFW). Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTension {
    /// The pair key (`pair_key` of `items`).
    pub key: Hash,
    /// The two blocks at the contents the verdict judged.
    pub items: Vec<PairItem>,
    /// The review whose `tension` verdict opened the tension.
    pub review: Hash,
    /// The reviewer's reason that the tension should block, when the verdict advised it.
    pub advice: Option<String>,
    /// Whether the tension blocks: set only by a person's acceptance of the advice.
    pub blocking: bool,
    /// The person's words, as relayed, when an acceptance was relayed.
    pub relay: Option<RelayEvidence>,
    /// The agent that relayed the acceptance at the command line.
    pub relayed_by: Option<ActorRun>,
    /// The version this one follows; `None` for the first.
    pub prev: Option<Hash>,
    /// Who wrote this version, in which run and role, and when.
    pub provenance: Provenance,
}
