//! wccm1: rdy commit records the staged files as one change in the bound view, writes them back in
//! canonical form with new ids, prints the consequences, maps engine refusals to E_WC_*, shows the
//! nonce, and recovers from a lost acknowledgement through the journal (spec §12.3, D-5T7RQ,
//! D-JHPWQ). Checkout then a commit with no edits records nothing (invariant 7).
use rdy_engine::wc::state::*;
use rdy_engine::*;
use rdy_model::{BlockId, ChangeLinks, Hash, Justification};
use rdy_qa::daemon::Served;
use rdy_qa::wc::*;
use serde_json::json;

fn journal(s: &Served, rewrite: &str, file: &str) -> String {
    let place = Place::locate(s.dir.path()).unwrap();
    let mut st = WcState::load(&place).unwrap();
    let nonce = new_nonce();
    st.pending = Some(Pending {
        nonce: nonce.clone(),
        view: "work".into(),
        assigned: vec![],
        rewrite: vec![BlockId::parse(rewrite).unwrap()],
        files: vec![file.into()],
        at_ms: 1,
    });
    st.save(&place).unwrap();
    nonce
}

#[test]
fn wccm1_commit_writes_back_ids_and_drops_comments() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let mut text = read(&s, "guides/intro.yaml");
    text.push_str(&format!(
        "# a comment\n- type: prose\n  parent: {}\n  body: Added.\n",
        seed.section
    ));
    std::fs::write(at(&s, "guides/intro.yaml"), text).unwrap();
    ok(&s, &["add", "guides"]);
    let (okk, v) = json(
        &s,
        &[
            "commit",
            "-m",
            "add a paragraph",
            "--implements",
            seed.decision.as_str(),
        ],
    );
    assert!(okk, "{v}");
    assert_eq!(v["view"], "work");
    assert!(v["consequences"].is_array(), "{v}");
    let back = read(&s, "guides/intro.yaml");
    assert!(!back.contains("# a comment"), "{back}");
    let added = entries(&s, "guides/intro.yaml")
        .into_iter()
        .find(|e| e["body"].as_str() == Some("Added."))
        .unwrap();
    let id = added["id"].as_str().unwrap().to_string();
    assert!(
        id.starts_with("p-") && added["order"].as_str().is_some(),
        "{added:?}"
    );
    assert_eq!(store_block(&s, "work", &id).body, "Added.");
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    {
        let change = r
            .read()
            .reader()
            .change(&v["change"].as_str().unwrap().parse::<Hash>().unwrap())
            .unwrap();
        assert_eq!(
            change.justification,
            Some(Justification::Implements(vec![
                BlockId::parse(&seed.decision).unwrap()
            ]))
        );
        assert!(change.links.client_nonce.is_some());
    }
    r.close().unwrap();
    let (_, st) = json(&s, &["status"]);
    assert!(
        st["working_copy"]["staged"].as_array().unwrap().is_empty(),
        "{st}"
    );
    assert!(
        st["working_copy"]["unstaged"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{st}"
    );
}

#[test]
fn wccm1_a_commit_keeps_store_only_links() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        let c = l
            .iter_mut()
            .find(|e| e["id"].as_str() == Some(seed.claim.as_str()))
            .unwrap();
        c.as_mapping_mut().unwrap().remove("links");
    });
    ok(&s, &["add", "guides/intro.yaml"]);
    ok(&s, &["commit", "-m", "drop the reader link"]);
    let b = store_block(&s, "work", &seed.claim);
    assert!(!b.links.contains_key("describes"), "{:?}", b.links);
    assert_eq!(b.links["decided_by"][0].target.to_string(), seed.decision);
}

#[test]
fn wccm1_nothing_staged_conflicts_and_stale_bases_are_refused() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(
        !okk && v["code"] == E_BAD_INPUT && v["message"] == "nothing staged",
        "{v}"
    );
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.note, "body": "Theirs."}]),
    );
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_STALE, "{v}");
    assert!(v["fix"].as_str().unwrap().contains("rdy update"), "{v}");
    assert!(
        !at(&s, ".rdy/wc/pending.yaml").exists(),
        "a refused commit drops its journal"
    );
    let place = Place::locate(s.dir.path()).unwrap();
    let mut st = WcState::load(&place).unwrap();
    st.conflicts.push(ConflictEntry {
        path: "notes.yaml".into(),
        kind: ConflictKind::Blocks,
        blocks: vec![],
        theirs_versions: Default::default(),
        local_name: None,
        store_name: None,
        store_parent: None,
    });
    st.save(&place).unwrap();
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_CONFLICT, "{v}");
}

