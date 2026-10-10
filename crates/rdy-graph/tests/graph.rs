use rdy_graph::*;
use rdy_model::*;
use std::collections::BTreeMap;

fn b(id: &str, ty: &str, parent: Option<&str>) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn dec(id: &str, status: &str) -> Block {
    let mut d = b(id, "decision", Some("s-SSSSS"));
    d.fields
        .insert("status".into(), FieldValue::Str(status.into()));
    d.fields.insert(
        "statement".into(),
        FieldValue::Str(format!("statement of {id}")),
    );
    d
}

fn link(block: &mut Block, name: &str, target: &str, part: Option<&str>) {
    block.links.entry(name.into()).or_default().push(Link {
        target: BlockId::parse(target).unwrap(),
        part: part.map(Into::into),
    });
}

fn map(blocks: Vec<Block>) -> BTreeMap<BlockId, Block> {
    blocks.into_iter().map(|b| (b.id.clone(), b)).collect()
}

#[test]
fn derived_decision_status() {
    let old = dec("D-AAAAA", "accepted");
    let partial_old = dec("D-PPPPP", "accepted");
    let mut new = dec("D-BBBBB", "accepted");
    link(&mut new, "supersedes", "D-AAAAA", None);
    link(&mut new, "supersedes", "D-PPPPP", Some("and moves"));
    let mut proposed = dec("D-CCCCC", "proposed");
    link(&mut proposed, "revokes", "D-BBBBB", None);
    let g = Graph::build_from(map(vec![old, partial_old, new, proposed]).values());
    assert!(matches!(
        g.status(&BlockId::parse("D-AAAAA").unwrap()),
        Some(DerivedStatus::Superseded { .. })
    ));
    assert!(
        matches!(g.status(&BlockId::parse("D-PPPPP").unwrap()), Some(DerivedStatus::PartiallySuperseded { parts, .. }) if parts == vec!["and moves".to_string()])
    );
    assert!(
        matches!(
            g.status(&BlockId::parse("D-BBBBB").unwrap()),
            Some(DerivedStatus::Live)
        ),
        "a proposed revocation does not count"
    );
    assert!(matches!(
        g.status(&BlockId::parse("D-CCCCC").unwrap()),
        Some(DerivedStatus::Proposed)
    ));
    assert_eq!(g.status(&BlockId::parse("s-SSSSS").unwrap()), None);
}

#[test]
fn edges_impact_and_governing() {
    let term = {
        let mut t = b("t-MEDGR", "term", Some("s-SSSSS"));
        t.body = "Balances.".into();
        t
    };
    let mut claim = b("c-CHAMP", "claim", Some("s-SSSSS"));
    claim.body = "On the {{t-MEDGR}}.".into();
    link(&mut claim, "decided_by", "D-AAAAA", None);
    let mut note = b("n-N0TE1", "note", Some("s-SSSSS"));
    note.body = "See {{c-CHAMP}}.".into();
    let mut d = dec("D-AAAAA", "accepted");
    link(&mut d, "affects", "t-MEDGR", None);
    let g = Graph::build_from(
        map(vec![
            term,
            claim,
            note,
            d,
            b("s-SSSSS", "section", Some("d-DDDDD")),
        ])
        .values(),
    );
    let ledger = BlockId::parse("t-MEDGR").unwrap();
    assert_eq!(
        g.incoming(&ledger)
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::References))
            .count(),
        1
    );
    let ids: Vec<(u32, String)> = g
        .impact(&ledger, 3)
        .into_iter()
        .map(|(d, id)| (d, id.to_string()))
        .collect();
    assert_eq!(
        ids,
        vec![
            (1, "D-AAAAA".into()),
            (1, "c-CHAMP".into()),
            (2, "n-N0TE1".into())
        ]
    );
    assert_eq!(
        g.governing(&BlockId::parse("c-CHAMP").unwrap()),
        vec![BlockId::parse("D-AAAAA").unwrap()]
    );
    assert_eq!(
        g.governing(&ledger),
        vec![BlockId::parse("D-AAAAA").unwrap()]
    );
}

#[test]
fn suspects_ignore_status_only_changes() {
    let proposed = dec("D-AAAAA", "proposed");
    let mut accepted = proposed.clone();
    accepted
        .fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    accepted.fields.insert(
        "decided_at".into(),
        FieldValue::Str("2026-10-03T00:00:00Z".into()),
    );
    let mut claim = b("c-CHAMP", "claim", Some("s-SSSSS"));
    claim.body = "Per {{D-AAAAA}}.".into();
    claim
        .pins
        .insert(proposed.id.clone(), proposed.content_hash());

    let blocks = map(vec![accepted.clone(), claim.clone()]);
    assert!(Graph::build_from(blocks.values()).suspects().is_empty());

    let mut edited = accepted.clone();
    edited
        .fields
        .insert("statement".into(), FieldValue::Str("different".into()));
    let blocks = map(vec![edited, claim.clone()]);
    let s = Graph::build_from(blocks.values()).suspects();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].reason, "changed");

    let mut sup = dec("D-BBBBB", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let blocks = map(vec![accepted, claim, sup]);
    let s = Graph::build_from(blocks.values()).suspects();
    assert!(
        s.iter()
            .any(|x| x.reason == "superseded" && x.block.as_str() == "c-CHAMP")
    );
}

