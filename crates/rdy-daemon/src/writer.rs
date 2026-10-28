//! The writer: one thread owns the project's `Rdy` and applies writes in arrival order. Each
//! write's state is stored into [`Shared::state`] before its reply is sent (publish before ack),
//! so a caller that has its reply reads a state that includes its write.
//!
//! Before each write, and on a reload (a new connection's), the writer reloads the config and
//! packs if they changed on disk (`maybe_reload`).
//!
//! After each message's publish (and the reply, for a write), the writer forwards the events of
//! the batches it sealed to the notifier (ib11), one message per batch: one unbounded channel
//! send each, so the notifier never slows a write. Each message carries the inbox subscriptions
//! of the connected sessions, read from the published state while no batch is open (ruling
//! task-11.1). The notifier reads a state that holds them. A write message forwards a pack
//! reload's batch before the write runs, so a write that fails its seal never decides it.
use crate::E_DAEMON_GONE;
use crate::daemon::log;
use crate::notify::{INBOX_URI, InboxSubs, NotifyMsg, inbox_subs};
use crate::shared::Shared;
use rdy_engine::engine::{ReadFn, WriteFn};
use rdy_engine::{E_INTERNAL, Event, Rdy, RdyError, ReloadOutcome};
use serde_json::Value;
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long the writer waits for a message before it offers the store an idle checkpoint.
pub const CHECKPOINT_TICK: Duration = Duration::from_millis(500);

/// Messages queued for the writer beyond this make senders wait.
const QUEUE_BOUND: usize = 64;

type Reply<T> = SyncSender<Result<T, RdyError>>;

enum Msg {
    Run(WriteFn, Reply<Value>),
    /// A serialized read (`Engine::read_serialized`): no write gate, no reload, no seal.
    Read(ReadFn, Reply<Value>),
    /// A clean republish after a panic elsewhere (`Engine::republish`): no write gate.
    Republish(Reply<()>),
    Reload(Reply<ReloadOutcome>),
    /// Logs what moved while no daemon ran (`Rdy::reconcile_transitions`); no reply.
    Reconcile,
    Shutdown(Reply<()>),
    /// Ends the thread without closing the store (`WriterHandle::abandon`).
    Abandon(SyncSender<()>),
}

/// The writer thread's state: the project, the state it publishes into, and its queue.
pub struct Writer {
    shared: Weak<Shared>,
    rx: Receiver<Msg>,
    /// Every event the project emits (`Rdy::subscribe`), forwarded to the notifier per batch.
    events: Receiver<Arc<Event>>,
    queued: Arc<AtomicUsize>,
    /// The idle checkpoint has failed and the failure was logged; cleared by a success.
    checkpoint_failing: bool,
    /// Logging a batch's tension moves has failed and the failure was logged; cleared once a
    /// batch logs them again.
    transitions_failing: bool,
    /// A clean republish after a panic panicked too, and the failure was logged: the published
    /// state may be stale, so the next message retries it first. Cleared by a success.
    republish_pending: bool,
}

/// A handle on the writer thread: send it writes, reloads and the shutdown.
#[derive(Clone)]
pub struct WriterHandle {
    tx: SyncSender<Msg>,
    queued: Arc<AtomicUsize>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

fn gone() -> RdyError {
    RdyError::new(
        E_DAEMON_GONE,
        "the daemon's writer has stopped",
        "run rdy serve --stop, then any command to start a fresh daemon; a write may or may not have committed",
    )
}

pub(crate) fn panic_text(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic with no message".to_string()
    }
}

impl Writer {
    /// Starts the writer thread (`rdy-writer`) owning `rdy`. It publishes into `shared` while
    /// any `Shared` lives; when none does, or every handle is dropped, it closes the store.
    /// `events` is `rdy`'s subscription (`Rdy::subscribe`): the writer forwards what it receives
    /// to the notifier after each publish.
    pub fn spawn(rdy: Rdy, events: Receiver<Arc<Event>>, shared: Weak<Shared>) -> WriterHandle {
        let (tx, rx) = mpsc::sync_channel(QUEUE_BOUND);
        let queued = Arc::new(AtomicUsize::new(0));
        let writer = Writer {
            shared,
            rx,
            events,
            queued: Arc::clone(&queued),
            checkpoint_failing: false,
            transitions_failing: false,
            republish_pending: false,
        };
        let thread = std::thread::Builder::new()
            .name("rdy-writer".into())
            .spawn(move || writer.run(rdy))
            .expect("the rdy-writer thread could not be started");
        WriterHandle {
            tx,
            queued,
            thread: Arc::new(Mutex::new(Some(thread))),
        }
    }

