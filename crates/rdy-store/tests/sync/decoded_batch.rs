//! One decoded value per checked record (D-3PNRK, D-D2NPN verify-on-receive): a batch check
//! decodes each present typed record once, and refs, record rules, features and the caller's
//! check all read that one value. The checks, their order and their refusals stay as they were.
use rdy_model::assessment::*;
use rdy_model::foreign::{ForeignId, ForeignLink, ForeignParts, ForeignPin};
use rdy_model::redaction::{
    AssessmentSupport, ChangeResidue, ReasonClass, RedactTarget, Redaction, RedactionManifest,
    RedactionResidue,
};
use rdy_model::review::ReviewV1;
use rdy_model::semantic::{ForeignPairItem, PairItem, PairSide, SemanticTensionV1, pair_key};
use rdy_model::*;
use rdy_store::bounded;
use rdy_store::closure::{END_OF_LOG, StoreSource, history_closure_before, write_batch};
use rdy_store::codec::{self, VERSIONED};
use rdy_store::decoded_record::DecodedRecord;
use rdy_store::format::{FEATURE_SNAPSHOT_DELTA, StoreFeatures, baseline_features};
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, ObjectRef, portable_refs_of};
use rdy_store::plan_record::PlanRecord;
use rdy_store::stream_error::{
    LIMIT_GRAPH_EDGES, LIMIT_HISTORY_DEPTH, LIMIT_OBJECT_RECORDS, LIMIT_PENDING_DROPS,
    LIMIT_PREREQUISITES, LIMIT_TOTAL_BYTES, Reason, StreamError,
};
use rdy_store::sync_stream::{
    BatchReport, CheckCaps, CheckedBatch, RecordFeatures, StreamHeader, StreamPurpose,
    StreamReader, StreamRecord, StreamTotals, StreamWriter, batch_binding, check_batch,
    check_batch_prepared_with, lower_caps_for_tests, record_rules_decoding_again_for_tests,
    sync_kinds,
};
use rdy_store::transition::{Author, Cause, SuspectMove, Transitions, TransitionsV1, Why};
use rdy_store::{LandRequest, Store};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

const REPO: RepoId = RepoId([7; 16]);

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("run-1".into()),
        role,
        at_ms: 1_760_000_000_000,
    }
}

// ---------------------------------------------------------------------------------------------
// the corpus: the released sync fixtures, and one batch of the kinds and encodings they lack

/// The released history fixtures this rdy reads, by file name.
const FIXTURES: [&str; 7] = [
    "v2-f1-e0.rdysync",
    "v2-f1-e1.rdysync",
    "v2-f1-e1-ck.rdysync",
    "v2-f1-e1-snapshot-delta.rdysync",
    "sync4-canon-history.bin",
    "sync4-view-history.bin",
    "sync4-reduction.bin",
];

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rdy-qa/fixtures/sync")
            .join(name),
    )
    .unwrap()
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

fn cited() -> ForeignId {
    ForeignId::new(RepoId([9; 16]), id("D-00001"))
}

