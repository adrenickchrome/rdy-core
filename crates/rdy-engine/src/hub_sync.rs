//! The sync exchange's push through put (D-H0BZN, D-3RSCW, D-4XHPH, D-5Y8Y1, D-47HVF, D-0SC9C):
//! the steps that rdyhub's wire fixture v1 covers.
//!
//! 1. **Remote.** The named remote, else the share remote, else the only remote, else
//!    `E_REMOTE_REQUIRED`. A remote that is not a share remote is `E_BAD_INPUT`
//!    (`SYNC_REMOTE_MODE`).
//! 2. **Guard.** One push or pull per remote of a project runs in this process; a second is
//!    `E_LOCKED` and sends nothing ([`RemoteGuard`]).
//! 3. **Trust and access.** The install's origin fence, its `ca_file`, the session's bound
//!    claim and the machine's sign-in credential, as `rdy publish` uses them. Access is asked
//!    for immediately before every request.
//! 4. **Discovery, then state** in its publish-upload form, with no views.
//! 5. **Checks before any transfer.** The hub's `min_rdy` (`E_STORE_FORMAT`), the hub repo's id
//!    (`E_REPO_MISMATCH`) and the history stream format. A `pending` write found here stops the
//!    push with [`PushOutcome::PendingWrite`]; both refs stay. Then the base rule: a null hub tip
//!    seeds, a tip local canon holds is the base, any other tip is `E_DIVERGED`.
//! 6. **Upload.** One push id per upload: the `upload` ref's, when one names this remote's canon
//!    upload (a stopped put resumes), else a new one. Known names the keys neither the hub's
//!    store nor the push's quarantine holds; only those travel, in Plan A's format-4 batches.
//!    The `upload` ref is written before the first put.
//! 7. **End.** After the last put the upload stays for the write step. A push that fails
//!    clears only an `upload` it wrote itself, and only when no put of it may have reached the
//!    quarantine.
//!
//! Seams for the steps that wire fixture v2 freezes: the outcome lookup runs where a found
//! `pending` stops the push now; the write step (with the render inputs and the gate settings)
//! follows the last put, where [`PushOutcome::Uploaded`] is returned now.
use crate::engine::Engine;
use crate::error::{
    E_DIVERGED, E_STORE_FORMAT, E_UPSTREAM_ACTOR_CLAIM, E_UPSTREAM_UNCERTAIN, RdyError,
    SYNC_DIVERGED_FIX, SYNC_DIVERGED_MESSAGE, SYNC_MIN_RDY_FIX, SYNC_MIN_RDY_MESSAGE,
    upstream_auth, upstream_protocol,
};
use crate::formats::formats;
use crate::hub_config::{BAD_REMOTE_NAME_FIX, BAD_REMOTE_NAME_MESSAGE, HubConfig, RemoteMode};
use crate::hub_credentials::HubCredentials;
use crate::hub_publish::{not_share_remote, repo_mismatch};
use crate::hub_publish_stream::{
    BatchLimits, HistorySent, ObjectWire, PreparedHistory, PushId, missing_with, send_objects,
};
use crate::hub_publish_wire::{ObjectPut, PublishState, UploadContext};
use crate::hub_sync_wire::{SyncStateRequest, SyncWire};
use crate::hub_types::{HubClock, HubTransport, Origin, RequestClaim};
use crate::session::Session;
use crate::sync_state::{RemoteGuard, remote_refs, write_steps};
use crate::{ReadState, hub_config};
use rdy_model::Hash;
use rdy_schema::Issue;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{KIND_CANON, SyncStep, SyncUpload};
use rdy_store::sync_stream::HISTORY_FORMAT;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A push's request: the remote to push to, or none for the default one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushRequest {
    pub remote: Option<String>,
}

/// How a push ended without an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// The hub tip is local canon's head (or both are empty): nothing to upload.
    UpToDate { tip: Option<Hash> },
    /// Every record the hub lacked is in the quarantine of `push`, from local canon's `head`
    /// above the hub tip `base`. The `upload` ref stays for the write step.
    Uploaded {
        push: String,
        base: Option<Hash>,
        head: Hash,
        /// The push id came from an `upload` ref an earlier push left.
        resumed: bool,
        sent: HistorySent,
    },
    /// A write of push `push` was sent and has no reply: the push stopped before known or put,
    /// and both refs stay.
    PendingWrite { push: String },
}

/// E_STORE_FORMAT when the hub repo `name` needs a newer rdy than this one (D-4XHPH).
pub fn check_minimum(state: &PublishState, name: &str) -> Result<(), RdyError> {
    let Some(min) = state.min_rdy.as_deref() else {
        return Ok(());
    };
    let (Ok(want), Ok(mine)) = (
        semver::Version::parse(min),
        semver::Version::parse(&formats().rdy),
    ) else {
        return Err(upstream_protocol());
    };
    if want <= mine {
        return Ok(());
    }
    Err(RdyError::new(
        E_STORE_FORMAT,
        SYNC_MIN_RDY_MESSAGE
            .replace("<name>", name)
            .replace("<version>", min),
        SYNC_MIN_RDY_FIX.replace("<version>", min),
    ))
}

