//! Redaction records and read identities (Task 1, D-3M81W, D-F0904). New records have a fixed
//! postcard shape; a removed block or change keeps its recorded identity through a skipped read
//! field, so every existing kind keeps its bytes and hashes.
use rdy_model::decision::is_frozen;
use rdy_model::redaction::*;
use rdy_model::*;
use std::collections::BTreeMap;

// Bytes and hashes recorded at BASE 192ace4 (before the redaction model existed).
const BLOCK: &str = "07702d58595a3334086465636973696f6e0107732d5041523031016d06046c6973740401030161036d61700501016b0206016e020d046e6f6e6500026f6b0101057469746c6503094f6c64207469746c6509426f647920746578740107726566696e65730107642d41424331320101700107642d41424331326cc8677933df2f583de49ba1c77d836754fe84fe0d16d5d283fdd6bb89ae7d20";
const BLOCKV: &str = "5c6c9c291c19d0d52e0fce6894883a83bcadc223d8296fbb33bc2ba6dcb8386e";
const BLOCKC: &str = "1132b101e2ba2a08a9c359c3b78fdec71ed7b63612a6d41206f107829ef3fffa";
const CHANGE: &str = "04776f726b030007702d58595a3334086465636973696f6e0107732d5041523031016d06046c6973740401030161036d61700501016b0206016e020d046e6f6e6500026f6b0101057469746c6503094f6c64207469746c6509426f647920746578740107726566696e65730107642d41424331320101700107642d41424331326cc8677933df2f583de49ba1c77d836754fe84fe0d16d5d283fdd6bb89ae7d2001de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07702d58595a3334086465636973696f6e0107732d5041523031016d06046c6973740401030161036d61700501016b0206016e020d046e6f6e6500026f6b0101057469746c6503094f6c64207469746c6509426f647920746578740107726566696e65730107642d41424331320101700107642d41424331326cc8677933df2f583de49ba1c77d836754fe84fe0d16d5d283fdd6bb89ae7d2002afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e393901f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc4528737076167656e743a61010272310180c0f2f5b46601010203776879036d736701f0877c0e1ab8f4c605052fb919df4dc8cb7ff4df5a9cd5e85e8796a5727533d901e672e5cd4b1892ec81b2f8b2ef2d451f0a7e3ba808d2211c045432f767de0d2e03616c7401016e010768756d616e3a6900";
const CHANGEID: &str = "46bab620a169a77ae758d3fd09f08e44e1eecfa75cbca5ab32d413d3cea4581b";
const OPS: [&str; 3] = [
    "0007702d58595a3334086465636973696f6e0107732d5041523031016d06046c6973740401030161036d61700501016b0206016e020d046e6f6e6500026f6b0101057469746c6503094f6c64207469746c6509426f647920746578740107726566696e65730107642d41424331320101700107642d41424331326cc8677933df2f583de49ba1c77d836754fe84fe0d16d5d283fdd6bb89ae7d20",
    "01de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07702d58595a3334086465636973696f6e0107732d5041523031016d06046c6973740401030161036d61700501016b0206016e020d046e6f6e6500026f6b0101057469746c6503094f6c64207469746c6509426f647920746578740107726566696e65730107642d41424331320101700107642d41424331326cc8677933df2f583de49ba1c77d836754fe84fe0d16d5d283fdd6bb89ae7d20",
    "02afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e3939",
];

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

