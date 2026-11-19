//! rtc1 (FW-MH.rules, D-ZSRBN): the `rdy_rule` input contract. The forms are closed and bounded,
//! and every refusal comes before any writer opens.
use rdy_engine::rule_types::{
    AnswerRule, MAX_EVIDENCE_CHARS, MAX_TEXT_BYTES, RuleRequest, RulesetRef, Scope, parse_request,
};
use rdy_engine::{E_BAD_INPUT, E_LIMIT, E_PERMISSION, E_VALIDATION};
use serde_json::{Value, json};

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn add() -> Value {
    json!({"scope": "project", "op": "add", "text": "Run the tests.",
           "why": "The team asked.", "evidence": "yes, run them"})
}

fn edit() -> Value {
    json!({"scope": "person", "op": "edit", "id": "ai-AAAAA", "text": "Run all tests.",
           "why": "Wider.", "evidence": "yes"})
}

fn remove() -> Value {
    json!({"scope": "project", "op": "remove", "id": "ai-AAAAA", "why": "Stale.",
           "evidence": "drop it"})
}

fn answer() -> Value {
    json!({"scope": "project", "answer": {"ref": "P-0123456789ab@1", "option": "o1",
           "card_hash": HASH, "evidence": "yes"}})
}

fn refused(v: &Value) -> (&'static str, String, String) {
    let e = parse_request(v).expect_err("the request is refused");
    (e.code, e.message, e.fix)
}

fn with(mut v: Value, k: &str, x: Value) -> Value {
    v.as_object_mut().unwrap().insert(k.to_string(), x);
    v
}

fn without(mut v: Value, k: &str) -> Value {
    v.as_object_mut().unwrap().remove(k);
    v
}

fn bad(msg: &str, fix: &str) -> (&'static str, String, String) {
    (E_BAD_INPUT, msg.to_string(), fix.to_string())
}

fn required(field: &str, ty: &str) -> (&'static str, String, String) {
    bad(
        &format!("rdy_rule: parameter \"{field}\" is required"),
        &format!("pass \"{field}\" as {ty}"),
    )
}

fn not_allowed(field: &str, form: &str) -> (&'static str, String, String) {
    bad(
        &format!("rdy_rule: parameter \"{field}\" is not allowed with {form}"),
        &format!("remove \"{field}\" or use the matching rdy_rule form"),
    )
}

