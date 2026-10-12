//! ib11: the notifier. The writer forwards each sealed batch's events (and each pack reload)
//! here, after the publish that holds them; this thread reads the published state, decides
//! which subscribed MCP sessions the batch concerns, and tells them through the hub (spec §10.3;
//! D-KTW43 "push to connected sessions", D-AHYBK; founder decision C, D-Z0EMP, with D-TPZ0W and
//! D-V4NZX for the inbox rule).
//!
//! The rule is closed (ledger O8, O9). For each batch, for each session with a known actor:
//! - `rdy://inbox` exactly when the batch gives the session at least one inbox entry
//!   (`batch_entries`, the rule the inbox read applies: matched events and the batch's logged
//!   moves; founder decision C, D-Z0EMP: "notifies exactly when the batch gives the session an
//!   entry"). A session with no persisted subscription, or whose cursor is already past the
//!   batch, gets none.
//! - `rdy://view/{name}` when an event names view `name`.
//! - `rdy://doc/{id}` when a `change_committed` or `landed` writes the block the URI follows, or,
//!   when that block is a doc, any block in it (ruling task-11.2). `rdy-mcp` resolves the URI's
//!   id, key or path to that block when the session subscribes. A written block's doc is its doc
//!   in the in-flight scope, else in canon, else the doc its last version in the batch sits in
//!   (a block the batch deleted).
//! - `rdy://proposal/{id}` when a `proposed` or `landed` names a version of proposal `id`.
//! - `rdy://project` on every `landed`.
//!
//! On a pack reload, `rdy://pack` and `rdy://project` reach every session subscribed to them.
//! Each URI goes to a session at most once per batch, and only when the session subscribed to
//! it. A notification is best effort: an error drops the URI it would have sent, and a panic
//! drops the batch (logged); neither stops the daemon.
//!
//! What the notifier reads (ruling task-11.1): the published state, which holds the batch since
//! the writer forwards after its publish, and the inbox subscriptions the writer read from that
//! state right after the publish, on its own thread, where no batch is open. The store's reader
//! can see a batch the writer has not sealed yet (held batches, D-Z0EMP), so the notifier never
//! reads a ref or a view head from it; it reads only objects by hash that the sealed batch's
//! events name (changes, landings, proposals and the block versions they delete), which no open
//! batch can change. A subscription change in a later batch never decides a notification for an
//! earlier one.
use crate::daemon::log;
use crate::shared::Shared;
use crate::writer::panic_text;
use rdy_engine::inbox::read::batch_entries;
use rdy_engine::{Event, EventKind, RdyError, ReadState, Scope};
use rdy_mcp::notify::Target;
use rdy_model::{BlockId, Hash, Op};
use rdy_store::inbox::InboxSub;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;

const INBOX: &str = "rdy://inbox";
/// The inbox resource's URI, as the writer lists its subscribers.
pub(crate) const INBOX_URI: &str = INBOX;
const PACK: &str = "rdy://pack";
const PROJECT: &str = "rdy://project";
const DOC: &str = "rdy://doc/";
const VIEW: &str = "rdy://view/";
const PROPOSAL: &str = "rdy://proposal/";

/// The persisted inbox subscription of each connected session subscribed to `rdy://inbox`, by
/// (actor, run), as of a sealed and published state.
pub(crate) type InboxSubs = BTreeMap<(String, Option<String>), InboxSub>;

/// What the writer forwards to the notifier.
pub(crate) enum NotifyMsg {
    /// The events of one sealed batch (all of one `seq`), after the publish that holds it, with
    /// the inbox subscriptions the writer read from that published state.
    Events {
        events: Vec<Arc<Event>>,
        subs: Arc<InboxSubs>,
    },
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
        NotifyMsg::Events { events, subs } => {
            // most batches reach no subscribed session: no state load then
            if !targets
                .iter()
                .any(|t| t.actor.is_some() && !t.uris.is_empty())
            {
                return;
            }
            let st = sh.state.load_full();
            let batch = Batch::new(&st, events, subs);
            for t in &targets {
                for uri in batch.uris_for(t) {
                    #[cfg(test)]
                    sh.delivered.lock().unwrap().push((t.sid, uri.clone()));
                    sh.hub.notify(t.sid, &uri);
                }
            }
        }
    }
}

