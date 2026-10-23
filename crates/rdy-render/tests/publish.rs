//! hpb14 (D-Q0AVZ, D-KDFZ3, D-0VCQ5, D-QYCRB, D-PJ65F; hub i-F19YZ): the publish coordinator with
//! the real renderer. The renderer captures the tag scope's render inputs and builds the
//! profile's publication from them in memory (`build_publication`), with the tag's stored name
//! and its redaction mark; the coordinator checks and uploads exactly that artifact. A fake
//! wire records every request; a real project and sign-in file stand behind it.
//!
//! Its 0600 credential fixture in a private config directory goes through `rdy_store::pio`
//! (FW-XP1 D-1RXEA).
use rdy_engine::hub_config::{
    Credential, CredentialFile, ReadyCredential, RemoteMode, encode_credentials,
};
use rdy_engine::hub_publish::{PublishRenderer, PublishRequest, PublishService};
use rdy_engine::hub_publish_wire::{
    Candidate, Discovery, FileIdentity, ObjectPut, ProfileIdentity, PublicationInputs,
    PublishState, PublishWire, StateRequest, UploadContext,
};
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use rdy_engine::hub_types::{
    HubCall, HubClock, HubFailure, HubReply, HubTransport, Origin, Secret,
};
use rdy_engine::publication::ReadPublicationSource;
use rdy_engine::publication_manifest::Manifest;
use rdy_engine::render_source::{ReadTagHistorySource, tag_is_redacted};
use rdy_engine::*;
use rdy_model::*;
use rdy_render::build_publication;
use rdy_render::publish::PublicationRenderer;
use rdy_render::render_input::capture_inputs;
use rdy_store::object_refs::ObjectKey;
use rdy_store::pio;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const T0: i64 = 1_800_000_000_000;

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
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
    let p = r.propose(&writer(), "work", "hpb14", None).unwrap();
    r.land(&founder(), &p.id).unwrap();
}

/// A loopback install origin on a port chosen at run time; nothing connects to it.
fn install() -> String {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    format!("https://127.0.0.1:{}", l.local_addr().unwrap().port())
}

/// A credential transport that must never be asked: the sign-in stays current.
struct NoApi;

impl HubTransport for NoApi {
    fn exchange(&self, call: HubCall) -> Result<HubReply, HubFailure> {
        panic!("an unexpected credential request: {:?}", call.destination)
    }
}

struct Clock;

impl HubClock for Clock {
    fn now_ms(&self) -> i64 {
        T0
    }
}

/// A fake hub that holds nothing and records what it is sent.
struct Wire {
    install: String,
    state: PublishState,
    steps: Mutex<Vec<&'static str>>,
    files: Mutex<BTreeMap<Hash, Vec<u8>>>,
    published: Mutex<Vec<(PublicationInputs, Manifest)>>,
}

impl Wire {
    fn step(&self, s: &'static str) {
        self.steps.lock().unwrap().push(s);
    }
}

impl PublishWire for Wire {
    fn discovery(&self, _cx: &UploadContext, _remote: &str) -> Result<Discovery, RdyError> {
        self.step("discovery");
        Ok(Discovery {
            sync_base: format!("{}/sync", self.install),
            protocols: vec![1],
        })
    }

    fn state(&self, _cx: &UploadContext, _r: &StateRequest) -> Result<PublishState, RdyError> {
        self.step("state");
        Ok(self.state.clone())
    }

    fn known(
        &self,
        _cx: &UploadContext,
        _push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.step("known");
        Ok(keys.to_vec())
    }

    fn put_objects(
        &self,
        _cx: &UploadContext,
        _push: &str,
        body: &mut dyn Read,
        _len: u64,
    ) -> Result<ObjectPut, RdyError> {
        self.step("objects");
        let mut b = Vec::new();
        body.read_to_end(&mut b).unwrap();
        Ok(ObjectPut {
            records: 1,
            bytes: b.len() as u64,
        })
    }

    fn known_files(
        &self,
        _cx: &UploadContext,
        _push: &str,
        files: &[FileIdentity],
    ) -> Result<Vec<Hash>, RdyError> {
        self.step("files_known");
        Ok(files.iter().map(|f| f.hash).collect())
    }

    fn put_file(
        &self,
        _cx: &UploadContext,
        _push: &str,
        file: &FileIdentity,
        _media_type: &str,
        body: &mut dyn Read,
    ) -> Result<FileIdentity, RdyError> {
        self.step("file");
        let mut b = Vec::new();
        body.read_to_end(&mut b).unwrap();
        self.files.lock().unwrap().insert(file.hash, b);
        Ok(*file)
    }

    fn publish(
        &self,
        _cx: &UploadContext,
        _push: &str,
        _site: &str,
        record: &PublicationInputs,
        manifest: &Manifest,
        _keep_latest: Option<bool>,
    ) -> Result<Candidate, RdyError> {
        self.step("publish");
        self.published
            .lock()
            .unwrap()
            .push((record.clone(), manifest.clone()));
        Ok(Candidate {
            id: "pubc_1".into(),
            state: "candidate".into(),
            url: format!("{}/app/publications/pubc_1", self.install),
        })
    }
}

