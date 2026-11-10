//! rpl2: the optional foreign part of a block. An empty part keeps every local byte and hash; a
//! foreign block has its own explicit wire layouts and content domain; neither the tracking nor
//! the re-affirmation exception can change foreign links or pins.
use rdy_model::block::{ADOPTED_AS, CONTENT_DOMAIN, FOREIGN_CONTENT_DOMAIN};
use rdy_model::decision::{only_reaffirmation_changed, only_tracking_changed};
use rdy_model::encoding::{
    BLOCK_FOREIGN_ENCODING, CHANGE_FOREIGN_ENCODING, EncodedIdentity, ForeignBlockV1,
    ForeignChangeV2, LegacyBlockV0, LegacyChangeLinksV0, LegacyChangeV0, encoded_hash,
};
use rdy_model::engine_fields::{REAFFIRMED, REFLECTS_AT};
use rdy_model::foreign::{ForeignId, ForeignLink, ForeignParts, ForeignPin};
use rdy_model::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn local() -> Block {
    let mut fields = BTreeMap::new();
    fields.insert("a_null".into(), FieldValue::Null);
    fields.insert("b_bool".into(), FieldValue::Bool(true));
    fields.insert("c_int".into(), FieldValue::Int(-42));
    fields.insert("d_str".into(), FieldValue::Str("Ünïcødé".into()));
    fields.insert(
        "e_list".into(),
        FieldValue::List(vec![FieldValue::Int(1), FieldValue::Str("x".into())]),
    );
    fields.insert(
        "f_map".into(),
        FieldValue::Map(BTreeMap::from([("k".to_string(), FieldValue::Bool(false))])),
    );
    Block {
        id: bid("c-7KQ2M"),
        block_type: "claim".into(),
        parent: Some(bid("s-1DENT")),
        order: "V".into(),
        fields,
        body: "Balances live on the {{t-MEDGR}}.".into(),
        links: BTreeMap::from([(
            "describes".to_string(),
            vec![Link {
                target: bid("t-MEDGR"),
                part: Some("balance".into()),
            }],
        )]),
        pins: BTreeMap::from([(bid("t-MEDGR"), Hash::of(b"pinned"))]),
        foreign: ForeignParts::default(),
        read_identity: None,
    }
}

fn repo() -> RepoId {
    RepoId([7; 16])
}

fn parts() -> ForeignParts {
    let target = ForeignId::new(repo(), bid("D-4W1ZR"));
    ForeignParts {
        links: BTreeMap::from([(
            "depends_on".to_string(),
            vec![ForeignLink {
                target: target.clone(),
                part: None,
            }],
        )]),
        pins: BTreeMap::from([(
            target,
            ForeignPin {
                content: Hash::of(b"cited content"),
                encoding: 0,
            },
        )]),
    }
}

fn foreign() -> Block {
    Block {
        foreign: parts(),
        ..local()
    }
}

/// The released encoding-0 layout of a block, written out field by field.
#[derive(Serialize)]
#[serde(rename = "Block")]
struct LocalLayout<'a> {
    id: &'a BlockId,
    block_type: &'a String,
    parent: &'a Option<BlockId>,
    order: &'a String,
    fields: &'a BTreeMap<String, FieldValue>,
    body: &'a String,
    links: &'a BTreeMap<String, Vec<Link>>,
    pins: &'a BTreeMap<BlockId, Hash>,
}

fn layout(b: &Block) -> LocalLayout<'_> {
    LocalLayout {
        id: &b.id,
        block_type: &b.block_type,
        parent: &b.parent,
        order: &b.order,
        fields: &b.fields,
        body: &b.body,
        links: &b.links,
        pins: &b.pins,
    }
}

