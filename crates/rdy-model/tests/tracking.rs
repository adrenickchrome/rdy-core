//! trk1: tracking fields are outside a block's content hash; only_tracking_changed says when an
//! edit of a frozen decision touches tracking fields and nothing else.
use rdy_model::decision::{RULING_FIELDS, only_tracking_changed};
use rdy_model::{Block, BlockId, FieldValue, Hash, Tracking};
use std::collections::{BTreeMap, BTreeSet};

fn tracking() -> Tracking {
    Tracking::new(BTreeMap::from([(
        "decision".to_string(),
        BTreeSet::from(["disposition".to_string(), "trigger".to_string()]),
    )]))
}
fn decision(fields: &[(&str, &str)]) -> Block {
    Block {
        id: BlockId::parse("D-7KQ2M").unwrap(),
        block_type: "decision".into(),
        parent: Some(BlockId::parse("s-1DENT").unwrap()),
        order: "V".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
            .collect(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

#[test]
fn trk1_tracking_fields_are_outside_the_content_hash() {
    let t = tracking();
    let a = decision(&[
        ("statement", "Ship weekly"),
        ("status", "accepted"),
        ("disposition", "M2d1"),
    ]);
    let b = decision(&[
        ("statement", "Ship weekly"),
        ("status", "accepted"),
        ("disposition", "M2d2"),
    ]);
    assert_eq!(
        a.content_hash_with(&t),
        b.content_hash_with(&t),
        "a tracking edit keeps the content hash"
    );
    assert_ne!(
        a.content_hash(),
        b.content_hash(),
        "without tracking the field counts"
    );
    assert_eq!(
        a.content_hash(),
        a.content_hash_with(&Tracking::default()),
        "an empty Tracking is today's hash"
    );
    let c = decision(&[
        ("statement", "Ship daily"),
        ("status", "accepted"),
        ("disposition", "M2d1"),
    ]);
    assert_ne!(
        a.content_hash_with(&t),
        c.content_hash_with(&t),
        "the ruling counts"
    );
    assert!(t.is_tracking("decision", "trigger") && !t.is_tracking("claim", "trigger"));
}

#[test]
fn trk1_only_tracking_changed_needs_a_tracking_difference_and_nothing_else() {
    let t = tracking();
    let old = decision(&[
        ("statement", "Ship weekly"),
        ("status", "accepted"),
        ("kind", "product"),
        ("disposition", "M2d1"),
    ]);
    let moved = decision(&[
        ("statement", "Ship weekly"),
        ("status", "accepted"),
        ("kind", "product"),
        ("disposition", "M2d2"),
    ]);
    assert!(only_tracking_changed(&old, &moved, &t));
    assert!(
        !only_tracking_changed(&old, &old, &t),
        "no tracking difference: not a tracking edit"
    );
    let ruling = decision(&[
        ("statement", "Ship daily"),
        ("status", "accepted"),
        ("kind", "product"),
        ("disposition", "M2d2"),
    ]);
    assert!(
        !only_tracking_changed(&old, &ruling, &t),
        "the statement changed too"
    );
    let other = decision(&[
        ("statement", "Ship weekly"),
        ("status", "accepted"),
        ("kind", "process"),
        ("disposition", "M2d2"),
    ]);
    assert!(
        !only_tracking_changed(&old, &other, &t),
        "a field the pack did not mark stays frozen"
    );
    let mut body = moved.clone();
    body.body = "more".into();
    assert!(!only_tracking_changed(&old, &body, &t));
    let mut linked = moved.clone();
    linked.links.insert("affects".into(), vec![]);
    assert!(
        !only_tracking_changed(&old, &linked, &t),
        "links are the ruling"
    );
    let mut pinned = moved.clone();
    pinned
        .pins
        .insert(BlockId::parse("C-4Q8ZT").unwrap(), Hash::of(b"claim"));
    assert!(
        !only_tracking_changed(&old, &pinned, &t),
        "msw7: pins are the ruling's references"
    );
    assert!(
        !only_tracking_changed(&old, &moved, &Tracking::default()),
        "no tracking fields: nothing is a tracking edit"
    );
    assert_eq!(RULING_FIELDS, ["statement", "options", "status"]);
}
