//! sig9 (D-1Z951, D-QVH4N, D-KQ4CA): signing, key show and verification through the chat tools
//! and the CLI, with the same results. `rdy sign` and `rdy_sign` sign a stored landing or tag
//! through the caller's agent; `rdy key show` and `rdy_key_show` report the configured keys;
//! `rdy log --verify` and `rdy_log {verify: true}` check every signature offline; `rdy fsck
//! --verbose` lists the signature state of canon. No path here contacts a hub, and reads never
//! ask an SSH agent. Every key is a fixed in-memory test key (`rdy_qa::signing`).
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::signing_crypto::UnavailableAgent;
use rdy_engine::{Rdy, Session};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{
    KeyShowParams, McpState, SignParams, rdy_answer, rdy_key_show, rdy_pending, rdy_propose,
    rdy_session, rdy_tag, rdy_write,
};
use rdy_mcp::view_tools::{LogParams, rdy_log};
use rdy_model::signature::{Signature, StatementSource};
use rdy_model::{Hash, Provenance, Role};
use rdy_qa::fixtures::*;
use rdy_qa::platform::{RecordingAgent, SocketAgent};
use rdy_qa::signing::{QaAgent, TestKey, sign_record};
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

/// A public key no test agent holds (a test key; no private half is in the repo).
const OTHER_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti user@example.com";

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn fp(key: TestKey) -> String {
    QaAgent::fingerprint(key)
}

/// The OpenSSH public-key line of `key`, with a comment.
fn public_line(key: TestKey) -> String {
    let mut k = ssh_key::PublicKey::from_bytes(&QaAgent::blob(key)).unwrap();
    k.set_comment("qa@example.com");
    k.to_openssh().unwrap()
}

/// A project whose person is `human:qa`, with one landing of the seed doc; its section id.
fn proj() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let (_, section, _) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    config(d.path(), "never", None, None, None);
    (d, r, section)
}

/// `.rdy/local.yaml`: the person and the four signing settings (`None` is an explicit null, so
/// no user config of this machine leaks in).
fn config(
    dir: &Path,
    sign: &str,
    key: Option<&str>,
    agent_key: Option<&str>,
    signers: Option<&str>,
) {
    let q = |v: Option<&str>| v.map_or("null".to_string(), |s| format!("{s:?}"));
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "actor: human:qa\nsigning:\n  key: {}\n  agent_key: {}\n  sign: {sign}\n  allowed_signers: {}\n",
            q(key),
            q(agent_key),
            q(signers)
        ),
    )
    .unwrap();
}

/// An allowed-signers file `signers` in the project that trusts the person's key for `human:qa`
/// and the agent's key for `agent:relay`.
fn trust_both(dir: &Path) {
    std::fs::write(
        dir.join("signers"),
        format!(
            "human:qa {}\nagent:relay {}\n",
            public_line(TestKey::Person),
            public_line(TestKey::Agent)
        ),
    )
    .unwrap();
}

/// Writes a claim under `section` into `view` and lands it unsigned; the landing.
fn land_claim(r: &mut Rdy, section: &str, view: &str) -> Hash {
    write(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": section, "body": format!("A claim of {view}.")}]),
    )
    .unwrap();
    land(r, view).unwrap().parse().unwrap()
}

/// The person's provenance on a stored signature.
fn by_person() -> Provenance {
    Provenance {
        actor: "human:qa".into(),
        run: None,
        role: Role::Approver,
        at_ms: 7,
    }
}

/// A damaged signature of `subject`.
fn damaged(subject: Hash) -> Signature {
    Signature {
        subject,
        namespace: "rdy-approval-v1".into(),
        statement: b"rdy-statement: 1\n".to_vec(),
        sig: b"damaged".to_vec(),
        signed_by: by_person(),
    }
}

/// Stores `sigs` in one sealed batch.
fn store(r: &mut Rdy, sigs: &[Signature]) {
    let frames = rdy_store::signature::frames(sigs).unwrap();
    r.store_mut().write(frames).unwrap();
    r.seal_open().unwrap();
}

/// The person's valid approval signature of the landing `subject`, with the statement of
/// `source` (the landing itself, or another one for a mismatch).
fn approval_of(r: &Rdy, subject: Hash, source: Hash) -> Signature {
    let st = r.read();
    let record = st.reader().landing(&source).unwrap();
    sign_record(
        subject,
        StatementSource::Approval(&record),
        st.repo_id().unwrap(),
        by_person(),
        TestKey::Person,
    )
}

