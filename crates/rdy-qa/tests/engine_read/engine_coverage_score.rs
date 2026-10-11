//! cc9: scores and rollups (FW-CH1, FW-CH7): integer arithmetic, dimensions without checks left
//! out, pack weights, the dismissed weight.
use rdy_engine::coverage::*;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn rdy_standard_pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

fn pack_with(section: &str) -> rdy_schema::Pack {
    let mut set = rdy_schema::PackSet::with_builtin();
    set.add_yaml(&format!(
        "name: p\nversion: \"1\"\nextends: rdy-standard\n{section}"
    ))
    .unwrap();
    set.resolve("p").unwrap()
}

/// One block's entry: `checks`, and one issue per (dimension, dismissed) pair.
fn entry(checks: [u32; 6], issues: Vec<(Dimension, bool)>) -> BlockEntry {
    let issues = issues
        .into_iter()
        .map(|(d, dismissed)| CoverageIssue {
            block: "c-AAAAA".into(),
            dimension: d,
            code: match (d, dismissed) {
                (Dimension::Grounded, _) => IssueCode::UntiedContent,
                (_, true) => IssueCode::DismissedTension,
                _ => IssueCode::OpenTension,
            },
            subject: None,
            fix: String::new(),
            dismissed: dismissed.then(|| Dismissal {
                review: "r".into(),
                actor: "agent:a".into(),
                reason: "x".into(),
            }),
        })
        .collect();
    BlockEntry {
        doc: None,
        checks,
        issues,
    }
}

/// Doc `ops` (overview: needs Summary and Goals; has Summary, stable, and Empty) holds term
/// Ledger, claim c1 (names Ledger twice, unlinked, no decision), claim c2 (describes Ledger,
/// decided_by d1), question q (unanswered), decision d1 (live, applied by c2), decision d2
/// (live, nothing applies it), decision d3 (live, needs_content: false). Doc `lone` holds one
/// prose block. Landed.
fn cc6_like_world(r: &mut Rdy) -> BTreeMap<String, String> {
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

#[test]
fn cc9_one_open_issue_in_one_of_two_blocks_halves_the_dimension() {
    let p = rdy_standard_pack();
    let a = entry([0, 1, 1, 0, 0, 0], vec![(Dimension::Consistent, false)]);
    let b = entry([0, 1, 1, 0, 0, 0], vec![]);
    let r = rollup(&p, [&a, &b].into_iter());
    assert_eq!(r.dimensions[&Dimension::Consistent], 50);
    assert_eq!(r.dimensions[&Dimension::Current], 100);
    assert_eq!(r.score, 75);
    assert!(!r.dimensions.contains_key(&Dimension::Grounded));
}

#[test]
fn cc9_a_dismissed_tension_costs_half_by_default() {
    let p = rdy_standard_pack();
    let a = entry([0, 1, 0, 0, 0, 0], vec![(Dimension::Consistent, true)]);
    assert_eq!(
        rollup(&p, [&a].into_iter()).dimensions[&Dimension::Consistent],
        50
    );
    let two = entry(
        [0, 1, 0, 0, 0, 0],
        vec![(Dimension::Consistent, true), (Dimension::Consistent, true)],
    );
    assert_eq!(
        rollup(&p, [&two].into_iter()).dimensions[&Dimension::Consistent],
        0,
        "capped at one check"
    );
}

#[test]
fn cc9_grounded_counts_each_check() {
    let p = rdy_standard_pack();
    let a = entry([3, 0, 0, 0, 0, 0], vec![(Dimension::Grounded, false)]);
    assert_eq!(
        rollup(&p, [&a].into_iter()).dimensions[&Dimension::Grounded],
        66
    );
}

#[test]
fn cc9_nothing_applicable_is_100_and_no_dimension() {
    let p = rdy_standard_pack();
    let r = rollup(&p, std::iter::empty());
    assert_eq!((r.score, r.dimensions.len()), (100, 0));
}

#[test]
fn cc9_a_zero_weight_leaves_the_dimension_out() {
    let p = pack_with("coverage:\n  weights: {consistent: 0}\n");
    let a = entry([0, 1, 1, 0, 0, 0], vec![(Dimension::Consistent, false)]);
    let r = rollup(&p, [&a].into_iter());
    assert_eq!(r.dimensions[&Dimension::Consistent], 0, "still reported");
    assert_eq!(r.score, 100, "but not weighed");
}

#[test]
fn cc9_the_report_lists_pages_lowest_first_and_areas() {
    // cc6's world: ops has issues, lone has an untied prose block
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = cc6_like_world(&mut r);
    let rep = r
        .read()
        .coverage_report(&rdy_engine::Scope::Canon, None)
        .unwrap();
    assert!(
        rep.pages
            .windows(2)
            .all(|w| (w[0].score, &w[0].path) <= (w[1].score, &w[1].path))
    );
    assert_eq!(rep.issues_total, rep.issues.len());
    assert_eq!(rep.scope, "canon");
    let again = r
        .read()
        .coverage_report(&rdy_engine::Scope::Canon, None)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&rep).unwrap(),
        serde_json::to_value(&again).unwrap(),
        "deterministic"
    );
}

