//! ib11: the notifier. The writer forwards each sealed batch's events (and each pack reload)
//! here, after the publish that holds them; this thread reads the published state, decides
//! which subscribed MCP sessions the batch concerns, and tells them through the hub (spec §10.3;
//! D-KTW43 "push to connected sessions", D-AHYBK; founder decision C, D-Z0EMP, with D-TPZ0W and
//! D-V4NZX for the inbox rule).
//!
//! The rule is closed (ledger O8, O9). For each batch, for each session with a known actor:
//! - `rdy://inbox` when the batch gives the session at least one inbox entry (`batch_entries`,
//!   the rule the inbox read applies: matched events and the batch's logged moves), or a
//!   `change_committed` or `landed` writes a block its in-flight changes cite. A session with
//!   no persisted subscription, or whose cursor is already past the batch, gets none.
//! - `rdy://view/{name}` when an event names view `name`.
//! - `rdy://doc/{id}` when a `change_committed` or `landed` writes a block whose doc is `id`.
//! - `rdy://proposal/{id}` when a `proposed` or `landed` names a version of proposal `id`.
//! - `rdy://project` on every `landed`.
//!
//! On a pack reload, `rdy://pack` and `rdy://project` reach every session subscribed to them.
//! Each URI goes to a session at most once per batch, and only when the session subscribed to
//! it. A notification is best effort: an error drops the URI it would have sent, and a panic
//! drops the batch (logged); neither stops the daemon.
//!
//! What the notifier reads: the published state (which holds the batch, since the writer
//! forwards after its publish) and the sessions' subscriptions through the state's reader. The
//! store's reader can see a batch the writer has not sealed yet (held batches, D-Z0EMP); only a
//! subscription is read that way, and a wrong guess costs one notification at most, never a
//! refusal or a record.
use crate::daemon::log;
use crate::shared::Shared;
use crate::writer::panic_text;
use rdy_engine::inbox::read::batch_entries;
use rdy_engine::{Event, EventKind, RdyError, ReadState, Scope};
use rdy_mcp::notify::Target;
use rdy_model::{BlockId, Hash};
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;

const INBOX: &str = "rdy://inbox";
const PACK: &str = "rdy://pack";
const PROJECT: &str = "rdy://project";
const DOC: &str = "rdy://doc/";
const VIEW: &str = "rdy://view/";
const PROPOSAL: &str = "rdy://proposal/";

/// What the writer forwards to the notifier.
pub(crate) enum NotifyMsg {
    /// The events of one sealed batch (all of one `seq`), after the publish that holds it.
    Events(Vec<Arc<Event>>),
    /// The pack was reloaded, and the state with the new pack is published.
    PackReloaded,
}

/// Starts the notifier thread (`rdy-notify`). It handles one message at a time and ends when
/// the queue's sender (in `Shared`) is dropped or no `Shared` lives.
pub(crate) fn spawn_notifier(shared: Weak<Shared>, rx: Receiver<NotifyMsg>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("rdy-notify".into())
        .spawn(move || {
            while let Ok(msg) = rx.recv() {
                let Some(sh) = shared.upgrade() else { break };
                if let Err(p) = catch_unwind(AssertUnwindSafe(|| deliver(&sh, &msg))) {
                    log(&format!("notify skipped a batch: {}", panic_text(&*p)));
                }
            }
        })
        .expect("the rdy-notify thread could not be started")
}

/// Sends what `msg` means to every session in the hub that subscribed to it.
fn deliver(sh: &Shared, msg: &NotifyMsg) {
    let targets = sh.hub.targets();
    match msg {
        NotifyMsg::PackReloaded => {
            for t in &targets {
                for uri in [PACK, PROJECT] {
                    if t.uris.contains(uri) {
                        sh.hub.notify(t.sid, uri);
                    }
                }
            }
        }
        NotifyMsg::Events(events) => {
            // most batches reach no subscribed session: no state load then
            if !targets
                .iter()
                .any(|t| t.actor.is_some() && !t.uris.is_empty())
            {
                return;
            }
            let st = sh.state.load_full();
            for t in &targets {
                for uri in uris_for(&st, t, events) {
                    sh.hub.notify(t.sid, &uri);
                }
            }
        }
    }
}

