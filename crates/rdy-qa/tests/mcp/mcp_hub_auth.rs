//! hsa8 (D-KQ4CA, n-XZDQD, n-N055T): `rdy_auth`, hub sign-in from chat. The card comes first;
//! the agent sends back its hash, the person's answer and their words, and the relay table
//! checks the actual session. Requests go through the real HTTPS transport to a loopback TLS
//! hub, inside the tool's blocking run. Each server gets a private user config home. The time a
//! pending sign-in may next be checked is moved by rewriting the saved schedule, never by
//! waiting; a held request is held by a channel.
#[path = "../../../rdy-engine/tests/support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::hub_config::{
    Credential, CredentialFile, ReadyCredential, encode_credentials, parse_credentials,
};
use rdy_engine::hub_types::{Origin, Secret};
use rdy_model::Hash;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::project;
use rdy_qa::mcp::{McpClient, Reply as McpReply};
use rdy_qa::platform::set_mode;
use serde_json::{Value, json};
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const DEVICE_CODE: &str = "device-code-sentinel-7Hq2";
const USER_CODE: &str = "BCDFGHJK";
const MACHINE: &str = "laptop";
const META: &str = "/.well-known/oauth-authorization-server/auth";

const METADATA_REPLY: &str = r#"{"issuer":"{o}/auth","authorization_endpoint":"{o}/auth/authorize","device_authorization_endpoint":"{o}/auth/device","token_endpoint":"{o}/auth/token","revocation_endpoint":"{o}/auth/revoke","registration_endpoint":"{o}/auth/register","scopes_supported":["pull","push","upload","change"],"response_types_supported":["code"],"grant_types_supported":["authorization_code","urn:ietf:params:oauth:grant-type:device_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]}"#;
const DEVICE_REPLY: &str = r#"{"device_code":"device-code-sentinel-7Hq2","user_code":"BCDFGHJK","verification_uri":"{o}/app/device","expires_in":600,"interval":5}"#;
const TOKEN_REPLY: &str = r#"{"access_token":"rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf","refresh_token":"rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC","token_type":"Bearer","expires_in":3600,"scope":"pull push upload change"}"#;
const ME_REPLY: &str = r#"{"id":"acc_00000000000000000000000001","handle":"test-person","earlier_handles":[{"handle":"test-person-old","until":"2026-09-01T08:00:00Z"}],"display_name":"Test Person","kind":"person","state":"active","primary_email":null,"two_factor":true,"sign_in_sources":["local"],"organizations":[{"id":"org_00000000000000000000000004","state":"active","name":"test-org-a","role":"owner"},{"id":"org_00000000000000000000000005","state":"active","name":"test-org-b","role":"member"}],"can_create_organization":true,"version":"test-version-1","created_at":"2026-08-01T08:00:00Z"}"#;

const SECRETS: [&str; 3] = [ACCESS, REFRESH, DEVICE_CODE];

fn sub(template: &str, origin: &str) -> String {
    template.replace("{o}", origin)
}

fn card(install: &str, machine: &str) -> String {
    format!(
        "Sign in to a hub\nInstall: {install}\nMachine: {machine}\nThis replaces this machine's saved sign-in for this install.\nConfirm only if you asked to sign in to this install.\nAnswer: confirm or cancel\n"
    )
}

/// Makes `dir` and its `rdy/` 0700, writes the person into `rdy/config.yaml` unless it is
/// there, and trusts each `(origin, CA PEM)` in it.
fn configure(dir: &Path, trust: &[(&str, &str)]) {
    set_mode(dir, 0o700).unwrap();
    std::fs::create_dir_all(dir.join("rdy")).unwrap();
    set_mode(&dir.join("rdy"), 0o700).unwrap();
    let mut text = "actor: human:qa\nhub:\n  installs:\n".to_string();
    for (i, (origin, pem)) in trust.iter().enumerate() {
        let file = dir.join(format!("ca-{i}.pem"));
        std::fs::write(&file, pem).unwrap();
        text.push_str(&format!(
            "    \"{origin}\":\n      ca_file: {}\n",
            file.display()
        ));
    }
    std::fs::write(dir.join("rdy/config.yaml"), text).unwrap();
}

