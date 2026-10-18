//! sig9 (D-1Z951, D-QVH4N, D-KQ4CA): later signing and the automatic-signing warnings have exact
//! texts on the chat path and the CLI, and the token budget of a verified log never cuts inside
//! a signature report. Every key is a fixed in-memory test key (`rdy_qa::signing`).
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_engine::signing_crypto::{SigningAgent, UnavailableAgent};
use rdy_mcp::tools::{
    McpState, rdy_answer, rdy_key_show, rdy_pending, rdy_propose, rdy_schema, rdy_session,
    rdy_sign, rdy_tag, rdy_tags, rdy_write,
};
use rdy_mcp::view_tools::rdy_log;
use rdy_model::signature::{Signature, StatementSource};
use rdy_model::{Hash, Provenance, Role};
use rdy_qa::daemon::{Served, human_command};
use rdy_qa::fixtures::*;
use rdy_qa::platform::SocketAgent;
use rdy_qa::signing::{QaAgent, TestKey, sign_record};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn fp(key: TestKey) -> String {
    QaAgent::fingerprint(key)
}

/// `.rdy/local.yaml`: the person and the four signing settings, both keys by fingerprint.
fn config(dir: &Path, sign: &str) {
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "actor: human:qa\nsigning:\n  key: {:?}\n  agent_key: {:?}\n  sign: {sign}\n  allowed_signers: null\n",
            fp(TestKey::Person),
            fp(TestKey::Agent)
        ),
    )
    .unwrap();
}

/// An MCP state on the project at `dir` with session `actor` (`role`, run r1) and `agent`.
fn mcp(dir: &Path, actor: &str, role: &str, agent: Arc<dyn SigningAgent>) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(Rdy::open(dir).unwrap())));
    st.agent = agent;
    rdy_session(
        &mut st,
        p(json!({"actor": actor, "run": "r1", "role": role})),
    )
    .unwrap();
    st
}

/// The agent writes a claim under `section`, proposes it, and relays the person's approval of
/// its card: the answer's reply.
fn relayed_approval(
    st: &mut McpState,
    section: &str,
    body: &str,
) -> Result<Value, rdy_engine::RdyError> {
    rdy_write(
        st,
        p(json!({"ops": [{"op": "create", "type": "claim", "parent": section, "body": body}]})),
    )
    .unwrap();
    rdy_propose(st, p(json!({"title": body, "summary": "Adds one claim."}))).unwrap();
    let queue = rdy_pending(st, p(json!({}))).unwrap();
    let item = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "approve")
        .unwrap_or_else(|| panic!("an approval card: {queue}"))
        .clone();
    rdy_answer(
        st,
        p(
            json!({"ref": item["ref"], "option": "approve", "card_hash": item["card_hash"],
            "evidence": "Yes, land it.", "question": "Shall I land it?"}),
        ),
    )
}

/// The full error triple of `e`.
fn triple(e: &rdy_engine::RdyError) -> (String, String, String) {
    (e.code.to_string(), e.message.clone(), e.fix.clone())
}

