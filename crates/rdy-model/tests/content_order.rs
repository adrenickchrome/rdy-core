//! ok2: a block's content hash leaves out its order key (D-TX3KH), so a move within its parent
//! keeps every pin on the block (D-H73DB). The version hash still covers the order key. A
//! redacted stub returns the content hash its residue recorded, whatever encoding made it.
use rdy_model::block::{CONTENT_DOMAIN, DERIVED_FIELDS};
use rdy_model::engine_fields::REAFFIRMED;
use rdy_model::redaction::{BlockResidue, RedactionResidue};
use rdy_model::{Block, BlockId, FieldValue, Hash, Link, Tracking};
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn tracking() -> Tracking {
    Tracking::new(BTreeMap::from([(
        "decision".to_string(),
        BTreeSet::from(["disposition".to_string()]),
    )]))
}

/// A decision with an ordinary field, a tracking field, a derived field, a link and a pin.
fn decision() -> Block {
    Block {
        id: id("D-7KQ2M"),
        block_type: "decision".into(),
        parent: Some(id("s-1DENT")),
        order: "V".into(),
        fields: BTreeMap::from([
            (
                "statement".to_string(),
                FieldValue::Str("Ship weekly".into()),
            ),
            ("status".to_string(), FieldValue::Str("accepted".into())),
            ("disposition".to_string(), FieldValue::Str("M2d1".into())),
        ]),
        body: "Weekly, per {{t-MEDGR}}.".into(),
        links: BTreeMap::from([(
            "affects".to_string(),
            vec![Link {
                target: id("t-MEDGR"),
                part: None,
            }],
        )]),
        pins: BTreeMap::from([(id("t-MEDGR"), Hash::of(b"pinned"))]),
        read_identity: None,
    }
}

#[test]
fn ok2_order_changes_version_but_not_content() {
    let a = decision();
    for key in ["W", "a0", "V0", "Zzzz"] {
        let mut moved = a.clone();
        moved.order = key.into();
        assert_ne!(
            moved.version_hash(),
            a.version_hash(),
            "the version covers the order key {key}"
        );
        assert_eq!(
            moved.content_hash(),
            a.content_hash(),
            "a move to {key} keeps the default content hash"
        );
        assert_eq!(
            moved.content_hash_with(&tracking()),
            a.content_hash_with(&tracking()),
            "a move to {key} keeps the pack tracking content hash"
        );
    }
    // a move and a tracking edit together still leave the tracking content hash unchanged
    let mut both = a.clone();
    both.order = "W".into();
    both.fields
        .insert("disposition".into(), FieldValue::Str("M2d2".into()));
    assert_eq!(
        both.content_hash_with(&tracking()),
        a.content_hash_with(&tracking())
    );
    assert_ne!(both.content_hash(), a.content_hash(), "without tracking");
    assert_eq!(CONTENT_DOMAIN, b"rdy-content-2\0");
}

#[test]
fn ok2_parent_body_and_links_still_change_content() {
    let a = decision();
    let t = tracking();
    let changed: Vec<(&str, Block)> = vec![
        (
            "id",
            Block {
                id: id("D-8KQ2M"),
                ..a.clone()
            },
        ),
        (
            "type",
            Block {
                block_type: "claim".into(),
                ..a.clone()
            },
        ),
        (
            "parent",
            Block {
                parent: Some(id("s-2DENT")),
                ..a.clone()
            },
        ),
        (
            "no parent",
            Block {
                parent: None,
                ..a.clone()
            },
        ),
        (
            "body",
            Block {
                body: "Daily.".into(),
                ..a.clone()
            },
        ),
        ("field", {
            let mut b = a.clone();
            b.fields
                .insert("statement".into(), FieldValue::Str("Ship daily".into()));
            b
        }),
        ("new field", {
            let mut b = a.clone();
            b.fields.insert("note".into(), FieldValue::Int(1));
            b
        }),
        (
            "link",
            Block {
                links: BTreeMap::new(),
                ..a.clone()
            },
        ),
        ("link part", {
            let mut b = a.clone();
            b.links.get_mut("affects").unwrap()[0].part = Some("p".into());
            b
        }),
    ];
    for (what, b) in &changed {
        assert_ne!(b.content_hash(), a.content_hash(), "{what}");
        assert_ne!(b.content_hash_with(&t), a.content_hash_with(&t), "{what}");
        assert_ne!(b.version_hash(), a.version_hash(), "{what}");
    }
    // derived fields, engine fields and pins stay outside the content hash
    let mut same: Vec<(&str, Block)> = vec![];
    for name in DERIVED_FIELDS {
        let mut b = a.clone();
        b.fields
            .insert(name.to_string(), FieldValue::Str("other".into()));
        same.push((name, b));
    }
    let mut meta = a.clone();
    meta.fields.insert(
        REAFFIRMED.into(),
        FieldValue::Map(BTreeMap::from([(
            "t-MEDGR".to_string(),
            FieldValue::Str("kept".into()),
        )])),
    );
    same.push(("engine field", meta));
    let mut pins = a.clone();
    pins.pins.insert(id("t-MEDGR"), Hash::of(b"other"));
    pins.pins.insert(id("c-WXMET"), Hash::of(b"new"));
    same.push(("pins", pins));
    for (what, b) in &same {
        assert_eq!(b.content_hash(), a.content_hash(), "{what}");
        assert_eq!(b.content_hash_with(&t), a.content_hash_with(&t), "{what}");
        assert_ne!(b.version_hash(), a.version_hash(), "{what}");
    }
}

#[test]
fn ok2_redacted_identity_is_not_recomputed() {
    // A residue recorded before content-2: its saved hashes are not hashes of this block under
    // either encoding, and a stub returns them unchanged for every tracking set.
    let saved_version = Hash::of(b"old version");
    let saved_content = Hash::of(b"old content-1 hash");
    let original = decision();
    let residue = RedactionResidue::Block(BlockResidue {
        version: saved_version,
        id: original.id.clone(),
        block_type: original.block_type.clone(),
        parent: original.parent.clone(),
        order: original.order.clone(),
        content_hash: saved_content,
        frozen: true,
        source_encoding: 0,
    });
    let bytes = postcard::to_allocvec(&residue).unwrap();
    let RedactionResidue::Block(back) = postcard::from_bytes::<RedactionResidue>(&bytes).unwrap()
    else {
        panic!("a block residue decodes as one");
    };
    let mut stub = back.stub(Hash::of(b"request"));
    assert!(stub.is_redacted());
    assert_eq!(stub.version_hash(), saved_version);
    assert_eq!(stub.content_hash(), saved_content);
    assert_eq!(stub.content_hash_with(&tracking()), saved_content);
    assert_ne!(stub.content_hash(), original.content_hash());
    // the stub's own order is not read again either
    stub.order = "W".into();
    assert_eq!(stub.version_hash(), saved_version);
    assert_eq!(stub.content_hash(), saved_content);
    // a residue made now records the content-2 hash, and its stub returns that
    let fresh = BlockResidue::of(&original).stub(Hash::of(b"request"));
    assert_eq!(fresh.content_hash(), original.content_hash());
    assert_eq!(fresh.version_hash(), original.version_hash());
}
