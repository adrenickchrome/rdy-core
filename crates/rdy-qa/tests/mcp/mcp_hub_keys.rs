//! hsa9 (D-XXA9D, D-1Z951, D-KQ4CA): the one complete client flow. Over MCP an agent signs
//! the person in to a hub (card, relayed answer, a state check that completes), adds a
//! remote, binds the person's and the agent's keys through the caller's SSH agent, returns
//! the hub's links, and retires and revokes keys. The same flow then runs over the CLI against
//! a fresh scripted hub, and every reply matches the chat reply. Each hub records exactly the
//! requests the flow needs: no confirmation request and no repeated mutation. The project's
//! store, its signing settings and the local actor do not change. The time a pending sign-in
//! may next be checked is moved by rewriting the saved schedule, never by waiting.
// the hub sign-in tests of this binary load the same fixture source as a module of their own
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-engine/tests/support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::hub_config::{Credential, encode_credentials, parse_credentials};
use rdy_engine::hub_types::Origin;
use rdy_engine::{Rdy, now_ms};
use rdy_model::Hash;
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::rdy_bin;
use rdy_qa::mcp::{McpClient, Reply as McpReply};
use rdy_qa::platform::{SocketAgent, set_mode};
use rdy_qa::signing::{QaAgent, TestKey};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const DEVICE_CODE: &str = "device-code-sentinel-7Hq2";
const USER_CODE: &str = "BCDFGHJK";
const MACHINE: &str = "laptop";
const ACCOUNT: &str = "acc_00000000000000000000000001";
const CHALLENGE_ID: &str = "kch_00000000000000000000000002";
const PERSON_KEY_ID: &str = "skey_00000000000000000000000003";
const AGENT_KEY_ID: &str = "skey_00000000000000000000000004";
const NONCE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const HUB_NOW: &str = "2026-10-18T09:00:00Z";
const COMPROMISED_INPUT: &str = "2026-10-17T13:00:00+02:00";
const COMPROMISED: &str = "2026-10-17T11:00:00Z";
const AGENT_LABEL: &str = "agent laptop";
const META: &str = "/.well-known/oauth-authorization-server/auth";

const METADATA_REPLY: &str = r#"{"issuer":"{o}/auth","authorization_endpoint":"{o}/auth/authorize","device_authorization_endpoint":"{o}/auth/device","token_endpoint":"{o}/auth/token","revocation_endpoint":"{o}/auth/revoke","registration_endpoint":"{o}/auth/register","scopes_supported":["pull","push","upload","change"],"response_types_supported":["code"],"grant_types_supported":["authorization_code","urn:ietf:params:oauth:grant-type:device_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]}"#;
const DEVICE_REPLY: &str = r#"{"device_code":"device-code-sentinel-7Hq2","user_code":"BCDFGHJK","verification_uri":"{o}/app/device","expires_in":600,"interval":5}"#;
const TOKEN_REPLY: &str = r#"{"access_token":"rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf","refresh_token":"rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC","token_type":"Bearer","expires_in":3600,"scope":"pull push upload change"}"#;
const ME_REPLY: &str = r#"{"id":"acc_00000000000000000000000001","handle":"test-person","earlier_handles":[{"handle":"test-person-old","until":"2026-09-01T08:00:00Z"}],"display_name":"Test Person","kind":"person","state":"active","primary_email":null,"two_factor":true,"sign_in_sources":["local"],"organizations":[{"id":"org_00000000000000000000000004","state":"active","name":"test-org-a","role":"owner"},{"id":"org_00000000000000000000000005","state":"active","name":"test-org-b","role":"member"}],"can_create_organization":true,"version":"test-version-1","created_at":"2026-08-01T08:00:00Z"}"#;

/// Every secret and proof part the flow makes: none may reach any output.
const HIDDEN: [&str; 7] = [
    ACCESS,
    REFRESH,
    DEVICE_CODE,
    CHALLENGE_ID,
    NONCE,
    "U1NIU0lH",
    "rdy-statement",
];

fn sub(template: &str, origin: &str) -> String {
    template.replace("{o}", origin)
}

fn card(install: &str) -> String {
    format!(
        "Sign in to a hub\nInstall: {install}\nMachine: {MACHINE}\nThis replaces this machine's saved sign-in for this install.\nConfirm only if you asked to sign in to this install.\nAnswer: confirm or cancel\n"
    )
}

fn fp(k: TestKey) -> String {
    QaAgent::fingerprint(k)
}

