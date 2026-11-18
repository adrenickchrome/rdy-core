//! Restore through the sync exchange (D-J3YA0, D-D5J18, D-QMZ92, D-0WWYR, D-H0BZN, D-47HVF):
//! the engine part of a dump's restore into a hub repo.
//!
//! 1. **Load.** The dump, and its increments when given, load into a new temporary directory
//!    under the system temp root with every check of a load (`load::load_chain`). The caller's
//!    dump files are only read. A project dump loads as a project with its project files; a hub
//!    dump loads as a holder.
//! 2. **State.** The caller's share remote, its trust and sign-in, then discovery and the state
//!    step, as a push runs them, naming the hub dump's views (at most 256). The hub's `min_rdy`
//!    and the stream format are checked; the dump's repo id must equal the hub repo's, or the
//!    hub repo has none (`E_REPO_MISMATCH`). The base rule does not apply: the hub chooses the
//!    seed or the force move, and its gate rules allow or refuse it.
//! 3. **Canon.** From the temporary store: the canon history rooted at its canon tip, above the
//!    hub tip when the temporary store holds it, with known and put under one push id, then one
//!    write of the kind `restore` that expects the state's tip and names the tags the hub
//!    lacks.
//! 4. **Inputs.** A project dump's gate settings and render inputs come from its own project
//!    files, captured once by this client at the dump's tip (so `rdy` names this client). A
//!    dump whose `.rdy.yaml` names no title sends no render inputs, never a title made from the
//!    temporary directory's name; its gate still names the pack by its digest. A hub dump sends
//!    no gate and no render inputs. A tag sent with no render inputs carries no disagreements
//!    and is listed in the outcome.
//! 5. **Views.** After the canon write's reply, each view of a hub dump that the hub lacks, or
//!    whose hub head the dump's chain holds, goes as its own upload and view write, with its own
//!    push id, expecting the hub's head. A hub view that moved past the dump's is not sent and
//!    is listed as kept. A project dump's views stay on the machine. Held chains and replaced
//!    marks travel only as records that canon or a view names.
//! 6. **No resumable state.** A restore records no `upload` or `pending` ref; the temporary
//!    store goes when the restore ends. A write whose reply was lost is looked up once, in the
//!    same run, by the push id it holds: taken, held and refused are reported; an unknown
//!    outcome or a failed lookup stops with `E_UPSTREAM_UNCERTAIN` in publish's words, naming
//!    the push id.
//! 7. **Cleanup.** The temporary store is removed after success, a refusal, a failure, and when
//!    the call unwinds. A cleanup failure is reported and never hides the restore's result.
//!
//! No store writer is held during a network step: the temporary store is only read once
//! loaded, and the caller's store is never opened.
use crate::Rdy;
use crate::error::{
    E_UPSTREAM_UNCERTAIN, RdyError, UPSTREAM_UNCERTAIN_FIX, upstream_protocol, upstream_refused,
    upstream_uncertain,
};
use crate::handle::io;
use crate::hub_publish_stream::{
    BatchLimits, HistorySent, PreparedHistory, PushId, missing_with, send_objects, send_root,
};
use crate::hub_publish_wire::UploadContext;
use crate::hub_sync::{
    CanonPreview, SyncObjects, SyncService, Target, WriteEnd, gate_settings, preview_move,
    write_refusal,
};
use crate::hub_sync_wire::{
    GateSettings, LookupOutcome, SyncState, WriteFailure, WriteKind, WriteOutcome, WriteRequest,
    WriteTag, check_view_names, encode_write,
};
use crate::read_state::ReadState;
use crate::render_inputs::{RenderInputs, validate_inputs};
use crate::session::{Scope, Session};
use rdy_model::Hash;
use rdy_schema::{Issue, Severity};
use rdy_store::holder::{StoreMode, sealed_mode};
use rdy_store::hub_canon::VIEW_REF;
use rdy_store::kinds;
use rdy_store::layout::STORE_DIR;
use rdy_store::object_refs::ObjectKey;
use rdy_store::{StoreError, StoreReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A restore's request: the dump, then its increments in chain order, and the remote (none
/// for the default one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreRequest {
    pub dump: Vec<PathBuf>,
    pub remote: Option<String>,
}

