//! fsck over a busy project and a reindex of it (plan M2a1, fsck2).
use rdy_engine::Rdy;
use rdy_qa::fixtures::*;
use serde_json::json;

fn doc(title: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "glossary"}}])
}

#[test]
fn fsck2_a_busy_project_checks_clean_before_and_after_reindex() {
    let (_d, mut r): (tempfile::TempDir, Rdy) = project();
    write(&mut r, "work", doc("One")).unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, None).unwrap();
    write(&mut r, "side", doc("Two")).unwrap();
    write(&mut r, "work", doc("Pending")).unwrap();
    let f = r.fsck().unwrap();
    assert_eq!(f.views, 2);
    assert!(f.landed_changes >= 1);
    let before = r.store().index_dump_for_tests().unwrap();
    r.reindex().unwrap();
    assert_eq!(r.store().index_dump_for_tests().unwrap(), before);
    let again = r.fsck().unwrap();
    assert_eq!(
        (again.views, again.landed_changes),
        (f.views, f.landed_changes)
    );
}
