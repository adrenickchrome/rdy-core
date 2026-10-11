//! cc10: confidence (FW-CV2): the level follows the closed rule; reasons are coverage issues.
use rdy_engine::coverage::*;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn ids(v: &[&str]) -> BTreeSet<BlockId> {
    v.iter().map(|i| BlockId::parse(i).unwrap()).collect()
}

/// Doc `ops` (overview: needs Summary and Goals; has Summary, stable, and Empty) holds term
/// Ledger, claim c1 (names Ledger twice, unlinked, no decision), claim c2 (describes Ledger,
/// decided_by d1), question q (unanswered), decision d1 (live, applied by c2), decision d2
/// (live, nothing applies it), decision d3 (live, needs_content: false). Doc `lone` holds one
/// prose block. Landed.
fn world(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$empty", "parent": "$o", "fields": {"title": "Empty"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Balances live in one store", "status": "proposed"}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "Balances are kept in one place.",
         "links": {"describes": [{"target": "$t"}], "decided_by": [{"target": "$d1"}]}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Refunds need two approvers", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d3", "parent": "$s", "fields": {"statement": "Reviews happen on Fridays", "status": "proposed", "needs_content": false}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$l", "body": "Nothing here."}
    ]))
    .unwrap();
    land(r, "work").unwrap(); // the landing accepts d1, d2 and d3: they are live
    w.created
}

fn id(text: &str) -> BlockId {
    BlockId::parse(text).unwrap()
}

/// Two landed claims with one body in a draft section: a duplicate definition that persists in
/// canon (FW-CH6a), advisory because the content is draft. The two claims and the tension's
/// label.
fn canon_duplicate_pair(r: &mut Rdy) -> (BlockId, BlockId, String) {
    let w = write(r, "dup", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "fees", "title": "Fees", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules", "lifecycle": "draft"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Fees are charged every month."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Fees are charged every month."}
    ]))
    .unwrap();
    land(r, "dup").unwrap();
    let (a, b) = (id(&w.created["$a"]), id(&w.created["$b"]));
    let st = r.read();
    let set = st.tensions().unwrap();
    let t = set
        .iter()
        .find(|t| {
            t.kind == tensions::TensionKind::DuplicateDefinition
                && t.blocks().contains(&a)
                && t.blocks().contains(&b)
        })
        .expect("a canon duplicate of the two claims");
    assert!(set.from_canon(&t.id), "every side is canon");
    (a, b, tensions::tension_label(&t.id))
}

/// A landed claim that cites a target at a pin, then a landed edit of the target. The claim and
/// the target.
fn landed_citation_then_target_changed(r: &mut Rdy) -> (BlockId, BlockId) {
    let w = write(r, "cite", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "pay", "title": "Pay", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Wallet"}, "body": "Holds cards."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Payment uses {{$t}}."}
    ]))
    .unwrap();
    land(r, "cite").unwrap();
    let (c, t) = (id(&w.created["$c"]), id(&w.created["$t"]));
    write(
        r,
        "edit",
        json!([{"op": "update", "id": t.to_string(), "body": "Holds cards and vouchers."}]),
    )
    .unwrap();
    land(r, "edit").unwrap();
    (c, t)
}

/// A copy of `i` whose blocks keep only the issues with one of `codes`.
fn keep_codes(i: &CoverageIssues, codes: &[IssueCode]) -> CoverageIssues {
    let mut out = i.clone();
    let keys: Vec<BlockId> = out.blocks.keys().cloned().collect();
    for k in keys {
        if let Some(e) = out.blocks.get_mut(&k) {
            e.issues.retain(|x| codes.contains(&x.code));
        }
    }
    out
}

/// One block `c-AAAAA` holding `n` unlinked-mention issues about `t-0000{k}`.
fn synthetic(n: usize) -> CoverageIssues {
    let block = BlockId::parse("c-AAAAA").unwrap();
    let issues = (0..n)
        .map(|k| CoverageIssue {
            block: block.to_string(),
            dimension: Dimension::Grounded,
            code: IssueCode::UnlinkedMention,
            subject: Some(format!("t-0000{k}")),
            fix: "link it".to_string(),
            dismissed: None,
        })
        .collect();
    let entry = BlockEntry {
        doc: None,
        checks: [n as u32, 0, 0, 0, 0, 0],
        issues,
    };
    // an empty project gives an empty `CoverageIssues` to put the block into
    let (_d, r) = project();
    let mut out = (*r.read().coverage_issues(&Scope::Canon).unwrap()).clone();
    out.blocks.insert(block, entry);
    out
}

/// The doc path of each block with an issue, as a caller builds it for the briefing.
fn doc_paths(st: &ReadState, i: &CoverageIssues) -> BTreeMap<String, String> {
    let m = st.materialized(&Scope::Canon).unwrap();
    i.blocks
        .iter()
        .filter(|(_, e)| !e.issues.is_empty())
        .filter_map(|(id, e)| Some((id.to_string(), m.path_of(e.doc.as_ref()?)?)))
        .collect()
}

