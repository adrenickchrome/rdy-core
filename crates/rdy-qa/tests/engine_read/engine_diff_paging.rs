//! dq2: the budget and the cursor of the diff request (O11): what a cut drops and in what order,
//! that pages join to the whole diff, and that the wrapper of a landing read counts.
use rdy_engine::diff::{DiffChange, DiffCounts, DiffEntry};
use rdy_engine::{
    DiffReply, DiffRequest, E_BAD_INPUT, E_LIMIT, E_PAGE_STALE, PAGE_STALE_FIX, Rdy, RdyError,
    testkit,
};
use rdy_model::Hash;
use rdy_qa::fixtures::*;
use serde_json::json;

const LARGE: usize = 100_000;

fn one(from: &str, detail: bool) -> DiffRequest {
    DiffRequest {
        from: from.into(),
        detail,
        ..Default::default()
    }
}

fn len<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_vec(v).unwrap().len()
}

fn ids(reply: &DiffReply) -> Vec<String> {
    reply.entries.iter().map(|e| e.id.to_string()).collect()
}

/// A landed doc of `n` sections, then a view `edit` that edits each section's title and body: `n`
/// updated entries, each with a body and a field value.
fn edited(n: usize) -> (tempfile::TempDir, Rdy) {
    let (d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "big", "title": "Big", "doc_type": "overview"}}),
    ];
    for i in 0..n {
        ops.push(
            json!({"op": "create", "type": "section", "id": format!("$s{i}"), "parent": "$d",
            "body": format!("Old text of section number {i} before anything changed it."),
            "fields": {"title": format!("Old title {i}")}}),
        );
    }
    let w = write(&mut r, "work", json!(ops)).unwrap();
    land(&mut r, "work").unwrap();
    let edits: Vec<_> = (0..n)
        .map(|i| {
            json!({"op": "update", "id": w.created[&format!("$s{i}")],
                "body": format!("New words of section number {i} once the edit changed it."),
                "fields": {"title": format!("New title {i}")}})
        })
        .collect();
    write(&mut r, "edit", json!(edits)).unwrap();
    (d, r)
}

fn whole(r: &Rdy, req: &DiffRequest) -> DiffReply {
    r.read().diff_reply(req, None).unwrap()
}

fn page(
    r: &Rdy,
    req: &DiffRequest,
    cursor: Option<&str>,
    max: usize,
) -> Result<DiffReply, RdyError> {
    r.read().diff_page(req, None, cursor, max)
}

/// F1 of O11: the reply without entries, marked cut, with `omitted` at the whole counts and a
/// `next` whose offset has as many digits as the entry count.
fn f1(reply: &DiffReply) -> usize {
    let token = Hash::of(b"any").to_string();
    let fixed = DiffReply {
        entries: vec![],
        truncated: true,
        omitted: Some(reply.counts.clone()),
        next: Some(format!("{token}:{}", reply.entries.len())),
        ..reply.clone()
    };
    len(&fixed)
}

/// The bytes of `entries` as the reply joins them: one comma between two.
fn joined(entries: &[DiffEntry]) -> usize {
    entries.iter().map(len).sum::<usize>() + entries.len().saturating_sub(1)
}

fn no_body(e: &DiffEntry) -> DiffEntry {
    DiffEntry {
        body: None,
        ..e.clone()
    }
}

fn no_values(e: &DiffEntry) -> DiffEntry {
    let mut e = no_body(e);
    for f in &mut e.fields {
        f.from = None;
        f.to = None;
    }
    e
}

fn has_values(e: &DiffEntry) -> bool {
    e.fields.iter().any(|f| f.from.is_some() || f.to.is_some())
}

#[test]
fn dq2_a_diff_under_budget_is_whole() {
    let (_d, r) = edited(3);
    let got = page(&r, &one("edit", true), None, LARGE).unwrap();
    assert_eq!(got, whole(&r, &one("edit", true)));
    assert!(!got.truncated);
    assert_eq!(got.omitted, None);
    assert_eq!(got.next, None);
    let v = serde_json::to_value(&got).unwrap();
    for key in ["truncated", "omitted", "next"] {
        assert!(v.get(key).is_none(), "{key}: {v}");
    }
}

