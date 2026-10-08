//! Coordination objects (plan M2d1): intents and alternative sets. Each is a chain of versions;
//! the first version's hash is the object's stable id, and the store keeps one ref per stable id
//! naming the latest version.
use crate::{BlockId, Hash, Provenance};
use serde::{Deserialize, Serialize};

/// An intent version (kind INTENT). Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// The first version's hash; `None` on the first version.
    pub stable: Option<Hash>,
    /// The version this one replaces; `None` on the first version.
    pub prev: Option<Hash>,
    /// The actor who posted the intent.
    pub actor: String,
    /// The poster's run, if any.
    pub run: Option<String>,
    /// The view the work happens in.
    pub view: String,
    /// The pack area the intent covers, if any.
    pub area: Option<String>,
    /// The blocks the intent covers.
    pub blocks: Vec<BlockId>,
    /// What the poster means to do, in a few words.
    pub angle: String,
    /// When the intent expires (expiry is read-time).
    pub expires_at_ms: i64,
    /// Open or withdrawn.
    pub status: IntentStatus,
    /// When this version was written.
    pub at_ms: i64,
}

impl Intent {
    /// The intent's stable id, given this version's own hash `own`.
    pub fn stable_id(&self, own: Hash) -> Hash {
        self.stable.unwrap_or(own)
    }
}

/// The stored status of an intent version. Field order is format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentStatus {
    Open,
    Withdrawn,
}

/// One member of an alternative set: a label and the view that holds it. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AltMember {
    pub label: String,
    pub view: String,
}

/// The status of an alternative set version. Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AltStatus {
    Open,
    /// `member` (a view name) was chosen by the landing `landing`.
    Chosen {
        member: String,
        landing: Hash,
    },
    Abandoned {
        reason: String,
    },
}

/// An alternative set version (kind ALT_SET). Field order is format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AltSet {
    /// The first version's hash; `None` on the first version.
    pub stable: Option<Hash>,
    /// The version this one replaces; `None` on the first version.
    pub prev: Option<Hash>,
    pub title: String,
    /// The block that states the question the members answer, if any.
    pub question: Option<BlockId>,
    pub members: Vec<AltMember>,
    pub status: AltStatus,
    /// Who wrote this version, and when.
    pub provenance: Provenance,
}

impl AltSet {
    /// The set's stable id, given this version's own hash `own`.
    pub fn stable_id(&self, own: Hash) -> Hash {
        self.stable.unwrap_or(own)
    }
}
