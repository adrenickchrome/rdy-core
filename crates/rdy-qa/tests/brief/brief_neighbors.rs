//! nb7 (D-YXBXH): a briefing lists every neighbor of its covered blocks after related_decisions.
//! The rows page like dependents, `neighbors_by_doc` counts the whole list on every page, a
//! neighbor change stales a page and never the write token, and a neighbor walk past a work
//! bound refuses the whole briefing.
use rdy_engine::brief::{Briefing, Coverage, NeighborDocCount, PAGE_SECTIONS};
use rdy_engine::neighbors::{Neighbor, NeighborVia};
use rdy_engine::*;
use rdy_model::{Block, BlockId, CROCKFORD};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn work() -> Scope {
    Scope::View("work".into())
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn blk(i: &str, ty: &str, parent: &str, order: &str, body: &str) -> Block {
    raw(i, ty, Some(parent), order, &[], body)
}

fn entity(i: &str, parent: &str, name: &str) -> Block {
    raw(i, "entity", Some(parent), "a0", &[("name", name)], "")
}

fn doc(i: &str, name: &str) -> Block {
    raw(
        i,
        "doc",
        None,
        "a1",
        &[("name", name), ("title", name), ("doc_type", "overview")],
        "",
    )
}

/// Doc `alpha` (d-0000A): section s-0000A with entities en-00001 (Ledger) and en-00002 (Queue)
/// and claims c-00001 (ties both), c-00002 (ties Ledger) and c-00003. Doc `beta` (d-0000B):
/// section s-0000B with c-00011 (ties Ledger), c-00012 and c-00013; section s-0000C with
/// c-00021 (ties both) and c-00022 (ties Queue). Written raw to view `work`.
fn ties(r: &mut Rdy) {
    raw_commit(
        r,
        "work",
        vec![
            doc("d-0000A", "alpha"),
            blk("s-0000A", "section", "d-0000A", "a1", ""),
            entity("en-00001", "s-0000A", "Ledger"),
            entity("en-00002", "s-0000A", "Queue"),
            blk(
                "c-00001",
                "claim",
                "s-0000A",
                "a1",
                "Uses {{en-00001}} and {{en-00002}}.",
            ),
            blk("c-00002", "claim", "s-0000A", "a2", "Also {{en-00001}}."),
            blk("c-00003", "claim", "s-0000A", "a3", "Stands alone."),
            doc("d-0000B", "beta"),
            blk("s-0000B", "section", "d-0000B", "a1", ""),
            blk(
                "c-00011",
                "claim",
                "s-0000B",
                "a1",
                "Beta uses {{en-00001}}.",
            ),
            blk("c-00012", "claim", "s-0000B", "a2", "Beta sibling."),
            blk("c-00013", "claim", "s-0000B", "a3", "Another beta sibling."),
            blk("s-0000C", "section", "d-0000B", "a2", ""),
            blk(
                "c-00021",
                "claim",
                "s-0000C",
                "a1",
                "Far uses {{en-00002}} and {{en-00001}}.",
            ),
            blk(
                "c-00022",
                "claim",
                "s-0000C",
                "a2",
                "Queue only {{en-00002}}.",
            ),
        ],
    );
}

fn row(
    block: &str,
    of: &[&str],
    via: NeighborVia,
    entity: Option<&str>,
    doc: &str,
    summary: &str,
) -> Neighbor {
    Neighbor {
        block: block.into(),
        of: of.iter().map(|s| s.to_string()).collect(),
        via,
        entity: entity.map(str::to_string),
        doc: doc.into(),
        summary: summary.into(),
    }
}

fn count(doc: &str, n: usize) -> NeighborDocCount {
    NeighborDocCount {
        doc: doc.into(),
        count: n,
    }
}

fn cov(ids: &[&str]) -> Coverage {
    Coverage {
        ids: ids.iter().map(|s| s.to_string()).collect(),
        path: None,
    }
}

/// The full rows of [`ties`] for covered c-00001 and c-00011, in Task 2's order.
fn tie_rows() -> Vec<Neighbor> {
    use NeighborVia::{Entity, Section};
    let (a, b) = ("d-0000A", "d-0000B");
    let en1 = Some("en-00001");
    vec![
        row("c-00002", &["c-00001"], Section, None, a, "Also Ledger."),
        row("c-00003", &["c-00001"], Section, None, a, "Stands alone."),
        row("c-00012", &["c-00011"], Section, None, b, "Beta sibling."),
        row(
            "c-00013",
            &["c-00011"],
            Section,
            None,
            b,
            "Another beta sibling.",
        ),
        row(
            "c-00021",
            &["c-00001", "c-00011"],
            Entity,
            en1,
            b,
            "Far uses Queue and Ledger.",
        ),
        row(
            "c-00001",
            &["c-00011"],
            Entity,
            en1,
            a,
            "Uses Ledger and Queue.",
        ),
        row("c-00002", &["c-00011"], Entity, en1, a, "Also Ledger."),
        row("c-00011", &["c-00001"], Entity, en1, b, "Beta uses Ledger."),
        row(
            "c-00022",
            &["c-00001"],
            Entity,
            Some("en-00002"),
            b,
            "Queue only Queue.",
        ),
    ]
}

#[test]
fn nb7_a_briefing_lists_every_neighbor_with_the_full_counts() {
    let (_d, mut r) = project();
    ties(&mut r);
    let b = r
        .read()
        .brief(&work(), &cov(&["c-00001", "c-00011"]), Some(1_000_000))
        .unwrap();
    assert!(!b.truncated && b.next.is_none(), "{:?}", b.omitted);
    assert_eq!(b.neighbors, tie_rows());
    assert_eq!(
        b.neighbors_by_doc,
        vec![count("d-0000A", 4), count("d-0000B", 5)]
    );
    // the rows serialize after related_decisions, with every field and no entity on a section row
    let v = serde_json::to_value(&b).unwrap();
    assert_eq!(
        v["neighbors"][0],
        json!({"block": "c-00002", "of": ["c-00001"], "via": "section", "doc": "d-0000A", "summary": "Also Ledger."})
    );
    assert_eq!(
        v["neighbors"][4],
        json!({"block": "c-00021", "of": ["c-00001", "c-00011"], "via": "entity", "entity": "en-00001", "doc": "d-0000B", "summary": "Far uses Queue and Ledger."})
    );
    assert_eq!(
        v["neighbors_by_doc"],
        json!([{"doc": "d-0000A", "count": 4}, {"doc": "d-0000B", "count": 5}])
    );
    let text = serde_json::to_string(&b).unwrap();
    let at = |k: &str| {
        text.find(&format!("\"{k}\":"))
            .unwrap_or_else(|| panic!("{k} in {text}"))
    };
    assert!(at("inflight") < at("neighbors") && at("neighbors") < at("neighbors_by_doc"));
    assert!(at("neighbors_by_doc") < at("base"), "{text}");
    // both keys are always present: a block with no neighbor gives []
    let alone = r.read().brief(&work(), &cov(&["s-0000A"]), None).unwrap();
    let v = serde_json::to_value(&alone).unwrap();
    assert_eq!(
        (&v["neighbors"], &v["neighbors_by_doc"]),
        (&json!([]), &json!([]))
    );
    assert_eq!(PAGE_SECTIONS[PAGE_SECTIONS.len() - 2], "neighbors");
    assert_eq!(PAGE_SECTIONS[PAGE_SECTIONS.len() - 1], "base.blocks");
}

/// Doc `wide`: section s-0000W with claims c-W0000.. (`n` of them; c-W0000 references entity
/// en-0000W, as do `far` claims in doc `far`, each in its own section). Written raw to `work`.
fn wide(r: &mut Rdy, n: usize, far: usize) {
    let mut blocks = vec![
        doc("d-0000W", "wide"),
        blk("s-0000W", "section", "d-0000W", "a1", ""),
        entity("en-0000W", "s-0000W", "Gauge"),
        doc("d-0000F", "far"),
    ];
    for i in 0..n {
        let body = if i == 0 {
            "The source uses {{en-0000W}}.".to_string()
        } else {
            format!("Wide claim {i} with a summary long enough to weigh on a page.")
        };
        blocks.push(blk(
            &nid("c", i),
            "claim",
            "s-0000W",
            &format!("a{i:05}"),
            &body,
        ));
    }
    for i in 0..far {
        let s = nid("sf", i + 1);
        blocks.push(blk(&s, "section", "d-0000F", &format!("a{i:05}"), ""));
        blocks.push(blk(
            &nid("f", i),
            "claim",
            &s,
            "a1",
            &format!("Far claim {i} uses {{{{en-0000W}}}}."),
        ));
    }
    raw_commit(r, "work", blocks);
}

/// The `n`th id with `prefix`, in id order for a fixed prefix.
fn nid(prefix: &str, n: usize) -> String {
    let mut body = [b'0'; 5];
    let mut v = n;
    for d in body.iter_mut().rev() {
        *d = CROCKFORD[v % 32];
        v /= 32;
    }
    format!("{prefix}-{}", std::str::from_utf8(&body).unwrap())
}

/// Every page of the briefing of `cov` at `max` tokens, first to last, each registered first.
fn pages(r: &Rdy, cov: &Coverage, max: usize) -> Vec<Briefing> {
    let first = r.read().brief(&work(), cov, Some(max)).unwrap();
    first.register(&r.briefs());
    let mut out = vec![first];
    while let Some(cursor) = out.last().unwrap().next.clone() {
        let p = r
            .read()
            .brief_page(&work(), &r.briefs(), &cursor, Some(max))
            .unwrap_or_else(|e| panic!("page {} at {max}: {e}", out.len()));
        out.push(p);
        assert!(out.len() < 1_000, "paging at {max} does not end");
    }
    out
}

#[test]
fn nb7_neighbor_pages_are_complete_once_and_make_progress() {
    let (_d, mut r) = project();
    wide(&mut r, 30, 4);
    let c = cov(&[&nid("c", 0)]);
    let full = r.read().brief(&work(), &c, Some(1_000_000)).unwrap();
    assert_eq!(full.neighbors.len(), 29 + 4);
    // in doc path order: far before wide
    assert_eq!(
        full.neighbors_by_doc,
        vec![count("d-0000F", 4), count("d-0000W", 29)]
    );
    for max in 1..=1_500 {
        let all = pages(&r, &c, max);
        let mut seen: Vec<Neighbor> = vec![];
        let mut last = 0usize;
        for (i, p) in all.iter().enumerate() {
            assert_eq!(p.base.token, full.base.token, "page {i} at {max}");
            assert_eq!(
                p.neighbors_by_doc, full.neighbors_by_doc,
                "page {i} at {max}: the counts are never cut"
            );
            // progress by offset: every page moves the cursor on, and none moves it back
            let moved: usize = p.offsets.iter().sum();
            if all.len() > 1 {
                assert!(moved > last || i == 0, "page {i} at {max} made no progress");
            }
            last = moved;
            seen.extend(p.neighbors.iter().cloned());
        }
        assert_eq!(seen, full.neighbors, "every row once, in order, at {max}");
        let at = PAGE_SECTIONS
            .iter()
            .position(|s| *s == "neighbors")
            .unwrap();
        assert_eq!(all.last().unwrap().offsets[at], full.neighbors.len());
    }
}

/// The first page of c-00001's briefing in [`ties`] at a budget that cuts it, registered, and
/// its cursor.
fn cut_first(r: &Rdy) -> (Briefing, String) {
    let b = r
        .read()
        .brief(&work(), &cov(&["c-00001"]), Some(150))
        .unwrap();
    b.register(&r.briefs());
    let cursor = b.next.clone().expect("the neighbors do not fit 150 tokens");
    (b, cursor)
}

#[test]
fn nb7_neighbor_changes_stale_the_page_but_not_the_write_token() {
    type Change = fn(&mut Rdy);
    let changes: [(&str, Change); 5] = [
        ("add a neighbor", |r| {
            write(r, "work", json!([{"op": "create", "type": "claim", "parent": "s-0000A", "body": "A new sibling."}]))
                .unwrap();
        }),
        ("remove a neighbor", |r| {
            write(r, "work", json!([{"op": "delete", "id": "c-00003"}])).unwrap();
        }),
        ("edit a neighbor's summary", |r| {
            write(
                r,
                "work",
                json!([{"op": "update", "id": "c-00003", "body": "Stands alone, still."}]),
            )
            .unwrap();
        }),
        ("move a neighbor to another doc", |r| {
            // from section s-0000C of beta to the top of alpha: its tie stays, its doc changes
            write(
                r,
                "work",
                json!([{"op": "move", "id": "c-00022", "parent": "d-0000A"}]),
            )
            .unwrap();
        }),
        ("change a neighbor's route", |r| {
            // c-00022 ties Queue by a reference; a describes link to Ledger adds the least entity
            write(r, "work", json!([{"op": "update", "id": "c-00022", "links": {"describes": [{"target": "en-00001"}]}}]))
                .unwrap();
        }),
    ];
    for (what, change) in changes {
        let (_d, mut r) = project();
        ties(&mut r);
        let (first, cursor) = cut_first(&r);
        change(&mut r);
        let e = r
            .read()
            .brief_page(&work(), &r.briefs(), &cursor, Some(150))
            .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                E_PAGE_STALE,
                "the briefing moved since its first page: brief again"
            ),
            "{what}"
        );
        // the write token records covers, dependencies and governance only: it still holds
        let again = r
            .read()
            .brief(&work(), &cov(&["c-00001"]), Some(150))
            .unwrap();
        assert_eq!(again.base.token, first.base.token, "{what}");
        let w = r
            .write_briefed(
                &writer(),
                WriteRequest {
                    view: "work".into(),
                    ops: parse_test_ops(json!([{"op": "update", "id": "c-00001", "body": "Uses {{en-00001}} and {{en-00002}}, again."}]))
                        .unwrap(),
                    justification: None,
                    message: String::new(),
                },
                Some(first.base.token.clone()),
            )
            .unwrap();
        assert!(
            !w.warnings
                .iter()
                .any(|x| x.code == rdy_engine::brief::W_BRIEF_STALE),
            "{what}: {:?}",
            w.warnings
        );
    }
    // an unchanged neighbor set keeps the page valid: an edit outside it moves nothing
    let (_d, mut r) = project();
    ties(&mut r);
    let (first, cursor) = cut_first(&r);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-00012", "body": "Beta sibling, edited."}]),
    )
    .unwrap();
    let p = r
        .read()
        .brief_page(&work(), &r.briefs(), &cursor, Some(150))
        .unwrap();
    assert_eq!(p.base.token, first.base.token);
    assert_eq!(p.neighbors_by_doc, first.neighbors_by_doc);
}

