//! Ported changes and their residues (hub ports). A port records its source in the seventh
//! link field and takes change encoding 3, or 4 with a foreign part. A port's residue takes
//! residue encoding 2. Every released body keeps its bytes and its hash.
use rdy_model::encoding::{
    CHANGE_PORT_ENCODING, CHANGE_PORT_FOREIGN_ENCODING, ChangeLinksV1, EncodedIdentity,
    ForeignChangeV2, LegacyChangeV0, LegacyRedactionResidueV0, PortChangeV3, PortForeignChangeV4,
    PortResidueV2, RESIDUE_ENCODING, RESIDUE_PORT_ENCODING, change_body, encoded_hash,
};
use rdy_model::foreign::{ForeignId, ForeignLink, ForeignParts, ForeignPin};
use rdy_model::redaction::*;
use rdy_model::*;
use std::collections::BTreeMap;

// Bytes and hashes recorded at the 0.57.0 base, before `ChangeLinks` gained its seventh
// field. Never regenerate them.
/// The encoding-0 body of `change(false, false)`.
const C0: &str = "04776f726b030007632d374b51324d05636c61696d00015601057469746c650304506f72740c506f7274656420746578742e01096465736372696265730107742d4d45444752000107742d4d45444752e4e7636c21ce34009ed2398a68e7297fc189ba08cd95198f60f295d3249ef26e01de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07632d374b51324d05636c61696d00015601057469746c650304506f72740c506f7274656420746578742e01096465736372696265730107742d4d45444752000107742d4d45444752e4e7636c21ce34009ed2398a68e7297fc189ba08cd95198f60f295d3249ef26e02afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e393901f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc45287370a706572736f6e3a6164610102723101809095e0d56601000107442d3457315a520f63617272792074686520636c61696d000001026e3100";
/// Its id.
const C0_ID: &str = "37bcf6bf4ae931cf642f46c0621ddfe8acf5b678b8c3d4e86e88198145c6dc83";
/// The encoding-1 body of `change(true, false)`.
const C1: &str = "04776f726b030007632d374b51324d05636c61696d00015601057469746c650304506f72740c506f7274656420746578742e01096465736372696265730107742d4d45444752000107742d4d45444752e4e7636c21ce34009ed2398a68e7297fc189ba08cd95198f60f295d3249ef26e01de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07632d374b51324d05636c61696d00015601057469746c650304506f72740c506f7274656420746578742e01096465736372696265730107742d4d45444752000107742d4d45444752e4e7636c21ce34009ed2398a68e7297fc189ba08cd95198f60f295d3249ef26e02afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e393901f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc45287370a706572736f6e3a6164610102723101809095e0d56601000107442d3457315a520f63617272792074686520636c61696d000001026e31000163874d0cc87c47e73a6a68204212f3d1ec68ac78dc28b23deb8f36ce24cdab1501e263a9d34e74e876d9a1cbcd3faaa3c213e189b5724b422be74e6f75b45922ab";
/// Its id.
const C1_ID: &str = "178f3ce9bad191d1890dd32a133d24b146c4f5aea9e26036e81f96cd06d82dbc";
/// The encoding-2 body of `change(true, true)`.
const C2: &str = "04776f726b030007632d374b51324d05636c61696d00015601057469746c650304506f72740c506f7274656420746578742e01096465736372696265730107742d4d45444752000107742d4d45444752e4e7636c21ce34009ed2398a68e7297fc189ba08cd95198f60f295d3249ef26e010a646570656e64735f6f6e010707070707070707070707070707070707442d3457315a5200010707070707070707070707070707070707442d3457315a527917ec8ab9d951596768e120ca9245689155c72ccf747ef14fad4ceadfb76f380001de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07632d374b51324d05636c61696d00015601057469746c650304506f72740c506f7274656420746578742e01096465736372696265730107742d4d45444752000107742d4d45444752e4e7636c21ce34009ed2398a68e7297fc189ba08cd95198f60f295d3249ef26e000002afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e393901f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc45287370a706572736f6e3a6164610102723101809095e0d56601000107442d3457315a520f63617272792074686520636c61696d000001026e31000163874d0cc87c47e73a6a68204212f3d1ec68ac78dc28b23deb8f36ce24cdab1501e263a9d34e74e876d9a1cbcd3faaa3c213e189b5724b422be74e6f75b45922ab";
/// Its id.
const C2_ID: &str = "aa0193bc2b56a05e5a69985d5315cd117462751a060dd2d6ae0d0acca1cb4b3e";
/// The encoding-0 body of the residue of `change(false, false)`, source encoding 0.
const R0: &str = "01490812cd292f9d45831d98bc33b1c95e52b022fb3cf7e29b7c69548824b3911a04776f726b030007632d374b51324d0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf01de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07632d374b51324d0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf02afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e393901f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc45287370a706572736f6e3a6164610102723101809095e0d56601000107442d3457315a520f63617272792074686520636c61696d000001026e3100";
/// Its frame hash.
const R0_ID: &str = "c26b1197fa477e74ba83d93c1b5408509b9b9626c7e564580e7fe01fa04eee1a";
/// The encoding-1 body of the residue of `change(true, false)`.
const R1: &str = "01178f3ce9bad191d1890dd32a133d24b146c4f5aea9e26036e81f96cd06d82dbc04776f726b030007632d374b51324d0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf01de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07632d374b51324d0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf02afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e393901f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc45287370a706572736f6e3a6164610102723101809095e0d56601000107442d3457315a520f63617272792074686520636c61696d000001026e31000163874d0cc87c47e73a6a68204212f3d1ec68ac78dc28b23deb8f36ce24cdab1501e263a9d34e74e876d9a1cbcd3faaa3c213e189b5724b422be74e6f75b45922ab01";
/// Its frame hash.
const R1_ID: &str = "4580214fa80dd828583b39dbcfdb59f3c2be9a17d63ecea644dc7fb0cd22f072";
/// Its JSON.
const R1_JSON: &str = r#"{"change":{"change":"178f3ce9bad191d1890dd32a133d24b146c4f5aea9e26036e81f96cd06d82dbc","view":"work","ops":[{"create":{"id":"c-7KQ2M","version":"0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf"}},{"update":{"base":"de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d","id":"c-7KQ2M","version":"0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf"}},{"delete":{"base":"afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e","id":"q-DEN99"}}],"deps":["f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc4528737"],"provenance":{"actor":"person:ada","run":"r1","role":"Writer","at_ms":1763856000000},"justification":{"Implements":["D-4W1ZR"]},"message":"carry the claim","links":{"intent":null,"alternative":null,"client_nonce":"n1","imported_by":null,"replayed_from":"63874d0cc87c47e73a6a68204212f3d1ec68ac78dc28b23deb8f36ce24cdab15","briefing":"e263a9d34e74e876d9a1cbcd3faaa3c213e189b5724b422be74e6f75b45922ab"},"source_encoding":1}}"#;
/// The encoding-1 body of the residue of `change(true, true)` (source encoding 2).
const R12: &str = "01aa0193bc2b56a05e5a69985d5315cd117462751a060dd2d6ae0d0acca1cb4b3e04776f726b030007632d374b51324de680bea9a5af9b580920557124fbdfe0eaadd0adb729f16d71bf935bcd66aa1601de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d07632d374b51324d0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf02afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e07712d44454e393901f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc45287370a706572736f6e3a6164610102723101809095e0d56601000107442d3457315a520f63617272792074686520636c61696d000001026e31000163874d0cc87c47e73a6a68204212f3d1ec68ac78dc28b23deb8f36ce24cdab1501e263a9d34e74e876d9a1cbcd3faaa3c213e189b5724b422be74e6f75b45922ab02";
/// Its frame hash.
const R12_ID: &str = "6ced58d561df4a81cef6dda359fc0bd31c1b28f1a14cfb9dc7ab4287408d2c97";
/// Its JSON.
const R12_JSON: &str = r#"{"change":{"change":"aa0193bc2b56a05e5a69985d5315cd117462751a060dd2d6ae0d0acca1cb4b3e","view":"work","ops":[{"create":{"id":"c-7KQ2M","version":"e680bea9a5af9b580920557124fbdfe0eaadd0adb729f16d71bf935bcd66aa16"}},{"update":{"base":"de8f3fcf327eb911b2c368e011e0ffdee22fed456b4f4508337018ba46a0639d","id":"c-7KQ2M","version":"0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf"}},{"delete":{"base":"afae16e5c35fafc0bb2c5333310842c4b11e4d3946d9e09df5c194047375d47e","id":"q-DEN99"}}],"deps":["f286bc69f76edf5946c3b4ef9035baeb30bb4568981070ea34026a2dc4528737"],"provenance":{"actor":"person:ada","run":"r1","role":"Writer","at_ms":1763856000000},"justification":{"Implements":["D-4W1ZR"]},"message":"carry the claim","links":{"intent":null,"alternative":null,"client_nonce":"n1","imported_by":null,"replayed_from":"63874d0cc87c47e73a6a68204212f3d1ec68ac78dc28b23deb8f36ce24cdab15","briefing":"e263a9d34e74e876d9a1cbcd3faaa3c213e189b5724b422be74e6f75b45922ab"},"source_encoding":2}}"#;
/// The encoding-1 body of the block residue of `block(false)`.
const RB: &str = "000391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf07632d374b51324d05636c61696d0001560b9a5d3d1128e0e2cdeceaac096c557fa32fa900eddaf07aaa65ab1f7f193d220000";
/// Its frame hash.
const RB_ID: &str = "6e2e7927a253e711cbfded9298b0213c0c52d6e28dd3afbd13efe4c81b0b00f1";
/// Its JSON.
const RB_JSON: &str = r#"{"block":{"version":"0391648ef4869a34464c0669269fdcece669ac34eaed32b7a54ec2d90c5425cf","id":"c-7KQ2M","block_type":"claim","parent":null,"order":"V","content_hash":"0b9a5d3d1128e0e2cdeceaac096c557fa32fa900eddaf07aaa65ab1f7f193d22","frozen":false,"source_encoding":0}}"#;

