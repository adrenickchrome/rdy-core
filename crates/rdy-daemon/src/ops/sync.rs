//! The sync ops (D-H0BZN): a push of local canon and named views to a share remote's hub repo,
//! a pull of hub canon and named views from it, and the restore of a dump into it (D-J3YA0). Each runs the engine's sync coordinator
//! ([`SyncService`]) with this crate's renderer for the render inputs a write carries. The
//! coordinator holds the store's writer only for its local steps, never across a request.
//!
//! A forced push, a restore into a hub repo that holds canon and a take of hub canon each remove
//! landings from a canon, and each needs a person (D-47HVF). The call first reads the engine's
//! preview of the move (no write). When landings leave, or the preview cannot name them all, it
//! returns the card and writes nothing. A call that carries the card's `card_hash` and the
//! person's answer runs the move only when the card is still the current one and the relay
//! check accepts the answer. The ops hold no exchange logic: the card is the only step here.
//!
//! As the publish helper does, each call builds its HTTPS senders on their first request,
//! fenced to that request's install, and drops them with the call. The user config, and so the
//! credentials, are this process's own. The ops route requests only; their surfaces come later.
use super::publish::{CredentialTransport, fenced};
use rdy_engine::answer::relay::{RelayInput, relayed};
use rdy_engine::config_levels::user_file_from_env;
use rdy_engine::engine::SyncCall;
use rdy_engine::hub_credentials::require_user_file;
use rdy_engine::hub_publish_stream::HistorySent;
use rdy_engine::hub_publish_wire::{Discovery, ObjectPut, UploadContext};
use rdy_engine::hub_sync::{
    CanonPreview, PreviewKind, PullOutcome, PullRequest, PushOutcome, PushRequest, Resolved,
    SyncService, WriteEnd,
};
use rdy_engine::hub_sync_wire::{
    GetRequest, HttpSyncWire, KnownPage, KnownPull, LookupOutcome, OutcomeReply, SyncState,
    SyncStateRequest, SyncWire, WriteFailure, WriteReply, WriteRequest,
};
use rdy_engine::hub_types::SystemHubClock;
use rdy_engine::sync_fetch::FetchOutcome;
use rdy_engine::sync_restore::{RestoreOutcome, RestoreRequest};
use rdy_engine::{E_BAD_INPUT, E_INTERNAL, E_ITEM_MOVED, Engine, RdyError, Session};
use rdy_mcp::tools::{AuthAnswerParam, PullInputs, PushInputs, RelayFields, require_writer};
use rdy_render::publish::PublicationRenderer;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::KIND_VIEW;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;

/// The restore op's arguments: the dump (relative to the project root), the remote (none for
/// the default) and the relay inputs of the card's answer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestoreArgs {
    dump: String,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    card_hash: Option<String>,
    #[serde(default)]
    answer: Option<AuthAnswerParam>,
    #[serde(default)]
    relayed_by: Option<String>,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    session_ref: Option<String>,
}

impl From<RestoreArgs> for PushInputs {
    fn from(a: RestoreArgs) -> PushInputs {
        PushInputs {
            remote: a.remote,
            restore: Some(a.dump),
            card_hash: a.card_hash,
            answer: a.answer,
            relayed_by: a.relayed_by,
            evidence: a.evidence,
            question: a.question,
            session_ref: a.session_ref,
            ..PushInputs::default()
        }
    }
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

/// The note of a forced push that found the hub tip in local canon (D-47HVF).
pub(crate) const FORCE_NOT_NEEDED: &str = "force not needed";

/// Who answers a card: the caller's session, the project's person the relay table names, and
/// whether the session is the relay (over MCP).
pub(crate) struct Who<'a> {
    pub(crate) session: &'a Session,
    pub(crate) person: &'a str,
    pub(crate) implicit: bool,
}

/// Which canon move a card names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Move {
    ForcePush,
    Restore,
    Take,
}

impl Move {
    fn word(self) -> &'static str {
        match self {
            Move::ForcePush => "force_push",
            Move::Restore => "restore",
            Move::Take => "take",
        }
    }
}

/// What the card step decided.
enum Step {
    /// Run the move: no landing leaves, or the person confirmed the current card.
    Go,
    /// Reply with this card and write nothing.
    Card(Value),
    /// The person cancelled the current card.
    Cancelled(Value),
}

fn tip_text(tip: &Option<rdy_model::Hash>) -> String {
    tip.map_or_else(|| "none".to_string(), |h| h.to_string())
}

