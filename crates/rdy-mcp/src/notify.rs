//! ib10: the hub of connected MCP sessions: who is connected, who they are (`rdy_session`) and
//! which resources they subscribed to. The hub only records and sends. Which session to tell about
//! which event is the caller's decision (the daemon, Task 11): it reads [`NotifyHub::targets`] and
//! calls [`NotifyHub::notify`].
//!
//! A notification is best effort (spec §10.3): the send runs on the tokio runtime that was current
//! when the session connected, and a failed send is dropped. The inbox is the reliable path.
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
}

/// One session as the hub knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub sid: u64,
    pub actor: Option<String>,
    pub run: Option<String>,
    pub uris: BTreeSet<String>,
}

struct Conn {
    sink: Sink,
    /// The runtime current at `connect`; a send is spawned on it.
    runtime: Option<tokio::runtime::Handle>,
    actor: Option<String>,
    run: Option<String>,
    uris: BTreeSet<String>,
}

impl Conn {
    fn new() -> Conn {
        Conn {
            sink: Sink::None,
            runtime: None,
            actor: None,
            run: None,
            uris: BTreeSet::new(),
        }
    }
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

    /// Records where session `sid` can be reached. The runtime current now is the one sends use.
    /// A session that is already known keeps its identity and subscriptions.
    pub fn connect(&self, sid: u64, sink: Sink) {
        let runtime = tokio::runtime::Handle::try_current().ok();
        let mut conns = self.conns();
        let c = conns.entry(sid).or_insert_with(Conn::new);
        c.sink = sink;
        c.runtime = runtime;
    }

    /// Records who session `sid` is (`rdy_session`).
    pub fn identify(&self, sid: u64, actor: &str, run: Option<&str>) {
        let mut conns = self.conns();
        let c = conns.entry(sid).or_insert_with(Conn::new);
        c.actor = Some(actor.to_string());
        c.run = run.map(str::to_string);
    }

    pub fn subscribe(&self, sid: u64, uri: &str) {
        let mut conns = self.conns();
        let c = conns.entry(sid).or_insert_with(Conn::new);
        c.uris.insert(uri.to_string());
    }

    pub fn unsubscribe(&self, sid: u64, uri: &str) {
        if let Some(c) = self.conns().get_mut(&sid) {
            c.uris.remove(uri);
        }
    }

    /// Forgets session `sid`: its identity, subscriptions and sink.
    pub fn disconnect(&self, sid: u64) {
        self.conns().remove(&sid);
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
}
