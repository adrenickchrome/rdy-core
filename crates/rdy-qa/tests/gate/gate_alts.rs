//! altg1: a member view proposes its own member; the landing chooses it and closes the others.
use rdy_engine::alt::*;
use rdy_engine::*;
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
