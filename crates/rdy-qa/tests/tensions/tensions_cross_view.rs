//! nb5 (D-YXBXH QE3, D-M1ZQG): review pairs across open views. The `premise`, `describes` and
//! `entity` rules pair in-flight blocks in two views that share no view, at the contents those
//! writes give them, after the same-view phase and inside the cap of 20 per change. `section`
//! and `area` never pair across views.
use rdy_engine::check::{AgentCheck, CaseInputs, build_case};
use rdy_engine::engine::write_typed;
use rdy_engine::semantic::api::run_review_submit;
use rdy_engine::semantic::candidates::*;
use rdy_engine::semantic::case::case_hash;
use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest, SubmitInfo};
use rdy_engine::tensions::SideRef;
use rdy_engine::{E_REVIEW_STALE, Engine, Local, Rdy, RdyError, ReadState, Scope, Session};
use rdy_model::check_request::CheckSubject;
use rdy_model::{BlockId, Hash, PairItem, Role, pair_key};
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn view(name: &str) -> Scope {
    Scope::View(name.into())
}

/// The content of `id` as `scope` holds it.
fn content(r: &Rdy, scope: &Scope, id: &BlockId) -> Hash {
    r.read()
        .materialized(scope)
        .unwrap()
        .get(id)
        .unwrap_or_else(|| panic!("{id} in {scope:?}"))
        .content_hash_with(r.store().tracking())
}

/// The version of `id` as `scope` holds it.
fn version(r: &Rdy, scope: &Scope, id: &BlockId) -> Hash {
    r.read()
        .materialized(scope)
        .unwrap()
        .version_of(id)
        .unwrap()
}

fn reqs(r: &Rdy, c: &Hash) -> Vec<Request> {
    r.read().requests_from(c).unwrap()
}

/// The key of the pair of `a` and `b` at those contents.
fn key_of(a: (&BlockId, Hash), b: (&BlockId, Hash)) -> Hash {
    pair_key(&[
        PairItem {
            block: a.0.clone(),
            content: a.1,
        },
        PairItem {
            block: b.0.clone(),
            content: b.1,
        },
    ])
}

/// The request in `qs` whose key is `key`.
fn with_key(qs: &[Request], key: Hash) -> Option<&Request> {
    qs.iter().find(|q| q.key == key)
}

/// Whether a request pairs two writes of different changes.
fn two_changes(q: &Request) -> bool {
    let sides: BTreeSet<Hash> = q
        .items
        .iter()
        .filter_map(|i| match i.side {
            SideRef::Change(c) => Some(c),
            _ => None,
        })
        .collect();
    sides.len() == 2
}

fn reviewer() -> Session {
    Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap()
}

fn verdict(q: &Request, hash: &Hash, word: &str) -> ReviewSubmitRequest {
    ReviewSubmitRequest {
        request: request_label(&q.key),
        case_hash: hash.to_string(),
        verdict: word.into(),
        citations: q
            .items
            .iter()
            .take(1)
            .map(|i| CitationRequest {
                block: i.block.to_string(),
                note: "these two cannot both hold".into(),
            })
            .collect(),
        severity: None,
        reason: None,
    }
}

/// Submits a `no_tension` verdict on `q` with the case hash read now.
fn judge(r: &mut Rdy, q: &Request) -> Result<SubmitInfo, RdyError> {
    let h = case_hash(&r.read().case_of(&q.key, &q.items).unwrap());
    r.submit_verdict(&reviewer(), verdict(q, &h, "no_tension"))
}

