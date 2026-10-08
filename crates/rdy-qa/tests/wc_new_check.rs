//! wcnw1: rdy new also writes the new file into a working copy bound to the target view.
//! wcck1: rdy check lints the working copy: new entries need no id, the split rules apply,
//! references resolve in the bound view, untracked files are warnings (spec §12.2).
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::wc::*;
use serde_json::Value;

fn check(s: &Served, args: &[&str]) -> (bool, Value) {
    let mut a = vec!["--json", "check"];
    a.extend_from_slice(args);
    let o = s.rdy(&a);
    let v = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| serde_json::from_slice(&o.stderr).unwrap_or(Value::Null));
    (o.status.success(), v)
}

#[test]
fn wcnw1_rdy_new_writes_the_file_and_its_folders() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    let text = ok(&s, &["new", "doc", "manuals/setup", "--title", "Setup"]);
    assert!(text.contains("wrote manuals/setup.yaml"), "{text}");
    assert!(at(&s, "manuals/_folder.yaml").exists());
    assert_eq!(
        entries(&s, "manuals/setup.yaml")[0]["fields"]["title"].as_str(),
        Some("Setup")
    );
    let (_, v) = json(&s, &["status"]);
    assert!(
        v["working_copy"]["unstaged"].as_array().unwrap().is_empty(),
        "tracked and clean: {v}"
    );
    ok(
        &s,
        &["new", "doc", "elsewhere", "--title", "E", "--view", "other"],
    );
    assert!(!at(&s, "elsewhere.yaml").exists(), "another view: no file");
}

#[test]
fn wcnw1_an_untracked_file_in_the_way_is_kept() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    put(&s, "late.yaml", "mine\n");
    let text = ok(&s, &["new", "doc", "late", "--title", "Late"]);
    assert!(
        text.contains("warning: not written: late.yaml is occupied"),
        "{text}"
    );
    assert_eq!(read(&s, "late.yaml"), "mine\n");
}

#[test]
fn wcck1_check_lints_the_working_copy() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let (okk, v) = check(&s, &[]);
    assert!(okk && v.as_array().unwrap().is_empty(), "{v}");
    let mut text = read(&s, "notes.yaml");
    text.push_str(&format!(
        "- type: prose\n  body: New, no id, see {{{{{}}}}}.\n",
        seed.claim
    ));
    std::fs::write(at(&s, "notes.yaml"), text).unwrap();
    let (okk, v) = check(&s, &[]);
    assert!(
        okk,
        "a new entry and a reference into the view are fine: {v}"
    );
    put(&s, "scratch.yaml", "just: a map\n");
    edit(&s, "guides/intro.yaml", |l| {
        let c = l
            .iter_mut()
            .find(|e| e["id"].as_str() == Some(seed.claim.as_str()))
            .unwrap();
        c.get_mut("links")
            .unwrap()
            .as_mapping_mut()
            .unwrap()
            .insert(
                "decided_by".into(),
                serde_yaml_ng::from_str(&format!("- target: {}", seed.decision)).unwrap(),
            );
    });
    let (okk, v) = check(&s, &[]);
    assert!(!okk, "{v}");
    let codes: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"E_WC_STORE_ONLY") && codes.contains(&"W_WC_UNTRACKED"),
        "{codes:?}"
    );
    let (okk, v) = check(&s, &["notes.yaml"]);
    assert!(okk, "one file in working-copy mode: {v}");
}

#[test]
fn wcck1_outside_a_checkout_a_path_is_required() {
    let (s, _seed) = seeded();
    let (okk, v) = check(&s, &[]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "{v}");
}

/// A directory outside the checkout, with a bad file in it, named through the checkout root and
/// `..`: `Path::starts_with` is lexical, so it must not count as under the root.
#[test]
fn wcck1_a_dotdot_path_out_of_the_checkout_is_checked_where_it_is() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    let outside = rdy_qa::daemon::short_tempdir();
    std::fs::write(outside.path().join("bad.yaml"), "just: a map\n").unwrap();
    let name = outside
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let root = s.dir.path().canonicalize().unwrap();
    let via_root = format!("{}/../{name}", root.display());
    let (okk, v) = check(&s, &[&via_root]);
    assert!(
        v.as_array().is_some_and(|a| a
            .iter()
            .any(|d| d["file"].as_str().is_some_and(|f| f.ends_with("bad.yaml")))),
        "the outside directory is the one checked: {v}"
    );
    assert!(!okk, "its bad file fails the check: {v}");
    // a `..` that stays inside the tree is still the working copy
    let inside = format!("{}/guides/../notes.yaml", root.display());
    let (okk, v) = check(&s, &[&inside]);
    assert!(okk, "{v}");
}

#[test]
fn wcck1_an_untracked_file_names_its_reason_once() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    put(&s, "scratch.yaml", "just: a map\n");
    let (okk, v) = check(&s, &[]);
    assert!(okk, "{v}");
    let d = v
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "W_WC_UNTRACKED")
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(
        d["message"], "scratch.yaml: not a doc file: the first entry is not type: doc",
        "{d}"
    );
    let o = s.rdy(&["add", "scratch.yaml"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert_eq!(err.matches("not a doc file").count(), 1, "{err}");
}

/// A link whose target is written as a question's key is a store-only link like one written as
/// its id (amendment §5.2): rdy check names it, and commit refuses it.
#[test]
fn wcck1_a_link_to_a_question_by_its_key_is_store_only() {
    let (s, seed) = seeded();
    store_write(
        &s,
        "work",
        serde_json::json!([{"op": "create", "type": "question", "parent": seed.section,
            "fields": {"prompt": "Keep it?", "key": "keep-it"}}]),
    );
    ok(&s, &["checkout"]);
    edit(&s, "notes.yaml", |l| {
        let n = l
            .iter_mut()
            .find(|e| e["id"].as_str() == Some(seed.note.as_str()))
            .unwrap();
        n.as_mapping_mut().unwrap().insert(
            "links".into(),
            serde_yaml_ng::from_str("describes:\n- keep-it\n").unwrap(),
        );
    });
    let (okk, v) = check(&s, &[]);
    assert!(!okk, "{v}");
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "E_WC_STORE_ONLY"
                && d["file"] == "notes.yaml"
                && d["message"].as_str().unwrap().contains("keep-it")),
        "{v}"
    );
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "keyed"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    assert!(
        v["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "E_WC_STORE_ONLY" && d["file"] == "notes.yaml"),
        "{v}"
    );
    assert!(
        store_block(&s, "work", &seed.note).links.is_empty(),
        "nothing recorded"
    );
}
