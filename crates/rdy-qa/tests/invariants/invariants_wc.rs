//! sg2: spec §15.3 invariant 7, a working copy equals the export of its view. Only people edit
//! the working copy (FW-V2), so each test checks a checkout nobody has touched, or one it edited
//! by hand on purpose.
use rdy_engine::Rdy;
use rdy_qa::wc::*;
use rdy_qa::wc_check::matches_export;
use serde_json::json;

#[test]
fn sg2_a_checkout_equals_the_export_of_its_view() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout", "work"]);
    if let Err(v) = matches_export(&s, "work", s.dir.path()) {
        panic!("the checkout differs from the export: {v:?}");
    }
}

#[test]
fn sg2_a_checkout_with_a_figure_matches_blobs() {
    let (s, seed) = seeded();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let h = r.store_mut().put_blob(b"\x89PNG fake").unwrap().to_string();
    r.close().unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "figure", "parent": seed.section, "fields": {"blob": h, "media_type": "image/png", "alt": "A"}}]),
    );
    ok(&s, &["checkout", "work"]);
    let held = std::fs::read_dir(at(&s, "_blobs")).unwrap().count();
    assert_eq!(held, 1, "_blobs holds the figure's file");
    if let Err(v) = matches_export(&s, "work", s.dir.path()) {
        panic!("the checkout differs from the export: {v:?}");
    }
}

#[test]
fn sg2_an_edited_file_is_a_wc_tree_violation() {
    let (s, seed) = seeded();
    ok(&s, &["checkout", "work"]);
    set_body(&s, "notes.yaml", &seed.note, "Edited by hand.");
    let v = matches_export(&s, "work", s.dir.path()).expect_err("the edit is a violation");
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, "wc_tree", "{v:?}");
    assert!(v[0].detail.starts_with("notes.yaml"), "{v:?}");
}

#[test]
fn sg2_a_missing_blob_is_a_wc_blobs_violation() {
    let (s, seed) = seeded();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let h = r.store_mut().put_blob(b"\x89PNG fake").unwrap().to_string();
    r.close().unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "figure", "parent": seed.section, "fields": {"blob": h, "media_type": "image/png", "alt": "A"}}]),
    );
    ok(&s, &["checkout", "work"]);
    std::fs::remove_file(at(&s, &format!("_blobs/{h}.png"))).unwrap();
    let v = matches_export(&s, "work", s.dir.path()).expect_err("the blob is gone");
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].rule, "wc_blobs", "{v:?}");
    assert_eq!(v[0].detail, format!("blob {h}: only in the export"));
}

#[test]
fn sg2_checkout_then_commit_with_no_edits_is_a_no_op() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout", "work"]);
    let log_len = |s: &rdy_qa::daemon::Served| {
        let (okk, v) = json(s, &["log"]);
        assert!(okk, "{v}");
        serde_json::to_string(&v).unwrap().len()
    };
    let shown = |s: &rdy_qa::daemon::Served| {
        let (okk, v) = json(s, &["view", "show", "work"]);
        assert!(okk, "{v}");
        v["changes"].as_array().map(|c| c.len())
    };
    let (log_before, shown_before) = (log_len(&s), shown(&s));
    assert!(shown_before.is_some(), "view show lists the changes");
    ok(&s, &["add", "."]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["message"] == "nothing staged", "{v}");
    assert_eq!(log_len(&s), log_before, "the log did not move");
    assert_eq!(shown(&s), shown_before, "the view holds the same changes");
    if let Err(v) = matches_export(&s, "work", s.dir.path()) {
        panic!("the checkout differs from the export: {v:?}");
    }
}