/// The content of a block as content-2 records it, plus the foreign links when given.
fn content_bytes(b: &Block, domain: &[u8], foreign_links: bool) -> Hash {
    #[derive(Serialize)]
    struct Content<'a> {
        id: &'a BlockId,
        block_type: &'a String,
        parent: &'a Option<BlockId>,
        fields: Vec<(&'a String, &'a FieldValue)>,
        body: &'a String,
        links: &'a BTreeMap<String, Vec<Link>>,
    }
    let content = Content {
        id: &b.id,
        block_type: &b.block_type,
        parent: &b.parent,
        fields: b
            .fields
            .iter()
            .filter(|(k, _)| !["status", "decided_at", "lifecycle"].contains(&k.as_str()))
            .filter(|(k, _)| ![REAFFIRMED, REFLECTS_AT, ADOPTED_AS].contains(&k.as_str()))
            .collect(),
        body: &b.body,
        links: &b.links,
    };
    let mut bytes = domain.to_vec();
    bytes.extend(postcard::to_allocvec(&content).unwrap());
    if foreign_links {
        bytes.extend(postcard::to_allocvec(&b.foreign.links).unwrap());
    }
    Hash::of(&bytes)
}

fn provenance() -> Provenance {
    Provenance {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Approver,
        at_ms: 1_759_449_600_000,
    }
}