fn creds_of(dir: &Path) -> PathBuf {
    dir.join("rdy/credentials.yaml")
}

fn read_creds(dir: &Path) -> CredentialFile {
    parse_credentials(&std::fs::read(creds_of(dir)).unwrap()).unwrap()
}

fn write_creds(dir: &Path, file: &CredentialFile) {
    std::fs::write(creds_of(dir), encode_credentials(file).unwrap()).unwrap();
    set_mode(&creds_of(dir), 0o600).unwrap();
}

/// Makes the pending sign-in of `install` due for its next check now.
fn make_due(dir: &Path, install: &str) {
    let mut file = read_creds(dir);
    let o = Origin::parse(install).unwrap();
    let Some(Credential::Pending(mut p)) = file.get(&o).cloned() else {
        panic!("no pending sign-in for {install}");
    };
    p.next_poll_at_ms = 0;
    file.set(o, Some(Credential::Pending(p)));
    write_creds(dir, &file);
}

/// A stdio MCP server on a new project, whose user config home is `home` (private).
fn chat(home: &Path) -> (tempfile::TempDir, McpClient) {
    let (dir, r) = project();
    drop(r);
    let h = home.to_string_lossy().into_owned();
    let c = McpClient::start_with_env(
        dir.path(),
        &[
            ("XDG_CONFIG_HOME", h.as_str()),
            ("HOME", h.as_str()),
            ("USER", "qa"),
        ],
    );
    (dir, c)
}

fn session(c: &mut McpClient, actor: &str, run: Option<&str>) {
    let r = c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
}

/// The reply of `rdy_auth`, after checking that it holds no secret or complete link.
fn auth(c: &mut McpClient, args: Value) -> McpReply {
    let r = c.call("rdy_auth", args.clone());
    let text = format!("{r:?}");
    for s in SECRETS {
        assert!(!text.contains(s), "{args}: a secret in {text}");
    }
    assert!(!text.contains("user_code="), "{text}");
    assert!(!text.contains("verification_uri_complete"), "{text}");
    r
}

fn ok(r: McpReply) -> Value {
    match r {
        McpReply::Ok(v) => v,
        other => panic!("expected a reply, got {other:?}"),
    }
}

