//! rfr2 (D-3DAX4 rule 5, D-6PR3Y): a figure's dark image is checked as its light image is, before
//! anything is written: the same checks and the same texts, with the dark field's name. A refused
//! write acknowledges nothing.
// the shared fixture is also a module of other render and engine tests, which keep it private
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::*;
use rdy_model::redaction::RedactTarget;
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::*;
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};

fn section_project() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
    ]))
    .unwrap();
    let s = res.created["$s"].clone();
    (d, r, s)
}

fn figure(parent: &str, blob: &str, dark: Option<&str>) -> Value {
    let mut fields = json!({"blob": blob, "media_type": "image/png", "alt": "chart"});
    if let Some(d) = dark {
        fields["dark_blob"] = json!(d);
        fields["dark_media_type"] = json!("image/png");
    }
    json!([{"op": "create", "type": "figure", "parent": parent, "fields": fields}])
}

/// `e`'s message with the block id that opens it written as `<id>`.
fn unnamed(e: &RdyError) -> String {
    match e.message.split_once("'s ") {
        Some((id, rest)) if BlockId::parse(id).is_ok() => format!("<id>'s {rest}"),
        _ => e.message.clone(),
    }
}

/// What a refused write must leave as it was: the log's end and the view's blocks.
fn state(r: &Rdy) -> (rdy_store::segment::Location, usize) {
    let m = r.read().materialized(&Scope::View("work".into())).unwrap();
    (r.store().log_end(), m.len())
}

#[test]
fn rfr2_both_blobs_are_validated_before_write() {
    let (_d, mut r, s) = section_project();
    let section = BlockId::parse(&s).unwrap();
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
    let dark = r
        .store_mut()
        .put_blob(b"\x89PNG dark chart")
        .unwrap()
        .to_string();
    // missing, malformed and wrong-kind objects, and a hash in a form a blob is not named by
    let bad = [
        "not-a-hash".to_string(),
        rdy_store::Store::blob_hash(b"never stored").to_string(),
        block_version,
        real.to_uppercase(),
        format!(" {real}"),
    ];
    for v in &bad {
        let light = write(&mut r, "work", figure(&s, v, None)).unwrap_err();
        let before = state(&r);
        let dark_e = write(&mut r, "work", figure(&s, &real, Some(v))).unwrap_err();
        assert_eq!(
            state(&r),
            before,
            "{v}: the refused write acknowledges nothing"
        );
        assert_eq!(dark_e.code, light.code, "{v}");
        assert_eq!(dark_e.code, "E_VALIDATION", "{v}");
        assert_eq!(
            unnamed(&dark_e),
            unnamed(&light).replacen("'s blob ", "'s dark_blob ", 1),
            "{v}"
        );
        assert_eq!(
            dark_e.fix,
            light.fix.replace("set blob ", "set dark_blob "),
            "{v}"
        );
        assert_eq!(
            dark_e.fix,
            "upload the file with rdy_asset and set dark_blob to the hash it returns"
        );
        // the light field's own texts are as before
        assert!(light.message.contains("'s blob "), "{v}: {light}");
        assert_eq!(
            light.fix,
            "upload the file with rdy_asset and set blob to the hash it returns"
        );
    }
    // a value that is not a string: the same refusal as for the light field
    let light = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "figure", "parent": s,
        "fields": {"blob": 7, "media_type": "image/png", "alt": "c"}}]),
    )
    .unwrap_err();
    let e = write(&mut r, "work", json!([{"op": "create", "type": "figure", "parent": s,
        "fields": {"blob": real, "media_type": "image/png", "alt": "c", "dark_blob": 7, "dark_media_type": "image/png"}}]))
    .unwrap_err();
    assert_eq!(e.code, light.code, "{e}");
    assert_eq!(
        unnamed(&e),
        unnamed(&light).replacen("'s blob ", "'s dark_blob ", 1)
    );

    // one field of the pair alone: the D-6PR3Y refusal
    let before = state(&r);
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "figure", "parent": s,
        "fields": {"blob": real, "media_type": "image/png", "alt": "c", "dark_blob": dark}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(
        e.message
            .ends_with("'s dark_blob and dark_media_type are set together"),
        "{e}"
    );
    assert_eq!(e.fix, "set both fields, or neither");
    assert_eq!(state(&r), before);

    // an update is checked as a create is
    let made = write(&mut r, "work", figure(&s, &real, Some(&dark))).unwrap();
    let fig = made.created.values().next().unwrap().clone();
    for v in &bad {
        let before = state(&r);
        let e = write(
            &mut r,
            "work",
            json!([{"op": "update", "id": fig, "fields": {"dark_blob": v}}]),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_VALIDATION", "{v}: {e}");
        assert!(e.message.contains("'s dark_blob "), "{v}: {e}");
        assert_eq!(state(&r), before, "{v}");
    }
    // clearing both dark fields leaves a light-only figure
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": fig, "unset": ["dark_blob", "dark_media_type"]}]),
    )
    .unwrap();
    let id = BlockId::parse(&fig).unwrap();
    let b = r.read().block(&Scope::View("work".into()), &id).unwrap();
    assert!(!b.fields.contains_key("dark_blob"), "{:?}", b.fields);
}

/// A removed blob is a blob of the project: the dark field takes it as the light field does
/// (the build shows the removal notice), and the outcome is the same for both fields.
#[test]
fn rfr2_a_removed_blob_is_taken_by_both_fields_alike() {
    let (dir, mut r, s) = section_project();
    let real = r.store_mut().put_blob(b"\x89PNG kept").unwrap();
    let gone = r.store_mut().put_blob(b"\x89PNG removed").unwrap();
    r.close().unwrap();
    let f = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new("redact", vec![RedactTarget::Blob(gone)])],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let mut r = Rdy::open(dir.path()).unwrap();
    assert_eq!(
        r.read().reader().blob_state(&gone).unwrap(),
        BlobState::Removed(f.requests[0].request)
    );
    let outcome = |r: &mut Rdy, light: &Hash, dark: Option<&Hash>| {
        let d = dark.map(|h| h.to_string());
        write(r, "work", figure(&s, &light.to_string(), d.as_deref()))
            .map(|_| ())
            .map_err(|e| e.code)
    };
    let as_light = outcome(&mut r, &gone, None);
    let as_dark = outcome(&mut r, &real, Some(&gone));
    assert_eq!(as_dark, as_light);
}