/// One batch as the notifier reads it, for every target in turn. What depends only on the
/// batch is computed once, on first use: the blocks it writes and their docs, and its events
/// as the inbox rule takes them. The inbox decision is computed once per (actor, run), so two
/// connections of one session cost one.
pub(crate) struct Batch<'a> {
    st: &'a ReadState,
    events: &'a [Arc<Event>],
    subs: &'a InboxSubs,
    written: OnceCell<Written>,
    docs: OnceCell<BTreeSet<BlockId>>,
    owned: OnceCell<Vec<Event>>,
    inbox: RefCell<BTreeMap<(String, Option<String>), bool>>,
}

impl<'a> Batch<'a> {
    /// `events`: one batch's events, all of one `seq`; `subs`: the inbox subscriptions as of
    /// the batch's publish.
    pub(crate) fn new(st: &'a ReadState, events: &'a [Arc<Event>], subs: &'a InboxSubs) -> Self {
        Batch {
            st,
            events,
            subs,
            written: OnceCell::new(),
            docs: OnceCell::new(),
            owned: OnceCell::new(),
            inbox: RefCell::new(BTreeMap::new()),
        }
    }

    /// The URIs the batch changed for session `t`, among those it subscribed to, by the closed
    /// rule of this module. Reads the published state and the subscriptions and nothing else;
    /// an error leaves out the URI it concerns.
    pub(crate) fn uris_for(&self, t: &Target) -> BTreeSet<String> {
        let Some(actor) = t.actor.as_deref() else {
            return BTreeSet::new();
        };
        if t.uris.is_empty() || self.events.is_empty() {
            return BTreeSet::new();
        }
        let subscribed = |prefix: &str| t.uris.iter().any(|u| u.starts_with(prefix));
        let mut out = BTreeSet::new();
        for e in self.events {
            if let Some(view) = view_of(&e.kind) {
                out.insert(format!("{VIEW}{view}"));
            }
            if matches!(e.kind, EventKind::Landed { .. }) {
                out.insert(PROJECT.to_string());
            }
        }
        if subscribed(PROPOSAL) {
            out.extend(proposal_uris(self.st, t, self.events));
        }
        if subscribed(DOC) {
            out.extend(self.doc_uris(t));
        }
        if t.uris.contains(INBOX) && self.inbox_moved(actor, t.run.as_deref()) {
            out.insert(INBOX.to_string());
        }
        out.retain(|u| t.uris.contains(u));
        out
    }

    fn written(&self) -> &Written {
        self.written
            .get_or_init(|| written_blocks(self.st, self.events).unwrap_or_default())
    }

    /// The `rdy://doc/` URIs of `t` whose followed block the batch writes, or, for a doc, whose
    /// doc holds a block the batch writes.
    fn doc_uris(&self, t: &Target) -> BTreeSet<String> {
        if t.docs.is_empty() || self.written().is_empty() {
            return BTreeSet::new();
        }
        let written = self.written();
        let docs = self.docs.get_or_init(|| written_docs(self.st, written));
        t.docs
            .iter()
            .filter(|(_, b)| written.contains_key(*b) || docs.contains(*b))
            .map(|(uri, _)| uri.clone())
            .collect()
    }