#[test]
fn wccm1_invalid_files_are_e_wc_invalid_with_positions() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let orig = read(&s, "notes.yaml");
    std::fs::write(
        at(&s, "notes.yaml"),
        format!("{orig}- type: claim\n  parent: {}\n", seed.notes),
    )
    .unwrap();
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    assert_eq!(v["diagnostics"][0]["file"], "notes.yaml");
    assert_eq!(v["diagnostics"][0]["code"], "E_BODY_REQUIRED");
    let o = s.rdy(&["commit", "-m", "x"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("notes.yaml:"),
        "text lists the diagnostics"
    );
    // the client lint does not check references; the engine refuses, still at the file
    std::fs::write(
        at(&s, "notes.yaml"),
        format!(
            "{orig}- type: claim\n  parent: {}\n  body: See {{{{nope}}}}.\n",
            seed.notes
        ),
    )
    .unwrap();
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    assert_eq!(v["diagnostics"][0]["file"], "notes.yaml", "{v}");
    assert_eq!(v["diagnostics"][0]["code"], "E_UNKNOWN_REF", "{v}");
}

#[test]
fn wccm1_deleting_a_doc_that_holds_a_decision_is_refused() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::remove_file(at(&s, "guides/intro.yaml")).unwrap();
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_INVALID, "{v}");
    assert!(
        v["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("live only in the store"),
        "{v}"
    );
    assert_eq!(
        store_block(&s, "work", &seed.decision).block_type,
        "decision"
    );
}

#[test]
fn wccm1_a_journal_whose_change_landed_in_the_view_finishes_the_write_back() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    // a lost acknowledgement: the change is in the view with the journal's nonce, the reply never came
    let nonce = journal(&s, &seed.notes, "notes.yaml");
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let ops = serde_json::from_value(json!([{"op": "update", "id": seed.note, "body": "Mine."}]))
        .unwrap();
    r.write_linked(
        &rdy_qa::fixtures::writer(),
        WriteRequest {
            view: "work".into(),
            ops,
            justification: None,
            message: "lost".into(),
        },
        ChangeLinks {
            client_nonce: Some(nonce),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    r.close().unwrap();
    let o = s.rdy(&["status"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("recovered commit"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!at(&s, ".rdy/wc/pending.yaml").exists());
    let (_, v) = json(&s, &["status"]);
    assert!(
        v["working_copy"]["staged"].as_array().unwrap().is_empty(),
        "the stage was consumed: {v}"
    );
    assert!(
        v["working_copy"]["unstaged"].as_array().unwrap().is_empty(),
        "the base matches the store: {v}"
    );
    let (okk, v) = json(&s, &["commit", "-m", "again"]);
    assert!(
        !okk && v["message"] == "nothing staged",
        "never committed twice: {v}"
    );
}

#[test]
fn wccm1_a_journal_with_no_change_is_dropped_and_the_stage_stays() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    journal(&s, &seed.notes, "notes.yaml");
    let o = s.rdy(&["status"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("did not reach the store"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!at(&s, ".rdy/wc/pending.yaml").exists());
    let (_, v) = json(&s, &["status"]);
    assert_eq!(v["working_copy"]["staged"][0]["path"], "notes.yaml", "{v}");
    ok(&s, &["commit", "-m", "now"]);
    assert_eq!(store_block(&s, "work", &seed.note).body, "Mine.");
}

#[test]
fn wccm1_checkout_then_commit_with_no_edits_records_nothing() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    ok(&s, &["add", "."]);
    let (okk, v) = json(&s, &["commit", "-m", "nothing"]);
    assert!(!okk && v["message"] == "nothing staged", "{v}");
}

#[test]
fn wccm1_view_show_marks_working_copy_commits() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "from files"]);
    let (okk, v) = json(&s, &["view", "show", "work"]);
    assert!(okk, "{v}");
    let text = serde_json::to_string(&v).unwrap();
    assert!(text.contains("\"client_nonce\""), "{v}");
    assert!(ok(&s, &["view", "show", "work"]).contains("from files [wc]"));
}

