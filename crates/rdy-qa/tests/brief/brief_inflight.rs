//! infl1: work in flight on a briefing's blocks: other views that change them and open proposals
//! that touch them (FW-E1, amendment §4.5 item 1).
use rdy_engine::*;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use serde_json::{Value, json};

fn upd(id: &impl ToString, body: &str) -> Value {
    json!([{"op": "update", "id": id.to_string(), "body": body}])
}
fn names(b: &Briefing) -> Vec<String> {
    b.inflight.views.iter().map(|v| v.view.clone()).collect()
}

#[test]
fn infl1_a_briefing_lists_other_views_that_change_a_covered_block() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write_as(
        &mut r,
        &as_agent("agent:a", "r1"),
        "va",
        upd(&s.a, "A in va."),
    )
    .unwrap();
    write_as(
        &mut r,
        &as_agent("agent:c", "r1"),
        "vc",
        upd(&s.c, "C in vc."),
    )
    .unwrap();
    let b = brief_of(&r, &Scope::Canon, &[&s.a, &s.b]);
    assert!(names(&b).contains(&"va".to_string()), "{:?}", b.inflight);
    assert!(
        !names(&b).contains(&"vc".to_string()),
        "vc changes no covered block: {:?}",
        b.inflight
    );
    let va = b.inflight.views.iter().find(|v| v.view == "va").unwrap();
    assert_eq!(va.blocks, vec![s.a.to_string()]);
    assert_eq!(
        va.owner,
        Some(OwnerInfo {
            actor: "agent:a".into(),
            run: Some("r1".into())
        })
    );
    let ms = r.read().view_head("va").unwrap().last_write_ms;
    assert_eq!(va.last_write_ms, ms);
    assert!(va.last_write.starts_with("20"), "{}", va.last_write);
}

#[test]
fn infl1_the_briefings_own_view_and_closed_views_are_not_listed() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(&mut r, "va", upd(&s.a, "A in va.")).unwrap();
    write(&mut r, "vb", upd(&s.a, "A in vb.")).unwrap();
    let in_va = brief_of(&r, &Scope::View("va".into()), &[&s.a]);
    assert!(
        !names(&in_va).contains(&"va".to_string()),
        "{:?}",
        in_va.inflight
    );
    assert!(
        names(&in_va).contains(&"vb".to_string()),
        "{:?}",
        in_va.inflight
    );
    r.view_close(&writer(), &["vb".to_string()], "done")
        .unwrap();
    let after = brief_of(&r, &Scope::View("va".into()), &[&s.a]);
    assert!(
        !names(&after).contains(&"vb".to_string()),
        "{:?}",
        after.inflight
    );
}

#[test]
fn infl1_open_proposals_that_touch_a_covered_block_are_listed_at_their_latest_version() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(&mut r, "pa", upd(&s.a, "A in pa.")).unwrap();
    let p1 = r.propose(&writer(), "pa", "Change A", None).unwrap();
    let b = brief_of(&r, &Scope::Canon, &[&s.a]);
    assert_eq!(b.inflight.proposals.len(), 1, "{:?}", b.inflight);
    let p = &b.inflight.proposals[0];
    assert_eq!(
        (p.r#ref.as_str(), p.view.as_str(), p.title.as_str()),
        (p1.r#ref.as_str(), "pa", "Change A")
    );
    assert_eq!(p.blocks, vec![s.a.to_string()]);
    write(&mut r, "pa", upd(&s.c, "C in pa.")).unwrap();
    let p2 = r
        .propose(&writer(), "pa", "Change A and C", Some(TEST_SUMMARY))
        .unwrap();
    let b = brief_of(&r, &Scope::Canon, &[&s.a]);
    let refs: Vec<&str> = b
        .inflight
        .proposals
        .iter()
        .map(|p| p.r#ref.as_str())
        .collect();
    assert_eq!(refs, vec![p2.r#ref.as_str()], "only the latest version");
    r.land(&approver(), &p2.id).unwrap();
    let b = brief_of(&r, &Scope::Canon, &[&s.a]);
    assert!(b.inflight.proposals.is_empty(), "landed: {:?}", b.inflight);
    assert!(
        !names(&b).contains(&"pa".to_string()),
        "pa's changes landed: {:?}",
        b.inflight
    );
}

#[test]
fn infl1_a_cut_shortens_inflight_and_keeps_coverage() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    for i in 0..40 {
        write(
            &mut r,
            &format!("v{i:02}"),
            upd(&s.a, &format!("A in v{i}.")),
        )
        .unwrap();
    }
    let full = brief_of(&r, &Scope::Canon, &[&s.a]);
    assert_eq!(
        full.inflight.views.len(),
        40,
        "the 780 edit collisions are cut before a view (Q15, ruling on item 54)"
    );
    let cov = Coverage {
        ids: vec![s.a.to_string()],
        path: None,
    };
    let cut = r.read().brief(&Scope::Canon, &cov, Some(300)).unwrap();
    assert!(cut.truncated);
    assert!(cut.inflight.views.len() < 40);
    assert_eq!(
        cut.omitted.get("inflight.views").copied(),
        Some(40 - cut.inflight.views.len())
    );
    assert_eq!(
        cut.base.token, full.base.token,
        "a cut never changes coverage"
    );
}

#[test]
fn infl1_an_open_proposals_written_blocks_are_read_once_per_version() {
    // a proposal version never changes, so the blocks it writes are memoised across briefings
    // and published states; a new version is read once more
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(&mut r, "pa", upd(&s.a, "A in pa.")).unwrap();
    r.propose(&writer(), "pa", "Change A", None).unwrap();
    assert_eq!(r.read().proposal_touched_cache_len(), 0);
    let first = brief_of(&r, &Scope::Canon, &[&s.a]);
    assert_eq!(r.read().proposal_touched_cache_len(), 1);
    write(&mut r, "other", upd(&s.c, "C in other.")).unwrap();
    let again = brief_of(&r, &Scope::Canon, &[&s.a, &s.b]);
    assert_eq!(again.inflight.proposals, first.inflight.proposals);
    assert_eq!(r.read().proposal_touched_cache_len(), 1, "read once");
    write(&mut r, "pa", upd(&s.c, "C in pa.")).unwrap();
    r.propose(&writer(), "pa", "Change A and C", None).unwrap();
    let b = brief_of(&r, &Scope::Canon, &[&s.a]);
    assert_eq!(b.inflight.proposals.len(), 1);
    assert_eq!(r.read().proposal_touched_cache_len(), 2);
}
