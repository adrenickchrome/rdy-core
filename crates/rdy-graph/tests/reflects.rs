//! rf3: `reflects` is a suspect and impact edge (D-4WC6R). It joins `SUSPECT_LINKS` and
//! `IMPACT_LINKS`, reuses the existing pin and deleted-target behavior, and is no lineage or
//! answer link.
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

fn graph(blocks: &[&Block]) -> Graph {
    let m: BTreeMap<BlockId, &Block> = blocks.iter().map(|b| (b.id.clone(), *b)).collect();
    Graph::build_from(m.into_values())
}

/// `block` reflects `targets`, each pinned to its current content.
fn reflects(block: &mut Block, targets: &[&Block]) {
    for t in targets {
        block
            .links
            .entry("reflects".into())
            .or_default()
            .push(Link {
                target: t.id.clone(),
                part: None,
            });
        block.pins.insert(t.id.clone(), t.content_hash());
    }
}

fn reasons(g: &Graph) -> Vec<(String, String, String)> {
    g.suspects()
        .into_iter()
        .map(|s| (s.block.to_string(), s.target.to_string(), s.reason))
        .collect()
}

#[test]
fn rf3_reflects_pins_drive_suspects_and_impact() {
    // the extended arrays keep their previous order and append reflects
    assert_eq!(
        SUSPECT_LINKS,
        [
            "depends_on",
            "conditional_on",
            "implements",
            "decided_by",
            "reflects"
        ]
    );
    assert_eq!(
        IMPACT_LINKS,
        [
            "depends_on",
            "conditional_on",
            "implements",
            "decided_by",
            "describes",
            "affects",
            "reflects"
        ]
    );
    let s1 = b("n-SRC11", "source");
    let s2 = b("n-SRC22", "source");
    let mut doc = b("n-DCCCC", "paragraph");
    reflects(&mut doc, &[&s1, &s2]);
    let g = graph(&[&s1, &s2, &doc]);
    assert!(g.suspects().is_empty(), "pinned and unchanged");

    // a changed source makes the reflecting block suspect and reaches it at depth one
    let mut s1c = s1.clone();
    s1c.body = "changed".into();
    let g = graph(&[&s1c, &s2, &doc]);
    assert_eq!(
        reasons(&g),
        vec![("n-DCCCC".into(), "n-SRC11".into(), "changed".into())]
    );
    assert_eq!(g.impact(&id("n-SRC11"), 1), vec![(1, id("n-DCCCC"))]);
    let hit = &g.impact_via(&id("n-SRC11"), 1)[0];
    assert_eq!(hit.via.kind, "reflects");

    // a deleted source is `deleted`
    let g = graph(&[&s1c, &doc]);
    assert_eq!(
        reasons(&g),
        vec![
            ("n-DCCCC".into(), "n-SRC11".into(), "changed".into()),
            ("n-DCCCC".into(), "n-SRC22".into(), "deleted".into())
        ]
    );

    // a repin clears only the changed-target suspect
    let mut repinned = doc.clone();
    repinned.pins.insert(s1c.id.clone(), s1c.content_hash());
    let g = graph(&[&s1c, &repinned]);
    assert_eq!(
        reasons(&g),
        vec![("n-DCCCC".into(), "n-SRC22".into(), "deleted".into())]
    );
}