#[test]
fn wccm1_link_targets_as_a_string_a_key_or_objects_all_commit() {
    let (s, seed) = seeded();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.note, "fields": {"key": "NOTE-1"}}]),
    );
    ok(&s, &["checkout"]);
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
                "describes".into(),
                serde_yaml_ng::from_str(&format!("[{}, NOTE-1]", seed.prose)).unwrap(),
            );
    });
    ok(&s, &["add", "guides/intro.yaml"]);
    ok(&s, &["commit", "-m", "mixed link forms"]);
    let b = store_block(&s, "work", &seed.claim);
    let targets: Vec<String> = b.links["describes"]
        .iter()
        .map(|l| l.target.to_string())
        .collect();
    assert_eq!(targets, [seed.prose.clone(), seed.note.clone()]);
    assert_eq!(
        b.links["decided_by"][0].target.to_string(),
        seed.decision,
        "the store-only kind stays"
    );
    let c = entry(&s, "guides/intro.yaml", &seed.claim);
    assert_eq!(
        c["links"]["describes"][1]["target"].as_str(),
        Some(seed.note.as_str()),
        "written back as an id: {c:?}"
    );
}

#[test]
fn wccm1_a_later_edit_to_a_staged_file_is_refused_not_overwritten() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Staged.");
    ok(&s, &["add", "notes.yaml"]);
    set_body(&s, "notes.yaml", &seed.note, "Unstaged later edit.");
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
    assert!(v["message"].as_str().unwrap().contains("notes.yaml"), "{v}");
    assert!(v["fix"].as_str().unwrap().contains("rdy add"), "{v}");
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"],
        "Unstaged later edit."
    );
    assert!(
        !at(&s, ".rdy/wc/pending.yaml").exists(),
        "refused before the journal"
    );
    assert_eq!(store_block(&s, "work", &seed.note).body, "Note.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "both edits"]);
    assert_eq!(
        store_block(&s, "work", &seed.note).body,
        "Unstaged later edit."
    );
}

#[test]
fn wccm1_a_file_written_after_a_staged_deletion_is_refused_not_deleted() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::remove_file(at(&s, "notes.yaml")).unwrap();
    ok(&s, &["add", "notes.yaml"]);
    let mine = "- type: prose\n  body: New text, never staged.\n";
    put(&s, "notes.yaml", mine);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(!okk && v["code"] == E_WC_DIRTY, "{v}");
    assert!(v["message"].as_str().unwrap().contains("notes.yaml"), "{v}");
    assert_eq!(read(&s, "notes.yaml"), mine);
    assert_eq!(store_block(&s, "work", &seed.note).body, "Note.");
}

#[test]
fn wccm1_recovery_keeps_an_edit_made_after_the_lost_reply() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    let nonce = journal(&s, &seed.notes, "notes.yaml");
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let ops = serde_json::from_value(json!([{"op": "update", "id": seed.note, "body": "Mine."}]))
        .unwrap();
    r.write_linked(
        &rdy_qa::fixtures::writer(),
        WriteRequest {
            view: "work".into(),
            ops,
            justification: None,
            message: "lost".into(),
        },
        ChangeLinks {
            client_nonce: Some(nonce),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    r.close().unwrap();
    // the person edits the file again before the next command
    set_body(&s, "notes.yaml", &seed.note, "Later.");
    let o = s.rdy(&["status"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("recovered commit") && err.contains("notes.yaml"),
        "{err}"
    );
    assert!(!at(&s, ".rdy/wc/pending.yaml").exists());
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"],
        "Later.",
        "the later edit survives"
    );
}

#[test]
fn wccm1_the_write_back_keeps_an_untracked_file_where_a_renamed_doc_would_go() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let memo = "my own memo, never tracked\n";
    put(&s, "memo.yaml", memo);
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.notes, "fields": {"name": "memo"}}]),
    );
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(okk, "{v}");
    assert_eq!(read(&s, "memo.yaml"), memo, "the untracked file is kept");
    assert_eq!(
        entry(&s, "notes.yaml", &seed.note)["body"],
        "Mine.",
        "the old path stays"
    );
    assert_eq!(v["kept"][0]["file"], "memo.yaml", "{v}");
    assert!(
        v["kept"][0]["fix"].as_str().unwrap().contains("rdy update"),
        "{v}"
    );
    assert_eq!(store_block(&s, "work", &seed.note).body, "Mine.");
    let (okk, v) = json(&s, &["commit", "-m", "y"]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT,
        "the kept file refuses commits: {v}"
    );
}

