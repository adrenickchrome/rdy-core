//! incl1: include adds the requested changes and their missing deps; excl1: exclude refuses
//! with dependents unless cascaded.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}
fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

#[test]
fn incl1_include_adds_the_requested_changes_and_their_missing_deps_and_lists_them() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "x")).unwrap();
    let x = c1.created.values().next().unwrap().clone();
    let v1 = version_of(&r, &view("a"), &x);
    let c2 = write_as(&mut r, &a, "a", update(&x, &v1, "x2")).unwrap();
    let b = as_agent("agent:b", "r1");
    let res = r
        .view_include(&b, "b", std::slice::from_ref(&c2.change))
        .unwrap();
    assert_eq!(
        (res.added, res.added_deps),
        (vec![c2.change.clone()], vec![c1.change.clone()])
    );
    assert_eq!((res.changes, res.view.as_str()), (2, "b"));
    assert!(res.conflicts.is_empty(), "{:?}", res.conflicts);
    assert_eq!(body_of(&r, &view("b"), &x), "x2");
    let again = r
        .view_include(&b, "b", &[c2.change.clone(), c1.change.clone()])
        .unwrap();
    assert!(
        again.added.is_empty() && again.added_deps.is_empty(),
        "already included changes are skipped: {again:?}"
    );
    assert_eq!(r.read().included("b").len(), 2);
    check(&r, &view("b")).unwrap();
    r.fsck().unwrap();
}

#[test]
fn incl1_a_dep_in_the_base_is_not_added_and_a_landed_change_is_refused_with_its_landing() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    let l1 = land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let v = version_of(&r, &Scope::Canon, &c);
    let c2 = write_as(&mut r, &a, "a", update(&c, &v, "two")).unwrap();
    let res = r
        .view_include(
            &as_agent("agent:b", "r1"),
            "b",
            std::slice::from_ref(&c2.change),
        )
        .unwrap();
    assert!(
        res.added_deps.is_empty(),
        "the producer of canon's version is in the base: {res:?}"
    );
    let landed = r.read().reader().landings().unwrap()[0].1.changes[0];
    let e = r
        .view_include(&as_agent("agent:b", "r1"), "b", &[landed.to_string()])
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(e.message.contains(&l1[..12]), "{e}");
    let e = r
        .view_include(&as_agent("agent:b", "r1"), "b", &["zz".into()])
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let e = r
        .view_include(&as_agent("agent:b", "r1"), "b", &["a".repeat(64)])
        .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
}

#[test]
fn incl1_including_a_colliding_change_records_the_same_conflict_in_both_views() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    let b = as_agent("agent:b", "r1");
    let ca = write_as(&mut r, &a, "a", update(&c, &v, "a's"))
        .unwrap()
        .change;
    let cb = write_as(&mut r, &b, "b", update(&c, &v, "b's"))
        .unwrap()
        .change;
    let ra = r.view_include(&a, "a", std::slice::from_ref(&cb)).unwrap();
    assert_eq!(ra.conflicts.len(), 1, "{ra:?}");
    assert_eq!(ra.conflicts[0].block, c);
    assert_eq!(ra.conflicts[0].base.as_deref(), Some(v.as_str()));
    let sides: Vec<Option<String>> = ra.conflicts[0]
        .sides
        .iter()
        .map(|s| s.change.clone())
        .collect();
    assert!(
        sides.contains(&Some(ca.clone())) && sides.contains(&Some(cb.clone())),
        "{sides:?}"
    );
    assert!(ra.conflicts[0].sides.iter().all(|s| s.landed.is_none()));
    assert_eq!(
        body_of(&r, &view("a"), &c),
        "one",
        "a conflicted block reads as its base"
    );
    let rb = r.view_include(&b, "b", std::slice::from_ref(&ca)).unwrap();
    assert_eq!(
        rb.conflicts[0].id, ra.conflicts[0].id,
        "the same conflict has the same id in every view"
    );
    let id = rdy_model::BlockId::parse(&c).unwrap();
    assert_eq!(
        r.read()
            .conflict_on(&view("a"), &id)
            .unwrap()
            .unwrap()
            .id
            .to_string(),
        ra.conflicts[0].id
    );
    check(&r, &view("a")).unwrap();
    check(&r, &view("b")).unwrap();
}

