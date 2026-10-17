//! cf1 (FW-CF1, D-X4GMH): a conflict on a block that a landing froze is resolved by taking the
//! landed side, a view op; the view then reads the frozen version, which equals canon, and the
//! change depends on every side. Every other edit stays E_FROZEN, whose fix names the op first.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

/// The frozen conflict of cres1 (`fixtures::frozen_conflict`), d conflicted in view b.
fn frozen_conflict() -> (tempfile::TempDir, Rdy, String, String) {
    let (dir, r, d, s, _cc) = frozen_conflict_c();
    (dir, r, d, s)
}

/// `frozen_conflict`, with agent:c's stale edit of d (in no other view).
fn frozen_conflict_c() -> (tempfile::TempDir, Rdy, String, String, String) {
    let (dir, mut r) = project();
    let (d, s, cc) = rdy_qa::fixtures::frozen_conflict(&mut r);
    let id = rdy_model::BlockId::parse(&d).unwrap();
    assert!(r.read().conflict_info(&view("b"), &id).unwrap().is_some());
    (dir, r, d, s, cc)
}

#[test]
fn cf1_taking_the_landed_side_reads_canons_frozen_version_and_depends_on_every_side() {
    let (_dir, mut r, d, s) = frozen_conflict();
    let b = as_agent("agent:b", "r1");
    let id = rdy_model::BlockId::parse(&d).unwrap();
    let info = r.read().conflict_info(&view("b"), &id).unwrap().unwrap();
    let side_changes: Vec<String> = info.sides.iter().filter_map(|s| s.change.clone()).collect();
    assert_eq!(side_changes.len(), 2, "{info:?}");
    let canon = r.read().block(&Scope::Canon, &id).unwrap();
    let res = r.view_resolve_landed(&b, "b", &d).unwrap();
    assert_eq!(res.view, "b");
    assert_eq!(res.block, d);
    assert_eq!(res.conflict, info.id);
    assert_eq!(res.version, canon.version_hash().to_string());
    // the view reads the frozen version, which equals canon; the conflict is gone
    assert_eq!(r.read().block(&view("b"), &id).unwrap(), canon);
    assert!(r.read().conflicts(&view("b")).unwrap().is_empty());
    // the change depends on every side, like any resolving write
    let deps: Vec<String> = r
        .read()
        .reader()
        .change(&res.change.parse().unwrap())
        .unwrap()
        .deps
        .iter()
        .map(|h| h.to_string())
        .collect();
    for c in &side_changes {
        assert!(deps.contains(c), "{c} missing from {deps:?}");
    }
    check(&r, &view("b")).unwrap();
    // a later write elsewhere in the view works, and the view lands with d unchanged
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "later"}]),
    )
    .unwrap();
    land(&mut r, "b").unwrap();
    assert_eq!(r.read().block(&Scope::Canon, &id).unwrap(), canon);
}