/// E_DIVERGED: local canon does not hold the hub tip `tip`.
fn diverged(tip: &Hash) -> RdyError {
    RdyError::new(
        E_DIVERGED,
        SYNC_DIVERGED_MESSAGE.replace("<tip>", &tip.to_string()),
        SYNC_DIVERGED_FIX,
    )
}

/// The hub repo's name in the words of D-4XHPH: the remote URL's path without its slashes.
fn repo_name(url: &str) -> String {
    url::Url::parse(url)
        .map(|u| u.path().trim_matches('/').to_string())
        .unwrap_or_default()
}

/// Whether local canon, from `head` back, holds the landing `tip`.
fn canon_holds(read: &ReadState, head: Option<Hash>, tip: &Hash) -> Result<bool, RdyError> {
    let reader = read.reader();
    let mut at = head;
    while let Some(h) = at {
        if h == *tip {
            return Ok(true);
        }
        at = reader.landing(&h)?.prev;
    }
    Ok(false)
}

/// The selected remote's install and trust.
struct Target {
    name: String,
    url: String,
    install: Origin,
    ca_file: Option<PathBuf>,
    claim: RequestClaim,
    bound: RequestClaim,
    account: String,
}

/// The sync coordinator of one project for the user config at `user_file`, with the credential
/// transport `api`, the clock `clock` and the sync wire `wire`.
pub struct SyncService<'a> {
    project: PathBuf,
    user_file: PathBuf,
    api: &'a dyn HubTransport,
    clock: &'a dyn HubClock,
    wire: &'a dyn SyncWire,
}

/// The sync wire's known and put steps for the history sender.
struct SyncObjects<'a>(&'a dyn SyncWire);

impl ObjectWire for SyncObjects<'_> {
    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.0.known(cx, push, keys)
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.0.put(cx, push, body, len)
    }
}

