//! wcco1: rdy checkout writes the view as files with the metadata split (MS, FW-D10, FW-D14,
//! FW-F2a), refuses over edits and untracked files, writes blobs, and the checked-out tree reads
//! back as the view (invariant 7). The bound view is the CLI default view (D-874AS).
use rdy_engine::*;
use rdy_qa::wc::*;
use serde_json::json;

#[test]
fn wcco1_checkout_writes_folders_docs_and_the_split() {
    let (s, seed) = seeded();
    let (okk, v) = json(&s, &["checkout"]);
    assert!(okk, "{v}");
    assert_eq!(v["view"], "work");
    assert_eq!(v["docs"], 2);
    assert_eq!(v["folders"], 1);
    let folder = read(&s, "guides/_folder.yaml");
    assert!(
        folder.contains(&format!("id: {}", seed.folder)) && !folder.contains("name:"),
        "{folder}"
    );
    let claim = entry(&s, "guides/intro.yaml", &seed.claim);
    assert_eq!(
        claim["links"]["describes"][0]["target"].as_str(),
        Some(seed.prose.as_str()),
        "{claim:?}"
    );
    assert!(
        claim["links"].get("decided_by").is_none(),
        "a store-only kind: {claim:?}"
    );
    assert!(
        claim.get("pins").is_none(),
        "pins are store-only: {claim:?}"
    );
    let text = read(&s, "guides/intro.yaml");
    assert!(
        !text.contains(&seed.decision),
        "supporting blocks stay out: {text}"
    );
    assert_eq!(entries(&s, "notes.yaml").len(), 2);
    assert!(at(&s, ".rdy/wc/bound.yaml").exists());
    let out = ok(&s, &["checkout", "--force"]);
    assert!(out.starts_with("checked out work at "), "{out}");
}

#[test]
fn wcco1_key_typed_refs_are_written_as_keys_and_id_refs_as_ids() {
    let (s, seed) = seeded();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.note, "fields": {"key": "NOTE-1"}}]),
    );
    let id_ref = format!("See {{{{{}}}}}.", seed.note);
    let w = store_write(
        &s,
        "work",
        json!([
            {"op": "create", "type": "prose", "id": "$k", "parent": seed.section, "body": "See {{NOTE-1}}."},
            {"op": "create", "type": "prose", "id": "$i", "parent": seed.section, "body": id_ref}
        ]),
    );
    ok(&s, &["checkout"]);
    assert_eq!(
        entry(&s, "guides/intro.yaml", &w.created["$k"])["body"].as_str(),
        Some("See {{NOTE-1}}.")
    );
    assert_eq!(
        entry(&s, "guides/intro.yaml", &w.created["$i"])["body"].as_str(),
        Some(id_ref.as_str())
    );
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["fields"]["key"].as_str(),
        Some("NOTE-1")
    );
}

#[test]
fn wcco1_checkout_refuses_to_overwrite_an_untracked_file() {
    let (s, _seed) = seeded();
    put(&s, "notes.yaml", "my own notes\n");
    let (okk, v) = json(&s, &["checkout"]);
    assert!(!okk);
    assert_eq!(v["code"], E_WC_OCCUPIED, "{v}");
    assert!(v["message"].as_str().unwrap().contains("notes.yaml"), "{v}");
    assert_eq!(read(&s, "notes.yaml"), "my own notes\n");
    assert!(!at(&s, "guides/intro.yaml").exists(), "nothing written");
    let (okk, v) = json(&s, &["checkout", "--force"]);
    assert!(okk, "{v}");
    assert!(read(&s, "notes.yaml").contains("type: doc"));
}

#[test]
fn wcco1_checkout_refuses_over_uncommitted_edits() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Changed.");
    let (okk, v) = json(&s, &["checkout", "work"]);
    assert!(!okk);
    assert_eq!(v["code"], E_WC_DIRTY, "{v}");
    assert!(v["message"].as_str().unwrap().contains("notes.yaml"), "{v}");
    ok(&s, &["checkout", "work", "--force"]);
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"].as_str(),
        Some("Note.")
    );
}