fn line(k: TestKey) -> String {
    ssh_key::PublicKey::from_bytes(&QaAgent::blob(k))
        .unwrap()
        .to_openssh()
        .unwrap()
        .trim()
        .to_string()
}

fn kind_of(k: TestKey) -> &'static str {
    match k {
        TestKey::Person => "person",
        TestKey::Agent => "agent",
    }
}

fn key_id(k: TestKey) -> &'static str {
    match k {
        TestKey::Person => PERSON_KEY_ID,
        TestKey::Agent => AGENT_KEY_ID,
    }
}

fn label_of(k: TestKey) -> &'static str {
    match k {
        TestKey::Person => "person key",
        TestKey::Agent => AGENT_LABEL,
    }
}

/// `ms` since the epoch as RFC3339 UTC, to the second.
fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

fn key_json(k: TestKey, state: &str, extra: &[(&str, &str)]) -> Value {
    let mut v = json!({"id": key_id(k), "account": ACCOUNT, "fingerprint": fp(k),
        "public_key": line(k), "label": label_of(k), "kind": kind_of(k), "state": state,
        "expires_at": null, "bound_at": null, "retired_at": null, "revoked_at": null,
        "compromised_from": null});
    for (name, value) in extra {
        v[*name] = json!(value);
    }
    v
}

fn confirm_url(origin: &str, k: TestKey) -> String {
    format!("{origin}/app/settings/keys/{}/confirm", key_id(k))
}