/// The card's text: the remote, the tips before and after, and each landing that leaves canon,
/// each line ending LF. A tip that moves changes the text, and so the hash.
fn render_card(mv: Move, p: &CanonPreview) -> String {
    let (title, whose, what) = match mv {
        Move::ForcePush => (
            "Force the hub's canon",
            "Hub",
            "hub canon moves to this project's canon",
        ),
        Move::Restore => (
            "Restore a dump into the hub's canon",
            "Hub",
            "hub canon moves to the dump's tip",
        ),
        Move::Take => (
            "Take the hub's canon",
            "Local",
            "this project's canon moves to hub canon, and its other landings stay in no canon",
        ),
    };
    let mut out = format!(
        "{title}\nRemote: {}\nMove: {what}\n{whose} canon before: {}\n{whose} canon after: {}\nLeaving canon: {} landings\n",
        p.remote,
        tip_text(&p.before),
        tip_text(&p.after),
        p.leaving.len()
    );
    for l in &p.leaving {
        out.push_str(&format!("  {l}\n"));
    }
    if !p.complete {
        out.push_str("This project does not hold every landing: more may leave canon.\n");
    }
    out.push_str("Confirm only if you asked for this move.\nAnswer: confirm or cancel\n");
    out
}

fn card_hash(card: &str) -> String {
    rdy_model::Hash::of(card.as_bytes()).to_string()
}

/// The card's reply members: the preview and the card with its hash.
fn card_json(mv: Move, p: &CanonPreview, card: &str) -> Value {
    json!({
        "status": "confirmation_required",
        "move": mv.word(),
        "remote": p.remote,
        "before": p.before.map(|h| h.to_string()),
        "after": p.after.map(|h| h.to_string()),
        "leaving": p.leaving.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "complete": p.complete,
        "card": card,
        "card_hash": card_hash(card),
    })
}

/// `E_ITEM_MOVED`: the answered card is not the current one.
fn card_moved(corrected: Option<Value>) -> RdyError {
    let e = RdyError::new(
        E_ITEM_MOVED,
        "the card the person answered is not the current card: the move changed since",
        "show the person the new card, then relay their answer with its card_hash",
    );
    match corrected {
        Some(v) => e.with_corrected(v),
        None => e,
    }
}

/// The card step of one move (see the module docs). A card is needed when landings leave, or
/// when this store cannot name them all. An answer to a card that is not the current one is
/// refused before the relay is read.
fn card_step(
    who: &Who<'_>,
    confirm: &RelayFields,
    mv: Move,
    preview: &CanonPreview,
) -> Result<Step, RdyError> {
    let needed = !preview.leaving.is_empty() || !preview.complete;
    let card = needed.then(|| render_card(mv, preview));
    let Some(given) = confirm.card_hash.as_deref() else {
        return Ok(match card {
            Some(c) => Step::Card(card_json(mv, preview, &c)),
            None => Step::Go,
        });
    };
    let Some(card) = card else {
        return Err(card_moved(None));
    };
    if given != card_hash(&card) {
        return Err(card_moved(Some(card_json(mv, preview, &card))));
    }
    let relay = RelayInput {
        relayed_by: confirm.relayed_by.clone(),
        evidence: confirm.evidence.clone(),
        question: confirm.question.clone(),
        session_ref: confirm.session_ref.clone(),
        kind: Some(rdy_model::EvidenceKind::Card),
    };
    relayed(who.session, who.person, &relay, who.implicit).map_err(|r| r.error())?;
    match confirm.answer {
        Some(AuthAnswerParam::Cancel) => Ok(Step::Cancelled(json!({
            "status": "cancelled",
            "move": mv.word(),
            "remote": preview.remote,
        }))),
        _ => Ok(Step::Go),
    }
}

/// Pushes (or restores) for `session` from the project at `root`, and replies with the push's
/// ends, or with the person's card (see the module docs). A reader is refused first.
pub(crate) fn push(
    engine: &dyn Engine,
    root: &Path,
    who: &Who<'_>,
    inputs: PushInputs,
) -> Result<Value, RdyError> {
    require_writer(who.session, "push")?;
    inputs.check()?;
    let relay = inputs.relay();
    let session = who.session;
    if let Some(dump) = &inputs.restore {
        let request = RestoreRequest {
            dump: vec![root.join(dump)],
            remote: inputs.remote.clone(),
        };
        return with_service(root, |s| {
            let preview = s.restore_preview(session, request.clone())?;
            match card_step(who, &relay, Move::Restore, &preview)? {
                Step::Card(v) | Step::Cancelled(v) => return Ok(v),
                Step::Go => {}
            }
            Ok(restore_json(&s.restore(session, request)?))
        });
    }
    let request = PushRequest {
        remote: inputs.remote,
        views: inputs.views,
        force: inputs.force,
    };
    with_service(root, |s| {
        if request.force {
            let preview = s.preview(
                engine,
                session,
                request.remote.as_deref(),
                PreviewKind::ForcePush,
            )?;
            match card_step(who, &relay, Move::ForcePush, &preview)? {
                Step::Card(v) | Step::Cancelled(v) => return Ok(v),
                Step::Go => {}
            }
        }
        Ok(push_json(&s.push(engine, session, request)?))
    })
}