/// The pair of `a` (written by change `ca` in view `va`) and `b` (by `cb` in `vb`) across views:
/// each change makes it at those two contents, the items name each write's version and content,
/// the case binds those versions under `rule`, and a verdict settles those contents.
fn assert_cross(r: &mut Rdy, a: (&BlockId, &str, Hash), b: (&BlockId, &str, Hash), rule: Rule) {
    let (ka, kb) = (content(r, &view(a.1), a.0), content(r, &view(b.1), b.0));
    let key = key_of((a.0, ka), (b.0, kb));
    let mut want = vec![
        Item {
            block: a.0.clone(),
            side: SideRef::Change(a.2),
            version: version(r, &view(a.1), a.0),
            content: ka,
        },
        Item {
            block: b.0.clone(),
            side: SideRef::Change(b.2),
            version: version(r, &view(b.1), b.0),
            content: kb,
        },
    ];
    want.sort();
    for c in [a.2, b.2] {
        let qs = reqs(r, &c);
        let q = with_key(&qs, key).unwrap_or_else(|| {
            panic!("change {c} pairs {} and {} across views: {qs:#?}", a.0, b.0)
        });
        assert_eq!(q.items, want, "the exact writes");
        assert_eq!(q.rule, rule);
        assert_eq!(q.change, c, "the request belongs to each change");
    }
    let q = with_key(&reqs(r, &a.2), key).unwrap().clone();
    assert_eq!(
        r.read()
            .pair_now(&[(a.0.clone(), ka), (b.0.clone(), kb)])
            .unwrap(),
        Some(want.clone()),
        "pair_now names the cross-view form"
    );
    let case = r.read().case_of(&q.key, &q.items).unwrap();
    assert_eq!(case.rule, Some(rule));
    let versions: Vec<String> = case.blocks.iter().map(|b| b.version.clone()).collect();
    assert_eq!(
        versions,
        want.iter()
            .map(|i| i.version.to_string())
            .collect::<Vec<_>>()
    );
    judge(r, &q).unwrap();
    let st = r.read();
    let settled = st
        .semantic()
        .settled(&key)
        .expect("the verdict settles the pair");
    let mut got: Vec<(BlockId, Hash)> = settled
        .items
        .iter()
        .map(|i| (i.block.clone(), i.content))
        .collect();
    got.sort();
    let mut exp = vec![(a.0.clone(), ka), (b.0.clone(), kb)];
    exp.sort();
    assert_eq!(got, exp, "the verdict binds the two contents");
}

#[test]
fn nb5_each_tie_rule_pairs_actual_contents_across_views() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let t = created(&put(&mut r, "seed", term(&s.section, "Rebase")), "$t");
    land(&mut r, "seed").unwrap();
    // premise, both edited: view a edits A, view b edits B, which builds on A
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A holds, mostly.")));
    let cb = change_of(&put(
        &mut r,
        "b",
        upd(&s.b, &format!("B builds on {{{{{}}}}}, firmly.", s.a)),
    ));
    // premise, one new: view c creates X on C, view d edits C
    let w = put(&mut r, "c", cite(&s.section, &s.c));
    let (x, cc) = (created(&w, "$y"), change_of(&w));
    let cd = change_of(&put(&mut r, "d", upd(&s.c, "C stands, edited.")));
    // describes, both new: views e and f each create a block about Rebase
    let about = |body: &str| {
        json!([{"op": "create", "type": "claim", "id": "$p", "parent": s.section.to_string(),
                "body": body, "links": {"describes": [{"target": t.to_string()}]}}])
    };
    let w = put(&mut r, "e", about("A pinned view never moves."));
    let (p, ce) = (created(&w, "$p"), change_of(&w));
    let w = put(&mut r, "f", about("Pins stay put."));
    let (q, cf) = (created(&w, "$p"), change_of(&w));
    // entity, both new, plural against singular: views g and h in a new page
    let w = put(
        &mut r,
        "g",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": s.section.to_string(),
                "body": "Rebases keep pins."}]),
    );
    let (n, cg) = (created(&w, "$n"), change_of(&w));
    let w = put(
        &mut r,
        "h",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$m", "parent": "$d", "body": "A rebase moves the pin."}
        ]),
    );
    let (m, ch) = (created(&w, "$m"), change_of(&w));

    assert_cross(&mut r, (&s.a, "a", ca), (&s.b, "b", cb), Rule::Premise);
    assert_cross(&mut r, (&x, "c", cc), (&s.c, "d", cd), Rule::Premise);
    assert_cross(&mut r, (&p, "e", ce), (&q, "f", cf), Rule::Describes);
    assert_cross(&mut r, (&n, "g", cg), (&m, "h", ch), Rule::Entity);
}

