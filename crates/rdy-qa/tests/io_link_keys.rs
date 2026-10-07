//! lkey1: a key works as a link target in import files, rdy edit and rdy check, and is stored as
//! the id (FW-F2; intake "keys in link targets").
use rdy_engine::{E_UNKNOWN_REF, Scope, block_to_yaml, check_path};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

const SPEC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Spec\n    doc_type: overview\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Rules\n- id: r-AAAAA\n  type: requirement\n  parent: s-AAAAA\n  order: a1\n  fields:\n    key: REQ-1\n  body: It must.\n";

fn claim(target: &str) -> String {
    format!(
        "- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: It holds.\n  links:\n    implements:\n    - target: {target}\n"
    )
}
fn tree(extra: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(t.path().join("docs/spec.yaml"), format!("{SPEC}{extra}")).unwrap();
    t
}
fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}
fn target_of(r: &rdy_engine::Rdy) -> String {
    let c = r
        .read()
        .block(
            &Scope::View("work".into()),
            &BlockId::parse("c-AAAAA").unwrap(),
        )
        .unwrap();
    c.links["implements"][0].target.to_string()
}

#[test]
fn lkey1_import_stores_a_key_link_target_as_the_id() {
    let (_d, mut r) = project();
    r.import(&approver(), "work", tree(&claim("REQ-1")).path())
        .unwrap();
    assert_eq!(target_of(&r), "r-AAAAA");
}

#[test]
fn lkey1_a_key_held_in_the_view_resolves_too() {
    let (_d, mut r) = project();
    r.import(&approver(), "work", tree("").path()).unwrap();
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(
        t.path().join("docs/more.yaml"),
        "- id: d-BBBBB\n  type: doc\n  order: a2\n  fields:\n    title: More\n    doc_type: overview\n- id: c-AAAAA\n  type: claim\n  parent: d-BBBBB\n  order: a1\n  body: It holds.\n  links:\n    implements:\n    - target: req-1\n",
    )
    .unwrap();
    r.import(&approver(), "work", t.path()).unwrap();
    assert_eq!(target_of(&r), "r-AAAAA");
}

#[test]
fn lkey1_an_unknown_key_is_e_unknown_ref_naming_it() {
    let (_d, mut r) = project();
    let e = r
        .import(&approver(), "work", tree(&claim("NOPE-9")).path())
        .unwrap_err();
    assert_eq!(e.code, E_UNKNOWN_REF, "{e}");
    assert!(e.message.contains("NOPE-9"), "{e}");
}

#[test]
fn lkey1_a_target_that_is_neither_an_id_nor_a_key_is_a_file_format_error() {
    let (_d, mut r) = project();
    let e = r
        .import(&approver(), "work", tree(&claim("\"not a key!\"")).path())
        .unwrap_err();
    assert_eq!(e.code, "E_FILE_FORMAT", "{e}");
    assert!(e.message.contains("not a key!"), "{e}");
}

#[test]
fn lkey1_rdy_check_accepts_a_key_link_target_and_reports_an_unknown_one() {
    let ok = check_path(&pack(), None, tree(&claim("REQ-1")).path()).unwrap();
    assert!(ok.iter().all(|d| d.code != "E_UNKNOWN_REF"), "{ok:?}");
    let bad = check_path(&pack(), None, tree(&claim("NOPE-9")).path()).unwrap();
    assert!(
        bad.iter()
            .any(|d| d.code == "E_UNKNOWN_REF" && d.message.contains("NOPE-9")),
        "{bad:?}"
    );
}

#[test]
fn lkey1_edit_yaml_resolves_a_key_link_target() {
    let (_d, mut r) = project();
    r.import(&approver(), "work", tree(&claim("r-AAAAA")).path())
        .unwrap();
    let c = r
        .read()
        .block(
            &Scope::View("work".into()),
            &BlockId::parse("c-AAAAA").unwrap(),
        )
        .unwrap();
    let mut y = block_to_yaml(&c);
    let at = y.find("links:").expect("the links");
    // an edit may not change the pins, so they follow the new links as they were
    let pins = y[y.find("pins:").expect("the pins")..].to_string();
    y.truncate(at);
    y.push_str("links:\n  describes:\n  - target: REQ-1\n");
    y.push_str(&pins);
    r.edit_yaml(&writer(), "work", &y, None).unwrap();
    let c = r
        .read()
        .block(
            &Scope::View("work".into()),
            &BlockId::parse("c-AAAAA").unwrap(),
        )
        .unwrap();
    assert_eq!(c.links["describes"][0].target.to_string(), "r-AAAAA");
}