fn h(b: &[u8]) -> Hash {
    Hash::of(b)
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn prov() -> Provenance {
    Provenance {
        actor: "agent:a".into(),
        run: Some("r1".into()),
        role: Role::Writer,
        at_ms: 1_759_449_600_000,
    }
}

/// The fixture block: every field kind, a link with a part label, and a pin.
fn block() -> Block {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_string(), FieldValue::Str("Old title".into()));
    fields.insert("n".to_string(), FieldValue::Int(-7));
    fields.insert("ok".to_string(), FieldValue::Bool(true));
    fields.insert("none".to_string(), FieldValue::Null);
    fields.insert(
        "list".to_string(),
        FieldValue::List(vec![FieldValue::Str("a".into())]),
    );
    let mut m = BTreeMap::new();
    m.insert("k".to_string(), FieldValue::Int(3));
    fields.insert("map".to_string(), FieldValue::Map(m));
    let mut links = BTreeMap::new();
    links.insert(
        "refines".to_string(),
        vec![Link {
            target: id("d-ABC12"),
            part: Some("p".into()),
        }],
    );
    let mut pins = BTreeMap::new();
    pins.insert(id("d-ABC12"), h(b"pin"));
    Block {
        id: id("p-XYZ34"),
        block_type: "decision".into(),
        parent: Some(id("s-PAR01")),
        order: "m".into(),
        fields,
        body: "Body text".into(),
        links,
        pins,
        read_identity: None,
    }
}

fn change() -> Change {
    let b = block();
    Change {
        view: "work".into(),
        ops: vec![
            Op::Create { block: b.clone() },
            Op::Update {
                base: h(b"base"),
                block: b,
            },
            Op::Delete {
                base: h(b"gone"),
                id: id("q-DEN99"),
            },
        ],
        deps: vec![h(b"dep")],
        provenance: prov(),
        justification: Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "why".into(),
        }),
        message: "msg".into(),
        links: ChangeLinks {
            intent: Some(h(b"intent")),
            alternative: Some(AltChoice {
                set: h(b"set"),
                member: "alt".into(),
            }),
            client_nonce: Some("n".into()),
            imported_by: Some(ActorRun {
                actor: "human:i".into(),
                run: None,
            }),
        },
        read_identity: None,
    }
}

fn decision(status: &str) -> Block {
    let mut b = block();
    b.fields
        .insert("status".into(), FieldValue::Str(status.to_string()));
    b
}

#[test]
fn rdx1_old_objects_keep_their_bytes_and_hashes() {
    let block_bytes = unhex(BLOCK);
    let decoded: Block = postcard::from_bytes(&block_bytes).unwrap();
    assert_eq!(decoded, block());
    assert_eq!(decoded.read_identity, None);
    assert_eq!(enc(&decoded), block_bytes, "a block gained or lost a byte");
    assert_eq!(decoded.version_hash().to_string(), BLOCKV);
    assert_eq!(decoded.content_hash().to_string(), BLOCKC);
    assert_eq!(decoded.version_hash(), hash_of(&decoded));

    let change_bytes = unhex(CHANGE);
    let decoded: Change = postcard::from_bytes(&change_bytes).unwrap();
    assert_eq!(decoded, change());
    assert_eq!(decoded.read_identity, None);
    assert!(!decoded.is_projection());
    assert_eq!(
        enc(&decoded),
        change_bytes,
        "a change gained or lost a byte"
    );
    assert_eq!(decoded.id().to_string(), CHANGEID);
    assert_eq!(decoded.id(), hash_of(&decoded));

    for (op, fixture) in change().ops.iter().zip(OPS) {
        let bytes = unhex(fixture);
        let decoded: Op = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(&decoded, op);
        assert_eq!(enc(&decoded), bytes, "an op gained or lost a byte");
    }
}

// Hand-written postcard pieces, independent of the derives under test.
fn s(text: &str) -> Vec<u8> {
    assert!(text.len() < 128);
    let mut out = vec![text.len() as u8];
    out.extend(text.as_bytes());
    out
}

