//! gconf1: propose reports conflicts and marks the blocking ones; land refuses only conflicts on
//! blocks the proposal writes; subsets must be closed.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}
fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

fn collide_and_land(first: &str, second: &str) {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    let sb = as_agent("agent:b", "r1");
    write_as(&mut r, &sa, "a", update(&c, &v, "a's")).unwrap();
    write_as(&mut r, &sb, "b", update(&c, &v, "b's")).unwrap();
    let pa = r.propose(&sa, "a", "a").unwrap();
    let pb = r.propose(&sb, "b", "b").unwrap();
    assert!(pa.conflicts.is_empty() && pb.conflicts.is_empty());
    let (pf, ps, sess_second) = if first == "a" {
        (&pa, &pb, &sb)
    } else {
        (&pb, &pa, &sa)
    };
    r.land_report(&approver(), &pf.r#ref).unwrap();
    // FW-HB1: the landing changed the block ps writes, so ps's approval does not survive it
    let e = r.land_report(&approver(), &ps.r#ref).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert!(
        e.message.contains(&c) && e.fix.contains("propose again"),
        "{e}"
    );
    // re-proposed on the new canon without resolving: the version carries the recorded conflict
    let p2 = r.propose(sess_second, second, "rebased").unwrap();
    assert_eq!(
        (p2.version, p2.blocking.len(), p2.conflicts.len()),
        (2, 1, 1)
    );
    let e = r.land_report(&approver(), &p2.r#ref).unwrap_err();
    assert_eq!(e.code, E_CONFLICT_UNRESOLVED, "{e}");
    assert_eq!(e.issues.len(), 1);
    assert_eq!(e.issues[0].block.as_ref().unwrap().to_string(), c);
    assert!(e.issues[0].message.contains("canon"), "{:?}", e.issues[0]);
    let e2 = r.land_report(&approver(), &p2.r#ref).unwrap_err();
    assert_eq!(
        e2.code, E_CONFLICT_UNRESOLVED,
        "refused the same way until resolved"
    );
    let conflict = r
        .read()
        .conflict_on(&view(second), &rdy_model::BlockId::parse(&c).unwrap())
        .unwrap()
        .unwrap();
    write_as(
        &mut r,
        sess_second,
        second,
        update(&c, &conflict.base.unwrap().to_string(), "both"),
    )
    .unwrap();
    assert!(
        r.read().conflicts(&view(second)).unwrap().is_empty(),
        "the view is resolved"
    );
    // Review Focus 2: p2 predates the resolution, so it still carries the conflict
    let e3 = r.land_report(&approver(), &p2.r#ref).unwrap_err();
    assert_eq!(e3.code, E_CONFLICT_UNRESOLVED, "{e3}");
    assert!(e3.fix.contains("propose again"), "{e3}");
    let p3 = r.propose(sess_second, second, "resolved").unwrap();
    assert_eq!(p3.version, 3);
    assert!(p3.conflicts.is_empty() && p3.blocking.is_empty());
    let e = r.land_report(&approver(), &p2.r#ref).unwrap_err();
    assert_eq!(
        e.code, E_PROPOSAL_MOVED,
        "the old version is not the latest any more: {e}"
    );
    r.land_report(&approver(), &p3.r#ref).unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &c), "both");
    check(&r, &Scope::Canon).unwrap();
    r.fsck().unwrap();
}

#[test]
fn gconf1_two_views_collide_a_lands_b_is_refused_until_it_resolves() {
    collide_and_land("a", "b");
}

#[test]
fn gconf1_two_views_collide_b_lands_a_is_refused_until_it_resolves() {
    collide_and_land("b", "a");
}

#[test]
fn gconf1_propose_lists_every_conflict_and_marks_the_blocking_ones() {
    let (_d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    let ca = write_as(&mut r, &sa, "a", update(&c, &v, "a's"))
        .unwrap()
        .change;
    let cy = write_as(&mut r, &sa, "a", claim(&section, "y"))
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
    r.view_include(&sa, "a", std::slice::from_ref(&cb)).unwrap();
    let whole = r.propose(&sa, "a", "whole").unwrap();
    assert_eq!(whole.conflicts.len(), 1);
    assert_eq!(
        whole
            .blocking
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>(),
        vec![whole.conflicts[0].id.clone()]
    );
    let e = r.land_report(&approver(), &whole.r#ref).unwrap_err();
    assert_eq!(e.code, E_CONFLICT_UNRESOLVED, "{e}");
    let subset = r
        .propose_subset(&sa, "a", "just y", Some(std::slice::from_ref(&cy)))
        .unwrap();
    assert_eq!(
        (subset.changes, subset.version),
        (1, 2),
        "a new version of the same proposal, with one change"
    );
    assert_eq!(subset.conflicts.len(), 1, "still listed");
    assert!(subset.blocking.is_empty(), "y is not conflicted");
    let info = r.land_report(&approver(), &subset.r#ref).unwrap();
    assert_eq!(info.rebased[0].dropped, 1);
    let st = r.read();
    assert_eq!(st.included("a").len(), 2, "ca and cb remain");
    assert_eq!(
        st.conflicts(&view("a")).unwrap().len(),
        1,
        "the conflict is untouched by a landing elsewhere"
    );
    assert_eq!(body_of(&r, &Scope::Canon, &c), "one");
    let _ = ca;
}

#[test]
fn gconf1_a_subset_must_be_closed_and_name_changes_in_the_view() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let sa = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &sa, "a", claim(&section, "x")).unwrap();
    let x = c1.created.values().next().unwrap().clone();
    let v1 = version_of(&r, &view("a"), &x);
    let c2 = write_as(&mut r, &sa, "a", update(&x, &v1, "x2"))
        .unwrap()
        .change;
    let e = r
        .propose_subset(&sa, "a", "t", Some(std::slice::from_ref(&c2)))
        .unwrap_err();
    assert_eq!(e.code, E_DEPS_MISSING, "{e}");
    assert_eq!(e.issues.len(), 1);
    assert!(
        e.issues[0].message.contains(&c1.change[..12]),
        "{:?}",
        e.issues[0]
    );
    let e = r
        .propose_subset(&sa, "a", "t", Some(&["b".repeat(64)]))
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let ok = r
        .propose_subset(&sa, "a", "t", Some(&[c2.clone(), c1.change.clone()]))
        .unwrap();
    assert_eq!(ok.changes, 2);
    r.land_report(&approver(), &ok.r#ref).unwrap();
    assert_eq!(body_of(&r, &Scope::Canon, &x), "x2");
}

/// Fix round 1: an empty subset is refused before anything is written; it used to write a
/// version with no changes that landed again and again as empty landings.
#[test]
fn gconf1_an_empty_subset_is_refused_and_writes_no_proposal() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let sa = as_agent("agent:a", "r1");
    write_as(&mut r, &sa, "a", claim(&section, "x")).unwrap();
    let e = r.propose_subset(&sa, "a", "empty", Some(&[])).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(e.message.contains("at least one change"), "{e}");
    assert!(
        e.fix.contains("rdy view show a") && e.fix.contains("omit changes"),
        "{e}"
    );
    assert_eq!(
        r.read().reader().get_ref("open/a").unwrap(),
        None,
        "no proposal ref is written"
    );
    let whole = r.propose(&sa, "a", "whole").unwrap();
    assert_eq!((whole.version, whole.changes), (1, 1));
    r.fsck().unwrap();
}

#[test]
fn gconf1_a_pinned_views_canon_collision_stays_e_stale_base() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    r.view_create(&sa, "pinned", None, true).unwrap();
    write_as(&mut r, &sa, "pinned", update(&c, &v, "pinned's")).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap();
    land(&mut r, "b").unwrap();
    let p = r.propose(&sa, "pinned", "p");
    let e = match p {
        Ok(p) => r.land_report(&approver(), &p.r#ref).unwrap_err(),
        Err(e) => e,
    };
    assert_eq!(
        e.code, E_STALE_BASE,
        "no conflict was recorded for a pinned view: {e}"
    );
}

/// Found by the deep run after M2c (sb2's `E_STALE_BASE` landing): a view that includes another
/// view's delete of a proposed decision also holds the decision's creation, which the delete
/// depends on. The version lists the delete before the creation, so folding the ops in list
/// order revived the decision in the prospective canon and the acceptance step then found the
/// view (where the decision is deleted) "changed after the proposal". The ops fold in dependency
/// order, as the merge applies them.
#[test]
fn gconf1_a_version_listing_a_delete_before_its_decisions_creation_lands() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let (sa, sb, sc) = (
        human_writer(),
        as_agent("agent:b", "r1"),
        as_agent("agent:c", "r1"),
    );
    // a records a proposed decision; b includes it and deletes it
    let decided = r
        .decide(
            &sa,
            "a",
            DecisionInput {
                parent: section,
                statement: "to be dropped".into(),
                ..Default::default()
            },
        )
        .unwrap();
    r.view_include(&sb, "b", std::slice::from_ref(&decided.change))
        .unwrap();
    let d = decided.created.values().next().unwrap().clone();
    let deleted = write_as(&mut r, &sb, "b", json!([{"op": "delete", "id": d}])).unwrap();
    // c includes b's delete: the include adds the creation too, after it in the set
    r.view_include(&sc, "c", std::slice::from_ref(&deleted.change))
        .unwrap();
    let p = r.propose(&sc, "c", "net nothing").unwrap();
    assert_eq!(p.changes, 2, "the delete and the creation it depends on");
    let info = r.land_report(&approver(), &p.r#ref).unwrap();
    assert!(info.warnings.is_empty(), "{info:?}");
    assert!(
        r.read()
            .materialized(&Scope::Canon)
            .unwrap()
            .get(&rdy_model::BlockId::parse(&d).unwrap())
            .is_none(),
        "the decision is deleted in canon"
    );
    check(&r, &Scope::Canon).unwrap();
}
