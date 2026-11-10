//! rpl3 (repo links, D-FK58A, D-2WY7Y, D-C94DV): the store feature `foreign_copy`, kind 29 and
//! the optional repo-links encodings. Every local value keeps its released encoding, bytes and
//! hash; a value takes its kind's optional encoding only for a foreign shape; the first record
//! that uses the foreign copy turns the feature on in its own batch; a released encoding never
//! carries a foreign shape and a load that hides the feature is refused; an encoding past the
//! registered ones is a bounded `E_STORE_FORMAT` refusal.
#[path = "support/carried.rs"]
mod carried_fixture;

use rdy_model::assessment::*;
use rdy_model::encoding::{ForeignBlockV1, ForeignChangeV2, encoded_hash};
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignId, ForeignLink, ForeignParts, ForeignPin,
    ForeignPublication, ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::redaction::{
    AssessmentSupport, ReasonClass, RedactTarget, Redaction, RedactionManifest,
};
use rdy_model::review::{ReviewSubjectV1, ReviewV1};
use rdy_model::semantic::{ForeignPairItem, PairItem, PairSide, SemanticTensionV1, pair_key};
use rdy_model::*;
use rdy_store::bounded::{self, RecordError};
use rdy_store::codec::{self, VERSIONED};
use rdy_store::dump::{DumpBatch, Frame};
use rdy_store::format::{
    self, FEATURE_FOREIGN_COPY, FEATURE_REQUIRED_RDY, FOREIGN_COPY_REQUIRED_RDY, FeatureClass,
    REF_STORE_FEATURES, StoreFeatures, baseline_features, registered_features,
};
use rdy_store::ingest::Ingest;
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectRef, portable_refs_of, refs_of};
use rdy_store::redaction_carried;
use rdy_store::transition::{
    Author, Cause, SideV1, SuspectMove, SuspectTarget, Transitions, TransitionsV1, Why,
};
use rdy_store::{Store, StoreError};
use serde::Serialize;
use std::collections::BTreeMap;

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn repo(n: u8) -> RepoId {
    RepoId([n; 16])
}

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("run-1".into()),
        role,
        at_ms: 1_760_000_000_000,
    }
}

