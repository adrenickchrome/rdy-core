//! wcup1: rdy update brings in the view's changes made elsewhere, keeps local edits, merges disjoint
//! ones, writes conflicts beside the file as .theirs.yaml, handles a folder renamed on both sides,
//! and resolution goes through rdy add (spec §12.4, D-WY79P).
use rdy_engine::*;
use rdy_qa::wc::*;
use serde_json::json;

#[test]
fn wcup1_store_changes_are_written_and_local_ones_kept() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Local.");
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.note, "body": "Theirs."},
            {"op": "update", "id": seed.claim, "body": "Theirs claim."}
        ]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert!(v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Theirs.")
    );
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.prose)["body"].as_str(),
        Some("Local."),
        "kept"
    );
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.claim)["body"].as_str(),
        Some("Theirs claim."),
        "merged"
    );
    ok(&s, &["add", "guides/intro.yaml"]);
    ok(&s, &["commit", "-m", "after update"]);
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Local.");
    assert_eq!(store_block(&s, "work", &seed.claim).body, "Theirs claim.");
}

#[test]
fn wcup1_a_block_changed_on_both_sides_is_a_conflict_with_a_theirs_file() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.note, "body": "Theirs."}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(v["conflicts"][0]["path"], "notes.yaml");
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Mine."),
        "local left as it is"
    );
    let theirs: Vec<serde_yaml_ng::Value> =
        serde_yaml_ng::from_str(&read(&s, "notes.theirs.yaml")).unwrap();
    assert!(theirs.iter().any(|e| e["body"].as_str() == Some("Theirs.")));
    let (_, st) = json(&s, &["status"]);
    assert_eq!(
        st["working_copy"]["conflicts"][0]["path"], "notes.yaml",
        "{st}"
    );
    let (okk, v) = json(&s, &["add", "notes.yaml"]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT
            && v["fix"].as_str().unwrap().contains("notes.theirs.yaml"),
        "{v}"
    );
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_CONFLICT, "{v}");
    std::fs::remove_file(at(&s, "notes.theirs.yaml")).unwrap();
    set_body(&s, "notes.yaml", &seed.note, "Resolved.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "resolve"]);
    assert_eq!(store_block(&s, "work", &seed.note).body, "Resolved.");
}

#[test]
fn wcup1_a_folder_renamed_on_both_sides_keeps_the_local_name() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "guides"), at(&s, "manuals")).unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.folder, "fields": {"name": "handbook"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert!(at(&s, "manuals/_folder.yaml").exists() && !at(&s, "handbook").exists());
    assert!(
        at(&s, "manuals/intro.yaml").exists(),
        "the folder's docs stay at the local place"
    );
    let t = read(&s, "manuals/_folder.theirs.yaml");
    assert!(
        t.contains("name: handbook") && t.contains("parent: ''"),
        "{t}"
    );
    let text = ok(&s, &["status"]);
    assert!(
        text.contains("folder renamed here to manuals, in the store to handbook"),
        "{text}"
    );
    std::fs::remove_file(at(&s, "manuals/_folder.theirs.yaml")).unwrap();
    ok(&s, &["add", "manuals"]);
    ok(&s, &["commit", "-m", "keep manuals"]);
    assert_eq!(
        store_block(&s, "work", &seed.folder).field_str("name"),
        Some("manuals"),
        "the name at commit wins"
    );
}

#[test]
fn wcup1_a_staged_file_edited_again_blocks_update() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "A.");
    ok(&s, &["add", "notes.yaml"]);
    set_body(&s, "notes.yaml", &seed.note, "B.");
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.prose, "body": "Theirs."}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(
        !okk && v["code"] == E_WC_DIRTY && v["message"].as_str().unwrap().contains("notes.yaml"),
        "{v}"
    );
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.prose)["body"].as_str(),
        Some("Hello."),
        "nothing written"
    );
}

#[test]
fn wcup1_a_staged_file_merges_and_stays_staged() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Staged edit.");
    ok(&s, &["add", "guides/intro.yaml"]);
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.claim, "body": "Theirs claim."}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    ok(&s, &["commit", "-m", "the staged edit on the new base"]);
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Staged edit.");
    assert_eq!(
        store_block(&s, "work", &seed.claim).body,
        "Theirs claim.",
        "the store's change is not reverted"
    );
}

#[test]
fn wcup1_a_new_store_doc_over_an_untracked_file_is_e_wc_occupied() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    put(&s, "extra.yaml", "just: a map\n");
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": "extra", "title": "Extra", "doc_type": "page"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(!okk && v["code"] == E_WC_OCCUPIED, "{v}");
    assert_eq!(read(&s, "extra.yaml"), "just: a map\n");
}