#[test]
fn nb5_section_and_area_never_cross_views() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // C and B share a parent only
    let ca = change_of(&put(&mut r, "a", upd(&s.c, "C changed.")));
    let cb = change_of(&put(
        &mut r,
        "b",
        upd(&s.b, &format!("B builds on {{{{{}}}}}, in view b.", s.a)),
    ));
    for c in [ca, cb] {
        let qs = reqs(&r, &c);
        assert!(!qs.is_empty(), "each change still pairs with canon");
        assert!(
            !qs.iter().any(two_changes),
            "no section pair across views: {qs:#?}"
        );
    }
    let qs = reqs(&r, &ca);
    assert!(
        qs.iter().any(|q| q.rule == Rule::Section
            && q.items
                .iter()
                .any(|i| i.block == s.b && matches!(i.side, SideRef::Canon { .. }))),
        "C pairs with B as canon holds it, within view a"
    );

    // two pages in one area: no link, no shared term, different parents
    let (_d, mut r) = qa_project();
    let mut claims = vec![];
    for (path, body) in [
        ("payments/fees", "Fees are monthly."),
        ("payments/refunds", "Refunds take a week."),
    ] {
        r.new_doc(&writer(), "seed", path, Some("page"), Some("Doc"))
            .unwrap();
        let doc = r.read().resolve_path(&view("seed"), path).unwrap();
        let w = write(&mut r, "seed", json!([
            {"op": "create", "type": "claim", "id": "$x", "parent": doc.to_string(), "body": body}
        ]))
        .unwrap();
        claims.push(created(&w, "$x"));
    }
    land(&mut r, "seed").unwrap();
    let ca = change_of(&put(&mut r, "a", upd(&claims[0], "Fees are yearly.")));
    let cb = change_of(&put(&mut r, "b", upd(&claims[1], "Refunds take a day.")));
    for c in [ca, cb] {
        let qs = reqs(&r, &c);
        assert_eq!(qs.len(), 1, "the area pair against canon: {qs:#?}");
        assert_eq!(qs[0].rule, Rule::Area);
        assert!(!two_changes(&qs[0]), "no area pair across views");
    }
}

/// Canon: section Many holds `siblings` claims (K0 first) and section Targets holds T1 and T2,
/// in one doc. View a makes K0 build on T1 and T2; views b and c edit T1 and T2.
fn many(siblings: usize) -> (tempfile::TempDir, Rdy, Hash, [BlockId; 2], [Hash; 2]) {
    let (d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Many"}}),
        json!({"op": "create", "type": "section", "id": "$u", "parent": "$d", "fields": {"title": "Targets"}}),
        json!({"op": "create", "type": "claim", "id": "$t1", "parent": "$u", "body": "T1 holds."}),
        json!({"op": "create", "type": "claim", "id": "$t2", "parent": "$u", "body": "T2 holds."}),
    ];
    for i in 0..siblings {
        ops.push(
            json!({"op": "create", "type": "claim", "id": format!("$k{i}"), "parent": "$s",
                        "body": format!("K{i} holds.")}),
        );
    }
    let w = write(&mut r, "seed", json!(ops)).unwrap();
    let (t1, t2, k0) = (created(&w, "$t1"), created(&w, "$t2"), created(&w, "$k0"));
    land(&mut r, "seed").unwrap();
    let ca = change_of(&put(
        &mut r,
        "a",
        upd(&k0, &format!("K0 builds on {{{{{t1}}}}} and {{{{{t2}}}}}.")),
    ));
    let cb = change_of(&put(&mut r, "b", upd(&t1, "T1 changed.")));
    let cc = change_of(&put(&mut r, "c", upd(&t2, "T2 changed.")));
    (d, r, ca, [t1, t2], [cb, cc])
}

#[test]
fn nb5_same_view_phase_precedes_cross_view_inside_twenty() {
    // 2 premise pairs with canon and 17 section pairs, then 2 cross-view premise pairs
    let (_d, r, ca, [t1, t2], [cb, cc]) = many(18);
    let qs = reqs(&r, &ca);
    assert_eq!(qs.len(), MAX_CANDIDATES);
    assert!(
        qs[..19].iter().all(|q| !two_changes(q)),
        "the same-view phase comes first, whole: {qs:#?}"
    );
    assert_eq!(qs[0].rule, Rule::Premise);
    assert!(qs[2..19].iter().all(|q| q.rule == Rule::Section));
    let last = &qs[19];
    assert!(two_changes(last), "{last:#?}");
    assert_eq!(last.rule, Rule::Premise);
    // ranked: same doc, same path, then the other block's id
    let (first, by) = if t1 < t2 { (&t1, cb) } else { (&t2, cc) };
    let theirs = last
        .items
        .iter()
        .find(|i| i.block == *first)
        .expect("the ranked first cross pair names the smaller target id");
    assert_eq!(theirs.side, SideRef::Change(by));

    // at 20 from the same-view phase, none is added and nothing is indexed
    let (_d, r, ca, _, _) = many(19);
    let st = r.read();
    let before = cross_view_index_visits();
    let qs = st.requests_from(&ca).unwrap();
    assert_eq!(
        cross_view_index_visits() - before,
        0,
        "a full first phase builds no index"
    );
    assert_eq!(qs.len(), MAX_CANDIDATES);
    assert!(!qs.iter().any(two_changes), "{qs:#?}");
}

