//! Paths: readable names for folders and docs, usable wherever an id is (addendum B §2.4).
use crate::error::*;
use crate::{Materialized, Rdy, Scope};
use rdy_model::BlockId;

pub(crate) fn resolve_in(m: &Materialized, path: &str) -> Result<BlockId, RdyError> {
    m.resolve_path(path).map_err(|miss| {
        let at = miss
            .reached
            .clone()
            .unwrap_or_else(|| "the top level".into());
        if miss.ambiguous.len() > 1 {
            return RdyError::new(
                E_UNKNOWN_BLOCK,
                format!(
                    "{path:?} matches more than one name under {at} ignoring case: {}",
                    miss.ambiguous.join(", ")
                ),
                "write the name with its exact case",
            );
        }
        let fix = if miss.children.is_empty() {
            format!("{at} is empty; list the tree with rdy ls")
        } else {
            format!("names under {at}: {}", miss.children.join(", "))
        };
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no folder or doc at {path:?}"),
            fix,
        )
    })
}

impl Rdy {
    pub fn resolve_path(&mut self, scope: &Scope, path: &str) -> Result<BlockId, RdyError> {
        resolve_in(self.materialized(scope)?, path)
    }

    /// Text matching the id grammar is an id (not checked to exist); anything else is a path.
    pub fn resolve_id(&mut self, scope: &Scope, text: &str) -> Result<BlockId, RdyError> {
        match BlockId::parse(text) {
            Ok(id) => Ok(id),
            Err(_) => self.resolve_path(scope, text),
        }
    }

    pub fn path_of(&mut self, scope: &Scope, id: &BlockId) -> Result<Option<String>, RdyError> {
        let m = self.materialized(scope)?;
        if m.get(id).is_none() {
            return Err(RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id}"),
                "check the id with rdy_read",
            ));
        }
        Ok(m.path_of(id))
    }
}