fn hb(x: &Hash) -> Vec<u8> {
    x.0.to_vec()
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// Provenance { actor "human:p", run None, role Approver, at_ms 5 }.
fn small_prov() -> (Provenance, Vec<u8>) {
    let p = Provenance {
        actor: "human:p".into(),
        run: None,
        role: Role::Approver,
        at_ms: 5,
    };
    (p, cat(&[s("human:p"), vec![0, 3, 10]]))
}

fn manifest() -> (RedactionManifest, Vec<u8>) {
    let x = id("p-XYZ34");
    let m = RedactionManifest {
        versions: vec![(x.clone(), h(b"v"))],
        blobs: vec![h(b"blob")],
        shared_blobs: vec![],
        spans: vec![DropSpan {
            place: FramePlace {
                segment: 3,
                offset: 300,
            },
            kind: 1,
            hash: h(b"v"),
            len: 200,
            residue: Some(h(b"res")),
        }],
        inside: vec![ImpactRow {
            kind: ImpactKind::Tension,
            record: "x".into(),
            relation: ImpactRelation::Quotes,
            erasable: false,
        }],
        outside: [
            CopyKind::Render,
            CopyKind::Export,
            CopyKind::Dump,
            CopyKind::Clone,
            CopyKind::Published,
            CopyKind::WorkingCopy,
        ]
        .into_iter()
        .map(|kind| CopyRow {
            kind,
            status: CopyStatus::Unknown,
        })
        .collect(),
        quarantine: vec!["q".into()],
        assessment: AssessmentSupport::NotImplemented,
    };
    let bytes = cat(&[
        vec![1],
        s("p-XYZ34"),
        hb(&h(b"v")),
        vec![1],
        hb(&h(b"blob")),
        vec![0],
        // spans: one; place (3, varint 300); kind byte 1; hash; len varint 200; Some(residue)
        vec![1, 3, 0xac, 0x02, 1],
        hb(&h(b"v")),
        vec![0xc8, 0x01, 1],
        hb(&h(b"res")),
        // inside: one; Tension = 9; "x"; Quotes = 3; erasable false
        vec![1, 9],
        s("x"),
        vec![3, 0],
        // outside: six kinds in order, each Unknown = 0
        vec![6, 0, 0, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0],
        vec![1],
        s("q"),
        // assessment: NotImplemented = 0
        vec![0],
    ]);
    (m, bytes)
}

fn roundtrip<T>(v: &T, bytes: &[u8])
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(enc(v), bytes);
    assert_eq!(&postcard::from_bytes::<T>(bytes).unwrap(), v);
}

