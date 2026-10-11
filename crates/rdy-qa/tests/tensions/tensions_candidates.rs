//! se4: candidate pairs per change: the five rules, their order, the cap, the exclusions.
use rdy_engine::semantic::candidates::*;
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;

/// The (blocks, rule) of each request, in order.
fn pairs(r: &Rdy, change: &rdy_model::Hash) -> Vec<(Vec<BlockId>, Rule)> {
    r.read()
        .requests_from(change)
        .unwrap()
        .into_iter()
        .map(|q| (q.items.iter().map(|i| i.block.clone()).collect(), q.rule))
        .collect()
}
fn sorted(a: &BlockId, b: &BlockId) -> Vec<BlockId> {
    let mut v = vec![a.clone(), b.clone()];
    v.sort();
    v
}

/// A's pairs when A is edited: B (builds on A) and D (A is decided_by D) by premise, both in
/// A's doc, so by id; then C by section (D is counted once, under premise).
fn a_pairs(s: &Spec) -> Vec<(Vec<BlockId>, Rule)> {
    let mut premise = [s.b.clone(), s.decision.clone()];
    premise.sort();
    let mut want: Vec<_> = premise
        .iter()
        .map(|x| (sorted(&s.a, x), Rule::Premise))
        .collect();
    want.push((sorted(&s.a, &s.c), Rule::Section));
    want
}

#[test]
fn se4_an_edit_pairs_with_its_premises_first_then_its_section() {
    let (_d, mut r) = project();
    let s: Spec = spec_doc(&mut r);
    let w = put(&mut r, "a", upd(&s.a, "A holds, mostly."));
    assert_eq!(pairs(&r, &change_of(&w)), a_pairs(&s));
}

#[test]
fn se4_section_neighbours_come_nearest_first() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // section Rules holds A, B, C, then D (decided into it after C): B and D are one step from C
    let w = put(&mut r, "a", upd(&s.c, "C changed."));
    let mut near = [s.b.clone(), s.decision.clone()];
    near.sort();
    let mut want: Vec<_> = near
        .iter()
        .map(|x| (sorted(x, &s.c), Rule::Section))
        .collect();
    want.push((sorted(&s.a, &s.c), Rule::Section));
    assert_eq!(pairs(&r, &change_of(&w)), want);
}

#[test]
fn se4_blocks_about_one_target_or_naming_one_entity_pair() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // in canon: term Rebase, claim x that names it, claim y that describes it
    let t = created(&put(&mut r, "seed", term(&s.section, "Rebase")), "$t");
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "claim", "id": "$x", "parent": s.section.to_string(), "body": "A rebase keeps pins."},
            {"op": "create", "type": "claim", "id": "$y", "parent": s.section.to_string(), "body": "Pins stay.",
             "links": {"describes": [{"target": t.to_string()}]}}
        ]),
    );
    let (x, y) = (created(&w, "$x"), created(&w, "$y"));
    land(&mut r, "seed").unwrap();
    // in flight: a new page with p (describes Rebase, never names it) and q (names Rebase)
    let w = put(
        &mut r,
        "a",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "A pinned view never moves.",
             "links": {"describes": [{"target": t.to_string()}]}},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$d", "body": "Rebase moves the pinned view too."}
        ]),
    );
    let (p, q) = (created(&w, "$p"), created(&w, "$q"));
    assert_eq!(
        pairs(&r, &change_of(&w)),
        vec![
            (sorted(&p, &y), Rule::Describes),
            (sorted(&q, &x), Rule::Entity),
            (sorted(&p, &q), Rule::Section),
        ],
        "the term's own block never takes part"
    );
}

#[test]
fn se4_at_most_twenty_per_change_closest_first() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ops: Vec<_> = (0..25)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s.section.to_string(), "body": format!("Claim {i} holds.")}))
        .collect();
    let w = put(&mut r, "a", json!(ops));
    let got = pairs(&r, &change_of(&w));
    assert_eq!(got.len(), MAX_CANDIDATES);
    let mut keys: Vec<_> = got.iter().map(|(b, _)| b.clone()).collect();
    keys.dedup();
    assert_eq!(keys.len(), got.len(), "a pair counts once");
}