#[test]
fn nb5_different_contents_do_not_hide_a_cross_view_pair() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A holds, mostly.")));
    let text = format!("B builds on {{{{{}}}}}, firmly.", s.a);
    let cb = change_of(&put(&mut r, "b", upd(&s.b, &text)));
    let ka = content(&r, &view("a"), &s.a);
    let canon_b = content(&r, &Scope::Canon, &s.b);
    let view_b = content(&r, &view("b"), &s.b);
    assert_ne!(canon_b, view_b);
    let (k_canon, k_view) = (
        key_of((&s.a, ka), (&s.b, canon_b)),
        key_of((&s.a, ka), (&s.b, view_b)),
    );
    assert_ne!(k_canon, k_view, "a pair key reads block and content");
    let qs = reqs(&r, &ca);
    assert!(
        with_key(&qs, k_canon).is_some(),
        "the same-view pair with canon B"
    );
    assert!(
        with_key(&qs, k_view).is_some(),
        "the canon pair does not hide the pair on view b's B: {qs:#?}"
    );
    // view c writes B with view b's text: one content, one pair
    let cc = change_of(&put(&mut r, "c", upd(&s.b, &text)));
    assert_eq!(
        content(&r, &view("c"), &s.b),
        view_b,
        "one content in two views"
    );
    let qs = reqs(&r, &ca);
    let on_b: Vec<&Request> = qs
        .iter()
        .filter(|q| {
            q.items
                .iter()
                .any(|i| i.block == s.b && i.content == view_b)
        })
        .collect();
    assert_eq!(
        on_b.len(),
        1,
        "identical contents in two views make one pair: {qs:#?}"
    );
    assert_eq!(on_b[0].key, k_view);
    let side = on_b[0]
        .items
        .iter()
        .find(|i| i.block == s.b)
        .unwrap()
        .side
        .clone();
    assert_eq!(
        side,
        SideRef::Change(cb.min(cc)),
        "the first write by change id names the equal side"
    );
    let all = r.read().review_requests().unwrap();
    assert_eq!(
        all.iter().filter(|q| q.key == k_view).count(),
        1,
        "queued once"
    );
}