/// `rdy --project <dir> <args>` in this process (`RDY_NO_DAEMON`), as the person, with
/// `SSH_AUTH_SOCK` set to `sock` or removed.
fn cli(dir: &Path, sock: Option<&Path>, args: &[&str]) -> Output {
    let (_cfg, env) = human_env("human:qa");
    let mut cmd = Command::new(rdy_bin());
    cmd.arg("--project")
        .arg(dir)
        .args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("SSH_AUTH_SOCK")
        .env("RDY_NO_DAEMON", "1")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(dir);
    if let Some(s) = sock {
        cmd.env("SSH_AUTH_SOCK", s);
    }
    cmd.output().expect("run rdy")
}

fn stdout(o: &Output) -> String {
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json_out(o: &Output) -> Value {
    let text = stdout(o);
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// The JSON error of a failed `--json` run.
fn json_err(o: &Output) -> Value {
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let text = String::from_utf8_lossy(&o.stderr).into_owned();
    serde_json::from_str(text.trim().lines().last().unwrap_or(""))
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// An MCP state on the project at `dir` with session `actor` (`role`) and `agent`.
fn mcp(
    dir: &Path,
    actor: &str,
    role: &str,
    agent: Arc<dyn rdy_engine::signing_crypto::SigningAgent>,
) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(Rdy::open(dir).unwrap())));
    st.agent = agent;
    rdy_session(
        &mut st,
        p(json!({"actor": actor, "run": "r1", "role": role})),
    )
    .unwrap();
    st
}

/// The kinds of a list of checks, sorted.
fn kinds(checks: &Value) -> Vec<String> {
    let mut k: Vec<String> = checks
        .as_array()
        .unwrap_or_else(|| panic!("checks: {checks}"))
        .iter()
        .map(|c| c["kind"].as_str().unwrap_or("null").to_string())
        .collect();
    k.sort();
    k
}

/// The check of `kind` in `checks`.
fn check_of<'a>(checks: &'a Value, kind: &str) -> &'a Value {
    checks
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == kind)
        .unwrap_or_else(|| panic!("{kind} in {checks}"))
}

/// The verification line of a valid, covered signature of `kind` by `key`.
fn valid_line(indent: usize, kind: &str, key: TestKey, trust: &str) -> String {
    format!(
        "{}signed {kind} valid by {} (ssh-ed25519), key {trust}, presence absent",
        " ".repeat(indent),
        fp(key)
    )
}