/// msw1's pinned case: b accepts d itself (not landed) and includes a's edit, which lands. The
/// frozen side did not land, so there is no landed side to take: E_FROZEN, as an edit gets, and
/// its fix does not offer the resolution.
#[test]
fn cf1_a_frozen_side_that_did_not_land_is_refused_with_e_frozen() {
    use rdy_model::{Change, ChangeLinks, FieldValue, Op};
    fn store_land(r: &mut Rdy, v: &str) {
        let p = r.store_mut().propose(v, v, writer().provenance()).unwrap();
        r.store_mut()
            .land_atomic(rdy_store::LandRequest {
                proposal: p,
                accept: None,
                approver: approver().provenance(),
                alt: None,
                relay: None,
                evidence: None,
            })
            .unwrap();
        r.invalidate();
    }
    let (_d, mut r) = project();
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}},
        {"op": "create", "type": "decision", "id": "$x", "parent": "$s", "fields": {"statement": "D", "status": "proposed"}}
    ])).unwrap();
    let d = seeded.created["$x"].clone();
    let id = rdy_model::BlockId::parse(&d).unwrap();
    store_land(&mut r, "seed");
    let vd = version_of(&r, &Scope::Canon, &d);
    r.view_create(&writer(), "b", None, true).unwrap();
    let mut acc = r.read().block(&Scope::Canon, &id).unwrap();
    acc.fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    r.store_mut()
        .commit_to_view(
            "b",
            Change {
                view: "b".into(),
                ops: vec![Op::Update {
                    base: vd.parse().unwrap(),
                    block: acc,
                }],
                deps: vec![],
                provenance: writer().provenance(),
                justification: None,
                message: "accept".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    let x = write(
        &mut r,
        "a",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by a"}}]),
    )
    .unwrap()
    .change;
    r.view_include(&writer(), "b", std::slice::from_ref(&x))
        .unwrap();
    store_land(&mut r, "a");
    assert!(r.read().conflict_info(&view("b"), &id).unwrap().is_some());
    let e = r.view_resolve_landed(&writer(), "b", &d).unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e}");
    assert!(!e.fix.contains("rdy view resolve"), "{e}");
    assert!(e.fix.contains("rdy view exclude"), "{e}");
    assert_eq!(r.read().conflicts(&view("b")).unwrap().len(), 1);
    // review round 2: the store refuses taking canon's side unchanged too, since canon's version
    // (a's edit, proposed) is not the frozen one: it would drop b's acceptance
    let canon: rdy_model::Hash = version_of(&r, &Scope::Canon, &d).parse().unwrap();
    let block = r.read().reader().get_block(&canon).unwrap();
    let conflicts = r.read().conflicts(&view("b")).unwrap().as_ref().clone();
    let m = r.read().materialized(&view("b")).unwrap();
    let e = r
        .store_mut()
        .commit_to_view_resolving(
            "b",
            Change {
                view: "b".into(),
                ops: vec![Op::Update { base: canon, block }],
                deps: vec![],
                provenance: writer().provenance(),
                justification: None,
                message: "take canon's side".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
            &|x| m.version_of(x),
            &conflicts,
            None,
        )
        .unwrap_err();
    assert!(matches!(e, rdy_store::StoreError::Frozen(_)), "{e:?}");
}

#[test]
fn cf1_a_block_with_no_conflict_is_refused() {
    let (_dir, mut r, d, s) = frozen_conflict();
    let b = as_agent("agent:b", "r1");
    let e = r.view_resolve_landed(&b, "b", &s).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(e.message.contains("has no conflict"), "{e}");
    assert!(e.fix.contains("rdy view show b"), "{e}");
    // and in canon's view a, which holds no conflict either
    let e = r
        .view_resolve_landed(&as_agent("agent:a", "r1"), "a", &d)
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}

#[test]
fn cf1_a_conflict_with_nothing_frozen_is_refused_pointing_to_a_write() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    let ca = write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": c, "base": v, "body": "a's"}]),
    )
    .unwrap()
    .change;
    let _ = ca;
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        json!([{"op": "update", "id": c, "base": v, "body": "b's"}]),
    )
    .unwrap()
    .change;
    r.view_include(&a, "a", std::slice::from_ref(&cb)).unwrap();
    assert_eq!(r.read().conflicts(&view("a")).unwrap().len(), 1);
    let e = r.view_resolve_landed(&a, "a", &c).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("no accepted or rejected decision"),
        "{e}"
    );
    assert!(e.fix.contains("base="), "{e}");
    assert_eq!(r.read().conflicts(&view("a")).unwrap().len(), 1);
}

#[test]
fn cf1_another_sessions_view_is_e_view_owner() {
    let (_dir, mut r, d, _s) = frozen_conflict();
    let e = r
        .view_resolve_landed(&as_agent("agent:c", "r1"), "b", &d)
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
}

#[test]
fn cf1_e_frozen_names_the_resolution_first() {
    let (_dir, mut r, d, _s) = frozen_conflict();
    let b = as_agent("agent:b", "r1");
    let id = rdy_model::BlockId::parse(&d).unwrap();
    let base = r
        .read()
        .conflict_info(&view("b"), &id)
        .unwrap()
        .unwrap()
        .base
        .unwrap();
    let e = write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "update", "id": d, "base": base, "fields": {"statement": "D again"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e}");
    assert!(
        e.message.contains("cannot be resolved by an edit"),
        "the message is kept: {e}"
    );
    let first = format!("take the landed side (rdy view resolve b {d} --landed)");
    assert!(e.fix.starts_with(&first), "{e}");
    assert!(
        e.fix.contains("rdy view exclude") && e.fix.contains("supersedes or revokes"),
        "the other ways out follow: {e}"
    );
}