#[test]
fn nb5_cross_view_pairs_expire_and_rekey_through_landing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A holds, mostly.")));
    let ka = content(&r, &view("a"), &s.a);
    let b_text = |n: &str| format!("B builds on {{{{{}}}}}, {n}.", s.a);
    let b1 = change_of(&put(&mut r, "b", upd(&s.b, &b_text("first"))));
    let cross_key = |r: &Rdy| key_of((&s.a, ka), (&s.b, content(r, &view("b"), &s.b)));
    let k1 = cross_key(&r);
    let q1 = with_key(&reqs(&r, &ca), k1)
        .expect("the cross pair")
        .clone();
    let h1 = case_hash(&r.read().case_of(&q1.key, &q1.items).unwrap());
    let stale = |r: &mut Rdy, q: &Request, h: &Hash| {
        let e = r
            .submit_verdict(&reviewer(), verdict(q, h, "no_tension"))
            .unwrap_err();
        assert_eq!(
            (e.code, e.message),
            (
                E_REVIEW_STALE,
                format!(
                    "request {} is no longer current: a side changed, landed or left every view",
                    request_label(&q.key)
                )
            )
        );
    };

    // an edit of one side ends the pair and keys a new one
    let b2 = change_of(&put(&mut r, "b", upd(&s.b, &b_text("second"))));
    let k2 = cross_key(&r);
    let qs = reqs(&r, &ca);
    assert!(with_key(&qs, k1).is_none(), "the edited side's pair ended");
    let q2 = with_key(&qs, k2).expect("the new contents pair").clone();
    assert!(reqs(&r, &b1).is_empty(), "an overwritten write makes none");
    assert!(with_key(&reqs(&r, &b2), k2).is_some());
    stale(&mut r, &q1, &h1);

    // excluding the other side ends it
    let h2 = case_hash(&r.read().case_of(&q2.key, &q2.items).unwrap());
    r.view_exclude(&ag("b"), "b", &[b1.to_string(), b2.to_string()], true)
        .unwrap();
    assert!(
        !reqs(&r, &ca).iter().any(two_changes),
        "no other side in flight"
    );
    stale(&mut r, &q2, &h2);

    // closing the other side's view ends it
    put(&mut r, "b", upd(&s.b, &b_text("third")));
    let k3 = cross_key(&r);
    let q3 = with_key(&reqs(&r, &ca), k3)
        .expect("the third pair")
        .clone();
    let h3 = case_hash(&r.read().case_of(&q3.key, &q3.items).unwrap());
    r.view_close(&ag("b"), &["b".into()], "done").unwrap();
    assert!(!reqs(&r, &ca).iter().any(two_changes));
    stale(&mut r, &q3, &h3);

    // landing one side keeps the pair's key in the in-flight/canon form; landing the last
    // in-flight side ends it
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ca = change_of(&put(&mut r, "a", upd(&s.a, "A holds, mostly.")));
    let ka = content(&r, &view("a"), &s.a);
    put(
        &mut r,
        "b",
        upd(&s.b, &format!("B builds on {{{{{}}}}}, firmly.", s.a)),
    );
    let k = key_of((&s.a, ka), (&s.b, content(&r, &view("b"), &s.b)));
    let q = with_key(&reqs(&r, &ca), k).expect("the cross pair").clone();
    let h = case_hash(&r.read().case_of(&q.key, &q.items).unwrap());
    land(&mut r, "b").unwrap();
    let qs = reqs(&r, &ca);
    let landed = with_key(&qs, k).expect("the same contents pair with canon B now");
    assert_eq!(landed.rule, Rule::Premise);
    assert!(
        landed
            .items
            .iter()
            .any(|i| i.block == s.b && matches!(i.side, SideRef::Canon { .. })),
        "{landed:#?}"
    );
    let e = r
        .submit_verdict(&reviewer(), verdict(&q, &h, "no_tension"))
        .unwrap_err();
    assert_eq!(
        (e.code, e.message),
        (
            E_REVIEW_STALE,
            format!(
                "the case of {} changed since you read it",
                request_label(&k)
            )
        ),
        "the case read before the landing is stale"
    );
    land(&mut r, "a").unwrap();
    assert!(reqs(&r, &ca).is_empty(), "the last in-flight side landed");
}

#[test]
fn nb5_everyday_and_deprecated_cross_view_sides_are_filtered() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "base", "title": "Base", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s",
             "fields": {"name": "Store", "everyday": true}, "body": "Store is a term."},
            {"op": "create", "type": "term", "id": "$u", "parent": "$s",
             "fields": {"name": "Ledger"}, "body": "Ledger is a term."}
        ]),
    )
    .unwrap();
    let (store, sec) = (created(&w, "$t"), created(&w, "$s"));
    land(&mut r, "seed").unwrap();
    let new = |r: &mut Rdy, v: &str, body: String, fields: Value| {
        let ops = json!([{"op": "create", "type": "claim", "id": "$x", "parent": sec.to_string(),
                          "body": body, "fields": fields}]);
        let w = put(r, v, ops);
        (created(&w, "$x"), change_of(&w))
    };
    // unlinked everyday mentions give no entity pair
    let (_, ca) = new(&mut r, "a", "The store keeps pins.".into(), json!({}));
    let (_, cb) = new(&mut r, "b", "Every store is fast.".into(), json!({}));
    for c in [ca, cb] {
        assert!(!reqs(&r, &c).iter().any(two_changes), "unlinked everyday");
    }
    // linked everyday mentions pair
    let (x, cc) = new(
        &mut r,
        "c",
        format!("The store keeps pins, see {{{{{store}}}}}."),
        json!({}),
    );
    let (y, cd) = new(
        &mut r,
        "d",
        format!("Every store is fast, per {{{{{store}}}}}."),
        json!({}),
    );
    let k = key_of(
        (&x, content(&r, &view("c"), &x)),
        (&y, content(&r, &view("d"), &y)),
    );
    for c in [cc, cd] {
        let qs = reqs(&r, &c);
        let q = with_key(&qs, k).unwrap_or_else(|| panic!("linked everyday pairs: {qs:#?}"));
        assert_eq!(q.rule, Rule::Entity);
    }
    // a deprecated side never enters, as subject or as other
    let (_, ce) = new(
        &mut r,
        "e",
        "The ledger is final.".into(),
        json!({"lifecycle": "deprecated"}),
    );
    let (w2, cf) = new(&mut r, "f", "A ledger grows.".into(), json!({}));
    assert!(
        !reqs(&r, &ce).iter().any(two_changes),
        "a deprecated subject"
    );
    assert!(
        !reqs(&r, &cf).iter().any(two_changes),
        "a deprecated other: {:#?}",
        reqs(&r, &cf)
    );
    let (v2, cg) = new(&mut r, "g", "The ledger is open.".into(), json!({}));
    let k = key_of(
        (&v2, content(&r, &view("g"), &v2)),
        (&w2, content(&r, &view("f"), &w2)),
    );
    assert!(
        with_key(&reqs(&r, &cg), k).is_some(),
        "a live ledger mention pairs"
    );
}

