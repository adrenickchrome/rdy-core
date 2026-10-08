//! pmap1 (spec §15.2): the incremental `with` agrees with the conflict-aware merge's state when
//! the merge records no conflict, over random op sequences in one view.
use proptest::prelude::*;
use rdy_engine::Scope;
use rdy_qa::fixtures::{cases, project, write};
use serde_json::json;

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(24), ..ProptestConfig::default() })]
    #[test]
    fn pmap1_with_matches_the_merge_state_for_random_single_view_histories(steps in proptest::collection::vec((0u8..3, 0u8..6), 1..12)) {
        let (_d, mut r) = project();
        write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "T", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": "seed"}
        ])).unwrap();
        let work = Scope::View("work".into());
        let mut m = (*r.read().materialized(&work).unwrap()).clone();
        let section = m.iter().find(|b| b.block_type == "section").unwrap().id.clone();
        for (kind, pick) in steps {
            let claims: Vec<_> = m.iter().filter(|b| b.block_type == "claim").map(|b| b.id.clone()).collect();
            let op = match kind {
                0 => json!({"op": "create", "type": "claim", "parent": section.to_string(), "body": format!("c{pick}")}),
                1 if !claims.is_empty() => { let id = &claims[pick as usize % claims.len()]; json!({"op": "update", "id": id.to_string(), "body": format!("u{pick}")}) }
                _ if claims.len() > 1 => { let id = &claims[pick as usize % claims.len()]; json!({"op": "delete", "id": id.to_string()}) }
                _ => continue,
            };
            if write(&mut r, "work", json!([op])).is_err() { continue; }
            let v = r.store().view("work").unwrap();
            let c = r.store().change(v.changes.last().unwrap()).unwrap();
            m = m.with(&c.ops);
            let merged = r.store().view_merge("work").unwrap();
            prop_assert!(merged.conflicts.is_empty());
            prop_assert_eq!(m.snapshot(), merged.state);
            let fresh = r.read().materialized(&work).unwrap();
            prop_assert_eq!(m.snapshot(), fresh.snapshot());
        }
    }
}
