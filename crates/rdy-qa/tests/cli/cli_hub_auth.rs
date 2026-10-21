//! hsa8 (D-KQ4CA, n-XZDQD, n-N055T): `rdy auth`, the command-line fallback of `rdy_auth`, and
//! the machine-token door. The commands run with no project, store or daemon, through the real
//! HTTPS transport to a loopback TLS hub; each child process gets a private environment (its
//! own user config home, no shell actor). The time a pending sign-in may next be checked is
//! moved by rewriting the saved schedule, never by waiting.
#[path = "../../../rdy-engine/tests/support/hub_tls.rs"]
mod hub_tls;

use hub_tls::{Recorded, Reply, Server};
use rdy_engine::hub_config::{
    Credential, CredentialFile, PendingSignIn, ReadyCredential, encode_credentials,
    parse_credentials,
};
use rdy_engine::hub_types::{Origin, Secret};
use rdy_model::Hash;
use rdy_qa::fixtures::{project, rdy_bin};
use rdy_qa::mcp::{McpClient, Reply as McpReply};
use rdy_qa::platform::set_mode;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const ACCESS_2: &str = "rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ";
const REFRESH_2: &str = "rdyh_p_000000000000000000000000000000000000000025UVUe";
const DEVICE_CODE: &str = "device-code-sentinel-7Hq2";
const USER_CODE: &str = "BCDFGHJK";
const ACCOUNT: &str = "acc_00000000000000000000000001";
const MACHINE: &str = "laptop";
const PERSON: &str = "human:qa";

const METADATA_REPLY: &str = r#"{"issuer":"{o}/auth","authorization_endpoint":"{o}/auth/authorize","device_authorization_endpoint":"{o}/auth/device","token_endpoint":"{o}/auth/token","revocation_endpoint":"{o}/auth/revoke","registration_endpoint":"{o}/auth/register","scopes_supported":["pull","push","upload","change"],"response_types_supported":["code"],"grant_types_supported":["authorization_code","urn:ietf:params:oauth:grant-type:device_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]}"#;
const DEVICE_REPLY: &str = r#"{"device_code":"device-code-sentinel-7Hq2","user_code":"BCDFGHJK","verification_uri":"{o}/app/device","expires_in":600,"interval":5}"#;
const TOKEN_REPLY: &str = r#"{"access_token":"rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf","refresh_token":"rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC","token_type":"Bearer","expires_in":3600,"scope":"pull push upload change"}"#;
const ROTATED_REPLY: &str = r#"{"access_token":"rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ","refresh_token":"rdyh_p_000000000000000000000000000000000000000025UVUe","token_type":"Bearer","expires_in":3600,"scope":"pull push upload change"}"#;
const ME_REPLY: &str = r#"{"id":"acc_00000000000000000000000001","handle":"test-person","earlier_handles":[{"handle":"test-person-old","until":"2026-09-01T08:00:00Z"}],"display_name":"Test Person","kind":"person","state":"active","primary_email":null,"two_factor":true,"sign_in_sources":["local"],"organizations":[{"id":"org_00000000000000000000000004","state":"active","name":"test-org-a","role":"owner"},{"id":"org_00000000000000000000000005","state":"active","name":"test-org-b","role":"member"}],"can_create_organization":true,"version":"test-version-1","created_at":"2026-08-01T08:00:00Z"}"#;

/// Every secret a fixture hands out or seeds: none may reach any output.
const SECRETS: [&str; 5] = [ACCESS, REFRESH, ACCESS_2, REFRESH_2, DEVICE_CODE];

fn sub(template: &str, origin: &str) -> String {
    template.replace("{o}", origin)
}

/// The exact sign-in card for `install` and `machine`.
fn card(install: &str, machine: &str) -> String {
    format!(
        "Sign in to a hub\nInstall: {install}\nMachine: {machine}\nThis replaces this machine's saved sign-in for this install.\nConfirm only if you asked to sign in to this install.\nAnswer: confirm or cancel\n"
    )
}

fn card_hash(install: &str, machine: &str) -> String {
    Hash::of(card(install, machine).as_bytes()).to_string()
}

/// A private user config home: `rdy/config.yaml` names the person, and the directories are
/// 0700.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().expect("tempdir");
        set_mode(dir.path(), 0o700).unwrap();
        std::fs::create_dir(dir.path().join("rdy")).unwrap();
        set_mode(&dir.path().join("rdy"), 0o700).unwrap();
        std::fs::write(
            dir.path().join("rdy/config.yaml"),
            format!("actor: {PERSON}\n"),
        )
        .unwrap();
        Home { dir }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("rdy/config.yaml")
    }

    fn creds(&self) -> PathBuf {
        self.dir.path().join("rdy/credentials.yaml")
    }

    /// The environment of a child: this config home only.
    fn env(&self) -> Vec<(String, String)> {
        let home = self.dir.path().to_string_lossy().into_owned();
        vec![
            ("XDG_CONFIG_HOME".into(), home.clone()),
            ("HOME".into(), home),
            ("USER".into(), "qa".into()),
        ]
    }

    /// Trusts `hub`'s CA for its install in the user config.
    fn trust(&self, hub: &Server) {
        self.trust_at(hub.origin(), hub);
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

    /// Adds an install that has settings only (a pinned origin), and no credential.
    fn settings_only(&self, origin: &str) {
        let mut text = std::fs::read_to_string(self.config()).unwrap();
        if !text.contains("hub:") {
            text.push_str("hub:\n  installs:\n");
        }
        text.push_str(&format!(
            "    \"{origin}\":\n      pinned_origins: [\"{origin}\"]\n"
        ));
        std::fs::write(self.config(), text).unwrap();
    }

    fn read_creds(&self) -> CredentialFile {
        parse_credentials(&std::fs::read(self.creds()).unwrap()).unwrap()
    }

    fn write_creds(&self, file: &CredentialFile) {
        std::fs::write(self.creds(), encode_credentials(file).unwrap()).unwrap();
        set_mode(&self.creds(), 0o600).unwrap();
    }

    /// Replaces the saved credentials with `records`.
    fn seed(&self, records: Vec<(&str, Credential)>) {
        let mut file = CredentialFile::default();
        for (o, c) in records {
            file.set(Origin::parse(o).unwrap(), Some(c));
        }
        self.write_creds(&file);
    }

    /// Makes the pending sign-in of `install` due for its next check now.
    fn make_due(&self, install: &str) {
        let mut file = self.read_creds();
        let o = Origin::parse(install).unwrap();
        let Some(Credential::Pending(mut p)) = file.get(&o).cloned() else {
            panic!("no pending sign-in for {install}");
        };
        p.next_poll_at_ms = 0;
        file.set(o, Some(Credential::Pending(p)));
        self.write_creds(&file);
    }

    /// `rdy <args>` in `cwd` with this home's environment only, and `extra` on top.
    fn command(&self, cwd: &Path, extra: &[(&str, &str)], args: &[&str]) -> Command {
        let mut cmd = Command::new(rdy_bin());
        cmd.env_clear()
            .envs(self.env())
            .envs(extra.iter().copied())
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null());
        // an instrumented build (coverage) writes its raw profile where this names, and into
        // its working directory when it is unset: keep it where the test run put it
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", profile);
        }
        cmd
    }

    fn run(&self, cwd: &Path, extra: &[(&str, &str)], args: &[&str]) -> Output {
        let o = self.command(cwd, extra, args).output().expect("run rdy");
        clean(&o, args);
        o
    }

    /// `rdy --json <args>` as the person: (success, the reply or the error).
    fn json(&self, cwd: &Path, args: &[&str]) -> (bool, Value) {
        self.json_as(cwd, &[], args)
    }

    fn json_as(&self, cwd: &Path, extra: &[(&str, &str)], args: &[&str]) -> (bool, Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        parsed(&self.run(cwd, extra, &all))
    }

    /// `rdy <args>` as text: (success, stdout, stderr).
    fn text(&self, cwd: &Path, extra: &[(&str, &str)], args: &[&str]) -> (bool, String, String) {
        let o = self.run(cwd, extra, args);
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }
}