fn refused(r: McpReply) -> (String, String) {
    match r {
        McpReply::ToolError(v) => (
            v["code"].as_str().unwrap_or("").to_string(),
            v["message"].as_str().unwrap_or("").to_string(),
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn routes(reqs: &[Recorded]) -> Vec<(String, String)> {
    reqs.iter()
        .map(|r| (r.method.clone(), r.target.clone()))
        .collect()
}

fn route(m: &str, p: &str) -> (String, String) {
    (m.to_string(), p.to_string())
}

fn queue_start(hub: &mut Server) {
    let o = hub.origin().to_string();
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(200, &sub(DEVICE_REPLY, &o)));
    hub.accept_n(2);
}

#[test]
fn hsa8_chat_signin_requires_the_shown_card_and_persons_evidence() {
    let home = tempfile::tempdir().unwrap();
    let mut hub = Server::start();
    let o = hub.origin().to_string();
    configure(home.path(), &[(&o, hub.ca_pem())]);
    let (_p, mut c) = chat(home.path());
    // a tool that needs a session
    let (code, _) = refused(auth(
        &mut c,
        json!({"op": "start", "install": o, "machine_name": MACHINE}),
    ));
    assert_eq!(code, "E_NO_SESSION");
    session(&mut c, "agent:claude", Some("r7"));

    // the card: exact text, its hash over its exact bytes, nothing else set
    let v = ok(auth(
        &mut c,
        json!({"op": "start", "install": format!("{o}/"), "machine_name": MACHINE}),
    ));
    let hash = Hash::of(card(&o, MACHINE).as_bytes()).to_string();
    assert_eq!(
        v,
        json!({"install": o, "state": "confirmation_required", "card": card(&o, MACHINE),
            "card_hash": hash, "user_code": null, "verification_uri": null,
            "expires_at_ms": null, "next_poll_at_ms": null, "account": null,
            "human_actor": null, "warnings": []})
    );
    assert_eq!(hash.len(), 64);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let answer = |hash: &str, machine: &str, evidence: Option<&str>| {
        let mut a = json!({"op": "start", "install": o, "machine_name": machine,
            "card_hash": hash, "answer": "confirm"});
        if let Some(e) = evidence {
            a["evidence"] = json!(e);
            a["question"] = json!("Sign in to this hub?");
        }
        a
    };
    // a wrong or altered hash, a changed machine or install: not the card that was shown
    let other = Hash::of(card(&o, "desk").as_bytes()).to_string();
    for args in [
        answer(&"0".repeat(64), MACHINE, Some("yes")),
        answer(&hash.to_uppercase(), MACHINE, Some("yes")),
        answer(&hash[..63], MACHINE, Some("yes")),
        answer(&other, MACHINE, Some("yes")),
        answer(&hash, "desk", Some("yes")),
        {
            let mut a = answer(&hash, MACHINE, Some("yes"));
            a["install"] = json!("https://127.0.0.1:9");
            a
        },
    ] {
        assert_eq!(
            refused(auth(&mut c, args.clone())),
            (
                "E_UPSTREAM_CONFIRM_HOST".into(),
                "the install host has not been confirmed".into()
            ),
            "{args}"
        );
    }
    // the right card without the person's words
    assert_eq!(
        refused(auth(&mut c, answer(&hash, MACHINE, None))),
        (
            "E_PERMISSION".into(),
            "an agent answers only by relaying a person's words".into()
        )
    );
    assert_eq!(
        refused(auth(&mut c, answer(&hash, MACHINE, Some("   ")))),
        (
            "E_PERMISSION".into(),
            "an agent answers only by relaying a person's words".into()
        ),
        "blank words are no evidence"
    );
    assert!(
        !creds_of(home.path()).exists(),
        "no refusal wrote a credential"
    );
    assert!(hub.requests().is_empty(), "no refusal sent a request");

    // the shown card and the person's words: the sign-in starts once
    queue_start(&mut hub);
    let v = ok(auth(
        &mut c,
        answer(&hash, MACHINE, Some("yes, sign me in")),
    ));
    assert_eq!(v["state"], "pending");
    assert_eq!(v["user_code"], USER_CODE);
    assert_eq!(
        routes(&hub.requests()),
        [route("GET", META), route("POST", "/auth/device")]
    );
    let start = &hub.requests()[1];
    assert!(start.body_text().contains("machine_name=laptop"));
    assert!(start.header("rdy-actor").is_none() && start.header("authorization").is_none());
    // answering the same card again returns the pending sign-in, with no new request
    let again = ok(auth(
        &mut c,
        answer(&hash, MACHINE, Some("yes, sign me in")),
    ));
    assert_eq!(again, v);
    assert_eq!(hub.requests().len(), 2);
    assert_eq!(hub.finish(), 0);
}

#[test]
fn hsa8_chat_signin_returns_a_code_and_code_free_link() {
    let home = tempfile::tempdir().unwrap();
    let mut hub = Server::start();
    let o = hub.origin().to_string();
    configure(home.path(), &[(&o, hub.ca_pem())]);
    let (_p, mut c) = chat(home.path());
    session(&mut c, "agent:claude", Some("r7"));
    let hash = Hash::of(card(&o, MACHINE).as_bytes()).to_string();
    queue_start(&mut hub);
    let pending = ok(auth(
        &mut c,
        json!({"op": "start", "install": o, "machine_name": MACHINE, "card_hash": hash,
            "answer": "confirm", "evidence": "yes", "session_ref": "chat-1"}),
    ));
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["user_code"], USER_CODE);
    assert_eq!(
        pending["verification_uri"],
        json!(format!("{o}/app/device"))
    );
    for k in ["card", "card_hash", "account", "human_actor"] {
        assert_eq!(pending[k], Value::Null, "{k}");
    }
    let next = pending["next_poll_at_ms"].as_i64().unwrap();
    let end = pending["expires_at_ms"].as_i64().unwrap();
    assert!(next < end, "{pending}");
    // before the next check is due: the same reply, no request, no wait
    assert_eq!(
        ok(auth(&mut c, json!({"op": "state", "install": o}))),
        pending
    );
    assert_eq!(hub.requests().len(), 2);

    // due, and the person has not entered the code yet: one poll, still pending
    make_due(home.path(), &o);
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(400, r#"{"error":"authorization_pending"}"#));
    hub.accept_n(2);
    let still = ok(auth(&mut c, json!({"op": "state", "install": o})));
    assert_eq!(still["state"], "pending");
    assert_eq!(still["user_code"], USER_CODE);
    assert!(still["next_poll_at_ms"].as_i64().unwrap() > 0);
    // due again, and the person has: one poll, then `/me`, then signed in
    make_due(home.path(), &o);
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(200, TOKEN_REPLY));
    hub.reply(Reply::json(200, ME_REPLY));
    hub.accept_n(3);
    let done = ok(auth(&mut c, json!({"op": "state", "install": o})));
    assert_eq!(done["state"], "signed_in");
    assert_eq!(done["human_actor"], "human:test-person");
    assert_eq!(done["account"], "acc_00000000000000000000000001");
    for k in [
        "card",
        "card_hash",
        "user_code",
        "verification_uri",
        "next_poll_at_ms",
    ] {
        assert_eq!(done[k], Value::Null, "{k}");
    }
    let reqs = hub.requests();
    assert_eq!(
        routes(&reqs),
        [
            route("GET", META),
            route("POST", "/auth/device"),
            route("GET", META),
            route("POST", "/auth/token"),
            route("GET", META),
            route("POST", "/auth/token"),
            route("GET", "/api/v1/me"),
        ]
    );
    assert!(
        reqs[3].body_text().contains(DEVICE_CODE),
        "the poll sends the device code"
    );
    assert_eq!(
        reqs[6].header("authorization"),
        Some(format!("Bearer {ACCESS}").as_str())
    );
    // a later state is local: signed in, no request
    assert_eq!(
        ok(auth(&mut c, json!({"op": "state", "install": o})))["state"],
        "signed_in"
    );
    assert_eq!(hub.finish(), 0);
    // the tokens are only in the private credential file
    let saved = read_creds(home.path());
    assert!(matches!(
        saved.get(&Origin::parse(&o).unwrap()),
        Some(Credential::Ready(r)) if r.access_token.expose() == ACCESS
    ));
}

#[test]
fn hsa8_auth_claims_belong_to_each_session() {
    let home = tempfile::tempdir().unwrap();
    let mut hub = Server::start();
    let o = hub.origin().to_string();
    configure(home.path(), &[(&o, hub.ca_pem())]);
    let (_p, mut c) = chat(home.path());
    let hash = Hash::of(card(&o, MACHINE).as_bytes()).to_string();
    let cancel = |evidence: Option<&str>| {
        let mut a = json!({"op": "start", "install": o, "machine_name": MACHINE,
            "card_hash": hash, "answer": "cancel"});
        if let Some(e) = evidence {
            a["evidence"] = json!(e);
        }
        a
    };
    let no_words = (
        "E_PERMISSION".to_string(),
        "an agent answers only by relaying a person's words".to_string(),
    );
    // a human actor cannot open an MCP session (D-ME2C0): MCP is an agent surface, so a
    // session named after the person is refused, and the person's words come only by relay
    let r = c.call(
        "rdy_session",
        json!({"actor": "human:qa", "run": null, "role": "writer"}),
    );
    assert_eq!(
        refused(r),
        (
            "E_PERMISSION".into(),
            "an agent session cannot claim a human actor".into()
        )
    );
    // each call is checked as the session that makes it
    for (actor, run) in [
        ("agent:one", Some("r1")),
        ("agent:two", Some("r2")),
        ("agent:one", None),
    ] {
        session(&mut c, actor, run);
        assert_eq!(refused(auth(&mut c, cancel(None))), no_words, "{actor}");
        assert_eq!(
            ok(auth(&mut c, cancel(Some("no, not now"))))["state"],
            "cancelled",
            "{actor}"
        );
        // state claims the caller's own actor
        let r = auth(&mut c, json!({"op": "state", "install": o}));
        assert_eq!(ok(r)["state"], "signed_out", "{actor}");
    }
    // an actor outside rdy's actor form cannot open a session, so it never reaches a hub claim
    let r = c.call(
        "rdy_session",
        json!({"actor": "qa-bot", "run": "r3", "role": "writer"}),
    );
    assert_eq!(
        refused(r),
        (
            "E_BAD_INPUT".into(),
            "the actor \"qa-bot\" is not in rdy's actor form".into()
        )
    );
    // a confirmed start by one agent leaves the session that agent: the next answer without
    // words is still refused, and the relay's person never becomes the caller
    session(&mut c, "agent:two", Some("r2"));
    queue_start(&mut hub);
    let mut confirm = cancel(Some("yes, sign in"));
    confirm["answer"] = json!("confirm");
    assert_eq!(ok(auth(&mut c, confirm))["state"], "pending");
    assert_eq!(refused(auth(&mut c, cancel(None))), no_words);
    let mut no_words_confirm = cancel(None);
    no_words_confirm["answer"] = json!("confirm");
    assert_eq!(refused(auth(&mut c, no_words_confirm)), no_words);
    // a second server on the same user config alternates with the first: its own session
    let (_p2, mut d) = chat(home.path());
    session(&mut d, "agent:three", Some("r9"));
    assert_eq!(refused(auth(&mut d, cancel(None))), no_words);
    assert_eq!(
        ok(auth(&mut d, json!({"op": "state", "install": o})))["state"],
        "pending",
        "the saved sign-in is the user's, read by any session"
    );
    assert_eq!(refused(auth(&mut c, cancel(None))), no_words);
    assert_eq!(hub.requests().len(), 2, "one start was sent");
    assert_eq!(hub.finish(), 0);
}

/// A loopback TCP gate in front of `upstream`: it accepts `n` connections in order and pipes
/// each to `upstream`; it reports the first one on `arrived` and holds it until `release`
/// gets a message.
struct Gate {
    origin: String,
    arrived: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Gate {
    fn start(upstream: &str, n: usize) -> Gate {
        let up_port: u16 = upstream.rsplit(':').next().unwrap().parse().unwrap();
        let listener = TcpListener::bind((hub_tls::LOOPBACK, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (arrived_tx, arrived) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            for i in 0..n {
                let (client, _) = listener.accept().expect("accept");
                if i == 0 {
                    arrived_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                let server = TcpStream::connect((hub_tls::LOOPBACK, up_port)).unwrap();
                pipe(client, server);
            }
        });
        Gate {
            origin: format!("https://{}:{port}", hub_tls::LOOPBACK),
            arrived,
            release,
            thread,
        }
    }
}

/// Copies both ways until each side ends.
fn pipe(client: TcpStream, server: TcpStream) {
    let (mut c_in, mut s_out) = (client.try_clone().unwrap(), server.try_clone().unwrap());
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut c_in, &mut s_out);
        let _ = s_out.shutdown(Shutdown::Write);
    });
    let (mut s_in, mut c_out) = (server, client);
    let _ = std::io::copy(&mut s_in, &mut c_out);
    let _ = c_out.flush();
    let _ = c_out.shutdown(Shutdown::Write);
    up.join().unwrap();
}

