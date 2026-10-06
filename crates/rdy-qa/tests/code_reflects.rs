//! refl1: a block declares the code it reflects; a bad glob is refused; export and import keep it.
use rdy_engine::*;
use rdy_model::FieldValue;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn refl1_a_written_reflects_list_lands_and_survives_export_and_import() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.c.to_string(),
            "fields": {"reflects": ["crates/*/src/error.rs"]}}]),
    )
    .unwrap();
    land(&mut r, "w").unwrap();
    let want = FieldValue::List(vec![FieldValue::Str("crates/*/src/error.rs".into())]);
    let m = r.read().materialized(&Scope::Canon).unwrap();
    assert_eq!(m.get(&s.c).unwrap().fields.get("reflects"), Some(&want));
    let out = d.path().join("export");
    r.read().export(&Scope::Canon, &out).unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let mut r2 = Rdy::init(fresh.path(), "rdy-standard").unwrap();
    r2.import(&approver(), "work", &out).unwrap();
    let m2 = r2.read().materialized(&Scope::View("work".into())).unwrap();
    let c2 = m2
        .iter()
        .find(|b| b.body == "C stands alone.")
        .expect("C imported");
    assert_eq!(c2.fields.get("reflects"), Some(&want));
}

#[test]
fn refl1_a_bad_glob_is_refused_with_e_bad_glob() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let e = write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.c.to_string(),
            "fields": {"reflects": ["src//x.rs"]}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert!(e.issues.iter().any(|i| i.code == "E_BAD_GLOB"), "{e:?}");
}