#[test]
fn wcup1_a_folder_renamed_in_the_store_carries_the_local_edits_along() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Local.");
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.folder, "fields": {"name": "handbook"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    assert!(!at(&s, "guides").exists(), "the old directory goes");
    assert_eq!(
        entry(&s, "handbook/intro.yaml", &seed.prose)["body"].as_str(),
        Some("Local."),
        "the edit moved with its folder"
    );
    assert!(at(&s, "handbook/_folder.yaml").exists());
    ok(&s, &["add", "handbook/intro.yaml"]);
    ok(&s, &["commit", "-m", "the edit under the new name"]);
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Local.");
    assert_eq!(
        store_block(&s, "work", &seed.folder).field_str("name"),
        Some("handbook")
    );
}

#[test]
fn wcup1_a_doc_renamed_on_both_sides_keeps_the_local_name() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "jot.yaml")).unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "memo"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(v["conflicts"][0]["path"], "jot.yaml", "{v}");
    assert!(!at(&s, "memo.yaml").exists() && at(&s, "jot.yaml").exists());
    let t = read(&s, "jot.theirs.yaml");
    assert!(
        t.starts_with("# rdy: in the store this doc is at memo.yaml\n"),
        "{t}"
    );
    let text = ok(&s, &["status"]);
    assert!(
        text.contains("doc renamed here to jot, in the store to memo"),
        "{text}"
    );
    let (okk, v) = json(&s, &["add", "."]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT
            && v["message"].as_str().unwrap().contains("jot.theirs.yaml"),
        "{v}"
    );
    std::fs::remove_file(at(&s, "jot.theirs.yaml")).unwrap();
    ok(&s, &["add", "jot.yaml"]);
    ok(&s, &["commit", "-m", "keep jot"]);
    assert_eq!(
        store_block(&s, "work", &seed.notes).field_str("name"),
        Some("jot")
    );
    assert_eq!(
        store_block(&s, "work", &seed.note).body,
        "Note.",
        "the doc's blocks are kept, not recreated"
    );
}

#[test]
fn wcup1_a_block_new_in_the_store_lands_after_its_sibling_in_a_merged_file() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.claim, "Mine claim.");
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "create", "type": "prose", "id": "$a", "parent": seed.section, "after": seed.prose, "body": "Added."}
        ]),
    );
    let added = w.created["$a"].clone();
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    let ids: Vec<String> = entries(&s, "guides/intro.yaml")
        .iter()
        .map(|e| e["id"].as_str().unwrap_or_default().to_string())
        .collect();
    let at_ = |id: &str| {
        ids.iter()
            .position(|i| i == id)
            .unwrap_or_else(|| panic!("{id} not in {ids:?}"))
    };
    assert_eq!(at_(&added), at_(&seed.prose) + 1, "{ids:?}");
    assert!(at_(&added) < at_(&seed.claim), "{ids:?}");
    ok(&s, &["add", "guides/intro.yaml"]);
    ok(
        &s,
        &["commit", "-m", "claim on top of the store's new prose"],
    );
    assert_eq!(store_block(&s, "work", &seed.claim).body, "Mine claim.");
    assert_eq!(store_block(&s, "work", &added).body, "Added.");
}

/// Adds top-level doc `other` (prose `Other.`) to the store before the checkout; returns its id
/// and its prose's id.
fn with_other(s: &rdy_qa::daemon::Served) -> (String, String) {
    let w = store_write(
        s,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$op", "parent": "$o", "body": "Other."}
        ]),
    );
    (w.created["$o"].clone(), w.created["$op"].clone())
}

fn by_id(a: &str) -> rdy_model::BlockId {
    rdy_model::BlockId::parse(a).unwrap()
}

/// Doc `here` is renamed to c here; the store renames doc `there` to c and edits `here`'s
/// prose. Update refuses with E_WC_CONFLICT before writing anything, whichever id sorts first.
fn two_docs_on_one_path(local_renames_smaller: bool) {
    let (s, seed) = seeded();
    let (other, other_p) = with_other(&s);
    ok(&s, &["checkout"]);
    let mut docs = [
        ("notes", seed.notes.clone(), seed.note.clone()),
        ("other", other, other_p),
    ];
    docs.sort_by_key(|d| by_id(&d.1));
    let (here, there) = if local_renames_smaller {
        (docs[0].clone(), docs[1].clone())
    } else {
        (docs[1].clone(), docs[0].clone())
    };
    let there_file = format!("{}.yaml", there.0);
    std::fs::rename(at(&s, &format!("{}.yaml", here.0)), at(&s, "c.yaml")).unwrap();
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": there.1, "fields": {"name": "c"}},
            {"op": "update", "id": here.2, "body": "Theirs."}
        ]),
    );
    let (c_before, there_before) = (read(&s, "c.yaml"), read(&s, &there_file));
    let (okk, v) = json(&s, &["update"]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT && v["message"].as_str().unwrap().contains("c.yaml"),
        "{v}"
    );
    assert_eq!(read(&s, "c.yaml"), c_before, "nothing written");
    assert_eq!(read(&s, &there_file), there_before, "nothing removed");
    // the fix: another name here, then update again
    std::fs::rename(at(&s, "c.yaml"), at(&s, "mine.yaml")).unwrap();
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    assert!(!at(&s, &there_file).exists());
    assert!(
        entries(&s, "c.yaml")
            .iter()
            .any(|e| e["id"].as_str() == Some(&there.2))
    );
    assert_eq!(
        entry(&s, "mine.yaml", &here.2)["body"].as_str(),
        Some("Theirs.")
    );
}

