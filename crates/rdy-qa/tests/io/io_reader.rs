//! ykey1: a malformed entry names the key at fault (FW-F10 import half). ord4: an order key that
//! YAML reads as a number is E_ORDER_KEY with a quoting fix, never a cascade. lbr1: a {{…}} that
//! names nothing says how to write it literally, and \{{ round-trips.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

fn tree(doc: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(t.path().join("docs/spec.yaml"), doc).unwrap();
    t
}

const HEAD: &str =
    "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Spec\n    doc_type: page\n";

#[test]
fn ord4_an_unquoted_order_key_is_e_order_key_with_a_quoting_fix() {
    let t = tree(&format!(
        "{HEAD}- id: p-AAAAA\n  type: prose\n  parent: d-AAAAA\n  order: 00E1\n  body: Hi.\n"
    ));
    let d = check_path(&pack(), None, t.path()).unwrap();
    let o = d
        .iter()
        .find(|d| d.code == "E_ORDER_KEY")
        .unwrap_or_else(|| panic!("{d:?}"));
    assert_eq!(o.line, 10, "{o:?}");
    assert!(
        o.message.contains("00E1") && o.message.contains("a number"),
        "{o:?}"
    );
    assert!(o.fix.contains("order: '00E1'"), "{o:?}");
    assert!(
        d.iter().all(|d| d.code != "E_MISSING_PARENT"),
        "no cascade: {d:?}"
    );
    let (_d, mut r) = project();
    let e = r.import(&approver(), "work", t.path()).unwrap_err();
    assert!(
        e.code == "E_ORDER_KEY"
            || e.issues.iter().any(|i| i.code == "E_ORDER_KEY")
            || e.message.contains("00E1"),
        "{e}"
    );
}

#[test]
fn ykey1_a_wrong_typed_or_unknown_key_is_named() {
    let t = tree(&format!(
        "{HEAD}- id: p-AAAAA\n  type: prose\n  parent: d-AAAAA\n  order: a1\n  body: [not, text]\n- id: p-BBBBB\n  type: prose\n  parent: d-AAAAA\n  order: a2\n  boddy: Hi.\n"
    ));
    let d = check_path(&pack(), None, t.path()).unwrap();
    assert!(
        d.iter().any(|d| d.code == "E_FILE_FORMAT"
            && d.message.contains("\"body\" must be a string, not a list")
            && d.line == 11),
        "{d:?}"
    );
    assert!(
        d.iter()
            .any(|d| d.code == "E_FILE_FORMAT" && d.message.contains("unknown key \"boddy\"")),
        "{d:?}"
    );
    let t = tree(&format!(
        "{HEAD}- id: p-AAAAA\n  type: prose\n  parent: d-AAAAA\n  body: Hi.\n"
    ));
    let d = check_path(&pack(), None, t.path()).unwrap();
    assert!(
        d.iter()
            .any(|d| d.message.contains("the entry has no \"order\"")),
        "{d:?}"
    );
}

#[test]
fn lbr1_an_unknown_brace_token_says_how_to_escape_it() {
    let t = tree(&format!(
        "{HEAD}- id: p-AAAAA\n  type: prose\n  parent: d-AAAAA\n  order: a1\n  body: Use {{{{name}}}} here.\n"
    ));
    let d = check_path(&pack(), None, t.path()).unwrap();
    let u = d
        .iter()
        .find(|d| d.code == "E_UNKNOWN_REF")
        .unwrap_or_else(|| panic!("{d:?}"));
    assert!(u.message.contains("{{name}}"), "{u:?}");
    assert!(u.fix.contains("\\{{name}}"), "{u:?}");
    let (_d, mut r) = project();
    let (_doc, s, _c) = seed_doc(&mut r, "work");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "parent": s, "body": "Use {{name}} here."}]),
    )
    .unwrap_err();
    assert!(
        e.fix.contains("\\{{name}}") || e.issues.iter().any(|i| i.fix.contains("\\{{name}}")),
        "{e:?}"
    );
}

#[test]
fn lbr1_an_escaped_brace_round_trips_through_export_and_import() {
    // guard: escaping already works; this pins the round trip the bootstrap needed
    let (_d, mut r) = project();
    let (_doc, s, _c) = seed_doc(&mut r, "work");
    let w = write(&mut r, "work", json!([{"op": "create", "type": "prose", "id": "$p", "parent": s, "body": "Write \\{{id}} for a reference."}])).unwrap();
    let p = w.created["$p"].clone();
    let out = tempfile::tempdir().unwrap();
    r.read()
        .export(&Scope::View("work".into()), &out.path().join("x"))
        .unwrap();
    let (_d2, mut r2) = project();
    r2.import(&approver(), "work", &out.path().join("x"))
        .unwrap();
    assert_eq!(
        body_of(&r2, &Scope::View("work".into()), &p),
        "Write \\{{id}} for a reference."
    );
}
