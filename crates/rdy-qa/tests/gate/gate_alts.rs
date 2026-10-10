//! altg1: a member view proposes its own member; the landing chooses it and closes the others.
use rdy_engine::alt::*;
use rdy_engine::*;
use rdy_model::Hash;
use rdy_qa::fixtures::*;
use serde_json::json;

fn m(label: &str, view: &str) -> AltMemberInput {
    AltMemberInput {
        label: label.into(),
        view: view.into(),
    }
}
fn pick(set: &str, chosen: &str) -> AltPick {
    AltPick {
        set: set.into(),
        chosen: chosen.into(),
    }
}
/// Canon with one claim; member views a and b edit it, c is a third member; returns the set id.
fn setup(r: &mut Rdy) -> String {
    let (_doc, _s, claim) = seed_doc(r, "seed");
    land(r, "seed").unwrap();
    let v = version_of(r, &Scope::Canon, &claim);
    for view in ["a", "b", "c"] {
        write(
            r,
            view,
            json!([{"op": "update", "id": claim, "base": v, "body": view}]),
        )
        .unwrap();
    }
    r.alt_create(
        &writer(),
        "T",
        None,
        &[m("x", "a"), m("y", "b"), m("z", "c")],
    )
    .unwrap()
    .id
}

#[test]
fn altg1_a_member_must_name_its_choice() {
    let (_d, mut r) = project();
    setup(&mut r);
    let e = r.propose(&writer(), "a", "x").unwrap_err();
    assert_eq!(e.code, E_ALT_UNCHOSEN);
    assert!(e.fix.contains("alternatives"), "{}", e.fix);
}

#[test]
fn altg1_a_member_cannot_choose_another_member() {
    let (_d, mut r) = project();
    let set = setup(&mut r);
    let e = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "y")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER);
    let e = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "b")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "by view name too");
    write(
        &mut r,
        "loner",
        json!([{"op": "create", "type": "doc", "fields": {"title": "L", "doc_type": "glossary"}}]),
    )
    .unwrap();
    let e = r
        .propose_with(&writer(), "loner", "l", None, None, Some(&pick(&set, "x")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "a non-member cannot name a set");
    let p = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    assert_eq!(p.alternative.as_ref().unwrap().member, "a");
    let p2 = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&set, "b")))
        .unwrap();
    assert_eq!(p2.alternative.as_ref().unwrap().set, set);
}

#[test]
fn altg1_landing_a_member_closes_the_other_open_members_and_chooses() {
    let (_d, mut r) = project();
    let set = setup(&mut r);
    r.view_close(&writer(), &["c".into()], "by hand").unwrap();
    let p = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    let rows = r.read().log_rows().unwrap();
    assert_eq!(rows[0].chosen.as_ref().unwrap().set, set);
    assert_eq!(rows[0].chosen.as_ref().unwrap().member, "a");
    assert!(r.read().view_head("b").unwrap().closed);
    let s = r.read().resolve_alt(&set).unwrap().2;
    assert!(matches!(s.status, rdy_model::AltStatus::Chosen { ref member, .. } if member == "a"));
    assert!(!r.read().view_head("a").unwrap().closed);
    let infos = r.read().alt_sets(true).unwrap();
    assert_eq!(infos[0].status, "chosen");
    assert_eq!(infos[0].chosen.as_ref().unwrap().member, "a");
    // b's later proposal cannot land into a chosen set
    let e = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&set, "y")))
        .unwrap_err();
    assert!(
        e.code == E_ALT_MEMBER || e.code == E_VIEW_CLOSED,
        "{}",
        e.code
    );
}

#[test]
fn altg1_land_reports_chosen_and_closed() {
    let (_d, mut r) = project();
    let set = setup(&mut r);
    let p = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&set, "y")))
        .unwrap();
    let info = r.land_report(&approver(), &p.id).unwrap();
    assert_eq!(info.chosen.as_ref().unwrap().member, "b");
    assert_eq!(info.closed, vec!["a".to_string(), "c".to_string()]);
    let v = serde_json::to_value(&info).unwrap();
    assert_eq!(v["closed"], json!(["a", "c"]));
}

