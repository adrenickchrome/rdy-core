//! cc14: search hits and fact sheets carry coverage and confidence (FW-SR1, FW-SR4, FW-CV2);
//! `coverage_below` keeps low-scoring hits and `sort: coverage` lists the lowest first.
use rdy_engine::search::SearchFilters;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Doc `ops` (overview) holds term Ledger, claim c1 (names Ledger twice, unlinked, no decision),
/// claim c2 (describes Ledger, decided_by d1), decisions d1, d2 and d3, question q. Doc `lone`
/// holds one prose block. Landed. The ids by placeholder.
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
    land(r, "work").unwrap();
    w.created
}

fn query(text: &str, filters: Value) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        filters: serde_json::from_value::<SearchFilters>(filters).unwrap(),
        ..Default::default()
    }
}

fn search(r: &Rdy, text: &str, filters: Value) -> Value {
    let res = r
        .read()
        .search(&Scope::Canon, &query(text, filters), None, Some(4000))
        .unwrap();
    serde_json::to_value(res).unwrap()
}

fn search_err(r: &Rdy, text: &str, filters: Value) -> RdyError {
    r.read()
        .search(&Scope::Canon, &query(text, filters), None, Some(4000))
        .unwrap_err()
}

#[test]
fn cc14_hits_carry_coverage_issues_and_confidence() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let c = world(&mut r);
    let res = search(&r, "append-only", json!({}));
    let hit = &res["matches"][0];
    assert_eq!(hit["id"], c["$c1"]);
    assert!(hit["coverage"].as_u64().unwrap() < 100);
    assert_eq!(hit["issues"], 3);
    assert_eq!(hit["confidence"], "low", "text with no entity behind it");
}

#[test]
fn cc14_a_fact_sheet_carries_confidence_and_reasons() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = world(&mut r);
    let res = search(&r, "Ledger", json!({}));
    assert_eq!(res["entities"][0]["confidence"], "medium");
    assert_eq!(res["entities"][0]["reasons"][0]["code"], "untied_content");
}

#[test]
fn cc14_coverage_below_and_sort_coverage() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = world(&mut r);
    let all = search(&r, "e", json!({}));
    let low = search(&r, "e", json!({"coverage_below": 60}));
    assert!(
        low["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["coverage"].as_u64().unwrap() < 60)
    );
    let sorted = search(&r, "e", json!({"sort": "coverage"}));
    let scores: Vec<u64> = sorted["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["coverage"].as_u64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(sorted["total"], all["total"], "a sort keeps every hit");
    assert!(
        search_err(&r, "e", json!({"sort": "score"}))
            .message
            .contains("sort takes coverage")
    );
    assert!(
        search_err(&r, "e", json!({"coverage_below": 101}))
            .message
            .contains("0 to 100")
    );
}

#[test]
fn cc14_a_sort_moves_the_lowest_first_and_a_bound_of_zero_keeps_nothing() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = world(&mut r);
    let plain = search(&r, "e", json!({}));
    let sorted = search(&r, "e", json!({"sort": "coverage"}));
    let ids = |v: &Value| -> Vec<String> {
        v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap().to_string())
            .collect()
    };
    let scores = |v: &Value| -> Vec<u64> {
        v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["coverage"].as_u64().unwrap())
            .collect()
    };
    let (lo, hi) = (scores(&sorted)[0], *scores(&sorted).last().unwrap());
    assert!(lo < hi, "the world has hits of different scores");
    let mut want = ids(&plain);
    want.sort_by_key(|id| {
        plain["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["id"] == *id)
            .map(|h| h["coverage"].as_u64().unwrap())
    });
    assert_eq!(
        ids(&sorted),
        want,
        "a stable sort keeps the default order between ties"
    );
    assert_eq!(search(&r, "e", json!({"coverage_below": 0}))["total"], 0);
}

#[test]
fn cc14_a_past_landing_carries_no_coverage_and_refuses_the_filter() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let c = world(&mut r);
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": c["$empty"], "body": "Later text."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let tag = r.read().resolve_read_scope("tag:v1.0.0", None).unwrap();
    let res = r
        .read()
        .search(&tag, &query("append-only", json!({})), None, Some(4000))
        .unwrap();
    let v = serde_json::to_value(res).unwrap();
    assert_eq!(v["total"], 1);
    assert!(v["matches"][0].get("coverage").is_none());
    let e = r
        .read()
        .search(
            &tag,
            &query("append-only", json!({"coverage_below": 50})),
            None,
            Some(4000),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
}