fn change(b: Block) -> Change {
    Change {
        view: "work".into(),
        ops: vec![
            Op::Create { block: b.clone() },
            Op::Update {
                base: Hash::of(b"base"),
                block: b.clone(),
            },
            Op::Delete {
                base: Hash::of(b"base"),
                id: b.id.clone(),
            },
        ],
        deps: vec![Hash::of(b"dep")],
        provenance: provenance(),
        justification: None,
        message: "legacy".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

#[test]
fn rpl2_empty_foreign_part_keeps_bytes_and_hashes() {
    let b = local();
    assert!(b.foreign.is_empty());
    assert_eq!(b.encoding(), 0);
    // the stored bytes are the released layout, field for field
    let bytes = postcard::to_allocvec(&b).unwrap();
    assert_eq!(bytes, postcard::to_allocvec(&layout(&b)).unwrap());
    assert_eq!(
        bytes,
        postcard::to_allocvec(&LegacyBlockV0::from_current(&b).unwrap()).unwrap()
    );
    // the golden version hash (tests/golden.rs) and its encoding-0 identity
    assert_eq!(
        b.version_hash().to_string(),
        "3900f27b4ae5bff77eb9b5e6313e5f30b77df8fb3274ce3d56e1b8074356d71e"
    );
    assert_eq!(b.version_hash(), encoded_hash(0, &bytes));
    assert_eq!(b.version_hash(), hash_of(&b));
    // the content-2 hash, unchanged
    assert_eq!(b.content_hash(), content_bytes(&b, CONTENT_DOMAIN, false));
    // a read gets the empty part back
    let read: Block = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(read, b);
    assert!(read.foreign.is_empty());
    // JSON keeps its released shape: no foreign key
    let json = serde_json::to_value(&b).unwrap();
    assert_eq!(json, serde_json::to_value(layout(&b)).unwrap());
    assert!(json.get("foreign").is_none());
    assert_eq!(serde_json::from_value::<Block>(json).unwrap(), b);
    // a change that embeds it keeps its encoding-1 id
    let c = change(b.clone());
    let body = postcard::to_allocvec(&c).unwrap();
    assert_eq!(c.id(), encoded_hash(1, &body));
    assert_eq!(postcard::from_bytes::<Change>(&body).unwrap(), c);
    // a redacted projection keeps the recorded identity and source encoding 0
    let residue = BlockResidue::of(&b);
    assert_eq!(residue.source_encoding, 0);
    let stub = residue.stub(Hash::of(b"request"));
    assert_eq!(stub.version_hash(), b.version_hash());
    assert_eq!(stub.content_hash(), b.content_hash());
    assert!(stub.foreign.is_empty());
}

#[test]
fn rpl2_foreign_content_excludes_pins_and_engine_fields() {
    let b = foreign();
    assert!(!b.foreign.is_empty());
    assert_eq!(b.encoding(), BLOCK_FOREIGN_ENCODING);
    // the foreign content domain covers the local content and the foreign links
    let content = b.content_hash();
    assert_eq!(content, content_bytes(&b, FOREIGN_CONTENT_DOMAIN, true));
    assert_ne!(content, local().content_hash(), "a new domain");
    assert_ne!(FOREIGN_CONTENT_DOMAIN, CONTENT_DOMAIN);
    assert_eq!(FOREIGN_CONTENT_DOMAIN, b"rdy-content-foreign-1\0");

    // pins, derived, engine, tracking fields, order and adopted_as stay out of it
    let mut same = vec![];
    let mut x = b.clone();
    for pin in x.foreign.pins.values_mut() {
        pin.content = Hash::of(b"another version");
        pin.encoding = 1;
    }
    same.push(("foreign pins", x));
    let mut x = b.clone();
    x.pins.insert(bid("t-AAAAA"), Hash::of(b"local pin"));
    same.push(("local pins", x));
    for field in [
        REAFFIRMED,
        REFLECTS_AT,
        ADOPTED_AS,
        "status",
        "decided_at",
        "lifecycle",
    ] {
        let mut x = b.clone();
        x.fields.insert(field.into(), FieldValue::Str("v".into()));
        same.push((field, x));
    }
    let mut x = b.clone();
    x.order = "W".into();
    same.push(("order", x));
    for (what, x) in &same {
        assert_eq!(x.content_hash(), content, "{what}");
        assert!(!x.foreign.is_empty(), "{what}");
    }
    let mut tracked = b.clone();
    tracked
        .fields
        .insert("disposition".into(), FieldValue::Str("M2".into()));
    let t = Tracking::new(BTreeMap::from([(
        "claim".to_string(),
        BTreeSet::from(["disposition".to_string()]),
    )]));
    assert_eq!(tracked.content_hash_with(&t), b.content_hash_with(&t));

    // a foreign link is content
    let mut x = b.clone();
    x.foreign
        .links
        .get_mut("depends_on")
        .unwrap()
        .first_mut()
        .unwrap()
        .part = Some("statement".into());
    assert_ne!(x.content_hash(), content, "a link's part");
    let mut x = b.clone();
    x.foreign.links.insert(
        "refines".into(),
        vec![ForeignLink {
            target: ForeignId::new(repo(), bid("D-00002")),
            part: None,
        }],
    );
    assert_ne!(x.content_hash(), content, "another link");

    // the version covers everything, through the explicit encoding-1 layout
    let wire = ForeignBlockV1::of(&b);
    let body = postcard::to_allocvec(&wire).unwrap();
    assert_eq!(
        b.version_hash(),
        encoded_hash(BLOCK_FOREIGN_ENCODING, &body)
    );
    assert_eq!(
        postcard::from_bytes::<ForeignBlockV1>(&body)
            .unwrap()
            .into_current(),
        b
    );
    assert_ne!(
        same[0].1.version_hash(),
        b.version_hash(),
        "a pin moves the version"
    );
    // a foreign block has no encoding-0 form: it is refused, never silently cut
    assert!(LegacyBlockV0::from_current(&b).is_none());
    assert!(postcard::to_allocvec(&b).is_err());
    assert!(serde_json::to_value(&b).is_err());

    // a redacted projection keeps the recorded identity and the foreign source encoding
    let residue = BlockResidue::of(&b);
    assert_eq!(residue.source_encoding, BLOCK_FOREIGN_ENCODING);
    assert_eq!(residue.version, b.version_hash());
    assert_eq!(residue.content_hash, content);
    let stub = residue.stub(Hash::of(b"request"));
    assert_eq!(stub.version_hash(), b.version_hash());
    assert_eq!(stub.content_hash(), content);
    assert!(stub.foreign.is_empty(), "a removal keeps no foreign part");
}

#[test]
fn rpl2_legacy_change_embeds_legacy_block() {
    #[derive(Serialize)]
    enum OpLayout<'a> {
        Create { block: LegacyBlockV0 },
        Update { base: Hash, block: LegacyBlockV0 },
        Delete { base: Hash, id: &'a BlockId },
    }
    #[derive(Serialize)]
    struct ChangeLayout<'a, L> {
        view: &'a String,
        ops: Vec<OpLayout<'a>>,
        deps: &'a Vec<Hash>,
        provenance: &'a Provenance,
        justification: &'a Option<Justification>,
        message: &'a String,
        links: L,
    }
    fn frozen(b: &Block) -> LegacyBlockV0 {
        LegacyBlockV0::from_current(b).unwrap()
    }
    fn ops(c: &Change) -> Vec<OpLayout<'_>> {
        c.ops
            .iter()
            .map(|op| match op {
                Op::Create { block } => OpLayout::Create {
                    block: frozen(block),
                },
                Op::Update { base, block } => OpLayout::Update {
                    base: *base,
                    block: frozen(block),
                },
                Op::Delete { base, id } => OpLayout::Delete { base: *base, id },
            })
            .collect()
    }

    // encodings 0 and 1 embed the frozen encoding-0 block layout
    let c = change(local());
    let v1 = ChangeLayout {
        view: &c.view,
        ops: ops(&c),
        deps: &c.deps,
        provenance: &c.provenance,
        justification: &c.justification,
        message: &c.message,
        links: &c.links,
    };
    let body1 = postcard::to_allocvec(&c).unwrap();
    assert_eq!(body1, postcard::to_allocvec(&v1).unwrap());
    let legacy = LegacyChangeV0::from_current(&c).unwrap();
    let v0 = ChangeLayout {
        view: &c.view,
        ops: ops(&c),
        deps: &c.deps,
        provenance: &c.provenance,
        justification: &c.justification,
        message: &c.message,
        links: LegacyChangeLinksV0::from_current(&c.links).unwrap(),
    };
    let body0 = postcard::to_allocvec(&legacy).unwrap();
    assert_eq!(body0, postcard::to_allocvec(&v0).unwrap());
    // reading them gives blocks with empty foreign parts
    let id = |version: u16, body: &[u8]| EncodedIdentity {
        hash: encoded_hash(version, body),
        version,
    };
    let read0 = postcard::from_bytes::<LegacyChangeV0>(&body0)
        .unwrap()
        .into_current(id(0, &body0));
    let read1: Change = postcard::from_bytes(&body1).unwrap();
    for read in [&read0, &read1] {
        assert_eq!(read, &c);
        for op in &read.ops {
            if let Op::Create { block } | Op::Update { block, .. } = op {
                assert!(block.foreign.is_empty());
            }
        }
    }

    // a change that embeds a foreign block has neither legacy form: only the explicit encoding 2
    let fc = change(foreign());
    assert!(LegacyChangeV0::from_current(&fc).is_none());
    assert!(postcard::to_allocvec(&fc).is_err(), "no encoding-1 body");
    let wire = ForeignChangeV2::of(&fc);
    let body2 = postcard::to_allocvec(&wire).unwrap();
    let read2 = postcard::from_bytes::<ForeignChangeV2>(&body2)
        .unwrap()
        .into_current(id(CHANGE_FOREIGN_ENCODING, &body2));
    assert_eq!(read2, fc);
    assert_eq!(
        read2.stored_identity().unwrap().hash,
        encoded_hash(CHANGE_FOREIGN_ENCODING, &body2)
    );
    match &read2.ops[0] {
        Op::Create { block } => assert_eq!(block.foreign, parts()),
        other => panic!("{other:?}"),
    }
    // encoding 2 embeds every block with its foreign part, empty or not
    let mixed = Change {
        ops: vec![
            Op::Create { block: local() },
            Op::Create {
                block: Block {
                    id: bid("c-00002"),
                    ..foreign()
                },
            },
        ],
        ..change(local())
    };
    let body = postcard::to_allocvec(&ForeignChangeV2::of(&mixed)).unwrap();
    let back = postcard::from_bytes::<ForeignChangeV2>(&body)
        .unwrap()
        .into_current(id(CHANGE_FOREIGN_ENCODING, &body));
    assert_eq!(back, mixed);
}

