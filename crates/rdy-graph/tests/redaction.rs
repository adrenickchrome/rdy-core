//! rdx3: the graph keeps removed targets apart from its tracking-sensitive content hashes. A pin
//! to a removed target keeps its value and names the request; it never yields a fabricated
//! `changed` or `deleted` suspect (D-3M81W).
use rdy_graph::*;
use rdy_model::redaction::{BlockResidue, CONTENT_REMOVED};
use rdy_model::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
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

fn dec(i: &str, status: &str) -> Block {
    let mut d = b(i, "decision", Some("s-SSSSS"));
    d.fields
        .insert("status".into(), FieldValue::Str(status.into()));
    d.fields.insert(
        "statement".into(),
        FieldValue::Str(format!("the removed statement of {i}")),
    );
    // a tracking field: left out of the tracking-sensitive content hash
    d.fields
        .insert("review_by".into(), FieldValue::Str("2027-01-01".into()));
    d
}

/// A claim that depends on `target` and cites it in its body, both pinned with `pin`.
fn premise(i: &str, target: &str, pin: Hash) -> Block {
    let mut c = b(i, "claim", Some("s-SSSSS"));
    c.body = format!("This holds while {{{{{target}}}}} does.");
    c.links.entry("depends_on".into()).or_default().push(Link {
        target: id(target),
        part: None,
    });
    c.pins.insert(id(target), pin);
    c
}

fn tracking() -> Arc<Tracking> {
    Arc::new(Tracking::new(BTreeMap::from([(
        "decision".to_string(),
        BTreeSet::from(["review_by".to_string()]),
    )])))
}

fn reasons(g: &Graph, of: &str) -> Vec<String> {
    g.suspects_of(&id(of))
        .into_iter()
        .map(|s| s.reason)
        .collect()
}

#[test]
fn rdx3_tracking_pins_name_removed_targets_without_a_false_hash() {
    let t = tracking();
    let request = Hash::of(b"the removal request");
    let target = dec("D-TARG1", "accepted");
    let pin = target.content_hash_with(&t);
    assert_ne!(
        pin,
        target.content_hash(),
        "the tracking hash differs from the default content hash"
    );
    let stub = BlockResidue::of(&target).stub(request);
    assert_eq!(stub.body, CONTENT_REMOVED);
    let held = premise("c-HE1D1", "D-TARG1", pin);
    let other = premise("c-0THER", "D-0THER", Hash::of(b"stale pin"));

    // before the removal the pin matches: no suspect
    let before = Graph::build_with([&target, &held, &other], t.clone());
    assert!(reasons(&before, "c-HE1D1").is_empty());
    assert!(before.redacted_pins_of(&id("c-HE1D1")).is_empty());

    // the removed target: the surviving pins keep their values, the request is named, and no
    // false suspect comes from the stub's recorded (non-tracking) content hash
    let g = Graph::build_with([&stub, &held, &other], t.clone());
    let pins: Vec<Option<Hash>> = g
        .outgoing(&id("c-HE1D1"))
        .iter()
        .filter(|e| e.to == id("D-TARG1"))
        .map(|e| e.pin)
        .collect();
    assert_eq!(pins, vec![Some(pin), Some(pin)], "a reference and a link");
    assert!(reasons(&g, "c-HE1D1").is_empty(), "{:?}", g.suspects());
    assert_eq!(
        g.redacted_pins_of(&id("c-HE1D1")),
        vec![(id("D-TARG1"), request)]
    );
    // an unrelated target that is simply gone is still `deleted`
    assert_eq!(reasons(&g, "c-0THER"), vec!["deleted".to_string()]);
    assert!(g.redacted_pins_of(&id("c-0THER")).is_empty());
    assert!(
        g.suspects().iter().all(|s| s.target != id("D-TARG1")),
        "{:?}",
        g.suspects()
    );

    // the graph carried across the removal equals a rebuild, and the request map follows it
    let carried = before.with_changes([&target], [&stub]);
    assert_eq!(carried, g);
    assert!(carried.suspect_candidates(&before).contains(&id("c-HE1D1")));
    let restored = carried.with_changes([&stub], [&target]);
    assert_eq!(restored, before);
    assert!(restored.redacted_pins_of(&id("c-HE1D1")).is_empty());

    // a new, non-erased version of the target is checked against the pin again
    let mut next = dec("D-TARG1", "accepted");
    next.fields.insert(
        "statement".into(),
        FieldValue::Str("a new statement".into()),
    );
    let after = carried.with_changes([&stub], [&next]);
    assert_eq!(reasons(&after, "c-HE1D1"), vec!["changed".to_string()]);
    assert!(after.redacted_pins_of(&id("c-HE1D1")).is_empty());
    assert_eq!(after, Graph::build_with([&next, &held, &other], t.clone()));

    // several pinned edges to removed targets: one row per target, sorted by target id
    let second = BlockResidue::of(&dec("D-AAAAA", "accepted")).stub(Hash::of(b"another request"));
    let mut two = premise("c-TW0TW", "D-TARG1", pin);
    two.links
        .entry("depends_on".into())
        .or_default()
        .push(Link {
            target: id("D-AAAAA"),
            part: None,
        });
    two.pins.insert(id("D-AAAAA"), Hash::of(b"pin of D-AAAAA"));
    let unpinned = {
        let mut c = b("c-NP1NP", "claim", Some("s-SSSSS"));
        c.body = "Cites {{D-TARG1}} with no pin.".into();
        c
    };
    let g2 = Graph::build_with([&stub, &second, &two, &unpinned], t);
    assert_eq!(
        g2.redacted_pins_of(&id("c-TW0TW")),
        vec![
            (id("D-AAAAA"), Hash::of(b"another request")),
            (id("D-TARG1"), request)
        ]
    );
    assert!(g2.redacted_pins_of(&id("c-NP1NP")).is_empty());
    assert!(reasons(&g2, "c-TW0TW").is_empty());

    // same_content compares recorded versions when either block is a projection
    assert!(same_content(&stub, &stub.clone()));
    assert!(
        same_content(&stub, &target),
        "the stub is the projection of that version"
    );
    assert!(!same_content(&stub, &next), "a later version differs");
    assert!(!same_content(&next, &stub));
    let mut other_stub = BlockResidue::of(&next).stub(request);
    assert!(!same_content(&stub, &other_stub));
    other_stub.read_identity.as_mut().unwrap().version = stub.version_hash();
    assert!(
        same_content(&stub, &other_stub),
        "only the recorded versions are compared"
    );
    let mut repinned = target.clone();
    repinned.pins.insert(id("c-HE1D1"), Hash::of(b"x"));
    assert!(
        same_content(&target, &repinned),
        "normal blocks keep the rule"
    );
}
