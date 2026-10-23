#![cfg(unix)]
// Unix only: it sets file modes for an editor script. FW-XP1 Phase 4 ports it.
//! rf5 (D-Z8P09, D-2WR0G, D-KQ4CA): rdy_schema's examples of a finding and a source are valid
//! writes, the server instructions and the schema guides name the supporting types and the
//! decision basis fixes, and `rdy new finding` and `rdy new source` build the same blocks as
//! chat, keeping the order of the evidence options. `rdy edit` reaches the same checks.
use rdy_engine::decision_fields::{ATTESTED_BY_FIX, TIME_FIX};
use rdy_engine::{Rdy, Scope};
use rdy_model::{BlockId, FieldValue};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{rdy_bin, writer};
use rdy_qa::mcp::McpClient;
use rdy_schema::supporting::{EVIDENCE_FIX, EVIDENCE_MESSAGE};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

const FINDING_LINE: &str = "Finding: record what surfaced with rdy_write, type finding, statement and 1 to 20 evidence entries; cite it from a decision with depends_on";
const TEXT_GUIDE: &str = "A finding records a statement and evidence. Each evidence entry is {quote, session_ref?, question?} or {source: \"{{S-…}}\"}. Each string has 1 to 2000 characters. A source names a release, commit or spec with title, kind and locator. Both types live only in the store. With no parent, they use the decision home.";

