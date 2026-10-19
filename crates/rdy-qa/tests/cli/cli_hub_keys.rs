//! hsa9 (D-XXA9D, D-1Z951, D-KQ4CA): `rdy remote` and `rdy key bind | retire | revoke`, the
//! command-line fallback of `rdy_remote` and the key tools. The commands run the real binary,
//! through the project's daemon or with `--no-daemon`, against a loopback TLS hub; each child
//! gets a private environment (its own user config home and its own test SSH agent). The
//! daemon signs through the caller's agent, and reads the user config of its own process. A
//! held hub request is held by a channel, never by a sleep.
// the hub sign-in tests of this binary load the same fixture source as a module of their own
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-engine/tests/support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::hub_config::{
    Credential, CredentialFile, ReadyCredential, RemoteMode, encode_credentials,
};
use rdy_engine::hub_remotes::{RemoteLevel, RemoteService};
use rdy_engine::hub_types::{Origin, Secret};
use rdy_engine::{Rdy, now_ms};
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::{land, rdy_bin, seed_doc};
use rdy_qa::mcp::{McpClient, Reply as McpReply};
use rdy_qa::platform::{RecordingAgent, SocketAgent, set_mode};
use rdy_qa::signing::{QaAgent, TestKey};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::time::Duration;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const ACCESS_2: &str = "rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ";
const REFRESH_2: &str = "rdyh_p_000000000000000000000000000000000000000025UVUe";
const ACCOUNT: &str = "acc_00000000000000000000000001";
const HANDLE: &str = "test-person";
const CHALLENGE_ID: &str = "kch_00000000000000000000000002";
const PERSON_KEY_ID: &str = "skey_00000000000000000000000003";
const AGENT_KEY_ID: &str = "skey_00000000000000000000000004";
const NONCE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const HUB_NOW: &str = "2026-10-18T09:00:00Z";
const COMPROMISED_INPUT: &str = "2026-10-17T13:00:00+02:00";
const COMPROMISED: &str = "2026-10-17T11:00:00Z";
/// The start of every SSHSIG proof in base64 (`SSHSIG`), and of a statement.
const PROOF_PREFIX: &str = "U1NIU0lH";
const STATEMENT_PREFIX: &str = "rdy-statement";

/// Every secret or proof part a fixture hands out or makes: none may reach any output.
const HIDDEN: [&str; 8] = [
    ACCESS,
    REFRESH,
    ACCESS_2,
    REFRESH_2,
    CHALLENGE_ID,
    NONCE,
    PROOF_PREFIX,
    STATEMENT_PREFIX,
];

// ---------------------------------------------------------------------------------------------
// Keys and hub replies
// ---------------------------------------------------------------------------------------------

fn fp(k: TestKey) -> String {
    QaAgent::fingerprint(k)
}

/// The key's public-key line, as a bind sends it.
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

/// One hub key of `k` in `state`, with `extra` members over the defaults.
fn key_json(k: TestKey, label: &str, state: &str, extra: &[(&str, &str)]) -> Value {
    let mut v = json!({"id": key_id(k), "account": ACCOUNT, "fingerprint": fp(k),
        "public_key": line(k), "label": label, "kind": kind_of(k), "state": state,
        "expires_at": null, "bound_at": null, "retired_at": null, "revoked_at": null,
        "compromised_from": null});
    for (name, value) in extra {
        v[*name] = json!(value);
    }
    v
}

fn default_label(k: TestKey) -> &'static str {
    match k {
        TestKey::Person => "person key",
        TestKey::Agent => "agent key",
    }
}

/// Queues a bind of `k`: the challenge (ending five minutes from now), then the pending key
/// and its confirmation link on `origin`.
fn queue_bind(hub: &mut Server, origin: &str, k: TestKey, label: Option<&str>) {
    let label = label.unwrap_or(default_label(k));
    let challenge = json!({"id": CHALLENGE_ID, "account": ACCOUNT, "kind": kind_of(k),
        "label": label, "namespace": "rdy-keybind-v1", "nonce": NONCE,
        "expires_at": rfc3339(now_ms() + 300_000)});
    let bound = json!({"key": key_json(k, label, "pending", &[]),
        "confirm_url": confirm_url(origin, k)});
    hub.reply(Reply::json(201, &challenge.to_string()));
    hub.reply(Reply::json(201, &bound.to_string()));
    hub.accept_n(2);
}

fn confirm_url(origin: &str, k: TestKey) -> String {
    format!("{origin}/app/settings/keys/{}/confirm", key_id(k))
}

/// Queues the key list of `k`, then its retire.
fn queue_retire(hub: &mut Server, k: TestKey) {
    queue_list(hub, k);
    let retired = key_json(
        k,
        default_label(k),
        "retired",
        &[
            ("bound_at", "2026-10-17T12:05:00Z"),
            ("retired_at", HUB_NOW),
        ],
    );
    hub.reply(Reply::json(200, &retired.to_string()));
    hub.accept_n(2);
}

/// Queues the key list of `k`, then its revoke compromised from `from`.
fn queue_revoke(hub: &mut Server, k: TestKey, from: &str) {
    queue_list(hub, k);
    let revoked = key_json(
        k,
        default_label(k),
        "revoked",
        &[
            ("bound_at", "2026-10-17T12:05:00Z"),
            ("revoked_at", HUB_NOW),
            ("compromised_from", from),
        ],
    );
    hub.reply(Reply::json(200, &revoked.to_string()));
    hub.accept_n(2);
}

