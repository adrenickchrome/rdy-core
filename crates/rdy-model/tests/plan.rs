//! cp1 (D-4JMPH, D-F0904, p-HF5W0, p-S33GZ): the PLAN record's wire order, the plan-item
//! identity, the four appended answer acts, and the engine-owned block fields `reaffirmed` and
//! `reflects_at`, which change a block's version but not its content and hold no references.
use rdy_model::engine_fields::{self, Against, REAFFIRMED, REFLECTS_AT, Reaffirmation, ReflectsAt};
use rdy_model::plan::{
    self, Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem,
};
use rdy_model::*;
use std::collections::{BTreeMap, BTreeSet};

fn enc<T: serde::Serialize>(t: &T) -> Vec<u8> {
    postcard::to_allocvec(t).unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A length-prefixed string, as postcard writes one shorter than 128 bytes.
fn s(text: &str) -> Vec<u8> {
    assert!(text.len() < 128);
    [&[text.len() as u8][..], text.as_bytes()].concat()
}

fn fill(b: u8) -> Vec<u8> {
    vec![b; 32]
}

/// One fixed plan with every field and enum position exercised.
fn fixed_plan() -> Plan {
    Plan {
        stable: None,
        prev: Some(Hash([1; 32])),
        view: "work".into(),
        proposal: None,
        answers: vec![PlanAnswer {
            item: Hash([2; 32]),
            act: PlanAct::Defer,
            reason: Some("later".into()),
            subject: None,
            cause: Hash([3; 32]),
            by: Provenance {
                actor: "human:nick".into(),
                run: None,
                role: Role::Approver,
                at_ms: 5,
            },
        }],
        heads: vec![PlanHead {
            repo: "core".into(),
            head: None,
        }],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: Some("r1".into()),
            role: Role::Writer,
            at_ms: -1,
        },
        seal: Some(PlanSeal {
            landing: Hash([4; 32]),
            version: Hash([5; 32]),
            decisions: vec![id("D-W2HXR")],
            items: vec![SealedItem {
                id: Hash([6; 32]),
                kind: PlanKind::UpdateImplementation,
                subject: "s".into(),
                cause: id("D-W2HXR"),
                edge: "implements".into(),
                outcome: PlanOutcome::Open,
                answer: Some(Hash([2; 32])),
            }],
        }),
    }
}