/// Ownership of the target is Task 15's (after R); here: sources come from any view, a new
/// target is created and owned, and an include never claims.
#[test]
fn incl1_changes_come_from_any_view_and_an_include_into_a_new_view_creates_and_owns_it() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let b = as_agent("agent:b", "r1");
    let ca = write_as(&mut r, &a, "a", claim(&section, "a"))
        .unwrap()
        .change;
    write_as(&mut r, &b, "b", claim(&section, "b")).unwrap();
    r.view_include(&b, "b", std::slice::from_ref(&ca)).unwrap();
    assert_eq!(
        (r.read().included("a").len(), r.read().included("b").len()),
        (1, 2),
        "the source is untouched"
    );
    let fresh = r
        .view_include(&as_agent("agent:c", "r1"), "c", std::slice::from_ref(&ca))
        .unwrap();
    assert_eq!(
        fresh.changes, 1,
        "an include into the session's own new view creates it"
    );
    assert_eq!(
        r.read()
            .view_head("c")
            .unwrap()
            .owner
            .as_ref()
            .unwrap()
            .actor,
        "agent:c"
    );
    r.store_mut()
        .view_append(
            "b",
            vec![rdy_store::view::ViewOpKind::Own { owner: None }],
            &human_writer().provenance(),
        )
        .unwrap();
    r.invalidate();
    write_as(&mut r, &a, "a", claim(&section, "a2")).unwrap();
    let a2 = r.read().included("a")[1].to_string();
    r.view_include(&human_writer(), "b", &[a2]).unwrap();
    assert_eq!(
        r.read().view_head("b").unwrap().owner,
        None,
        "an include never claims"
    );
    let e = r.view_include(&human_writer(), "b", &[]).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}

#[test]
fn excl1_exclude_refuses_with_dependents_and_cascade_removes_them_in_order() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "x")).unwrap();
    let x = c1.created.values().next().unwrap().clone();
    let v1 = version_of(&r, &view("a"), &x);
    let c2 = write_as(&mut r, &a, "a", update(&x, &v1, "x2")).unwrap();
    let v2 = version_of(&r, &view("a"), &x);
    let c3 = write_as(&mut r, &a, "a", update(&x, &v2, "x3")).unwrap();
    let other = write_as(&mut r, &a, "a", claim(&section, "y")).unwrap();
    let e = r
        .view_exclude(&a, "a", std::slice::from_ref(&c1.change), false)
        .unwrap_err();
    assert_eq!(e.code, E_HAS_DEPENDENTS, "{e}");
    assert_eq!(e.issues.len(), 2, "{e:?}");
    assert!(e.fix.contains("--cascade"), "{e}");
    assert_eq!(r.read().included("a").len(), 4, "nothing written");
    let res = r
        .view_exclude(&a, "a", std::slice::from_ref(&c1.change), true)
        .unwrap();
    assert_eq!(res.removed, vec![c1.change.clone()]);
    assert_eq!(
        res.cascaded,
        vec![c2.change.clone(), c3.change.clone()],
        "dependents in included order"
    );
    assert_eq!(res.changes, 1);
    assert_eq!(
        r.read().included("a").as_ref(),
        &vec![other.change.parse().unwrap()]
    );
    assert!(
        r.read()
            .materialized(&view("a"))
            .unwrap()
            .get(&rdy_model::BlockId::parse(&x).unwrap())
            .is_none()
    );
    let e = r
        .view_exclude(&a, "a", std::slice::from_ref(&c1.change), false)
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "not in the view any more: {e}");
    let e = r
        .view_exclude(&a, "nope", std::slice::from_ref(&other.change), false)
        .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    check(&r, &view("a")).unwrap();
}

