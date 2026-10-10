//! dhome1: a configured decision home (FW-PR2): a decision given no parent is recorded there; no
//! home is a clear E_BAD_INPUT; a home that names nothing, or not a doc, is E_CONFIG.
use rdy_engine::{DecisionInput, E_BAD_INPUT, E_CONFIG, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

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
    let parent = r.decision_parent("work", None).unwrap();
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
    assert_eq!(r.decision_parent("work", None).unwrap(), reg);
    assert_eq!(
        r.decision_parent("work", Some("s-AAAAA")).unwrap(),
        "s-AAAAA"
    );
}

#[test]
fn dhome1_no_home_is_a_clear_error() {
    let (_d, r) = project();
    assert_eq!(r.read().decisions_home(), None);
    let e = r.decision_parent("work", None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "no parent given, and the project names no decision home"
    );
    assert_eq!(
        e.fix,
        "pass parent (the doc or section to record the decision in), or set decisions.home in .rdy.yaml to a doc's path, key or id"
    );
}

#[test]
fn dhome1_a_home_that_is_a_folder_or_missing_is_e_config() {
    let (_d, mut r) = project();
    home(&mut r);
    configure(&mut r, "decisions");
    let e = r.decision_parent("work", None).unwrap_err();
    assert_eq!(e.code, E_CONFIG);
    assert_eq!(
        e.message,
        "decisions.home \"decisions\" is a folder, not a doc"
    );
    let (_d2, mut r2) = project();
    configure(&mut r2, "nowhere/at-all");
    let e = r2.decision_parent("work", None).unwrap_err();
    assert_eq!(e.code, E_CONFIG);
    assert!(
        e.message
            .starts_with("decisions.home \"nowhere/at-all\" names no block in view work: "),
        "{e}"
    );
    assert_eq!(
        e.fix,
        "set decisions.home in .rdy.yaml to an existing doc's path, key or id"
    );
}
