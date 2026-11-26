//! The sync exchange tests' shared harness (scc8): a private home with a user config and a
//! sign-in file, a project with landings and remotes, an injected clock and a credential
//! transport that refuses every request. Tokens are made here; no real secret is used.
#![allow(dead_code)]

use rdy_engine::hub_config::{Credential, CredentialFile, ReadyCredential, encode_credentials};
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use rdy_engine::hub_types::{
    HubCall, HubClock, HubFailure, HubOperation, HubReply, HubTransport, Origin, Secret,
};
use rdy_engine::*;
use rdy_model::{Hash, Role};
use rdy_store::pio;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

pub const ACCOUNT: &str = "acc_00000000000000000000000001";
pub const HANDLE: &str = "test-person";
pub const T0: i64 = 1_800_000_000_000;
pub const HOUR_MS: i64 = 3_600_000;
pub const REPO_PATH: &str = "/acme/docs";
const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A hub token of kind `kind` whose secret starts with `tag`, padded with `0`, and its checksum.
pub fn token(kind: char, tag: &str) -> String {
    let mut secret: String = tag.chars().filter(char::is_ascii_alphanumeric).collect();
    while secret.len() < 40 {
        secret.push('0');
    }
    secret.truncate(40);
    let body = format!("rdyh_{kind}_{secret}");
    let mut crc = u64::from(crc32fast::hash(body.as_bytes()));
    let mut sum = [b'0'; 6];
    for slot in sum.iter_mut().rev() {
        *slot = ALPHABET[(crc % 62) as usize];
        crc /= 62;
    }
    format!("{body}{}", std::str::from_utf8(&sum).unwrap())
}

pub fn access() -> String {
    token('a', "SYNCACCESS")
}

pub fn refresh() -> String {
    token('r', "SYNCREFRESH")
}

pub fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

pub fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

/// The pushing session: an agent run, whose claim the hub binds.
pub fn agent() -> Session {
    Session::agent("agent:pusher".into(), Some("run-7".into()), Role::Writer).unwrap()
}

/// Writes one doc named `name` in view `work`, then proposes and lands it. Returns the landing.
pub fn land_doc(r: &mut Rdy, name: &str) -> Hash {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}
            ]))
            .unwrap(),
            justification: None,
            message: "scc8".into(),
        },
    )
    .unwrap();
    let p = r
        .propose(
            &writer(),
            "work",
            "batch",
            Some(crate::common::TEST_SUMMARY),
        )
        .unwrap();
    crate::common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

pub fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    pio::make_private_dir(dir.path()).unwrap();
    dir
}

/// A ready sign-in to `install` that lasts an hour from T0.
pub fn ready(install: &str) -> (String, Credential) {
    (
        install.to_string(),
        Credential::Ready(ReadyCredential {
            access_token: Secret::new(access()),
            refresh_token: Secret::new(refresh()),
            expires_at_ms: T0 + HOUR_MS,
            account: ACCOUNT.to_string(),
            handle: HANDLE.to_string(),
        }),
    )
}

/// Writes the credential file (0600) holding exactly `records`.
pub fn seed(creds: &Path, records: &[(String, Credential)]) {
    let mut file = CredentialFile::default();
    for (o, c) in records {
        file.set(Origin::parse(o).unwrap(), Some(c.clone()));
    }
    let bytes = encode_credentials(&file).unwrap();
    let tmp = creds.with_extension("seed");
    let mut f = fs::File::create(&tmp).unwrap();
    pio::make_private_file(&f).unwrap();
    f.write_all(&bytes).unwrap();
    drop(f);
    fs::rename(&tmp, creds).unwrap();
}

/// A project with landings, its remotes, the user config and a sign-in to `install`. The
/// engine is the in-process one.
pub struct Env {
    pub _home: tempfile::TempDir,
    pub _project: tempfile::TempDir,
    pub project: PathBuf,
    pub user_file: PathBuf,
    pub creds: PathBuf,
    pub engine: Local,
    /// The landings, oldest first.
    pub landings: Vec<Hash>,
    pub url: String,
}