#[test]
fn sig9_chat_and_cli_sign_check_and_show_the_same_keys() {
    let (d, r, section) = proj();
    drop(r);
    trust_both(d.path());
    let (pfp, afp) = (fp(TestKey::Person), fp(TestKey::Agent));
    config(d.path(), "auto", Some(&pfp), Some(&afp), Some("signers"));

    // the chat path only: the agent writes, proposes, relays the person's approval and tag
    let mut st = mcp(d.path(), "agent:relay", "writer", Arc::new(QaAgent::new()));
    rdy_write(
        &mut st,
        p(json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": "A relayed claim."}]})),
    )
    .unwrap();
    rdy_propose(
        &mut st,
        p(json!({"title": "Relayed claim", "summary": "Adds one claim."})),
    )
    .unwrap();
    let queue = rdy_pending(&mut st, p(json!({}))).unwrap();
    let item = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "approve")
        .unwrap_or_else(|| panic!("an approval card: {queue}"))
        .clone();
    let answered = rdy_answer(
        &mut st,
        p(
            json!({"ref": item["ref"], "option": "approve", "card_hash": item["card_hash"],
            "evidence": "Yes, land it.", "question": "Shall I land the relayed claim?"}),
        ),
    )
    .unwrap();
    assert_eq!(
        kinds(&answered["signatures"]),
        ["approval", "relay"],
        "{answered:#}"
    );
    let landing = answered["landing"]["landing"].as_str().unwrap().to_string();
    let tagged = rdy_tag(
        &mut st,
        p(json!({"version": "v1.0.0", "evidence": "Tag it v1.0.0.", "question": "Shall I tag it?"})),
    )
    .unwrap();
    assert_eq!(kinds(&tagged["signatures"]), ["relay", "tag"], "{tagged:#}");
    let chat_keys = rdy_key_show(&mut st, p(json!({}))).unwrap();
    let chat_log = rdy_log(&mut st, p(json!({"verify": true}))).unwrap();
    drop(st);

    // both surfaces report the same local keys
    assert_eq!(
        chat_keys,
        json!({
            "key": {"fingerprint": pfp, "type": "ssh-ed25519", "held": true},
            "agent_key": {"fingerprint": afp, "type": "ssh-ed25519", "held": true},
            "sign": "auto"
        })
    );
    let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
    let cli_keys = json_out(&cli(
        d.path(),
        Some(agent.socket_path()),
        &["--json", "key", "show"],
    ));
    assert_eq!(cli_keys, chat_keys);

    // the CLI verifies the records the chat tools wrote, with no agent, as the chat log does
    let cli_log = json_out(&cli(d.path(), None, &["--json", "log", "--verify"]));
    assert_eq!(cli_log, chat_log);
    let head = &cli_log[0];
    assert_eq!(head["landing"], landing.as_str());
    let sigs = &head["signatures"];
    assert_eq!(kinds(sigs), ["approval", "relay"]);
    for c in sigs.as_array().unwrap() {
        assert_eq!(
            (&c["signed"], &c["statement"], &c["trust"], &c["code"]),
            (
                &json!("valid"),
                &json!("match"),
                &json!("trusted"),
                &Value::Null
            ),
            "{c}"
        );
    }
    assert_eq!(check_of(sigs, "approval")["key"], pfp.as_str());
    assert_eq!(check_of(sigs, "relay")["key"], afp.as_str());
    let tag_sigs = &head["tag_signatures"]["v1.0.0"];
    assert_eq!(kinds(tag_sigs), ["relay", "tag"]);
    assert_eq!(check_of(tag_sigs, "tag")["trust"], "trusted");
    let text = stdout(&cli(d.path(), None, &["log", "--verify"]));
    let lines: Vec<&str> = text.lines().collect();
    for want in [
        valid_line(4, "approval", TestKey::Person, "trusted"),
        valid_line(4, "relay", TestKey::Agent, "trusted"),
        "    tag v1.0.0:".to_string(),
        valid_line(8, "tag", TestKey::Person, "trusted"),
        valid_line(8, "relay", TestKey::Agent, "trusted"),
    ] {
        assert!(lines.contains(&want.as_str()), "{want}\n{text}");
    }

    // a later CLI signature of an unsigned landing appears in the chat log
    let mut r = Rdy::open(d.path()).unwrap();
    let later = land_claim(&mut r, &section, "later").to_string();
    drop(r);
    config(d.path(), "never", Some(&pfp), Some(&afp), Some("signers"));
    let text = stdout(&cli(
        d.path(),
        Some(agent.socket_path()),
        &["sign", &later[..12]],
    ));
    assert_eq!(text, format!("signed approval of {later} with {pfp}\n"));
    let mut st = mcp(
        d.path(),
        "agent:reader",
        "reader",
        Arc::new(UnavailableAgent),
    );
    let log = rdy_log(&mut st, p(json!({"verify": true, "limit": 1}))).unwrap();
    assert_eq!(log[0]["landing"], later.as_str());
    let sigs = &log[0]["signatures"];
    assert_eq!(kinds(sigs), ["approval"]);
    assert_eq!(sigs[0]["key"], pfp.as_str());
    assert_eq!(sigs[0]["trust"], "trusted");
    assert_eq!(sigs[0]["signed_by"]["actor"], "human:qa");
}