fn block_of(id_: &str, block_type: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: block_type.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn local_block() -> Block {
    block_of("c-00001", "claim", "a local claim")
}

fn cited() -> ForeignId {
    ForeignId::new(repo(7), id("D-00001"))
}

/// A block that cites a delivered decision: one foreign link and its pin.
fn foreign_block() -> Block {
    let mut b = block_of("c-00002", "claim", "cites a delivered decision");
    b.foreign = ForeignParts {
        links: BTreeMap::from([(
            "governed_by".to_string(),
            vec![ForeignLink {
                target: cited(),
                part: None,
            }],
        )]),
        pins: BTreeMap::from([(
            cited(),
            ForeignPin {
                content: h(70),
                encoding: 0,
            },
        )]),
    };
    b
}

fn change_of(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: prov("agent:w", Role::Writer),
        justification: None,
        message: "a change".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn answer(act: AnswerAct) -> Answer {
    Answer {
        item: "adopt:item".into(),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: Some(h(30)),
        targets: vec![],
        person: prov("human:alice", Role::Approver),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

fn why(cause: Cause, id: Option<Hash>) -> Why {
    Why {
        cause,
        id,
        actor: None,
        run: None,
    }
}

/// One new suspect, moved by `cause`.
fn transitions(cause: Cause, by: Option<Hash>) -> Transitions {
    Transitions {
        at_ms: 5,
        resolved: vec![],
        opened: vec![],
        gone: vec![],
        new: vec![SuspectMove {
            block: id("c-00001"),
            target: id("c-00003"),
            reason: "pinned content changed".into(),
            writers: vec![Author {
                actor: "agent:w".into(),
                run: None,
            }],
            why: why(cause, by),
        }],
    }
}

fn pair_items() -> Vec<PairItem> {
    vec![
        PairItem {
            block: id("c-00001"),
            content: h(40),
        },
        PairItem {
            block: id("c-00003"),
            content: h(41),
        },
    ]
}

fn pair_review() -> Review {
    let items = pair_items();
    Review {
        subject: ReviewSubject::Pair {
            key: pair_key(&items),
            items,
        },
        case_hash: Some(h(42)),
        verdict: Verdict::Tension,
        reason: "the two disagree".into(),
        citations: vec![],
        provenance: prov("agent:r", Role::Reviewer),
    }
}

/// A local pair review whose second side is a cited block.
fn foreign_review() -> ReviewV1 {
    let mut r = ReviewV1::of(&pair_review());
    if let ReviewSubjectV1::Pair { items, .. } = &mut r.subject {
        items[1] = PairSide::Foreign(ForeignPairItem {
            target: cited(),
            content: h(43),
        });
    }
    r
}

fn tension() -> SemanticTension {
    let items = pair_items();
    SemanticTension {
        key: pair_key(&items),
        items,
        review: h(44),
        advice: None,
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: prov("agent:r", Role::Reviewer),
    }
}

fn scores(n: u32) -> AssessmentScores {
    AssessmentScores {
        overall: n,
        grounded: n,
        consistent: n,
        current: n,
        connected: n,
        complete: n,
        governed: n,
    }
}

fn assessment() -> Assessment {
    let blocks = vec![id("c-00001")];
    Assessment {
        proposal: h(50),
        base: None,
        method: 1,
        pack: AssessmentPack {
            name: "rdy-qa".into(),
            version: "1.0.0".into(),
            digest: h(51),
        },
        config: h(52),
        policy: None,
        facts: AssessmentFacts {
            changes: 1,
            consequences: 0,
            plan: AssessmentPlanCounts {
                open: 0,
                closed: 0,
                deferred: 0,
                by_kind: vec![],
            },
            supersedes: vec![],
            reaffirms: vec![],
            reversibility: vec![],
            exceptions: vec![],
        },
        tensions: vec![],
        tensions_omitted: 0,
        suspects: vec![],
        suspects_omitted: 0,
        coverage: AssessmentCoverage {
            pages: vec![],
            corpus: AssessmentScoreChange {
                before: scores(50),
                after: scores(60),
            },
        },
        confidence: AssessmentConfidence {
            level: AssessmentLevel::High,
            reasons: vec![],
        },
        reach: blocks.clone(),
        card: AssessmentCard {
            hash: h(53),
            facts: vec!["Changes: 1".into()],
        },
        scope: AssessmentScope {
            proposal: h(50),
            blocks,
        },
        issues: vec![],
    }
}

fn foreign_assessment() -> AssessmentV2 {
    let mut a = AssessmentV2::of(&assessment());
    a.foreign.reach.push(AssessmentForeignReach {
        target: cited(),
        version: h(54),
    });
    a.foreign.tensions.push(AssessmentForeignTension {
        tension: h(55),
        repo: repo(7),
        entry: h(56),
    });
    a
}

fn redaction(targets: Vec<RedactTarget>) -> Redaction {
    Redaction {
        carrier: h(60),
        actor: prov("human:alice", Role::Approver),
        targets,
        reason_class: ReasonClass::Secret,
        reason: "a secret".into(),
        manifest: RedactionManifest {
            versions: vec![],
            blobs: vec![],
            shared_blobs: vec![],
            spans: vec![],
            inside: vec![],
            outside: vec![],
            quarantine: vec![],
            assessment: AssessmentSupport::Carried,
        },
    }
}

fn foreign_target() -> RedactTarget {
    RedactTarget::ForeignVersion {
        repo: repo(7),
        id: id("D-00001"),
        version: h(61),
    }
}

fn delivery() -> ForeignEntry {
    ForeignEntry {
        repo: repo(7),
        source: ForeignSource::Link {
            link: "rl-00001".into(),
            site: "hub".into(),
            at_ms: 9,
        },
        event: ForeignEvent::Delivery {
            prev: None,
            publication: ForeignPublication {
                tag: "v1".into(),
                profile: "decisions".into(),
                profile_version: None,
            },
            set: None,
            added: vec![ForeignVersion {
                id: id("D-00001"),
                version: h(62),
                content: h(63),
                encoding: 0,
                derived: ForeignDerived::default(),
            }],
            changed: vec![],
            removed: vec![],
            shells: vec![],
            selection: selection_identity([(&id("D-00001"), &h(62))]).unwrap(),
        },
    }
}

/// A named foreign shape: its frame and the encoding it takes.
type Shape = (&'static str, (u8, Vec<u8>), u16);

fn postcard_of<T: Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

/// The stored payload of a frame of encoding `version` with body `body`.
fn payload(version: u16, body: &[u8]) -> Vec<u8> {
    if version == 0 {
        return body.to_vec();
    }
    let mut p = version.to_le_bytes().to_vec();
    p.extend_from_slice(body);
    p
}

/// The raw kind of logical kind `kind` at encoding `version`.
fn raw(kind: u8, version: u16) -> u8 {
    if version == 0 { kind } else { kind | VERSIONED }
}

/// The encoding of a written frame.
fn version_of(item: &(u8, Vec<u8>)) -> u16 {
    codec::split(item.0, &item.1).unwrap().version
}

fn features_of(item: &(u8, Vec<u8>)) -> Vec<String> {
    format::required_features(item.0, &item.1)
        .into_iter()
        .map(|f| f.name)
        .collect()
}

fn log_manifest(s: &Store) -> StoreFeatures {
    let v = s.reader().get_ref(REF_STORE_FEATURES).unwrap().unwrap();
    StoreFeatures::parse(&v).unwrap()
}

fn names(f: &StoreFeatures) -> Vec<&str> {
    f.features.iter().map(|f| f.name.as_str()).collect()
}

/// The frames of the last sealed batch of `s`: (raw kind, ref name of a REF_UPDATE).
fn last_batch(s: &Store) -> Vec<(u8, Option<String>)> {
    let r = s.reader();
    let mut last = None;
    r.for_each_batch(s.log_end(), |b| {
        last = Some(b);
        Ok(())
    })
    .unwrap();
    last.unwrap()
        .frames
        .iter()
        .map(|f| {
            let name = (f.kind() == kinds::REF_UPDATE)
                .then(|| {
                    f.present_payload()
                        .and_then(|p| postcard::from_bytes::<kinds::RefUpdate>(p).ok())
                        .map(|u| u.name)
                })
                .flatten();
            (f.kind(), name)
        })
        .collect()
}

/// Every local value of a kind that gains an optional encoding is written in its kind's released
/// encoding, with the bytes and hash it had before repo links; a wire value with no foreign shape
/// writes the same frame as its runtime value; a stored read keeps its identity. The publication
/// selection identity, which a delivery stores from repo links on (D-C94DV), keeps its frozen
/// vector.
#[test]
fn rpl3_old_objects_keep_identity() {
    // BLOCK: encoding 0, its released body, its model identity
    let b = local_block();
    let item = codec::item_new(kinds::BLOCK, &b);
    assert_eq!(item, (kinds::BLOCK, postcard_of(&b)));
    assert_eq!(Hash::of(&item.1), b.version_hash());
    assert_eq!(b.encoding(), 0);
    // CHANGE: encoding 1, the change's id
    let c = change_of(vec![Op::Create { block: b.clone() }]);
    let item = codec::item(kinds::CHANGE, &c).unwrap();
    assert_eq!(
        item,
        (kinds::CHANGE | VERSIONED, payload(1, &postcard_of(&c)))
    );
    assert_eq!(Hash::of(&item.1), c.id());
    assert_eq!(c.encoding(), 1);
    // ANSWER, TRANSITION, REVIEW, TENSION, REDACTION: encoding 0, their own bytes
    for act in [
        AnswerAct::Confirm,
        AnswerAct::Dispute,
        AnswerAct::AcceptVerdict,
    ] {
        let a = answer(act);
        assert_eq!(
            codec::item_new(kinds::ANSWER, &a),
            (kinds::ANSWER, postcard_of(&a))
        );
        assert_eq!(a.encoding(), 0);
    }
    let t = transitions(Cause::Committed, Some(h(1)));
    assert_eq!(t.frame(), (kinds::TRANSITION, postcard_of(&t)));
    let r = pair_review();
    assert_eq!(
        codec::item_new(kinds::REVIEW, &r),
        (kinds::REVIEW, postcard_of(&r))
    );
    let st = tension();
    assert_eq!(
        codec::item_new(kinds::TENSION, &st),
        (kinds::TENSION, postcard_of(&st))
    );
    let rq = redaction(vec![RedactTarget::AllVersions(id("c-00001"))]);
    assert_eq!(rq.encoding(), 0);
    assert_eq!(
        codec::item_new(kinds::REDACTION, &rq),
        (kinds::REDACTION, postcard_of(&rq))
    );
    // ASSESSMENT: encoding 1, its own body
    let a = assessment();
    assert_eq!(
        codec::item_new(kinds::ASSESSMENT, &a),
        (kinds::ASSESSMENT | VERSIONED, payload(1, &postcard_of(&a)))
    );
    // a wire value with no foreign shape is written as its runtime value
    assert_eq!(
        codec::item_new(kinds::TRANSITION, &TransitionsV1::of(&t)),
        t.frame()
    );
    assert_eq!(
        codec::item_new(kinds::REVIEW, &ReviewV1::of(&r)),
        codec::item_new(kinds::REVIEW, &r)
    );
    assert_eq!(
        codec::item_new(kinds::TENSION, &SemanticTensionV1::of(&st)),
        codec::item_new(kinds::TENSION, &st)
    );
    assert_eq!(
        codec::item_new(kinds::ASSESSMENT, &AssessmentV2::of(&a)),
        codec::item_new(kinds::ASSESSMENT, &a)
    );
    // a local pair keeps its released key: the wire sides do not enter it
    assert_eq!(
        pair_key(&pair_items()),
        Hash::of(format!("rdy-pair-1\0c-00001\0{}\0c-00003\0{}\0", h(40), h(41)).as_bytes())
    );
    // a stored read decodes to the same value, canonical in its encoding, and keeps its hash
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("s")).unwrap();
    let frames = vec![
        codec::item_new(kinds::BLOCK, &b),
        codec::item(kinds::CHANGE, &c).unwrap(),
        codec::item_new(kinds::ANSWER, &answer(AnswerAct::Confirm)),
        codec::item_new(kinds::REVIEW, &r),
        codec::item_new(kinds::TENSION, &st),
        t.frame(),
    ];
    let hashes = s.write(frames.clone()).unwrap();
    let reader = s.reader();
    let got: Block = reader.get_typed(kinds::BLOCK, &hashes[0]).unwrap();
    assert_eq!(got, b);
    let got: Change = reader.get_typed(kinds::CHANGE, &hashes[1]).unwrap();
    assert_eq!((got.id(), got.encoding()), (c.id(), 1));
    let got: Review = reader.get_typed(kinds::REVIEW, &hashes[3]).unwrap();
    assert_eq!(got, r);
    // the wire types read the released encodings as all-local values
    let wire: ReviewV1 = reader.get_typed(kinds::REVIEW, &hashes[3]).unwrap();
    assert_eq!(wire, ReviewV1::of(&r));
    let wire: SemanticTensionV1 = reader.get_typed(kinds::TENSION, &hashes[4]).unwrap();
    assert_eq!(wire, SemanticTensionV1::of(&st));
    let wire: TransitionsV1 = reader.get_typed(kinds::TRANSITION, &hashes[5]).unwrap();
    assert_eq!(wire, TransitionsV1::of(&t));
    for (item, hash) in frames.iter().zip(&hashes) {
        assert_eq!(Hash::of(&item.1), *hash);
        let o = codec::split(item.0, &item.1).unwrap();
        match codec::logical_kind(item.0) {
            kinds::BLOCK => assert!(codec::is_canonical(&o, &b)),
            kinds::REVIEW => assert!(codec::is_canonical(&o, &r)),
            kinds::TENSION => assert!(codec::is_canonical(&o, &st)),
            kinds::TRANSITION => assert!(codec::is_canonical(&o, &t)),
            _ => {}
        }
    }
    // reindex and reopen read every local frame as before: no feature turns on
    drop(reader);
    s.reindex().unwrap();
    assert!(!log_manifest(&s).has(FEATURE_FOREIGN_COPY));
    drop(s);
    let s = Store::open(&d.path().join("s")).unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    // the appended variants keep their ordinals after the released ones
    assert_eq!(postcard_of(&AnswerAct::AcceptVerdict), [16]);
    assert_eq!(postcard_of(&AnswerAct::Adopt), [17]);
    assert_eq!(postcard_of(&AnswerAct::KeepPin), [18]);
    assert_eq!(
        (AnswerAct::Adopt.as_str(), AnswerAct::KeepPin.as_str()),
        ("adopt", "keep_pin")
    );
    assert_eq!(postcard_of(&Cause::Recheck), [9]);
    assert_eq!(postcard_of(&Cause::Delivered), [10]);
    assert_eq!(Cause::Delivered.name(), "delivered");
    assert_eq!(postcard_of(&foreign_target())[0], 3);
    assert_eq!(
        serde_json::to_value(foreign_target()).unwrap(),
        serde_json::json!({"foreign_version": {
            "repo": repo(7).to_string(),
            "id": "D-00001",
            "version": h(61).to_string(),
        }})
    );
    // D-C94DV: the selection identity a delivery stores keeps the vector Repo links freeze
    let fixed = [
        (id("D-00001"), Hash::of(b"1")),
        (id("d-00002"), Hash::of(b"2")),
        (id("s-1DENT"), Hash::of(b"3")),
    ];
    assert_eq!(
        selection_identity(fixed.iter().map(|(i, v)| (i, v)))
            .unwrap()
            .to_string(),
        "90cb277b71b183434bcdee864c9cad5aca5be38b562ff9d94365251c55cb0660"
    );
    let mut bytes = b"rdy-publication-selection-identity\0".to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        selection_identity(std::iter::empty::<(&BlockId, &Hash)>()),
        Some(Hash::of(&bytes))
    );
}

/// The first record that uses the foreign copy turns `foreign_copy` on in the batch that writes
/// it, with class incompatible and its own minimum; the seven older minima stay. A FOREIGN
/// entry, a block with a foreign part and a repo_link block are each a first use; a local block
/// and a local instruction are not. A first use whose batch never sealed leaves no feature: the
/// open puts FORMAT back.
#[test]
fn rpl3_feature_first_use_is_atomic() {
    // the registry: one new feature, incompatible, with its own minimum; the others unchanged
    let registered = registered_features();
    let foreign = registered
        .iter()
        .find(|f| f.name == FEATURE_FOREIGN_COPY)
        .expect("foreign_copy is registered");
    assert_eq!(foreign.class, FeatureClass::Incompatible);
    assert_eq!(foreign.required_rdy, FOREIGN_COPY_REQUIRED_RDY);
    assert_eq!(
        format::required_rdy(FEATURE_FOREIGN_COPY),
        FOREIGN_COPY_REQUIRED_RDY
    );
    assert_eq!(registered.len(), 8);
    for f in registered.iter().filter(|f| f.name != FEATURE_FOREIGN_COPY) {
        assert_eq!(f.required_rdy, FEATURE_REQUIRED_RDY, "{}", f.name);
        assert_eq!(f.class, FeatureClass::Incompatible, "{}", f.name);
    }
    assert_eq!(FEATURE_REQUIRED_RDY, "0.26.0");
    assert!(format::valid_required_rdy(FOREIGN_COPY_REQUIRED_RDY));
    // a new store does not use it
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    // local writes do not turn it on: a local block, a local instruction, a local answer
    s.write(vec![
        codec::item_new(kinds::BLOCK, &local_block()),
        codec::item_new(
            kinds::BLOCK,
            &block_of("i-00001", "instruction", "Use short sentences."),
        ),
        codec::item_new(kinds::ANSWER, &answer(AnswerAct::Confirm)),
    ])
    .unwrap();
    assert!(!log_manifest(&s).has(FEATURE_FOREIGN_COPY));
    assert!(!s.has_feature_now(FEATURE_FOREIGN_COPY));
    // the first FOREIGN entry turns it on in its own sealed batch, and FORMAT mirrors it
    let entry = codec::item_new(kinds::FOREIGN, &delivery());
    assert_eq!(version_of(&entry), 1);
    let ids = s.write(vec![entry.clone()]).unwrap();
    assert_eq!(ids, vec![Hash::of(&entry.1)]);
    let manifest = log_manifest(&s);
    assert_eq!(
        names(&manifest),
        ["content_hash_v2", "foreign_copy", "object_encoding"]
    );
    let batch = last_batch(&s);
    assert_eq!(batch[0].0, entry.0, "the entry leads its batch");
    assert!(
        batch
            .iter()
            .any(|(_, n)| n.as_deref() == Some(REF_STORE_FEATURES)),
        "the manifest ref is in the entry's batch"
    );
    assert_eq!(
        format::read_format_file(&dir).unwrap(),
        Some(manifest.clone())
    );
    assert!(s.has_feature_now(FEATURE_FOREIGN_COPY));
    // a stored entry reads back, and a reopen keeps the feature
    let got: ForeignEntry = s.reader().get_typed(kinds::FOREIGN, &ids[0]).unwrap();
    assert_eq!(got, delivery());
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(log_manifest(&s), manifest);
    drop(s);
    // a block with a foreign part, and a repo_link block, are first uses too, each in its batch
    for (name, item) in [
        (
            "foreign block",
            codec::item_new(kinds::BLOCK, &foreign_block()),
        ),
        (
            "repo_link block",
            codec::item_new(kinds::BLOCK, &block_of("rl-00001", "repo_link", "")),
        ),
        (
            "repo_link in a change",
            codec::item(
                kinds::CHANGE,
                &change_of(vec![Op::Create {
                    block: block_of("rl-00002", "repo_link", ""),
                }]),
            )
            .unwrap(),
        ),
    ] {
        let dir = d.path().join(name.replace(' ', "-"));
        let mut s = Store::create(&dir).unwrap();
        s.write(vec![item]).unwrap();
        assert!(log_manifest(&s).has(FEATURE_FOREIGN_COPY), "{name}");
        assert!(
            last_batch(&s)
                .iter()
                .any(|(_, n)| n.as_deref() == Some(REF_STORE_FEATURES)),
            "{name}: in the same sealed batch"
        );
    }
    // a first use whose batch never sealed: FORMAT lists the feature, the sealed log does not,
    // and the open puts FORMAT back to the log's manifest
    let dir = d.path().join("torn");
    drop(Store::create(&dir).unwrap());
    let ahead = baseline_features().merged(std::slice::from_ref(foreign));
    format::write_format_file(&dir, &ahead).unwrap();
    carried_fixture::append_raw(&dir, &[entry], false);
    let s = Store::open(&dir).unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    assert!(!s.has_feature_now(FEATURE_FOREIGN_COPY));
    assert_eq!(
        format::read_format_file(&dir).unwrap(),
        Some(baseline_features())
    );
}

/// Every foreign shape needs `foreign_copy`, through its kind's optional encoding (or kind 29,
/// or a repo_link block); a released encoding never carries one, and an optional encoding never
/// carries a value without one; a load whose manifest does not name the feature its records use
/// is refused before it appends anything.
#[test]
fn rpl3_new_shapes_refuse_without_feature() {
    let foreign_change = change_of(vec![Op::Create {
        block: foreign_block(),
    }]);
    let delivered = transitions(Cause::Delivered, Some(h(64)));
    let mut foreign_side = TransitionsV1::of(&transitions(Cause::Committed, Some(h(1))));
    foreign_side.new[0].target = SuspectTarget::Foreign(cited());
    let mut foreign_tension = SemanticTensionV1::of(&tension());
    foreign_tension.items[1] = PairSide::Foreign(ForeignPairItem {
        target: cited(),
        content: h(45),
    });
    let mut opened = TransitionsV1::of(&transitions(Cause::Committed, Some(h(1))));
    opened.new.clear();
    opened.opened.push(rdy_store::transition::TensionMoveV1 {
        tension: h(65),
        kind: "duplicate_definition".into(),
        severity: "advisory".into(),
        sides: vec![SideV1::Foreign {
            repo: repo(7),
            entry: h(66),
        }],
        rekey: None,
        why: why(Cause::Delivered, Some(h(66))),
    });
    // each foreign shape: its frame, its encoding, its feature
    let shapes: Vec<Shape> = vec![
        (
            "FOREIGN entry",
            codec::item_new(kinds::FOREIGN, &delivery()),
            1,
        ),
        (
            "foreign block",
            codec::item_new(kinds::BLOCK, &foreign_block()),
            1,
        ),
        (
            "foreign change",
            codec::item(kinds::CHANGE, &foreign_change).unwrap(),
            2,
        ),
        ("delivered transition", delivered.frame(), 1),
        (
            "foreign suspect target",
            codec::item_new(kinds::TRANSITION, &foreign_side),
            1,
        ),
        (
            "foreign tension side",
            codec::item_new(kinds::TRANSITION, &opened),
            1,
        ),
        (
            "foreign pair review",
            codec::item_new(kinds::REVIEW, &foreign_review()),
            1,
        ),
        (
            "foreign pair tension",
            codec::item_new(kinds::TENSION, &foreign_tension),
            1,
        ),
        (
            "adopt answer",
            codec::item_new(kinds::ANSWER, &answer(AnswerAct::Adopt)),
            1,
        ),
        (
            "keep-pin answer",
            codec::item_new(kinds::ANSWER, &answer(AnswerAct::KeepPin)),
            1,
        ),
        (
            "foreign assessment",
            codec::item_new(kinds::ASSESSMENT, &foreign_assessment()),
            2,
        ),
        (
            "foreign_version target",
            codec::item_new(kinds::REDACTION, &redaction(vec![foreign_target()])),
            1,
        ),
        (
            "repo_link block",
            codec::item_new(kinds::BLOCK, &block_of("rl-00001", "repo_link", "")),
            0,
        ),
    ];
    for (name, item, version) in &shapes {
        assert_eq!(version_of(item), *version, "{name}: its encoding");
        assert!(
            features_of(item).contains(&FEATURE_FOREIGN_COPY.to_string()),
            "{name}: needs foreign_copy"
        );
    }
    // a foreign block's frame hash is its version hash; a foreign change's is its id
    assert_eq!(Hash::of(&shapes[1].1.1), foreign_block().version_hash());
    assert_eq!(
        shapes[1].1.1,
        payload(1, &postcard_of(&ForeignBlockV1::of(&foreign_block())))
    );
    assert_eq!(Hash::of(&shapes[2].1.1), foreign_change.id());
    assert_eq!(
        foreign_change.id(),
        encoded_hash(2, &postcard_of(&ForeignChangeV2::of(&foreign_change)))
    );
    // no local value needs it
    for item in [
        codec::item_new(kinds::BLOCK, &local_block()),
        codec::item_new(
            kinds::BLOCK,
            &block_of("i-00001", "instruction", "Use short sentences."),
        ),
        codec::item_new(kinds::ANSWER, &answer(AnswerAct::AcceptVerdict)),
        transitions(Cause::Landed, Some(h(2))).frame(),
        codec::item_new(kinds::REVIEW, &pair_review()),
        codec::item_new(kinds::ASSESSMENT, &assessment()),
        codec::item_new(
            kinds::REDACTION,
            &redaction(vec![RedactTarget::AllVersions(id("c-00001"))]),
        ),
    ] {
        assert!(!features_of(&item).contains(&FEATURE_FOREIGN_COPY.to_string()));
    }
    // the runtime reads what it can hold: a delivered move, a foreign block or change, the new
    // acts and a foreign target; a foreign side or target, pair side or assessment part only
    // through its wire type
    let read = |item: &(u8, Vec<u8>)| item.clone();
    let t: Transitions = codec::decode_record(shapes[3].1.0, &shapes[3].1.1).unwrap();
    assert_eq!(t, delivered);
    let b: Block = codec::decode_record(shapes[1].1.0, &shapes[1].1.1).unwrap();
    assert_eq!(b, foreign_block());
    let c: Change = codec::decode_record(shapes[2].1.0, &shapes[2].1.1).unwrap();
    assert_eq!(
        (c.id(), c.ops),
        (foreign_change.id(), foreign_change.ops.clone())
    );
    let a: Answer = codec::decode_record(shapes[8].1.0, &shapes[8].1.1).unwrap();
    assert_eq!(a.act, AnswerAct::Adopt);
    // an answer's id is its frame's hash, in either encoding
    assert_eq!(a.id(), Hash::of(&shapes[8].1.1));
    let local = answer(AnswerAct::Confirm);
    assert_eq!(
        local.id(),
        Hash::of(&codec::item_new(kinds::ANSWER, &local).1)
    );
    let side = read(&shapes[4].1);
    assert!(codec::decode_record::<Transitions>(side.0, &side.1).is_err());
    assert_eq!(
        codec::decode_record::<TransitionsV1>(side.0, &side.1).unwrap(),
        foreign_side
    );
    let pair = read(&shapes[6].1);
    assert!(codec::decode_record::<Review>(pair.0, &pair.1).is_err());
    assert_eq!(
        codec::decode_record::<ReviewV1>(pair.0, &pair.1).unwrap(),
        foreign_review()
    );
    let pa = read(&shapes[10].1);
    assert!(codec::decode_record::<Assessment>(pa.0, &pa.1).is_err());
    assert_eq!(
        codec::decode_record::<AssessmentV2>(pa.0, &pa.1).unwrap(),
        foreign_assessment()
    );
    // the new edges: a delivered cause and a foreign side name their FOREIGN entry, a foreign
    // assessment its held version and entry, a foreign target its held BLOCK version
    let some = |kind: u8, hash: Hash| ObjectRef {
        kind: Some(kind),
        hash,
    };
    assert_eq!(
        refs_of(shapes[3].1.0, &shapes[3].1.1).unwrap(),
        vec![some(kinds::FOREIGN, h(64))]
    );
    assert_eq!(
        refs_of(shapes[5].1.0, &shapes[5].1.1).unwrap(),
        vec![some(kinds::FOREIGN, h(66))]
    );
    assert!(
        refs_of(shapes[10].1.0, &shapes[10].1.1)
            .unwrap()
            .iter()
            .any(|r| *r == some(kinds::BLOCK, h(54)))
    );
    assert!(
        refs_of(shapes[10].1.0, &shapes[10].1.1)
            .unwrap()
            .contains(&some(kinds::FOREIGN, h(56)))
    );
    assert!(
        refs_of(shapes[11].1.0, &shapes[11].1.1)
            .unwrap()
            .contains(&some(kinds::BLOCK, h(61)))
    );
    // a released encoding never carries a foreign shape: each body framed at the released
    // encoding is refused by every reader, so no foreign shape slips past the feature
    let smuggled: Vec<(&str, u8, u16, Vec<u8>)> = vec![
        (
            "adopt act",
            kinds::ANSWER,
            0,
            postcard_of(&answer(AnswerAct::Adopt)),
        ),
        (
            "keep-pin act",
            kinds::ANSWER,
            0,
            postcard_of(&answer(AnswerAct::KeepPin)),
        ),
        (
            "delivered cause",
            kinds::TRANSITION,
            0,
            postcard_of(&delivered),
        ),
        (
            "foreign target",
            kinds::REDACTION,
            0,
            postcard_of(&redaction(vec![foreign_target()])),
        ),
    ];
    for (name, kind, version, body) in &smuggled {
        let p = payload(*version, body);
        let r = raw(*kind, *version);
        let refused = match *kind {
            kinds::ANSWER => codec::decode_record::<Answer>(r, &p).is_err(),
            kinds::TRANSITION => codec::decode_record::<Transitions>(r, &p).is_err(),
            _ => codec::decode_record::<Redaction>(r, &p).is_err(),
        };
        assert!(refused, "{name}: a released encoding cannot hold it");
        assert_eq!(refs_of(r, &p), Err(RecordError::Decode), "{name}");
    }
    // the frozen carried-proof decoder keeps the released targets: a carried record that names
    // a cited version does not decode, whole or in place
    let carried = |targets: Vec<RedactTarget>| RedactionCarried {
        request: h(67),
        apply: h(68),
        landing: h(69),
        approved_by: prov("human:alice", Role::Approver),
        reason_class: ReasonClass::Secret,
        targets,
        spans: vec![CarriedSpan {
            place: FramePlace {
                segment: 0,
                offset: 0,
            },
            kind: kinds::BLOCK,
            hash: h(71),
            len: 40,
            residue: None,
            held: false,
        }],
    };
    let old = postcard_of(&carried(vec![RedactTarget::AllVersions(id("c-00001"))]));
    assert!(redaction_carried::decode_carried(&old).is_ok());
    assert_eq!(redaction_carried::check_record(&old), Ok(()));
    let cited_target = postcard_of(&carried(vec![foreign_target()]));
    assert!(redaction_carried::decode_carried(&cited_target).is_err());
    assert_eq!(
        redaction_carried::check_record(&cited_target),
        Err(RecordError::Decode)
    );
    // an optional encoding never carries a value without a foreign shape
    let local_c = change_of(vec![Op::Create {
        block: local_block(),
    }]);
    let no_shape: Vec<(&str, u8, u16, Vec<u8>)> = vec![
        (
            "local block",
            kinds::BLOCK,
            1,
            postcard_of(&ForeignBlockV1::of(&local_block())),
        ),
        (
            "local change",
            kinds::CHANGE,
            2,
            postcard_of(&ForeignChangeV2::of(&local_c)),
        ),
        (
            "confirm act",
            kinds::ANSWER,
            1,
            postcard_of(&answer(AnswerAct::Confirm)),
        ),
        (
            "local transition",
            kinds::TRANSITION,
            1,
            postcard_of(&TransitionsV1::of(&transitions(Cause::Landed, Some(h(2))))),
        ),
        (
            "local pair",
            kinds::REVIEW,
            1,
            postcard_of(&ReviewV1::of(&pair_review())),
        ),
        (
            "local tension",
            kinds::TENSION,
            1,
            postcard_of(&SemanticTensionV1::of(&tension())),
        ),
        (
            "local assessment",
            kinds::ASSESSMENT,
            2,
            postcard_of(&AssessmentV2::of(&assessment())),
        ),
        (
            "local targets",
            kinds::REDACTION,
            1,
            postcard_of(&redaction(vec![RedactTarget::AllVersions(id("c-00001"))])),
        ),
    ];
    for (name, kind, version, body) in &no_shape {
        let p = payload(*version, body);
        let r = raw(*kind, *version);
        assert_eq!(refs_of(r, &p), Err(RecordError::Decode), "{name}");
        let o = codec::split(r, &p).unwrap();
        let refused = match *kind {
            kinds::BLOCK => codec::decode_split::<Block>(&o).is_err(),
            kinds::CHANGE => codec::decode_split::<Change>(&o).is_err(),
            kinds::ANSWER => codec::decode_split::<Answer>(&o).is_err(),
            kinds::TRANSITION => codec::decode_split::<TransitionsV1>(&o).is_err(),
            kinds::REVIEW => codec::decode_split::<ReviewV1>(&o).is_err(),
            kinds::TENSION => codec::decode_split::<SemanticTensionV1>(&o).is_err(),
            kinds::ASSESSMENT => codec::decode_split::<AssessmentV2>(&o).is_err(),
            _ => codec::decode_split::<Redaction>(&o).is_err(),
        };
        assert!(
            refused,
            "{name}: only a foreign shape takes the optional encoding"
        );
    }
    // canonical: a value only in the encoding its writer chooses
    let p = payload(1, &no_shape[0].3);
    let o = codec::split(raw(kinds::BLOCK, 1), &p).unwrap();
    assert!(!codec::is_canonical(&o, &local_block()));
    let o = codec::split(shapes[1].1.0, &shapes[1].1.1).unwrap();
    assert!(codec::is_canonical(&o, &foreign_block()));
    let local = codec::item_new(kinds::BLOCK, &local_block());
    let o = codec::split(local.0, &local.1).unwrap();
    assert!(codec::is_canonical(&o, &local_block()));
    assert!(!codec::is_canonical(&o, &foreign_block()));
    // a dump-4 load whose manifest hides foreign_copy is refused, and appends nothing: the
    // batches of a new store, then one batch that holds the foreign shape
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    let s = Store::create(&src).unwrap();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    drop(s);
    for (n, (name, item, _)) in shapes.iter().enumerate() {
        let to = d.path().join(format!("to-{n}"));
        let mut i = Ingest::create(&to).unwrap();
        for (k, b) in batches.iter().enumerate() {
            i.batch(k as u64 + 1, b).unwrap();
        }
        let hash = Hash::of(&item.1);
        let foreign = DumpBatch {
            start: i.end(),
            digest: kinds::batch_digest([&hash]),
            frames: vec![Frame::Present {
                kind: item.0,
                hash,
                payload: item.1.clone(),
            }],
        };
        i.batch(batches.len() as u64 + 1, &foreign).unwrap();
        let end = i.end();
        match i.upgrade(&format::DumpStore::Features(baseline_features())) {
            Err(StoreError::Dump { what, .. }) => assert_eq!(
                what,
                "the dump's records use feature foreign_copy, which its manifest does not name",
                "{name}"
            ),
            other => panic!("{name}: a hidden foreign_copy was kept: {other:?}"),
        }
        assert_eq!(i.end(), end, "{name}: nothing appended");
    }
}

/// An encoding past the newest one a kind registers is the unknown-encoding refusal
/// (E_STORE_FORMAT, naming the kind and the encoding), never a guess; FOREIGN has no encoding 0;
/// a hostile FOREIGN body is refused within the record bounds before it allocates.
#[test]
fn rpl3_unknown_encoding_is_bounded() {
    // the registered encodings: FOREIGN at 1, and each optional encoding beside the latest
    assert_eq!(
        (codec::first(kinds::FOREIGN), codec::latest(kinds::FOREIGN)),
        (1, 1)
    );
    for (kind, latest, optional) in [
        (kinds::BLOCK, 0, 1),
        (kinds::CHANGE, 1, 2),
        (kinds::TRANSITION, 0, 1),
        (kinds::REVIEW, 0, 1),
        (kinds::TENSION, 0, 1),
        (kinds::ANSWER, 0, 1),
        (kinds::ASSESSMENT, 1, 2),
        (kinds::REDACTION, 0, 1),
    ] {
        assert_eq!(codec::latest(kind), latest, "kind {kind}");
        assert_eq!(codec::optional(kind), Some(optional), "kind {kind}");
        assert_eq!(codec::newest(kind), optional, "kind {kind}");
    }
    assert_eq!(codec::optional(kinds::LANDING), None);
    // one past the newest encoding: a split refusal, and the store's E_STORE_FORMAT
    for (kind, name, version) in [
        (kinds::FOREIGN, "FOREIGN", 2u16),
        (kinds::BLOCK, "BLOCK", 2),
        (kinds::CHANGE, "CHANGE", 3),
        (kinds::TRANSITION, "TRANSITION", 2),
        (kinds::REVIEW, "REVIEW", 2),
        (kinds::TENSION, "TENSION", 2),
        (kinds::ANSWER, "ANSWER", 2),
        (kinds::ASSESSMENT, "ASSESSMENT", 3),
        (kinds::REDACTION, "REDACTION", 2),
    ] {
        let p = payload(version, b"any body");
        let r = raw(kind, version);
        assert_eq!(
            codec::split(r, &p).unwrap_err(),
            RecordError::UnknownVersion,
            "{name}"
        );
        // FOREIGN is not portable yet: refs_of refuses its kind before its encoding
        let refused = if kind == kinds::FOREIGN {
            RecordError::Kind
        } else {
            RecordError::UnknownVersion
        };
        assert_eq!(refs_of(r, &p).unwrap_err(), refused, "{name}");
        match codec::decode_record::<ForeignEntry>(r, &p) {
            Err(StoreError::Format { message, fix }) => {
                assert_eq!(
                    message,
                    format!(
                        "the store holds an object this rdy cannot read: kind {name}, encoding {version}"
                    )
                );
                assert_eq!(fix, "open the project with a newer rdy");
            }
            other => panic!("{name}: {other:?}"),
        }
    }
    // FOREIGN has no encoding 0: an unmarked frame is an unknown encoding too
    let body = postcard_of(&delivery());
    assert_eq!(
        codec::split(kinds::FOREIGN, &body).unwrap_err(),
        RecordError::UnknownVersion
    );
    assert!(matches!(
        codec::decode_record::<ForeignEntry>(kinds::FOREIGN, &body),
        Err(StoreError::Format { .. })
    ));
    // a FOREIGN entry decodes only as itself, within the bounds
    let entry = codec::item_new(kinds::FOREIGN, &delivery());
    assert_eq!(
        codec::decode_bounded::<ForeignEntry>(entry.0, &entry.1).unwrap(),
        delivery()
    );
    assert_eq!(
        codec::decode_bounded::<Block>(entry.0, &entry.1).unwrap_err(),
        RecordError::Kind
    );
    // FOREIGN is registered, not portable yet: refs_of refuses it as a kind
    assert_eq!(refs_of(entry.0, &entry.1).unwrap_err(), RecordError::Kind);
    // a hostile list length is refused before a collection grows
    let mut hostile = postcard_of(&repo(7));
    hostile.push(0); // source: Link
    hostile.extend(postcard_of(&"rl-00001"));
    hostile.extend(postcard_of(&"hub"));
    hostile.extend(postcard_of(&9i64));
    hostile.push(0); // event: Delivery
    hostile.push(0); // prev: None
    hostile.extend(postcard_of(&"v1"));
    hostile.extend(postcard_of(&"decisions"));
    hostile.push(0); // profile_version: None
    hostile.push(0); // set: None
    hostile.extend([0xff, 0xff, 0xff, 0xff, 0x0f]); // added: u32::MAX versions
    let p = payload(1, &hostile);
    assert_eq!(
        codec::decode_bounded::<ForeignEntry>(raw(kinds::FOREIGN, 1), &p).unwrap_err(),
        RecordError::Limit {
            name: bounded::COLLECTION_ELEMENTS,
            maximum: bounded::MAX_ELEMENTS as u64,
        }
    );
    // a hostile text length is refused before the text is read
    let mut long_text = postcard_of(&repo(7));
    long_text.push(0); // source: Link
    long_text.extend([0x81, 0x80, 0x40]); // a link text of 1,048,577 bytes
    let p = payload(1, &long_text);
    assert_eq!(
        codec::decode_bounded::<ForeignEntry>(raw(kinds::FOREIGN, 1), &p).unwrap_err(),
        RecordError::Limit {
            name: bounded::TEXT_BYTES,
            maximum: bounded::MAX_TEXT_BYTES as u64,
        }
    );
    // a record past 16,777,216 bytes is refused before it is decoded
    let p = payload(1, &vec![0u8; bounded::MAX_RECORD_BYTES + 1]);
    assert_eq!(
        codec::decode_bounded::<ForeignEntry>(raw(kinds::FOREIGN, 1), &p).unwrap_err(),
        RecordError::Limit {
            name: bounded::RECORD_BYTES,
            maximum: bounded::MAX_RECORD_BYTES as u64,
        }
    );
    // a truncated entry and trailing bytes do not decode
    let mut cut = entry.1.clone();
    cut.truncate(cut.len() - 1);
    assert_eq!(
        codec::decode_bounded::<ForeignEntry>(entry.0, &cut).unwrap_err(),
        RecordError::Decode
    );
    let mut long = entry.1.clone();
    long.push(0);
    assert_eq!(
        codec::decode_bounded::<ForeignEntry>(entry.0, &long).unwrap_err(),
        RecordError::Decode
    );
}

#[test]
fn rpl18_a_deviation_exception_takes_the_foreign_assessment_encoding() {
    // repo links (D-8VJAZ): the released layout (encoding 1) has six exception kinds; a record
    // with a deviation row is written in encoding 2, with an empty foreign part
    let mut a = assessment();
    a.facts.exceptions = vec![AssessmentException {
        kind: AssessmentExceptionKind::Deviation,
        reference: "D-00001@0123456789ab".into(),
    }];
    assert!(a.has_foreign_shape());
    let wire = AssessmentV2::of(&a);
    assert!(wire.has_foreign() && wire.foreign.is_empty());
    let item = codec::item_new(kinds::ASSESSMENT, &a);
    assert_eq!(item, codec::item_new(kinds::ASSESSMENT, &wire));
    assert_eq!(
        item,
        (raw(kinds::ASSESSMENT, 2), payload(2, &postcard_of(&wire)))
    );
    assert!(features_of(&item).contains(&FEATURE_FOREIGN_COPY.to_string()));
    // the runtime record and the wire record both read it back
    let read: Assessment = codec::decode_record(item.0, &item.1).unwrap();
    assert_eq!(read, a);
    let o = codec::split(item.0, &item.1).unwrap();
    assert!(codec::is_canonical(&o, &read));
    assert_eq!(
        codec::decode_record::<AssessmentV2>(item.0, &item.1).unwrap(),
        wire
    );
    // a released-layout frame that carries the deviation tag does not decode
    let forged = (kinds::ASSESSMENT | VERSIONED, payload(1, &postcard_of(&a)));
    assert!(codec::decode_record::<Assessment>(forged.0, &forged.1).is_err());
    assert!(codec::decode_record::<AssessmentV2>(forged.0, &forged.1).is_err());
    // a record with no deviation keeps the released layout and its bytes
    let local = assessment();
    assert!(!local.has_foreign_shape());
    assert_eq!(
        codec::item_new(kinds::ASSESSMENT, &local),
        (
            kinds::ASSESSMENT | VERSIONED,
            payload(1, &postcard_of(&local))
        )
    );
}

/// rpl32 fix round 1 (D-FK58A): an assessment's foreign part is no portable edge. Its portable
/// edges are its local part's, so a tag whose landing holds an approved foreign tension carries
/// no FOREIGN entry and no delivered version; every edge, the foreign part's included, stays in
/// `refs_of` for the source store's own checks. The record's bytes do not change.
#[test]
fn rpl32_portable_refs_leave_out_an_assessments_foreign_part() {
    let some = |kind: u8, hash: Hash| ObjectRef {
        kind: Some(kind),
        hash,
    };
    let foreign = codec::item_new(kinds::ASSESSMENT, &foreign_assessment());
    let local = codec::item_new(kinds::ASSESSMENT, &AssessmentV2::of(&assessment()));
    let all = refs_of(foreign.0, &foreign.1).unwrap();
    let portable = portable_refs_of(foreign.0, &foreign.1).unwrap();
    assert_eq!(portable, refs_of(local.0, &local.1).unwrap());
    assert!(!portable.contains(&some(kinds::BLOCK, h(54))));
    assert!(!portable.contains(&some(kinds::FOREIGN, h(56))));
    let mut back = portable.clone();
    back.extend([some(kinds::BLOCK, h(54)), some(kinds::FOREIGN, h(56))]);
    back.sort();
    assert_eq!(all, back);
    // a record without a foreign part has the same edges both ways, and a FOREIGN entry is
    // refused as a kind both ways
    assert_eq!(
        portable_refs_of(local.0, &local.1).unwrap(),
        refs_of(local.0, &local.1).unwrap()
    );
    let entry = codec::item_new(kinds::FOREIGN, &delivery());
    assert_eq!(
        portable_refs_of(entry.0, &entry.1).unwrap_err(),
        RecordError::Kind
    );
}