    /// Forwards the events received since the last forward to the notifier, one message per
    /// batch (events of one batch share their `seq`, and a batch's events arrive together). It
    /// runs after the publish that holds them. A send never blocks; with no `Shared` or no
    /// notifier the events are dropped (notifications are best effort).
    ///
    /// The inbox subscriptions are read once per message, from the state published after the
    /// whole message ran (ruling task-11.1). A message that sealed two batches, X1 then X2,
    /// decides X1 from the subscriptions as of X2; and if a later seal of the message failed,
    /// from the state left by the failure, whose index may hold the removed batch. Every
    /// `WriteFn` today seals at most one batch that can change a subscription, and no
    /// notification is ever more than best effort; snapshotting per seal would need a hook
    /// between the batches of one message.
    fn forward(&self) {
        let Ok(first) = self.events.try_recv() else {
            return;
        };
        let sh = self.shared.upgrade();
        // the inbox subscriptions as of the state just published, read here on the writer where
        // no batch is open (ruling task-11.1): the notifier never reads them from the live index.
        // Only the identified subscribers of rdy://inbox are listed, so a write with no inbox
        // subscriber clones nothing from the hub.
        let subs = Arc::new(
            sh.as_ref()
                .map(|sh| {
                    let sessions = sh.hub.subscribers(INBOX_URI);
                    if sessions.is_empty() {
                        InboxSubs::new()
                    } else {
                        inbox_subs(&sh.state.load(), &sessions)
                    }
                })
                .unwrap_or_default(),
        );
        let send = |events: Vec<Arc<Event>>| {
            if let Some(sh) = &sh {
                let subs = Arc::clone(&subs);
                let _ = sh.notify.send(NotifyMsg::Events { events, subs });
            }
        };
        let mut batch = vec![first];
        while let Ok(e) = self.events.try_recv() {
            if batch[0].seq != e.seq {
                send(std::mem::take(&mut batch));
            }
            batch.push(e);
        }
        send(batch);
    }

    /// Stores `rdy`'s current state into the shared state. False if every `Shared` is gone.
    fn publish(&self, rdy: &Rdy) -> bool {
        match self.shared.upgrade() {
            Some(sh) => {
                sh.state.store(rdy.read());
                // after the store: the site worker reads a state that holds this publish
                sh.site.published();
                true
            }
            None => false,
        }
    }

    /// After a panic the caches may be half-updated: republishes a clean state (`invalidate`). A
    /// panic in that republish is caught and logged, and the last published state stays, so the
    /// writer keeps serving; the next message retries the republish. The failure is logged once,
    /// until a retry succeeds. `after` names what panicked first.
    fn invalidate_after_panic(&mut self, rdy: &mut Rdy, after: &str) {
        match catch_unwind(AssertUnwindSafe(|| rdy.invalidate())) {
            Ok(()) => self.republish_pending = false,
            Err(p) => {
                // logged once while it keeps failing, as transitions_failing and checkpoint_failing
                if !self.republish_pending {
                    #[cfg(test)]
                    if let Some(sh) = self.shared.upgrade() {
                        sh.republish_failure_logs.fetch_add(1, Ordering::SeqCst);
                    }
                    log(&format!(
                        "the clean republish after a panic in {after} panicked too ({}); the last published state stays until a retry succeeds",
                        panic_text(&*p)
                    ));
                }
                self.republish_pending = true;
            }
        }
    }

    /// Retries a clean republish that panicked (see [`Self::invalidate_after_panic`]).
    fn retry_republish(&mut self, rdy: &mut Rdy) {
        if self.republish_pending {
            self.invalidate_after_panic(rdy, "the retried republish");
            if !self.republish_pending {
                log("the retried clean republish succeeded");
            }
        }
    }

