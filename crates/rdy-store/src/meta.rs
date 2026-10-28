//! The project's metadata refs: `meta/store_features` (the store's feature manifest,
//! `crate::format`), `meta/upgrade` (the last store upgrade's receipt), a dump's historical
//! `meta/store_format`, `meta/repo_id` and `meta/origin`, and a holder's `meta/store_mode`
//! (`crate::holder`). Only the store writes a `meta/` ref: a new store's first batch, a feature's
//! first use, a load's upgrade batch (`Ingest::upgrade`), the upgrade transaction and a clone's
//! origin batch (`Store::put_origin`).
use crate::Store;
use crate::error::StoreError;
use crate::format::{REF_STORE_FEATURES, REF_STORE_FORMAT, REF_UPGRADE, StoreFeatures};
use crate::holder::REF_STORE_MODE;
use crate::store::{StoreReader, batch_open};
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

/// The fsck text of a store with no `meta/repo_id`.
const NO_REPO_ID: &str = "meta/repo_id is missing; load the project from a dump with this rdy";

/// The meta refs this rdy knows (closed), in the order the fsck text names them.
const KNOWN: [&str; 6] = [
    REF_STORE_FEATURES,
    REF_UPGRADE,
    REF_STORE_FORMAT,
    REF_REPO_ID,
    REF_ORIGIN,
    REF_STORE_MODE,
];

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

    /// The store's features: the log's `meta/store_features` manifest. A store with none, or an
    /// invalid one, is refused (E_STORE_FORMAT).
    pub fn store_features(&self) -> Result<StoreFeatures, StoreError> {
        match self.get_ref(REF_STORE_FEATURES)? {
            Some(v) => crate::format::decode_features(&v),
            None => Err(crate::format::ManifestError("invalid JSON".into()).store_error()),
        }
    }

    /// Where this project was cloned from; None for a project that is not a clone. A value
    /// that does not decode is `StoreError::Decode`.
    pub fn origin(&self) -> Result<Option<Origin>, StoreError> {
        self.get_ref(REF_ORIGIN)?
            .map(|v| decode_origin(&v).ok_or_else(|| bad("meta/origin does not decode")))
            .transpose()
    }

    /// fsck (closed), checked in this order (O22): (1) no `meta/` ref other than the six, in
    /// name order; then `meta/store_features` holds a valid manifest and `meta/upgrade`, when
    /// present, a valid receipt; (2) `meta/repo_id`, when present, holds 16 bytes; (3)
    /// `meta/repo_id` is present
    /// (RULING F2: a load gives an older log one); (4) `meta/origin`, when present, decodes;
    /// (5) it names this repo id when `fork` is false and another one when it is true; (6) every
    /// ref of the indexed log meets the rules of the mode the log's first batch set, in log
    /// order (`crate::holder`); (7) that mode is the index's (`meta/store_mode`), and a holder's
    /// refs are all in its families. The first failure is returned. Part of
    /// `check_objects`; a clone runs it alone before it writes its origin (ruling task-5).
    pub fn check_meta(&self) -> Result<(), StoreError> {
        for (name, _) in self.refs_with_prefix(META_REF)? {
            if !KNOWN.contains(&name.as_str()) {
                return Err(bad(format!(
                    "{name:?} is not a meta ref this rdy knows (meta/store_features, meta/upgrade, meta/store_format, meta/repo_id, meta/origin, meta/store_mode)"
                )));
            }
        }
        if let Some(v) = self.get_ref(REF_STORE_FEATURES)? {
            StoreFeatures::parse(&v)
                .map_err(|e| bad(format!("meta/store_features is not a valid manifest: {e}")))?;
        }
        if let Some(v) = self.get_ref(REF_UPGRADE)? {
            crate::format::UpgradeReceipt::parse(&v)
                .map_err(|e| bad(format!("meta/upgrade is not a valid receipt: {e}")))?;
        }
        let Some(r) = self.repo_id()? else {
            return Err(bad(NO_REPO_ID));
        };
        if let Some(o) = self.origin()? {
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
        let creation = self.check_ref_history()?;
        self.check_mode_against(creation)
    }
}

impl Store {
    /// The clone's batch (D-C8C70, O23): `meta/origin`, and for a fork a new `meta/repo_id`
    /// first, in one `write` of `ref_items`: one marker, one sync. It writes with held batches
    /// off and puts them back as they were after, so no TRANSITION frame joins the batch and no
    /// batch stays open. A batch already open is refused (`StoreError::Refused`); so is a clone
    /// (no new repo id) of a store with no repo id. Returns the repo id after it.
    pub fn put_origin(
        &mut self,
        origin: &Origin,
        new_repo_id: Option<RepoId>,
    ) -> Result<RepoId, StoreError> {
        if self.open_batch().is_some() {
            return Err(batch_open("before the origin batch"));
        }
        let after = match new_repo_id {
            Some(id) => id,
            None => self.reader().repo_id()?.ok_or_else(|| bad(NO_REPO_ID))?,
        };
        let mut refs = Vec::with_capacity(2);
        if let Some(id) = &new_repo_id {
            refs.push((REF_REPO_ID.to_string(), encode_repo_id(id)));
        }
        refs.push((REF_ORIGIN.to_string(), encode_origin(origin)));
        let items = self.ref_items_pub(refs);
        let held = self.held();
        // no batch is open, so turning held batches off seals nothing
        self.set_held(false)?;
        let wrote = self.write(items);
        self.set_held(held)?;
        wrote?;
        Ok(after)
    }
}
