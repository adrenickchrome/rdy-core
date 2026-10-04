use rdy_engine::*;
use rdy_schema::PackSet;
use std::path::Path;

fn pack() -> rdy_schema::Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: V\n  fields:\n    title: Transactions\n    doc_type: glossary\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: V\n  fields:\n    title: Terms\n";
const FOLDER: &str = "id: fd-AAAAA\ntype: folder\norder: V\n";

fn check(root: &Path) -> Vec<Diagnostic> {
    check_path(&pack(), None, root).unwrap()
}

fn one(root: &Path) -> Diagnostic {
    let d = check(root);
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