    /// Whether the batch gives the session (`actor`, `run`) an inbox entry under its persisted
    /// subscription (as of the batch's publish): the inbox read's own rule. False with no
    /// subscription, when the cursor is already past the batch, or on an error.
    fn inbox_moved(&self, actor: &str, run: Option<&str>) -> bool {
        let key = (actor.to_string(), run.map(str::to_string));
        if let Some(moved) = self.inbox.borrow().get(&key) {
            return *moved;
        }
        let moved = self
            .subs
            .get(&key)
            .is_some_and(|sub| inbox_moved(self.st, sub, self.owned()).unwrap_or(false));
        self.inbox.borrow_mut().insert(key, moved);
        moved
    }

    fn owned(&self) -> &[Event] {
        self.owned
            .get_or_init(|| self.events.iter().map(|e| (**e).clone()).collect())
    }
}

/// The URIs that one batch's `events` (all of one `seq`) changed for session `t` (see
/// [`Batch::uris_for`]).
#[cfg(test)]
pub(crate) fn uris_for(
    st: &ReadState,
    t: &Target,
    events: &[Arc<Event>],
    subs: &InboxSubs,
) -> BTreeSet<String> {
    Batch::new(st, events, subs).uris_for(t)
}

/// The persisted inbox subscription of each session in `sessions` (the identified subscribers
/// of `rdy://inbox`, by (actor, run)), read from `st`. The writer calls it right after it
/// publishes a sealed state, so no open batch is read (ruling task-11.1). A read that fails
/// leaves its session out: that session hears of no inbox entry for these batches (best effort).
pub(crate) fn inbox_subs(st: &ReadState, sessions: &[(String, Option<String>)]) -> InboxSubs {
    let reader = st.reader();
    sessions
        .iter()
        .filter_map(|(actor, run)| {
            let sub = reader.inbox(actor, run.as_deref()).ok()??;
            Some(((actor.clone(), run.clone()), sub))
        })
        .collect()
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

/// What a batch writes: each block a `change_committed` or `landed` creates, updates or
/// deletes, with where its last version in the batch sits (a deleted block's is the version it
/// deleted), or `None` when that version does not read.
type Written = BTreeMap<BlockId, Option<Place>>;

/// Where a block version sits: whether it is a doc, and its parent.
struct Place {
    doc: bool,
    parent: Option<BlockId>,
}

/// The blocks the batch's `change_committed` and `landed` events write (create, update or
/// delete), in the order of their changes, a later op's version winning.
fn written_blocks(st: &ReadState, events: &[Arc<Event>]) -> Result<Written, RdyError> {
    let reader = st.reader();
    let mut changes: Vec<Hash> = Vec::new();
    for e in events {
        match &e.kind {
            EventKind::ChangeCommitted { change, .. } => changes.push(*change),
            EventKind::Landed { landing, .. } => changes.extend(reader.landing(landing)?.changes),
            _ => {}
        }
    }
    let place = |b: &rdy_model::Block| Place {
        doc: b.block_type == "doc",
        parent: b.parent.clone(),
    };
    let mut out = Written::new();
    for c in changes {
        for op in reader.change(&c)?.ops {
            let at = match &op {
                Op::Create { block } | Op::Update { block, .. } => Some(place(block)),
                Op::Delete { base, .. } => reader.get_block(base).ok().map(|b| place(&b)),
            };
            out.insert(op.block_id().clone(), at);
        }
    }
    Ok(out)
}

/// The doc of each written block: its doc in the in-flight scope, else in canon, else the doc
/// its last version in the batch sits in, walking up through the batch's own versions (a block
/// the batch deleted, perhaps with its parents).
fn written_docs(st: &ReadState, written: &Written) -> BTreeSet<BlockId> {
    let inflight = st.materialized(&Scope::Inflight).ok();
    let canon = st.materialized(&Scope::Canon).ok();
    let in_scope = |b: &BlockId| {
        inflight
            .as_ref()
            .and_then(|m| m.doc_of(b))
            .or_else(|| canon.as_ref().and_then(|m| m.doc_of(b)))
            .map(|d| d.id.clone())
    };
    let doc_of = |b: &BlockId| {
        let mut cur = b.clone();
        for _ in 0..=written.len() {
            if let Some(d) = in_scope(&cur) {
                return Some(d);
            }
            let at = written.get(&cur)?.as_ref()?;
            if at.doc {
                return Some(cur);
            }
            cur = at.parent.clone()?;
        }
        None
    };
    written.keys().filter_map(doc_of).collect()
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

/// Whether the batch (`events`, not empty) gives the session whose persisted subscription is
/// `sub` (as of the batch's publish) an inbox entry: the inbox read's own rule, and nothing
/// else. False when the subscription's cursor is already past the batch.
fn inbox_moved(st: &ReadState, sub: &InboxSub, events: &[Event]) -> Result<bool, RdyError> {
    if events[0].seq <= sub.cursor.at {
        return Ok(false);
    }
    let infl = st.run_in_flight(&sub.actor, sub.run.as_deref())?;
    Ok(!batch_entries(st, sub, &infl, events)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::engine::{Engine, Local};
    use rdy_engine::inbox::sub::subscribe;
    use rdy_engine::{Rdy, Session, WriteRequest};
    use rdy_model::Role;
    use serde_json::json;

    const PACK_YAML: &str = "name: notify-qa\nversion: '1'\nextends: rdy-standard\nareas:\n  payments:\n    paths: [payments/**]\n  identity:\n    paths: [identity/**]\n";

    /// A project whose pack declares areas `payments` and `identity`, behind an engine.
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

    fn human(role: Role) -> Session {
        Session::human("human:qa".into(), role)
    }

    fn target(name: &str, uris: &[&str]) -> Target {
        Target {
            sid: 1,
            actor: Some(format!("agent:{name}")),
            run: Some(name.into()),
            uris: uris.iter().map(|u| u.to_string()).collect(),
            docs: BTreeMap::new(),
        }
    }

    /// The events of the first batch since `from` that holds an event `pick` accepts.
    fn batch_since(
        e: &Local,
        from: rdy_store::segment::Location,
        pick: fn(&EventKind) -> bool,
    ) -> Vec<Arc<Event>> {
        let all = e.read().replay(from).unwrap();
        let seq = all
            .iter()
            .find(|ev| pick(&ev.kind))
            .expect("a matching event")
            .seq;
        all.into_iter()
            .filter(|ev| ev.seq == seq)
            .map(Arc::new)
            .collect()
    }

    fn is_landed(k: &EventKind) -> bool {
        matches!(k, EventKind::Landed { .. })
    }

    fn is_committed(k: &EventKind) -> bool {
        matches!(k, EventKind::ChangeCommitted { .. })
    }

    /// The person lands a new doc at `path`; returns the landing batch's events.
    fn land(e: &Local, path: &str) -> Vec<Arc<Event>> {
        let from = e.read().seq();
        let path = path.to_string();
        e.write(Box::new(move |r| {
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
        batch_since(e, from, is_landed)
    }

    /// The person writes `ops` in view `view`, and lands the view when `land` is set. Returns the
    /// write's result.
    fn write(e: &Local, view: &str, ops: serde_json::Value, land: bool) -> serde_json::Value {
        let view = view.to_string();
        e.write(Box::new(move |r| {
            let w = r.write(
                &human(Role::Writer),
                WriteRequest {
                    view: view.clone(),
                    ops: serde_json::from_value(ops).unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )?;
            if land {
                let p = r.propose(&human(Role::Writer), &view, "t")?;
                r.land(&human(Role::Approver), &p.id)?;
            }
            Ok(serde_json::to_value(w).unwrap())
        }))
        .unwrap()
    }

    /// Doc `ops` (path `ops`) with claims c1 and c2 under a section, landed. Returns the ids of
    /// the doc, c1 and c2.
    fn ops_doc(e: &Local) -> (BlockId, BlockId, BlockId) {
        let w = write(
            e,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
                {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "One holds."},
                {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Two holds."}
            ]),
            true,
        );
        let id = |k: &str| BlockId::parse(w["created"][k].as_str().unwrap()).unwrap();
        (id("$d"), id("$c1"), id("$c2"))
    }

    /// A target subscribed to the doc URIs of the probe: by doc id, by path, and by the ids of
    /// claims c1 and c2, each with the block it follows (as `rdy-mcp` resolves them).
    fn doc_target(d: &BlockId, c1: &BlockId, c2: &BlockId) -> Target {
        let mut t = target("a", &[]);
        for (text, block) in [
            (d.to_string(), d),
            ("ops".to_string(), d),
            (c1.to_string(), c1),
            (c2.to_string(), c2),
        ] {
            let uri = format!("{DOC}{text}");
            t.uris.insert(uri.clone());
            t.docs.insert(uri, block.clone());
        }
        t
    }

    /// The inbox subscriptions the writer would read for `targets`.
    fn subs_of(st: &ReadState, targets: &[Target]) -> InboxSubs {
        let hub = rdy_mcp::notify::NotifyHub::new();
        // one hub session per target (the helper `target` gives every target sid 1)
        for (sid, t) in (0u64..).zip(targets) {
            hub.connect(sid, rdy_mcp::notify::Sink::None);
            if let Some(actor) = &t.actor {
                hub.identify(sid, actor, t.run.as_deref());
            }
            for uri in &t.uris {
                hub.subscribe(sid, uri);
            }
        }
        inbox_subs(st, &hub.subscribers(INBOX))
    }

    fn sorted(set: BTreeSet<String>) -> Vec<String> {
        set.into_iter().collect()
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
        let subs = subs_of(&st, std::slice::from_ref(&t));
        let got = sorted(uris_for(&st, &t, &batch, &subs));
        assert_eq!(got, vec![INBOX.to_string(), PROJECT.to_string()]);
    }

    #[test]
    fn ib11_a_target_without_a_subscription_gets_no_inbox_update() {
        let (_d, e) = project();
        subscribe(&e, &agent("a"), Some(vec!["payments".into()])).unwrap();
        let batch = land(&e, "payments/fees");
        let st = e.read();
        // agent b subscribed to rdy://inbox on its connection, but has no persisted subscription
        let (a, b) = (target("a", &[INBOX]), target("b", &[INBOX]));
        let subs = subs_of(&st, &[a.clone(), b.clone()]);
        assert!(uris_for(&st, &b, &batch, &subs).is_empty());
        // the same batch reaches agent a, whose subscription names the area
        assert_eq!(
            sorted(uris_for(&st, &a, &batch, &subs)),
            vec![INBOX.to_string()]
        );
    }

    #[test]
    fn ib11_a_failed_seal_after_a_landing_leaves_its_inbox_decision() {
        let (_d, e) = project();
        subscribe(&e, &agent("a"), Some(vec!["payments".into()])).unwrap();
        // landing X, and the subscriptions the writer reads right after X's publish
        let x = land(&e, "payments/fees");
        let st = e.read();
        let t = target("a", &[INBOX]);
        let subs = subs_of(&st, std::slice::from_ref(&t));
        // the next write is a held batch that moves a's areas to identity; its seal fails
        let sealed = e.write(Box::new(|r| {
            let mut sub = r.read().reader().inbox("agent:a", Some("a"))?.unwrap();
            sub.areas = vec!["identity".into()];
            r.store_mut().put_inbox(&sub)?;
            r.store_mut().fail_next_append_after(0);
            r.seal_open()?;
            Ok(serde_json::Value::Null)
        }));
        assert!(sealed.is_err(), "the seal must fail: {sealed:?}");
        // the live index holds the batch the failed seal removed from the log
        let live = st.reader().inbox("agent:a", Some("a")).unwrap().unwrap();
        assert_eq!(live.areas, vec!["identity".to_string()]);
        // the decision for X reads the sealed subscription only: a is told
        assert_eq!(
            sorted(uris_for(&st, &t, &x, &subs)),
            vec![INBOX.to_string()]
        );
    }

    /// Founder decision C (D-Z0EMP; ledger: Task 11 notifies `rdy://inbox` exactly when the
    /// batch gives the session an entry): a commit that writes a block the session's in-flight
    /// change cites, but gives the session no entry, sends no `rdy://inbox`.
    #[test]
    fn ib11_a_write_to_a_cited_block_with_no_entry_sends_no_inbox_update() {
        let (_d, e) = project();
        let (_doc, c1, _c2) = ops_doc(&e);
        let parent = e.read().block(&Scope::Canon, &c1).unwrap().parent.unwrap();
        let k = agent("k");
        subscribe(&e, &k, None).unwrap();
        // k's claim links to c1 with `describes`: a citation, not a premise
        let ops = json!([{"op": "create", "type": "claim", "id": "$y", "parent": parent.to_string(),
            "body": "Y holds.", "links": {"describes": [{"target": c1.to_string()}]}}]);
        e.write(Box::new(move |r| {
            r.write(
                &k,
                WriteRequest {
                    view: "k".into(),
                    ops: serde_json::from_value(ops).unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )?;
            Ok(serde_json::Value::Null)
        }))
        .unwrap();
        let from = e.read().seq();
        write(
            &e,
            "w2",
            json!([{"op": "update", "id": c1.to_string(), "body": "One holds, still."}]),
            false,
        );
        let batch = batch_since(&e, from, is_committed);
        let st = e.read();
        let infl = st.run_in_flight("agent:k", Some("k")).unwrap();
        assert!(infl.cited.contains(&c1), "{infl:?}");
        let t = target("k", &[INBOX]);
        let subs = subs_of(&st, std::slice::from_ref(&t));
        let sub = &subs[&("agent:k".to_string(), Some("k".to_string()))];
        let evs: Vec<Event> = batch.iter().map(|e| (**e).clone()).collect();
        assert!(batch_entries(&st, sub, &infl, &evs).unwrap().is_empty());
        assert!(uris_for(&st, &t, &batch, &subs).is_empty());
    }

    #[test]
    fn ib11_a_doc_uri_follows_its_doc_or_its_block() {
        let (_d, e) = project();
        let (d, c1, c2) = ops_doc(&e);
        let from = e.read().seq();
        // a commit that updates claim c1
        write(
            &e,
            "w2",
            json!([{"op": "update", "id": c1.to_string(), "body": "One holds, still."}]),
            false,
        );
        let batch = batch_since(&e, from, is_committed);
        let st = e.read();
        let t = doc_target(&d, &c1, &c2);
        let got = sorted(uris_for(&st, &t, &batch, &InboxSubs::new()));
        // the doc by id and by path hear of any block in it; a claim URI hears of that claim only
        assert_eq!(
            got,
            vec![
                format!("{DOC}{c1}"),
                format!("{DOC}{d}"),
                format!("{DOC}ops")
            ]
        );
    }

    #[test]
    fn ib11_a_landing_that_deletes_a_claim_notifies_its_doc() {
        let (_d, e) = project();
        let (d, c1, c2) = ops_doc(&e);
        let from = e.read().seq();
        write(
            &e,
            "w3",
            json!([{"op": "delete", "id": c2.to_string()}]),
            true,
        );
        let batch = batch_since(&e, from, is_landed);
        let st = e.read();
        assert!(
            st.materialized(&Scope::Canon).unwrap().get(&c2).is_none(),
            "c2 is gone from canon"
        );
        let t = doc_target(&d, &c1, &c2);
        let got = sorted(uris_for(&st, &t, &batch, &InboxSubs::new()));
        assert_eq!(
            got,
            vec![
                format!("{DOC}{c2}"),
                format!("{DOC}{d}"),
                format!("{DOC}ops")
            ]
        );
    }
}
