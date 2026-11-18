//! The sync ops (D-H0BZN): a push of local canon and named views to a share remote's hub repo,
//! and a pull of hub canon and named views from it. Each runs the engine's sync coordinator
//! ([`SyncService`]) with this crate's renderer for the render inputs a write carries. The
//! coordinator holds the store's writer only for its local steps, never across a request.
//!
//! As the publish helper does, each call builds its HTTPS senders on their first request,
//! fenced to that request's install, and drops them with the call. The user config, and so the
//! credentials, are this process's own. The ops route requests only; their surfaces come later.
use super::publish::{CredentialTransport, fenced};
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::hub_credentials::require_user_file;
use rdy_engine::hub_publish_stream::HistorySent;
use rdy_engine::hub_publish_wire::{Discovery, ObjectPut, UploadContext};
use rdy_engine::hub_sync::{
    PullOutcome, PullRequest, PushOutcome, PushRequest, Resolved, SyncService, WriteEnd,
};
use rdy_engine::hub_sync_wire::{
    GetRequest, HttpSyncWire, KnownPage, KnownPull, LookupOutcome, OutcomeReply, SyncState,
    SyncStateRequest, SyncWire, WriteFailure, WriteReply, WriteRequest,
};
use rdy_engine::hub_types::SystemHubClock;
use rdy_engine::sync_fetch::FetchOutcome;
use rdy_engine::{E_INTERNAL, Engine, RdyError, Session};
use rdy_render::publish::PublicationRenderer;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::KIND_VIEW;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;

/// The push op's arguments: the remote (none for the default), the named views and `force`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PushArgs {
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    views: Vec<String>,
    #[serde(default)]
    force: bool,
}

/// The pull op's arguments: the remote (none for the default), the named views, `take` and
/// `strict`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PullArgs {
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    views: Vec<String>,
    #[serde(default)]
    take: bool,
    #[serde(default)]
    strict: bool,
}

/// The sync wire of one call, built on its first request.
struct LazyWire<'a> {
    project: &'a Path,
    user_file: &'a Path,
    built: OnceLock<Result<HttpSyncWire, RdyError>>,
}

impl LazyWire<'_> {
    fn wire(&self, cx: &UploadContext) -> Result<&HttpSyncWire, RdyError> {
        let built = self.built.get_or_init(|| {
            fenced(self.project, self.user_file, &cx.install).map(HttpSyncWire::new)
        });
        built.as_ref().map_err(Clone::clone)
    }
}

impl SyncWire for LazyWire<'_> {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        self.wire(cx)?.discovery(cx, remote)
    }

    fn state(&self, cx: &UploadContext, request: &SyncStateRequest) -> Result<SyncState, RdyError> {
        self.wire(cx)?.state(cx, request)
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.wire(cx)?.known(cx, push, keys)
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.wire(cx)?.put(cx, push, body, len)
    }

    fn known_pull(&self, cx: &UploadContext, request: &KnownPull) -> Result<KnownPage, RdyError> {
        self.wire(cx)?.known_pull(cx, request)
    }

    fn get(
        &self,
        cx: &UploadContext,
        request: &GetRequest,
        sink: &mut dyn Write,
        limit: u64,
    ) -> Result<u64, RdyError> {
        self.wire(cx)?.get(cx, request, sink, limit)
    }

    fn write(
        &self,
        cx: &UploadContext,
        request: &WriteRequest,
    ) -> Result<WriteReply, WriteFailure> {
        self.wire(cx)
            .map_err(WriteFailure::not_sent)?
            .write(cx, request)
    }

    fn outcome(&self, cx: &UploadContext, push: &str) -> Result<OutcomeReply, RdyError> {
        self.wire(cx)?.outcome(cx, push)
    }
}

