//! The citing side of a repo link through the adapter (D-DAPR8, D-FK58A, D-MEMMQ; hub tb-G28CM
//! row 70): the take of one link delivery into this project store, and the revocation of a
//! link's source.
//!
//! The take and its checks are core's (`rdy_engine::foreign_delivery`): core checks the whole
//! stream, compares it with the source's last delivery, checks the caller's file once more and
//! only then seals the new BLOCK versions, the FOREIGN entry and the source's ref as one batch,
//! or writes nothing. This module only hands it the hub's file through the store's one writer:
//! it checks the input's bounds, clones the file handle for the writer and passes the owned
//! outcome back. Every error is core's, unchanged.
//!
//! The file stays the hub's, on success and on failure: core reads it and never unlinks it, and
//! its clone is closed when the call returns. A clone may share the caller's cursor, so the take
//! seeks before each read; a caller must not read, seek or change the file during the call. Link
//! permission, live publications, scheduling, coalescing and retries are the hub's (D-WDKWQ). No
//! CLI or MCP tool takes or exports a delivery.
use crate::store::{Snapshot, StoreHandle};
use rdy_engine::engine::with_writer_result;
use rdy_engine::foreign_delivery::{self, check_take_bounds};
use rdy_engine::{E_IO, RdyError};
use rdy_model::BlockId;
use rdy_model::foreign::{ForeignSet, ForeignSource};
use rdy_store::foreign::SourceView;
use std::fs::File;

pub use rdy_engine::foreign_delivery::{DeliveryTaken, LinkRevoked};

/// Where a revocation comes from: the citing project's `repo_link` block, the hub site that
/// sends it and the time it was sent, in milliseconds since the epoch. Provenance only, never a
/// core actor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSource {
    pub link: BlockId,
    pub site: String,
    pub at_ms: i64,
}

/// Where a delivery comes from, as [`LinkSource`] gives it, and the set it serves: none, the
/// organization or a named team.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliverySource {
    pub link: BlockId,
    pub site: String,
    pub at_ms: i64,
    pub set: Option<ForeignSet>,
}

fn link_source(link: BlockId, site: String, at_ms: i64) -> ForeignSource {
    ForeignSource::Link {
        link: link.as_str().to_string(),
        site,
        at_ms,
    }
}

impl StoreHandle {
    /// Takes the link delivery in `stream` for `source` into this project store: the new BLOCK
    /// versions, the FOREIGN entry and the source's ref in one sealed batch, or nothing (see the
    /// module docs). A delivery that is the same as the source's last one writes nothing and
    /// returns `unchanged: true`. A holder is refused (`this store is a holder`), as are this
    /// project's own repo (`a project cannot cite itself`) and a revoked link (`the repo link is
    /// revoked`). The bounds are checked before the handle is cloned. The call takes `&self`:
    /// writes through a shared handle are ordered by the store's one writer, and a captured
    /// snapshot keeps the state it was taken with.
    pub fn take_delivery(
        &self,
        stream: &File,
        source: DeliverySource,
    ) -> Result<DeliveryTaken, RdyError> {
        let DeliverySource {
            link,
            site,
            at_ms,
            set,
        } = source;
        let source = link_source(link, site, at_ms);
        let len = stream.metadata().map_err(io)?.len();
        check_take_bounds(&source, set.as_ref(), len)?;
        // the writer's closure must own what it reads
        let file = stream.try_clone().map_err(io)?;
        with_writer_result(self.local(), move |rdy| {
            foreign_delivery::take_delivery(rdy, &file, &source, set)
        })
    }

    /// Revokes the source of `source.link` in this project store: a Revoked entry after its
    /// newest entry, sealed as one batch. The source keeps its last view and refuses every later
    /// delivery. A source revoked already writes nothing (`unchanged: true`); a link with no
    /// delivery is `E_NOT_FOUND`. The call takes `&self`, as `take_delivery` does.
    pub fn revoke_link(&self, source: LinkSource) -> Result<LinkRevoked, RdyError> {
        let source = link_source(source.link, source.site, source.at_ms);
        with_writer_result(self.local(), move |rdy| {
            foreign_delivery::revoke_link(rdy, &source)
        })
    }
}

impl Snapshot {
    /// Test hook: the view of the Link source of `link` in this snapshot's foreign cut, `None`
    /// when it has no entry there. No product path reaches it.
    #[doc(hidden)]
    pub fn foreign_source_for_tests(&self, link: &BlockId) -> Result<Option<SourceView>, RdyError> {
        Ok(self.read_state().foreign()?.link_source(link).cloned())
    }
}

/// E_IO: the caller's stream file cannot be read or cloned.
fn io(e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("stream input: {e}"),
        "check that the stream file can be read",
    )
}
