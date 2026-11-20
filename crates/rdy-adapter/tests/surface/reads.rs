//! rpl38: the adapter's read and search replies are the engine builders' replies, less `next`
//! and the transport `reminder`; inputs that need a session are refused (TASK-1613).

use rdy_adapter::{ReadRequest, SearchFilters, SearchRequest, StoreHandle};
use rdy_engine::search::SearchQuery;
use rdy_engine::tool_reads;
use rdy_engine::{Rdy, ReadState, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};
use std::sync::Arc;

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    doc: String,
    proposal: String,
}

/// A landed and tagged project with a doc of many claims, and the engine state to compare with.
/// `expect` runs on the engine state before the project closes, so the store is free for the
/// handle; its result is what the adapter must return.
fn fixture<T>(expect: impl FnOnce(&Arc<ReadState>, &str, &str) -> T) -> (Fixture, T) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Terms"}}),
        json!({"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}),
    ];
    for i in 0..40 {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$s", "body": format!("Claim {i} says ledger text. {}", "x".repeat(200))}));
    }
    let w = r
        .write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(Value::Array(ops)).unwrap(),
                justification: None,
                message: "rpl38".into(),
            },
        )
        .unwrap();
    let doc = w.created["$doc"].clone();
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    let proposal = p.r#ref.clone();
    r.land(&session(Role::Approver), &p.id).unwrap();
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let expected = {
        let state = r.read();
        expect(&state, &doc, &proposal)
    };
    r.close().unwrap();
    (Fixture { dir, doc, proposal }, expected)
}

fn without_next(mut v: Value) -> Value {
    v.as_object_mut().unwrap().remove("next");
    v
}

fn snapshot(f: &Fixture) -> rdy_adapter::Snapshot {
    StoreHandle::open(f.dir.path()).unwrap().snapshot().unwrap()
}

const TOKEN: &str = "abababababababababababababababababababababababababababababababab";

type ReadCase = (
    Option<&'static str>,
    &'static str,
    Option<&'static str>,
    Option<usize>,
);

const READ_CASES: [ReadCase; 6] = [
    (None, "compact", None, Some(100_000)),
    (Some("canon"), "markdown", None, Some(100_000)),
    (Some("tag:1.0.0"), "yaml", None, Some(100_000)),
    (None, "compact", Some("ledger"), Some(100_000)),
    // a cut read: truncated and omitted, no next
    (None, "compact", None, Some(500)),
    (None, "markdown", None, Some(300)),
];

