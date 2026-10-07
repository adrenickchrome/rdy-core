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
