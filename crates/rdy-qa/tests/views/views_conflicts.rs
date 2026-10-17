//! cres1: a conflicted block reads as its base with both sides listed; a write from the base or a
//! side resolves it; stale writes to clean blocks keep E_STALE_BASE.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_store::view::ViewOpKind;
use serde_json::json;

fn update(id: &str, base: Option<&str>, body: &str) -> serde_json::Value {
    let mut op = json!({"op": "update", "id": id, "body": body});
    if let Some(b) = base {
        op["base"] = json!(b);
    }
    json!([op])
}
fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

/// a and b edit the seed claim from canon; a includes b's change (through the store, since
/// include is built beside this task).
fn conflicted() -> (tempfile::TempDir, Rdy, String, String, String, String) {
    let (d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    let ca = write_as(&mut r, &a, "a", update(&c, Some(&v), "a's"))
        .unwrap()
        .change;
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, Some(&v), "b's"),
    )
    .unwrap()
    .change;
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include {
                changes: vec![cb.parse().unwrap()],
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    (d, r, c, v, ca, cb)
}

#[test]
fn cres1_a_conflicted_block_reads_as_its_base_with_both_sides() {
    let (_d, r, c, v, ca, cb) = conflicted();
    assert_eq!(body_of(&r, &view("a"), &c), "one");
    assert_eq!(version_of(&r, &view("a"), &c), v);
    let id = rdy_model::BlockId::parse(&c).unwrap();
    let info = r
        .read()
        .conflict_info(&view("a"), &id)
        .unwrap()
        .expect("conflicted");
    assert_eq!(info.sides.len(), 2);
    let by_change: std::collections::BTreeMap<String, &ConflictSide> = info
        .sides
        .iter()
        .map(|s| (s.change.clone().unwrap(), s))
        .collect();
    assert!(
        by_change[&ca].summary.starts_with("agent:a:"),
        "{:?}",
        by_change[&ca]
    );
    assert!(
        by_change[&cb].summary.contains("b's"),
        "{:?}",
        by_change[&cb]
    );
    assert!(
        r.read().conflict_info(&view("b"), &id).unwrap().is_none(),
        "b has only its own change"
    );
    assert!(
        r.read()
            .conflict_info(&Scope::Canon, &id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cres1_a_write_from_the_conflict_base_resolves_it_and_depends_on_both_sides() {
    let (_d, mut r, c, v, ca, cb) = conflicted();
    let a = as_agent("agent:a", "r1");
    let res = write_as(&mut r, &a, "a", update(&c, Some(&v), "merged")).unwrap();
    let deps: Vec<String> = r
        .read()
        .reader()
        .change(&res.change.parse().unwrap())
        .unwrap()
        .deps
        .iter()
        .map(|h| h.to_string())
        .collect();
    assert!(deps.contains(&ca) && deps.contains(&cb), "{deps:?}");
    assert!(r.read().conflicts(&view("a")).unwrap().is_empty());
    assert_eq!(body_of(&r, &view("a"), &c), "merged");
    check(&r, &view("a")).unwrap();
    let l = land(&mut r, "a").unwrap();
    assert!(!l.is_empty());
    assert_eq!(
        body_of(&r, &Scope::Canon, &c),
        "merged",
        "a resolved view lands"
    );
}

#[test]
fn cres1_a_write_from_a_sides_version_resolves_it_too() {
    let (_d, mut r, c, _v, _ca, _cb) = conflicted();
    let b_version = version_of(&r, &view("b"), &c);
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", update(&c, Some(&b_version), "b's, kept")).unwrap();
    assert!(r.read().conflicts(&view("a")).unwrap().is_empty());
    assert_eq!(body_of(&r, &view("a"), &c), "b's, kept");
}

#[test]
fn cres1_a_delete_resolves_a_conflict() {
    let (_d, mut r, c, v, _ca, _cb) = conflicted();
    let a = as_agent("agent:a", "r1");
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "delete", "id": c, "base": v}]),
    )
    .unwrap();
    assert!(r.read().conflicts(&view("a")).unwrap().is_empty());
    assert!(
        r.read()
            .materialized(&view("a"))
            .unwrap()
            .get(&rdy_model::BlockId::parse(&c).unwrap())
            .is_none()
    );
}

