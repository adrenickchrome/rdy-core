//! ib10: the hub of connected MCP sessions: who is connected, who they are (`rdy_session`) and
//! which resources they subscribed to. The hub only records and sends. Which session to tell about
//! which event is the caller's decision (the daemon, Task 11): it reads [`NotifyHub::targets`] and
//! calls [`NotifyHub::notify`].
//!
//! A notification is best effort (spec §10.3): the send runs on the tokio runtime that was current
//! when the session connected, and a failed send is dropped. The inbox is the reliable path.
use rdy_model::BlockId;
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::service::SubscriptionSink;
use rmcp::{Peer, RoleServer};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// Where a session's notifications go.
#[derive(Clone, Debug)]
pub enum Sink {
    /// A client on the legacy lifecycle: `resources/subscribe` and notifications on its peer.
    Legacy(Peer<RoleServer>),
    /// A client on `subscriptions/listen`: the stream's filter-enforcing sink.
    Listen(SubscriptionSink),
    /// No way to reach the session (yet).
    None,
    /// A sink the hub's unit tests tell apart; it sends nothing.
    #[cfg(test)]
    Test(u32),
}

/// One session as the hub knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub sid: u64,
    pub actor: Option<String>,
    pub run: Option<String>,
    pub uris: BTreeSet<String>,
    /// The block each subscribed `rdy://doc/{id}` URI follows, resolved when it subscribed
    /// (ruling task-11.2): a path or key names its block, so the daemon matches writes by id.
    pub docs: BTreeMap<String, BlockId>,
}

struct Conn {
    sink: Sink,
    /// The runtime current at `connect`; a send is spawned on it.
    runtime: Option<tokio::runtime::Handle>,
    actor: Option<String>,
    run: Option<String>,
    uris: BTreeSet<String>,
    docs: BTreeMap<String, BlockId>,
}

impl Conn {
    fn new() -> Conn {
        Conn {
            sink: Sink::None,
            runtime: None,
            actor: None,
            run: None,
            uris: BTreeSet::new(),
            docs: BTreeMap::new(),
        }
    }
}

/// What a `subscriptions/listen` stream changed in the hub (`NotifyHub::listen`).
#[derive(Debug)]
pub struct Listening {
    added: Vec<String>,
    prev: Sink,
}

/// The sessions connected to one server (one project's daemon, or one stdio process).
#[derive(Default)]
pub struct NotifyHub {
    conns: Mutex<BTreeMap<u64, Conn>>,
}

impl NotifyHub {
    pub fn new() -> Arc<NotifyHub> {
        Arc::new(NotifyHub::default())
    }

