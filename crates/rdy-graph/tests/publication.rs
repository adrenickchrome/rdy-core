//! pub2: a selected graph keeps only the selected blocks. `Graph::induced` retains an edge only
//! when both of its endpoints are selected, and drops every content hash, decision status and
//! removal record of a block outside the selection (D-0VCQ5). Graph queries on the result can
//! never observe an excluded block.
use rdy_graph::*;
use rdy_model::redaction::BlockResidue;
use rdy_model::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn ids(list: &[&str]) -> BTreeSet<BlockId> {
    list.iter().map(|s| id(s)).collect()
}

fn b(i: &str, ty: &str, parent: Option<&str>) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: parent.map(id),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn dec(i: &str, parent: &str, status: &str) -> Block {
    let mut d = b(i, "decision", Some(parent));
    d.fields
        .insert("status".into(), FieldValue::Str(status.into()));
    d.fields.insert(
        "statement".into(),
        FieldValue::Str(format!("statement of {i}")),
    );
    // a tracking field: left out of the tracking-sensitive content hash
    d.fields
        .insert("review_by".into(), FieldValue::Str("2027-01-01".into()));
    d
}

fn link(block: &mut Block, name: &str, target: &str, part: Option<&str>) {
    block.links.entry(name.into()).or_default().push(Link {
        target: id(target),
        part: part.map(Into::into),
    });
}

fn tracking() -> Arc<Tracking> {
    Arc::new(Tracking::new(BTreeMap::from([(
        "decision".to_string(),
        BTreeSet::from(["review_by".to_string()]),
    )])))
}

/// Every link kind the fixture uses, each with at least one edge inside the selection and one
/// edge that crosses out of it.
const LINK_KINDS: [&str; 11] = [
    "affects",
    "answers",
    "conditional_on",
    "decided_by",
    "depends_on",
    "describes",
    "implements",
    "refines",
    "revokes",
    "see_also",
    "supersedes",
];

/// The selected blocks of [`fixture`].
const KEEP: [&str; 8] = [
    "d-KEEP1", "s-KEEP1", "D-KEEP1", "D-KEEP2", "D-KEEP3", "c-KEEP1", "q-KEEP1", "t-KEEP1",
];

/// The excluded blocks of [`fixture`].
const DROP: [&str; 6] = [
    "f-R00T0", "d-DR0P1", "s-DR0P1", "D-DR0P1", "D-DR0P2", "c-DR0P1",
];

