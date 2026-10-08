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
    let tools = c.tools();
    let d = tools.iter().find(|t| t["name"] == "rdy_brief").unwrap()["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(d.contains("page: <next>"), "{d}");
    drop(c);
    s.stop();
}

#[test]
fn bpag1_a_page_after_a_new_dependent_landed_is_stale() {
    // the token covers only the covered blocks and their outgoing suspect-link targets, so a new
    // dependent of H leaves it as it was; the entries it sorts among still shifted
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    let first = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    first.register(&r.briefs());
    let cursor = first.next.clone().unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "extra", "title": "Extra", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$es", "parent": "$e", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "parent": "$es", "body": format!("A late claim cites {{{{{h}}}}}.")}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let again = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    assert_eq!(again.base.token, first.base.token, "the token did not move");
    let e = r
        .read()
        .brief_page(&Scope::Canon, &r.briefs(), &cursor, Some(400))
        .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE, "{e}");
}

#[test]
fn bpag1_an_offset_past_the_end_of_its_section_is_stale_not_malformed() {
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    let first = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    first.register(&r.briefs());
    let cursor = first.next.clone().unwrap();
    // the same cursor, digest kept, with the dependents offset far past the 60 entries there are
    let (token, rest) = cursor.split_once(':').unwrap();
    let (offsets, digest) = rest.split_once(':').unwrap();
    let mut parts: Vec<String> = offsets.split(',').map(str::to_string).collect();
    parts[1] = "100000".to_string();
    let forged = format!("{token}:{}:{digest}", parts.join(","));
    let e = r
        .read()
        .brief_page(&Scope::Canon, &r.briefs(), &forged, Some(400))
        .unwrap_err();
    assert_eq!(e.code, E_PAGE_STALE, "{e}");
    // a cursor without its digest is malformed, not stale
    let e = r
        .read()
        .brief_page(
            &Scope::Canon,
            &r.briefs(),
            &format!("{token}:{offsets}"),
            Some(400),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}

/// How many detail entries a briefing shows, over every paged section.
fn shown(b: &Briefing) -> usize {
    b.governs.len()
        + b.dependents.len()
        + b.lineage.len()
        + b.will_go_stale.len()
        + b.suspects.len()
        + b.inflight.views.len()
        + b.inflight.proposals.len()
        + b.inflight.intents.len()
        + b.base.blocks.len()
}

fn json_tokens(b: &Briefing) -> usize {
    serde_json::to_string(b).unwrap().len() / 4
}

#[test]
fn bfit1_a_cut_briefing_with_its_cursor_fits_every_budget() {
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    let wide = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    wide.register(&r.briefs());
    let cursor = wide
        .next
        .clone()
        .expect("60 dependents do not fit 400 tokens");
    for max in 1..=1500 {
        let b = r.read().brief(&Scope::Canon, &cov(&h), Some(max)).unwrap();
        assert!(
            json_tokens(&b) <= max || shown(&b) == 0,
            "brief at {max}: {} tokens with {} entries shown, next {:?}",
            json_tokens(&b),
            shown(&b),
            b.next
        );
        let p = r
            .read()
            .brief_page(&Scope::Canon, &r.briefs(), &cursor, Some(max))
            .unwrap();
        assert!(
            json_tokens(&p) <= max || shown(&p) <= 1,
            "page at {max}: {} tokens with {} entries shown, next {:?}",
            json_tokens(&p),
            shown(&p),
            p.next
        );
    }
}

/// A canon briefing of H (60 dependents) cut to 400 tokens and registered, with view `other`'s
/// unlanded write to H listed in its `inflight.views`.
fn listed_other(r: &mut Rdy, h: &BlockId) -> Briefing {
    write(
        r,
        "other",
        json!([{"op": "update", "id": h.to_string(), "body": "H, in other."}]),
    )
    .unwrap();
    let first = r.read().brief(&Scope::Canon, &cov(h), Some(400)).unwrap();
    first.register(&r.briefs());
    assert!(first.next.is_some(), "60 dependents do not fit 400 tokens");
    assert_eq!(
        first
            .inflight
            .views
            .iter()
            .map(|v| v.view.as_str())
            .collect::<Vec<_>>(),
        vec!["other"]
    );
    first
}

/// Pages from `cursor` to the end; every page is served.
fn page_to_end(r: &Rdy, scope: &Scope, cursor: String) -> usize {
    let (mut next, mut pages) = (Some(cursor), 0);
    while let Some(c) = next {
        let p = r
            .read()
            .brief_page(scope, &r.briefs(), &c, Some(400))
            .unwrap_or_else(|e| panic!("page {pages}: {e}"));
        next = p.next.clone();
        pages += 1;
        assert!(pages < 100, "paging does not end");
    }
    pages
}

#[test]
fn bpag1_a_listed_views_write_to_an_uncovered_block_keeps_the_cursor_valid() {
    // inflight is advice: a write by a view it lists moves that view's last_write, which must
    // not refuse a page of a briefing whose covered blocks, dependents and the rest did not move
    let (_d, mut r) = project();
    let (hub_doc, h) = hub(&mut r, 60);
    let first = listed_other(&mut r, &h);
    let before = r.read().view_head("other").unwrap().last_write_ms;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    // writes until the view's last write moved (one write may land in the same millisecond)
    while r.read().view_head("other").unwrap().last_write_ms == before {
        assert!(
            std::time::Instant::now() < deadline,
            "last_write never moved"
        );
        write(
            &mut r,
            "other",
            json!([{"op": "update", "id": hub_doc.to_string(), "fields": {"title": "Hub, in other."}}]),
        )
        .unwrap();
    }
    let again = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    assert_ne!(
        again.inflight.views[0].last_write_ms,
        first.inflight.views[0].last_write_ms
    );
    assert!(page_to_end(&r, &Scope::Canon, first.next.clone().unwrap()) > 1);
}

#[test]
fn bpag1_a_view_that_starts_changing_a_covered_block_keeps_the_cursor_valid() {
    // an unlanded write to H in another view changes no canon entry: the page is served, and
    // the new inflight entry may shift an advisory page
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    let first = r.read().brief(&Scope::Canon, &cov(&h), Some(400)).unwrap();
    first.register(&r.briefs());
    assert!(first.inflight.views.is_empty());
    write(
        &mut r,
        "late",
        json!([{"op": "update", "id": h.to_string(), "body": "H, in late."}]),
    )
    .unwrap();
    assert!(page_to_end(&r, &Scope::Canon, first.next.clone().unwrap()) > 1);
}

#[test]
fn bpag1_an_inflight_list_that_shrank_is_clamped_not_stale() {
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    let first = listed_other(&mut r, &h);
    // the first page showed the one inflight view: its cursor's inflight.views offset is 1
    let (_token, rest) = first.next.as_deref().unwrap().split_once(':').unwrap();
    let offsets: Vec<&str> = rest.split(':').next().unwrap().split(',').collect();
    assert_eq!(offsets[5], "1", "{rest}");
    r.view_close(&writer(), &["other".to_string()], "done")
        .unwrap();
    assert!(page_to_end(&r, &Scope::Canon, first.next.clone().unwrap()) > 1);
}

#[test]
fn bpag1_a_page_in_another_scope_is_bad_input_naming_both() {
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 60);
    // view other changed H, so its briefing of H differs from canon's
    let first = listed_other(&mut r, &h);
    let e = r
        .read()
        .brief_page(
            &Scope::View("other".into()),
            &r.briefs(),
            first.next.as_deref().unwrap(),
            Some(400),
        )
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("canon") && e.message.contains("view:other"),
        "{e}"
    );
    assert!(e.fix.contains("canon"), "{e}");
    // the same cursor in its own scope is served
    assert!(page_to_end(&r, &Scope::Canon, first.next.clone().unwrap()) > 1);
}

#[test]
fn bpag1_a_briefing_is_cut_only_when_the_whole_does_not_fit() {
    // per-doc counts belong to a cut briefing: they never make a briefing that fits whole cut
    let (_d, mut r) = project();
    let (_hub_doc, h) = hub(&mut r, 3);
    let full = r
        .read()
        .brief(&Scope::Canon, &cov(&h), Some(100_000))
        .unwrap();
    assert!(!full.truncated && full.docs.is_empty());
    let whole = json_tokens(&full);
    for max in 1..=whole + 300 {
        let b = r.read().brief(&Scope::Canon, &cov(&h), Some(max)).unwrap();
        assert_eq!(b.truncated, whole > max, "at {max} (whole {whole})");
        assert_eq!(!b.docs.is_empty(), b.truncated, "at {max}");
    }
}
