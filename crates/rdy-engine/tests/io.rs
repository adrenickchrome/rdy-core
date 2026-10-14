use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::collections::BTreeMap;

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}
fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    }
}

/// Briefs `id` in `scope`, registers the briefing and returns its token.
fn brief_of(r: &Rdy, scope: &Scope, id: &BlockId) -> String {
    let cov = Coverage {
        ids: vec![id.to_string()],
        path: None,
    };
    let b = r.read().brief(scope, &cov, None).unwrap();
    b.register(&r.briefs());
    b.base.token
}

fn rich_project(dir: &std::path::Path) -> Rdy {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let blob = r
        .store_mut()
        .put_blob(&[0u8, 159, 146, 150, 255])
        .unwrap()
        .to_string();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
        {"op": "create", "type": "doc", "id": "$doc", "parent": "$f", "fields": {"title": "Ünïcødé — design", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Components"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Line one.\nLine two with \"quotes\" and: colons\n\ttabbed\n"},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "Flow"}, "body": "graph TD\n  A[{{$t}}] --> B{{Decide?}}\n"},
        {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": "yaml"}, "body": "image: {{ .Values.image }}\n"},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Write \\{{literal}} for braces."},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "application/octet-stream", "alt": "bytes"}},
        {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": ["a", "b"], "rows": [[1, true], ["x", null]], "normative": false}},
        {"op": "create", "type": "folder", "parent": "$f", "fields": {"name": "empty"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    let t = res.created["$t"].clone();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Uses {{{{{t}}}}}."), "links": {"describes": [{"target": t, "part": "the balance part"}]}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    r.decide(
        &agent(),
        "work",
        DecisionInput {
            parent: s,
            statement: "Ledger is append-only".into(),
            options: vec!["append-only".into(), "mutable".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let p = r.propose(&agent(), "work", "more").unwrap();
    r.land(&founder(), &p.id).unwrap();
    r
}

#[test]
fn export_import_round_trip_preserves_every_version_hash() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let a = rich_project(a_dir.path());
    let summary = a.read().export(&Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.docs, 1);
    assert_eq!(summary.folders, 2);
    assert_eq!(summary.blobs, 1);
    let original: BTreeMap<BlockId, Hash> = a
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot()
        .blocks;
    assert_eq!(summary.blocks, original.len());

    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    assert_eq!(
        b.import(&agent(), "import", out.path()).unwrap_err().code,
        "E_PERMISSION"
    );
    b.import(&founder(), "import", out.path()).unwrap();
    let p = b.propose(&founder(), "import", "migrate").unwrap();
    b.land(&founder(), &p.id).unwrap();
    let imported = b
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot()
        .blocks;
    assert_eq!(imported, original);

    // the blob came across too
    let fig = b
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .find(|x| x.block_type == "figure")
        .unwrap()
        .clone();
    let h: Hash = fig.field_str("blob").unwrap().parse().unwrap();
    assert_eq!(
        b.store().get_blob(&h).unwrap(),
        vec![0u8, 159, 146, 150, 255]
    );

    // importing twice collides
    assert_eq!(
        b.import(&founder(), "again", out.path()).unwrap_err().code,
        "E_ALREADY_EXISTS"
    );
}

#[test]
fn single_block_yaml_round_trip_and_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let term = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .find(|x| x.block_type == "term")
        .unwrap()
        .clone();
    let text = block_to_yaml(&term);
    assert_eq!(block_from_yaml(&text).unwrap(), term);

    let mut edited = term.clone();
    edited.body = "Edited definition.".into();
    r.edit_block(&founder(), "work", edited.clone(), None)
        .unwrap();
    assert_eq!(
        r.read()
            .block(&Scope::View("work".into()), &term.id)
            .unwrap()
            .body,
        "Edited definition."
    );

    let mut moved = term.clone();
    moved.order = "zz".into();
    assert_eq!(
        r.edit_block(&founder(), "work", moved, None)
            .unwrap_err()
            .code,
        "E_BAD_INPUT"
    );
    assert_eq!(block_from_yaml("id: [").unwrap_err().code, "E_BAD_INPUT");
}

// ---- import failure paths ----

use std::path::Path;

fn exported(dir: &Path, out: &Path) -> Rdy {
    let a = rich_project(dir);
    a.read().export(&Scope::Canon, out).unwrap();
    a
}

fn doc_file(out: &Path) -> std::path::PathBuf {
    out.join("docs/design/n-c-d-design.yaml")
}

fn mutate_docs(out: &Path, f: impl FnOnce(&mut Vec<serde_yaml_ng::Value>)) {
    let p = doc_file(out);
    let mut list: Vec<serde_yaml_ng::Value> =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    f(&mut list);
    std::fs::write(&p, serde_yaml_ng::to_string(&list).unwrap()).unwrap();
}

fn find<'a>(list: &'a mut [serde_yaml_ng::Value], ty: &str) -> &'a mut serde_yaml_ng::Value {
    list.iter_mut()
        .find(|v| v["type"].as_str() == Some(ty))
        .unwrap()
}