#[test]
fn altg1_land_rechecks_the_set_at_approval() {
    let (_d, mut r) = project();
    let set = setup(&mut r);
    let pa = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    let pb = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&set, "y")))
        .unwrap();
    r.land(&approver(), &pa.id).unwrap();
    let e = r.land(&approver(), &pb.id).unwrap_err();
    assert_eq!(
        e.code, E_ALT_MEMBER,
        "the set was chosen between propose and approve"
    );
    // a view that joins a set after proposing must propose again
    write(
        &mut r,
        "late",
        json!([{"op": "create", "type": "doc", "fields": {"title": "L", "doc_type": "glossary"}}]),
    )
    .unwrap();
    write(
        &mut r,
        "late2",
        json!([{"op": "create", "type": "doc", "fields": {"title": "M", "doc_type": "glossary"}}]),
    )
    .unwrap();
    let pl = r.propose(&writer(), "late", "late").unwrap();
    r.alt_create(&writer(), "U", None, &[m("p", "late"), m("q", "late2")])
        .unwrap();
    assert_eq!(
        r.land(&approver(), &pl.id).unwrap_err().code,
        E_ALT_UNCHOSEN
    );
    // abandoned: no member can land
    let u = r.read().alt_of_view("late").unwrap().unwrap().0.to_string();
    let p2 = r
        .propose_with(&writer(), "late2", "q", None, None, Some(&pick(&u, "q")))
        .unwrap();
    r.alt_abandon(&writer(), &u, "no").unwrap();
    assert_eq!(r.land(&approver(), &p2.id).unwrap_err().code, E_ALT_MEMBER);
}

/// Canon with a seeded section; view a creates claim `alt A` and view b creates claim `alt B`,
/// both before any set exists (their changes carry no alternative link). Returns the section.
fn two_claims(r: &mut Rdy) -> String {
    let (_doc, section, _claim) = seed_doc(r, "seed");
    land(r, "seed").unwrap();
    for (view, body) in [("a", "alt A"), ("b", "alt B")] {
        write(
            r,
            view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": body}]),
        )
        .unwrap();
    }
    section
}

fn canon_bodies(r: &Rdy) -> Vec<String> {
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    m.iter().map(|b| b.body.clone()).collect()
}

#[test]
fn altg1_a_proposal_from_before_the_set_cannot_land_once_another_member_is_chosen() {
    let (_d, mut r) = project();
    let section = two_claims(&mut r);
    let pb = r.propose(&writer(), "b", "b early").unwrap();
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    assert_eq!(
        r.land(&approver(), &pb.id).unwrap_err().code,
        E_ALT_UNCHOSEN
    );
    let pa = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &pa.id).unwrap();
    let e = r.land(&approver(), &pb.id).unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message
            .contains("is chosen; this proposal can no longer land"),
        "{}",
        e.message
    );
    let bodies = canon_bodies(&r);
    assert!(bodies.iter().any(|b| b == "alt A"));
    assert!(!bodies.iter().any(|b| b == "alt B"), "{bodies:?}");
    // the chosen member's own view goes on landing work without naming a choice
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "after"}]),
    )
    .unwrap();
    let later = r.propose(&writer(), "a", "after").unwrap();
    r.land(&approver(), &later.id).unwrap();
}

#[test]
fn altg1_a_proposal_from_before_the_set_cannot_land_once_the_set_is_abandoned() {
    let (_d, mut r) = project();
    two_claims(&mut r);
    let pb = r.propose(&writer(), "b", "b early").unwrap();
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    r.alt_abandon(&writer(), &set, "neither").unwrap();
    let e = r.land(&approver(), &pb.id).unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message
            .contains("is abandoned; this proposal can no longer land"),
        "{}",
        e.message
    );
    assert!(!canon_bodies(&r).iter().any(|b| b == "alt B"));
}

#[test]
fn altg1_a_members_change_lands_only_from_its_own_view_with_its_pick() {
    let (_d, mut r) = project();
    let section = two_claims(&mut r);
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    // written in member view b while the set is open: the change carries b's link
    let b2 = write(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "alt B2"}]),
    )
    .unwrap()
    .change;
    // included into a view outside the set, it cannot be proposed from there
    r.view_include(&writer(), "d", std::slice::from_ref(&b2))
        .unwrap();
    let e = r.propose(&writer(), "d", "d").unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message.contains("member y's alternative"),
        "{}",
        e.message
    );
    assert!(e.fix.contains("propose from b"), "{}", e.fix);
    // nor from another member view, under that member's own pick
    r.view_include(&writer(), "a", std::slice::from_ref(&b2))
        .unwrap();
    let e = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    // from its own view with its own pick it proposes and lands
    let pb = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&set, "y")))
        .unwrap();
    r.land(&approver(), &pb.id).unwrap();
    assert!(canon_bodies(&r).iter().any(|b| b == "alt B2"));
}