/// The form encoding of a query value.
fn form(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                char::from(b).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// One step of the flow on a hub: the replies it queues, then the routes it must record.
#[derive(Clone, Copy)]
enum Step {
    Card,
    Confirm,
    Complete,
    AddRemote,
    Bind(TestKey),
    Retire(TestKey),
    /// A revoke, with the compromise time given (non-UTC) or not.
    Revoke(TestKey, bool),
}

const FLOW: [Step; 9] = [
    Step::Card,
    Step::Confirm,
    Step::Complete,
    Step::AddRemote,
    Step::Bind(TestKey::Person),
    Step::Bind(TestKey::Agent),
    Step::Retire(TestKey::Person),
    Step::Revoke(TestKey::Agent, false),
    Step::Revoke(TestKey::Person, true),
];

fn route(m: &str, p: &str) -> (String, String) {
    (m.to_string(), p.to_string())
}

impl Step {
    /// Queues this step's replies on `hub`.
    fn queue(self, hub: &mut Server) {
        let o = hub.origin().to_string();
        let replies: Vec<Reply> = match self {
            Step::Card | Step::AddRemote => vec![],
            Step::Confirm => vec![
                Reply::json(200, &sub(METADATA_REPLY, &o)),
                Reply::json(200, &sub(DEVICE_REPLY, &o)),
            ],
            Step::Complete => vec![
                Reply::json(200, &sub(METADATA_REPLY, &o)),
                Reply::json(200, TOKEN_REPLY),
                Reply::json(200, ME_REPLY),
            ],
            Step::Bind(k) => {
                let challenge = json!({"id": CHALLENGE_ID, "account": ACCOUNT,
                    "kind": kind_of(k), "label": label_of(k), "namespace": "rdy-keybind-v1",
                    "nonce": NONCE, "expires_at": rfc3339(now_ms() + 300_000)});
                let bound = json!({"key": key_json(k, "pending", &[]),
                    "confirm_url": confirm_url(&o, k)});
                vec![
                    Reply::json(201, &challenge.to_string()),
                    Reply::json(201, &bound.to_string()),
                ]
            }
            Step::Retire(k) | Step::Revoke(k, _) => {
                let bound = [("bound_at", "2026-10-17T12:05:00Z")];
                let list = json!({"items": [key_json(k, "active", &bound)], "next": null});
                let changed = match self {
                    Step::Retire(_) => key_json(
                        k,
                        "retired",
                        &[
                            ("bound_at", "2026-10-17T12:05:00Z"),
                            ("retired_at", HUB_NOW),
                        ],
                    ),
                    _ => {
                        let given = matches!(self, Step::Revoke(_, true));
                        key_json(
                            k,
                            "revoked",
                            &[
                                ("bound_at", "2026-10-17T12:05:00Z"),
                                ("revoked_at", HUB_NOW),
                                (
                                    "compromised_from",
                                    if given { COMPROMISED } else { HUB_NOW },
                                ),
                            ],
                        )
                    }
                };
                vec![
                    Reply::json(200, &list.to_string()),
                    Reply::json(200, &changed.to_string()),
                ]
            }
        };
        let n = replies.len();
        for r in replies {
            hub.reply(r);
        }
        if n > 0 {
            hub.accept_n(n);
        }
    }

    /// The `(method, path)` and body of each request this step sends.
    fn requests(self) -> Vec<((String, String), Option<String>)> {
        match self {
            Step::Card | Step::AddRemote => vec![],
            Step::Confirm => vec![
                (route("GET", META), None),
                (route("POST", "/auth/device"), None),
            ],
            Step::Complete => vec![
                (route("GET", META), None),
                (route("POST", "/auth/token"), None),
                (route("GET", "/api/v1/me"), None),
            ],
            Step::Bind(k) => {
                let challenge = if k == TestKey::Agent {
                    json!({"kind": "agent", "label": AGENT_LABEL})
                } else {
                    json!({"kind": "person"})
                };
                vec![
                    (
                        route("POST", "/api/v1/me/keys/challenges"),
                        Some(challenge.to_string()),
                    ),
                    (route("POST", "/api/v1/me/keys"), None),
                ]
            }
            Step::Retire(k) | Step::Revoke(k, _) => {
                let find = format!("/api/v1/me/keys?fingerprint={}", form(&fp(k)));
                let (act, body) = match self {
                    Step::Retire(_) => ("retire", Some(String::new())),
                    Step::Revoke(_, true) => (
                        "revoke",
                        Some(format!("{{\"compromised_from\":\"{COMPROMISED}\"}}")),
                    ),
                    _ => ("revoke", Some(String::new())),
                };
                vec![
                    (route("GET", &find), Some(String::new())),
                    (
                        route("POST", &format!("/api/v1/me/keys/{}/{act}", key_id(k))),
                        body,
                    ),
                ]
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// One side of the flow: a hub, a user config home, a project and the caller's agent
// ---------------------------------------------------------------------------------------------

struct Side {
    /// Taken by [`Side::finish`].
    hub: Option<Server>,
    o: String,
    home: tempfile::TempDir,
    proj: tempfile::TempDir,
    agent: SocketAgent,
}

impl Side {
    fn new() -> Side {
        let hub = Server::start();
        let o = hub.origin().to_string();
        let home = tempfile::tempdir().unwrap();
        set_mode(home.path(), 0o700).unwrap();
        std::fs::create_dir(home.path().join("rdy")).unwrap();
        set_mode(&home.path().join("rdy"), 0o700).unwrap();
        let pem = home.path().join("ca.pem");
        std::fs::write(&pem, hub.ca_pem()).unwrap();
        std::fs::write(
            home.path().join("rdy/config.yaml"),
            format!(
                "actor: human:qa\nsigning:\n  key: \"{}\"\n  agent_key: \"{}\"\n  sign: never\nhub:\n  installs:\n    \"{o}\":\n      ca_file: {}\n",
                fp(TestKey::Person),
                fp(TestKey::Agent),
                pem.display()
            ),
        )
        .unwrap();
        let proj = short_tempdir();
        drop(Rdy::init(proj.path(), "rdy-standard").unwrap());
        let cfg = proj.path().join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str("serve:\n  idle_timeout: 60\n");
        std::fs::write(&cfg, text).unwrap();
        std::fs::write(proj.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
        Side {
            hub: Some(hub),
            o,
            home,
            proj,
            agent: SocketAgent::start(Arc::new(QaAgent::new())).unwrap(),
        }
    }

    fn creds(&self) -> PathBuf {
        self.home.path().join("rdy/credentials.yaml")
    }

    /// Makes the pending sign-in due for its next check now.
    fn make_due(&self) {
        let mut file = parse_credentials(&std::fs::read(self.creds()).unwrap()).unwrap();
        let o = Origin::parse(&self.o).unwrap();
        let Some(Credential::Pending(mut p)) = file.get(&o).cloned() else {
            panic!("no pending sign-in for {}", self.o);
        };
        p.next_poll_at_ms = 0;
        file.set(o, Some(Credential::Pending(p)));
        std::fs::write(self.creds(), encode_credentials(&file).unwrap()).unwrap();
        set_mode(&self.creds(), 0o600).unwrap();
    }

    /// What must stay the same: the store's history files (its log, which holds its refs, and
    /// its format), the local config with its actor, and the user config with the signing
    /// settings.
    fn fixed(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        let store = self.proj.path().join(".rdy/store");
        let mut walk = vec![store.clone()];
        while let Some(dir) = walk.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk.push(p);
                } else if !runtime(&p) {
                    let name = p.strip_prefix(&store).unwrap().display().to_string();
                    out.insert(name, std::fs::read(&p).unwrap());
                }
            }
        }
        for f in [
            self.proj.path().join(".rdy/local.yaml"),
            self.home.path().join("rdy/config.yaml"),
        ] {
            out.insert(f.display().to_string(), std::fs::read(&f).unwrap());
        }
        out
    }

    fn hub(&mut self) -> &mut Server {
        self.hub.as_mut().expect("the hub")
    }

    /// The requests this side's hub read.
    fn requests(&self) -> Vec<Recorded> {
        self.hub.as_ref().expect("the hub").requests()
    }

    /// Joins the hub's threads and proves that no further request waits; the unused replies.
    fn finish(&mut self) -> usize {
        self.hub.take().expect("the hub").finish()
    }
}

/// A store file that is not history: the index (rebuilt from the log), its open marker, and
/// the store lock.
fn runtime(p: &std::path::Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    name.starts_with("index.") || name == "lock"
}

impl Drop for Side {
    fn drop(&mut self) {
        let _ = Command::new(rdy_bin())
            .env_clear()
            .arg("--project")
            .arg(self.proj.path())
            .args(["serve", "--stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `rdy --json --project <p> <args>` as the person of `side`, with nothing else of this process.
fn cli(side: &Side, args: &[&str]) -> Value {
    let home = side.home.path().to_string_lossy().into_owned();
    let o: Output = Command::new(rdy_bin())
        .env_clear()
        .env("XDG_CONFIG_HOME", &home)
        .env("HOME", &home)
        .env("USER", "qa")
        .env("SSH_AUTH_SOCK", side.agent.socket_path())
        .arg("--json")
        .arg("--project")
        .arg(side.proj.path())
        .args(args)
        .current_dir(side.proj.path())
        .stdin(Stdio::null())
        .output()
        .expect("run rdy");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    for s in HIDDEN {
        assert!(!all.contains(s), "rdy {args:?} printed {s}:\n{all}");
    }
    assert!(o.status.success(), "rdy {args:?}: {all}");
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {all}"))
}

fn tool(c: &mut McpClient, name: &str, args: Value) -> Value {
    let r = c.call(name, args.clone());
    let text = format!("{r:?}");
    for s in HIDDEN {
        assert!(!text.contains(s), "{name} {args}: {s} in {text}");
    }
    match r {
        McpReply::Ok(v) => v,
        other => panic!("{name} {args}: {other:?}"),
    }
}

/// The type of each value: keys, nullability and integer types, before any normalization.
fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!("integer"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(a) => Value::Array(a.iter().map(shape).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
    }
}

/// `v` with only the fixture's own parts made common: its generated origin, a card hash
/// checked against its own card's bytes, and the clock fields.
fn normal(v: &Value, origin: &str) -> Value {
    match v {
        Value::String(s) => Value::String(s.replace(origin, "{o}")),
        Value::Array(a) => Value::Array(a.iter().map(|x| normal(x, origin)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let x = match (k.as_str(), x) {
                        ("expires_at_ms" | "next_poll_at_ms", Value::Number(_)) => json!("{ms}"),
                        ("card_hash", Value::String(h)) => {
                            assert_eq!(
                                *h,
                                Hash::of(card(origin).as_bytes()).to_string(),
                                "the hash of this fixture's card"
                            );
                            json!("{card_hash}")
                        }
                        _ => normal(x, origin),
                    };
                    (k.clone(), x)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Checks a side's recorded requests: exactly the flow's, with no confirmation request, each
/// key request carrying `actor` and the side's bearer.
fn check_requests(side: &Side, actor: &str, run: Option<&str>) {
    let reqs = side.requests();
    let want: Vec<_> = FLOW.iter().flat_map(|s| s.requests()).collect();
    let got: Vec<(String, String)> = reqs
        .iter()
        .map(|r| (r.method.clone(), r.target.clone()))
        .collect();
    let routes: Vec<(String, String)> = want.iter().map(|(r, _)| r.clone()).collect();
    assert_eq!(got, routes, "exactly the flow's requests, each sent once");
    for (r, (_, body)) in reqs.iter().zip(&want) {
        assert!(!r.target.contains("confirm"), "a confirmation request");
        if let Some(b) = body {
            assert_eq!(r.body_text(), b, "{}", r.target);
        }
        if r.target.starts_with("/api/v1/me/keys") {
            assert_eq!(r.header("rdy-actor"), Some(actor), "{}", r.target);
            if let Some(run) = run {
                assert_eq!(r.header("rdy-run"), Some(run), "{}", r.target);
            }
            assert_eq!(
                r.header("authorization"),
                Some(&*format!("Bearer {ACCESS}"))
            );
        }
    }
    // the proof sent is SSHSIG under the keybind namespace, from the bound key
    for r in reqs.iter().filter(|r| r.target == "/api/v1/me/keys") {
        let body: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(body["challenge"], CHALLENGE_ID);
        assert!(body["proof"].as_str().unwrap().starts_with("U1NIU0lH"));
        let sent = body["public_key"].as_str().unwrap();
        assert!(sent == line(TestKey::Person) || sent == line(TestKey::Agent));
    }
}

#[test]
fn hsa9_signin_remote_bind_retire_revoke_is_one_client_flow() {
    // ---- over MCP: an agent session, relaying the person ----
    let mut chat_side = Side::new();
    let o = chat_side.o.clone();
    let h = chat_side.home.path().to_string_lossy().into_owned();
    let sock = chat_side.agent.socket_path().to_string_lossy().into_owned();
    let mut c = McpClient::start_with_env(
        chat_side.proj.path(),
        &[
            ("XDG_CONFIG_HOME", h.as_str()),
            ("HOME", h.as_str()),
            ("USER", "qa"),
            ("SSH_AUTH_SOCK", sock.as_str()),
        ],
    );
    tool(
        &mut c,
        "rdy_session",
        json!({"actor": "agent:claude", "run": "r7", "role": "writer"}),
    );
    let key_show = tool(&mut c, "rdy_key_show", json!({}));
    let before = chat_side.fixed();
    let mut chat = Vec::new();
    let mut hash = String::new();
    for step in FLOW {
        step.queue(chat_side.hub());
        if let Step::Complete = step {
            chat_side.make_due();
        }
        let v = match step {
            Step::Card => tool(
                &mut c,
                "rdy_auth",
                json!({"op": "start", "install": o, "machine_name": MACHINE}),
            ),
            Step::Confirm => tool(
                &mut c,
                "rdy_auth",
                json!({"op": "start", "install": o, "machine_name": MACHINE,
                    "card_hash": hash, "answer": "confirm",
                    "evidence": "yes, sign me in", "question": "Sign in to this hub?"}),
            ),
            Step::Complete => tool(&mut c, "rdy_auth", json!({"op": "state", "install": o})),
            Step::AddRemote => tool(
                &mut c,
                "rdy_remote",
                json!({"op": "add", "name": "origin", "url": format!("{o}/team/p"),
                    "mode": "share"}),
            ),
            Step::Bind(TestKey::Person) => tool(&mut c, "rdy_key_bind", json!({"kind": "person"})),
            Step::Bind(TestKey::Agent) => tool(
                &mut c,
                "rdy_key_bind",
                json!({"kind": "agent", "label": AGENT_LABEL, "remote": "origin"}),
            ),
            Step::Retire(k) => tool(&mut c, "rdy_key_retire", json!({"key": fp(k)})),
            Step::Revoke(k, false) => tool(&mut c, "rdy_key_revoke", json!({"key": line(k)})),
            Step::Revoke(k, true) => tool(
                &mut c,
                "rdy_key_revoke",
                json!({"key": fp(k), "compromised_since": COMPROMISED_INPUT}),
            ),
        };
        if let Step::Card = step {
            hash = v["card_hash"].as_str().unwrap().to_string();
            assert_eq!(hash, Hash::of(card(&o).as_bytes()).to_string());
        }
        chat.push(v);
    }
    assert_eq!(
        tool(&mut c, "rdy_key_show", json!({})),
        key_show,
        "signing settings unchanged"
    );
    assert_eq!(chat_side.fixed(), before, "store and settings unchanged");
    drop(c);

    // the replies the flow promises
    assert_eq!(chat[0]["state"], "confirmation_required");
    assert_eq!(chat[0]["card"], card(&o));
    assert_eq!(chat[1]["state"], "pending");
    assert_eq!(chat[1]["user_code"], USER_CODE);
    assert_eq!(chat[1]["verification_uri"], format!("{o}/app/device"));
    assert_eq!(chat[2]["state"], "signed_in");
    assert_eq!(chat[2]["human_actor"], "human:test-person");
    let creds = std::fs::read_to_string(chat_side.creds()).unwrap();
    let saved = parse_credentials(creds.as_bytes()).unwrap();
    assert_eq!(saved.len(), 1, "one ready credential");
    assert!(matches!(
        saved.get(&Origin::parse(&o).unwrap()),
        Some(Credential::Ready(_))
    ));
    assert_eq!(
        chat[3],
        json!({"remote": {"name": "origin", "url": format!("{o}/team/p"), "mode": "share",
            "level": "project", "install": o}, "removed": false, "warnings": []})
    );
    for (i, k) in [(4, TestKey::Person), (5, TestKey::Agent)] {
        assert_eq!(
            chat[i],
            json!({"key": fp(k), "kind": kind_of(k), "state": "pending",
                "confirm_url": confirm_url(&o, k), "remote": "origin", "warnings": []}),
            "a bind returns the hub's link and waits for the person there"
        );
    }
    assert_eq!(
        chat[6],
        json!({"key": fp(TestKey::Person), "state": "retired", "remote": "origin",
            "warnings": []})
    );
    assert_eq!(
        chat[7],
        json!({"key": fp(TestKey::Agent), "state": "revoked", "revoked_at": HUB_NOW,
            "compromised_from": HUB_NOW, "remote": "origin", "warnings": []}),
        "the hub chose the time"
    );
    assert_eq!(
        chat[8],
        json!({"key": fp(TestKey::Person), "state": "revoked", "revoked_at": HUB_NOW,
            "compromised_from": COMPROMISED, "remote": "origin", "warnings": []}),
        "the given time reached the hub as UTC"
    );
    check_requests(&chat_side, "agent:claude", Some("r7"));
    let signs = chat_side.agent.requests();
    assert!(signs >= 2, "the caller's agent was asked: {signs}");

    // ---- over the CLI, as the person, against a fresh scripted hub ----
    let mut cli_side = Side::new();
    let p = cli_side.o.clone();
    let before = cli_side.fixed();
    let shown = cli(&cli_side, &["key", "show"]);
    let mut out = Vec::new();
    let mut hash = String::new();
    for step in FLOW {
        step.queue(cli_side.hub());
        if let Step::Complete = step {
            cli_side.make_due();
        }
        let signin = [
            "auth",
            "sign-in",
            "--install",
            &p,
            "--machine-name",
            MACHINE,
        ];
        let v = match step {
            Step::Card => cli(&cli_side, &signin),
            Step::Confirm => {
                let mut a = signin.to_vec();
                a.extend(["--card-hash", &hash, "--answer", "confirm"]);
                cli(&cli_side, &a)
            }
            Step::Complete => cli(&cli_side, &["auth", "state", "--install", &p]),
            Step::AddRemote => cli(
                &cli_side,
                &[
                    "remote",
                    "add",
                    "origin",
                    &format!("{p}/team/p"),
                    "--mode",
                    "share",
                ],
            ),
            Step::Bind(TestKey::Person) => cli(&cli_side, &["key", "bind", "--kind", "person"]),
            Step::Bind(TestKey::Agent) => cli(
                &cli_side,
                &[
                    "key",
                    "bind",
                    "--kind",
                    "agent",
                    "--label",
                    AGENT_LABEL,
                    "--remote",
                    "origin",
                ],
            ),
            Step::Retire(k) => cli(&cli_side, &["key", "retire", &fp(k)]),
            Step::Revoke(k, false) => cli(&cli_side, &["key", "revoke", &line(k)]),
            Step::Revoke(k, true) => cli(
                &cli_side,
                &[
                    "key",
                    "revoke",
                    &fp(k),
                    "--compromised-since",
                    COMPROMISED_INPUT,
                ],
            ),
        };
        if let Step::Card = step {
            hash = v["card_hash"].as_str().unwrap().to_string();
        }
        out.push(v);
    }
    assert_eq!(cli(&cli_side, &["key", "show"]), shown);
    assert_eq!(cli_side.fixed(), before, "store and settings unchanged");
    check_requests(&cli_side, "human:test-person", None);

    // every reply is the chat reply: the same keys, nulls and integers, then the same values
    // once each fixture's origin, card hash and clock fields are made common
    for (i, (a, b)) in chat.iter().zip(&out).enumerate() {
        assert_eq!(shape(a), shape(b), "step {i}");
        assert_eq!(normal(a, &o), normal(b, &p), "step {i}");
    }
    assert_eq!(chat_side.finish(), 0);
    assert_eq!(cli_side.finish(), 0);
}
