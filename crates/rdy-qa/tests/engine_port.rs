//! port1: version reads, scope resolution and export run on a ReadState and see only what it
//! was published with.
use rdy_engine::{Scope, VersionPosition};
use rdy_qa::fixtures::{approver, land, project, write};
use serde_json::json;

fn one(r: &mut rdy_engine::Rdy, view: &str, name: &str) {
    write(r, view, json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}])).unwrap();
}

#[test]
fn port1_a_held_states_version_position_does_not_see_a_later_tag() {
    let (_d, mut r) = project();
    one(&mut r, "work", "a");
    land(&mut r, "work").unwrap();
    let held = r.read();
    let before: VersionPosition = held.version_position(&Scope::Canon).unwrap();
    assert_eq!(before.version, None);
    assert_eq!(before.landings_since, 1);
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    assert_eq!(
        held.version_position(&Scope::Canon).unwrap(),
        before,
        "held"
    );
    assert!(held.versions().unwrap().is_empty());
    assert_eq!(held.version_position_cache_len(), 1);
    let now = r.read();
    assert_eq!(now.versions().unwrap().len(), 1);
    let pos = now.version_position(&Scope::Canon).unwrap();
    assert_eq!(
        pos.version.as_ref().map(|v| v.name.as_str()),
        Some("v1.0.0")
    );
    assert_eq!(pos.landings_since, 0);
    assert!(matches!(
        now.resolve_scope("tag:v1.0.0").unwrap(),
        Scope::At(_)
    ));
    assert_eq!(
        held.resolve_scope("tag:v1.0.0").unwrap_err().code,
        rdy_engine::E_NOT_FOUND
    );
}

#[test]
fn port1_resolve_read_scope_uses_the_states_views_and_export_reads_the_state() {
    let (_d, mut r) = project();
    one(&mut r, "work", "a");
    let held = r.read();
    one(&mut r, "side", "b");
    let e = held.resolve_read_scope("side", None).unwrap_err();
    assert_eq!(e.code, rdy_engine::E_NOT_FOUND);
    assert!(r.read().resolve_read_scope("side", None).is_ok());
    assert!(
        held.resolve_read_scope("side", Some("side")).is_ok(),
        "own view is created lazily"
    );
    let out = tempfile::tempdir().unwrap();
    let s = held
        .export(&Scope::View("work".into()), &out.path().join("x"))
        .unwrap();
    assert_eq!(s.docs, 1);
    let s2 = r
        .read()
        .export(&Scope::View("side".into()), &out.path().join("y"))
        .unwrap();
    assert_eq!(s2.docs, 1);
    assert_eq!(held.landings().unwrap().len(), 0);
    land(&mut r, "work").unwrap();
    assert_eq!(held.landings().unwrap().len(), 0, "held");
    assert_eq!(r.read().landings().unwrap().len(), 1);
    let l = r.read().landings().unwrap()[0].0;
    assert_eq!(r.read().resolve_landing(&l.to_string()[..8]).unwrap(), l);
}
