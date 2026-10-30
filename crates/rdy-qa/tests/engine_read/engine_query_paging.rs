//! dq5: `ReadState::query_page`: a page that fits its budget, keeps whole matches and continues
//! through `next`, and the refusals of a stale or bad cursor and of a budget too small.
use rdy_engine::filter::{QueryMatch, QueryReply};
use rdy_engine::{
    E_BAD_INPUT, E_LIMIT, E_PAGE_STALE, PAGE_STALE_FIX, Rdy, RdyError, Scope, WriteRequest,
};
use rdy_model::{EditorialClass, Justification};
use rdy_qa::fixtures::*;
use serde_json::json;

const PROSE: &str = "type = prose";

fn work() -> Scope {
    Scope::View("work".into())
}

fn page(
    r: &Rdy,
    filter: &str,
    cursor: Option<&str>,
    max_tokens: usize,
) -> Result<QueryReply, RdyError> {
    r.read()
        .query_page(&work(), "work", filter, cursor, max_tokens)
}

/// A doc with `n` prose blocks, in `work`.
fn proses(n: usize) -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "doc", "title": "Doc", "doc_type": "page"}}),
    ];
    ops.extend((0..n).map(|i| {
        json!({"op": "create", "type": "prose", "parent": "$d", "body": format!("Prose number {i}.")})
    }));
    let w = write(&mut r, "work", json!(ops)).unwrap();
    let doc = w.created["$d"].clone();
    (d, r, doc)
}

fn tokens(reply: &QueryReply) -> usize {
    serde_json::to_string(reply).unwrap().len() / 4
}

fn ids(matches: &[QueryMatch]) -> Vec<String> {
    matches.iter().map(|m| m.id.clone()).collect()
}

/// The bytes of a reply with no matches and a `next` as long as the longest for `total`.
fn metadata_tokens(total: usize) -> usize {
    let next = format!("{}:{total}", "0".repeat(64));
    tokens(&QueryReply {
        matches: vec![],
        total,
        next: Some(next),
    })
}

#[test]
fn dq5_a_query_cut_keeps_whole_matches_and_continues() {
    let (_d, r, _) = proses(40);
    let whole = r.read().query(&work(), PROSE).unwrap();
    assert_eq!(whole.total, 40);
    assert_eq!(whole.next, None);
    let (max_tokens, mut joined, mut cursor, mut pages) = (200, vec![], None::<String>, 0);
    loop {
        let reply = page(&r, PROSE, cursor.as_deref(), max_tokens).unwrap();
        assert!(tokens(&reply) <= max_tokens, "{}", tokens(&reply));
        assert_eq!(reply.total, 40, "the total is the whole query's");
        assert!(!reply.matches.is_empty());
        for m in &reply.matches {
            assert_eq!(
                whole.matches.iter().find(|w| w.id == m.id),
                Some(m),
                "a match is never cut"
            );
        }
        joined.extend(ids(&reply.matches));
        pages += 1;
        let Some(next) = reply.next else { break };
        assert_eq!(next.split_once(':').unwrap().0.len(), 64);
        assert_eq!(
            next.split_once(':').unwrap().1,
            joined.len().to_string(),
            "the offset is the next match's"
        );
        cursor = Some(next);
    }
    assert!(pages > 1, "the budget cuts the query");
    assert_eq!(joined, ids(&whole.matches));
}

#[test]
fn dq5_a_query_page_is_whole_exactly_when_it_fits() {
    let (_d, r, _) = proses(5);
    let whole = r.read().query(&work(), PROSE).unwrap();
    let exact = tokens(&whole);
    assert_eq!(page(&r, PROSE, None, exact).unwrap(), whole);
    let cut = page(&r, PROSE, None, exact - 1);
    if let Ok(cut) = &cut {
        assert!(cut.next.is_some());
        assert!(tokens(cut) < exact);
    }
}

#[test]
fn dq5_a_query_page_after_the_match_set_changes_is_stale() {
    let (_d, mut r, doc) = proses(10);
    let first = page(&r, PROSE, None, 60).unwrap();
    let next = first.next.clone().unwrap();
    let stale = |e: RdyError| {
        assert_eq!(e.code, E_PAGE_STALE);
        assert_eq!(
            e.message,
            "the matches behind this page changed since the first page, or the query differs"
        );
        assert_eq!(e.fix, PAGE_STALE_FIX);
    };
    // another filter with the same matches, and another scope text, are other queries
    stale(page(&r, "TYPE = prose", Some(&next), 60).unwrap_err());
    stale(
        r.read()
            .query_page(&work(), "view:work", PROSE, Some(&next), 60)
            .unwrap_err(),
    );
    // an edit that leaves the set alone does not stale the page
    let some_id = first.matches[0].id.clone();
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: parse_test_ops(json!([{"op": "update", "id": some_id, "body": "Edited."}]))
                .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: "an edit".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    page(&r, PROSE, Some(&next), 60).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "parent": doc, "body": "One more."}]),
    )
    .unwrap();
    stale(page(&r, PROSE, Some(&next), 60).unwrap_err());
}

#[test]
fn dq5_a_query_budget_that_holds_no_match_is_refused() {
    let (_d, r, _) = proses(10);
    let e = page(&r, PROSE, None, metadata_tokens(10)).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "the token budget cannot hold one match");
    assert_eq!(e.fix, "increase max_tokens");
}

#[test]
fn dq5_a_query_budget_under_the_metadata_is_refused() {
    let (_d, r, _) = proses(10);
    let e = page(&r, PROSE, None, metadata_tokens(10) - 1).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "the token budget cannot hold the reply metadata");
    assert_eq!(e.fix, "increase max_tokens");
}

#[test]
fn dq5_an_empty_query_over_a_tiny_budget_is_refused() {
    let (_d, r, _) = proses(1);
    let e = page(&r, "type = claim", None, 1).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "the token budget cannot hold the reply metadata");
    assert_eq!(e.fix, "increase max_tokens");
    // the empty reply fits with room for it
    assert_eq!(
        page(&r, "type = claim", None, 6).unwrap(),
        QueryReply {
            matches: vec![],
            total: 0,
            next: None
        }
    );
}

#[test]
fn dq5_a_query_bad_cursor_names_rdy_query() {
    let (_d, r, _) = proses(10);
    let next = page(&r, PROSE, None, 60).unwrap().next.unwrap();
    let token = next.split_once(':').unwrap().0;
    let past = format!("{token}:10");
    for bad in ["word", past.as_str()] {
        let e = page(&r, PROSE, Some(bad), 60).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{bad}");
        assert_eq!(e.message, "page is not a cursor from rdy_query's next");
        assert_eq!(e.fix, "pass next from the previous page, unchanged");
    }
}