#[test]
fn wcup1_two_docs_on_one_path_are_refused_when_the_local_one_sorts_first() {
    two_docs_on_one_path(true);
}

#[test]
fn wcup1_two_docs_on_one_path_are_refused_when_the_store_one_sorts_first() {
    two_docs_on_one_path(false);
}

/// Folder `here` is renamed to z here; the store renames folder `there` to z and retitles
/// `here`. Update refuses with E_WC_CONFLICT before writing anything, whichever id sorts first.
fn two_folders_on_one_path(local_renames_smaller: bool) {
    let (s, seed) = seeded();
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "extra", "title": "Extra"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "ex", "title": "Ex", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "Ex."}
        ]),
    );
    ok(&s, &["checkout"]);
    let mut folders = [
        ("guides", seed.folder.clone()),
        ("extra", w.created["$f"].clone()),
    ];
    folders.sort_by_key(|f| by_id(&f.1));
    let (here, there) = if local_renames_smaller {
        (folders[0].clone(), folders[1].clone())
    } else {
        (folders[1].clone(), folders[0].clone())
    };
    std::fs::rename(at(&s, here.0), at(&s, "z")).unwrap();
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": there.1, "fields": {"name": "z"}},
            {"op": "update", "id": here.1, "fields": {"title": "Retitled"}}
        ]),
    );
    let z_before = read(&s, "z/_folder.yaml");
    let there_before = read(&s, &format!("{}/_folder.yaml", there.0));
    let (okk, v) = json(&s, &["update"]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT
            && v["message"].as_str().unwrap().contains("z/_folder.yaml"),
        "{v}"
    );
    assert_eq!(read(&s, "z/_folder.yaml"), z_before, "nothing written");
    assert_eq!(
        read(&s, &format!("{}/_folder.yaml", there.0)),
        there_before,
        "nothing removed"
    );
}

#[test]
fn wcup1_two_folders_on_one_path_are_refused_when_the_local_one_sorts_first() {
    two_folders_on_one_path(true);
}

#[test]
fn wcup1_two_folders_on_one_path_are_refused_when_the_store_one_sorts_first() {
    two_folders_on_one_path(false);
}

#[test]
fn wcup1_a_path_one_doc_leaves_and_another_takes_is_not_occupied() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.notes, "fields": {"name": "old"}},
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "New notes", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$n", "body": "New."}
        ]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(
        entry(&s, "old.yaml", &seed.note)["body"].as_str(),
        Some("Note.")
    );
    assert_eq!(
        entry(&s, "notes.yaml", &w.created["$p"])["body"].as_str(),
        Some("New.")
    );
    let (okk, v) = json(&s, &["add", "."]);
    assert!(okk, "{v}");
    assert!(
        v["staged"].as_array().unwrap().is_empty() && v["deleted"].as_array().unwrap().is_empty(),
        "nothing to commit after the update: {v}"
    );
}

#[test]
fn wcup1_two_docs_swapping_names_in_the_store_update_cleanly() {
    let (s, seed) = seeded();
    let (other, other_p) = with_other(&s);
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.notes, "fields": {"name": "swapping"}},
            {"op": "update", "id": other, "fields": {"name": "notes"}}
        ]),
    );
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "other"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(
        entry(&s, "other.yaml", &seed.note)["body"].as_str(),
        Some("Note.")
    );
    assert_eq!(
        entry(&s, "notes.yaml", &other_p)["body"].as_str(),
        Some("Other.")
    );
    let (okk, v) = json(&s, &["add", "."]);
    assert!(okk, "{v}");
    assert!(
        v["staged"].as_array().unwrap().is_empty() && v["deleted"].as_array().unwrap().is_empty(),
        "nothing to commit after the update: {v}"
    );
}

