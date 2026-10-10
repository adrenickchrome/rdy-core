//! mfb6: rdy commit prints and carries the write reply's later keys (M2f-a hand-over item 4):
//! intents, tensions opened (on_change) and suggestions. related_decisions cannot reach a commit
//! (decisions are never in files); brief_text's unit tests cover it.
use rdy_qa::wc::*;
use serde_json::json;

#[test]
fn mfb6_a_commit_prints_and_carries_the_intents_on_its_blocks() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let (okk, v) = s.json_as(
        "agent:a",
        Some("ra"),
        &[
            "--json",
            "intent",
            "post",
            "--blocks",
            seed.note.as_str(),
            "--angle",
            "rewording",
        ],
    );
    assert!(okk, "{v}");
    set_body(&s, "notes.yaml", &seed.note, "Note, reworded.");
    ok(&s, &["add", "notes.yaml"]);
    let text = ok(&s, &["commit", "-m", "reword"]);
    assert!(
        text.contains("\nintents on these blocks:\n  intent "),
        "{text}"
    );
    assert!(text.contains(" by agent:a/ra "), "{text}");
    set_body(&s, "notes.yaml", &seed.note, "Note, reworded again.");
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "again"]);
    assert!(okk, "{v}");
    assert_eq!(v["intents"][0]["actor"], "agent:a", "{v}");
}

#[test]
fn mfb6_a_commit_that_opens_a_tension_prints_it() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "notes.yaml", |list| {
        for _ in 0..2 {
            list.push(
                serde_yaml_ng::to_value(
                    json!({"type": "term", "fields": {"name": "Fee"}, "body": "Fee is a term."}),
                )
                .unwrap(),
            );
        }
    });
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "two terms"]);
    assert!(okk, "{v}");
    assert_eq!(
        v["on_change"]["tensions"][0]["kind"], "duplicate_definition",
        "{v}"
    );
    edit(&s, "notes.yaml", |list| {
        list.push(
            serde_yaml_ng::to_value(
                json!({"type": "term", "fields": {"name": "Rate"}, "body": "Rate is a term."}),
            )
            .unwrap(),
        );
        list.push(
            serde_yaml_ng::to_value(
                json!({"type": "term", "fields": {"name": "Rate"}, "body": "Rate is a number."}),
            )
            .unwrap(),
        );
    });
    ok(&s, &["add", "notes.yaml"]);
    let text = ok(&s, &["commit", "-m", "two more"]);
    assert!(text.contains("\ntensions opened:\n  "), "{text}");
    assert!(text.contains("duplicate_definition"), "{text}");
}

#[test]
fn mfb6_a_commit_that_names_an_entity_prints_the_suggestion() {
    let (s, seed) = seeded();
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "entity", "parent": seed.section,
            "fields": {"name": "Ledger service", "aliases": ["ledger"]}}]),
    );
    ok(&s, &["checkout"]);
    edit(&s, "notes.yaml", |list| {
        list.push(
            serde_yaml_ng::to_value(json!({"type": "prose", "body": "The ledger closes daily."}))
                .unwrap(),
        );
    });
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "ledger"]);
    assert!(okk, "{v}");
    assert_eq!(v["suggestions"][0]["kind"], "link", "{v}");
    edit(&s, "notes.yaml", |list| {
        list.push(
            serde_yaml_ng::to_value(
                json!({"type": "prose", "body": "The ledger reopens at nine."}),
            )
            .unwrap(),
        );
    });
    ok(&s, &["add", "notes.yaml"]);
    let text = ok(&s, &["commit", "-m", "ledger again"]);
    assert!(text.contains("\nsuggestions:\n  link "), "{text}");
}