#[test]
fn cres1_a_write_without_a_base_or_with_a_foreign_base_is_refused_naming_the_conflict() {
    let (_d, mut r, c, _v, _ca, _cb) = conflicted();
    let a = as_agent("agent:a", "r1");
    let e = write_as(&mut r, &a, "a", update(&c, None, "x")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("conflicted") && e.fix.contains("base="),
        "{e}"
    );
    let e = write_as(
        &mut r,
        &a,
        "a",
        update(&c, Some(&rdy_model::Hash::of(b"nope").to_string()), "x"),
    )
    .unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
    assert!(
        e.message.contains("is neither its base") && e.message.contains("nor a side"),
        "{e}"
    );
    assert_eq!(
        r.read().conflicts(&view("a")).unwrap().len(),
        1,
        "nothing written"
    );
}

#[test]
fn cres1_own_view_stale_writes_on_clean_blocks_keep_e_stale_base() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    let v = version_of(&r, &view("seed"), &c);
    write(&mut r, "seed", update(&c, Some(&v), "two")).unwrap();
    let e = write(&mut r, "seed", update(&c, Some(&v), "three")).unwrap_err();
    assert_eq!(e.code, E_STALE_BASE, "{e}");
    assert!(
        e.message.contains("changed since you read it"),
        "the M1 text is unchanged: {e}"
    );
    assert!(!e.message.contains("conflicted"));
}

/// Fix round 1: a conflict one of whose sides is an accepted (frozen) decision cannot be
/// resolved by an edit, which would rewrite the accepted decision; the fix is to drop the stale
/// change. a proposes decision d; b, forked from a, edits d's statement; a lands, so the gate
/// accepts d; the landing rebases b, leaving d conflicted between b's edit and the acceptance.
#[test]
fn cres1_a_conflict_with_a_frozen_side_cannot_be_resolved_by_an_edit() {
    let (_d, mut r) = project();
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    let s = seeded.created["$s"].clone();
    land(&mut r, "seed").unwrap();
    // (1) a creates decision d, proposed
    let a = as_agent("agent:a", "r1");
    let b = as_agent("agent:b", "r1");
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
    // (2) b includes a's change and edits d from vd
    r.view_fork(&b, "a", "b").unwrap();
    write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by b"}}]),
    )
    .unwrap();
    // (3) a lands: the gate accepts d
    land(&mut r, "a").unwrap();
    let id = rdy_model::BlockId::parse(&d).unwrap();
    let accepted = r.read().block(&Scope::Canon, &id).unwrap();
    assert_eq!(accepted.field_str("status"), Some("accepted"));
    // (4) the landing rebased b: d is conflicted, b reads it at vd
    let info = r
        .read()
        .conflict_info(&view("b"), &id)
        .unwrap()
        .expect("d conflicted in b");
    assert_eq!(info.base.as_deref(), Some(vd.as_str()));
    // (5) resolving from the base or from either side is refused: a side is frozen
    let sides: Vec<String> = info
        .sides
        .iter()
        .filter_map(|s| s.version.clone())
        .collect();
    for base in std::iter::once(vd.clone()).chain(sides) {
        let e = write_as(
            &mut r,
            &b,
            "b",
            json!([{"op": "update", "id": d, "base": base, "fields": {"statement": "D rewritten"}}]),
        )
        .unwrap_err();
        assert_eq!(e.code, E_FROZEN, "base {base}: {e}");
        assert!(
            e.message.contains("cannot be resolved by an edit")
                && e.fix.contains("rdy view exclude"),
            "{e}"
        );
    }
    let e = write_as(
        &mut r,
        &b,
        "b",
        json!([{"op": "delete", "id": d, "base": vd}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e}");
    // canon d is unchanged and b still holds the conflict
    assert_eq!(r.read().block(&Scope::Canon, &id).unwrap(), accepted);
    assert_eq!(r.read().conflicts(&view("b")).unwrap().len(), 1);
}

/// Fix round 1: a resolution from a side carries the side's name; that name is checked against
/// the view, which reads the conflict's base, not against the side.
#[test]
fn cres1_a_resolution_from_a_side_checks_the_sides_name_against_the_view() {
    let (_d, mut r) = project();
    let (doc, _section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &doc);
    let a = as_agent("agent:a", "r1");
    let ca = write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": doc, "base": v, "fields": {"name": "other"}}]),
    )
    .unwrap();
    let a_version = version_of(&r, &view("a"), &doc);
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        json!([{"op": "update", "id": doc, "base": v, "fields": {"title": "B title"}}]),
    )
    .unwrap()
    .change;
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include {
                changes: vec![cb.parse().unwrap()],
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    assert_eq!(
        r.read().conflicts(&view("a")).unwrap().len(),
        1,
        "{}",
        ca.change
    );
    // nothing in the view holds "other" now: the view reads the doc as "seed"
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "create", "type": "doc",
        "fields": {"name": "other", "title": "Other", "doc_type": "page"}}]),
    )
    .unwrap();
    let e = write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": doc, "base": a_version, "fields": {"title": "Merged"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PATH_CLASH, "{e}");
    check(&r, &view("a")).unwrap();
}

