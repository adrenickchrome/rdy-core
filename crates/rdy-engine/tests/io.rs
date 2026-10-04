use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::collections::BTreeMap;

fn agent() -> Session { Session { actor: "agent:a".into(), run: None, role: Role::Writer } }
fn founder() -> Session { Session { actor: "human:founder".into(), run: None, role: Role::Approver } }

fn rich_project(dir: &std::path::Path) -> Rdy {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(&[0u8, 159, 146, 150, 255]).unwrap().to_string();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Ünïcødé — design", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Components"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Line one.\nLine two with \"quotes\" and: colons\n\ttabbed\n"},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "Flow"}, "body": "graph TD\n  A[{{$t}}] --> B{{Decide?}}\n"},
        {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": "yaml"}, "body": "image: {{ .Values.image }}\n"},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Write \\{{literal}} for braces."},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "application/octet-stream", "alt": "bytes"}},
        {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": ["a", "b"], "rows": [[1, true], ["x", null]], "normative": false}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    let t = res.created["$t"].clone();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Uses {{{{{t}}}}}."), "links": {"describes": [{"target": t, "part": "the balance part"}]}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    r.decide(&agent(), "work", DecisionInput { parent: s, statement: "Ledger is append-only".into(), options: vec!["append-only".into(), "mutable".into()], ..Default::default() }).unwrap();
    let p = r.propose(&agent(), "work", "more").unwrap();
    r.land(&founder(), &p.id).unwrap();
    r
}

#[test]
fn export_import_round_trip_preserves_every_version_hash() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut a = rich_project(a_dir.path());
    let summary = a.export(&Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.docs, 1);
    assert_eq!(summary.blobs, 1);
    let original: BTreeMap<BlockId, Hash> = a.materialized(&Scope::Canon).unwrap().snapshot.blocks.clone();
    assert_eq!(summary.blocks, original.len());

    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    assert_eq!(b.import(&agent(), "import", out.path()).unwrap_err().code, "E_PERMISSION");
    b.import(&founder(), "import", out.path()).unwrap();
    let p = b.propose(&founder(), "import", "migrate").unwrap();
    b.land(&founder(), &p.id).unwrap();
    let imported = b.materialized(&Scope::Canon).unwrap().snapshot.blocks.clone();
    assert_eq!(imported, original);

    // the blob came across too
    let fig = b.materialized(&Scope::Canon).unwrap().blocks.values().find(|x| x.block_type == "figure").unwrap().clone();
    let h: Hash = fig.field_str("blob").unwrap().parse().unwrap();
    assert_eq!(b.store().get_blob(&h).unwrap(), vec![0u8, 159, 146, 150, 255]);

    // importing twice collides
    assert_eq!(b.import(&founder(), "again", out.path()).unwrap_err().code, "E_ALREADY_EXISTS");
}

#[test]
fn single_block_yaml_round_trip_and_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let term = r.materialized(&Scope::Canon).unwrap().blocks.values().find(|x| x.block_type == "term").unwrap().clone();
    let text = block_to_yaml(&term);
    assert_eq!(block_from_yaml(&text).unwrap(), term);

    let mut edited = term.clone();
    edited.body = "Edited definition.".into();
    r.edit_block(&founder(), "work", edited.clone()).unwrap();
    assert_eq!(r.block(&Scope::View("work".into()), &term.id).unwrap().body, "Edited definition.");

    let mut moved = term.clone();
    moved.order = "zz".into();
    assert_eq!(r.edit_block(&founder(), "work", moved).unwrap_err().code, "E_BAD_INPUT");
    assert_eq!(block_from_yaml("id: [").unwrap_err().code, "E_BAD_INPUT");
}

// ---- import failure paths ----

use std::path::Path;

fn exported(dir: &Path, out: &Path) -> Rdy {
    let mut a = rich_project(dir);
    a.export(&Scope::Canon, out).unwrap();
    a
}

fn doc_file(out: &Path) -> std::path::PathBuf {
    std::fs::read_dir(out.join("docs")).unwrap().next().unwrap().unwrap().path()
}

fn mutate_docs(out: &Path, f: impl FnOnce(&mut Vec<serde_yaml_ng::Value>)) {
    let p = doc_file(out);
    let mut list: Vec<serde_yaml_ng::Value> = serde_yaml_ng::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    f(&mut list);
    std::fs::write(&p, serde_yaml_ng::to_string(&list).unwrap()).unwrap();
}

