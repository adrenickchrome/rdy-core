//! rpag1, rfoc1, rpat1: rdy_read pages whole blocks with a cursor (M2c3's names and shape), reads
//! one section by <doc>#<heading-slug>, and filters by pattern.
use rdy_engine::RdyError;
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn st_with(ops: Value) -> (tempfile::TempDir, McpState, BTreeMap<String, String>) {
    let (d, mut r) = project();
    let res = write(&mut r, "work", ops).unwrap();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    (d, st, res.created.clone())
}

fn many(n: usize) -> Value {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Many", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Claims"}}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim number {i} holds.")}));
    }
    Value::Array(ops)
}

fn read(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_read(st, serde_json::from_value(args).unwrap())
}

#[test]
fn rpag1_paging_shows_every_block_once() {
    let (_d, mut st, c) = st_with(many(200));
    let full = read(&mut st, json!({"id": c["$d"], "max_tokens": 100_000})).unwrap();
    assert!(
        full.get("next").is_none() && full.get("truncated").is_none(),
        "{full}"
    );
    let all: Vec<String> = full["text"]
        .as_str()
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(all.len(), 202);
    let mut seen: Vec<String> = vec![];
    let mut v = read(&mut st, json!({"id": c["$d"], "max_tokens": 300})).unwrap();
    let mut pages = 1;
    loop {
        seen.extend(v["text"].as_str().unwrap().lines().map(String::from));
        let Some(next) = v["next"].as_str().map(String::from) else {
            break;
        };
        assert_eq!(v["truncated"], true);
        assert_eq!(
            v["omitted"].as_u64().unwrap() as usize,
            202 - seen.len(),
            "{v}"
        );
        assert!(serde_json::to_string(&v).unwrap().len() / 4 <= 300, "{v}");
        let (token, offset) = next.split_once(':').unwrap();
        assert_eq!(
            (token.len(), offset.parse::<usize>().unwrap()),
            (64, seen.len())
        );
        v = read(&mut st, json!({"page": next, "max_tokens": 300})).unwrap();
        pages += 1;
        assert!(pages < 200, "paging does not end");
    }
    assert!(pages > 2, "{pages}");
    assert_eq!(seen, all);
}

/// A well-formed cursor no read in this session made.
fn foreign_token() -> String {
    "0123456789abcdef".repeat(4)
}

#[test]
fn rpag1_a_page_of_a_read_that_moved_is_stale() {
    let (_d, mut st, c) = st_with(many(200));
    let first = read(&mut st, json!({"id": c["$d"], "max_tokens": 300})).unwrap();
    let next = first["next"].as_str().unwrap().to_string();
    let target = c["$c150"].clone();
    st.engine
        .write(Box::new(move |r| {
            write(
                r,
                "work",
                json!([{"op": "update", "id": target, "body": "Claim 150 changed."}]),
            )?;
            Ok(Value::Null)
        }))
        .unwrap();
    let e = read(&mut st, json!({"page": next})).unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e}");
    assert!(e.message.contains("moved since its first page"), "{e}");
    assert_eq!(
        e.fix,
        "the read or briefing moved; start again without a cursor"
    );
    let e = read(&mut st, json!({"page": format!("{}:5", foreign_token())})).unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e}");
    assert!(e.message.contains("unknown"), "{e}");
    assert_eq!(
        e.fix,
        "the read or briefing moved; start again without a cursor"
    );
}

#[test]
fn rpag1_malformed_cursors_and_page_with_id_are_refused() {
    let (_d, mut st, c) = st_with(many(5));
    let t = foreign_token();
    for bad in [
        "x:y".to_string(),
        t.clone(),
        format!("{}:1", t.to_uppercase()),
        format!("{t}:-1"),
        format!("{t}:1,2"),
        format!("{}:1", &t[..20]),
        String::new(),
    ] {
        let e = read(&mut st, json!({"page": bad})).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{bad:?}: {e}");
        assert_eq!(
            e.message, "page is not a cursor from rdy_read's next",
            "{bad:?}"
        );
    }
    let e = read(&mut st, json!({"page": format!("{t}:1"), "id": c["$d"]})).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_BAD_INPUT", "page continues a read: pass it alone")
    );
    let e = read(&mut st, json!({})).unwrap_err();
    assert_eq!(e.message, "pass id, or page to continue a read");
}