    fn conns(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, Conn>> {
        self.conns.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Records where session `sid` can be reached, and makes the session known: the one call
    /// that adds a session, made when it starts, before its first request. The runtime current
    /// now is the one sends use. A session that is already known keeps its identity and
    /// subscriptions. Every other call leaves an unknown session unknown, so a call still in
    /// flight when the session ended (`disconnect`) never brings it back.
    pub fn connect(&self, sid: u64, sink: Sink) {
        let runtime = tokio::runtime::Handle::try_current().ok();
        let mut conns = self.conns();
        let c = conns.entry(sid).or_insert_with(Conn::new);
        c.sink = sink;
        c.runtime = runtime;
    }

    /// Sets where the known session `sid` is reached, and returns the sink it replaces (`None`
    /// for an unknown session, which stays unknown).
    pub fn set_sink(&self, sid: u64, sink: Sink) -> Option<Sink> {
        let mut conns = self.conns();
        let c = conns.get_mut(&sid)?;
        Some(std::mem::replace(&mut c.sink, sink))
    }

    /// Records who the known session `sid` is (`rdy_session`).
    pub fn identify(&self, sid: u64, actor: &str, run: Option<&str>) {
        if let Some(c) = self.conns().get_mut(&sid) {
            c.actor = Some(actor.to_string());
            c.run = run.map(str::to_string);
        }
    }

    /// Subscribes the known session `sid` to `uri`. True when it was not subscribed yet.
    pub fn subscribe(&self, sid: u64, uri: &str) -> bool {
        self.conns()
            .get_mut(&sid)
            .is_some_and(|c| c.uris.insert(uri.to_string()))
    }

    /// Subscribes the known session `sid` to the doc resource `uri`, which follows block `block`
    /// (the URI's id, key or path, resolved by the caller). True when it was not subscribed yet.
    pub fn subscribe_doc(&self, sid: u64, uri: &str, block: BlockId) -> bool {
        let mut conns = self.conns();
        let Some(c) = conns.get_mut(&sid) else {
            return false;
        };
        c.docs.insert(uri.to_string(), block);
        c.uris.insert(uri.to_string())
    }

    /// Starts a `subscriptions/listen` stream of session `sid`: subscribes it to `uris` (each
    /// with the block a doc URI follows) and makes `sink` its sink. The returned `Listening`
    /// holds what `end_listen` undoes: the URIs the stream added and the sink it replaced.
    pub fn listen(&self, sid: u64, sink: Sink, uris: Vec<(String, Option<BlockId>)>) -> Listening {
        let added = uris
            .into_iter()
            .filter_map(|(uri, block)| {
                let new = match block {
                    Some(b) => self.subscribe_doc(sid, &uri, b),
                    None => self.subscribe(sid, &uri),
                };
                new.then_some(uri)
            })
            .collect();
        let prev = self.set_sink(sid, sink).unwrap_or(Sink::None);
        Listening { added, prev }
    }

    /// Ends a listen stream of session `sid`: removes only the URIs the stream added, so a URI
    /// the session subscribed to before (`resources/subscribe`) stays, and gives back the sink
    /// the stream replaced.
    pub fn end_listen(&self, sid: u64, l: Listening) {
        for uri in &l.added {
            self.unsubscribe(sid, uri);
        }
        self.set_sink(sid, l.prev);
    }

    pub fn unsubscribe(&self, sid: u64, uri: &str) {
        if let Some(c) = self.conns().get_mut(&sid) {
            c.uris.remove(uri);
            c.docs.remove(uri);
        }
    }

    /// Forgets session `sid`: its identity, subscriptions and sink.
    pub fn disconnect(&self, sid: u64) {
        self.conns().remove(&sid);
    }

    /// The (actor, run) of every identified session subscribed to `uri`, sorted and distinct:
    /// what the daemon's writer reads per write, without cloning any session's subscriptions.
    pub fn subscribers(&self, uri: &str) -> Vec<(String, Option<String>)> {
        let mut out: Vec<(String, Option<String>)> = self
            .conns()
            .values()
            .filter(|c| c.uris.contains(uri))
            .filter_map(|c| Some((c.actor.clone()?, c.run.clone())))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every session the hub knows, in session order.
    pub fn targets(&self) -> Vec<Target> {
        self.conns()
            .iter()
            .map(|(sid, c)| Target {
                sid: *sid,
                actor: c.actor.clone(),
                run: c.run.clone(),
                uris: c.uris.clone(),
                docs: c.docs.clone(),
            })
            .collect()
    }

    /// Tells session `sid` that `uri` changed. It sends nothing to a session with no sink or no
    /// runtime, spawns the send and ignores its result.
    pub fn notify(&self, sid: u64, uri: &str) {
        let (sink, runtime) = {
            let conns = self.conns();
            let Some(c) = conns.get(&sid) else { return };
            let Some(rt) = c.runtime.clone() else { return };
            (c.sink.clone(), rt)
        };
        let uri = uri.to_string();
        runtime.spawn(async move {
            match sink {
                Sink::Legacy(peer) => {
                    let _ = peer
                        .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
                        .await;
                }
                Sink::Listen(sink) => {
                    let _ = sink.notify_resource_updated(uri).await;
                }
                Sink::None => {}
                #[cfg(test)]
                Sink::Test(_) => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ib10_the_hub_records_identity_and_subscriptions() {
        let hub = NotifyHub::new();
        hub.connect(7, Sink::None);
        hub.identify(7, "agent:a", Some("r1"));
        hub.subscribe(7, "rdy://inbox");
        hub.subscribe(7, "rdy://pack");
        hub.unsubscribe(7, "rdy://pack");
        hub.connect(3, Sink::None);
        hub.subscribe(3, "rdy://pack");
        let t = hub.targets();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].sid, 3);
        assert_eq!(t[0].actor, None);
        assert_eq!(t[1].sid, 7);
        assert_eq!(t[1].actor.as_deref(), Some("agent:a"));
        assert_eq!(t[1].run.as_deref(), Some("r1"));
        assert_eq!(
            t[1].uris.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["rdy://inbox"]
        );
        // a second connect keeps who the session is and what it subscribed to
        hub.connect(7, Sink::None);
        assert_eq!(hub.targets()[1].uris.len(), 1);
        hub.disconnect(7);
        assert_eq!(hub.targets().len(), 1);
        // a notify to a session with no sink or an unknown session does nothing
        hub.notify(7, "rdy://inbox");
        hub.notify(3, "rdy://pack");
    }

    /// Final review fix round 1 (Minor 5): an `rdy_session` or a subscribe still in flight when
    /// the session ended never brings it back into the hub.
    #[test]
    fn ib10_a_late_call_after_disconnect_does_not_bring_the_session_back() {
        let hub = NotifyHub::new();
        hub.connect(7, Sink::None);
        hub.identify(7, "agent:a", Some("r1"));
        hub.disconnect(7);
        hub.identify(7, "agent:a", Some("r1"));
        hub.subscribe(7, "rdy://inbox");
        hub.subscribe_doc(7, "rdy://doc/ops", BlockId::parse("d-AAAAA").unwrap());
        assert_eq!(hub.targets(), vec![]);
    }

    /// The sink of session `sid`, as the tests tell it apart: `Some(n)` for `Sink::Test(n)`.
    fn test_sink(hub: &NotifyHub, sid: u64) -> Option<u32> {
        match hub.conns().get(&sid).map(|c| &c.sink) {
            Some(Sink::Test(n)) => Some(*n),
            _ => None,
        }
    }

    /// Final review fix round 1 (Minor 6): the end of a listen stream gives back the sink it
    /// replaced (the legacy peer) and removes only the URIs the stream added.
    #[test]
    fn ib10_a_listen_stream_end_keeps_the_legacy_sink_and_subscriptions() {
        let hub = NotifyHub::new();
        hub.connect(7, Sink::Test(1));
        hub.subscribe(7, "rdy://pack");
        let d = BlockId::parse("d-AAAAA").unwrap();
        let l = hub.listen(
            7,
            Sink::Test(2),
            vec![
                ("rdy://pack".into(), None),
                ("rdy://inbox".into(), None),
                ("rdy://doc/ops".into(), Some(d)),
            ],
        );
        assert_eq!(test_sink(&hub, 7), Some(2));
        assert_eq!(hub.targets()[0].uris.len(), 3);
        hub.end_listen(7, l);
        assert_eq!(test_sink(&hub, 7), Some(1), "the legacy sink is back");
        let t = hub.targets();
        assert_eq!(
            t[0].uris.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["rdy://pack"],
            "the legacy subscription stays"
        );
        assert!(t[0].docs.is_empty());
    }

    #[test]
    fn ib11_the_writer_lists_only_the_identified_subscribers_of_a_uri() {
        let hub = NotifyHub::new();
        for sid in [1, 2, 3, 4] {
            hub.connect(sid, Sink::None);
        }
        hub.identify(1, "agent:b", Some("b"));
        hub.identify(2, "agent:a", Some("a"));
        hub.identify(3, "agent:a", Some("a"));
        for sid in [1, 2, 3, 4] {
            hub.subscribe(sid, "rdy://inbox");
        }
        hub.subscribe(1, "rdy://pack");
        hub.unsubscribe(1, "rdy://inbox");
        assert_eq!(
            hub.subscribers("rdy://inbox"),
            vec![("agent:a".to_string(), Some("a".to_string()))],
            "one entry per (actor, run); an unidentified session has none"
        );
    }

    #[test]
    fn ib11_a_doc_subscription_keeps_the_block_it_follows() {
        let hub = NotifyHub::new();
        let d = BlockId::parse("d-AAAAA").unwrap();
        hub.connect(4, Sink::None);
        hub.subscribe_doc(4, "rdy://doc/ops", d.clone());
        let t = hub.targets();
        assert!(t[0].uris.contains("rdy://doc/ops"));
        assert_eq!(t[0].docs.get("rdy://doc/ops"), Some(&d));
        hub.unsubscribe(4, "rdy://doc/ops");
        let t = hub.targets();
        assert!(t[0].uris.is_empty() && t[0].docs.is_empty());
    }
}