/// Canon: 40 groups, each a section and a term; change k (of 1,000, one block each, in view
/// `v<k mod 10>`) creates a claim in group k / 25 that describes its term.
fn thousand() -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = project();
    let letters = |j: usize| -> String {
        let a = b"abcdefghijklmnopqrstuvwxyz";
        format!("{}{}", a[j / 26] as char, a[j % 26] as char)
    };
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "pool", "title": "Pool", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$g", "parent": "$d", "fields": {"title": "Glossary"}}),
    ];
    for j in 0..40 {
        ops.push(
            json!({"op": "create", "type": "section", "id": format!("$s{j}"), "parent": "$d",
                        "fields": {"title": format!("Group {}", letters(j))}}),
        );
        ops.push(
            json!({"op": "create", "type": "term", "id": format!("$t{j}"), "parent": "$g",
                        "fields": {"name": format!("Widget{}", letters(j))},
                        "body": format!("Widget{} is a term.", letters(j))}),
        );
    }
    let w = write(&mut r, "seed", json!(ops)).unwrap();
    let groups: Vec<(BlockId, BlockId)> = (0..40)
        .map(|j| {
            (
                created(&w, &format!("$s{j}")),
                created(&w, &format!("$t{j}")),
            )
        })
        .collect();
    land(&mut r, "seed").unwrap();
    for k in 0..1000 {
        let (s, t) = &groups[k / 25];
        put(
            &mut r,
            &format!("v{}", k % 10),
            json!([{"op": "create", "type": "claim", "parent": s.to_string(),
                    "body": format!("Claim {k} holds."),
                    "links": {"describes": [{"target": t.to_string()}]}}]),
        );
    }
    (d, r)
}

/// The queue as standalone `requests_from` calls build it: the changes by (time, id), each
/// change's requests, each key once.
fn standalone_queue(st: &ReadState) -> (Vec<Request>, usize) {
    let set = st.tensions().unwrap();
    let mut changes: Vec<(i64, Hash)> = set.writes().changes().map(|f| (f.at_ms, f.id)).collect();
    changes.sort();
    let (mut out, mut seen, mut full) = (vec![], BTreeSet::new(), 0);
    for (_, c) in changes {
        let qs = st.requests_from(&c).unwrap();
        assert!(qs.len() <= MAX_CANDIDATES);
        if qs.len() == MAX_CANDIDATES {
            full += 1;
        }
        for q in qs {
            if seen.insert(q.key) {
                out.push(q);
            }
        }
    }
    (out, full)
}