#[test]
fn sig9_all_new_inputs_refuse_unknown_keys() {
    let refusal = |e: rdy_engine::RdyError| (e.code.to_string(), e.message, e.fix);
    // the chat tools' strict parameters
    let e = parse_params::<SignParams>("rdy_sign", p(json!({"target": "x", "knd": "tag"})));
    assert_eq!(
        refusal(e.unwrap_err()),
        (
            "E_BAD_INPUT".into(),
            "rdy_sign: unknown parameter \"knd\"".into(),
            "did you mean \"kind\"? the parameters here are kind, target".into()
        )
    );
    let e = parse_params::<SignParams>("rdy_sign", p(json!({"targt": "x"})));
    assert_eq!(
        refusal(e.unwrap_err()).2,
        "did you mean \"target\"? the parameters here are kind, target"
    );
    let e = parse_params::<SignParams>("rdy_sign", p(json!({"target": "x", "kind": "approve"})));
    assert_eq!(
        refusal(e.unwrap_err()),
        (
            "E_BAD_INPUT".into(),
            "rdy_sign: parameter \"kind\" must be one of approval, relay, tag, not a string".into(),
            "pass \"kind\" as one of approval, relay, tag".into()
        )
    );
    let e = parse_params::<KeyShowParams>("rdy_key_show", p(json!({"key": "x"})));
    assert_eq!(
        refusal(e.unwrap_err()),
        (
            "E_BAD_INPUT".into(),
            "rdy_key_show: unknown parameter \"key\"".into(),
            "remove \"key\"; rdy_key_show takes no parameters".into()
        )
    );
    let e = parse_params::<LogParams>("rdy_log", p(json!({"verfy": true})));
    assert_eq!(
        refusal(e.unwrap_err()).2,
        "did you mean \"verify\"? the parameters here are limit, max_tokens, since, verify"
    );
    let e = parse_params::<LogParams>("rdy_log", p(json!({"verify": "yes"})));
    assert_eq!(
        refusal(e.unwrap_err()).1,
        "rdy_log: parameter \"verify\" must be a boolean, not a string"
    );

    // the daemon ops' arguments
    let (d, r, _) = proj();
    let e = Local::new(r);
    let s = Session::human("human:qa".into(), Role::Writer).unwrap();
    let cx = OpContext {
        engine: &e,
        session: &s,
        human: "human:qa",
        cwd: d.path(),
        agent: &UnavailableAgent,
    };
    for (op, args, near) in [
        ("sign", json!({"target": "x", "knd": "tag"}), "kind"),
        ("log", json!({"verfy": true}), "verify"),
        ("fsck", json!({"verbos": true}), "verbose"),
    ] {
        let err = dispatch(&cx, op, args).unwrap_err();
        assert_eq!(err.code, "E_BAD_INPUT", "{op}: {err}");
        assert!(
            err.fix.contains(&format!("did you mean \"{near}\"")),
            "{op}: {err:?}"
        );
    }
    let err = dispatch(&cx, "key_show", json!({"key": "x"})).unwrap_err();
    assert_eq!(err.code, "E_BAD_INPUT", "{err}");
    assert!(err.message.contains("key"), "{err:?}");
    let err = dispatch(&cx, "sign", json!({"target": "x", "kind": "approve"})).unwrap_err();
    assert_eq!(err.code, "E_BAD_INPUT", "{err}");
    assert!(
        err.message.contains("`approval`, `relay`, `tag`"),
        "the closed kinds: {err:?}"
    );

    // the CLI's flags
    let o = cli(d.path(), None, &["sign", "x", "--kind", "approve"]);
    assert!(!o.status.success());
    let text = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        text.contains("[possible values: approval, relay, tag]"),
        "{text}"
    );
    let o = cli(d.path(), None, &["log", "--verfy"]);
    assert!(!o.status.success());
    let text = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(text.contains("--verify"), "{text}");
}

#[test]
fn sig9_verify_is_opt_in_and_unsigned_is_neutral() {
    let (d, mut r, section) = proj();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    land_claim(&mut r, &section, "next");
    drop(r);

    let plain = json_out(&cli(d.path(), None, &["--json", "log"]));
    let verified = json_out(&cli(d.path(), None, &["--json", "log", "--verify"]));
    let rows = plain.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(row.get("signatures").is_none(), "{row}");
        assert!(row.get("tag_signatures").is_none(), "{row}");
    }
    // the verified rows are the plain rows, each with two members more
    let mut want = plain.clone();
    want[0]["signatures"] = json!([]);
    want[0]["tag_signatures"] = json!({});
    want[1]["signatures"] = json!([]);
    want[1]["tag_signatures"] = json!({"v1.0.0": []});
    assert_eq!(verified, want);

    // the plain text is unchanged; the verified text adds `signed: no` under each subject
    let text = stdout(&cli(d.path(), None, &["log"]));
    let heads: Vec<&str> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("    assessment "))
        .collect();
    assert_eq!(heads.len(), 2, "{text}");
    let vtext = stdout(&cli(d.path(), None, &["log", "--verify"]));
    let got: Vec<&str> = vtext
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("    assessment "))
        .collect();
    assert_eq!(
        got,
        [
            heads[0],
            "    signed: no",
            heads[1],
            "    signed: no",
            "    tag v1.0.0:",
            "        signed: no",
        ],
        "{vtext}"
    );

    // the chat tool gives the same two answers
    let mut st = mcp(d.path(), "agent:a", "reader", Arc::new(UnavailableAgent));
    assert_eq!(rdy_log(&mut st, p(json!({}))).unwrap(), plain);
    assert_eq!(
        rdy_log(&mut st, p(json!({"verify": false}))).unwrap(),
        plain
    );
    assert_eq!(rdy_log(&mut st, p(json!({"verify": true}))).unwrap(), want);
}

