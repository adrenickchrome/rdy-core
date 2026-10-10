//! wcdf1: the commit planner turns staged files into one write: updates with checked-out bases,
//! creates with client ids placed by position, deletes of doc assets only, moves, renames and
//! retypes, and the per-kind link rule that never drops a store-only link (FW-D14). The
//! working-copy lint refuses supporting blocks, store-only links and pins (MS, FW-D10).
use rdy_engine::wc::plan::*;
use rdy_engine::wc::state::*;
use rdy_qa::daemon::Served;
use rdy_qa::wc::*;
use serde_json::{Value, json};

fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

/// Plans the commit, and checks that planning left every base copy as it was.
fn plan(s: &Served) -> Result<CommitPlan, WcInvalid> {
    let place = Place::locate(s.dir.path()).unwrap();
    let st = WcState::load(&place).unwrap();
    let before = snapshot(&place.state.join("base/files"));
    let p = plan_commit(&place, &st, &pack());
    assert_eq!(
        snapshot(&place.state.join("base/files")),
        before,
        "planning changed base/files"
    );
    p
}

/// Every file under `dir`, by relative path, with its bytes.
fn snapshot(dir: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let path = e.path();
            if e.file_type().unwrap().is_dir() {
                todo.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

/// Sends the plan's ops to the bound view through the engine, as one write.
fn send(s: &Served, p: &CommitPlan) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    let place = Place::locate(s.dir.path()).unwrap();
    let view = WcState::load(&place).unwrap().bound.unwrap().view;
    s.stop();
    let mut r = rdy_engine::Rdy::open(s.dir.path()).unwrap();
    let w = rdy_qa::fixtures::write(&mut r, &view, Value::Array(p.ops.clone()));
    r.close().unwrap();
    w
}

/// No `before` names a block that a later op creates or moves: the engine places each op
/// against the blocks that already sit under the parent.
fn assert_befores_exist(p: &CommitPlan) {
    for (i, o) in p.ops.iter().enumerate() {
        let before = o
            .get("before")
            .or_else(|| o.get("move_to").and_then(|m| m.get("before")));
        let Some(before) = before else { continue };
        let later = p.ops[i + 1..]
            .iter()
            .any(|l| l["id"] == *before && (l["op"] == "create" || l.get("move_to").is_some()));
        assert!(
            !later,
            "op {i} is placed before {before}, which a later op places: {:?}",
            p.ops
        );
    }
}

/// Whether the refusal holds an error with `code` at `file` whose message names `text`.
fn refused_at(e: &WcInvalid, code: &str, file: &str, text: &str) -> bool {
    e.diags
        .iter()
        .any(|d| d.code == code && d.file == file && d.message.contains(text))
}

/// Stages paths as `rdy add` does (Task 7 builds it in parallel): a file's bytes, or a deletion.
fn stage(s: &Served, paths: &[&str]) {
    let place = Place::locate(s.dir.path()).unwrap();
    let mut st = WcState::load(&place).unwrap();
    for rel in paths {
        match std::fs::read(at(s, rel)) {
            Ok(bytes) => stage_put(&place, rel, &bytes).unwrap(),
            Err(_) => {
                st.stage.deleted.insert(rel.to_string());
            }
        }
    }
    st.save(&place).unwrap();
}

fn op<'a>(p: &'a CommitPlan, kind: &str, id: &str) -> &'a Value {
    p.ops
        .iter()
        .find(|o| o["op"] == kind && o["id"] == id)
        .unwrap_or_else(|| panic!("no {kind} {id} in {:?}", p.ops))
}

fn base_version(s: &Served, id: &str) -> String {
    let place = Place::locate(s.dir.path()).unwrap();
    WcState::load(&place).unwrap().base.blocks[&rdy_model::BlockId::parse(id).unwrap()]
        .version
        .clone()
}

fn yaml(text: &str) -> serde_yaml_ng::Value {
    serde_yaml_ng::from_str(text).unwrap()
}