#[test]
fn cc10_the_level_follows_the_closed_rule() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let st = r.read();
    let i = st.coverage_issues(&Scope::Canon).unwrap();
    let paths = doc_paths(&st, &i);
    // d1: no issue
    assert_eq!(
        confidence_of(&i, &ids(&[&c["$d1"]]), None, &paths).confidence,
        Level::High
    );
    // c1: completeness issues only
    let k = confidence_of(&i, &ids(&[&c["$c1"]]), None, &paths);
    assert_eq!(k.confidence, Level::Medium);
    assert_eq!(k.reasons[0].code, IssueCode::UntiedContent);
    // the same block as a search hit: text with no entity behind it
    assert_eq!(
        confidence_of(
            &i,
            &ids(&[&c["$c1"]]),
            Some(&BlockId::parse(&c["$c1"]).unwrap()),
            &paths
        )
        .confidence,
        Level::Low
    );
}

#[test]
fn cc10_an_open_suspect_anywhere_in_the_neighbourhood_is_low_and_leads_the_reasons() {
    let (_d, mut r) = project();
    let (citer, target) = landed_citation_then_target_changed(&mut r);
    let st = r.read();
    let i = st.coverage_issues(&Scope::Canon).unwrap();
    let paths = doc_paths(&st, &i);
    let k = confidence_of(
        &i,
        &[citer.clone(), target].into_iter().collect(),
        None,
        &paths,
    );
    assert_eq!(k.confidence, Level::Low);
    assert_eq!(k.reasons[0].code, IssueCode::OpenSuspect);
}

#[test]
fn cc10_a_dismissed_tension_alone_is_medium() {
    let (_d, mut r) = project();
    let (a, _b, label) = canon_duplicate_pair(&mut r);
    r.dismiss_tension(&writer(), &label, "on purpose").unwrap();
    let st = r.read();
    let i = st.coverage_issues(&Scope::Canon).unwrap();
    let paths = doc_paths(&st, &i);
    let only_tension: CoverageIssues = keep_codes(&i, &[IssueCode::DismissedTension]);
    assert_eq!(
        confidence_of(&only_tension, &[a].into_iter().collect(), None, &paths).confidence,
        Level::Medium
    );
}

#[test]
fn cc10_reasons_are_capped_at_five() {
    let i = synthetic(7); // one block with seven unlinked-mention issues
    let k = confidence_of(&i, &ids(&["c-AAAAA"]), None, &BTreeMap::new());
    assert_eq!((k.reasons.len(), k.reasons_omitted), (5, 2));
}

#[test]
fn cc10_a_fact_sheet_neighbourhood_holds_describers_and_mentioners() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let n = r
        .read()
        .fact_sheet_neighbourhood(&Scope::Canon, &BlockId::parse(&c["$t"]).unwrap())
        .unwrap();
    assert_eq!(n, ids(&[&c["$t"], &c["$c1"], &c["$c2"]]));
}

#[test]
fn cc10_a_hit_neighbourhood_holds_the_block_and_the_entities_it_ties_to() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let st = r.read();
    let n = st
        .hit_neighbourhood(&Scope::Canon, &BlockId::parse(&c["$c2"]).unwrap())
        .unwrap();
    assert_eq!(n, ids(&[&c["$c2"], &c["$t"]]));
    // c1 names Ledger without a tie: no entity behind it
    let n = st
        .hit_neighbourhood(&Scope::Canon, &BlockId::parse(&c["$c1"]).unwrap())
        .unwrap();
    assert_eq!(n, ids(&[&c["$c1"]]));
}

#[test]
fn cc10_reasons_follow_the_closed_order_across_docs() {
    // two blocks whose block-id order is the reverse of their docs' path order
    let (a, b) = (id("c-AAAAA"), id("c-BBBBB"));
    let mut i = synthetic(3); // c-AAAAA holds three unlinked mentions
    let mut e = i.blocks.get(&a).unwrap().clone();
    e.issues.iter_mut().for_each(|x| x.block = b.to_string());
    i.blocks.insert(b.clone(), e); // c-BBBBB holds three as well
    let paths: BTreeMap<String, String> = [
        (a.to_string(), "z/last.md".to_string()),
        (b.to_string(), "a/first.md".to_string()),
    ]
    .into_iter()
    .collect();
    let k = confidence_of(
        &i,
        &[a.clone(), b.clone()].into_iter().collect(),
        None,
        &paths,
    );
    let blocks: Vec<&str> = k.reasons.iter().map(|x| x.block.as_str()).collect();
    // the path order puts all of c-BBBBB first; the cap of five drops one of c-AAAAA
    assert_eq!(
        blocks,
        ["c-BBBBB", "c-BBBBB", "c-BBBBB", "c-AAAAA", "c-AAAAA"]
    );
    assert_eq!(k.reasons_omitted, 1);
}

