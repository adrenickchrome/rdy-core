//! hpb14: the one publish coordinator (D-Q0AVZ, D-KDFZ3, D-48NYM, D-0VCQ5, D-QYCRB, D-VRW0A,
//! D-PJ65F, D-B8N21, D-WDKWQ, D-50JXR; hub i-6ZXW0, i-DETA1, i-F19YZ, r-K71GM). It composes
//! HA2a's settings and credentials, the typed wire, the history sender and the file sender over
//! one tagged snapshot, and stores nothing. Every test injects the credential transport, the
//! clock, the wire and the renderer; a real project and real sign-in file stand behind them.
//! Barriers order the redaction race; nothing sleeps or measures time.
//!
//! Its 0600 credential fixtures in private config directories go through `rdy_store::pio`
//! (FW-XP1 D-1RXEA).
mod common;

use rdy_engine::hub_config::{
    BAD_REMOTE_NAME_FIX, BAD_REMOTE_NAME_MESSAGE, Credential, CredentialFile, HubConfig,
    ReadyCredential, RemoteMode, encode_credentials, parse_credentials,
};
use rdy_engine::hub_publish::{
    INVENTORY_ROWS, PublishRenderer, PublishRequest, PublishResult, PublishService, RenderedUpload,
    result_json,
};
use rdy_engine::hub_publish_files::ArtifactSource;
use rdy_engine::hub_publish_wire::{
    Candidate, Discovery, FileIdentity, ObjectPut, ProfileIdentity, PublicationInputs,
    PublishState, PublishWire, StateRequest, UploadContext, encode_publish,
};
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use rdy_engine::hub_types::{
    HubCall, HubClock, HubFailure, HubMetadata, HubOperation, HubReply, HubTransport, MeReply,
    Origin, RefreshReply, RequestClaim, Secret, TokenPair,
};
use rdy_engine::publication::{self, Audience, Resolved, reference_summary};
use rdy_engine::publication_manifest::{
    MANIFEST_RECIPE, Manifest, ManifestEntry, NOTICE_SLOT, manifest_hash,
};
use rdy_engine::redaction::RedactInput;
use rdy_engine::render_inputs::{RenderInputs, capture};
use rdy_engine::*;
use rdy_model::{BlockId, Hash, RepoId, Role};
use rdy_schema::{Issue, Severity};
use rdy_store::object_refs::ObjectKey;
use rdy_store::pio;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, OnceLock};

const ACCOUNT: &str = "acc_00000000000000000000000001";
const HANDLE: &str = "test-person";
const NEW_HANDLE: &str = "renamed-person";
const SITE: &str = "docs";
const T0: i64 = 1_800_000_000_000;
const HOUR_MS: i64 = 3_600_000;
const SYNC_PATH: &str = "/sync";
const CANDIDATE_ID: &str = "pubc_00000000000000000000000001";
const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const RENDER_WARNINGS: [&str; 2] = [
    "the org profile references blocks outside its selection",
    "the scope warns",
];

/// A hub token of kind `kind` whose secret starts with `tag`, padded with `0`, and its checksum.
fn token(kind: char, tag: &str) -> String {
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

fn access1() -> String {
    token('a', "FIRSTACCESS")
}
fn refresh1() -> String {
    token('r', "FIRSTREFRESH")
}
fn access2() -> String {
    token('a', "SECONDACCESS")
}
fn refresh2() -> String {
    token('r', "SECONDREFRESH")
}

/// Two loopback install origins on ports generated at run time; nothing connects to them.
fn origins() -> &'static [String; 2] {
    static ORIGINS: OnceLock<[String; 2]> = OnceLock::new();
    ORIGINS.get_or_init(|| {
        let held: Vec<TcpListener> = (0..2)
            .map(|_| TcpListener::bind(("127.0.0.1", 0)).unwrap())
            .collect();
        let made = |i: usize| {
            let port = held[i].local_addr().unwrap().port();
            format!("https://127.0.0.1:{port}")
        };
        [made(0), made(1)]
    })
}

fn install() -> &'static str {
    &origins()[0]
}

fn other_install() -> &'static str {
    &origins()[1]
}

fn candidate_url() -> String {
    format!("{}/app/publications/{CANDIDATE_ID}", install())
}

// ---------------------------------------------------------------------------------------------
// The source project
// ---------------------------------------------------------------------------------------------

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

/// The publishing session: an agent run, whose claim the hub binds byte for byte.
fn agent() -> Session {
    Session::agent("agent:publisher".into(), Some("run-7".into()), Role::Writer).unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "hpb14".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) -> (Hash, Hash) {
    r.tag(&founder(), name, None, None).unwrap();
    let (_, hash, tag) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap();
    (hash, tag.landing)
}

