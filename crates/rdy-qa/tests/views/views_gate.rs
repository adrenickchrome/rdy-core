//! gconf1: propose reports conflicts and marks the blocking ones; land refuses only conflicts on
//! blocks the proposal writes; subsets must be closed.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::{TEST_SUMMARY, assert_not_ready};
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
    let pa = r.propose(&sa, "a", "a", Some(TEST_SUMMARY)).unwrap();
    let pb = r.propose(&sb, "b", "b", Some(TEST_SUMMARY)).unwrap();
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
    let p2 = r
        .propose(sess_second, second, "rebased", Some(TEST_SUMMARY))
        .unwrap();
    assert_eq!(
        (p2.version, p2.blocking.len(), p2.conflicts.len()),
        (2, 1, 1)
    );
    // D-ME2HD: the conflict is the refusal's one readiness issue; the version's listing names
    // its block and its canon side
    let e = r.land_report(&approver(), &p2.r#ref).unwrap_err();
    assert_not_ready(&r, &e, &p2.r#ref, &[E_CONFLICT_UNRESOLVED]);
    assert_eq!(p2.blocking[0].block, c);
    assert!(
        p2.blocking[0]
            .sides
            .iter()
            .any(|s| s.landed.is_some() || s.change.is_none()),
        "{:?}",
        p2.blocking[0]
    );
    let e2 = r.land_report(&approver(), &p2.r#ref).unwrap_err();
    assert_eq!(
        (e2.code, &e2.message, &e2.issues),
        (e.code, &e.message, &e.issues),
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
    let issues = assert_not_ready(&r, &e3, &p2.r#ref, &[E_CONFLICT_UNRESOLVED]);
    assert!(issues[0].fix.contains("propose again"), "{e3}");
    let p3 = r.propose(sess_second, second, "resolved", None).unwrap();
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
    let whole = r.propose(&sa, "a", "whole", Some(TEST_SUMMARY)).unwrap();
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
    assert_not_ready(&r, &e, &whole.r#ref, &[E_CONFLICT_UNRESOLVED]);
    let subset = r
        .propose_subset(&sa, "a", "just y", Some(std::slice::from_ref(&cy)), None)
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
        .propose_subset(&sa, "a", "t", Some(std::slice::from_ref(&c2)), None)
        .unwrap_err();
    assert_eq!(e.code, E_DEPS_MISSING, "{e}");
    assert_eq!(e.issues.len(), 1);
    assert!(
        e.issues[0].message.contains(&c1.change[..12]),
        "{:?}",
        e.issues[0]
    );
    let e = r
        .propose_subset(&sa, "a", "t", Some(&["b".repeat(64)]), None)
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let ok = r
        .propose_subset(
            &sa,
            "a",
            "t",
            Some(&[c2.clone(), c1.change.clone()]),
            Some(TEST_SUMMARY),
        )
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
    let e = r
        .propose_subset(&sa, "a", "empty", Some(&[]), None)
        .unwrap_err();
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
    let whole = r.propose(&sa, "a", "whole", None).unwrap();
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
    let p = r.propose(&sa, "pinned", "p", None);
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
    let p = r
        .propose(&sc, "c", "net nothing", Some(TEST_SUMMARY))
        .unwrap();
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

/// Final review fix round 1 (Important 1): a subset must also carry the change that created the
/// parent of each block it creates, unless that parent is in the view's base; leaving it out is
/// `E_DEPS_MISSING` naming that change, not a late `E_VALIDATION`/`E_MISSING_PARENT`.
#[test]
fn gconf1_a_subset_without_the_creation_of_a_childs_parent_is_e_deps_missing() {
    let (_d, mut r) = project();
    let (doc, _section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let sa = as_agent("agent:a", "r1");
    let c1 = write_as(
        &mut r,
        &sa,
        "a",
        json!([{"op": "create", "type": "section", "parent": doc, "fields": {"title": "New"}}]),
    )
    .unwrap();
    let new = c1.created.values().next().unwrap().clone();
    let c2 = write_as(&mut r, &sa, "a", claim(&new, "child")).unwrap();
    let e = r
        .propose_subset(&sa, "a", "t", Some(std::slice::from_ref(&c2.change)), None)
        .unwrap_err();
    assert_eq!(e.code, E_DEPS_MISSING, "{e}");
    assert_eq!(e.issues.len(), 1, "{e:?}");
    assert!(
        e.issues[0].message.contains(&c1.change[..12]) && e.issues[0].message.contains(&new),
        "{:?}",
        e.issues[0]
    );
    assert!(e.fix.contains(&c1.change[..12]), "{e}");
    assert_eq!(r.read().reader().get_ref("open/a").unwrap(), None);
    let ok = r
        .propose_subset(
            &sa,
            "a",
            "t",
            Some(&[c2.change.clone(), c1.change.clone()]),
            Some(TEST_SUMMARY),
        )
        .unwrap();
    r.land_report(&approver(), &ok.r#ref).unwrap();
    let child = c2.created.values().next().unwrap().clone();
    assert_eq!(body_of(&r, &Scope::Canon, &child), "child");
}

#[test]
fn msw2_an_approve_path_stale_base_tells_the_approver_what_the_author_must_do() {
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
    let changes = r.store_mut().included("pinned").unwrap().as_ref().clone();
    let pid = r
        .store_mut()
        .propose_changes(
            "pinned",
            "p",
            Some(TEST_SUMMARY),
            sa.provenance(),
            changes.clone(),
            None,
        )
        .unwrap();
    r.invalidate();
    let pref = r.resolve_proposal(&pid.to_string()).unwrap().to_string();
    // M2d2: the gate reports the edit collision as a blocking tension before it merges; a
    // human approver dismisses it, and the merge's stale base still refuses. D-ME2HD: both are
    // readiness issues of one refusal, in the queue's order; the version's listing names the
    // tensions
    let t = r.land_report(&approver(), &pid.to_string()).unwrap_err();
    assert_not_ready(&r, &t, &pref, &[E_BLOCKING_TENSION, E_STALE_BASE]);
    let (listed, _) = r
        .read()
        .proposal_tensions("pinned", &changes, &Default::default(), 100)
        .unwrap();
    let refusing: Vec<String> = listed
        .iter()
        .filter(|t| t.refuses == Some(true))
        .map(|t| t.id.clone())
        .collect();
    assert!(!refusing.is_empty(), "{listed:#?}");
    for label in &refusing {
        r.dismiss_tension(&approver(), label, "the approver sees the stale base")
            .unwrap();
    }
    let e = r.land_report(&approver(), &pid.to_string()).unwrap_err();
    let issues = assert_not_ready(&r, &e, &pref, &[E_STALE_BASE]);
    let stale = &issues[0];
    assert!(
        !stale.message.contains("you read it"),
        "the message speaks to the approver: {e}"
    );
    assert!(
        stale.fix.contains("pinned") && stale.fix.contains(&c),
        "the fix names the view and the block: {e}"
    );
    assert!(!stale.fix.contains("rebase the view"), "{e}");
}

#[test]
fn msw2_an_unreadable_producer_fails_the_conflict_check_instead_of_settling_canons_side() {
    // fix round 1: canon's side of b's conflict is a's change; b's second change settles the
    // conflict only if canon's side has no known producer, so a producer lookup that fails must
    // fail the check, not read as "no producer"
    let (d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    let sb = as_agent("agent:b", "r1");
    write_as(&mut r, &sa, "a", update(&c, &v, "a's")).unwrap();
    write_as(&mut r, &sb, "b", update(&c, &v, "b1")).unwrap();
    let b1 = version_of(&r, &view("b"), &c);
    write_as(&mut r, &sb, "b", update(&c, &b1, "b2")).unwrap();
    land(&mut r, "a").unwrap();
    let pb = r.propose(&sb, "b", "b", None).unwrap();
    assert_eq!(
        pb.conflicts.len(),
        1,
        "healthy index: canon's side holds the conflict open"
    );
    let y: rdy_model::Hash = version_of(&r, &Scope::Canon, &c).parse().unwrap();
    r.close().unwrap();
    {
        // damage: a second producer of canon's version, indexed as landed by a landing the
        // index does not hold, so the producer lookup fails
        let idx = rdy_store::index::Index::open(&d.path().join(".rdy/store/index.redb")).unwrap();
        let upto = idx.indexed_upto().unwrap();
        let fake = rdy_model::Hash([0xAB; 32]);
        idx.apply_durable(
            &[
                rdy_store::index::IndexEntry::Producer {
                    version: y,
                    change: fake,
                },
                rdy_store::index::IndexEntry::Landed {
                    change: fake,
                    landing: rdy_model::Hash([0xCD; 32]),
                },
            ],
            upto,
        )
        .unwrap();
    }
    let mut r = Rdy::open(d.path()).unwrap();
    assert!(r.store().producer(&y).is_err(), "the damage is real");
    let shown = r.read().proposal_show(&pb.r#ref);
    assert!(
        shown.is_err(),
        "proposal show must not report no conflict: {:?}",
        shown.map(|s| s.conflicts.len())
    );
    let again = r.propose(&sb, "b", "again", None);
    assert!(
        again.is_err(),
        "propose must not report no conflict: {:?}",
        again.map(|p| p.conflicts.len())
    );
    let approved = r.land_report(&approver(), &pb.r#ref);
    assert!(
        approved.is_err(),
        "must not land over canon's side: {:?}",
        approved.map(|_| ())
    );
}

/// Final ruling round 3: a landing that keeps a landed move passes the landing audit and fsck.
/// m moves c2 before c; w edits c2's body from canon; m lands, then w lands with c2 at canon's
/// key. The audit finds no violation before w lands, nor in w's whole landing (its acceptance
/// change too) on w's base; fsck reads the stored version.
#[test]
fn ok6_a_kept_move_passes_the_landing_audit_and_fsck() {
    use rdy_qa::landing_audit::{Landed, audit, conflicts_at, landed_of};
    let (_d, mut r) = project();
    let (_doc, s, c) = seed_doc(&mut r, "seed");
    let c2 = write(
        &mut r,
        "seed",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": s, "body": "two"}]),
    )
    .unwrap()
    .created["$n"]
        .clone();
    land(&mut r, "seed").unwrap();
    let sm = as_agent("agent:m", "r1");
    let sw = as_agent("agent:w", "r1");
    write_as(
        &mut r,
        &sm,
        "m",
        json!([{"op": "move", "id": c2, "parent": s, "before": c}]),
    )
    .unwrap();
    let v2 = version_of(&r, &Scope::Canon, &c2);
    let ww = write_as(&mut r, &sw, "w", update(&c2, &v2, "two, as w reads it")).unwrap();
    land(&mut r, "m").unwrap();
    let before = r.read();
    let base = before.effective_base("w");
    let mine = Landed {
        view: "w".into(),
        changes: vec![ww.change.parse().unwrap()],
        alternative: None,
    };
    assert!(
        audit(&before, &mine).is_empty(),
        "{:?}",
        audit(&before, &mine)
    );
    drop(before);
    let l = land(&mut r, "w").unwrap();
    let landed = landed_of(&r, &l.parse().unwrap()).unwrap();
    assert_eq!(landed.changes.len(), 2, "w's change and its acceptance");
    let v = conflicts_at(&r.read(), &landed, base);
    assert!(v.is_empty(), "{v:?}");
    let kids: Vec<String> = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(Some(&rdy_model::BlockId::parse(&s).unwrap()))
        .iter()
        .map(|b| b.id.to_string())
        .collect();
    assert_eq!(kids, vec![c2.clone(), c.clone()], "m's move stays");
    assert_eq!(body_of(&r, &Scope::Canon, &c2), "two, as w reads it");
    r.fsck().unwrap();
    check(&r, &Scope::Canon).unwrap();
}