/// `landings` landings, the user config `config`, and on `install` the share remote `share`
/// (when given) and the publish remote `hub`, both at `<install>/acme/docs`.
pub fn env_with(install: &str, config: &str, share: Option<&str>, landings: usize) -> Env {
    let home_dir = private_dir();
    let project_dir = private_dir();
    let rdy = home_dir.path().join("rdy");
    pio::create_private_dir(&rdy).unwrap();
    let user_file = rdy.join("config.yaml");
    fs::write(&user_file, config).unwrap();
    #[cfg(unix)]
    fs::set_permissions(&user_file, pio::permissions(0o644)).unwrap();
    let project = project_dir.path().to_path_buf();
    let mut r = Rdy::init(&project, "rdy-standard").unwrap();
    let mut made = Vec::new();
    for i in 0..landings {
        made.push(land_doc(&mut r, &format!("d{i}")));
    }
    let url = format!("{install}{REPO_PATH}");
    let remotes = RemoteService::new(&project, &user_file);
    remotes
        .add(
            "hub",
            &url,
            rdy_engine::hub_config::RemoteMode::Publish,
            RemoteLevel::Project,
        )
        .unwrap();
    if let Some(name) = share {
        remotes
            .add(
                name,
                &url,
                rdy_engine::hub_config::RemoteMode::Share,
                RemoteLevel::Project,
            )
            .unwrap();
    }
    let creds = rdy.join("credentials.yaml");
    seed(&creds, &[ready(install)]);
    Env {
        _home: home_dir,
        _project: project_dir,
        project,
        user_file,
        creds,
        engine: Local::new(r),
        landings: made,
        url,
    }
}

/// A credential transport that refuses every request: a ready sign-in needs none.
#[derive(Default)]
pub struct NoApi {
    pub seen: Mutex<Vec<String>>,
}

impl HubTransport for NoApi {
    fn exchange(&self, call: HubCall) -> Result<HubReply, HubFailure> {
        let op = match call.operation {
            HubOperation::Metadata => "Metadata",
            HubOperation::Refresh { .. } => "Refresh",
            HubOperation::Me => "Me",
            _ => "Other",
        };
        self.seen.lock().unwrap().push(op.to_string());
        panic!("an unexpected {op} request");
    }
}

pub struct Clock(pub AtomicI64);

impl Clock {
    pub fn at(ms: i64) -> Clock {
        Clock(AtomicI64::new(ms))
    }
}

impl HubClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------------------------
// The fake hub and the fake renderer (8a2)
// ---------------------------------------------------------------------------------------------

use rdy_engine::hub_publish::{PublishRenderer, RenderedUpload};
use rdy_engine::hub_publish_wire::{Discovery, ObjectPut, PublishState, UploadContext};
use rdy_engine::hub_sync::{
    OutcomePollClock, PullOutcome, PullRequest, PushOutcome, PushRequest, SyncService,
};
use rdy_engine::hub_sync_wire::{
    DumpRequest, GetRequest, HubRequest, KnownPage, KnownPull, LookupOutcome, OutcomeReply,
    SyncState, SyncStateRequest, SyncWire, WriteFailure, WriteKind, WriteOutcome, WriteReply,
    WriteRequest as SyncWrite,
};
use rdy_engine::render_inputs::{DisagreementInput, RenderInputs};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::StreamPurpose;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

type WaitProbe = Box<dyn Fn(Duration) + Send + std::marker::Sync>;

/// The fake monotonic clock of a busy outcome lookup's waits (r59016). It moves only when a test
/// advances it or a wait runs, and a wait never sleeps: it records the delay, runs the probe
/// (which a test uses to look at the hub while the lookup waits), then moves the clock on.
#[derive(Default)]
pub struct PollClock {
    now: Mutex<Duration>,
    /// Each wait's delay, in order.
    pub delays: Mutex<Vec<Duration>>,
    probe: Mutex<Option<WaitProbe>>,
}

impl PollClock {
    pub fn advance(&self, by: Duration) {
        *self.now.lock().unwrap() += by;
    }

    pub fn elapsed(&self) -> Duration {
        *self.now.lock().unwrap()
    }

    /// Runs `f` with the delay at the start of every wait.
    pub fn on_wait(&self, f: impl Fn(Duration) + Send + std::marker::Sync + 'static) {
        *self.probe.lock().unwrap() = Some(Box::new(f));
    }

    /// The delays waited, in whole seconds.
    pub fn seconds(&self) -> Vec<u64> {
        self.delays
            .lock()
            .unwrap()
            .iter()
            .map(Duration::as_secs)
            .collect()
    }
}

impl OutcomePollClock for PollClock {
    fn now(&self) -> Duration {
        self.elapsed()
    }

    fn wait(&self, delay: Duration) {
        self.delays.lock().unwrap().push(delay);
        if let Some(f) = self.probe.lock().unwrap().as_ref() {
            f(delay);
        }
        self.advance(delay);
    }
}

