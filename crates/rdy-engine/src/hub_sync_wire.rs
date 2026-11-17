//! The sync exchange's wire steps that rdyhub's frozen wire fixture v1 covers (D-H0BZN):
//! discovery, the state step in its publish-upload form, the known step in its push form, and
//! put. Each reuses the publication upload's request, refusal and reply code
//! ([`HttpPublishWire`]): the origin fence, the closed header set, the actor binding, the hub
//! code table, and D-N8PR2's additive reply decoding with its token scan over every key and
//! string. The state and known bodies go as the sync media type.
//!
//! - **State.** The request names this rdy, the stream formats the client reads and writes,
//!   the bound actor and run, and a null site. The reply keeps publish's members and checks,
//!   except that an incompatible feature is known when the real feature registry names it
//!   (D-4XHPH), and no stream format is required here: the exchange checks the one it sends.
//! - **Seams for later steps.** The state request has one place for the named views; the get,
//!   write, outcome and dump steps join [`SyncWire`] when the hub's wire fixture v2 freezes them.
use crate::error::{E_BAD_INPUT, RdyError, actor_unbound, upstream_protocol};
use crate::formats::formats;
use crate::hub_http::HttpHubTransport;
use crate::hub_publish_wire::{
    Discovery, HttpPublishWire, ObjectPut, PublishState, PublishWire, STATE_REQUEST_BYTES,
    SYNC_MEDIA_TYPE, UploadContext, check_sync_state, encode, formats_ok,
};
use crate::hub_types::RequestClaim;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::HISTORY_FORMAT;
use serde::Serialize;
use std::io::Read;

/// The state step's request in its publish-upload form (D-H0BZN, wire fixture v1). `actor`
/// and `run` must be the context claim's; a `None` run is JSON null. `site` is always null for
/// a sync exchange.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SyncStateRequest {
    pub rdy: String,
    pub stream_read: Vec<u32>,
    pub stream_write: Vec<u32>,
    pub actor: String,
    pub run: Option<String>,
    pub site: Option<String>,
    // Seam (8a2): the named views of a state request with views (D-3TBYC) go here.
}

impl SyncStateRequest {
    /// The request of this rdy under `claim`: its version, and Plan A's history stream format
    /// to read and write.
    pub fn new(claim: &RequestClaim) -> SyncStateRequest {
        SyncStateRequest {
            rdy: formats().rdy,
            stream_read: vec![HISTORY_FORMAT],
            stream_write: vec![HISTORY_FORMAT],
            actor: claim.actor.clone(),
            run: claim.run.clone(),
            site: None,
        }
    }
}

/// The sync exchange's wire steps. [`HttpSyncWire`] sends them; tests inject another
/// implementation.
pub trait SyncWire: Send + Sync {
    /// `GET <remote>` with the sync media type.
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError>;
    /// `POST <base>/state`.
    fn state(
        &self,
        cx: &UploadContext,
        request: &SyncStateRequest,
    ) -> Result<PublishState, RdyError>;
    /// `POST <base>/known`, push form: the keys of `keys` that neither the hub's store nor the
    /// quarantine of `push` holds.
    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError>;
    /// `POST <base>/objects/put?push=<push>`: one batch of `len` bytes.
    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError>;
}

/// The sync wire over [`HttpHubTransport::upload`], through the publication upload's code.
#[derive(Debug)]
pub struct HttpSyncWire {
    inner: HttpPublishWire,
}

impl HttpSyncWire {
    pub fn new(transport: HttpHubTransport) -> HttpSyncWire {
        HttpSyncWire {
            inner: HttpPublishWire::new(transport),
        }
    }
}

/// The request's own checks: the context's claim, a version, and 1 to 16 distinct formats.
fn check_request(cx: &UploadContext, r: &SyncStateRequest) -> Result<(), RdyError> {
    if r.actor != cx.claim.actor || r.run != cx.claim.run {
        return Err(actor_unbound());
    }
    if semver::Version::parse(&r.rdy).is_err() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the client version is not a version",
            "send this rdy's version",
        ));
    }
    if !formats_ok(&r.stream_read) || !formats_ok(&r.stream_write) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "the stream formats are not valid",
            "send 1 to 16 distinct stream formats",
        ));
    }
    Ok(())
}

impl SyncWire for HttpSyncWire {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        self.inner.discovery(cx, remote)
    }

    fn state(
        &self,
        cx: &UploadContext,
        request: &SyncStateRequest,
    ) -> Result<PublishState, RdyError> {
        check_request(cx, request)?;
        let json = encode(request, STATE_REQUEST_BYTES)?;
        let mut st = self.inner.state_as(cx, json, SYNC_MEDIA_TYPE)?;
        if !check_sync_state(&mut st) {
            return Err(upstream_protocol());
        }
        Ok(st)
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.inner.known_as(cx, push, keys, SYNC_MEDIA_TYPE)
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.inner.put_objects(cx, push, body, len)
    }
}