#[test]
fn cc14_the_level_alone_is_the_level_of_the_full_confidence() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let (citer, target) = landed_citation_then_target_changed(&mut r);
    let st = r.read();
    let i = st.coverage_issues(&Scope::Canon).unwrap();
    let paths = doc_paths(&st, &i);
    let b = |k: &str| BlockId::parse(&c[k]).unwrap();
    let cases: Vec<(BTreeSet<BlockId>, Option<BlockId>)> = vec![
        (ids(&[&c["$c1"]]), Some(b("$c1"))),
        (ids(&[&c["$c1"]]), None),
        (ids(&[&c["$c2"], &c["$t"]]), Some(b("$c2"))),
        (ids(&[&c["$t"]]), Some(b("$c1"))),
        (ids(&[&c["$d2"]]), None),
        (ids(&[&c["$d3"]]), Some(b("$d3"))),
        ([citer.clone()].into_iter().collect(), Some(citer.clone())),
        ([target].into_iter().collect(), None),
        (BTreeSet::new(), Some(citer)),
        (BTreeSet::new(), None),
    ];
    for (hood, hit) in cases {
        assert_eq!(
            confidence_level_of(&i, &hood, hit.as_ref()),
            confidence_of(&i, &hood, hit.as_ref(), &paths).confidence,
            "{hood:?} {hit:?}"
        );
    }
}

/// Final review M2 (the model's "linked or not"): a fact sheet's neighbourhood holds every
/// scored block whose text names the entity, also one that cannot link to it: a section titled
/// after it and an accepted (frozen) decision whose statement names it without `affects`. An
/// open tension on that decision then lowers the fact sheet's confidence to Low.
#[test]
fn cc10_a_fact_sheet_neighbourhood_holds_mentioners_that_cannot_link() {
    let (_d, mut r) = project();
    let c = world(&mut r);
    let w = write(&mut r, "more", json!([
        {"op": "create", "type": "section", "id": "$ls", "parent": c["$o"], "fields": {"title": "Ledger rules"}},
        {"op": "create", "type": "decision", "id": "$d4", "parent": "$ls", "fields": {"statement": "The Ledger is audited every year", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d5", "parent": "$ls", "fields": {"statement": "The Ledger is audited every year", "status": "proposed"}}
    ]))
    .unwrap()
    .created;
    land(&mut r, "more").unwrap();
    let st = r.read();
    let t = id(&c["$t"]);
    let n = st.fact_sheet_neighbourhood(&Scope::Canon, &t).unwrap();
    for k in ["$ls", "$d4", "$d5"] {
        assert!(n.contains(&id(&w[k])), "{k} names the Ledger: {n:?}");
    }
    let i = st.coverage_issues(&Scope::Canon).unwrap();
    let d4 = &i.blocks[&id(&w["$d4"])];
    assert_eq!(
        d4.checks[0], 0,
        "a frozen decision cannot link: no Grounded check"
    );
    assert!(
        d4.issues.iter().any(|x| x.code == IssueCode::OpenTension),
        "guard: the two decisions duplicate each other: {:?}",
        d4.issues
    );
    let paths = doc_paths(&st, &i);
    assert_eq!(
        confidence_of(&i, &n, None, &paths).confidence,
        Level::Low,
        "the open tension on a mentioner lowers the fact sheet"
    );
}

/// Final review M8 (task 10): an open (not dismissed) canon tension in the neighbourhood is Low,
/// and the open tension leads the reasons.
#[test]
fn cc10_an_open_canon_tension_is_low() {
    let (_d, mut r) = project();
    let (a, _b, _label) = canon_duplicate_pair(&mut r);
    let st = r.read();
    let i = st.coverage_issues(&Scope::Canon).unwrap();
    let paths = doc_paths(&st, &i);
    let k = confidence_of(&i, &[a].into_iter().collect(), None, &paths);
    assert_eq!(k.confidence, Level::Low);
    assert_eq!(k.reasons[0].code, IssueCode::OpenTension);
}

/// Final review M8 (task 10): the JSON shape: the level in lowercase, the reasons, and
/// `reasons_omitted` only when the cap cut some.
#[test]
fn cc10_the_confidence_json_shape() {
    let cut = serde_json::to_value(confidence_of(
        &synthetic(7),
        &ids(&["c-AAAAA"]),
        None,
        &BTreeMap::new(),
    ))
    .unwrap();
    assert_eq!(cut["confidence"], "medium", "{cut}");
    assert_eq!(cut["reasons"].as_array().unwrap().len(), 5);
    assert_eq!(cut["reasons_omitted"], 2);
    let whole = serde_json::to_value(confidence_of(
        &synthetic(3),
        &ids(&["c-AAAAA"]),
        None,
        &BTreeMap::new(),
    ))
    .unwrap();
    assert!(whole.get("reasons_omitted").is_none(), "{whole}");
    let high = serde_json::to_value(confidence_of(
        &synthetic(0),
        &ids(&["c-AAAAA"]),
        None,
        &BTreeMap::new(),
    ))
    .unwrap();
    assert_eq!(high["confidence"], "high", "{high}");
}
