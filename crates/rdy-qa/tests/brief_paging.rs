//! bpag1: a cut briefing carries per-doc counts and a cursor; paging through it shows every entry
//! once; a page over a briefing that moved is stale (FW-LP4).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};

/// Doc `hub` with claim H; doc `many` with `n` claims that each reference H; landed.
fn hub(r: &mut Rdy, n: usize) -> (BlockId, BlockId) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "hub", "title": "Hub", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$h", "parent": "$s", "body": "H holds."}
    ]))
    .unwrap();
    let (hub_doc, h) = (w.created["$d"].clone(), w.created["$h"].clone());
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$m", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$ms", "parent": "$m", "fields": {"title": "S"}}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$ms", "body": format!("Claim {i} cites {{{{{h}}}}}.")}));
    }
    write(r, "work", Value::Array(ops)).unwrap();
    land(r, "work").unwrap();
    (
        BlockId::parse(&hub_doc).unwrap(),
        BlockId::parse(&h).unwrap(),
    )
}
fn cov(h: &BlockId) -> Coverage {
    Coverage {
        ids: vec![h.to_string()],
        path: None,
    }
}

#[test]
fn bpag1_a_cut_briefing_carries_a_cursor_and_per_doc_counts() {
    let (_d, mut r) = project();
    let (hub_doc, h) = hub(&mut r, 60);
    let b = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    assert!(b.truncated && b.next.is_some(), "{:?}", b.next);
    assert_eq!(b.docs.len(), 1, "{:?}", b.docs);
    let d = &b.docs[0];
    assert_eq!(
        (d.doc.as_str(), d.title.as_str(), d.blocks),
        (hub_doc.to_string().as_str(), "Hub", 1)
    );
    assert_eq!((d.dependents, d.will_go_stale), (60, 60));
    let full = r
        .read()
        .brief(&Scope::Canon, &cov(&h), Some(100_000))
        .unwrap();
    assert!(!full.truncated && full.next.is_none() && full.docs.is_empty());
}

#[test]
fn bpag1_paging_through_shows_every_entry_once() {
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    let full = r
        .read()
        .brief(&Scope::Canon, &cov(&h), Some(100_000))
        .unwrap();
    let first = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    first.register(&r.briefs());
    let (mut deps, mut stale) = (first.dependents.clone(), first.will_go_stale.clone());
    let mut next = first.next.clone();
    let mut pages = 1;
    while let Some(cursor) = next {
        let p = r
            .read()
            .brief_page(&Scope::Canon, &r.briefs(), &cursor, Some(400))
            .unwrap();
        assert_eq!(p.base.token, first.base.token);
        deps.extend(p.dependents.clone());
        stale.extend(p.will_go_stale.clone());
        next = p.next.clone();
        pages += 1;
        assert!(pages < 100, "paging does not end");
    }
    assert!(pages > 1);
    assert_eq!(deps, full.dependents);
    assert_eq!(stale, full.will_go_stale);
}

#[test]
fn bpag1_paging_ends_even_when_the_budget_fits_nothing() {
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 8);
    let full = r
        .read()
        .brief(&Scope::Canon, &cov(&h), Some(100_000))
        .unwrap();
    let first = r.read().brief(&Scope::Canon, &cov(&h), Some(1)).unwrap();
    first.register(&r.briefs());
    let (mut deps, mut next, mut pages) = (first.dependents.clone(), first.next.clone(), 1);
    while let Some(cursor) = next {
        let p = r
            .read()
            .brief_page(&Scope::Canon, &r.briefs(), &cursor, Some(1))
            .unwrap();
        deps.extend(p.dependents.clone());
        next = p.next.clone();
        pages += 1;
        assert!(pages < 100, "paging does not end");
    }
    assert_eq!(deps, full.dependents);
}

#[test]
fn bpag1_a_page_of_a_briefing_that_moved_is_stale() {
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    let first = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    first.register(&r.briefs());
    let cursor = first.next.clone().unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": h.to_string(), "body": "H, changed."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let e = r
        .read()
        .brief_page(&Scope::Canon, &r.briefs(), &cursor, Some(400))
        .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE, "{e}");
    let unknown = format!("{}:0,0,0,0,0,0,0", "0".repeat(64));
    let e = r
        .read()
        .brief_page(&Scope::Canon, &r.briefs(), &unknown, None)
        .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE, "{e}");
    let e = r
        .read()
        .brief_page(&Scope::Canon, &r.briefs(), "nonsense", None)
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}

#[test]
fn bpag1_rdy_brief_pages_over_mcp_and_page_stands_alone() {
    let s = Served::new();
    let h = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let (_doc, h) = hub(&mut r, 60);
        r.close().unwrap();
        h
    };
    let mut c = McpClient::start_proxy(&s);
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:p", "run": "p1", "role": "writer"}),
    );
    assert!(r.code().is_none());
    let b = c
        .call(
            "rdy_brief",
            json!({"ids": [h.to_string()], "scope": "canon", "max_tokens": 400}),
        )
        .ok();
    let cursor = b["next"].as_str().unwrap().to_string();
    let p = c
        .call(
            "rdy_brief",
            json!({"page": cursor, "scope": "canon", "max_tokens": 400}),
        )
        .ok();
    assert_eq!(p["base"]["token"], b["base"]["token"]);
    let e = c.call("rdy_brief", json!({"page": cursor, "ids": [h.to_string()]}));
    assert_eq!(e.code(), Some("E_BAD_INPUT"));
    drop(c);
    s.stop();
}