/// A hub refusal of the outcome lookup while the hub still runs the write: `E_HUB_BUSY` with the
/// `retry_after` its error object validated to `seconds`.
pub fn busy_refusal(seconds: u64) -> RdyError {
    let mut e = RdyError::new("E_HUB_BUSY", "the hub is busy", "wait before trying again");
    e.upstream_retry_after = Some(seconds);
    e
}

/// One wire request as the fake hub saw it.
#[derive(Clone, Debug)]
pub struct Call {
    pub step: &'static str,
    pub push: Option<String>,
    pub bearer: String,
    pub ca_file: Option<PathBuf>,
    pub sync_base: Option<String>,
    pub remote: Option<String>,
    /// Whether the context let a put body go as zstd.
    pub zstd: bool,
}

pub type Hook = Box<dyn FnMut(&'static str) + Send>;

/// How the fake hub answers one write.
#[derive(Debug)]
pub enum WritePlan {
    /// Takes the push's quarantine and moves canon or the view to the push's head.
    Taken { min_rdy_raised: bool },
    /// Holds the write as the landing request `number`, version `version`.
    Held { number: u64, version: u64 },
    /// Refuses it with the hub's error.
    Refuse(RdyError),
    /// The reply is lost; `apply` says whether the write took.
    Lost { apply: bool },
    /// The request never left.
    NotSent(RdyError),
}

/// A fake hub. Its store holds `held`; each push id has its own quarantine of the keys its
/// puts carried. `fail` fails the n-th request (from 1) of a step; `hook` runs once, at the
/// start of the first request of its step. `store` is the hub repo's history for the pull
/// steps.
pub struct Hub {
    pub state: Mutex<PublishState>,
    /// Each view the hub repo holds: its root and head.
    pub views: Mutex<BTreeMap<String, (Hash, Hash)>>,
    pub held: Mutex<BTreeSet<ObjectKey>>,
    pub quarantine: Mutex<BTreeMap<String, BTreeSet<ObjectKey>>>,
    /// Each push id's quarantine bytes so far.
    pub bytes: Mutex<BTreeMap<String, u64>>,
    pub fail: Mutex<BTreeMap<(&'static str, usize), RdyError>>,
    pub hook: Mutex<Option<(&'static str, Hook)>>,
    /// Runs at the start of every request of its step.
    pub each: Mutex<Option<(&'static str, Hook)>>,
    pub calls: Mutex<Vec<Call>>,
    /// Each put's push id and the keys its batch carried, in order.
    pub puts: Mutex<Vec<(String, Vec<ObjectKey>)>>,
    /// Each put's batch root and prerequisites, in order.
    pub roots: Mutex<Vec<(Hash, Vec<ObjectKey>)>>,
    /// Each push id's batch root.
    pub heads: Mutex<BTreeMap<String, Hash>>,
    pub states: Mutex<Vec<SyncStateRequest>>,
    pub writes: Mutex<Vec<SyncWrite>>,
    /// The answer of the n-th write (from 1); a write with none is taken.
    pub plan: Mutex<BTreeMap<usize, WritePlan>>,
    /// The outcome of each push; a push with none is unknown.
    pub outcomes: Mutex<BTreeMap<String, OutcomeReply>>,
    pub store: Mutex<Option<Arc<ReadState>>>,
    /// Keys the hub's export order puts first (they reached the hub during a pull).
    pub late: Mutex<BTreeSet<ObjectKey>>,
    /// The most keys a known pull page gives.
    pub page: Mutex<usize>,
    pub pulls: Mutex<Vec<KnownPull>>,
    pub gets: Mutex<Vec<GetRequest>>,
    /// Each get's reply, in order: the bytes the hub produced and the bytes the client read.
    pub get_replies: Mutex<Vec<(u64, u64)>>,
    /// When set, the next get of two or more keys has its reply padded one byte past the state's
    /// `batch_bytes` (the client must split it), and the knob clears.
    pub pad_get: Mutex<bool>,
    /// When set to `(cap, marker)`, a get whose export passes `cap` bytes is refused as the hub
    /// refuses a get over its batch cap: `E_HUB_VALIDATION`, with the structured over-cap marker
    /// on the error when `marker` is set.
    pub refuse_get_over: Mutex<Option<(u64, bool)>>,
    /// A get reply served once in place of the store's batch.
    pub get_bytes: Mutex<Option<Vec<u8>>>,
    /// The dump files the dump step serves, by id (D-7CDEQ); an id with none is refused as
    /// wire fixture v5 refuses it.
    pub dumps: Mutex<BTreeMap<String, Vec<u8>>>,
    /// The id of each dump step, in order.
    pub dump_ids: Mutex<Vec<String>>,
}

pub fn base_state() -> PublishState {
    PublishState {
        canon: None,
        repo_id: None,
        rdy: "0.53.0".into(),
        stream_read: vec![2, 4],
        stream_write: vec![2, 4],
        features: vec![],
        min_rdy: None,
        batch_bytes: 67_108_864,
        batch_records: 100_000,
        quarantine_seconds: 3_600,
        profile: None,
        audience: None,
        artifact_bytes: 201_326_592,
        artifact_files: 32_000,
        zstd: false,
    }
}

impl Hub {
    pub fn new() -> Hub {
        Hub {
            state: Mutex::new(base_state()),
            views: Mutex::default(),
            held: Mutex::default(),
            quarantine: Mutex::default(),
            bytes: Mutex::default(),
            fail: Mutex::default(),
            hook: Mutex::default(),
            each: Mutex::default(),
            calls: Mutex::default(),
            puts: Mutex::default(),
            roots: Mutex::default(),
            heads: Mutex::default(),
            states: Mutex::default(),
            writes: Mutex::default(),
            plan: Mutex::default(),
            outcomes: Mutex::default(),
            store: Mutex::default(),
            late: Mutex::default(),
            page: Mutex::new(usize::MAX),
            pulls: Mutex::default(),
            gets: Mutex::default(),
            get_replies: Mutex::default(),
            pad_get: Mutex::default(),
            refuse_get_over: Mutex::default(),
            get_bytes: Mutex::default(),
            dumps: Mutex::default(),
            dump_ids: Mutex::default(),
        }
    }

    pub fn with_state(self, f: impl FnOnce(&mut PublishState)) -> Hub {
        f(&mut self.state.lock().unwrap());
        self
    }

    pub fn fail_at(&self, step: &'static str, n: usize, e: RdyError) {
        self.fail.lock().unwrap().insert((step, n), e);
    }

    pub fn plan_write(&self, n: usize, p: WritePlan) {
        self.plan.lock().unwrap().insert(n, p);
    }

    pub fn answer(&self, push: &str, outcome: LookupOutcome) {
        self.outcomes.lock().unwrap().insert(
            push.to_string(),
            OutcomeReply {
                outcome,
                kind: None,
                view: None,
                request_state: None,
            },
        );
    }

    /// Answers the lookup of `push` as held, landing request `number` version 1, with the
    /// request's state now (`open`, `landed` or `closed`; rdyhub-design i-6YB7Y).
    pub fn answer_held(&self, push: &str, number: u64, request_state: &str) {
        self.outcomes.lock().unwrap().insert(
            push.to_string(),
            OutcomeReply {
                outcome: LookupOutcome::Held {
                    request: HubRequest { number, version: 1 },
                    tip: None,
                },
                kind: Some("landings".into()),
                view: None,
                request_state: Some(request_state.into()),
            },
        );
    }

    /// Serves the history of `read` to the pull steps.
    /// Serves pulls from `read`, a copy of the project: the hub repo is its repo.
    pub fn serve(&self, read: Arc<ReadState>) {
        self.state.lock().unwrap().repo_id = read.reader().repo_id().unwrap();
        *self.store.lock().unwrap() = Some(read);
    }

    pub fn steps(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().iter().map(|c| c.step).collect()
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.calls.lock().unwrap().clear();
        self.puts.lock().unwrap().clear();
        self.writes.lock().unwrap().clear();
    }

    pub fn sent_keys(&self) -> Vec<ObjectKey> {
        self.puts
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, k)| k.clone())
            .collect()
    }

    pub fn writes(&self) -> Vec<SyncWrite> {
        self.writes.lock().unwrap().clone()
    }

    /// Records the request, runs the hook of its step once, then fails it when asked.
    pub fn enter(
        &self,
        step: &'static str,
        cx: &UploadContext,
        push: Option<&str>,
        remote: Option<&str>,
    ) -> Result<(), RdyError> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(Call {
                step,
                push: push.map(str::to_string),
                bearer: cx.bearer.expose().to_string(),
                ca_file: cx.ca_file.clone(),
                sync_base: cx.sync_base.clone(),
                remote: remote.map(str::to_string),
                zstd: cx.zstd,
            });
            calls.iter().filter(|c| c.step == step).count()
        };
        let hook = {
            let mut h = self.hook.lock().unwrap();
            match h.as_ref() {
                Some((s, _)) if *s == step => h.take(),
                _ => None,
            }
        };
        if let Some((_, mut f)) = hook {
            f(step);
        }
        if let Some((s, f)) = self.each.lock().unwrap().as_mut()
            && *s == step
        {
            f(step);
        }
        match self.fail.lock().unwrap().remove(&(step, n)) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The write of `w` took: its quarantine moves to the store, and canon or the view moves.
    fn take(&self, w: &SyncWrite, tip: Hash) {
        let q = self
            .quarantine
            .lock()
            .unwrap()
            .remove(&w.push)
            .unwrap_or_default();
        self.held.lock().unwrap().extend(q);
        match &w.view {
            None => self.state.lock().unwrap().canon = Some(tip),
            Some(name) => {
                let mut views = self.views.lock().unwrap();
                let root = views.get(name).map(|v| v.0).unwrap_or(tip);
                views.insert(name.clone(), (root, tip));
            }
        }
    }

    /// The hub store's history of `purpose` from `root` above `base`, in export order, with the
    /// late keys first.
    pub fn history(
        &self,
        purpose: StreamPurpose,
        root: Hash,
        base: Option<Hash>,
    ) -> Vec<ObjectKey> {
        let read = self.store.lock().unwrap().clone().expect("a hub store");
        let found = rdy_store::closure::history_closure_before(
            read.reader(),
            purpose,
            root,
            &BTreeSet::new(),
            base,
            read.seq(),
        )
        .unwrap();
        let late = self.late.lock().unwrap().clone();
        let mut out: Vec<ObjectKey> = late
            .iter()
            .filter(|k| found.records.contains(k))
            .copied()
            .collect();
        out.extend(found.records.iter().filter(|k| !late.contains(k)).copied());
        out
    }
}

pub fn hub_not_found() -> RdyError {
    RdyError::new(
        "E_HUB_NOT_FOUND",
        "hub repo acme/docs holds no such record",
        "run rdy pull again; it reads the hub's state first",
    )
}

/// Wire fixture v5's refusal of a dump that does not exist, is not ready or another account
/// started, as the publish mapping gives it.
pub fn dump_not_found(id: &str) -> RdyError {
    RdyError::new(
        "E_HUB_NOT_FOUND",
        format!("no dump {id} of yours in acme/docs"),
        "start a dump with dumps.create, then wait for it to succeed",
    )
}

impl SyncWire for Hub {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        self.enter("discovery", cx, None, Some(remote))?;
        Ok(Discovery {
            sync_base: format!("{}/sync{REPO_PATH}", cx.install),
            protocols: vec![1],
        })
    }

    fn state(&self, cx: &UploadContext, request: &SyncStateRequest) -> Result<SyncState, RdyError> {
        self.enter("state", cx, None, None)?;
        assert_eq!(request.site, None);
        assert_eq!(request.actor, cx.claim.actor);
        self.states.lock().unwrap().push(request.clone());
        let views = self.views.lock().unwrap();
        let named = request
            .views
            .iter()
            .flatten()
            .map(|n| {
                let v = views.get(n);
                rdy_engine::hub_sync_wire::HubViewState {
                    name: n.clone(),
                    head: v.map(|v| v.1),
                    root: v.map(|v| v.0),
                }
            })
            .collect();
        Ok(SyncState {
            state: self.state.lock().unwrap().clone(),
            views: named,
        })
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.enter("known", cx, Some(push), None)?;
        let held = self.held.lock().unwrap();
        let q = self.quarantine.lock().unwrap();
        let mine = q.get(push).cloned().unwrap_or_default();
        Ok(keys
            .iter()
            .filter(|k| !held.contains(k) && !mine.contains(k))
            .copied()
            .collect())
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn std::io::Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.enter("put", cx, Some(push), None)?;
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, len);
        let report = stream_check::check_batch(&bytes[..], None).unwrap();
        assert!(matches!(
            report.header.purpose,
            StreamPurpose::CanonHistory | StreamPurpose::ViewHistory
        ));
        self.roots
            .lock()
            .unwrap()
            .push((report.header.tag, report.header.prerequisites.clone()));
        self.heads
            .lock()
            .unwrap()
            .insert(push.to_string(), report.header.tag);
        self.puts
            .lock()
            .unwrap()
            .push((push.to_string(), report.keys.clone()));
        let mut q = self.quarantine.lock().unwrap();
        let mine = q.entry(push.to_string()).or_default();
        mine.extend(report.keys.iter().copied());
        let mut bytes = self.bytes.lock().unwrap();
        let total = bytes.entry(push.to_string()).or_default();
        *total += len;
        Ok(ObjectPut {
            records: mine.len() as u64,
            bytes: *total,
        })
    }

    fn known_pull(&self, cx: &UploadContext, request: &KnownPull) -> Result<KnownPage, RdyError> {
        self.enter("known_pull", cx, None, None)?;
        self.pulls.lock().unwrap().push(request.clone());
        let read = self.store.lock().unwrap().clone().expect("a hub store");
        let reader = read.reader();
        if !reader
            .has_kind(request.root.kind, &request.root.hash)
            .unwrap()
        {
            return Err(hub_not_found());
        }
        // a base the hub store does not hold counts as none
        let base = request
            .base
            .filter(|b| reader.has_kind(b.kind, &b.hash).unwrap())
            .map(|b| b.hash);
        let all = self.history(request.purpose, request.root.hash, base);
        let start = match request.after {
            None => 0,
            Some(a) => all
                .iter()
                .position(|k| *k == a)
                .map_or(all.len(), |p| p + 1),
        };
        let size = *self.page.lock().unwrap();
        let keys: Vec<ObjectKey> = all[start..].iter().take(size).copied().collect();
        let next = (start + keys.len() < all.len()).then(|| *keys.last().unwrap());
        Ok(KnownPage { keys, next })
    }

    fn dump(
        &self,
        cx: &UploadContext,
        request: &DumpRequest,
        sink: &mut dyn std::io::Write,
        limit: u64,
    ) -> Result<u64, RdyError> {
        self.enter("dump", cx, None, None)?;
        self.dump_ids.lock().unwrap().push(request.dump.clone());
        let bytes = self.dumps.lock().unwrap().get(&request.dump).cloned();
        let bytes = bytes.ok_or_else(|| dump_not_found(&request.dump))?;
        assert!(bytes.len() as u64 <= limit);
        sink.write_all(&bytes)
            .map_err(|e| RdyError::new("E_IO", e.to_string(), "check the disk"))?;
        Ok(bytes.len() as u64)
    }

    fn get(
        &self,
        cx: &UploadContext,
        request: &GetRequest,
        sink: &mut dyn std::io::Write,
        limit: u64,
    ) -> Result<u64, RdyError> {
        self.enter("get", cx, None, None)?;
        self.gets.lock().unwrap().push(request.clone());
        let mut bytes = match self.get_bytes.lock().unwrap().take() {
            Some(b) => b,
            None => {
                // the hub's export of exactly the keys asked for (the core read a hub serves)
                let read = self.store.lock().unwrap().clone().expect("a hub store");
                let mut out = Vec::new();
                stream_export::write_history_keys(
                    &read,
                    request.purpose,
                    request.root.hash,
                    &request.keys,
                    &mut out,
                )?;
                out
            }
        };
        if request.keys.len() >= 2 && std::mem::take(&mut *self.pad_get.lock().unwrap()) {
            let cap = self.state.lock().unwrap().batch_bytes;
            let past = usize::try_from(cap).unwrap() + 1;
            if bytes.len() < past {
                bytes.resize(past, 0);
            }
        }
        let refusal = *self.refuse_get_over.lock().unwrap();
        if let Some((cap, marker)) = refusal
            && bytes.len() as u64 > cap
        {
            self.get_replies
                .lock()
                .unwrap()
                .push((bytes.len() as u64, 0));
            let mut e = RdyError::new(
                "E_HUB_VALIDATION",
                format!("the records you asked for are larger than {cap} bytes"),
                "ask for fewer records in each get",
            );
            if marker {
                e.hub_get_cap = Some(cap);
            }
            return Err(e);
        }
        // as the transport streams a reply: piece by piece, each written while the bytes so far
        // are within `limit`; the first piece past it fails the get
        let mut n = 0u64;
        for piece in bytes.chunks(rdy_engine::hub_http::UPLOAD_BUFFER_BYTES) {
            n += piece.len() as u64;
            if n > limit {
                self.get_replies
                    .lock()
                    .unwrap()
                    .push((bytes.len() as u64, n));
                return Err(rdy_engine::upstream_protocol());
            }
            sink.write_all(piece).unwrap();
        }
        self.get_replies
            .lock()
            .unwrap()
            .push((bytes.len() as u64, n));
        Ok(n)
    }

    fn write(&self, cx: &UploadContext, request: &SyncWrite) -> Result<WriteReply, WriteFailure> {
        self.enter("write", cx, Some(&request.push), None)
            .map_err(WriteFailure::not_sent)?;
        let n = {
            let mut w = self.writes.lock().unwrap();
            w.push(request.clone());
            w.len()
        };
        let plan = self
            .plan
            .lock()
            .unwrap()
            .remove(&n)
            .unwrap_or(WritePlan::Taken {
                min_rdy_raised: false,
            });
        let kind = request.root_kind();
        let tip = self
            .heads
            .lock()
            .unwrap()
            .get(&request.push)
            .copied()
            .or(request.expected.map(|k| k.hash));
        match plan {
            WritePlan::Taken { min_rdy_raised } => {
                let tip = tip.expect("a pushed head");
                self.take(request, tip);
                Ok(WriteReply {
                    outcome: WriteOutcome::Taken,
                    tip: Some(ObjectKey { kind, hash: tip }),
                    request: None,
                    min_rdy_raised,
                })
            }
            WritePlan::Held { number, version } => Ok(WriteReply {
                outcome: WriteOutcome::Held,
                tip: self
                    .state
                    .lock()
                    .unwrap()
                    .canon
                    .map(|hash| ObjectKey { kind, hash }),
                request: Some(HubRequest { number, version }),
                min_rdy_raised: false,
            }),
            WritePlan::Refuse(e) => Err(WriteFailure::refused(e)),
            WritePlan::Lost { apply } => {
                if apply {
                    self.take(request, tip.expect("a pushed head"));
                }
                Err(WriteFailure::Uncertain)
            }
            WritePlan::NotSent(e) => Err(WriteFailure::not_sent(e)),
        }
    }

    fn outcome(&self, cx: &UploadContext, push: &str) -> Result<OutcomeReply, RdyError> {
        self.enter("outcome", cx, Some(push), None)?;
        Ok(self
            .outcomes
            .lock()
            .unwrap()
            .get(push)
            .cloned()
            .unwrap_or(OutcomeReply {
                outcome: LookupOutcome::Unknown,
                kind: None,
                view: None,
                request_state: None,
            }))
    }
}