impl<'a> SyncService<'a> {
    /// The coordinator of the project at `project` with the user config `user_file`
    /// (`<home>/rdy/config.yaml`), whose sibling `credentials.yaml` holds the sign-ins.
    pub fn new(
        project: &Path,
        user_file: &Path,
        api: &'a dyn HubTransport,
        clock: &'a dyn HubClock,
        wire: &'a dyn SyncWire,
    ) -> SyncService<'a> {
        SyncService {
            project: project.to_path_buf(),
            user_file: user_file.to_path_buf(),
            api,
            clock,
            wire,
        }
    }

    fn credentials(&self) -> HubCredentials<'a> {
        HubCredentials::new(Some(&self.project), &self.user_file, self.api, self.clock)
    }

    /// The context of one request on `t`: current access to its install (renewed when it
    /// expires), checked to hold the run's account and bound claim, then the state's
    /// redaction epoch.
    fn context(
        &self,
        t: &Target,
        read: &ReadState,
        sync_base: Option<&str>,
    ) -> Result<UploadContext, RdyError> {
        let grant = self.credentials().access(&t.install, &t.claim)?;
        if grant.account != t.account || grant.claim != t.bound {
            return Err(upstream_auth(t.install.as_str()));
        }
        read.ensure_current_epoch()?;
        Ok(UploadContext {
            install: t.install.clone(),
            sync_base: sync_base.map(str::to_string),
            ca_file: t.ca_file.clone(),
            claim: grant.claim,
            bearer: grant.access_token,
        })
    }

    /// `e` with the settings' warnings, and with the account handle refreshed once when the hub
    /// refused the actor claim (as `rdy publish` does).
    fn fail(&self, t: Option<&Target>, warnings: &[Issue], mut e: RdyError) -> RdyError {
        if e.code == E_UPSTREAM_ACTOR_CLAIM
            && let Some(t) = t
        {
            match self.credentials().refresh_handle(&t.install, &t.claim) {
                Ok(update) => e.issues.extend(update.warnings),
                Err(f) => e.issues.push(Issue {
                    severity: rdy_schema::Severity::Error,
                    code: f.code,
                    block: None,
                    message: f.message,
                    fix: f.fix,
                }),
            }
        }
        e.issues.extend(warnings.iter().cloned());
        e
    }

    /// Pushes local canon to the share remote `request.remote` (see the module docs).
    pub fn push(
        &self,
        engine: &dyn Engine,
        session: &Session,
        request: PushRequest,
    ) -> Result<PushOutcome, RdyError> {
        // 1. the remote
        if let Some(name) = &request.remote
            && !hub_config::is_remote_name(name)
        {
            return Err(RdyError::new(
                crate::error::E_BAD_INPUT,
                BAD_REMOTE_NAME_MESSAGE,
                BAD_REMOTE_NAME_FIX,
            ));
        }
        let settings = HubConfig::load(Some(&self.project), Some(&self.user_file))?;
        let warnings = settings.warnings.clone();
        let remote = settings
            .select_remote(request.remote.as_deref())
            .map_err(|e| self.fail(None, &warnings, e))?;
        if remote.mode != RemoteMode::Share {
            return Err(self.fail(None, &warnings, not_share_remote(&remote.name)));
        }

        // 2. one push or pull per remote
        let _guard = RemoteGuard::take(&self.project, &remote.name)?;

        // 3. the install, its CA file and pins; the session's claim; access
        let install = remote.url.origin().clone();
        settings
            .fence(&install)
            .map_err(|e| self.fail(None, &warnings, e))?;
        let ca_file = settings.install(&install).and_then(|i| i.ca_file.clone());
        let claim =
            RequestClaim::from_session(session).map_err(|e| self.fail(None, &warnings, e))?;
        let first = self
            .credentials()
            .access_on_use(&install, &claim)
            .map_err(|e| self.fail(None, &warnings, e))?;
        let target = Target {
            name: remote.name.clone(),
            url: remote.url.as_str().to_string(),
            install,
            ca_file,
            claim,
            bound: first.claim,
            account: first.account,
        };
        self.exchange(engine, &target)
            .map_err(|e| self.fail(Some(&target), &warnings, e))
    }

    /// Steps 4 to 7 on the selected `t`.
    fn exchange(&self, engine: &dyn Engine, t: &Target) -> Result<PushOutcome, RdyError> {
        let read = engine.read();
        // 4. discovery, then state
        let cx = self.context(t, &read, None)?;
        let base_url = self.wire.discovery(&cx, &t.url)?.sync_base;
        let cx = self.context(t, &read, Some(&base_url))?;
        // Seam (8a2): a push with views names them in this request.
        let state = self.wire.state(&cx, &SyncStateRequest::new(&cx.claim))?;
        drop(cx);

        // 5. the minimum, the repo, the stream format, a pending write, the base rule
        check_minimum(&state, &repo_name(&t.url))?;
        let repo_id = read.repo_id()?;
        if state.repo_id.is_some_and(|r| r != repo_id) {
            return Err(repo_mismatch());
        }
        if !state.stream_read.contains(&HISTORY_FORMAT)
            || !state.stream_write.contains(&HISTORY_FORMAT)
        {
            return Err(upstream_protocol());
        }
        let refs = remote_refs(engine, &t.name)?;
        if let Some(p) = refs.pending {
            // Seam (8a2): the outcome lookup runs here, before any put or write.
            return Ok(PushOutcome::PendingWrite { push: p.push });
        }
        let head = read.canon_head();
        let base = state.canon;
        if let Some(tip) = &base
            && !canon_holds(&read, head, tip)?
        {
            return Err(diverged(tip));
        }
        let Some(head) = head.filter(|h| Some(*h) != base) else {
            return Ok(PushOutcome::UpToDate { tip: base });
        };

        // 6. the upload: a stopped one of this remote's canon resumes under its push id
        let resumed = refs.upload.as_ref().and_then(|u| {
            let same =
                u.kind == KIND_CANON && u.view.is_none() && u.url == t.url && u.repo_id == repo_id;
            if same { PushId::parse(&u.push) } else { None }
        });
        if refs.upload.is_some() && resumed.is_none() {
            // another upload of this remote waits for its write: never replaced
            return Err(RdyError::from(rdy_store::StoreError::SyncBusy(
                t.name.clone(),
            )));
        }
        let was_resumed = resumed.is_some();
        let push = resumed.unwrap_or_else(PushId::new);
        let history = PreparedHistory::canon(Arc::clone(&read), head, base)?;
        let wire = SyncObjects(self.wire);
        let mut context = || self.context(t, &read, Some(&base_url));
        let missing = missing_with(&history.keys(), &push, &wire, &mut context)?;
        let limits = BatchLimits {
            bytes: state.batch_bytes,
            records: state.batch_records,
        };
        let mut wrote = false;
        let mut sent = HistorySent::default();
        let mut before_put = || {
            if !was_resumed {
                write_steps(
                    engine,
                    &t.name,
                    vec![SyncStep::Upload(SyncUpload {
                        repo_id,
                        url: t.url.clone(),
                        push: push.as_str().to_string(),
                        kind: KIND_CANON,
                        view: None,
                        at_ms: self.clock.now_ms(),
                    })],
                )?;
                wrote = true;
            }
            Ok(())
        };
        let result = send_objects(
            &history,
            &missing,
            &push,
            limits,
            &wire,
            &mut context,
            &mut before_put,
            &mut sent,
        );
        if let Err(e) = result {
            // 7. a failed push clears only its own upload, and only when no put of it may have
            // reached the quarantine
            if wrote && sent.batches == 0 && e.code != E_UPSTREAM_UNCERTAIN {
                let _ = write_steps(
                    engine,
                    &t.name,
                    vec![SyncStep::ClearUpload {
                        push: push.as_str().to_string(),
                    }],
                );
            }
            return Err(e);
        }
        // Seam (8a2): the write step follows here, with the render inputs and, for canon, the
        // gate settings; it sends the write under this push id.
        Ok(PushOutcome::Uploaded {
            push: push.as_str().to_string(),
            base,
            head,
            resumed: was_resumed,
            sent,
        })
    }
}