#[test]
fn sig9_verification_needs_neither_agent_nor_hub() {
    let (d, mut r, section) = proj();
    let first: Hash = r.read().canon_head().unwrap();
    let second = land_claim(&mut r, &section, "two");
    let third = land_claim(&mut r, &section, "three");
    // valid with an unknown key; valid, but naming another landing's statement; damaged
    let sigs = vec![
        approval_of(&r, first, first),
        approval_of(&r, second, first),
        damaged(third),
    ];
    store(&mut r, &sigs);
    drop(r);

    // no agent can be reached: a recording socket that counts every contact, and no hub at all
    let recorder = RecordingAgent::start().unwrap();
    let cli_log = json_out(&cli(
        d.path(),
        Some(recorder.socket_path()),
        &["--json", "log", "--verify"],
    ));
    let text = stdout(&cli(
        d.path(),
        Some(recorder.socket_path()),
        &["log", "--verify"],
    ));
    assert_eq!(recorder.connections(), 0, "verification asked an agent");
    let mut st = mcp(d.path(), "agent:a", "reader", Arc::new(UnavailableAgent));
    let chat_log = rdy_log(&mut st, p(json!({"verify": true}))).unwrap();
    assert_eq!(chat_log, cli_log, "the same reports on both surfaces");

    let pfp = fp(TestKey::Person);
    let row = |h: &Hash| {
        cli_log
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["landing"] == h.to_string())
            .unwrap()
            .clone()
    };
    let unknown = &row(&first)["signatures"][0];
    assert_eq!(
        (
            &unknown["kind"],
            &unknown["key"],
            &unknown["key_type"],
            &unknown["signed"],
            &unknown["statement"],
            &unknown["trust"],
            &unknown["presence"],
            &unknown["code"]
        ),
        (
            &json!("approval"),
            &json!(pfp),
            &json!("ssh-ed25519"),
            &json!("valid"),
            &json!("match"),
            &json!("unknown"),
            &json!("absent"),
            &Value::Null
        )
    );
    assert_eq!(
        unknown["warnings"],
        json!([{
            "code": "W_SIG_UNKNOWN_KEY",
            "message": format!("key {pfp} is not trusted for human:qa in rdy-approval-v1"),
            "fix": "add the key and principal to signing.allowed_signers"
        }])
    );
    let mismatch = &row(&second)["signatures"][0];
    assert_eq!(
        (
            &mismatch["signed"],
            &mismatch["statement"],
            &mismatch["code"]
        ),
        (
            &json!("valid"),
            &json!("mismatch"),
            &json!("E_SIG_OBJECT_MISMATCH")
        )
    );
    let invalid = &row(&third)["signatures"][0];
    assert_eq!(
        (
            &invalid["kind"],
            &invalid["key"],
            &invalid["signed"],
            &invalid["trust"],
            &invalid["code"]
        ),
        (
            &Value::Null,
            &Value::Null,
            &json!("invalid"),
            &json!("unknown"),
            &json!("E_SIG_INVALID")
        )
    );
    let lines: Vec<&str> = text.lines().collect();
    for want in [
        format!("    signed approval valid by {pfp} (ssh-ed25519), key unknown, presence absent"),
        format!(
            "    signed approval valid by {pfp} (ssh-ed25519), key unknown, presence absent, statement mismatch"
        ),
        "    signed unknown invalid by unknown (unknown), E_SIG_INVALID, presence absent".into(),
    ] {
        assert!(lines.contains(&want.as_str()), "{want}\n{text}");
    }
}

