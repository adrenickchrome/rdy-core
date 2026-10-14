//! ls8: a proposal version as a read scope: its base plus its changes, in review mode.
use rdy_engine::{Rdy, Scope};
use rdy_qa::fixtures::writer;
use serde_json::json;

fn proposed() -> (tempfile::TempDir, Rdy, String) {
    let _clock = rdy_qa::golden::fixture_clock();
    let (d, mut r) = rdy_qa::golden::tiny_with_doc();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Proposed doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    let p = r.propose(&writer(), "w", "Add a doc").unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    (d, r, pref)
}

fn titles(r: &Rdy, scope: &Scope) -> Vec<String> {
    let m = r.read().materialized(scope).unwrap();
    let mut t: Vec<String> = m
        .iter()
        .filter_map(|b| b.field_str("title").map(str::to_string))
        .collect();
    t.sort();
    t
}

#[test]
fn ls8_a_version_reads_its_base_plus_its_changes() {
    let (_d, mut r, pref) = proposed();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Later doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    assert!(matches!(scope, Scope::Proposal(_)));
    let t = titles(&r, &scope);
    assert!(t.contains(&"Proposed doc".to_string()), "{t:?}");
    assert!(
        !t.contains(&"Later doc".to_string()),
        "a version never moves: {t:?}"
    );
    assert!(titles(&r, &Scope::View("w".into())).contains(&"Later doc".to_string()));
}

#[test]
fn ls8_the_latest_ref_follows_new_versions() {
    let (_d, mut r, pref) = proposed();
    let stable = pref.split('@').next().unwrap().to_string();
    let v1 = r
        .read()
        .resolve_scope(&format!("proposal:{stable}"))
        .unwrap();
    rdy_qa::golden::write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "fields": {"title": "Second version doc", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    r.propose(&writer(), "w", "Add two docs").unwrap();
    let v2 = r
        .read()
        .resolve_scope(&format!("proposal:{stable}"))
        .unwrap();
    assert_ne!(v1, v2);
    assert_eq!(
        r.read().resolve_scope(&format!("proposal:{pref}")).unwrap(),
        v1,
        "@1 stays"
    );
    // a ref is P- and 4 to 64 hex digits (gate.rs:410-418); a non-hex ref is E_BAD_INPUT
    let e = r
        .read()
        .resolve_scope("proposal:P-ffffffffffff@9")
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
}

#[test]
fn ls8_the_position_names_the_proposal() {
    let (_d, r, pref) = proposed();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let p = r.read().version_position(&scope).unwrap();
    assert!(!p.frozen);
    let v = p.view.expect("a proposal reads as a draft");
    assert_eq!(v.name, "Add a doc");
    assert_eq!(v.unlanded_changes, 1);
}

#[test]
fn ls8_coverage_refuses_a_proposal_version() {
    let (_d, r, pref) = proposed();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let e = r.read().coverage_scope(&scope).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message
            .starts_with("coverage is computed for canon and views, not for proposal:"),
        "{e}"
    );
}

#[test]
fn ls8_a_cursor_reads_back() {
    let (_d, r, pref) = proposed();
    let scope = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let Scope::Proposal(v) = &scope else {
        panic!("{scope:?}")
    };
    assert_eq!(
        r.read().resolve_scope(&format!("proposal:{v}")).unwrap(),
        scope,
        "a hash resolves too"
    );
}
