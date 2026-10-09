//! The writer: one thread owns the project's `Rdy` and applies writes in arrival order. Each
//! write's state is stored into [`Shared::state`] before its reply is sent (publish before ack),
//! so a caller that has its reply reads a state that includes its write.
//!
//! Before each write, and on a reload (a new connection's), the writer reloads the config and
//! packs if they changed on disk (`maybe_reload`).
use crate::E_DAEMON_GONE;
use crate::daemon::log;
use crate::shared::Shared;
use rdy_engine::engine::WriteFn;
use rdy_engine::{E_INTERNAL, Rdy, RdyError, ReloadOutcome};
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
    Reload(Reply<ReloadOutcome>),
    Shutdown(Reply<()>),
}

/// The writer thread's state: the project, the state it publishes into, and its queue.
pub struct Writer {
    shared: Weak<Shared>,
    rx: Receiver<Msg>,
    queued: Arc<AtomicUsize>,
    /// The idle checkpoint has failed and the failure was logged; cleared by a success.
    checkpoint_failing: bool,
    /// Settling the tension set has failed and the failure was logged; cleared by a success.
    settle_failing: bool,
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
    pub fn spawn(rdy: Rdy, shared: Weak<Shared>) -> WriterHandle {
        let (tx, rx) = mpsc::sync_channel(QUEUE_BOUND);
        let queued = Arc::new(AtomicUsize::new(0));
        let writer = Writer {
            shared,
            rx,
            queued: Arc::clone(&queued),
            checkpoint_failing: false,
            settle_failing: false,
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

    /// Stores `rdy`'s current state into the shared state. False if every `Shared` is gone.
    fn publish(&self, rdy: &Rdy) -> bool {
        match self.shared.upgrade() {
            Some(sh) => {
                sh.state.store(rdy.read());
                true
            }
            None => false,
        }
    }

    /// Reloads the config and packs if the watch saw them change. A reload bumps the pack
    /// generation and clears the pack error; files that do not load stay out (the loaded pack
    /// stays), are recorded as seen (as the reload read them, so a fix that lands meanwhile is
    /// still a change) so they are not parsed again until they change, and set the pack error,
    /// bumping the generation when the error is new. A failed check is logged.
    ///
    /// A panic never ends the writer: the caches may be half-updated, so they are cleared (the
    /// caller republishes); the previous pack stays loaded, the pack error says so, and the call
    /// answers `E_INTERNAL`.
    fn maybe_reload(&self, rdy: &mut Rdy) -> Result<ReloadOutcome, RdyError> {
        match catch_unwind(AssertUnwindSafe(|| self.reload_unguarded(rdy))) {
            Ok(out) => out,
            Err(p) => {
                rdy.invalidate();
                let text = format!(
                    "pack reload panicked: {}; the previous pack stays loaded",
                    panic_text(&*p)
                );
                // the watch still sees the change, so the next write retries the reload: like an
                // invalid pack, the generation moves and the log speaks only when the error is new
                if let Some(sh) = self.shared.upgrade() {
                    let mut error = sh.pack_error_lock();
                    if error.as_deref() != Some(text.as_str()) {
                        *error = Some(text.clone());
                        sh.pack_gen.fetch_add(1, Ordering::SeqCst);
                        drop(error);
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
        match &out {
            Ok(ReloadOutcome::Reloaded) => {
                sh.watch.seen(rdy.pack_fingerprint());
                let generation = {
                    let mut error = sh.pack_error_lock();
                    *error = None;
                    sh.pack_gen.fetch_add(1, Ordering::SeqCst) + 1
                };
                log(&format!("pack reloaded (generation {generation})"));
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
                    *error = Some(text);
                    sh.pack_gen.fetch_add(1, Ordering::SeqCst);
                    drop(error);
                    log(&format!("pack invalid: {e}"));
                }
            }
            Ok(ReloadOutcome::Unchanged) => {
                sh.watch.seen(rdy.pack_fingerprint());
                // the files are back to the loaded pack's bytes: nothing stands in error, and the
                // pack did not change
                let mut error = sh.pack_error_lock();
                if error.take().is_some() {
                    // each session hears `pack_changed` once: the pack stands as loaded, but the
                    // `pack_error` it was told about is gone
                    let generation = sh.pack_gen.fetch_add(1, Ordering::SeqCst) + 1;
                    drop(error);
                    log(&format!(
                        "pack error cleared: the files are back to the loaded pack (generation {generation})"
                    ));
                }
            }
            Err(e) => log(&format!("pack check failed: {e}")),
        }
        sh.state.store(rdy.read());
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

    /// Settles the tension set after a write (tevt1). A failure never fails the write, and a
    /// panic never ends the writer: the caches may be half-updated, so a clean state is
    /// republished. Either is logged once, until a settle succeeds again.
    fn settle(&mut self, rdy: &mut Rdy) {
        let failed = match catch_unwind(AssertUnwindSafe(|| rdy.settle_tensions())) {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(p) => {
                rdy.invalidate();
                self.publish(rdy);
                Some(format!("internal error: {}", panic_text(&*p)))
            }
        };
        match failed {
            Some(e) if !self.settle_failing => {
                self.settle_failing = true;
                log(&format!(
                    "settling the tension set failed ({e}); no tension events are sent until it succeeds, and it is retried after every write"
                ));
            }
            Some(_) => {}
            None => self.settle_failing = false,
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
                    // its outcome is logged and reported in the replies' notices; a reload's
                    // tension events are settled before the write's, with no `by`
                    if let Ok(ReloadOutcome::Reloaded) = self.maybe_reload(&mut rdy) {
                        self.settle(&mut rdy);
                    }
                    let out = match catch_unwind(AssertUnwindSafe(|| f(&mut rdy))) {
                        Ok(out) => out,
                        Err(p) => {
                            // the caches may be half-updated: republish a clean state
                            rdy.invalidate();
                            Err(RdyError::new(
                                E_INTERNAL,
                                format!("internal error on the writer: {}", panic_text(&*p)),
                                "this is a bug in rdy; the request may or may not have been applied: re-read before retrying",
                            ))
                        }
                    };
                    let published = self.publish(&rdy);
                    let _ = reply.send(out);
                    // the tension events follow the reply, before the next message
                    self.settle(&mut rdy);
                    published
                }
                Msg::Reload(reply) => {
                    let out = self.maybe_reload(&mut rdy);
                    let published = self.publish(&rdy);
                    let reloaded = matches!(out, Ok(ReloadOutcome::Reloaded));
                    let _ = reply.send(out);
                    // a reload changes the tension set without a store write (M8)
                    if reloaded {
                        self.settle(&mut rdy);
                    }
                    published
                }
                Msg::Shutdown(reply) => {
                    self.publish(&rdy);
                    let _ = reply.send(rdy.close());
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

    #[test]
    fn reload1_an_invalid_reload_never_records_bytes_newer_than_it_evaluated() {
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        let pack = dir.path().join(".rdy/packs/proj.yaml");
        std::fs::write(&pack, PACK_A).unwrap();
        std::fs::write(dir.path().join(".rdy/config.yaml"), "pack: proj\n").unwrap();
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
    }

    #[test]
    fn msw9_a_panic_in_the_pack_reload_before_a_write_does_not_kill_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let sh = Shared::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
        std::fs::write(dir.path().join(".rdy/packs/proj.yaml"), PACK_A).unwrap();
        std::fs::write(dir.path().join(".rdy/config.yaml"), "pack: proj\n").unwrap();
        *sh.after_reload.lock().unwrap() = Some(Box::new(|| panic!("reload test panic")));
        let out = sh
            .writer
            .run(Box::new(|_: &mut Rdy| Ok(serde_json::Value::Null)));
        assert!(out.is_ok(), "{out:?}");
        assert!(sh.writer.is_alive(), "the writer survives a reload panic");
        sh.shutdown().unwrap();
    }
}
