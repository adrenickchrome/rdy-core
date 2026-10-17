use rdy_engine::*;
use rdy_schema::{PackSet, Severity};
use std::path::Path;

fn pack() -> rdy_schema::Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: V\n  fields:\n    title: Transactions\n    doc_type: page\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: V\n  fields:\n    title: Terms\n";
const FOLDER: &str = "id: fd-AAAAA\ntype: folder\norder: V\n";

fn check(root: &Path) -> Vec<Diagnostic> {
    check_path(&pack(), None, root).unwrap()
}

fn one(root: &Path) -> Diagnostic {
    let d = check(root);
    assert_eq!(d.len(), 1, "{d:#?}");
    d[0].clone()
}

/// The one error (skeleton warnings aside).
fn one_error(root: &Path) -> Diagnostic {
    let d: Vec<Diagnostic> = check(root)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(d.len(), 1, "{d:#?}");
    d[0].clone()
}

#[test]
fn a_clean_tree_has_no_diagnostics() {
    let t = tempfile::tempdir().unwrap();
    put(t.path(), "wallets/_folder.yaml", FOLDER);
    put(t.path(), "wallets/transactions.yaml", DOC);
    assert_eq!(check(t.path()), vec![]);
    assert_eq!(
        check_path(&pack(), None, &t.path().join("wallets/transactions.yaml")).unwrap(),
        vec![],
        "a single file"
    );
}

#[test]
fn syntax_errors_carry_their_position() {
    let t = tempfile::tempdir().unwrap();
    put(
        t.path(),
        "x.yaml",
        "- id: d-AAAAA\n  type: doc\n    order: V\n",
    );
    let d = one(t.path());
    assert_eq!((d.code.as_str(), d.line), ("E_FILE_FORMAT", 3), "{d:?}");
}

#[test]
fn entry_shape_errors_point_at_the_entry() {
    let t = tempfile::tempdir().unwrap();
    put(t.path(), "x.yaml", "- type: doc\n  order: V\n");
    let d = one(t.path());
    assert_eq!(
        (d.code.as_str(), d.line, d.column),
        ("E_FILE_FORMAT", 1, 3),
        "{d:?}"
    );
}

#[test]
fn bad_and_reserved_names() {
    for file in ["has space.yaml", "index.yaml", "_x.yaml", "con.yaml"] {
        let t = tempfile::tempdir().unwrap();
        put(t.path(), file, DOC);
        let d = check(t.path());
        assert!(
            d.iter()
                .any(|d| d.code == "E_BAD_NAME" && d.file.ends_with(file)),
            "{file}: {d:#?}"
        );
    }
}

#[test]
fn sibling_names_clash_ignoring_case() {
    let t = tempfile::tempdir().unwrap();
    put(t.path(), "wallets/_folder.yaml", FOLDER);
    put(t.path(), "Wallets.yaml", DOC);
    assert!(check(t.path()).iter().any(|d| d.code == "E_PATH_CLASH"));
}

#[test]
fn name_and_parent_must_not_be_written_on_folders_and_docs() {
    let t = tempfile::tempdir().unwrap();
    put(
        t.path(),
        "x.yaml",
        &DOC.replace(
            "    title: Transactions\n",
            "    title: Transactions\n    name: y\n",
        ),
    );
    let d = one(t.path());
    assert_eq!(
        (d.code.as_str(), d.line, d.column),
        ("E_FILE_FORMAT", 6, 5),
        "{d:?}"
    );
    let t = tempfile::tempdir().unwrap();
    put(
        t.path(),
        "f/_folder.yaml",
        &format!("{FOLDER}parent: fd-BBBBB\n"),
    );
    let d = one(t.path());
    assert_eq!(
        (d.code.as_str(), d.line, d.column),
        ("E_FILE_FORMAT", 4, 1),
        "{d:?}"
    );
}

#[test]
fn schema_errors_point_at_the_field() {
    let t = tempfile::tempdir().unwrap();
    put(
        t.path(),
        "x.yaml",
        &DOC.replace("    title: Terms\n", "    title: Terms\n    colour: red\n"),
    );
    let d = one(t.path());
    assert_eq!(
        (d.code.as_str(), d.line, d.column),
        ("E_FIELD_UNKNOWN", 13, 5),
        "{d:?}"
    );
}

#[test]
fn duplicate_ids_and_unknown_references() {
    let t = tempfile::tempdir().unwrap();
    put(t.path(), "a.yaml", DOC);
    put(t.path(), "b.yaml", &DOC.replace("Transactions", "Other"));
    assert!(
        check(t.path())
            .iter()
            .any(|d| d.code == "E_ALREADY_EXISTS" && d.file.ends_with("b.yaml"))
    );
    let t = tempfile::tempdir().unwrap();
    put(
        t.path(),
        "a.yaml",
        &format!(
            "{DOC}- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: V\n  body: See {{{{t-ZZZZZ}}}}.\n  links:\n    describes:\n    - target: m-ZZZZZ\n"
        ),
    );
    let d = check(t.path());
    let refs: Vec<_> = d
        .iter()
        .filter(|d| d.code == "E_UNKNOWN_REF")
        .map(|d| d.line)
        .collect();
    assert_eq!(refs, vec![17, 18], "body line, then links line: {d:#?}");
}

