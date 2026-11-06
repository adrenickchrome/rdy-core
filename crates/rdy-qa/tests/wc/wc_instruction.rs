//! ain1 fix round 1 (D-4W1ZR): an instruction's `decided_by` is a store-only link, so its file
//! never shows it. The file lint trusts the hidden link: an untouched checkout checks clean, and
//! a commit of an unrelated edit with an instruction in the tree lands and keeps the reason.
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::wc::*;
use serde_json::json;

const ID: &str = "ai-AAAAA";

/// The seeded project with instruction `ai-AAAAA` (applies_to [all], decided_by the seed's
/// decision) under the notes doc, in view `work`.
fn with_instruction() -> (rdy_qa::daemon::Served, Seed) {
    let (s, seed) = seeded();
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "instruction", "id": ID, "parent": seed.notes,
                "fields": {"applies_to": ["all"]}, "body": "Run the tests.",
                "links": {"decided_by": [{"target": seed.decision}]}}]),
    );
    (s, seed)
}

fn reason_of(s: &rdy_qa::daemon::Served) -> Vec<BlockId> {
    store_block(s, "work", ID)
        .links
        .get("decided_by")
        .map(|l| l.iter().map(|l| l.target.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn ain1_untouched_checkout_with_an_instruction_checks_clean() {
    let (s, _seed) = with_instruction();
    ok(&s, &["checkout"]);
    let text = read(&s, "notes.yaml");
    assert!(text.contains(ID), "{text}");
    assert!(!text.contains("decided_by"), "{text}");
    s.stop();
    let r = Rdy::open(s.dir.path()).expect("open");
    {
        let st = r.read();
        let mat = st.materialized(&Scope::View("work".into())).unwrap();
        let diags = check_wc(st.pack(), &mat, s.dir.path(), None).unwrap();
        assert!(
            diags
                .iter()
                .all(|d| d.severity != rdy_schema::Severity::Error),
            "{diags:?}"
        );
    }
    r.close().expect("close");
    let (okk, v) = json(&s, &["check"]);
    assert!(okk, "{v}");
}

#[test]
fn ain1_unrelated_commit_with_an_instruction_keeps_its_reason() {
    let (s, seed) = with_instruction();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Edited.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "edit the note"]);
    assert_eq!(store_block(&s, "work", &seed.note).body, "Edited.");
    assert_eq!(reason_of(&s), vec![BlockId::parse(&seed.decision).unwrap()]);
    // an edit of the instruction itself keeps its hidden reason too
    set_body(&s, "notes.yaml", ID, "Run every test.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "edit the instruction"]);
    let b = store_block(&s, "work", ID);
    assert_eq!(b.body, "Run every test.");
    assert_eq!(reason_of(&s), vec![BlockId::parse(&seed.decision).unwrap()]);
}

#[test]
fn ain1_new_instruction_in_a_file_still_needs_a_reason() {
    // the stored-block check stays strict: an instruction a file adds has no stored reason
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "notes.yaml", |l| {
        let at = l
            .iter()
            .position(|e| e["id"].as_str() == Some(seed.note.as_str()))
            .unwrap();
        let entry: serde_yaml_ng::Value = serde_yaml_ng::from_str(&format!(
            "id: {ID}\ntype: instruction\nparent: {}\nfields:\n  applies_to: [all]\nbody: Run the tests.\n",
            seed.notes
        ))
        .unwrap();
        l.insert(at + 1, entry);
    });
    // rdy check names it, from the view's missing reason
    let msg = format!("instruction {ID} requires a decided_by decision");
    let o = s.rdy(&["--json", "check"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success() && out.contains(&msg), "{out}");
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "new instruction"]);
    assert!(!okk, "{v}");
    assert!(v.to_string().contains(&msg), "{v}");
}