#[test]
fn wcco1_checking_out_another_view_replaces_the_tracked_files() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "other",
        json!([{"op": "delete", "id": seed.note}, {"op": "delete", "id": seed.notes}]),
    );
    ok(&s, &["checkout", "other"]);
    assert!(!at(&s, "notes.yaml").exists());
    assert!(at(&s, "guides/intro.yaml").exists());
    put(&s, "keep.txt", "mine");
    ok(&s, &["checkout", "work"]);
    assert!(at(&s, "notes.yaml").exists());
    assert_eq!(read(&s, "keep.txt"), "mine");
}

#[test]
fn wcco1_blobs_go_to_underscore_blobs_with_an_extension() {
    let (s, seed) = seeded();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let h = r.store_mut().put_blob(b"\x89PNG fake").unwrap().to_string();
    r.close().unwrap();
    store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "figure", "parent": seed.section, "fields": {"blob": h, "media_type": "image/png", "alt": "A"}}]),
    );
    let (okk, v) = json(&s, &["checkout"]);
    assert!(okk, "{v}");
    assert_eq!(v["blobs"], 1);
    assert_eq!(
        std::fs::read(at(&s, &format!("_blobs/{h}.png"))).unwrap(),
        b"\x89PNG fake"
    );
}

#[test]
fn wcco1_the_checked_out_tree_reads_back_as_the_view() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    {
        let st = r.read();
        let m = st.materialized(&Scope::View("work".into())).unwrap();
        let pack = st.pack().clone();
        let tree = rdy_engine::wc::layout::read_tree(
            s.dir.path(),
            &rdy_engine::wc::ignore::Ignore::empty(),
            &pack,
        );
        assert!(
            tree.diags.is_empty() && tree.untracked.is_empty(),
            "{:?} {:?}",
            tree.diags,
            tree.untracked.len()
        );
        let in_files: std::collections::BTreeSet<String> =
            tree.blocks.iter().map(|b| b.block.id.to_string()).collect();
        let doc_assets: std::collections::BTreeSet<String> = m
            .iter()
            .filter(|b| pack.class_of(&b.block_type) == rdy_schema::TypeClass::DocAsset)
            .map(|b| b.id.to_string())
            .collect();
        assert_eq!(in_files, doc_assets);
        for b in &tree.blocks {
            let stored = m.get(&b.block.id).unwrap();
            assert_eq!(b.block.body, stored.body, "{}", b.block.id);
            assert_eq!(b.block.block_type, stored.block_type);
            assert_eq!(b.block.parent, stored.parent);
            assert_eq!(b.block.order, stored.order);
        }
    }
    r.close().unwrap();
}

#[test]
fn wcco1_the_bound_view_is_the_cli_default_view() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout", "side"]);
    ok(&s, &["new", "doc", "fresh", "--title", "Fresh"]);
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    {
        let st = r.read();
        assert!(
            st.resolve_id(&Scope::View("side".into()), "fresh").is_ok(),
            "written to the bound view"
        );
        assert!(st.resolve_id(&Scope::View("work".into()), "fresh").is_err());
    }
    r.close().unwrap();
}

#[test]
fn wcco1_rdys_own_fields_stay_in_the_store() {
    let (s, _seed) = seeded();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let mut doc = rdy_qa::fixtures::raw(
        "d-KAAAA",
        "doc",
        None,
        "a1",
        &[
            ("name", "kdoc"),
            ("title", "K"),
            ("doc_type", "page"),
            ("key", "kdoc"),
        ],
        "",
    );
    doc.fields
        .insert("key_auto".into(), rdy_model::FieldValue::Bool(true));
    rdy_qa::fixtures::raw_commit(&mut r, "work", vec![doc]);
    r.close().unwrap();
    ok(&s, &["checkout"]);
    let head = entry(&s, "kdoc.yaml", "d-KAAAA");
    assert_eq!(head["fields"]["key"].as_str(), Some("kdoc"), "{head:?}");
    assert!(
        head["fields"].get("key_auto").is_none(),
        "rdy's marker stays in the store: {head:?}"
    );
}

