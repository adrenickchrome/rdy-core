//! sord1, scol1, stot1: search results (FW-SR1 text order, FW-SR3 totals and collapse). A literal
//! of several words finds the exact phrase first, then blocks that hold all the words; hits with
//! identical matching lines collapse into one entry that lists every block id; the result
//! carries totals per doc path and per block type.
use rdy_engine::search::{SearchFilters, SearchMatch, TOTALS_DOCS};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn q(text: &str, regex: bool, ignore_case: bool) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        regex,
        ignore_case,
        filters: SearchFilters::default(),
    }
}

fn search(r: &Rdy, query: &SearchQuery) -> SearchResult {
    r.read()
        .search(&Scope::Canon, query, None, Some(100_000))
        .unwrap()
}

fn ids(res: &SearchResult) -> Vec<String> {
    res.matches.iter().map(|m| m.id.clone()).collect()
}

/// Doc `den` holding, in this order: p2 "The fox is brown.", p1 "A brown fox runs.", p4
/// "fox\nbrown", p3 "A brown dog.", p5 "Another brown fox."; landed. Returns [p1, p2, p3, p4, p5].
fn foxes(r: &mut Rdy) -> Vec<String> {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "den", "title": "Den", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p2", "parent": "$d", "body": "The fox is brown."},
        {"op": "create", "type": "prose", "id": "$p1", "parent": "$d", "body": "A brown fox runs."},
        {"op": "create", "type": "prose", "id": "$p4", "parent": "$d", "body": "fox\nbrown"},
        {"op": "create", "type": "prose", "id": "$p3", "parent": "$d", "body": "A brown dog."},
        {"op": "create", "type": "prose", "id": "$p5", "parent": "$d", "body": "Another brown fox."}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    ["$p1", "$p2", "$p3", "$p4", "$p5"]
        .iter()
        .map(|k| w.created[*k].clone())
        .collect()
}