#[test]
fn sig9_key_show_reports_identity_type_and_agent_presence() {
    let (d, r, _) = proj();
    drop(r);
    let pfp = fp(TestKey::Person);
    let ofp = rdy_engine::signing_crypto::key_fingerprint(OTHER_KEY).unwrap();
    let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();

    // a public-key line the agent holds; a fingerprint it does not hold
    config(
        d.path(),
        "auto",
        Some(&public_line(TestKey::Person)),
        Some(&ofp),
        None,
    );
    let want = json!({
        "key": {"fingerprint": pfp, "type": "ssh-ed25519", "held": true},
        "agent_key": {"fingerprint": ofp, "type": null, "held": false},
        "sign": "auto"
    });
    let sock = Some(agent.socket_path());
    assert_eq!(
        json_out(&cli(d.path(), sock, &["--json", "key", "show"])),
        want
    );
    assert_eq!(
        stdout(&cli(d.path(), sock, &["key", "show"])),
        format!(
            "person: {pfp} (ssh-ed25519), held yes\nagent: {ofp} (unknown), held no\nsign: auto\n"
        )
    );
    let mut st = mcp(d.path(), "agent:a", "reader", Arc::new(QaAgent::new()));
    assert_eq!(rdy_key_show(&mut st, p(json!({}))).unwrap(), want);

    // no agent: E_NO_AGENT, never a claim that the key is not held
    let no_agent = (
        json!("E_NO_AGENT"),
        json!("the SSH agent could not sign: SSH_AUTH_SOCK is not set"),
        json!("check SSH_AUTH_SOCK and the configured key, then try again"),
    );
    st.agent = Arc::new(UnavailableAgent);
    let e = rdy_key_show(&mut st, p(json!({}))).unwrap_err();
    assert_eq!((json!(e.code), json!(e.message), json!(e.fix)), no_agent);
    drop(st);
    let e = json_err(&cli(d.path(), None, &["--json", "key", "show"]));
    assert_eq!(
        (e["code"].clone(), e["message"].clone(), e["fix"].clone()),
        no_agent
    );

    // no key set: no agent request at all
    config(d.path(), "never", None, None, None);
    let recorder = RecordingAgent::start().unwrap();
    let rec = Some(recorder.socket_path());
    assert_eq!(
        json_out(&cli(d.path(), rec, &["--json", "key", "show"])),
        json!({"key": null, "agent_key": null, "sign": "never"})
    );
    assert_eq!(
        stdout(&cli(d.path(), rec, &["key", "show"])),
        "person: not set\nagent: not set\nsign: never\n"
    );
    assert_eq!(recorder.connections(), 0, "key show asked an agent");

    // an agent key equal to the person's is ignored, and the shared warning follows the lines
    config(
        d.path(),
        "always",
        Some(&public_line(TestKey::Person)),
        Some(&pfp),
        None,
    );
    let warning = "warning W_CONFIG_UNKNOWN: signing.agent_key equals signing.key; rdy ignores signing.agent_key; set signing.agent_key to a different key";
    assert_eq!(
        stdout(&cli(d.path(), sock, &["key", "show"])),
        format!("person: {pfp} (ssh-ed25519), held yes\nagent: not set\nsign: always\n{warning}\n")
    );
    let o = cli(d.path(), sock, &["--json", "key", "show"]);
    assert_eq!(
        json_out(&o),
        json!({"key": {"fingerprint": pfp, "type": "ssh-ed25519", "held": true}, "agent_key": null, "sign": "always"})
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains(warning),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    // a malformed selector, an empty or blank one included, is the shared input error, before
    // any agent request, on key show and on rdy sign; only null is unset
    let r = Rdy::open(d.path()).unwrap();
    let landing = r.read().canon_head().unwrap().to_string();
    drop(r);
    for bad in ["not a key", "", "   "] {
        for (setting, key, agent_key) in [
            ("signing.key", Some(bad), None),
            ("signing.agent_key", None, Some(bad)),
        ] {
            config(d.path(), "auto", key, agent_key, None);
            let want = (
                json!("E_BAD_INPUT"),
                json!(format!(
                    "{setting} is not a public key or SHA256 fingerprint"
                )),
                json!(format!(
                    "set {setting} to a public key line or its SHA256 fingerprint"
                )),
            );
            let e = json_err(&cli(d.path(), rec, &["--json", "key", "show"]));
            assert_eq!(
                (e["code"].clone(), e["message"].clone(), e["fix"].clone()),
                want,
                "key show {setting} {bad:?}"
            );
            if setting == "signing.key" {
                let e = json_err(&cli(d.path(), rec, &["--json", "sign", &landing]));
                assert_eq!(
                    (e["code"].clone(), e["message"].clone(), e["fix"].clone()),
                    want,
                    "sign {bad:?}"
                );
            }
        }
    }
    assert_eq!(recorder.connections(), 0);
}

#[test]
fn sig9_fsck_verbose_reports_signatures_and_refuses_cleanup_on_failure() {
    let (d, mut r, section) = proj();
    let first: Hash = r.read().canon_head().unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let second = land_claim(&mut r, &section, "two");
    let sig = approval_of(&r, second, second);
    store(&mut r, &[sig]);
    drop(r);
    // a configured trust file that does not exist: fsck never opens it
    config(d.path(), "never", None, None, Some("missing-signers"));

    let plain = json_out(&cli(d.path(), None, &["--json", "fsck"]));
    assert!(plain.get("signature_states").is_none(), "{plain}");
    let v = json_out(&cli(d.path(), None, &["--json", "fsck", "--verbose"]));
    let states = v["signature_states"].as_array().unwrap();
    assert_eq!(states.len(), 2, "{v}");
    assert_eq!(states[0]["landing"], second.to_string());
    assert_eq!(states[0]["tag_signatures"], json!({}));
    let c = &states[0]["signatures"][0];
    assert_eq!(states[0]["signatures"].as_array().unwrap().len(), 1);
    assert_eq!(
        (
            &c["kind"],
            &c["signed"],
            &c["statement"],
            &c["trust"],
            &c["code"]
        ),
        (
            &json!("approval"),
            &json!("valid"),
            &json!("match"),
            &json!("unknown"),
            &Value::Null
        )
    );
    assert_eq!(
        states[1],
        json!({"landing": first.to_string(), "signatures": [], "tag_signatures": {"v1.0.0": []}})
    );
    let summary = format!(
        "ok: {} frames, 2 landings, 1 versions",
        plain["frames"].as_u64().unwrap()
    );
    assert_eq!(
        stdout(&cli(d.path(), None, &["fsck"])).lines().next(),
        Some(summary.as_str())
    );
    let text = stdout(&cli(d.path(), None, &["fsck", "--verbose"]));
    let got: Vec<&str> = text.lines().take(7).collect();
    let signed = valid_line(4, "approval", TestKey::Person, "unknown");
    let (l2, l1) = (format!("landing {second}"), format!("landing {first}"));
    assert_eq!(
        got,
        [
            summary.as_str(),
            l2.as_str(),
            signed.as_str(),
            l1.as_str(),
            "    signed: no",
            "    tag v1.0.0:",
            "        signed: no",
        ],
        "{text}"
    );

    // a damaged signature fails fsck, verbose or not, and no quarantine file is removed
    let mut r = Rdy::open(d.path()).unwrap();
    store(&mut r, &[damaged(first)]);
    r.close().unwrap();
    let quarantined = d.path().join(".rdy/store/index.redb.damaged-1");
    std::fs::write(&quarantined, b"old index").unwrap();
    let e = json_err(&cli(
        d.path(),
        None,
        &["--json", "fsck", "--verbose", "--clean-quarantine"],
    ));
    assert_eq!(e["code"], "E_SIG_INVALID", "{e}");
    assert_eq!(e["issues"].as_array().unwrap().len(), 1, "{e}");
    assert!(e.get("signature_states").is_none(), "{e}");
    assert!(
        quarantined.exists(),
        "a failed fsck removes no quarantine file"
    );
    let o = cli(d.path(), None, &["fsck", "--verbose", "--clean-quarantine"]);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).starts_with("error E_SIG_INVALID: signature "),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(quarantined.exists());
}

