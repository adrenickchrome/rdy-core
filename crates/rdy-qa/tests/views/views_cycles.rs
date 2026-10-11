//! cyc1: an include, a fork and a landing that would make two blocks each other's ancestors are
//! refused; a parent loop a landing's rebase leaves in a view, or one in a damaged store, never
//! hangs a write.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::sync::mpsc;
use std::time::Duration;

/// Doc O with sections A and B, landed. Returns (A, B).
fn seed_ab(r: &mut Rdy) -> (String, String) {
    let res = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "o", "title": "O", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "A"}},
            {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "B"}},
        ]),
    )
    .expect("seed");
    land(r, "seed").expect("land seed");
    (res.created["$a"].clone(), res.created["$b"].clone())
}

fn mv(id: &str, parent: &str) -> serde_json::Value {
    json!([{"op": "move", "id": id, "parent": parent}])
}

fn cycle_refusal(e: &RdyError, a: &str, b: &str) {
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("each other's ancestors")
            && e.message.contains(a)
            && e.message.contains(b),
        "{e}"
    );
}

#[test]
fn cyc1_an_include_that_would_make_two_blocks_each_others_ancestors_is_refused_and_writes_nothing()
{
    let (_d, mut r) = project();
    let (a, b) = seed_ab(&mut r);
    let na = as_agent("agent:a", "r1");
    let sb = as_agent("agent:b", "r1");
    write_as(&mut r, &na, "next", mv(&a, &b)).unwrap();
    let side = write_as(&mut r, &sb, "side", mv(&b, &a)).unwrap().change;
    let before = (
        r.read().view_head("next").cloned(),
        r.read().included("next"),
    );
    let e = r
        .view_include(&na, "next", std::slice::from_ref(&side))
        .unwrap_err();
    cycle_refusal(&e, &a, &b);
    assert!(e.message.contains(&side[..12]), "names the change: {e}");
    assert!(e.fix.contains("exclude the change"), "{e:?}");
    let after = (
        r.read().view_head("next").cloned(),
        r.read().included("next"),
    );
    assert_eq!(before, after, "nothing written");
    // the view still writes
    write_as(
        &mut r,
        &na,
        "next",
        json!([{"op": "create", "type": "claim", "parent": b, "body": "x"}]),
    )
    .unwrap();
    r.fsck().unwrap();
}

/// A landing rebases every open view onto it and is not refused for a view it leaves looping
/// (the gate refuses that view's proposal: `the_gate_refuses_a_parent_cycle_made_by_two_views`);
/// a move into the loop in that view errors instead of hanging the writer.
#[test]
fn cyc1_a_move_into_a_loop_a_landing_left_in_a_view_errors_and_does_not_hang() {
    let (_d, mut r) = project();
    let (a, b) = seed_ab(&mut r);
    write(&mut r, "x", mv(&a, &b)).unwrap();
    let c = write(
        &mut r,
        "x",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": b, "body": "c"}]),
    )
    .unwrap()
    .created["$c"]
        .clone();
    write(&mut r, "y", mv(&b, &a)).unwrap();
    land(&mut r, "y").unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = write(&mut r, "x", mv(&c, &a));
        let _ = tx.send(res.map(|_| ()).map_err(|e| e.to_string()));
    });
    let res = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the write hung in a parent loop");
    let e = res.unwrap_err();
    assert!(e.contains("loop"), "{e}");
}

#[test]
fn cyc1_a_landing_that_would_make_canon_cyclic_is_refused() {
    let (_d, mut r) = project();
    let (a, b) = seed_ab(&mut r);
    // x is pinned: the landing of y does not rebase it, so its proposal lands behind canon
    r.view_create(&writer(), "x", None, true).unwrap();
    write(&mut r, "x", mv(&a, &b)).unwrap();
    let px = r.propose(&writer(), "x", "x").unwrap();
    write(&mut r, "y", mv(&b, &a)).unwrap();
    land(&mut r, "y").unwrap();
    let canon = r.read().canon_head();
    // the gate's own check of the prospective canon refuses it
    let e = r.land(&approver(), &px.id).unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert!(
        e.issues.iter().any(|i| i.code == "E_PARENT_CYCLE"),
        "{:?}",
        e.issues
    );
    assert_eq!(r.read().canon_head(), canon, "nothing landed");
    r.fsck().unwrap();
}

#[test]
fn cyc1_a_fork_whose_rebase_onto_canon_would_be_cyclic_is_refused() {
    let (_d, mut r) = project();
    let (a, b) = seed_ab(&mut r);
    write(&mut r, "x", mv(&a, &b)).unwrap();
    r.view_close(&writer(), &["x".to_string()], "parked")
        .unwrap();
    write(&mut r, "y", mv(&b, &a)).unwrap();
    land(&mut r, "y").unwrap();
    let e = r.view_fork(&writer(), "x", "z").unwrap_err();
    cycle_refusal(&e, &a, &b);
    assert!(!r.read().has_view("z"), "nothing written");
    r.fsck().unwrap();
}

/// A parent loop written straight to the store (bypassing every rule): a move under it errors
/// instead of looping in the ancestor walk.
#[test]
fn cyc1_a_move_under_a_parent_loop_in_a_damaged_store_errors_and_does_not_hang() {
    let (_d, mut r) = project();
    let (_doc, _s, c) = seed_doc(&mut r, "dmg");
    raw_commit(
        &mut r,
        "dmg",
        vec![
            raw(
                "s-XQ00X",
                "section",
                Some("s-XQ00Y"),
                "a",
                &[("title", "X")],
                "",
            ),
            raw(
                "s-XQ00Y",
                "section",
                Some("s-XQ00X"),
                "a",
                &[("title", "Y")],
                "",
            ),
        ],
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = write(&mut r, "dmg", mv(&c, "s-XQ00X"));
        let _ = tx.send(res.map(|_| ()).map_err(|e| e.to_string()));
    });
    let res = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the write hung in a parent loop");
    let e = res.unwrap_err();
    assert!(e.contains("loop"), "{e}");
}