fn queue_list(hub: &Server, k: TestKey) {
    let item = key_json(
        k,
        default_label(k),
        "active",
        &[("bound_at", "2026-10-17T12:05:00Z")],
    );
    hub.reply(Reply::json(
        200,
        &json!({"items": [item], "next": null}).to_string(),
    ));
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

fn find_target(k: TestKey) -> String {
    format!("/api/v1/me/keys?fingerprint={}", form(&fp(k)))
}

fn routes(reqs: &[Recorded]) -> Vec<(String, String)> {
    reqs.iter()
        .map(|r| (r.method.clone(), r.target.clone()))
        .collect()
}

fn route(m: &str, p: &str) -> (String, String) {
    (m.to_string(), p.to_string())
}

fn bind_routes() -> Vec<(String, String)> {
    vec![
        route("POST", "/api/v1/me/keys/challenges"),
        route("POST", "/api/v1/me/keys"),
    ]
}

fn act_routes(k: TestKey, act: &str) -> Vec<(String, String)> {
    vec![
        route("GET", &find_target(k)),
        route("POST", &format!("/api/v1/me/keys/{}/{act}", key_id(k))),
    ]
}

// ---------------------------------------------------------------------------------------------
// Homes, projects and commands
// ---------------------------------------------------------------------------------------------

/// A private user config home: `rdy/config.yaml` names the person, the directories are 0700.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().expect("tempdir");
        set_mode(dir.path(), 0o700).unwrap();
        std::fs::create_dir(dir.path().join("rdy")).unwrap();
        set_mode(&dir.path().join("rdy"), 0o700).unwrap();
        std::fs::write(dir.path().join("rdy/config.yaml"), "actor: human:qa\n").unwrap();
        Home { dir }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("rdy/config.yaml")
    }

    fn creds(&self) -> PathBuf {
        self.dir.path().join("rdy/credentials.yaml")
    }

    fn env(&self) -> Vec<(String, String)> {
        let home = self.dir.path().to_string_lossy().into_owned();
        vec![
            ("XDG_CONFIG_HOME".into(), home.clone()),
            ("HOME".into(), home),
            ("USER".into(), "qa".into()),
        ]
    }

    /// Trusts `hub`'s CA for the install `origin` (the hub itself, or a gate in front of it).
    fn trust_at(&self, origin: &str, hub: &Server) {
        let port = origin.rsplit(':').next().unwrap();
        let pem = self.dir.path().join(format!("ca-{port}.pem"));
        std::fs::write(&pem, hub.ca_pem()).unwrap();
        let mut text = std::fs::read_to_string(self.config()).unwrap();
        if !text.contains("hub:") {
            text.push_str("hub:\n  installs:\n");
        }
        text.push_str(&format!(
            "    \"{origin}\":\n      ca_file: {}\n",
            pem.display()
        ));
        std::fs::write(self.config(), text).unwrap();
    }

    /// A ready sign-in to `origin` with `access` and `refresh`, fresh for 50 minutes.
    fn signed_in(&self, origin: &str, access: &str, refresh: &str) {
        let mut file = CredentialFile::default();
        file.set(
            Origin::parse(origin).unwrap(),
            Some(Credential::Ready(ReadyCredential {
                access_token: Secret::new(access),
                refresh_token: Secret::new(refresh),
                expires_at_ms: now_ms() + 3_000_000,
                account: ACCOUNT.into(),
                handle: HANDLE.into(),
            })),
        );
        std::fs::write(self.creds(), encode_credentials(&file).unwrap()).unwrap();
        set_mode(&self.creds(), 0o600).unwrap();
    }
}

/// A served project in a short directory: the person's two keys in `.rdy/local.yaml`, an idle
/// timeout of 60 s, and `rdy serve --stop` when it is dropped.
struct Proj {
    dir: tempfile::TempDir,
}