#[test]
fn wcup1_a_folder_the_store_deleted_stays_while_a_doc_in_it_is_edited_here() {
    let (s, _seed) = seeded();
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "old", "title": "Old"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "d", "title": "D", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "D."}
        ]),
    );
    let (f, d, p) = (
        w.created["$f"].clone(),
        w.created["$d"].clone(),
        w.created["$p"].clone(),
    );
    ok(&s, &["checkout"]);
    set_body(&s, "old/d.yaml", &p, "Mine.");
    store_write(
        &s,
        "work",
        json!([
            {"op": "delete", "id": p},
            {"op": "delete", "id": d},
            {"op": "delete", "id": f}
        ]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    let paths: Vec<&str> = v["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["old/_folder.yaml", "old/d.yaml"], "{v}");
    assert!(
        at(&s, "old/_folder.yaml").exists(),
        "the folder of a kept doc stays"
    );
    assert_eq!(entry(&s, "old/d.yaml", &p)["body"].as_str(), Some("Mine."));
    // resolution goes through rdy add
    std::fs::remove_file(at(&s, "old/_folder.theirs.yaml")).unwrap();
    std::fs::remove_file(at(&s, "old/d.theirs.yaml")).unwrap();
    ok(&s, &["add", "old"]);
    let (_, st) = json(&s, &["status"]);
    assert!(
        st["working_copy"]["conflicts"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{st}"
    );
    // the commit records the kept folder and doc again
    ok(&s, &["commit", "-m", "keep old"]);
    let kept = entries(&s, "old/d.yaml")
        .into_iter()
        .find(|e| e["body"].as_str() == Some("Mine."))
        .expect("the edited block");
    assert_eq!(
        store_block(&s, "work", kept["id"].as_str().unwrap()).body,
        "Mine."
    );
}

#[test]
fn wcup1_an_update_that_fails_before_changing_a_file_leaves_no_marker() {
    use std::os::unix::fs::PermissionsExt;
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "My precious local edit.");
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "doc", "parent": seed.folder, "fields": {"name": "zz", "title": "Zz", "doc_type": "page"}}]),
    );
    let mode = |m: u32| {
        std::fs::set_permissions(at(&s, "guides"), std::fs::Permissions::from_mode(m)).unwrap()
    };
    mode(0o555);
    let (okk, v) = json(&s, &["update"]);
    mode(0o755);
    assert!(!okk && v["code"] == E_IO, "{v}");
    ok(&s, &["status"]);
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert!(at(&s, "guides/zz.yaml").exists());
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("My precious local edit.")
    );
}

#[test]
fn wcup1_an_interrupted_update_is_finished_by_rdy_update() {
    use std::os::unix::fs::PermissionsExt;
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Local.");
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.claim, "body": "Theirs claim."},
            {"op": "update", "id": seed.note, "body": "Theirs."}
        ]),
    );
    // guides/intro.yaml is written first; notes.yaml then fails in the read-only root
    let root = s.dir.path().to_path_buf();
    let mode =
        |m: u32| std::fs::set_permissions(&root, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o555);
    let (okk, v) = json(&s, &["update"]);
    mode(0o755);
    assert!(!okk && v["code"] == E_IO, "{v}");
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.claim)["body"].as_str(),
        Some("Theirs claim."),
        "the update got part way"
    );
    // status still shows the store, and the interrupted update under the working copy
    let (okk, v) = json(&s, &["status"]);
    let i = &v["working_copy"]["interrupted"];
    assert!(
        okk && v["views"].is_array()
            && i["code"] == E_WC_DIRTY
            && i["message"].as_str().unwrap().contains("update")
            && i["fix"].as_str().unwrap().starts_with("rdy update"),
        "{v}"
    );
    let text = ok(&s, &["status"]);
    assert!(
        text.contains("working copy: an update of work was interrupted")
            && text.contains("fix: rdy update"),
        "{text}"
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.prose)["body"].as_str(),
        Some("Local."),
        "the local edit is kept"
    );
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Theirs.")
    );
    ok(&s, &["status"]);
    ok(&s, &["add", "guides/intro.yaml"]);
    ok(&s, &["commit", "-m", "after the finished update"]);
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Local.");
    assert_eq!(store_block(&s, "work", &seed.claim).body, "Theirs claim.");
    assert_eq!(store_block(&s, "work", &seed.note).body, "Theirs.");
}