/// The source: doc `a` (which references doc `b`) and doc `b`; profile `site` (org) selects
/// `a`, so its one reference is excluded and warns. Landed and tagged `v1.0.0`; canon then
/// moves on.
#[test]
fn hpb14_the_publication_renderer_uploads_the_tagged_site() {
    let home = tempfile::Builder::new()
        .permissions(pio::permissions(0o700))
        .tempdir()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let rdy = home.path().join("rdy");
    pio::dir_mode(&mut fs::DirBuilder::new(), 0o700)
        .create(&rdy)
        .unwrap();
    let user_file = rdy.join("config.yaml");
    fs::write(&user_file, "# person settings\n").unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$b", "body": "Text of b."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, b, admin) = (&w.created["$a"], &w.created["$b"], &w.created["$admin"]);
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{b}}}}}.")},
            {"op": "create", "type": "publication_profile", "id": "$site", "parent": admin,
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}
        ]),
    );
    let site = BlockId::parse(&w.created["$site"]).unwrap();
    land(&mut r);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        json!([{"op": "update", "id": a, "fields": {"title": "A, later"}}]),
    );
    land(&mut r);

    let origin = install();
    RemoteService::new(dir.path(), &user_file)
        .add("hub", &origin, RemoteMode::Publish, RemoteLevel::Project)
        .unwrap();
    let mut file = CredentialFile::default();
    file.set(
        Origin::parse(&origin).unwrap(),
        Some(Credential::Ready(ReadyCredential {
            access_token: Secret::new(ACCESS),
            refresh_token: Secret::new(REFRESH),
            expires_at_ms: T0 + 3_600_000,
            account: "acc_00000000000000000000000001".into(),
            handle: "test-person".into(),
        })),
    );
    let creds = rdy.join("credentials.yaml");
    let mut f = pio::file_mode(
        OpenOptions::new().write(true).create(true).truncate(true),
        0o600,
    )
    .open(&creds)
    .unwrap();
    f.write_all(&encode_credentials(&file).unwrap()).unwrap();
    drop(f);

    let read = r.read();
    let landing = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap()
        .2
        .landing;
    let scope = Scope::At(landing);
    let mut state = PublishState {
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
    };
    state.profile = Some(ProfileIdentity {
        id: site.clone(),
        version: Hash::of(b"the hub's site metadata"),
    });
    let wire = Wire {
        install: origin.clone(),
        state,
        steps: Mutex::new(vec![]),
        files: Mutex::new(BTreeMap::new()),
        published: Mutex::new(vec![]),
    };
    let renderer = PublicationRenderer;
    let result = PublishService::new(dir.path(), &user_file, &NoApi, &Clock, &wire)
        .run(
            r.read(),
            &Session::agent("agent:publisher".into(), Some("run-1".into()), Role::Writer).unwrap(),
            PublishRequest {
                tag: "1.0.0".into(),
                to: "hub".into(),
                site: "docs".into(),
                keep_latest: Some(true),
            },
            &renderer,
        )
        .unwrap();

    // the same build, made here from the tag's own inputs
    let inputs = capture_inputs(&read, &scope).unwrap();
    assert_eq!(renderer.capture(&read, &scope).unwrap(), inputs);
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&scope).unwrap();
    let redacted =
        tag_is_redacted(&mut ReadTagHistorySource::new(&read), landing, &inputs.pack).unwrap();
    assert!(!redacted, "guard");
    let want =
        build_publication(&source, &m, "v1.0.0", redacted, &site.to_string(), &inputs).unwrap();

    assert_eq!(result.state, "candidate");
    assert_eq!(result.record.tag, "v1.0.0", "the stored name");
    assert_eq!(result.record.manifest_hash, want.manifest_hash);
    assert_eq!(result.record.render_inputs, inputs);
    assert_eq!(
        result.render_warnings, want.warnings,
        "unchanged, in the build's order"
    );
    assert_eq!(
        result.render_warnings,
        [
            "Org profile has 1 references outside its selection. Review these references before you share the output."
        ],
        "the org profile's reference warning, first"
    );
    let published = wire.published.lock().unwrap().clone();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].1, want.manifest);
    // every distinct file went once, with the artifact's bytes
    let sent = wire.files.lock().unwrap().clone();
    let mut distinct: BTreeMap<Hash, Vec<u8>> = BTreeMap::new();
    for (path, bytes) in &want.files {
        distinct.insert(Hash::of(bytes), bytes.clone());
        assert!(want.manifest.files.iter().any(|e| &e.path == path));
    }
    assert_eq!(sent, distinct);
    // the site shows the tagged title, not canon's later one
    assert!(
        want.files
            .values()
            .all(|b| !String::from_utf8_lossy(b).contains("A, later"))
    );
    let steps = wire.steps.lock().unwrap().clone();
    assert_eq!(steps.first(), Some(&"discovery"));
    assert_eq!(steps.last(), Some(&"publish"));
}
