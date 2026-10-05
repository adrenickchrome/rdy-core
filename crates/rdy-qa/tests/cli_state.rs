//! port1 at the human boundary: status, ls, render and reindex read a ReadState; reindex
//! republishes it.
use rdy_qa::fixtures::{land, project, rdy_bin, write};
use serde_json::{Value, json};
use std::process::Command;

fn rdy(dir: &std::path::Path, args: &[&str]) -> (bool, Value) {
    let out = Command::new(rdy_bin())
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let text = if out.status.success() {
        &out.stdout
    } else {
        &out.stderr
    };
    (
        out.status.success(),
        serde_json::from_slice(text).unwrap_or(Value::Null),
    )
}

#[test]
fn port1_status_reports_the_states_views_landings_and_seq() {
    let (dir, mut r) = project();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}])).unwrap();
    land(&mut r, "work").unwrap();
    write(&mut r, "side", json!([{"op": "create", "type": "doc", "fields": {"name": "b", "title": "B", "doc_type": "glossary"}}])).unwrap();
    drop(r);
    let (ok, v) = rdy(dir.path(), &["status"]);
    assert!(ok, "{v}");
    assert_eq!(v["landings"], 1);
    assert!(v["seq"]["offset"].as_u64().unwrap() > 0, "{v}");
    let views = v["views"].as_array().unwrap();
    assert!(
        views
            .iter()
            .any(|w| w["name"] == "side" && w["changes"] == 1),
        "{v}"
    );
    let (ok, ls) = rdy(dir.path(), &["ls", "--scope", "canon"]);
    assert!(ok, "{ls}");
    assert!(ls.to_string().contains("\"a\""), "{ls}");
    assert!(!ls.to_string().contains("\"b\""), "canon has no b: {ls}");
}

#[test]
fn port1_reindex_republishes_the_state() {
    let (dir, mut r) = project();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "glossary"}}])).unwrap();
    let old = r.read();
    r.reindex().unwrap();
    let new = r.read();
    assert!(!std::sync::Arc::ptr_eq(&old, &new));
    assert_eq!(
        old.materialized(&rdy_engine::Scope::View("work".into()))
            .unwrap()
            .len(),
        1,
        "the old state still reads"
    );
    assert_eq!(
        new.materialized(&rdy_engine::Scope::View("work".into()))
            .unwrap()
            .len(),
        1
    );
    drop((old, new));
    drop(r);
    let (ok, v) = rdy(dir.path(), &["reindex"]);
    assert!(ok, "{v}");
    let (ok, _) = rdy(dir.path(), &["fsck"]);
    assert!(ok);
}

#[test]
fn port1_render_reads_the_state_it_is_given() {
    let (dir, mut r) = project();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "Alpha", "doc_type": "glossary"}}])).unwrap();
    let held = r.read();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"name": "b", "title": "Beta", "doc_type": "glossary"}}])).unwrap();
    let out = dir.path().join("site-held");
    let s = rdy_render::render_site(&held, &rdy_engine::Scope::View("work".into()), &out).unwrap();
    let out2 = dir.path().join("site-now");
    let s2 =
        rdy_render::render_site(&r.read(), &rdy_engine::Scope::View("work".into()), &out2).unwrap();
    assert!(
        s2.pages > s.pages,
        "the held state rendered the older view: {} vs {}",
        s.pages,
        s2.pages
    );
}
