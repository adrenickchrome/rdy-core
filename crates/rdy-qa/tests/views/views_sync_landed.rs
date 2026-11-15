//! scc1 (D-QCE8M, D-NME32, D-EHC2C): a view replay that lands makes its source read landed. The
//! replay op itself changes neither its source view nor canon; the landing that carries the
//! replay lands the source too, at that landing, and the source cannot land again.
use rdy_engine::*;
use rdy_model::Hash;
use rdy_qa::fixtures::*;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

fn hash(s: &str) -> Hash {
    s.parse().unwrap()
}

#[test]
fn scc1_view_replayed_source_reads_landed_after_its_replay_lands() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let sa = as_agent("agent:a", "ra");
    let sb = as_agent("agent:b", "rb");
    // the source: in flight in view a, and included in pinned view p too
    let src = write_as(&mut r, &sa, "a", claim(&section, "from a"))
        .unwrap()
        .change;
    r.view_create(&sa, "p", None, true).unwrap();
    r.view_include(&sa, "p", std::slice::from_ref(&src))
        .unwrap();
    // its replay in view b
    r.view_create(&sb, "b", None, false).unwrap();
    let rep = r.view_replay(&sb, "b", std::slice::from_ref(&src)).unwrap();
    assert_eq!(rep.replayed.len(), 1, "{rep:?}");
    let copy = rep.replayed[0].change.clone();
    let (src_h, copy_h) = (hash(&src), hash(&copy));
    // D-EHC2C: the replay changes neither its source view nor canon
    assert_eq!(r.store().landing_of(&src_h).unwrap(), None);
    assert_eq!(r.store().landing_of(&copy_h).unwrap(), None);
    assert!(r.store().included("a").unwrap().contains(&src_h));
    assert!(r.store().included("p").unwrap().contains(&src_h));

    // b's landing carries the replay: the replay and its source read landed there
    let landing = land(&mut r, "b").unwrap();
    let l = hash(&landing);
    assert_eq!(r.store().landing_of(&copy_h).unwrap(), Some(l));
    assert_eq!(
        r.store().landing_of(&src_h).unwrap(),
        Some(l),
        "the source reads landed"
    );
    // the pinned view still holds the source, which reads landed there
    assert!(r.store().included("p").unwrap().contains(&src_h));
    // the source cannot land again: an include of it is refused as landed
    r.view_create(&sa, "x", None, false).unwrap();
    let e = r
        .view_include(&sa, "x", std::slice::from_ref(&src))
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e:?}");
    assert_eq!(
        e.message,
        format!("change {} already landed in {}", src_h.short(), l.short())
    );
    assert_eq!(e.fix, "it is in canon; nothing to include");
    // fsck reads the same membership from canon's records
    r.store().check_objects().unwrap();
}
