//! qclean1: rdy fsck lists quarantine files; --clean-quarantine removes them, for the human only.
use rdy_qa::daemon::Served;
use serde_json::json;

#[test]
fn qclean1_fsck_lists_quarantine_and_only_the_human_cleans_it() {
    let s = Served::new();
    let store = s.dir.path().join(".rdy/store");
    std::fs::create_dir_all(store.join("segments")).unwrap();
    std::fs::write(store.join("segments/seg-0.log.torn-77"), b"torn").unwrap();
    std::fs::write(store.join("index.redb.damaged-1"), b"old index").unwrap();
    let (ok, v) = s.json(&["--json", "fsck"]);
    assert!(ok, "{v}");
    assert_eq!(
        v["quarantine"],
        json!(["index.redb.damaged-1", "segments/seg-0.log.torn-77"])
    );
    assert!(
        v.get("removed").is_none() && v.get("skipped").is_none(),
        "{v}"
    );
    let (ok, v) = s.json_as(
        "agent:x",
        Some("r"),
        &["--json", "fsck", "--clean-quarantine"],
    );
    assert!(!ok);
    assert_eq!(v["code"], "E_PERMISSION", "{v}");
    assert!(
        store.join("index.redb.damaged-1").exists(),
        "an agent's request removes nothing"
    );
    let o = s.rdy(&["fsck", "--clean-quarantine"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("ok: "), "{out}");
    assert_eq!(
        &lines[1..],
        &[
            "removed index.redb.damaged-1",
            "removed segments/seg-0.log.torn-77"
        ],
        "{out}"
    );
    let (ok, v) = s.json(&["--json", "fsck", "--clean-quarantine"]);
    assert!(ok, "{v}");
    assert_eq!(
        (
            v["removed"].clone(),
            v["skipped"].clone(),
            v["quarantine"].clone()
        ),
        (json!([]), json!([]), json!([]))
    );
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok && v["quarantine"] == json!([]), "{v}");
    let (ok, v) = s.json(&["--json", "log"]);
    assert!(ok, "the store still reads: {v}");
}