#[test]
fn sord1_the_exact_phrase_comes_before_all_the_words_then_reading_order() {
    let (_d, mut r) = project();
    let p = foxes(&mut r);
    let res = search(&r, &q("brown fox", false, false));
    assert_eq!(
        ids(&res),
        vec![p[0].clone(), p[4].clone(), p[1].clone(), p[3].clone()]
    );
    assert_eq!(res.total, 4);
    let words: Vec<bool> = res.matches.iter().map(|m| m.all_words).collect();
    assert_eq!(words, vec![false, false, true, true]);
    let lines = |m: &SearchMatch| {
        m.lines
            .iter()
            .map(|l| (l.line, l.text.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lines(&res.matches[2]),
        vec![(1, "The fox is brown.".to_string())]
    );
    assert_eq!(
        lines(&res.matches[3]),
        vec![(1, "fox".to_string()), (2, "brown".to_string())]
    );
    assert_eq!(
        serde_json::to_value(&res.matches[2]).unwrap()["all_words"],
        true
    );
    assert!(
        serde_json::to_value(&res.matches[0])
            .unwrap()
            .get("all_words")
            .is_none()
    );
    // case: the words follow ignore_case like the phrase
    assert_eq!(search(&r, &q("BROWN FOX", false, false)).total, 0);
    assert_eq!(ids(&search(&r, &q("BROWN FOX", false, true))), ids(&res));
    // a pattern has one strength: only the lines it matches
    assert_eq!(
        ids(&search(&r, &q("brown fox", true, false))),
        vec![p[0].clone(), p[4].clone()]
    );
    // one word: reading order
    assert_eq!(
        ids(&search(&r, &q("fox", false, false))),
        vec![p[1].clone(), p[0].clone(), p[3].clone(), p[4].clone()]
    );
}

/// Docs a (code and prose `port = 8080`), b (code `port = 8080`), c (code `port = 8080`, code
/// `port = 9090`); landed. Returns (a's code, a's prose, b's code, c's code).
fn ports(r: &mut Rdy) -> (String, String, String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
        {"op": "create", "type": "code", "id": "$ak", "parent": "$a", "body": "port = 8080", "fields": {"lang": "sh"}},
        {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "port = 8080"},
        {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
        {"op": "create", "type": "code", "id": "$bk", "parent": "$b", "body": "port = 8080", "fields": {"lang": "sh"}},
        {"op": "create", "type": "doc", "id": "$c", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
        {"op": "create", "type": "code", "id": "$ck", "parent": "$c", "body": "port = 8080", "fields": {"lang": "sh"}},
        {"op": "create", "type": "code", "parent": "$c", "body": "port = 9090", "fields": {"lang": "sh"}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    let c = |k: &str| w.created[k].clone();
    (c("$ak"), c("$ap"), c("$bk"), c("$ck"))
}

#[test]
fn scol1_identical_hits_collapse_into_one_entry_that_keeps_every_id() {
    let (_d, mut r) = project();
    let (ak, ap, bk, ck) = ports(&mut r);
    let query = q("port = 8080", false, false);
    let res = search(&r, &query);
    assert_eq!(res.total, 4, "total counts blocks");
    assert_eq!(
        ids(&res),
        vec![ak.clone(), ap.clone()],
        "a prose line is not a code line"
    );
    assert_eq!(
        serde_json::to_value(&res.matches[0].also).unwrap(),
        json!([{"id": bk, "path": "b"}, {"id": ck, "path": "c"}])
    );
    assert!(
        serde_json::to_value(&res.matches[1])
            .unwrap()
            .get("also")
            .is_none()
    );
    // one entry per page: every block still shows exactly once
    let mut seen = vec![];
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let p = r
            .read()
            .search(&Scope::Canon, &query, cursor.as_deref(), Some(1))
            .unwrap();
        assert_eq!(p.total, 4);
        for m in &p.matches {
            seen.push(m.id.clone());
            seen.extend(m.also.iter().map(|a| a.id.clone()));
        }
        cursor = p.next.clone();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, vec![ak, bk, ck, ap]);
}

#[test]
fn stot1_totals_count_hits_per_doc_path_and_per_type_on_every_page() {
    let (_d, mut r) = project();
    ports(&mut r);
    let query = q("port = 8080", false, false);
    let want = json!({
        "docs": [{"path": "a", "hits": 2}, {"path": "b", "hits": 1}, {"path": "c", "hits": 1}],
        "types": {"code": 3, "prose": 1}
    });
    assert_eq!(
        serde_json::to_value(&search(&r, &query).totals).unwrap(),
        want
    );
    let first = r
        .read()
        .search(&Scope::Canon, &query, None, Some(1))
        .unwrap();
    assert_eq!(
        first.matches.len(),
        1,
        "totals take the budget; the first entry still comes"
    );
    assert_eq!(serde_json::to_value(&first.totals).unwrap(), want);
    let second = r
        .read()
        .search(&Scope::Canon, &query, first.next.as_deref(), Some(1))
        .unwrap();
    assert_eq!(serde_json::to_value(&second.totals).unwrap(), want);
    let none = search(&r, &q("haystack", false, false));
    assert_eq!(
        serde_json::to_value(&none.totals).unwrap(),
        json!({"docs": [], "types": {}})
    );
}

#[test]
fn stot1_the_doc_list_is_capped_and_says_how_many_it_left_out() {
    let (_d, mut r) = project();
    let mut ops = vec![];
    for i in 0..60 {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$d{i}"), "fields": {"name": format!("d{i:02}"), "title": format!("D{i}"), "doc_type": "page"}}));
        ops.push(
            json!({"op": "create", "type": "prose", "parent": format!("$d{i}"), "body": "needle"}),
        );
    }
    write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    let res = search(&r, &q("needle", false, false));
    assert_eq!(res.total, 60);
    assert_eq!(res.matches.len(), 1, "sixty identical lines are one entry");
    assert_eq!(res.matches[0].also.len(), 59);
    assert_eq!(res.totals.docs.len(), TOTALS_DOCS);
    assert_eq!(res.totals.more_docs, 60 - TOTALS_DOCS);
    assert_eq!(res.totals.docs[0].path.as_deref(), Some("d00"));
    let last = format!("d{:02}", TOTALS_DOCS - 1);
    assert_eq!(
        res.totals.docs[TOTALS_DOCS - 1].path.as_deref(),
        Some(last.as_str())
    );
    assert_eq!(res.totals.types["prose"], 60);
}