#[test]
fn altg1_a_reopened_member_view_the_set_did_not_choose_cannot_propose() {
    let (_d, mut r) = project();
    two_claims(&mut r);
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let pa = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &pa.id).unwrap();
    assert!(r.read().view_head("b").unwrap().closed);
    let u = r.undo(&writer(), "b", None, false).unwrap();
    assert!(u.reopened);
    let e = r.propose(&writer(), "b", "b again").unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message.contains("view b is its member y"),
        "{}",
        e.message
    );
    assert!(e.fix.contains("outside the set"), "{}", e.fix);
}

/// Final-review fix round 1: why a view was closed is readable where the view is: the close op of
/// `rdy view show` and the closed row of `rdy view ls --all` carry the reason (a landing's, or one
/// given by hand); other ops and open rows carry none.
#[test]
fn altg1_a_close_reason_shows_in_view_show_and_view_ls() {
    let (_d, mut r) = project();
    let set = setup(&mut r);
    r.view_close(&writer(), &["c".into()], "by hand").unwrap();
    let p = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    let st = r.read();
    let b = st.view_show("b").unwrap();
    assert_eq!(b.ops[0].kind, "close");
    let reason = b.ops[0].reason.clone().expect("the close op's reason");
    assert!(
        reason.starts_with(&format!("alternative chosen: {set} chose ")),
        "{reason}"
    );
    assert!(b.ops[1..].iter().all(|o| o.reason.is_none()), "{:?}", b.ops);
    let v = serde_json::to_value(&b).unwrap();
    assert_eq!(v["ops"][0]["reason"], json!(reason));
    assert!(v["ops"][1].get("reason").is_none(), "{v}");
    let rows = st.view_list(true, 30, now_ms()).unwrap();
    let row = |name: &str| rows.iter().find(|x| x.name == name).unwrap().clone();
    assert_eq!(row("b").close_reason.as_deref(), Some(reason.as_str()));
    assert_eq!(row("c").close_reason.as_deref(), Some("by hand"));
    assert_eq!(row("a").close_reason, None);
    let a = serde_json::to_value(row("a")).unwrap();
    assert!(a.get("close_reason").is_none(), "{a}");
}

/// Final-review fix round 1: the lost-member rule holds with a pick too. A view a settled set did
/// not choose cannot propose as a member of a later open set, and a pick on the settled set
/// itself names the real fix (a view outside the set) in one hop.
#[test]
fn altg1_a_lost_member_cannot_propose_under_a_later_set_or_a_pick() {
    let (_d, mut r) = project();
    let section = two_claims(&mut r);
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let pa = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &pa.id).unwrap();
    assert!(r.undo(&writer(), "b", None, false).unwrap().reopened);
    // a pick on the chosen set: the fix is the outside view, not "propose without alternatives"
    let e = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&set, "y")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(e.fix.contains("outside the set"), "{}", e.fix);
    // b joins a later open set, and its pick there is refused too
    write(
        &mut r,
        "d",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "alt D"}]),
    )
    .unwrap();
    let later = r
        .alt_create(&writer(), "U", None, &[m("p", "b"), m("q", "d")])
        .unwrap()
        .id;
    let e = r
        .propose_with(&writer(), "b", "p", None, None, Some(&pick(&later, "p")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message.contains("view b is its member y"),
        "{}",
        e.message
    );
    assert!(e.fix.contains("outside the set"), "{}", e.fix);
    // the later set's other member still proposes and lands
    let pd = r
        .propose_with(&writer(), "d", "q", None, None, Some(&pick(&later, "q")))
        .unwrap();
    r.land(&approver(), &pd.id).unwrap();
    assert!(canon_bodies(&r).iter().any(|b| b == "alt D"));
}

