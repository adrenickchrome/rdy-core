//! tree1 (spec §15.1, intake "full tree 9.77 ms"): the full tree is memoised per state and
//! refreshed by publication, never stale.
use rdy_engine::Scope;
use rdy_qa::fixtures::{land, project, write};
use serde_json::json;
use std::sync::Arc;

#[test]
fn tree1_the_full_tree_is_memoised_within_a_state_and_fresh_after_a_write() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "f"}},
        {"op": "create", "type": "doc", "parent": "$f", "fields": {"name": "a", "title": "A", "doc_type": "page"}}
    ])).unwrap();
    let work = Scope::View("work".into());
    let s = r.read();
    let t1 = s.tree_full(&work).unwrap();
    let t2 = s.tree_full(&work).unwrap();
    assert!(Arc::ptr_eq(&t1, &t2), "memoised");
    assert_eq!(
        s.tree(&work, None, None).unwrap(),
        *t1,
        "tree(None, None) is the memo's content"
    );
    assert_eq!(t1.len(), 1);
    assert_eq!(t1[0].children.len(), 1);
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "parent": t1[0].id.clone(), "fields": {"name": "b", "title": "B", "doc_type": "page"}}])).unwrap();
    let s2 = r.read();
    let t3 = s2.tree_full(&work).unwrap();
    assert!(!Arc::ptr_eq(&t1, &t3));
    assert_eq!(t3[0].children.len(), 2);
    assert_eq!(
        s.tree_full(&work).unwrap()[0].children.len(),
        1,
        "the old state keeps its tree"
    );
    land(&mut r, "work").unwrap();
    let s3 = r.read();
    assert_eq!(s3.tree_full(&Scope::Canon).unwrap()[0].children.len(), 2);
    assert_eq!(
        s3.tree(&Scope::Canon, Some("f"), Some(0)).unwrap()[0].collapsed,
        2,
        "a path or depth is computed, not memoised"
    );
}

#[test]
fn tree1_resolve_path_and_path_of_round_trip_on_the_state() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "Top"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "leaf", "title": "L", "doc_type": "page"}}
    ])).unwrap();
    let s = r.read();
    let work = Scope::View("work".into());
    let id = s.resolve_path(&work, "top/LEAF").unwrap();
    assert_eq!(s.path_of(&work, &id).unwrap().as_deref(), Some("Top/leaf"));
    assert_eq!(s.resolve_id(&work, &id.to_string()).unwrap(), id);
    let e = s.resolve_path(&work, "top/nope").unwrap_err();
    assert_eq!(e.code, rdy_engine::E_UNKNOWN_BLOCK);
    assert!(e.fix.contains("leaf"), "{e}");
    let ctx = s.context(&work, &id).unwrap();
    assert_eq!(ctx.block_type, "doc");
    assert!(s.impact(&work, &id, 3).unwrap().is_empty());
    assert!(s.suspects(&work).unwrap().is_empty());
}
