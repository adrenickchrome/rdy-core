//! bsrch1: deterministic search over block text (FW-PR1): literal or regex, fields and bodies,
//! code included, by scope, in reading order, windowed and capped, paged with a stale check, and
//! bounded against hostile patterns.
use rdy_engine::search::{LINE_CHARS, LINES_PER_MATCH};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

// no filters
fn q(text: &str, regex: bool, ignore_case: bool) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        regex,
        ignore_case,
        ..Default::default()
    }
}

/// Doc `guide` with section Setup: a prose, a code and a flow block mentioning `rdy init`; landed.
/// Returns (section, prose, code, flow).
fn guide(r: &mut Rdy) -> (String, String, String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Setup"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Install the tool.\nRun rdy init to start.\nThen run rdy serve."},
        {"op": "create", "type": "code", "id": "$c", "parent": "$s", "body": "rdy init\nrdy serve --detach", "fields": {"lang": "sh"}},
        {"op": "create", "type": "flow", "id": "$f", "parent": "$s", "fields": {"name": "Start", "steps": ["Run rdy init", "Done"]}}
    ]))
    .unwrap();
    land(r, "work").unwrap();
    let c = |k: &str| w.created[k].clone();
    (c("$s"), c("$p"), c("$c"), c("$f"))
}

fn search(r: &Rdy, scope: &Scope, query: &SearchQuery) -> SearchResult {
    r.read().search(scope, query, None, Some(100_000)).unwrap()
}

#[test]
fn bsrch1_a_literal_finds_lines_in_bodies_fields_and_code_in_reading_order() {
    let (_d, mut r) = project();
    let (_s, p, c, f) = guide(&mut r);
    let res = search(&r, &Scope::Canon, &q("rdy init", false, false));
    assert_eq!(
        serde_json::to_value(&res).unwrap(),
        json!({
            "query": "rdy init", "regex": false, "ignore_case": false, "total": 3,
            "totals": {"docs": [{"path": "guide", "hits": 3}], "types": {"code": 1, "flow": 1, "prose": 1}},
            "matches": [
                {"id": p, "type": "prose", "path": "guide", "lines": [{"field": "body", "line": 2, "text": "Run rdy init to start."}], "coverage": 75, "issues": 1, "confidence": "low"},
                {"id": c, "type": "code", "path": "guide", "lines": [{"field": "body", "line": 1, "text": "rdy init"}], "coverage": 75, "issues": 1, "confidence": "low"},
                {"id": f, "type": "flow", "path": "guide", "lines": [{"field": "steps[0]", "line": 1, "text": "Run rdy init"}], "coverage": 75, "issues": 1, "confidence": "low"}
            ]
        })
    );
}

#[test]
fn bsrch1_case_and_regex_change_what_matches() {
    let (_d, mut r) = project();
    guide(&mut r);
    assert_eq!(
        search(&r, &Scope::Canon, &q("RDY INIT", false, false)).total,
        0
    );
    assert_eq!(
        search(&r, &Scope::Canon, &q("RDY INIT", false, true)).total,
        3
    );
    assert_eq!(
        search(&r, &Scope::Canon, &q("rdy.init", false, false)).total,
        0,
        "a literal dot is a dot"
    );
    assert_eq!(
        search(&r, &Scope::Canon, &q("rdy.init", true, false)).total,
        3
    );
    let both = search(&r, &Scope::Canon, &q("rdy (init|serve)", true, false));
    let lines: Vec<Vec<usize>> = both
        .matches
        .iter()
        .map(|m| m.lines.iter().map(|l| l.line).collect())
        .collect();
    assert_eq!(lines, vec![vec![2, 3], vec![1, 2], vec![1]]);
}

#[test]
fn bsrch1_the_scope_decides_what_is_searched() {
    let (_d, mut r) = project();
    let (s, ..) = guide(&mut r);
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "Run rdy init again."}]),
    )
    .unwrap();
    assert_eq!(
        search(&r, &Scope::Canon, &q("rdy init", false, false)).total,
        3
    );
    assert_eq!(
        search(
            &r,
            &Scope::View("work".into()),
            &q("rdy init", false, false)
        )
        .total,
        4
    );
    land(&mut r, "work").unwrap();
    let tag = r.read().resolve_read_scope("tag:v1.0.0", None).unwrap();
    assert_eq!(search(&r, &tag, &q("rdy init", false, false)).total, 3);
    assert_eq!(
        search(&r, &Scope::Canon, &q("rdy init", false, false)).total,
        4
    );
}