    /// Reloads the config and packs if the watch saw them change. A reload bumps the pack
    /// generation and clears the pack error; files that do not load stay out (the loaded pack
    /// stays), are recorded as seen (as the reload read them, so a fix that lands meanwhile is
    /// still a change) so they are not parsed again until they change, and set the pack error,
    /// bumping the generation when the error is new. A failed check is logged. The generation
    /// moves only after the state that holds the move is published: the site keys its builds by
    /// the generation, and a newer generation read with an older state would keep the old
    /// pack's pages under the new key.
    ///
    /// A panic never ends the writer: the caches may be half-updated, so they are cleared and a
    /// clean state is published; the previous pack stays loaded, the pack error says so, and the
    /// call answers `E_INTERNAL`.
    fn maybe_reload(&mut self, rdy: &mut Rdy) -> Result<ReloadOutcome, RdyError> {
        match catch_unwind(AssertUnwindSafe(|| self.reload_unguarded(rdy))) {
            Ok(out) => out,
            Err(p) => {
                self.invalidate_after_panic(rdy, "the pack reload");
                let text = format!(
                    "pack reload panicked: {}; the previous pack stays loaded",
                    panic_text(&*p)
                );
                // the watch still sees the change, so the next write retries the reload: like an
                // invalid pack, the generation moves and the log speaks only when the error is new
                if let Some(sh) = self.shared.upgrade() {
                    let new = {
                        let mut error = sh.pack_error_lock();
                        let new = error.as_deref() != Some(text.as_str());
                        if new {
                            *error = Some(text.clone());
                        }
                        new
                    };
                    build_tensions(rdy);
                    sh.state.store(rdy.read());
                    // after the store, as a reload's: see the doc above
                    if new {
                        sh.pack_gen.fetch_add(1, Ordering::SeqCst);
                    }
                    sh.site.published();
                    if new {
                        log(&text);
                    }
                }
                Err(RdyError::new(
                    E_INTERNAL,
                    format!("internal error on the writer: {text}"),
                    "this is a bug in rdy; fix or revert the pack files, then retry",
                ))
            }
        }
    }

    /// The body of [`Self::maybe_reload`], outside its panic guard.
    fn reload_unguarded(&self, rdy: &mut Rdy) -> Result<ReloadOutcome, RdyError> {
        let Some(sh) = self.shared.upgrade() else {
            return Ok(ReloadOutcome::Unchanged);
        };
        if !sh.watch.changed() {
            return Ok(ReloadOutcome::Unchanged);
        }
        let out = rdy.reload_pack();
        #[cfg(test)]
        if let Some(f) = sh.after_reload.lock().unwrap().take() {
            f();
        }
        /// What moves the generation, logged once it moved.
        enum Moved {
            Reloaded,
            Invalid(String),
            Cleared,
        }
        // the pack error is set here, before the generation moves (`Shared::pack_status`)
        let moved = match &out {
            Ok(ReloadOutcome::Reloaded) => {
                sh.watch.seen(rdy.pack_fingerprint());
                *sh.pack_error_lock() = None;
                Some(Moved::Reloaded)
            }
            Ok(ReloadOutcome::Invalid {
                error: e,
                fingerprint,
            }) => {
                // the files as the reload read them, never newer: a fix landing after the read is
                // still a change
                sh.watch.seen(fingerprint);
                let text = e.to_string();
                let mut error = sh.pack_error_lock();
                if error.as_deref() != Some(text.as_str()) {
                    *error = Some(text.clone());
                    Some(Moved::Invalid(text))
                } else {
                    None
                }
            }
            Ok(ReloadOutcome::Unchanged) => {
                sh.watch.seen(rdy.pack_fingerprint());
                // the files are back to the loaded pack's bytes: nothing stands in error, and the
                // pack did not change
                // each session hears `pack_changed` once: the pack stands as loaded, but the
                // `pack_error` it was told about is gone
                sh.pack_error_lock().take().map(|_| Moved::Cleared)
            }
            Err(e) => {
                log(&format!("pack check failed: {e}"));
                None
            }
        };
        // a reload publishes a state with its tension set built, as a write does
        build_tensions(rdy);
        #[cfg(test)]
        if let Some(f) = sh.before_reload_store.lock().unwrap().take() {
            f();
        }
        sh.state.store(rdy.read());
        // after the store: a reader that sees the new generation sees this state or a newer one
        if let Some(moved) = moved {
            let generation = sh.pack_gen.fetch_add(1, Ordering::SeqCst) + 1;
            log(&match moved {
                Moved::Reloaded => format!("pack reloaded (generation {generation})"),
                Moved::Invalid(e) => format!("pack invalid: {e}"),
                Moved::Cleared => format!(
                    "pack error cleared: the files are back to the loaded pack (generation {generation})"
                ),
            });
        }
        sh.site.published();
        if matches!(out, Ok(ReloadOutcome::Reloaded)) {
            // after the publish: the notifier tells the sessions subscribed to rdy://pack and
            // rdy://project; the reply notice (`pack_changed`) stays as it is
            let _ = sh.notify.send(NotifyMsg::PackReloaded);
        }
        out
    }