/// The arguments of `v` as `&str`.
fn args(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// Checks that no output of `rdy <args>` holds a secret, a device code or a complete link.
fn clean(o: &Output, args: &[&str]) {
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let token_door = args.contains(&"token");
    for s in SECRETS {
        if token_door && (s == ACCESS || s == ACCESS_2) {
            continue;
        }
        assert!(!all.contains(s), "rdy {args:?} printed a secret:\n{all}");
    }
    assert!(!all.contains("user_code="), "rdy {args:?}:\n{all}");
    assert!(!all.contains("verification_uri_complete"), "{all}");
    assert!(!all.contains("refresh_token"), "rdy {args:?}:\n{all}");
}

/// Whether the command succeeded, and its JSON: stdout on success, stderr on failure.
fn parsed(o: &Output) -> (bool, Value) {
    let ok = o.status.success();
    let text = String::from_utf8_lossy(if ok { &o.stdout } else { &o.stderr }).into_owned();
    let v = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
        serde_json::from_str(text.trim().lines().next().unwrap_or(""))
            .unwrap_or(Value::String(text))
    });
    (ok, v)
}

/// A hub that answers a confirmed start: its metadata, then the device start.
fn queue_start(hub: &mut Server) {
    let o = hub.origin().to_string();
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(200, &sub(DEVICE_REPLY, &o)));
    hub.accept_n(2);
}

/// A hub that answers a due check that completes: metadata, the tokens and `/me`.
fn queue_complete(hub: &mut Server) {
    let o = hub.origin().to_string();
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(200, TOKEN_REPLY));
    hub.reply(Reply::json(200, ME_REPLY));
    hub.accept_n(3);
}

/// `(method, path)` of each request.
fn routes(reqs: &[Recorded]) -> Vec<(String, String)> {
    reqs.iter()
        .map(|r| (r.method.clone(), r.target.clone()))
        .collect()
}

fn route(m: &str, p: &str) -> (String, String) {
    (m.to_string(), p.to_string())
}

const META: &str = "/.well-known/oauth-authorization-server/auth";

fn now() -> i64 {
    rdy_engine::now_ms()
}

fn ready(access: &str, refresh: &str, expires_at_ms: i64) -> Credential {
    Credential::Ready(ReadyCredential {
        access_token: Secret::new(access),
        refresh_token: Secret::new(refresh),
        expires_at_ms,
        account: ACCOUNT.into(),
        handle: "test-person".into(),
    })
}

fn pending(origin: &str, expires_at_ms: i64) -> Credential {
    Credential::Pending(PendingSignIn {
        machine_name: MACHINE.into(),
        device_code: Secret::new(DEVICE_CODE),
        user_code: USER_CODE.into(),
        verification_uri: format!("{origin}/app/device"),
        expires_at_ms,
        next_poll_at_ms: expires_at_ms.min(now() + 5_000),
        interval_seconds: 5,
    })
}

fn err_code(v: &Value) -> &str {
    v["code"].as_str().unwrap_or("")
}

/// The card of a text sign-in request: the six lines, the hash line and the answer line.
fn text_card(stdout: &str, install: &str) -> String {
    let want = format!(
        "{}card hash: {}\nanswer with --answer and --card-hash\n",
        card(install, MACHINE),
        card_hash(install, MACHINE)
    );
    assert_eq!(stdout, want);
    card_hash(install, MACHINE)
}

