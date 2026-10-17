//! A project's repo id and origin as the replies show them (plan Export and clone, ec10;
//! D-DAGNB, D-C8C70, D-KQ4CA): `rdy status`, `rdy --json status` and `rdy_overview` name the repo
//! id and, for a clone or a fork, where it came from. Both read the store's meta refs
//! (`meta/repo_id`, `meta/origin`); nothing here writes.
//!
//! Platform (D-1RXEA): none.
use crate::clone::OriginReply;
use crate::error::{E_INTERNAL, RdyError};
use crate::read_state::ReadState;
use rdy_model::RepoId;
use rdy_store::meta::Origin;
use serde_json::{Map, Value};

impl ReadState {
    /// The project's repo id. `fsck` guarantees a store has one, so a store with none is an
    /// internal error that names the fix.
    pub fn repo_id(&self) -> Result<RepoId, RdyError> {
        self.reader().repo_id()?.ok_or_else(|| {
            RdyError::new(
                E_INTERNAL,
                "the project has no repo id",
                "run rdy fsck, then load the project from a dump with this rdy",
            )
        })
    }

    /// Where this project was cloned or forked from; None for a project that is neither.
    pub fn origin(&self) -> Result<Option<Origin>, RdyError> {
        Ok(self.reader().origin()?)
    }
}

/// The keys a status or an overview gains: `repo_id`, and `origin` for a clone or
/// `forked_from` for a fork when the project has an origin. In an `OriginReply`, `tag` and
/// `landing` are JSON `null` when absent.
pub fn repo_json(st: &ReadState) -> Result<Map<String, Value>, RdyError> {
    let mut out = Map::new();
    out.insert("repo_id".into(), Value::String(st.repo_id()?.to_string()));
    if let Some(o) = st.origin()? {
        let key = if o.fork { "forked_from" } else { "origin" };
        let reply = serde_json::to_value(OriginReply::from(&o))
            .map_err(|e| RdyError::new(E_INTERNAL, e.to_string(), "report this as a bug"))?;
        out.insert(key.into(), reply);
    }
    Ok(out)
}
