//! Explicit request order for an MCP session (DECISIONS-rdy-m2 §8: "a per-connection sequence
//! and an in-order writer, not by scheduler accident").
//!
//! rmcp runs every request in a task of its own and sends replies from a set of concurrent send
//! tasks, so neither the order in which tool bodies start nor the order in which replies reach
//! the stream is promised by rmcp; it depends on how the runtime polls those tasks. Three pieces
//! make it explicit:
//!
//! - [`Stamped`], the transport, stamps each `tools/call` with the next [`Seq`] as its line is
//!   read (reads are sequential), in the request's extensions, which rmcp hands to the handler.
//! - [`Ordered`], the service, makes each stamped call wait for its turn before rmcp's dispatch
//!   runs it, and passes the turn on when the call's reply is built: tool calls apply in arrival
//!   order.
//! - [`Stamped::send`] holds a stamped call's reply until every earlier stamped reply has been
//!   written: replies leave in arrival order.
//!
//! A turn whose holder goes away without taking it (a call cancelled by the client, whose reply
//! rmcp then drops, or a session torn down mid-wait) is retired, so the calls after it are not
//! held. Requests other than `tools/call` (`initialize`, `ping`, `tools/list`, ...) are not
//! stamped and are neither held nor ordered.
use rmcp::model::{
    ClientNotification, ClientRequest, GetExtensions, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, RequestId,
};
use rmcp::service::{
    NotificationContext, RequestContext, RoleServer, RxJsonRpcMessage, Service, ServiceRole,
    TxJsonRpcMessage,
};
use rmcp::transport::Transport;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// A `tools/call`'s place in its session's arrival order, stamped by [`Stamped`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seq(pub u64);

/// Turns taken in sequence order: turn `n` starts once every turn before it is done or retired.
#[derive(Default)]
pub struct Turns {
    state: Mutex<TurnState>,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct TurnState {
    /// The turn being taken (every lower one is done or retired).
    next: u64,
    /// Turns above `next` that will not be taken.
    retired: BTreeSet<u64>,
}

impl Turns {
    fn lock(&self) -> MutexGuard<'_, TurnState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Waits for turn `seq`. The returned [`Turn`] passes the turn on when dropped; dropped
    /// before its turn came (this future cancelled), it retires the turn.
    pub async fn take(self: &Arc<Self>, seq: u64) -> Turn {
        let turn = Turn {
            turns: Arc::clone(self),
            seq,
        };
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            // registered before the check, so a pass between the check and the wait is seen
            changed.as_mut().enable();
            // above `seq` only when `seq` itself was retired: then there is nothing to wait for
            if self.lock().next >= seq {
                return turn;
            }
            changed.await;
        }
    }

    /// Ends turn `seq`: passes it on if it is the current one, or retires it if it is still to
    /// come. A turn already passed is left alone, so ending one twice is harmless.
    pub fn end(&self, seq: u64) {
        {
            let mut st = self.lock();
            if seq == st.next {
                st.next += 1;
                loop {
                    let next = st.next;
                    if !st.retired.remove(&next) {
                        break;
                    }
                    st.next += 1;
                }
            } else if seq > st.next {
                st.retired.insert(seq);
            }
        }
        self.changed.notify_waiters();
    }

    /// The turn being taken (for the tests).
    #[doc(hidden)]
    pub fn current(&self) -> u64 {
        self.lock().next
    }
}

/// A held (or awaited) turn; dropping it ends the turn.
pub struct Turn {
    turns: Arc<Turns>,
    seq: u64,
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.turns.end(self.seq);
    }
}

/// What a session's transport and service share.
#[derive(Default)]
pub struct Order {
    /// Tool bodies, in arrival order.
    run: Arc<Turns>,
    /// Replies to tool calls, in arrival order.
    write: Arc<Turns>,
    /// The stamped calls whose reply has not been written yet, by request id.
    pending: Mutex<HashMap<RequestId, VecDeque<u64>>>,
}

impl Order {
    fn pending(&self) -> MutexGuard<'_, HashMap<RequestId, VecDeque<u64>>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The transport side: stamps `tools/call` requests as they are read and writes their replies
/// in stamp order. Everything else passes through.
pub struct Stamped<T> {
    inner: T,
    order: Arc<Order>,
    next: u64,
}

impl<T> Stamped<T> {
    pub fn new(inner: T, order: Arc<Order>) -> Stamped<T> {
        Stamped {
            inner,
            order,
            next: 0,
        }
    }