/// A folder with a selected doc and an excluded doc. Every kind of edge runs both inside the
/// selected doc and across to the excluded one, in both directions.
fn fixture(t: &Arc<Tracking>) -> Vec<Block> {
    let root = b("f-R00T0", "folder", None);
    // a selected root that only excluded blocks point to
    let term = b("t-KEEP1", "term", None);
    let doc = b("d-KEEP1", "doc", Some("f-R00T0"));
    let sec = b("s-KEEP1", "section", Some("d-KEEP1"));
    let question = b("q-KEEP1", "question", Some("s-KEEP1"));
    let other_doc = b("d-DR0P1", "doc", Some("f-R00T0"));
    let other_sec = b("s-DR0P1", "section", Some("d-DR0P1"));

    let mut d1 = dec("D-KEEP1", "s-KEEP1", "accepted");
    link(&mut d1, "answers", "q-KEEP1", None);

    let mut d2 = dec("D-KEEP2", "s-KEEP1", "accepted");
    link(&mut d2, "supersedes", "D-KEEP1", Some("the scope"));
    link(&mut d2, "refines", "D-KEEP1", None);
    link(&mut d2, "refines", "D-DR0P1", None);
    link(&mut d2, "affects", "c-KEEP1", None);

    // proposed: its revocation does not move D-KEEP2's status
    let mut d3 = dec("D-KEEP3", "s-KEEP1", "proposed");
    link(&mut d3, "revokes", "D-KEEP2", None);

    // excluded and accepted: in the source graph it supersedes D-KEEP1 whole
    let mut x1 = dec("D-DR0P1", "s-DR0P1", "accepted");
    link(&mut x1, "supersedes", "D-KEEP1", None);
    link(&mut x1, "answers", "q-KEEP1", None);
    link(&mut x1, "affects", "c-KEEP1", None);

    // excluded and accepted: in the source graph it revokes D-KEEP2
    let mut x2 = dec("D-DR0P2", "s-DR0P1", "accepted");
    link(&mut x2, "revokes", "D-KEEP2", None);

    let mut claim = b("c-KEEP1", "claim", Some("s-KEEP1"));
    claim.body = "Holds while {{D-KEEP1}} and {{D-DR0P1}} do.".into();
    link(&mut claim, "depends_on", "D-KEEP1", None);
    link(&mut claim, "depends_on", "D-DR0P1", None);
    link(&mut claim, "conditional_on", "D-KEEP2", None);
    link(&mut claim, "conditional_on", "D-DR0P1", None);
    link(&mut claim, "decided_by", "D-KEEP2", None);
    link(&mut claim, "decided_by", "D-DR0P1", None);
    link(&mut claim, "implements", "D-KEEP1", None);
    link(&mut claim, "implements", "D-DR0P1", None);
    link(&mut claim, "describes", "s-KEEP1", None);
    link(&mut claim, "describes", "s-DR0P1", None);
    link(&mut claim, "see_also", "D-KEEP3", None);
    link(&mut claim, "see_also", "c-DR0P1", None);
    // a current pin to a selected target, and a stale pin to an excluded one
    claim.pins.insert(id("D-KEEP1"), d1.content_hash_with(t));
    claim
        .pins
        .insert(id("D-DR0P1"), Hash::of(b"a stale pin of D-DR0P1"));

    let mut other = b("c-DR0P1", "claim", Some("s-DR0P1"));
    other.body = "Restates {{c-KEEP1}} with {{t-KEEP1}}.".into();
    link(&mut other, "depends_on", "c-KEEP1", None);
    link(&mut other, "decided_by", "D-KEEP2", None);
    link(&mut other, "describes", "s-KEEP1", None);
    link(&mut other, "see_also", "c-KEEP1", None);

    vec![
        root, term, doc, sec, question, other_doc, other_sec, d1, d2, d3, x1, x2, claim, other,
    ]
}

fn build(blocks: &[Block], t: &Arc<Tracking>) -> Graph {
    let mut sorted: Vec<&Block> = blocks.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    Graph::build_with(sorted, t.clone())
}

/// The outgoing edges of each id in `among`, in id order.
fn all_edges(g: &Graph, among: &BTreeSet<BlockId>) -> Vec<Edge> {
    among
        .iter()
        .flat_map(|i| g.outgoing(i).iter().cloned())
        .collect()
}

