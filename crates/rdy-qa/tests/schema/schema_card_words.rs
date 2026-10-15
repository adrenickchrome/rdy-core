//! di3: the authored words of a decision or question card (FW-I1). `rdy_decide` writes the title,
//! the why and the recommendation; a question carries its options; a lint warns when the words
//! are missing or long, and never refuses a write.
use rdy_engine::engine::Local;
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_mcp::tools::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

const WORDS: &str = "W_CARD_WORDS";

fn section(r: &mut Rdy) -> String {
    let res = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Register", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rows"}}
    ]))
    .unwrap();
    res.created["$s"].clone()
}

/// An MCP state on a fresh project, with an agent writer session, and a section in its view.
fn mcp() -> (tempfile::TempDir, McpState, String) {
    let (d, r) = project();
    let mut st = McpState::new(std::sync::Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(
            json!({"actor": "agent:designer", "run": "run-3", "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
    let res = rdy_write(&mut st, serde_json::from_value(json!({"ops": [
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Register", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Rows"}}
    ]})).unwrap()).unwrap();
    let s = res["created"]["$s"].as_str().unwrap().to_string();
    (d, st, s)
}

fn card_issues(warnings: &[rdy_schema::Issue]) -> Vec<(String, String)> {
    warnings
        .iter()
        .filter(|w| w.code == WORDS)
        .map(|w| (w.message.clone(), w.fix.clone()))
        .collect()
}

fn decide(r: &mut Rdy, parent: &str, d: DecisionInput) -> rdy_engine::WriteResult {
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: parent.to_string(),
            statement: "Balances live in one store".into(),
            ..d
        },
    )
    .unwrap()
}

fn question(r: &mut Rdy, parent: &str, fields: Value) -> rdy_engine::WriteResult {
    let mut f = json!({"prompt": "Who approves refunds?"});
    f.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    write(
        r,
        "work",
        json!([
            {"op": "create", "type": "question", "id": "$q", "parent": parent, "fields": f}
        ]),
    )
    .unwrap()
}

fn good_decision() -> DecisionInput {
    DecisionInput {
        title: Some("Keep balances in one store".into()),
        why: Some("Two stores would drift apart.".into()),
        recommend: Some("confirm".into()),
        recommend_reason: Some("It matches the ruling of 2026-09-02.".into()),
        ..Default::default()
    }
}

#[test]
fn di3_rdy_decide_writes_the_card_words() {
    let (_d, mut st, s) = mcp();
    let out = rdy_decide(
        &mut st,
        serde_json::from_value(json!({
            "parent": s,
            "statement": "Balances live in one store",
            "title": "Keep balances in one store",
            "why": "Two stores would drift apart.",
            "recommend": "confirm",
            "recommend_reason": "It matches the ruling of 2026-09-02."
        }))
        .unwrap(),
    )
    .unwrap();
    let id = BlockId::parse(out["created"]["$decision"].as_str().unwrap()).unwrap();
    let b = st
        .engine
        .read()
        .block(&Scope::View(st.view.clone()), &id)
        .unwrap();
    assert_eq!(b.field_str("title"), Some("Keep balances in one store"));
    assert_eq!(b.field_str("why"), Some("Two stores would drift apart."));
    assert_eq!(b.field_str("recommend"), Some("confirm"));
    assert_eq!(
        b.field_str("recommend_reason"),
        Some("It matches the ruling of 2026-09-02.")
    );
    let warned: Vec<&Value> = out["warnings"]
        .as_array()
        .map(|w| w.iter().filter(|w| w["code"] == WORDS).collect())
        .unwrap_or_default();
    assert!(warned.is_empty(), "{warned:?}");
}

#[test]
fn di3_a_decision_without_a_title_warns() {
    let (_d, mut r) = project();
    let s = section(&mut r);
    let res = decide(&mut r, &s, DecisionInput::default());
    let id = &res.created["$decision"];
    assert_eq!(
        card_issues(&res.warnings),
        vec![
            (
                format!("{id} has no card title"),
                "add a plain-words title of 80 characters or fewer".to_string()
            ),
            (
                format!("{id} has no card why"),
                "add one line on why a person must answer it".to_string()
            ),
        ]
    );
    let res = decide(&mut r, &s, good_decision());
    assert!(card_issues(&res.warnings).is_empty(), "{:?}", res.warnings);
    let q = question(&mut r, &s, json!({}));
    let qid = &q.created["$q"];
    assert_eq!(
        card_issues(&q.warnings)
            .iter()
            .map(|(m, _)| m.clone())
            .collect::<Vec<_>>(),
        vec![
            format!("{qid} has no card title"),
            format!("{qid} has no card why")
        ]
    );
}