/// Whether the project's filesystem treats names that differ only in case as one name.
fn case_insensitive(s: &Served) -> bool {
    let probe = at(s, "CaseProbe");
    std::fs::write(&probe, "x").unwrap();
    let same = at(s, "caseprobe").exists();
    std::fs::remove_file(probe).unwrap();
    same
}

#[test]
fn wccm1_a_case_only_rename_from_the_store_survives_the_write_back() {
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
    set_body(&s, "notes.yaml", &seed.note, "Mine.");
    ok(&s, &["add", "notes.yaml"]);
    ok(&s, &["commit", "-m", "x"]);
    let top: Vec<String> = std::fs::read_dir(s.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        top.contains(&"Notes.yaml".to_string()) && !top.contains(&"notes.yaml".to_string()),
        "{top:?}"
    );
    assert_eq!(entry(&s, "Notes.yaml", &seed.note)["body"], "Mine.");
    assert!(
        at(&s, ".rdy/wc/base/files/Notes.yaml").exists(),
        "the base copy stays"
    );
    let (_, st) = json(&s, &["status"]);
    assert!(
        st["working_copy"]["staged"].as_array().unwrap().is_empty(),
        "{st}"
    );
    assert!(
        st["working_copy"]["unstaged"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{st}"
    );
}

#[test]
fn wccm1_the_write_back_keeps_an_untracked_file_where_a_projected_directory_would_go() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    put(&s, "manual", "my own file, never tracked\n");
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.folder, "fields": {"name": "manual"}}]),
    );
    set_body(&s, "guides/intro.yaml", &seed.prose, "Mine.");
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "x"]);
    assert!(okk, "{v}");
    assert_eq!(
        read(&s, "manual"),
        "my own file, never tracked\n",
        "the untracked file is kept"
    );
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.prose)["body"],
        "Mine.",
        "the old path stays"
    );
    assert!(v["kept"].as_array().is_some_and(|k| !k.is_empty()), "{v}");
    assert_eq!(store_block(&s, "work", &seed.prose).body, "Mine.");
}

/// How many entries of `rel` hold `body`.
fn with_body(s: &Served, rel: &str, body: &str) -> usize {
    entries(s, rel)
        .iter()
        .filter(|e| e["body"].as_str() == Some(body))
        .count()
}