/// A block that cites a delivered decision: encoding 1 of BLOCK.
fn foreign_block(id_: &str) -> Block {
    let mut b = block_of(id_, "claim", "cites a delivered decision");
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

fn change_of(ops: Vec<Op>, base: u8) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![h(base)],
        provenance: prov("agent:w", Role::Writer),
        justification: None,
        message: "a change".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn port(mut c: Change) -> Change {
    c.links.ported_from = Some(PortedFrom {
        repo_id: RepoId([9; 16]),
        change: h(77),
    });
    c
}

fn answer(act: AnswerAct) -> Answer {
    Answer {
        item: format!("adopt:{}@{}", "09".repeat(16), "ab".repeat(6)),
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

fn intent(prev: Option<Hash>) -> Intent {
    Intent {
        stable: None,
        prev,
        actor: "agent:w".into(),
        run: None,
        view: "work".into(),
        area: None,
        blocks: vec![],
        angle: "an angle".into(),
        expires_at_ms: 1,
        status: IntentStatus::Open,
        at_ms: 1,
    }
}

fn alt_set() -> AltSet {
    AltSet {
        stable: Some(h(58)),
        prev: Some(h(59)),
        title: "a choice".into(),
        question: None,
        members: vec![AltMember {
            label: "one".into(),
            view: "one".into(),
        }],
        status: AltStatus::Chosen {
            member: "one".into(),
            landing: h(57),
        },
        provenance: prov("agent:w", Role::Writer),
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

/// A pair review whose second side is a cited block: encoding 1 of REVIEW.
fn foreign_review() -> ReviewV1 {
    let mut r = ReviewV1::of(&pair_review());
    if let rdy_model::review::ReviewSubjectV1::Pair { items, .. } = &mut r.subject {
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
        prev: Some(h(46)),
        provenance: prov("agent:r", Role::Reviewer),
    }
}

fn foreign_tension() -> SemanticTensionV1 {
    let mut t = SemanticTensionV1::of(&tension());
    t.items[1] = PairSide::Foreign(ForeignPairItem {
        target: cited(),
        content: h(45),
    });
    t
}

fn why(cause: Cause, id: Option<Hash>) -> Why {
    Why {
        cause,
        id,
        actor: None,
        run: None,
    }
}

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
        base: Some(h(49)),
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

/// An assessment with a foreign part: encoding 2 of ASSESSMENT.
fn foreign_assessment() -> AssessmentV2 {
    let mut a = AssessmentV2::of(&assessment());
    a.foreign.reach.push(AssessmentForeignReach {
        target: cited(),
        version: h(54),
    });
    a.foreign.tensions.push(AssessmentForeignTension {
        tension: h(55),
        repo: RepoId([9; 16]),
        entry: h(56),
    });
    a
}

/// A removal request of a cited version: encoding 1 of REDACTION.
fn foreign_redaction() -> Redaction {
    Redaction {
        carrier: h(60),
        actor: prov("human:alice", Role::Approver),
        targets: vec![RedactTarget::ForeignVersion {
            repo: RepoId([9; 16]),
            id: id("D-00001"),
            version: h(61),
        }],
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

/// A present record of the frame `(raw, payload)`.
fn record(item: (u8, Vec<u8>)) -> StreamRecord {
    let (raw, payload) = item;
    let o = codec::split(raw, &payload).unwrap();
    StreamRecord::Present {
        key: ObjectKey {
            kind: o.kind,
            hash: Hash::of(&payload),
        },
        encoding: o.version,
        payload,
    }
}

/// One record of each portable kind and encoding the fixtures lack, named; TRANSITION is a
/// store's local record, never a batch's, so it is decoded on its own.
fn supplement() -> Vec<(&'static str, StreamRecord)> {
    let foreign_change = change_of(
        vec![Op::Create {
            block: foreign_block("c-00010"),
        }],
        80,
    );
    let plain_port = port(change_of(
        vec![Op::Create {
            block: block_of("c-00011", "claim", "ported text"),
        }],
        81,
    ));
    let foreign_port = port(change_of(
        vec![
            Op::Create {
                block: block_of("c-00012", "claim", "ported text"),
            },
            Op::Create {
                block: foreign_block("f-00012"),
            },
        ],
        82,
    ));
    let port_residue = RedactionResidue::Change(ChangeResidue::of(&port(change_of(
        vec![Op::Update {
            base: h(83),
            block: block_of("c-00013", "claim", "ported text"),
        }],
        84,
    ))));
    vec![
        (
            "foreign block",
            record(codec::item_new(kinds::BLOCK, &foreign_block("c-00009"))),
        ),
        (
            "foreign change",
            record(codec::item(kinds::CHANGE, &foreign_change).unwrap()),
        ),
        (
            "ported change",
            record(codec::item(kinds::CHANGE, &plain_port).unwrap()),
        ),
        (
            "foreign ported change",
            record(codec::item(kinds::CHANGE, &foreign_port).unwrap()),
        ),
        (
            "adopt answer",
            record(codec::item_new(kinds::ANSWER, &answer(AnswerAct::Adopt))),
        ),
        (
            "intent",
            record(codec::item_new(kinds::INTENT, &intent(Some(h(85))))),
        ),
        (
            "alt set",
            record(codec::item_new(kinds::ALT_SET, &alt_set())),
        ),
        (
            "foreign review",
            record(codec::item_new(kinds::REVIEW, &foreign_review())),
        ),
        (
            "tension",
            record(codec::item_new(kinds::TENSION, &tension())),
        ),
        (
            "foreign tension",
            record(codec::item_new(kinds::TENSION, &foreign_tension())),
        ),
        (
            "foreign redaction",
            record(codec::item_new(kinds::REDACTION, &foreign_redaction())),
        ),
        (
            "port residue",
            record(codec::item_new(kinds::REDACTION_RESIDUE, &port_residue)),
        ),
        (
            "foreign assessment",
            record(codec::item_new(kinds::ASSESSMENT, &foreign_assessment())),
        ),
    ]
}

/// The TRANSITION records, encodings 0 and 1, decoded on their own.
fn transition_records() -> Vec<StreamRecord> {
    let delivered = transitions(Cause::Delivered, Some(h(64)));
    vec![
        record(transitions(Cause::Committed, Some(h(1))).frame()),
        record(delivered.frame()),
        record(codec::item_new(
            kinds::TRANSITION,
            &TransitionsV1::of(&transitions(Cause::Landed, Some(h(2)))),
        )),
    ]
}

/// The supplement as one tag history: its prerequisites are exactly the keys its records need
/// and the absent root TAG.
fn supplement_batch() -> Vec<u8> {
    let records: Vec<StreamRecord> = supplement().into_iter().map(|(_, r)| r).collect();
    let root = ObjectKey {
        kind: kinds::TAG,
        hash: h(99),
    };
    let held: BTreeSet<ObjectKey> = records.iter().map(StreamRecord::key).collect();
    let features = baseline_features();
    let mut pre: BTreeSet<ObjectKey> = BTreeSet::from([root]);
    for r in &records {
        for e in r.refs(&features).unwrap() {
            let key = ObjectKey {
                kind: e.kind.expect("the supplement names no kind-None edge"),
                hash: e.hash,
            };
            if !held.contains(&key) {
                pre.insert(key);
            }
        }
    }
    let header = StreamHeader::tag_history(features, REPO, root.hash, pre.into_iter().collect());
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    for r in &records {
        w.record(r).unwrap();
    }
    w.finish().unwrap().0
}

/// Every history batch of the corpus, named.
fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = FIXTURES
        .iter()
        .map(|n| (n.to_string(), fixture(n)))
        .collect();
    out.push(("supplement".into(), supplement_batch()));
    out
}

/// The records of `bytes`, read through the framing alone.
fn records_of(bytes: &[u8]) -> (StreamHeader, Vec<StreamRecord>) {
    let (mut r, header) = StreamReader::open(bytes).unwrap();
    let mut out = vec![];
    while let Some(rec) = r.next().unwrap() {
        out.push(rec);
    }
    (header, out)
}

/// The present records of `bytes` that hold a typed value: all but blob chunks.
fn typed_records(bytes: &[u8]) -> u64 {
    records_of(bytes)
        .1
        .iter()
        .filter(|r| matches!(r, StreamRecord::Present { key, .. } if key.kind != kinds::BLOB_CHUNK))
        .count() as u64
}

// ---------------------------------------------------------------------------------------------
// the baseline: the batch check as it ran before it shared one decoded value

/// What the baseline says one frame of a passing batch holds.
#[derive(Debug, PartialEq, Eq)]
struct FrameWant {
    raw_kind: u8,
    key: ObjectKey,
    offset: u64,
    frame_len: u32,
    dropped: bool,
    proof: Option<ObjectKey>,
    links: Vec<ObjectKey>,
    features: Vec<String>,
    index_links: u64,
}

/// The batch check of `bytes` as it ran before one value was shared: the links of
/// `StreamRecord::refs`, the record rules each decoding the bytes again, the features of
/// `format::required_features` over the bytes, and the edge, dependency, proof, root and
/// prerequisite rules in their order, within `caps`. With each frame's facts, computed apart:
/// its resolved links, its needed features and its index link count (`portable_refs_of` with no
/// feature, the store's `links_entry`).
fn baseline(bytes: &[u8], caps: CheckCaps) -> Result<(BatchReport, Vec<FrameWant>), StreamError> {
    let (mut reader, header) = StreamReader::open_history(bytes)?;
    let mut keys: Vec<ObjectKey> = vec![];
    let mut seen = RecordFeatures::default();
    let mut at: Vec<u64> = vec![];
    let mut index: HashMap<ObjectKey, u32> = HashMap::new();
    let mut edges: Vec<ObjectRef> = vec![];
    let mut ends: Vec<u32> = vec![];
    let mut proofs: Vec<(ObjectKey, usize)> = vec![];
    let mut frames: Vec<FrameWant> = vec![];
    while let Some(r) = reader.next()? {
        let (n, offset) = reader.position();
        let key = r.key();
        if index.contains_key(&key) {
            return Err(
                StreamError::bad_record(n, offset, Reason::ObjectIsRepeated).with_object(Some(key))
            );
        }
        let refs = r
            .refs(&header.features)
            .map_err(|e| StreamError::from_record(n, offset, &e).with_object(Some(key)))?;
        record_rules_decoding_again_for_tests(n, offset, &r)?;
        seen.add(&r);
        let proof = r.proof();
        if (edges.len() + proofs.len() + refs.len() + usize::from(proof.is_some())) as u64
            > caps.edges
        {
            return Err(
                StreamError::limit(Some(n), offset, LIMIT_GRAPH_EDGES).with_object(Some(key))
            );
        }
        let (dropped, features, index_links) = match &r {
            StreamRecord::Present { payload, .. } => {
                let mut f: Vec<String> =
                    rdy_store::format::required_features(r.raw_kind(), payload)
                        .into_iter()
                        .map(|f| f.name)
                        .collect();
                f.sort();
                let links = if key.kind == kinds::BLOB_CHUNK {
                    0
                } else {
                    portable_refs_of(&StoreFeatures::of(vec![]), r.raw_kind(), payload)
                        .unwrap()
                        .len() as u64
                };
                (false, f, links)
            }
            StreamRecord::Dropped { .. } => (true, vec![], 0),
        };
        frames.push(FrameWant {
            raw_kind: r.raw_kind(),
            key,
            offset,
            frame_len: r.frame_len() as u32,
            dropped,
            proof,
            links: vec![],
            features,
            index_links,
        });
        edges.extend(refs);
        if let Some(p) = proof {
            proofs.push((p, keys.len()));
        }
        index.insert(key, keys.len() as u32);
        keys.push(key);
        at.push(offset);
        ends.push(edges.len() as u32);
    }
    let totals = reader.finish()?;
    let prerequisites = &header.prerequisites;
    let is_prerequisite = |k: &ObjectKey| prerequisites.binary_search(k).is_ok();
    let mut needed: BTreeSet<ObjectKey> = BTreeSet::new();
    let mut depth: Vec<u64> = vec![];
    let mut start = 0usize;
    for (i, end) in ends.iter().enumerate() {
        let n = i as u64 + 1;
        let fail = |reason| StreamError::bad_record(n, at[i], reason).with_object(Some(keys[i]));
        let mut deps = vec![];
        for e in &edges[start..*end as usize] {
            let target = match e.kind {
                Some(kind) => ObjectKey { kind, hash: e.hash },
                None => {
                    let mut found = sync_kinds()
                        .iter()
                        .map(|&kind| ObjectKey { kind, hash: e.hash })
                        .filter(|k| index.contains_key(k) || is_prerequisite(k));
                    let Some(first) = found.next() else {
                        return Err(fail(Reason::UnlistedDependency(*e)));
                    };
                    if found.next().is_some() {
                        return Err(fail(Reason::AmbiguousObjectKind));
                    }
                    first
                }
            };
            match index.get(&target) {
                Some(&j) if (j as usize) < i => deps.push(depth[j as usize]),
                Some(_) => return Err(fail(Reason::DependencyCycle)),
                None if is_prerequisite(&target) => {
                    needed.insert(target);
                }
                None => return Err(fail(Reason::UnlistedDependency(*e))),
            }
            frames[i].links.push(target);
        }
        let d = deps.into_iter().max().map_or(1, |d| d + 1);
        if d > caps.depth {
            return Err(
                StreamError::limit(Some(n), at[i], LIMIT_HISTORY_DEPTH).with_object(Some(keys[i]))
            );
        }
        depth.push(d);
        start = *end as usize;
    }
    for &(p, i) in &proofs {
        if !index.contains_key(&p) {
            if !is_prerequisite(&p) {
                let named = ObjectRef {
                    kind: Some(p.kind),
                    hash: p.hash,
                };
                return Err(StreamError::bad_record(
                    i as u64 + 1,
                    at[i],
                    Reason::UnlistedDependency(named),
                )
                .with_object(Some(keys[i])));
            }
            needed.insert(p);
        }
    }
    let at_header = |reason| StreamError::bad_record(0, 12, reason);
    let root = header.root();
    if !index.contains_key(&root) {
        if !is_prerequisite(&root) {
            let reason = match header.purpose {
                StreamPurpose::CanonHistory => Reason::RootLandingNotListed,
                StreamPurpose::ViewHistory => Reason::RootViewOpNotListed,
                _ => Reason::RootTagNotListed,
            };
            return Err(at_header(reason));
        }
        needed.insert(root);
    }
    if let Some(extra) = prerequisites.iter().find(|k| !needed.contains(k)) {
        return Err(at_header(Reason::UnneededPrerequisite(*extra)));
    }
    Ok((
        BatchReport {
            header,
            keys,
            record_features: seen.finish(),
            records: totals.records,
            bytes: totals.bytes,
            digest: totals.digest,
        },
        frames,
    ))
}

fn prepared(bytes: &[u8]) -> Result<CheckedBatch, StreamError> {
    check_batch_prepared_with(bytes, |_| Ok::<(), StreamError>(()), |_, _, _, _| Ok(()))
}

/// Each frame of `c`, as the baseline states one.
fn frames_of(c: &CheckedBatch) -> Vec<FrameWant> {
    c.frames()
        .iter()
        .enumerate()
        .map(|(i, f)| FrameWant {
            raw_kind: f.raw_kind(),
            key: f.key(),
            offset: f.offset(),
            frame_len: f.frame_len(),
            dropped: f.is_dropped(),
            proof: f.proof(),
            links: c.links(i).to_vec(),
            features: c.features(i).map(|f| f.name.clone()).collect(),
            index_links: f.index_links(),
        })
        .collect()
}

/// `bytes` gives the baseline's result through [`check_batch`] and the prepared check, within
/// `caps`: the same report and frame facts, or the same complete error (code, message, fix,
/// record, offset and object). Returns the error, if any.
fn same_as_baseline(name: &str, bytes: &[u8], caps: CheckCaps) -> Option<StreamError> {
    let _g = lower_caps_for_tests(caps);
    let want = baseline(bytes, caps);
    let got = check_batch(bytes);
    let ready = prepared(bytes);
    match want {
        Ok((report, frames)) => {
            assert_eq!(got.as_ref(), Ok(&report), "{name}");
            let ready = ready.unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(ready.report(), &report, "{name}");
            assert_eq!(frames_of(&ready), frames, "{name}");
            assert_eq!(ready.input_len(), bytes.len() as u64, "{name}");
            let totals = StreamTotals {
                records: report.records,
                bytes: report.bytes,
                digest: report.digest,
            };
            assert_eq!(ready.binding(), batch_binding(&report.header, &totals));
            assert_eq!(ready.into_report(), report, "{name}");
            None
        }
        Err(e) => {
            assert_eq!(got.as_ref().map(|_| ()), Err(&e), "{name}");
            assert_eq!(ready.map(|_| ()), Err(e.clone()), "{name}");
            Some(e)
        }
    }
}

const RELEASED: CheckCaps = CheckCaps::RELEASED;

/// The TAG key a crafted batch names as its absent root.
fn root() -> ObjectKey {
    ObjectKey {
        kind: kinds::TAG,
        hash: h(0x11),
    }
}

/// `records` as one tag history rooted at the absent [`root`], whose prerequisites are the root
/// and `extra`.
fn crafted(records: &[StreamRecord], extra: &[ObjectKey]) -> Vec<u8> {
    let mut pre: Vec<ObjectKey> = extra.to_vec();
    pre.push(root());
    pre.sort();
    pre.dedup();
    let header = StreamHeader::tag_history(baseline_features(), REPO, root().hash, pre);
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    for r in records {
        w.record(r).unwrap();
    }
    w.finish().unwrap().0
}

/// The object frames of `records` written by hand after a crafted header (the writer refuses a
/// repeated key), with a trailer that agrees with them.
fn hand_written(records: &[StreamRecord], extra: &[ObjectKey]) -> Vec<u8> {
    let empty = crafted(&[], extra);
    let head_end = empty.len() - 37 - 48;
    let mut out = empty[..head_end].to_vec();
    let (hl, hh) = (
        u32::from_le_bytes(empty[12..16].try_into().unwrap()),
        Hash(empty[17..49].try_into().unwrap()),
    );
    let mut chain = vec![0xF0u8];
    chain.extend_from_slice(&u64::from(hl).to_le_bytes());
    chain.extend_from_slice(&hh.0);
    for r in records {
        let StreamRecord::Present { payload, .. } = r else {
            panic!("present records only")
        };
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.push(r.raw_kind());
        out.extend_from_slice(&Hash::of(payload).0);
        out.extend_from_slice(payload);
        chain.push(r.raw_kind());
        chain.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        chain.extend_from_slice(&Hash::of(payload).0);
    }
    let mut trailer = (records.len() as u64).to_le_bytes().to_vec();
    trailer.extend_from_slice(&(out.len() as u64).to_le_bytes());
    trailer.extend_from_slice(&Hash::of(&chain).0);
    out.extend_from_slice(&(trailer.len() as u32).to_le_bytes());
    out.push(0xFF);
    out.extend_from_slice(&Hash::of(&trailer).0);
    out.extend_from_slice(&trailer);
    out
}

/// A present record of logical kind `kind`, encoding 0, holding `payload`.
fn raw(kind: u8, payload: Vec<u8>) -> StreamRecord {
    StreamRecord::Present {
        key: ObjectKey {
            kind,
            hash: Hash::of(&payload),
        },
        encoding: 0,
        payload,
    }
}

/// A present record of the versioned frame of `kind` at `version` holding `body`.
fn versioned(kind: u8, version: u16, body: &[u8]) -> StreamRecord {
    let mut payload = version.to_le_bytes().to_vec();
    payload.extend_from_slice(body);
    record((kind | VERSIONED, payload))
}

fn intent_record(prev: Option<Hash>) -> StreamRecord {
    record(codec::item_new(kinds::INTENT, &intent(prev)))
}

fn tag_record() -> StreamRecord {
    record(codec::item_new(
        kinds::TAG,
        &Tag {
            name: "v1".into(),
            landing: h(0x21),
            message: None,
            tagged_by: prov("human:alice", Role::Approver),
        },
    ))
}

fn sorted(mut keys: Vec<ObjectKey>) -> Vec<ObjectKey> {
    keys.sort();
    keys
}

fn code(e: &StreamError) -> (&'static str, &str) {
    (e.code, e.message.as_str())
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn r59010_each_present_kind_decodes_once() {
    // the corpus holds every portable kind in every encoding it admits, and dropped forms
    let mut seen: BTreeSet<(u8, u16)> = BTreeSet::new();
    let mut dropped: BTreeSet<u8> = BTreeSet::new();
    for (_, bytes) in corpus() {
        for r in records_of(&bytes).1 {
            match r {
                StreamRecord::Present { key, encoding, .. } => {
                    seen.insert((key.kind, encoding));
                }
                StreamRecord::Dropped { key, .. } => {
                    dropped.insert(key.kind);
                }
            }
        }
    }
    for r in transition_records() {
        let StreamRecord::Present { key, encoding, .. } = &r else {
            unreachable!()
        };
        seen.insert((key.kind, *encoding));
        // a TRANSITION is decoded on its own: one value, the links of its bytes
        bounded::reset_decodes_for_tests();
        let d = DecodedRecord::decode(&r).unwrap();
        assert_eq!(bounded::decodes_for_tests(), 1);
        assert_eq!(d.kind(), Some(kinds::TRANSITION));
        let StreamRecord::Present { payload, .. } = &r else {
            unreachable!()
        };
        assert_eq!(
            r.refs(&baseline_features()).unwrap(),
            portable_refs_of(&baseline_features(), r.raw_kind(), payload).unwrap()
        );
    }
    for kind in rdy_store::object_refs::PORTABLE_KINDS {
        for v in codec::first(kind)..=codec::newest(kind) {
            assert!(seen.contains(&(kind, v)), "kind {kind} encoding {v}");
        }
    }
    assert_eq!(
        dropped,
        BTreeSet::from([kinds::BLOCK, kinds::CHANGE, kinds::BLOB_CHUNK])
    );

    for (name, bytes) in corpus() {
        // the report, links and features of the check before the shared value
        assert_eq!(same_as_baseline(&name, &bytes, RELEASED), None, "{name}");
        // one typed value per present typed record, whatever the kind; a chunk and a dropped
        // record have none
        let want = typed_records(&bytes);
        bounded::reset_decodes_for_tests();
        check_batch(&bytes[..]).unwrap();
        assert_eq!(bounded::decodes_for_tests(), want, "{name}");
        bounded::reset_decodes_for_tests();
        let ready = prepared(&bytes).unwrap();
        assert_eq!(bounded::decodes_for_tests(), want, "{name}: prepared");
        // the caller's check reads the same value: no decode of its own, one value at a time
        let mut calls = 0u64;
        let mut typed = 0u64;
        bounded::reset_decodes_for_tests();
        check_batch_prepared_with(
            &bytes[..],
            |_| Ok::<(), StreamError>(()),
            |_, _, r, d| {
                calls += 1;
                match r {
                    StreamRecord::Present { key, .. } => {
                        assert_eq!(d.kind(), Some(key.kind));
                        if key.kind != kinds::BLOB_CHUNK {
                            typed += 1;
                        }
                    }
                    StreamRecord::Dropped { .. } => {
                        assert!(matches!(d, DecodedRecord::Dropped))
                    }
                }
                assert_eq!(bounded::decodes_for_tests(), typed, "{name}");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls, ready.report().records, "{name}");
    }
}

#[test]
fn r59010_bad_records_keep_the_same_refusal() {
    let good = intent_record(None);
    let good_key = good.key();
    // each bad record second, after a good one, so its position is not the first's
    let mut cases: Vec<(String, StreamRecord, Vec<ObjectKey>)> = vec![];
    // malformed postcard: every typed record of the corpus, cut by one byte and lengthened by
    // one byte
    let mut samples: Vec<StreamRecord> = supplement().into_iter().map(|(_, r)| r).collect();
    for (_, bytes) in corpus() {
        samples.extend(
            records_of(&bytes)
                .1
                .into_iter()
                .filter(|r| matches!(r, StreamRecord::Present { .. })),
        );
    }
    for r in &samples {
        let StreamRecord::Present {
            key,
            encoding,
            payload,
        } = r
        else {
            unreachable!()
        };
        let head = if *encoding == 0 { 0 } else { 2 };
        if payload.len() > head {
            let mut cut = payload.clone();
            cut.pop();
            cases.push((format!("{key} cut"), record((r.raw_kind(), cut)), vec![]));
        }
        let mut long = payload.clone();
        long.push(0);
        cases.push((format!("{key} long"), record((r.raw_kind(), long)), vec![]));
    }
    cases.push((
        "garbage block".into(),
        raw(kinds::BLOCK, vec![0xff; 9]),
        vec![],
    ));
    // excess nesting: a field 200 lists deep
    let mut deep = FieldValue::Str("x".into());
    for _ in 0..200 {
        deep = FieldValue::List(vec![deep]);
    }
    let mut b = block_of("p-DEEP1", "prose", "");
    b.fields.insert("rows".into(), deep);
    cases.push((
        "deep block".into(),
        record(codec::item_new(kinds::BLOCK, &b)),
        vec![],
    ));
    // a noncanonical snapshot: the empty map with a length varint that is not minimal
    cases.push((
        "noncanonical snapshot".into(),
        raw(kinds::SNAPSHOT, vec![0x80, 0x00]),
        vec![],
    ));
    // a noncanonical delta: an empty delta whose `set` length varint is not minimal
    let delta = SnapshotDelta {
        landing: h(0x31),
        base: None,
        full: h(0x32),
        set: vec![],
        drop: vec![],
    };
    let body = postcard::to_allocvec(&delta).unwrap();
    let canonical = versioned(kinds::SNAPSHOT_DELTA, 1, &body);
    let mut wide = body[..body.len() - 2].to_vec();
    wide.extend_from_slice(&[0x80, 0x00, 0x00]);
    cases.push((
        "noncanonical delta".into(),
        versioned(kinds::SNAPSHOT_DELTA, 1, &wide),
        vec![ObjectKey {
            kind: kinds::LANDING,
            hash: h(0x31),
        }],
    ));
    // bad blob length and chunks: the store's check reads the manifest's links only (the
    // engine's limits refuse them)
    for (len, chunks) in [(5u64, 2u8), ((1 << 20) + 1, 1), (0, 1)] {
        let m = kinds::BlobManifest {
            len,
            chunks: (0..chunks).map(|i| h(0x40 + i)).collect(),
        };
        let keys = m
            .chunks
            .iter()
            .map(|c| ObjectKey {
                kind: kinds::BLOB_CHUNK,
                hash: *c,
            })
            .collect();
        cases.push((
            format!("blob {len}/{chunks}"),
            record(codec::item_new(kinds::BLOB_MANIFEST, &m)),
            keys,
        ));
    }
    cases.push((
        "noncanonical blob manifest".into(),
        raw(kinds::BLOB_MANIFEST, vec![0x80, 0x00, 0x00]),
        vec![],
    ));
    // an invalid ANSWER: an adopt act whose item is not an adopt item
    let mut a = answer(AnswerAct::Adopt);
    a.item = "adopt:item".into();
    cases.push((
        "invalid answer".into(),
        record(codec::item_new(kinds::ANSWER, &a)),
        vec![],
    ));
    // an invalid PLAN: a whole version with no view name
    let plan = Plan {
        stable: None,
        prev: None,
        view: String::new(),
        proposal: None,
        answers: vec![],
        heads: vec![],
        provenance: prov("agent:w", Role::Writer),
        seal: None,
        read_identity: None,
    };
    cases.push((
        "invalid plan".into(),
        raw(kinds::PLAN, postcard::to_allocvec(&plan).unwrap()),
        vec![],
    ));
    let mut refused = BTreeMap::new();
    for (name, bad, extra) in &cases {
        let mut extra = extra.clone();
        // a record that passes still needs its links listed
        if let Ok(refs) = bad.refs(&baseline_features()) {
            extra.extend(
                refs.into_iter()
                    .filter_map(|e| e.kind.map(|kind| ObjectKey { kind, hash: e.hash })),
            );
        }
        extra.retain(|k| *k != good_key);
        let bytes = crafted(&[good.clone(), bad.clone()], &extra);
        if let Some(e) = same_as_baseline(name, &bytes, RELEASED) {
            assert_eq!(e.record, Some(2), "{name}: {e:?}");
            assert_eq!(e.object, Some(bad.key()), "{name}");
            refused.insert(name.clone(), e);
        }
    }
    // the refusals of the named cases, as the check always gave them
    let msg = |name: &str| refused[name].message.clone();
    assert!(msg("garbage block").ends_with(": record does not decode"));
    assert!(msg("deep block").ends_with("stream exceeds 64 nesting"));
    let snap = &cases
        .iter()
        .find(|c| c.0 == "noncanonical snapshot")
        .unwrap()
        .1;
    assert!(
        msg("noncanonical snapshot").ends_with(&rdy_store::snapshot::noncanonical_snapshot(
            &snap.key().hash
        )),
        "{}",
        msg("noncanonical snapshot")
    );
    assert!(msg("noncanonical delta").ends_with(": record does not decode"));
    assert!(msg("invalid answer").ends_with(": invalid adopt answer item"));
    assert!(msg("invalid plan").ends_with(": invalid fields"));
    // the canonical forms pass, and the store's check passes the manifests the engine refuses
    for name in [
        "blob 5/2",
        "blob 1048577/1",
        "blob 0/1",
        "noncanonical blob manifest",
    ] {
        assert!(!refused.contains_key(name), "{name}");
    }
    let bytes = crafted(
        &[canonical],
        &[ObjectKey {
            kind: kinds::LANDING,
            hash: h(0x31),
        }],
    );
    assert_eq!(same_as_baseline("canonical delta", &bytes, RELEASED), None);
    // every cut or lengthened typed record is refused; a chunk's bytes are any bytes
    for (name, bad, _) in &cases {
        if (name.ends_with(" cut") || name.ends_with(" long"))
            && bad.key().kind != kinds::BLOB_CHUNK
        {
            assert!(refused.contains_key(name), "{name}");
        }
    }
}

#[test]
fn r59010_bad_edges_and_trailers_keep_the_same_refusal() {
    let a = intent_record(None);
    let b = intent_record(Some(a.key().hash));
    let a_key = a.key();
    let fine = crafted(&[a.clone(), b.clone()], &[]);
    assert_eq!(same_as_baseline("fine", &fine, RELEASED), None);
    let head = crafted(&[], &[]).len() - 37 - 48;
    let mut cases: Vec<(&str, Vec<u8>, (&str, String))> = vec![];
    // wrong hash: one payload byte changed under its frame's hash
    let mut wrong = fine.clone();
    wrong[head + 37 + 3] ^= 1;
    cases.push((
        "wrong hash",
        wrong,
        (
            "E_BAD_RECORD",
            format!("stream record 1 at byte {head}: hash mismatch"),
        ),
    ));
    // duplicate key
    cases.push((
        "duplicate key",
        hand_written(&[a.clone(), a.clone()], &[]),
        (
            "E_BAD_RECORD",
            format!(
                "stream record 2 at byte {}: object is repeated",
                head + 37 + a.frame_len() as usize
            ),
        ),
    ));
    // wrong dependency order: the record that needs another before it
    cases.push((
        "dependency order",
        crafted(&[b.clone(), a.clone()], &[]),
        (
            "E_BAD_RECORD",
            format!("stream record 1 at byte {head}: dependency cycle"),
        ),
    ));
    // a missing prerequisite
    cases.push((
        "missing prerequisite",
        crafted(std::slice::from_ref(&b), &[]),
        (
            "E_BAD_RECORD",
            format!(
                "stream record 1 at byte {head}: unlisted dependency {}:{}",
                kinds::INTENT,
                a_key.hash
            ),
        ),
    ));
    // an ambiguous prerequisite: a kind-None edge that two listed kinds hold
    let mut confirm = answer(AnswerAct::Confirm);
    confirm.item = "P-x@1".into();
    confirm.card_hash = None;
    confirm.targets = vec![h(0x51)];
    let targets = record(codec::item_new(kinds::ANSWER, &confirm));
    let two = [
        ObjectKey {
            kind: kinds::BLOCK,
            hash: h(0x51),
        },
        ObjectKey {
            kind: kinds::CHANGE,
            hash: h(0x51),
        },
    ];
    let bytes = crafted(std::slice::from_ref(&targets), &two);
    let head2 = crafted(&[], &two).len() - 37 - 48;
    cases.push((
        "ambiguous prerequisite",
        bytes,
        (
            "E_BAD_RECORD",
            format!("stream record 1 at byte {head2}: ambiguous object kind"),
        ),
    ));
    // the same edge with one listed kind passes, its link resolved to it
    let one = crafted(std::slice::from_ref(&targets), &two[..1]);
    assert_eq!(same_as_baseline("one kind", &one, RELEASED), None);
    assert_eq!(prepared(&one).unwrap().links(0), &two[..1]);
    // an unneeded prerequisite
    let extra = ObjectKey {
        kind: kinds::BLOCK,
        hash: h(0x52),
    };
    cases.push((
        "unneeded prerequisite",
        crafted(std::slice::from_ref(&a), &[extra]),
        (
            "E_BAD_RECORD",
            format!("stream record 0 at byte 12: unneeded prerequisite {extra}"),
        ),
    ));
    // a bad proof: a dropped record whose apply is neither in the batch nor listed
    let dropped = StreamRecord::Dropped {
        key: ObjectKey {
            kind: kinds::BLOB_CHUNK,
            hash: h(0x53),
        },
        encoding: 0,
        original_len: 10,
        apply: h(0x54),
        residue: None,
    };
    let empty = crafted(&[], &[]);
    let mut w = StreamWriter::new(
        Vec::new(),
        &StreamHeader::tag_history(baseline_features(), REPO, root().hash, vec![root()]),
    )
    .unwrap();
    w.record(&dropped).unwrap();
    let no_proof = w.finish().unwrap().0;
    cases.push((
        "bad proof",
        no_proof,
        (
            "E_BAD_RECORD",
            format!(
                "stream record 1 at byte {}: unlisted dependency {}:{}",
                empty.len() - 37 - 48,
                kinds::REDACTION_APPLY,
                h(0x54)
            ),
        ),
    ));
    // the proof listed passes, and the dropped frame keeps it
    let proof_key = ObjectKey {
        kind: kinds::REDACTION_APPLY,
        hash: h(0x54),
    };
    let mut w = StreamWriter::new(
        Vec::new(),
        &StreamHeader::tag_history(
            baseline_features(),
            REPO,
            root().hash,
            sorted(vec![proof_key, root()]),
        ),
    )
    .unwrap();
    w.record(&dropped).unwrap();
    let with_proof = w.finish().unwrap().0;
    assert_eq!(same_as_baseline("proof", &with_proof, RELEASED), None);
    let ready = prepared(&with_proof).unwrap();
    assert!(ready.frames()[0].is_dropped());
    assert_eq!(ready.frames()[0].proof(), Some(proof_key));
    // a bad trailer: its record count changed, its hash made to agree
    let mut bad_trailer = fine.clone();
    let t = bad_trailer.len() - 48;
    bad_trailer[t] = 3;
    let hash = Hash::of(&bad_trailer[t..]);
    bad_trailer[t - 32..t].copy_from_slice(&hash.0);
    cases.push((
        "bad trailer",
        bad_trailer,
        (
            "E_STREAM_TRUNCATED",
            "stream trailer is missing or does not match".into(),
        ),
    ));
    // trailing bytes
    let mut trailing = fine.clone();
    trailing.push(0);
    cases.push((
        "trailing bytes",
        trailing,
        (
            "E_BAD_RECORD",
            format!("stream record 4 at byte {}: trailing bytes", fine.len()),
        ),
    ));
    // the end of the input before the trailer
    cases.push((
        "no trailer",
        fine[..fine.len() - 85].to_vec(),
        (
            "E_STREAM_TRUNCATED",
            "stream trailer is missing or does not match".into(),
        ),
    ));
    for (name, bytes, (want_code, want)) in &cases {
        let e = same_as_baseline(name, bytes, RELEASED).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(code(&e), (*want_code, want.as_str()), "{name}");
    }

    // every count bound at N and N + 1, with small bounds
    let exceeds = |label: &str| ("E_LIMIT", format!("stream exceeds {label}"));
    let chain = |n: usize| -> Vec<StreamRecord> {
        let mut out = vec![];
        let mut prev = None;
        for _ in 0..n {
            let r = intent_record(prev);
            prev = Some(r.key().hash);
            out.push(r);
        }
        out
    };
    let lower = |f: &dyn Fn(&mut CheckCaps)| {
        let mut c = RELEASED;
        f(&mut c);
        c
    };
    // records: 3, then 4
    let caps = lower(&|c| c.records = 3);
    assert_eq!(
        same_as_baseline("3 records", &crafted(&chain(3), &[]), caps),
        None
    );
    let e = same_as_baseline("4 records", &crafted(&chain(4), &[]), caps).unwrap();
    assert_eq!(
        code(&e),
        (
            exceeds(LIMIT_OBJECT_RECORDS).0,
            exceeds(LIMIT_OBJECT_RECORDS).1.as_str()
        )
    );
    assert_eq!(e.record, Some(4));
    // edges: a chain of 4 has 3 edges; then 4 edges
    let caps = lower(&|c| c.edges = 3);
    assert_eq!(
        same_as_baseline("3 edges", &crafted(&chain(4), &[]), caps),
        None
    );
    let e = same_as_baseline("4 edges", &crafted(&chain(5), &[]), caps).unwrap();
    assert_eq!(
        code(&e),
        (
            exceeds(LIMIT_GRAPH_EDGES).0,
            exceeds(LIMIT_GRAPH_EDGES).1.as_str()
        )
    );
    assert_eq!(e.record, Some(5));
    // history depth: 4, then 5
    let caps = lower(&|c| c.depth = 4);
    assert_eq!(
        same_as_baseline("depth 4", &crafted(&chain(4), &[]), caps),
        None
    );
    let e = same_as_baseline("depth 5", &crafted(&chain(5), &[]), caps).unwrap();
    assert_eq!(
        code(&e),
        (
            exceeds(LIMIT_HISTORY_DEPTH).0,
            exceeds(LIMIT_HISTORY_DEPTH).1.as_str()
        )
    );
    assert_eq!(e.record, Some(5));
    // prerequisites: 3, then 4
    let pre = |n: u8| -> Vec<ObjectKey> {
        (0..n)
            .map(|i| ObjectKey {
                kind: kinds::BLOCK,
                hash: h(0x60 + i),
            })
            .collect()
    };
    let snapshot = |n: u8| -> StreamRecord {
        record(codec::item_new(
            kinds::SNAPSHOT,
            &StateSnapshot {
                blocks: (0..n)
                    .map(|i| (id(&format!("b-0000{i}")), h(0x60 + i)))
                    .collect(),
            },
        ))
    };
    let caps = lower(&|c| c.prerequisites = 3);
    assert_eq!(
        same_as_baseline("3 prerequisites", &crafted(&[snapshot(2)], &pre(2)), caps),
        None
    );
    let e = same_as_baseline("4 prerequisites", &crafted(&[snapshot(3)], &pre(3)), caps).unwrap();
    assert_eq!(
        code(&e),
        (
            exceeds(LIMIT_PREREQUISITES).0,
            exceeds(LIMIT_PREREQUISITES).1.as_str()
        )
    );
    assert_eq!((e.record, e.offset), (Some(0), 12));
    // pending drops: 2, then 3
    let drops = |n: u8| -> Vec<u8> {
        let mut w = StreamWriter::new(
            Vec::new(),
            &StreamHeader::tag_history(
                baseline_features(),
                REPO,
                root().hash,
                sorted(vec![proof_key, root()]),
            ),
        )
        .unwrap();
        for i in 0..n {
            w.record(&StreamRecord::Dropped {
                key: ObjectKey {
                    kind: kinds::BLOB_CHUNK,
                    hash: h(0x70 + i),
                },
                encoding: 0,
                original_len: 10,
                apply: proof_key.hash,
                residue: None,
            })
            .unwrap();
        }
        w.finish().unwrap().0
    };
    let caps = lower(&|c| c.pending_drops = 2);
    assert_eq!(same_as_baseline("2 drops", &drops(2), caps), None);
    let e = same_as_baseline("3 drops", &drops(3), caps).unwrap();
    assert_eq!(
        code(&e),
        (
            exceeds(LIMIT_PENDING_DROPS).0,
            exceeds(LIMIT_PENDING_DROPS).1.as_str()
        )
    );
    assert_eq!(e.record, Some(3));
    // total bytes: the whole batch, then one byte less
    let caps = lower(&|c| c.total_bytes = fine.len() as u64);
    assert_eq!(same_as_baseline("bytes at N", &fine, caps), None);
    let caps = lower(&|c| c.total_bytes = fine.len() as u64 - 1);
    let e = same_as_baseline("bytes at N + 1", &fine, caps).unwrap();
    assert_eq!(
        code(&e),
        (
            exceeds(LIMIT_TOTAL_BYTES).0,
            exceeds(LIMIT_TOTAL_BYTES).1.as_str()
        )
    );
    assert_eq!(e.record, Some(3), "the trailer would pass the bound");
    // the released bounds again once the guard drops
    assert_eq!(
        check_batch(&crafted(&chain(5), &[])[..]).unwrap().records,
        5
    );
    // a root tag that the batch holds needs no prerequisite
    let t = tag_record();
    let header = StreamHeader::tag_history(
        baseline_features(),
        REPO,
        t.key().hash,
        vec![ObjectKey {
            kind: kinds::LANDING,
            hash: h(0x21),
        }],
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    w.record(&t).unwrap();
    assert_eq!(
        same_as_baseline("held root", &w.finish().unwrap().0, RELEASED),
        None
    );
}

/// A store whose one landing's change creates a 40,000-byte block, compacted so the change's
/// copy of the block maps onto the BLOCK frame's bytes; its canon history from that landing.
fn mapped_history() -> (tempfile::TempDir, Vec<u8>) {
    let text = |n: usize, seed: u64| -> String {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                char::from(b'a' + (x % 26) as u8)
            })
            .collect()
    };
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("p");
    let mut s = Store::create(&path).unwrap();
    let mut c = change_of(
        vec![Op::Create {
            block: block_of("b-00001", "claim", &text(40_000, 7)),
        }],
        0,
    );
    c.deps.clear();
    s.commit_to_view("work", c).unwrap();
    let p = s
        .propose("work", "p1", prov("agent:w", Role::Writer))
        .unwrap();
    let landing = s
        .land_atomic(LandRequest {
            proposal: p,
            accept: None,
            approver: prov("human:alice", Role::Approver),
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap()
        .landing;
    let r = s.compact(false).unwrap();
    assert!(r.deduplicated_bytes >= 4096, "{}", r.deduplicated_bytes);
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    let reader = s.reader();
    let found = history_closure_before(
        &reader,
        StreamPurpose::CanonHistory,
        landing,
        &BTreeSet::new(),
        None,
        END_OF_LOG,
    )
    .unwrap();
    let header = StreamHeader::canon_history(
        found.features,
        reader.repo_id().unwrap().unwrap(),
        landing,
        found.prerequisites,
    );
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    let (bytes, _) = write_batch(&mut source, &header, &found.records, Vec::new()).unwrap();
    drop(reader);
    drop(s);
    (d, bytes)
}

#[test]
fn r59010_index_links_keep_the_landing_state_edge() {
    let (_d, mapped) = mapped_history();
    let mut landings = 0;
    let mut changes = 0;
    let mut deltas = 0;
    for (name, bytes) in [
        ("mapped", mapped),
        ("reduction", fixture("sync4-reduction.bin")),
        ("canon", fixture("sync4-canon-history.bin")),
    ] {
        assert_eq!(same_as_baseline(name, &bytes, RELEASED), None, "{name}");
        let (header, records) = records_of(&bytes);
        assert!(header.features.has(FEATURE_SNAPSHOT_DELTA), "{name}");
        let ready = prepared(&bytes).unwrap();
        for (i, (f, r)) in ready.frames().iter().zip(&records).enumerate() {
            let StreamRecord::Present { payload, .. } = r else {
                assert_eq!(f.index_links(), 0);
                continue;
            };
            // the store's index count: every link, with no feature (`links_entry`)
            let old = if f.key().kind == kinds::BLOB_CHUNK {
                0
            } else {
                portable_refs_of(&StoreFeatures::of(vec![]), r.raw_kind(), payload)
                    .unwrap()
                    .len() as u64
            };
            assert_eq!(f.index_links(), old, "{name} {}", f.key());
            match DecodedRecord::decode(r).unwrap() {
                DecodedRecord::Landing(l) => {
                    landings += 1;
                    // the portable links leave the full state out; the index keeps it
                    let state = ObjectKey {
                        kind: kinds::SNAPSHOT,
                        hash: l.state,
                    };
                    assert!(!ready.links(i).contains(&state), "{name}");
                    assert_eq!(f.index_links(), ready.links(i).len() as u64 + 1, "{name}");
                }
                DecodedRecord::Change(c) => {
                    if c.ops
                        .iter()
                        .any(|op| matches!(op, Op::Create { .. } | Op::Update { .. }))
                    {
                        changes += 1;
                    }
                    assert_eq!(f.index_links(), ready.links(i).len() as u64, "{name}");
                }
                DecodedRecord::Plan(p) => {
                    if matches!(*p, PlanRecord::Delta(_)) {
                        deltas += 1;
                    }
                    assert_eq!(f.index_links(), ready.links(i).len() as u64, "{name}");
                }
                _ => assert_eq!(f.index_links(), ready.links(i).len() as u64, "{name}"),
            }
        }
    }
    assert!(landings >= 3, "{landings}");
    assert!(changes >= 2, "{changes}");
    assert!(deltas >= 1, "{deltas}");
}

#[test]
fn r59010_checked_batch_is_made_only_by_the_check() {
    // no JSON, no default, no clone and no report makes one: each assertion is ambiguous, so it
    // fails to compile, if the type ever gains the trait
    trait AmbiguousIfDeserialize<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfDeserialize<()> for T {}
    #[allow(dead_code)]
    struct Deserializes;
    impl<T: serde::de::DeserializeOwned> AmbiguousIfDeserialize<Deserializes> for T {}
    <CheckedBatch as AmbiguousIfDeserialize<_>>::check();
    trait AmbiguousIfMade<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfMade<()> for T {}
    #[allow(dead_code)]
    struct Defaults;
    impl<T: Default> AmbiguousIfMade<Defaults> for T {}
    #[allow(dead_code)]
    struct Clones;
    impl<T: Clone> AmbiguousIfMade<Clones> for T {}
    #[allow(dead_code)]
    struct FromReport;
    impl<T: From<BatchReport>> AmbiguousIfMade<FromReport> for T {}
    <CheckedBatch as AmbiguousIfMade<_>>::check();

    // a changed byte is another input: refused, no capsule
    let marker = "a payload no capsule keeps";
    let b = record(codec::item_new(
        kinds::BLOCK,
        &block_of("c-00077", "claim", marker),
    ));
    let bytes = crafted(std::slice::from_ref(&b), &[]);
    let ready = prepared(&bytes).unwrap();
    let mut changed = bytes.clone();
    let at = bytes.len() - 85 - 3;
    changed[at] ^= 1;
    assert!(prepared(&changed).is_err());
    // the capsule keeps facts, never the payload or the decoded value
    let shown = format!("{ready:?}");
    assert!(!shown.contains(marker), "{shown}");
    let hex: String = marker.bytes().map(|b| format!("{b:02x}")).collect();
    assert!(!shown.contains(&hex));
    assert!(!shown.contains(&format!("{:?}", marker.as_bytes())));
    // its binding is the bytes': the same bytes again give the same one, other bytes another
    assert_eq!(prepared(&bytes).unwrap().binding(), ready.binding());
    let other = crafted(&[intent_record(None)], &[]);
    assert_ne!(prepared(&other).unwrap().binding(), ready.binding());
    // a stream format of the prelude is bound too: a format-4 copy of a format-2 tag history
    let mut four = bytes.clone();
    four[8..12].copy_from_slice(&4u32.to_le_bytes());
    if let Ok(c) = prepared(&four) {
        assert_ne!(c.binding(), ready.binding());
    }
}