/// Review round 1 (finding 1): the store takes a side unchanged through a frozen conflict only
/// when that side is canon's frozen version; the unlanded side is refused, so it can never land
/// over the accepted decision.
#[test]
fn cf1_the_store_refuses_taking_an_unlanded_side_unchanged() {
    use rdy_model::{Change, ChangeLinks, Op};
    let (_dir, mut r, d, _s) = frozen_conflict();
    let id = rdy_model::BlockId::parse(&d).unwrap();
    let canon = version_of(&r, &Scope::Canon, &d);
    let info = r.read().conflict_info(&view("b"), &id).unwrap().unwrap();
    let unlanded: rdy_model::Hash = info
        .sides
        .iter()
        .filter_map(|s| s.version.clone())
        .find(|v| *v != canon)
        .unwrap()
        .parse()
        .unwrap();
    let block = r.read().reader().get_block(&unlanded).unwrap();
    let conflicts = r.read().conflicts(&view("b")).unwrap().as_ref().clone();
    let m = r.read().materialized(&view("b")).unwrap();
    let b = as_agent("agent:b", "r1");
    let e = r
        .store_mut()
        .commit_to_view_resolving(
            "b",
            Change {
                view: "b".into(),
                ops: vec![Op::Update {
                    base: unlanded,
                    block,
                }],
                deps: vec![],
                provenance: b.provenance(),
                justification: None,
                message: "take b's side".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
            &|x| m.version_of(x),
            &conflicts,
            None,
        )
        .unwrap_err();
    assert!(matches!(e, rdy_store::StoreError::Frozen(_)), "{e:?}");
}

/// Review round 1 (finding 2): after a resolution, including another stale edit conflicts d
/// again, with the resolution as the frozen side; its version is canon's, so it resolves again.
#[test]
fn cf1_a_second_conflict_after_a_resolution_resolves_again() {
    let (_dir, mut r, d, _s, cc) = frozen_conflict_c();
    let b = as_agent("agent:b", "r1");
    let id = rdy_model::BlockId::parse(&d).unwrap();
    let canon = r.read().block(&Scope::Canon, &id).unwrap();
    r.view_resolve_landed(&b, "b", &d).unwrap();
    r.view_include(&b, "b", std::slice::from_ref(&cc)).unwrap();
    assert!(
        r.read().conflict_info(&view("b"), &id).unwrap().is_some(),
        "c's stale edit conflicts d again"
    );
    r.view_resolve_landed(&b, "b", &d).unwrap();
    assert!(r.read().conflicts(&view("b")).unwrap().is_empty());
    assert_eq!(r.read().block(&view("b"), &id).unwrap(), canon);
}

/// Review round 1 (finding 4): the landed side's key is checked against the view, as a
/// resolving write from a side is (cres1): the view reads the base, whose key another block may
/// have taken since.
#[test]
fn cf1_the_landed_sides_key_is_checked_against_the_view() {
    let (_dir, mut r) = project();
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    let s = seeded.created["$s"].clone();
    land(&mut r, "seed").unwrap();
    let (a, b) = (as_agent("agent:a", "r1"), as_agent("agent:b", "r1"));
    let d = write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": s,
        "fields": {"statement": "D", "status": "proposed"}}]),
    )
    .unwrap()
    .created["$x"]
        .clone();
    let vd = version_of(&r, &view("a"), &d);
    r.view_fork(&b, "a", "b").unwrap();
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by b"}}]),
    )
    .unwrap();
    // a gives d the key KK after the fork, then lands: the landed side carries KK
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"key": "KK"}}]),
    )
    .unwrap();
    land(&mut r, "a").unwrap();
    let id = rdy_model::BlockId::parse(&d).unwrap();
    assert!(r.read().conflict_info(&view("b"), &id).unwrap().is_some());
    // the view reads d without KK, so another block takes it
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "create", "type": "claim", "parent": s, "fields": {"key": "KK"}, "body": "z"}]),
    )
    .unwrap();
    let e = r.view_resolve_landed(&b, "b", &d).unwrap_err();
    assert_eq!(e.code, E_KEY_CLASH, "{e}");
    assert_eq!(r.read().conflicts(&view("b")).unwrap().len(), 1);
}

const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