#[test]
fn nb5_queue_walk_indexes_each_live_write_at_most_once() {
    let (_d, r) = thousand();
    let st = r.read();
    let live = st.tensions().unwrap().writes().live().count();
    assert_eq!(live, 1000);
    let (visits, builds) = (cross_view_index_visits(), request_builds());
    let walk = st.review_requests().unwrap();
    assert_eq!(
        cross_view_index_visits() - visits,
        1000,
        "one construction visit per live write in one walk"
    );
    assert_eq!(
        request_builds() - builds,
        1000,
        "one computation per change"
    );
    assert!(
        walk.iter().any(two_changes),
        "the walk holds cross-view pairs"
    );
    let (want, full) = standalone_queue(&st);
    assert!(full > 0, "some change reaches the cap of 20");
    assert_eq!(walk.len(), want.len());
    for (a, b) in walk.iter().zip(&want) {
        assert_eq!(
            a, b,
            "the walk's order, keys and items are the standalone calls'"
        );
    }
    drop(st);

    // a walk the same-view phase fills makes no construction visit
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    for v in ["a", "b"] {
        let ops: Vec<_> = (0..25)
            .map(|i| json!({"op": "create", "type": "claim", "parent": s.section.to_string(), "body": format!("Claim {v}{i} holds.")}))
            .collect();
        put(&mut r, v, json!(ops));
    }
    let st = r.read();
    let visits = cross_view_index_visits();
    let walk = st.review_requests().unwrap();
    assert_eq!(walk.len(), 2 * MAX_CANDIDATES);
    assert_eq!(cross_view_index_visits() - visits, 0);
}

fn subject(ids: &[&BlockId]) -> CheckSubject {
    CheckSubject {
        blocks: ids.iter().map(|b| (*b).clone()).collect(),
        change: None,
        version: None,
        code: None,
    }
}

/// The canon pair's items, and the contradiction case of A and B as view c holds them.
fn scope_local(st: &ReadState, s: &Spec) -> (Option<Vec<Item>>, Value, Hash) {
    let tracking = st.reader().tracking();
    let canon = st.materialized(&Scope::Canon).unwrap();
    let in_c = st.materialized(&view("c")).unwrap();
    let c = |id: &BlockId| canon.get(id).unwrap().content_hash_with(tracking);
    let items = st
        .pair_in_canon(&[(s.a.clone(), c(&s.a)), (s.b.clone(), c(&s.b))])
        .unwrap();
    let built = build_case(
        AgentCheck::Contradiction,
        &subject(&[&s.a, &s.b]),
        CaseInputs {
            st,
            before: &canon,
            after: &in_c,
            changes: &[],
        },
    )
    .unwrap();
    (items, built.case, built.hash)
}

#[test]
fn nb5_m3d_canon_case_stays_scope_local() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    // view c exists and touches neither A nor B
    put(
        &mut r,
        "c",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "c-notes", "title": "C notes", "doc_type": "page"}}]),
    );
    let before = scope_local(&r.read(), &s);
    assert!(before.0.is_some(), "canon holds the pair");
    assert_eq!(before.1["rule"], "premise", "{:#}", before.1);
    let e = Local::new(r);
    let c = ag("c");
    let run = rdy_engine::check::api::run_review(
        &e,
        &c,
        "human:qa",
        true,
        None,
        rdy_engine::check::api::ReviewRequest::from_value(json!({
            "op": "run", "check": "contradiction", "subject": [s.a.to_string(), s.b.to_string()]
        }))
        .unwrap(),
    )
    .unwrap();
    let (rq, h) = (
        run["request"].as_str().unwrap().to_string(),
        run["case_hash"].as_str().unwrap().to_string(),
    );
    assert_eq!(h, before.2.to_string(), "the run's case is view c's case");
    // an unrelated view writes B without its premise: neither check path moves
    let b = s.b.clone();
    write_typed(&e, move |r| {
        Ok(put(r, "x", upd(&b, "B stands on its own.")))
    })
    .unwrap();
    let after = scope_local(&e.read(), &s);
    assert_eq!(after.0, before.0, "pair_in_canon");
    assert_eq!(after.1, before.1, "the check case");
    assert_eq!(after.2, before.2, "the check case hash");
    let a = s.a.to_string();
    let v = run_review_submit(
        &e,
        &c,
        ReviewSubmitRequest {
            request: rq,
            case_hash: h,
            verdict: "no_tension".into(),
            citations: vec![CitationRequest {
                block: a,
                note: "these hold together".into(),
            }],
            severity: None,
            reason: None,
        },
    )
    .unwrap();
    assert_eq!(v["verdict"], "no_tension", "{v}");
}