fn claim_pinned(id: &str, target: &Block) -> Block {
    let mut c = b(id, "claim", Some("s-SSSSS"));
    c.body = format!("Per {{{{{}}}}}.", target.id);
    c.pins.insert(target.id.clone(), target.content_hash());
    c
}

#[test]
fn accepted_revoker_makes_referrers_suspect_proposed_does_not() {
    let target = dec("D-AAAAA", "accepted");
    let claim = claim_pinned("c-CHAMP", &target);
    let mut revoker = dec("D-BBBBB", "accepted");
    link(&mut revoker, "revokes", "D-AAAAA", None);
    let blocks = map(vec![target.clone(), claim.clone(), revoker]);
    let g = Graph::build_from(blocks.values());
    assert!(matches!(
        g.status(&target.id),
        Some(DerivedStatus::Revoked { .. })
    ));
    let s = g.suspects();
    assert!(
        s.iter()
            .any(|x| x.reason == "revoked" && x.block == claim.id)
    );

    let mut proposed = dec("D-BBBBB", "proposed");
    link(&mut proposed, "revokes", "D-AAAAA", None);
    let blocks = map(vec![target, claim, proposed]);
    let g = Graph::build_from(blocks.values());
    assert!(g.suspects().is_empty());
}

#[test]
fn pinned_target_absent_is_deleted() {
    let gone = dec("D-AAAAA", "accepted");
    let claim = claim_pinned("c-CHAMP", &gone);
    let blocks = map(vec![claim]);
    let s = Graph::build_from(blocks.values()).suspects();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].reason, "deleted");
}