    fn idle_tick(&mut self, rdy: &mut Rdy) {
        match rdy.checkpoint_if_idle() {
            Err(e) if !self.checkpoint_failing => {
                self.checkpoint_failing = true;
                log(&format!(
                    "the idle index checkpoint failed ({e}); writes are still durable in the log, and it is retried every tick"
                ));
            }
            Err(_) | Ok(false) => {}
            Ok(true) => self.checkpoint_failing = false,
        }
    }

    /// Logs, once while it stands, that a batch's tension moves could not be computed (the
    /// write succeeded; the next batch that computes logs them), and logs when it clears.
    fn log_transition_error(&mut self, rdy: &Rdy) {
        match rdy.last_transition_error() {
            Some(e) if !self.transitions_failing => {
                self.transitions_failing = true;
                log(&format!(
                    "logging tension moves failed ({e}); the next write logs them"
                ));
            }
            Some(_) => {}
            None if self.transitions_failing => {
                self.transitions_failing = false;
                log("logging tension moves works again");
            }
            None => {}
        }
    }

    /// Seals the batch a write left open, with its moves (`Rdy::seal_open`). A panic never ends
    /// the writer: the caches may be half-updated, so a clean state is republished.
    fn seal_open(&mut self, rdy: &mut Rdy) -> Result<(), RdyError> {
        match catch_unwind(AssertUnwindSafe(|| rdy.seal_open())) {
            Ok(out) => out,
            Err(p) => {
                self.invalidate_after_panic(rdy, "the seal of a write");
                Err(RdyError::new(
                    E_INTERNAL,
                    format!("internal error on the writer: {}", panic_text(&*p)),
                    "this is a bug in rdy; the request may or may not have been applied: re-read before retrying",
                ))
            }
        }
    }