#[test]
fn rfoc1_doc_hash_slug_reads_one_section() {
    let (_d, mut st, c) = st_with(json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$a", "parent": "$d", "fields": {"title": "Set up"}},
        {"op": "create", "type": "claim", "id": "$a1", "parent": "$a", "body": "First setup."},
        {"op": "create", "type": "section", "id": "$b", "parent": "$d", "fields": {"title": "Goals"}},
        {"op": "create", "type": "claim", "id": "$b1", "parent": "$b", "body": "First goal."},
        {"op": "create", "type": "section", "id": "$g", "parent": "$d", "fields": {"title": "Goals"}},
        {"op": "create", "type": "claim", "id": "$g1", "parent": "$g", "body": "Second goal."}
    ]));
    let text = |v: &Value| v["text"].as_str().unwrap().to_string();
    let v = read(&mut st, json!({"id": format!("{}#set-up", c["$d"])})).unwrap();
    assert_eq!(v["id"], c["$a"].as_str(), "{v}");
    assert!(
        text(&v).contains("First setup.") && !text(&v).contains("goal"),
        "{v}"
    );
    let v = read(&mut st, json!({"id": "guide#goals"})).unwrap();
    assert!(
        text(&v).contains("First goal.") && !text(&v).contains("Second goal."),
        "{v}"
    );
    let v = read(&mut st, json!({"id": format!("{}#goals-2", c["$d"])})).unwrap();
    assert!(
        text(&v).contains("Second goal.") && !text(&v).contains("First goal."),
        "{v}"
    );
    let e = read(&mut st, json!({"id": format!("{}#nope", c["$d"])})).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK", "{e}");
    assert!(e.message.contains("#nope"), "{e}");
    let e = read(&mut st, json!({"id": format!("{}#goals", c["$a"])})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    let e = read(&mut st, json!({"id": format!("{}#", c["$d"])})).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    let v = read(&mut st, json!({"id": format!("[{}] see #goals", c["$a1"])})).unwrap();
    assert_eq!(
        v["id"],
        c["$a1"].as_str(),
        "a ref line is never split at #: {v}"
    );
}

#[test]
fn rpat1_a_pattern_keeps_matching_blocks_and_code_lines() {
    let (_d, mut st, c) = st_with(json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Config", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Server"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Alpha listens on one port."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Beta is unrelated."},
        {"op": "create", "type": "code", "id": "$k", "parent": "$s", "fields": {"lang": "toml"}, "body": "[server]\nhost = \"0.0.0.0\"\nport = 7007"}
    ]));
    let v = read(&mut st, json!({"id": c["$d"], "pattern": "7007"})).unwrap();
    assert_eq!(v["matched"], 1, "{v}");
    let lines: Vec<&str> = v["text"].as_str().unwrap().lines().map(str::trim).collect();
    assert_eq!(lines.len(), 2, "{v}");
    assert!(lines[0].starts_with(&format!("{} code: ", c["$k"])), "{v}");
    assert_eq!(lines[1], "| port = 7007");
    let v = read(&mut st, json!({"id": c["$d"], "pattern": "ALPHA"})).unwrap();
    assert_eq!(v["matched"], 1, "{v}");
    assert!(
        v["text"].as_str().unwrap().contains(c["$a"].as_str()),
        "{v}"
    );
    let v = read(
        &mut st,
        json!({"id": c["$d"], "pattern": "/[Bb]eta|port = \\d+/"}),
    )
    .unwrap();
    assert_eq!(v["matched"], 2, "{v}");
    let v = read(&mut st, json!({"id": c["$d"], "pattern": "zzz"})).unwrap();
    assert_eq!(
        (v["matched"].as_u64(), v["text"].as_str()),
        (Some(0), Some("")),
        "{v}"
    );
}

#[test]
fn rpat1_bad_patterns_are_refused() {
    let (_d, mut st, c) = st_with(many(3));
    for (pat, msg) in [
        ("/(/", "pattern is not a valid regex"),
        ("", "pattern is empty"),
    ] {
        let e = read(&mut st, json!({"id": c["$d"], "pattern": pat})).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{pat:?}: {e}");
        assert!(e.message.starts_with(msg), "{e}");
    }
    let e = read(&mut st, json!({"id": c["$d"], "pattern": "x".repeat(1025)})).unwrap_err();
    assert_eq!(e.message, "pattern is longer than 1024 characters");
    let ok = read(&mut st, json!({"id": c["$d"], "pattern": "x".repeat(1024)})).unwrap();
    assert_eq!(ok["matched"], 0);
}