#[test]
fn wcco1_a_symlinked_tracked_folder_is_never_read_or_deleted_through() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let outside = tempfile::tempdir().unwrap();
    let x = outside.path().join("x");
    std::fs::rename(at(&s, "guides"), &x).unwrap();
    std::os::unix::fs::symlink(&x, at(&s, "guides")).unwrap();
    // _folder.yaml keeps the base copy's bytes; intro.yaml is someone else's file
    let folder = std::fs::read_to_string(x.join("_folder.yaml")).unwrap();
    std::fs::write(x.join("intro.yaml"), "not rdy's\n").unwrap();
    store_write(
        &s,
        "other",
        json!([{"op": "update", "id": seed.folder, "fields": {"name": "manuals"}}]),
    );
    let (okk, v) = json(&s, &["checkout", "other"]);
    assert!(!okk);
    assert_eq!(v["code"], E_WC_DIRTY, "{v}");
    let msg = v["message"].as_str().unwrap();
    assert!(
        msg.contains("guides/_folder.yaml") && msg.contains("guides/intro.yaml"),
        "{v}"
    );
    let (okk, v) = json(&s, &["checkout", "other", "--force"]);
    assert!(okk, "{v}");
    assert_eq!(
        std::fs::read_to_string(x.join("_folder.yaml")).unwrap(),
        folder,
        "never deleted through the link"
    );
    assert_eq!(
        std::fs::read_to_string(x.join("intro.yaml")).unwrap(),
        "not rdy's\n"
    );
    assert!(at(&s, "manuals/intro.yaml").exists());
    ok(&s, &["checkout", "other"]);
}

/// Sets `dir`'s mode; returns whether a write in it is now refused (never for root).
fn read_only(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let probe = dir.join(".probe");
    let refused = std::fs::write(&probe, "x").is_err();
    let _ = std::fs::remove_file(probe);
    refused
}

fn writable(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn wcco1_an_interrupted_checkout_is_refused_until_forced() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "other",
        json!([{"op": "update", "id": seed.section, "fields": {"title": "Other start"}}]),
    );
    // the marker cannot be written: nothing changes
    let wc = at(&s, ".rdy/wc");
    if !read_only(&wc) {
        writable(&wc);
        eprintln!("skipped: permissions do not refuse writes here");
        return;
    }
    let (okk, v) = json(&s, &["checkout", "other"]);
    writable(&wc);
    assert!(!okk);
    assert_eq!(v["code"], E_IO, "{v}");
    assert!(
        !read(&s, "guides/intro.yaml").contains("Other start"),
        "nothing written"
    );
    ok(&s, &["checkout"]);
    // the save fails after the files are written
    let stage = at(&s, ".rdy/wc/stage");
    assert!(read_only(&stage));
    let (okk, v) = json(&s, &["checkout", "other"]);
    writable(&stage);
    assert!(!okk);
    assert_eq!(v["code"], E_IO, "{v}");
    assert!(read(&s, "guides/intro.yaml").contains("Other start"));
    for args in [
        &["checkout"][..],
        &["checkout", "other"],
        &["checkout", "work"],
    ] {
        let (okk, v) = json(&s, args);
        assert!(!okk, "{args:?}");
        assert_eq!(v["code"], E_WC_DIRTY, "{args:?}: {v}");
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("interrupted") && msg.contains("other"), "{v}");
        assert!(
            v["fix"]
                .as_str()
                .unwrap()
                .contains("rdy checkout other --force"),
            "{v}"
        );
    }
    ok(&s, &["checkout", "other", "--force"]);
    assert!(!at(&s, ".rdy/wc/checkout.yaml").exists());
    assert!(read(&s, ".rdy/wc/bound.yaml").contains("view: other"));
    ok(&s, &["checkout", "work"]);
    assert!(!read(&s, "guides/intro.yaml").contains("Other start"));
}