#[test]
fn excl1_excluding_a_side_removes_the_conflict_and_the_view_floats_when_emptied() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    let l: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    let ca = write_as(&mut r, &a, "a", update(&c, &v, "a's"))
        .unwrap()
        .change;
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap()
    .change;
    r.view_include(&a, "a", std::slice::from_ref(&cb)).unwrap();
    assert_eq!(r.read().conflicts(&view("a")).unwrap().len(), 1);
    let res = r.view_exclude(&a, "a", &[cb], false).unwrap();
    assert!(res.conflicts.is_empty(), "{res:?}");
    assert_eq!(body_of(&r, &view("a"), &c), "a's");
    r.view_exclude(&a, "a", &[ca], false).unwrap();
    let st = r.read();
    assert_eq!(st.included("a").len(), 0);
    assert_eq!(st.effective_base("a"), Some(l));
    assert_eq!(
        body_of(&r, &view("a"), &c),
        "one",
        "an emptied unpinned view floats on canon"
    );
}

fn section(parent: &str, title: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "section", "parent": parent, "fields": {"title": title}}])
}

/// Final review fix round 1 (Important 1): a change's store deps cover its op bases, deletes and
/// conflict sides, not the parent it creates a block under. An include brings the change that
/// created each created block's parent (and that change's own closure), so the target view holds
/// no orphan and the landing is not refused late with `E_MISSING_PARENT`.
#[test]
fn incl1_including_a_child_brings_the_changes_that_created_its_parents_and_the_view_lands() {
    let (_d, mut r) = project();
    let (doc, _section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c0 = write_as(&mut r, &a, "a", section(&doc, "Outer")).unwrap();
    let outer = c0.created.values().next().unwrap().clone();
    let c1 = write_as(&mut r, &a, "a", section(&outer, "Inner")).unwrap();
    let inner = c1.created.values().next().unwrap().clone();
    let c2 = write_as(&mut r, &a, "a", claim(&inner, "child")).unwrap();
    let child = c2.created.values().next().unwrap().clone();
    let b = as_agent("agent:b", "r1");
    let res = r
        .view_include(&b, "b", std::slice::from_ref(&c2.change))
        .unwrap();
    assert_eq!(res.added, vec![c2.change.clone()]);
    let mut deps = res.added_deps.clone();
    deps.sort();
    let mut want = vec![c0.change.clone(), c1.change.clone()];
    want.sort();
    assert_eq!(
        deps, want,
        "the parents' creations come with the child: {res:?}"
    );
    assert_eq!(body_of(&r, &view("b"), &child), "child");
    check(&r, &view("b")).unwrap();
    land(&mut r, "b").unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &child), "child");
    check(&r, &Scope::Canon).unwrap();
    r.fsck().unwrap();
}

/// A parent no unlanded change creates (here: it landed after a pinned view's base) cannot be
/// brought along; the include is refused naming the parent, and writes nothing.
#[test]
fn incl1_a_child_whose_parent_cannot_be_brought_along_is_refused_naming_the_parent() {
    let (_d, mut r) = project();
    let (doc, _section, _c) = seed_doc(&mut r, "seed");
    let l1 = land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", section(&doc, "New")).unwrap();
    let new = c1.created.values().next().unwrap().clone();
    let c2 = write_as(&mut r, &a, "a", claim(&new, "child")).unwrap();
    let p = r
        .propose_subset(
            &a,
            "a",
            "the section",
            Some(std::slice::from_ref(&c1.change)),
        )
        .unwrap();
    r.land_report(&approver(), &p.r#ref).unwrap();
    let b = as_agent("agent:b", "r1");
    r.view_create(&b, "pinned", Some(&format!("landing:{l1}")), true)
        .unwrap();
    let head = r.read().view_head("pinned").unwrap().head;
    let e = r
        .view_include(&b, "pinned", std::slice::from_ref(&c2.change))
        .unwrap_err();
    assert_eq!(e.code, E_DEPS_MISSING, "{e}");
    assert_eq!(e.issues.len(), 1, "{e:?}");
    assert!(
        e.issues[0].message.contains(&new) && e.issues[0].message.contains(&c2.change[..12]),
        "{:?}",
        e.issues[0]
    );
    assert_eq!(e.issues[0].code, E_DEPS_MISSING);
    assert_eq!(
        r.read().view_head("pinned").unwrap().head,
        head,
        "nothing was written"
    );
}