#[test]
fn rtc1_forms_are_closed_and_bounded() {
    // the four forms parse
    assert!(matches!(parse_request(&add()), Ok(RuleRequest::Add(_))));
    assert!(matches!(parse_request(&edit()), Ok(RuleRequest::Edit(_))));
    assert!(matches!(
        parse_request(&remove()),
        Ok(RuleRequest::Remove(_))
    ));
    assert!(matches!(
        parse_request(&answer()),
        Ok(RuleRequest::Answer(_))
    ));
    // optional keys of add and edit
    let full = with(
        with(
            with(
                with(add(), "ruleset", json!("project:d-AAAAA")),
                "applies_to",
                json!(["all", "act:write"]),
            ),
            "question",
            json!("Run them?"),
        ),
        "session_ref",
        json!("s-1"),
    );
    let Ok(RuleRequest::Add(a)) = parse_request(&full) else {
        panic!("a full add parses")
    };
    assert_eq!(a.scope, Scope::Project);
    assert_eq!(a.text, "Run the tests.");
    assert_eq!(a.applies_to.as_deref().unwrap(), ["all", "act:write"]);
    assert_eq!(a.relay.question.as_deref(), Some("Run them?"));
    assert_eq!(a.relay.session_ref.as_deref(), Some("s-1"));
    assert!(matches!(a.ruleset, Some(RulesetRef::Doc(_))));
    // the submitted bytes are kept, spaces and all
    let spaced = with(add(), "text", json!("  keep\n\n  my bytes  \n"));
    let Ok(RuleRequest::Add(a)) = parse_request(&spaced) else {
        panic!()
    };
    assert_eq!(a.text, "  keep\n\n  my bytes  \n");

    // not an object
    assert_eq!(
        refused(&json!([])),
        bad(
            "rdy_rule: the parameters must be an object, not a list",
            "pass the parameters as an object"
        )
    );

    // every required field, per form
    for (form, v, fields) in [
        ("add", add(), vec!["scope", "op", "text", "why", "evidence"]),
        (
            "edit",
            edit(),
            vec!["scope", "op", "id", "text", "why", "evidence"],
        ),
        (
            "remove",
            remove(),
            vec!["scope", "op", "id", "why", "evidence"],
        ),
    ] {
        for f in fields {
            let got = refused(&without(v.clone(), f));
            let ty = if f == "applies_to" {
                "a list"
            } else {
                "a string"
            };
            assert_eq!(got, required(f, ty), "{form} without {f}");
        }
    }
    assert_eq!(refused(&json!({})), required("scope", "a string"));
    assert_eq!(
        refused(&json!({"scope": "person"})),
        required("op", "a string")
    );
    for f in ["ref", "option", "card_hash", "evidence"] {
        let mut v = answer();
        v["answer"].as_object_mut().unwrap().remove(f);
        assert_eq!(refused(&v), required(&format!("answer.{f}"), "a string"));
    }

    // every forbidden field, per form
    let extras = [
        ("id", json!("ai-AAAAA")),
        ("text", json!("t")),
        ("ruleset", json!("d-AAAAA")),
        ("applies_to", json!(["all"])),
        ("question", json!("q")),
        ("session_ref", json!("s")),
        ("answer", json!({})),
    ];
    let forbidden = |form: &str| -> Vec<&str> {
        match form {
            "add" => vec!["id", "answer"],
            "edit" => vec!["answer"],
            "remove" => vec!["text", "ruleset", "applies_to", "answer"],
            _ => vec![],
        }
    };
    for (form, v) in [("add", add()), ("edit", edit()), ("remove", remove())] {
        for f in forbidden(form) {
            let x = extras.iter().find(|e| e.0 == f).unwrap().1.clone();
            assert_eq!(
                refused(&with(v.clone(), f, x)),
                not_allowed(f, form),
                "{form} with {f}"
            );
        }
    }
    // the answer form takes no mutation key, not even question or session_ref at top level
    for f in [
        "text",
        "id",
        "ruleset",
        "applies_to",
        "why",
        "evidence",
        "question",
        "session_ref",
    ] {
        let x = if f == "applies_to" {
            json!(["all"])
        } else {
            json!("x")
        };
        assert_eq!(
            refused(&with(answer(), f, x)),
            not_allowed(f, "answer"),
            "answer with {f}"
        );
    }

    // an op next to an answer is the mutation form with a refused answer
    assert_eq!(
        refused(&with(answer(), "op", json!("remove"))),
        not_allowed("answer", "remove")
    );

    // null is invalid for every supplied key
    for f in [
        "scope",
        "op",
        "text",
        "id",
        "ruleset",
        "applies_to",
        "why",
        "evidence",
        "question",
        "session_ref",
        "answer",
    ] {
        let got = refused(&with(add(), f, Value::Null));
        assert_eq!(got.0, E_BAD_INPUT, "null {f}");
        assert!(got.1.contains(&format!("\"{f}\"")), "{f}: {got:?}");
        assert!(got.1.ends_with("not null"), "{f}: {got:?}");
    }
    for f in ["ref", "option", "card_hash", "evidence", "question"] {
        let mut v = answer();
        v["answer"][f] = Value::Null;
        let got = refused(&v);
        assert_eq!(got.0, E_BAD_INPUT, "null answer.{f}");
        assert!(got.1.contains(&format!("\"answer.{f}\"")), "{got:?}");
    }
    // a wrong type
    assert_eq!(
        refused(&with(add(), "text", json!(3))),
        bad(
            "rdy_rule: parameter \"text\" must be a string, not a number",
            "pass \"text\" as a string"
        )
    );
    assert_eq!(
        refused(&with(add(), "applies_to", json!("all"))),
        bad(
            "rdy_rule: parameter \"applies_to\" must be a list of strings, not a string",
            "pass \"applies_to\" as a list of strings"
        )
    );
    assert_eq!(
        refused(&with(add(), "applies_to", json!(["all", 4]))).1,
        "rdy_rule: parameter \"applies_to[1]\" must be a string, not a number"
    );
    assert_eq!(
        refused(&with(add(), "answer", json!("x"))).1,
        "rdy_rule: parameter \"answer\" must be an object, not a string"
    );

    // misspelt keys name the nearest key, nested ones by path
    assert_eq!(
        refused(&with(without(add(), "evidence"), "evidnce", json!("x"))),
        bad(
            "rdy_rule: unknown parameter \"evidnce\"",
            "did you mean \"evidence\"? the parameters here are scope, op, text, id, ruleset, applies_to, why, evidence, question, session_ref, answer"
        )
    );
    assert_eq!(
        refused(&with(add(), "zzzzzz", json!("x"))).1,
        "rdy_rule: unknown parameter \"zzzzzz\""
    );
    let mut v = answer();
    v["answer"]["cardhash"] = json!(HASH);
    let got = refused(&v);
    assert_eq!(got.1, "rdy_rule: unknown parameter \"answer.cardhash\"");
    assert_eq!(
        got.2,
        "did you mean \"card_hash\"? the parameters here are ref, option, card_hash, evidence, question"
    );

    // bad scope and op
    assert_eq!(
        refused(&with(add(), "scope", json!("team"))),
        bad(
            "rdy_rule: \"scope\" must be person, project",
            "pass one of: person, project"
        )
    );
    assert_eq!(
        refused(&with(add(), "op", json!("rename"))),
        bad(
            "rdy_rule: \"op\" must be add, edit, remove",
            "pass one of: add, edit, remove"
        )
    );

    // blank required strings (but not evidence)
    for f in ["text", "why"] {
        for blank in ["", "  \n\t"] {
            assert_eq!(
                refused(&with(add(), f, json!(blank))),
                bad(
                    &format!("rdy_rule: \"{f}\" must not be blank"),
                    &format!("pass nonblank text for \"{f}\"")
                )
            );
        }
    }
    assert_eq!(
        refused(&with(edit(), "id", json!(" "))).1,
        "rdy_rule: \"id\" must not be blank"
    );
    assert_eq!(
        refused(&with(add(), "question", json!(" "))).1,
        "rdy_rule: \"question\" must not be blank"
    );
    assert_eq!(
        refused(&with(add(), "session_ref", json!(""))).1,
        "rdy_rule: \"session_ref\" must not be blank"
    );
    // blank evidence is a permission refusal
    for blank in ["", "   "] {
        assert_eq!(
            refused(&with(add(), "evidence", json!(blank))),
            (
                E_PERMISSION,
                "an agent answers only by relaying a person's words".to_string(),
                "ask the person, then pass their words as evidence and your question".to_string()
            )
        );
    }
    let mut v = answer();
    v["answer"]["evidence"] = json!(" ");
    assert_eq!(refused(&v).0, E_PERMISSION);

    // bounds: text 65536 bytes
    let ok = "a".repeat(MAX_TEXT_BYTES);
    assert!(parse_request(&with(add(), "text", json!(ok))).is_ok());
    let long = "a".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(
        refused(&with(add(), "text", json!(long))),
        (
            E_LIMIT,
            "rdy_rule: \"text\" exceeds 65536 bytes".to_string(),
            "use at most 65536 bytes for \"text\"".to_string()
        )
    );
    // why: one line, 2000 characters, 8000 bytes
    assert_eq!(
        refused(&with(add(), "why", json!("a\nb"))),
        bad(
            "rdy_rule: \"why\" must be one line",
            "give the reason in one line"
        )
    );
    assert_eq!(
        refused(&with(add(), "why", json!("a\rb"))).1,
        "rdy_rule: \"why\" must be one line"
    );
    assert!(parse_request(&with(add(), "why", json!("é".repeat(2000)))).is_ok());
    assert_eq!(
        refused(&with(add(), "why", json!("é".repeat(2001)))),
        (
            E_LIMIT,
            "rdy_rule: \"why\" exceeds 2000 characters".to_string(),
            "use at most 2000 characters for \"why\"".to_string()
        )
    );
    // evidence and question: the relay's own limit and words
    assert!(
        parse_request(&with(
            add(),
            "evidence",
            json!("é".repeat(MAX_EVIDENCE_CHARS))
        ))
        .is_ok()
    );
    let too_long = bad(
        "evidence and question take at most 2000 characters each",
        "quote the person's answer, not the whole conversation",
    );
    assert_eq!(
        refused(&with(
            add(),
            "evidence",
            json!("é".repeat(MAX_EVIDENCE_CHARS + 1))
        )),
        too_long
    );
    assert_eq!(
        refused(&with(
            add(),
            "question",
            json!("q".repeat(MAX_EVIDENCE_CHARS + 1))
        )),
        too_long
    );
    // session_ref: 256 characters
    assert!(parse_request(&with(add(), "session_ref", json!("é".repeat(256)))).is_ok());
    assert_eq!(
        refused(&with(add(), "session_ref", json!("é".repeat(257)))),
        bad(
            "session_ref takes at most 256 characters",
            "use a short session id, not the conversation"
        )
    );
    // id: 96 bytes, then the block id grammar
    assert_eq!(
        refused(&with(edit(), "id", json!("a".repeat(97)))),
        (
            E_LIMIT,
            "rdy_rule: \"id\" exceeds 96 bytes".to_string(),
            "use at most 96 bytes for \"id\"".to_string()
        )
    );
    let got = refused(&with(edit(), "id", json!("not-an-id")));
    assert_eq!(got.0, E_BAD_INPUT);
    assert!(got.1.contains("bad id"), "{got:?}");
    // ruleset: 1024 bytes
    assert_eq!(
        refused(&with(add(), "ruleset", json!("a".repeat(1025)))).0,
        E_LIMIT
    );
    // applies_to: 1 to 1024 selectors of at most 256 bytes
    assert!(parse_request(&with(add(), "applies_to", json!(vec!["all"; 1024]))).is_ok());
    assert_eq!(
        refused(&with(add(), "applies_to", json!(vec!["all"; 1025]))),
        (
            E_LIMIT,
            "rdy_rule: \"applies_to\" exceeds 1024 selectors".to_string(),
            "use at most 1024 selectors for \"applies_to\"".to_string()
        )
    );
    assert_eq!(
        refused(&with(add(), "applies_to", json!([]))).0,
        E_LIMIT,
        "an empty list is outside 1 to 1024"
    );
    let long_selector = format!("area:{}", "a".repeat(252));
    assert_eq!(
        refused(&with(add(), "applies_to", json!([long_selector]))),
        (
            E_LIMIT,
            "rdy_rule: \"applies_to\" exceeds 256 bytes".to_string(),
            "use at most 256 bytes for \"applies_to\"".to_string()
        )
    );

    // the answer: ref 128 bytes, the ref grammar, option 144 bytes, card_hash 64 lowercase hex
    let with_answer = |k: &str, x: Value| {
        let mut v = answer();
        v["answer"][k] = x;
        v
    };
    assert_eq!(
        refused(&with_answer("ref", json!("P".repeat(129)))).0,
        E_LIMIT
    );
    for good in [
        "P-0123456789ab@1",
        "ai-AAAAA@0123456789ab",
        "defer:P-0123456789ab@2",
        "advice:P-0123456789ab@2",
    ] {
        assert!(
            parse_request(&with_answer("ref", json!(good))).is_ok(),
            "{good}"
        );
    }
    for no in [
        "P-0123456789ab",
        "reaffirm:ai-AAAAA@0123456789ab",
        "tn-abcdef",
        "defer:P-0123456789ab",
        "nonsense",
    ] {
        let got = refused(&with_answer("ref", json!(no)));
        assert_eq!(got.0, E_BAD_INPUT, "{no}");
        assert!(got.1.contains(no), "{no}: {got:?}");
    }
    // the verdict option is exactly 144 bytes and is accepted; one more byte is over the limit
    let verdict = format!("accept_verdict:{HASH}:{HASH}");
    assert_eq!(verdict.len(), 144);
    let Ok(RuleRequest::Answer(AnswerRule { option, .. })) =
        parse_request(&with_answer("option", json!(verdict)))
    else {
        panic!("the verdict option parses")
    };
    assert_eq!(option, verdict);
    assert_eq!(
        refused(&with_answer("option", json!(format!("{verdict}0")))),
        (
            E_LIMIT,
            "rdy_rule: \"answer.option\" exceeds 144 bytes".to_string(),
            "use at most 144 bytes for \"answer.option\"".to_string()
        )
    );
    assert_eq!(
        refused(&with_answer(
            "option",
            json!(format!("accept_verdict:{HASH}:xyz"))
        ))
        .0,
        E_BAD_INPUT
    );
    for no in [
        HASH.to_uppercase(),
        HASH[1..].to_string(),
        format!("{HASH}0"),
        String::new(),
    ] {
        assert_eq!(
            refused(&with_answer("card_hash", json!(no))),
            bad(
                "rdy_rule: \"answer.card_hash\" must be a full lowercase hash",
                "pass the card_hash of the card the person saw"
            )
        );
    }
    // answer evidence and question follow the relay limits
    assert_eq!(
        refused(&with_answer("evidence", json!("x".repeat(2001)))),
        too_long
    );
    assert_eq!(
        refused(&with_answer("question", json!("x".repeat(2001)))),
        too_long
    );
    let Ok(RuleRequest::Answer(a)) = parse_request(&with_answer("question", json!("Which?")))
    else {
        panic!()
    };
    assert_eq!(a.relay.question.as_deref(), Some("Which?"));
    assert_eq!(a.relay.evidence, "yes");
    assert_eq!(a.card_hash.to_string(), HASH);
    assert_eq!(a.scope, Scope::Project);
}

