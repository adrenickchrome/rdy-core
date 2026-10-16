//! The project's metadata refs (store format 12): `meta/store_format`, `meta/repo_id` and
//! `meta/origin`. Only the store writes a `meta/` ref: a new store's first batch, a load's
//! upgrade batch (`Ingest::upgrade`) and a clone's origin batch (`Store::put_origin`, Task 5).
use crate::error::StoreError;
use crate::format::REF_STORE_FORMAT;
use crate::store::StoreReader;
use rdy_model::{Hash, RepoId};
use serde::{Deserialize, Serialize};

/// The prefix of every metadata ref.
pub const META_REF: &str = "meta/";
/// The ref holding the project's repo id: its 16 bytes.
pub const REF_REPO_ID: &str = "meta/repo_id";
/// The ref holding a clone's origin (`Origin`, postcard).
pub const REF_ORIGIN: &str = "meta/origin";
/// The first store format whose stores hold a repo id.
pub const FIRST_REPO_ID_STORE_FORMAT: u32 = 12;

/// The meta refs this rdy knows (closed), in the order the fsck text names them.
const KNOWN: [&str; 3] = [REF_STORE_FORMAT, REF_REPO_ID, REF_ORIGIN];

/// Where a clone came from (D-C8C70): the source's repo id, its newest version and its canon
/// head when the clone was made. `fork`: the clone took a new repo id (D-DAGNB `forked_from`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub fork: bool,
    pub repo_id: RepoId,
    pub tag: Option<String>,
    pub landing: Option<Hash>,
}

/// The value of `meta/repo_id`: the id's 16 bytes.
pub fn encode_repo_id(id: &RepoId) -> Vec<u8> {
    id.0.to_vec()
}

/// A `meta/repo_id` value: exactly 16 bytes, else None.
pub fn decode_repo_id(v: &[u8]) -> Option<RepoId> {
    <[u8; 16]>::try_from(v).ok().map(RepoId)
}

/// The value of `meta/origin`: postcard.
pub fn encode_origin(o: &Origin) -> Vec<u8> {
    postcard::to_allocvec(o).expect("an origin always encodes")
}

/// A `meta/origin` value: the whole value decodes, else None.
pub fn decode_origin(v: &[u8]) -> Option<Origin> {
    match postcard::take_from_bytes::<Origin>(v) {
        Ok((o, [])) => Some(o),
        _ => None,
    }
}

fn bad(m: impl Into<String>) -> StoreError {
    StoreError::Decode(m.into())
}

impl StoreReader {
    /// The project's repo id; None for a store that has none. A value that is not 16 bytes is
    /// `StoreError::Decode`.
    pub fn repo_id(&self) -> Result<Option<RepoId>, StoreError> {
        self.get_ref(REF_REPO_ID)?
            .map(|v| {
                decode_repo_id(&v)
                    .ok_or_else(|| bad(format!("meta/repo_id holds {} bytes, not 16", v.len())))
            })
            .transpose()
    }

    /// Where this project was cloned from; None for a project that is not a clone. A value
    /// that does not decode is `StoreError::Decode`.
    pub fn origin(&self) -> Result<Option<Origin>, StoreError> {
        self.get_ref(REF_ORIGIN)?
            .map(|v| decode_origin(&v).ok_or_else(|| bad("meta/origin does not decode")))
            .transpose()
    }

    /// fsck (closed), checked in this order (O22): (1) no `meta/` ref other than the three, in
    /// name order; (2) `meta/repo_id`, when present, holds 16 bytes; (3) Task 2 adds: `meta/repo_id`
    /// is present; (4) `meta/origin`, when present, decodes; (5) it names this repo id when `fork`
    /// is false and another one when it is true. The first failure is returned.
    pub(crate) fn check_meta(&self) -> Result<(), StoreError> {
        for (name, _) in self.refs_with_prefix(META_REF)? {
            if !KNOWN.contains(&name.as_str()) {
                return Err(bad(format!(
                    "{name:?} is not a meta ref this rdy knows (meta/store_format, meta/repo_id, meta/origin)"
                )));
            }
        }
        let repo = self.repo_id()?;
        let Some(o) = self.origin()? else {
            return Ok(());
        };
        if let Some(r) = repo {
            if !o.fork && o.repo_id != r {
                return Err(bad(format!(
                    "meta/origin names repo {} for a clone, but meta/repo_id is {r}",
                    o.repo_id
                )));
            }
            if o.fork && o.repo_id == r {
                return Err(bad(format!(
                    "meta/origin names this repo {r} as the repo it was forked from"
                )));
            }
        }
        Ok(())
    }
}
