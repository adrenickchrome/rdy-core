//! rtc2 (FW-MH.rules, D-ZSRBN): the request receipt of a rule change. The request's view name is
//! the digest of its identity and exact words; the view is reused only when its owner and its
//! receipt match. The rest of the lifecycle needs landings: `gate_rule_lifecycle` in rdy-qa.
use rdy_engine::rule_types::{RuleRequest, parse_request};
use rdy_engine::rules::{Destination, MAX_RECEIPT_BYTES, Prepared, RuleCaller, RuleMutation};
use rdy_engine::{E_LIMIT, E_STALE_BASE, Rdy, RdyError, Session, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn agent(actor: &str, run: &str) -> Session {
    Session::agent(actor.into(), Some(run.into()), Role::Writer).unwrap()
}

fn request(v: Value) -> RuleRequest {
    parse_request(&v).unwrap()
}

fn caller(s: &Session, destination: Destination) -> RuleCaller<'_> {
    RuleCaller {
        session: s,
        person: "human:qa",
        surface: "mcp",
        destination,
    }
}

fn prepare(r: &mut Rdy, s: &Session, d: Destination, v: Value) -> Result<Prepared, RdyError> {
    let req = request(v);
    r.rule_prepare(&caller(s, d), RuleMutation::of(&req).unwrap())
}

fn view_name(r: &Rdy, s: &Session, d: Destination, v: Value) -> String {
    let req = request(v);
    r.rule_view_name(&caller(s, d), RuleMutation::of(&req).unwrap())
        .unwrap()
}

fn add(text: &str, evidence: &str) -> Value {
    json!({"scope": "project", "op": "add", "text": text, "why": "They asked.",
           "evidence": evidence, "question": "Save this rule?", "session_ref": "chat-1"})
}