#[test]
fn di3_a_long_why_warns() {
    let (_d, mut r) = project();
    let s = section(&mut r);
    let long = "w".repeat(161);
    let res = decide(
        &mut r,
        &s,
        DecisionInput {
            why: Some(long.clone()),
            title: Some("t".repeat(81)),
            recommend_reason: Some("r".repeat(201)),
            recommend: Some("reject".into()),
            ..Default::default()
        },
    );
    let id = &res.created["$decision"];
    assert_eq!(
        card_issues(&res.warnings),
        vec![
            (
                format!("{id}: title is longer than 80 characters"),
                "shorten title to 80 characters or fewer".to_string()
            ),
            (
                format!("{id}: why is longer than 160 characters"),
                "shorten why to 160 characters or fewer".to_string()
            ),
            (
                format!("{id}: recommend_reason is longer than 200 characters"),
                "shorten recommend_reason to 200 characters or fewer".to_string()
            ),
        ]
    );
    // the limits are inclusive
    let ok = decide(
        &mut r,
        &s,
        DecisionInput {
            why: Some("w".repeat(160)),
            title: Some("t".repeat(80)),
            ..Default::default()
        },
    );
    let id = &ok.created["$decision"];
    assert_eq!(
        card_issues(&ok.warnings)
            .iter()
            .map(|(m, _)| m.clone())
            .collect::<Vec<_>>(),
        Vec::<String>::new(),
        "{id}"
    );
}

#[test]
fn di3_a_question_with_one_option_warns() {
    let (_d, mut r) = project();
    let s = section(&mut r);
    let base = json!({"title": "Who approves refunds?", "why": "Refunds wait on it."});
    let with = |options: Value| {
        let mut f = base.clone();
        f["options"] = options;
        f
    };
    let q = question(&mut r, &s, with(json!(["Finance"])));
    let id = &q.created["$q"];
    assert_eq!(
        card_issues(&q.warnings),
        vec![(
            format!("{id} has 1 options"),
            "give two to four options".to_string()
        )]
    );
    let q = question(&mut r, &s, with(json!(["a", "b", "c", "d", "e"])));
    assert_eq!(
        card_issues(&q.warnings)[0].0,
        format!("{} has 5 options", q.created["$q"])
    );
    for n in 2..=4 {
        let opts: Vec<String> = (0..n).map(|i| format!("option {i}")).collect();
        let q = question(&mut r, &s, with(json!(opts)));
        assert!(card_issues(&q.warnings).is_empty(), "{n}: {:?}", q.warnings);
    }
    let q = question(&mut r, &s, with(json!(["Finance", "o".repeat(61)])));
    assert_eq!(
        card_issues(&q.warnings)[0].0,
        format!("{}: options is longer than 60 characters", q.created["$q"])
    );
    // a non-string entry is counted and named, never dropped
    let q = question(&mut r, &s, with(json!(["Finance", "Support", 1, 2, 3])));
    let id = &q.created["$q"];
    assert_eq!(
        card_issues(&q.warnings),
        vec![
            (
                format!("{id} has 5 options"),
                "give two to four options".to_string()
            ),
            (
                format!("{id}: options holds an entry that is not a string"),
                "make every option a string".to_string()
            ),
        ]
    );
    let q = question(&mut r, &s, with(json!(["Finance", 1])));
    let id = &q.created["$q"];
    assert_eq!(
        card_issues(&q.warnings),
        vec![(
            format!("{id}: options holds an entry that is not a string"),
            "make every option a string".to_string()
        )]
    );
    // no options at all is not a card-words issue: the question is not a card
    let q = question(&mut r, &s, base);
    assert!(card_issues(&q.warnings).is_empty(), "{:?}", q.warnings);
}