#[test]
fn msw2_a_members_change_from_before_the_set_cannot_land_from_a_non_member_view() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "alt A"}]),
    )
    .unwrap();
    let early_b = write(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "alt B"}]),
    )
    .unwrap()
    .change;
    r.alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&early_b))
        .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(!canon_bodies(&r).iter().any(|b| b == "alt B"));
}

#[test]
fn msw2_a_pre_set_change_two_members_hold_is_common_ground() {
    // fix round 1: a view forked from member b holds b's pre-set change; once both are members,
    // that change is no member's alternative, so it lands with either and chooses nothing
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "ground G"}]),
    )
    .unwrap();
    r.view_fork(&writer(), "b", "a").unwrap();
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "alt A"}]),
    )
    .unwrap();
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let p = r
        .propose_with(&writer(), "a", "a", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    let bodies = canon_bodies(&r);
    assert!(
        bodies.iter().any(|b| b == "ground G") && bodies.iter().any(|b| b == "alt A"),
        "{bodies:?}"
    );
}

#[test]
fn msw2_a_change_linked_to_a_set_that_dropped_its_member_binds_to_the_members_open_set() {
    // fix round 1: H is linked to S0's member b; b leaves S0 and joins S2, so H is b's
    // alternative in S2 and a non-member view cannot land it
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    for v in ["b", "c", "e", "a"] {
        write(
            &mut r,
            v,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("alt {v}")}]),
        )
        .unwrap();
    }
    let s0 = r
        .alt_create(
            &writer(),
            "S0",
            None,
            &[m("x", "b"), m("y", "c"), m("z", "e")],
        )
        .unwrap()
        .id;
    let h = write(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "H"}]),
    )
    .unwrap()
    .change;
    r.alt_remove(&writer(), &s0, "x").unwrap();
    r.alt_create(&writer(), "S2", None, &[m("p", "a"), m("q", "b")])
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&h))
        .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(!canon_bodies(&r).iter().any(|b| b == "H"));
}

#[test]
fn msw2_a_pre_set_change_only_another_member_holds_is_that_members_alternative() {
    // fix round 1 ruling: written in member b, excluded by b, included only by member a: it is
    // a's alternative, so a non-member view cannot land it while the set is open
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "alt B"}]),
    )
    .unwrap();
    let w = write(
        &mut r,
        "b",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "W"}]),
    )
    .unwrap()
    .change;
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "alt A"}]),
    )
    .unwrap();
    r.view_include(&writer(), "a", std::slice::from_ref(&w))
        .unwrap();
    r.view_exclude(&writer(), "b", std::slice::from_ref(&w), false)
        .unwrap();
    r.alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&w))
        .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message.contains("member x"),
        "a's alternative: {}",
        e.message
    );
    assert!(!canon_bodies(&r).iter().any(|b| b == "W"));
}

fn claim_in(r: &mut Rdy, view: &str, section: &str, body: &str) -> String {
    write(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": section, "body": body}]),
    )
    .unwrap()
    .change
}

#[test]
fn msw2_a_pre_set_change_a_non_member_wrote_binds_to_the_one_member_that_holds_it() {
    // fix round 2 (P2): Z is written in non-member n and included only by member a before the
    // set: the writer does not matter, so Z is a's alternative while the set is open
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    claim_in(&mut r, "b", &section, "alt B");
    let z = claim_in(&mut r, "n", &section, "Z");
    r.view_include(&writer(), "a", std::slice::from_ref(&z))
        .unwrap();
    r.alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&z))
        .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message.contains("member x"),
        "a's alternative: {}",
        e.message
    );
    assert!(!canon_bodies(&r).iter().any(|b| b == "Z"));
}

#[test]
fn msw2_a_pre_set_change_stays_bound_after_its_writer_leaves_the_set() {
    // fix round 2 (P3): P is written in b, included by member a and excluded by b; b leaves the
    // set, and P is still a's alternative, since a is the one member that holds it
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    let p = claim_in(&mut r, "b", &section, "P");
    claim_in(&mut r, "c", &section, "alt C");
    r.view_include(&writer(), "a", std::slice::from_ref(&p))
        .unwrap();
    r.view_exclude(&writer(), "b", std::slice::from_ref(&p), false)
        .unwrap();
    let set = r
        .alt_create(
            &writer(),
            "T",
            None,
            &[m("x", "a"), m("y", "b"), m("z", "c")],
        )
        .unwrap()
        .id;
    r.alt_remove(&writer(), &set, "y").unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&p))
        .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(
        e.message.contains("member x"),
        "a's alternative: {}",
        e.message
    );
    assert!(!canon_bodies(&r).iter().any(|b| b == "P"));
}