#[test]
fn partial_supersede_is_not_suspect_and_superseder_is_not_suspect() {
    let target = dec("D-AAAAA", "accepted");
    let claim = claim_pinned("c-CHAMP", &target);
    let mut sup = dec("D-BBBBB", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", Some("the moves part"));
    let blocks = map(vec![target.clone(), claim, sup.clone()]);
    let g = Graph::build_from(blocks.values());
    assert!(matches!(
        g.status(&target.id),
        Some(DerivedStatus::PartiallySuperseded { .. })
    ));
    assert!(g.suspects().is_empty());

    let mut whole = dec("D-BBBBB", "accepted");
    link(&mut whole, "supersedes", "D-AAAAA", None);
    let blocks = map(vec![target, whole.clone()]);
    let s = Graph::build_from(blocks.values()).suspects();
    assert!(s.iter().all(|x| x.block != whole.id));
}

#[test]
fn decision_naming_the_old_id_it_supersedes_is_not_suspect() {
    let target = dec("D-AAAAA", "accepted");
    let mut sup = dec("D-BBBBB", "accepted");
    sup.fields.insert(
        "statement".into(),
        FieldValue::Str("replaces {{D-AAAAA}}".into()),
    );
    link(&mut sup, "supersedes", "D-AAAAA", None);
    sup.pins.insert(target.id.clone(), target.content_hash());
    let blocks = map(vec![target, sup.clone()]);
    let g = Graph::build_from(blocks.values());
    assert!(
        g.outgoing(&sup.id)
            .iter()
            .any(|e| e.kind == EdgeKind::References),
        "the reference edge exists"
    );
    assert!(g.suspects().iter().all(|x| x.block != sup.id));
}

#[test]
fn impact_depth_cutoff_and_diamond() {
    let t = b("t-MEDGR", "term", Some("s-SSSSS"));
    let mut x = b("c-XXXXX", "claim", Some("s-SSSSS"));
    x.body = "{{t-MEDGR}}".into();
    let mut y = b("c-YYYYY", "claim", Some("s-SSSSS"));
    y.body = "{{t-MEDGR}}".into();
    let mut z = b("n-ZZZZZ", "note", Some("s-SSSSS"));
    z.body = "{{c-XXXXX}} {{c-YYYYY}}".into();
    let g = Graph::build_from(map(vec![t, x, y, z]).values());
    let id = BlockId::parse("t-MEDGR").unwrap();
    let d1: Vec<_> = g
        .impact(&id, 1)
        .into_iter()
        .map(|(d, i)| (d, i.to_string()))
        .collect();
    assert_eq!(d1, vec![(1, "c-XXXXX".into()), (1, "c-YYYYY".into())]);
    let d3: Vec<_> = g
        .impact(&id, 3)
        .into_iter()
        .map(|(d, i)| (d, i.to_string()))
        .collect();
    assert_eq!(
        d3,
        vec![
            (1, "c-XXXXX".into()),
            (1, "c-YYYYY".into()),
            (2, "n-ZZZZZ".into())
        ]
    );
}

#[test]
fn suspects_of_matches_the_whole_scope_suspects_filtered_to_one_block() {
    let target = dec("D-AAAAA", "accepted");
    let claim = claim_pinned("c-CHAMP", &target);
    let other = claim_pinned("c-THREE", &target);
    let mut revoker = dec("D-BBBBB", "accepted");
    link(&mut revoker, "revokes", "D-AAAAA", None);
    let blocks = map(vec![target, claim.clone(), other.clone(), revoker.clone()]);
    let g = Graph::build_from(blocks.values());
    for b in blocks.values() {
        let expect: Vec<_> = g
            .suspects()
            .into_iter()
            .filter(|s| s.block == b.id)
            .collect();
        assert_eq!(g.suspects_of(&b.id), expect, "{}", b.id);
    }
    assert_eq!(g.suspects_of(&claim.id).len(), 1);
    assert!(g.suspects_of(&revoker.id).is_empty());
}

/// Fix round 1: a stray self-pin never surfaces as a suspect, whatever its value.
#[test]
fn a_self_pin_is_never_a_suspect() {
    let mut c = b("c-AAAAA", "claim", Some("s-SSSSS"));
    c.body = "This claim, {{c-AAAAA}}, holds.".into();
    c.pins.insert(c.id.clone(), Hash([7; 32]));
    let blocks = map(vec![c.clone()]);
    let g = Graph::build_from(blocks.values());
    assert!(g.suspects().is_empty(), "{:?}", g.suspects());
    assert!(g.suspects_of(&c.id).is_empty());
}

#[test]
fn cgr1_coord_edges_are_kept_once_and_indexed_both_ways() {
    use rdy_graph::coord::{Coord, CoordKind, Node};
    use rdy_model::{BlockId, Hash};
    let b = Node::Block(BlockId::parse("c-AAAAA").unwrap());
    let c = Node::Change(Hash([1; 32]));
    let mut g = Coord::default();
    g.add(b.clone(), CoordKind::AuthoredIn, c.clone());
    g.add(b.clone(), CoordKind::AuthoredIn, c.clone());
    g.add(
        c.clone(),
        CoordKind::MadeBy,
        Node::Run("agent:a".into(), "r".into()),
    );
    assert_eq!(g.edge_count(), 2);
    assert_eq!(g.outgoing(&b).len(), 1);
    assert_eq!(g.incoming(&c)[0].from, b);
    assert_eq!(g.outgoing(&c)[0].kind, CoordKind::MadeBy);
    assert!(g.incoming(&Node::Area("x".into())).is_empty());
    // the edge as stored (its JSON form is pinned in rdy-qa's engine_coord, final-review fix
    // round 1: rdy-graph has no JSON dependency)
    assert_eq!(g.outgoing(&b)[0].kind, CoordKind::AuthoredIn);
    assert_eq!(g.outgoing(&b)[0].from, b);
    assert_eq!(g.outgoing(&b)[0].to, c);
}

/// mfb2: claim A decided_by D; claim B implements D; D affects C; claim E references D.
fn governed_fixture() -> (Graph, BlockId, BlockId, BlockId, BlockId, BlockId) {
    let mut a = b("c-AAAAA", "claim", Some("s-SSSSS"));
    link(&mut a, "decided_by", "D-DDDDD", None);
    let mut bb = b("c-BBBBB", "claim", Some("s-SSSSS"));
    link(&mut bb, "implements", "D-DDDDD", None);
    let c = b("c-CCCCC", "claim", Some("s-SSSSS"));
    let mut d = dec("D-DDDDD", "accepted");
    link(&mut d, "affects", "c-CCCCC", None);
    let mut e = b("c-EEEEE", "claim", Some("s-SSSSS"));
    e.body = "Per {{D-DDDDD}}.".into();
    let g = Graph::build_from(
        map(vec![
            a,
            bb,
            c,
            d,
            e,
            b("s-SSSSS", "section", Some("d-DDDDD")),
        ])
        .values(),
    );
    let id = |s: &str| BlockId::parse(s).unwrap();
    (
        g,
        id("c-AAAAA"),
        id("c-BBBBB"),
        id("c-CCCCC"),
        id("D-DDDDD"),
        id("c-EEEEE"),
    )
}

#[test]
fn mfb2_governed_by_lists_decided_by_implements_and_affects_once() {
    // build, with the file's helper: claim A decided_by D; claim B implements D; D affects C;
    // claim E references D (not a governing link)
    let (g, a, b, c, d, e) = governed_fixture();
    assert_eq!(g.governed_by(&d), {
        let mut v = vec![a, b, c];
        v.sort();
        v
    });
    assert!(!g.governed_by(&d).contains(&e));
}
