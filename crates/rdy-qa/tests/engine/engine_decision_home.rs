//! dhome1, ndh2: a configured decision home (FW-PR2, D-P3YD5): a decision given no parent is
//! recorded there; with no home, a decision with no parent sits at the project root; a home that
//! names nothing, or neither a doc nor a folder, is E_CONFIG.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::{DecisionInput, E_CONFIG, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

/// Both fixes of a home that names no block, or neither a doc nor a folder (D-P3YD5).
const HOME_FIX: &str =
    "set decisions.home in .rdy.yaml to the path, key or id of a doc or a folder that exists";

/// Folder `decisions` holding doc `register` (key REG-1), landed. Returns (folder, register).
fn home(r: &mut Rdy) -> (String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "decisions", "title": "Decisions"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "register", "title": "Register", "doc_type": "overview", "key": "REG-1"}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    (w.created["$f"].clone(), w.created["$d"].clone())
}

/// Appends `decisions: {home: <home>}` to the project config and reloads it.
fn configure(r: &mut Rdy, home: &str) {
    let cfg = r.root().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap() + &format!("decisions:\n  home: {home}\n");
    std::fs::write(&cfg, text).unwrap();
    r.reload_pack().unwrap();
}

#[test]
fn dhome1_a_decision_without_a_parent_goes_to_the_home() {
    let (_d, mut r) = project();
    let (_f, reg) = home(&mut r);
    configure(&mut r, "decisions/register");
    assert_eq!(r.read().decisions_home(), Some("decisions/register"));
    let parent = r.decision_parent("work", None).unwrap().unwrap();
    assert_eq!(parent, reg);
    let w = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent,
                statement: "Keys come from paths".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let id = BlockId::parse(&w.created["$decision"]).unwrap();
    let b = r.read().block(&Scope::View("work".into()), &id).unwrap();
    assert_eq!(b.parent.map(|p| p.to_string()), Some(reg));
}

#[test]
fn dhome1_the_home_may_be_a_key_and_a_given_parent_wins() {
    let (_d, mut r) = project();
    let (_f, reg) = home(&mut r);
    configure(&mut r, "REG-1");
    assert_eq!(r.decision_parent("work", None).unwrap(), Some(reg));
    assert_eq!(
        r.decision_parent("work", Some("s-AAAAA")).unwrap(),
        Some("s-AAAAA".to_string())
    );
}

#[test]
fn dhome1_a_home_that_is_a_folder_or_missing_is_e_config() {
    let (_d2, mut r2) = project();
    configure(&mut r2, "nowhere/at-all");
    let e = r2.decision_parent("work", None).unwrap_err();
    assert_eq!(e.code, E_CONFIG);
    assert!(
        e.message
            .starts_with("decisions.home \"nowhere/at-all\" names no block in view work: "),
        "{e}"
    );
    assert_eq!(e.fix, HOME_FIX);
}

/// The parent of block `id` in view `work`, as text.
fn parent_of(r: &Rdy, id: &str) -> Option<String> {
    let id = BlockId::parse(id).unwrap();
    r.read()
        .block(&Scope::View("work".into()), &id)
        .unwrap()
        .parent
        .map(|p| p.to_string())
}

/// `decide` in view `work` with `parent` as given; returns the new decision's id.
fn decide(r: &mut Rdy, parent: &str, statement: &str) -> String {
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: parent.into(),
            statement: statement.into(),
            ..Default::default()
        },
    )
    .unwrap()
    .created["$decision"]
        .clone()
}

fn finding_op() -> serde_json::Value {
    json!({"op": "create", "type": "finding", "id": "$f",
        "fields": {"statement": "The release changed retention.", "evidence": [{"quote": "Retention is now seven days."}]}})
}

fn source_op() -> serde_json::Value {
    json!({"op": "create", "type": "source", "id": "$s",
        "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}})
}

#[test]
fn ndh2_no_home_puts_a_decision_at_the_project_root() {
    let (_d, mut r) = project();
    assert_eq!(r.read().decisions_home(), None);
    assert_eq!(r.decision_parent("work", None).unwrap(), None);
    let id = decide(&mut r, "", "Keys come from paths");
    assert_eq!(parent_of(&r, &id), None);
    let scope = Scope::View("work".into());
    let ctx = r
        .read()
        .context(&scope, &BlockId::parse(&id).unwrap())
        .unwrap();
    let ctx = serde_json::to_value(&ctx).unwrap();
    assert_eq!(ctx["doc"], json!(null), "{ctx}");
    assert_eq!(ctx["parent"], json!(null), "{ctx}");
}

#[test]
fn ndh2_a_folder_home_takes_decisions_findings_and_sources() {
    let (_d, mut r) = project();
    let (folder, _reg) = home(&mut r);
    configure(&mut r, "decisions");
    assert_eq!(
        r.decision_parent("work", None).unwrap(),
        Some(folder.clone())
    );
    let parent = r.decision_parent("work", None).unwrap().unwrap_or_default();
    let id = decide(&mut r, &parent, "Keys come from paths");
    assert_eq!(parent_of(&r, &id), Some(folder.clone()));
    for (op, ty) in [(finding_op(), "finding"), (source_op(), "source")] {
        let w = write(&mut r, "work", json!([op])).unwrap();
        let id = w.created.values().next().unwrap().clone();
        assert_eq!(parent_of(&r, &id), Some(folder.clone()), "{ty}");
    }
}

#[test]
fn ndh2_a_home_of_another_type_is_e_config() {
    let (_d, mut r) = project();
    let (_f, reg) = home(&mut r);
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "section", "id": "$s", "parent": reg, "fields": {"title": "Log"}}]),
    )
    .unwrap();
    let sec = w.created["$s"].clone();
    configure(&mut r, &sec);
    let e = r.decision_parent("work", None).unwrap_err();
    assert_eq!(e.code, E_CONFIG);
    assert_eq!(
        e.message,
        format!("decisions.home \"{sec}\" is a section, not a doc or a folder")
    );
    assert_eq!(e.fix, HOME_FIX);
    let (_d2, mut r2) = project();
    configure(&mut r2, "nowhere/at-all");
    let e = r2.decision_parent("work", None).unwrap_err();
    assert_eq!(e.code, E_CONFIG);
    assert!(
        e.message
            .starts_with("decisions.home \"nowhere/at-all\" names no block in view work: "),
        "{e}"
    );
    assert_eq!(e.fix, HOME_FIX);
}