/// The outcome lookup of a write whose reply was lost: its push id, its view (none for canon)
/// and the hub's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreLookup {
    pub push: String,
    pub view: Option<String>,
    pub outcome: LookupOutcome,
}

/// How a restore ended without an error: the remote; whether the dump was a hub dump (a
/// holder); canon's end (None when the dump holds no canon); the lookups of lost replies; each
/// tag sent with no render inputs; each view's end in the order sent; each hub view that moved
/// past the dump's and was not sent; and the failure to remove the temporary store, when there
/// was one.
#[derive(Clone, Debug)]
pub struct RestoreOutcome {
    pub remote: String,
    pub holder: bool,
    pub canon: Option<WriteEnd>,
    pub lookups: Vec<RestoreLookup>,
    pub without_inputs: Vec<String>,
    pub views: Vec<(String, WriteEnd)>,
    pub kept: Vec<String>,
    pub cleanup: Option<RdyError>,
}

fn store(e: StoreError) -> RdyError {
    e.into()
}

/// The loaded dump: its temporary directory and the store open in it. Dropping it closes the
/// store and removes the directory; [`Loaded::close`] does so and reports a failure.
struct Loaded {
    rdy: Option<Rdy>,
    dir: Option<tempfile::TempDir>,
    holder: bool,
    /// Whether the dump's `.rdy.yaml` names a title (`name`).
    titled: bool,
}

impl Loaded {
    /// Step 1: a new temporary directory under `root` (the system temp root when None), the
    /// load of `dump` into it, then the open of the project or holder it holds.
    fn load(root: Option<&Path>, dump: &[PathBuf]) -> Result<Loaded, RdyError> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("rdy-restore-");
        let dir = match root {
            Some(r) => builder.tempdir_in(r),
            None => builder.tempdir(),
        }
        .map_err(io)?;
        let mut loaded = Loaded {
            rdy: None,
            dir: Some(dir),
            holder: false,
            titled: false,
        };
        match loaded.open(dump) {
            Ok(()) => Ok(loaded),
            Err(e) => Err(with_cleanup(e, loaded.close())),
        }
    }

    fn open(&mut self, dump: &[PathBuf]) -> Result<(), RdyError> {
        let root = self.dir.as_ref().map(|d| d.path().to_path_buf());
        let root = root.expect("a restore's directory");
        crate::load::load_chain(dump, &root, None)?;
        let mode = sealed_mode(&root.join(".rdy").join(STORE_DIR)).map_err(store)?;
        let r = match mode {
            StoreMode::Holder => Rdy::open_holder(&root)?,
            StoreMode::Project => Rdy::open(&root)?,
        };
        self.holder = mode == StoreMode::Holder;
        self.titled = !self.holder
            && r.config
                .name
                .as_deref()
                .is_some_and(|n| !n.trim().is_empty());
        self.rdy = Some(r);
        Ok(())
    }

    fn read(&self) -> Arc<ReadState> {
        self.rdy.as_ref().expect("a loaded store").read()
    }

    /// Step 7: closes the store, then removes the directory.
    fn close(mut self) -> Result<(), RdyError> {
        let closed = self.rdy.take().map(Rdy::close).transpose();
        let removed = self.dir.take().map(|d| d.close()).transpose().map_err(io);
        closed?;
        removed?;
        Ok(())
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        // the store closes before its directory goes; the directory's own drop removes it
        drop(self.rdy.take());
        drop(self.dir.take());
    }
}

/// `e` with the cleanup's failure, when there was one, as a warning beside it.
fn with_cleanup(mut e: RdyError, cleanup: Result<(), RdyError>) -> RdyError {
    if let Err(c) = cleanup {
        e.issues.push(Issue {
            severity: Severity::Warning,
            code: c.code,
            block: None,
            message: c.message,
            fix: c.fix,
        });
    }
    e
}

/// Restores `request` through `service` for `session` (see the module docs).
pub(crate) fn restore(
    service: &SyncService<'_>,
    session: &Session,
    request: &RestoreRequest,
) -> Result<RestoreOutcome, RdyError> {
    let loaded = Loaded::load(service.scratch.as_deref(), &request.dump)?;
    let result = service.with_target(session, request.remote.as_deref(), |t| {
        Exchange::run(service, t, &loaded)
    });
    let cleanup = loaded.close();
    match result {
        Ok(mut out) => {
            out.cleanup = cleanup.err();
            Ok(out)
        }
        Err(e) => Err(with_cleanup(e, cleanup)),
    }
}