fn bid(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

/// Removes every version of `targets` with a person's approval.
fn redact(r: &mut Rdy, targets: &[String]) {
    let s = writer();
    let res = r
        .redact_in(
            &s,
            "redact",
            RedactInput {
                targets: targets.to_vec(),
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(res.redaction.is_some());
    let p = r
        .propose(&s, "redact", "Remove", Some("test summary"))
        .unwrap();
    r.land(&founder(), &p.id).unwrap();
}

/// The ids of the source: docs `a` (which references `b`) and `b`, and under `admin` the
/// profiles `site` (org, selects a: one excluded reference), `press` (public, selects b: clear)
/// and `leak` (public, selects a: blocked).
struct Ids {
    a: BlockId,
    b: BlockId,
    site: BlockId,
    press: BlockId,
    leak: BlockId,
}

fn source(r: &mut Rdy) -> Ids {
    let w = write(
        r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$b", "body": "Text of b."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, b, admin) = (bid(&w, "$a"), bid(&w, "$b"), bid(&w, "$admin"));
    let w = write(
        r,
        json!([
            {"op": "create", "type": "prose", "parent": a.to_string(), "body": format!("See {{{{{b}}}}}.")},
            {"op": "create", "type": "publication_profile", "id": "$site", "parent": admin.to_string(),
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$press", "parent": admin.to_string(),
                "fields": {"key": "press", "title": "Press", "audience": "public", "select": [b.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$leak", "parent": admin.to_string(),
                "fields": {"key": "leak", "title": "Leak", "audience": "public", "select": [a.to_string()]}}
        ]),
    );
    land(r);
    Ids {
        a,
        b,
        site: bid(&w, "$site"),
        press: bid(&w, "$press"),
        leak: bid(&w, "$leak"),
    }
}

// ---------------------------------------------------------------------------------------------
// The environment: the project, its user config and the sign-in file
// ---------------------------------------------------------------------------------------------

struct Env {
    _home: tempfile::TempDir,
    _project: tempfile::TempDir,
    project: PathBuf,
    user_file: PathBuf,
    creds: PathBuf,
    r: Rdy,
    ids: Ids,
    tag_hash: Hash,
    landing: Hash,
}

fn private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(pio::permissions(0o700))
        .tempdir()
        .unwrap()
}

/// The source tagged `v1.0.0`, a publish remote `hub` and a share remote `shared` on the
/// install, the user config `config` and a sign-in to the install that lasts an hour from T0.
fn env_with(config: &str) -> Env {
    let home_dir = private_dir();
    let project_dir = private_dir();
    let rdy = home_dir.path().join("rdy");
    pio::dir_mode(&mut fs::DirBuilder::new(), 0o700)
        .create(&rdy)
        .unwrap();
    let user_file = rdy.join("config.yaml");
    fs::write(&user_file, config).unwrap();
    fs::set_permissions(&user_file, pio::permissions(0o644)).unwrap();
    let project = project_dir.path().to_path_buf();
    let mut r = Rdy::init(&project, "rdy-standard").unwrap();
    let ids = source(&mut r);
    let (tag_hash, landing) = tag(&mut r, "v1.0.0");
    let remotes = RemoteService::new(&project, &user_file);
    remotes
        .add("hub", install(), RemoteMode::Publish, RemoteLevel::Project)
        .unwrap();
    remotes
        .add("shared", install(), RemoteMode::Share, RemoteLevel::Project)
        .unwrap();
    let e = Env {
        _home: home_dir,
        _project: project_dir,
        creds: rdy.join("credentials.yaml"),
        project,
        user_file,
        r,
        ids,
        tag_hash,
        landing,
    };
    seed(
        &e.creds,
        &[(install(), ready(HANDLE, &access1(), &refresh1()))],
    );
    e
}

fn env() -> Env {
    env_with("# person settings\n")
}

fn ready(handle: &str, access: &str, refresh: &str) -> Credential {
    Credential::Ready(ReadyCredential {
        access_token: Secret::new(access),
        refresh_token: Secret::new(refresh),
        expires_at_ms: T0 + HOUR_MS,
        account: ACCOUNT.to_string(),
        handle: handle.to_string(),
    })
}

/// Writes the credential file (0600) holding exactly `records`.
fn seed(creds: &Path, records: &[(&str, Credential)]) {
    let mut file = CredentialFile::default();
    for (o, c) in records {
        file.set(Origin::parse(o).unwrap(), Some(c.clone()));
    }
    let bytes = encode_credentials(&file).unwrap();
    let tmp = creds.with_extension("seed");
    let mut f = pio::file_mode(
        OpenOptions::new().write(true).create(true).truncate(true),
        0o600,
    )
    .open(&tmp)
    .unwrap();
    f.write_all(&bytes).unwrap();
    fs::set_permissions(&tmp, pio::permissions(0o600)).unwrap();
    fs::rename(&tmp, creds).unwrap();
}

fn held(e: &Env) -> ReadyCredential {
    let bytes = fs::read(&e.creds).unwrap();
    match parse_credentials(&bytes)
        .unwrap()
        .get(&Origin::parse(install()).unwrap())
    {
        Some(Credential::Ready(r)) => r.clone(),
        other => panic!("not ready: {other:?}"),
    }
}

/// Every file below `dir`, by relative path, with its bytes; an empty `.lock` file (the
/// credential and config services' lock files, which hold no setting) is left out.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut all = every_file(dir);
    all.retain(|p, b| !(b.is_empty() && p.extension().is_some_and(|x| x == "lock")));
    all
}

fn every_file(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn request(tag: &str, to: &str, site: &str) -> PublishRequest {
    PublishRequest {
        tag: tag.into(),
        to: to.into(),
        site: site.into(),
        keep_latest: None,
    }
}

fn standard() -> PublishRequest {
    request("v1.0.0", "hub", SITE)
}

// ---------------------------------------------------------------------------------------------
// The credential transport and the clock
// ---------------------------------------------------------------------------------------------

type Answer = Result<HubReply, HubFailure>;

fn op_name(op: &HubOperation) -> &'static str {
    match op {
        HubOperation::Metadata => "Metadata",
        HubOperation::Refresh { .. } => "Refresh",
        HubOperation::Me => "Me",
        _ => "Other",
    }
}

/// A credential transport that answers each request with the next queued reply, which must be
/// for that operation, and records every operation it saw. With nothing queued, any request
/// fails the test.
#[derive(Default)]
struct Api {
    queue: Mutex<VecDeque<(&'static str, Answer)>>,
    seen: Mutex<Vec<&'static str>>,
}

impl Api {
    fn reply(&self, op: &'static str, a: Answer) -> &Api {
        self.queue.lock().unwrap().push_back((op, a));
        self
    }

    fn ops(&self) -> Vec<&'static str> {
        self.seen.lock().unwrap().clone()
    }
}

impl HubTransport for Api {
    fn exchange(&self, call: HubCall) -> Answer {
        let op = op_name(&call.operation);
        self.seen.lock().unwrap().push(op);
        let (want, a) = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("an unexpected {op} request"));
        assert_eq!(want, op, "the request order");
        a
    }
}

fn meta() -> Answer {
    let i = install();
    Ok(HubReply::Metadata(HubMetadata {
        issuer: format!("{i}/auth"),
        device_authorization_endpoint: format!("{i}/auth/device"),
        token_endpoint: format!("{i}/auth/token"),
        revocation_endpoint: format!("{i}/auth/revoke"),
    }))
}

fn refreshed() -> Answer {
    Ok(HubReply::Refresh(RefreshReply::Tokens(TokenPair {
        access_token: Secret::new(access2()),
        refresh_token: Secret::new(refresh2()),
        expires_in: 3600,
    })))
}

struct Clock(AtomicI64);

impl Clock {
    fn at(ms: i64) -> Clock {
        Clock(AtomicI64::new(ms))
    }
}

impl HubClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------------------------
// The wire
// ---------------------------------------------------------------------------------------------

/// One wire request as the fake hub saw it.
#[derive(Clone, Debug)]
struct Call {
    step: &'static str,
    bearer: String,
    claim: RequestClaim,
    sync_base: Option<String>,
    push: Option<String>,
}

type Hook = Box<dyn FnMut(&'static str) + Send>;

/// A fake hub. It holds nothing, so every key and file is missing; each request is recorded
/// with its context. `fail` makes one step fail with a given error; `hook` runs at the start of
/// every request.
struct Wire {
    state: Mutex<PublishState>,
    candidate: Mutex<Candidate>,
    fail: Mutex<BTreeMap<&'static str, RdyError>>,
    hook: Mutex<Option<Hook>>,
    calls: Mutex<Vec<Call>>,
    /// Each objects batch's bytes, each sent file by hash, and the publish bodies.
    batches: Mutex<Vec<Vec<u8>>>,
    files: Mutex<BTreeMap<Hash, Vec<u8>>>,
    published: Mutex<Vec<(Value, Option<bool>)>>,
}

fn base_state() -> PublishState {
    PublishState {
        canon: None,
        repo_id: None,
        rdy: "0.11.0".into(),
        stream_read: vec![rdy_engine::formats::formats().stream],
        stream_write: vec![rdy_engine::formats::formats().stream],
        features: vec![],
        min_rdy: None,
        batch_bytes: 67_108_864,
        batch_records: 100_000,
        quarantine_seconds: 86_400,
        profile: None,
        audience: None,
        artifact_bytes: 201_326_592,
        artifact_files: 32_000,
    }
}

impl Wire {
    fn new() -> Wire {
        Wire {
            state: Mutex::new(base_state()),
            candidate: Mutex::new(Candidate {
                id: CANDIDATE_ID.into(),
                state: "candidate".into(),
                url: candidate_url(),
            }),
            fail: Mutex::new(BTreeMap::new()),
            hook: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            batches: Mutex::new(Vec::new()),
            files: Mutex::new(BTreeMap::new()),
            published: Mutex::new(Vec::new()),
        }
    }

    fn with_state(self, f: impl FnOnce(&mut PublishState)) -> Wire {
        f(&mut self.state.lock().unwrap());
        self
    }

    fn failing(self, step: &'static str, e: RdyError) -> Wire {
        self.fail.lock().unwrap().insert(step, e);
        self
    }

    fn on_step(&self, hook: impl FnMut(&'static str) + Send + 'static) {
        *self.hook.lock().unwrap() = Some(Box::new(hook));
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn steps(&self) -> Vec<&'static str> {
        self.calls().iter().map(|c| c.step).collect()
    }

    /// Records the request, runs the hook, then answers its configured failure, if any.
    fn enter(
        &self,
        step: &'static str,
        cx: &UploadContext,
        push: Option<&str>,
    ) -> Result<(), RdyError> {
        self.calls.lock().unwrap().push(Call {
            step,
            bearer: cx.bearer.expose().to_string(),
            claim: cx.claim.clone(),
            sync_base: cx.sync_base.clone(),
            push: push.map(str::to_string),
        });
        if let Some(h) = self.hook.lock().unwrap().as_mut() {
            h(step);
        }
        match self.fail.lock().unwrap().get(step) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

impl PublishWire for Wire {
    fn discovery(&self, cx: &UploadContext, remote: &str) -> Result<Discovery, RdyError> {
        assert_eq!(remote, install(), "discovery reads the remote's URL");
        self.enter("discovery", cx, None)?;
        Ok(Discovery {
            sync_base: format!("{}{SYNC_PATH}", install()),
            protocols: vec![1],
        })
    }

    fn state(&self, cx: &UploadContext, request: &StateRequest) -> Result<PublishState, RdyError> {
        self.enter("state", cx, None)?;
        assert_eq!(request.site, SITE);
        assert_eq!(
            request.actor, cx.claim.actor,
            "the state names the bound actor"
        );
        assert_eq!(request.run, cx.claim.run);
        Ok(self.state.lock().unwrap().clone())
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.enter("known", cx, Some(push))?;
        Ok(keys.to_vec())
    }

    fn put_objects(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn Read,
        len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.enter("objects", cx, Some(push))?;
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, len);
        let mut batches = self.batches.lock().unwrap();
        batches.push(bytes);
        let total: u64 = batches.iter().map(|b| b.len() as u64).sum();
        Ok(ObjectPut {
            records: batches.len() as u64,
            bytes: total,
        })
    }

    fn known_files(
        &self,
        cx: &UploadContext,
        push: &str,
        files: &[FileIdentity],
    ) -> Result<Vec<Hash>, RdyError> {
        self.enter("files_known", cx, Some(push))?;
        Ok(files.iter().map(|f| f.hash).collect())
    }

    fn put_file(
        &self,
        cx: &UploadContext,
        push: &str,
        file: &FileIdentity,
        _media_type: &str,
        body: &mut dyn Read,
    ) -> Result<FileIdentity, RdyError> {
        self.enter("file", cx, Some(push))?;
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        self.files.lock().unwrap().insert(file.hash, bytes);
        Ok(*file)
    }

    fn publish(
        &self,
        cx: &UploadContext,
        push: &str,
        site: &str,
        record: &PublicationInputs,
        manifest: &Manifest,
        keep_latest: Option<bool>,
    ) -> Result<Candidate, RdyError> {
        self.enter("publish", cx, Some(push))?;
        let body = encode_publish(push, site, record, manifest, keep_latest).unwrap();
        self.published
            .lock()
            .unwrap()
            .push((serde_json::from_slice(&body).unwrap(), keep_latest));
        Ok(self.candidate.lock().unwrap().clone())
    }
}

// ---------------------------------------------------------------------------------------------
// The renderer
// ---------------------------------------------------------------------------------------------

/// An immutable two-file artifact: one page with its empty notice slot and one stylesheet.
struct Mem {
    manifest: Manifest,
    files: BTreeMap<String, Vec<u8>>,
}

impl ArtifactSource for Mem {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn paths(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    fn open(&self, path: &str) -> Result<Box<dyn Read + '_>, RdyError> {
        Ok(Box::new(std::io::Cursor::new(self.files[path].as_slice())))
    }
}

fn page_artifact(profile: &str) -> Mem {
    let head = "<html><main>".to_string();
    let page = format!(
        "{head}{}<h1>{profile}</h1></main></html>",
        std::str::from_utf8(NOTICE_SLOT).unwrap()
    );
    let css = b"main { margin: 0 }".to_vec();
    let mut files = BTreeMap::new();
    files.insert("index.html".to_string(), page.clone().into_bytes());
    files.insert("assets/site.css".to_string(), css.clone());
    let manifest = Manifest {
        files: vec![
            ManifestEntry {
                path: "assets/site.css".into(),
                hash: Hash::of(&css),
                media_type: "text/css".into(),
                size: css.len() as u64,
                notice_offset: None,
            },
            ManifestEntry {
                path: "index.html".into(),
                hash: Hash::of(page.as_bytes()),
                media_type: "text/html".into(),
                size: page.len() as u64,
                notice_offset: Some(head.len() as u64),
            },
        ],
    };
    Mem { manifest, files }
}

/// What the fake renderer was asked: the scope, the tag and the profile of each render.
#[derive(Default)]
struct Renderer {
    captures: AtomicUsize,
    renders: Mutex<Vec<(Scope, String, String)>>,
    manifest_hash: Mutex<Option<Hash>>,
}

impl PublishRenderer for Renderer {
    fn capture(&self, read: &ReadState, _scope: &Scope) -> Result<RenderInputs, RdyError> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        capture(read)
    }

    fn render(
        &self,
        _read: &ReadState,
        scope: &Scope,
        tag: &str,
        profile: &str,
        _inputs: &RenderInputs,
    ) -> Result<RenderedUpload, RdyError> {
        self.renders
            .lock()
            .unwrap()
            .push((scope.clone(), tag.to_string(), profile.to_string()));
        let artifact = page_artifact(profile);
        *self.manifest_hash.lock().unwrap() =
            Some(manifest_hash(&artifact.manifest, MANIFEST_RECIPE).unwrap());
        Ok(RenderedUpload {
            artifact: Box::new(artifact),
            warnings: RENDER_WARNINGS.iter().map(|w| w.to_string()).collect(),
        })
    }
}

fn run_with(
    e: &Env,
    api: &Api,
    clock: &Clock,
    wire: &Wire,
    session: &Session,
    req: PublishRequest,
    renderer: &Renderer,
) -> Result<PublishResult, RdyError> {
    PublishService::new(&e.project, &e.user_file, api, clock, wire).run(
        e.r.read(),
        session,
        req,
        renderer,
    )
}

fn run(e: &Env, wire: &Wire, req: PublishRequest) -> Result<PublishResult, RdyError> {
    run_with(
        e,
        &Api::default(),
        &Clock::at(T0),
        wire,
        &agent(),
        req,
        &Renderer::default(),
    )
}

fn triple(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

fn resolved_at(e: &Env, selector: &str) -> Resolved {
    publication::resolve(&e.r.read(), &Scope::At(e.landing), selector).unwrap()
}

// ---------------------------------------------------------------------------------------------
// hpb14
// ---------------------------------------------------------------------------------------------

/// A full upload under one push id returns the hub's candidate, its page and `review: hub`,
/// and changes no byte of the project: no log record, ref, tag, config or receipt.
#[test]
fn hpb14_end_to_end_upload_returns_candidate_and_writes_no_project_record() {
    let e = env();
    let refs_before = e.r.read().reader().refs_with_prefix("").unwrap();
    let tree_before = tree(&e.project);
    let config_before = tree(e.user_file.parent().unwrap());
    let wire =
        Wire::new().with_state(|s| s.repo_id = Some(e.r.store().repo_id().unwrap().unwrap()));
    let renderer = Renderer::default();
    let mut req = standard();
    req.keep_latest = Some(false);
    let r = run_with(
        &e,
        &Api::default(),
        &Clock::at(T0),
        &wire,
        &agent(),
        req,
        &renderer,
    )
    .unwrap();

    // the candidate, as the hub made it
    assert_eq!(r.id, CANDIDATE_ID);
    assert_eq!(r.state, "candidate");
    assert_eq!(r.url, candidate_url());
    assert_eq!(r.review, "hub");
    assert_eq!(r.audience, Audience::Org, "implicit:org's audience");
    // the step order, one push id and one sync base after discovery
    let steps = wire.steps();
    assert_eq!(&steps[..3], ["discovery", "state", "known"]);
    let files_at = steps.iter().position(|s| *s == "files_known").unwrap();
    assert!(steps[3..files_at].iter().all(|s| *s == "objects") && files_at > 3);
    assert!(
        steps[files_at + 1..steps.len() - 1]
            .iter()
            .all(|s| *s == "file")
    );
    assert_eq!(steps.len() - files_at - 2, 2, "both artifact files");
    assert_eq!(steps.last(), Some(&"publish"));
    let calls = wire.calls();
    assert_eq!(calls[0].sync_base, None);
    let base = format!("{}{SYNC_PATH}", install());
    assert!(
        calls[1..]
            .iter()
            .all(|c| c.sync_base.as_deref() == Some(&base))
    );
    assert_eq!(r.push.len(), 26);
    assert!(
        calls[2..]
            .iter()
            .all(|c| c.push.as_deref() == Some(r.push.as_str())),
        "every upload request names the one push id"
    );
    // the record: the tag's stored name and landing, the implicit selection, the manifest
    let implicit = resolved_at(&e, "implicit:org");
    assert_eq!(r.record.tag, "v1.0.0");
    assert_eq!(r.record.landing, e.landing);
    assert_eq!(r.record.profile, None);
    assert_eq!(r.record.selection, implicit.entries);
    assert_eq!(r.record.selection_hash, implicit.selection_hash);
    assert_eq!(r.record.manifest_recipe, 1);
    assert_eq!(
        Some(r.record.manifest_hash),
        *renderer.manifest_hash.lock().unwrap()
    );
    assert_eq!(r.record.repo_id, e.r.store().repo_id().unwrap().unwrap());
    assert_eq!(r.record.render_inputs, capture(&e.r.read()).unwrap());
    let renders = renderer.renders.lock().unwrap().clone();
    assert_eq!(
        renders,
        vec![(
            Scope::At(e.landing),
            "v1.0.0".to_string(),
            "implicit:org".to_string()
        )]
    );
    // keep_latest went beside the record, unchanged
    let published = wire.published.lock().unwrap().clone();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].1, Some(false));
    assert_eq!(published[0].0["keep_latest"], json!(false));
    assert_eq!(published[0].0["site"], json!(SITE));
    // the history is the tag's: its batches carry its root
    assert!(!wire.batches.lock().unwrap().is_empty());
    let _ = e.tag_hash;

    // nothing local changed
    assert_eq!(
        e.r.read().reader().refs_with_prefix("").unwrap(),
        refs_before
    );
    assert_eq!(tree(&e.project), tree_before, "no project byte changed");
    assert_eq!(
        tree(e.user_file.parent().unwrap()),
        config_before,
        "no config or credential byte changed"
    );

    // absent keep_latest is absent on the wire
    let wire = Wire::new();
    run(&e, &wire, standard()).unwrap();
    let published = wire.published.lock().unwrap().clone();
    assert_eq!(published[0].1, None);
    assert!(published[0].0.get("keep_latest").is_none());
}

/// The site's profile resolves at the tag, not in current canon: the tag's revision, its
/// selection and its audience, whatever canon holds now and whatever version the hub names.
#[test]
fn hpb14_site_profile_is_resolved_at_the_tag() {
    let mut e = env();
    let at_tag = resolved_at(&e, &e.ids.site.to_string());
    // canon moves on: the profile now selects both docs, for the public
    write(
        &mut e.r,
        json!([{"op": "update", "id": e.ids.site.to_string(),
            "fields": {"title": "Site now", "audience": "public", "select": [e.ids.a.to_string(), e.ids.b.to_string()]}}]),
    );
    land(&mut e.r);
    let canon = publication::resolve(&e.r.read(), &Scope::Canon, &e.ids.site.to_string()).unwrap();
    assert_ne!(canon.profile.version, at_tag.profile.version, "guard");
    assert_ne!(canon.entries, at_tag.entries, "guard");
    assert_eq!(canon.profile.audience, Audience::Public, "guard");

    for audience in [None, Some(Audience::Org)] {
        let site = e.ids.site.clone();
        let canon_version = canon.profile.version.unwrap();
        let wire = Wire::new().with_state(|s| {
            s.profile = Some(ProfileIdentity {
                id: site,
                version: canon_version,
            });
            s.audience = audience;
        });
        let renderer = Renderer::default();
        let r = run_with(
            &e,
            &Api::default(),
            &Clock::at(T0),
            &wire,
            &agent(),
            standard(),
            &renderer,
        )
        .unwrap();
        assert_eq!(
            r.record.profile,
            Some(ProfileIdentity {
                id: e.ids.site.clone(),
                version: at_tag.profile.version.unwrap(),
            }),
            "the tagged revision, not the hub's version"
        );
        assert_eq!(r.record.selection, at_tag.entries);
        assert_eq!(r.record.selection_hash, at_tag.selection_hash);
        assert_eq!(r.audience, Audience::Org, "the tagged profile's audience");
        assert_eq!(r.references, reference_summary(&at_tag.references));
        assert_eq!(
            renderer.renders.lock().unwrap().clone(),
            vec![(
                Scope::At(e.landing),
                "v1.0.0".to_string(),
                e.ids.site.to_string()
            )]
        );
        let doc_ids: Vec<&BlockId> = r.inventory.docs.iter().map(|d| &d.id).collect();
        assert_eq!(doc_ids, vec![&e.ids.a], "the tagged selection's docs");
    }
}

/// Each refusal before upload stops at its own step: request checks and settings send nothing;
/// a repo, audience or public reference refusal stops after state, before any render or upload.
#[test]
fn hpb14_wrong_remote_project_or_public_reference_stops_before_upload() {
    let e = env();
    let other_repo = RepoId::new();
    let long_tag = "v".repeat(1_048_577);
    let profile = |id: &BlockId| {
        let id = id.clone();
        move |s: &mut PublishState| {
            s.profile = Some(ProfileIdentity {
                id,
                version: Hash::of(b"any"),
            });
        }
    };
    type Setup = Box<dyn FnOnce(&mut PublishState)>;
    let none: fn() -> Setup = || Box::new(|_| {});
    type Case<'s> = (
        &'s str,
        PublishRequest,
        Setup,
        (String, String, String),
        Vec<&'s str>,
    );
    let cases: Vec<Case> = vec![
        (
            "missing remote",
            request("v1.0.0", "nowhere", SITE),
            none(),
            (
                E_NOT_FOUND.into(),
                "remote nowhere does not exist".into(),
                "list the remotes, then pass a listed name".into(),
            ),
            vec![],
        ),
        (
            "share remote",
            request("v1.0.0", "shared", SITE),
            none(),
            (
                E_BAD_INPUT.into(),
                "remote shared is not a publish remote".into(),
                "set its mode to publish, or choose a publish remote".into(),
            ),
            vec![],
        ),
        (
            "remote name",
            request("v1.0.0", "-bad", SITE),
            none(),
            (
                E_BAD_INPUT.into(),
                BAD_REMOTE_NAME_MESSAGE.into(),
                BAD_REMOTE_NAME_FIX.into(),
            ),
            vec![],
        ),
        (
            "site name",
            request("v1.0.0", "hub", "-docs"),
            none(),
            (
                E_BAD_INPUT.into(),
                "the site name is invalid".into(),
                "use 1 to 64 letters, digits, underscores or hyphens, starting with a letter or digit".into(),
            ),
            vec![],
        ),
        (
            "long site name",
            request("v1.0.0", "hub", &"d".repeat(65)),
            none(),
            (
                E_BAD_INPUT.into(),
                "the site name is invalid".into(),
                "use 1 to 64 letters, digits, underscores or hyphens, starting with a letter or digit".into(),
            ),
            vec![],
        ),
        (
            "tag bound",
            request(&long_tag, "hub", SITE),
            none(),
            (
                E_LIMIT.into(),
                "upload tag exceeds 1048576 bytes".into(),
                "reduce the tag name to the stated limit".into(),
            ),
            vec![],
        ),
        (
            "missing tag",
            request("v9.9.9", "hub", SITE),
            none(),
            (
                E_NOT_FOUND.into(),
                "no version v9.9.9".into(),
                "list versions with rdy tags".into(),
            ),
            vec![],
        ),
        (
            "another project",
            standard(),
            Box::new(move |s: &mut PublishState| s.repo_id = Some(other_repo)),
            (
                E_REPO_MISMATCH.into(),
                "the remote holds another source project".into(),
                "choose a remote for this project".into(),
            ),
            vec!["discovery", "state"],
        ),
        (
            "audience",
            standard(),
            Box::new(move |s: &mut PublishState| s.audience = Some(Audience::Public)),
            (
                E_BAD_INPUT.into(),
                "the tagged profile audience does not match the site".into(),
                "ask an owner to create a site for this audience".into(),
            ),
            vec!["discovery", "state"],
        ),
        (
            "explicit audience",
            standard(),
            {
                let set = profile(&e.ids.press);
                Box::new(move |s: &mut PublishState| {
                    set(s);
                    s.audience = Some(Audience::Org);
                })
            },
            (
                E_BAD_INPUT.into(),
                "the tagged profile audience does not match the site".into(),
                "ask an owner to create a site for this audience".into(),
            ),
            vec!["discovery", "state"],
        ),
        (
            "public reference",
            standard(),
            Box::new(profile(&e.ids.leak)),
            (E_PUBLICATION_REFERENCE.into(), String::new(), String::new()),
            vec!["discovery", "state"],
        ),
    ];
    for (name, req, setup, want, steps) in cases {
        let wire = Wire::new();
        setup(&mut wire.state.lock().unwrap());
        let api = Api::default();
        let renderer = Renderer::default();
        let err =
            run_with(&e, &api, &Clock::at(T0), &wire, &agent(), req, &renderer).expect_err(name);
        if want.0 == E_PUBLICATION_REFERENCE {
            assert_eq!(err.code, E_PUBLICATION_REFERENCE, "{name}: {err:?}");
        } else {
            assert_eq!(
                triple(&err),
                (want.0.as_str(), want.1.as_str(), want.2.as_str()),
                "{name}"
            );
        }
        assert_eq!(wire.steps(), steps, "{name}: the requests made");
        assert_eq!(
            renderer.captures.load(Ordering::SeqCst),
            0,
            "{name}: no capture"
        );
        assert!(
            renderer.renders.lock().unwrap().is_empty(),
            "{name}: no render"
        );
        assert!(
            wire.batches.lock().unwrap().is_empty(),
            "{name}: no history"
        );
        assert!(
            wire.published.lock().unwrap().is_empty(),
            "{name}: no publish"
        );
        assert!(api.ops().is_empty(), "{name}: access was current");
    }

    // the same project, the cleared public profile and the matching audience pass
    let wire = Wire::new().with_state(|s| {
        profile(&e.ids.press)(s);
        s.audience = Some(Audience::Public);
        s.repo_id = Some(e.r.store().repo_id().unwrap().unwrap());
    });
    let r = run(&e, &wire, standard()).unwrap();
    assert_eq!(r.audience, Audience::Public);
}

/// Each request gets current access right before it is sent: an expiry mid-upload renews the
/// token once, every later request carries the new one, and every request carries the same
/// bound claim.
#[test]
fn hpb14_each_request_uses_current_access_and_the_same_claim() {
    let e = env();
    let clock = Arc::new(Clock::at(T0));
    let api = Api::default();
    api.reply("Metadata", meta()).reply("Refresh", refreshed());
    let wire = Wire::new();
    let later = Arc::clone(&clock);
    let mut moved = false;
    wire.on_step(move |step| {
        if step == "objects" && !moved {
            moved = true;
            // the token now has less than a minute left
            later.0.store(T0 + HOUR_MS - 30_000, Ordering::SeqCst);
        }
    });
    // a person's session: the claim is bound to the account's handle
    let person = Session::human("human:founder".into(), Role::Writer).unwrap();
    let r = run_with(
        &e,
        &api,
        &clock,
        &wire,
        &person,
        standard(),
        &Renderer::default(),
    )
    .unwrap();
    assert_eq!(r.state, "candidate");
    assert_eq!(api.ops(), ["Metadata", "Refresh"], "one renewal");
    let calls = wire.calls();
    let first_objects = calls.iter().position(|c| c.step == "objects").unwrap();
    for (i, c) in calls.iter().enumerate() {
        let want = if i <= first_objects {
            access1()
        } else {
            access2()
        };
        assert_eq!(
            c.bearer, want,
            "request {i} ({}) carries current access",
            c.step
        );
        assert_eq!(
            c.claim, calls[0].claim,
            "request {i} ({}): one claim",
            c.step
        );
    }
    assert_eq!(
        calls[0].claim.actor,
        format!("human:{HANDLE}"),
        "bound to the handle"
    );
    assert!(
        calls
            .iter()
            .skip(first_objects + 1)
            .any(|c| c.step == "publish")
    );
    let now = held(&e);
    assert_eq!(
        now.access_token.expose(),
        access2(),
        "the renewal was saved"
    );
}

/// Runs the standard publish on its own thread and holds it at the first `during` request
/// while `between` runs on this thread, then lets it go on, and returns the run's outcome.
fn hold_at(
    e: &mut Env,
    wire: &Wire,
    during: &'static str,
    between: impl FnOnce(&mut Env),
) -> Result<PublishResult, RdyError> {
    let read = e.r.read();
    let reached = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let (r1, r2) = (Arc::clone(&reached), Arc::clone(&resume));
    let mut held_once = false;
    wire.on_step(move |step| {
        if step == during && !held_once {
            held_once = true;
            r1.wait();
            r2.wait();
        }
    });
    let (project, user_file) = (e.project.clone(), e.user_file.clone());
    let api = Api::default();
    let clock = Clock::at(T0);
    std::thread::scope(|s| {
        let (api, clock) = (&api, &clock);
        let h = s.spawn(move || {
            PublishService::new(&project, &user_file, api, clock, wire).run(
                read,
                &agent(),
                standard(),
                &Renderer::default(),
            )
        });
        reached.wait();
        between(e);
        resume.wait();
        h.join().unwrap()
    })
}

/// A redaction applied while the upload runs stops it before its next phase: no later request
/// and no publish is sent, and no success is returned. A redaction that lands while the publish
/// request is in flight returns no success either, though the hub answered with a valid
/// candidate: the refusal keeps the push id, since the candidate may exist, and nothing is
/// sent again.
#[test]
fn hpb14_redaction_between_phases_aborts_before_publish() {
    for during in ["objects", "file", "publish"] {
        let mut e = env();
        let wire = Wire::new();
        let target = e.ids.b.to_string();
        let outcome = hold_at(&mut e, &wire, during, |e| redact(&mut e.r, &[target]));
        let err = outcome.expect_err(during);
        assert_eq!(err.code, E_REDACTED_STATE, "{during}: {err:?}");
        let steps = wire.steps();
        let at = steps.iter().position(|s| *s == during).unwrap();
        if during == "publish" {
            let push = wire
                .calls()
                .iter()
                .find_map(|c| c.push.clone())
                .expect("a push id was sent");
            assert_eq!(
                err.issues,
                vec![Issue {
                    severity: Severity::Error,
                    code: E_UPSTREAM_UNCERTAIN,
                    block: None,
                    message: "upload phase publish".into(),
                    fix: format!("check push {push} on the hub before publishing again"),
                }],
                "the refusal keeps the push id"
            );
            assert_eq!(
                &steps[at..],
                ["publish"],
                "publish is sent once and nothing after it: {steps:?}"
            );
            continue;
        }
        assert!(
            !steps.contains(&"publish"),
            "{during}: no publish after a redaction: {steps:?}"
        );
        match during {
            "objects" => assert_eq!(&steps[at + 1..], [] as [&str; 0], "no files phase"),
            _ => assert_eq!(steps.iter().filter(|s| **s == "file").count(), 1),
        }
    }
}

/// The race harness never outwaits a run that ends before the held request: it returns
/// that outcome (a failed run here) at once, so a broken run fails the test and does not hold
/// the test binary for hours (the M3a gate hangs).
#[test]
fn hpb14_the_race_harness_returns_when_the_run_ends_before_the_held_request() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut e = env();
        let wire = Wire::new().failing("state", upstream_transport());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            hold_at(&mut e, &wire, "objects", |_| {
                panic!("the run never reached objects, so nothing runs between")
            })
        }));
        let _ = tx.send(outcome.map(|o| o.map(|_| ()).map_err(|e| e.code)));
    });
    let outcome = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the race harness returns within 30 s when the run ends before the held request");
    let refused = outcome.expect_err("hold_at reports a run that ended early as a panic");
    let text = refused
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        text.contains("ended before it reached objects") && text.contains(E_UPSTREAM_TRANSPORT),
        "the panic names the held request and the run's outcome: {text}"
    );
}