#[test]
fn wcup1_a_conflicted_file_takes_the_store_changes_that_do_not_conflict() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Mine.");
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.prose, "body": "Theirs."},
            {"op": "update", "id": seed.claim, "body": "Theirs claim."},
            {"op": "create", "type": "prose", "id": "$a", "parent": seed.section, "body": "Added by them."}
        ]),
    );
    let added = w.created["$a"].clone();
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(v["conflicts"][0]["path"], "guides/intro.yaml", "{v}");
    let f = "guides/intro.yaml";
    assert_eq!(
        entry(&s, f, &seed.prose)["body"].as_str(),
        Some("Mine."),
        "the conflicting block stays local"
    );
    assert_eq!(
        entry(&s, f, &seed.claim)["body"].as_str(),
        Some("Theirs claim.")
    );
    assert_eq!(
        entry(&s, f, &added)["body"].as_str(),
        Some("Added by them.")
    );
    std::fs::remove_file(at(&s, "guides/intro.theirs.yaml")).unwrap();
    ok(&s, &["add", f]);
    ok(&s, &["commit", "-m", "keep mine"]);
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Mine.");
    assert_eq!(
        store_block(&s, "work", &seed.claim).body,
        "Theirs claim.",
        "not reverted"
    );
    assert_eq!(
        store_block(&s, "work", &added).body,
        "Added by them.",
        "not deleted"
    );
}

#[test]
fn wcup1_a_case_only_rename_in_the_store_takes_its_spelling_on_disk() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "Notes"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    let names: Vec<String> = std::fs::read_dir(at(&s, ""))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase() == "notes.yaml")
        .collect();
    assert_eq!(names, ["Notes.yaml"], "the store's spelling is on disk");
    assert_eq!(
        entry(&s, "Notes.yaml", &seed.note)["body"].as_str(),
        Some("Note.")
    );
    let (okk, v) = json(&s, &["add", "."]);
    assert!(okk, "{v}");
    assert!(
        v["staged"].as_array().unwrap().is_empty() && v["deleted"].as_array().unwrap().is_empty(),
        "the update leaves nothing to commit, so the store's rename is not reverted: {v}"
    );
}

/// How many entries of `rel` hold `body`.
fn with_body(s: &rdy_qa::daemon::Served, rel: &str, body: &str) -> usize {
    entries(s, rel)
        .iter()
        .filter(|e| e["body"].as_str() == Some(body))
        .count()
}

/// A kept file (committed, but not written back) is turned by rdy update into a conflict: the
/// store's side beside it, the base moved to the store's, so no edit discards the others and the
/// block the commit created is never recorded twice.
#[test]
fn wcup1_update_turns_a_kept_file_into_a_conflict_beside_it() {
    use rdy_engine::wc::state::*;
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let orig = read(&s, "notes.yaml");
    put(
        &s,
        "notes.yaml",
        &format!("{orig}- type: prose\n  body: Added once.\n"),
    );
    ok(&s, &["add", "notes.yaml"]);
    // a lost acknowledgement of the create
    let place = Place::locate(s.dir.path()).unwrap();
    let mut st = WcState::load(&place).unwrap();
    let nonce = new_nonce();
    st.pending = Some(Pending {
        nonce: nonce.clone(),
        view: "work".into(),
        assigned: vec![],
        rewrite: vec![rdy_model::BlockId::parse(&seed.notes).unwrap()],
        files: vec!["notes.yaml".into()],
        at_ms: 1,
    });
    st.save(&place).unwrap();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let ops = serde_json::from_value(json!([
        {"op": "create", "type": "prose", "parent": seed.notes, "after": seed.note, "body": "Added once."}
    ]))
    .unwrap();
    r.write_linked(
        &rdy_qa::fixtures::writer(),
        WriteRequest {
            view: "work".into(),
            ops,
            justification: None,
            message: "lost".into(),
        },
        rdy_model::ChangeLinks {
            client_nonce: Some(nonce),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    r.close().unwrap();
    // a later edit, so recovery keeps the file
    let mut text = read(&s, "notes.yaml");
    text.push_str("# a later comment\n");
    put(&s, "notes.yaml", &text);
    // an edit elsewhere that the way out must not discard
    set_body(&s, "guides/intro.yaml", &seed.prose, "Mine, elsewhere.");
    let (_, st) = json(&s, &["status"]);
    assert_eq!(st["working_copy"]["conflicts"][0]["kind"], "kept", "{st}");
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(v["conflicts"][0]["path"], "notes.yaml", "{v}");
    assert_eq!(v["conflicts"][0]["kind"], "blocks", "{v}");
    assert_eq!(
        read(&s, "notes.yaml"),
        text,
        "the local file stays as it is"
    );
    assert_eq!(with_body(&s, "notes.theirs.yaml", "Added once."), 1);
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.prose)["body"],
        "Mine, elsewhere.",
        "the other edit stays"
    );
    // resolved by taking the store's side
    put(&s, "notes.yaml", &read(&s, "notes.theirs.yaml"));
    std::fs::remove_file(at(&s, "notes.theirs.yaml")).unwrap();
    ok(&s, &["add", "notes.yaml", "guides/intro.yaml"]);
    ok(&s, &["commit", "-m", "resolved"]);
    assert_eq!(
        with_body(&s, "notes.yaml", "Added once."),
        1,
        "{}",
        read(&s, "notes.yaml")
    );
    assert_eq!(
        store_block(&s, "work", &seed.prose).body,
        "Mine, elsewhere."
    );
    let (_, st) = json(&s, &["status"]);
    for k in ["conflicts", "staged", "unstaged"] {
        assert!(st["working_copy"][k].as_array().unwrap().is_empty(), "{st}");
    }
}