fn id_of(list: &[serde_yaml_ng::Value], ty: &str) -> String {
    list.iter()
        .find(|v| v["type"].as_str() == Some(ty))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn assert_import_fails(out: &Path, code: &str) {
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let before = b.store().view_state("import").unwrap().blocks.clone();
    let err = b.import(&founder(), "import", out).unwrap_err();
    assert_eq!(err.code, code, "{err}");
    b.invalidate();
    assert_eq!(
        b.store().view_state("import").unwrap().blocks,
        before,
        "failed import must not change the view"
    );
    assert!(
        b.read()
            .materialized(&Scope::View("import".into()))
            .unwrap()
            .is_empty()
    );
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
    let f = std::fs::read_dir(out.path().join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&f, b"tampered").unwrap();
    assert_import_fails(out.path(), "E_BAD_INPUT");
}

#[test]
fn import_rejects_unknown_reference() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        find(l, "diagram")["body"] = "see {{t-ZZZZZ}}".into()
    });
    assert_import_fails(out.path(), "E_UNKNOWN_REF");
}

#[test]
fn import_rejects_unknown_link_target() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        find(l, "claim")["links"]["describes"][0]["target"] = "t-ZZZZZ".into()
    });
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

// a doc file's blocks must lead to its doc, so a bad parent is refused as E_FILE_FORMAT
#[test]
fn import_rejects_dangling_parent() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        find(l, "section")["parent"] = "d-ZZZZZ".into()
    });
    assert_import_fails(out.path(), "E_FILE_FORMAT");
}

#[test]
fn import_rejects_parent_cycle() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        let t = id_of(l, "term");
        find(l, "section")["parent"] = t.into();
    });
    assert_import_fails(out.path(), "E_FILE_FORMAT");
}

#[test]
fn import_rejects_self_parent() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        let s = id_of(l, "section");
        find(l, "section")["parent"] = s.into();
    });
    assert_import_fails(out.path(), "E_FILE_FORMAT");
}

#[test]
fn import_rejects_invalid_block() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        find(l, "term")["fields"]
            .as_mapping_mut()
            .unwrap()
            .remove("name");
    });
    assert_import_fails(out.path(), "E_VALIDATION");
}

#[test]
fn import_rejects_unsupported_or_missing_manifest() {
    let (_d, out, _a) = setup();
    std::fs::write(
        out.path().join("rdy-export.yaml"),
        "format: 4\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    assert_import_fails(out.path(), "E_BAD_INPUT");
    std::fs::remove_file(out.path().join("rdy-export.yaml")).unwrap();
    assert_import_fails(out.path(), "E_BAD_INPUT");
}

#[test]
fn edit_block_applies_field_unset_and_links_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let claim = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .find(|x| x.block_type == "claim")
        .unwrap()
        .clone();
    let mut e = claim.clone();
    e.links.clear();
    r.edit_block(&founder(), "work", e, None).unwrap();
    assert!(
        r.read()
            .block(&Scope::View("work".into()), &claim.id)
            .unwrap()
            .links
            .is_empty()
    );

    let sec = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .find(|x| x.block_type == "section")
        .unwrap()
        .clone();
    let mut e = sec.clone();
    e.fields
        .insert("title".into(), FieldValue::Str("Renamed".into()));
    e.fields
        .insert("lifecycle".into(), FieldValue::Str("stable".into()));
    // bwr1: making the section stable touches stable content, so the edit cites a briefing
    let t = brief_of(&r, &Scope::Canon, &sec.id);
    r.edit_block(&founder(), "work", e, Some(t))
        .unwrap_or_else(|x| panic!("{x}"));
    let got = r
        .read()
        .block(&Scope::View("work".into()), &sec.id)
        .unwrap();
    assert_eq!(got.field_str("title"), Some("Renamed"));
    let mut e = got.clone();
    e.fields.remove("lifecycle");
    let t = brief_of(&r, &Scope::View("work".into()), &sec.id);
    r.edit_block(&founder(), "work", e, Some(t)).unwrap();
    assert!(
        !r.read()
            .block(&Scope::View("work".into()), &sec.id)
            .unwrap()
            .fields
            .contains_key("lifecycle")
    );
}