fn find<'a>(list: &'a mut [serde_yaml_ng::Value], ty: &str) -> &'a mut serde_yaml_ng::Value {
    list.iter_mut().find(|v| v["type"].as_str() == Some(ty)).unwrap()
}

fn id_of(list: &[serde_yaml_ng::Value], ty: &str) -> String {
    list.iter().find(|v| v["type"].as_str() == Some(ty)).unwrap()["id"].as_str().unwrap().to_string()
}

fn assert_import_fails(out: &Path, code: &str) {
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let before = b.store().view_state("import").unwrap().blocks.clone();
    let err = b.import(&founder(), "import", out).unwrap_err();
    assert_eq!(err.code, code, "{err}");
    b.invalidate();
    assert_eq!(b.store().view_state("import").unwrap().blocks, before, "failed import must not change the view");
    assert!(b.materialized(&Scope::View("import".into())).unwrap().blocks.is_empty());
}

fn setup() -> (tempfile::TempDir, tempfile::TempDir, Rdy) {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let a = exported(d.path(), out.path());
    (d, out, a)
}

#[test]
fn import_rejects_corrupt_blob() {
    let (_d, out, _a) = setup();
    let f = std::fs::read_dir(out.path().join("blobs")).unwrap().next().unwrap().unwrap().path();
    std::fs::write(&f, b"tampered").unwrap();
    assert_import_fails(out.path(), "E_BAD_INPUT");
}

#[test]
fn import_rejects_unknown_reference() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| find(l, "diagram")["body"] = "see {{t-ZZZZZ}}".into());
    assert_import_fails(out.path(), "E_UNKNOWN_REF");
}

#[test]
fn import_rejects_unknown_link_target() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| find(l, "claim")["links"]["describes"][0]["target"] = "t-ZZZZZ".into());
    assert_import_fails(out.path(), "E_UNKNOWN_REF");
}

#[test]
fn import_rejects_duplicate_id() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        let last = l.last().unwrap().clone();
        l.push(last);
    });
    assert_import_fails(out.path(), "E_ALREADY_EXISTS");
}

#[test]
fn import_rejects_dangling_parent() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| find(l, "section")["parent"] = "d-ZZZZZ".into());
    assert_import_fails(out.path(), "E_UNKNOWN_BLOCK");
}

#[test]
fn import_rejects_parent_cycle() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        let s = id_of(l, "section");
        find(l, "doc")["parent"] = s.into();
    });
    assert_import_fails(out.path(), "E_BAD_INPUT");
}

#[test]
fn import_rejects_self_parent() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        let s = id_of(l, "section");
        find(l, "section")["parent"] = s.into();
    });
    assert_import_fails(out.path(), "E_BAD_INPUT");
}

#[test]
fn import_rejects_invalid_block() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        find(l, "term")["fields"].as_mapping_mut().unwrap().remove("name");
    });
    assert_import_fails(out.path(), "E_VALIDATION");
}

#[test]
fn import_rejects_unsupported_or_missing_manifest() {
    let (_d, out, _a) = setup();
    std::fs::write(out.path().join("rdy-export.yaml"), "format: 2\npack: rdy-standard\nscope: canon\n").unwrap();
    assert_import_fails(out.path(), "E_BAD_INPUT");
    std::fs::remove_file(out.path().join("rdy-export.yaml")).unwrap();
    assert_import_fails(out.path(), "E_BAD_INPUT");
}

#[test]
fn edit_block_applies_field_unset_and_links_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let claim = r.materialized(&Scope::Canon).unwrap().blocks.values().find(|x| x.block_type == "claim").unwrap().clone();
    let mut e = claim.clone();
    e.links.clear();
    r.edit_block(&founder(), "work", e).unwrap();
    assert!(r.block(&Scope::View("work".into()), &claim.id).unwrap().links.is_empty());

    let sec = r.materialized(&Scope::Canon).unwrap().blocks.values().find(|x| x.block_type == "section").unwrap().clone();
    let mut e = sec.clone();
    e.fields.insert("title".into(), FieldValue::Str("Renamed".into()));
    e.fields.insert("lifecycle".into(), FieldValue::Str("stable".into()));
    r.edit_block(&founder(), "work", e).unwrap_or_else(|x| panic!("{x}"));
    let got = r.block(&Scope::View("work".into()), &sec.id).unwrap();
    assert_eq!(got.field_str("title"), Some("Renamed"));
    let mut e = got.clone();
    e.fields.remove("lifecycle");
    r.edit_block(&founder(), "work", e).unwrap();
    assert!(!r.block(&Scope::View("work".into()), &sec.id).unwrap().fields.contains_key("lifecycle"));
}