// shared fixtures (base-compatible)

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn h(b: &[u8]) -> Hash {
    Hash::of(b)
}

fn prov() -> Provenance {
    Provenance {
        actor: "person:ada".into(),
        run: Some("r1".into()),
        role: Role::Writer,
        at_ms: 1_763_856_000_000,
    }
}

fn block(foreign: bool) -> Block {
    let target = ForeignId::new(RepoId([7; 16]), bid("D-4W1ZR"));
    Block {
        id: bid("c-7KQ2M"),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([("title".to_string(), FieldValue::Str("Port".into()))]),
        body: "Ported text.".into(),
        links: BTreeMap::from([(
            "describes".to_string(),
            vec![Link {
                target: bid("t-MEDGR"),
                part: None,
            }],
        )]),
        pins: BTreeMap::from([(bid("t-MEDGR"), h(b"pinned"))]),
        foreign: if foreign {
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
                        content: h(b"cited content"),
                        encoding: 0,
                    },
                )]),
            }
        } else {
            ForeignParts::default()
        },
        read_identity: None,
    }
}

/// A change of every op kind. `replay` sets the encoding-1 options; `foreign` embeds a block
/// with a foreign part.
fn change(replay: bool, foreign: bool) -> Change {
    Change {
        view: "work".into(),
        ops: vec![
            Op::Create {
                block: block(foreign),
            },
            Op::Update {
                base: h(b"base"),
                block: block(false),
            },
            Op::Delete {
                base: h(b"gone"),
                id: bid("q-DEN99"),
            },
        ],
        deps: vec![h(b"dep")],
        provenance: prov(),
        justification: Some(Justification::Implements(vec![bid("D-4W1ZR")])),
        message: "carry the claim".into(),
        links: ChangeLinks {
            client_nonce: Some("n1".into()),
            replayed_from: replay.then(|| h(b"replayed")),
            briefing: replay.then(|| h(b"briefing")),
            ..ChangeLinks::default()
        },
        read_identity: None,
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

fn source() -> PortedFrom {
    PortedFrom {
        repo_id: RepoId([9; 16]),
        change: h(b"source change"),
    }
}

/// `c` as a port of [`source`].
fn ported(mut c: Change) -> Change {
    c.links.ported_from = Some(source());
    c
}

/// The postcard bytes of `Some(source())`: the option tag, the 16 repo-id bytes, the 32 hash
/// bytes.
fn source_tail() -> Vec<u8> {
    let mut t = vec![1u8];
    t.extend([9u8; 16]);
    t.extend(h(b"source change").0);
    t
}

fn stored(version: u16, body: &[u8]) -> EncodedIdentity {
    EncodedIdentity {
        hash: encoded_hash(version, body),
        version,
    }
}

#[test]
fn hpt1_plain_and_foreign_ports_choose_three_and_four() {
    // a plain port: encoding 3, the encoding-1 body with ported_from appended to its links
    let plain = ported(change(true, false));
    assert_eq!(plain.encoding(), CHANGE_PORT_ENCODING);
    assert_eq!(CHANGE_PORT_ENCODING, 3);
    let body3 = change_body(&plain, 3).unwrap();
    let mut want = unhex(C1);
    want.extend(source_tail());
    assert_eq!(body3, want, "the seventh link field ends the body");
    assert_eq!(enc(&PortChangeV3::of(&plain)), body3);
    assert_eq!(plain.id(), encoded_hash(3, &body3));
    let back = postcard::from_bytes::<PortChangeV3>(&body3)
        .unwrap()
        .into_current(stored(3, &body3));
    assert_eq!(back, plain);
    assert_eq!(back.id(), plain.id());
    assert_eq!(back.links.ported_from, Some(source()));

    // a port with a foreign part: encoding 4, the encoding-2 body with ported_from appended
    let foreign = ported(change(true, true));
    assert_eq!(foreign.encoding(), CHANGE_PORT_FOREIGN_ENCODING);
    assert_eq!(CHANGE_PORT_FOREIGN_ENCODING, 4);
    let body4 = change_body(&foreign, 4).unwrap();
    let mut want = unhex(C2);
    want.extend(source_tail());
    assert_eq!(body4, want);
    assert_eq!(enc(&PortForeignChangeV4::of(&foreign)), body4);
    assert_eq!(foreign.id(), encoded_hash(4, &body4));
    let back = postcard::from_bytes::<PortForeignChangeV4>(&body4)
        .unwrap()
        .into_current(stored(4, &body4));
    assert_eq!(back, foreign);
    assert!(back.embeds_foreign());

    // the choice is by value: no port source keeps 1 and 2
    assert_eq!(change(true, false).encoding(), 1);
    assert_eq!(change(true, true).encoding(), 2);
    // a value has a form only in the encoding it chooses (and 0 has no port form)
    for v in [0, 1, 2, 4, 5] {
        assert_eq!(change_body(&plain, v), None, "plain port in {v}");
    }
    for v in [0, 1, 2, 3, 5] {
        assert_eq!(change_body(&foreign, v), None, "foreign port in {v}");
    }
    assert_eq!(change_body(&change(true, false), 3), None);
    assert_eq!(change_body(&change(true, true), 4), None);
    assert!(LegacyChangeV0::from_current(&ported(change(false, false))).is_none());
    // the released postcard form of the runtime links never writes a seventh field: a port's
    // postcard form is its encoding-3 body, never an encoding-1 one
    assert_eq!(
        Some(postcard::to_allocvec(&plain).unwrap()),
        change_body(&plain, 3)
    );
    assert!(ChangeLinksV1::from_current(&plain.links).is_none());
    // the port's source and its replay are alike for merge callers
    assert!(plain.is_replay_like());
    assert!(change(true, false).is_replay_like());
    assert!(!change(false, false).is_replay_like());
    let mut only_port = ported(change(false, false));
    assert!(only_port.is_replay_like());
    only_port.links.ported_from = None;
    assert!(!only_port.is_replay_like());

    // JSON writes the source as hex and leaves an absent one out
    let json = serde_json::to_value(&plain.links).unwrap();
    assert_eq!(
        json["ported_from"],
        serde_json::json!({
            "repo_id": "09090909090909090909090909090909",
            "change": h(b"source change").to_string(),
        })
    );
    let back: ChangeLinks = serde_json::from_value(json).unwrap();
    assert_eq!(back, plain.links);
    let unported = serde_json::to_value(&change(true, false).links).unwrap();
    assert!(unported.get("ported_from").is_none(), "{unported}");
    let back: ChangeLinks = serde_json::from_value(unported).unwrap();
    assert_eq!(back, change(true, false).links);
}

#[test]
fn hpt1_old_changes_keep_bytes_and_hashes() {
    // encoding 0: the frozen four-option links
    let c0 = change(false, false);
    assert_eq!(change_body(&c0, 0).unwrap(), unhex(C0));
    assert_eq!(encoded_hash(0, &unhex(C0)).to_string(), C0_ID);
    let back = postcard::from_bytes::<LegacyChangeV0>(&unhex(C0))
        .unwrap()
        .into_current(stored(0, &unhex(C0)));
    assert_eq!(back, c0);
    assert_eq!(back.id().to_string(), C0_ID);
    assert_eq!(back.links.ported_from, None);
    assert_eq!(change_body(&back, 0).unwrap(), unhex(C0));

    // encoding 1: the frozen six link fields, the runtime postcard form
    let c1 = change(true, false);
    assert_eq!(c1.encoding(), 1);
    assert_eq!(change_body(&c1, 1).unwrap(), unhex(C1));
    assert_eq!(enc(&c1), unhex(C1));
    assert_eq!(c1.id().to_string(), C1_ID);
    let back: Change = postcard::from_bytes(&unhex(C1)).unwrap();
    assert_eq!(back, c1);
    assert_eq!(back.links.ported_from, None);
    assert_eq!(
        enc(&ChangeLinksV1::from_current(&c1.links).unwrap()),
        enc(&c1.links)
    );
    // a ported body is no encoding-1 body: its seventh field is a trailing byte run
    let mut longer = unhex(C1);
    longer.extend(source_tail());
    assert!(postcard::from_bytes::<Change>(&longer).is_ok_and(|c| c == c1));
    assert_ne!(encoded_hash(1, &longer), c1.id());

    // encoding 2: the foreign layout, its links the frozen six fields
    let c2 = change(true, true);
    assert_eq!(c2.encoding(), 2);
    assert_eq!(change_body(&c2, 2).unwrap(), unhex(C2));
    assert_eq!(enc(&ForeignChangeV2::of(&c2)), unhex(C2));
    assert_eq!(c2.id().to_string(), C2_ID);
    let back = postcard::from_bytes::<ForeignChangeV2>(&unhex(C2))
        .unwrap()
        .into_current(stored(2, &unhex(C2)));
    assert_eq!(back, c2);
    assert_eq!(back.id().to_string(), C2_ID);
    assert_eq!(back.links.ported_from, None);
}

#[test]
fn hpr1_residue_two_appends_origin_after_source_encoding() {
    for (c, source_encoding) in [
        (ported(change(true, false)), 3u8),
        (ported(change(true, true)), 4u8),
    ] {
        let r = RedactionResidue::Change(ChangeResidue::of(&c));
        let RedactionResidue::Change(cr) = &r else {
            unreachable!()
        };
        assert_eq!(cr.change, c.id());
        assert_eq!(cr.source_encoding, u16::from(source_encoding));
        assert_eq!(cr.links.ported_from, Some(source()));
        assert_eq!(r.encoding(), RESIDUE_PORT_ENCODING);
        assert_eq!(RESIDUE_PORT_ENCODING, 2);

        // the encoding-1 body of the same residue with no source, then the source
        let mut without = r.clone();
        if let RedactionResidue::Change(c) = &mut without {
            c.links.ported_from = None;
        }
        assert_eq!(without.encoding(), RESIDUE_ENCODING);
        let v1 = enc(&without);
        assert_eq!(*v1.last().unwrap(), source_encoding, "v1 ends with it");
        let port = PortResidueV2::of(&r).unwrap();
        let body = enc(&port);
        let mut want = v1.clone();
        want.extend(source_tail());
        assert_eq!(body, want, "the source follows source_encoding");
        let mut buf = vec![0u8; 4096];
        assert_eq!(r.body_into(&mut buf).unwrap(), &body[..]);
        let back = postcard::from_bytes::<PortResidueV2>(&body)
            .unwrap()
            .into_current();
        assert_eq!(back, r);
        // the residue's own (encoding-1) postcard form has no seventh field
        assert!(postcard::to_allocvec(&r).is_err());
        // the projection keeps the source
        let projected = cr
            .project(|id, v| {
                Ok::<_, ()>(Block::redacted(
                    id.clone(),
                    "claim".into(),
                    None,
                    "V".into(),
                    RedactedBlockIdentity {
                        version: v,
                        content_hash: v,
                        frozen: false,
                        request: h(b"request"),
                    },
                ))
            })
            .unwrap();
        assert_eq!(projected.links.ported_from, Some(source()));
        assert_eq!(projected.id(), c.id());

        // JSON shows the source as hex
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            json["change"]["links"]["ported_from"],
            serde_json::json!({
                "repo_id": "09090909090909090909090909090909",
                "change": h(b"source change").to_string(),
            })
        );
        let back: RedactionResidue = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }
    // only a port's residue: a block residue has no encoding-2 form
    let block = RedactionResidue::Block(BlockResidue::of(&block(false)));
    assert_eq!(block.encoding(), RESIDUE_ENCODING);
    assert!(PortResidueV2::of(&block).is_none());
    // a decoded encoding-2 body that names no source chooses encoding 1, so a reader refuses it
    let empty = PortResidueV2 {
        residue: RedactionResidue::Change(ChangeResidue::of(&change(true, false))),
        ported_from: None,
    };
    let back = postcard::from_bytes::<PortResidueV2>(&enc(&empty))
        .unwrap()
        .into_current();
    assert_eq!(back.encoding(), RESIDUE_ENCODING);
}

#[test]
fn hpr1_old_residue_bytes_hashes_and_json_stay_fixed() {
    // historical encoding 0 reads, keeps its bytes, and names source encoding 0
    let frozen: LegacyRedactionResidueV0 = postcard::from_bytes(&unhex(R0)).unwrap();
    assert_eq!(enc(&frozen), unhex(R0));
    assert_eq!(encoded_hash(0, &unhex(R0)).to_string(), R0_ID);
    let r0 = frozen.into_current();
    let mut want = ChangeResidue::of(&change(false, false));
    want.source_encoding = 0;
    assert_eq!(r0, RedactionResidue::Change(want));
    assert_eq!(
        enc(&LegacyRedactionResidueV0::from_current(&r0).unwrap()),
        unhex(R0)
    );

    // an unported change residue stays encoding 1, byte for byte, JSON for JSON
    for (c, bytes, id, json) in [
        (change(true, false), R1, R1_ID, R1_JSON),
        (change(true, true), R12, R12_ID, R12_JSON),
    ] {
        let r = RedactionResidue::Change(ChangeResidue::of(&c));
        assert_eq!(r.encoding(), RESIDUE_ENCODING);
        assert_eq!(enc(&r), unhex(bytes));
        let mut buf = vec![0u8; 4096];
        assert_eq!(r.body_into(&mut buf).unwrap(), &unhex(bytes)[..]);
        assert_eq!(encoded_hash(1, &unhex(bytes)).to_string(), id);
        assert_eq!(serde_json::to_string(&r).unwrap(), json);
        assert_eq!(
            postcard::from_bytes::<RedactionResidue>(&unhex(bytes)).unwrap(),
            r
        );
        assert_eq!(serde_json::from_str::<RedactionResidue>(json).unwrap(), r);
    }

    // a block residue stays encoding 1
    let rb = RedactionResidue::Block(BlockResidue::of(&block(false)));
    assert_eq!(rb.encoding(), RESIDUE_ENCODING);
    assert_eq!(enc(&rb), unhex(RB));
    assert_eq!(encoded_hash(1, &unhex(RB)).to_string(), RB_ID);
    assert_eq!(serde_json::to_string(&rb).unwrap(), RB_JSON);
}