fn find<'a>(list: &'a mut [serde_yaml_ng::Value], id: &str) -> &'a mut serde_yaml_ng::Value {
    list.iter_mut()
        .find(|e| e["id"].as_str() == Some(id))
        .unwrap()
}

#[test]
fn wcdf1_an_edited_body_is_one_update_with_its_checked_out_base() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let base = base_version(&s, &seed.prose);
    set_body(&s, "guides/intro.yaml", &seed.prose, "Changed.");
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    assert_eq!(p.ops.len(), 1, "{:?}", p.ops);
    let u = op(&p, "update", &seed.prose);
    assert_eq!(u["base"], base.as_str());
    assert_eq!(u["body"], "Changed.");
    assert!(
        u.get("links").is_none() && u.get("fields").is_none() && u.get("move_to").is_none(),
        "{u}"
    );
    assert!(p.committed.contains("guides/intro.yaml"));
    assert!(p.rewrite.iter().any(|r| r.to_string() == seed.intro));
}

/// The base is the files `base.files` names: a stray file under `base/files` (a stale copy that a
/// power loss brought back) is not part of it, so it is not planned as a deletion.
#[test]
fn wcdf1_a_stray_file_in_base_files_is_not_part_of_the_base() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let place = Place::locate(s.dir.path()).unwrap();
    let stray = place.state.join("base/files/stray.yaml");
    std::fs::write(
        &stray,
        "- id: d-KAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Stray\n    doc_type: page\n",
    )
    .unwrap();
    set_body(&s, "guides/intro.yaml", &seed.prose, "Changed.");
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    assert_eq!(p.ops.len(), 1, "{:?}", p.ops);
    op(&p, "update", &seed.prose);
    assert!(stray.exists(), "planning removed the stray file");
}

#[test]
fn wcdf1_removing_every_reader_link_keeps_decided_by() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        find(l, &seed.claim)
            .as_mapping_mut()
            .unwrap()
            .remove("links");
    });
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    let u = op(&p, "update", &seed.claim);
    assert_eq!(
        u["links"],
        json!({"describes": []}),
        "only the reader kind the file dropped: {u}"
    );
}

#[test]
fn wcdf1_a_kept_kind_carries_its_hidden_links() {
    let (s, seed) = seeded();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.claim, "links": {"describes": [{"target": seed.prose}, {"target": seed.decision}]}}]),
    );
    ok(&s, &["checkout"]);
    let c = entry(&s, "guides/intro.yaml", &seed.claim);
    assert_eq!(
        c["links"]["describes"].as_sequence().unwrap().len(),
        1,
        "the decision target is store-only: {c:?}"
    );
    edit(&s, "guides/intro.yaml", |l| {
        let c = find(l, &seed.claim);
        let d = c.get_mut("links").unwrap().get_mut("describes").unwrap();
        d.as_sequence_mut()
            .unwrap()
            .push(yaml(&format!("target: {}", seed.note)));
    });
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    let u = op(&p, "update", &seed.claim);
    let targets: Vec<&str> = u["links"]["describes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["target"].as_str().unwrap())
        .collect();
    assert_eq!(
        targets,
        [
            seed.prose.as_str(),
            seed.note.as_str(),
            seed.decision.as_str()
        ]
    );
}

#[test]
fn wcdf1_a_supporting_block_missing_from_the_file_is_never_deleted() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        l.retain(|e| e["id"].as_str() != Some(seed.claim.as_str()))
    });
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    op(&p, "delete", &seed.claim);
    assert!(
        !serde_json::to_string(&p.ops)
            .unwrap()
            .contains(&seed.decision),
        "{:?}",
        p.ops
    );
}

