//! The destination daemon's fetch of its project's incomplete history (D-GGTYA, O17).
//!
//! A URL clone over MCP or through a daemon returns once the project is ready and hands the
//! rest of its history to the daemon of the new project ([`hand_off`]): the clone starts or
//! wakes that daemon through the ordinary connect and start steps and never queues work on
//! the writer of the project it was called from. A daemon scans for an incomplete history once
//! at its own startup ([`run`], after ready) and then advances it one step at a time:
//!
//! - each step runs on the runtime's blocking pool, one awaited step at a time; only the
//!   checked take and the final ref batch run on the writer, as the engine's step asks;
//! - before each step every call already queued on the writer runs first;
//! - the fetch owns the remote's one-sync guard for its whole life, so a second sync of that
//!   remote meanwhile gets the ordinary `E_LOCKED`;
//! - a stop ends the fetch at the next step boundary; the runtime's drop joins the step in
//!   flight before the store closes, and the checked staged files stay for the next start.
//!
//! The store holds at most one incomplete history, so one job per daemon is enough.
use crate::client::{StartOptions, connect_or_start};
use crate::daemon::{DaemonHandle, log};
use crate::layout::RunDir;
use crate::session::Proto;
use crate::shared::Shared;
use crate::spawn::{serve_command, spawn_serve};
use rdy_engine::engine::Engine;
use rdy_engine::history_fetch::HistoryFetchStep;
use rdy_engine::{E_INTERNAL, E_IO, RdyError, Session, classify};
use rdy_model::Role;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

/// The remote whose history the project of `engine` still fetches, if any.
fn pending_remote(engine: &dyn Engine) -> Result<Option<String>, RdyError> {
    let read = engine.read();
    let histories = read.reader().histories()?;
    Ok(histories
        .into_iter()
        .find(|(_, h)| !h.complete)
        .map(|(name, _)| name))
}

/// Runs every call queued on the writer before the next step: a no-op read joins the queue
/// behind them, and returns once they ran.
async fn yield_to_calls(shared: &Arc<Shared>) {
    while shared.writer.queue_len() > 0 {
        let sh = Arc::clone(shared);
        let _ = tokio::task::spawn_blocking(move || {
            sh.writer.read(Box::new(|_| Ok(serde_json::Value::Null)))
        })
        .await;
    }
}

/// The daemon's history job: finds the project's incomplete history, if any, and fetches it
/// to its end between calls (module docs). `human` is the daemon's configured human, as a
/// session names it; none reads it from the environment. Errors are logged; the project stays
/// usable and incomplete, and the next pull or start resumes it.
pub(crate) async fn run(shared: Arc<Shared>, handle: DaemonHandle, human: Option<String>) {
    let stopped = handle.stopped();
    if *stopped.borrow() {
        return;
    }
    let engine = shared.engine();
    let remote = match pending_remote(&*engine) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            log(&format!("history fetch not started: {e}"));
            return;
        }
    };
    // the daemon stays up while the history arrives
    let _hold = handle.hold();
    let root = shared.root.clone();
    let session = match human
        .map_or_else(|| rdy_engine::human_actor_for_env(&root), Ok)
        .and_then(|h| classify(&h, None, None, Role::Writer))
    {
        Ok(s) => s,
        Err(e) => {
            log(&format!("history fetch not started: {e}"));
            return;
        }
    };
    log(&format!("history fetch from remote {remote} started"));
    let begin = {
        let engine = Arc::clone(&engine);
        let root = root.clone();
        let session = session.clone();
        tokio::task::spawn_blocking(move || {
            crate::ops::sync::with_service(&root, |s| s.begin_history(&*engine, &session, &remote))
        })
        .await
    };
    let fetch = match begin {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => {
            log(&format!("history fetch stopped: {e}"));
            return;
        }
        Err(e) => {
            log(&format!("history fetch stopped: {e}"));
            return;
        }
    };
    let step = move |fetch: &mut rdy_engine::history_fetch::HistoryFetch| {
        crate::ops::sync::with_service(&root, |s| s.resume_history_step(&*engine, &session, fetch))
    };
    match drive(&shared, stopped, fetch, step).await {
        None => log("history fetch paused by the stop; the next start resumes it"),
        Some(Ok(p)) => log(&format!(
            "history fetch complete: {} of {} records",
            p.held, p.total
        )),
        Some(Err(e)) => log(&format!("history fetch stopped: {e}")),
    }
}