#[test]
fn rpl38_adapter_read_matches_the_engine_reply() {
    let (f, (wants, want_proposal)) = fixture(|state, doc, proposal| {
        let wants: Vec<Value> = READ_CASES
            .iter()
            .map(|(scope, format, pattern, max)| {
                let entries = tool_reads::read_entries(
                    state,
                    doc,
                    "canon",
                    &scope.map(String::from),
                    format,
                    *pattern,
                )
                .unwrap();
                without_next(tool_reads::read_page(&entries, TOKEN, 0, *max))
            })
            .collect();
        (
            wants,
            tool_reads::read_proposal(state, proposal, None, None).unwrap(),
        )
    });
    let snap = snapshot(&f);
    for ((scope, format, pattern, max), want) in READ_CASES.into_iter().zip(wants) {
        let got = snap
            .read(ReadRequest {
                id: f.doc.clone(),
                scope: scope.map(String::from),
                format: Some(format.into()),
                pattern: pattern.map(String::from),
                max_tokens: max,
                page: None,
            })
            .unwrap();
        assert_eq!(got, want, "{scope:?} {format} {pattern:?} {max:?}");
        assert!(got.get("next").is_none());
        if max == Some(500) {
            assert_eq!(got["truncated"], json!(true));
            assert!(got["omitted"].as_u64().unwrap() > 0);
        }
    }

    // a proposal version reads as the builder reads it
    let got = snap
        .read(ReadRequest {
            id: f.proposal.clone(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(got, want_proposal);
}

#[test]
fn rpl38_adapter_search_matches_the_engine_reply() {
    let cases = [(None, None), (Some("tag:1.0.0"), Some(300usize))];
    let (f, wants) = fixture(|state, _, _| {
        cases
            .iter()
            .map(|(scope, max)| {
                let resolved =
                    tool_reads::scope_of("canon", state, &scope.map(String::from)).unwrap();
                without_next(
                    tool_reads::search_reply(
                        state,
                        &resolved,
                        &SearchQuery {
                            text: "ledger".into(),
                            regex: false,
                            ignore_case: true,
                            filters: SearchFilters::default(),
                        },
                        None,
                        *max,
                        None,
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<Value>>()
    });
    let snap = snapshot(&f);
    for ((scope, max), want) in cases.into_iter().zip(wants) {
        let got = snap
            .search(SearchRequest {
                query: "ledger".into(),
                ignore_case: true,
                scope: scope.map(String::from),
                max_tokens: max,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got, want, "{scope:?} {max:?}");
        assert!(got.get("next").is_none());
    }
}

#[test]
fn rpl38_adapter_refuses_session_bound_inputs() {
    let (f, ()) = fixture(|_, _, _| ());
    let snap = snapshot(&f);
    let read = |scope: Option<&str>, page: Option<&str>| {
        snap.read(ReadRequest {
            id: f.doc.clone(),
            scope: scope.map(String::from),
            page: page.map(String::from),
            ..Default::default()
        })
    };
    let search = |scope: Option<&str>, page: Option<&str>, brief: bool| {
        snap.search(SearchRequest {
            query: "ledger".into(),
            scope: scope.map(String::from),
            page: page.map(String::from),
            brief,
            ..Default::default()
        })
    };
    for scope in ["work", "view:work", "inflight"] {
        assert_eq!(read(Some(scope), None).unwrap_err().code, "E_BAD_INPUT");
        assert_eq!(
            search(Some(scope), None, false).unwrap_err().code,
            "E_BAD_INPUT"
        );
    }
    let cursor = format!("{}:1", "ab".repeat(32));
    assert_eq!(read(None, Some(&cursor)).unwrap_err().code, "E_BAD_INPUT");
    assert_eq!(
        search(None, Some(&cursor), false).unwrap_err().code,
        "E_BAD_INPUT"
    );
    assert_eq!(search(None, None, true).unwrap_err().code, "E_BAD_INPUT");
    // the accepted scopes still work
    assert!(read(Some("canon"), None).is_ok());
    assert!(read(Some("tag:1.0.0"), None).is_ok());
}

// ---------------------------------------------------------------------------------------------
// crb1: compare and diff of two scopes are the engine's, and refuse what read refuses (D-2QX9W)

/// The scope pairs `(from, to)` of the compare cases: canon's one landing by a short id, the
/// tag and the landed proposal version.
fn crb1_pairs(state: &ReadState, proposal: &str) -> Vec<(String, String)> {
    let l1 = state.canon_head().unwrap().to_string();
    vec![
        (format!("landing:{}", &l1[..12]), "canon".into()),
        ("canon".into(), "tag:1.0.0".into()),
        (format!("proposal:{proposal}"), "canon".into()),
    ]
}

fn crb1_scope(state: &ReadState, text: &str) -> rdy_engine::Scope {
    tool_reads::scope_of("canon", state, &Some(text.to_string())).unwrap()
}

#[test]
fn crb1_compare_equals_the_engine_compare() {
    let (f, (pairs, wants)) = fixture(|state, _, proposal| {
        let pairs = crb1_pairs(state, proposal);
        let wants: Vec<rdy_adapter::Compare> = pairs
            .iter()
            .map(|(from, to)| {
                state
                    .compare(&crb1_scope(state, to), &crb1_scope(state, from))
                    .unwrap()
            })
            .collect();
        (pairs, wants)
    });
    let snap = snapshot(&f);
    for ((from, to), want) in pairs.iter().zip(wants) {
        let got = snap.compare(Some(from.as_str()), to).unwrap();
        assert_eq!(got, want, "{from} {to}");
    }
}

#[test]
fn crb1_diff_equals_the_engine_diff() {
    let (f, (pairs, wants)) = fixture(|state, _, proposal| {
        let pairs = crb1_pairs(state, proposal);
        let wants: Vec<rdy_adapter::Diff> = pairs
            .iter()
            .map(|(from, to)| {
                state
                    .diff(&crb1_scope(state, from), &crb1_scope(state, to))
                    .unwrap()
            })
            .collect();
        (pairs, wants)
    });
    let snap = snapshot(&f);
    for ((from, to), want) in pairs.iter().zip(wants) {
        let got = snap.diff(Some(from.as_str()), to).unwrap();
        assert_eq!(got, want, "{from} {to}");
    }
}

#[test]
fn crb1_compare_refuses_what_read_refuses() {
    let (f, ()) = fixture(|_, _, _| ());
    let snap = snapshot(&f);
    let key = |e: rdy_adapter::RdyError| (e.code, e.message, e.fix);
    let read = |scope: &str| {
        key(snap
            .read(ReadRequest {
                id: f.doc.clone(),
                scope: Some(scope.into()),
                ..Default::default()
            })
            .unwrap_err())
    };
    let none = rdy_adapter::Hash::of(b"none");
    let unreached = format!("landing:{none}");
    for scope in ["work", "view:work", "inflight", unreached.as_str()] {
        let want = read(scope);
        assert_eq!(
            key(snap.compare(Some(scope), "canon").unwrap_err()),
            want,
            "{scope}"
        );
        assert_eq!(
            key(snap.compare(Some("canon"), scope).unwrap_err()),
            want,
            "{scope}"
        );
        assert_eq!(
            key(snap.diff(Some(scope), "canon").unwrap_err()),
            want,
            "{scope}"
        );
        assert_eq!(
            key(snap.diff(Some("canon"), scope).unwrap_err()),
            want,
            "{scope}"
        );
    }
    let (code, message, _) = key(snap.compare(Some(unreached.as_str()), "canon").unwrap_err());
    assert_eq!(code, "E_NOT_FOUND");
    assert_eq!(
        message,
        format!("landing {none} is not reached from canon, a tag, a held chain or a replaced mark")
    );
}

// ---------------------------------------------------------------------------------------------
// crb5: the compare page is rdy_diff's body, and every page carries the whole diff's counts

/// The names of the crb5 project: the doc, landing L1 (the doc and 40 claims, tagged 1.0.0) and
/// the proposal version that landed L2 (each of the 40 claims updated).
struct Crb5Names {
    doc: String,
    l1: String,
    proposal: String,
}

/// A project with two landings: L1 writes a doc with 40 claims and is tagged `v1.0.0`, L2
/// updates every claim. `expect` runs on the engine state before the project closes.
fn crb5_fixture<T>(
    expect: impl FnOnce(&Arc<ReadState>, &Crb5Names) -> T,
) -> (tempfile::TempDir, T) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let write = |r: &mut Rdy, ops: Vec<Value>| {
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(Value::Array(ops)).unwrap(),
                justification: None,
                message: "crb5".into(),
            },
        )
        .unwrap()
    };
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Terms"}}),
    ];
    for i in 0..40 {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim {i} says ledger text. {}", "x".repeat(200))}));
    }
    let w = write(&mut r, ops);
    let doc = w.created["$doc"].clone();
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let l1 = r.read().canon_head().unwrap().to_string();
    let ops = (0..40)
        .map(|i| json!({"op": "update", "id": w.created[&format!("$c{i}")], "body": format!("Claim {i} says new ledger text. {}", "y".repeat(200))}))
        .collect();
    write(&mut r, ops);
    let p = r
        .propose(&session(Role::Writer), "work", "T2", Some("test summary"))
        .unwrap();
    let proposal = p.r#ref.clone();
    r.land(&session(Role::Approver), &p.id).unwrap();
    let names = Crb5Names { doc, l1, proposal };
    let expected = {
        let state = r.read();
        expect(&state, &names)
    };
    r.close().unwrap();
    (dir, expected)
}

fn crb5_query(from: &str, to: &str, max_tokens: usize) -> rdy_adapter::compare::DiffQuery {
    rdy_adapter::compare::DiffQuery {
        from: Some(from.into()),
        to: to.into(),
        doc: None,
        detail: false,
        max_tokens: Some(max_tokens),
        page: None,
    }
}

#[test]
fn crb5_diff_reply_equals_rdy_diff() {
    use rdy_engine::diff_request::DiffRequest;
    let (dir, (queries, wants)) = crb5_fixture(|state, n| {
        let short = format!("landing:{}", &n.l1[..12]);
        let proposal = format!("proposal:{}", n.proposal);
        // (from, to, doc, detail, max_tokens): whole, rooted with detail, cut with detail, cut
        let cases: [(&str, &str, Option<&str>, bool, usize); 4] = [
            (&short, "canon", None, false, 100_000),
            ("tag:1.0.0", "canon", Some(&n.doc), true, 100_000),
            ("tag:1.0.0", &proposal, None, true, 2_000),
            ("canon", "tag:1.0.0", None, false, 300),
        ];
        let mut queries = vec![];
        let mut wants = vec![];
        for (from, to, doc, detail, max) in cases {
            let req = DiffRequest {
                from: from.into(),
                to: Some(to.into()),
                doc: doc.map(str::to_string),
                detail,
            };
            let reply = state.diff_page(&req, None, None, max).unwrap();
            wants.push(serde_json::to_value(reply).unwrap());
            queries.push(rdy_adapter::compare::DiffQuery {
                doc: doc.map(str::to_string),
                detail,
                ..crb5_query(from, to, max)
            });
        }
        (queries, wants)
    });
    let snap = StoreHandle::open(dir.path()).unwrap().snapshot().unwrap();
    for (q, want) in queries.iter().zip(wants) {
        let got = snap.diff_reply(q).unwrap();
        // a cursor is a token of the two states and an offset: it names no tool, so the two
        // cursors are equal as text
        assert_eq!(got, want, "{q:?}");
    }
    // the cut cases are cut, so a cursor was compared
    assert_eq!(
        snap.diff_reply(&queries[3]).unwrap()["truncated"],
        json!(true)
    );
}

/// The pages of `query` from its first, following `next` until it is absent.
fn crb5_pages(snap: &rdy_adapter::Snapshot, query: &rdy_adapter::compare::DiffQuery) -> Vec<Value> {
    let mut pages = vec![snap.diff_reply(query).unwrap()];
    while let Some(next) = pages.last().unwrap().get("next").and_then(Value::as_str) {
        let q = rdy_adapter::compare::DiffQuery {
            page: Some(next.to_string()),
            ..query.clone()
        };
        pages.push(snap.diff_reply(&q).unwrap());
        assert!(pages.len() <= 41, "the pages never end");
    }
    pages
}

#[test]
fn crb5_counts_are_the_whole_diff_on_every_page() {
    let (dir, ()) = crb5_fixture(|_, _| ());
    let snap = StoreHandle::open(dir.path()).unwrap().snapshot().unwrap();
    let whole = snap
        .diff_reply(&crb5_query("tag:1.0.0", "canon", 100_000))
        .unwrap();
    assert_eq!(
        whole["counts"],
        json!({"added": 0, "removed": 0, "updated": 40, "moved": 0})
    );
    assert!(whole.get("truncated").is_none(), "{whole:#}");
    let pages = crb5_pages(&snap, &crb5_query("tag:1.0.0", "canon", 300));
    assert!(pages.len() > 1, "{pages:#?}");
    assert_eq!(pages[0]["truncated"], json!(true));
    for page in &pages {
        assert_eq!(page["counts"], whole["counts"], "{page:#}");
    }
}

#[test]
fn crb5_pages_join_to_the_whole_diff() {
    let (dir, ()) = crb5_fixture(|_, _| ());
    let snap = StoreHandle::open(dir.path()).unwrap().snapshot().unwrap();
    let whole = snap
        .diff_reply(&crb5_query("tag:1.0.0", "canon", 100_000))
        .unwrap();
    let pages = crb5_pages(&snap, &crb5_query("tag:1.0.0", "canon", 300));
    let joined: Vec<Value> = pages
        .iter()
        .flat_map(|p| p["entries"].as_array().unwrap().clone())
        .collect();
    assert_eq!(Value::Array(joined), whole["entries"]);
}