#[test]
fn rtc1_selectors_and_handles_use_existing_validation() {
    let sel = |list: Value| parse_request(&with(add(), "applies_to", list));
    // every selector form
    let all = json!([
        "all",
        "area:payments",
        "type:decision",
        "act:write",
        "act:decide",
        "act:propose",
        "act:relay",
        "act:review",
        "act:publish",
        "act:redact"
    ]);
    let Ok(RuleRequest::Add(a)) = sel(all) else {
        panic!("every form is valid")
    };
    assert_eq!(a.applies_to.unwrap().len(), 10);
    // an unknown act and the other malformed selectors give the existing issues
    for no in [
        "act:dance",
        "area:",
        "area:UPPER",
        "type:",
        "everything",
        "act:",
        "area:a b",
    ] {
        let e = sel(json!(["all", no])).expect_err(no);
        assert_eq!(e.code, E_VALIDATION, "{no}");
        assert_eq!(
            e.message,
            format!("instruction has invalid selector {no}"),
            "{no}"
        );
        assert_eq!(
            e.fix,
            "use all, area:<name>, type:<name>, or act:<write|decide|propose|relay|review|publish|redact>"
        );
        assert_eq!(e.issues.len(), 1);
        assert_eq!(e.issues[0].code, E_VALIDATION);
    }
    // every invalid selector gets its own issue, in list order
    let e = sel(json!(["act:x", "all", "area:"])).unwrap_err();
    let msgs: Vec<&str> = e.issues.iter().map(|i| i.message.as_str()).collect();
    assert_eq!(
        msgs,
        [
            "instruction has invalid selector act:x",
            "instruction has invalid selector area:"
        ]
    );
    // an edit names its instruction in the same words
    let e = parse_request(&with(edit(), "applies_to", json!(["act:x"]))).unwrap_err();
    assert_eq!(e.message, "instruction ai-AAAAA has invalid selector act:x");

    // a missing area warns, with the existing text and the pack's areas; the form stays valid
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    let Ok(RuleRequest::Add(a)) = sel(json!(["area:nowhere", "area:nowhere", "all"])) else {
        panic!()
    };
    let warns = a.selector_warnings(&pack);
    assert_eq!(warns.len(), 1);
    assert_eq!(warns[0].code, "W_INSTRUCTION_SELECTOR");
    assert_eq!(
        warns[0].message,
        "area:nowhere matches nothing: the pack has no area nowhere"
    );
    assert_eq!(
        warns[0].fix,
        "define area nowhere in the pack, or correct the selector"
    );
    assert_eq!(warns[0].block, None, "a new instruction has no id yet");
    let Ok(RuleRequest::Edit(e)) =
        parse_request(&with(edit(), "applies_to", json!(["area:nowhere"])))
    else {
        panic!()
    };
    assert_eq!(
        e.selector_warnings(&pack)[0]
            .block
            .as_ref()
            .map(|b| b.as_str()),
        Some("ai-AAAAA")
    );
    // no selectors given: nothing to warn about
    let Ok(RuleRequest::Add(a)) = parse_request(&add()) else {
        panic!()
    };
    assert_eq!(a.applies_to, None);
    assert!(a.selector_warnings(&pack).is_empty());

    // rulesets: a doc id, a path, or the matching handle of the destination
    let rs = |scope: &str, v: &str| {
        parse_request(&with(
            with(add(), "scope", json!(scope)),
            "ruleset",
            json!(v),
        ))
    };
    let doc = |r: Result<RuleRequest, _>| match r {
        Ok(RuleRequest::Add(a)) => a.ruleset.unwrap(),
        other => panic!("{other:?}"),
    };
    let id = rdy_model::BlockId::parse("d-AAAAA").unwrap();
    assert_eq!(doc(rs("project", "d-AAAAA")), RulesetRef::Doc(id.clone()));
    assert_eq!(
        doc(rs("project", "project:d-AAAAA")),
        RulesetRef::Doc(id.clone())
    );
    assert_eq!(doc(rs("person", "person:d-AAAAA")), RulesetRef::Doc(id));
    assert_eq!(
        doc(rs("person", "rules/my-rules")),
        RulesetRef::Path("rules/my-rules".into())
    );
    assert_eq!(
        doc(rs("project", "project:rules/my-rules")),
        RulesetRef::Path("rules/my-rules".into())
    );
    // the other destination's handle, a built-in handle and an organization-style handle are outside
    let outside = |v: &str| {
        (
            E_BAD_INPUT,
            format!("ruleset {v} is outside the selected scope"),
            "use a ruleset in the selected project".to_string(),
        )
    };
    for (scope, handle) in [
        ("project", "person:d-AAAAA"),
        ("person", "project:d-AAAAA"),
        ("project", "rdy:code-review"),
        ("person", "rdy:code-review"),
        ("person", "rdy:"),
    ] {
        let e = rs(scope, handle).unwrap_err();
        assert_eq!(
            (e.code, e.message, e.fix),
            outside(handle),
            "{scope} {handle}"
        );
    }
    // a handle with nothing after it is blank
    assert_eq!(
        rs("project", "project:").unwrap_err().message,
        "rdy_rule: \"ruleset\" must not be blank"
    );
    // the edit form takes the same ruleset and selectors
    assert!(parse_request(&with(edit(), "ruleset", json!("person:d-AAAAA"))).is_ok());
    let e = parse_request(&with(edit(), "ruleset", json!("project:d-AAAAA"))).unwrap_err();
    assert_eq!(
        e.message,
        "ruleset project:d-AAAAA is outside the selected scope"
    );
}

