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