#[test]
fn se4_deprecated_blocks_never_take_part() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "section", "id": "$old", "parent": s.doc.to_string(), "fields": {"title": "Old", "lifecycle": "deprecated"}},
            {"op": "create", "type": "claim", "id": "$e", "parent": "$old", "body": format!("E builds on {{{{{}}}}}.", s.a)}
        ]),
    );
    land(&mut r, "seed").unwrap();
    let w = put(&mut r, "a", upd(&s.a, "A holds, mostly."));
    assert_eq!(
        pairs(&r, &change_of(&w)),
        a_pairs(&s),
        "E builds on A but is deprecated"
    );
}

#[test]
fn se4_blocks_in_one_area_pair_last() {
    let (_d, mut r) = qa_project();
    // two pages in area payments: no link, no shared term, different parents
    let mut claims = vec![];
    for (path, body) in [
        ("payments/fees", "Fees are monthly."),
        ("payments/refunds", "Refunds take a week."),
    ] {
        r.new_doc(&writer(), "seed", path, Some("page"), Some("Doc"))
            .unwrap();
        let doc = r
            .read()
            .resolve_path(&Scope::View("seed".into()), path)
            .unwrap();
        let w = write(&mut r, "seed", json!([
            {"op": "create", "type": "claim", "id": "$x", "parent": doc.to_string(), "body": body}
        ])).unwrap();
        claims.push(created(&w, "$x"));
    }
    land(&mut r, "seed").unwrap();
    let w = put(&mut r, "a", upd(&claims[0], "Fees are yearly."));
    assert_eq!(
        pairs(&r, &change_of(&w)),
        vec![(sorted(&claims[0], &claims[1]), Rule::Area)]
    );
}

#[test]
fn se4_requests_end_with_their_last_in_flight_side() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let w = put(&mut r, "a", upd(&s.c, "C changed."));
    let c = change_of(&w);
    assert_eq!(pairs(&r, &c).len(), 3, "B, D and A by section");
    land(&mut r, "a").unwrap();
    assert!(
        r.read().requests_from(&c).unwrap().is_empty(),
        "a landed change makes no request (F3)"
    );
}

#[test]
fn se4_request_labels_are_rq_and_12_hex() {
    let k = rdy_model::Hash([0xab; 32]);
    assert_eq!(request_label(&k), format!("rq-{}", k.short()));
    assert_eq!(request_hex("rq-ABABAB").as_deref(), Some("ababab"));
    assert_eq!(request_hex("rq-abc"), None);
}

#[test]
fn se4_pair_rule_names_the_best_rule_that_holds_now() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A holds, mostly."));
    let st = r.read();
    assert_eq!(st.pair_rule(&s.a, &s.b).unwrap(), Some(Rule::Premise));
    assert_eq!(st.pair_rule(&s.a, &s.c).unwrap(), Some(Rule::Section));
    assert_eq!(st.pair_rule(&s.a, &s.a).unwrap(), None);
    assert_eq!(
        st.pair_rule(&s.a, &s.doc).unwrap(),
        None,
        "a doc takes no part"
    );
}

#[test]
fn se4_item_now_finds_the_in_flight_write_else_canon() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let w = put(&mut r, "a", upd(&s.a, "A holds, mostly."));
    let st = r.read();
    let items = st.requests_from(&change_of(&w)).unwrap()[0].items.clone();
    let mine = items.iter().find(|i| i.block == s.a).unwrap();
    assert_eq!(
        st.item_now(&s.a, mine.content).unwrap().as_ref(),
        Some(mine),
        "A's content is the change's write"
    );
    let theirs = items
        .iter()
        .find(|i| i.block == s.b || i.block == s.decision)
        .unwrap();
    let got = st.item_now(&theirs.block, theirs.content).unwrap().unwrap();
    assert!(
        matches!(got.side, rdy_engine::tensions::SideRef::Canon { .. }),
        "no write touches it"
    );
    assert_eq!(
        st.item_now(&s.a, theirs.content).unwrap(),
        None,
        "no version of A has that content"
    );
}
