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