#[test]
fn di3_recommended_must_be_an_option() {
    let (_d, mut r) = project();
    let s = section(&mut r);
    let f = |rec: &str| {
        json!({"title": "Who approves?", "why": "Refunds wait on it.",
            "options": ["Finance", "Support"], "recommended": rec,
            "recommend_reason": "Finance owns the ledger."})
    };
    let q = question(&mut r, &s, f("Legal"));
    assert_eq!(
        card_issues(&q.warnings),
        vec![(
            format!("{} recommends an option it does not list", q.created["$q"]),
            "set recommended to one of the options".to_string()
        )]
    );
    let q = question(&mut r, &s, f("Finance"));
    assert!(card_issues(&q.warnings).is_empty(), "{:?}", q.warnings);
}

#[test]
fn di3_a_blank_recommendation_is_still_a_recommendation() {
    let (_d, mut r) = project();
    let s = section(&mut r);
    for blank in ["", "   "] {
        let q = question(
            &mut r,
            &s,
            json!({"title": "Who approves?", "why": "Refunds wait on it.",
                "options": ["Finance", "Support"], "recommended": blank}),
        );
        let id = &q.created["$q"];
        assert_eq!(
            card_issues(&q.warnings),
            vec![
                (
                    format!("{id} recommends an option it does not list"),
                    "set recommended to one of the options".to_string()
                ),
                (
                    format!("{id} recommends without a reason"),
                    "add recommend_reason in one line".to_string()
                ),
            ],
            "{blank:?}"
        );
        // a blank reason still counts as missing; with a real one only the membership warns
        let q = question(
            &mut r,
            &s,
            json!({"title": "Who approves?", "why": "Refunds wait on it.",
                "options": ["Finance", "Support"], "recommended": blank,
                "recommend_reason": "Finance owns the ledger."}),
        );
        assert_eq!(
            card_issues(&q.warnings),
            vec![(
                format!("{} recommends an option it does not list", q.created["$q"]),
                "set recommended to one of the options".to_string()
            )],
            "{blank:?}"
        );
    }
}

#[test]
fn di3_a_recommendation_needs_a_reason() {
    let (_d, mut r) = project();
    let s = section(&mut r);
    let res = decide(
        &mut r,
        &s,
        DecisionInput {
            recommend_reason: None,
            ..DecisionInput {
                title: good_decision().title,
                why: good_decision().why,
                recommend: Some("confirm".into()),
                ..Default::default()
            }
        },
    );
    assert_eq!(
        card_issues(&res.warnings),
        vec![(
            format!("{} recommends without a reason", res.created["$decision"]),
            "add recommend_reason in one line".to_string()
        )]
    );
    let q = question(
        &mut r,
        &s,
        json!({"title": "Who approves?", "why": "Refunds wait on it.",
            "options": ["Finance", "Support"], "recommended": "Finance"}),
    );
    assert_eq!(
        card_issues(&q.warnings),
        vec![(
            format!("{} recommends without a reason", q.created["$q"]),
            "add recommend_reason in one line".to_string()
        )]
    );
}

#[test]
fn di3_card_words_never_refuse_a_write() {
    let (_d, mut r) = project();
    let s = section(&mut r);
    // every issue at once: the write still lands, and the block is in the view
    let q = question(
        &mut r,
        &s,
        json!({"title": "t".repeat(200), "options": ["only"], "recommended": "other"}),
    );
    assert!(card_issues(&q.warnings).len() >= 4, "{:?}", q.warnings);
    let qid = BlockId::parse(&q.created["$q"]).unwrap();
    assert!(r.read().block(&Scope::View("work".into()), &qid).is_ok());
    let d = decide(
        &mut r,
        &s,
        DecisionInput {
            recommend: Some("reject".into()),
            ..Default::default()
        },
    );
    assert!(!card_issues(&d.warnings).is_empty());
    land(&mut r, "work").unwrap();
}

#[test]
fn di3_fields_may_not_carry_the_card_words() {
    let (_d, mut st, s) = mcp();
    for name in ["title", "why", "recommend", "recommend_reason"] {
        let mut fields = serde_json::Map::new();
        fields.insert(name.to_string(), json!("x"));
        let e = rdy_decide(
            &mut st,
            serde_json::from_value(json!({"parent": s, "statement": "x", "fields": fields}))
                .unwrap(),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{name}: {e}");
        assert_eq!(
            e.message,
            format!("fields.{name} is set by rdy_decide, not through fields")
        );
    }
}
