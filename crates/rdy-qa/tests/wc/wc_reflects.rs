//! rf8 (D-4WC6R, D-XC9B9): `rdy commit` and `rdy edit` take `--reflects <source>` (repeatable)
//! and `--reflects-code`, send the raw object the daemon's writer resolves, and store the same
//! normalized justification rdy_write stores. A lost acknowledgement recovers to the one change
//! with that justification; `code_recorded` reaches the JSON reply unchanged.
use rdy_engine::wc::state::*;
use rdy_engine::*;
use rdy_model::{BlockId, ChangeLinks, Hash, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::rdy_bin;
use rdy_qa::wc::*;
use serde_json::{Value, json};

fn journal(s: &Served, rewrite: &str, file: &str) -> String {
    let place = Place::locate(s.dir.path()).unwrap();
    let mut st = WcState::load(&place).unwrap();
    let nonce = new_nonce();
    st.pending = Some(Pending {
        nonce: nonce.clone(),
        view: "work".into(),
        assigned: vec![],
        rewrite: vec![BlockId::parse(rewrite).unwrap()],
        files: vec![file.into()],
        at_ms: 1,
    });
    st.save(&place).unwrap();
    nonce
}

fn stored(s: &Served, change: &str) -> Option<Justification> {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let j = r
        .read()
        .reader()
        .change(&change.parse::<Hash>().unwrap())
        .unwrap()
        .justification;
    r.close().unwrap();
    j
}

/// `rdy --project <dir> --json edit <args>` as the human, with `EDITOR=editor` (a command line
/// with no quoting: the edit runs `$EDITOR "$1"`).
fn edit_cli(s: &Served, editor: &str, args: &[&str]) -> (bool, Value) {
    let out = std::process::Command::new(rdy_bin())
        .arg("--project")
        .arg(s.dir.path())
        .arg("--json")
        .arg("edit")
        .args(args)
        .env("EDITOR", editor)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(s.env_pairs())
        .output()
        .unwrap();
    let text = if out.status.success() {
        &out.stdout
    } else {
        &out.stderr
    };
    let v = serde_json::from_slice(text).unwrap_or_else(|_| {
        json!({"raw": String::from_utf8_lossy(text), "stdout": String::from_utf8_lossy(&out.stdout)})
    });
    (out.status.success(), v)
}

#[test]
fn rf8_mcp_cli_and_recovered_commit_record_one_justification() {
    let (s, seed) = seeded();
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "create", "type": "source", "id": "$src", "parent": seed.section,
                "fields": {"title": "Release 2", "kind": "release", "locator": "v2", "key": "SRC-ONE"}},
            {"op": "update", "id": seed.note, "fields": {"reflects": ["src/*.rs"]}}
        ]),
    );
    store_land(&s, "work");
    let src = w.created["$src"].clone();
    let want = Justification::Reflects {
        sources: vec![BlockId::parse(&src).unwrap()],
        code: false,
    };
    ok(&s, &["checkout"]);

    // commit: a key and an id of one source normalize to one
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    // the flags conflict with the other justifications
    for extra in [
        vec!["--implements", seed.decision.as_str()],
        vec!["--editorial", "typo", "x"],
    ] {
        let mut args = vec!["commit", "-m", "x", "--reflects", "SRC-ONE"];
        args.extend(extra.iter().copied());
        let o = s.rdy(&args);
        assert!(!o.status.success(), "{args:?}");
        let mut args = vec!["commit", "-m", "x", "--reflects-code"];
        args.extend(extra.iter().copied());
        assert!(!s.rdy(&args).status.success(), "{args:?}");
    }
    // a refusal of the source is the writer's own, not a problem in a file; the stage stays
    let (okk, v) = json(
        &s,
        &["commit", "-m", "x", "--reflects", seed.claim.as_str()],
    );
    assert!(!okk, "{v}");
    assert_eq!(v["code"], E_BAD_INPUT, "{v}");
    assert_eq!(
        v["message"],
        format!(
            "reflects names {}, a claim; reflects names a source",
            seed.claim
        )
    );
    let (okk, v) = json(
        &s,
        &[
            "commit",
            "-m",
            "follow release 2",
            "--reflects",
            "SRC-ONE",
            "--reflects",
            src.as_str(),
        ],
    );
    assert!(okk, "{v}");
    assert_eq!(
        v["justification"],
        json!({"reflects": {"sources": [src], "code": false}}),
        "{v}"
    );
    assert!(v.get("code_recorded").is_none(), "{v}");
    let change = v["change"].as_str().unwrap().to_string();
    assert_eq!(stored(&s, &change), Some(want.clone()));

    // --reflects-code with no linked checkout: code_recorded is [] in the reply, as written
    set_body(&s, "notes.yaml", &seed.note, "Mine, with code.");
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "code", "--reflects-code"]);
    assert!(okk, "{v}");
    assert_eq!(v["code_recorded"], json!([]), "{v}");
    assert_eq!(
        v["justification"],
        json!({"reflects": {"sources": [], "code": true}})
    );
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "W_REFLECTS_NO_CODE"),
        "{v}"
    );

    // edit: the same flags, conflicting with --reason
    let (okk, _) = edit_cli(
        &s,
        "true",
        &[seed.note.as_str(), "--reflects", "SRC-ONE", "--reason", "x"],
    );
    assert!(!okk);
    let (okk, v) = edit_cli(
        &s,
        "perl -pi -es/code/release/",
        &[seed.note.as_str(), "--reflects", "SRC-ONE"],
    );
    assert!(okk, "{v}");
    assert_eq!(
        v["justification"],
        json!({"reflects": {"sources": [src], "code": false}}),
        "{v}"
    );
    let change = v["change"].as_str().unwrap().to_string();
    assert_eq!(stored(&s, &change), Some(want.clone()));
    assert_eq!(
        store_block(&s, "work", &seed.note).body,
        "Mine, with release."
    );

    // a lost acknowledgement: the change is in the view with the journal's nonce; recovery
    // finishes the one change, which keeps its justification
    ok(&s, &["update"]);
    set_body(&s, "notes.yaml", &seed.note, "Recovered.");
    ok(&s, &["add", "notes.yaml"]);
    let nonce = journal(&s, &seed.notes, "notes.yaml");
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let ops = parse_ops(vec![
        json!({"op": "update", "id": seed.note, "body": "Recovered."}),
    ])
    .unwrap();
    let lost = r
        .write_reflecting(
            &rdy_qa::fixtures::writer(),
            WriteRequest {
                view: "work".into(),
                ops,
                justification: None,
                message: "lost".into(),
            },
            ReflectsInput {
                sources: vec!["SRC-ONE".into()],
                code: false,
            },
            ChangeLinks {
                client_nonce: Some(nonce.clone()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    r.close().unwrap();
    let o = s.rdy(&["status"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("recovered commit"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let (okk, v) = json(&s, &["commit", "-m", "again"]);
    assert!(
        !okk && v["message"] == "nothing staged",
        "never committed twice: {v}"
    );
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    {
        let st = r.read();
        let found = st.change_by_nonce("work", &nonce, 1000).unwrap();
        assert_eq!(
            found.change.map(|h| h.to_string()),
            Some(lost.change.clone())
        );
        assert_eq!(
            st.included("work").last().map(ToString::to_string),
            Some(lost.change.clone()),
            "the lost change is the view's last"
        );
        assert_eq!(
            st.reader()
                .change(&lost.change.parse::<Hash>().unwrap())
                .unwrap()
                .justification,
            Some(want)
        );
    }
    r.close().unwrap();
}