/// The verified log of `r`'s published state, with no trust file.
fn verified(st: &rdy_engine::ReadState) -> Value {
    rdy_engine::signing_read::verified_log(st, None, None, None).unwrap()
}

#[test]
fn sig9_a_verified_read_never_shows_a_signature_of_an_unsealed_batch() {
    let (d, r, _) = proj();
    let landing = r.read().canon_head().unwrap();
    let sig = approval_of(&r, landing, landing);
    let shared = rdy_daemon::Shared::new(r);
    let e = shared.engine();

    // a writer appends a valid signature of the landing, then holds its batch unsealed
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let writer = {
        let e = Arc::clone(&e);
        std::thread::spawn(move || {
            e.write(Box::new(move |r| {
                r.store_mut()
                    .write(rdy_store::signature::frames(&[sig]).unwrap())?;
                held_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                r.store_mut().abandon_open_batch();
                Err(rdy_engine::RdyError::new(
                    rdy_engine::E_INTERNAL,
                    "the test abandons the batch",
                    "none",
                ))
            }))
        })
    };
    held_rx.recv().unwrap();

    // while the batch is held, the daemon's verified log reads the published state only
    let s = Session::human("human:qa".into(), Role::Writer).unwrap();
    let cx = OpContext {
        engine: &*e,
        session: &s,
        human: "human:qa",
        cwd: d.path(),
        agent: &UnavailableAgent,
    };
    let v = dispatch(&cx, "log", json!({"verify": true})).unwrap();
    assert_eq!(v[0]["landing"], landing.to_string(), "{v}");
    assert_eq!(
        v[0]["signatures"],
        json!([]),
        "a signature of an unsealed batch is not in the published state: {v}"
    );
    assert_eq!(
        verified(&e.read())[0]["signatures"],
        json!([]),
        "the engine's verified log reads the same: {v}"
    );

    // the batch is abandoned: after a reopen the landing is still unsigned
    go_tx.send(()).unwrap();
    assert!(writer.join().unwrap().is_err());
    drop(e);
    let _ = shared.shutdown();
    drop(shared);
    let mut r = Rdy::open(d.path()).unwrap();
    assert_eq!(verified(&r.read())[0]["signatures"], json!([]));

    // the same signature in a sealed batch is reported
    let sig = approval_of(&r, landing, landing);
    store(&mut r, &[sig]);
    let v = verified(&r.read());
    let c = &v[0]["signatures"][0];
    assert_eq!(
        (&c["signed"], &c["statement"]),
        (&json!("valid"), &json!("match")),
        "{v}"
    );
}