/// A kept rename (the store's path held an untracked file): update asks for the file in the way
/// first, then moves the doc to the store's path with the store's side beside it.
#[test]
fn wcup1_update_of_a_kept_rename_asks_for_the_file_in_the_way_first() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let memo = "my own memo, never tracked\n";
    put(&s, "memo.yaml", memo);
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "memo"}}]),
    );
    let orig = read(&s, "notes.yaml");
    put(
        &s,
        "notes.yaml",
        &format!("{orig}- type: prose\n  body: Added once.\n"),
    );
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(okk && v["kept"][0]["file"] == "memo.yaml", "{v}");
    let (okk, v) = json(&s, &["update"]);
    assert!(
        !okk && v["code"] == E_WC_OCCUPIED && v["message"].as_str().unwrap().contains("memo.yaml"),
        "{v}"
    );
    assert_eq!(read(&s, "memo.yaml"), memo);
    std::fs::rename(at(&s, "memo.yaml"), at(&s, "memo.txt")).unwrap();
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(v["conflicts"][0]["path"], "memo.yaml", "{v}");
    assert!(!at(&s, "notes.yaml").exists());
    assert_eq!(with_body(&s, "memo.theirs.yaml", "Added once."), 1);
    put(&s, "memo.yaml", &read(&s, "memo.theirs.yaml"));
    std::fs::remove_file(at(&s, "memo.theirs.yaml")).unwrap();
    // the file now equals its base: adding it resolves the conflict and stages nothing
    ok(&s, &["add", "memo.yaml"]);
    assert_eq!(with_body(&s, "memo.yaml", "Added once."), 1);
    assert_eq!(read(&s, "memo.txt"), memo);
    let (_, st) = json(&s, &["status"]);
    for k in ["conflicts", "staged", "unstaged"] {
        assert!(st["working_copy"][k].as_array().unwrap().is_empty(), "{st}");
    }
}

#[test]
fn msw5_finishing_an_update_keeps_a_file_where_a_new_folder_goes() {
    use std::os::unix::fs::PermissionsExt;
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.claim, "body": "Theirs claim."},
            {"op": "update", "id": seed.note, "body": "Theirs."},
            {"op": "create", "type": "folder", "fields": {"name": "zfold", "title": "Zfold"}}
        ]),
    );
    // guides/intro.yaml is written, notes.yaml fails in the read-only root, zfold/ is not reached
    let root = s.dir.path().to_path_buf();
    let mode =
        |m: u32| std::fs::set_permissions(&root, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o555);
    // as root a read-only directory still takes writes: the update cannot be stopped here
    let probe = root.join(".msw5-probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        mode(0o755);
        eprintln!("msw5: skipped, a read-only directory takes writes here (running as root?)");
        return;
    }
    let (okk, v) = json(&s, &["update"]);
    mode(0o755);
    assert!(!okk && v["code"] == E_IO, "{v}");
    assert!(!at(&s, "zfold").exists(), "stopped before the new folder");
    put(&s, "zfold", "mine\n");
    let (okk, v) = json(&s, &["update"]);
    assert!(
        std::fs::symlink_metadata(at(&s, "zfold"))
            .unwrap()
            .is_file(),
        "the untracked file was replaced by a directory: {v}"
    );
    assert_eq!(read(&s, "zfold"), "mine\n");
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
}

/// Stops an update midway, as `msw5_finishing_an_update_keeps_a_file_where_a_new_folder_goes`
/// does: the claim and the note change with `extra`, the root goes read-only, so
/// `guides/intro.yaml` is written and `notes.yaml` fails. False when a read-only directory
/// still takes writes (running as root): the caller skips.
fn msw5_interrupt(s: &rdy_qa::daemon::Served, seed: &Seed, extra: &[serde_json::Value]) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mut ops = vec![
        json!({"op": "update", "id": seed.claim, "body": "Theirs claim."}),
        json!({"op": "update", "id": seed.note, "body": "Theirs."}),
    ];
    ops.extend_from_slice(extra);
    store_write(s, "work", serde_json::Value::Array(ops));
    let root = s.dir.path().to_path_buf();
    let mode =
        |m: u32| std::fs::set_permissions(&root, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o555);
    let probe = root.join(".msw5-probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        mode(0o755);
        eprintln!("msw5: skipped, a read-only directory takes writes here (running as root?)");
        return false;
    }
    let (okk, v) = json(s, &["update"]);
    mode(0o755);
    assert!(!okk && v["code"] == E_IO, "{v}");
    true
}