#[test]
fn hsa8_cli_card_confirmation_has_no_agent_bypass() {
    let home = Home::new();
    let cwd = tempfile::tempdir().unwrap();
    let c = cwd.path();
    let mut hub = Server::start();
    home.trust(&hub);
    let o = hub.origin().to_string();
    let signin = [
        "auth",
        "sign-in",
        "--install",
        &o,
        "--machine-name",
        MACHINE,
    ];
    let with = |extra: &[&str]| {
        let mut a: Vec<String> = signin.iter().map(|s| s.to_string()).collect();
        a.extend(extra.iter().map(|s| s.to_string()));
        a
    };

    // the person: the card, in text, with its hash
    let (ok, out, _) = home.text(c, &[], &signin);
    assert!(ok);
    let hash = text_card(&out, &o);
    let card_args = with(&["--card-hash", &hash]);

    // the person answers as themselves: cancel needs no request and writes nothing
    let (ok, out, _) = home.text(
        c,
        &[],
        &args(&with(&["--card-hash", &hash, "--answer", "cancel"])),
    );
    assert!(ok, "{out}");
    assert_eq!(out, format!("sign-in cancelled for {o}\n"));
    // a person who names a relay is refused
    let (ok, e) = home.json(
        c,
        &args(&with(&[
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
            "--relayed-by",
            PERSON,
            "--evidence",
            "yes",
        ])),
    );
    assert!(!ok);
    assert_eq!(
        (err_code(&e), e["message"].as_str().unwrap()),
        (
            "E_PERMISSION",
            "a person answers as themselves; only the relaying agent records a relay"
        )
    );

    // an agent: the relay table decides, every refusal before any request or write
    let agent: &[(&str, &str)] = &[("RDY_ACTOR", "agent:bot"), ("RDY_RUN", "r1")];
    let refusals: Vec<(Vec<String>, &str, &str)> = vec![
        (
            with(&["--card-hash", &hash, "--answer", "confirm"]),
            "E_PERMISSION",
            "an agent answers only by relaying a person's words",
        ),
        (
            with(&[
                "--card-hash",
                &hash,
                "--answer",
                "confirm",
                "--evidence",
                "yes, sign in",
            ]),
            "E_BAD_INPUT",
            "relayed_by and evidence go together",
        ),
        (
            with(&[
                "--card-hash",
                &hash,
                "--answer",
                "confirm",
                "--relayed-by",
                "agent:other",
                "--evidence",
                "yes, sign in",
            ]),
            "E_PERMISSION",
            "agent:bot relays only as itself, not as agent:other",
        ),
        (
            with(&[
                "--card-hash",
                &hash,
                "--answer",
                "confirm",
                "--relayed-by",
                PERSON,
                "--evidence",
                "yes, sign in",
            ]),
            "E_PERMISSION",
            "agent:bot relays only as itself, not as human:qa",
        ),
        (
            with(&[
                "--card-hash",
                &"0".repeat(64),
                "--answer",
                "confirm",
                "--relayed-by",
                "agent:bot/r1",
                "--evidence",
                "yes, sign in",
            ]),
            "E_UPSTREAM_CONFIRM_HOST",
            "the install host has not been confirmed",
        ),
        (
            with(&[
                "--card-hash",
                &hash.to_uppercase(),
                "--answer",
                "confirm",
                "--relayed-by",
                "agent:bot/r1",
                "--evidence",
                "yes, sign in",
            ]),
            "E_UPSTREAM_CONFIRM_HOST",
            "the install host has not been confirmed",
        ),
        (
            with(&[
                "--answer",
                "confirm",
                "--relayed-by",
                "agent:bot",
                "--evidence",
                "yes",
            ]),
            "E_BAD_INPUT",
            "auth start needs card_hash",
        ),
        (
            with(&["--relayed-by", "agent:bot", "--evidence", "yes"]),
            "E_BAD_INPUT",
            "auth start needs card_hash",
        ),
        (card_args.clone(), "E_BAD_INPUT", "auth start needs answer"),
        (
            with(&[
                "--card-hash",
                &hash,
                "--answer",
                "confirm",
                "--relayed-by",
                "agent:bot",
                "--question",
                "sign in?",
            ]),
            "E_BAD_INPUT",
            "auth start needs evidence",
        ),
    ];
    for (a, code, message) in &refusals {
        let (ok, e) = home.json_as(c, agent, &args(a));
        assert!(!ok, "{a:?}");
        assert_eq!(
            (err_code(&e), e["message"].as_str().unwrap_or("")),
            (*code, *message),
            "{a:?}: {e}"
        );
    }
    // the same card hash under another machine name or install is not that card
    let other_machine = [
        "auth",
        "sign-in",
        "--install",
        &o,
        "--machine-name",
        "desk",
        "--card-hash",
        &hash,
        "--answer",
        "confirm",
        "--relayed-by",
        "agent:bot",
        "--evidence",
        "yes",
    ];
    let (ok, e) = home.json_as(c, agent, &other_machine);
    assert!(!ok);
    assert_eq!(err_code(&e), "E_UPSTREAM_CONFIRM_HOST");
    let other = "https://127.0.0.1:9".to_string();
    let other_install = [
        "auth",
        "sign-in",
        "--install",
        &other,
        "--machine-name",
        MACHINE,
        "--card-hash",
        &hash,
        "--answer",
        "confirm",
        "--relayed-by",
        "agent:bot",
        "--evidence",
        "yes",
    ];
    let (ok, e) = home.json_as(c, agent, &other_install);
    assert!(!ok);
    assert_eq!(err_code(&e), "E_UPSTREAM_CONFIRM_HOST", "{e}");
    // no switch skips the card or the relay, and there is no token input
    for flag in [
        "--yes",
        "--force",
        "--confirmed-host",
        "--token",
        "--approve",
        "--open",
    ] {
        let mut a = with(&["--card-hash", &hash, "--answer", "confirm"]);
        a.push(flag.to_string());
        a.push("x".to_string());
        let o = home.command(c, agent, &args(&a)).output().unwrap();
        assert!(!o.status.success(), "{flag}");
        assert_eq!(o.status.code(), Some(2), "{flag}: a usage error");
    }
    for sub in ["login", "logout"] {
        let o = home.command(c, &[], &["auth", sub]).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "no {sub} alias");
    }
    assert!(
        !home.creds().exists(),
        "no refusal or cancel wrote a credential"
    );
    assert!(hub.requests().is_empty(), "no refusal sent a request");

    // a relayed cancel, then a relayed confirm: one sign-in starts, as the relay checked
    let (ok, v) = home.json_as(
        c,
        agent,
        &args(&with(&[
            "--card-hash",
            &hash,
            "--answer",
            "cancel",
            "--relayed-by",
            "agent:bot/r1",
            "--evidence",
            "no, cancel it",
        ])),
    );
    assert!(ok, "{v}");
    assert_eq!(v["state"], "cancelled");
    queue_start(&mut hub);
    let (ok, out, _) = home.text(
        c,
        agent,
        &args(&with(&[
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
            "--relayed-by",
            "agent:bot",
            "--evidence",
            "yes, sign in",
            "--question",
            "Sign in to the hub?",
        ])),
    );
    assert!(ok, "{out}");
    assert_eq!(
        out,
        format!(
            "sign-in pending for {o}\ncode: {USER_CODE}\nopen: {o}/app/device\ncheck again with rdy auth state --install {o}\n"
        )
    );
    assert_eq!(
        routes(&hub.requests()),
        [route("GET", META), route("POST", "/auth/device")]
    );
    assert_eq!(hub.finish(), 0);
}