#[test]
fn cc9_a_path_keeps_the_docs_under_it_and_an_unknown_path_is_not_found() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = cc6_like_world(&mut r);
    let st = r.read();
    let whole = st.coverage_report(&Scope::Canon, None).unwrap();
    let lone = st.coverage_report(&Scope::Canon, Some("lone")).unwrap();
    assert_eq!(lone.pages.len(), 1);
    assert_eq!(lone.pages[0].path, "lone");
    assert!(lone.issues_total < whole.issues_total);
    assert_eq!(
        lone.score, lone.pages[0].score,
        "one page: the page is the whole"
    );
    let e = st
        .coverage_report(&Scope::Canon, Some("nowhere"))
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND");
}

#[test]
fn cc9_pages_and_the_corpus_agree_on_the_same_entries() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = cc6_like_world(&mut r);
    let st = r.read();
    let pages = st.page_rollups(&Scope::Canon).unwrap();
    assert_eq!(pages.len(), 2, "ops and lone");
    let issues = st.coverage_issues(&Scope::Canon).unwrap();
    let all: BTreeSet<_> = issues.blocks.keys().cloned().collect();
    let corpus = st.coverage_rollup(&Scope::Canon, &all).unwrap();
    let rep = st.coverage_report(&Scope::Canon, None).unwrap();
    assert_eq!(
        (corpus.score, &corpus.dimensions),
        (rep.score, &rep.dimensions)
    );
    let none = st.coverage_rollup(&Scope::Canon, &BTreeSet::new()).unwrap();
    assert_eq!((none.score, none.dimensions.len()), (100, 0));
}

#[test]
fn cc13_doc_rollups_equal_the_page_rollups_of_those_docs() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = cc6_like_world(&mut r);
    let st = r.read();
    let all = st.page_rollups(&Scope::Canon).unwrap();
    let docs: BTreeSet<_> = all.keys().collect();
    assert_eq!(st.doc_rollups(&Scope::Canon, &docs).unwrap(), all);
    let one: BTreeSet<_> = all.keys().take(1).collect();
    let got = st.doc_rollups(&Scope::Canon, &one).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(
        got.get(*one.iter().next().unwrap()),
        all.get(*one.iter().next().unwrap())
    );
}

/// Final review M8 (task 2): the weighted sum is in u64, so a weight of u32::MAX neither
/// overflows nor skews the mean: Grounded at 50 and Consistent at 100, both weighted u32::MAX,
/// give 75.
#[test]
fn cc9_a_weight_of_u32_max_does_not_overflow() {
    let p = pack_with("coverage:\n  weights: {grounded: 4294967295, consistent: 4294967295}\n");
    let a = entry([2, 1, 0, 0, 0, 0], vec![(Dimension::Grounded, false)]);
    let r = rollup(&p, [&a].into_iter());
    assert_eq!(r.dimensions[&Dimension::Grounded], 50);
    assert_eq!(r.dimensions[&Dimension::Consistent], 100);
    assert_eq!(r.score, 75);
}

/// Final review M8 (task 9): the pack's `dismissed_weight` sets what a dismissed tension costs:
/// at 25 one dismissed tension on one check leaves 75.
#[test]
fn cc9_a_dismissed_weight_of_25_leaves_75() {
    let p = pack_with("coverage:\n  dismissed_weight: 25\n");
    let a = entry([0, 1, 0, 0, 0, 0], vec![(Dimension::Consistent, true)]);
    assert_eq!(
        rollup(&p, [&a].into_iter()).dimensions[&Dimension::Consistent],
        75
    );
}

