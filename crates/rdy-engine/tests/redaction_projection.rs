//! rdx3: computed reads over removed content (D-3M81W, D-CC90E). A removed block version reads as
//! a stub that keeps its recorded identity: materialization and history never hash the stub,
//! paths name it by its id, diffs show the notice, and the working copy writes the notice
//! without submitting it back. The tests build explicit projections with Task 1's model.
use rdy_engine::Materialized;
use rdy_engine::check_path;
use rdy_engine::diff::{DiffChange, diff_states};
use rdy_engine::wc::files::checkout;
use rdy_engine::wc::ignore::Ignore;
use rdy_engine::wc::layout::read_tree;
use rdy_engine::wc::merge::{apply_update, plan_update};
use rdy_engine::wc::plan::plan_commit;
use rdy_engine::wc::project::{Projection, project_state};
use rdy_engine::wc::state::{Place, WcState, stage_put};
use rdy_engine::worddiff::{Op as PieceOp, Piece};
use rdy_graph::Graph;
use rdy_model::decision::{self, is_frozen};
use rdy_model::redaction::{BlockResidue, CONTENT_REMOVED, ChangeResidue};
use rdy_model::{
    Block, BlockId, Change, ChangeLinks, FieldValue, Hash, Link, Op, Provenance, Role, Tracking,
    hash_of,
};
use rdy_schema::{Pack, PackSet, validate_block, validate_read_block};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