/// A lost publish reply, or a reply that breaks the candidate contract, may mean the candidate
/// exists: the push id is kept in the uncertainty and nothing is sent again. A lost write in an
/// earlier phase names that phase; a failed read stays a transport failure.
#[test]
fn hpb14_lost_publish_reply_preserves_push_and_never_retries() {
    let e = env();
    let check = |wire: &Wire, phase: &str, step: &str| {
        let err = run(&e, wire, standard()).unwrap_err();
        let push = wire
            .calls()
            .iter()
            .find_map(|c| c.push.clone())
            .expect("a push id was sent");
        let fix = format!("check push {push} on the hub before publishing again");
        assert_eq!(
            triple(&err),
            (
                E_UPSTREAM_UNCERTAIN,
                "the publication upload may have completed",
                fix.as_str()
            ),
            "{phase}"
        );
        assert_eq!(
            err.issues,
            vec![Issue {
                severity: Severity::Error,
                code: E_UPSTREAM_UNCERTAIN,
                block: None,
                message: format!("upload phase {phase}"),
                fix: fix.clone(),
            }],
            "{phase}"
        );
        assert!(
            wire.calls()
                .iter()
                .filter_map(|c| c.push.as_deref())
                .all(|p| p == push),
            "{phase}: one push id"
        );
        assert_eq!(
            wire.steps().iter().filter(|s| **s == step).count(),
            1,
            "{phase}: {step} sent once"
        );
        assert_eq!(
            wire.steps().last(),
            Some(&step),
            "{phase}: nothing after it"
        );
    };
    check(
        &Wire::new().failing("publish", upstream_uncertain()),
        "publish",
        "publish",
    );
    check(
        &Wire::new().failing("objects", upstream_uncertain()),
        "objects",
        "objects",
    );
    let wire = Wire::new().failing("file", upstream_uncertain());
    check(&wire, "files", "file");
    for url in [
        format!("http://{}/app/c", install().trim_start_matches("https://")),
        format!("{}/app/c", other_install()),
        format!(
            "https://user@{}/app/c",
            install().trim_start_matches("https://")
        ),
        format!("{}/app/c#review", install()),
        "/app/c".to_string(),
    ] {
        let wire = Wire::new();
        wire.candidate.lock().unwrap().url = url;
        check(&wire, "publish", "publish");
    }

    // a failed read is no uncertainty
    let wire = Wire::new().failing("state", upstream_transport());
    let err = run(&e, &wire, standard()).unwrap_err();
    assert_eq!(triple(&err), triple(&upstream_transport()));
    assert!(err.issues.is_empty());
    assert_eq!(wire.steps(), ["discovery", "state"]);
    let wire = Wire::new().failing("files_known", upstream_transport());
    let err = run(&e, &wire, standard()).unwrap_err();
    assert_eq!(err.code, E_UPSTREAM_TRANSPORT);
    assert_eq!(wire.steps().last(), Some(&"files_known"));
}