#[test]
fn hsa8_cli_auth_works_without_a_project_or_daemon() {
    // an empty directory, an empty `.rdy/`, a damaged project and a missing --project: the
    // same replies, with and without --no-daemon, and nothing is created there
    let empty = tempfile::tempdir().unwrap();
    let bare = tempfile::tempdir().unwrap();
    std::fs::create_dir(bare.path().join(".rdy")).unwrap();
    let damaged = tempfile::tempdir().unwrap();
    std::fs::create_dir(damaged.path().join(".rdy")).unwrap();
    std::fs::write(damaged.path().join(".rdy.yaml"), "pack: [unclosed\n").unwrap();
    std::fs::write(damaged.path().join(".rdy/store"), "not a store").unwrap();
    let mut transcripts: Vec<Vec<Value>> = Vec::new();
    for (place, flags) in [
        (empty.path(), vec![]),
        (bare.path(), vec!["--no-daemon"]),
        (damaged.path(), vec![]),
        (
            damaged.path(),
            vec!["--no-daemon", "--project", "/nonexistent/rdy-project"],
        ),
    ] {
        let before = listing(place);
        let home = Home::new();
        let mut hub = Server::start();
        home.trust(&hub);
        let o = hub.origin().to_string();
        let run = |args: &[&str]| {
            let mut a: Vec<&str> = flags.clone();
            a.extend_from_slice(args);
            let (ok, v) = home.json(place, &a);
            assert!(ok, "{a:?} in {}: {v}", place.display());
            v
        };
        let mut seen = Vec::new();
        let card_reply = run(&[
            "auth",
            "sign-in",
            "--install",
            &o,
            "--machine-name",
            MACHINE,
        ]);
        assert_eq!(card_reply["state"], "confirmation_required");
        let hash = card_reply["card_hash"].as_str().unwrap().to_string();
        assert_eq!(hash, card_hash(&o, MACHINE));
        seen.push(card_reply);
        queue_start(&mut hub);
        seen.push(run(&[
            "auth",
            "sign-in",
            "--install",
            &o,
            "--machine-name",
            MACHINE,
            "--card-hash",
            &hash,
            "--answer",
            "confirm",
        ]));
        seen.push(run(&["auth", "state", "--install", &o]));
        seen.push(run(&["auth", "installs"]));
        seen.push(run(&["auth", "sign-out", "--install", &o]));
        seen.push(run(&["auth", "state", "--install", &o]));
        seen.push(run(&["auth", "installs"]));
        // the text forms too
        let mut text_flags = flags.clone();
        text_flags.extend(["auth", "installs"]);
        let (ok, out, _) = home.text(place, &[], &text_flags);
        assert!(ok);
        assert_eq!(out, "no saved hub sign-ins\n");
        let mut text_flags = flags.clone();
        text_flags.extend(["auth", "state", "--install", &o]);
        let (ok, out, _) = home.text(place, &[], &text_flags);
        assert!(ok);
        assert_eq!(out, format!("signed out of {o}\n"));
        assert_eq!(
            routes(&hub.requests()),
            [route("GET", META), route("POST", "/auth/device")],
            "only the confirmed start sent requests"
        );
        assert_eq!(hub.finish(), 0);
        assert_eq!(
            listing(place),
            before,
            "nothing was made in {}",
            place.display()
        );
        transcripts.push(seen.iter().map(|v| normalized(v, &o)).collect());
    }
    for t in &transcripts[1..] {
        assert_eq!(t, &transcripts[0]);
    }
    let states: Vec<&str> = transcripts[0]
        .iter()
        .map(|v| v["state"].as_str().unwrap_or("rows"))
        .collect();
    assert_eq!(
        states,
        [
            "confirmation_required",
            "pending",
            "pending",
            "rows",
            "signed_out",
            "signed_out",
            "rows"
        ]
    );
    assert_eq!(transcripts[0][3]["installs"][0]["state"], "pending");
    assert_eq!(transcripts[0][6], json!({"installs": [], "warnings": []}));
}

/// Every path under `dir`, sorted.
fn listing(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out.sort();
    out
}

/// The kind of each JSON value, recursively: keys, nulls and integer types stay; every scalar
/// value becomes its type name.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(shape).collect()),
        Value::Null => Value::Null,
        Value::Bool(_) => json!("bool"),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!("integer"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
    }
}

/// `v` with the fixture's origin as `{o}` and every `expires_at_ms` and `next_poll_at_ms`
/// value as `"time"`: the only parts two fixtures may differ in. A card and its hash stay, and
/// the hash is checked against the card's exact bytes.
fn normalized(v: &Value, origin: &str) -> Value {
    if let (Some(c), Some(h)) = (v["card"].as_str(), v["card_hash"].as_str()) {
        assert_eq!(
            Hash::of(c.as_bytes()).to_string(),
            h,
            "the hash is of the card"
        );
    }
    fn walk(v: &Value, origin: &str) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| {
                        let x = match k.as_str() {
                            "expires_at_ms" | "next_poll_at_ms" if x.is_i64() => json!("time"),
                            "card_hash" => json!("hash"),
                            _ => walk(x, origin),
                        };
                        (k.clone(), x)
                    })
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, origin)).collect()),
            Value::String(s) => Value::String(s.replace(origin, "{o}")),
            other => other.clone(),
        }
    }
    walk(v, origin)
}