#[test]
fn dq2_a_cut_drops_bodies_first_then_values_then_entries() {
    let (_d, r) = edited(4);
    let req = one("edit", true);
    let all = whole(&r, &req);
    assert_eq!(all.entries.len(), 4);
    let fixed = f1(&all);
    let last = all.entries.len() - 1;

    // step 1: only the last entry loses its body
    let mut less = all.entries.clone();
    less[last] = no_body(&less[last]);
    let got = page(&r, &req, None, (fixed + joined(&less)) / 4).unwrap();
    assert_eq!(got.entries.len(), 4);
    assert!(got.truncated);
    assert_eq!(got.next, None);
    assert_eq!(got.entries[last].body, None);
    assert!(got.entries[0].body.is_some());
    assert!(got.entries[last - 1].body.is_some());
    assert!(got.entries.iter().all(has_values));
    assert_eq!(got.omitted, Some(DiffCounts::default()));

    // step 2: every body is gone, and only the last entry loses its values
    let mut less: Vec<DiffEntry> = all.entries.iter().map(no_body).collect();
    less[last] = no_values(&less[last]);
    let got = page(&r, &req, None, (fixed + joined(&less)) / 4).unwrap();
    assert_eq!(got.entries.len(), 4);
    assert!(got.truncated);
    assert_eq!(got.next, None);
    assert!(got.entries.iter().all(|e| e.body.is_none()));
    assert!(!has_values(&got.entries[last]));
    assert!(has_values(&got.entries[0]));
    assert!(has_values(&got.entries[last - 1]));

    // step 3: entries leave from the end
    let bare: Vec<DiffEntry> = all.entries.iter().map(no_values).collect();
    let got = page(&r, &req, None, (fixed + joined(&bare[..2])) / 4).unwrap();
    assert_eq!(got.entries, bare[..2].to_vec());
    assert!(got.truncated);
    assert!(got.next.as_deref().is_some_and(|n| n.ends_with(":2")));
}

#[test]
fn dq2_counts_are_never_cut_and_omitted_counts_what_left() {
    let (_d, r) = edited(5);
    let req = one("edit", false);
    let all = whole(&r, &req);
    let fixed = f1(&all);
    let max = (fixed + joined(&all.entries[..2])) / 4;
    let first = page(&r, &req, None, max).unwrap();
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.counts, all.counts);
    assert_eq!(first.counts.updated, 5);
    assert_eq!(
        first.omitted,
        Some(DiffCounts {
            updated: 3,
            ..Default::default()
        })
    );
    let second = page(&r, &req, first.next.as_deref(), max).unwrap();
    assert_eq!(second.counts, all.counts);
    assert_eq!(
        second.omitted,
        Some(DiffCounts {
            updated: 1,
            ..Default::default()
        })
    );
}

#[test]
fn dq2_pages_join_to_the_whole_diff() {
    let (_d, r) = edited(12);
    for detail in [false, true] {
        let req = one("edit", detail);
        let all = whole(&r, &req);
        let max = (f1(&all) + 3 * 500) / 4;
        let mut got = vec![];
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let p = page(&r, &req, cursor.as_deref(), max).unwrap();
            assert!(!p.entries.is_empty());
            got.extend(ids(&p));
            pages += 1;
            assert!(pages <= all.entries.len(), "the pages loop");
            match p.next {
                Some(n) => cursor = Some(n),
                None => break,
            }
        }
        assert!(pages > 1);
        assert_eq!(got, ids(&all), "detail {detail}");
    }
}

#[test]
fn dq2_a_page_after_a_write_is_stale() {
    let (_d, mut r) = edited(6);
    let req = one("edit", false);
    let all = whole(&r, &req);
    let max = (f1(&all) + joined(&all.entries[..2])) / 4;
    let first = page(&r, &req, None, max).unwrap();
    let next = first.next.unwrap();

    write(
        &mut r,
        "edit",
        json!([
            {"op": "update", "id": all.entries[0].id.to_string(), "body": "One more edit."}
        ]),
    )
    .unwrap();
    let e = page(&r, &req, Some(&next), max).unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE);
    assert_eq!(
        e.message,
        "the diff behind this page changed since the first page, or the request differs"
    );
    assert_eq!(e.fix, PAGE_STALE_FIX);

    // a cursor of another request is stale too: the token holds `detail`
    let e = page(
        &r,
        &one("edit", true),
        first_cursor(&r, &req, max).as_deref(),
        LARGE,
    )
    .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE);
}

