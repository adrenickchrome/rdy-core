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
    let (dir, mut r) = project();
    let (d, s) = rdy_qa::fixtures::frozen_conflict(&mut r);
    let id = rdy_model::BlockId::parse(&d).unwrap();
    assert!(r.read().conflict_info(&view("b"), &id).unwrap().is_some());
    (dir, r, d, s)
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
