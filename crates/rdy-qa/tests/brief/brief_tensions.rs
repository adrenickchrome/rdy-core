//! tbrf1: a briefing's inflight lists the tensions on its blocks (amendment §6).
use rdy_engine::brief::{Coverage, PAGE_SECTIONS};
use rdy_engine::tensions::*;
use rdy_engine::{Scope, Session};
use rdy_model::Role;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;

#[test]
fn tbrf1_a_briefing_lists_the_tensions_on_its_blocks_and_they_are_not_in_the_token() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let cov = Coverage {
        ids: vec![s.a.to_string()],
        path: None,
    };
    let before = r.read().brief(&Scope::Canon, &cov, None).unwrap();
    assert!(before.inflight.tensions.is_empty());
    put(&mut r, "b", cite(&s.section, &s.a));
    put(&mut r, "a", upd(&s.a, "A changed."));
    let b = r.read().brief(&Scope::Canon, &cov, None).unwrap();
    assert_eq!(b.inflight.tensions.len(), 1);
    assert_eq!(b.inflight.tensions[0].kind, TensionKind::PremiseShift);
    assert_eq!(b.inflight.tensions[0].status, "open");
    assert_eq!(
        b.base.token, before.base.token,
        "tensions are not part of the token"
    );
    let t = one(&r, TensionKind::PremiseShift);
    let rev = Session::agent("agent:r".into(), Some("rr".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension_label(&t.id), "fine")
        .unwrap();
    let b = r.read().brief(&Scope::Canon, &cov, None).unwrap();
    assert_eq!(b.inflight.tensions[0].status, "dismissed");
}

#[test]
fn tbrf1_paging_has_a_tensions_section_before_the_base() {
    assert_eq!(PAGE_SECTIONS.len(), 10);
    assert_eq!(PAGE_SECTIONS[8], "inflight.tensions");
    assert_eq!(PAGE_SECTIONS[9], "base.blocks");
}