#[test]
fn edit_block_respects_decision_rules() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let dec = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .find(|x| x.block_type == "decision")
        .unwrap()
        .clone();
    assert_eq!(dec.field_str("status"), Some("accepted"));
    let mut e = dec.clone();
    e.body = "changed".into();
    assert_eq!(
        r.edit_block(&founder(), "work", e, None).unwrap_err().code,
        "E_FROZEN"
    );

    let res = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: dec.parent.clone().unwrap().to_string(),
                statement: "Another".into(),
                options: vec!["x".into(), "y".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let id = BlockId::parse(res.created.values().next().unwrap()).unwrap();
    let mut e = r.read().block(&Scope::View("work".into()), &id).unwrap();
    e.fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    assert_eq!(
        r.edit_block(&agent(), "work", e, None).unwrap_err().code,
        "E_PERMISSION"
    );
}

#[test]
fn export_refuses_a_non_empty_directory() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let a = rich_project(a_dir.path());
    std::fs::create_dir_all(out.path().join("docs")).unwrap();
    std::fs::write(out.path().join("docs/d-STALE.yaml"), "[]").unwrap();
    let e = a.read().export(&Scope::Canon, out.path()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.fix.contains("empty directory"), "{}", e.fix);
    assert_eq!(
        std::fs::read_dir(out.path().join("docs")).unwrap().count(),
        1,
        "nothing written"
    );
    // a directory that does not exist yet is fine
    a.read()
        .export(&Scope::Canon, &out.path().join("fresh"))
        .unwrap();
}

#[test]
fn import_skips_dotfiles_and_non_yaml_files() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let a = rich_project(a_dir.path());
    a.read().export(&Scope::Canon, out.path()).unwrap();
    std::fs::write(out.path().join("docs/.DS_Store"), [0u8, 1, 2]).unwrap();
    std::fs::write(out.path().join("docs/README.txt"), "notes").unwrap();
    std::fs::write(out.path().join("blobs/.DS_Store"), [0u8, 1, 2]).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "import", out.path()).unwrap();
    let n = a.read().materialized(&Scope::Canon).unwrap().len();
    assert_eq!(
        b.read()
            .materialized(&Scope::View("import".into()))
            .unwrap()
            .len(),
        n
    );
}