fn pack() -> Pack {
    PackSet::with_builtin().resolve("rdy-standard").unwrap()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn blk(i: &str, ty: &str, parent: Option<&str>, order: &str, body: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: parent.map(id),
        order: order.into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn with_fields(mut b: Block, fields: &[(&str, &str)]) -> Block {
    for (k, v) in fields {
        b.fields.insert((*k).into(), FieldValue::Str((*v).into()));
    }
    b
}

fn doc(i: &str, parent: Option<&str>, name: &str, title: &str) -> Block {
    with_fields(
        blk(i, "doc", parent, "V", ""),
        &[("name", name), ("title", title), ("doc_type", "page")],
    )
}

fn section(i: &str, parent: &str, title: &str) -> Block {
    with_fields(
        blk(i, "section", Some(parent), "V", ""),
        &[("title", title)],
    )
}

fn decision(i: &str, status: &str) -> Block {
    with_fields(
        blk(i, "decision", Some("s-SECT1"), "V", ""),
        &[
            ("status", status),
            ("statement", "the removed statement"),
            ("review_by", "2027-01-01"),
        ],
    )
}

fn prov() -> Provenance {
    Provenance {
        actor: "agent:a".into(),
        run: Some("r1".into()),
        role: Role::Writer,
        at_ms: 1_759_449_600_000,
    }
}

fn change(ops: Vec<Op>, deps: Vec<Hash>, message: &str) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps,
        provenance: prov(),
        justification: None,
        message: message.into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn creates(blocks: &[&Block]) -> Vec<Op> {
    blocks
        .iter()
        .map(|b| Op::Create {
            block: (*b).clone(),
        })
        .collect()
}

fn request() -> Hash {
    Hash::of(b"the removal request")
}

fn stub_of(b: &Block) -> Block {
    BlockResidue::of(b).stub(request())
}

/// A state of `blocks`, each removed one replaced by its stub.
fn state(blocks: &[&Block], removed: &[&str]) -> Materialized {
    let ops: Vec<Op> = blocks
        .iter()
        .map(|b| Op::Create {
            block: if removed.contains(&b.id.as_str()) {
                stub_of(b)
            } else {
                (*b).clone()
            },
        })
        .collect();
    Materialized::default().with(&ops)
}

fn json(v: &impl serde::Serialize) -> String {
    serde_json::to_string(v).unwrap()
}

#[test]
fn rdx3_history_and_materialization_never_hash_the_stub() {
    let d = doc("d-D0C11", None, "plan", "Plan");
    let s = section("s-SECT1", "d-D0C11", "Goals");
    let a1 = blk("p-AAAAA", "prose", Some("s-SECT1"), "V", "Alpha stays.");
    let b1 = blk(
        "p-BBBBB",
        "prose",
        Some("s-SECT1"),
        "W",
        "the canary-b1 text",
    );
    let c = blk("p-CCCCC", "prose", Some("s-SECT1"), "X", "Gamma stays.");
    let c1 = change(creates(&[&d, &s, &a1, &b1, &c]), vec![], "seed");
    let mut b2 = b1.clone();
    b2.body = "the canary-b2 text".into();
    let mut a2 = a1.clone();
    a2.body = "Alpha moves on.".into();
    let c2 = change(
        vec![
            Op::Update {
                base: b1.version_hash(),
                block: b2.clone(),
            },
            Op::Update {
                base: a1.version_hash(),
                block: a2.clone(),
            },
        ],
        vec![c1.id()],
        "edit",
    );

    // every version of p-BBBBB is removed; the changes are read through their residues
    let originals: BTreeMap<Hash, Block> = [&d, &s, &a1, &a2, &b1, &b2, &c]
        .into_iter()
        .map(|b| (b.version_hash(), b.clone()))
        .collect();
    let removed: BTreeMap<Hash, BlockResidue> = [&b1, &b2]
        .into_iter()
        .map(|b| (b.version_hash(), BlockResidue::of(b)))
        .collect();
    let resolve = |i: &BlockId, v: Hash| -> Result<Block, String> {
        match removed.get(&v) {
            Some(r) => {
                assert_eq!(&r.id, i);
                Ok(r.stub(request()))
            }
            None => originals.get(&v).cloned().ok_or(format!("no version {v}")),
        }
    };
    let p1 = ChangeResidue::of(&c1).project(resolve).unwrap();
    let p2 = ChangeResidue::of(&c2).project(resolve).unwrap();
    for (orig, proj) in [(&c1, &p1), (&c2, &p2)] {
        assert!(proj.is_projection());
        assert_eq!(proj.id(), orig.id(), "the change keeps its recorded id");
        assert_eq!(proj.deps, orig.deps);
        assert_eq!(proj.provenance, orig.provenance);
        assert_eq!(proj.message, orig.message);
        assert_eq!(proj.ops.len(), orig.ops.len());
        for (o, p) in orig.ops.iter().zip(&proj.ops) {
            assert_eq!(o.block_id(), p.block_id(), "op order is kept");
            assert_eq!(o.base(), p.base(), "op bases are kept");
            let (Op::Create { block: ob } | Op::Update { block: ob, .. }) = o else {
                unreachable!()
            };
            let (Op::Create { block: pb } | Op::Update { block: pb, .. }) = p else {
                unreachable!()
            };
            assert_eq!(pb.version_hash(), ob.version_hash());
            if ob.id == b1.id {
                assert!(pb.is_redacted());
                assert_eq!(pb.body, CONTENT_REMOVED);
                assert!(pb.fields.is_empty() && pb.links.is_empty() && pb.pins.is_empty());
                assert_ne!(hash_of(pb), ob.version_hash());
            } else {
                assert_eq!(pb, ob, "an unaffected sibling op is unchanged");
            }
        }
    }

    // history replay: the projected changes give the original versions and snapshot ids
    let m_orig = Materialized::default().with(&c1.ops).with(&c2.ops);
    let m_p1 = Materialized::default().with(&p1.ops);
    let m_proj = m_p1.with(&p2.ops);
    assert_eq!(m_proj.snapshot(), m_orig.snapshot());
    assert_eq!(m_proj.snapshot_id(), m_orig.snapshot_id());
    assert_eq!(m_proj.version_of(&b1.id), Some(b2.version_hash()));
    assert_ne!(
        m_proj.version_of(&b1.id),
        Some(hash_of(m_proj.get(&b1.id).unwrap())),
        "the notice is never hashed as a version"
    );
    for i in [&d.id, &s.id, &a1.id, &c.id] {
        assert_eq!(m_proj.get(i), m_orig.get(i), "{i} stays readable");
    }
    assert_eq!(m_proj.get(&b1.id).unwrap().body, CONTENT_REMOVED);
    let rebuilt = Materialized::build(
        m_orig.snapshot(),
        m_proj.iter().map(|b| (b.id.clone(), b.clone())).collect(),
    );
    assert_eq!(rebuilt.snapshot(), m_orig.snapshot());

    // the removal itself changes no version, so the diff across it is empty
    assert!(diff_states(&m_orig, &m_proj).entries.is_empty());

    // the history step c2, read through the projection: the stub keeps its ids and the notice
    let step = diff_states(&m_p1, &m_proj);
    assert_eq!(step.entries.len(), 2, "{step:#?}");
    let eb = step.entries.iter().find(|e| e.id == b1.id).unwrap();
    assert_eq!(eb.from_version, Some(b1.version_hash()));
    assert_eq!(eb.to_version, Some(b2.version_hash()));
    assert_eq!(eb.change, DiffChange::Updated);
    assert!(eb.body.is_none() && eb.fields.is_empty() && eb.links.is_none());
    let ea = step.entries.iter().find(|e| e.id == a1.id).unwrap();
    assert_eq!(ea.from_version, Some(a1.version_hash()));
    assert!(ea.body.is_some());
    // a removed block added on one side shows only the notice
    let added = diff_states(&Materialized::default(), &m_p1);
    let eb = added.entries.iter().find(|e| e.id == b1.id).unwrap();
    assert_eq!(eb.change, DiffChange::Added);
    assert_eq!(eb.to_version, Some(b1.version_hash()));
    assert_eq!(
        eb.body,
        Some(vec![Piece {
            op: PieceOp::Add,
            text: CONTENT_REMOVED.into()
        }])
    );
    for d in [&step, &added] {
        assert!(!json(d).contains("canary"), "{}", json(d));
    }

    // a new, non-erased version after the removal: the notice is replaced whole, never word by
    // word against new text
    let mut b3 = blk("p-BBBBB", "prose", Some("s-SECT1"), "W", "");
    b3.body = "Content was rewritten.".into();
    let m3 = m_proj.with(&[Op::Update {
        base: b2.version_hash(),
        block: b3.clone(),
    }]);
    let next = diff_states(&m_proj, &m3);
    assert_eq!(next.entries.len(), 1, "{next:#?}");
    let e = &next.entries[0];
    assert_eq!(e.id, b1.id);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.from_version, Some(b2.version_hash()));
    assert_eq!(e.to_version, Some(b3.version_hash()));
    assert_eq!(
        e.body,
        Some(vec![
            Piece {
                op: PieceOp::Del,
                text: CONTENT_REMOVED.into()
            },
            Piece {
                op: PieceOp::Add,
                text: "Content was rewritten.".into()
            }
        ])
    );
    let back = diff_states(&m3, &m_proj);
    assert_eq!(
        back.entries[0].body,
        Some(vec![
            Piece {
                op: PieceOp::Del,
                text: "Content was rewritten.".into()
            },
            Piece {
                op: PieceOp::Add,
                text: CONTENT_REMOVED.into()
            }
        ])
    );
}

#[test]
fn rdx3_structural_stubs_keep_unaffected_children_readable() {
    let f = with_fields(
        blk("fd-F0DR1", "folder", None, "V", ""),
        &[("name", "secret-folder"), ("title", "Secret folder")],
    );
    let d = doc("d-D0C11", Some("fd-F0DR1"), "plan", "Plan");
    let s = section("s-SECT1", "d-D0C11", "Secret heading");
    let p = blk("p-PR0S1", "prose", Some("s-SECT1"), "V", "Kept text.");
    let gone = with_fields(
        blk("d-D0C44", "doc", None, "W", ""),
        &[
            ("name", "secret-doc"),
            ("title", "Secret doc"),
            ("doc_type", "page"),
        ],
    );
    // a doc from M1 without a stored name keeps its derived name
    let old = with_fields(
        blk("d-D0C33", "doc", None, "X", ""),
        &[("title", "Old"), ("doc_type", "page")],
    );
    let all = [&f, &d, &s, &p, &gone, &old];
    let m = state(&all, &["fd-F0DR1", "s-SECT1", "d-D0C44"]);

    // the removed folder and doc are named by their ids; no removed name or title is shown
    assert_eq!(m.name_of(&f.id).as_deref(), Some("fd-F0DR1"));
    assert_eq!(m.name_of(&gone.id).as_deref(), Some("d-D0C44"));
    let top: BTreeSet<String> = m.names_under(None).into_values().collect();
    assert_eq!(
        top,
        BTreeSet::from(["fd-F0DR1".into(), "d-D0C44".into(), "old".into()])
    );
    for (i, label) in [
        (&f.id, CONTENT_REMOVED),
        (&s.id, CONTENT_REMOVED),
        (&gone.id, CONTENT_REMOVED),
        (&d.id, "Plan"),
    ] {
        assert_eq!(m.label_of(i), Some(label), "{i}");
    }
    assert_eq!(m.label_of(&p.id), None, "a prose block has no label");

    // the descendants stay reachable through the neutral segment
    assert_eq!(m.path_of(&d.id).as_deref(), Some("fd-F0DR1/plan"));
    assert_eq!(m.path_of(&s.id).as_deref(), Some("fd-F0DR1/plan"));
    assert_eq!(m.path_of(&p.id).as_deref(), Some("fd-F0DR1/plan"));
    assert_eq!(m.resolve_path("fd-F0DR1/plan"), Ok(d.id.clone()));
    assert_eq!(m.resolve_path("FD-F0DR1/plan"), Ok(d.id.clone()));
    assert_eq!(m.resolve_path("d-D0C44"), Ok(gone.id.clone()));
    assert!(m.resolve_path("secret-folder/plan").is_err());
    assert_eq!(m.folder_depth(&d.id), 1);
    let kids: Vec<&BlockId> = m.children(Some(&s.id)).iter().map(|b| &b.id).collect();
    assert_eq!(kids, vec![&p.id]);
    assert_eq!(m.doc_of(&p.id).map(|b| &b.id), Some(&d.id));
    assert_eq!(m.get(&p.id).unwrap().body, "Kept text.");
    let miss = m.resolve_path("fd-F0DR1/nothing").unwrap_err();
    assert_eq!(miss.reached.as_deref(), Some("fd-F0DR1"));
    assert_eq!(miss.children, vec!["plan".to_string()]);

    // a diff names removed docs and folders by their ids
    let diff = diff_states(&Materialized::default(), &m);
    let ef = diff.entries.iter().find(|e| e.id == f.id).unwrap();
    assert_eq!(ef.path.as_deref(), Some("fd-F0DR1"));
    let ed = diff.entries.iter().find(|e| e.id == d.id).unwrap();
    assert_eq!(ed.path.as_deref(), Some("fd-F0DR1/plan"));
    assert!(!json(&diff).contains("ecret"), "{}", json(&diff));

    // a removed doc is no doc from M1: the check counts only the nameless one
    let empty = tempfile::tempdir().unwrap();
    let diags = check_path(&pack(), Some(&m), empty.path()).unwrap();
    let nameless: Vec<_> = diags
        .iter()
        .filter(|d| d.code == "W_NAMELESS_DOC")
        .collect();
    assert_eq!(nameless.len(), 1, "{diags:#?}");
    assert!(
        nameless[0]
            .message
            .starts_with("1 doc in canon has no stored name"),
        "{:?}",
        nameless[0]
    );
    assert!(nameless[0].message.contains("d-D0C33"));
    assert_eq!(
        nameless[0].fix,
        "write to each once (any edit) to store its name"
    );
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

fn project(m: &Materialized) -> Projection {
    project_state(m, &pack(), "work", "canon:test", None).unwrap()
}

#[test]
fn rdx3_working_copy_update_is_safe_and_old_files_are_reported() {
    let pack = pack();
    let f = with_fields(
        blk("fd-F0DR1", "folder", None, "V", ""),
        &[("name", "notes")],
    );
    let d = doc("d-D0C11", Some("fd-F0DR1"), "plan", "Plan");
    let s = section("s-SECT1", "d-D0C11", "Secret heading");
    let p1 = blk("p-PR0S1", "prose", Some("s-SECT1"), "V", "the canary text");
    let p2 = blk("p-PR0S2", "prose", Some("s-SECT1"), "W", "Kept words.");
    let all = [&f, &d, &s, &p1, &p2];
    let before = state(&all, &[]);
    let after = state(&all, &["s-SECT1", "p-PR0S1"]);
    let file = "notes/plan.yaml";

    // checkout of the state before the removal
    let dir = tempfile::tempdir().unwrap();
    let place = Place::main(dir.path());
    let mut st = WcState::default();
    let tree = read_tree(&place.root, &Ignore::empty(), &pack);
    checkout(&place, &mut st, &tree, &project(&before), false).unwrap();
    assert!(read(&place.root, file).contains("the canary text"));

    // the store's projection after the removal holds the notice and the original versions
    let theirs = project(&after);
    let text = &theirs.files.iter().find(|x| x.path == file).unwrap().text;
    assert!(text.contains(CONTENT_REMOVED), "{text}");
    for secret in ["canary", "Secret heading"] {
        assert!(!json(&theirs).contains(secret), "{}", json(&theirs));
    }
    assert_eq!(
        theirs.blocks["p-PR0S1"].version,
        p1.version_hash().to_string()
    );
    assert_eq!(
        theirs.blocks["s-SECT1"].version,
        s.version_hash().to_string()
    );

    // a fresh checkout writes the notice
    let fresh = tempfile::tempdir().unwrap();
    let fresh_place = Place::main(fresh.path());
    let mut fresh_st = WcState::default();
    let fresh_tree = read_tree(&fresh_place.root, &Ignore::empty(), &pack);
    checkout(&fresh_place, &mut fresh_st, &fresh_tree, &theirs, false).unwrap();
    let fresh_text = read(&fresh_place.root, file);
    assert!(fresh_text.contains(CONTENT_REMOVED) && !fresh_text.contains("canary"));

    // the old file stays as it was until the person updates: planning writes nothing, and the
    // plan reports the file it rewrites
    let st = WcState::load(&place).unwrap();
    let tree = read_tree(&place.root, &Ignore::empty(), &pack);
    let plan = plan_update(&place, &st, &tree, &theirs, &pack).unwrap();
    assert!(read(&place.root, file).contains("the canary text"));
    assert!(
        std::fs::read_to_string(place.base_file(file))
            .unwrap()
            .contains("the canary text")
    );
    let written: Vec<&str> = plan.writes.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(written, vec![file]);
    assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
    for (_, t) in plan.writes.iter().chain(&plan.base_files) {
        assert!(
            !t.contains("canary") && !t.contains("Secret heading"),
            "{t}"
        );
    }
    let mut st = st;
    apply_update(&place, &mut st, &plan, &theirs).unwrap();
    let updated = read(&place.root, file);
    assert!(updated.contains(CONTENT_REMOVED), "{updated}");
    assert!(!updated.contains("canary") && !updated.contains("Secret heading"));
    assert!(
        !std::fs::read_to_string(place.base_file(file))
            .unwrap()
            .contains("canary")
    );

    // staging the file unchanged submits nothing
    let st = WcState::load(&place).unwrap();
    stage_put(&place, file, updated.as_bytes()).unwrap();
    let unchanged = plan_commit(&place, &st, &pack).unwrap_or_else(|e| panic!("{:#?}", e.diags));
    assert!(unchanged.ops.is_empty(), "{:?}", unchanged.ops);

    // an edit beside the notice commits only that edit: the projected entries are not
    // submitted as new source
    let edited = updated.replace("Kept words.", "Kept words, edited.");
    assert_ne!(edited, updated);
    std::fs::write(place.root.join(file), &edited).unwrap();
    stage_put(&place, file, edited.as_bytes()).unwrap();
    let plan = plan_commit(&place, &st, &pack).unwrap_or_else(|e| panic!("{:#?}", e.diags));
    assert_eq!(plan.ops.len(), 1, "{:?}", plan.ops);
    let op = &plan.ops[0];
    assert_eq!(op["op"], "update");
    assert_eq!(op["id"], "p-PR0S2");
    assert_eq!(op["base"], p2.version_hash().to_string().as_str());
    assert_eq!(op["body"], "Kept words, edited.");
    assert!(!plan.touched.contains(&s.id) && !plan.touched.contains(&p1.id));

    // an edit of the notice text itself is the person's new source: normal checks apply
    let forged = updated.replacen(CONTENT_REMOVED, "Content restored?", 1);
    stage_put(&place, file, forged.as_bytes()).unwrap();
    let refused = plan_commit(&place, &st, &pack);
    match refused {
        Err(e) => assert!(
            e.diags.iter().any(|d| d.code == "E_FIELD_REQUIRED"),
            "{:#?}",
            e.diags
        ),
        Ok(p) => panic!("an edited section stub passed the lint: {:?}", p.ops),
    }
}

#[test]
fn rdx3_pins_and_frozen_decisions_keep_their_guards() {
    let pack = pack();
    let t = Arc::new(Tracking::new(BTreeMap::from([(
        "decision".to_string(),
        BTreeSet::from(["review_by".to_string()]),
    )])));
    let sect = section("s-SECT1", "d-D0C11", "Decisions");
    let acc = decision("D-ACC11", "accepted");
    let rej = decision("D-REJ11", "rejected");
    let draft = decision("D-PR0P1", "proposed");
    let mut premise = blk(
        "c-PREM1",
        "claim",
        Some("s-SECT1"),
        "W",
        "This holds while {{D-ACC11}} does.",
    );
    premise
        .links
        .entry("decided_by".into())
        .or_default()
        .push(Link {
            target: acc.id.clone(),
            part: None,
        });
    let pin = acc.content_hash_with(&t);
    premise.pins.insert(acc.id.clone(), pin);
    let all = [&sect, &acc, &rej, &draft, &premise];
    let m = state(&all, &["D-ACC11", "D-REJ11", "D-PR0P1"]);

    // accepted and rejected stubs stay frozen; a draft stub does not become frozen, and no
    // status is inferred from the removed fields
    for (i, frozen) in [(&acc.id, true), (&rej.id, true), (&draft.id, false)] {
        let b = m.get(i).unwrap();
        assert!(b.is_redacted());
        assert_eq!(is_frozen(b), frozen, "{i}");
        assert_eq!(decision::status(b), None, "{i}");
    }
    // a new version over a stub is never a tracking-only edit: the frozen guard applies
    let mut next = acc.clone();
    next.fields
        .insert("review_by".into(), FieldValue::Str("2028-01-01".into()));
    assert!(decision::only_tracking_changed(&acc, &next, &t));
    assert!(!decision::only_tracking_changed(
        m.get(&acc.id).unwrap(),
        &next,
        &t
    ));

    // the read schema keeps the frozen guard's fix; the write schema ignores the marker
    for (i, frozen) in [(&acc.id, true), (&draft.id, false)] {
        let mut b = m.get(i).unwrap().clone();
        assert!(
            validate_read_block(&pack, &b, Some(&sect)).is_empty(),
            "{i}"
        );
        assert!(
            validate_block(&pack, &b, Some(&sect))
                .iter()
                .any(|x| x.code == "E_FIELD_REQUIRED"),
            "{i}: a stub never passes as a written block"
        );
        b.order = String::new();
        let issues = validate_read_block(&pack, &b, Some(&sect));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].code, "E_ORDER_KEY");
        assert_eq!(
            issues[0].fix.contains("this decision is frozen"),
            frozen,
            "{:?}",
            issues[0]
        );
    }

    // the surviving pin keeps its value and names the request; no false suspect
    assert_eq!(m.get(&premise.id).unwrap().pins, premise.pins);
    let g = Graph::build_with(m.iter(), t.clone());
    assert!(g.suspects_of(&premise.id).is_empty(), "{:?}", g.suspects());
    assert_eq!(
        g.redacted_pins_of(&premise.id),
        vec![(acc.id.clone(), request())]
    );
    // a new, non-erased version is checked against the pin again
    let mut rewritten = decision("D-ACC11", "accepted");
    rewritten.fields.insert(
        "statement".into(),
        FieldValue::Str("a new statement".into()),
    );
    let stub = m.get(&acc.id).unwrap().clone();
    let m2 = m.with(&[Op::Update {
        base: acc.version_hash(),
        block: rewritten.clone(),
    }]);
    let g2 = g.with_changes([&stub], [&rewritten]);
    assert_eq!(g2, Graph::build_with(m2.iter(), t));
    let reasons: Vec<String> = g2
        .suspects_of(&premise.id)
        .into_iter()
        .map(|s| s.reason)
        .collect();
    assert_eq!(reasons, vec!["changed".to_string()]);
    assert!(g2.redacted_pins_of(&premise.id).is_empty());
    assert!(is_frozen(m2.get(&acc.id).unwrap()));
}