#[test]
fn wccm1_a_file_kept_by_recovery_is_a_conflict_and_is_never_recorded_twice() {
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
    let nonce = journal(&s, &seed.notes, "notes.yaml");
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
        ChangeLinks {
            client_nonce: Some(nonce),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    r.close().unwrap();
    // the person edits the file again before the next command: the write-back keeps it
    let mut text = read(&s, "notes.yaml");
    text.push_str("# a later comment\n");
    put(&s, "notes.yaml", &text);
    let o = s.rdy(&["status"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && err.contains("W_WC_KEPT"), "{err}");
    assert!(
        err.contains("rdy update"),
        "the fix names the update that resolves it: {err}"
    );
    let (_, st) = json(&s, &["status"]);
    assert_eq!(
        st["working_copy"]["conflicts"][0]["path"], "notes.yaml",
        "{st}"
    );
    assert_eq!(st["working_copy"]["conflicts"][0]["kind"], "kept", "{st}");
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "again"]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT,
        "never recorded twice: {v}"
    );
    assert!(v["fix"].as_str().unwrap().contains("rdy update"), "{v}");
    assert!(!at(&s, ".rdy/wc/pending.yaml").exists());
    // the other way out: the store's version, holding the block once
    ok(&s, &["checkout", "--force"]);
    assert_eq!(
        with_body(&s, "notes.yaml", "Added once."),
        1,
        "{}",
        read(&s, "notes.yaml")
    );
    let (_, st) = json(&s, &["status"]);
    assert!(
        st["working_copy"]["conflicts"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{st}"
    );
    assert!(
        st["working_copy"]["unstaged"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{st}"
    );
}

#[test]
fn wccm1_a_renamed_doc_kept_by_the_write_back_is_a_conflict_and_is_never_recorded_twice() {
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
    assert!(okk, "{v}");
    assert_eq!(v["kept"][0]["file"], "memo.yaml", "{v}");
    let fix = v["kept"][0]["fix"].as_str().unwrap();
    assert!(fix.contains("rdy update"), "{v}");
    let (_, st) = json(&s, &["status"]);
    assert_eq!(
        st["working_copy"]["conflicts"][0]["path"], "notes.yaml",
        "{st}"
    );
    assert_eq!(st["working_copy"]["conflicts"][0]["kind"], "kept", "{st}");
    assert!(
        st["working_copy"]["conflicts"][0]["message"]
            .as_str()
            .unwrap()
            .contains("memo.yaml"),
        "{st}"
    );
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "again"]);
    assert!(
        !okk && v["code"] == E_WC_CONFLICT,
        "never recorded twice: {v}"
    );
    // the other way out: move the untracked file aside, then check out again
    std::fs::rename(at(&s, "memo.yaml"), at(&s, "memo.txt")).unwrap();
    ok(&s, &["checkout", "--force"]);
    assert_eq!(read(&s, "memo.txt"), memo);
    assert!(!at(&s, "notes.yaml").exists());
    assert_eq!(
        with_body(&s, "memo.yaml", "Added once."),
        1,
        "{}",
        read(&s, "memo.yaml")
    );
}

/// A commit whose only change is a block moved into a file that is not staged records nothing:
/// it is refused and the stage stays, so staging the other file is all a retry needs (R-P11).
#[test]
fn wccm1_a_move_into_an_unstaged_file_keeps_the_stage() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let mut moved = None;
    edit(&s, "guides/intro.yaml", |l| {
        let i = l
            .iter()
            .position(|e| e["id"].as_str() == Some(seed.prose.as_str()))
            .unwrap();
        moved = Some(l.remove(i));
    });
    let mut p = moved.unwrap();
    p.as_mapping_mut()
        .unwrap()
        .insert("parent".into(), seed.notes.clone().into());
    edit(&s, "notes.yaml", |l| l.push(p));
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "move"]);
    assert!(!okk, "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("notes.yaml")
            && v["fix"].as_str().unwrap().contains("rdy add notes.yaml"),
        "{v}"
    );
    assert!(
        at(&s, ".rdy/wc/stage/files/guides/intro.yaml").exists(),
        "the stage stays"
    );
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "move"]);
    assert!(okk, "{v}");
    assert_eq!(
        store_block(&s, "work", &seed.prose)
            .parent
            .unwrap()
            .to_string(),
        seed.notes
    );
}

/// An edit R-P4 ignores (a hand-edited `order`) records nothing; the file goes back to its base
/// text, so status is clean.
#[test]
fn wccm1_an_order_only_edit_leaves_the_file_clean() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let before = read(&s, "notes.yaml");
    edit(&s, "notes.yaml", |l| {
        let n = l
            .iter_mut()
            .find(|e| e["id"].as_str() == Some(seed.note.as_str()))
            .unwrap();
        n.as_mapping_mut()
            .unwrap()
            .insert("order".into(), "zz".into());
    });
    ok(&s, &["add", "notes.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "order"]);
    assert!(okk && v["change"].is_null(), "{v}");
    assert_eq!(read(&s, "notes.yaml"), before);
    let (_, st) = json(&s, &["status"]);
    for k in ["staged", "unstaged"] {
        assert!(st["working_copy"][k].as_array().unwrap().is_empty(), "{st}");
    }
}
