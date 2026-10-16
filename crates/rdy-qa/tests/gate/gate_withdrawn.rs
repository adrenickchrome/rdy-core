//! vw2 (FW-VW2, D-K0SS1): a version that a close of its view withdrew is refused with
//! `E_VIEW_CLOSED` when it is approved or landed, or when its advice is accepted. A closed view
//! takes no propose (R2). A losing member's version meets `E_VIEW_CLOSED` before the
//! alternative checks (R1).
use rdy_engine::alt::*;
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::{BATCH_VIEW, batch_project};
use rdy_qa::tensions::ag;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

/// A landed seed doc, then view "v" with one claim, proposed once by `writer()`. Returns the
/// seed's section for a later claim.
fn proposed_in_v() -> (tempfile::TempDir, Rdy, String, ProposalInfo) {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "v", claim(&section, "a")).unwrap();
    let p = r.propose(&writer(), "v", "first", None).unwrap();
    (d, r, section, p)
}

fn close_v(r: &mut Rdy) {
    r.view_close(&writer(), &["v".into()], "done").unwrap();
}

/// The op at view `view`'s head.
fn head_op(r: &Rdy, view: &str) -> String {
    r.read().view_head(view).unwrap().head.to_string()
}

/// The exact refusal of `pref`, which a close of view `view` withdrew.
fn is_withdrawn(e: &RdyError, pref: &str, view: &str) {
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_VIEW_CLOSED,
            format!("{pref} was withdrawn when view {view:?} closed").as_str()
        ),
        "{e}"
    );
    assert_eq!(
        e.fix,
        format!(
            "reopen the view with rdy undo --view {view} <close op> (rdy view show {view} lists the ops), then propose again"
        ),
        "{e}"
    );
}

#[test]
fn vw2_landing_a_withdrawn_version_is_e_view_closed() {
    let (_d, mut r, _section, p) = proposed_in_v();
    close_v(&mut r);
    let before = r.read().log_rows().unwrap().len();
    let e = r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_VIEW_CLOSED,
            format!("{} was withdrawn when view \"v\" closed", p.r#ref).as_str()
        ),
        "{e}"
    );
    is_withdrawn(&e, &p.r#ref, "v");
    assert_eq!(r.read().log_rows().unwrap().len(), before);
}

#[test]
fn vw2_after_an_undo_of_the_close_the_new_version_lands() {
    let (_d, mut r, section, p1) = proposed_in_v();
    close_v(&mut r);
    let close = head_op(&r, "v");
    r.undo(&writer(), "v", Some(&close), false).unwrap();
    write(&mut r, "v", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "v", "second", None).unwrap();
    assert_eq!(
        r.read().proposal_show(&p1.r#ref).unwrap().state,
        "superseded"
    );
    r.land(&approver(), &p2.id).unwrap();
    // @2 carried @1's change, so @1 has landed as well (landed comes before superseded)
    assert_eq!(r.read().proposal_show(&p1.r#ref).unwrap().state, "landed");
}

/// R2: a closed view takes no propose; no version and no open ref is written.
#[test]
fn vw2_proposing_a_closed_view_is_e_view_closed_and_writes_no_version() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "v", claim(&section, "a")).unwrap();
    close_v(&mut r);
    let e = r.propose(&writer(), "v", "t", None).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_VIEW_CLOSED, "view \"v\" is closed"),
        "{e}"
    );
    assert!(r.read().open_proposals().unwrap().is_empty());
    assert!(r.read().reader().get_ref("open/v").unwrap().is_none());
}

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
/// Canon with one claim; member views a and b edit it, c is a third member; returns the set id
/// (as `gate_alts.rs` builds it).
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

/// R1: the landing that chose x closed b and withdrew its version; the refusal is
/// `E_VIEW_CLOSED`, before the alternative checks.
#[test]
fn vw2_a_losing_members_version_is_refused_with_e_view_closed() {
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
        (e.code, e.message.as_str()),
        (
            E_VIEW_CLOSED,
            format!("{} was withdrawn when view \"b\" closed", pb.r#ref).as_str()
        ),
        "{e}"
    );
}

#[test]
fn vw2_accepting_advice_on_a_withdrawn_version_is_e_view_closed() {
    let (_d, mut r, b) = batch_project();
    r.view_close(&ag(BATCH_VIEW), &[BATCH_VIEW.into()], "done")
        .unwrap();
    let e = r
        .accept_advice(
            &writer(),
            "human:qa",
            AcceptRequest {
                proposal: b.r#ref.clone(),
                advised_digest: b.report["advised_digest"].as_str().unwrap().into(),
                ..Default::default()
            },
        )
        .unwrap_err();
    is_withdrawn(&e, &b.r#ref, BATCH_VIEW);
}

/// Guard (precedence): a landed version is `E_ALREADY_LANDED` after its view closes.
#[test]
fn vw2_a_landed_version_of_a_closed_view_is_still_e_already_landed() {
    let (_d, mut r, _section, p) = proposed_in_v();
    r.land(&approver(), &p.id).unwrap();
    close_v(&mut r);
    let e = r.land(&approver(), &p.id).unwrap_err();
    assert_eq!(e.code, E_ALREADY_LANDED, "{e}");
}