/// A served project whose canon holds the doc `register` (the decision home) and a source keyed
/// REL-2 under it, and whose view `work` adds a proposed decision there. Returns (register,
/// source, decision).
fn served() -> (Served, String, String, String) {
    let s = Served::new();
    let cfg = s.dir.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap() + "decisions:\n  home: register\n";
    std::fs::write(&cfg, text).unwrap();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = rdy_qa::fixtures::write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "register", "title": "Register", "doc_type": "overview"}},
        {"op": "create", "type": "source", "id": "$src", "parent": "$d", "fields": {"title": "Release 2", "kind": "release", "locator": "v2", "key": "REL-2"}}
    ]))
    .unwrap();
    let (reg, src) = (w.created["$d"].clone(), w.created["$src"].clone());
    // landed, so an agent's own view holds the home too
    rdy_qa::fixtures::land(&mut r, "work").unwrap();
    let d = r
        .decide(
            &writer(),
            "work",
            rdy_engine::DecisionInput {
                parent: reg.clone(),
                statement: "Retention follows the release".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    r.close().unwrap();
    (s, reg, src, d)
}

fn block(s: &Served, id: &str) -> rdy_model::Block {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let b = r
        .read()
        .block(&Scope::View("work".into()), &BlockId::parse(id).unwrap())
        .unwrap();
    r.close().unwrap();
    b
}

fn evidence(v: Value) -> FieldValue {
    rdy_engine::json_to_field(&v).unwrap()
}

/// The text a failed command printed (its error line).
fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn rf5_supporting_mcp_schema_and_cli_match() {
    let (s, reg, src, decision) = served();

    // chat: the instruction line, the guides and the two examples
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_session", json!({"actor": "agent:p", "role": "writer"}));
    assert!(r.code().is_none(), "{:?}", r.ok());
    assert!(
        rdy_mcp::server::INSTRUCTIONS.contains(FINDING_LINE),
        "{}",
        rdy_mcp::server::INSTRUCTIONS
    );
    let sch = c.call("rdy_schema", json!({})).ok();
    assert!(sch["text"].as_str().unwrap().contains(TEXT_GUIDE), "{sch}");
    let rule = sch["decisions"]["rule"].as_str().unwrap();
    assert!(
        rule.contains(ATTESTED_BY_FIX) && rule.contains(TIME_FIX),
        "{rule}"
    );
    for (ty, fields) in [
        (
            "finding",
            json!({"statement": "The release changed retention.", "evidence": [{"quote": "Retention is now seven days."}]}),
        ),
        (
            "source",
            json!({"title": "Release 2", "kind": "release", "locator": "v2"}),
        ),
    ] {
        let one = c.call("rdy_schema", json!({"block_type": ty})).ok();
        let ex = &one["example"];
        assert_eq!(
            ex,
            &json!({"op": "create", "type": ty, "fields": fields}),
            "{one}"
        );
        // the example is a valid write as it stands: it goes to the decision home
        let w = c.call("rdy_write", json!({"ops": [ex]})).ok();
        let id = w["created"]["op0"].as_str().unwrap().to_string();
        let ctx = c.call("rdy_context", json!({"id": id})).ok();
        assert_eq!(ctx["parent"], reg, "{ctx}");
    }
    drop(c);

    // the CLI: rdy new source, to the home of the human's view
    let out = s.rdy(&[
        "new",
        "source",
        "--title",
        "Release 3",
        "--kind",
        "commit",
        "--locator",
        "abc123",
        "--version",
        "3",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let new_src = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("created source "))
        .unwrap_or_else(|| panic!("{text:?}"))
        .to_string();
    assert!(new_src.starts_with("S-"), "{text:?}");
    let b = block(&s, &new_src);
    assert_eq!(b.parent.as_ref().map(|p| p.to_string()), Some(reg.clone()));
    assert_eq!(b.field_str("kind"), Some("commit"));
    assert_eq!(b.field_str("version"), Some("3"));

    // rdy new finding: each --question and --session joins the latest --quote; a --source is its
    // own entry and neither starts nor ends a quote's group; a key resolves to the source's id
    let (good, v) = s.json(&[
        "--json",
        "new",
        "finding",
        "--statement",
        "Retention changed.",
        "--quote",
        "A",
        "--source",
        "REL-2",
        "--question",
        "Q?",
        "--quote",
        "B",
        "--session",
        "s5",
        "--source",
        &new_src,
    ]);
    assert!(good, "{v}");
    let fid = v["created"]["$finding"].as_str().unwrap().to_string();
    assert!(fid.starts_with("F-"), "{v}");
    assert_eq!(v["view"], "work");
    let f = block(&s, &fid);
    assert_eq!(f.parent.as_ref().map(|p| p.to_string()), Some(reg.clone()));
    assert_eq!(
        f.fields["evidence"],
        evidence(json!([
            {"quote": "A", "question": "Q?"},
            {"source": format!("{{{{{src}}}}}")},
            {"quote": "B", "session_ref": "s5"},
            {"source": format!("{{{{{new_src}}}}}")}
        ]))
    );
    assert!(f.pins.contains_key(&BlockId::parse(&src).unwrap()));
    let out = s.rdy(&[
        "new",
        "finding",
        "--statement",
        "Text out.",
        "--quote",
        "C",
        "--parent",
        &reg,
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.lines()
            .next()
            .unwrap()
            .starts_with("created finding F-"),
        "{text:?}"
    );

    // the grouping refusals, before any write
    for (args, message, fix) in [
        (
            vec!["--quote", "A", "--question", "Q1", "--question", "Q2"],
            "--question is repeated for one --quote",
            "give each quote at most one --question and one --session",
        ),
        (
            vec![
                "--quote",
                "A",
                "--session",
                "x",
                "--source",
                "REL-2",
                "--session",
                "y",
            ],
            "--session is repeated for one --quote",
            "give each quote at most one --question and one --session",
        ),
        (
            vec!["--question", "Q", "--quote", "A"],
            "--question needs a preceding --quote",
            "pass --quote before --question",
        ),
        (
            vec!["--source", "REL-2", "--session", "x"],
            "--session needs a preceding --quote",
            "pass --quote before --session",
        ),
    ] {
        let mut a = vec!["--json", "new", "finding", "--statement", "S"];
        a.extend(args.iter());
        let (good, e) = s.json(&a);
        assert!(!good, "{args:?}: {e}");
        assert_eq!(
            (&e["code"], &e["message"], &e["fix"]),
            (&json!("E_BAD_INPUT"), &json!(message), &json!(fix)),
            "{args:?}"
        );
    }
    // no evidence: the engine's evidence issue
    let (good, e) = s.json(&["--json", "new", "finding", "--statement", "S"]);
    assert!(!good);
    assert_eq!(e["code"], "E_VALIDATION", "{e}");
    assert_eq!(e["message"], EVIDENCE_MESSAGE, "{e}");
    assert_eq!(e["fix"], EVIDENCE_FIX, "{e}");

    // rdy edit reaches the write's checks: a bad time is refused, a good one is written
    let editor = s.dir.path().join("ed.sh");
    let edit = |script: &str| {
        std::fs::write(&editor, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::process::Command::new(rdy_bin())
            .arg("--project")
            .arg(s.dir.path())
            .args(["edit", &decision])
            .env("EDITOR", &editor)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .envs(s.env_pairs())
            .output()
            .unwrap()
    };
    let out =
        edit("perl -0pi -e 's/^fields:\\n/fields:\\n  review_by: next quarter\\n/m' \"$1\"\n");
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("field \"review_by\" must be an RFC 3339 UTC time or a date"),
        "{err}"
    );
    let out = edit("perl -0pi -e 's/^fields:\\n/fields:\\n  review_by: 2027-01-01\\n/m' \"$1\"\n");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.starts_with("change "), "{text:?}");
    assert_eq!(
        block(&s, &decision).field_str("review_by"),
        Some("2027-01-01")
    );
}