#[test]
fn pub2_induced_graph_has_only_selected_endpoints() {
    let t = tracking();
    let blocks = fixture(&t);
    let source = build(&blocks, &t);
    let keep = ids(&KEEP);
    let drop = ids(&DROP);
    let every: BTreeSet<BlockId> = keep.union(&drop).cloned().collect();
    let g = source.induced(&keep);

    // the fixture exercises every kind on both sides of the boundary
    let source_edges = all_edges(&source, &every);
    let crosses = |e: &Edge| !(keep.contains(&e.from) && keep.contains(&e.to));
    let kinds = |pred: &dyn Fn(&Edge) -> bool| -> BTreeSet<String> {
        source_edges
            .iter()
            .filter(|e| pred(e))
            .map(|e| e.kind.name())
            .collect()
    };
    let mut expected: BTreeSet<String> = LINK_KINDS.iter().map(|s| s.to_string()).collect();
    expected.insert("contains".into());
    expected.insert("references".into());
    assert_eq!(kinds(&|e| crosses(e)), expected, "edges crossing out");
    assert_eq!(kinds(&|e| !crosses(e)), expected, "edges inside");

    // every edge of the induced graph has both endpoints selected; every selected edge stays
    let kept = all_edges(&g, &every);
    assert!(kept.iter().all(|e| !crosses(e)), "{kept:?}");
    let inside: Vec<Edge> = source_edges
        .iter()
        .filter(|e| !crosses(e))
        .cloned()
        .collect();
    assert_eq!(kept, inside);
    let kept_kinds: BTreeSet<String> = kept.iter().map(|e| e.kind.name()).collect();
    assert_eq!(kept_kinds, expected);

    // the same graph as the selected blocks' alone, with their outward edges cut: an edge an
    // excluded block makes leaves no trace, not even an empty list
    let selected: Vec<Block> = blocks
        .iter()
        .filter(|b| keep.contains(&b.id))
        .cloned()
        .collect();
    assert_eq!(g, build(&selected, &t).induced(&keep));
    assert_eq!(source.incoming(&id("t-KEEP1")).len(), 1);
    assert!(g.incoming(&id("t-KEEP1")).is_empty());

    // outgoing and incoming lists are each the source's, filtered, and agree with each other
    for i in &keep {
        let filtered =
            |l: &[Edge]| -> Vec<Edge> { l.iter().filter(|e| !crosses(e)).cloned().collect() };
        assert_eq!(
            g.outgoing(i),
            filtered(source.outgoing(i)).as_slice(),
            "out {i}"
        );
        assert_eq!(
            g.incoming(i),
            filtered(source.incoming(i)).as_slice(),
            "in {i}"
        );
        for e in g.outgoing(i) {
            assert_eq!(e.from, *i);
            assert!(g.incoming(&e.to).contains(e), "{e:?} has no incoming twin");
        }
        for e in g.incoming(i) {
            assert_eq!(e.to, *i);
            assert!(
                g.outgoing(&e.from).contains(e),
                "{e:?} has no outgoing twin"
            );
        }
    }
    for i in &drop {
        assert!(g.outgoing(i).is_empty(), "out {i}");
        assert!(g.incoming(i).is_empty(), "in {i}");
    }

    // an excluded decision has no status, and moves no selected decision's status
    assert_eq!(
        source.status(&id("D-KEEP1")),
        Some(DerivedStatus::Superseded {
            by: vec![id("D-DR0P1")]
        })
    );
    assert_eq!(
        g.status(&id("D-KEEP1")),
        Some(DerivedStatus::PartiallySuperseded {
            by: vec![id("D-KEEP2")],
            parts: vec!["the scope".into()]
        })
    );
    assert_eq!(
        source.status(&id("D-KEEP2")),
        Some(DerivedStatus::Revoked {
            by: vec![id("D-DR0P2")]
        })
    );
    assert_eq!(g.status(&id("D-KEEP2")), Some(DerivedStatus::Live));
    assert_eq!(g.status(&id("D-KEEP3")), Some(DerivedStatus::Proposed));
    for i in ["D-DR0P1", "D-DR0P2"] {
        assert!(source.status(&id(i)).is_some());
        assert_eq!(g.status(&id(i)), None, "{i}");
    }

    // the graph queries see only selected blocks
    assert_eq!(
        g.governing(&id("c-KEEP1")),
        vec![id("D-KEEP1"), id("D-KEEP2")]
    );
    assert!(source.governing(&id("c-KEEP1")).contains(&id("D-DR0P1")));
    assert_eq!(g.governed_by(&id("D-KEEP2")), vec![id("c-KEEP1")]);
    assert!(source.governed_by(&id("D-KEEP2")).contains(&id("c-DR0P1")));
    let reached: BTreeSet<BlockId> = g
        .impact_via(&id("D-KEEP1"), 10)
        .into_iter()
        .flat_map(|h| [h.id, h.via.from])
        .collect();
    assert!(reached.is_subset(&keep), "{reached:?}");
    assert!(
        source
            .impact(&id("c-KEEP1"), 10)
            .iter()
            .any(|(_, i)| drop.contains(i)),
        "the source impact crosses out"
    );
    assert_eq!(g.impact(&id("c-KEEP1"), 10), vec![(1, id("D-KEEP2"))]);
    for s in g.suspects() {
        assert!(keep.contains(&s.block) && keep.contains(&s.target), "{s:?}");
    }
    assert!(
        source.suspects().iter().any(|s| s.target == id("D-DR0P1")),
        "the source has a suspect on the excluded decision"
    );
}