/// Fix round 1: likewise a side's key is checked against the view.
#[test]
fn cres1_a_resolution_from_a_side_checks_the_sides_key_against_the_view() {
    let (_d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": c, "base": v, "fields": {"key": "KK"}}]),
    )
    .unwrap();
    let a_version = version_of(&r, &view("a"), &c);
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, Some(&v), "b's"),
    )
    .unwrap()
    .change;
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include {
                changes: vec![cb.parse().unwrap()],
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    assert_eq!(r.read().conflicts(&view("a")).unwrap().len(), 1);
    // nothing in the view holds "KK" now: the view reads c without it
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section,
        "fields": {"key": "KK"}, "body": "z"}]),
    )
    .unwrap();
    let e = write_as(&mut r, &a, "a", update(&c, Some(&a_version), "merged")).unwrap_err();
    assert_eq!(e.code, E_KEY_CLASH, "{e}");
    // a move (or a repin) from the side carries the side's key just the same
    let mv = json!([{"op": "move", "id": c, "base": a_version, "parent": section}]);
    let e = write_as(&mut r, &a, "a", mv).unwrap_err();
    assert_eq!(e.code, E_KEY_CLASH, "{e}");
    check(&r, &view("a")).unwrap();
}

#[test]
fn msw1_a_create_on_a_create_create_conflicted_id_is_refused_naming_the_conflict() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let mk = |body: &str| json!([{"op": "create", "type": "claim", "id": "c-PR0B1", "parent": section, "body": body}]);
    write(&mut r, "x", mk("x's")).unwrap();
    let cy = write(&mut r, "y", mk("y's")).unwrap().change;
    r.store_mut()
        .view_append(
            "x",
            vec![ViewOpKind::Include {
                changes: vec![cy.parse().unwrap()],
            }],
            &writer().provenance(),
        )
        .unwrap();
    r.invalidate();
    let cs = r.read().conflicts(&view("x")).unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].base, None, "a create/create conflict");
    let e = write(&mut r, "x", mk("third")).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS, "{e}");
    assert!(e.message.contains("conflicted"), "{e}");
    assert_eq!(
        r.read().conflicts(&view("x")).unwrap().len(),
        1,
        "nothing written"
    );
}