#[test]
fn edit_block_refuses_a_no_op_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let term = r
        .read()
        .materialized(&Scope::View("work".into()))
        .unwrap()
        .iter()
        .find(|x| x.block_type == "term")
        .unwrap()
        .clone();
    let e = r.edit_block(&founder(), "work", term, None).unwrap_err();
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
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "S"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    // an older project can hold such a figure: commit it past the write path's blob check
    let figure = Block {
        id: BlockId::parse("fg-AAAAA").unwrap(),
        block_type: "figure".into(),
        parent: Some(BlockId::parse(&res.created["$s"]).unwrap()),
        order: "a1".into(),
        fields: [
            ("blob", missing.as_str()),
            ("media_type", "application/octet-stream"),
            ("alt", "gone"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
        .collect(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    };
    let change = Change {
        view: "work".into(),
        ops: vec![Op::Create { block: figure }],
        deps: vec![],
        provenance: agent().provenance(),
        justification: None,
        message: "raw".into(),
        links: ChangeLinks::default(),
    };
    a.store_mut().commit_to_view("work", change).unwrap();
    a.invalidate();
    let out = parent.path().join("exp");
    let e = a
        .read()
        .export(&Scope::View("work".into()), &out)
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    assert!(e.message.contains("fg-AAAAA"), "names the figure: {e}");
    assert_eq!(
        std::fs::read_dir(parent.path()).unwrap().count(),
        0,
        "nothing written, no staging dir left behind"
    );
}

#[test]
fn export_creates_missing_parents_and_clears_a_leftover_staging_dir() {
    let a_dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let a = rich_project(a_dir.path());
    let deeper = root.path().join("nested/deeper");
    // an interrupted earlier export left its staging dir behind
    std::fs::create_dir_all(deeper.join(".exp.rdy-export-partial/docs")).unwrap();
    std::fs::write(
        deeper.join(".exp.rdy-export-partial/docs/d-STALE.yaml"),
        "[]",
    )
    .unwrap();
    a.read().export(&Scope::Canon, &deeper.join("exp")).unwrap();
    assert!(!deeper.join("exp/docs/d-STALE.yaml").exists());
    assert!(deeper.join("exp/rdy-export.yaml").exists());
    let names: Vec<_> = std::fs::read_dir(&deeper)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("exp")]);
}

fn remove_blob_files(out: &Path) {
    for f in std::fs::read_dir(out.join("blobs")).unwrap() {
        std::fs::remove_file(f.unwrap().path()).unwrap();
    }
}

#[test]
fn import_rejects_a_figure_whose_blob_is_missing() {
    let (_d, out, _a) = setup();
    remove_blob_files(out.path());
    assert_import_fails(out.path(), "E_UNKNOWN_REF");
}

#[test]
fn import_accepts_a_missing_blob_file_when_the_project_already_has_the_blob() {
    let (_d, out, _a) = setup();
    remove_blob_files(out.path());
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.store_mut().put_blob(&[0u8, 159, 146, 150, 255]).unwrap();
    b.import(&founder(), "import", out.path()).unwrap();
}

#[test]
fn import_rejects_an_export_from_an_unrelated_pack() {
    let (_d, out, _a) = setup();
    std::fs::write(
        out.path().join("rdy-export.yaml"),
        "format: 3\npack: other-pack\nscope: canon\n",
    )
    .unwrap();
    assert_import_fails(out.path(), "E_BAD_INPUT");
}

#[test]
fn import_accepts_an_export_into_a_pack_that_extends_its_pack() {
    let (_d, out, _a) = setup();
    let b_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(b_dir.path().join(".rdy/packs")).unwrap();
    std::fs::write(
        b_dir.path().join(".rdy/packs/team.yaml"),
        "name: team\nversion: \"1\"\nextends: rdy-standard\n",
    )
    .unwrap();
    let mut b = Rdy::init(b_dir.path(), "team").unwrap();
    b.import(&founder(), "import", out.path()).unwrap();
}

#[test]
fn a_refused_import_stores_no_blobs() {
    let (_d, out, _a) = setup();
    mutate_docs(out.path(), |l| {
        find(l, "term")["fields"]
            .as_mapping_mut()
            .unwrap()
            .remove("name");
    });
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    assert_eq!(
        b.import(&founder(), "import", out.path()).unwrap_err().code,
        "E_VALIDATION"
    );
    assert!(
        !b.store()
            .has(&rdy_store::Store::blob_hash(&[0u8, 159, 146, 150, 255]))
            .unwrap()
    );
}

fn files(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = Default::default();
    walk(root, root, &mut out);
    out
}