/// A renderer that captures with `render_inputs::capture` and the disagreements it is given,
/// and records each scope it captured.
#[derive(Default)]
pub struct Renderer {
    pub scopes: Mutex<Vec<Scope>>,
    pub disagreements: Mutex<Vec<DisagreementInput>>,
}

impl PublishRenderer for Renderer {
    fn capture(&self, read: &ReadState, scope: &Scope) -> Result<RenderInputs, RdyError> {
        self.scopes.lock().unwrap().push(scope.clone());
        let mut inputs = rdy_engine::render_inputs::capture(read)?;
        inputs.disagreements = self.disagreements.lock().unwrap().clone();
        Ok(inputs)
    }

    fn render(
        &self,
        _: &ReadState,
        _: &Scope,
        _: &str,
        _: &str,
        _: &RenderInputs,
    ) -> Result<RenderedUpload, RdyError> {
        panic!("a sync renders nothing")
    }
}

/// The sync service of `e` over `hub` with the clock at `now`.
pub struct Sync<'a> {
    pub e: &'a Env,
    pub hub: &'a Hub,
    pub api: NoApi,
    pub clock: Clock,
    pub renderer: Renderer,
    /// The fake clock of the service's busy outcome lookups.
    pub poll: Arc<PollClock>,
}

impl<'a> Sync<'a> {
    pub fn new(e: &'a Env, hub: &'a Hub) -> Sync<'a> {
        Sync {
            e,
            hub,
            api: NoApi::default(),
            clock: Clock::at(T0),
            renderer: Renderer::default(),
            poll: Arc::default(),
        }
    }

    pub fn service(&self) -> SyncService<'_> {
        SyncService::new(
            &self.e.project,
            &self.e.user_file,
            &self.api,
            &self.clock,
            self.hub,
            &self.renderer,
        )
        .with_outcome_poll_clock(&*self.poll)
    }

    pub fn push(&self, request: PushRequest) -> Result<PushOutcome, RdyError> {
        self.service().push(&self.e.engine, &agent(), request)
    }

    pub fn pull(&self, request: PullRequest) -> Result<PullOutcome, RdyError> {
        self.service().pull(&self.e.engine, &founder(), request)
    }
}