#[test]
fn msw1_a_frozen_conflict_names_only_changes_the_view_can_exclude() {
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
    store_land(&mut r, "seed"); // canon holds d, proposed
    let vd = version_of(&r, &Scope::Canon, &d);
    // b accepts d through the store (only the gate writes status: accepted)
    let mut acc = r.read().block(&Scope::Canon, &id).unwrap();
    acc.fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    let cb = r
        .store_mut()
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
        .unwrap()
        .to_string();
    r.invalidate();
    // a edits d's statement from vd; it lands, still proposed
    let x = write(
        &mut r,
        "a",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by a"}}]),
    )
    .unwrap()
    .change;
    store_land(&mut r, "a");
    assert!(
        r.read().conflict_info(&view("b"), &id).unwrap().is_some(),
        "d conflicted in b"
    );
    let e = write(
        &mut r,
        "b",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D merged"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e}");
    assert!(
        !e.fix.contains(&x),
        "x landed: the view cannot exclude it: {e}"
    );
    assert_eq!(
        r.view_exclude(&writer(), "b", std::slice::from_ref(&x), false)
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert!(e.fix.contains(&cb), "cb is b's own: the hint names it: {e}");
    r.view_exclude(&writer(), "b", std::slice::from_ref(&cb), false)
        .unwrap();
    assert!(r.read().conflicts(&view("b")).unwrap().is_empty());
}

/// Final review M7: a pinned view is never rebased, so it still includes a side change that
/// landed after its pin; the E_FROZEN hint names it too, since the view can exclude it.
#[test]
fn msw1_a_frozen_conflict_in_a_pinned_view_names_a_landed_side_it_includes() {
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
    store_land(&mut r, "seed"); // canon holds d, proposed
    let vd = version_of(&r, &Scope::Canon, &d);
    r.view_create(&writer(), "b", None, true).unwrap(); // pinned
    // b accepts d through the store (only the gate writes status: accepted)
    let mut acc = r.read().block(&Scope::Canon, &id).unwrap();
    acc.fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    let cb = r
        .store_mut()
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
        .unwrap()
        .to_string();
    r.invalidate();
    // a edits d's statement from vd; b includes it, then it lands; b is pinned, so it keeps it
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
    assert!(
        r.read().included("b").iter().any(|h| h.to_string() == x),
        "the pinned view still includes x"
    );
    assert!(
        r.read().conflict_info(&view("b"), &id).unwrap().is_some(),
        "d conflicted in b"
    );
    let e = write(
        &mut r,
        "b",
        json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D merged"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e}");
    assert!(e.fix.contains(&cb), "{e}");
    assert!(
        e.fix.contains(&x),
        "x landed, but b includes it and can exclude it: {e}"
    );
    r.view_exclude(&writer(), "b", std::slice::from_ref(&x), false)
        .unwrap();
    assert!(r.read().conflicts(&view("b")).unwrap().is_empty());
}

/// msw1 (#1160, fix round 1): an import is a second way to create a block. A create/create
/// conflict has no base, so the view reads the block as absent; the import must still refuse it,
/// as a write does, and leave the conflict in place.
#[test]
fn msw1_an_import_never_resolves_a_create_create_conflict() {
    let (_d, mut r) = project();
    let mk = |title: &str| {
        json!([{"op": "create", "type": "doc", "id": "d-D36QY",
                "fields": {"name": "dd", "title": title, "doc_type": "overview"}}])
    };
    write(&mut r, "x", mk("X title")).unwrap();
    let cy = write(&mut r, "y", mk("Y title")).unwrap().change;
    r.store_mut()
        .view_append(
            "x",
            vec![ViewOpKind::Include {
                changes: vec![cy.parse().unwrap()],
            }],
            &writer().provenance(),
        )
        .unwrap();
    r.invalidate();
    let cs = r.read().conflicts(&view("x")).unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].base, None, "a create/create conflict");
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("export");
    r.read().export(&view("y"), &dir).unwrap();
    let e = r.import(&approver(), "x", &dir).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS, "{e}");
    assert!(e.message.contains("conflicted"), "{e}");
    assert_eq!(
        r.read().conflicts(&view("x")).unwrap().len(),
        1,
        "nothing imported"
    );
}

/// msw1 (fix round 1): the frozen check reads a side's landing only once it knows a side or the
/// base is frozen, so a damaged landed entry does not block the resolution of a plain conflict.
#[test]
fn msw1_a_plain_conflict_resolves_even_when_a_sides_landed_entry_is_damaged() {
    let (_d, mut r, c, v, ca, cb) = conflicted();
    let cbh: rdy_model::Hash = cb.parse().unwrap();
    r.store_mut()
        .set_landed_raw_for_tests(&cbh, &[0xAB; 3])
        .unwrap();
    r.invalidate();
    assert!(
        r.store_mut().landing_of(&cbh).is_err(),
        "the damaged entry reads as an error"
    );
    let a = as_agent("agent:a", "r1");
    let res = write_as(&mut r, &a, "a", update(&c, Some(&v), "merged")).unwrap();
    let deps: Vec<String> = r
        .read()
        .reader()
        .change(&res.change.parse().unwrap())
        .unwrap()
        .deps
        .iter()
        .map(|h| h.to_string())
        .collect();
    assert!(deps.contains(&ca) && deps.contains(&cb), "{deps:?}");
    assert!(r.read().conflicts(&view("a")).unwrap().is_empty());
    assert_eq!(body_of(&r, &view("a"), &c), "merged");
}