#[test]
fn json_shape() {
    let t = tempfile::tempdir().unwrap();
    put(t.path(), "index.yaml", DOC);
    let v = serde_json::to_value(check(t.path())).unwrap();
    let first = &v[0];
    for k in [
        "file", "line", "column", "code", "message", "fix", "severity",
    ] {
        assert!(first.get(k).is_some(), "{k} missing in {first}");
    }
    assert_eq!(first["severity"], "error");
}

#[test]
fn a_doc_file_holds_one_doc_and_no_folders() {
    for (ty, extra) in [
        (
            "doc",
            "  fields:\n    name: other\n    title: Other\n    doc_type: page\n",
        ),
        ("folder", ""),
    ] {
        let t = tempfile::tempdir().unwrap();
        let id = if ty == "doc" { "d-BBBBB" } else { "fd-BBBBB" };
        put(
            t.path(),
            "x.yaml",
            &format!("{DOC}- id: {id}\n  type: {ty}\n  parent: d-AAAAA\n  order: W\n{extra}"),
        );
        let d = one(t.path());
        assert_eq!(
            (d.code.as_str(), d.line, d.column),
            ("E_FILE_FORMAT", 14, 3),
            "{ty}: {d:?}"
        );
        assert!(d.message.contains("one doc"), "{d:?}");
    }
}

#[test]
fn inner_blocks_stay_inside_their_files_doc() {
    let t = tempfile::tempdir().unwrap();
    put(t.path(), "a.yaml", DOC);
    // s-BBBBB's parent is the doc of another file
    put(
        t.path(),
        "b.yaml",
        "- id: d-BBBBB\n  type: doc\n  order: W\n  fields:\n    title: Other\n    doc_type: page\n- id: s-BBBBB\n  type: section\n  parent: d-AAAAA\n  order: V\n  fields:\n    title: Terms\n",
    );
    let d = one_error(t.path());
    assert_eq!(
        (d.code.as_str(), d.line, d.column),
        ("E_FILE_FORMAT", 9, 3),
        "{d:?}"
    );
    assert!(
        d.file.ends_with("b.yaml") && d.message.contains("d-AAAAA"),
        "{d:?}"
    );
}

#[test]
fn inside_a_project_canon_resolves_references_and_parents() {
    use rdy_model::*;
    use serde_json::json;
    let agent = Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    let founder = Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = r
        .write(
            &agent,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "folder", "id": "$w", "fields": {"name": "wallets"}},
                    {"op": "create", "type": "doc", "id": "$t", "parent": "$w", "fields": {"title": "Transactions", "doc_type": "page"}},
                    {"op": "create", "type": "section", "id": "$s", "parent": "$t", "fields": {"title": "Terms"}}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    // two docs from M1, without a stored name
    let legacy: Vec<Op> = ["d-AAAAB", "d-AAAAC"]
        .iter()
        .map(|id| Op::Create {
            block: Block {
                id: BlockId::parse(id).unwrap(),
                block_type: "doc".into(),
                parent: None,
                order: "V".into(),
                fields: [
                    ("title".to_string(), FieldValue::Str("Old".into())),
                    ("doc_type".to_string(), FieldValue::Str("page".into())),
                ]
                .into_iter()
                .collect(),
                body: String::new(),
                links: Default::default(),
                pins: Default::default(),
                read_identity: None,
            },
        })
        .collect();
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: legacy,
                deps: vec![],
                provenance: agent.provenance(),
                justification: None,
                message: "m1".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    let p = r.propose(&agent, "work", "seed", None).unwrap();
    r.land(&founder, &p.id).unwrap();
    let canon = r.read().materialized(&Scope::Canon).unwrap().clone();
    let (w, s) = (&res.created["$w"], &res.created["$s"]);

    let t = tempfile::tempdir().unwrap();
    // a doc that references a canon section, and a doc inside the canon folder
    put(
        t.path(),
        "notes.yaml",
        &format!(
            "- id: d-NNNNN\n  type: doc\n  order: V\n  fields:\n    title: Notes\n    doc_type: page\n- id: s-NNNNN\n  type: section\n  parent: d-NNNNN\n  order: V\n  fields:\n    title: Terms\n- id: c-NNNNN\n  type: claim\n  parent: s-NNNNN\n  order: V\n  body: See {{{{{s}}}}}.\n"
        ),
    );
    put(
        t.path(),
        "wallets/_folder.yaml",
        &format!("id: {w}\ntype: folder\norder: V\n"),
    );
    put(
        t.path(),
        "wallets/limits.yaml",
        "- id: d-MMMMM\n  type: doc\n  order: W\n  fields:\n    title: Limits\n    doc_type: page\n",
    );
    let d = check_path(&pack(), Some(&canon), t.path()).unwrap();
    assert!(
        !d.iter()
            .any(|d| d.code == "E_UNKNOWN_REF" || d.code == "E_MISSING_PARENT"),
        "{d:#?}"
    );
    let nameless: Vec<_> = d.iter().filter(|d| d.code == "W_NAMELESS_DOC").collect();
    assert_eq!(nameless.len(), 1, "one summary warning: {d:#?}");
    assert_eq!(nameless[0].file, "canon");
    assert!(nameless[0].message.contains("2 docs"), "{:?}", nameless[0]);
    assert_eq!(d.len(), 1, "{d:#?}");
    // without canon the reference is unknown
    assert!(
        check(t.path()).iter().any(|d| d.code == "E_UNKNOWN_REF"),
        "the canon is what resolves it"
    );
}
