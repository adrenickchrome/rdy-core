//! sig6: `rdy fsck` checks every stored signature with no SSH agent and no trust file. A failure
//! returns its bounded issues (the first 999, then one E_LIMIT count) and removes no quarantine
//! file. The pure check, trust, coverage and the fork source are in rdy-engine's
//! `tests/signature_check.rs`.
use rdy_engine::Rdy;
use rdy_model::signature::Signature;
use rdy_model::{Hash, Provenance, Role};
use rdy_qa::daemon::{Served, human_command};
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

/// A stored signature of `subject` whose statement and blob are damaged.
fn damaged(subject: Hash, at_ms: i64) -> Signature {
    Signature {
        subject,
        namespace: "rdy-approval-v1".into(),
        statement: b"rdy-statement: 1\n".to_vec(),
        sig: b"damaged".to_vec(),
        signed_by: Provenance {
            actor: "agent:s".into(),
            run: None,
            role: Role::Writer,
            at_ms,
        },
    }
}

#[test]
fn sig6_cli_fsck_fails_closed_without_agent_and_keeps_quarantine() {
    let s = Served::new();
    // the direct handle writes before any command starts the daemon
    let mut r = Rdy::open(s.dir.path()).unwrap();
    seed_doc(&mut r, "work");
    let l: Hash = land(&mut r, "work").unwrap().parse().unwrap();
    let sigs: Vec<Signature> = (0..1001).map(|i| damaged(l, i)).collect();
    let frames = rdy_store::signature::frames(&sigs).unwrap();
    let mut ids = r.store_mut().write(frames).unwrap();
    r.seal_open().unwrap();
    r.close().unwrap();
    ids.sort();
    let store = s.dir.path().join(".rdy/store");
    std::fs::write(store.join("index.redb.damaged-1"), b"old index").unwrap();

    // no SSH_AUTH_SOCK reaches the client or the daemon it starts; no trust file is set
    let run = |args: &[&str]| {
        let mut cmd = human_command(s.dir.path(), &s.env_pairs(), args);
        cmd.env_remove("SSH_AUTH_SOCK");
        cmd.output().expect("run rdy")
    };
    let o = run(&["--json", "fsck", "--clean-quarantine"]);
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    let v: Value = serde_json::from_str(stderr.trim()).unwrap_or_else(|_| panic!("{stderr}"));
    let first = format!("signature {} is invalid", ids[0]);
    assert_eq!(
        (&v["code"], &v["message"], &v["fix"]),
        (
            &json!("E_SIG_INVALID"),
            &json!(first),
            &json!("check the source of this signature; it may be damaged or forged")
        ),
        "{v}"
    );
    let issues = v["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1000);
    for (i, id) in issues[..999].iter().zip(&ids) {
        assert_eq!(
            i,
            &json!({
                "severity": "error", "code": "E_SIG_INVALID", "block": null,
                "message": format!("signature {id} is invalid"),
                "fix": "check the source of this signature; it may be damaged or forged"
            })
        );
    }
    assert_eq!(
        issues[999],
        json!({
            "severity": "error", "code": "E_LIMIT", "block": null,
            "message": "fsck omitted 2 signature issues",
            "fix": "fix the reported signatures, then run rdy fsck again"
        })
    );
    assert!(
        store.join("index.redb.damaged-1").exists(),
        "a failed fsck removes no quarantine file"
    );

    // the text form names the code and the issues
    let o = run(&["fsck"]);
    assert!(!o.status.success());
    let text = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        text.starts_with(&format!("error E_SIG_INVALID: {first}\n")),
        "{text}"
    );
    assert!(
        text.contains("  E_LIMIT None: fsck omitted 2 signature issues"),
        "{text}"
    );
    assert!(store.join("index.redb.damaged-1").exists());
}