#[test]
fn export_writes_the_folder_tree() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    exported(a_dir.path(), out.path());
    let f = files(out.path());
    for p in [
        "rdy-export.yaml",
        "docs/design/_folder.yaml",
        "docs/design/n-c-d-design.yaml",
        "docs/design/empty/_folder.yaml",
    ] {
        assert!(
            f.contains_key(p),
            "{p} missing from {:?}",
            f.keys().collect::<Vec<_>>()
        );
    }
    assert!(String::from_utf8_lossy(&f["rdy-export.yaml"]).contains("format: 3"));
    let folder = String::from_utf8_lossy(&f["docs/design/_folder.yaml"]).to_string();
    assert!(
        folder.contains("type: folder") && folder.contains("title: Design"),
        "{folder}"
    );
    assert!(
        !folder.contains("name:") && !folder.contains("parent:"),
        "the file system holds name and parent: {folder}"
    );
    let doc: Vec<serde_yaml_ng::Value> =
        serde_yaml_ng::from_slice(&f["docs/design/n-c-d-design.yaml"]).unwrap();
    assert_eq!(doc[0]["type"].as_str(), Some("doc"));
    assert!(doc[0].get("parent").is_none() && doc[0]["fields"].get("name").is_none());
    assert!(
        doc[1].get("parent").is_some(),
        "inner blocks keep their parent"
    );
}

#[test]
fn export_import_export_is_byte_identical() {
    let a_dir = tempfile::tempdir().unwrap();
    let out1 = tempfile::tempdir().unwrap();
    let a = exported(a_dir.path(), out1.path());
    let original = a
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot()
        .blocks;
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "import", out1.path()).unwrap();
    let p = b.propose(&founder(), "import", "migrate").unwrap();
    b.land(&founder(), &p.id).unwrap();
    assert_eq!(
        b.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .snapshot()
            .blocks,
        original
    );
    let out2_dir = tempfile::tempdir().unwrap();
    let out2 = out2_dir.path().join("again");
    b.read().export(&Scope::Canon, &out2).unwrap();
    let (mut again, mut first) = (files(&out2), files(out1.path()));
    let rows = |m: &mut std::collections::BTreeMap<String, Vec<u8>>| -> Vec<serde_yaml_ng::Value> {
        serde_yaml_ng::from_slice(&m.remove("authors.yaml").expect("authors.yaml")).unwrap()
    };
    let (again_rows, first_rows) = (rows(&mut again), rows(&mut first));
    assert_eq!(again, first);
    // mfb3: one change per author, so each block keeps its author, run and role, and its time
    // is its author's newest time
    let mut newest: BTreeMap<String, i64> = BTreeMap::new();
    for r in &first_rows {
        let at = newest
            .entry(r["actor"].as_str().unwrap().into())
            .or_default();
        *at = (*at).max(r["at_ms"].as_i64().unwrap());
    }
    assert_eq!(again_rows.len(), first_rows.len());
    for (x, y) in again_rows.iter().zip(&first_rows) {
        for key in ["id", "actor", "run", "role"] {
            assert_eq!(x[key], y[key], "{key}");
        }
        assert_eq!(
            x["at_ms"].as_i64(),
            Some(newest[y["actor"].as_str().unwrap()])
        );
    }
}

#[test]
fn a_nameless_m1_doc_exports_with_a_warning_and_imports_named() {
    let a_dir = tempfile::tempdir().unwrap();
    let mut a = Rdy::init(a_dir.path(), "rdy-standard").unwrap();
    let mut fields = BTreeMap::new();
    fields.insert("title".to_string(), FieldValue::Str("Old Notes".into()));
    fields.insert("doc_type".to_string(), FieldValue::Str("page".into()));
    let block = Block {
        id: BlockId::parse("d-AAAAB").unwrap(),
        block_type: "doc".into(),
        parent: None,
        order: "V".into(),
        fields,
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
    };
    a.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Create { block }],
                deps: vec![],
                provenance: agent().provenance(),
                justification: None,
                message: String::new(),
                links: ChangeLinks::default(),
            },
        )
        .unwrap();
    a.invalidate();
    let out = tempfile::tempdir().unwrap();
    let s = a
        .read()
        .export(&Scope::View("work".into()), out.path())
        .unwrap();
    assert_eq!(s.warnings.len(), 1);
    assert!(s.warnings[0].contains("d-AAAAB"));
    assert!(out.path().join("docs/old-notes.yaml").exists());
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "import", out.path()).unwrap();
    let doc = b
        .read()
        .block(
            &Scope::View("import".into()),
            &BlockId::parse("d-AAAAB").unwrap(),
        )
        .unwrap();
    assert_eq!(doc.field_str("name"), Some("old-notes"));
}

