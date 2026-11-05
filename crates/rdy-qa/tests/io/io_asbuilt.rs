//! ab1 (D-WJ0SJ): the as-built fields change no stored format. A released dump loads as it was,
//! with no `built` on its decisions and no default added by the load. Decisions and content
//! blocks written afterwards carry the new fields through a dump and a load, with the same block
//! versions and the same tracking behavior. The released fixtures and their digests are never
//! touched.
use rdy_engine::load::load;
use rdy_engine::*;
use rdy_model::{BlockId, EditorialClass, Justification};
use rdy_qa::fixtures::*;
use serde_json::{from_value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name)
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Every block of canon with its version: the stored state a load must reproduce.
fn versions(r: &Rdy) -> BTreeMap<String, String> {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    m.ids()
        .map(|b| (b.to_string(), version_of(r, &Scope::Canon, b.as_str())))
        .collect()
}

fn dump_and_load(r: &Rdy) -> (tempfile::TempDir, Rdy) {
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.rdydump");
    r.read().dump(&file).unwrap();
    let dir = out.path().join("loaded");
    load(&file, &dir).unwrap();
    let loaded = Rdy::open(&dir).unwrap();
    (out, loaded)
}

fn edit_built(
    r: &mut Rdy,
    view: &str,
    dec: &str,
    built: &str,
    reason: Option<&str>,
) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: from_value(json!([{"op": "update", "id": dec, "fields": {"built": built}}]))
                .unwrap(),
            justification: reason.map(|w| Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: w.into(),
            }),
            message: String::new(),
        },
    )
}

#[test]
fn ab1_absent_old_fields_and_new_fields_survive_dump_load() {
    // a released fixture loads with no built anywhere: the load adds no default
    let to = tempfile::tempdir().unwrap();
    let dir = to.path().join("p");
    load(&fixture("v4-f1-e1.rdydump"), &dir).unwrap();
    let mut r = Rdy::open(&dir).unwrap();
    r.fsck().unwrap();
    let old = r.read().query(&Scope::Canon, "type = decision").unwrap();
    assert!(!old.matches.is_empty(), "the fixture holds decisions");
    for m in &old.matches {
        let b = r.read().block(&Scope::Canon, &id(&m.id)).unwrap();
        assert!(
            !b.fields.contains_key("built"),
            "{} has built after the load",
            m.id
        );
    }
    let old_versions = versions(&r);
    r.reindex().unwrap();
    assert_eq!(versions(&r), old_versions, "a reindex adds nothing");

    // new fields come in through the tools: a decision with its default, a note with overrides
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let made = write(&mut r, "seed", json!([
        {"op": "create", "type": "decision", "id": "$d", "fields": {"statement": "Ship the check", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$p", "fields": {"statement": "Meet weekly", "status": "proposed", "needs_content": false}},
        {"op": "create", "type": "note", "id": "$n", "parent": section, "body": "Planned text.", "fields": {"planned": true, "since": "0.36.0"}}
    ])).unwrap();
    land(&mut r, "seed").unwrap();
    let (shipped, process, note) = (
        made.created["$d"].clone(),
        made.created["$p"].clone(),
        made.created["$n"].clone(),
    );
    let field = |r: &Rdy, b: &str, f: &str| {
        r.read()
            .block(&Scope::Canon, &id(b))
            .unwrap()
            .field_str(f)
            .map(str::to_string)
    };
    assert_eq!(field(&r, &shipped, "built").as_deref(), Some("planned"));
    assert_eq!(field(&r, &process, "built").as_deref(), Some("none"));
    assert_eq!(field(&r, &note, "since").as_deref(), Some("0.36.0"));

    // a dump and a load reproduce every block version, the old ones with no built
    let before = versions(&r);
    let (_keep, mut loaded) = dump_and_load(&r);
    loaded.fsck().unwrap();
    assert_eq!(versions(&loaded), before, "the same stored block versions");
    for m in &old.matches {
        assert_eq!(field(&loaded, &m.id, "built"), None, "{}", m.id);
    }
    assert_eq!(
        field(&loaded, &shipped, "built").as_deref(),
        Some("planned")
    );
    assert_eq!(field(&loaded, &process, "built").as_deref(), Some("none"));
    assert_eq!(field(&loaded, &note, "since").as_deref(), Some("0.36.0"));

    // tracking works the same after the load, on a new decision and on an old one that had no
    // built: both need a reason, and neither moves its ruling
    let e = edit_built(&mut loaded, "reg", &shipped, "0.38.0", None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("editorial reason"), "{}", e.message);
    edit_built(
        &mut loaded,
        "reg",
        &shipped,
        "0.38.0",
        Some("shipped in 0.38.0"),
    )
    .unwrap();
    land(&mut loaded, "reg").unwrap();
    assert_eq!(field(&loaded, &shipped, "built").as_deref(), Some("0.38.0"));
    assert_eq!(
        field(&loaded, &shipped, "status").as_deref(),
        Some("accepted")
    );
    if let Some(m) = old.matches.first() {
        let e = edit_built(&mut loaded, "old", &m.id, "0.38.0", None).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{}", m.id);
        edit_built(
            &mut loaded,
            "old",
            &m.id,
            "0.38.0",
            Some("shipped in 0.38.0"),
        )
        .unwrap();
        let in_view = loaded
            .read()
            .block(&Scope::View("old".into()), &id(&m.id))
            .unwrap();
        assert_eq!(in_view.field_str("built"), Some("0.38.0"));
        assert_eq!(
            field(&loaded, &m.id, "built"),
            None,
            "canon keeps it absent"
        );
    }

    // and a second dump and load of the edited state keeps them
    let edited = versions(&loaded);
    let (_keep2, again) = dump_and_load(&loaded);
    assert_eq!(versions(&again), edited);
    assert_eq!(field(&again, &shipped, "built").as_deref(), Some("0.38.0"));
}