#[test]
fn hsa8_cli_and_mcp_auth_replies_match() {
    // the command line as the person, and chat as an agent relaying the person
    let cli_home = Home::new();
    let mut cli_hub = Server::start();
    cli_home.trust(&cli_hub);
    let co = cli_hub.origin().to_string();
    let cwd = tempfile::tempdir().unwrap();
    let c = cwd.path();
    let cli = |args: &[&str]| {
        let (ok, v) = cli_home.json(c, args);
        assert!(ok, "{args:?}: {v}");
        v
    };
    let mcp_home = Home::new();
    let mut mcp_hub = Server::start();
    mcp_home.trust(&mcp_hub);
    let mo = mcp_hub.origin().to_string();
    let (dir, r) = project();
    drop(r);
    let env = mcp_home.env();
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut chat = McpClient::start_with_env(dir.path(), &pairs);
    assert!(
        chat.call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "qa", "role": "writer"})
        )
        .is_ok()
    );
    let mut mcp = |args: Value| match chat.call("rdy_auth", args.clone()) {
        McpReply::Ok(v) => v,
        other => panic!("{args}: {other:?}"),
    };

    let mut pairs_seen: Vec<(Value, Value)> = Vec::new();
    // the card
    let a = cli(&[
        "auth",
        "sign-in",
        "--install",
        &co,
        "--machine-name",
        MACHINE,
    ]);
    let b = mcp(json!({"op": "start", "install": mo, "machine_name": MACHINE}));
    assert_eq!(a["card"], json!(card(&co, MACHINE)));
    assert_eq!(b["card"], json!(card(&mo, MACHINE)));
    pairs_seen.push((a.clone(), b.clone()));
    // the confirmed start
    queue_start(&mut cli_hub);
    queue_start(&mut mcp_hub);
    let ch = a["card_hash"].as_str().unwrap();
    let mh = b["card_hash"].as_str().unwrap();
    pairs_seen.push((
        cli(&[
            "auth",
            "sign-in",
            "--install",
            &co,
            "--machine-name",
            MACHINE,
            "--card-hash",
            ch,
            "--answer",
            "confirm",
        ]),
        mcp(
            json!({"op": "start", "install": mo, "machine_name": MACHINE, "card_hash": mh,
            "answer": "confirm", "evidence": "yes, sign in"}),
        ),
    ));
    // pending, not yet due
    pairs_seen.push((
        cli(&["auth", "state", "--install", &co]),
        mcp(json!({"op": "state", "install": mo})),
    ));
    // due: the check completes
    cli_home.make_due(&co);
    mcp_home.make_due(&mo);
    queue_complete(&mut cli_hub);
    queue_complete(&mut mcp_hub);
    pairs_seen.push((
        cli(&["auth", "state", "--install", &co]),
        mcp(json!({"op": "state", "install": mo})),
    ));
    pairs_seen.push((cli(&["auth", "installs"]), mcp(json!({"op": "installs"}))));
    // a cancel, then sign-out (its revoke answered)
    pairs_seen.push((
        cli(&[
            "auth",
            "sign-in",
            "--install",
            &co,
            "--machine-name",
            MACHINE,
            "--card-hash",
            ch,
            "--answer",
            "cancel",
        ]),
        mcp(
            json!({"op": "start", "install": mo, "machine_name": MACHINE, "card_hash": mh,
            "answer": "cancel", "evidence": "no"}),
        ),
    ));
    for (hub, o) in [(&mut cli_hub, &co), (&mut mcp_hub, &mo)] {
        hub.reply(Reply::json(200, &sub(METADATA_REPLY, o)));
        hub.reply(Reply::empty(200));
        hub.accept_n(2);
    }
    pairs_seen.push((
        cli(&["auth", "sign-out", "--install", &co]),
        mcp(json!({"op": "sign_out", "install": mo})),
    ));
    pairs_seen.push((cli(&["auth", "installs"]), mcp(json!({"op": "installs"}))));
    let states: Vec<Value> = pairs_seen.iter().map(|(a, _)| a["state"].clone()).collect();
    assert_eq!(
        states,
        [
            json!("confirmation_required"),
            json!("pending"),
            json!("pending"),
            json!("signed_in"),
            Value::Null,
            json!("cancelled"),
            json!("signed_out"),
            Value::Null
        ]
    );
    for (a, b) in &pairs_seen {
        assert_eq!(shape(a), shape(b), "the same keys, nulls and integer types");
        assert_eq!(normalized(a, &co), normalized(b, &mo));
    }
    let signed_in = &pairs_seen[3].0;
    assert_eq!(signed_in["human_actor"], "human:test-person");
    assert_eq!(signed_in["account"], ACCOUNT);
    assert_eq!(
        pairs_seen[1].0["verification_uri"],
        json!(format!("{co}/app/device"))
    );
    for (a, b) in &pairs_seen {
        for s in SECRETS {
            assert!(
                !a.to_string().contains(s) && !b.to_string().contains(s),
                "{a} {b}"
            );
        }
    }
    assert_eq!(routes(&cli_hub.requests()), routes(&mcp_hub.requests()));
    assert_eq!(cli_hub.finish(), 0);
    assert_eq!(mcp_hub.finish(), 0);
}