/// The steps of one fetch: before each, every call queued on the writer runs; then `step`
/// runs on the blocking pool, awaited, with `fetch` moved in and back (a stop that drops this
/// task while a step runs drops the fetch, and its guard, with the step's end). Ends at the
/// first complete step (its progress), the first error, or a stop seen at a step boundary
/// (None). Public for the daemon's tests, which drive it with their own steps.
#[doc(hidden)]
pub async fn drive<F: Send + 'static>(
    shared: &Arc<Shared>,
    stopped: tokio::sync::watch::Receiver<bool>,
    mut fetch: F,
    step: impl Fn(&mut F) -> Result<HistoryFetchStep, RdyError> + Send + Sync + 'static,
) -> Option<Result<rdy_engine::partial_history::HistoryProgress, RdyError>> {
    let step = Arc::new(step);
    loop {
        if *stopped.borrow() {
            return None;
        }
        yield_to_calls(shared).await;
        let step = Arc::clone(&step);
        let stepped = tokio::task::spawn_blocking(move || {
            let r = step(&mut fetch);
            (fetch, r)
        })
        .await;
        let (back, r) = match stepped {
            Ok(v) => v,
            Err(e) => {
                return Some(Err(RdyError::new(
                    E_INTERNAL,
                    format!("a history step did not finish: {e}"),
                    "run rdy pull to resume the history",
                )));
            }
        };
        fetch = back;
        match r {
            Ok(HistoryFetchStep::More(_)) => {}
            Ok(HistoryFetchStep::Complete(p)) => return Some(Ok(p)),
            Err(e) => return Some(Err(e)),
        }
    }
}

/// Hands the incomplete history of the project at `dir` to that project's own daemon: starts
/// or wakes it through the ordinary connect and start steps, opens a session as `session`'s
/// actor and run, and pings it. The daemon took the fetch over at its startup scan ([`run`]),
/// so true once it answered. The ask runs on a runtime and a thread of its own, so no lock or
/// runtime of the caller is held across it.
pub fn hand_off(dir: &Path, session: &Session, version: &str) -> Result<bool, RdyError> {
    let project = dir.to_path_buf();
    let run = RunDir::of_project(dir);
    let spawned = project.clone();
    let mut opts = StartOptions::new(
        version.to_string(),
        Proto::Ctl,
        project.clone(),
        Box::new(move |run| {
            spawn_serve(&mut serve_command(
                &std::env::current_exe()?,
                &spawned,
                run,
            )?)
        }),
    );
    opts.start_timeout = rdy_engine::serve_config(&project).start_timeout;
    let actor = json!({"actor": session.actor, "run": session.run});
    std::thread::scope(|s| {
        s.spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    RdyError::new(
                        E_IO,
                        format!("cannot start a runtime to reach the clone's daemon: {e}"),
                        "run rdy pull in the clone",
                    )
                })?;
            rt.block_on(async move {
                let mut c = connect_or_start(&run, opts)
                    .await
                    .map_err(crate::maintenance::rdy_error)?;
                c.ctl("session", actor)
                    .await
                    .map_err(crate::maintenance::rdy_error)?;
                c.ctl("ping", json!({}))
                    .await
                    .map_err(crate::maintenance::rdy_error)?;
                c.close();
                Ok::<bool, RdyError>(true)
            })
        })
        .join()
        .unwrap_or_else(|_| {
            Err(RdyError::new(
                E_INTERNAL,
                "the hand-off to the clone's daemon panicked",
                "this is a bug in rdy; run rdy pull in the clone",
            ))
        })
    })
}