#[test]
fn lkey1_a_write_resolves_a_key_link_target() {
    // guard: rdy_write already resolves keys in link targets (Resolver::id)
    let (_d, mut r) = project();
    r.import(&approver(), "work", tree(&claim("r-AAAAA")).path())
        .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-AAAAA", "links": {"describes": [{"target": "REQ-1"}]}}]),
    )
    .unwrap();
    let c = r
        .read()
        .block(
            &Scope::View("work".into()),
            &BlockId::parse("c-AAAAA").unwrap(),
        )
        .unwrap();
    assert_eq!(c.links["describes"][0].target.to_string(), "r-AAAAA");
}

const REQ2: &str = "- id: r-BBBBB\n  type: requirement\n  parent: s-AAAAA\n  order: a3\n  fields:\n    key: REQ-2\n  body: It also must.\n";

fn targets(r: &rdy_engine::Rdy, kind: &str) -> Vec<String> {
    let c = r
        .read()
        .block(
            &Scope::View("work".into()),
            &BlockId::parse("c-AAAAA").unwrap(),
        )
        .unwrap();
    c.links[kind].iter().map(|l| l.target.to_string()).collect()
}

#[test]
fn llst1_a_write_takes_a_string_or_a_list_for_a_link_kind() {
    let (_d, mut r) = project();
    r.import(
        &approver(),
        "work",
        tree(&format!("{REQ2}{}", claim("r-AAAAA"))).path(),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-AAAAA", "links": {"implements": "REQ-1"}}]),
    )
    .unwrap();
    assert_eq!(targets(&r, "implements"), vec!["r-AAAAA"]);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-AAAAA", "links": {"describes": ["REQ-2", {"target": "r-AAAAA", "part": "the rule"}]}}]),
    )
    .unwrap();
    assert_eq!(targets(&r, "describes"), vec!["r-BBBBB", "r-AAAAA"]);
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-AAAAA", "links": {"describes": 7}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_BAD_INPUT, "{e}");
}

#[test]
fn llst1_an_import_takes_a_string_or_a_list_for_a_link_kind() {
    let (_d, mut r) = project();
    let c = "- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: It holds.\n  links:\n    implements: REQ-1\n    describes: [REQ-2, r-AAAAA]\n";
    r.import(&approver(), "work", tree(&format!("{REQ2}{c}")).path())
        .unwrap();
    assert_eq!(targets(&r, "implements"), vec!["r-AAAAA"]);
    assert_eq!(targets(&r, "describes"), vec!["r-BBBBB", "r-AAAAA"]);
}

#[test]
fn llst1_rdy_write_implements_takes_a_key_or_a_list() {
    use rdy_qa::daemon::Served;
    use rdy_qa::mcp::McpClient;
    let s = Served::new();
    {
        let mut r = rdy_engine::Rdy::open(s.dir.path()).unwrap();
        r.import(
            &approver(),
            "work",
            tree(&format!("{REQ2}{}", claim("r-AAAAA"))).path(),
        )
        .unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
    }
    let mut c = McpClient::start_proxy(&s);
    let ok = c.call(
        "rdy_session",
        json!({"actor": "agent:k", "run": "k1", "role": "writer"}),
    );
    assert!(ok.code().is_none());
    let one = c
        .call("rdy_write", json!({"implements": "REQ-1", "ops": [{"op": "update", "id": "c-AAAAA", "body": "One."}]}))
        .ok();
    let many = c
        .call("rdy_write", json!({"implements": ["REQ-2", "r-AAAAA", "REQ-1"], "ops": [{"op": "update", "id": "c-AAAAA", "body": "Two."}]}))
        .ok();
    let empty = c.call(
        "rdy_write",
        json!({"implements": [], "ops": [{"op": "update", "id": "c-AAAAA", "body": "Three."}]}),
    );
    assert_eq!(empty.code(), Some("E_BAD_INPUT"));
    drop(c);
    s.stop();
    let r = rdy_engine::Rdy::open(s.dir.path()).unwrap();
    let just = |v: &serde_json::Value| {
        let h: rdy_model::Hash = v["change"].as_str().unwrap().parse().unwrap();
        r.read().reader().change(&h).unwrap().justification
    };
    let ids = |v: &[&str]| {
        v.iter()
            .map(|s| BlockId::parse(s).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        just(&one),
        Some(rdy_model::Justification::Implements(ids(&["r-AAAAA"])))
    );
    assert_eq!(
        just(&many),
        Some(rdy_model::Justification::Implements(ids(&[
            "r-BBBBB", "r-AAAAA"
        ]))),
        "resolved, deduplicated, in order"
    );
    r.close().unwrap();
}