/// Final review M8 (task 9, D-T5S9A): an unapplied decision's issue sits on the decision and
/// counts toward its home page and the corpus: d2 (live, nothing applies it) lowers the ops
/// page's Governed score and the corpus's below 100; the lone page has no Governed check.
#[test]
fn cc9_an_unapplied_decision_lowers_its_page_and_the_corpus() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let c = cc6_like_world(&mut r);
    let st = r.read();
    let rep = st.coverage_report(&Scope::Canon, None).unwrap();
    let unapplied: Vec<&CoverageIssue> = rep
        .issues
        .iter()
        .filter(|i| i.code == IssueCode::UnappliedDecision)
        .collect();
    assert_eq!(unapplied.len(), 1, "{unapplied:?}");
    assert_eq!(unapplied[0].block, c["$d2"], "on the decision itself");
    assert!(rep.dimensions[&Dimension::Governed] < 100, "{rep:?}");
    let pages = st.page_rollups(&Scope::Canon).unwrap();
    let ops = rdy_model::BlockId::parse(&c["$o"]).unwrap();
    let lone = rdy_model::BlockId::parse(&c["$l"]).unwrap();
    assert!(pages[&ops].dimensions[&Dimension::Governed] < 100);
    assert!(!pages[&lone].dimensions.contains_key(&Dimension::Governed));
}

/// Final review M8 (task 9): a report lists at most PAGES_CAP pages, lowest first, and counts
/// the rest in `pages_omitted`: 22 one-block pages give 20 and 2.
#[test]
fn cc9_the_report_cuts_pages_at_the_cap() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let mut ops = vec![];
    for k in 0..PAGES_CAP + 2 {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$d{k}"),
            "fields": {"name": format!("page-{k:02}"), "title": format!("Page {k}"), "doc_type": "page"}}));
        ops.push(
            json!({"op": "create", "type": "prose", "parent": format!("$d{k}"), "body": "Text."}),
        );
    }
    write(&mut r, "work", serde_json::Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    let rep = r.read().coverage_report(&Scope::Canon, None).unwrap();
    assert_eq!((rep.pages.len(), rep.pages_omitted), (PAGES_CAP, 2));
}

/// Final review M8 (task 9): areas and folder paths. In the QA pack, payments/fees holds an
/// untied claim and identity/login a claim that describes a term. The report scores each area
/// over the blocks in it, and a folder path keeps the docs under the folder: `payments` gives
/// the one page payments/fees, scored as that page.
#[test]
fn cc9_the_report_scores_areas_and_takes_a_folder_path() {
    let (_d, mut r) = rdy_qa::packs::qa_project();
    let work = Scope::View("work".into());
    let doc = |r: &mut Rdy, path: &str| -> String {
        r.new_doc(&writer(), "work", path, Some("page"), Some("T"))
            .unwrap();
        r.read().resolve_path(&work, path).unwrap().to_string()
    };
    let fees = doc(&mut r, "payments/fees");
    let login = doc(&mut r, "identity/login");
    write(&mut r, "work", json!([
        {"op": "create", "type": "section", "id": "$p", "parent": fees, "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "parent": "$p", "body": "Fees are charged monthly."},
        {"op": "create", "type": "section", "id": "$i", "parent": login, "fields": {"title": "S"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$i", "fields": {"name": "Session"}, "body": "A login."},
        {"op": "create", "type": "claim", "parent": "$i", "body": "Sessions expire.", "links": {"describes": [{"target": "$t"}]}}
    ]))
    .unwrap();
    let st = r.read();
    let rep = st.coverage_report(&work, None).unwrap();
    let issues = st.coverage_issues(&work).unwrap();
    let area_score = |area: &str| -> u32 {
        let ids: BTreeSet<rdy_model::BlockId> = issues
            .blocks
            .keys()
            .filter(|id| st.areas_of(&work, id).unwrap().contains(&area.to_string()))
            .cloned()
            .collect();
        st.coverage_rollup(&work, &ids).unwrap().score
    };
    let of = |area: &str| rep.areas.iter().find(|a| a.area == area).map(|a| a.score);
    assert_eq!(
        of("payments"),
        Some(area_score("payments")),
        "{:?}",
        rep.areas
    );
    assert_eq!(
        of("identity"),
        Some(area_score("identity")),
        "{:?}",
        rep.areas
    );
    assert_ne!(
        of("payments"),
        of("identity"),
        "the untied claim lowers payments"
    );
    let folder = st.coverage_report(&work, Some("payments")).unwrap();
    assert_eq!(folder.pages.len(), 1, "{:?}", folder.pages);
    assert_eq!(folder.pages[0].path, "payments/fees");
    assert_eq!(folder.score, folder.pages[0].score);
}