#[test]
fn ndh2_a_blank_parent_counts_as_none() {
    let (_d, mut r) = project();
    assert_eq!(r.decision_parent("work", Some("")).unwrap(), None);
    assert_eq!(r.decision_parent("work", Some("  ")).unwrap(), None);
    let (_f, reg) = home(&mut r);
    configure(&mut r, "decisions/register");
    assert_eq!(
        r.decision_parent("work", Some("")).unwrap(),
        Some(reg.clone())
    );
    assert_eq!(r.decision_parent("work", Some("  ")).unwrap(), Some(reg));
}

/// `op` with `"parent": parent`.
fn with_parent(mut op: serde_json::Value, parent: &str) -> serde_json::Value {
    op["parent"] = json!(parent);
    op
}

fn decision_op() -> serde_json::Value {
    json!({"op": "create", "type": "decision", "id": "$d",
        "fields": {"statement": "Retention follows the release.", "status": "proposed"}})
}

#[test]
fn ndh2_rdy_write_a_blank_parent_counts_as_none() {
    // the write path takes a blank parent as the CLI and rdy_decide do (D-X3136, FW-UX1)
    let (_d, mut r) = project();
    let (_e, mut h) = project();
    let (_f, reg) = home(&mut h);
    configure(&mut h, "decisions/register");
    for blank in ["", "  "] {
        for (op, ty) in [
            (finding_op(), "finding"),
            (source_op(), "source"),
            (decision_op(), "decision"),
        ] {
            // no home: the project root
            let w = write(&mut r, "work", json!([with_parent(op.clone(), blank)]))
                .unwrap_or_else(|e| panic!("{ty} {blank:?}: {e}"));
            let id = w.created.values().next().unwrap().clone();
            assert_eq!(parent_of(&r, &id), None, "{ty} {blank:?}");
            // a home: the home
            let w = write(&mut h, "work", json!([with_parent(op, blank)]))
                .unwrap_or_else(|e| panic!("{ty} {blank:?}: {e}"));
            let id = w.created.values().next().unwrap().clone();
            assert_eq!(parent_of(&h, &id), Some(reg.clone()), "{ty} {blank:?}");
        }
    }
}

/// A landed open question at the project root; returns its card ref (`<id>@<version>`).
fn root_question(r: &mut Rdy) -> (String, String) {
    let w = write(
        r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q",
            "fields": {"prompt": "Who approves refunds?", "options": ["Finance", "Support lead"]}}]),
    )
    .unwrap();
    land(r, "qs").unwrap();
    let q = w.created["$q"].clone();
    let v = version_of(r, &Scope::Canon, &q);
    let item = format!("{q}@{}", &v[..12]);
    (q, item)
}

/// Answers `item` with its first option at the command line (view `work`); returns the parent of
/// the decision it records.
fn answered_parent(r: &mut Rdy, item: &str) -> Option<String> {
    r.answer(
        &writer(),
        rdy_qa::intake::PERSON,
        AnswerRequest {
            r#ref: item.into(),
            option: "o1".into(),
            ..AnswerRequest::default()
        },
    )
    .unwrap();
    let (_, a) = r.store().answer_of(item).unwrap().unwrap();
    let change = r.store().reader().change(&a.targets[0]).unwrap();
    let id = change.ops[0].block_id().to_string();
    let b = r
        .read()
        .block(&Scope::View("work".into()), &BlockId::parse(&id).unwrap())
        .unwrap();
    assert_eq!(b.block_type, "decision");
    b.parent.map(|p| p.to_string())
}

#[test]
fn ndh2_rdy_answer_puts_the_decision_where_the_question_sits() {
    // no home: a question at the root gives a decision at the root
    let (_d, mut r) = project();
    let (q, item) = root_question(&mut r);
    assert_eq!(parent_of_canon(&r, &q), None);
    assert_eq!(answered_parent(&mut r, &item), None);
    // a home: the decision goes to the home
    let (_d2, mut r2) = project();
    let (_f, reg) = home(&mut r2);
    configure(&mut r2, "decisions/register");
    let (_q, item) = root_question(&mut r2);
    assert_eq!(answered_parent(&mut r2, &item), Some(reg));
}

fn parent_of_canon(r: &Rdy, id: &str) -> Option<String> {
    r.read()
        .block(&Scope::Canon, &BlockId::parse(id).unwrap())
        .unwrap()
        .parent
        .map(|p| p.to_string())
}