#[test]
fn rdx1_records_have_fixed_field_and_variant_order() {
    let x = id("p-XYZ34");
    let (p, pb) = small_prov();
    let (m, mb) = manifest();

    // REDACTION: carrier, actor, targets (all three selectors), class, reason, manifest.
    let request = Redaction {
        carrier: h(b"carrier"),
        actor: p.clone(),
        targets: vec![
            RedactTarget::AllVersions(x.clone()),
            RedactTarget::Version {
                id: x.clone(),
                version: h(b"v"),
            },
            RedactTarget::Blob(h(b"blob")),
        ],
        reason_class: ReasonClass::Legal,
        reason: "r".into(),
        manifest: m.clone(),
    };
    let request_bytes = cat(&[
        hb(&h(b"carrier")),
        pb.clone(),
        vec![3, 0],
        s("p-XYZ34"),
        vec![1],
        s("p-XYZ34"),
        hb(&h(b"v")),
        vec![2],
        hb(&h(b"blob")),
        vec![2],
        s("r"),
        mb.clone(),
    ]);
    roundtrip(&request, &request_bytes);

    // REDACTION_APPLY: request, landing, report hash.
    let apply = RedactionApply {
        request: h(b"req"),
        landing: h(b"land"),
        report_hash: h(b"rep"),
    };
    roundtrip(
        &apply,
        &cat(&[hb(&h(b"req")), hb(&h(b"land")), hb(&h(b"rep"))]),
    );

    // REDACTION_RESIDUE, block form.
    let block_residue = RedactionResidue::Block(BlockResidue {
        version: h(b"v"),
        id: x.clone(),
        block_type: "decision".into(),
        parent: Some(id("s-PAR01")),
        order: "m".into(),
        content_hash: h(b"c"),
        frozen: true,
    });
    roundtrip(
        &block_residue,
        &cat(&[
            vec![0],
            hb(&h(b"v")),
            s("p-XYZ34"),
            s("decision"),
            vec![1],
            s("s-PAR01"),
            s("m"),
            hb(&h(b"c")),
            vec![1],
        ]),
    );

    // REDACTION_RESIDUE, change form: create, update, delete in that order.
    let change_residue = RedactionResidue::Change(ChangeResidue {
        change: h(b"chg"),
        view: "work".into(),
        ops: vec![
            ResidualOp::Create {
                id: x.clone(),
                version: h(b"v1"),
            },
            ResidualOp::Update {
                base: h(b"v1"),
                id: x.clone(),
                version: h(b"v2"),
            },
            ResidualOp::Delete {
                base: h(b"v2"),
                id: x.clone(),
            },
        ],
        deps: vec![h(b"dep")],
        provenance: p,
        justification: None,
        message: "m".into(),
        links: ChangeLinks::default(),
    });
    roundtrip(
        &change_residue,
        &cat(&[
            vec![1],
            hb(&h(b"chg")),
            s("work"),
            vec![3, 0],
            s("p-XYZ34"),
            hb(&h(b"v1")),
            vec![1],
            hb(&h(b"v1")),
            s("p-XYZ34"),
            hb(&h(b"v2")),
            vec![2],
            hb(&h(b"v2")),
            s("p-XYZ34"),
            vec![1],
            hb(&h(b"dep")),
            pb,
            vec![0],
            s("m"),
            vec![0, 0, 0, 0],
        ]),
    );

    // The closed enums: declaration order is the variant index.
    let classes = [
        ReasonClass::Secret,
        ReasonClass::Personal,
        ReasonClass::Legal,
        ReasonClass::Retraction,
    ];
    for (i, c) in classes.iter().enumerate() {
        roundtrip(c, &[i as u8]);
    }
    let kinds = [
        ImpactKind::BlockVersion,
        ImpactKind::Change,
        ImpactKind::Landing,
        ImpactKind::Tag,
        ImpactKind::View,
        ImpactKind::Proposal,
        ImpactKind::StateSnapshot,
        ImpactKind::AuditText,
        ImpactKind::Review,
        ImpactKind::Tension,
        ImpactKind::Answer,
    ];
    for (i, k) in kinds.iter().enumerate() {
        roundtrip(k, &[i as u8]);
    }
    let relations = [
        ImpactRelation::Removed,
        ImpactRelation::Embeds,
        ImpactRelation::References,
        ImpactRelation::Quotes,
    ];
    for (i, r) in relations.iter().enumerate() {
        roundtrip(r, &[i as u8]);
    }
    roundtrip(&AssessmentSupport::NotImplemented, &[0]);
    roundtrip(&CopyStatus::Unknown, &[0]);
}