/// A push of `e`'s canon to `remote` (none: the default) through `hub`.
pub fn push(e: &Env, hub: &Hub, remote: Option<&str>) -> Result<PushOutcome, RdyError> {
    Sync::new(e, hub).push(PushRequest {
        remote: remote.map(str::to_string),
        ..PushRequest::default()
    })
}

pub fn upload_of(e: &Env, remote: &str) -> Option<rdy_store::sync_state::SyncUpload> {
    e.engine.read().reader().sync_upload(remote).unwrap()
}

pub fn pending_of(e: &Env, remote: &str) -> Option<rdy_store::sync_state::SyncPending> {
    e.engine.read().reader().sync_pending(remote).unwrap()
}

pub fn canon_of(e: &Env, remote: &str) -> Option<rdy_store::sync_state::SyncCanon> {
    e.engine.read().reader().sync_canon(remote).unwrap()
}

pub fn repo_of(e: &Env) -> rdy_model::RepoId {
    e.engine.read().reader().repo_id().unwrap().unwrap()
}

/// Writes sync steps for `remote` straight to the store, as a test seed.
pub fn seed_sync(e: &Env, remote: &str, steps: Vec<rdy_store::sync_state::SyncStep>) {
    let remote = remote.to_string();
    rdy_engine::engine::with_writer_result(&e.engine, move |r| {
        r.store_mut()
            .write_sync(&remote, steps)
            .map_err(RdyError::from)?;
        r.invalidate();
        Ok(())
    })
    .unwrap();
}