#[test]
fn wcdf1_new_entries_are_creates_placed_by_position() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        let i = l
            .iter()
            .position(|e| e["id"].as_str() == Some(seed.prose.as_str()))
            .unwrap();
        l.insert(
            i + 1,
            yaml(&format!(
                "type: prose\nparent: {}\nbody: Inserted.\n",
                seed.section
            )),
        );
    });
    put(
        &s,
        "fresh.yaml",
        "- type: doc\n  fields:\n    title: Fresh\n    doc_type: page\n- type: prose\n  body: Body.\n",
    );
    stage(&s, &["guides/intro.yaml", "fresh.yaml"]);
    let p = plan(&s).unwrap();
    let ins = p
        .ops
        .iter()
        .find(|o| o["op"] == "create" && o["body"] == "Inserted.")
        .unwrap();
    assert_eq!(ins["after"], seed.prose.as_str());
    assert_eq!(ins["parent"], seed.section.as_str());
    let id = ins["id"].as_str().unwrap();
    assert!(
        id.starts_with("p-") && p.created.iter().any(|c| c.to_string() == id),
        "{ins}"
    );
    let di = p
        .ops
        .iter()
        .position(|o| o["op"] == "create" && o["type"] == "doc")
        .unwrap();
    let doc_id = p.ops[di]["id"].as_str().unwrap().to_string();
    assert_eq!(p.ops[di]["fields"]["name"], "fresh");
    let pi = p
        .ops
        .iter()
        .position(|o| o["op"] == "create" && o["body"] == "Body.")
        .unwrap();
    assert!(
        di < pi && p.ops[pi]["parent"] == doc_id.as_str(),
        "{:?}",
        p.ops
    );
    assert!(
        p.rewrite.iter().any(|r| r.to_string() == doc_id),
        "the new doc's file is rewritten"
    );
}

#[test]
fn wcdf1_a_moved_paragraph_is_one_move_to() {
    let (s, seed) = seeded();
    let w = store_write(
        &s,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$t", "parent": seed.section, "after": seed.claim, "body": "Third."}]),
    );
    let third = w.created["$t"].clone();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        let i = l
            .iter()
            .position(|e| e["id"].as_str() == Some(seed.prose.as_str()))
            .unwrap();
        let p = l.remove(i);
        let j = l
            .iter()
            .position(|e| e["id"].as_str() == Some(third.as_str()))
            .unwrap();
        l.insert(j + 1, p);
    });
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    assert_eq!(p.ops.len(), 1, "{:?}", p.ops);
    let u = op(&p, "update", &seed.prose);
    assert_eq!(
        u["move_to"],
        json!({"parent": seed.section, "after": third})
    );
}

#[test]
fn wcdf1_a_file_moved_and_renamed_is_one_update_of_the_doc() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "guides/journal.yaml")).unwrap();
    stage(&s, &["notes.yaml", "guides/journal.yaml"]);
    let p = plan(&s).unwrap();
    assert_eq!(p.ops.len(), 1, "{:?}", p.ops);
    let u = op(&p, "update", &seed.notes);
    assert_eq!(u["fields"]["name"], "journal");
    assert_eq!(u["move_to"]["parent"], seed.folder.as_str());
    assert!(p.removed_files.contains("notes.yaml"));
}

#[test]
fn wcdf1_a_type_edit_is_an_update_with_type() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        find(l, &seed.claim)
            .as_mapping_mut()
            .unwrap()
            .insert("type".into(), "requirement".into());
    });
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    assert_eq!(op(&p, "update", &seed.claim)["type"], "requirement");
}