    fn stamp(&mut self, msg: &mut RxJsonRpcMessage<RoleServer>) {
        match msg {
            JsonRpcMessage::Request(JsonRpcRequest { id, request, .. })
                if matches!(request, ClientRequest::CallToolRequest(_)) =>
            {
                let seq = self.next;
                self.next += 1;
                request.extensions_mut().insert(Seq(seq));
                let mut pending = self.order.pending();
                let queued = pending.entry(id.clone()).or_default();
                if queued.is_empty() {
                    queued.push_back(seq);
                } else {
                    // an id already in flight (a client bug): rmcp answers only one of the two,
                    // so this one's reply is not waited for and is written when it comes
                    drop(pending);
                    self.order.write.end(seq);
                }
            }
            JsonRpcMessage::Notification(JsonRpcNotification {
                notification: ClientNotification::CancelledNotification(c),
                ..
            }) => {
                // rmcp drops the reply of a call cancelled before it is sent: its write turn
                // is retired here, before rmcp sees the notification
                let Some(id) = &c.params.request_id else {
                    return;
                };
                let seq = {
                    let mut pending = self.order.pending();
                    let seq = pending.get_mut(id).and_then(VecDeque::pop_front);
                    if pending.get(id).is_some_and(VecDeque::is_empty) {
                        pending.remove(id);
                    }
                    seq
                };
                if let Some(seq) = seq {
                    self.order.write.end(seq);
                }
            }
            _ => {}
        }
    }

    /// The stamp of the call `msg` answers, if it is a reply to one.
    fn reply_seq(&self, msg: &TxJsonRpcMessage<RoleServer>) -> Option<u64> {
        let id = match msg {
            JsonRpcMessage::Response(r) => &r.id,
            JsonRpcMessage::Error(e) => e.id.as_ref()?,
            _ => return None,
        };
        let mut pending = self.order.pending();
        let seq = pending.get_mut(id).and_then(VecDeque::pop_front);
        if pending.get(id).is_some_and(VecDeque::is_empty) {
            pending.remove(id);
        }
        seq
    }
}

impl<T> Transport<RoleServer> for Stamped<T>
where
    T: Transport<RoleServer>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let seq = self.reply_seq(&item);
        let write = Arc::clone(&self.order.write);
        // the inner send does nothing until polled (it takes the stream's lock then)
        let send = self.inner.send(item);
        async move {
            let _turn = match seq {
                Some(seq) => Some(write.take(seq).await),
                None => None,
            };
            send.await
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        let mut msg = self.inner.receive().await?;
        self.stamp(&mut msg);
        Some(msg)
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

/// The service side: a stamped call waits for its turn, and holds it until its reply is built.
pub struct Ordered<S> {
    inner: S,
    order: Arc<Order>,
}

impl<S> Ordered<S> {
    pub fn new(inner: S, order: Arc<Order>) -> Ordered<S> {
        Ordered { inner, order }
    }
}

impl<S: Service<RoleServer>> Service<RoleServer> for Ordered<S> {
    async fn handle_request(
        &self,
        request: <RoleServer as ServiceRole>::PeerReq,
        context: RequestContext<RoleServer>,
    ) -> Result<<RoleServer as ServiceRole>::Resp, rmcp::ErrorData> {
        let _turn = match context.extensions.get::<Seq>() {
            Some(&Seq(seq)) => Some(self.order.run.take(seq).await),
            None => None,
        };
        self.inner.handle_request(request, context).await
    }

    fn handle_notification(
        &self,
        notification: <RoleServer as ServiceRole>::PeerNot,
        context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = Result<(), rmcp::ErrorData>> + Send + '_ {
        self.inner.handle_notification(notification, context)
    }

    fn get_info(&self) -> <RoleServer as ServiceRole>::Info {
        self.inner.get_info()
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [rmcp::model::ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn ord1_turns_go_in_order_and_a_retired_turn_is_skipped() {
        let t = Arc::new(Turns::default());
        let order = Arc::new(Mutex::new(vec![]));
        let mut tasks = vec![];
        // started newest first: the order comes from the stamps, not from who waits first
        for seq in [4u64, 3, 1, 0] {
            let (t, order) = (Arc::clone(&t), Arc::clone(&order));
            tasks.push(tokio::spawn(async move {
                let _turn = t.take(seq).await;
                order.lock().unwrap().push(seq);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
        }
        // turn 2 never comes: retiring it lets 3 and 4 go
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*order.lock().unwrap(), vec![0, 1]);
        t.end(2);
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 3, 4]);
        assert_eq!(t.current(), 5);
        // ending a passed turn again changes nothing
        t.end(3);
        assert_eq!(t.current(), 5);
    }

    #[tokio::test]
    async fn ord1_a_wait_dropped_before_its_turn_retires_it() {
        let t = Arc::new(Turns::default());
        let first = t.take(0).await;
        let waiting = {
            let t = Arc::clone(&t);
            tokio::spawn(async move {
                let _turn = t.take(1).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        waiting.abort();
        let _ = waiting.await;
        drop(first);
        // turn 1 was retired by the dropped wait: turn 2 does not hang
        let next = tokio::time::timeout(Duration::from_secs(2), t.take(2)).await;
        assert!(next.is_ok(), "turn 2 waited for an abandoned turn 1");
    }
}