#[test]
fn cc14_the_cli_and_mcp_take_coverage_below_and_sort() {
    let s = rdy_qa::daemon::Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        world(&mut r);
        r.close().unwrap();
    }
    let (ok, v) = s.json(&["--json", "search", "e", "--sort", "coverage"]);
    assert!(ok, "{v}");
    let scores: Vec<u64> = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["coverage"].as_u64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|w| w[0] <= w[1]), "{scores:?}");
    let (ok, v) = s.json(&["--json", "search", "e", "--coverage-below", "0"]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 0);
    let (ok, v) = s.json(&["--json", "search", "e", "--sort", "score"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    let out = s.rdy(&["search", "Ledger"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("  confidence medium"), "{text}");
    assert!(text.contains("  coverage "), "{text}");
    s.stop();

    let (dir, mut r) = rdy_qa::fixtures::project();
    world(&mut r);
    drop(r);
    let mut c = rdy_qa::mcp::McpClient::start(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:m", "run": "m1", "role": "writer"})
        )
        .code()
        .is_none()
    );
    let v = c
        .call(
            "rdy_search",
            json!({"query": "e", "scope": "canon", "sort": "coverage", "coverage_below": 101}),
        )
        .code()
        .map(str::to_string);
    assert_eq!(v.as_deref(), Some("E_BAD_INPUT"));
    let v = c
        .call(
            "rdy_search",
            json!({"query": "append-only", "scope": "canon", "coverage_below": 100}),
        )
        .ok();
    assert_eq!(v["total"], 1, "{v}");
    c.alive();
}

/// The entry scores of a search that collapses nothing, in result order.
fn entry_scores(v: &Value) -> Vec<(String, u64)> {
    v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            assert!(h.get("also").is_none(), "this query collapses nothing: {h}");
            (
                h["id"].as_str().unwrap().to_string(),
                h["coverage"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn cc14_coverage_below_is_strictly_below_and_keeps_some_hits() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let _c = world(&mut r);
    let all = entry_scores(&search(&r, "e", json!({})));
    let mut distinct: Vec<u64> = all.iter().map(|(_, s)| *s).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(distinct.len() >= 2, "hits of different scores: {all:?}");
    // a bound at each score but the lowest: a hit that scores the bound drops out, a lower one stays
    for &n in &distinct[1..] {
        let want: Vec<&(String, u64)> = all.iter().filter(|(_, s)| *s < n).collect();
        let got = search(&r, "e", json!({"coverage_below": n}));
        assert!(
            !want.is_empty() && want.len() < all.len(),
            "the bound {n} splits the hits"
        );
        assert_eq!(got["total"], want.len(), "bound {n}");
        let got = entry_scores(&got);
        assert!(got.iter().all(|(_, s)| *s < n), "bound {n}: {got:?}");
        assert_eq!(
            got.iter().collect::<Vec<_>>(),
            want,
            "bound {n} keeps the default order"
        );
    }
    // the bound at the lowest score keeps nothing; the bound 100 keeps every hit under 100
    assert_eq!(
        search(&r, "e", json!({"coverage_below": distinct[0]}))["total"],
        0
    );
    assert_eq!(
        search(&r, "e", json!({"coverage_below": 100}))["total"],
        all.iter().filter(|(_, s)| *s < 100).count()
    );
}

/// A term, and `n` claims that cite it, each with its own text. Landed. The term's id.
fn citing_world(r: &mut Rdy, n: usize) -> String {
    citing_world_of(r, n).0
}

/// As `citing_world`, with a second term `Purse` that every claim cites too. The two term ids.
fn citing_world_of(r: &mut Rdy, n: usize) -> (String, String) {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$o", "fields": {"name": "pay", "title": "Pay", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Wallet"}, "body": "Holds cards."}),
        json!({"op": "create", "type": "term", "id": "$u", "parent": "$s", "fields": {"name": "Purse"}, "body": "Holds coins."}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$s",
            "body": format!("Payment uses {{{{$t}}}} in flow number {i} with {{{{$u}}}}, which is unlike the rest.")}));
    }
    let w = write(r, "pay", json!(ops)).unwrap();
    land(r, "pay").unwrap();
    (w.created["$t"].clone(), w.created["$u"].clone())
}