/// The preview of the restore of `request` (`SyncService::restore_preview`): no write is sent
/// and nothing is written but the temporary store.
pub(crate) fn preview(
    service: &SyncService<'_>,
    session: &Session,
    request: &RestoreRequest,
) -> Result<CanonPreview, RdyError> {
    let loaded = Loaded::load(service.scratch.as_deref(), &request.dump)?;
    let result = service.with_target(session, request.remote.as_deref(), |t| {
        let read = loaded.read();
        let (_, state) = service.open(t, &read, &[])?;
        let head = read.reader().canon_head().map_err(store)?;
        Ok(preview_move(read.reader(), &t.name, state.canon, head))
    });
    let cleanup = loaded.close();
    match result {
        // a preview has no outcome to carry the cleanup's failure: it is the result
        Ok(p) => cleanup.map(|()| p),
        Err(e) => Err(with_cleanup(e, cleanup)),
    }
}

/// What a project dump's restore captured once: its render inputs when its `.rdy.yaml` names a
/// title, the gate settings, and the tip whose scope they hold.
struct Inputs {
    render: Option<RenderInputs>,
    gate: GateSettings,
    head: Option<Hash>,
}

/// One upload and its write.
struct Job {
    kind: WriteKind,
    view: Option<String>,
    head: Hash,
    base: Option<Hash>,
    expected: Option<Hash>,
    history: PreparedHistory,
}

/// The exchange of one restore on the selected remote, over the loaded store.
struct Exchange<'a, 's> {
    service: &'a SyncService<'s>,
    t: &'a Target,
    base_url: String,
    read: Arc<ReadState>,
    state: SyncState,
    inputs: Option<Inputs>,
}