fn first_cursor(r: &Rdy, req: &DiffRequest, max: usize) -> Option<String> {
    page(r, req, None, max).unwrap().next
}

#[test]
fn dq2_a_bad_cursor_is_refused() {
    let (_d, r) = edited(4);
    let req = one("edit", false);
    let all = whole(&r, &req);
    let max = (f1(&all) + joined(&all.entries[..2])) / 4;
    let next = first_cursor(&r, &req, max).unwrap();
    let token = next.split_once(':').unwrap().0;
    for bad in [
        "word".to_string(),
        format!("{}:1", &token[..63]),
        format!("{token}:4"),
        format!("{token}:999999999"),
    ] {
        let e = page(&r, &req, Some(&bad), LARGE).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{bad}");
        assert_eq!(
            e.message, "page is not a cursor from rdy_diff's next",
            "{bad}"
        );
        assert_eq!(
            e.fix, "pass next from the previous page, unchanged",
            "{bad}"
        );
    }
    // the last offset that holds an entry is good
    assert_eq!(
        page(&r, &req, Some(&format!("{token}:3")), LARGE)
            .unwrap()
            .entries
            .len(),
        1
    );
}

#[test]
fn dq2_a_budget_that_holds_no_entry_is_refused() {
    let (_d, r) = edited(2);
    let req = one("edit", false);
    let e = page(&r, &req, None, f1(&whole(&r, &req)) / 4).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "the token budget cannot hold one diff entry");
    assert_eq!(e.fix, "increase max_tokens, or narrow the diff with doc");
}

#[test]
fn dq2_a_page_fits_its_budget() {
    let (_d, r) = edited(10);
    for detail in [false, true] {
        let req = one("edit", detail);
        let all = whole(&r, &req);
        let floor = (f1(&all) + 400) / 4;
        for max in (floor..floor + 800).step_by(37) {
            let mut cursor: Option<String> = None;
            loop {
                let p = page(&r, &req, cursor.as_deref(), max).unwrap();
                assert!(len(&p) / 4 <= max, "{} over {max}", len(&p) / 4);
                match p.next {
                    Some(n) => cursor = Some(n),
                    None => break,
                }
            }
        }
    }
}

#[test]
fn dq2_a_page_at_the_limit_passes() {
    let d = tempfile::tempdir().unwrap();
    testkit::generate_tree(d.path(), 2, 10, 97);
    let mut r = Rdy::open(d.path()).unwrap();
    land(&mut r, "work").unwrap();
    let got = page(&r, &one("canon", false), None, LARGE).unwrap();
    assert!(!got.entries.is_empty());
    assert!(len(&got) / 4 <= LARGE);
    assert!(got.truncated);
    assert!(got.next.is_some());
    assert_eq!(got.counts.added, whole_len(&r));
}

fn whole_len(r: &Rdy) -> usize {
    r.read()
        .materialized(&rdy_engine::Scope::Canon)
        .unwrap()
        .len()
}

/// Three landings, the last of 12 new claims; returns the last landing.
fn landed() -> (tempfile::TempDir, Rdy, Hash) {
    let (d, mut r) = project();
    let (_, s, _) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let ops: Vec<_> = (0..12)
        .map(|i| json!({"op": "create", "type": "claim", "parent": s, "body": format!("Claim number {i}.")}))
        .collect();
    write(&mut r, "many", json!(ops)).unwrap();
    let l = land(&mut r, "many").unwrap().parse().unwrap();
    (d, r, l)
}

