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