#[test]
fn wcdf1_the_lint_refuses_supporting_blocks_store_only_links_and_pins() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        l.push(yaml(&format!(
            "type: decision\nparent: {}\nfields:\n  statement: X\n  status: proposed\n",
            seed.section
        )));
        let c = find(l, &seed.claim);
        let links = c.get_mut("links").unwrap().as_mapping_mut().unwrap();
        links.insert(
            "decided_by".into(),
            yaml(&format!("- target: {}", seed.decision)),
        );
        links.insert(
            "describes".into(),
            yaml(&format!("- target: {}", seed.decision)),
        );
        find(l, &seed.prose).as_mapping_mut().unwrap().insert(
            "pins".into(),
            yaml(&format!("{}: {}", seed.claim, "ab".repeat(32))),
        );
    });
    stage(&s, &["guides/intro.yaml"]);
    let e = plan(&s).unwrap_err();
    let has = |code: &str, text: &str| {
        e.diags
            .iter()
            .any(|d| d.code == code && (d.message.contains(text) || d.fix.contains(text)))
    };
    assert!(has("E_WC_SUPPORTING", "rdy_decide"), "{:?}", e.diags);
    assert!(has("E_WC_STORE_ONLY", "decided_by"), "{:?}", e.diags);
    assert!(
        has("E_WC_STORE_ONLY", "decisions and questions"),
        "{:?}",
        e.diags
    );
    assert!(has("E_WC_STORE_ONLY", "pins"), "{:?}", e.diags);
    assert!(
        e.diags.iter().all(|d| d.file == "guides/intro.yaml"),
        "{:?}",
        e.diags
    );
}

#[test]
fn wcdf1_a_block_moved_into_an_unstaged_file_is_left_with_a_warning() {
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
    stage(&s, &["guides/intro.yaml"]);
    let plan = plan(&s).unwrap();
    assert!(
        !plan
            .ops
            .iter()
            .any(|o| o["op"] == "delete" && o["id"] == seed.prose.as_str()),
        "{:?}",
        plan.ops
    );
    assert!(
        plan.warnings
            .iter()
            .any(|d| d.code == "W_WC_UNSTAGED_MOVE" && d.fix.contains("rdy add notes.yaml")),
        "{:?}",
        plan.warnings
    );
}

#[test]
fn wcdf1_nothing_staged_is_an_empty_plan() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    let p = plan(&s).unwrap();
    assert!(p.ops.is_empty() && p.committed.is_empty(), "{:?}", p.ops);
}

#[test]
fn wcdf1_a_link_written_as_a_string_or_a_list_is_the_same_link() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    for form in [seed.prose.clone(), format!("[{}]", seed.prose)] {
        edit(&s, "guides/intro.yaml", |l| {
            let c = find(l, &seed.claim);
            c.get_mut("links")
                .unwrap()
                .as_mapping_mut()
                .unwrap()
                .insert("describes".into(), yaml(&form));
            c.as_mapping_mut()
                .unwrap()
                .insert("body".into(), "Claims still hold.".into());
        });
        stage(&s, &["guides/intro.yaml"]);
        let p = plan(&s).unwrap();
        let u = op(&p, "update", &seed.claim);
        assert!(
            u.get("links").is_none(),
            "{form}: the same target in another form is no link change: {u}"
        );
    }
}

#[test]
fn wcdf1_a_link_target_written_as_a_key_is_sent_as_the_key() {
    let (s, seed) = seeded();
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": seed.note, "fields": {"key": "NOTE-1"}}]),
    );
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        let c = find(l, &seed.claim);
        c.get_mut("links")
            .unwrap()
            .as_mapping_mut()
            .unwrap()
            .insert(
                "describes".into(),
                yaml(&format!("[{}, NOTE-1]", seed.prose)),
            );
    });
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    let u = op(&p, "update", &seed.claim);
    assert_eq!(
        u["links"]["describes"],
        json!([{"target": seed.prose}, {"target": "NOTE-1"}]),
        "{u}"
    );
}

#[test]
fn wcdf1_the_lint_refuses_rdys_own_fields() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "notes.yaml", |l| {
        find(l, &seed.notes)
            .get_mut("fields")
            .unwrap()
            .as_mapping_mut()
            .unwrap()
            .insert("key_auto".into(), true.into());
    });
    stage(&s, &["notes.yaml"]);
    let e = plan(&s).unwrap_err();
    assert!(
        e.diags
            .iter()
            .any(|d| d.code == "E_WC_STORE_ONLY" && d.message.contains("key_auto")),
        "{:?}",
        e.diags
    );
}