#[test]
fn bsrch1_lines_are_capped_per_block_and_long_lines_are_windowed() {
    let (_d, mut r) = project();
    let many: Vec<String> = (0..15).map(|i| format!("needle {i}")).collect();
    let long = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hay", "title": "Hay", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "id": "$m", "parent": "$d", "body": many.join("\n")},
        {"op": "create", "type": "prose", "id": "$l", "parent": "$d", "body": long}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let res = search(&r, &Scope::Canon, &q("needle", false, false));
    assert_eq!(res.total, 2);
    assert_eq!(res.matches[0].lines.len(), LINES_PER_MATCH);
    assert_eq!(res.matches[0].more_lines, 15 - LINES_PER_MATCH);
    let text = &res.matches[1].lines[0].text;
    assert_eq!(
        text,
        &format!("…{}needle{}…", "a".repeat(80), "b".repeat(LINE_CHARS - 86))
    );
    assert_eq!(text.chars().count(), LINE_CHARS + 2);
}

/// Doc `many` holding 60 claims `needle <i>`, landed.
fn needles(r: &mut Rdy) -> String {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
    ]))
    .unwrap();
    let s = w.created["$s"].clone();
    let ops: Vec<Value> = (0..60)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s, "body": format!("needle {i}")}))
        .collect();
    write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
    s
}

#[test]
fn bsrch1_paging_through_shows_every_match_once() {
    let (_d, mut r) = project();
    needles(&mut r);
    let query = q("needle", false, false);
    let full = search(&r, &Scope::Canon, &query);
    assert_eq!(
        (full.total, full.truncated, full.next.clone()),
        (60, false, None)
    );
    let first = r
        .read()
        .search(&Scope::Canon, &query, None, Some(300))
        .unwrap();
    assert!(first.truncated && first.next.is_some());
    assert_eq!(first.omitted, 60 - first.matches.len());
    let mut ids: Vec<String> = first.matches.iter().map(|m| m.id.clone()).collect();
    let mut next = first.next.clone();
    let mut pages = 1;
    while let Some(cursor) = next {
        let p = r
            .read()
            .search(&Scope::Canon, &query, Some(&cursor), Some(300))
            .unwrap();
        assert_eq!(p.total, 60);
        assert_eq!(p.omitted, 60 - ids.len() - p.matches.len());
        ids.extend(p.matches.iter().map(|m| m.id.clone()));
        next = p.next.clone();
        pages += 1;
        assert!(pages < 100, "paging does not end");
    }
    assert!(pages > 1);
    let all: Vec<String> = full.matches.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, all);
}

#[test]
fn bsrch1_a_page_over_moved_matches_is_stale_and_a_bad_cursor_is_bad_input() {
    let (_d, mut r) = project();
    let s = needles(&mut r);
    let query = q("needle", false, false);
    let first = r
        .read()
        .search(&Scope::Canon, &query, None, Some(300))
        .unwrap();
    let cursor = first.next.clone().unwrap();
    let token = cursor.split(':').next().unwrap().to_string();
    let page = |r: &Rdy, query: &SearchQuery, cursor: &str| {
        r.read()
            .search(&Scope::Canon, query, Some(cursor), Some(300))
            .unwrap_err()
            .code
    };
    // malformed cursors, and an offset past the matches, are bad input
    for bad in [
        "nonsense",
        "abc:1",
        &format!("{token}:x"),
        &format!("{token}:999"),
    ] {
        assert_eq!(page(&r, &query, bad), E_BAD_INPUT, "{bad}");
    }
    // a well-formed cursor whose matches are not these is stale: another token, another query
    assert_eq!(
        page(&r, &query, &format!("{}:0", "0".repeat(64))),
        E_PAGE_STALE
    );
    assert_eq!(
        page(&r, &q("needle 1", false, false), &cursor),
        E_PAGE_STALE
    );
    // a new matching block moves the matches
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s, "body": "needle new"}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    assert_eq!(page(&r, &query, &cursor), E_PAGE_STALE);
}