/// A change creating the target block and an unrelated block, so a projection resolves both.
fn two_block_change() -> (Block, Block, Change) {
    let target = block();
    let mut other = block();
    other.id = id("p-QQQ77");
    other.body = "Other text".into();
    let c = Change {
        view: "work".into(),
        ops: vec![
            Op::Create {
                block: target.clone(),
            },
            Op::Create {
                block: other.clone(),
            },
        ],
        deps: vec![h(b"dep")],
        provenance: prov(),
        justification: None,
        message: "add two".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    (target, other, c)
}

#[test]
fn rdx1_a_projection_keeps_its_recorded_identity() {
    let request = h(b"request");
    let original = block();
    let stub = BlockResidue::of(&original).stub(request);
    assert!(stub.is_redacted());
    assert_eq!(stub.redaction(), Some(request));
    assert_eq!(stub.version_hash(), original.version_hash());
    assert_eq!(stub.version_hash().to_string(), BLOCKV);
    assert_eq!(stub.content_hash(), original.content_hash());
    // The stub's own bytes hash to something else: the identity is recorded, not recomputed.
    assert_ne!(hash_of(&stub), original.version_hash());
    // Copying a projection keeps its marker.
    let copy = stub.clone();
    assert_eq!(copy.read_identity, stub.read_identity);
    assert_eq!(copy.version_hash(), original.version_hash());

    // A normal block uses the existing algorithms.
    assert!(!original.is_redacted());
    assert_eq!(original.redaction(), None);
    assert_eq!(original.version_hash(), hash_of(&original));

    let (target, other, c) = two_block_change();
    let residue = ChangeResidue::of(&c);
    assert_eq!(residue.change, c.id());
    assert_eq!(
        residue.ops,
        vec![
            ResidualOp::Create {
                id: target.id.clone(),
                version: target.version_hash(),
            },
            ResidualOp::Create {
                id: other.id.clone(),
                version: other.version_hash(),
            },
        ]
    );
    let projected = residue
        .project(|bid, version| {
            if *bid == target.id {
                assert_eq!(version, target.version_hash());
                Ok::<_, ()>(BlockResidue::of(&target).stub(request))
            } else {
                assert_eq!(version, other.version_hash());
                Ok(other.clone())
            }
        })
        .unwrap();
    assert!(projected.is_projection());
    assert_eq!(projected.id(), c.id());
    assert_ne!(hash_of(&projected), c.id());
    assert_eq!(projected.view, c.view);
    assert_eq!(projected.deps, c.deps);
    assert_eq!(projected.provenance, c.provenance);
    assert_eq!(projected.message, c.message);
    assert_eq!(projected.links, c.links);
    // Op order is kept; the target is a stub, the unrelated block stays readable.
    let Op::Create { block: first } = &projected.ops[0] else {
        panic!("first op is a create");
    };
    let Op::Create { block: second } = &projected.ops[1] else {
        panic!("second op is a create");
    };
    assert!(first.is_redacted());
    assert_eq!(first.version_hash(), target.version_hash());
    assert_eq!(second, &other);
    assert_eq!(projected.clone().id(), c.id());

    // A normal change keeps the existing algorithm.
    assert!(!c.is_projection());
    assert_eq!(c.id(), hash_of(&c));
    assert_eq!(change().id().to_string(), CHANGEID);
}

#[test]
fn rdx1_a_stub_has_no_source_text_or_links() {
    let original = block();
    let residue = BlockResidue::of(&original);
    let stub = residue.stub(h(b"request"));
    assert_eq!(stub.id, original.id);
    assert_eq!(stub.block_type, original.block_type);
    assert_eq!(stub.parent, original.parent);
    assert_eq!(stub.order, original.order);
    assert!(stub.fields.is_empty());
    assert!(stub.links.is_empty());
    assert!(stub.pins.is_empty());
    assert_eq!(stub.body, CONTENT_REMOVED);
    assert_eq!(CONTENT_REMOVED, "Content removed.");
    for text in [format!("{stub:?}"), format!("{residue:?}")] {
        for removed in ["Old title", "title", "Body text", "refines", "list", "map"] {
            assert!(!text.contains(removed), "{removed:?} survived in {text}");
        }
        assert!(
            !text.contains("d-ABC12"),
            "a link target survived in {text}"
        );
    }
    assert!(stub.ref_tokens().is_empty());
}

#[test]
fn rdx1_a_stub_keeps_the_frozen_guard_fact() {
    let request = h(b"request");
    for status in ["accepted", "rejected"] {
        let original = decision(status);
        assert!(is_frozen(&original));
        let residue = BlockResidue::of(&original);
        assert!(residue.frozen, "{status}");
        let stub = residue.stub(request);
        assert!(is_frozen(&stub), "a {status} decision lost its write guard");
        // The fact comes from the marker; no status string is restored.
        assert!(!stub.fields.contains_key("status"));
        assert_eq!(decision::status(&stub), None);
    }
    let draft = decision("proposed");
    let stub = BlockResidue::of(&draft).stub(request);
    assert!(!stub.read_identity.as_ref().unwrap().frozen);
    assert!(!is_frozen(&stub), "a draft became frozen");
    let mut note = block();
    note.block_type = "note".into();
    assert!(!is_frozen(&BlockResidue::of(&note).stub(request)));
}

/// The report hash through the bounded encoder, with a fallibly allocated record buffer.
fn digest(m: &RedactionManifest) -> Hash {
    let mut buf = record_buffer().unwrap();
    assert_eq!(buf.len(), MAX_RECORD_BYTES);
    m.report_hash(&mut buf).unwrap()
}

#[test]
fn rdx1_manifest_hash_ignores_presentation() {
    let (m, bytes) = manifest();
    let mut domain = REPORT_DOMAIN.to_vec();
    assert_eq!(domain, b"rdy-redaction-report-v1\0");
    domain.extend(&bytes);
    let digest_m = digest(&m);
    assert_eq!(digest_m, Hash::of(&domain));
    // The same manifest has one digest, however it is later shown.
    assert_eq!(digest(&m.clone()), digest_m);
    assert_eq!(digest(&manifest().0), digest_m);

    let mut target = m.clone();
    target.versions[0].1 = h(b"other version");
    assert_ne!(digest(&target), digest_m, "a target change kept the digest");
    let mut span = m.clone();
    span.spans[0].len += 1;
    assert_ne!(digest(&span), digest_m, "a span change kept the digest");
    let mut outside = m.clone();
    outside.outside.pop();
    assert_ne!(digest(&outside), digest_m, "an outside row kept the digest");
}

/// A manifest with empty object lists, the six Unknown outside rows and these quarantine names.
fn quarantine_manifest(quarantine: Vec<String>) -> RedactionManifest {
    let mut m = manifest().0;
    m.versions.clear();
    m.blobs.clear();
    m.spans.clear();
    m.inside.clear();
    m.quarantine = quarantine;
    m
}

#[test]
fn rdx1_report_hash_is_bounded_and_fallible() {
    // Oversized: 100000 names of 183 bytes encode to 18500022 bytes, over the record bound.
    let names = (0..100_000)
        .map(|i| format!("segments/{i:08}.torn-{}", "x".repeat(160)))
        .collect();
    let big = quarantine_manifest(names);
    assert_eq!(enc(&big).len(), 18_500_022);
    let mut buf = record_buffer().unwrap();
    assert_eq!(
        big.report_hash(&mut buf),
        Err(postcard::Error::SerializeBufferFull),
        "an oversized manifest was hashed"
    );

    // Boundary: an encoding of exactly MAX_RECORD_BYTES hashes; one byte less of buffer refuses.
    let pad = |i: usize, extra: usize| format!("segments/{i:08}.torn-{}", "x".repeat(142 + extra));
    let base: Vec<String> = (0..100_000).map(|i| pad(i, 0)).collect();
    let deficit = MAX_RECORD_BYTES - enc(&quarantine_manifest(base)).len();
    assert!(deficit < 100_000, "deficit {deficit}");
    let names = (0..100_000)
        .map(|i| pad(i, usize::from(i < deficit)))
        .collect();
    let exact = quarantine_manifest(names);
    let bytes = enc(&exact);
    assert_eq!(bytes.len(), MAX_RECORD_BYTES);
    let mut domain = REPORT_DOMAIN.to_vec();
    domain.extend(&bytes);
    assert_eq!(exact.report_hash(&mut buf), Ok(Hash::of(&domain)));
    assert_eq!(
        exact.report_hash(&mut buf[..MAX_RECORD_BYTES - 1]),
        Err(postcard::Error::SerializeBufferFull)
    );
    // The digest does not depend on the buffer's size or earlier contents.
    let (m, _) = manifest();
    let mut small = vec![0xffu8; 4096];
    assert_eq!(m.report_hash(&mut small), Ok(digest(&m)));
}

#[test]
fn rdx1_human_readable_hashes_are_full_lowercase_hex() {
    let (m, _) = manifest();
    let text = serde_yaml_ng::to_string(&m).unwrap();
    assert!(text.contains(&h(b"v").to_string()), "{text}");
    assert!(text.contains(&h(b"blob").to_string()), "{text}");
    assert!(text.contains(&h(b"res").to_string()), "{text}");
    assert_eq!(
        serde_yaml_ng::from_str::<RedactionManifest>(&text).unwrap(),
        m
    );
    let target = RedactTarget::Blob(h(b"blob"));
    let text = serde_yaml_ng::to_string(&target).unwrap();
    assert!(text.contains(&h(b"blob").to_string()), "{text}");
    assert_eq!(
        serde_yaml_ng::from_str::<RedactTarget>(&text).unwrap(),
        target
    );
    let residue = ChangeResidue::of(&two_block_change().2);
    let text = serde_yaml_ng::to_string(&residue).unwrap();
    assert!(text.contains(&residue.change.to_string()), "{text}");
    assert!(text.contains(&h(b"dep").to_string()), "{text}");
    assert_eq!(
        serde_yaml_ng::from_str::<ChangeResidue>(&text).unwrap(),
        residue
    );

    // The bounded encoder writes the same bytes as the allocating one, and refuses a short buffer.
    let mut buf = [0u8; 4096];
    assert_eq!(encode_into(&m, &mut buf).unwrap(), &enc(&m)[..]);
    assert!(encode_into(&m, &mut [0u8; 8]).is_err());
}

#[test]
fn rdx1_residue_link_hashes_are_full_lowercase_hex() {
    use serde_yaml_ng::Value;
    let original = change();
    let residue = ChangeResidue::of(&original);
    let intent = h(b"intent");
    let set = h(b"set");
    assert_eq!(residue.links.intent, Some(intent));
    assert_eq!(residue.links.alternative.as_ref().unwrap().set, set);

    // Out: the nested link hashes are full lowercase hex strings.
    let value = serde_yaml_ng::to_value(&residue).unwrap();
    let links = &value["links"];
    assert_eq!(
        links["intent"],
        Value::String(intent.to_string()),
        "{links:?}"
    );
    assert_eq!(
        links["alternative"]["set"],
        Value::String(set.to_string()),
        "{links:?}"
    );
    assert_eq!(links["alternative"]["member"], Value::String("alt".into()));
    assert_eq!(links["client_nonce"], Value::String("n".into()));
    assert_eq!(
        links["imported_by"]["actor"],
        Value::String("human:i".into())
    );
    assert_eq!(
        serde_yaml_ng::from_value::<ChangeResidue>(value.clone()).unwrap(),
        residue
    );

    // In: hex text decodes to the hashes it names; absent links stay absent.
    let (other_intent, other_set) = (h(b"other intent"), h(b"other set"));
    let mut edited = value.clone();
    edited["links"]["intent"] = Value::String(other_intent.to_string());
    edited["links"]["alternative"]["set"] = Value::String(other_set.to_string());
    let decoded = serde_yaml_ng::from_value::<ChangeResidue>(edited).unwrap();
    assert_eq!(decoded.links.intent, Some(other_intent));
    assert_eq!(decoded.links.alternative.unwrap().set, other_set);
    let mut empty = residue.clone();
    empty.links = ChangeLinks::default();
    let text = serde_yaml_ng::to_string(&empty).unwrap();
    assert_eq!(
        serde_yaml_ng::from_str::<ChangeResidue>(&text).unwrap(),
        empty
    );
    // A byte array is not the human-readable form, and a short hash is refused.
    let mut array = value.clone();
    array["links"]["intent"] = serde_yaml_ng::to_value(intent.0).unwrap();
    assert!(serde_yaml_ng::from_value::<ChangeResidue>(array).is_err());
    let mut short = value;
    short["links"]["intent"] = Value::String(intent.short());
    assert!(serde_yaml_ng::from_value::<ChangeResidue>(short).is_err());

    // Postcard: the links keep ChangeLinks's own bytes, as the residue's last field.
    let bytes = enc(&residue);
    assert!(bytes.ends_with(&enc(&original.links)));
    assert_eq!(
        postcard::from_bytes::<ChangeResidue>(&bytes).unwrap(),
        residue
    );
}