#[test]
fn nb7_neighbors_do_not_expand_coverage_or_confidence() {
    let (_d, mut r) = project();
    ties(&mut r);
    let c = cov(&["c-00001"]);
    let before = r.read().brief(&work(), &c, Some(1_000_000)).unwrap();
    assert_eq!(before.covers, 1);
    let recorded: BTreeSet<BlockId> = before.record.recorded.keys().cloned().collect();
    assert_eq!(
        recorded,
        BTreeSet::from([id("c-00001"), id("en-00001"), id("en-00002")]),
        "the covered block and its reference targets; no neighbor"
    );
    assert_eq!(before.record.covers, BTreeSet::from([id("c-00001")]));
    let neighbors: BTreeSet<&str> = before.neighbors.iter().map(|n| n.block.as_str()).collect();
    let reasons = serde_json::to_value(&before.coverage).unwrap();
    for n in &neighbors {
        assert!(
            !reasons.to_string().contains(n),
            "{n} is in coverage: {reasons}"
        );
    }
    // a new neighbor with coverage issues of its own changes neither coverage nor confidence
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": "s-0000A", "body": "An untied sibling."}]),
    )
    .unwrap();
    let after = r.read().brief(&work(), &c, Some(1_000_000)).unwrap();
    assert_eq!(after.neighbors.len(), before.neighbors.len() + 1);
    assert_eq!(after.covers, before.covers);
    assert_eq!(after.record.recorded, before.record.recorded);
    assert_eq!(after.base.token, before.base.token);
    assert_eq!(
        serde_json::to_value(&after.coverage).unwrap(),
        serde_json::to_value(&before.coverage).unwrap(),
        "confidence and reasons as before"
    );
}