/// Runs `f` with the sync coordinator of the project at `root`.
fn with_service<T>(
    root: &Path,
    f: impl FnOnce(&SyncService<'_>) -> Result<T, RdyError>,
) -> Result<T, RdyError> {
    let user_file = require_user_file(user_file_from_env())?;
    let api = CredentialTransport {
        project: root,
        user_file: &user_file,
        built: OnceLock::new(),
    };
    let wire = LazyWire {
        project: root,
        user_file: &user_file,
        built: OnceLock::new(),
    };
    let clock = SystemHubClock;
    let service = SyncService::new(root, &user_file, &api, &clock, &wire, &PublicationRenderer);
    f(&service)
}

/// Pushes for `session` from the project at `root`, and replies with the push's ends.
pub(crate) fn push(
    engine: &dyn Engine,
    root: &Path,
    session: &Session,
    args: PushArgs,
) -> Result<Value, RdyError> {
    let request = PushRequest {
        remote: args.remote,
        views: args.views,
        force: args.force,
    };
    let out = with_service(root, |s| s.push(engine, session, request))?;
    Ok(push_json(&out))
}

/// Pulls for `session` into the project at `root`, and replies with the pull's fetches and
/// replays.
pub(crate) fn pull(
    engine: &dyn Engine,
    root: &Path,
    session: &Session,
    args: PullArgs,
) -> Result<Value, RdyError> {
    let request = PullRequest {
        remote: args.remote,
        views: args.views,
        take: args.take,
        strict: args.strict,
    };
    let out = with_service(root, |s| s.pull(engine, session, request))?;
    pull_json(&out)
}

fn sent_json(s: &HistorySent) -> Value {
    json!({"batches": s.batches, "records": s.records, "bytes": s.bytes})
}

fn end_json(end: &WriteEnd) -> Value {
    match end {
        WriteEnd::UpToDate { tip } => json!({
            "status": "up_to_date",
            "tip": tip.map(|h| h.to_string()),
        }),
        WriteEnd::Taken {
            push,
            base,
            head,
            tip,
            resumed,
            sent,
            min_rdy_raised,
        } => json!({
            "status": "taken",
            "push": push,
            "base": base.map(|h| h.to_string()),
            "head": head.to_string(),
            "tip": tip.to_string(),
            "resumed": resumed,
            "sent": sent_json(sent),
            "min_rdy_raised": min_rdy_raised,
        }),
        WriteEnd::Held {
            push,
            head,
            request,
            resumed,
            sent,
            min_rdy_raised,
        } => json!({
            "status": "held",
            "push": push,
            "head": head.to_string(),
            "request": {"number": request.number, "version": request.version},
            "resumed": resumed,
            "sent": sent_json(sent),
            "min_rdy_raised": min_rdy_raised,
        }),
        WriteEnd::Refused { push, error } => json!({
            "status": "refused",
            "push": push,
            "error": error,
        }),
        WriteEnd::HeldOpen { head, request } => json!({
            "status": "held_open",
            "head": head.to_string(),
            "request": request,
        }),
    }
}

fn lookup_json(r: &Option<Resolved>) -> Value {
    let Some(r) = r else {
        return Value::Null;
    };
    let outcome = match &r.outcome {
        LookupOutcome::Taken { tip } => json!({"outcome": "taken", "tip": tip.hash.to_string()}),
        LookupOutcome::Held { request, tip } => json!({
            "outcome": "held",
            "request": {"number": request.number, "version": request.version},
            "tip": tip.map(|t| t.hash.to_string()),
        }),
        LookupOutcome::Refused { code } => json!({"outcome": "refused", "code": code}),
        LookupOutcome::Unknown => json!({"outcome": "unknown"}),
    };
    json!({
        "push": r.push,
        "kind": if r.kind == KIND_VIEW { "view" } else { "canon" },
        "view": r.view,
        "result": outcome,
        "fetched": r.fetched,
    })
}

fn push_json(out: &PushOutcome) -> Value {
    let views: Vec<Value> = out
        .views
        .iter()
        .map(|(name, end)| json!({"view": name, "end": end_json(end)}))
        .collect();
    json!({
        "remote": out.remote,
        "lookup": lookup_json(&out.lookup),
        "canon": end_json(&out.canon),
        "force_not_needed": out.force_not_needed,
        "views": views,
    })
}

fn internal() -> RdyError {
    RdyError::new(
        E_INTERNAL,
        "the sync result could not be encoded",
        "report this as a bug in rdy",
    )
}

fn fetch_json(f: &FetchOutcome) -> Value {
    json!({
        "records_taken": f.records_taken,
        "tags_taken": f.tags_taken,
        "tags_kept": f.tags_kept,
        "unchanged": f.unchanged,
    })
}

fn pull_json(out: &PullOutcome) -> Result<Value, RdyError> {
    let views_fetched: Vec<Value> = out
        .views_fetched
        .iter()
        .map(|(name, f)| json!({"view": name, "fetched": fetch_json(f)}))
        .collect();
    Ok(json!({
        "remote": out.remote,
        "lookup": lookup_json(&out.lookup),
        "fetched": out.fetched.as_ref().map(fetch_json),
        "canon": serde_json::to_value(&out.canon).map_err(|_| internal())?,
        "views_fetched": views_fetched,
        "views": serde_json::to_value(&out.views).map_err(|_| internal())?,
    }))
}