    /// The loop: one message at a time, in arrival order, until shutdown or disconnect.
    fn run(mut self, mut rdy: Rdy) {
        loop {
            let msg = match self.rx.recv_timeout(CHECKPOINT_TICK) {
                Ok(msg) => {
                    self.queued.fetch_sub(1, Ordering::Relaxed);
                    msg
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.idle_tick(&mut rdy);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let published = match msg {
                Msg::Run(f, reply) => {
                    self.retry_republish(&mut rdy);
                    // its outcome is logged and reported in the replies' notices; a reload logs
                    // its moves in a batch of its own, before the write's
                    // a reload publishes, even one that panicked: the forward below reads a state
                    // that holds any batch the reload sealed
                    let _ = self.maybe_reload(&mut rdy);
                    // the reload's batch is sealed and published, and `f` has opened no batch
                    // yet: the reload's batch is forwarded now, with the subscriptions as of its
                    // publish, so a write that changes them and then fails its seal never decides
                    // it (ruling task-11.1)
                    self.forward();
                    // a read-only store (D-WWAK7) runs no write closure; the store's own gate
                    // refuses its appends too
                    let out = match rdy.ensure_writable() {
                        Err(e) => Err(e),
                        Ok(()) => match catch_unwind(AssertUnwindSafe(|| f(&mut rdy))) {
                            Ok(out) => out,
                            Err(p) => {
                                // the caches may be half-updated: republish a clean state
                                self.invalidate_after_panic(&mut rdy, "a write");
                                Err(RdyError::new(
                                    E_INTERNAL,
                                    format!("internal error on the writer: {}", panic_text(&*p)),
                                    "this is a bug in rdy; the request may or may not have been applied: re-read before retrying",
                                ))
                            }
                        },
                    };
                    // a raw write that `f` left open is sealed, with its moves, before the reply
                    let sealed = self.seal_open(&mut rdy);
                    let out = match (out, sealed) {
                        (Ok(_), Err(e)) => Err(e),
                        (out, _) => out,
                    };
                    // the first read after a big write must not build the tension set: it is
                    // built here, before the state is published (the reply waits for it)
                    build_tensions(&rdy);
                    let published = self.publish(&rdy);
                    let _ = reply.send(out);
                    // after the reply: the write's batches, one message each
                    self.forward();
                    self.log_transition_error(&rdy);
                    published
                }
                Msg::Read(f, reply) => {
                    self.retry_republish(&mut rdy);
                    // a read writes nothing, so a read-only store (D-WWAK7) runs it: it passes
                    // no write gate and seals no batch
                    let out = match catch_unwind(AssertUnwindSafe(|| f(&rdy))) {
                        Ok(out) => out,
                        Err(p) => {
                            self.invalidate_after_panic(&mut rdy, "a read");
                            Err(RdyError::new(
                                E_INTERNAL,
                                format!("internal error on the writer: {}", panic_text(&*p)),
                                "this is a bug in rdy; re-read before retrying",
                            ))
                        }
                    };
                    let published = self.publish(&rdy);
                    let _ = reply.send(out);
                    published
                }
                Msg::Republish(reply) => {
                    let out = match catch_unwind(AssertUnwindSafe(|| rdy.invalidate())) {
                        Ok(()) => {
                            self.republish_pending = false;
                            Ok(())
                        }
                        Err(p) => {
                            // the next message retries it, as after a writer panic
                            self.republish_pending = true;
                            Err(RdyError::new(
                                E_INTERNAL,
                                format!("the clean republish panicked: {}", panic_text(&*p)),
                                "this is a bug in rdy; re-read before retrying",
                            ))
                        }
                    };
                    let published = self.publish(&rdy);
                    let _ = reply.send(out);
                    published
                }
                Msg::Reload(reply) => {
                    self.retry_republish(&mut rdy);
                    let out = self.maybe_reload(&mut rdy);
                    let published = self.publish(&rdy);
                    let _ = reply.send(out);
                    self.forward();
                    self.log_transition_error(&rdy);
                    published
                }
                Msg::Reconcile => {
                    self.retry_republish(&mut rdy);
                    match catch_unwind(AssertUnwindSafe(|| rdy.reconcile_transitions())) {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => log(&format!(
                            "logging tension moves failed ({e}); the next write logs them"
                        )),
                        Err(p) => self.invalidate_after_panic(
                            &mut rdy,
                            &format!("the reconcile of tension moves ({})", panic_text(&*p)),
                        ),
                    }
                    build_tensions(&rdy);
                    let published = self.publish(&rdy);
                    self.forward();
                    self.log_transition_error(&rdy);
                    published
                }
                Msg::Shutdown(reply) => {
                    self.publish(&rdy);
                    let _ = reply.send(rdy.close());
                    return;
                }
                Msg::Abandon(reply) => {
                    // no close and no drop: either would write to the store's files, whose path
                    // may now hold another project; the lock goes with the process
                    std::mem::forget(rdy);
                    let _ = reply.send(());
                    return;
                }
            };
            if !published {
                // every `Shared` is gone: nothing can read what this thread publishes
                break;
            }
        }
        if let Err(e) = rdy.close() {
            log(&format!("closing the store failed ({e})"));
        }
    }
}

/// Builds the tension set of `rdy`'s current state, so a read of the state, once published,
/// never builds it (a big write's set takes seconds; a write that logged its moves built it
/// already). An error is memoised in the state and read as the state's tension error. A panic
/// is caught: the next read builds the set again.
fn build_tensions(rdy: &Rdy) {
    let st = rdy.read();
    let _ = catch_unwind(AssertUnwindSafe(|| st.tensions()));
}

impl WriterHandle {
    fn send(&self, msg: Msg) -> Result<(), RdyError> {
        self.queued.fetch_add(1, Ordering::Relaxed);
        self.tx.send(msg).map_err(|_| {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            gone()
        })
    }

    /// Runs `f` on the writer and returns its result once the state that includes it is
    /// published. Blocks: call it from the blocking pool, never from an async task.
    pub fn run(&self, f: WriteFn) -> Result<Value, RdyError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(Msg::Run(f, tx))?;
        rx.recv().map_err(|_| gone())?
    }

    /// Runs the read `f` on the writer, serialized with every write, and returns its result.
    /// Blocks: call it from the blocking pool, never from an async task.
    pub fn read(&self, f: ReadFn) -> Result<Value, RdyError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(Msg::Read(f, tx))?;
        rx.recv().map_err(|_| gone())?
    }