/// D-F0904: a PLAN encodes its fields in declaration order, each enum by its fixed variant index.
/// The expected bytes are written out field by field from postcard's rules, not taken from the
/// encoder. The plan-item identity is BLAKE3 over a preimage built here independently.
#[test]
fn cp1_plan_wire_order_and_item_domain_are_fixed() {
    let expected = [
        vec![0x00],                     // stable: None
        [vec![0x01], fill(1)].concat(), // prev: Some
        s("work"),                      // view
        vec![0x00],                     // proposal: None
        vec![0x01],                     // answers: 1
        fill(2),                        // .item
        vec![0x01],                     // .act: Defer
        [vec![0x01], s("later")].concat(),
        vec![0x00], // .subject: None
        fill(3),    // .cause
        s("human:nick"),
        vec![0x00, 0x03, 0x0a], // run None, Approver, at_ms 5 (zigzag 10)
        vec![0x01],             // heads: 1
        s("core"),
        vec![0x00], // head: None (unborn)
        s("agent:a"),
        [vec![0x01], s("r1")].concat(),
        vec![0x01, 0x01], // Writer, at_ms -1 (zigzag 1)
        vec![0x01],       // seal: Some
        fill(4),          // .landing
        fill(5),          // .version
        vec![0x01],       // .decisions: 1
        s("D-W2HXR"),
        vec![0x01], // .items: 1
        fill(6),    // .id
        vec![0x04], // .kind: UpdateImplementation
        s("s"),
        s("D-W2HXR"),
        s("implements"),
        vec![0x09], // .outcome: Open
        [vec![0x01], fill(2)].concat(),
    ]
    .concat();
    let p = fixed_plan();
    assert_eq!(hex(&enc(&p)), hex(&expected));
    assert_eq!(p.id(), Hash::of(&expected));
    let back: Plan = postcard::from_bytes(&expected).unwrap();
    assert_eq!(back, p);

    let kinds = [
        (PlanKind::Update, "update"),
        (PlanKind::Rejustify, "rejustify"),
        (PlanKind::Reaffirm, "reaffirm"),
        (PlanKind::Confirm, "confirm"),
        (PlanKind::UpdateImplementation, "update_implementation"),
    ];
    for (i, (k, name)) in kinds.iter().enumerate() {
        assert_eq!(enc(k), vec![i as u8], "{k:?}");
        assert_eq!(k.as_str(), *name);
    }
    let acts = [
        (PlanAct::Confirm, "confirm"),
        (PlanAct::Defer, "defer"),
        (PlanAct::Done, "done"),
    ];
    for (i, (a, name)) in acts.iter().enumerate() {
        assert_eq!(enc(a), vec![i as u8], "{a:?}");
        assert_eq!(a.as_str(), *name);
    }
    let outcomes = [
        (PlanOutcome::Updated, "updated"),
        (PlanOutcome::Relinked, "relinked"),
        (PlanOutcome::Repinned, "repinned"),
        (PlanOutcome::Reaffirmed, "reaffirmed"),
        (PlanOutcome::Retired, "retired"),
        (PlanOutcome::Committed, "committed"),
        (PlanOutcome::Confirmed, "confirmed"),
        (PlanOutcome::Done, "done"),
        (PlanOutcome::Deferred, "deferred"),
        (PlanOutcome::Open, "open"),
    ];
    for (i, (o, name)) in outcomes.iter().enumerate() {
        assert_eq!(enc(o), vec![i as u8], "{o:?}");
        assert_eq!(o.as_str(), *name);
    }

    for (k, name) in kinds {
        let preimage = [
            &b"rdy-plan-item\0"[..],
            b"D-W2HXR",
            b"\0",
            b"c-7KQ2M",
            b"\0",
            name.as_bytes(),
        ]
        .concat();
        let h = plan::item_hash(&id("D-W2HXR"), "c-7KQ2M", k);
        assert_eq!(h, Hash(*blake3::hash(&preimage).as_bytes()), "{k:?}");
        let full = h.to_string();
        assert_eq!(plan::item_display(&h), format!("pi-{}", &full[..16]));
    }
    // the separators keep the parts apart
    assert_ne!(
        plan::item_hash(&id("D-W2HXR"), "ab", PlanKind::Update),
        plan::item_hash(&id("D-W2HXR"), "a", PlanKind::Update)
    );

    // O1: a head is a full lowercase SHA-1 or SHA-256 object id
    assert!(plan::valid_git_hash(&"a".repeat(40)));
    assert!(plan::valid_git_hash(&"0".repeat(64)));
    for bad in [
        "",
        &"a".repeat(39),
        &"a".repeat(41),
        &"A".repeat(40),
        &"g".repeat(40),
    ] {
        assert!(!plan::valid_git_hash(bad), "{bad}");
    }
}

/// The encoding of `full_answer()`, pinned when the ANSWER kind was added (store format 11):
/// the same literal as `repr.rs`'s, repeated so this test stands alone.
const ANSWER_HEX: &str = concat!(
    "09502d4141414141403103010672656a656374010652656a6563740307436f6e6669726d0652656a656374054c",
    "6174657201bccd670673e993991bd31a5245563902bcea170b4f6e4646c8a5b2623b4c26f0010538661423236a",
    "5d8df6286a8633369e2a03c5f90d48eded333222dbee8cdbdd0a68756d616e3a6e69636b00038080e682b96601",
    "0b6167656e743a72656c6179010572756e2d3901106e6f2c206e6f742074686174206f6e6506736573732d310001010109746f6f206561726c79",
);

fn full_answer(act: AnswerAct) -> Answer {
    Answer {
        item: "P-AAAAA@1".into(),
        act,
        option: Some("reject".into()),
        label: Some("Reject".into()),
        options: vec!["Confirm".into(), "Reject".into(), "Later".into()],
        card_hash: Some(Hash::of(b"card")),
        targets: vec![Hash::of(b"t")],
        person: Provenance {
            actor: "human:nick".into(),
            run: None,
            role: Role::Approver,
            at_ms: 1_760_000_000_000,
        },
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: Some("run-9".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "no, not that one".into(),
            session_ref: "sess-1".into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Card),
        note: Some("too early".into()),
    }
}

