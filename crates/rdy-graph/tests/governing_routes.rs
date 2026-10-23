//! rf3: one governing route query (system/graph, D-VPATZ) and its shared inverse. Four routes
//! reach a governing decision: `decided_by`, `implements`, `affects`, and `describes/affects`
//! through an entity. The graph owns the filtering, priority and deduplication; the caller
//! supplies only the defines-trait test.
use rdy_graph::*;
use rdy_model::*;
use std::collections::BTreeMap;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn b(i: &str, ty: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: Some(id("s-SSSSS")),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn dec(i: &str, status: &str) -> Block {
    let mut d = b(i, "decision");
    d.fields
        .insert("status".into(), FieldValue::Str(status.into()));
    d.fields.insert(
        "statement".into(),
        FieldValue::Str(format!("statement of {i}")),
    );
    d
}

fn link(block: &mut Block, name: &str, target: &str) {
    block.links.entry(name.into()).or_default().push(Link {
        target: id(target),
        part: None,
    });
}

fn graph(blocks: &[&Block]) -> Graph {
    let m: BTreeMap<BlockId, &Block> = blocks.iter().map(|b| (b.id.clone(), *b)).collect();
    Graph::build_from(m.into_values())
}

/// The entity ids every test below uses; any other target is not an entity.
fn is_entity(t: &BlockId) -> bool {
    t.as_str().starts_with("e-")
}

fn routes(
    g: &Graph,
    block: &str,
    st: GoverningStatuses,
    cache: &mut GoverningCache,
) -> Vec<(String, &'static str, Option<String>)> {
    g.governing_routes(&id(block), st, is_entity, cache)
        .into_iter()
        .map(|r| {
            (
                r.decision.to_string(),
                r.via,
                r.entity.map(|e| e.to_string()),
            )
        })
        .collect()
}

fn ids(v: Vec<BlockId>) -> Vec<String> {
    v.into_iter().map(|i| i.to_string()).collect()
}

#[test]
fn rf3_four_routes_have_one_priority() {
    let mut x = b("n-XXXXX", "paragraph");
    link(&mut x, "decided_by", "D-AAAAA");
    link(&mut x, "implements", "D-BBBBB");
    link(&mut x, "describes", "e-EEEE2");
    link(&mut x, "describes", "e-EEEE1");
    // D-AAAAA is also reached by affects and an entity; decided_by wins
    let mut a = dec("D-AAAAA", "accepted");
    link(&mut a, "affects", "n-XXXXX");
    link(&mut a, "affects", "e-EEEE1");
    // D-BBBBB: implements wins over an entity route
    let mut bb = dec("D-BBBBB", "accepted");
    link(&mut bb, "affects", "e-EEEE1");
    // D-CCCCC affects the block directly and through an entity; affects wins
    let mut c = dec("D-CCCCC", "accepted");
    link(&mut c, "affects", "n-XXXXX");
    link(&mut c, "affects", "e-EEEE2");
    // D-DDDDD reaches only through two entities; the lowest entity id wins
    let mut d = dec("D-DDDDD", "accepted");
    link(&mut d, "affects", "e-EEEE2");
    link(&mut d, "affects", "e-EEEE1");
    // D-FFFFF is reached by the entity route alone, from the higher entity
    let mut f = dec("D-FFFFF", "accepted");
    link(&mut f, "affects", "e-EEEE2");
    let e1 = b("e-EEEE1", "entity");
    let e2 = b("e-EEEE2", "entity");
    let g = graph(&[&x, &a, &bb, &c, &d, &f, &e1, &e2]);
    let mut cache = GoverningCache::default();
    let got = routes(&g, "n-XXXXX", GoverningStatuses::Live, &mut cache);
    assert_eq!(
        got,
        vec![
            ("D-AAAAA".to_string(), "decided_by", None),
            ("D-BBBBB".to_string(), "implements", None),
            ("D-CCCCC".to_string(), "affects", None),
            (
                "D-DDDDD".to_string(),
                "describes/affects",
                Some("e-EEEE1".to_string())
            ),
            (
                "D-FFFFF".to_string(),
                "describes/affects",
                Some("e-EEEE2".to_string())
            ),
        ]
    );
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(got, sorted, "rows are sorted by decision id");
    // asking again from the same cache gives the same rows
    assert_eq!(
        routes(&g, "n-XXXXX", GoverningStatuses::Live, &mut cache),
        got
    );
}

#[test]
fn rf3_status_filters_and_nonroutes_are_closed() {
    let mut x = b("n-XXXXX", "paragraph");
    let names = [
        ("D-ST001", "accepted"),
        ("D-ST002", "accepted"),
        ("D-ST003", "accepted"),
        ("D-ST004", "accepted"),
        ("D-ST005", "proposed"),
        ("D-ST006", "rejected"),
    ];
    for (n, _) in names {
        link(&mut x, "decided_by", n);
    }
    let blocks: Vec<Block> = names.iter().map(|(n, s)| dec(n, s)).collect();
    let mut superseder = dec("D-ST00A", "accepted");
    link(&mut superseder, "supersedes", "D-ST003");
    let mut partial = dec("D-ST00B", "accepted");
    partial
        .links
        .entry("supersedes".into())
        .or_default()
        .push(Link {
            target: id("D-ST002"),
            part: Some("one part".into()),
        });
    let mut revoker = dec("D-ST00C", "accepted");
    link(&mut revoker, "revokes", "D-ST004");
    // non-routes: a reference, a text mention, an entity's home, a describes target that is no
    // entity, and a status-less target
    let mut mention = dec("D-ST007", "accepted");
    mention.body = "see {{n-XXXXX}}".into();
    x.body = "uses {{D-ST008}}".into();
    let refd = dec("D-ST008", "accepted");
    let mut home = dec("D-ST009", "accepted");
    link(&mut home, "affects", "n-ZZZZZ");
    let mut entity = b("e-EEEE1", "entity");
    entity.parent = Some(id("n-XXXXX"));
    let mut plain = b("n-PPPPP", "paragraph");
    let mut notent = dec("D-NNNNN", "accepted");
    link(&mut notent, "affects", "n-PPPPP");
    link(&mut x, "describes", "n-PPPPP");
    link(&mut plain, "decided_by", "D-NNNNN");
    let statusless = b("n-NSTAT", "paragraph");
    link(&mut x, "implements", "n-NSTAT");
    let mut all: Vec<&Block> = blocks.iter().collect();
    all.extend([
        &x,
        &superseder,
        &partial,
        &revoker,
        &mention,
        &refd,
        &home,
        &entity,
        &plain,
        &notent,
        &statusless,
    ]);
    let g = graph(&all);
    // every DerivedStatus variant is present
    let mut seen = std::collections::BTreeSet::new();
    for (n, _) in names {
        seen.insert(g.status(&id(n)).unwrap().tag());
    }
    assert_eq!(seen.len(), 6, "{seen:?}");
    let mut cache = GoverningCache::default();
    let decisions = |st| -> Vec<String> {
        routes(&g, "n-XXXXX", st, &mut GoverningCache::default())
            .into_iter()
            .map(|r| r.0)
            .collect()
    };
    assert_eq!(
        decisions(GoverningStatuses::Live),
        vec!["D-ST001", "D-ST002"]
    );
    assert_eq!(
        decisions(GoverningStatuses::LiveOrProposed),
        vec!["D-ST001", "D-ST002", "D-ST005"]
    );
    assert_eq!(
        decisions(GoverningStatuses::All),
        vec![
            "D-ST001", "D-ST002", "D-ST003", "D-ST004", "D-ST005", "D-ST006"
        ]
    );
    // the nonroutes never appear, whatever the filter
    for st in [
        GoverningStatuses::Live,
        GoverningStatuses::LiveOrProposed,
        GoverningStatuses::All,
    ] {
        for r in routes(&g, "n-XXXXX", st, &mut cache) {
            assert!(
                !["D-ST007", "D-ST008", "D-ST009", "D-NNNNN"].contains(&r.0.as_str()),
                "{r:?}"
            );
            assert_ne!(r.0, "n-NSTAT");
        }
    }
    // an entity with a home but no describing block adds nothing for the home's block either
    assert!(
        routes(&g, "n-ZZZZZ", GoverningStatuses::All, &mut cache)
            .iter()
            .all(|r| r.1 == "affects")
    );
    // a describes target that is no entity gives no route for `plain`'s describer
    assert!(
        routes(&g, "n-XXXXX", GoverningStatuses::All, &mut cache)
            .iter()
            .all(|r| r.0 != "D-NNNNN")
    );
}

#[test]
fn rf3_entity_affects_is_scanned_once_per_graph_read() {
    let mut e = b("e-EEEE1", "entity");
    e.parent = Some(id("s-SSSSS"));
    let mut d1 = dec("D-AAAAA", "accepted");
    link(&mut d1, "affects", "e-EEEE1");
    let mut d2 = dec("D-BBBBB", "proposed");
    link(&mut d2, "affects", "e-EEEE1");
    let mut blocks = vec![e, d1, d2];
    let n = 25;
    for i in 0..n {
        let mut x = b(&format!("n-X{i:04}"), "paragraph");
        link(&mut x, "describes", "e-EEEE1");
        blocks.push(x);
    }
    let refs: Vec<&Block> = blocks.iter().collect();
    let g = graph(&refs);
    let mut cache = GoverningCache::default();
    assert_eq!(cache.entity_scans(), 0);
    for i in 0..n {
        let r = g.governing_routes(
            &id(&format!("n-X{i:04}")),
            GoverningStatuses::Live,
            is_entity,
            &mut cache,
        );
        assert_eq!(r.len(), 1);
    }
    assert_eq!(cache.entity_scans(), 1, "one scan for one entity");
    // a second status filter reuses the unfiltered candidates
    let r = g.governing_routes(
        &id("n-X0000"),
        GoverningStatuses::LiveOrProposed,
        is_entity,
        &mut cache,
    );
    assert_eq!(r.len(), 2);
    assert_eq!(cache.entity_scans(), 1);
}

#[test]
fn rf3_governed_through_is_the_shared_inverse() {
    let mut x = b("n-XXXXX", "paragraph");
    link(&mut x, "decided_by", "D-AAAAA");
    link(&mut x, "describes", "e-EEEE1");
    link(&mut x, "describes", "e-EEEE2");
    link(&mut x, "describes", "n-PPPPP");
    let mut y = b("n-YYYYY", "paragraph");
    link(&mut y, "implements", "D-AAAAA");
    let mut z = b("n-ZZZZZ", "paragraph");
    link(&mut z, "describes", "e-EEEE1");
    let mut w = b("n-WWWWW", "paragraph");
    link(&mut w, "describes", "n-PPPPP");
    let plain = b("n-PPPPP", "paragraph");
    let e1 = b("e-EEEE1", "entity");
    let e2 = b("e-EEEE2", "entity");
    let mut a = dec("D-AAAAA", "accepted");
    link(&mut a, "affects", "e-EEEE1");
    link(&mut a, "affects", "e-EEEE2");
    link(&mut a, "affects", "n-PPPPP");
    link(&mut a, "affects", "n-QQQQQ");
    let q = b("n-QQQQQ", "paragraph");
    let mut p = dec("D-PRPPP", "proposed");
    link(&mut p, "affects", "e-EEEE1");
    link(&mut p, "affects", "n-WWWWW");
    let g = graph(&[&x, &y, &z, &w, &plain, &e1, &e2, &a, &q, &p]);
    let mut cache = GoverningCache::default();
    // the inverse agrees with governing_routes for every block under every status filter
    let every: Vec<BlockId> = [
        "n-XXXXX", "n-YYYYY", "n-ZZZZZ", "n-WWWWW", "n-PPPPP", "n-QQQQQ",
    ]
    .iter()
    .map(|s| id(s))
    .collect();
    for st in [
        GoverningStatuses::Live,
        GoverningStatuses::LiveOrProposed,
        GoverningStatuses::All,
    ] {
        for d in ["D-AAAAA", "D-PRPPP"] {
            let through = g.governed_through(&id(d), st, is_entity, &mut cache);
            let mut sorted = through.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(through, sorted, "sorted and once each");
            for blk in &every {
                let reached = g
                    .governing_routes(blk, st, is_entity, &mut cache)
                    .iter()
                    .any(|r| r.decision.as_str() == d);
                // a block the decision affects directly is reached by `affects` either way
                assert_eq!(through.contains(blk), reached, "{d} and {blk} under {st:?}");
            }
        }
    }
    // the live accepted decision: direct, implements, affected, and describing blocks, once
    assert_eq!(
        ids(g.governed_through(
            &id("D-AAAAA"),
            GoverningStatuses::Live,
            is_entity,
            &mut cache
        )),
        vec![
            "e-EEEE1", "e-EEEE2", "n-PPPPP", "n-QQQQQ", "n-XXXXX", "n-YYYYY", "n-ZZZZZ"
        ]
    );
    // a proposed decision is excluded by Live and reaches its describers otherwise
    assert!(
        g.governed_through(
            &id("D-PRPPP"),
            GoverningStatuses::Live,
            is_entity,
            &mut cache
        )
        .is_empty()
    );
    assert_eq!(
        ids(g.governed_through(
            &id("D-PRPPP"),
            GoverningStatuses::LiveOrProposed,
            is_entity,
            &mut cache
        )),
        vec!["e-EEEE1", "n-WWWWW", "n-XXXXX", "n-ZZZZZ"]
    );
    // a missing id and a non-decision return nothing
    for none in ["D-MSSNG", "n-XXXXX"] {
        assert!(
            g.governed_through(&id(none), GoverningStatuses::All, is_entity, &mut cache)
                .is_empty()
        );
    }
    // a describes target that is no entity adds nothing: `n-WWWWW` is reached only by D-PRPPP's
    // own direct affects, and `n-PPPPP`'s describers are not reached through `n-PPPPP`
    assert!(
        !g.governed_through(
            &id("D-AAAAA"),
            GoverningStatuses::All,
            is_entity,
            &mut cache
        )
        .contains(&id("n-WWWWW"))
    );
}

#[test]
fn rf3_direct_inverse_does_not_follow_entities() {
    let mut x = b("n-XXXXX", "paragraph");
    link(&mut x, "describes", "e-EEEE1");
    link(&mut x, "decided_by", "D-AAAAA");
    let mut y = b("n-YYYYY", "paragraph");
    link(&mut y, "implements", "D-AAAAA");
    let e1 = b("e-EEEE1", "entity");
    let mut z = b("n-ZZZZZ", "paragraph");
    link(&mut z, "describes", "e-EEEE1");
    let mut a = dec("D-AAAAA", "accepted");
    link(&mut a, "affects", "e-EEEE1");
    link(&mut a, "affects", "n-QQQQQ");
    let q = b("n-QQQQQ", "paragraph");
    let g = graph(&[&x, &y, &z, &e1, &a, &q]);
    assert_eq!(
        ids(g.governed_by(&id("D-AAAAA"))),
        vec!["e-EEEE1", "n-QQQQQ", "n-XXXXX", "n-YYYYY"]
    );
    // the describing-only block is reached through the entity by governed_through alone
    let mut cache = GoverningCache::default();
    assert!(
        g.governed_through(
            &id("D-AAAAA"),
            GoverningStatuses::Live,
            is_entity,
            &mut cache
        )
        .contains(&id("n-ZZZZZ"))
    );
    assert!(!g.governed_by(&id("D-AAAAA")).contains(&id("n-ZZZZZ")));
}
