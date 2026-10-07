//! ent1: the entity helpers (FW-EN1, FW-EN2, FW-EN3): words and phrases, phrase finding,
//! aliases, a value's text, code literals, and the text a mention scan reads.
use rdy_model::entity::*;
use rdy_model::{Block, BlockId, FieldValue};
use std::collections::{BTreeMap, BTreeSet};

fn block(t: &str, fields: &[(&str, FieldValue)], body: &str) -> Block {
    Block {
        id: BlockId::parse("c-AAAAA").unwrap(),
        block_type: t.into(),
        parent: None,
        order: "a1".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn s(x: &str) -> FieldValue {
    FieldValue::Str(x.into())
}

#[test]
fn ent1_words_keep_inner_dashes_dots_and_underscores_and_fold_case() {
    let w: Vec<String> = words("The Ledger-service runs rdy_search v1.2. Done!")
        .into_iter()
        .map(|w| w.folded)
        .collect();
    assert_eq!(
        w,
        vec![
            "the",
            "ledger-service",
            "runs",
            "rdy_search",
            "v1.2",
            "done"
        ]
    );
    let first = &words("  Ab-c d")[0];
    assert_eq!((first.start, first.end), (2, 6));
    assert_eq!(phrase("  Request   TIMEOUT "), "request timeout");
    assert_eq!(phrase("--"), "");
}

#[test]
fn ent1_find_phrases_prefers_the_longest_and_never_overlaps() {
    let known: BTreeSet<&str> = ["ledger", "ledger service", "service", "id"].into();
    let text = "The Ledger Service calls the ledger. An id.";
    let found = find_phrases(text, &|p| known.contains(p));
    let got: Vec<(&str, &str)> = found
        .iter()
        .map(|(p, r)| (p.as_str(), &text[r.clone()]))
        .collect();
    assert_eq!(
        got,
        vec![("ledger service", "Ledger Service"), ("ledger", "ledger")]
    );
}

#[test]
fn ent1_aliases_are_the_string_items_trimmed_and_without_repeats() {
    let b = block(
        "entity",
        &[
            ("name", s("Ledger service")),
            (
                "aliases",
                FieldValue::List(vec![
                    s(" ledger "),
                    s("LEDGER"),
                    FieldValue::Int(3),
                    s(""),
                    s("the books"),
                ]),
            ),
        ],
        "",
    );
    assert_eq!(aliases(&b), vec!["ledger", "the books"]);
    let many: Vec<FieldValue> = (0..100).map(|i| s(&format!("a{i}"))).collect();
    let b = block("entity", &[("aliases", FieldValue::List(many))], "");
    assert_eq!(aliases(&b).len(), MAX_ALIASES);
}

#[test]
fn ent1_a_value_reads_as_its_value_and_unit() {
    let v = |val: &str, unit: Option<&str>| {
        let mut f = vec![("name", s("Timeout")), ("value", s(val))];
        if let Some(u) = unit {
            f.push(("unit", s(u)));
        }
        value_text(&block("value", &f, ""))
    };
    assert_eq!(v("5", Some("ms")).as_deref(), Some("5 ms"));
    assert_eq!(v("50", Some("%")).as_deref(), Some("50%"));
    assert_eq!(v(" 3 ", None).as_deref(), Some("3"));
    assert_eq!(v(" ", Some("ms")), None);
    assert_eq!(value_text(&block("term", &[("value", s("x"))], "")), None);
}

#[test]
fn ent1_code_literals_are_short_single_line_spans() {
    let text = "Set `max_conn` and `max_conn`; not `` or `x`.\nA `{{c-AAAAA}}` ref and `unclosed\nline` end.";
    assert_eq!(code_literals(text), vec!["max_conn", "max_conn"]);
}

#[test]
fn ent1_text_sources_skip_code_bodies_and_keys_and_blank_references() {
    let c = block(
        "flow",
        &[
            ("key", s("REQ-1")),
            ("name", s("Close")),
            ("steps", FieldValue::List(vec![s("see Ledger")])),
        ],
        "Uses {{m-AAAAA}} and the Ledger.",
    );
    assert_eq!(
        text_sources(&c),
        vec![
            (
                "body".to_string(),
                format!("Uses {} and the Ledger.", " ".repeat(11))
            ),
            ("name".to_string(), "Close".to_string()),
            ("steps[0]".to_string(), "see Ledger".to_string()),
        ]
    );
    let code = block("code", &[("lang", s("sh"))], "ledger start");
    assert_eq!(
        text_sources(&code),
        vec![("lang".to_string(), "sh".to_string())]
    );
}