/// D-F0904: the ten old acts keep their indices and names, the four new ones append at 10 to 13,
/// and an old answer's bytes are unchanged. Only the act's byte differs between acts.
#[test]
fn cp1_old_answer_discriminants_and_record_bytes_stay_fixed() {
    let acts = [
        (AnswerAct::RejectProposal, "reject_proposal"),
        (AnswerAct::Later, "later"),
        (AnswerAct::Confirm, "confirm"),
        (AnswerAct::RejectDecision, "reject_decision"),
        (AnswerAct::AnswerQuestion, "answer_question"),
        (AnswerAct::Dismiss, "dismiss"),
        (AnswerAct::Tag, "tag"),
        (AnswerAct::ViewOwn, "view_own"),
        (AnswerAct::CloseStale, "close_stale"),
        (AnswerAct::Propose, "propose"),
        (AnswerAct::KeepSuspect, "keep_suspect"),
        (AnswerAct::AcceptDeferrals, "accept_deferrals"),
        (AnswerAct::RefuseDeferrals, "refuse_deferrals"),
        (AnswerAct::RejectReaffirm, "reject_reaffirm"),
    ];
    for (i, (a, name)) in acts.iter().enumerate() {
        assert_eq!(enc(a), vec![i as u8], "{a:?}");
        assert_eq!(a.as_str(), *name);
        let back: AnswerAct = postcard::from_bytes(&[i as u8]).unwrap();
        assert_eq!(back, *a);
    }
    // 14 to 16 are the carried acts (ok4_new_shapes_have_fixed_ordinals_and_bytes); 17 is none
    assert!(postcard::from_bytes::<AnswerAct>(&[17]).is_err());
    assert_eq!(
        hex(&enc(&full_answer(AnswerAct::RejectDecision))),
        ANSWER_HEX
    );
    // the act is the byte after the item ref
    let item_len = 1 + "P-AAAAA@1".len();
    for (i, (a, _)) in acts.iter().enumerate() {
        let mut want = enc(&full_answer(AnswerAct::RejectDecision));
        want[item_len] = i as u8;
        assert_eq!(enc(&full_answer(*a)), want, "{a:?}");
    }
}

/// The golden block of `golden.rs`, which has neither engine field.
fn golden_block() -> Block {
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
        id: id("c-7KQ2M"),
        block_type: "claim".into(),
        parent: Some(id("s-1DENT")),
        order: "V".into(),
        fields,
        body: "Balances live on the {{t-MEDGR}}.".into(),
        links: BTreeMap::from([(
            "describes".to_string(),
            vec![Link {
                target: id("t-MEDGR"),
                part: Some("balance".into()),
            }],
        )]),
        pins: BTreeMap::from([(id("t-MEDGR"), Hash::of(b"pinned"))]),
        read_identity: None,
    }
}

fn custom_tracking() -> Tracking {
    Tracking::new(BTreeMap::from([(
        "claim".to_string(),
        BTreeSet::from(["d_str".to_string()]),
    )]))
}

fn reaffirmed() -> BTreeMap<BlockId, Reaffirmation> {
    BTreeMap::from([
        (
            id("t-MEDGR"),
            Reaffirmation {
                against: Against::Retiring(vec![id("D-0AAAA"), id("D-W2HXR")]),
                reason: "Still true after {{D-W2HXR}} retires.".into(),
            },
        ),
        (
            id("c-1BBBB"),
            Reaffirmation {
                against: Against::Deleted,
                reason: "Kept on purpose.".into(),
            },
        ),
        (
            id("c-2CCCC"),
            Reaffirmation {
                against: Against::Retiring(vec![]),
                reason: "Changed target; {{c-2CCCC|@key}} still holds.".into(),
            },
        ),
    ])
}

fn reflects_at() -> ReflectsAt {
    ReflectsAt {
        files: Hash::of(b"files"),
        commits: BTreeMap::from([
            ("core".to_string(), Some("a".repeat(40))),
            ("hub".to_string(), None),
            ("web".to_string(), Some("b".repeat(64))),
        ]),
    }
}