#[test]
fn rtc1_reply_shapes_and_the_blocker_list() {
    use rdy_engine::rule_types::{
        CardEntry, MAX_BLOCKERS, Outcome, ProjectInfo, Reply, RuleInfo, normal_path,
    };
    let project = ProjectInfo {
        name: "p".into(),
        repo_id: "r".into(),
    };
    let base = |outcome| Reply {
        scope: Scope::Person,
        project: project.clone(),
        rule: RuleInfo {
            id: "ai-AAAAA".into(),
            text: "t".into(),
            ruleset: "d-AAAAA".into(),
            retired: false,
        },
        decision: "D-AAAAA".into(),
        outcome,
    };
    let landed = base(Outcome::Landed {
        landing: json!({"x": 1}),
    })
    .to_json();
    assert_eq!(
        landed,
        json!({"scope": "person", "project": {"name": "p", "repo_id": "r"},
               "rule": {"id": "ai-AAAAA", "text": "t", "ruleset": "d-AAAAA", "retired": false},
               "decision": "D-AAAAA", "landing": {"x": 1}})
    );
    let entry = CardEntry {
        item_ref: "P-abcd@1".into(),
        card_hash: HASH.into(),
        card: json!({"title": "c"}),
    };
    let card = base(Outcome::Card {
        view: "v".into(),
        proposal_ref: "P-abcd@1".into(),
        item_ref: "P-abcd@1".into(),
        card_hash: HASH.into(),
        card: json!({"title": "c"}),
        cards: vec![entry.clone()],
    })
    .to_json();
    assert_eq!(card["state"], "card");
    assert_eq!(card["cards"][0]["ref"], "P-abcd@1");
    assert!(card.get("landing").is_none());
    let not_ready = base(Outcome::NotReady {
        view: "v".into(),
        item_ref: "P-abcd@1".into(),
        issues: vec![json!({"code": "E_X"})],
    })
    .to_json();
    assert_eq!(not_ready["state"], "not_ready");
    assert!(not_ready.get("card").is_none() && not_ready.get("landing").is_none());
    let answered = base(Outcome::Answered {
        view: "v".into(),
        proposal_ref: "P-abcd@1".into(),
        item_ref: "defer:P-abcd@1".into(),
        answer: json!({"ok": true}),
        remaining_cards: vec![entry],
    })
    .to_json();
    assert_eq!(answered["state"], "answered");
    assert_eq!(answered["remaining_cards"][0]["card_hash"], HASH);

    // the blocker list is ascending and bounded: a longer list is E_LIMIT, never cut
    let ids = |n: usize| {
        (0..n)
            .rev()
            .map(|i| format!("ai-{i:05}"))
            .collect::<Vec<_>>()
    };
    let ok = normal_path(Scope::Project, &project, ids(MAX_BLOCKERS)).unwrap();
    assert_eq!(ok["state"], "normal_path");
    assert_eq!(ok["blocks"][0], "ai-00000");
    assert_eq!(ok["blocks"].as_array().unwrap().len(), MAX_BLOCKERS);
    let e = normal_path(Scope::Project, &project, ids(MAX_BLOCKERS + 1)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_LIMIT,
            "rdy_rule: \"blocks\" exceeds 1024 ids",
            "use at most 1024 ids for \"blocks\""
        )
    );
}