/// The result names the hub as the review and gives a bounded local inventory: never a first
/// publication, a baseline or a disclosure comparison.
#[test]
fn hpb14_result_never_claims_a_missing_baseline_is_first() {
    let mut e = env();
    let extra = 205;
    let ops: Vec<Value> = (0..extra)
        .map(|i| json!({"op": "create", "type": "doc", "fields": {"name": format!("n{i:03}"), "title": format!("N {i}"), "doc_type": "page"}}))
        .collect();
    write(&mut e.r, Value::Array(ops));
    let blob = e.r.store_mut().put_blob(b"figure bytes").unwrap();
    write(
        &mut e.r,
        json!([{"op": "create", "type": "figure", "parent": e.ids.a.to_string(),
            "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}}]),
    );
    land(&mut e.r);
    let (_, landing) = tag(&mut e.r, "v1.1.0");
    let resolved = publication::resolve(&e.r.read(), &Scope::At(landing), "implicit:org").unwrap();
    assert!(resolved.docs.len() > INVENTORY_ROWS, "guard");
    let wire = Wire::new();
    let r = run(&e, &wire, request("v1.1.0", "hub", SITE)).unwrap();
    let v = result_json(&r).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "id",
        "state",
        "url",
        "push",
        "audience",
        "record",
        "references",
        "inventory",
        "warnings",
        "render_warnings",
        "review",
    ];
    want.sort();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(sorted, want);
    assert_eq!(v["review"], "hub");
    assert_eq!(v["state"], "candidate");
    assert_eq!(v["audience"], "org");
    let inv = v["inventory"].as_object().unwrap();
    let mut inv_keys: Vec<&str> = inv.keys().map(String::as_str).collect();
    inv_keys.sort();
    assert_eq!(
        inv_keys,
        [
            "attachments",
            "attachments_omitted",
            "docs",
            "docs_omitted",
            "selection_count"
        ]
    );
    assert_eq!(inv["docs"].as_array().unwrap().len(), INVENTORY_ROWS);
    assert_eq!(
        inv["docs_omitted"],
        json!(resolved.docs.len() - INVENTORY_ROWS)
    );
    assert_eq!(inv["selection_count"], json!(resolved.entries.len()));
    assert_eq!(inv["attachments_omitted"], json!(0));
    let first_doc = &inv["docs"][0];
    let d = &resolved.docs[0];
    assert_eq!(
        *first_doc,
        json!({"id": d.id.to_string(), "path": d.path, "digest": d.digest.to_string(), "redacted": d.redacted})
    );
    let att = &inv["attachments"][0];
    let a = &resolved.attachments[0];
    assert_eq!(
        *att,
        json!({"block": a.block.to_string(), "home_doc": a.home_doc.to_string(), "blob": a.blob.to_string(), "available": true})
    );
    let hex = att["blob"].as_str().unwrap();
    assert!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let docs: Vec<String> = inv["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect();
    let want_docs: Vec<String> = resolved.docs[..INVENTORY_ROWS]
        .iter()
        .map(|d| d.id.to_string())
        .collect();
    assert_eq!(docs, want_docs, "the first rows in the resolution's order");
    // the record as sent, lowercase hashes; no key outside it claims a baseline
    assert_eq!(
        v["record"]["selection"].as_array().unwrap().len(),
        resolved.entries.len()
    );
    assert_eq!(v["record"]["landing"], json!(landing.to_string()));
    assert_eq!(v["record"]["profile"], Value::Null);
    fn keys_of(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    out.push(k.clone());
                    keys_of(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| keys_of(x, out)),
            _ => {}
        }
    }
    let mut all = Vec::new();
    for (k, x) in v.as_object().unwrap() {
        if k != "record" {
            keys_of(x, &mut all);
        }
    }
    for word in [
        "first",
        "baseline",
        "previous",
        "added",
        "comparison",
        "disclosure",
        "new",
    ] {
        assert!(!all.iter().any(|k| k.contains(word)), "{word} in {all:?}");
    }
}

/// Settings warnings are kept in first-seen order with exact repeats removed, the renderer's
/// warnings unchanged in theirs; a failure carries the same warnings as issues, and a refused
/// actor claim adds the handle warning without sending anything again.
#[test]
fn hpb14_warnings_survive_success_and_failure() {
    let mut e = env_with("hub:\n  bogus: 1\n  other: 2\n");
    // the same unknown setting in the project's own config: the same issue again
    let project_cfg = e.project.join(".rdy.yaml");
    let mut text = fs::read_to_string(&project_cfg).unwrap();
    text.push_str("hub:\n  bogus: 1\n");
    fs::write(&project_cfg, text).unwrap();
    let settings = HubConfig::load(Some(&e.project), Some(&e.user_file)).unwrap();
    assert!(
        settings.warnings.len() >= 2,
        "guard: {:?}",
        settings.warnings
    );
    let mut once: Vec<Issue> = Vec::new();
    for w in &settings.warnings {
        if !once.contains(w) {
            once.push(w.clone());
        }
    }
    assert!(
        once.len() < settings.warnings.len(),
        "guard: an exact repeat"
    );

    let r = run(&e, &Wire::new(), standard()).unwrap();
    assert_eq!(r.warnings, once, "first seen, each once");
    assert_eq!(
        r.render_warnings, RENDER_WARNINGS,
        "the renderer's, in its order"
    );

    // a failure: the same warnings, as issues after the error's own
    let wire = Wire::new().failing("state", upstream_transport());
    let err = run(&e, &wire, standard()).unwrap_err();
    assert_eq!(err.issues, once);
    let err = run(&e, &Wire::new(), request("v1.0.0", "shared", SITE)).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(err.issues, once, "a settings refusal carries them too");
    let err = run(&e, &Wire::new(), request("v1.0.0", "nowhere", SITE)).unwrap_err();
    assert_eq!(err.issues, once);
    // an uncertain upload: its one phase issue, then the warnings
    let wire = Wire::new().failing("publish", upstream_uncertain());
    let err = run(&e, &wire, standard()).unwrap_err();
    assert_eq!(err.issues.len(), 1 + once.len());
    assert_eq!(err.issues[0].message, "upload phase publish");
    assert_eq!(&err.issues[1..], once.as_slice());

    // a refused actor claim: /me once, the handle warning, the original refusal, no resend
    let person = Session::human("human:founder".into(), Role::Writer).unwrap();
    let refusal = RdyError::new("E_HUB_ACTOR_CLAIM", "the hub's message", "the hub's fix");
    let wire = Wire::new().failing("objects", refusal.clone());
    let api = Api::default();
    api.reply(
        "Me",
        Ok(HubReply::Me(MeReply {
            account: ACCOUNT.into(),
            handle: NEW_HANDLE.into(),
        })),
    );
    let err = run_with(
        &e,
        &api,
        &Clock::at(T0),
        &wire,
        &person,
        standard(),
        &Renderer::default(),
    )
    .unwrap_err();
    assert_eq!(triple(&err), triple(&refusal), "the original refusal");
    assert_eq!(api.ops(), ["Me"]);
    assert_eq!(wire.steps().iter().filter(|s| **s == "objects").count(), 1);
    assert_eq!(wire.steps().last(), Some(&"objects"), "nothing sent again");
    assert_eq!(&err.issues[..once.len()], once.as_slice());
    let handle: Vec<&Issue> = err.issues[once.len()..].iter().collect();
    assert_eq!(handle.len(), 1, "{:?}", err.issues);
    assert_eq!(handle[0].code, W_UPSTREAM_HANDLE_CHANGED);
    assert_eq!(held(&e).handle, NEW_HANDLE);
    e.r.invalidate();
}

/// An engine that runs without a daemon has no publisher: it refuses and names both ways on.
#[test]
fn hpb14_an_engine_without_a_daemon_refuses_to_publish() {
    let dir = tempfile::tempdir().unwrap();
    let local = rdy_engine::engine::Local::new(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let err = local.publish(standard(), &agent()).unwrap_err();
    assert_eq!(
        triple(&err),
        (
            E_BAD_INPUT,
            "rdy_publish runs in the daemon, and this session runs without one",
            "drop --no-daemon, or run rdy publish <tag> --to <remote> --site <site>"
        )
    );
}

/// The request is strict: unknown keys and a `keep_latest` that is not a boolean or null are
/// refused; absent and null are both no value.
#[test]
fn hpb14_the_request_is_strict_and_keep_latest_is_optional() {
    let parse = |v: Value| serde_json::from_value::<PublishRequest>(v);
    let base = json!({"tag": "v1.0.0", "to": "hub", "site": SITE});
    assert_eq!(parse(base.clone()).unwrap().keep_latest, None);
    let mut with = base.clone();
    with["keep_latest"] = Value::Null;
    assert_eq!(parse(with.clone()).unwrap().keep_latest, None);
    with["keep_latest"] = json!(true);
    assert_eq!(parse(with.clone()).unwrap().keep_latest, Some(true));
    with["keep_latest"] = json!(false);
    assert_eq!(parse(with.clone()).unwrap().keep_latest, Some(false));
    for bad in [json!("true"), json!(1), json!([]), json!({})] {
        with["keep_latest"] = bad.clone();
        assert!(parse(with.clone()).is_err(), "{bad}");
    }
    let mut extra = base.clone();
    extra["audience"] = json!("public");
    assert!(parse(extra).is_err(), "no caller audience");
    let mut extra = base;
    extra["profile"] = json!("implicit:org");
    assert!(parse(extra).is_err(), "no caller profile");
}