/// msw20: canon with a section; a writes `alt A`, b writes `alt B` before the set; the set
/// {x: a, y: b}; b then writes `B2` (linked to y); a records decision "Pick A" and lands
/// choosing x. Returns (set, section, alt B's change, B2's change, the decision).
fn chosen_a(r: &mut Rdy) -> (String, String, String, String, String) {
    let (_doc, section, _claim) = seed_doc(r, "seed");
    land(r, "seed").unwrap();
    claim_in(r, "a", &section, "alt A");
    let b = claim_in(r, "b", &section, "alt B");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let b2 = claim_in(r, "b", &section, "B2");
    let dec = r
        .decide(
            &writer(),
            "a",
            DecisionInput {
                parent: section.clone(),
                statement: "Pick A".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let p = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    (set, section, b, b2, dec)
}

fn short(change: &str) -> String {
    change[..12].to_string()
}

#[test]
fn msw20_a_pre_set_change_stays_bound_when_another_member_includes_it_later() {
    // the re-review's P1 asymmetry: G is a's alone at creation; b's later include made it common
    // ground, so any view could land it. Now it stays x's alternative.
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let g = claim_in(&mut r, "a", &section, "G");
    claim_in(&mut r, "b", &section, "alt B");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    r.view_include(&writer(), "b", std::slice::from_ref(&g))
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&g))
        .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(e.message.contains("member x"), "{}", e.message);
    let e = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&set, "y")))
        .unwrap_err();
    assert_eq!(
        e.code, E_ALT_MEMBER,
        "b cannot choose y with x's change: {}",
        e.message
    );
    assert!(!canon_bodies(&r).iter().any(|b| b == "G"));
}

#[test]
fn msw20_a_change_a_member_includes_after_the_set_was_created_is_not_bound() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    claim_in(&mut r, "b", &section, "alt B");
    r.alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let k = claim_in(&mut r, "n", &section, "K");
    r.view_include(&writer(), "a", std::slice::from_ref(&k))
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&k))
        .unwrap();
    let p = r.propose(&writer(), "d", "d").unwrap();
    r.land(&approver(), &p.id).unwrap();
    assert!(canon_bodies(&r).iter().any(|b| b == "K"));
}

#[test]
fn msw20_common_ground_at_creation_stays_common_when_a_member_excludes_it() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let g = claim_in(&mut r, "b", &section, "ground G");
    r.view_fork(&writer(), "b", "a").unwrap();
    r.alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    r.view_exclude(&writer(), "b", std::slice::from_ref(&g), false)
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&g))
        .unwrap();
    let p = r.propose(&writer(), "d", "d").unwrap();
    r.land(&approver(), &p.id).unwrap();
    assert!(canon_bodies(&r).iter().any(|b| b == "ground G"));
}

#[test]
fn msw20_a_member_added_later_is_bound_to_what_it_held_when_it_joined() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    claim_in(&mut r, "b", &section, "alt B");
    let k = claim_in(&mut r, "c", &section, "K");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    r.alt_add(&writer(), &set, &m("z", "c")).unwrap();
    r.view_exclude(&writer(), "c", std::slice::from_ref(&k), false)
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&k))
        .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(e.message.contains("member z"), "{}", e.message);
    assert!(!canon_bodies(&r).iter().any(|b| b == "K"));
}

/// guard: may pass before the fix (Task 2 never counted a removed member).
#[test]
fn msw20_removing_a_member_releases_its_bindings() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    claim_in(&mut r, "b", &section, "alt B");
    let k = claim_in(&mut r, "c", &section, "K");
    let set = r
        .alt_create(
            &writer(),
            "T",
            None,
            &[m("x", "a"), m("y", "b"), m("z", "c")],
        )
        .unwrap()
        .id;
    r.alt_remove(&writer(), &set, "z").unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&k))
        .unwrap();
    let p = r.propose(&writer(), "d", "d").unwrap();
    r.land(&approver(), &p.id).unwrap();
    assert!(canon_bodies(&r).iter().any(|b| b == "K"));
}

