//! rdr1 at the engine boundary: a reader cloned from an engine's store keeps answering while the
//! engine writes, and reads exactly the acknowledged state.
use rdy_engine::Scope;
use rdy_qa::fixtures::{land, project, write};
use serde_json::json;

#[test]
fn rdr1_an_engine_readers_view_matches_the_engines_after_each_write() {
    let (_d, mut r) = project();
    let reader = r.store().reader();
    for i in 0..5 {
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "doc", "fields": {"name": format!("d{i}"), "title": "D", "doc_type": "page"}}]),
        )
        .unwrap();
        assert_eq!(
            reader.view_state("work").unwrap(),
            r.store().view_state("work").unwrap()
        );
        assert_eq!(reader.included("work").unwrap().len(), i + 1);
    }
    land(&mut r, "work").unwrap();
    assert_eq!(
        reader.canon_head().unwrap(),
        r.store().canon_head().unwrap()
    );
    assert_eq!(reader.canon_state().unwrap().blocks.len(), 5);
    assert!(
        reader.included("work").unwrap().is_empty(),
        "the landing rebased the view"
    );
    let _ = r.read().materialized(&Scope::Canon).unwrap();
}