#[test]
fn pub2_induced_graph_preserves_order_pins_tracking_and_source() {
    let t = tracking();
    let blocks = fixture(&t);
    let source = build(&blocks, &t);
    let before = source.clone();
    let keep = ids(&KEEP);
    let g = source.induced(&keep);

    // the input is unchanged and the tracking fields are the same value
    assert_eq!(source, before);
    assert!(Arc::ptr_eq(g.tracking(), source.tracking()));

    // edge order, kinds, parts and pins as the source lists them
    let row = |e: &Edge| (e.to.to_string(), e.kind.name(), e.part.clone(), e.pin);
    let pin = blocks
        .iter()
        .find(|b| b.id == id("D-KEEP1"))
        .unwrap()
        .content_hash_with(&t);
    assert_eq!(
        g.outgoing(&id("c-KEEP1"))
            .iter()
            .map(row)
            .collect::<Vec<_>>(),
        vec![
            ("D-KEEP1".into(), "references".into(), None, Some(pin)),
            ("D-KEEP2".into(), "conditional_on".into(), None, None),
            ("D-KEEP2".into(), "decided_by".into(), None, None),
            ("D-KEEP1".into(), "depends_on".into(), None, Some(pin)),
            ("s-KEEP1".into(), "describes".into(), None, None),
            ("D-KEEP1".into(), "implements".into(), None, Some(pin)),
            ("D-KEEP3".into(), "see_also".into(), None, None),
        ]
    );
    let from = |e: &Edge| (e.from.to_string(), e.kind.name(), e.part.clone());
    assert_eq!(
        g.incoming(&id("D-KEEP1"))
            .iter()
            .map(from)
            .collect::<Vec<_>>(),
        vec![
            ("s-KEEP1".into(), "contains".into(), None),
            ("D-KEEP2".into(), "refines".into(), None),
            (
                "D-KEEP2".into(),
                "supersedes".into(),
                Some("the scope".into())
            ),
            ("c-KEEP1".into(), "references".into(), None),
            ("c-KEEP1".into(), "depends_on".into(), None),
            ("c-KEEP1".into(), "implements".into(), None),
        ]
    );

    // the content hashes keep the tracking rule: the current tracking pin is no suspect, and the
    // stale pin to the excluded decision is gone with its edge
    assert_ne!(
        pin,
        blocks
            .iter()
            .find(|b| b.id == id("D-KEEP1"))
            .unwrap()
            .content_hash()
    );
    assert!(
        g.suspects_of(&id("c-KEEP1"))
            .iter()
            .all(|s| s.target != id("D-KEEP1")),
        "{:?}",
        g.suspects_of(&id("c-KEEP1"))
    );
    assert!(
        source
            .suspects_of(&id("c-KEEP1"))
            .iter()
            .any(|s| s.target == id("D-DR0P1"))
    );

    // a selected pin that is stale stays a suspect: the hash is the source's
    let mut stale = blocks.clone();
    let claim = stale.iter_mut().find(|b| b.id == id("c-KEEP1")).unwrap();
    claim.pins.insert(id("D-KEEP1"), Hash::of(b"stale"));
    let stale_g = build(&stale, &t).induced(&keep);
    assert_eq!(
        stale_g.suspects_of(&id("c-KEEP1")),
        vec![Suspect {
            block: id("c-KEEP1"),
            target: id("D-KEEP1"),
            reason: "changed".into()
        }]
    );

    // selecting every block keeps the whole graph; selecting again changes nothing
    let all: BTreeSet<BlockId> = blocks.iter().map(|b| b.id.clone()).collect();
    assert_eq!(source.induced(&all), source);
    assert_eq!(g.induced(&keep), g);
    // the result is an ordinary graph: carrying it across a write equals a rebuild
    let d3 = blocks.iter().find(|b| b.id == id("D-KEEP3")).unwrap();
    let mut d3b = d3.clone();
    d3b.fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    let carried = g.with_changes([d3], [&d3b]);
    assert_eq!(
        carried.status(&id("D-KEEP2")),
        Some(DerivedStatus::Revoked {
            by: vec![id("D-KEEP3")]
        })
    );
    assert_eq!(source, before, "the source still unchanged");
}