#[test]
fn nb7_mcp_and_cli_share_neighbor_rows_and_counts() {
    let s = Served::new();
    let target = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$sa", "parent": "$a", "fields": {"title": "S"}},
            {"op": "create", "type": "entity", "id": "$e", "parent": "$sa", "fields": {"name": "Ledger"}},
            {"op": "create", "type": "claim", "id": "$c1", "parent": "$sa", "body": "Uses {{$e}}."},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$sa", "body": "A sibling."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "beta", "title": "Beta", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$sb", "parent": "$b", "fields": {"title": "T"}},
            {"op": "create", "type": "claim", "id": "$c3", "parent": "$sb", "body": "Also uses {{$e}}."}
        ]))
        .unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
        w.created["$c1"].clone()
    };
    let mut c = McpClient::start_proxy(&s);
    let reply = c.call(
        "rdy_session",
        json!({"actor": "agent:nb7", "run": "n1", "role": "writer"}),
    );
    assert!(reply.code().is_none());
    let m = c
        .call("rdy_brief", json!({"ids": [target], "scope": "canon"}))
        .ok();
    drop(c);
    let (ok, cli) = s.json(&["--json", "brief", &target]);
    assert!(ok, "{cli}");
    assert_eq!(m["neighbors"], cli["neighbors"]);
    assert_eq!(m["neighbors_by_doc"], cli["neighbors_by_doc"]);
    assert_eq!(m["base"]["token"], cli["base"]["token"]);
    let rows = m["neighbors"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{m}");
    assert_eq!(rows[0]["via"], "section");
    assert_eq!(rows[1]["via"], "entity");
    // the text: after related decisions, one line per row and per doc count
    let o = s.rdy(&["brief", &target]);
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let line = |r: &Value| {
        let of: Vec<&str> = r["of"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        let through = r["entity"]
            .as_str()
            .map(|e| format!(" through {e}"))
            .unwrap_or_default();
        format!(
            "  {} ({} neighbor of {}{through}; doc {}) {}",
            r["block"].as_str().unwrap(),
            r["via"].as_str().unwrap(),
            of.join(", "),
            r["doc"].as_str().unwrap(),
            r["summary"].as_str().unwrap()
        )
    };
    let counts: Vec<String> = m["neighbors_by_doc"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| format!("  {}: {}", d["doc"].as_str().unwrap(), d["count"]))
        .collect();
    let want = format!(
        "neighbors:\n{}\n{}\nneighbors by doc:\n{}\n",
        line(&rows[0]),
        line(&rows[1]),
        counts.join("\n")
    );
    assert!(text.contains(&want), "{want}\nin\n{text}");
    assert!(
        rows[1]["entity"].is_string() && line(&rows[1]).contains(" through "),
        "an entity row names its entity"
    );
    s.stop();
}

/// A raw section of `n` claims c-00000.. under doc d-0000M, each with a short body.
fn section_of(n: usize) -> Vec<Block> {
    let mut v = vec![
        doc("d-0000M", "limits"),
        blk("s-0000M", "section", "d-0000M", "a1", ""),
    ];
    for i in 0..n {
        v.push(blk(&nid("c", i), "claim", "s-0000M", "a1", "x"));
    }
    v
}

fn refused(r: &Rdy, c: &Coverage) -> RdyError {
    let e = r.read().brief(&work(), c, None).unwrap_err();
    assert_eq!(e.code, E_LIMIT, "{e}");
    e
}

#[test]
fn nb7_neighbor_limit_returns_no_successful_partial_brief() {
    // section: a source with 100,001 section neighbors
    let (_d, mut r) = project();
    raw_commit(&mut r, "work", section_of(100_002));
    let e = refused(&r, &cov(&["c-00000"]));
    assert_eq!(
        (e.message.as_str(), e.fix.as_str()),
        (
            "section neighbors of c-00000 exceed 100000 blocks",
            "split the parent of c-00000 into smaller sections"
        )
    );
    drop((r, _d));

    // entity: an entity with 100,001 tied content blocks, 1,000 to a section
    let (_d, mut r) = project();
    let mut blocks = vec![
        doc("d-0000E", "tied"),
        blk("s-0000E", "section", "d-0000E", "a1", ""),
        entity("en-0000E", "s-0000E", "Gauge"),
    ];
    for i in 0..100_001 {
        let s = nid("sx", i / 1_000);
        if i % 1_000 == 0 {
            blocks.push(blk(&s, "section", "d-0000E", "a2", ""));
        }
        blocks.push(blk(&nid("c", i), "claim", &s, "a1", "{{en-0000E}}"));
    }
    raw_commit(&mut r, "work", blocks);
    let e = refused(&r, &cov(&["c-00000"]));
    assert_eq!(
        (e.message.as_str(), e.fix.as_str()),
        (
            "entity en-0000E exceeds 100000 tied content blocks",
            "split en-0000E into narrower entities"
        )
    );
    drop((r, _d));

    // rows: two sources in two sections, 50,001 section neighbors each
    let (_d, mut r) = project();
    let mut blocks = vec![
        doc("d-0000R", "rows"),
        blk("s-0000R", "section", "d-0000R", "a1", ""),
        blk("s-0000S", "section", "d-0000R", "a2", ""),
    ];
    for i in 0..50_002 {
        blocks.push(blk(&nid("c", i), "claim", "s-0000R", "a1", "x"));
        blocks.push(blk(&nid("k", i), "claim", "s-0000S", "a1", "x"));
    }
    raw_commit(&mut r, "work", blocks);
    let e = refused(&r, &cov(&["c-00000", "k-00000"]));
    assert_eq!(
        (e.message.as_str(), e.fix.as_str()),
        (
            "neighbor result exceeds 100000 rows",
            "brief fewer blocks, or split the change into smaller proposals"
        )
    );
    drop((r, _d));

    // visits: every block of a doc whose one section holds 1,000 claims
    let (_d, mut r) = project();
    raw_commit(&mut r, "work", section_of(1_000));
    let e = refused(
        &r,
        &Coverage {
            ids: vec![],
            path: Some("limits".into()),
        },
    );
    assert_eq!(
        (e.message.as_str(), e.fix.as_str()),
        (
            "neighbor walk exceeds 1000000 visits",
            "brief fewer blocks, or split the change into smaller proposals"
        )
    );
}

#[test]
fn nb7_hundred_thousand_block_briefing_pages_local_neighbors() {
    let (_d, mut r) = project();
    // the local doc: section s-0000M with the entity and claims c-00000..c-00004; c-00000 and
    // c-00001 reference the entity
    let mut blocks = vec![
        doc("d-0000M", "local"),
        blk("s-0000M", "section", "d-0000M", "a1", ""),
        entity("en-0000M", "s-0000M", "Gauge"),
    ];
    for i in 0..5 {
        let body = if i < 2 {
            format!("Claim {i} uses {{{{en-0000M}}}}.")
        } else {
            format!("Claim {i}.")
        };
        blocks.push(blk(
            &nid("c", i),
            "claim",
            "s-0000M",
            &format!("a{i}"),
            &body,
        ));
    }
    // the far doc: 1,000 sections of 98 claims; two claims of section 7 reference the entity
    blocks.push(doc("d-0000F", "far"));
    let mut n = 0;
    for s in 1..=1_000 {
        let sid = nid("sf", s);
        blocks.push(blk(&sid, "section", "d-0000F", "a1", ""));
        for k in 0..98 {
            n += 1;
            let body = if s == 7 && k < 2 {
                "Far claim uses {{en-0000M}}.".to_string()
            } else {
                format!("Far claim {n}.")
            };
            blocks.push(blk(&nid("cf", n), "claim", &sid, "a1", &body));
        }
    }
    // padding to exactly 100,000 blocks
    let mut pad = 0;
    while blocks.len() < 100_000 {
        pad += 1;
        blocks.push(blk(&nid("p", pad), "prose", "d-0000F", "a2", "Padding."));
    }
    raw_commit(&mut r, "work", blocks);
    assert_eq!(r.read().materialized(&work()).unwrap().len(), 100_000);

    let all = pages(&r, &cov(&["c-00000"]), 120);
    assert!(all.len() > 1, "the neighbors page");
    let got: Vec<(String, NeighborVia, Option<String>)> = all
        .iter()
        .flat_map(|p| p.neighbors.iter())
        .map(|n| (n.block.clone(), n.via, n.entity.clone()))
        .collect();
    let s = |b: String| (b, NeighborVia::Section, None);
    let e = |b: String| (b, NeighborVia::Entity, Some("en-0000M".to_string()));
    assert_eq!(
        got,
        vec![
            s(nid("c", 1)),
            s(nid("c", 2)),
            s(nid("c", 3)),
            s(nid("c", 4)),
            e(nid("cf", 589)),
            e(nid("cf", 590)),
        ]
    );
    for p in &all {
        assert_eq!(
            p.neighbors_by_doc,
            vec![count("d-0000F", 2), count("d-0000M", 4)]
        );
    }
}
