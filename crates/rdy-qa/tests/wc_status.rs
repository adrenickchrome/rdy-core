//! wcss1: rdy status lists the working copy's changes, staged or not, untracked files with a lint
//! hint, and conflicts (spec §12.2). wcad1: rdy add stages files, directories, deletions and renames.
use rdy_engine::*;
use rdy_qa::wc::*;
use serde_json::Value;

fn changes(list: &Value) -> Vec<(String, String)> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["change"].as_str().unwrap().to_string(),
                e["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn wcss1_status_outside_a_checkout_has_no_working_copy_part() {
    let (s, _seed) = seeded();
    let (okk, v) = json(&s, &["status"]);
    assert!(okk && v.get("working_copy").is_none(), "{v}");
}

#[test]
fn wcss1_status_lists_modified_added_renamed_and_untracked() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let (okk, v) = json(&s, &["status"]);
    assert!(okk, "{v}");
    assert_eq!(v["working_copy"]["view"], "work");
    assert!(
        v["working_copy"]["unstaged"].as_array().unwrap().is_empty(),
        "{v}"
    );
    assert!(ok(&s, &["status"]).contains("\nworking copy bound to work, clean"));
    set_body(&s, "guides/intro.yaml", &seed.prose, "Changed.");
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "journal.yaml")).unwrap();
    put(
        &s,
        "fresh.yaml",
        "- type: doc\n  fields:\n    title: Fresh\n    doc_type: page\n",
    );
    put(&s, "scratch.yaml", "just: a map\n");
    let (_, v) = json(&s, &["status"]);
    let un = changes(&v["working_copy"]["unstaged"]);
    assert!(
        un.contains(&("modified".into(), "guides/intro.yaml".into())),
        "{un:?}"
    );
    assert!(
        un.contains(&("renamed".into(), "journal.yaml".into())),
        "{un:?}"
    );
    assert!(
        un.contains(&("added".into(), "fresh.yaml".into())),
        "{un:?}"
    );
    let renamed = v["working_copy"]["unstaged"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["change"] == "renamed")
        .unwrap();
    assert_eq!(renamed["from"], "notes.yaml");
    assert_eq!(v["working_copy"]["untracked"][0]["path"], "scratch.yaml");
    assert_eq!(
        v["working_copy"]["untracked"][0]["hint"],
        "rdy check scratch.yaml"
    );
    let text = ok(&s, &["status"]);
    assert!(
        text.contains("\nworking copy bound to work\nnot staged:\n"),
        "{text}"
    );
    assert!(text.contains("\n  modified  guides/intro.yaml"), "{text}");
    assert!(
        text.contains("\n  renamed   notes.yaml -> journal.yaml"),
        "{text}"
    );
    assert!(
        text.contains("\nuntracked (check with rdy check <path>):\n  scratch.yaml: "),
        "{text}"
    );
}

#[test]
fn wcad1_add_stages_files_dirs_and_deletions() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Changed.");
    std::fs::remove_file(at(&s, "notes.yaml")).unwrap();
    let (okk, v) = json(&s, &["add", "guides"]);
    assert!(okk, "{v}");
    assert_eq!(v["staged"], serde_json::json!(["guides/intro.yaml"]));
    let (okk, v) = json(&s, &["add", "notes.yaml"]);
    assert!(okk, "{v}");
    assert_eq!(v["deleted"], serde_json::json!(["notes.yaml"]));
    let (_, v) = json(&s, &["status"]);
    let st = changes(&v["working_copy"]["staged"]);
    assert!(
        st.contains(&("modified".into(), "guides/intro.yaml".into())),
        "{st:?}"
    );
    assert!(
        st.contains(&("deleted".into(), "notes.yaml".into())),
        "{st:?}"
    );
    assert!(
        v["working_copy"]["unstaged"].as_array().unwrap().is_empty(),
        "{v}"
    );
    set_body(&s, "guides/intro.yaml", &seed.prose, "Changed again.");
    let (_, v) = json(&s, &["status"]);
    assert!(
        changes(&v["working_copy"]["unstaged"])
            .contains(&("modified".into(), "guides/intro.yaml".into())),
        "modified after staging: {v}"
    );
}

#[test]
fn wcad1_add_refuses_what_is_not_in_the_tree_and_stages_nothing() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    put(&s, "README.md", "hi\n");
    put(&s, "scratch.yaml", "just: a map\n");
    put(&s, "notes.theirs.yaml", "x\n");
    let (okk, v) = json(&s, &["add", "notes.yaml", "README.md"]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "{v}");
    let (_, st) = json(&s, &["status"]);
    assert!(
        st["working_copy"]["staged"].as_array().unwrap().is_empty(),
        "one refusal stages nothing: {st}"
    );
    let (okk, v) = json(&s, &["add", "scratch.yaml"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    let (okk, v) = json(&s, &["add", "notes.theirs.yaml"]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "{v}");
    let (okk, v) = json(&s, &["add", "missing.yaml"]);
    assert!(!okk && v["code"] == E_NOT_FOUND, "{v}");
}

#[test]
fn wcad1_add_outside_a_checkout_is_e_wc_not_bound() {
    let (s, _seed) = seeded();
    let (okk, v) = json(&s, &["add", "."]);
    assert!(!okk && v["code"] == E_WC_NOT_BOUND, "{v}");
}

#[test]
fn wcad1_two_adds_at_once_both_land() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "A.");
    set_body(&s, "notes.yaml", &seed.note, "B.");
    let a = s.spawn(&["add", "guides/intro.yaml"]);
    let b = s.spawn(&["add", "notes.yaml"]);
    let (oa, ob) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    assert!(
        oa.status.success() && ob.status.success(),
        "{} {}",
        String::from_utf8_lossy(&oa.stderr),
        String::from_utf8_lossy(&ob.stderr)
    );
    let (_, v) = json(&s, &["status"]);
    assert_eq!(
        v["working_copy"]["staged"].as_array().unwrap().len(),
        2,
        "{v}"
    );
}

#[test]
fn wcss1_a_staged_rename_and_move_are_one_entry_each_and_a_folder_has_a_slash() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "journal.yaml")).unwrap();
    std::fs::rename(at(&s, "guides/intro.yaml"), at(&s, "intro.yaml")).unwrap();
    let (okk, v) = json(&s, &["add", "."]);
    assert!(okk, "{v}");
    let (_, v) = json(&s, &["status"]);
    let w = &v["working_copy"];
    assert!(w["unstaged"].as_array().unwrap().is_empty(), "{v}");
    let st = changes(&w["staged"]);
    assert_eq!(st.len(), 2, "a rename and a move, no deletions: {st:?}");
    assert!(
        st.contains(&("renamed".into(), "journal.yaml".into())),
        "{st:?}"
    );
    assert!(
        st.contains(&("moved".into(), "intro.yaml".into())),
        "{st:?}"
    );
    // the folder's file is listed by its directory
    ok(&s, &["checkout", "--force"]);
    std::fs::remove_file(at(&s, "journal.yaml")).unwrap();
    std::fs::remove_file(at(&s, "intro.yaml")).unwrap();
    std::fs::write(
        at(&s, "guides/_folder.yaml"),
        read(&s, "guides/_folder.yaml").replace("Guides", "Guides!"),
    )
    .unwrap();
    let (_, v) = json(&s, &["status"]);
    let un = changes(&v["working_copy"]["unstaged"]);
    assert_eq!(
        un,
        vec![("modified".to_string(), "guides/".to_string())],
        "{v}"
    );
}