#[test]
fn msw5_finishing_an_update_keeps_a_link_where_a_new_doc_goes() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let doc = json!({"op": "create", "type": "doc", "fields": {"name": "zdoc", "title": "Zdoc", "doc_type": "page"}});
    if !msw5_interrupt(&s, &seed, &[doc]) {
        return;
    }
    assert!(!at(&s, "zdoc.yaml").exists(), "stopped before the new doc");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("mine.yaml");
    std::fs::write(&target, "mine\n").unwrap();
    std::os::unix::fs::symlink(&target, at(&s, "zdoc.yaml")).unwrap();
    let (okk, v) = json(&s, &["update"]);
    assert!(
        std::fs::symlink_metadata(at(&s, "zdoc.yaml"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link was replaced by a file: {v}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine\n");
    assert!(
        !okk && v["code"] == E_WC_DIRTY && v["message"].as_str().unwrap().contains("zdoc.yaml"),
        "{v}"
    );
}

#[test]
fn msw5_finishing_an_update_names_the_file_where_a_new_folder_goes() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let folder =
        json!({"op": "create", "type": "folder", "fields": {"name": "zfold", "title": "Zfold"}});
    if !msw5_interrupt(&s, &seed, &[folder]) {
        return;
    }
    put(&s, "zfold", "mine\n");
    let (okk, v) = json(&s, &["update"]);
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
    assert_eq!(
        v["message"], "zfold is a file where the update writes zfold/_folder.yaml",
        "{v}"
    );
    assert_eq!(
        v["fix"], "move zfold aside, then run rdy update again to finish the update",
        "{v}"
    );
    // the fix works: with the file moved aside the update finishes
    std::fs::rename(at(&s, "zfold"), at(&s, "zfold.mine")).unwrap();
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert!(at(&s, "zfold/_folder.yaml").is_file(), "{v}");
    assert_eq!(read(&s, "zfold.mine"), "mine\n");
}

#[test]
fn msw5_finishing_an_update_keeps_a_directory_or_link_where_a_removed_doc_was() {
    let (s, seed) = seeded();
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "zgone", "title": "Zgone", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "zlink", "title": "Zlink", "doc_type": "page"}}
        ]),
    );
    ok(&s, &["checkout"]);
    assert!(at(&s, "zgone.yaml").is_file() && at(&s, "zlink.yaml").is_file());
    let gone = [
        json!({"op": "delete", "id": w.created["$g"]}),
        json!({"op": "delete", "id": w.created["$l"]}),
    ];
    // the removes come after the writes: the update stops before it reaches them
    if !msw5_interrupt(&s, &seed, &gone) {
        return;
    }
    // a directory and a link where the update removes a file: it leaves them, as a fresh
    // update does
    std::fs::remove_file(at(&s, "zgone.yaml")).unwrap();
    std::fs::create_dir(at(&s, "zgone.yaml")).unwrap();
    put(&s, "zgone.yaml/mine", "mine\n");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("mine.yaml");
    std::fs::write(&target, "mine\n").unwrap();
    std::fs::remove_file(at(&s, "zlink.yaml")).unwrap();
    std::os::unix::fs::symlink(&target, at(&s, "zlink.yaml")).unwrap();
    let (okk, v) = json(&s, &["update"]);
    assert!(okk, "{v}");
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Theirs.")
    );
    assert!(
        std::fs::symlink_metadata(at(&s, "zgone.yaml"))
            .unwrap()
            .is_dir(),
        "{v}"
    );
    assert_eq!(read(&s, "zgone.yaml/mine"), "mine\n");
    assert!(
        std::fs::symlink_metadata(at(&s, "zlink.yaml"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "{v}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine\n");
}

#[test]
fn msw16_a_rename_conflict_resolves_by_taking_the_store_name() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "jot.yaml")).unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "memo"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"][0]["path"] == "jot.yaml", "{v}");
    std::fs::remove_file(at(&s, "jot.theirs.yaml")).unwrap();
    std::fs::rename(at(&s, "jot.yaml"), at(&s, "memo.yaml")).unwrap();
    ok(&s, &["add", "memo.yaml"]);
    let (_, v) = json(&s, &["commit", "-m", "take memo"]);
    assert!(
        v["code"] != E_WC_CONFLICT,
        "the conflict is still open: {v}"
    );
    assert_eq!(
        store_block(&s, "work", &seed.notes).field_str("name"),
        Some("memo")
    );
}