#[test]
fn pub2_induced_graph_preserves_selected_redaction_metadata() {
    let t = tracking();
    let blocks = fixture(&t);
    let keep = ids(&KEEP);
    let kept_request = Hash::of(b"removal of D-KEEP1");
    let dropped_request = Hash::of(b"removal of D-DR0P1");
    let removed: Vec<Block> = blocks
        .iter()
        .map(|b| match b.id.as_str() {
            "D-KEEP1" => BlockResidue::of(b).stub(kept_request),
            "D-DR0P1" => BlockResidue::of(b).stub(dropped_request),
            _ => b.clone(),
        })
        .collect();
    let source = build(&removed, &t);
    assert_eq!(
        source.redacted_pins_of(&id("c-KEEP1")),
        vec![
            (id("D-DR0P1"), dropped_request),
            (id("D-KEEP1"), kept_request)
        ]
    );

    let g = source.induced(&keep);
    // the selected removal keeps its request and its guard against a false suspect
    assert_eq!(
        g.redacted_pins_of(&id("c-KEEP1")),
        vec![(id("D-KEEP1"), kept_request)]
    );
    assert!(
        g.suspects_of(&id("c-KEEP1")).is_empty(),
        "{:?}",
        g.suspects()
    );
    for i in &keep {
        for (target, _) in g.redacted_pins_of(i) {
            assert!(keep.contains(&target), "{i} names {target}");
        }
    }

    // the excluded removal is gone: the selected graph is the same whether or not the excluded
    // decision was removed, and whatever its content was
    let only_kept: Vec<Block> = blocks
        .iter()
        .map(|b| match b.id.as_str() {
            "D-KEEP1" => BlockResidue::of(b).stub(kept_request),
            _ => b.clone(),
        })
        .collect();
    let plain = build(&only_kept, &t).induced(&keep);
    assert_eq!(plain, g);
    assert!(g.suspect_candidates(&plain).is_empty());

    // and an unremoved selected decision has no removal record
    let none = build(&blocks, &t).induced(&keep);
    assert!(none.redacted_pins_of(&id("c-KEEP1")).is_empty());
    assert_ne!(none, g);
}

#[test]
fn pub2_induced_graph_handles_empty_and_unknown_sets() {
    let t = tracking();
    let blocks = fixture(&t);
    let source = build(&blocks, &t);
    let before = source.clone();
    let empty = Graph::build_with(std::iter::empty::<&Block>(), t.clone());

    // nothing selected: empty maps, the same tracking
    let none = source.induced(&BTreeSet::new());
    assert_eq!(none, empty);
    assert!(Arc::ptr_eq(none.tracking(), source.tracking()));
    assert!(none.suspects().is_empty());
    for b in &blocks {
        assert!(none.outgoing(&b.id).is_empty());
        assert!(none.incoming(&b.id).is_empty());
        assert_eq!(none.status(&b.id), None);
        assert!(none.redacted_pins_of(&b.id).is_empty());
    }

    // ids the graph does not know create nothing
    let unknown = ids(&["x-ZZZZZ", "D-NEW01"]);
    assert_eq!(source.induced(&unknown), empty);
    let mut mixed = unknown.clone();
    mixed.insert(id("D-KEEP1"));
    let g = source.induced(&mixed);
    assert_eq!(g, source.induced(&ids(&["D-KEEP1"])));
    for i in &unknown {
        assert!(g.outgoing(i).is_empty());
        assert!(g.incoming(i).is_empty());
        assert_eq!(g.status(i), None);
    }
    // one decision alone: no edges, its own status only
    assert!(g.outgoing(&id("D-KEEP1")).is_empty());
    assert!(g.incoming(&id("D-KEEP1")).is_empty());
    assert_eq!(g.status(&id("D-KEEP1")), Some(DerivedStatus::Live));

    // an empty graph selects to an empty graph
    assert_eq!(empty.induced(&ids(&KEEP)), empty);
    assert_eq!(source, before);
}