#[test]
fn hsa8_a_paused_hub_call_does_not_block_other_sessions() {
    let s = Served::new();
    let mut hub = Server::start();
    let gate = Gate::start(hub.origin(), 2);
    let o = gate.origin.clone();
    let cfg = s.user_config();
    let home = cfg.parent().unwrap().parent().unwrap().to_path_buf();
    configure(&home, &[(&o, hub.ca_pem())]);
    let mut file = CredentialFile::default();
    file.set(
        Origin::parse(&o).unwrap(),
        Some(Credential::Ready(ReadyCredential {
            access_token: Secret::new(ACCESS),
            refresh_token: Secret::new(REFRESH),
            expires_at_ms: rdy_engine::now_ms() + 3_000_000,
            account: "acc_00000000000000000000000001".into(),
            handle: "test-person".into(),
        })),
    );
    write_creds(&home, &file);
    // both sessions are connections to one daemon, which runs exactly one async worker: a hub
    // call that ran on that worker instead of the blocking pool would hold B's call too
    let mut env = s.env_pairs();
    env.push(("TOKIO_WORKER_THREADS".into(), "1".into()));
    let mut a = McpClient::start_proxy_at(s.dir.path(), &env);
    let mut b = McpClient::start_proxy_at(s.dir.path(), &env);
    session(&mut a, "agent:a", Some("ra"));
    session(&mut b, "agent:b", Some("rb"));
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::empty(200));
    hub.accept_n(2);
    // A signs out: its discovery request reaches the gate, which holds it
    let id = a.next_id();
    a.send_raw(
        &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "rdy_auth", "arguments": {"op": "sign_out", "install": o}}})
        .to_string(),
    );
    gate.arrived
        .recv_timeout(Duration::from_secs(60))
        .expect("A's hub request arrives");
    // while it is held, B's ordinary read completes, and A has no reply yet: had A's call
    // taken the one worker, B would have waited for A's request to give up
    let r = b.call("rdy_overview", json!({}));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(a.drain(Duration::ZERO), 0, "A is still held at the gate");
    assert!(hub.requests().is_empty(), "A is still held at the gate");
    // released, A completes: discovery, then its one revoke
    gate.release.send(()).unwrap();
    let reply = a.recv(id, Duration::from_secs(60));
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["state"], "signed_out", "{v}");
    assert_eq!(v["warnings"], json!([]), "the revoke was answered");
    assert_eq!(
        routes(&hub.requests()),
        [route("GET", META), route("POST", "/auth/revoke")]
    );
    gate.thread.join().unwrap();
    assert_eq!(hub.finish(), 0);
    assert!(read_creds(&home).is_empty());
}

