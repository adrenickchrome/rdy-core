#![cfg(unix)]
// Unix only: it runs the working-copy CLI against a served project. FW-XP1 Phase 4 ports it.
//! ab1 (D-WJ0SJ): a content block's `planned` and `since` overrides are ordinary fields of its
//! working-copy entry. They round-trip through a checkout and a commit; a typo or an invalid
//! version is refused whole, and nothing in the store moves. A decision, with its `built`,
//! stays in the store and never reaches a file.
use rdy_engine::E_WC_INVALID;
use rdy_qa::wc::*;
use serde_json::Value;

/// Sets `fields` (a YAML mapping) on block `id` of `rel`, replacing its `fields` entry.
fn set_fields(s: &rdy_qa::daemon::Served, rel: &str, id: &str, fields: &str) {
    edit(s, rel, |list| {
        let e = list
            .iter_mut()
            .find(|e| e["id"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("{id} not in {rel}"));
        e.as_mapping_mut()
            .unwrap()
            .insert("fields".into(), serde_yaml_ng::from_str(fields).unwrap());
    });
}

fn refused(s: &rdy_qa::daemon::Served, rel: &str, what: &str) -> Value {
    let o = s.rdy(&["--json", "check"]);
    assert!(!o.status.success(), "{what}: check passed");
    ok(s, &["add", rel]);
    let (good, v) = json(s, &["commit", "-m", what]);
    assert!(!good && v["code"] == E_WC_INVALID, "{what}: {v}");
    v
}

#[test]
fn ab1_content_overrides_round_trip_and_bad_input_is_atomic() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let rel = "guides/intro.yaml";
    let original = read(&s, rel);
    assert!(
        !original.contains("built") && !original.contains(&seed.decision),
        "a decision and its status are store-only:\n{original}"
    );

    // planned and since are plain fields of a content entry: a commit records them
    set_fields(&s, rel, &seed.claim, "planned: true\nsince: v0.36.0\n");
    set_fields(&s, rel, &seed.prose, "planned: false\n");
    let (good, v) = json(&s, &["check"]);
    assert!(good && v.as_array().unwrap().is_empty(), "{v}");
    ok(&s, &["add", rel]);
    ok(&s, &["commit", "-m", "mark the claim"]);
    let claim = store_block(&s, "work", &seed.claim);
    assert_eq!(
        claim.fields.get("planned"),
        Some(&rdy_model::FieldValue::Bool(true))
    );
    assert_eq!(
        claim.field_str("since"),
        Some("v0.36.0"),
        "the spelling is kept"
    );
    let prose = store_block(&s, "work", &seed.prose);
    assert_eq!(
        prose.fields.get("planned"),
        Some(&rdy_model::FieldValue::Bool(false)),
        "false is stored, not dropped"
    );
    assert!(!prose.fields.contains_key("since"));

    // a refresh writes the same values back to the file
    let (good, v) = json(&s, &["update"]);
    assert!(good, "{v}");
    let e = entry(&s, rel, &seed.claim);
    assert_eq!(e["fields"]["planned"], serde_yaml_ng::Value::Bool(true));
    assert_eq!(e["fields"]["since"], serde_yaml_ng::Value::from("v0.36.0"));
    let e = entry(&s, rel, &seed.prose);
    assert_eq!(e["fields"]["planned"], serde_yaml_ng::Value::Bool(false));

    // each bad input is refused whole: the diagnostic names it, and the store keeps the claim
    let good_file = read(&s, rel);
    let before = store_block(&s, "work", &seed.claim);
    for (label, fields, code, message) in [
        (
            "typo",
            "planned: true\nsinse: 0.37.0\n",
            "E_FIELD_UNKNOWN",
            "type claim has no field \"sinse\"",
        ),
        (
            "version",
            "since: \"1.2\"\n",
            "E_BAD_INPUT",
            "Field \"since\" must be a release version.",
        ),
        (
            "metadata",
            "since: 1.2.0+build.5\n",
            "E_BAD_INPUT",
            "Field \"since\" must be a release version.",
        ),
        (
            "kind",
            "planned: hide\n",
            "E_FIELD_KIND",
            "field \"planned\" must be Bool",
        ),
    ] {
        set_fields(&s, rel, &seed.claim, fields);
        let v = refused(&s, rel, label);
        let found = v["diagnostics"].as_array().unwrap().iter().any(|d| {
            d["file"] == rel
                && d["code"] == code
                && d["message"].as_str().is_some_and(|m| m.contains(message))
        });
        assert!(found, "{label}: {v}");
        assert_eq!(
            store_block(&s, "work", &seed.claim),
            before,
            "{label}: nothing is recorded"
        );
        put(&s, rel, &good_file);
        ok(&s, &["add", rel]);
    }

    // the supporting decision still has its status in the store, and no file shows it
    let decision = store_block(&s, "work", &seed.decision);
    assert_eq!(decision.field_str("built"), Some("planned"));
    for f in [rel, "notes.yaml"] {
        let text = read(&s, f);
        assert!(
            !text.contains("built") && !text.contains(&seed.decision),
            "{f}:\n{text}"
        );
    }
}