/// The person's provenance on a stored signature.
fn by_person(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:qa".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
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

/// Stores `sigs` in one sealed batch.
fn store(r: &mut Rdy, sigs: &[Signature]) {
    let frames = rdy_store::signature::frames(sigs).unwrap();
    r.store_mut().write(frames).unwrap();
    r.seal_open().unwrap();
}

#[test]
fn sig9_sign_later_and_automatic_warnings_have_exact_text() {
    let (pfp, afp) = (fp(TestKey::Person), fp(TestKey::Agent));
    let agent = SocketAgent::start(Arc::new(QaAgent::new())).unwrap();
    let s = Served::new();
    // the direct handle writes before any command starts the daemon
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_, section, _) = seed_doc(&mut r, "seed");
    let seed = land(&mut r, "seed").unwrap();
    r.close().unwrap();
    config(s.dir.path(), "never");

    // the person signs their own landing later through the daemon, under `never`
    let run = |args: &[&str]| {
        let mut cmd = human_command(s.dir.path(), &s.env_pairs(), args);
        cmd.env("SSH_AUTH_SOCK", agent.socket_path());
        cmd.output().expect("run rdy")
    };
    let o = run(&["sign", &seed[..10]]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        o.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(out, format!("signed approval of {seed} with {pfp}\n"));
    let o = run(&["sign", &seed]);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "already signed\n");
    let o = run(&["--json", "sign", &seed, "--kind", "approval"]);
    let check: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        (
            &check["kind"],
            &check["key"],
            &check["signed"],
            &check["statement"]
        ),
        (
            &json!("approval"),
            &json!(pfp),
            &json!("valid"),
            &json!("match")
        ),
        "{check}"
    );
    assert!(
        check.get("subject").is_none(),
        "the CLI extracts the check: {check}"
    );
    // a person cannot sign as an agent's relay
    let o = run(&["--json", "sign", &seed, "--kind", "relay"]);
    assert!(!o.status.success());
    let stop = run(&["serve", "--stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );

    // an agent signs a relayed landing later: its relay by default, then the person's statement
    let mut st = mcp(
        s.dir.path(),
        "agent:relay",
        "writer",
        Arc::new(QaAgent::new()),
    );
    let answered = relayed_approval(&mut st, &section, "A relayed claim.").unwrap();
    assert_eq!(answered["signatures"], json!([]), "never: unsigned");
    let landing = answered["landing"]["landing"].as_str().unwrap().to_string();
    let relay = rdy_sign(&mut st, p(json!({"target": landing}))).unwrap();
    assert_eq!(
        (&relay["kind"], &relay["key"], &relay["signed"]),
        (&json!("relay"), &json!(afp), &json!("valid")),
        "{relay}"
    );
    let person = rdy_sign(&mut st, p(json!({"target": landing, "kind": "approval"}))).unwrap();
    assert_eq!(
        (
            &person["kind"],
            &person["key"],
            &person["signed_by"]["actor"]
        ),
        (&json!("approval"), &json!(pfp), &json!("agent:relay")),
        "{person}"
    );
    let again = rdy_sign(&mut st, p(json!({"target": landing}))).unwrap();
    assert_eq!(again, relay, "already signed: the stored check");
    let log = rdy_log(&mut st, p(json!({"verify": true, "limit": 1}))).unwrap();
    let kinds: Vec<&str> = log[0]["signatures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.len(), 2, "no second relay: {log}");

    // a reader may show keys and verify, but not sign
    let mut reader = McpState::new(Arc::clone(&st.engine));
    reader.agent = Arc::new(QaAgent::new());
    rdy_session(
        &mut reader,
        p(json!({"actor": "agent:look", "role": "reader"})),
    )
    .unwrap();
    rdy_key_show(&mut reader, p(json!({}))).unwrap();
    rdy_log(&mut reader, p(json!({"verify": true}))).unwrap();
    let e = rdy_sign(&mut reader, p(json!({"target": landing}))).unwrap_err();
    assert_eq!(
        triple(&e),
        (
            "E_PERMISSION".into(),
            "role reader cannot sign".into(),
            "start a writer session for this act".into()
        )
    );

    // auto with no agent: the act is saved unsigned, with the shared warning
    config(s.dir.path(), "auto");
    st.agent = Arc::new(UnavailableAgent);
    let answered = relayed_approval(&mut st, &section, "An unsigned claim.").unwrap();
    assert_eq!(answered["signatures"], json!([]));
    let warnings = answered["landing"]["warnings"].as_array().unwrap();
    assert_eq!(
        warnings.last().unwrap(),
        &json!({
            "severity": "warning",
            "code": "W_SIGNING_FAILED",
            "block": null,
            "message": "the act was saved without signatures: E_NO_AGENT: the SSH agent could not sign: SSH_AUTH_SOCK is not set",
            "fix": "fix the signing error, then run rdy sign"
        })
    );

    // always with no agent: the tag is refused and nothing is written
    config(s.dir.path(), "always");
    let e = rdy_tag(
        &mut st,
        p(json!({"version": "v1.0.0", "evidence": "Tag it.", "question": "Shall I tag it?"})),
    )
    .unwrap_err();
    assert_eq!(
        triple(&e),
        (
            "E_NO_AGENT".into(),
            "the SSH agent could not sign: SSH_AUTH_SOCK is not set".into(),
            "check SSH_AUTH_SOCK and the configured key, then try again".into()
        )
    );
    assert_eq!(
        rdy_tags(&mut st, p(json!({}))).unwrap()["versions"],
        json!([])
    );
    // explicit signing has no automatic mode: the agent's sign of a tag needs a relayed tag
    config(s.dir.path(), "never");
    st.agent = Arc::new(QaAgent::new());
    let tagged = rdy_tag(
        &mut st,
        p(json!({"version": "v1.0.0", "evidence": "Tag it.", "question": "Shall I tag it?"})),
    )
    .unwrap();
    assert_eq!(tagged["signatures"], json!([]));
    let tag = rdy_sign(&mut st, p(json!({"target": "tag:v1.0.0"}))).unwrap();
    assert_eq!((&tag["kind"], &tag["key"]), (&json!("relay"), &json!(afp)));

    // rdy_schema states each signing code's shared message and fix, and the signing rules
    let schema = rdy_schema(&mut st, p(json!({}))).unwrap();
    for (code, when, fix) in [
        (
            "E_SIGNING_KEY_MISSING",
            "{setting} is not set",
            "set {setting} in the user config or .rdy/local.yaml",
        ),
        (
            "E_SIGNING_KEY_TYPE",
            "{key} has unsupported signing key type {type}",
            "set an ssh-ed25519 or sk-ssh-ed25519@openssh.com key",
        ),
        (
            "E_NO_AGENT",
            "the SSH agent could not sign: {cause}",
            "check SSH_AUTH_SOCK and the configured key, then try again",
        ),
        (
            "E_SIG_INVALID",
            "signature {hash} is invalid",
            "check the source of this signature; it may be damaged or forged",
        ),
        (
            "E_SIG_OBJECT_MISMATCH",
            "signature {hash} does not match its subject",
            "run rdy sign for this subject, or refuse the record",
        ),
        (
            "W_SIG_UNKNOWN_KEY",
            "key {key} is not trusted for {principal} in {namespace}",
            "add the key and principal to signing.allowed_signers",
        ),
        (
            "W_SIGNING_FAILED",
            "the act was saved without signatures: {code}: {message}",
            "fix the signing error, then run rdy sign",
        ),
    ] {
        assert_eq!(
            schema["errors"][code],
            json!({"when": when, "fix": fix}),
            "{code}"
        );
    }
    assert_eq!(
        schema["signing"],
        "Signing uses only the caller's SSH agent. Accepted key types are ssh-ed25519 and sk-ssh-ed25519@openssh.com. signing.sign is never, auto or always; the default is auto. Presence is an authenticated flag, not proof that a person read the act."
    );
}

#[test]
fn sig9_budgets_do_not_hide_a_partial_signature_report() {
    let (d, mut r) = project();
    let (_, section, _) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let mut landings = vec![];
    for n in 0..6 {
        landings.push(land_claim(&mut r, &section, &format!("v{n}")));
    }
    let repo = r.read().repo_id().unwrap();
    let sigs: Vec<Signature> = landings
        .iter()
        .enumerate()
        .map(|(n, h)| {
            let record = r.read().reader().landing(h).unwrap();
            sign_record(
                *h,
                StatementSource::Approval(&record),
                repo,
                by_person(n as i64),
                TestKey::Person,
            )
        })
        .collect();
    store(&mut r, &sigs);
    drop(r);
    config(d.path(), "never");

    let mut st = mcp(d.path(), "agent:a", "reader", Arc::new(UnavailableAgent));
    let full = rdy_log(&mut st, p(json!({"verify": true, "max_tokens": 100_000}))).unwrap();
    let rows = full.as_array().unwrap().clone();
    assert_eq!(rows.len(), 7);
    // a budget for about two rows: whole rows only, each with its whole report
    let two = serde_json::to_string(&rows[..2]).unwrap().len() / 4 + 40;
    let cut = rdy_log(&mut st, p(json!({"verify": true, "max_tokens": two}))).unwrap();
    let items = cut["items"].as_array().unwrap();
    assert!(!items.is_empty() && items.len() < rows.len(), "{cut}");
    assert_eq!(cut["truncated"], true);
    assert_eq!(
        cut["omitted"].as_u64().unwrap() as usize,
        rows.len() - items.len()
    );
    assert_eq!(
        items.as_slice(),
        &rows[..items.len()],
        "complete rows, in order"
    );
    for row in items {
        for c in row["signatures"].as_array().unwrap() {
            assert_eq!(c.as_object().unwrap().len(), 11, "a whole report: {c}");
        }
    }
    // a budget that cannot hold one row is refused, not answered with nothing
    let e = rdy_log(&mut st, p(json!({"verify": true, "max_tokens": 10}))).unwrap_err();
    assert_eq!(
        triple(&e),
        (
            "E_LIMIT".into(),
            "the token budget cannot hold one verified log row".into(),
            "increase max_tokens".into()
        )
    );
    // the plain log keeps its old budget behaviour
    let plain = rdy_log(&mut st, p(json!({"max_tokens": 10}))).unwrap();
    assert_eq!(plain["items"], json!([]), "{plain}");
    drop(st);

    // more signatures of one subject than one response holds
    let mut r = Rdy::open(d.path()).unwrap();
    let head = landings[5];
    let flood: Vec<Signature> = (0..4097)
        .map(|n| Signature {
            subject: head,
            namespace: "rdy-approval-v1".into(),
            statement: b"rdy-statement: 1\n".to_vec(),
            sig: b"damaged".to_vec(),
            signed_by: by_person(1000 + n),
        })
        .collect();
    store(&mut r, &flood);
    drop(r);
    let mut st = mcp(d.path(), "agent:a", "reader", Arc::new(UnavailableAgent));
    let e = rdy_log(&mut st, p(json!({"verify": true}))).unwrap_err();
    let cap = (
        "E_LIMIT".to_string(),
        "signing input exceeds 4096 signatures per subject".to_string(),
        "reduce the input to the stated limit".to_string(),
    );
    assert_eq!(triple(&e), cap);
    rdy_log(&mut st, p(json!({}))).unwrap();
    drop(st);
    let (_cfg, env) = human_env("human:qa");
    let mut cmd = human_command(d.path(), &env, &["--json", "log", "--verify"]);
    cmd.env("RDY_NO_DAEMON", "1").env_remove("SSH_AUTH_SOCK");
    let o = cmd.output().unwrap();
    assert!(!o.status.success());
    let v: Value = serde_json::from_slice(&o.stderr).unwrap();
    assert_eq!(
        (
            v["code"].as_str().unwrap().to_string(),
            v["message"].as_str().unwrap().to_string(),
            v["fix"].as_str().unwrap().to_string()
        ),
        cap
    );
}