#[test]
fn bsrch1_patterns_are_bounded() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "bounds", "title": "Bounds", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$d", "body": format!("{}!", "a".repeat(5000))}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let run = |text: &str, regex: bool| {
        r.read()
            .search(&Scope::Canon, &q(text, regex, false), None, None)
    };
    let e = run(&"a".repeat(1025), false).unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e}");
    assert_eq!(e.message, "query is 1025 bytes; the limit is 1024");
    let e = run("(?:\\w{200}){200}", true).unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e}");
    let e = run("(", true).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.starts_with("query is not a valid pattern: ")
            && e.message.contains("unclosed group"),
        "{e}"
    );
    let deep = format!("{}a{}", "(".repeat(100), ")".repeat(100));
    assert_eq!(run(&deep, true).unwrap_err().code, E_BAD_INPUT);
    let e = run("", false).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "query is empty")
    );
    // guard: the regex engine is linear, so the classic backtracking bomb just returns
    assert_eq!(run("(a+)+$", true).unwrap().total, 0);
    // a pattern that matches the empty string reports only real matches
    assert_eq!(run("q*", true).unwrap().total, 0);
    assert_eq!(run("a*!", true).unwrap().total, 1);
}

/// Fix round 1: a pattern that matches empty text makes each line's walk restart after every empty
/// match, and a restart can scan to the line's end. A long line must not cost its length squared:
/// the search stops with `E_LIMIT`. Pinned by outcome, not by time.
#[test]
fn bsrch1_a_pattern_that_matches_empty_text_on_a_long_line_is_bounded() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "long", "title": "Long", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$d", "body": "x".repeat(200_000)}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let run = |text: &str, regex: bool| {
        r.read()
            .search(&Scope::Canon, &q(text, regex, false), None, None)
    };
    // the charge is the worst case, so even q* (cheap per restart) is refused on a line over
    // about 8 KiB: a deterministic bound, recorded in PERF.md (final review 3)
    for pattern in ["(?:x[^z]*z)?", "(?:.*TODO)?", "q*"] {
        let e = run(pattern, true).unwrap_err();
        assert_eq!(e.code, E_LIMIT, "{pattern}: {e}");
        assert_eq!(
            (e.message.as_str(), e.fix.as_str()),
            (
                "the pattern can match empty text, and searching past empty matches on a line over about 8 KiB can cost too much: rdy refuses it in this scope",
                "make the pattern match at least one character, e.g. x+ rather than x*"
            ),
            "{pattern}"
        );
    }
    // a pattern that matches empty text once per line, and plain text, still search this line
    assert_eq!(run("^", true).unwrap().total, 0);
    assert_eq!(run("x", false).unwrap().total, 1);
}

/// Fix round 1: short patterns with large counted repeats over about 1 MB of paragraph-length
/// lines return a correct result (the lazy DFA has room for their states). Pinned by outcome.
#[test]
fn bsrch1_counted_repeats_over_paragraph_lines_return_a_result() {
    let (_d, mut r) = project();
    let sentence =
        "the quick brown fox jumps over the lazy dog and then some more words follow here ";
    let line: String = sentence.chars().cycle().take(968).collect();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "prose", "title": "Prose", "doc_type": "overview"}}
    ]))
    .unwrap();
    let d = w.created["$d"].clone();
    let ops: Vec<Value> = (0..1000)
        .map(|_| json!({"op": "create", "type": "prose", "parent": d, "body": line}))
        .collect();
    write(&mut r, "work", Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    let run = |text: &str| {
        r.read()
            .search(&Scope::Canon, &q(text, true, false), None, None)
            .unwrap()
            .total
    };
    assert_eq!(run("fox"), 1000);
    assert_eq!(run("(?:[a-z ]{0,20}){1,40}\\d"), 0);
    assert_eq!(run("(?:[a-z ]?){1,1000}\\d"), 0);
}

/// Merge of AE2 into M2d1: search reads the `inflight` scope (canon plus every open view's
/// changes), as every read does.
#[test]
fn bsrch1_search_reads_the_inflight_scope() {
    let (_d, mut r) = project();
    let (_s, p, _c, _f) = guide(&mut r);
    let v = version_of(&r, &Scope::Canon, &p);
    write(
        &mut r,
        "a",
        json!([{"op": "update", "id": p, "base": v, "body": "Run rdy deploy to ship."}]),
    )
    .unwrap();
    let query = q("rdy deploy", false, false);
    assert_eq!(search(&r, &Scope::Canon, &query).total, 0);
    let res = search(&r, &Scope::Inflight, &query);
    assert_eq!(res.total, 1);
    assert_eq!(
        serde_json::to_value(&res).unwrap()["matches"][0]["id"],
        json!(p)
    );
}