#[test]
fn wcdf1_a_new_doc_with_two_paragraphs_commits() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    put(
        &s,
        "fresh.yaml",
        "- type: doc\n  fields:\n    title: Fresh\n    doc_type: page\n- type: prose\n  body: One.\n- type: prose\n  body: Two.\n",
    );
    stage(&s, &["fresh.yaml"]);
    let p = plan(&s).unwrap();
    assert_befores_exist(&p);
    let one = p.ops.iter().find(|o| o["body"] == "One.").unwrap();
    let two = p.ops.iter().find(|o| o["body"] == "Two.").unwrap();
    assert!(
        one.get("before").is_none() && one.get("after").is_none(),
        "{one}"
    );
    assert_eq!(two["after"], one["id"], "{two}");
    send(&s, &p).unwrap();
}

#[test]
fn wcdf1_two_new_paragraphs_at_the_top_of_a_section_commit() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        let i = l
            .iter()
            .position(|e| e["id"].as_str() == Some(seed.prose.as_str()))
            .unwrap();
        l.insert(
            i,
            yaml(&format!(
                "type: prose\nparent: {}\nbody: Second.\n",
                seed.section
            )),
        );
        l.insert(
            i,
            yaml(&format!(
                "type: prose\nparent: {}\nbody: First.\n",
                seed.section
            )),
        );
    });
    stage(&s, &["guides/intro.yaml"]);
    let p = plan(&s).unwrap();
    assert_befores_exist(&p);
    let first = p.ops.iter().find(|o| o["body"] == "First.").unwrap();
    let second = p.ops.iter().find(|o| o["body"] == "Second.").unwrap();
    assert_eq!(
        first["before"],
        seed.prose.as_str(),
        "the first kept sibling: {first}"
    );
    assert_eq!(second["after"], first["id"], "{second}");
    send(&s, &p).unwrap();
}

#[test]
fn wcdf1_a_paragraph_moved_in_at_the_top_then_a_new_one_commits() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let mut moved = None;
    edit(&s, "notes.yaml", |l| {
        let i = l
            .iter()
            .position(|e| e["id"].as_str() == Some(seed.note.as_str()))
            .unwrap();
        moved = Some(l.remove(i));
    });
    let mut n = moved.unwrap();
    n.as_mapping_mut()
        .unwrap()
        .insert("parent".into(), seed.section.clone().into());
    edit(&s, "guides/intro.yaml", |l| {
        let i = l
            .iter()
            .position(|e| e["id"].as_str() == Some(seed.prose.as_str()))
            .unwrap();
        l.insert(
            i,
            yaml(&format!(
                "type: prose\nparent: {}\nbody: After the note.\n",
                seed.section
            )),
        );
        l.insert(i, n);
    });
    stage(&s, &["guides/intro.yaml", "notes.yaml"]);
    let p = plan(&s).unwrap();
    assert_befores_exist(&p);
    assert_eq!(
        op(&p, "update", &seed.note)["move_to"],
        json!({"parent": seed.section, "before": seed.prose})
    );
    send(&s, &p).unwrap();
}

#[test]
fn wcdf1_a_staged_file_that_does_not_parse_is_refused_not_deleted() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    put(
        &s,
        "notes.yaml",
        &format!("- type: doc\n  id: {}\n  fields: [\n", seed.notes),
    );
    stage(&s, &["notes.yaml"]);
    let e = plan(&s).unwrap_err();
    assert!(
        refused_at(&e, "E_WC_INVALID", "notes.yaml", "notes.yaml is not read"),
        "{:?}",
        e.diags
    );
}

#[test]
fn wcdf1_a_staged_file_whose_first_entry_is_not_the_doc_is_refused() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "notes.yaml", |l| l.reverse());
    stage(&s, &["notes.yaml"]);
    let e = plan(&s).unwrap_err();
    assert!(
        refused_at(&e, "E_WC_INVALID", "notes.yaml", "notes.yaml is not read"),
        "{:?}",
        e.diags
    );
}