#[test]
fn edit_block_respects_decision_rules() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let dec = r.materialized(&Scope::Canon).unwrap().blocks.values().find(|x| x.block_type == "decision").unwrap().clone();
    assert_eq!(dec.field_str("status"), Some("accepted"));
    let mut e = dec.clone();
    e.body = "changed".into();
    assert_eq!(r.edit_block(&founder(), "work", e).unwrap_err().code, "E_FROZEN");

    let res = r.decide(&agent(), "work", DecisionInput { parent: dec.parent.clone().unwrap().to_string(), statement: "Another".into(), options: vec!["x".into(), "y".into()], ..Default::default() }).unwrap();
    let id = BlockId::parse(res.created.values().next().unwrap()).unwrap();
    let mut e = r.block(&Scope::View("work".into()), &id).unwrap();
    e.fields.insert("status".into(), FieldValue::Str("accepted".into()));
    assert_eq!(r.edit_block(&agent(), "work", e).unwrap_err().code, "E_PERMISSION");
}

#[test]
fn export_refuses_a_non_empty_directory() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut a = rich_project(a_dir.path());
    std::fs::create_dir_all(out.path().join("docs")).unwrap();
    std::fs::write(out.path().join("docs/d-STALE.yaml"), "[]").unwrap();
    let e = a.export(&Scope::Canon, out.path()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.fix.contains("empty directory"), "{}", e.fix);
    assert_eq!(std::fs::read_dir(out.path().join("docs")).unwrap().count(), 1, "nothing written");
    // a directory that does not exist yet is fine
    a.export(&Scope::Canon, &out.path().join("fresh")).unwrap();
}

#[test]
fn import_skips_dotfiles_and_non_yaml_files() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut a = rich_project(a_dir.path());
    a.export(&Scope::Canon, out.path()).unwrap();
    std::fs::write(out.path().join("docs/.DS_Store"), [0u8, 1, 2]).unwrap();
    std::fs::write(out.path().join("docs/README.txt"), "notes").unwrap();
    std::fs::write(out.path().join("blobs/.DS_Store"), [0u8, 1, 2]).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "import", out.path()).unwrap();
    let n = a.materialized(&Scope::Canon).unwrap().blocks.len();
    assert_eq!(b.materialized(&Scope::View("import".into())).unwrap().blocks.len(), n);
}

#[test]
fn edit_block_refuses_a_no_op_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let term = r.materialized(&Scope::View("work".into())).unwrap().blocks.values().find(|x| x.block_type == "term").unwrap().clone();
    let e = r.edit_block(&founder(), "work", term).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("nothing changed"), "{}", e.message);
}

#[test]
fn export_of_a_figure_whose_blob_is_missing_fails_and_writes_nothing() {
    let a_dir = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let mut a = Rdy::init(a_dir.path(), "rdy-standard").unwrap();
    let missing = rdy_store::Store::blob_hash(b"never stored").to_string();
    let res = a.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "D", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "S"}},
        {"op": "create", "type": "figure", "id": "$f", "parent": "$s", "fields": {"blob": missing, "media_type": "application/octet-stream", "alt": "gone"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let out = parent.path().join("exp");
    let e = a.export(&Scope::View("work".into()), &out).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    assert!(e.message.contains(&res.created["$f"]), "names the figure: {e}");
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0, "nothing written, no staging dir left behind");
}

#[test]
fn export_creates_missing_parents_and_clears_a_leftover_staging_dir() {
    let a_dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut a = rich_project(a_dir.path());
    let deeper = root.path().join("nested/deeper");
    // an interrupted earlier export left its staging dir behind
    std::fs::create_dir_all(deeper.join(".exp.rdy-export-partial/docs")).unwrap();
    std::fs::write(deeper.join(".exp.rdy-export-partial/docs/d-STALE.yaml"), "[]").unwrap();
    a.export(&Scope::Canon, &deeper.join("exp")).unwrap();
    assert!(!deeper.join("exp/docs/d-STALE.yaml").exists());
    assert!(deeper.join("exp/rdy-export.yaml").exists());
    let names: Vec<_> = std::fs::read_dir(&deeper).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("exp")]);
}