#[test]
fn rpat1_compact_matches_a_blocks_whole_text_as_markdown_does() {
    let (_d, mut st, c) = st_with(json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Goals"}},
        {"op": "create", "type": "claim", "id": "$p", "parent": "$s", "body": "First goal. The service listens on port 7007."},
        {"op": "create", "type": "table", "id": "$t", "parent": "$s", "fields": {"columns": ["Host", "Port"], "rows": [["alpha", "80"], ["beta", "8443"]]}},
        {"op": "create", "type": "diagram", "id": "$g", "parent": "$s", "fields": {"lang": "mermaid", "alt": "Flow"}, "body": "graph TD\n  A-->B\n  B-->Gamma"},
        {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "Unrelated."}
    ]));
    for (pat, block) in [("7007", "$p"), ("8443", "$t"), ("gamma", "$g")] {
        let v = read(&mut st, json!({"id": c["$d"], "pattern": pat})).unwrap();
        assert_eq!(v["matched"], 1, "{pat}: {v}");
        let lines: Vec<&str> = v["text"].as_str().unwrap().lines().map(str::trim).collect();
        assert_eq!(lines.len(), 1, "{pat}: the head line alone: {v}");
        assert!(lines[0].starts_with(c[block].as_str()), "{pat}: {v}");
        let md = read(
            &mut st,
            json!({"id": c["$d"], "pattern": pat, "format": "markdown"}),
        )
        .unwrap();
        assert_eq!(
            md["matched"], v["matched"],
            "{pat}: compact and markdown keep the same blocks"
        );
    }
}

#[test]
fn rpag1_a_page_keeps_its_reads_view_after_rdy_session_moves() {
    let (_d, mut st, c) = st_with(many(200));
    let first = read(&mut st, json!({"id": c["$d"], "max_tokens": 300})).unwrap();
    let next = first["next"].as_str().unwrap().to_string();
    let want = read(&mut st, json!({"page": next, "max_tokens": 300})).unwrap();
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "other"}))
            .unwrap(),
    )
    .unwrap();
    let e = read(&mut st, json!({"id": c["$d"]})).unwrap_err();
    assert_eq!(
        e.code, "E_UNKNOWN_BLOCK",
        "guard: the doc is not in the new view: {e}"
    );
    let v = read(&mut st, json!({"page": next, "max_tokens": 300})).unwrap();
    assert_eq!(v, want, "the cursor continues its own read");
}

#[test]
fn rpag1_every_page_fits_max_tokens_under_a_conflict_on_the_root() {
    let (_d, mut r) = project();
    let res = write(&mut r, "seed", many(60)).unwrap();
    land(&mut r, "seed").unwrap();
    let s = res.created["$s"].clone();
    let base = version_of(&r, &rdy_engine::Scope::Canon, &s);
    let (sa, sb) = (as_agent("agent:a", "r1"), as_agent("agent:b", "r1"));
    for (who, view, title) in [(&sa, "a", "A's claims"), (&sb, "b", "B's claims")] {
        write_as(
            &mut r,
            who,
            view,
            json!([{"op": "update", "id": s, "base": base, "fields": {"title": title}}]),
        )
        .unwrap();
    }
    let pa = r.propose(&sa, "a", "a").unwrap();
    r.land_report(&approver(), &pa.r#ref).unwrap();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:b", "role": "writer", "view": "b"})).unwrap(),
    )
    .unwrap();
    let max = 300;
    let mut v = read(&mut st, json!({"id": s, "max_tokens": max})).unwrap();
    assert!(
        serde_json::to_string(&v["conflict"]).unwrap().len() > 400,
        "guard: the root's conflict is larger than the old 400-byte reserve: {v}"
    );
    let mut pages = 1;
    loop {
        assert!(serde_json::to_string(&v).unwrap().len() / 4 <= max, "{v}");
        assert!(v.get("cut").is_none(), "{v}");
        let Some(next) = v["next"].as_str().map(String::from) else {
            break;
        };
        v = read(&mut st, json!({"page": next, "max_tokens": max})).unwrap();
        pages += 1;
        assert!(pages < 100, "paging does not end");
    }
    assert!(pages > 1, "{pages}");
}