/// The names of the views a hub store holds (`view/<name>`), sorted.
fn hub_view_names(reader: &StoreReader) -> Result<Vec<String>, RdyError> {
    let mut names: Vec<String> = reader
        .refs_with_prefix(VIEW_REF)
        .map_err(store)?
        .into_iter()
        .filter_map(|(name, _)| name.strip_prefix(VIEW_REF).map(str::to_string))
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// Whether the view op chain from `head` back holds `target`.
fn op_chain_holds(reader: &StoreReader, head: Hash, target: &Hash) -> Result<bool, RdyError> {
    let mut at = Some(head);
    while let Some(h) = at {
        if h == *target {
            return Ok(true);
        }
        at = reader.view_op(&h).map_err(store)?.prev;
    }
    Ok(false)
}

/// `E_UPSTREAM_UNCERTAIN` in publish's words for the write of push `push`, whose outcome the
/// lookup did not give: an issue names the push id, and `cause` (the lookup's own failure)
/// rides beside it.
fn uncertain(push: &str, cause: Option<RdyError>) -> RdyError {
    let mut e = upstream_uncertain();
    e.issues.push(Issue {
        severity: Severity::Error,
        code: E_UPSTREAM_UNCERTAIN,
        block: None,
        message: format!("the restore's write of push {push} has no known outcome"),
        fix: UPSTREAM_UNCERTAIN_FIX.to_string(),
    });
    if let Some(c) = cause {
        e.issues.push(Issue {
            severity: Severity::Warning,
            code: c.code,
            block: None,
            message: c.message,
            fix: c.fix,
        });
    }
    e
}

impl<'a, 's> Exchange<'a, 's> {
    /// Steps 2 to 6 on `t` for the loaded dump.
    fn run(
        service: &'a SyncService<'s>,
        t: &'a Target,
        loaded: &Loaded,
    ) -> Result<RestoreOutcome, RdyError> {
        let read = loaded.read();
        let head = read.reader().canon_head().map_err(store)?;
        // a project dump's views stay on the machine
        let views = if loaded.holder {
            hub_view_names(read.reader())?
        } else {
            Vec::new()
        };
        check_view_names(&views)?;
        let (base_url, state) = service.open(t, &read, &views)?;
        let inputs = if loaded.holder {
            None
        } else {
            Some(capture(service, &read, head, loaded.titled)?)
        };
        let x = Exchange {
            service,
            t,
            base_url,
            read,
            state,
            inputs,
        };
        let mut out = RestoreOutcome {
            remote: t.name.clone(),
            holder: loaded.holder,
            canon: None,
            lookups: Vec::new(),
            without_inputs: Vec::new(),
            views: Vec::new(),
            kept: Vec::new(),
            cleanup: None,
        };
        if let Some(head) = head {
            let tip = x.state.canon;
            let base = match tip {
                Some(tip)
                    if x.read
                        .reader()
                        .has_kind(kinds::LANDING, &tip)
                        .map_err(store)? =>
                {
                    Some(tip)
                }
                _ => None,
            };
            let history = PreparedHistory::canon(Arc::clone(&x.read), head, base)?;
            let job = Job {
                kind: WriteKind::Restore,
                view: None,
                head,
                base,
                expected: tip,
                history,
            };
            out.canon = Some(x.send(job, &mut out)?);
        }
        for name in views {
            let reader = x.read.reader();
            let Some(mine) = reader.view_head(&name).map_err(store)? else {
                continue;
            };
            let hub = x.state.view_head(&name);
            if let Some(h) = hub
                && !op_chain_holds(reader, mine.head, &h)?
            {
                // the hub's view moved past the dump's: it stays as it is
                out.kept.push(name);
                continue;
            }
            let history = PreparedHistory::view(Arc::clone(&x.read), mine.head, hub)?;
            let job = Job {
                kind: WriteKind::View,
                view: Some(name.clone()),
                head: mine.head,
                base: hub,
                expected: hub,
                history,
            };
            let end = x.send(job, &mut out)?;
            out.views.push((name, end));
        }
        Ok(out)
    }

    fn context(&self) -> Result<UploadContext, RdyError> {
        self.service
            .context(self.t, &self.read, Some(&self.base_url))
    }

    /// The tags of `pool` (the keys known named missing), each with the disagreements at its
    /// scope when render inputs go (none when they do not), by name.
    fn tags(
        &self,
        pool: &[ObjectKey],
        render: Option<&RenderInputs>,
    ) -> Result<Vec<WriteTag>, RdyError> {
        let mut out = Vec::new();
        for k in pool.iter().filter(|k| k.kind == kinds::TAG) {
            let tag = self.read.reader().read_tag(&k.hash).map_err(store)?;
            let at_head = Some(tag.landing) == self.inputs.as_ref().and_then(|i| i.head);
            let disagreements = match render {
                Some(r) if at_head => r.disagreements.clone(),
                Some(_) => {
                    self.service
                        .renderer
                        .capture(&self.read, &Scope::At(tag.landing))?
                        .disagreements
                }
                None => Vec::new(),
            };
            out.push(WriteTag {
                name: tag.name,
                disagreements,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// One upload and its write (steps 3 to 6).
    fn send(&self, job: Job, out: &mut RestoreOutcome) -> Result<WriteEnd, RdyError> {
        let keys = job.history.keys();
        let push = PushId::new();
        let id = push.as_str().to_string();
        let wire = SyncObjects(self.service.wire);
        let mut context = || self.context();
        let missing = missing_with(&keys, &push, &wire, &mut context)?;
        let limits = BatchLimits {
            bytes: self.state.batch_bytes,
            records: self.state.batch_records,
        };
        let mut sent = HistorySent::default();
        send_objects(
            &job.history,
            &missing,
            &push,
            limits,
            &wire,
            &mut context,
            &mut || Ok(()),
            &mut sent,
        )?;
        if job.expected != Some(job.head) && sent.batches == 0 {
            // the hub holds every record, yet the write moves canon or the view: a batch of no
            // record names the head the write moves to
            send_root(&job.history, &push, limits, &wire, &mut context, &mut sent)?;
        }
        if job.expected == Some(job.head) && sent.batches == 0 {
            // the hub tip or head is the dump's, and the hub holds every attachment
            return Ok(WriteEnd::UpToDate {
                tip: Some(job.head),
            });
        }
        let canon = job.kind == WriteKind::Restore;
        let render = if canon {
            self.inputs.as_ref().and_then(|i| i.render.clone())
        } else {
            None
        };
        let tags = self.tags(&missing, render.as_ref())?;
        let request = WriteRequest {
            expected: job.expected.map(|hash| ObjectKey {
                kind: if canon {
                    kinds::LANDING
                } else {
                    kinds::VIEW_OP
                },
                hash,
            }),
            gate: if canon {
                self.inputs.as_ref().map(|i| i.gate.clone())
            } else {
                None
            },
            kind: job.kind,
            push: id.clone(),
            render_inputs: render,
            tags,
            view: job.view.clone(),
        };
        encode_write(&request)?;
        if request.render_inputs.is_none() {
            out.without_inputs
                .extend(request.tags.iter().map(|t| t.name.clone()));
        }
        let cx = self.context()?;
        let reply = match self.service.wire.write(&cx, &request) {
            Ok(r) => r,
            Err(WriteFailure::Refused(e)) => {
                return Ok(WriteEnd::Refused {
                    push: Some(id),
                    error: write_refusal(*e),
                });
            }
            Err(WriteFailure::NotSent(e)) => return Err(*e),
            Err(WriteFailure::Uncertain) => {
                drop(cx);
                return self.look_up(&job, &request, sent, out);
            }
        };
        drop(cx);
        match (reply.outcome, reply.tip, reply.request) {
            (WriteOutcome::Taken, Some(tip), _) => Ok(WriteEnd::Taken {
                push: id,
                base: job.base,
                head: job.head,
                tip: tip.hash,
                resumed: false,
                sent,
                min_rdy_raised: reply.min_rdy_raised,
            }),
            (WriteOutcome::Held, _, Some(request)) => Ok(WriteEnd::Held {
                push: id,
                head: job.head,
                request,
                resumed: false,
                sent,
                min_rdy_raised: reply.min_rdy_raised,
            }),
            // a reply the wire should not have passed: its outcome is not known
            _ => self.look_up(&job, &request, sent, out),
        }
    }

    /// Step 6: the one outcome lookup of the write `request`, whose reply was lost.
    fn look_up(
        &self,
        job: &Job,
        request: &WriteRequest,
        sent: HistorySent,
        out: &mut RestoreOutcome,
    ) -> Result<WriteEnd, RdyError> {
        let id = request.push.clone();
        let reply = self
            .context()
            .and_then(|cx| self.service.wire.outcome(&cx, &id))
            .map_err(|e| uncertain(&id, Some(e)))?;
        let end = match &reply.outcome {
            LookupOutcome::Taken { tip } if tip.kind == request.root_kind() => WriteEnd::Taken {
                push: id.clone(),
                base: job.base,
                head: job.head,
                tip: tip.hash,
                resumed: false,
                sent,
                min_rdy_raised: false,
            },
            LookupOutcome::Held { request: held, .. } if job.kind != WriteKind::View => {
                WriteEnd::Held {
                    push: id.clone(),
                    head: job.head,
                    request: *held,
                    resumed: false,
                    sent,
                    min_rdy_raised: false,
                }
            }
            LookupOutcome::Refused { .. } => WriteEnd::Refused {
                push: Some(id.clone()),
                error: upstream_refused(),
            },
            LookupOutcome::Taken { .. } | LookupOutcome::Held { .. } => {
                return Err(upstream_protocol());
            }
            LookupOutcome::Unknown => return Err(uncertain(&id, None)),
        };
        out.lookups.push(RestoreLookup {
            push: id,
            view: job.view.clone(),
            outcome: reply.outcome,
        });
        Ok(end)
    }
}

/// Step 4 for a project dump: one capture by this client at the dump's tip, from its own
/// project files. Without a title, the render inputs stay home; the gate still goes.
fn capture(
    service: &SyncService<'_>,
    read: &ReadState,
    head: Option<Hash>,
    titled: bool,
) -> Result<Inputs, RdyError> {
    let scope = head.map_or(Scope::Canon, Scope::At);
    let inputs = service.renderer.capture(read, &scope)?;
    validate_inputs(&inputs)?;
    let gate = gate_settings(read, &inputs);
    Ok(Inputs {
        render: titled.then_some(inputs),
        gate,
        head,
    })
}