#[test]
fn wcdf1_a_broken_folder_file_refuses_every_file_under_it() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    put(
        &s,
        "guides/_folder.yaml",
        "- type: folder\n  fields: {title: \"Guides\n",
    );
    stage(&s, &["guides/_folder.yaml"]);
    let e = plan(&s).unwrap_err();
    assert!(
        refused_at(
            &e,
            "E_WC_INVALID",
            "guides/_folder.yaml",
            "guides/_folder.yaml is not read"
        ),
        "{:?}",
        e.diags
    );
    assert!(
        refused_at(
            &e,
            "E_WC_INVALID",
            "guides/intro.yaml",
            "guides/_folder.yaml"
        ),
        "{:?}",
        e.diags
    );
}

#[test]
fn wcdf1_a_staged_deletion_of_only_the_folder_file_is_refused() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::remove_file(at(&s, "guides/_folder.yaml")).unwrap();
    stage(&s, &["guides/_folder.yaml"]);
    let e = plan(&s).unwrap_err();
    assert!(
        refused_at(
            &e,
            "E_WC_INVALID",
            "guides/intro.yaml",
            "guides/_folder.yaml"
        ),
        "{:?}",
        e.diags
    );
}

#[test]
fn wcdf1_a_doc_moved_into_a_directory_that_is_not_a_folder_is_refused() {
    let (s, _seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::create_dir_all(at(&s, "archive")).unwrap();
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "archive/notes.yaml")).unwrap();
    stage(&s, &["notes.yaml", "archive/notes.yaml"]);
    let e = plan(&s).unwrap_err();
    assert!(
        refused_at(
            &e,
            "E_WC_INVALID",
            "archive/notes.yaml",
            "archive/_folder.yaml"
        ),
        "{:?}",
        e.diags
    );
}

#[test]
fn wcdf1_a_case_only_rename_without_the_old_path_is_a_clash() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    let place = Place::locate(s.dir.path()).unwrap();
    let base = std::fs::read(place.base_file("notes.yaml")).unwrap();
    set_body(&s, "notes.yaml", &seed.note, "Edited.");
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "Notes.yaml")).unwrap();
    stage(&s, &["Notes.yaml"]);
    let e = plan(&s).unwrap_err();
    assert!(
        e.diags.iter().any(|d| d.code == "E_PATH_CLASH"
            && d.file == "Notes.yaml"
            && d.message.contains("notes.yaml")),
        "{:?}",
        e.diags
    );
    assert_eq!(std::fs::read(place.base_file("notes.yaml")).unwrap(), base);
}

#[test]
fn wcdf1_a_case_only_rename_with_the_old_path_deleted_is_a_rename() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    std::fs::rename(at(&s, "notes.yaml"), at(&s, "Notes.yaml")).unwrap();
    stage(&s, &["Notes.yaml"]);
    let place = Place::locate(s.dir.path()).unwrap();
    let mut st = WcState::load(&place).unwrap();
    st.stage.deleted.insert("notes.yaml".into());
    st.save(&place).unwrap();
    let p = plan(&s).unwrap();
    assert_eq!(p.ops.len(), 1, "{:?}", p.ops);
    assert_eq!(op(&p, "update", &seed.notes)["fields"]["name"], "Notes");
}

/// A file that retypes a block to a supporting type is told to record it with its tool, not to
/// write another id prefix: E_WC_SUPPORTING comes first and E_PREFIX is not raised.
#[test]
fn wcdf1_a_retype_to_a_supporting_type_leads_with_e_wc_supporting() {
    let (s, seed) = seeded();
    ok(&s, &["checkout"]);
    edit(&s, "guides/intro.yaml", |l| {
        find(l, &seed.claim)
            .as_mapping_mut()
            .unwrap()
            .insert("type".into(), "decision".into());
    });
    stage(&s, &["guides/intro.yaml"]);
    let e = plan(&s).unwrap_err();
    assert_eq!(e.diags[0].code, "E_WC_SUPPORTING", "{:?}", e.diags);
    assert!(
        e.diags.iter().all(|d| d.code != "E_PREFIX"),
        "{:?}",
        e.diags
    );
}