#[test]
fn dq2_a_wrapped_reply_fits_its_budget_exactly() {
    let (_d, r, l) = landed();
    let read = r.read();
    let full = read.landing_show(&l, None, None).unwrap();
    let row = serde_json::to_value(&full.landing).unwrap();
    let max = len(&full) / 4;

    let fits = read.landing_show(&l, None, Some(max)).unwrap();
    assert!(!fits.diff.truncated);
    assert_eq!(fits.diff.next, None);
    assert_eq!(fits.diff.entries, full.diff.entries);

    let cut = read.landing_show(&l, None, Some(max - 1)).unwrap();
    assert!(cut.diff.truncated);
    assert!(cut.diff.next.is_some());
    assert!(cut.diff.entries.len() < full.diff.entries.len());
    assert!(!cut.diff.entries.is_empty());
    assert_eq!(serde_json::to_value(&cut.landing).unwrap(), row);
    assert!(len(&cut) / 4 < max);
}

#[test]
fn dq2_a_row_larger_than_the_budget_is_refused() {
    let (_d, r, l) = landed();
    let read = r.read();
    let row = len(&read.log_row(&l).unwrap());
    let e = read.landing_show(&l, None, Some(row / 4 - 1)).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "the token budget cannot hold the reply metadata");
    assert_eq!(e.fix, "increase max_tokens");
}

#[test]
fn dq2_an_empty_diff_over_a_tiny_budget_is_refused() {
    let (_d, r) = edited(1);
    let req = DiffRequest {
        from: "canon".into(),
        to: Some("canon".into()),
        ..Default::default()
    };
    assert!(whole(&r, &req).entries.is_empty());
    let e = page(&r, &req, None, 1).unwrap_err();
    assert_eq!(e.code, E_LIMIT);
    assert_eq!(e.message, "the token budget cannot hold the reply metadata");
    assert_eq!(e.fix, "increase max_tokens");
}

#[test]
fn dq2_landing_show_pages_name_rdy_log() {
    let (_d, r, l) = landed();
    let read = r.read();
    let e = read
        .landing_show(&l, Some("word"), Some(LARGE))
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, "page is not a cursor from rdy_log's next");
    assert_eq!(e.fix, "pass next from the previous page, unchanged");

    // the pages of a landing read join to its change list
    let full = read.landing_show(&l, None, None).unwrap();
    let max = (len(&full.landing) + f1(&full.diff) + 700) / 4;
    let (mut got, mut cursor) = (vec![], None::<String>);
    loop {
        let p = read.landing_show(&l, cursor.as_deref(), Some(max)).unwrap();
        assert!(len(&p) / 4 <= max);
        got.extend(ids(&p.diff));
        match p.diff.next {
            Some(n) => cursor = Some(n),
            None => break,
        }
    }
    assert_eq!(got, ids(&full.diff));
    assert!(
        full.diff
            .entries
            .iter()
            .all(|e| e.change == DiffChange::Added)
    );
}

/// O1, TASK-931 X2: one line for PERF.md, as information. It asserts nothing about time.
#[test]
#[ignore = "builds about 90,000 blocks; the orchestrator runs it on the bench"]
fn dq2_info_a_canon_against_landing_diff_at_100k_blocks() {
    let d = tempfile::tempdir().unwrap();
    // 90 folders, not 100: a first landing near 100,000 blocks stores no assessment (KNOWN-ISSUES)
    testkit::generate_tree(d.path(), 90, 10, 97);
    let mut r = Rdy::open(d.path()).unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, None).unwrap();

    let read = r.read();
    let canon = read.materialized(&rdy_engine::Scope::Canon).unwrap();
    let edits: Vec<_> = canon
        .iter()
        .filter(|b| b.block_type == "claim")
        .take(100)
        .map(|b| json!({"op": "update", "id": b.id.to_string(), "body": format!("Edited {}.", b.id)}))
        .collect();
    let blocks = canon.len();
    drop((read, canon));
    write(&mut r, "edit", json!(edits)).unwrap();
    land(&mut r, "edit").unwrap();

    let read = r.read();
    read.resolve_read_scope("tag:v0.1.0", None).unwrap();
    let req = DiffRequest {
        from: "tag:v0.1.0".into(),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let got = read.diff_reply(&req, None).unwrap();
    let ms = start.elapsed().as_millis();
    println!(
        "dq2: canon against landing at {blocks} blocks, {} entries: {ms} ms",
        got.entries.len()
    );
    assert_eq!(got.entries.len(), 100);
}