/// Review round 1 (finding 3): the landed side is canon's version. In a pinned view, a landed
/// tracking edit that canon has since moved past is not the landed side: E_FROZEN, as an edit
/// gets, without the resolution.
#[test]
fn cf1_a_landed_side_that_canon_moved_past_is_not_taken() {
    use rdy_model::{EditorialClass, Justification};
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let d = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: section,
                statement: "Ship weekly".into(),
                fields: [("disposition".to_string(), json!("first"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "seed").unwrap();
    let id = rdy_model::BlockId::parse(&d).unwrap();
    let track = |r: &mut Rdy, v: &str, disposition: &str| {
        r.write(
            &writer(),
            WriteRequest {
                view: v.into(),
                ops: serde_json::from_value(
                    json!([{"op": "update", "id": d, "fields": {"disposition": disposition}}]),
                )
                .unwrap(),
                justification: Some(Justification::Editorial {
                    class: EditorialClass::Clarify,
                    reason: "track".into(),
                }),
                message: String::new(),
            },
        )
        .unwrap()
        .change
    };
    r.view_create(&writer(), "b", None, true).unwrap();
    track(&mut r, "b", "view b");
    let y = track(&mut r, "y", "old canon");
    r.view_include(&writer(), "b", std::slice::from_ref(&y))
        .unwrap();
    land(&mut r, "y").unwrap();
    track(&mut r, "z", "new canon");
    land(&mut r, "z").unwrap();
    assert_eq!(
        r.read()
            .block(&Scope::Canon, &id)
            .unwrap()
            .field_str("disposition"),
        Some("new canon")
    );
    assert!(r.read().conflict_info(&view("b"), &id).unwrap().is_some());
    let e = r.view_resolve_landed(&writer(), "b", &d).unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e}");
    assert!(!e.fix.contains("rdy view resolve"), "{e}");
}

/// Review round 2 (finding 1): the landed block is checked against the view as a write is. A
/// landing moved d into section B; the view still reads d under A, so it may delete B; taking
/// the landed side would then leave d under a parent the view lacks: refused, nothing written.
#[test]
fn cf1_a_landed_side_whose_parent_the_view_deleted_is_refused() {
    let (_dir, mut r) = project();
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "A"}},
        {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "B"}}
    ]))
    .unwrap();
    let (sa, sb) = (seeded.created["$a"].clone(), seeded.created["$b"].clone());
    land(&mut r, "seed").unwrap();
    let (a, b) = (as_agent("agent:a", "r1"), as_agent("agent:b", "r1"));
    let d = write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": sa,
        "fields": {"statement": "D", "status": "proposed"}}]),
    )
    .unwrap()
    .created["$x"]
        .clone();
    let vd = version_of(&r, &view("a"), &d);
    r.view_fork(&b, "a", "b").unwrap();
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by b"}}]),
    )
    .unwrap();
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "move", "id": d, "base": vd, "parent": sb}]),
    )
    .unwrap();
    land(&mut r, "a").unwrap();
    let id = rdy_model::BlockId::parse(&d).unwrap();
    assert!(r.read().conflict_info(&view("b"), &id).unwrap().is_some());
    let vb = version_of(&r, &view("b"), &sb);
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "delete", "id": sb, "base": vb}]),
    )
    .unwrap();
    let e = r.view_resolve_landed(&b, "b", &d).unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert!(e.message.contains(&sb), "{e}");
    assert_eq!(r.read().conflicts(&view("b")).unwrap().len(), 1);
    check(&r, &view("b")).unwrap();
}

const CYCLE_PACK: &str = "\
name: cyc
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    children: [section]
";

/// Review round 3: the landed block's parent chain is checked against the view. A landing moved
/// d under section B; the view, which reads d under A, moved B under d; taking the landed side
/// would make the loop d -> B -> d: refused, and the view is left as it was.
#[test]
fn cf1_a_landed_side_that_would_make_a_parent_loop_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/cyc.yaml"), CYCLE_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: cyc\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "A"}},
        {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "B"}}
    ]))
    .unwrap();
    let (sa, sb) = (seeded.created["$a"].clone(), seeded.created["$b"].clone());
    land(&mut r, "seed").unwrap();
    let (a, b) = (as_agent("agent:a", "r1"), as_agent("agent:b", "r1"));
    let d = write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": sa,
        "fields": {"statement": "D", "status": "proposed"}}]),
    )
    .unwrap()
    .created["$x"]
        .clone();
    let vd = version_of(&r, &view("a"), &d);
    r.view_fork(&b, "a", "b").unwrap();
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by b"}}]),
    )
    .unwrap();
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "move", "id": d, "base": vd, "parent": sb}]),
    )
    .unwrap();
    land(&mut r, "a").unwrap();
    let id = rdy_model::BlockId::parse(&d).unwrap();
    assert!(r.read().conflict_info(&view("b"), &id).unwrap().is_some());
    let vb = version_of(&r, &view("b"), &sb);
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "move", "id": sb, "base": vb, "parent": d}]),
    )
    .unwrap();
    let head = r.read().view_head("b").unwrap().head;
    let e = r.view_resolve_landed(&b, "b", &d).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(e.message.contains("loop") && e.fix.contains(&sb), "{e}");
    assert_eq!(r.read().view_head("b").unwrap().head, head);
    check(&r, &view("b")).unwrap();
}