impl Proj {
    fn new() -> Proj {
        let dir = short_tempdir();
        drop(Rdy::init(dir.path(), "rdy-standard").expect("init"));
        let cfg = dir.path().join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).unwrap();
        text.push_str("serve:\n  idle_timeout: 60\n");
        std::fs::write(&cfg, text).unwrap();
        std::fs::write(
            dir.path().join(".rdy/local.yaml"),
            format!(
                "signing:\n  key: \"{}\"\n  agent_key: \"{}\"\n  sign: never\n",
                fp(TestKey::Person),
                fp(TestKey::Agent)
            ),
        )
        .unwrap();
        Proj { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Adds remote `name` at `url` to the project config, as `home`'s user.
    fn remote(&self, home: &Home, name: &str, url: &str, mode: RemoteMode) {
        RemoteService::new(self.path(), &home.config())
            .add(name, url, mode, RemoteLevel::Project)
            .unwrap();
    }

    fn daemon_running(&self) -> bool {
        self.path().join(".rdy/run/daemon.pid").exists()
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = Command::new(rdy_bin())
            .env_clear()
            .arg("--project")
            .arg(self.dir.path())
            .args(["serve", "--stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Who runs a command: a user config home, an SSH agent socket and extra variables.
struct Caller<'a> {
    home: &'a Home,
    sock: Option<&'a Path>,
    extra: Vec<(&'a str, &'a str)>,
}

fn caller<'a>(home: &'a Home, sock: Option<&'a Path>) -> Caller<'a> {
    Caller {
        home,
        sock,
        extra: vec![],
    }
}

/// `rdy --project <p> <args>` as `c`, with no other variable of this process.
fn command(p: &Path, c: &Caller<'_>, args: &[&str]) -> Command {
    let mut cmd = Command::new(rdy_bin());
    cmd.env_clear()
        .envs(c.home.env())
        .envs(c.extra.iter().copied())
        .arg("--project")
        .arg(p)
        .args(args)
        .current_dir(p)
        .stdin(Stdio::null());
    if let Some(s) = c.sock {
        cmd.env("SSH_AUTH_SOCK", s);
    }
    cmd
}

fn run(p: &Path, c: &Caller<'_>, args: &[&str]) -> Output {
    let o = command(p, c, args).output().expect("run rdy");
    hidden(&o, args);
    o
}

/// Checks that no output of `rdy <args>` holds a token, a challenge or a proof.
fn hidden(o: &Output, args: &[&str]) {
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    for s in HIDDEN {
        assert!(!all.contains(s), "rdy {args:?} printed {s}:\n{all}");
    }
    assert!(!all.contains("Bearer"), "rdy {args:?}:\n{all}");
}

/// Whether the command succeeded, and its JSON: stdout on success, stderr on failure.
fn parsed(o: &Output) -> (bool, Value) {
    let ok = o.status.success();
    let text = String::from_utf8_lossy(if ok { &o.stdout } else { &o.stderr }).into_owned();
    let v = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
        serde_json::from_str(text.trim().lines().last().unwrap_or(""))
            .unwrap_or(Value::String(text))
    });
    (ok, v)
}

fn json_ok(p: &Path, c: &Caller<'_>, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (ok, v) = parsed(&run(p, c, &all));
    assert!(ok, "rdy {all:?}: {v}");
    v
}

fn json_err(p: &Path, c: &Caller<'_>, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (ok, v) = parsed(&run(p, c, &all));
    assert!(!ok, "rdy {all:?} succeeded: {v}");
    v
}

fn text_ok(p: &Path, c: &Caller<'_>, args: &[&str]) -> String {
    let o = run(p, c, args);
    assert!(
        o.status.success(),
        "rdy {args:?}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Starts the project's daemon as `c`: it keeps `c`'s environment for its whole life.
fn start_daemon(p: &Path, c: &Caller<'_>) {
    let o = run(p, c, &["serve", "--detach"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// A hub with a share remote `origin` on it, a home that trusts it and is signed in there, a
/// served project and the test agent.
struct Kit {
    hub: Server,
    o: String,
    home: Home,
    proj: Proj,
    agent: SocketAgent,
}

fn kit() -> Kit {
    let hub = Server::start();
    let o = hub.origin().to_string();
    let home = Home::new();
    home.trust_at(&o, &hub);
    home.signed_in(&o, ACCESS, REFRESH);
    let proj = Proj::new();
    proj.remote(&home, "origin", &format!("{o}/p"), RemoteMode::Share);
    Kit {
        hub,
        o,
        home,
        proj,
        agent: SocketAgent::start(Arc::new(QaAgent::new())).unwrap(),
    }
}

/// An MCP client of `p`'s daemon (`rdy mcp`, the proxy, which starts the daemon if none runs)
/// for `home`, signing through `sock`, with an agent session.
fn chat(p: &Path, home: &Home, sock: &Path) -> McpClient {
    let mut env = home.env();
    env.push(("SSH_AUTH_SOCK".into(), sock.to_string_lossy().into_owned()));
    let mut c = McpClient::start_proxy_at(p, &env);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:claude", "run": "r7", "role": "writer"}),
    );
    assert!(r.is_ok(), "{r:?}");
    c
}

/// Stops the project's daemon, so a `--no-daemon` command may open the store.
fn stop_daemon(p: &Path, c: &Caller<'_>) {
    let o = run(p, c, &["serve", "--stop"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// A tool's reply, after checking that it holds no token, challenge or proof.
fn tool(c: &mut McpClient, name: &str, args: Value) -> McpReply {
    let r = c.call(name, args.clone());
    let text = format!("{r:?}");
    for s in HIDDEN {
        assert!(!text.contains(s), "{name} {args}: {s} in {text}");
    }
    r
}

fn ok(r: McpReply) -> Value {
    match r {
        McpReply::Ok(v) => v,
        other => panic!("expected a reply, got {other:?}"),
    }
}

fn refusal(r: McpReply) -> Value {
    match r {
        McpReply::ToolError(v) => v,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// An error's code, message and fix.
fn cmf(v: &Value) -> (Value, Value, Value) {
    (v["code"].clone(), v["message"].clone(), v["fix"].clone())
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn hsa9_remote_and_key_cli_match_mcp() {
    // remotes: the same edits on two projects, one over the CLI, one over MCP
    let (home_a, home_b) = (Home::new(), Home::new());
    let (pa, pb) = (Proj::new(), Proj::new());
    let none = RecordingAgent::start().unwrap();
    let me = caller(&home_a, Some(none.socket_path()));
    let mut c = chat(pb.path(), &home_b, none.socket_path());
    let u = "https://127.0.0.1:9/team/p";
    let m = "https://127.0.0.1:8/mirror";
    let steps: Vec<(Vec<&str>, Value)> = vec![
        (vec!["remote", "list"], json!({"op": "list"})),
        (
            vec!["remote", "add", "origin", u, "--mode", "share"],
            json!({"op": "add", "name": "origin", "url": u, "mode": "share"}),
        ),
        (
            vec![
                "remote", "add", "mirror", m, "--mode", "publish", "--level", "user",
            ],
            json!({"op": "add", "name": "mirror", "url": m, "mode": "publish", "level": "user"}),
        ),
        (vec!["remote", "list"], json!({"op": "list"})),
        (
            vec!["remote", "set-mode", "origin", "publish"],
            json!({"op": "set_mode", "name": "origin", "mode": "publish"}),
        ),
        (
            vec!["remote", "set-mode", "mirror", "share", "--level", "local"],
            json!({"op": "set_mode", "name": "mirror", "mode": "share", "level": "local"}),
        ),
        (
            vec!["remote", "rm", "mirror", "--level", "user"],
            json!({"op": "rm", "name": "mirror", "level": "user"}),
        ),
        (vec!["remote", "list"], json!({"op": "list"})),
    ];
    let mut replies = Vec::new();
    for (args, mcp) in &steps {
        let mut all = vec!["--json"];
        all.extend(args.iter().copied());
        let (cli_ok, cli) = parsed(&run(pa.path(), &me, &all));
        let chat_reply = tool(&mut c, "rdy_remote", mcp.clone());
        match chat_reply {
            McpReply::Ok(v) => {
                assert!(cli_ok, "{args:?}: {cli}");
                assert_eq!(cli, v, "{args:?}");
                replies.push(v);
            }
            McpReply::ToolError(v) => {
                assert!(!cli_ok, "{args:?}: {cli}");
                assert_eq!(cmf(&cli), cmf(&v), "{args:?}");
                let issues = |e: &Value| e["issues"].as_array().cloned().unwrap_or_default();
                assert_eq!(issues(&cli), issues(&v), "{args:?}");
                replies.push(v);
            }
            other => panic!("{other:?}"),
        }
    }
    let row = |name: &str, url: &str, mode: &str, level: &str, install: &str| json!({"name": name, "url": url, "mode": mode, "level": level, "install": install});
    let origin_row = |mode: &str| row("origin", u, mode, "project", "https://127.0.0.1:9");
    assert_eq!(replies[0], json!({"remotes": [], "warnings": []}));
    assert_eq!(
        replies[1],
        json!({"remote": origin_row("share"), "removed": false, "warnings": []})
    );
    assert_eq!(
        replies[3],
        json!({"remotes": [row("mirror", m, "publish", "user", "https://127.0.0.1:8"),
            origin_row("share")], "warnings": []})
    );
    assert_eq!(
        replies[4],
        json!({"remote": origin_row("publish"), "removed": false, "warnings": []})
    );
    // a mode change at a level that has no entry of that name
    assert_eq!(
        (replies[5]["code"].clone(), replies[5]["message"].clone()),
        (
            json!("E_NOT_FOUND"),
            json!("remote mirror does not exist at this config level")
        )
    );
    assert_eq!(
        replies[6],
        json!({"remote": row("mirror", m, "publish", "user", "https://127.0.0.1:8"),
            "removed": true, "warnings": []})
    );
    assert_eq!(
        replies[7],
        json!({"remotes": [origin_row("publish")], "warnings": []})
    );
    // a second share remote is refused alike, with the shared text
    let mut both = |args: &[&str], mcp: Value| -> (Value, Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let cli = parsed(&run(pa.path(), &me, &all)).1;
        let chat_reply = tool(&mut c, "rdy_remote", mcp);
        let v = match chat_reply {
            McpReply::Ok(v) | McpReply::ToolError(v) => v,
            other => panic!("{other:?}"),
        };
        (cli, v)
    };
    let (cli, v) = both(
        &["remote", "add", "two", m, "--mode", "share"],
        json!({"op": "add", "name": "two", "url": m, "mode": "share"}),
    );
    assert_eq!(cli, v);
    assert_eq!(v["remote"]["mode"], "share", "{v}");
    let (cli, v) = both(
        &[
            "remote", "set-mode", "origin", "share", "--level", "project",
        ],
        json!({"op": "set_mode", "name": "origin", "mode": "share", "level": "project"}),
    );
    assert_eq!(
        cmf(&v),
        (
            json!("E_REMOTE_EXISTS"),
            json!("a share remote already exists"),
            json!("change its mode before adding another share remote")
        )
    );
    assert_eq!(cmf(&cli), cmf(&v));
    // the CLI's request checks are the tool's: a missing or unused field has the same text
    let v = refusal(tool(&mut c, "rdy_remote", json!({"op": "rm"})));
    assert_eq!(
        cmf(&v),
        (
            json!("E_BAD_INPUT"),
            json!("remote rm needs name"),
            json!("pass name")
        )
    );
    assert_eq!(none.connections(), 0, "remote edits ask no agent");

    // the text forms
    let (home, proj) = (Home::new(), Proj::new());
    let me = caller(&home, Some(none.socket_path()));
    assert_eq!(
        text_ok(proj.path(), &me, &["remote", "list"]),
        "no remotes\n"
    );
    assert_eq!(
        text_ok(
            proj.path(),
            &me,
            &[
                "remote",
                "add",
                "origin",
                &format!("{u}/"),
                "--mode",
                "share"
            ]
        ),
        format!("remote origin: share {u}\n")
    );
    assert_eq!(
        text_ok(
            proj.path(),
            &me,
            &[
                "remote", "add", "mirror", m, "--mode", "publish", "--level", "local"
            ]
        ),
        format!("remote mirror: publish {m}\n")
    );
    assert_eq!(
        text_ok(proj.path(), &me, &["remote", "list"]),
        format!("mirror publish {m} (local)\norigin share {u} (project)\n")
    );
    assert_eq!(
        text_ok(
            proj.path(),
            &me,
            &["remote", "set-mode", "origin", "publish"]
        ),
        format!("remote origin: publish {u}\n")
    );
    assert_eq!(
        text_ok(
            proj.path(),
            &me,
            &["remote", "rm", "mirror", "--level", "local"]
        ),
        "removed remote mirror from local config\n"
    );
    // no alias between the singular key command and the plural block-key command
    for args in [
        ["keys", "bind", "--kind", "person"],
        ["key", "assign", "--kind", "person"],
    ] {
        let o = run(proj.path(), &me, &args);
        assert!(!o.status.success(), "{args:?}");
    }
    for args in [["remotes", "list"], ["remote", "remove"]] {
        let o = run(proj.path(), &me, &args);
        assert!(!o.status.success(), "{args:?}");
    }

    // key text and JSON: bind, retire and revoke, as text then as JSON
    let mut k = kit();
    let o = k.o.clone();
    queue_bind(&mut k.hub, &o, TestKey::Person, None);
    assert_eq!(
        text_ok(
            k.proj.path(),
            &caller(&k.home, Some(k.agent.socket_path())),
            &["key", "bind", "--kind", "person"]
        ),
        format!(
            "key {}: pending\nconfirm: {}\n",
            fp(TestKey::Person),
            confirm_url(&o, TestKey::Person)
        )
    );
    queue_bind(&mut k.hub, &o, TestKey::Agent, Some("agent laptop"));
    assert_eq!(
        json_ok(
            k.proj.path(),
            &caller(&k.home, Some(k.agent.socket_path())),
            &[
                "key",
                "bind",
                "--kind",
                "agent",
                "--label",
                "agent laptop",
                "--remote",
                "origin"
            ]
        ),
        json!({"key": fp(TestKey::Agent), "kind": "agent", "state": "pending",
            "confirm_url": confirm_url(&o, TestKey::Agent), "remote": "origin", "warnings": []})
    );
    queue_retire(&mut k.hub, TestKey::Person);
    assert_eq!(
        text_ok(
            k.proj.path(),
            &caller(&k.home, Some(k.agent.socket_path())),
            &["key", "retire", &fp(TestKey::Person)]
        ),
        format!("key {}: retired\n", fp(TestKey::Person))
    );
    queue_retire(&mut k.hub, TestKey::Person);
    assert_eq!(
        json_ok(
            k.proj.path(),
            &caller(&k.home, Some(k.agent.socket_path())),
            &["key", "retire", &line(TestKey::Person)]
        ),
        json!({"key": fp(TestKey::Person), "state": "retired", "remote": "origin",
            "warnings": []})
    );
    queue_revoke(&mut k.hub, TestKey::Agent, COMPROMISED);
    assert_eq!(
        text_ok(
            k.proj.path(),
            &caller(&k.home, Some(k.agent.socket_path())),
            &[
                "key",
                "revoke",
                &fp(TestKey::Agent),
                "--compromised-since",
                COMPROMISED_INPUT
            ]
        ),
        format!(
            "key {}: revoked\nrevoked at: {HUB_NOW}\ncompromised from: {COMPROMISED}\n",
            fp(TestKey::Agent)
        )
    );
    queue_revoke(&mut k.hub, TestKey::Agent, HUB_NOW);
    assert_eq!(
        json_ok(
            k.proj.path(),
            &caller(&k.home, Some(k.agent.socket_path())),
            &["key", "revoke", &fp(TestKey::Agent)]
        ),
        json!({"key": fp(TestKey::Agent), "state": "revoked", "revoked_at": HUB_NOW,
            "compromised_from": HUB_NOW, "remote": "origin", "warnings": []})
    );
    let mut want = bind_routes();
    want.extend(bind_routes());
    want.extend(act_routes(TestKey::Person, "retire"));
    want.extend(act_routes(TestKey::Person, "retire"));
    want.extend(act_routes(TestKey::Agent, "revoke"));
    want.extend(act_routes(TestKey::Agent, "revoke"));
    let reqs = k.hub.requests();
    assert_eq!(routes(&reqs), want);
    assert_eq!(
        reqs[9].body_text(),
        format!("{{\"compromised_from\":\"{COMPROMISED}\"}}")
    );
    assert!(
        reqs[11].body.is_empty(),
        "no time is sent when none is given"
    );
    assert_eq!(k.hub.finish(), 0);
}

#[test]
fn hsa9_daemon_keybind_uses_the_callers_agent() {
    let mut k = kit();
    let o = k.o.clone();
    // the daemon runs with an agent of its own, which also holds both keys
    let daemons = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
    start_daemon(k.proj.path(), &caller(&k.home, Some(daemons.socket_path())));
    assert!(k.proj.daemon_running());
    for key in [TestKey::Person, TestKey::Agent] {
        queue_bind(&mut k.hub, &o, key, None);
        let before = k.agent.requests();
        let v = json_ok(
            k.proj.path(),
            &caller(&k.home, Some(k.agent.socket_path())),
            &["key", "bind", "--kind", kind_of(key)],
        );
        assert_eq!(v["key"], fp(key));
        assert_eq!(v["state"], "pending");
        assert!(
            k.agent.requests() > before,
            "the caller's agent signs the proof"
        );
    }
    assert_eq!(
        daemons.requests(),
        0,
        "the daemon's own agent is never asked"
    );
    // with no agent at the caller, the daemon does not fall back to its own
    let v = json_err(
        k.proj.path(),
        &caller(&k.home, None),
        &["key", "bind", "--kind", "person"],
    );
    assert_eq!(v["code"], "E_NO_AGENT", "{v}");
    assert_eq!(daemons.requests(), 0);
    let mut want = bind_routes();
    want.extend(bind_routes());
    assert_eq!(
        routes(&k.hub.requests()),
        want,
        "the failed bind sent nothing"
    );
    assert_eq!(k.hub.finish(), 0);
}

#[test]
fn hsa9_no_daemon_keybind_uses_the_same_workflow() {
    let mut k = kit();
    let o = k.o.clone();
    let direct = |args: &[&str]| -> Vec<String> {
        let mut all = vec!["--no-daemon".to_string(), "--json".to_string()];
        all.extend(args.iter().map(|s| s.to_string()));
        all
    };
    let mut replies = Vec::new();
    for daemon in [false, true] {
        queue_bind(&mut k.hub, &o, TestKey::Person, Some("laptop key"));
        let args = ["key", "bind", "--kind", "person", "--label", "laptop key"];
        let before = k.agent.requests();
        let v = if daemon {
            json_ok(
                k.proj.path(),
                &caller(&k.home, Some(k.agent.socket_path())),
                &args,
            )
        } else {
            let all = direct(&args);
            let all: Vec<&str> = all.iter().map(String::as_str).collect();
            let (ok, v) = parsed(&run(
                k.proj.path(),
                &caller(&k.home, Some(k.agent.socket_path())),
                &all,
            ));
            assert!(ok, "{v}");
            assert!(!k.proj.daemon_running(), "--no-daemon started no daemon");
            v
        };
        assert!(k.agent.requests() > before, "this process's agent signs");
        replies.push(v);
    }
    assert_eq!(replies[0], replies[1]);
    assert_eq!(
        replies[0],
        json!({"key": fp(TestKey::Person), "kind": "person", "state": "pending",
            "confirm_url": confirm_url(&o, TestKey::Person), "remote": "origin",
            "warnings": []})
    );
    // the same requests, byte for byte: Ed25519 signs deterministically
    let reqs = k.hub.requests();
    let mut want = bind_routes();
    want.extend(bind_routes());
    assert_eq!(routes(&reqs), want);
    assert_eq!(reqs[0].body, reqs[2].body);
    assert_eq!(reqs[1].body, reqs[3].body);
    for r in &reqs {
        assert_eq!(r.header("rdy-actor"), Some("human:test-person"));
        assert_eq!(
            r.header("authorization"),
            Some(&*format!("Bearer {ACCESS}"))
        );
    }

    // retire runs the same way, too
    stop_daemon(k.proj.path(), &caller(&k.home, None));
    for daemon in [false, true] {
        queue_retire(&mut k.hub, TestKey::Agent);
        let args = ["key", "retire", &fp(TestKey::Agent)];
        let v = if daemon {
            json_ok(
                k.proj.path(),
                &caller(&k.home, Some(k.agent.socket_path())),
                &args,
            )
        } else {
            let all = direct(&args);
            let all: Vec<&str> = all.iter().map(String::as_str).collect();
            parsed(&run(
                k.proj.path(),
                &caller(&k.home, Some(k.agent.socket_path())),
                &all,
            ))
            .1
        };
        assert_eq!(v["state"], "retired", "{v}");
    }
    assert_eq!(k.hub.finish(), 0);
}

#[test]
fn hsa9_daemon_credentials_use_the_daemons_user_config_home() {
    let mut hub = Server::start();
    let o = hub.origin().to_string();
    let (callers, daemons) = (Home::new(), Home::new());
    for (home, access, refresh) in [(&callers, ACCESS, REFRESH), (&daemons, ACCESS_2, REFRESH_2)] {
        home.trust_at(&o, &hub);
        home.signed_in(&o, access, refresh);
    }
    let proj = Proj::new();
    proj.remote(&callers, "origin", &format!("{o}/p"), RemoteMode::Share);
    let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
    start_daemon(proj.path(), &caller(&daemons, None));
    let creds = |h: &Home| std::fs::read(h.creds()).unwrap();
    let (callers_before, daemons_before) = (creds(&callers), creds(&daemons));

    // through the daemon: the daemon's own sibling credential file, whoever calls
    let me = caller(&callers, Some(agent.socket_path()));
    queue_retire(&mut hub, TestKey::Person);
    let v = json_ok(proj.path(), &me, &["key", "retire", &fp(TestKey::Person)]);
    assert_eq!(v["state"], "retired");
    queue_bind(&mut hub, &o, TestKey::Person, None);
    let v = json_ok(proj.path(), &me, &["key", "bind", "--kind", "person"]);
    assert_eq!(v["state"], "pending");
    // remote edits read and lock the daemon's user config too
    let v = json_ok(
        proj.path(),
        &me,
        &[
            "remote",
            "add",
            "mine",
            "https://127.0.0.1:7/x",
            "--mode",
            "publish",
            "--level",
            "user",
        ],
    );
    assert_eq!(v["remote"]["level"], "user");
    let daemon_cfg = std::fs::read_to_string(daemons.config()).unwrap();
    let caller_cfg = std::fs::read_to_string(callers.config()).unwrap();
    assert!(daemon_cfg.contains("mine"), "{daemon_cfg}");
    assert!(!caller_cfg.contains("mine"), "{caller_cfg}");
    let reqs = hub.requests();
    assert_eq!(reqs.len(), 4);
    for r in &reqs {
        assert_eq!(
            r.header("authorization"),
            Some(&*format!("Bearer {ACCESS_2}")),
            "{}",
            r.target
        );
    }
    // without the daemon, this process's own user config home
    stop_daemon(proj.path(), &me);
    queue_retire(&mut hub, TestKey::Person);
    let v = json_ok(
        proj.path(),
        &me,
        &["--no-daemon", "key", "retire", &fp(TestKey::Person)],
    );
    assert_eq!(v["state"], "retired");
    let reqs = hub.requests();
    for r in &reqs[4..] {
        assert_eq!(
            r.header("authorization"),
            Some(&*format!("Bearer {ACCESS}"))
        );
    }
    assert_eq!(creds(&callers), callers_before);
    assert_eq!(creds(&daemons), daemons_before);
    assert_eq!(hub.finish(), 0);
}

#[test]
fn hsa9_key_show_and_local_work_stay_offline() {
    let k = kit();
    // a landing to sign, written before any command starts the daemon
    let landing = {
        let mut r = Rdy::open(k.proj.path()).unwrap();
        seed_doc(&mut r, "seed");
        let id = land(&mut r, "seed").unwrap();
        r.close().unwrap();
        id
    };
    let me = caller(&k.home, Some(k.agent.socket_path()));
    for direct in [true, false] {
        let flag: &[&str] = if direct { &["--no-daemon"] } else { &[] };
        let with = |args: &[&str]| -> Vec<String> {
            flag.iter().chain(args).map(|s| s.to_string()).collect()
        };
        for args in [
            vec!["--json", "key", "show"],
            vec!["key", "show"],
            vec!["status"],
            vec!["log"],
            vec!["--json", "remote", "list"],
            vec!["remote", "list"],
            vec!["config"],
            vec!["config", "--show-origin"],
        ] {
            let all = with(&args);
            let all: Vec<&str> = all.iter().map(String::as_str).collect();
            let o = run(k.proj.path(), &me, &all);
            assert!(
                o.status.success(),
                "{all:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
    }
    let o = run(k.proj.path(), &me, &["--json", "sign", &landing]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = run(k.proj.path(), &me, &["auth", "installs"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // the chat tools of SIG and the remote list
    let mut c = chat(k.proj.path(), &k.home, k.agent.socket_path());
    for (name, args) in [
        ("rdy_key_show", json!({})),
        ("rdy_remote", json!({"op": "list"})),
        ("rdy_overview", json!({})),
    ] {
        let r = tool(&mut c, name, args);
        assert!(r.is_ok(), "{name}: {r:?}");
    }
    assert_eq!(k.hub.accepted(), 0, "no local command reached the hub");
    assert_eq!(k.hub.finish(), 0);
}

#[test]
fn hsa9_all_client_surfaces_hide_credential_and_proof_bytes() {
    let mut k = kit();
    let o = k.o.clone();
    let me = caller(&k.home, Some(k.agent.socket_path()));
    // successes over the CLI, both ways (directly first: a running daemon holds the store)
    for direct in [true, false] {
        let pre: &[&str] = if direct { &["--no-daemon"] } else { &[] };
        let cmd = |args: &[&str]| -> Vec<String> {
            pre.iter().chain(args).map(|s| s.to_string()).collect()
        };
        queue_bind(&mut k.hub, &o, TestKey::Person, None);
        let all = cmd(&["key", "bind", "--kind", "person"]);
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        text_ok(k.proj.path(), &me, &all);
        queue_retire(&mut k.hub, TestKey::Person);
        let all = cmd(&["--json", "key", "retire", &fp(TestKey::Person)]);
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        text_ok(k.proj.path(), &me, &all);
    }
    // a refused bind, a lost revoke and a refused retire: errors in both forms
    k.hub.reply(Reply::json(
        201,
        &json!({"id": CHALLENGE_ID, "account": ACCOUNT, "kind": "person",
            "label": "person key", "namespace": "rdy-keybind-v1", "nonce": NONCE,
            "expires_at": rfc3339(now_ms() + 300_000)})
        .to_string(),
    ));
    k.hub.reply(Reply::json(
        422,
        r#"{"code":"E_HUB_KEY_PROOF","message":"proof U1NIU0lH rejected"}"#,
    ));
    k.hub.accept_n(2);
    let v = json_err(k.proj.path(), &me, &["key", "bind", "--kind", "person"]);
    assert_eq!(v["code"], "E_UPSTREAM_REFUSED", "{v}");
    queue_list(&k.hub, TestKey::Person);
    k.hub.reply(Reply::Close);
    k.hub.accept_n(2);
    let o2 = run(k.proj.path(), &me, &["key", "revoke", &fp(TestKey::Person)]);
    assert!(!o2.status.success());
    assert!(
        String::from_utf8_lossy(&o2.stderr).contains("E_UPSTREAM_UNCERTAIN"),
        "{}",
        String::from_utf8_lossy(&o2.stderr)
    );
    k.hub.reply(Reply::json(
        403,
        r#"{"code":"E_HUB_KEY_ACTOR","message":"token rdyh_a_ refused"}"#,
    ));
    k.hub.accept_n(1);
    let v = json_err(k.proj.path(), &me, &["key", "retire", &fp(TestKey::Person)]);
    assert_eq!(v["code"], "E_UPSTREAM_REFUSED", "{v}");
    // configuration output, in every form
    for args in [
        vec!["config"],
        vec!["config", "--show-origin"],
        vec!["--json", "config"],
        vec!["--json", "config", "--show-origin"],
        vec!["remote", "list"],
        vec!["auth", "installs"],
        vec!["--json", "auth", "installs"],
    ] {
        let _ = run(k.proj.path(), &me, &args);
    }
    // the chat tools
    let mut c = chat(k.proj.path(), &k.home, k.agent.socket_path());
    queue_bind(&mut k.hub, &o, TestKey::Agent, None);
    let _ = ok(tool(&mut c, "rdy_key_bind", json!({"kind": "agent"})));
    queue_revoke(&mut k.hub, TestKey::Agent, HUB_NOW);
    let _ = ok(tool(
        &mut c,
        "rdy_key_revoke",
        json!({"key": fp(TestKey::Agent)}),
    ));
    k.hub.reply(Reply::Close);
    k.hub.accept_n(1);
    let _ = refusal(tool(&mut c, "rdy_key_bind", json!({"kind": "agent"})));
    let _ = ok(tool(&mut c, "rdy_remote", json!({"op": "list"})));
    let _ = ok(tool(&mut c, "rdy_key_show", json!({})));
    // no config file holds a token or a proof, and the credential file holds no proof
    for file in [
        k.home.config(),
        k.proj.path().join(".rdy.yaml"),
        k.proj.path().join(".rdy/local.yaml"),
    ] {
        let text = std::fs::read_to_string(&file).unwrap();
        for s in HIDDEN {
            assert!(!text.contains(s), "{}: {s}", file.display());
        }
    }
    let creds = std::fs::read_to_string(k.home.creds()).unwrap();
    for s in [CHALLENGE_ID, NONCE, PROOF_PREFIX, STATEMENT_PREFIX] {
        assert!(!creds.contains(s), "credentials hold {s}");
    }
    // the proof did reach the hub, and only there
    let reqs = k.hub.requests();
    assert!(
        reqs.iter()
            .any(|r| r.target == "/api/v1/me/keys" && r.body_text().contains(PROOF_PREFIX))
    );
    assert_eq!(k.hub.finish(), 0);
}

#[test]
fn hsa9_hostile_fields_fail_before_network_or_agent() {
    let k = kit();
    let rec = RecordingAgent::start().unwrap();
    let me = caller(&k.home, Some(rec.socket_path()));
    let long = |n: usize| "a".repeat(n);
    let wide = "é".repeat(65);
    let key_line = line(TestKey::Person);
    let fpp = fp(TestKey::Person);
    let over_key = format!("{key_line} {}", long(16_385));
    let over_url = format!("https://127.0.0.1:9/{}", long(4_097));
    let over_time = format!("2026-10-17T11:00:00{}Z", "0".repeat(60));
    let label_257 = long(257);
    let name_65 = long(65);
    // (CLI arguments, tool, tool arguments, code)
    let cases: Vec<(Vec<&str>, &str, Value, &str)> = vec![
        (
            vec!["key", "bind", "--kind", "person", "--label", &label_257],
            "rdy_key_bind",
            json!({"kind": "person", "label": label_257}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "bind", "--kind", "person", "--label", &wide],
            "rdy_key_bind",
            json!({"kind": "person", "label": wide}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "bind", "--kind", "person", "--label", "a\u{7}b"],
            "rdy_key_bind",
            json!({"kind": "person", "label": "a\u{7}b"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "bind", "--kind", "person", "--label", ""],
            "rdy_key_bind",
            json!({"kind": "person", "label": ""}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "bind", "--kind", "agent", "--remote", "bad name"],
            "rdy_key_bind",
            json!({"kind": "agent", "remote": "bad name"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "bind", "--kind", "agent", "--remote", &name_65],
            "rdy_key_bind",
            json!({"kind": "agent", "remote": name_65}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "bind", "--kind", "agent", "--remote", "nowhere"],
            "rdy_key_bind",
            json!({"kind": "agent", "remote": "nowhere"}),
            "E_NOT_FOUND",
        ),
        (
            vec!["key", "retire", &over_key],
            "rdy_key_retire",
            json!({"key": over_key}),
            "E_LIMIT",
        ),
        (
            vec!["key", "retire", "not a key"],
            "rdy_key_retire",
            json!({"key": "not a key"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "retire", &fpp, "--remote", "bad/name"],
            "rdy_key_retire",
            json!({"key": fpp, "remote": "bad/name"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "revoke", "SHA256:short"],
            "rdy_key_revoke",
            json!({"key": "SHA256:short"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "revoke", &fpp, "--compromised-since", &over_time],
            "rdy_key_revoke",
            json!({"key": fpp, "compromised_since": over_time}),
            "E_LIMIT",
        ),
        (
            vec!["key", "revoke", &fpp, "--compromised-since", "yesterday"],
            "rdy_key_revoke",
            json!({"key": fpp, "compromised_since": "yesterday"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["key", "revoke", &fpp, "--remote", &name_65],
            "rdy_key_revoke",
            json!({"key": fpp, "remote": name_65}),
            "E_BAD_INPUT",
        ),
        (
            vec!["remote", "add", "n", &over_url, "--mode", "share"],
            "rdy_remote",
            json!({"op": "add", "name": "n", "url": over_url, "mode": "share"}),
            "E_LIMIT",
        ),
        (
            vec![
                "remote",
                "add",
                "n",
                "http://127.0.0.1:9/p",
                "--mode",
                "share",
            ],
            "rdy_remote",
            json!({"op": "add", "name": "n", "url": "http://127.0.0.1:9/p", "mode": "share"}),
            "E_BAD_INPUT",
        ),
        (
            vec![
                "remote",
                "add",
                "n",
                "https://u:p@127.0.0.1:9/p",
                "--mode",
                "share",
            ],
            "rdy_remote",
            json!({"op": "add", "name": "n", "url": "https://u:p@127.0.0.1:9/p", "mode": "share"}),
            "E_BAD_INPUT",
        ),
        (
            vec![
                "remote",
                "add",
                &name_65,
                "https://127.0.0.1:9/p",
                "--mode",
                "share",
            ],
            "rdy_remote",
            json!({"op": "add", "name": name_65, "url": "https://127.0.0.1:9/p", "mode": "share"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["remote", "rm", "--", "-x"],
            "rdy_remote",
            json!({"op": "rm", "name": "-x"}),
            "E_BAD_INPUT",
        ),
        (
            vec!["remote", "set-mode", "bad name", "share"],
            "rdy_remote",
            json!({"op": "set_mode", "name": "bad name", "mode": "share"}),
            "E_BAD_INPUT",
        ),
    ];
    let mut c = chat(k.proj.path(), &k.home, rec.socket_path());
    let before = std::fs::read(k.proj.path().join(".rdy.yaml")).unwrap();
    for (args, name, targs, code) in &cases {
        let v = json_err(k.proj.path(), &me, args);
        assert_eq!(v["code"], *code, "{args:?}: {v}");
        let t = refusal(tool(&mut c, name, targs.clone()));
        assert_eq!(cmf(&t), cmf(&v), "{name} {targs}");
        // no text repeats an input over its bound
        let text = format!("{} {}", v["message"], v["fix"]);
        for value in targs.as_object().unwrap().values() {
            if let Some(given) = value.as_str().filter(|g| g.len() > 64) {
                assert!(!text.contains(given), "{name}: {text}");
            }
        }
    }
    assert_eq!(rec.connections(), 0, "no agent was asked");
    assert_eq!(k.hub.accepted(), 0, "no request was sent");
    assert_eq!(
        std::fs::read(k.proj.path().join(".rdy.yaml")).unwrap(),
        before,
        "no refused edit changed the project config"
    );
    // no expiry, private key, confirmation or token argument exists on either surface
    for (args, targs) in [
        (
            vec!["key", "bind", "--kind", "person", "--expires", "1d"],
            json!({"kind": "person", "expires": "1d"}),
        ),
        (
            vec!["key", "bind", "--kind", "person", "--private-key", "x"],
            json!({"kind": "person", "private_key": "x"}),
        ),
        (
            vec!["key", "revoke", &fpp, "--confirm"],
            json!({"key": fpp, "confirm": true}),
        ),
        (
            vec!["key", "retire", &fpp, "--token", "x"],
            json!({"key": fpp, "token": "x"}),
        ),
    ] {
        let o = run(k.proj.path(), &me, &args);
        assert!(!o.status.success(), "{args:?}");
        let tool_name = format!("rdy_key_{}", args[1]);
        let t = refusal(tool(&mut c, &tool_name, targs));
        assert_eq!(t["code"], "E_BAD_INPUT");
    }
    assert_eq!(rec.connections(), 0);
    assert_eq!(k.hub.finish(), 0);
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
fn hsa9_a_paused_hub_op_does_not_block_other_clients() {
    let mut hub = Server::start();
    let gate = Gate::start(hub.origin(), 2);
    let o = gate.origin.clone();
    let home = Home::new();
    home.trust_at(&o, &hub);
    home.signed_in(&o, ACCESS, REFRESH);
    let proj = Proj::new();
    proj.remote(&home, "origin", &format!("{o}/p"), RemoteMode::Share);
    // one daemon with exactly one async worker: an op that ran on that worker instead of the
    // blocking pool would hold B's op too
    let mut daemon = caller(&home, None);
    daemon.extra.push(("TOKIO_WORKER_THREADS", "1"));
    start_daemon(proj.path(), &daemon);
    queue_retire(&mut hub, TestKey::Person);
    // A retires a key through the daemon: its first hub request reaches the gate, which holds it
    let me = caller(&home, None);
    let fpp = fp(TestKey::Person);
    let mut a = command(proj.path(), &me, &["--json", "key", "retire", &fpp])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn A");
    gate.arrived
        .recv_timeout(Duration::from_secs(60))
        .expect("A's hub request arrives");
    // while it is held, B's ordinary read op completes, and A has not finished
    let v = json_ok(proj.path(), &me, &["status"]);
    assert!(v["views"].is_array(), "{v}");
    assert!(
        a.try_wait().unwrap().is_none(),
        "A is still held at the gate"
    );
    assert!(hub.requests().is_empty(), "A is still held at the gate");
    // released, A completes: its key lookup, then its one retire
    gate.release.send(()).unwrap();
    let mut out = String::new();
    a.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert!(a.wait().unwrap().success(), "{out}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["state"], "retired", "{v}");
    assert_eq!(
        routes(&hub.requests()),
        act_routes(TestKey::Person, "retire")
    );
    gate.thread.join().unwrap();
    assert_eq!(hub.finish(), 0);
}