/// The canon history's records from `head` above `base`, as the store's closure gives them.
pub fn closure(e: &Env, head: Hash, base: Option<Hash>) -> BTreeSet<ObjectKey> {
    let read = e.engine.read();
    rdy_store::closure::history_closure_before(
        read.reader(),
        StreamPurpose::CanonHistory,
        head,
        &BTreeSet::new(),
        base,
        read.seq(),
    )
    .unwrap()
    .records
    .into_iter()
    .collect()
}

/// A copy of `e`'s project as it stands: the same repo, opened as its own engine. The hub's
/// store for the pull steps.
pub fn copy_project(e: &Env) -> (tempfile::TempDir, Local) {
    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let p = entry.path();
            if p.is_dir() {
                copy_dir(&p, &to.join(entry.file_name()));
            } else {
                let dest = to.join(entry.file_name());
                match fs::copy(&p, &dest) {
                    Ok(_) => {}
                    // Windows: the open engine's whole-file locks (lock, index) block reads; the copy opens without them.
                    #[cfg(windows)]
                    Err(err) if err.raw_os_error() == Some(33) => {
                        let _ = fs::remove_file(&dest);
                    }
                    Err(err) => panic!("copy {}: {err}", p.display()),
                }
            }
        }
    }
    let dir = private_dir();
    let to = dir.path().join("hub");
    copy_dir(&e.project, &to);
    let r = Rdy::open(&to).unwrap();
    (dir, Local::new(r))
}