fn decision(fields: &[(&str, &str)]) -> Block {
    Block {
        id: bid("D-7KQ2M"),
        block_type: "decision".into(),
        parent: Some(bid("s-1DENT")),
        order: "V".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
            .collect(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::from([(bid("t-MEDGR"), Hash::of(b"local pin"))]),
        foreign: parts(),
        read_identity: None,
    }
}

/// Every way an edit can move the foreign part: a link added, a link's part changed, a pin's
/// content or encoding changed, a pin added, a pin dropped, the whole part dropped.
fn smuggled(b: &Block) -> Vec<(&'static str, Block)> {
    let target = ForeignId::new(repo(), bid("D-4W1ZR"));
    let mut out = vec![];
    let mut x = b.clone();
    x.foreign.links.insert(
        "refines".into(),
        vec![ForeignLink {
            target: ForeignId::new(repo(), bid("D-00002")),
            part: None,
        }],
    );
    out.push(("link added", x));
    let mut x = b.clone();
    x.foreign.links.get_mut("depends_on").unwrap()[0].part = Some("statement".into());
    out.push(("link part", x));
    let mut x = b.clone();
    x.foreign.pins.get_mut(&target).unwrap().content = Hash::of(b"newer");
    out.push(("pin content", x));
    let mut x = b.clone();
    x.foreign.pins.get_mut(&target).unwrap().encoding = 1;
    out.push(("pin encoding", x));
    let mut x = b.clone();
    x.foreign.pins.insert(
        ForeignId::new(repo(), bid("D-00002")),
        ForeignPin {
            content: Hash::of(b"other"),
            encoding: 0,
        },
    );
    out.push(("pin added", x));
    let mut x = b.clone();
    x.foreign.pins.clear();
    out.push(("pins dropped", x));
    let mut x = b.clone();
    x.foreign = ForeignParts::default();
    out.push(("part dropped", x));
    out
}

#[test]
fn rpl2_tracking_cannot_smuggle_foreign_links_or_pins() {
    let t = Tracking::new(BTreeMap::from([(
        "decision".to_string(),
        BTreeSet::from(["disposition".to_string()]),
    )]));
    let old = decision(&[
        ("statement", "Ship weekly"),
        ("status", "accepted"),
        ("disposition", "M2d1"),
    ]);
    let new = decision(&[
        ("statement", "Ship weekly"),
        ("status", "accepted"),
        ("disposition", "M2d2"),
    ]);
    assert!(
        only_tracking_changed(&old, &new, &t),
        "a tracking edit with the same foreign part"
    );
    for (what, x) in smuggled(&new) {
        assert!(!only_tracking_changed(&old, &x, &t), "{what}");
    }
}

#[test]
fn rpl2_reaffirmation_cannot_smuggle_foreign_links_or_pins() {
    let old = decision(&[("statement", "Ship weekly"), ("status", "accepted")]);
    let mut new = old.clone();
    new.fields.insert(
        REAFFIRMED.into(),
        FieldValue::Map(BTreeMap::from([(
            "t-MEDGR".to_string(),
            FieldValue::Str("still holds".into()),
        )])),
    );
    new.pins
        .insert(bid("t-MEDGR"), Hash::of(b"refreshed local pin"));
    assert!(
        only_reaffirmation_changed(&old, &new),
        "a re-affirmation refreshes local pins with the same foreign part"
    );
    for (what, x) in smuggled(&new) {
        assert!(!only_reaffirmation_changed(&old, &x), "{what}");
    }
}
