//! sg2: spec §15.3 invariant 7, a working copy equals the export of its view. Only people edit
//! the working copy (FW-V2), so each test checks a checkout nobody has touched, or one it edited
//! by hand on purpose.
use rdy_engine::Rdy;
use rdy_qa::wc::*;
use rdy_qa::wc_check::{
    STORE_ONLY_FIELDS, STORE_ONLY_LINK_KINDS, STORE_ONLY_TARGET_TYPES, SUPPORTING_TYPES,
    matches_export,
};
use rdy_schema::{PackSet, TypeClass};
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
    // the whole log, compared as a value (final review M9, minors item 8: a length could hide
    // a changed entry of the same size)
    let log = |s: &rdy_qa::daemon::Served| {
        let (okk, v) = json(s, &["log"]);
        assert!(okk, "{v}");
        v
    };
    let shown = |s: &rdy_qa::daemon::Served| {
        let (okk, v) = json(s, &["view", "show", "work"]);
        assert!(okk, "{v}");
        v["changes"].as_array().map(|c| c.len())
    };
    let (log_before, shown_before) = (log(&s), shown(&s));
    assert!(shown_before.is_some(), "view show lists the changes");
    ok(&s, &["add", "."]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["message"] == "nothing staged", "{v}");
    assert_eq!(log(&s), log_before, "the log did not move");
    assert_eq!(shown(&s), shown_before, "the view holds the same changes");
    if let Err(v) = matches_export(&s, "work", s.dir.path()) {
        panic!("the checkout differs from the export: {v:?}");
    }
}

/// The oracle holds its own closed lists, from the spec's metadata split (working-copy page,
/// "The metadata split") and not from the product, so a field or kind the product wrongly marks
/// store-only fails here instead of vanishing on both sides of the comparison.
#[test]
fn sg2_the_oracles_store_only_lists_equal_the_products() {
    assert_eq!(
        STORE_ONLY_FIELDS,
        rdy_schema::STORE_ONLY_FIELDS,
        "store-only fields"
    );
    assert_eq!(
        STORE_ONLY_LINK_KINDS,
        rdy_schema::STORE_ONLY_LINK_KINDS,
        "store-only link kinds"
    );
    assert_eq!(
        STORE_ONLY_TARGET_TYPES,
        rdy_schema::STORE_ONLY_TARGET_TYPES,
        "link target types that are store-only"
    );
    let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
    let mut supporting: Vec<&str> = pack
        .block_types
        .keys()
        .filter(|t| pack.class_of(t) == TypeClass::Supporting)
        .map(String::as_str)
        .collect();
    supporting.sort_unstable();
    let mut want = SUPPORTING_TYPES.to_vec();
    want.sort_unstable();
    assert_eq!(supporting, want, "supporting block types");
}