/// The URIs that one batch's `events` (all of one `seq`) changed for session `t`, among those
/// it subscribed to, by the closed rule of this module. Reads `st` and nothing else; an error
/// leaves out the URI it concerns.
pub(crate) fn uris_for(st: &ReadState, t: &Target, events: &[Arc<Event>]) -> BTreeSet<String> {
    let Some(actor) = t.actor.as_deref() else {
        return BTreeSet::new();
    };
    if t.uris.is_empty() || events.is_empty() {
        return BTreeSet::new();
    }
    let subscribed = |prefix: &str| t.uris.iter().any(|u| u.starts_with(prefix));
    let mut out = BTreeSet::new();
    for e in events {
        if let Some(view) = view_of(&e.kind) {
            out.insert(format!("{VIEW}{view}"));
        }
        if matches!(e.kind, EventKind::Landed { .. }) {
            out.insert(PROJECT.to_string());
        }
    }
    if subscribed(PROPOSAL) {
        out.extend(proposal_uris(st, t, events));
    }
    let inbox = t.uris.contains(INBOX);
    if subscribed(DOC) || inbox {
        let written = written_blocks(st, events).unwrap_or_default();
        if subscribed(DOC) {
            out.extend(doc_uris(st, &written));
        }
        if inbox && inbox_moved(st, actor, t.run.as_deref(), events, &written).unwrap_or(false) {
            out.insert(INBOX.to_string());
        }
    }
    out.retain(|u| t.uris.contains(u));
    out
}

/// The view an event names, if any.
fn view_of(k: &EventKind) -> Option<&str> {
    match k {
        EventKind::ChangeCommitted { view, .. }
        | EventKind::ViewOp { view, .. }
        | EventKind::ViewRebased { view, .. }
        | EventKind::ViewClosed { view, .. }
        | EventKind::Proposed { view, .. }
        | EventKind::Landed { view, .. } => Some(view),
        _ => None,
    }
}

/// The blocks the batch's `change_committed` and `landed` events write (create, update or
/// delete).
fn written_blocks(st: &ReadState, events: &[Arc<Event>]) -> Result<BTreeSet<BlockId>, RdyError> {
    let reader = st.reader();
    let mut changes: Vec<Hash> = Vec::new();
    for e in events {
        match &e.kind {
            EventKind::ChangeCommitted { change, .. } => changes.push(*change),
            EventKind::Landed { landing, .. } => changes.extend(reader.landing(landing)?.changes),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    for c in changes {
        for op in reader.change(&c)?.ops {
            out.insert(op.block_id().clone());
        }
    }
    Ok(out)
}

/// `rdy://doc/{id}` for the doc of each written block: its doc in the in-flight scope, else in
/// canon (a block the batch deleted).
fn doc_uris(st: &ReadState, written: &BTreeSet<BlockId>) -> BTreeSet<String> {
    let inflight = st.materialized(&Scope::Inflight).ok();
    let canon = st.materialized(&Scope::Canon).ok();
    written
        .iter()
        .filter_map(|b| {
            let doc = inflight
                .as_ref()
                .and_then(|m| m.doc_of(b))
                .or_else(|| canon.as_ref().and_then(|m| m.doc_of(b)))?;
            Some(format!("{DOC}{}", doc.id))
        })
        .collect()
}

/// `rdy://proposal/{id}` URIs `t` subscribed to whose proposal a `proposed` or `landed` of the
/// batch names a version of. A subscribed id is `P-<prefix>` or `P-<prefix>@<n>`; it names the
/// proposal whose stable id starts with the prefix.
fn proposal_uris(st: &ReadState, t: &Target, events: &[Arc<Event>]) -> BTreeSet<String> {
    let reader = st.reader();
    let stables: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Proposed { proposal, .. } | EventKind::Landed { proposal, .. } => {
                let p = reader.proposal(proposal).ok()?;
                Some(p.stable.unwrap_or(*proposal).to_string())
            }
            _ => None,
        })
        .collect();
    t.uris
        .iter()
        .filter(|uri| {
            let Some(prefix) = uri
                .strip_prefix(PROPOSAL)
                .and_then(|id| id.strip_prefix("P-"))
                .map(|id| {
                    id.split('@')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                })
            else {
                return false;
            };
            !prefix.is_empty() && stables.iter().any(|s| s.starts_with(&prefix))
        })
        .cloned()
        .collect()
}