/// The view op head of view `name` in `e`.
pub fn view_head(e: &Env, name: &str) -> Hash {
    e.engine.read().view_head(name).unwrap().head
}

/// Writes one doc named `doc` into view `name`, making the view when it is new.
pub fn write_in(engine: &Local, name: &str, doc: &str) {
    rdy_engine::engine::with_writer_result(engine, {
        let name = name.to_string();
        let doc = doc.to_string();
        move |r| {
            r.write(
                &writer(),
                WriteRequest {
                    view: name,
                    ops: serde_json::from_value(json!([
                        {"op": "create", "type": "doc", "fields": {"name": doc, "title": doc, "doc_type": "page"}}
                    ]))
                    .unwrap(),
                    justification: None,
                    message: "scc8".into(),
                },
            )?;
            Ok(())
        }
    })
    .unwrap();
}

/// Lands one more doc named `name` in `engine`'s project; returns the landing.
pub fn land_in(engine: &Local, name: &str) -> Hash {
    let name = name.to_string();
    rdy_engine::engine::with_writer_result(engine, move |r| Ok(land_doc(r, &name))).unwrap()
}

pub fn write_kinds(hub: &Hub) -> Vec<(WriteKind, Option<String>)> {
    hub.writes().into_iter().map(|w| (w.kind, w.view)).collect()
}