/// Adds an ordinary key rdy does not know (`actro`) and an unknown hub key (`hub.instalss`) to
/// the user config under `home`.
fn mistype(home: &Path) {
    let file = home.join("rdy/config.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let text = text.replacen("hub:\n", "hub:\n  instalss: {}\n", 1);
    std::fs::write(&file, format!("{text}actro: human:wrong\n")).unwrap();
}

/// The whole tool error of a refusal.
fn refusal(r: McpReply) -> Value {
    match r {
        McpReply::ToolError(v) => v,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A config mistake made while a session is open is reported by the next `rdy_auth` call: the
/// warnings of the project and the user config as they are now, an ordinary unknown key as
/// well as a hub key, are a failure's issues and a successful reply's warnings.
#[test]
fn hsa8_chat_auth_failures_carry_config_warnings() {
    let home = tempfile::tempdir().unwrap();
    let hub = Server::start();
    let o = hub.origin().to_string();
    configure(home.path(), &[(&o, hub.ca_pem())]);
    let (p, mut c) = chat(home.path());
    session(&mut c, "agent:claude", Some("r7"));
    let wrong = json!({"op": "start", "install": o, "machine_name": MACHINE,
        "card_hash": "0".repeat(64), "answer": "confirm", "evidence": "yes"});
    let e = refusal(auth(&mut c, wrong.clone()));
    assert_eq!(
        (e["code"].clone(), e["issues"].clone()),
        (json!("E_UPSTREAM_CONFIRM_HOST"), json!([]))
    );
    mistype(home.path());
    let cfg = home.path().join("rdy/config.yaml");
    let warnings = rdy_engine::config_levels::config_warnings(p.path(), Some(&cfg));
    let found: Vec<bool> = ["actro", "hub.instalss"]
        .iter()
        .map(|k| warnings.iter().any(|w| w.message.contains(k)))
        .collect();
    assert_eq!(found, [true, true], "{warnings:?}");
    let issues = serde_json::to_value(&warnings).unwrap();
    for (args, code) in [
        (wrong, "E_UPSTREAM_CONFIRM_HOST"),
        (
            json!({"op": "start", "install": o, "machine_name": MACHINE, "answer": "confirm"}),
            "E_BAD_INPUT",
        ),
        (
            json!({"op": "sign_out", "install": o, "machine_name": MACHINE}),
            "E_BAD_INPUT",
        ),
    ] {
        let e = refusal(auth(&mut c, args.clone()));
        assert_eq!(e["code"], code, "{args}: {e}");
        assert_eq!(e["issues"], issues, "{args}: {e}");
    }
    // a successful reply carries them too, as the config is now: the card and an offline state
    let v = ok(auth(
        &mut c,
        json!({"op": "start", "install": o, "machine_name": MACHINE}),
    ));
    assert_eq!(
        (v["state"].clone(), v["warnings"].clone()),
        (json!("confirmation_required"), issues.clone())
    );
    let v = ok(auth(&mut c, json!({"op": "state", "install": o})));
    assert_eq!(
        (v["state"].clone(), v["warnings"].clone()),
        (json!("signed_out"), issues)
    );
    assert!(hub.requests().is_empty());
    assert_eq!(hub.finish(), 0);
}

/// Sign-out from chat removes the saved sign-in even when the settings stop loading during the
/// session: a user config that no longer parses, or one with two share remotes. The record is
/// removed durably first; the hub sign-out, which needs the settings, cannot be confirmed, so
/// the reply is signed_out with `W_UPSTREAM_SIGNOUT_LOCAL`, and no request is sent.
#[test]
fn hsa8_chat_sign_out_is_local_first_whatever_the_settings() {
    for broken in [
        "actor: [human:qa\n",
        "remotes:\n  a: {url: \"https://a.example.com\", mode: share}\n  b: {url: \"https://b.example.com\", mode: share}\n",
    ] {
        let home = tempfile::tempdir().unwrap();
        let hub = Server::start();
        let o = hub.origin().to_string();
        configure(home.path(), &[(&o, hub.ca_pem())]);
        let (_p, mut c) = chat(home.path());
        session(&mut c, "agent:claude", Some("r7"));
        let mut file = CredentialFile::default();
        file.set(
            Origin::parse(&o).unwrap(),
            Some(Credential::Ready(ReadyCredential {
                access_token: Secret::new(ACCESS),
                refresh_token: Secret::new(REFRESH),
                expires_at_ms: rdy_engine::now_ms() + 3_000_000,
                account: "acc_00000000000000000000000001".into(),
                handle: "test-person".into(),
            })),
        );
        write_creds(home.path(), &file);
        let cfg = home.path().join("rdy/config.yaml");
        let text = std::fs::read_to_string(&cfg).unwrap();
        std::fs::write(&cfg, format!("{text}{broken}")).unwrap();
        // the settings are refused by a call that needs them
        let (code, _) = refused(auth(&mut c, json!({"op": "state", "install": o})));
        assert!(
            code == "E_CONFIG" || code == "E_REMOTE_EXISTS",
            "{broken}: {code}"
        );
        let v = ok(auth(&mut c, json!({"op": "sign_out", "install": o})));
        assert_eq!(v["state"], "signed_out", "{broken}: {v}");
        let codes: Vec<&str> = v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(codes.last(), Some(&"W_UPSTREAM_SIGNOUT_LOCAL"), "{v}");
        assert!(
            read_creds(home.path()).is_empty(),
            "{broken}: the record is removed"
        );
        assert!(hub.requests().is_empty(), "no request without the settings");
        assert_eq!(hub.finish(), 0);
    }
}