#[test]
fn name_or_parent_on_a_folder_or_doc_entry_is_refused_with_its_position() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    exported(a_dir.path(), out.path());
    let folder = out.path().join("docs/design/_folder.yaml");
    let text = std::fs::read_to_string(&folder).unwrap();
    std::fs::write(&folder, format!("{text}parent: fd-AAAAA\n")).unwrap();
    let line = text.lines().count() + 1;
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let e = b.import(&founder(), "import", out.path()).unwrap_err();
    assert_eq!(e.code, "E_FILE_FORMAT");
    assert!(
        e.message
            .starts_with(&format!("docs/design/_folder.yaml:{line}:1:")),
        "{e}"
    );
}

#[test]
fn format_2_import_ignores_hidden_and_non_yaml_files() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    exported(a_dir.path(), out.path());
    std::fs::write(out.path().join("docs/.DS_Store"), [0u8, 1]).unwrap();
    std::fs::write(out.path().join("docs/design/README.md"), "notes").unwrap();
    std::fs::create_dir_all(out.path().join("docs/.git/objects")).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "import", out.path()).unwrap();
}

fn raw_doc(r: &mut Rdy, id: &str, name: &str) {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_string(), FieldValue::Str(name.into()));
    fields.insert("name".to_string(), FieldValue::Str(name.into()));
    fields.insert("doc_type".to_string(), FieldValue::Str("page".into()));
    let block = Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "doc".into(),
        parent: None,
        order: "V".into(),
        fields,
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
    };
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Create { block }],
                deps: vec![],
                provenance: agent().provenance(),
                justification: None,
                message: "raw".into(),
                links: ChangeLinks::default(),
            },
        )
        .unwrap();
    r.invalidate();
}

#[test]
fn import_refuses_names_that_clash_with_the_view_or_each_other() {
    let mk = |id: &str| {
        let d = tempfile::tempdir().unwrap();
        let mut a = Rdy::init(d.path(), "rdy-standard").unwrap();
        raw_doc(&mut a, id, "x");
        let out = tempfile::tempdir().unwrap();
        a.read()
            .export(&Scope::View("work".into()), out.path())
            .unwrap();
        (d, out)
    };
    let (_d1, o1) = mk("d-AAAAA");
    let (_d2, o2) = mk("d-BBBBB");
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "imp", o1.path()).unwrap();
    let e = b.import(&founder(), "imp", o2.path()).unwrap_err();
    assert_eq!(e.code, "E_PATH_CLASH", "{e}");
    assert!(
        e.message.contains("d-AAAAA") && e.message.contains("d-BBBBB"),
        "{e}"
    );
}