/// An engine over `inner` that runs `after` once, right after the next write returns: another
/// caller's write that takes the writer the instant it is released.
struct ThenWrite {
    inner: Local,
    after: std::sync::Mutex<Option<rdy_engine::engine::WriteFn>>,
}

impl rdy_engine::engine::Engine for ThenWrite {
    fn read(&self) -> Arc<rdy_engine::ReadState> {
        self.inner.read()
    }
    fn write(&self, f: rdy_engine::engine::WriteFn) -> Result<Value, rdy_engine::RdyError> {
        let out = self.inner.write(f);
        if let Some(next) = self.after.lock().unwrap().take() {
            self.inner.write(next).unwrap();
        }
        out
    }
    fn open_report(&self) -> rdy_engine::OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> std::path::PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<rdy_engine::BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
        self.inner.leases()
    }
}

#[test]
fn sig9_a_verbose_fsck_reports_the_state_it_checked() {
    let (d, r, _) = proj();
    let landing = r.read().canon_head().unwrap();
    // a tag of the landing commits the instant fsck's write releases the writer
    let e = ThenWrite {
        inner: Local::new(r),
        after: std::sync::Mutex::new(Some(Box::new(|r| {
            r.tag(&approver(), "v1.0.0", None, None)?;
            Ok(Value::Null)
        }))),
    };
    let s = Session::human("human:qa".into(), Role::Writer).unwrap();
    let cx = OpContext {
        engine: &e,
        session: &s,
        human: "human:qa",
        cwd: d.path(),
        agent: &UnavailableAgent,
    };
    let v = dispatch(&cx, "fsck", json!({"verbose": true})).unwrap();
    assert_eq!(v["versions"], 0, "the check ran before the tag: {v}");
    assert_eq!(
        v["signature_states"],
        json!([{"landing": landing.to_string(), "signatures": [], "tag_signatures": {}}]),
        "the signature states are of the checked state, without the later tag: {v}"
    );
    // the tag committed: the next verbose fsck checks it and lists it
    let v = dispatch(&cx, "fsck", json!({"verbose": true})).unwrap();
    assert_eq!(v["versions"], 1, "{v}");
    assert_eq!(
        v["signature_states"][0]["tag_signatures"],
        json!({"v1.0.0": []}),
        "{v}"
    );
}