    /// Republishes a clean state from the store on the writer (`Rdy::invalidate`), once it has
    /// published. Blocks: call it from the blocking pool.
    pub fn republish(&self) -> Result<(), RdyError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(Msg::Republish(tx))?;
        rx.recv().map_err(|_| gone())?
    }

    /// Asks the writer to log what moved while no daemon ran (`Rdy::reconcile_transitions`),
    /// without waiting: the daemon calls it once after `ready`. A full queue drops it; the next
    /// write then reconciles in its own batch.
    pub fn reconcile_soon(&self) {
        self.queued.fetch_add(1, Ordering::Relaxed);
        if self.tx.try_send(Msg::Reconcile).is_err() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Reloads the config and packs on the writer if the watch saw them change (`Unchanged`
    /// otherwise). Blocks: call it from the blocking pool.
    pub fn reload(&self) -> Result<ReloadOutcome, RdyError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(Msg::Reload(tx))?;
        rx.recv().map_err(|_| gone())?
    }

    /// Publishes the last state, closes the store and joins the thread. A second call is
    /// `Ok(())`.
    pub fn shutdown(&self) -> Result<(), RdyError> {
        let mut thread = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        let Some(handle) = thread.take() else {
            return Ok(());
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let out = match self.send(Msg::Shutdown(tx)) {
            Ok(()) => rx.recv().ok(),
            Err(_) => None,
        };
        let joined = handle.join();
        match out {
            Some(out) => out,
            // the thread ended on its own: a disconnect closed the store, a panic did not
            None if joined.is_ok() => Ok(()),
            None => Err(gone()),
        }
    }

    /// Ends the writer thread without closing the store or writing to it, and joins it: for a
    /// daemon whose project is gone or replaced (final ruling), which exits right after. The
    /// store stays locked until the process exits. A later `shutdown` is `Ok(())`.
    pub fn abandon(&self) {
        let mut thread = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        let Some(handle) = thread.take() else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(1);
        if self.send(Msg::Abandon(tx)).is_ok() {
            let _ = rx.recv();
        }
        let _ = handle.join();
    }

    /// Messages sent and not yet taken by the writer.
    #[doc(hidden)]
    pub fn queue_len(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    /// The writer thread is running (not shut down and not ended).
    #[doc(hidden)]
    pub fn is_alive(&self) -> bool {
        self.thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|h| !h.is_finished())
    }
}

#[cfg(test)]
mod tests {
    use crate::shared::Shared;
    use rdy_engine::{Rdy, ReloadOutcome};

    const PACK_A: &str = "name: proj\nversion: '1'\nextends: rdy-standard\n";
    const PACK_BAD: &str = "name: proj\nversion: '1'\nextends: no-such-pack-at-all\n";
    const PACK_FIXED: &str = "# fixed\nname: proj\nversion: '2'\nextends: rdy-standard\n";

    /// The daemon lines a fault test expects ([`crate::daemon::capture_log_for_tests`]), taken
    /// while no other such test of this module runs: their lines overlap ("the retried clean
    /// republish succeeded"), and each test asserts its own.
    struct FaultLog {
        capture: crate::daemon::LogCapture,
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    fn fault_log(expected: &[&str]) -> FaultLog {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        FaultLog {
            capture: crate::daemon::capture_log_for_tests(expected),
            _serial: serial,
        }
    }

    /// The text every test hook's full-publish panic gives.
    const HOOK: &str = "test hook: a full publish panics";

    #[test]
    fn reload1_an_invalid_reload_never_records_bytes_newer_than_it_evaluated() {
        let log = fault_log(&["pack invalid: "]);
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        let pack = dir.path().join(".rdy/packs/proj.yaml");
        std::fs::write(&pack, PACK_A).unwrap();
        std::fs::write(dir.path().join(".rdy.yaml"), "pack: proj\n").unwrap();
        let out = sh.writer.reload().unwrap();
        assert!(matches!(out, ReloadOutcome::Reloaded), "{out:?}");
        // the reload reads the invalid file, then the file is fixed before anything is recorded
        std::fs::write(&pack, PACK_BAD).unwrap();
        let fixed = pack.clone();
        *sh.after_reload.lock().unwrap() = Some(Box::new(move || {
            std::fs::write(&fixed, PACK_FIXED).unwrap()
        }));
        let out = sh.writer.reload().unwrap();
        assert!(matches!(out, ReloadOutcome::Invalid { .. }), "{out:?}");
        assert!(sh.pack_status().1.is_some());
        assert!(
            sh.watch.changed(),
            "the fixed bytes were never evaluated: the watch must still see a change"
        );
        let out = sh.writer.reload().unwrap();
        assert!(matches!(out, ReloadOutcome::Reloaded), "{out:?}");
        assert_eq!(sh.pack_status().1, None, "the fix clears the pack error");
        assert_eq!(sh.state.load().pack().version, "2");
        sh.shutdown().unwrap();
        let lines = log.capture.lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0]
                .starts_with("pack invalid: E_CONFIG: unknown pack \"no-such-pack-at-all\" (fix: "),
            "{lines:?}"
        );
    }

    #[test]
    fn msw9_a_panic_in_the_pack_reload_before_a_write_does_not_kill_the_writer() {
        let log = fault_log(&["pack reload panicked", "clean republish"]);
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        std::fs::write(dir.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
        std::fs::write(dir.path().join(".rdy.yaml"), "pack: proj\n").unwrap();
        *sh.after_reload.lock().unwrap() = Some(Box::new(|| panic!("reload test panic")));
        let out = sh
            .writer
            .run(Box::new(|_: &mut Rdy| Ok(serde_json::Value::Null)));
        assert!(out.is_ok(), "{out:?}");
        assert!(sh.writer.is_alive(), "the writer survives a reload panic");
        sh.shutdown().unwrap();
        assert_eq!(
            log.capture.lines(),
            ["pack reload panicked: reload test panic; the previous pack stays loaded"]
        );
    }

    #[test]
    fn msw9_a_panic_inside_the_pack_reloads_publish_does_not_kill_the_writer() {
        let log = fault_log(&["pack reload panicked", "clean republish"]);
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        // reload_pack's own publish panics, then the panic arm's clean republish panics too
        let out = sh.writer.run(Box::new(|rdy: &mut Rdy| {
            rdy.panic_full_publishes_for_tests(2);
            Ok(serde_json::Value::Null)
        }));
        assert!(out.is_ok(), "{out:?}");
        std::fs::write(dir.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
        std::fs::write(dir.path().join(".rdy.yaml"), "pack: proj\n").unwrap();
        let out = sh
            .writer
            .run(Box::new(|_: &mut Rdy| Ok(serde_json::Value::Null)));
        assert!(out.is_ok(), "{out:?}");
        assert!(
            sh.writer.is_alive(),
            "the writer survives a panic in the reload's publish"
        );
        assert!(
            sh.pack_status()
                .1
                .is_some_and(|e| e.contains("pack reload panicked")),
            "{:?}",
            sh.pack_status()
        );
        // the hook is spent: the next write first retries the clean republish, so the state
        // it publishes holds the pack that reload_pack swapped in
        let out = sh
            .writer
            .run(Box::new(|_: &mut Rdy| Ok(serde_json::Value::Null)));
        assert!(out.is_ok(), "{out:?}");
        assert_eq!(
            sh.pack_status().1,
            None,
            "the reload succeeds and clears the error"
        );
        assert_eq!(sh.state.load().pack().name, "proj");
        sh.shutdown().unwrap();
        assert_eq!(
            log.capture.lines(),
            [
                format!(
                    "the clean republish after a panic in the pack reload panicked too ({HOOK}); the last published state stays until a retry succeeds"
                ),
                format!("pack reload panicked: {HOOK}; the previous pack stays loaded"),
                "the retried clean republish succeeded".to_string(),
            ]
        );
    }

    /// Final review M4: a clean republish that keeps failing logs once, not on every message,
    /// until a retry succeeds.
    #[test]
    fn msw9_a_republish_that_keeps_failing_logs_once() {
        use std::sync::atomic::Ordering;
        let log = fault_log(&["pack reload panicked", "clean republish"]);
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        // the write panics; its clean republish and the next three retries panic too
        let out = sh.writer.run(Box::new(|rdy: &mut Rdy| {
            rdy.panic_full_publishes_for_tests(4);
            panic!("write test panic")
        }));
        assert!(out.is_err());
        for _ in 0..4 {
            let out = sh
                .writer
                .run(Box::new(|_: &mut Rdy| Ok(serde_json::Value::Null)));
            assert!(out.is_ok(), "{out:?}");
        }
        assert_eq!(sh.republish_failure_logs.load(Ordering::SeqCst), 1);
        sh.shutdown().unwrap();
        assert_eq!(
            log.capture.lines(),
            [
                format!(
                    "the clean republish after a panic in a write panicked too ({HOOK}); the last published state stays until a retry succeeds"
                ),
                "the retried clean republish succeeded".to_string(),
            ]
        );
    }

    #[test]
    fn msw9_a_panic_in_a_writes_clean_republish_does_not_kill_the_writer() {
        let log = fault_log(&["pack reload panicked", "clean republish"]);
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        // the write panics, then the f arm's clean republish panics too
        let out = sh.writer.run(Box::new(|rdy: &mut Rdy| {
            rdy.panic_full_publishes_for_tests(1);
            panic!("write test panic")
        }));
        let e = out.unwrap_err();
        assert_eq!(e.code, rdy_engine::E_INTERNAL, "{e:?}");
        assert!(
            sh.writer.is_alive(),
            "the writer survives a panic in the clean republish"
        );
        let out = sh
            .writer
            .run(Box::new(|_: &mut Rdy| Ok(serde_json::Value::Null)));
        assert!(out.is_ok(), "{out:?}");
        sh.shutdown().unwrap();
        assert_eq!(
            log.capture.lines(),
            [
                format!(
                    "the clean republish after a panic in a write panicked too ({HOOK}); the last published state stays until a retry succeeds"
                ),
                "the retried clean republish succeeded".to_string(),
            ]
        );
    }

    /// Fix round 2 of task 11 (ruling task-11.1): in one write message, a pack reload seals and
    /// publishes its own batch R with a move on a's change; then the write's held batch moves a's
    /// cursor past R and its seal fails, so the live index keeps that cursor. The decision for R
    /// reads a's subscription as R's publish left it: a is told.
    #[test]
    fn ib11_a_reload_batch_is_decided_before_the_write_in_the_same_message() {
        use rdy_engine::{Session, WriteRequest};
        use rdy_mcp::notify::Sink;
        use rdy_model::Role;
        use serde_json::{Value, json};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        let engine = sh.engine();
        let write = |s: Session, view: &str, ops: Value, land: bool| {
            let view = view.to_string();
            engine
                .write(Box::new(move |r: &mut Rdy| {
                    let w = r.write(
                        &s,
                        WriteRequest {
                            view: view.clone(),
                            ops: serde_json::from_value(ops).unwrap(),
                            justification: None,
                            message: String::new(),
                        },
                    )?;
                    if land {
                        let h = Session::human("human:qa".into(), Role::Approver).unwrap();
                        let p = r.propose(&s, &view, "t", Some("test summary"))?;
                        r.land(&h, &p.id)?;
                    }
                    Ok(serde_json::to_value(w).unwrap())
                }))
                .unwrap()
        };
        let agent =
            |n: &str| Session::agent(format!("agent:{n}"), Some(n.into()), Role::Writer).unwrap();
        // claim A, landed; then agents a and b each update it in their own views: a collision
        let w = write(
            Session::human("human:qa".into(), Role::Writer).unwrap(),
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
                {"op": "create", "type": "claim", "id": "$a", "parent": "$d", "body": "A holds."}
            ]),
            true,
        );
        let a = w["created"]["$a"].as_str().unwrap().to_string();
        let upd = |body: &str| json!([{"op": "update", "id": a, "body": body}]);
        write(agent("a"), "a", upd("A by a."), false);
        write(agent("b"), "b", upd("A by b."), false);
        rdy_engine::inbox::sub::subscribe(&*engine, &agent("a"), None).unwrap();
        // a's MCP session, subscribed to rdy://inbox
        sh.hub.connect(1, Sink::None);
        sh.hub.identify(1, "agent:a", Some("a"));
        sh.hub.subscribe(1, "rdy://inbox");
        // a pack that makes the collision advisory: the next write reloads it first, in a batch
        // of its own that re-keys the collision on a's change
        std::fs::write(
            dir.path().join(".rdy/packs/soft.yaml"),
            "name: soft\nversion: \"1\"\nextends: rdy-standard\ntensions:\n  edit_collision: advisory\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".rdy.yaml"), "pack: soft\n").unwrap();
        let before = sh.state.load().seq();
        let out = sh.writer.run(Box::new(|r: &mut Rdy| {
            // the write's held batch acks a's inbox past the reload's batch; its seal fails
            let mut sub = r.read().reader().inbox("agent:a", Some("a"))?.unwrap();
            sub.cursor.at = r.store().log_end();
            r.store_mut().put_inbox(&sub)?;
            r.store_mut().fail_next_append_after(0);
            Ok(Value::Null)
        }));
        assert!(out.is_err(), "the seal must fail: {out:?}");
        assert_eq!(sh.state.load().pack().name, "soft", "the reload ran");
        let live = sh
            .state
            .load()
            .reader()
            .inbox("agent:a", Some("a"))
            .unwrap()
            .unwrap();
        assert!(
            live.cursor.at > before,
            "the live index holds the unsealed cursor, past the reload's batch: {live:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let told = || {
            sh.delivered
                .lock()
                .unwrap()
                .contains(&(1, "rdy://inbox".to_string()))
        };
        while !told() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            told(),
            "a hears of the reload's move: {:?}",
            sh.delivered.lock().unwrap()
        );
        let _ = sh.shutdown();
    }
}
