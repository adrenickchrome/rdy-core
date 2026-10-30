//! ap10 (D-DKAMT; O24): `rdy log --verify` and `rdy fsck --verbose` print a policy landing as
//! `signed: policy`, with the line that names the policy version, the landing that carried it and
//! that landing's signature state. The chat tool gives the same rows.
use rdy_engine::Rdy;
use rdy_engine::engine::Local;
use rdy_engine::signing_crypto::UnavailableAgent;
use rdy_mcp::tools::{McpState, rdy_session};
use rdy_mcp::view_tools::{LogParams, rdy_log};
use rdy_model::signature::Signature;
use rdy_model::{Hash, Provenance, Role};
use rdy_qa::every_kind::add_carried_fixture_records;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

/// `rdy --project <dir> <args>` in this process (`RDY_NO_DAEMON`), as the person.
fn cli(dir: &Path, args: &[&str]) -> Output {
    let (_cfg, env) = human_env("human:qa");
    Command::new(rdy_bin())
        .arg("--project")
        .arg(dir)
        .args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("SSH_AUTH_SOCK")
        .env("RDY_NO_DAEMON", "1")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(dir)
        .output()
        .expect("run rdy")
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

/// A project with a policy landing (the carried fixture): the policy landing and the landing
/// that carried its policy version.
fn policy_project() -> (
    tempfile::TempDir,
    Hash,
    Hash,
    rdy_model::assessment::PolicyRef,
) {
    let (d, mut r) = project();
    let fx = add_carried_fixture_records(&mut r);
    let carrier = {
        let st = r.read();
        let reader = st.reader();
        let change = reader.producers(&fx.policy.version).unwrap()[0];
        reader
            .landing_of(&change)
            .unwrap()
            .expect("a landing carries it")
    };
    r.close().unwrap();
    (d, fx.landing, carrier, fx.policy)
}

/// Stores a damaged signature of `subject`.
fn put_damaged(dir: &Path, subject: Hash) {
    let mut r = Rdy::open(dir).unwrap();
    let g = Signature {
        subject,
        namespace: "rdy-approval-v1".into(),
        statement: b"rdy-statement: 1\n".to_vec(),
        sig: b"damaged".to_vec(),
        signed_by: Provenance {
            actor: "human:qa".into(),
            run: None,
            role: Role::Approver,
            at_ms: 7,
        },
    };
    r.store_mut().put_signature(&g).unwrap();
    r.seal_open().unwrap();
    r.close().unwrap();
}

#[test]
fn ap10_cli_log_verify_prints_the_policy_lines() {
    let (d, policy_landing, carrier, policy) = policy_project();
    let short = |h: &Hash| h.to_string()[..12].to_string();
    let who = format!("policy {}@{}", policy.block, short(&policy.version));

    // the carrying landing holds no signature
    let rows = json_out(&cli(d.path(), &["--json", "log", "--verify"]));
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["landing"] == policy_landing.to_string())
        .unwrap();
    assert_eq!(row["signed"], "policy", "{row:#}");
    assert_eq!(
        row["policy_check"],
        json!({"landing": carrier.to_string(), "signed": "no"})
    );
    let text = stdout(&cli(d.path(), &["log", "--verify"]));
    let at = text
        .lines()
        .position(|l| l.starts_with(&short(&policy_landing)))
        .unwrap_or_else(|| panic!("{text}"));
    let tail: Vec<&str> = text.lines().skip(at).collect();
    let lines = [
        "    signed: policy".to_string(),
        format!("    {who} landed in {}: signed no", short(&carrier)),
    ];
    for want in &lines {
        assert!(tail.contains(&want.as_str()), "{want}\n{text}");
    }
    // the ordinary landing that carried the policy reads `signed: no`
    assert!(text.contains("    signed: no"), "{text}");
    // the plain log has neither
    let plain = stdout(&cli(d.path(), &["log"]));
    assert!(!plain.contains("signed"), "{plain}");

    // the fsck states print the same lines under the landing
    let fsck = stdout(&cli(d.path(), &["fsck", "--verbose"]));
    let at = fsck
        .lines()
        .position(|l| l == format!("landing {policy_landing}"))
        .unwrap_or_else(|| panic!("{fsck}"));
    let block: Vec<&str> = fsck.lines().skip(at + 1).take(2).collect();
    assert_eq!(block, lines.iter().map(String::as_str).collect::<Vec<_>>());

    // a damaged signature on the carrying landing reads invalid
    put_damaged(d.path(), carrier);
    let text = stdout(&cli(d.path(), &["log", "--verify"]));
    assert!(
        text.contains(&format!(
            "    {who} landed in {}: signed invalid",
            short(&carrier)
        )),
        "{text}"
    );

    // the chat tool gives the same rows as the CLI
    let want = json_out(&cli(d.path(), &["--json", "log", "--verify"]));
    let mut st = McpState::new(Arc::new(Local::new(Rdy::open(d.path()).unwrap())));
    st.agent = Arc::new(UnavailableAgent);
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:a", "role": "reader"})).unwrap(),
    )
    .unwrap();
    let mcp = rdy_log(
        &mut st,
        serde_json::from_value::<LogParams>(json!({"verify": true})).unwrap(),
    )
    .unwrap();
    assert_eq!(mcp, want);
}