#[test]
fn export_refuses_sibling_names_that_clash() {
    let d = tempfile::tempdir().unwrap();
    let mut a = Rdy::init(d.path(), "rdy-standard").unwrap();
    raw_doc(&mut a, "d-AAAAA", "x");
    raw_doc(&mut a, "d-BBBBB", "X");
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("exp");
    let e = a
        .read()
        .export(&Scope::View("work".into()), &out)
        .unwrap_err();
    assert_eq!(e.code, "E_PATH_CLASH", "{e}");
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[test]
fn import_refuses_a_second_doc_or_a_folder_inside_a_doc_file() {
    for ty in ["doc", "folder"] {
        let (_d, out, _a) = setup();
        mutate_docs(out.path(), |l| {
            let head = id_of(l, "doc");
            let entry: serde_yaml_ng::Value = serde_yaml_ng::from_str(&if ty == "doc" {
                format!("{{id: d-QQQQQ, type: doc, parent: {head}, order: W, fields: {{name: other, title: Other, doc_type: page}}}}")
            } else {
                format!("{{id: fd-QQQQQ, type: folder, parent: {head}, order: W}}")
            })
            .unwrap();
            l.push(entry);
        });
        assert_import_fails(out.path(), "E_FILE_FORMAT");
    }
}

#[test]
fn import_refuses_an_inner_block_whose_parent_is_in_another_doc_file() {
    let (_d, out, _a) = setup();
    let mut head = String::new();
    mutate_docs(out.path(), |l| head = id_of(l, "doc"));
    std::fs::write(
        out.path().join("docs/other.yaml"),
        format!("- id: d-QQQQQ\n  type: doc\n  order: W\n  fields:\n    title: Other\n    doc_type: page\n- id: s-QQQQQ\n  type: section\n  parent: {head}\n  order: V\n  fields:\n    title: Terms\n"),
    )
    .unwrap();
    assert_import_fails(out.path(), "E_FILE_FORMAT");
}

#[test]
fn import_refuses_folders_nested_more_than_32_deep() {
    let out = tempfile::tempdir().unwrap();
    std::fs::write(
        out.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    let mut dir = out.path().join("docs");
    for i in 0..33 {
        dir = dir.join(format!("f{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("_folder.yaml"),
            format!("id: fd-{i:05}\ntype: folder\norder: V\n"),
        )
        .unwrap();
    }
    assert_import_fails(out.path(), "E_BAD_INPUT");
    // 32 deep imports
    std::fs::remove_dir_all(dir).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "import", out.path()).unwrap();
}

#[test]
fn a_duplicated_doc_is_reported_as_a_duplicate_id_not_a_name_clash() {
    let doc =
        "- id: d-AAAAB\n  type: doc\n  order: V\n  fields:\n    title: Notes\n    doc_type: page\n";
    let out = tempfile::tempdir().unwrap();
    std::fs::write(
        out.path().join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(out.path().join("docs")).unwrap();
    std::fs::write(out.path().join("docs/notes.yaml"), doc).unwrap();
    std::fs::write(out.path().join("docs/copy.yaml"), doc).unwrap();
    assert_import_fails(out.path(), "E_ALREADY_EXISTS");
}

/// hd3 fix round 1: a link `part` that is not text (YAML `.nan`, `.inf`, a number, a bool) is
/// refused with the link-forms text, never dropped; a null part is no part.
#[test]
fn hd3_a_yaml_link_part_that_is_not_text_is_refused_not_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let r = rich_project(dir.path());
    let blocks = r.read().materialized(&Scope::Canon).unwrap();
    let term = blocks.iter().find(|x| x.block_type == "term").unwrap();
    let target = blocks
        .iter()
        .find(|x| x.block_type == "claim")
        .unwrap()
        .id
        .to_string();
    let yaml = |part: &str| {
        format!(
            "id: {}\ntype: term\norder: {}\nlinks:\n  refines:\n  - target: {target}\n    part: {part}\n",
            term.id, term.order
        )
    };
    for part in [".nan", ".inf", "-.inf", "1", "true"] {
        let e = block_from_yaml(&yaml(part)).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{part}: {e}");
        assert!(
            e.message.contains("refines: a link kind takes"),
            "{part}: {e}"
        );
    }
    let b = block_from_yaml(&yaml("~")).unwrap();
    assert_eq!(b.links["refines"][0].part, None, "a null part is no part");
    let b = block_from_yaml(&yaml("w")).unwrap();
    assert_eq!(b.links["refines"][0].part.as_deref(), Some("w"));
}

#[test]
fn hd8_block_from_yaml_refuses_a_misspelt_key() {
    let e = block_from_yaml("id: p-AAAAA\ntype: prose\norder: a1\nparnet: d-AAAAA\n").unwrap_err();
    assert_eq!(e.code, "E_FILE_FORMAT");
    assert_eq!(e.message, "line 4: unknown key \"parnet\" in the block");
    assert_eq!(e.fix, "did you mean \"parent\"? rename it");
    assert_eq!(
        block_from_yaml("id: [").unwrap_err().code,
        "E_BAD_INPUT",
        "syntax stays E_BAD_INPUT"
    );
}

#[test]
fn hd8_block_from_yaml_refuses_a_misspelt_link_key() {
    let yaml = "id: p-AAAAA\ntype: prose\norder: a1\nlinks:\n  refines:\n  - target: c-AAAAA\n    prat: x\n";
    let e = block_from_yaml(yaml).unwrap_err();
    assert_eq!(e.code, "E_FILE_FORMAT");
    assert_eq!(e.message, "line 7: unknown key \"prat\" in a refines link");
    assert_eq!(e.fix, "did you mean \"part\"? rename it");
}
