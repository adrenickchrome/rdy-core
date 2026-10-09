//! tsrch1: search's tension filter (FW-SR4) keeps only hits involved in an open tension.
use rdy_engine::tensions::*;
use rdy_engine::{Scope, Session};
use rdy_model::Role;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;

/// AE2's search query with `text` and the tension filter set.
fn search_query(text: &str, tension: bool) -> rdy_engine::search::SearchQuery {
    rdy_engine::search::SearchQuery {
        text: text.into(),
        filters: rdy_engine::search::SearchFilters {
            tension,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn tsrch1_the_tension_filter_keeps_only_blocks_in_an_open_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "Shared word: alpha."));
    put(&mut r, "b", upd(&s.a, "Shared word: beta."));
    put(&mut r, "c", upd(&s.c, "Shared word: gamma."));
    let t = one(&r, TensionKind::EditCollision);
    let st = r.read();
    let blocks = st.open_tension_blocks(rdy_engine::now_ms()).unwrap();
    assert!(blocks.contains(&s.a) && !blocks.contains(&s.c));
    // The inflight scope leaves out a block two views edit, so search the views one by one.
    let ids = |view: &str, tension: bool| -> Vec<String> {
        let q = search_query("Shared word", tension);
        st.search(&Scope::View(view.into()), &q, None, None)
            .unwrap()
            .matches
            .iter()
            .map(|m| m.id.clone())
            .collect()
    };
    assert_eq!(ids("a", false), [s.a.to_string()]);
    assert_eq!(ids("a", true), [s.a.to_string()]);
    assert_eq!(ids("c", false), [s.c.to_string()]);
    assert!(
        ids("c", true).is_empty(),
        "c's block is in no open tension: {:?}",
        ids("c", true)
    );
    drop(st);
    let rev = Session::agent("agent:r".into(), Some("rr".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension_label(&t.id), "fine")
        .unwrap();
    assert!(
        r.read()
            .open_tension_blocks(rdy_engine::now_ms())
            .unwrap()
            .is_empty(),
        "a dismissed tension is not open"
    );
}

/// Checklist line 47: the blocks of a read-time overlap are in an open tension while the intent
/// is open, and not once it expired.
#[test]
fn tsrch1_a_read_time_overlap_counts_until_its_intent_expires() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let it = r
        .intent_post(
            &ag("i"),
            Some("i"),
            None,
            &[s.a.to_string()],
            "rework A",
            None,
        )
        .unwrap();
    put(&mut r, "b", upd(&s.a, "A by b."));
    let st = r.read();
    assert!(
        st.open_tension_blocks(rdy_engine::now_ms())
            .unwrap()
            .contains(&s.a)
    );
    assert!(
        !st.open_tension_blocks(it.expires_at_ms)
            .unwrap()
            .contains(&s.a)
    );
}

/// Checklist line 47: an all-canon tension (FW-CH6) counts: its blocks are in an open tension.
#[test]
fn tsrch1_an_all_canon_tension_counts() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let w = put(
        &mut r,
        "w",
        serde_json::json!([{"op": "create", "type": "claim", "id": "$y",
            "parent": s.section.to_string(), "body": "Y.",
            "links": {"decided_by": [{"target": s.decision.to_string()}]}}]),
    );
    let y = created(&w, "$y");
    land(&mut r, "w").unwrap();
    r.supersede(
        &ag("n"),
        "n",
        &s.decision.to_string(),
        None,
        rdy_engine::DecisionInput {
            parent: s.section.to_string(),
            statement: "A new rule".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "n").unwrap();
    let t = one_involving(&r, TensionKind::DeprecatedReference, &y);
    assert!(t.sides.iter().all(|x| matches!(x, SideRef::Canon { .. })));
    let blocks = r.read().open_tension_blocks(rdy_engine::now_ms()).unwrap();
    assert!(
        blocks.contains(&y) && blocks.contains(&s.decision),
        "{blocks:?}"
    );
}
