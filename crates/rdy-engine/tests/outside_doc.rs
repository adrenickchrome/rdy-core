//! ndh1 (D-P3YD5, D-X3136): a decision, a question, a finding or a source may sit at the project
//! root or directly in a folder. Such a block lives only in the store: the tree export and the
//! working copy leave it out, and every other block keeps its parent rule.
mod common;

use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::path::Path;

fn writer() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn approver() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> Result<WriteResult, RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap();
}

/// A project with no decision home.
fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The ndh1 canon's ids.
struct Canon {
    area: BlockId,
    root_question: BlockId,
    folder_decision: BlockId,
    moved_question: BlockId,
}

impl Canon {
    fn outside(&self) -> [&BlockId; 3] {
        [
            &self.root_question,
            &self.folder_decision,
            &self.moved_question,
        ]
    }
}

fn parent_in(r: &Rdy, scope: &Scope, id: &BlockId) -> Option<BlockId> {
    r.read()
        .materialized(scope)
        .unwrap()
        .get(id)
        .unwrap()
        .parent
        .clone()
}

/// Writes, through `rdy_write` in view `work`: a folder `area` holding doc `guide` (a section, a
/// claim and a question), a question with no parent and a proposed decision in `area`; then
/// moves the doc-held question to the root and on into `area`. Lands the view.
fn canon(r: &mut Rdy) -> Canon {
    let work = Scope::View("work".into());
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area"}},
            {"op": "create", "type": "doc", "id": "$guide", "parent": "$area", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "One claim."},
            {"op": "create", "type": "question", "id": "$held", "parent": "$s", "fields": {"prompt": "Which ledger?"}},
            {"op": "create", "type": "question", "id": "$rootq", "fields": {"prompt": "Who owns rdy?"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$area",
             "fields": {"statement": "Use one ledger.", "status": "proposed"}}
        ]),
    )
    .unwrap();
    let id = |n: &str| bid(&w.created[n]);
    let c = Canon {
        area: id("$area"),
        root_question: id("$rootq"),
        folder_decision: id("$dec"),
        moved_question: id("$held"),
    };
    assert_eq!(parent_in(r, &work, &c.root_question), None);
    assert_eq!(
        parent_in(r, &work, &c.folder_decision),
        Some(c.area.clone())
    );
    write(
        r,
        "work",
        json!([{"op": "move", "id": c.moved_question.to_string()}]),
    )
    .unwrap();
    assert_eq!(parent_in(r, &work, &c.moved_question), None);
    write(
        r,
        "work",
        json!([{"op": "move", "id": c.moved_question.to_string(), "parent": c.area.to_string()}]),
    )
    .unwrap();
    assert_eq!(parent_in(r, &work, &c.moved_question), Some(c.area.clone()));
    land(r, "work");
    c
}

#[test]
fn ndh1_a_write_places_supporting_blocks_at_the_root_and_in_a_folder() {
    let (_dir, mut r) = project();
    let c = canon(&mut r);
    // other content keeps its rule: a prose with no parent is an orphan
    let e = write(
        &mut r,
        "more",
        json!([{"op": "create", "type": "prose", "body": "Loose words."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(e.issues.iter().any(|i| i.code == "E_ORPHAN"), "{e}");
    // the view landed: canon holds the three blocks where the writes put them
    assert_eq!(parent_in(&r, &Scope::Canon, &c.root_question), None);
    assert_eq!(
        parent_in(&r, &Scope::Canon, &c.folder_decision),
        Some(c.area.clone())
    );
    assert_eq!(
        parent_in(&r, &Scope::Canon, &c.moved_question),
        Some(c.area.clone())
    );
}

/// Every file under `dir`, read whole.
fn files_under(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((
                    p.display().to_string(),
                    std::fs::read_to_string(&p).unwrap(),
                ));
            }
        }
    }
    out
}

/// Stores a landing whose snapshot adds the prose `p-H0ST3` with no parent to canon, as
/// `pub12_implicit_refuses_a_stored_orphan_doc_as_a_named_profile_does` forges its orphan doc.
fn orphan_prose_landing(r: &mut Rdy) -> Hash {
    write(
        r,
        "seed",
        json!([{"op": "create", "type": "doc", "fields": {"name": "kept", "title": "Kept", "doc_type": "page"}}]),
    )
    .unwrap();
    land(r, "seed");
    let head = r.read().canon_head().unwrap();
    let mut snapshot = r.read().materialized(&Scope::Canon).unwrap().snapshot();
    let prose = Block {
        id: bid("p-H0ST3"),
        block_type: "prose".into(),
        parent: None,
        order: "V".into(),
        fields: Default::default(),
        body: "Loose words.".into(),
        links: Default::default(),
        pins: Default::default(),
        read_identity: None,
    };
    let hash = r.store_mut().put(rdy_store::kinds::BLOCK, &prose).unwrap();
    snapshot.blocks.insert(prose.id.clone(), hash);
    let state = r
        .store_mut()
        .put(rdy_store::kinds::SNAPSHOT, &snapshot)
        .unwrap();
    let mut landing = r.store().landing(&head).unwrap();
    // a forged record from a read: it no longer has the read's identity
    landing.read_identity = None;
    landing.state = state;
    landing.prev = Some(head);
    let damaged = r
        .store_mut()
        .put(rdy_store::kinds::LANDING, &landing)
        .unwrap();
    r.invalidate();
    damaged
}

#[test]
fn ndh1_the_tree_export_leaves_out_blocks_outside_a_doc() {
    let (_dir, mut r) = project();
    let c = canon(&mut r);
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("x");
    let summary = r.read().export(&Scope::Canon, &dest).unwrap();
    for (path, text) in files_under(&dest.join("docs")) {
        for id in c.outside() {
            assert!(!text.contains(id.as_str()), "{path} holds {id}");
        }
    }
    let total = r.read().materialized(&Scope::Canon).unwrap().len();
    assert_eq!(summary.blocks, total - 3);

    // every other block outside the tree is still refused, with the same words
    let (_dir2, mut r2) = project();
    let damaged = orphan_prose_landing(&mut r2);
    let out2 = tempfile::tempdir().unwrap();
    let e = r2
        .read()
        .export(&Scope::At(damaged), &out2.path().join("x"))
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_IO",
            "1 blocks are not inside any folder or doc reachable from the top level (or lie more than 32 folders deep), so they cannot be exported",
            "run rdy fsck; move or delete those blocks, then export again"
        ),
        "{e}"
    );
}

#[test]
fn ndh1_the_working_copy_never_shows_a_block_outside_a_doc() {
    let (_dir, mut r) = project();
    let c = canon(&mut r);
    let p = r.read().wc_project(&Scope::Canon, None).unwrap();
    for f in &p.files {
        for id in c.outside() {
            assert!(!f.text.contains(id.as_str()), "{} holds {id}", f.path);
        }
    }
    for id in c.outside() {
        assert!(!p.blocks.contains_key(id.as_str()), "{id} is projected");
    }
    // the folder file (`tree_io::FOLDER_FILE`) holds only the folder's own entry
    let folder = p
        .files
        .iter()
        .find(|f| f.path == "area/_folder.yaml")
        .expect("area/_folder.yaml");
    assert_eq!(folder.id, c.area.as_str());
    let entry: serde_yaml_ng::Value = serde_yaml_ng::from_str(&folder.text).unwrap();
    assert!(entry.is_mapping(), "{}", folder.text);
    assert_eq!(
        entry.get("id").and_then(|v| v.as_str()),
        Some(c.area.as_str()),
        "{}",
        folder.text
    );
}
