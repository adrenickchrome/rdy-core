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