/// Whether the batch moves the inbox of session (`actor`, `run`): it gives an entry under the
/// session's persisted subscription (the inbox read's own rule), or it writes a block the
/// session's in-flight changes cite. False with no subscription, or when the subscription's
/// cursor is already past the batch.
fn inbox_moved(
    st: &ReadState,
    actor: &str,
    run: Option<&str>,
    events: &[Arc<Event>],
    written: &BTreeSet<BlockId>,
) -> Result<bool, RdyError> {
    let Some(sub) = st.reader().inbox(actor, run)? else {
        return Ok(false);
    };
    if events[0].seq <= sub.cursor.at {
        return Ok(false);
    }
    let infl = st.run_in_flight(actor, run)?;
    if written.iter().any(|b| infl.cited.contains(b)) {
        return Ok(true);
    }
    let batch: Vec<Event> = events.iter().map(|e| (**e).clone()).collect();
    Ok(!batch_entries(st, &sub, &infl, &batch)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::engine::{Engine, Local};
    use rdy_engine::inbox::sub::subscribe;
    use rdy_engine::{Rdy, Session};
    use rdy_model::Role;

    const PACK_YAML: &str = "name: notify-qa\nversion: '1'\nextends: rdy-standard\nareas:\n  payments:\n    paths: [payments/**]\n";

    /// A project whose pack declares area `payments`, behind an engine.
    fn project() -> (tempfile::TempDir, Local) {
        let dir = tempfile::tempdir().unwrap();
        drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
        std::fs::write(dir.path().join(".rdy/packs/notify-qa.yaml"), PACK_YAML).unwrap();
        std::fs::write(dir.path().join(".rdy.yaml"), "pack: notify-qa\n").unwrap();
        let e = Local::new(Rdy::open(dir.path()).unwrap());
        (dir, e)
    }

    fn agent(name: &str) -> Session {
        Session::agent(format!("agent:{name}"), Some(name.into()), Role::Writer).unwrap()
    }

    fn target(name: &str, uris: &[&str]) -> Target {
        Target {
            sid: 1,
            actor: Some(format!("agent:{name}")),
            run: Some(name.into()),
            uris: uris.iter().map(|u| u.to_string()).collect(),
        }
    }

    /// The person lands a new doc at `path`; returns the landing batch's events.
    fn land(e: &Local, path: &str) -> Vec<Arc<Event>> {
        let from = e.read().seq();
        let path = path.to_string();
        e.write(Box::new(move |r| {
            let human = |role| Session::human("human:qa".into(), role);
            r.new_doc(
                &human(Role::Writer),
                "work",
                &path,
                Some("overview"),
                Some("T"),
            )?;
            let p = r.propose(&human(Role::Writer), "work", "t")?;
            r.land(&human(Role::Approver), &p.id)?;
            Ok(serde_json::Value::Null)
        }))
        .unwrap();
        let all = e.read().replay(from).unwrap();
        let seq = all
            .iter()
            .find(|ev| matches!(ev.kind, EventKind::Landed { .. }))
            .expect("a landed event")
            .seq;
        all.into_iter()
            .filter(|ev| ev.seq == seq)
            .map(Arc::new)
            .collect()
    }

    #[test]
    fn ib11_uris_for_sends_each_uri_once_per_batch() {
        let (_d, e) = project();
        subscribe(&e, &agent("a"), Some(vec!["payments".into()])).unwrap();
        let mut batch = land(&e, "payments/fees");
        // two matching events in one batch: the landing, twice
        let landed = batch
            .iter()
            .find(|ev| matches!(ev.kind, EventKind::Landed { .. }))
            .cloned()
            .unwrap();
        batch.push(landed);
        let st = e.read();
        let t = target("a", &[INBOX, PROJECT, PACK]);
        let got: Vec<String> = uris_for(&st, &t, &batch).into_iter().collect();
        assert_eq!(got, vec![INBOX.to_string(), PROJECT.to_string()]);
    }

    #[test]
    fn ib11_a_target_without_a_subscription_gets_no_inbox_update() {
        let (_d, e) = project();
        subscribe(&e, &agent("a"), Some(vec!["payments".into()])).unwrap();
        let batch = land(&e, "payments/fees");
        let st = e.read();
        // agent b subscribed to rdy://inbox on its connection, but has no persisted subscription
        let b = target("b", &[INBOX]);
        assert!(uris_for(&st, &b, &batch).is_empty());
        // the same batch reaches agent a, whose subscription names the area
        let a = target("a", &[INBOX]);
        assert_eq!(
            uris_for(&st, &a, &batch).into_iter().collect::<Vec<_>>(),
            vec![INBOX.to_string()]
        );
    }
}