fn with_metadata(mut b: Block) -> Block {
    b.fields.insert(
        REAFFIRMED.into(),
        engine_fields::encode_reaffirmed(&reaffirmed()),
    );
    b.fields.insert(
        REFLECTS_AT.into(),
        engine_fields::encode_reflects_at(&reflects_at()),
    );
    b
}

/// p-Z8ZHE: the engine fields are metadata. Adding or changing them gives a new version hash
/// and leaves the content hash (default and with tracking fields) as it was, so no pin goes stale.
#[test]
fn cp1_metadata_changes_version_but_not_content() {
    let plain = golden_block();
    let meta = with_metadata(plain.clone());
    assert_ne!(meta.version_hash(), plain.version_hash());
    assert_eq!(meta.content_hash(), plain.content_hash());
    for t in [Tracking::default(), custom_tracking()] {
        assert_eq!(
            meta.content_hash_with(&t),
            plain.content_hash_with(&t),
            "{t:?}"
        );
    }
    // each field alone
    for name in [REAFFIRMED, REFLECTS_AT] {
        let mut one = plain.clone();
        one.fields.insert(name.into(), meta.fields[name].clone());
        assert_ne!(one.version_hash(), plain.version_hash(), "{name}");
        assert_eq!(one.content_hash(), plain.content_hash(), "{name}");
        assert_eq!(
            one.content_hash_with(&custom_tracking()),
            plain.content_hash_with(&custom_tracking()),
            "{name}"
        );
    }
    // a changed reason is a new version with the same content
    let mut changed = reaffirmed();
    changed.get_mut(&id("c-1BBBB")).unwrap().reason = "Kept again.".into();
    let mut again = meta.clone();
    again.fields.insert(
        REAFFIRMED.into(),
        engine_fields::encode_reaffirmed(&changed),
    );
    assert_ne!(again.version_hash(), meta.version_hash());
    assert_eq!(again.content_hash(), meta.content_hash());
    // an ordinary field still changes both, and a tracking one only the version
    let mut ordinary = meta.clone();
    ordinary.fields.insert("c_int".into(), FieldValue::Int(7));
    assert_ne!(ordinary.content_hash(), meta.content_hash());
    let mut tracked = meta.clone();
    tracked
        .fields
        .insert("d_str".into(), FieldValue::Str("other".into()));
    assert_ne!(tracked.content_hash(), meta.content_hash());
    assert_eq!(
        tracked.content_hash_with(&custom_tracking()),
        meta.content_hash_with(&custom_tracking())
    );
    // the helpers read back what they wrote
    assert_eq!(
        engine_fields::decode_reaffirmed(&meta.fields[REAFFIRMED]),
        Ok(reaffirmed())
    );
    assert_eq!(
        engine_fields::decode_reflects_at(&meta.fields[REFLECTS_AT]),
        Ok(reflects_at())
    );
}

/// The base encoding of a block without the engine fields, pinned at the base commit: Block's
/// bytes and its version hash stay what they were. `GOLDEN_CONTENT` and
/// `GOLDEN_CONTENT_TRACKED` are its historical content-1 hashes, which [`content_1`] recomputes;
/// the content-2 hashes are `GOLDEN_CONTENT_2` and `GOLDEN_CONTENT_2_TRACKED`.
const GOLDEN_BLOCK_HEX: &str = concat!(
    "07632d374b51324d05636c61696d0107732d3144454e5401560606615f6e756c6c0006625f626f6f6c010105635f696e74",
    "025305645f737472030bc39c6ec3af63c3b864c3a906655f6c6973740402020203017805665f6d61700501016b0100",
    "2142616c616e636573206c697665206f6e20746865207b7b742d4d454447527d7d2e01096465736372696265730107",
    "742d4d45444752010762616c616e63650107742d4d45444752e4e7636c21ce34009ed2398a68e7297fc189ba08cd95",
    "198f60f295d3249ef26e",
);
const GOLDEN_CONTENT: &str = "03ac8d9c5bf3c66abca69e48b866bb5b0069d2ca684a51a330320173e6cf1692";
const GOLDEN_CONTENT_TRACKED: &str =
    "4782c40e22637879d83532792288c708944eb087c542f3023ebe1239e86e3049";
const GOLDEN_CONTENT_2: &str = "4ba94b30d0b0efcce50721247469fa0c71573daccb051117f5f6802ae5d12c08";
const GOLDEN_CONTENT_2_TRACKED: &str =
    "e868067306472b7a38acb73d6aba0772fa6db12a88c584c908b8709989d65939";