#[test]
fn rtc2_receipt_binds_identity_and_exact_words() {
    let (_d, mut r) = project();
    let a = agent("agent:a", "r1");
    let d = Destination::Project;

    // the view is named by the full digest, and the same request names the same view
    let first = prepare(&mut r, &a, d, add("Run the tests.", "yes")).unwrap();
    let digest = first.view.strip_prefix("rule-").expect("the rule- name");
    assert_eq!(digest.len(), 64, "{}", first.view);
    assert!(
        digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(
        view_name(&r, &a, d, add("Run the tests.", "yes")),
        first.view
    );

    // the same request: the same view and proposal, and nothing written
    let names = r.read().view_names();
    let head = r.read().view_head(&first.view).unwrap().head;
    let again = prepare(&mut r, &a, d, add("Run the tests.", "yes")).unwrap();
    assert_eq!(again.state, first.state);
    assert_eq!(again.rule, first.rule);
    assert_eq!(again.decision, first.decision);
    assert_eq!(r.read().view_names(), names);
    assert_eq!(r.read().view_head(&first.view).unwrap().head, head);

    // the receipt is the change's message, versioned, and holds no originating project
    let st = r.read();
    let change = st.included(&first.view)[0];
    let message = st.reader().change(&change).unwrap().message;
    drop(st);
    let receipt: Value = serde_json::from_str(&message).unwrap();
    assert_eq!(receipt["rdy_rule_receipt"], json!(1));
    assert_eq!(receipt["actor"], json!("agent:a"));
    assert_eq!(receipt["run"], json!("r1"));
    assert_eq!(receipt["session_ref"], json!("chat-1"));
    assert_eq!(receipt["op"], json!("add"));
    assert_eq!(receipt["text"], json!("Run the tests."));
    assert_eq!(receipt["evidence"], json!("yes"));
    assert_eq!(receipt["question"], json!("Save this rule?"));
    assert_eq!(receipt["ruleset"], json!(""));
    assert_eq!(receipt["applies_to"], Value::Null);
    assert_eq!(
        receipt["repo_id"],
        json!(r.read().repo_id().unwrap().to_string())
    );

    // every changed word is a new request
    for changed in [
        add("Run all the tests.", "yes"),
        add("Run the tests.", "yes, please"),
        json!({"scope": "project", "op": "add", "text": "Run the tests.", "why": "Changed.",
               "evidence": "yes", "question": "Save this rule?", "session_ref": "chat-1"}),
        json!({"scope": "project", "op": "add", "text": "Run the tests.", "why": "They asked.",
               "evidence": "yes", "question": "Another?", "session_ref": "chat-1"}),
        json!({"scope": "project", "op": "add", "text": "Run the tests.", "why": "They asked.",
               "evidence": "yes", "question": "Save this rule?", "session_ref": "chat-2"}),
        json!({"scope": "project", "op": "add", "text": "Run the tests.", "why": "They asked.",
               "evidence": "yes", "question": "Save this rule?", "session_ref": "chat-1",
               "applies_to": ["all"]}),
        json!({"scope": "project", "op": "add", "text": "Run the tests.", "why": "They asked.",
               "evidence": "yes", "question": "Save this rule?", "session_ref": "chat-1",
               "ruleset": "my-set"}),
    ] {
        assert_ne!(
            view_name(&r, &a, d, changed.clone()),
            first.view,
            "{changed}"
        );
    }
    // a length prefix keeps two fields from trading bytes
    let split = |text: &str, why: &str| {
        view_name(
            &r,
            &a,
            d,
            json!({"scope": "project", "op": "add", "text": text, "why": why, "evidence": "yes"}),
        )
    };
    assert_ne!(split("ab", "c"), split("a", "bc"));

    // the same words from another run or another actor are a new request, and both stay
    let other_run = agent("agent:a", "r2");
    let b = prepare(&mut r, &other_run, d, add("Run the tests.", "yes")).unwrap();
    assert_ne!(b.view, first.view);
    assert!(r.read().view_head(&first.view).is_some());
    assert!(r.read().view_head(&b.view).is_some());
    let other_actor = agent("agent:b", "r1");
    assert_ne!(
        view_name(&r, &other_actor, d, add("Run the tests.", "yes")),
        first.view
    );

    // an allowed layer prefix is the same input as no prefix
    assert_eq!(
        view_name(
            &r,
            &a,
            d,
            json!({"scope": "project", "op": "add", "text": "x", "why": "y", "evidence": "z",
                   "ruleset": "project:my-set"})
        ),
        view_name(
            &r,
            &a,
            d,
            json!({"scope": "project", "op": "add", "text": "x", "why": "y", "evidence": "z",
                   "ruleset": "my-set"})
        ),
    );
    // an absent session reference reads the surface's default
    assert_eq!(
        view_name(
            &r,
            &a,
            d,
            json!({"scope": "project", "op": "add", "text": "x", "why": "y", "evidence": "z"})
        ),
        view_name(
            &r,
            &a,
            d,
            json!({"scope": "project", "op": "add", "text": "x", "why": "y", "evidence": "z",
                   "session_ref": "mcp"})
        ),
    );
}

#[test]
fn rtc2_receipt_scope_spellings_share_one_meta_request() {
    let (_d, mut r) = project();
    let a = agent("agent:a", "r1");
    let d = Destination::Meta;
    let spelled = |scope: &str| json!({"scope": scope, "op": "add", "text": "Be brief.", "why": "Asked.", "evidence": "yes"});
    let person = prepare(&mut r, &a, d, spelled("person")).unwrap();
    let names = r.read().view_names();
    let project = prepare(&mut r, &a, d, spelled("project")).unwrap();
    assert_eq!(project.view, person.view);
    assert_eq!(project.state, person.state);
    assert_eq!(project.rule, person.rule);
    assert_eq!(r.read().view_names(), names);
}

#[test]
fn rtc2_receipt_refuses_a_hostile_view_of_the_same_name() {
    let (_d, mut r) = project();
    let a = agent("agent:a", "r1");
    let d = Destination::Project;
    let name = view_name(&r, &a, d, add("Run the tests.", "yes"));
    let unrelated = |text: &str| {
        WriteRequest {
        view: name.clone(),
        ops: serde_json::from_value(json!([
            {"op": "create", "type": "doc", "fields": {"name": text, "title": text, "doc_type": "page"}}
        ]))
        .unwrap(),
        justification: None,
        message: "unrelated".into(),
    }
    };
    let moved = |e: RdyError| {
        assert_eq!(e.code, E_STALE_BASE, "{e}");
        assert_eq!(
            e.message,
            format!("rule view {name} changed outside this request")
        );
        assert_eq!(
            e.fix,
            "read the view and continue through the normal workflow"
        );
    };

    // another owner holds the name: the view is not this request's, and is not touched
    let b = agent("agent:b", "r1");
    r.write(&b, unrelated("other")).unwrap();
    let head = r.read().view_head(&name).unwrap().head;
    moved(prepare(&mut r, &a, d, add("Run the tests.", "yes")).unwrap_err());
    assert_eq!(r.read().view_head(&name).unwrap().head, head);

    // the same owner with unrelated contents: the same refusal; the name alone is no authority
    let (_d2, mut r2) = project();
    let name2 = view_name(&r2, &a, d, add("Run the tests.", "yes"));
    let req = WriteRequest {
        view: name2.clone(),
        ops: serde_json::from_value(json!([
            {"op": "create", "type": "doc", "fields": {"name": "n", "title": "n", "doc_type": "page"}}
        ]))
        .unwrap(),
        justification: None,
        message: "unrelated".into(),
    };
    r2.write(&a, req).unwrap();
    let e = prepare(&mut r2, &a, d, add("Run the tests.", "yes")).unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
    assert_eq!(
        e.message,
        format!("rule view {name2} changed outside this request")
    );

    // a receipt's words with extra content in the view: the same refusal
    let (_d3, mut r3) = project();
    let first = prepare(&mut r3, &a, d, add("Run the tests.", "yes")).unwrap();
    let extra = WriteRequest {
        view: first.view.clone(),
        ops: serde_json::from_value(json!([
            {"op": "create", "type": "doc", "fields": {"name": "x", "title": "x", "doc_type": "page"}}
        ]))
        .unwrap(),
        justification: None,
        message: "more".into(),
    };
    r3.write(&a, extra).unwrap();
    let e = prepare(&mut r3, &a, d, add("Run the tests.", "yes")).unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
}

#[test]
fn rtc2_receipt_size_is_measured_before_any_write() {
    let (_d, mut r) = project();
    let a = agent("agent:a", "r1");
    // parsed input stays well under the bound, so this request comes straight from the type:
    // every byte is a control character, which JSON writes in six
    let mut req =
        request(json!({"scope": "project", "op": "add", "text": "x", "why": "y", "evidence": "z"}));
    let RuleRequest::Add(rule) = &mut req else {
        panic!("an add");
    };
    rule.text = "\u{1}".repeat(MAX_RECEIPT_BYTES / 6 + 1);
    let names = r.read().view_names();
    let e = r
        .rule_prepare(
            &caller(&a, Destination::Project),
            RuleMutation::of(&req).unwrap(),
        )
        .unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e}");
    assert_eq!(
        e.message,
        format!("rdy_rule: \"receipt\" exceeds {MAX_RECEIPT_BYTES} bytes")
    );
    assert_eq!(
        e.fix,
        format!("use at most {MAX_RECEIPT_BYTES} bytes for \"receipt\"")
    );
    assert_eq!(r.read().view_names(), names);
}
