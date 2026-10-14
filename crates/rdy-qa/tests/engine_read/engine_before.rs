//! Placing a block first: `before` on create and move (first1).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use serde_json::json;

fn bodies(r: &mut Rdy, scope: &Scope, parent: &str) -> Vec<String> {
    let p = BlockId::parse(parent).unwrap();
    r.read()
        .materialized(scope)
        .unwrap()
        .children(Some(&p))
        .iter()
        .map(|b| b.body.clone())
        .collect()
}

fn section_project() -> (tempfile::TempDir, Rdy, String, String, String) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "first"}
    ]))
    .unwrap();
    let id = |k: &str| res.created[k].clone();
    (d, r, id("$d"), id("$s"), id("$a"))
}

#[test]
fn first1_before_puts_a_block_first() {
    let (_d, mut r, doc, s, a) = section_project();
    let scope = Scope::View("work".into());
    let zero = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "before": a, "body": "zeroth"}]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    assert_eq!(bodies(&mut r, &scope, &s), ["zeroth", "first"]);
    let last = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "last"}]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    write(
        &mut r,
        "work",
        json!([{"op": "move", "id": last, "parent": s, "before": zero}]),
    )
    .unwrap();
    assert_eq!(bodies(&mut r, &scope, &s), ["last", "zeroth", "first"]);
    // at the top level too
    let top = write(&mut r, "work", json!([{"op": "create", "type": "doc", "before": doc, "fields": {"title": "Intro", "doc_type": "page"}}]))
        .unwrap()
        .created["op0"]
        .clone();
    assert_eq!(
        r.read().materialized(&scope).unwrap().children(None)[0]
            .id
            .to_string(),
        top
    );
    check(&r, &scope).unwrap();
}

/// Includes Review Focus 2: moving a block before itself is refused.
#[test]
fn first1_before_is_checked_like_after() {
    let (_d, mut r, doc, s, a) = section_project();
    let e = write(&mut r, "work", json!([{"op": "create", "type": "claim", "parent": s, "after": a, "before": a, "body": "x"}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("not both"), "{e}");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "before": doc, "body": "x"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("is not a child of"), "{e}");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": a, "parent": s, "before": a}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("before itself"), "{e}");
}

#[test]
fn first1_before_a_tied_sibling_goes_directly_before_it() {
    let (_d, mut r, _doc, s, _a) = section_project();
    land(&mut r, "work").unwrap();
    for (view, body) in [("va", "twin a"), ("vb", "twin b")] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": s, "body": body}]),
        )
        .unwrap();
    }
    // the two claims share an order key (made by hand: see `land_with_a_tie`)
    land_with_a_tie(&mut r, "va");
    land_with_a_tie(&mut r, "vb");
    let p = BlockId::parse(&s).unwrap();
    let kids: Vec<(String, String)> = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .children(Some(&p))
        .iter()
        .map(|b| (b.order.clone(), b.id.to_string()))
        .collect();
    assert_eq!(
        kids[1].0, kids[2].0,
        "the two views produced tied keys: {kids:?}"
    );
    write(&mut r, "vc", json!([{"op": "create", "type": "claim", "parent": s, "before": kids[2].1, "body": "ahead"}])).unwrap();
    let got = bodies(&mut r, &Scope::View("vc".into()), &s);
    // directly before the second twin, so between the twins (an exact `before`, also under a tie;
    // ruling of 2026-10-12 on D-8EH6E)
    assert_eq!(got[2], "ahead", "directly before the second twin: {got:?}");
    assert_ne!(got[1], "ahead", "{got:?}");
    check(&r, &Scope::View("vc".into())).unwrap();
}