/// Whether the project's filesystem treats names that differ only in case as one name.
fn case_insensitive(s: &rdy_qa::daemon::Served) -> bool {
    let probe = at(s, "CaseProbe");
    std::fs::write(&probe, "x").unwrap();
    let same = at(s, "caseprobe").exists();
    std::fs::remove_file(probe).unwrap();
    same
}

/// The names in `dir`, as the filesystem stores them.
fn names(s: &rdy_qa::daemon::Served, dir: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(at(s, dir))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn wcco1_a_case_only_doc_rename_checks_out_on_a_case_insensitive_filesystem() {
    let (s, seed) = seeded();
    if !case_insensitive(&s) {
        eprintln!("skipped: the filesystem is case-sensitive");
        return;
    }
    ok(&s, &["checkout"]);
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "Notes"}}]),
    );
    let (okk, v) = json(&s, &["checkout"]);
    assert!(okk, "{v}");
    let top = names(&s, "");
    assert!(
        top.contains(&"Notes.yaml".to_string()) && !top.contains(&"notes.yaml".to_string()),
        "{top:?}"
    );
    ok(&s, &["checkout"]);
}

#[test]
fn wcco1_a_case_only_folder_rename_renames_the_directory_on_a_case_insensitive_filesystem() {
    let (s, seed) = seeded();
    if !case_insensitive(&s) {
        eprintln!("skipped: the filesystem is case-sensitive");
        return;
    }
    ok(&s, &["checkout"]);
    put(&s, "guides/notes.txt", "mine\n");
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.folder, "fields": {"name": "Guides"}}]),
    );
    let (okk, v) = json(&s, &["checkout"]);
    assert!(okk, "{v}");
    let top = names(&s, "");
    assert!(
        top.contains(&"Guides".to_string()) && !top.contains(&"guides".to_string()),
        "{top:?}"
    );
    assert_eq!(
        names(&s, "Guides"),
        ["_folder.yaml", "intro.yaml", "notes.txt"]
    );
    ok(&s, &["checkout"]);
}

/// #695: a damaged `bound.yaml` stops `rdy new` before it lands anything; it never falls back to
/// the default view outside the checkout.
#[test]
fn msw10_damaged_wc_state_refuses_rdy_new_before_it_lands() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout", "side"]);
    std::fs::write(at(&s, ".rdy/wc/bound.yaml"), "view: [\n").unwrap();
    let (okk, v) = json(&s, &["new", "doc", "fresh", "--title", "Fresh"]);
    assert!(!okk && v["code"] == E_IO, "{v}");
    assert!(v["message"].as_str().unwrap().contains("bound.yaml"), "{v}");
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    {
        let st = r.read();
        for view in ["side", "work"] {
            assert!(
                st.resolve_id(&Scope::View(view.into()), "fresh").is_err(),
                "landed in {view}"
            );
        }
    }
    r.close().unwrap();
}

/// #695: a damaged `bound.yaml` stops `rdy edit` before the editor opens; the block is unchanged
/// in the bound view and in the default view.
#[test]
fn msw10_damaged_wc_state_refuses_rdy_edit_before_the_editor() {
    let (s, seed) = seeded();
    ok(&s, &["checkout", "side"]);
    std::fs::write(at(&s, ".rdy/wc/bound.yaml"), "view: [\n").unwrap();
    let marker = at(&s, "editor-ran");
    let script = at(&s, "ed.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ntouch '{}'\nsed -i.bak 's/Claims hold./Edited./' \"$1\"\n",
            marker.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let o = rdy_qa::daemon::human_command(
        s.dir.path(),
        &s.env_pairs(),
        &["--json", "edit", &seed.claim],
    )
    .env("EDITOR", &script)
    .output()
    .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    let v: serde_json::Value = serde_json::from_str(&err).unwrap_or_else(|_| panic!("{err}"));
    assert!(v["code"] == E_IO, "{v}");
    assert!(v["message"].as_str().unwrap().contains("bound.yaml"), "{v}");
    assert!(!marker.exists(), "the editor opened");
    for view in ["side", "work"] {
        assert_eq!(
            store_block(&s, view, &seed.claim).body,
            "Claims hold.",
            "{view}"
        );
    }
}