/// A loopback TCP gate in front of `upstream`: it accepts `n` connections in order and pipes
/// each to `upstream`; it reports connection `hold` on `arrived` and holds it, unconnected,
/// until `release` gets a message.
struct Gate {
    origin: String,
    arrived: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Gate {
    fn start(upstream: &str, n: usize, hold: usize) -> Gate {
        let up_port: u16 = upstream.rsplit(':').next().unwrap().parse().unwrap();
        let listener = TcpListener::bind((hub_tls::LOOPBACK, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (arrived_tx, arrived) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            for i in 0..n {
                let (client, _) = listener.accept().expect("accept");
                if i == hold {
                    arrived_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                let server = TcpStream::connect((hub_tls::LOOPBACK, up_port)).unwrap();
                std::thread::spawn(move || pipe(client, server));
            }
        });
        Gate {
            origin: format!("https://{}:{port}", hub_tls::LOOPBACK),
            arrived,
            release,
            thread,
        }
    }

    /// Waits until the consumer's refresh request is held, then checks that nothing is on its
    /// pipe and that the old pair has already left the file of `install`.
    fn held(&self, home: &Home, install: &str, consumer: &Consumer) {
        self.arrived
            .recv_timeout(Duration::from_secs(60))
            .expect("the refresh request arrives");
        assert!(
            matches!(consumer.chunks.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "nothing reaches the pipe while the refresh is held"
        );
        assert_eq!(
            home.read_creds().get(&Origin::parse(install).unwrap()),
            Some(&Credential::ReauthRequired),
            "the refresh token is sent only from a record that no longer holds it"
        );
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

/// A credential consumer: `rdy --json auth token` running with its stdout on a pipe that this
/// test reads as the bytes arrive, while the child still runs.
struct Consumer {
    child: std::process::Child,
    chunks: mpsc::Receiver<Vec<u8>>,
}

impl Consumer {
    fn start(mut cmd: Command) -> Consumer {
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rdy");
        let mut stdout = child.stdout.take().unwrap();
        let (tx, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Consumer { child, chunks }
    }

    /// The next bytes on the pipe, or `None` once it has closed with nothing more.
    fn next(&self) -> Option<Vec<u8>> {
        self.chunks.recv_timeout(Duration::from_secs(60)).ok()
    }

    /// Waits for the child: its output, with `first` and the rest of the pipe as stdout,
    /// checked by `clean`.
    fn finish(mut self, first: Vec<u8>, args: &[&str]) -> Output {
        let mut stdout = first;
        while let Some(more) = self.next() {
            stdout.extend(more);
        }
        let mut stderr = Vec::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        let status = self.child.wait().unwrap();
        let o = Output {
            status,
            stdout,
            stderr,
        };
        clean(&o, args);
        o
    }
}

#[test]
fn hsa8_token_consumer_gets_access_only_after_durable_refresh() {
    let home = Home::new();
    let cwd = tempfile::tempdir().unwrap();
    let c = cwd.path();
    let hub = Server::start();
    home.trust(&hub);
    let o = hub.origin().to_string();
    let token = ["--json", "auth", "token", "--install", &o];

    // not signed in: the shared failure, nothing on stdout
    let out = home.run(c, &[], &token);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(parsed(&out).1["code"], "E_UPSTREAM_AUTH");

    // fresh access is returned from the file, with no request, as one JSON line
    let later = now() + 3_000_000;
    home.seed(vec![(&o, ready(ACCESS, REFRESH, later))]);
    let out = home.run(c, &[], &token);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    assert_eq!(stdout.matches('\n').count(), 1, "one line: {stdout}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap(),
        json!({"install": o, "access_token": ACCESS, "expires_at_ms": later,
            "human_actor": "human:test-person"})
    );
    assert!(hub.requests().is_empty());
    assert_eq!(hub.finish(), 0);

    // expiring access: the refresh is sent once, the rotated pair is saved, and only then is
    // the new access printed; the refresh token never leaves. A gate holds the refresh request
    // while the consumer's pipe is watched: nothing is on it, and the old pair has already left
    // the file; when access appears on the pipe, the rotated pair is already saved
    let mut hub = Server::start();
    let gate = Gate::start(hub.origin(), 2, 1);
    let o = gate.origin.clone();
    home.trust_at(&o, &hub);
    home.seed(vec![(&o, ready(ACCESS, REFRESH, now() + 1_000))]);
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(200, ROTATED_REPLY));
    hub.accept_n(2);
    let token = ["--json", "auth", "token", "--install", &o];
    let consumer =
        Consumer::start(home.command(c, &[("RDY_ACTOR", "agent:sync"), ("RDY_RUN", "s1")], &token));
    gate.held(&home, &o, &consumer);
    gate.release.send(()).unwrap();
    let first = consumer.next().expect("access reaches the pipe");
    let saved = home.read_creds();
    let Some(Credential::Ready(r)) = saved.get(&Origin::parse(&o).unwrap()) else {
        panic!("the rotated pair is saved before access reaches the pipe");
    };
    assert_eq!(r.access_token.expose(), ACCESS_2);
    let out = consumer.finish(first, &token);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    assert_eq!(stdout.matches('\n').count(), 1, "one line: {stdout}");
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let saved = home.read_creds();
    let Some(Credential::Ready(r)) = saved.get(&Origin::parse(&o).unwrap()) else {
        panic!("the rotated pair is saved");
    };
    assert_eq!(printed["access_token"], ACCESS_2);
    assert_eq!(
        r.access_token.expose(),
        ACCESS_2,
        "the printed access is the saved one"
    );
    assert_eq!(r.refresh_token.expose(), REFRESH_2);
    assert_eq!(printed["expires_at_ms"], json!(r.expires_at_ms));
    assert_eq!(
        printed.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["access_token", "expires_at_ms", "human_actor", "install"]
    );
    let reqs = hub.requests();
    assert_eq!(
        routes(&reqs),
        [route("GET", META), route("POST", "/auth/token")]
    );
    assert!(reqs[1].body_text().contains("grant_type=refresh_token"));
    assert!(reqs[1].body_text().contains(REFRESH));
    assert!(
        reqs.iter().all(|r| r.header("rdy-actor").is_none()),
        "no claim on OAuth routes"
    );
    gate.thread.join().unwrap();
    assert_eq!(hub.finish(), 0);

    // the final save fails: the hub rotated the pair, but this machine could not keep it, so
    // no access reaches the pipe. While the refresh is held, after the old pair has left the
    // file, a directory is put at the replacement's temp name: the save cannot remove or create
    // it, whatever the user id (a read-only directory would not stop root)
    let mut hub = Server::start();
    let gate = Gate::start(hub.origin(), 2, 1);
    let o = gate.origin.clone();
    home.trust_at(&o, &hub);
    home.seed(vec![(&o, ready(ACCESS, REFRESH, now() + 1_000))]);
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(200, ROTATED_REPLY));
    hub.accept_n(2);
    let token = ["--json", "auth", "token", "--install", &o];
    let consumer = Consumer::start(home.command(c, &[], &token));
    gate.held(&home, &o, &consumer);
    let blocker = home.creds().with_file_name(".credentials.yaml.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), "").unwrap();
    gate.release.send(()).unwrap();
    let rest = consumer.next();
    assert_eq!(
        rest.map(|b| String::from_utf8_lossy(&b).into_owned()),
        None,
        "no access reaches the pipe"
    );
    let out = consumer.finish(Vec::new(), &token);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(parsed(&out).1["code"], "E_UPSTREAM_AUTH");
    assert!(blocker.is_dir(), "the save failed at its temp name");
    std::fs::remove_dir_all(&blocker).unwrap();
    assert_eq!(
        home.read_creds().get(&Origin::parse(&o).unwrap()),
        Some(&Credential::ReauthRequired),
        "the rotated pair was not kept, and the old one is gone"
    );
    assert_eq!(
        routes(&hub.requests()),
        [route("GET", META), route("POST", "/auth/token")]
    );
    gate.thread.join().unwrap();
    assert_eq!(hub.finish(), 0);

    // a refused refresh: no access is printed, and the old refresh token is not kept
    let mut hub = Server::start();
    home.trust(&hub);
    let o = hub.origin().to_string();
    home.seed(vec![(&o, ready(ACCESS, REFRESH, now() + 1_000))]);
    hub.reply(Reply::json(200, &sub(METADATA_REPLY, &o)));
    hub.reply(Reply::json(400, r#"{"error":"invalid_grant"}"#));
    hub.accept_n(2);
    let out = home.run(c, &[], &["--json", "auth", "token", "--install", &o]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no partial success on stdout");
    assert_eq!(parsed(&out).1["code"], "E_UPSTREAM_AUTH");
    assert_eq!(
        home.read_creds().get(&Origin::parse(&o).unwrap()),
        Some(&Credential::ReauthRequired)
    );
    assert_eq!(hub.finish(), 0);

    // no --json: refused before anything is read or sent, even to a pipe
    let out = home.run(c, &[], &["auth", "token", "--install", &o]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error E_BAD_INPUT: auth token needs --json and a pipe for its output\nfix: run this command from the credential consumer with piped output\n"
    );
}

/// One installs round: the saved records, the JSON rows and the text.
type Round<'a> = (Vec<(&'a str, Credential)>, Value, String);

#[test]
fn hsa8_installs_lists_credentials_without_exporting_them() {
    let home = Home::new();
    let cwd = tempfile::tempdir().unwrap();
    let c = cwd.path();
    let a = "https://127.0.0.1:1";
    let b = "https://127.0.0.1:2";
    let settings_only = "https://127.0.0.1:3";
    home.settings_only(settings_only);
    let (ok, v) = home.json(c, &["auth", "installs"]);
    assert!(ok, "{v}");
    assert_eq!(
        v,
        json!({"installs": [], "warnings": []}),
        "settings make no row"
    );

    let later = now() + 3_000_000;
    let pending_end = now() + 500_000;
    // every saved state across two installs: signed in and pending, then reauth and expired
    let rounds: Vec<Round> = vec![
        (
            vec![
                (b, pending(b, pending_end)),
                (a, ready(ACCESS, REFRESH, later)),
            ],
            json!([
                {"install": a, "state": "signed_in", "account": ACCOUNT,
                 "human_actor": "human:test-person", "expires_at_ms": later},
                {"install": b, "state": "pending", "account": null, "human_actor": null,
                 "expires_at_ms": pending_end},
            ]),
            format!("{a} signed_in human:test-person\n{b} pending -\n"),
        ),
        (
            vec![(a, Credential::ReauthRequired), (b, pending(b, 1_000))],
            json!([
                {"install": a, "state": "reauth_required", "account": null, "human_actor": null,
                 "expires_at_ms": null},
                {"install": b, "state": "signed_out", "account": null, "human_actor": null,
                 "expires_at_ms": null},
            ]),
            format!("{a} reauth_required -\n{b} signed_out -\n"),
        ),
    ];
    let (dir, r) = project();
    drop(r);
    let env = home.env();
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut chat = McpClient::start_with_env(dir.path(), &pairs);
    assert!(
        chat.call(
            "rdy_session",
            json!({"actor": "agent:qa", "run": "qa", "role": "reader"})
        )
        .is_ok()
    );
    for (records, rows, text) in rounds {
        home.seed(records);
        let before = std::fs::read(home.creds()).unwrap();
        let (ok, v) = home.json(c, &["auth", "installs"]);
        assert!(ok, "{v}");
        assert_eq!(v, json!({"installs": rows, "warnings": []}));
        let (ok, out, _) = home.text(c, &[], &["auth", "installs"]);
        assert!(ok);
        assert_eq!(out, text);
        let McpReply::Ok(m) = chat.call("rdy_auth", json!({"op": "installs"})) else {
            panic!("installs over MCP");
        };
        assert_eq!(m, v, "the same reply over MCP");
        for s in SECRETS {
            assert!(!m.to_string().contains(s));
        }
        assert_eq!(
            std::fs::read(home.creds()).unwrap(),
            before,
            "listing changes no record"
        );
    }
}

/// Adds an ordinary key rdy does not know (`actro`) and an unknown hub key (`hub.instalss`) to
/// the user config, and returns the warnings rdy finds in that file alone.
fn mistype(home: &Home) -> Vec<rdy_schema::Issue> {
    let text = std::fs::read_to_string(home.config()).unwrap();
    let text = if text.contains("hub:\n") {
        text.replacen("hub:\n", "hub:\n  instalss: {}\n", 1)
    } else {
        format!("{text}hub:\n  instalss: {{}}\n")
    };
    std::fs::write(home.config(), format!("{text}actro: human:wrong\n")).unwrap();
    let warnings = rdy_engine::config_levels::user_config_warnings(&home.config());
    let found: Vec<bool> = ["actro", "hub.instalss"]
        .iter()
        .map(|k| warnings.iter().any(|w| w.message.contains(k)))
        .collect();
    assert_eq!(found, [true, true], "{warnings:?}");
    warnings
}

/// The warnings as the CLI prints them on stderr, one line each.
fn warning_lines(warnings: &[rdy_schema::Issue]) -> String {
    warnings
        .iter()
        .map(|w| format!("warning {}: {}; {}\n", w.code, w.message, w.fix))
        .collect()
}

/// `rdy auth` has no project, so it reports the user config's warnings itself: every one of
/// them, an ordinary unknown key as well as a hub key. A state reply carries them in its
/// warnings, as `rdy_auth` does: in its JSON, and after its text on stdout. The installs list and
/// the token door, which carry none, are followed by them on stderr. After a failure they are the
/// error's issues. The token door's stdout stays its one line.
#[test]
fn hsa8_auth_relays_user_config_warnings() {
    let home = Home::new();
    let cwd = tempfile::tempdir().unwrap();
    let c = cwd.path();
    let hub = Server::start();
    home.trust(&hub);
    let o = hub.origin().to_string();
    let warnings = mistype(&home);
    let lines = warning_lines(&warnings);
    let issues = serde_json::to_value(&warnings).unwrap();
    let start = [
        "auth",
        "sign-in",
        "--install",
        &o,
        "--machine-name",
        MACHINE,
    ];

    // a card, as text and as JSON: the reply and its warnings on stdout, once
    let (ok, out, err) = home.text(c, &[], &start);
    assert!(ok, "{err}");
    assert!(out.starts_with(&card(&o, MACHINE)), "{out}");
    assert!(out.ends_with(&format!("--card-hash\n{lines}")), "{out}");
    assert_eq!(err, "");
    let mut json_start = vec!["--json"];
    json_start.extend_from_slice(&start);
    let o_ = home.run(c, &[], &json_start);
    assert!(o_.status.success());
    let stdout = String::from_utf8(o_.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["state"], "confirmation_required");
    assert_eq!(
        v["warnings"], issues,
        "the reply carries the config's warnings"
    );
    assert_eq!(String::from_utf8(o_.stderr).unwrap(), "");

    // failures after the config is read carry the warnings as their issues: a wrong answer,
    // a wrong card and a missing sign-in at the token door
    let wrong = "0".repeat(64);
    let failures: Vec<(Vec<&str>, &str)> = vec![
        (
            vec![
                "auth",
                "sign-in",
                "--install",
                &o,
                "--machine-name",
                MACHINE,
                "--answer",
                "confirm",
            ],
            "E_BAD_INPUT",
        ),
        (
            vec![
                "auth",
                "sign-in",
                "--install",
                &o,
                "--machine-name",
                MACHINE,
                "--card-hash",
                &wrong,
                "--answer",
                "confirm",
            ],
            "E_UPSTREAM_CONFIRM_HOST",
        ),
        (vec!["auth", "token", "--install", &o], "E_UPSTREAM_AUTH"),
    ];
    for (args, code) in &failures {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = home.run(c, &[], &all);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let (_, e) = parsed(&out);
        assert_eq!(e["code"], *code, "{e}");
        assert_eq!(e["issues"], issues, "{args:?}: {e}");
    }
    // as text, each issue follows the error
    let (ok, out, err) = home.text(c, &[], &failures[1].0);
    assert!(!ok && out.is_empty());
    assert!(err.starts_with("error E_UPSTREAM_CONFIRM_HOST: "), "{err}");
    for w in &warnings {
        assert!(
            err.contains(&format!(
                "  {} None: {} (fix: {})",
                w.code, w.message, w.fix
            )),
            "{err}"
        );
    }

    // the token door: exactly its line on stdout, the warnings on stderr
    let later = now() + 3_000_000;
    home.seed(vec![(&o, ready(ACCESS, REFRESH, later))]);
    let out = home.run(c, &[], &["--json", "auth", "token", "--install", &o]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.matches('\n').count(), 1, "one line: {stdout}");
    assert!(stdout.ends_with('\n'), "{stdout}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap(),
        json!({"install": o, "access_token": ACCESS, "expires_at_ms": later,
            "human_actor": "human:test-person"})
    );
    assert_eq!(String::from_utf8(out.stderr).unwrap(), lines);
    // installs adds the same lines on stderr; state carries them after its text, and in JSON
    let (ok, _, err) = home.text(c, &[], &["auth", "installs"]);
    assert!(ok);
    assert_eq!(err, lines);
    let (ok, out, err) = home.text(c, &[], &["auth", "state", "--install", &o]);
    assert!(ok);
    assert_eq!(
        out,
        format!("signed in to {o} as human:test-person\n{lines}")
    );
    assert_eq!(err, "");
    let (ok, v) = home.json(c, &["auth", "state", "--install", &o]);
    assert!(ok);
    assert_eq!(v["warnings"], issues);
    assert!(hub.requests().is_empty(), "no request was needed");
    assert_eq!(hub.finish(), 0);
}

/// The codes of a reply's warnings, in order.
fn warning_codes(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("no warnings in {v}"))
        .iter()
        .map(|w| w["code"].as_str().unwrap_or("").to_string())
        .collect()
}

/// Sign-out removes this machine's saved sign-in even when the settings do not load: a user
/// config that does not parse, or one with two share remotes. The record is removed durably
/// first; the hub sign-out, which needs the settings, then cannot be confirmed, so the reply is
/// signed_out with `W_UPSTREAM_SIGNOUT_LOCAL`, and no request is sent.
#[test]
fn hsa8_cli_sign_out_is_local_first_whatever_the_settings() {
    let cwd = tempfile::tempdir().unwrap();
    let c = cwd.path();
    for broken in [
        format!("actor: [{PERSON}\n"),
        "remotes:\n  a: {url: \"https://a.example.com\", mode: share}\n  b: {url: \"https://b.example.com\", mode: share}\n".to_string(),
    ] {
        let home = Home::new();
        let hub = Server::start();
        home.trust(&hub);
        let o = hub.origin().to_string();
        let text = std::fs::read_to_string(home.config()).unwrap();
        std::fs::write(home.config(), format!("{text}{broken}")).unwrap();
        // the settings are refused by a command that needs them
        let (ok, e) = home.json(c, &["auth", "state", "--install", &o]);
        assert!(!ok, "{broken}: {e}");
        home.seed(vec![(&o, ready(ACCESS, REFRESH, now() + 3_000_000))]);
        let (ok, v) = home.json(c, &["auth", "sign-out", "--install", &o]);
        assert!(ok, "{broken}: {v}");
        assert_eq!((v["install"].as_str(), v["state"].as_str()), (Some(o.as_str()), Some("signed_out")));
        assert_eq!(warning_codes(&v), ["W_UPSTREAM_SIGNOUT_LOCAL"], "{v}");
        assert!(home.read_creds().is_empty(), "{broken}: the record is removed");
        // as text, again with nothing saved: a no-op reply
        let (ok, out, err) = home.text(c, &[], &["auth", "sign-out", "--install", &o]);
        assert!(ok, "{err}");
        assert_eq!(out, format!("signed out of {o}\n"));
        assert!(hub.requests().is_empty(), "no request without the settings");
        assert_eq!(hub.finish(), 0);
    }
}

/// An explicit human actor signs out when the settings fail only their remote checks: the
/// user config still reads within its bounds, so its `actor` is the person, and `RDY_ACTOR`
/// naming it is that person, not `human:$USER`. The record is removed durably, then the reply
/// is signed_out with `W_UPSTREAM_SIGNOUT_LOCAL`. A config that does not parse names no actor,
/// so the same caller stays refused there and the record is unchanged.
#[test]
fn hsa8_cli_sign_out_keeps_the_configured_actor_when_remotes_fail() {
    let cwd = tempfile::tempdir().unwrap();
    let c = cwd.path();
    let human: &[(&str, &str)] = &[("RDY_ACTOR", "human:test-person")];
    let shares = "remotes:\n  a: {url: \"https://a.example.com\", mode: share}\n  b: {url: \"https://b.example.com\", mode: share}\n";
    let home = Home::new();
    let hub = Server::start();
    std::fs::write(home.config(), "actor: human:test-person\n").unwrap();
    home.trust(&hub);
    let o = hub.origin().to_string();
    let text = std::fs::read_to_string(home.config()).unwrap();
    std::fs::write(home.config(), format!("{text}{shares}")).unwrap();
    let (ok, e) = home.json_as(c, human, &["auth", "state", "--install", &o]);
    assert!(!ok, "{e}");
    assert_eq!(err_code(&e), "E_REMOTE_EXISTS", "{e}");
    home.seed(vec![(&o, ready(ACCESS, REFRESH, now() + 3_000_000))]);
    let (ok, v) = home.json_as(c, human, &["auth", "sign-out", "--install", &o]);
    assert!(ok, "{v}");
    assert_eq!(
        (v["install"].as_str(), v["state"].as_str()),
        (Some(o.as_str()), Some("signed_out"))
    );
    assert_eq!(warning_codes(&v), ["W_UPSTREAM_SIGNOUT_LOCAL"], "{v}");
    assert!(
        home.read_creds().is_empty(),
        "the record is removed durably"
    );
    assert!(hub.requests().is_empty(), "no request without the settings");
    assert_eq!(hub.finish(), 0);

    // a config that does not parse: the actor is not known, so the explicit human is refused
    let home = Home::new();
    std::fs::write(home.config(), "actor: [human:test-person\n").unwrap();
    let o = "https://hub.example.com";
    home.seed(vec![(o, ready(ACCESS, REFRESH, now() + 3_000_000))]);
    let before = std::fs::read(home.creds()).unwrap();
    let (ok, e) = home.json_as(c, human, &["auth", "sign-out", "--install", o]);
    assert!(!ok, "{e}");
    assert_eq!(err_code(&e), "E_ACTOR_UNBOUND", "{e}");
    assert_eq!(std::fs::read(home.creds()).unwrap(), before, "unchanged");
}