/// Pulls for `session` into the project at `root`, and replies with the pull's fetches and
/// replays, or with the person's card for a take of hub canon (see the module docs). A reader
/// is refused first.
pub(crate) fn pull(
    engine: &dyn Engine,
    root: &Path,
    who: &Who<'_>,
    inputs: PullInputs,
) -> Result<Value, RdyError> {
    require_writer(who.session, "pull")?;
    inputs.check()?;
    let relay = inputs.relay();
    let session = who.session;
    // a take with views takes only those views: no canon-take card
    let canon_take = inputs.take && inputs.views.is_empty();
    let request = PullRequest {
        remote: inputs.remote,
        views: inputs.views,
        take: inputs.take,
        strict: inputs.strict,
    };
    with_service(root, |s| {
        if canon_take {
            let preview = s.preview(
                engine,
                session,
                request.remote.as_deref(),
                PreviewKind::TakePull,
            )?;
            match card_step(who, &relay, Move::Take, &preview)? {
                Step::Card(v) | Step::Cancelled(v) => return Ok(v),
                Step::Go => {}
            }
        }
        pull_json(&s.pull(engine, session, request)?)
    })
}

/// Restores the dump of `args` for `who` into the hub repo of the project's share remote at
/// `root`: the push of [`push`] with a dump, so the same card applies.
pub(crate) fn restore(
    engine: &dyn Engine,
    root: &Path,
    who: &Who<'_>,
    args: RestoreArgs,
) -> Result<Value, RdyError> {
    push(engine, root, who, PushInputs::from(args))
}

/// Runs an engine-seam sync call (`rdy_push`, `rdy_pull`): the session is the relay.
pub(crate) fn call(
    engine: &dyn Engine,
    root: &Path,
    session: &Session,
    call: SyncCall,
) -> Result<Value, RdyError> {
    let who = Who {
        session,
        person: &call.person,
        implicit: true,
    };
    let bad = |e: serde_json::Error| {
        RdyError::new(
            E_BAD_INPUT,
            format!("the sync inputs are not valid: {e}"),
            "send the inputs of rdy_push or rdy_pull",
        )
    };
    match call.op {
        "push" => push(
            engine,
            root,
            &who,
            serde_json::from_value(call.args).map_err(bad)?,
        ),
        "pull" => pull(
            engine,
            root,
            &who,
            serde_json::from_value(call.args).map_err(bad)?,
        ),
        other => Err(RdyError::new(
            E_BAD_INPUT,
            format!("unknown sync call {other:?}"),
            "one of: push, pull",
        )),
    }
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
    let outcome = outcome_json(&r.outcome);
    json!({
        "push": r.push,
        "kind": if r.kind == KIND_VIEW { "view" } else { "canon" },
        "view": r.view,
        "result": outcome,
        "fetched": r.fetched,
    })
}

fn outcome_json(o: &LookupOutcome) -> Value {
    match o {
        LookupOutcome::Taken { tip } => json!({"outcome": "taken", "tip": tip.hash.to_string()}),
        LookupOutcome::Held { request, tip } => json!({
            "outcome": "held",
            "request": {"number": request.number, "version": request.version},
            "tip": tip.map(|t| t.hash.to_string()),
        }),
        LookupOutcome::Refused { code } => json!({"outcome": "refused", "code": code}),
        LookupOutcome::Unknown => json!({"outcome": "unknown"}),
    }
}

fn restore_json(out: &RestoreOutcome) -> Value {
    let views: Vec<Value> = out
        .views
        .iter()
        .map(|(name, end)| json!({"view": name, "end": end_json(end)}))
        .collect();
    let lookups: Vec<Value> = out
        .lookups
        .iter()
        .map(|l| json!({"push": l.push, "view": l.view, "result": outcome_json(&l.outcome)}))
        .collect();
    json!({
        "remote": out.remote,
        "holder": out.holder,
        "canon": out.canon.as_ref().map(end_json),
        "lookups": lookups,
        "without_inputs": out.without_inputs,
        "views": views,
        "kept": out.kept,
        "cleanup": out.cleanup,
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
        "notes": if out.force_not_needed { json!([FORCE_NOT_NEEDED]) } else { json!([]) },
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
        "warnings": serde_json::to_value(&out.warnings).map_err(|_| internal())?,
    }))
}