/// msw16 fix round 1: the store's name counts only when the tree holds the doc there. An
/// unrelated `rdy add` of the store's directory neither trips on the conflict nor resolves it.
#[test]
fn msw16_the_store_name_counts_only_when_the_doc_is_there() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "jot.yaml")).unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "move", "id": seed.notes, "parent": seed.folder}]),
    );
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "memo"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"][0]["path"] == "jot.yaml", "{v}");
    set_body(&s, "guides/intro.yaml", &seed.prose, "Unrelated.");
    let (okk, v) = json(&s, &["add", "guides"]);
    assert!(okk, "an unrelated add is refused: {v}");
    assert!(v["resolved"].as_array().unwrap().is_empty(), "{v}");
    std::fs::remove_file(at(&s, "jot.theirs.yaml")).unwrap();
    let (okk, v) = json(&s, &["add", "guides"]);
    assert!(okk && v["resolved"].as_array().unwrap().is_empty(), "{v}");
    let (okk, v) = json(&s, &["commit", "-m", "too early"]);
    assert!(!okk && v["code"] == E_WC_CONFLICT, "{v}");
    ok(&s, &["add", "jot.yaml"]);
    ok(&s, &["commit", "-m", "keep jot"]);
    assert_eq!(
        store_block(&s, "work", &seed.notes).field_str("name"),
        Some("jot")
    );
    assert_eq!(store_block(&s, "work", &seed.note).body, "Note.");
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Unrelated.");
}

/// msw16 fix round 1: renamed and edited on both sides gives two conflicts at the local path;
/// taking the store's name resolves both.
#[test]
fn msw16_a_doc_renamed_and_edited_on_both_sides_resolves_under_the_store_name() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "jot.yaml")).unwrap();
    set_body(&s, "jot.yaml", &seed.note, "Mine.");
    store_write(
        &s,
        "work",
        json!([
            {"op": "update", "id": seed.notes, "fields": {"name": "memo"}},
            {"op": "update", "id": seed.note, "body": "Theirs."}
        ]),
    );
    let (okk, v) = json(&s, &["update"]);
    let c = v["conflicts"].as_array().unwrap();
    assert!(
        okk && c.len() == 2 && c.iter().all(|c| c["path"] == "jot.yaml"),
        "{v}"
    );
    std::fs::remove_file(at(&s, "jot.theirs.yaml")).unwrap();
    set_body(&s, "jot.yaml", &seed.note, "Resolved.");
    std::fs::rename(at(&s, "jot.yaml"), at(&s, "memo.yaml")).unwrap();
    let (okk, v) = json(&s, &["add", "memo.yaml"]);
    assert!(okk && v["resolved"].as_array().unwrap().len() == 2, "{v}");
    ok(&s, &["commit", "-m", "take memo"]);
    assert_eq!(
        store_block(&s, "work", &seed.notes).field_str("name"),
        Some("memo")
    );
    assert_eq!(store_block(&s, "work", &seed.note).body, "Resolved.");
}

/// msw16 fix round 1: a folder renamed on both sides, taken under the store's name: the
/// `_folder.theirs.yaml` moved with the directory and still blocks.
#[test]
fn msw16_a_folder_taken_under_the_store_name_waits_for_its_moved_theirs_file() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "guides"), at(&s, "manuals")).unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.folder, "fields": {"name": "handbook"}}]),
    );
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"][0]["path"] == "manuals", "{v}");
    std::fs::rename(at(&s, "manuals"), at(&s, "handbook")).unwrap();
    assert!(at(&s, "handbook/_folder.theirs.yaml").exists());
    let (okk, v) = json(&s, &["add", "handbook"]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT
            && v["message"]
                .as_str()
                .unwrap()
                .contains("handbook/_folder.theirs.yaml"),
        "{v}"
    );
    std::fs::remove_file(at(&s, "handbook/_folder.theirs.yaml")).unwrap();
    let (okk, v) = json(&s, &["add", "handbook"]);
    assert!(okk && v["resolved"] == json!(["manuals"]), "{v}");
    // the files now match the store, so nothing is left to commit
    let (okk, v) = json(&s, &["commit", "-m", "take handbook"]);
    assert!(!okk && v["code"] == E_BAD_INPUT, "{v}");
    let (okk, v) = json(&s, &["update"]);
    assert!(okk && v["conflicts"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(
        store_block(&s, "work", &seed.folder).field_str("name"),
        Some("handbook")
    );
}
