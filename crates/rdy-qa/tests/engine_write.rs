//! The write path's checks: asset blobs (blob2), decision permissions (dec1), fix hints (hint1).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn section_project() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
    ]))
    .unwrap();
    let s = res.created["$s"].clone();
    (d, r, s)
}

fn figure(parent: &str, blob: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "figure", "parent": parent,
            "fields": {"blob": blob, "media_type": "image/png", "alt": "chart"}}])
}

/// Includes Review Focus 2: a real hash in uppercase or with a leading space is refused too.
#[test]
fn blob2_a_figure_must_name_a_blob_in_the_store() {
    let (_d, mut r, s) = section_project();
    let section = rdy_model::BlockId::parse(&s).unwrap();
    let block_version = r
        .read()
        .block(&Scope::View("work".into()), &section)
        .unwrap()
        .version_hash()
        .to_string();
    let real = r
        .store_mut()
        .put_blob(b"\x89PNG chart")
        .unwrap()
        .to_string();
    let bad = [
        "not-a-hash".to_string(),
        rdy_store::Store::blob_hash(b"never stored").to_string(),
        block_version,
        real.to_uppercase(),
        format!(" {real}"),
    ];
    for blob in &bad {
        let e = write(&mut r, "work", figure(&s, blob)).unwrap_err();
        assert_eq!(e.code, "E_VALIDATION", "{blob}: {e}");
        assert!(e.fix.contains("rdy_asset"), "{blob}: {e}");
    }
    write(&mut r, "work", figure(&s, &real)).unwrap();
}

const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: T\n    doc_type: glossary\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Terms\n";

/// A format-2 export holding one doc with one figure naming `blob`.
fn export_with_figure(blob: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    let fig = format!(
        "- id: fg-AAAAA\n  type: figure\n  parent: s-AAAAA\n  order: a1\n  fields:\n    blob: \"{blob}\"\n    media_type: image/png\n    alt: chart\n"
    );
    std::fs::write(t.path().join("docs/t.yaml"), format!("{DOC}{fig}")).unwrap();
    t
}

#[test]
fn blob2_import_refuses_a_figure_whose_blob_is_not_a_blob() {
    for blob in [
        "not-a-hash".to_string(),
        rdy_store::Store::blob_hash(b"never stored").to_string(),
    ] {
        let t = export_with_figure(&blob);
        let (_d, mut r) = project();
        let e = r.import(&approver(), "import", t.path()).unwrap_err();
        assert_eq!(e.code, "E_UNKNOWN_REF", "{blob}: {e}");
    }
    // control: the same export carrying its blob imports
    let bytes = b"\x89PNG chart";
    let h = rdy_store::Store::blob_hash(bytes).to_string();
    let t = export_with_figure(&h);
    std::fs::create_dir_all(t.path().join("blobs")).unwrap();
    std::fs::write(t.path().join("blobs").join(&h), bytes).unwrap();
    let (_d, mut r) = project();
    r.import(&approver(), "import", t.path()).unwrap();
}

#[test]
fn dec1_a_writer_cannot_set_decided_at() {
    let (_d, mut r, s) = section_project();
    let e = write(&mut r, "work", json!([{"op": "create", "type": "decision", "parent": s,
        "fields": {"statement": "Wallets stay", "status": "proposed", "decided_at": "2026-10-04T00:00:00Z"}}]))
    .unwrap_err();
    assert_eq!(e.code, "E_PERMISSION", "{e}");
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Wallets stay".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let did = d.created["$decision"].clone();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": did, "fields": {"decided_at": "2026-10-04T00:00:00Z"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PERMISSION", "{e}");
    // other edits of a proposed decision still work, and landing sets decided_at
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": did, "fields": {"statement": "Wallets stay put"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = rdy_model::BlockId::parse(&did).unwrap();
    assert!(
        r.read()
            .block(&Scope::Canon, &id)
            .unwrap()
            .field_str("decided_at")
            .is_some()
    );
}

#[test]
fn hint1_fix_hints_name_only_tools_that_exist() {
    let e = RdyError::from(rdy_store::StoreError::UnknownBlock(
        rdy_model::BlockId::parse("c-AAAAA").unwrap(),
    ));
    assert!(
        !e.fix.contains("rdy_search") && e.fix.contains("rdy_read"),
        "{e}"
    );
    let (_d, mut r, s) = section_project();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "See {{nope}}."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF", "{e}");
    assert!(
        !e.fix.contains("rdy_search") && e.fix.contains("rdy_read"),
        "{e}"
    );
}
