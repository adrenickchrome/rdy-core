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
    d.fields.insert("status".into(), FieldValue::Str(status.into()));
    d.fields.insert("statement".into(), FieldValue::Str(format!("statement of {id}")));
    d
}

fn link(block: &mut Block, name: &str, target: &str, part: Option<&str>) {
    block.links.entry(name.into()).or_default().push(Link { target: BlockId::parse(target).unwrap(), part: part.map(Into::into) });
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
    let g = Graph::build(&map(vec![old, partial_old, new, proposed]));
    assert!(matches!(g.status(&BlockId::parse("D-AAAAA").unwrap()), Some(DerivedStatus::Superseded { .. })));
    assert!(matches!(g.status(&BlockId::parse("D-PPPPP").unwrap()), Some(DerivedStatus::PartiallySuperseded { parts, .. }) if parts == vec!["and moves".to_string()]));
    assert!(matches!(g.status(&BlockId::parse("D-BBBBB").unwrap()), Some(DerivedStatus::Live)), "a proposed revocation does not count");
    assert!(matches!(g.status(&BlockId::parse("D-CCCCC").unwrap()), Some(DerivedStatus::Proposed)));
    assert_eq!(g.status(&BlockId::parse("s-SSSSS").unwrap()), None);
}

#[test]
fn edges_impact_and_governing() {
    let term = { let mut t = b("t-MEDGR", "term", Some("s-SSSSS")); t.body = "Balances.".into(); t };
    let mut claim = b("c-CHAMP", "claim", Some("s-SSSSS"));
    claim.body = "On the {{t-MEDGR}}.".into();
    link(&mut claim, "decided_by", "D-AAAAA", None);
    let mut note = b("n-N0TE1", "note", Some("s-SSSSS"));
    note.body = "See {{c-CHAMP}}.".into();
    let mut d = dec("D-AAAAA", "accepted");
    link(&mut d, "affects", "t-MEDGR", None);
    let g = Graph::build(&map(vec![term, claim, note, d, b("s-SSSSS", "section", Some("d-DDDDD"))]));
    let ledger = BlockId::parse("t-MEDGR").unwrap();
    assert_eq!(g.incoming(&ledger).iter().filter(|e| matches!(e.kind, EdgeKind::References)).count(), 1);
    let ids: Vec<(u32, String)> = g.impact(&ledger, 3).into_iter().map(|(d, id)| (d, id.to_string())).collect();
    assert_eq!(ids, vec![(1, "D-AAAAA".into()), (1, "c-CHAMP".into()), (2, "n-N0TE1".into())]);
    assert_eq!(g.governing(&BlockId::parse("c-CHAMP").unwrap()), vec![BlockId::parse("D-AAAAA").unwrap()]);
    assert_eq!(g.governing(&ledger), vec![BlockId::parse("D-AAAAA").unwrap()]);
}

#[test]
fn suspects_ignore_status_only_changes() {
    let proposed = dec("D-AAAAA", "proposed");
    let mut accepted = proposed.clone();
    accepted.fields.insert("status".into(), FieldValue::Str("accepted".into()));
    accepted.fields.insert("decided_at".into(), FieldValue::Str("2026-10-03T00:00:00Z".into()));
    let mut claim = b("c-CHAMP", "claim", Some("s-SSSSS"));
    claim.body = "Per {{D-AAAAA}}.".into();
    claim.pins.insert(proposed.id.clone(), proposed.version_hash());
    let old = proposed.clone();
    let lookup = move |h: &Hash| if *h == old.version_hash() { Some(old.clone()) } else { None };

    let blocks = map(vec![accepted.clone(), claim.clone()]);
    assert!(Graph::build(&blocks).suspects(&blocks, &lookup).is_empty());

    let mut edited = accepted.clone();
    edited.fields.insert("statement".into(), FieldValue::Str("different".into()));
    let blocks = map(vec![edited, claim.clone()]);
    let s = Graph::build(&blocks).suspects(&blocks, &lookup);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].reason, "changed");

    let mut sup = dec("D-BBBBB", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let blocks = map(vec![accepted, claim, sup]);
    let s = Graph::build(&blocks).suspects(&blocks, &lookup);
    assert!(s.iter().any(|x| x.reason == "superseded" && x.block.as_str() == "c-CHAMP"));
}