/// The retired content-1 hash of `b` (domain `rdy-content-1\0`, with the order key), recomputed
/// here only to check the historical vectors. No production path accepts it.
fn content_1(b: &Block, t: &Tracking) -> Hash {
    #[derive(serde::Serialize)]
    struct Content1<'a> {
        id: &'a BlockId,
        block_type: &'a String,
        parent: &'a Option<BlockId>,
        order: &'a String,
        fields: Vec<(&'a String, &'a FieldValue)>,
        body: &'a String,
        links: &'a BTreeMap<String, Vec<Link>>,
    }
    let tracked = t.fields(&b.block_type);
    let content = Content1 {
        id: &b.id,
        block_type: &b.block_type,
        parent: &b.parent,
        order: &b.order,
        fields: b
            .fields
            .iter()
            .filter(|(k, _)| !["status", "decided_at", "lifecycle"].contains(&k.as_str()))
            .filter(|(k, _)| !engine_fields::is_engine_field(k))
            .filter(|(k, _)| !tracked.is_some_and(|f| f.contains(k.as_str())))
            .collect(),
        body: &b.body,
        links: &b.links,
    };
    let mut bytes = b"rdy-content-1\0".to_vec();
    bytes.extend(enc(&content));
    Hash::of(&bytes)
}

#[test]
fn cp1_old_block_encoding_is_unchanged() {
    let b = golden_block();
    assert!(!b.fields.contains_key(REAFFIRMED) && !b.fields.contains_key(REFLECTS_AT));
    assert_eq!(hex(&enc(&b)), GOLDEN_BLOCK_HEX);
    assert_eq!(
        b.version_hash().to_string(),
        "3900f27b4ae5bff77eb9b5e6313e5f30b77df8fb3274ce3d56e1b8074356d71e"
    );
    assert_eq!(b.content_hash().to_string(), GOLDEN_CONTENT_2);
    assert_eq!(
        b.content_hash_with(&custom_tracking()).to_string(),
        GOLDEN_CONTENT_2_TRACKED
    );
    // the historical content-1 vectors, by the legacy calculation
    assert_eq!(
        content_1(&b, &Tracking::default()).to_string(),
        GOLDEN_CONTENT
    );
    assert_eq!(
        content_1(&b, &custom_tracking()).to_string(),
        GOLDEN_CONTENT_TRACKED
    );
    assert_ne!(GOLDEN_CONTENT_2, GOLDEN_CONTENT);
    let back: Block = postcard::from_bytes(&enc(&b)).unwrap();
    assert_eq!(back, b);
}

/// p-RNK9B: text inside the engine fields is never a reference, at any depth, so a reason that
/// names `{{D-x}}` adds no graph edge. The same text in an ordinary field still is one.
#[test]
fn cp1_metadata_text_is_not_a_reference() {
    let mut b = golden_block();
    b.body = String::new();
    b.fields.clear();
    let meta = with_metadata(b.clone());
    assert!(meta.ref_tokens().is_empty(), "{:?}", meta.ref_tokens());
    // nested deeper than the engine shapes: still nothing
    let mut deep = b.clone();
    deep.fields.insert(
        REAFFIRMED.into(),
        FieldValue::Map(BTreeMap::from([(
            "x".to_string(),
            FieldValue::List(vec![FieldValue::Map(BTreeMap::from([(
                "y".to_string(),
                FieldValue::Str("{{D-W2HXR}}".into()),
            )]))]),
        )])),
    );
    deep.fields
        .insert(REFLECTS_AT.into(), FieldValue::Str("{{t-MEDGR}}".into()));
    assert!(deep.ref_tokens().is_empty(), "{:?}", deep.ref_tokens());
    // control: an ordinary field with the same text is scanned
    let mut ordinary = meta.clone();
    ordinary.fields.insert(
        "note".into(),
        FieldValue::Map(BTreeMap::from([(
            "why".to_string(),
            FieldValue::Str("Still true after {{D-W2HXR}} retires.".into()),
        )])),
    );
    assert_eq!(
        ordinary.ref_tokens(),
        BTreeSet::from(["D-W2HXR".to_string()])
    );
}