#[test]
fn msw20_a_change_bound_in_two_open_sets_names_both_and_says_to_exclude_it() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    let h = claim_in(&mut r, "b", &section, "H");
    r.view_fork(&writer(), "b", "c").unwrap();
    claim_in(&mut r, "e", &section, "alt E");
    let s1 = r
        .alt_create(&writer(), "S1", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let s2 = r
        .alt_create(&writer(), "S2", None, &[m("p", "c"), m("q", "e")])
        .unwrap()
        .id;
    let e = r
        .propose_with(&writer(), "b", "y", None, None, Some(&pick(&s1, "y")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    let (lo, hi) = if s1 < s2 {
        ((&s1, "y"), (&s2, "p"))
    } else {
        ((&s2, "p"), (&s1, "y"))
    };
    assert_eq!(
        e.message,
        format!(
            "change {} is bound in 2 open sets: {} member {}, {} member {}; one proposal chooses in one set only",
            short(&h),
            lo.0,
            lo.1,
            hi.0,
            hi.1
        )
    );
    assert_eq!(
        e.fix,
        format!(
            "exclude {} from view b (rdy view exclude) and propose without it; it can land once only one of these sets is open",
            short(&h)
        )
    );
}

#[test]
fn msw20_a_losing_members_pre_set_change_is_refused_from_a_non_member_view() {
    let (_d, mut r) = project();
    let (set, _section, b, _b2, dec) = chosen_a(&mut r);
    r.view_include(&writer(), "d", std::slice::from_ref(&b))
        .unwrap();
    let e = r.propose(&writer(), "d", "d").unwrap_err();
    assert_eq!(e.code, E_ALT_CHOSEN, "{}", e.message);
    assert_eq!(
        e.message,
        format!(
            "change {} belongs to member y, which lost set {set} (chosen by {dec})",
            short(&b)
        )
    );
    assert_eq!(
        e.fix,
        format!("land it with a decision that supersedes or refines {dec}")
    );
    assert!(!canon_bodies(&r).iter().any(|x| x == "alt B"));
}

#[test]
fn msw20_a_losing_members_linked_change_is_refused_from_the_chosen_members_view() {
    let (_d, mut r) = project();
    let (_set, _section, _b, b2, _dec) = chosen_a(&mut r);
    r.view_include(&writer(), "a", std::slice::from_ref(&b2))
        .unwrap();
    let e = r.propose(&writer(), "a", "a again").unwrap_err();
    assert_eq!(e.code, E_ALT_CHOSEN, "{}", e.message);
    assert!(e.message.contains("member y"), "{}", e.message);
    assert!(!canon_bodies(&r).iter().any(|x| x == "B2"));
}

/// guard: passes before the fix (nothing binds a chosen set yet); it pins the release. Each
/// release runs in its own project, so the two decisions never meet.
#[test]
fn msw20_a_losing_members_change_lands_with_a_decision_that_refines_or_supersedes_the_choice() {
    for supersede in [false, true] {
        let (_d, mut r) = project();
        let (_set, section, b, b2, dec) = chosen_a(&mut r);
        r.view_include(&writer(), "d", &[b, b2]).unwrap();
        let next = DecisionInput {
            parent: section.clone(),
            statement: "Take B too".into(),
            refines: if supersede { vec![] } else { vec![dec.clone()] },
            ..Default::default()
        };
        if supersede {
            r.supersede(&writer(), "d", &dec, None, next).unwrap();
        } else {
            r.decide(&writer(), "d", next).unwrap();
        }
        let p = r.propose(&writer(), "d", "d").unwrap();
        r.land(&approver(), &p.id).unwrap();
        let bodies = canon_bodies(&r);
        assert!(
            bodies.iter().any(|x| x == "B2") && bodies.iter().any(|x| x == "alt B"),
            "supersede {supersede}: {bodies:?}"
        );
    }
}

/// guard: passes before the fix; an abandoned set binds nothing.
#[test]
fn msw20_an_abandoned_set_binds_nothing() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    let b = claim_in(&mut r, "b", &section, "alt B");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let b2 = claim_in(&mut r, "b", &section, "B2");
    r.alt_abandon(&writer(), &set, "neither").unwrap();
    r.view_include(&writer(), "d", &[b, b2]).unwrap();
    let p = r.propose(&writer(), "d", "d").unwrap();
    r.land(&approver(), &p.id).unwrap();
    let bodies = canon_bodies(&r);
    assert!(
        bodies.iter().any(|x| x == "alt B") && bodies.iter().any(|x| x == "B2"),
        "{bodies:?}"
    );
}

/// RULING Q1: a choice that created no decision releases a loser only with a new decision.
#[test]
fn msw20_a_choice_with_no_decision_releases_a_losers_change_only_with_a_new_decision() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    let b = claim_in(&mut r, "b", &section, "alt B");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let p = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    let landing = r.land(&approver(), &p.id).unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&b))
        .unwrap();
    let e = r.propose(&writer(), "d", "d").unwrap_err();
    assert_eq!(e.code, E_ALT_CHOSEN, "{}", e.message);
    assert_eq!(
        e.message,
        format!(
            "change {} belongs to member y, which lost set {set} (chosen by landing {}, which recorded no decision)",
            short(&b),
            short(&landing)
        )
    );
    assert_eq!(
        e.fix,
        "land it with a new decision that records the revisit"
    );
    r.decide(
        &writer(),
        "d",
        DecisionInput {
            parent: section.clone(),
            statement: "Take B too".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let p = r.propose(&writer(), "d", "d").unwrap();
    r.land(&approver(), &p.id).unwrap();
    assert!(canon_bodies(&r).iter().any(|x| x == "alt B"));
}

#[test]
fn msw20_reindex_and_reopen_reproduce_the_same_bindings() {
    let (d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let g = claim_in(&mut r, "a", &section, "G");
    claim_in(&mut r, "b", &section, "alt B");
    let info = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    r.view_include(&writer(), "b", std::slice::from_ref(&g))
        .unwrap();
    let stable: Hash = info.set.parse().unwrap();
    let before = r.read().coord_heads().alt_binds.clone();
    let g_hash: Hash = g.parse().unwrap();
    assert_eq!(
        before[&stable].bound.get(&g_hash).map(String::as_str),
        Some("a")
    );
    r.reindex().unwrap();
    assert_eq!(r.read().coord_heads().alt_binds, before, "after reindex");
    drop(r);
    let mut r = Rdy::open(d.path()).unwrap();
    assert_eq!(
        r.read().coord_heads().alt_binds,
        before,
        "after reopen (derived from history)"
    );
    r.view_include(&writer(), "d", std::slice::from_ref(&g))
        .unwrap();
    assert_eq!(
        r.propose(&writer(), "d", "d").unwrap_err().code,
        E_ALT_MEMBER
    );
}

/// FW-AL1a Q3: a settled set's members never change, so its losers stay bound. The remove is
/// refused by the open-set rule M2d1 already has; the loser's change still needs a release.
#[test]
fn msw20_alt_remove_on_a_chosen_set_is_refused_so_losers_stay_bound() {
    let (_d, mut r) = project();
    let (set, _section, b, _b2, _dec) = chosen_a(&mut r);
    let e = r.alt_remove(&writer(), &set, "y").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{}", e.message);
    assert_eq!(e.message, format!("{set} is chosen"));
    r.view_include(&writer(), "d", std::slice::from_ref(&b))
        .unwrap();
    let e = r.propose(&writer(), "d", "d").unwrap_err();
    assert_eq!(e.code, E_ALT_CHOSEN, "{}", e.message);
    assert!(e.message.contains("member y"), "{}", e.message);
}

/// msw20 fix round 1 (Group 8 ruling, Q1): when the choice recorded no decision, only a decision
/// the proposal creates in a change that is not itself bound to a loser releases it. The loser's
/// own decision, written in its member view after the set was created, never does.
#[test]
fn msw20_a_losers_own_decision_does_not_release_it_when_the_choice_recorded_none() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    let b = claim_in(&mut r, "b", &section, "alt B");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let use_b = r
        .decide(
            &writer(),
            "b",
            DecisionInput {
                parent: section.clone(),
                statement: "Use B".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .change;
    let p = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    r.view_include(&writer(), "d", &[b, use_b]).unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_CHOSEN, "{}", e.message);
    assert!(e.message.contains("member y"), "{}", e.message);
    let bodies = canon_bodies(&r);
    assert!(!bodies.iter().any(|x| x == "alt B"), "{bodies:?}");
}

/// msw20 fix round 1 (Group 8 ruling, Q1): an Update of a decision never releases a loser, not
/// even a tracking-only one; only a decision the proposal creates does.
#[test]
fn msw20_an_update_only_proposal_does_not_release_a_losers_change() {
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
    let d = tempfile::tempdir().unwrap();
    drop(Rdy::init(d.path(), "rdy-standard").unwrap());
    std::fs::write(d.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(d.path().join(".rdy/config.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    let old = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: section.clone(),
                statement: "Old".into(),
                fields: [("disposition".to_string(), json!("M2"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    let b = claim_in(&mut r, "b", &section, "alt B");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let p = r
        .propose_with(&writer(), "a", "x", None, None, Some(&pick(&set, "x")))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&b))
        .unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: "d".into(),
            ops: serde_json::from_value(
                json!([{"op": "update", "id": old, "fields": {"disposition": "M3"}}]),
            )
            .unwrap(),
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "moved to M3".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_CHOSEN, "{}", e.message);
    assert!(
        e.message.contains("which recorded no decision"),
        "{}",
        e.message
    );
}

/// msw20 fix round 1: a change linked to a listed member of a set is not bound a second time in
/// the same set when another member joins holding it; the gate names its one binding.
#[test]
fn msw20_a_change_linked_to_a_member_is_not_bound_again_when_another_member_joins() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    claim_in(&mut r, "b", &section, "alt B");
    let info = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let set = info.id.clone();
    let h = claim_in(&mut r, "b", &section, "H");
    r.view_exclude(&writer(), "b", std::slice::from_ref(&h), false)
        .unwrap();
    r.view_include(&writer(), "c", std::slice::from_ref(&h))
        .unwrap();
    r.alt_add(&writer(), &set, &m("z", "c")).unwrap();
    let stable: Hash = info.set.parse().unwrap();
    let h_hash: Hash = h.parse().unwrap();
    assert_eq!(
        r.read().coord_heads().alt_binds[&stable].bound.get(&h_hash),
        None,
        "H keeps its link to y only"
    );
    let e = r
        .propose_with(&writer(), "c", "z", None, None, Some(&pick(&set, "z")))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "{}", e.message);
    assert!(e.message.contains("member y"), "{}", e.message);
    assert!(!e.message.contains("open sets"), "{}", e.message);
}

/// msw20 fix round 1: with a choice decision, a decision that neither supersedes nor refines it
/// does not release a loser's change.
#[test]
fn msw20_an_unrelated_decision_does_not_release_a_losers_change() {
    let (_d, mut r) = project();
    let (_set, section, b, _b2, dec) = chosen_a(&mut r);
    r.view_include(&writer(), "d", std::slice::from_ref(&b))
        .unwrap();
    r.decide(
        &writer(),
        "d",
        DecisionInput {
            parent: section.clone(),
            statement: "Unrelated".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let e = r
        .propose(&writer(), "d", "d")
        .and_then(|p| r.land(&approver(), &p.id))
        .unwrap_err();
    assert_eq!(e.code, E_ALT_CHOSEN, "{}", e.message);
    assert!(e.message.contains(&dec), "{}", e.message);
    assert!(!canon_bodies(&r).iter().any(|x| x == "alt B"));
}

/// msw20 fix round 1: two members that hold a change when one of them joins make it common
/// ground; any view lands it after the joiner excludes it.
#[test]
fn msw20_two_holders_when_a_member_joins_make_common_ground() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    claim_in(&mut r, "a", &section, "alt A");
    claim_in(&mut r, "b", &section, "alt B");
    let set = r
        .alt_create(&writer(), "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap()
        .id;
    let k = claim_in(&mut r, "n", &section, "K");
    r.view_include(&writer(), "a", std::slice::from_ref(&k))
        .unwrap();
    r.view_include(&writer(), "c", std::slice::from_ref(&k))
        .unwrap();
    r.alt_add(&writer(), &set, &m("z", "c")).unwrap();
    r.view_exclude(&writer(), "c", std::slice::from_ref(&k), false)
        .unwrap();
    r.view_include(&writer(), "d", std::slice::from_ref(&k))
        .unwrap();
    let p = r.propose(&writer(), "d", "d").unwrap();
    r.land(&approver(), &p.id).unwrap();
    assert!(canon_bodies(&r).iter().any(|b| b == "K"));
}