fn first_page_next(r: &Rdy, filters: Value) -> String {
    let res = r
        .read()
        .search(
            &Scope::Canon,
            &query("Payment uses", filters),
            None,
            Some(300),
        )
        .unwrap();
    res.next.expect("a second page")
}

fn next_page(r: &Rdy, filters: Value, next: &str) -> Result<SearchResult, RdyError> {
    r.read().search(
        &Scope::Canon,
        &query("Payment uses", filters),
        Some(next),
        Some(300),
    )
}

#[test]
fn cc14_a_plain_search_pages_on_after_a_write_that_moves_coverage_but_no_hit() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let t = citing_world(&mut r, 12);
    let next = first_page_next(&r, json!({}));
    let before = search(&r, "Payment uses", json!({}));
    // an edit of the cited term opens a suspect on every citing claim: their coverage moves, no
    // hit changes, and the matches and their order stay
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": t, "body": "Holds cards and vouchers."}]),
    )
    .unwrap();
    land(&mut r, "edit").unwrap();
    let after = search(&r, "Payment uses", json!({}));
    assert_ne!(
        before["matches"][0]["confidence"], after["matches"][0]["confidence"],
        "the write did move the hits' coverage"
    );
    let page = next_page(&r, json!({}), &next).expect("the cursor still holds");
    assert!(!page.matches.is_empty());
}

#[test]
fn cc14_a_sorted_or_bounded_search_goes_stale_when_a_write_moves_a_hit_score() {
    for filters in [json!({"sort": "coverage"}), json!({"coverage_below": 100})] {
        let (_d, mut r) = rdy_qa::fixtures::project();
        let (t, u) = citing_world_of(&mut r, 12);
        // the first edit lowers every claim's score; the cursor comes from after it
        let edit = |r: &mut Rdy, id: &str, body: &str, name: &str| {
            write(r, name, json!([{"op": "update", "id": id, "body": body}])).unwrap();
            land(r, name).unwrap();
        };
        edit(&mut r, &t, "Holds cards and vouchers.", "e1");
        let next = first_page_next(&r, filters.clone());
        // the second edit opens one more suspect on every claim: no hit changes
        edit(&mut r, &u, "Holds coins and notes.", "e2");
        let e = next_page(&r, filters.clone(), &next).unwrap_err();
        assert_eq!(e.code, E_PAGE_STALE, "{filters}");
    }
}

#[test]
fn cc14_a_plain_search_page_carries_coverage_on_each_entry_and_the_lowest_level_of_a_group() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let t = citing_world(&mut r, 12);
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": t, "body": "Holds cards and vouchers."}]),
    )
    .unwrap();
    land(&mut r, "edit").unwrap();
    let plain = search(&r, "Payment uses", json!({}));
    let sorted = search(&r, "Payment uses", json!({"sort": "coverage"}));
    for h in plain["matches"].as_array().unwrap() {
        assert!(h["coverage"].is_u64() && h["issues"].is_u64(), "{h}");
        assert_eq!(h["confidence"], "low", "an open suspect: {h}");
        let same = sorted["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == h["id"]);
        if let Some(s) = same {
            assert_eq!(s["coverage"], h["coverage"]);
            assert_eq!(s["issues"], h["issues"]);
        }
    }
}

/// Final review M8 (task 14.post): a plain search collapses blocks with identical matching lines
/// into one entry, which carries its first block's coverage and the lowest level of all its
/// blocks. Claims a and b say "Fees apply." (too short for a duplicate); a describes the term
/// Fee (no issue, high on its own), b ties to nothing (untied text as a hit: low). The entry is
/// a's, lists b under also, has a's issue count (0) and is low.
#[test]
fn cc14_a_collapsed_entry_takes_the_lowest_level_of_its_blocks() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let w = write(&mut r, "fees", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "fees", "title": "Fees", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Rules"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Fee"}, "body": "A charge."},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Fees apply.",
         "links": {"describes": [{"target": "$t"}]}},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Fees apply."}
    ]))
    .unwrap()
    .created;
    land(&mut r, "fees").unwrap();
    let res = search(&r, "Fees apply", json!({}));
    let entries = res["matches"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "one collapsed entry: {res}");
    let e = &entries[0];
    assert_eq!(e["id"], w["$a"], "{e}");
    assert!(
        e["also"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == w["$b"]),
        "{e}"
    );
    assert_eq!(e["issues"], 0, "a's own issues: {e}");
    assert_eq!(e["confidence"], "low", "b's level: {e}");
}
