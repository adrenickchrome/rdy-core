//! ok4 (D-AYYD8, QD1, D-C8RZW, D-69AY3): the carried shapes. An ASSESSMENT, a DISPUTE and its
//! DISPUTE_RESOLUTION, a CHECK_REQUEST, Check reviews of its three verdicts and the three carried
//! answer acts are stored, loaded, streamed, taken, reindexed and checked byte for byte, with no
//! product writer. Old encoding-0 changes, landings and residues keep their ids and bytes beside
//! the latest encodings. Every carried rule refuses before a byte is written.
#[path = "support/carried.rs"]
mod carried_fixture;

use rdy_model::assessment::*;
use rdy_model::check_request::{CheckCode, CheckMode, CheckRequest, CheckSubject};
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::encoding::{
    EncodedIdentity, LegacyBlockResidueV0, LegacyChangeLinksV0, LegacyChangeV0, LegacyLandingV0,
    LegacyRedactionResidueV0, encoded_hash,
};
use rdy_model::plan::{PlanAct, PlanKind, PlanOutcome};
use rdy_model::redaction::{AssessmentSupport, ChangeResidue, ImpactKind, RedactionResidue};
use rdy_model::*;
use rdy_store::carried_records as carried;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::codec;
use rdy_store::format::{
    self, FEATURE_REQUIRED_RDY, FeatureClass, REF_STORE_FEATURES, StoreFeature, StoreFeatures,
    baseline_features, registered_features,
};
use rdy_store::ingest::Ingest;
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, ObjectRef, refs_of};
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{TagChoice, TakeError, UploadFacts, UploadTarget};
use rdy_store::{LandRequest, Store, StoreError, StoreReader};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

const TAG: &str = "v1";

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
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

fn writer() -> Provenance {
    prov("agent:w", Role::Writer)
}

fn approver() -> Provenance {
    prov("human:alice", Role::Approver)
}

fn block(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: writer(),
        justification: None,
        message: "m".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// An assessment of `proposal` on `base`, with every list populated, its policy accepted by
/// `policy` under a named rule (or, with none, a NeedsPerson verdict), and a tension dismissed by
/// review `dismissal`.
fn assessment(
    proposal: Hash,
    base: Option<Hash>,
    policy: Option<PolicyRef>,
    dismissal: Hash,
    blocks: Vec<BlockId>,
) -> Assessment {
    let verdict = match &policy {
        Some(_) => PolicyVerdict::Accepts {
            rule: "maintainers-approve".into(),
        },
        None => PolicyVerdict::NeedsPerson {
            role: Some("maintainer".into()),
            reasons: vec!["no authority policy is set".into()],
        },
    };
    let scores = AssessmentScores {
        overall: 90,
        grounded: 91,
        consistent: 92,
        current: 93,
        connected: 94,
        complete: 95,
        governed: 96,
    };
    let issue = AssessmentIssue {
        block: blocks[0].clone(),
        dimension: "grounded".into(),
        code: "untied_content".into(),
        subject: Some("a subject".into()),
        fix: "tie it".into(),
        dismissal: Some(dismissal),
    };
    Assessment {
        proposal,
        base,
        method: 1,
        pack: AssessmentPack {
            name: "rdy-qa".into(),
            version: "1.0.0".into(),
            digest: Hash::of(b"pack digest"),
        },
        config: Hash::of(b"config"),
        policy: Some(AssessedPolicy { policy, verdict }),
        facts: AssessmentFacts {
            changes: 1,
            consequences: 2,
            plan: AssessmentPlanCounts {
                open: 1,
                closed: 2,
                deferred: 3,
                by_kind: vec![(
                    "tension".into(),
                    AssessmentCounts {
                        open: 1,
                        closed: 0,
                        deferred: 0,
                    },
                )],
            },
            supersedes: vec![],
            reaffirms: vec![],
            reversibility: vec![],
            exceptions: vec![AssessmentException {
                kind: AssessmentExceptionKind::OneWay,
                reference: "D-00001".into(),
            }],
        },
        tensions: vec![AssessmentTension {
            id: Hash::of(b"tension id"),
            kind: "edit_collision".into(),
            severity: AssessmentSeverity::Blocking,
            status: AssessmentTensionStatus::Dismissed,
            answer: None,
            dismissal: Some(dismissal),
            summary: "two edits".into(),
            blocks: blocks.clone(),
            vkey: Hash::of(b"tension vkey"),
        }],
        tensions_omitted: 7,
        suspects: vec![],
        suspects_omitted: 5,
        coverage: AssessmentCoverage {
            pages: vec![],
            corpus: AssessmentScoreChange {
                before: scores.clone(),
                after: scores,
            },
        },
        confidence: AssessmentConfidence {
            level: AssessmentLevel::Medium,
            reasons: vec![issue.clone()],
        },
        reach: blocks.clone(),
        card: AssessmentCard {
            hash: Hash::of(b"card"),
            facts: vec!["a fact".into()],
        },
        scope: AssessmentScope { proposal, blocks },
        issues: vec![issue],
    }
}

fn tension_review(blocks: Vec<BlockId>) -> Review {
    Review {
        subject: ReviewSubject::Tension {
            id: Hash::of(b"tension id"),
            vkey: Hash::of(b"tension vkey"),
            kind: "edit_collision".into(),
            severity: "blocking".into(),
            sides: vec!["a".into(), "b".into()],
            blocks,
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: "fine".into(),
        citations: vec![],
        provenance: prov("human:alice", Role::Reviewer),
    }
}

fn check_review(request: Hash, case: Hash, verdict: Verdict, on: &BlockId) -> Review {
    let (reason, citations) = match verdict {
        Verdict::Pass => (String::new(), vec![]),
        _ => (
            format!("{verdict:?} found"),
            vec![Citation {
                block: on.clone(),
                note: "here".into(),
            }],
        ),
    };
    Review {
        subject: ReviewSubject::Check { request },
        case_hash: Some(case),
        verdict,
        reason,
        citations,
        provenance: prov("agent:checker", Role::Reviewer),
    }
}

fn check_request(change: Hash, version: Hash, on: &BlockId) -> CheckRequest {
    CheckRequest {
        check: "consistency".into(),
        subject: CheckSubject {
            blocks: vec![on.clone()],
            change: Some(change),
            version: Some(version),
            code: Some(CheckCode {
                repo: "app".into(),
                from: "a".repeat(40),
                to: "b".repeat(40),
            }),
        },
        scope: "docs".into(),
        case_hash: Hash::of(b"case"),
        mode: CheckMode::Independent,
        reason: Some("a second look".into()),
        requested_by: writer(),
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: None,
        }),
        relay: Some(RelayEvidence {
            quoted: "check it".into(),
            session_ref: "sess-1".into(),
            question: None,
        }),
    }
}

fn answer(item: String, act: AnswerAct, targets: Vec<Hash>) -> Answer {
    Answer {
        item,
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: approver(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

/// A source project that holds every carried shape, built through the store's own writers and
/// carrier APIs (no product path writes one):
///
/// - landing `l1` of change `c1` (the policy block `p` and the claim `b`);
/// - proposal `v2` of change `c2`, its ASSESSMENT (base `l1`, the policy's version, a dismissed
///   tension) and a NeedsPerson ASSESSMENT, then landing `l2` of `v2`: a policy landing that
///   names the first assessment and the policy;
/// - a DISPUTE of `l2`; landing `l3` of `c3`, the correction; its DISPUTE_RESOLUTION;
/// - a CHECK_REQUEST of `c2` and `v2` with its `check/` ref, and a Check REVIEW of each verdict;
/// - a Dispute, a ResolveDispute and an AcceptVerdict answer; tag `v1` at `l3`.
struct Carriers {
    dir: tempfile::TempDir,
    path: PathBuf,
    repo: RepoId,
    b: BlockId,
    policy: PolicyRef,
    c2: Hash,
    v2: Hash,
    l1: Hash,
    l2: Hash,
    l3: Hash,
    tag: Hash,
    dismissal: Hash,
    assessment: Hash,
    needs_person: Hash,
    dispute: Hash,
    resolution: Hash,
    request: Hash,
    pass: Hash,
    concern: Hash,
    fail: Hash,
    dispute_answer: Hash,
    resolve_answer: Hash,
    verdict_answer: Hash,
}

/// The landing of `proposal` by the approver, with `alter` run on its record first.
fn land_altered(s: &mut Store, proposal: Hash, alter: &dyn Fn(&mut LandingRecord)) -> Hash {
    let req = LandRequest {
        proposal,
        accept: None,
        approver: approver(),
        alt: None,
        relay: None,
        evidence: None,
    };
    let prepared = s.prepare_land_altered_for_tests(req, alter).unwrap();
    s.commit_land(prepared, &[]).unwrap().landing
}

fn put(s: &mut Store, kind: u8, v: &(impl serde::Serialize + 'static)) -> Hash {
    s.put(kind, v).unwrap()
}

fn carriers() -> Carriers {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let p = block("p-00001", "The authority policy.");
    let b = block("b-00001", "A claim.");
    s.commit_to_view(
        "work",
        change(vec![
            Op::Create { block: p.clone() },
            Op::Create { block: b.clone() },
        ]),
    )
    .unwrap();
    let v1 = s.propose("work", "first", writer()).unwrap();
    let l1 = s.land(&v1, approver()).unwrap();
    let mut b2 = b.clone();
    b2.body = "A claim, edited.".into();
    let c2 = s
        .commit_to_view(
            "work",
            change(vec![Op::Update {
                base: b.version_hash(),
                block: b2.clone(),
            }]),
        )
        .unwrap();
    let v2 = s.propose("work", "second", writer()).unwrap();
    let policy = PolicyRef {
        block: p.id.clone(),
        version: p.version_hash(),
    };
    let dismissal = put(&mut s, kinds::REVIEW, &tension_review(vec![b.id.clone()]));
    let a = assessment(
        v2,
        Some(l1),
        Some(policy.clone()),
        dismissal,
        vec![b.id.clone()],
    );
    let assessment_hash = put(&mut s, kinds::ASSESSMENT, &a);
    let needs = assessment(v2, None, None, dismissal, vec![b.id.clone()]);
    let needs_person = put(&mut s, kinds::ASSESSMENT, &needs);
    let named = policy.clone();
    let l2 = land_altered(&mut s, v2, &move |l| {
        l.assessment = Some(assessment_hash);
        l.policy = Some(named.clone());
        l.assurance = Assurance::Policy;
    });
    let dispute = put(
        &mut s,
        kinds::DISPUTE,
        &Dispute {
            landing: l2,
            opened_by: approver(),
            reason: "the edit is wrong".into(),
            at_ms: 1_760_000_000_001,
        },
    );
    let mut b3 = b2.clone();
    b3.body = "A claim, corrected.".into();
    s.commit_to_view(
        "work",
        change(vec![Op::Update {
            base: b2.version_hash(),
            block: b3,
        }]),
    )
    .unwrap();
    let v3 = s.propose("work", "third", writer()).unwrap();
    let l3 = s.land(&v3, approver()).unwrap();
    let resolution = put(
        &mut s,
        kinds::DISPUTE_RESOLUTION,
        &DisputeResolution {
            dispute,
            outcome: DisputeOutcome::Upheld,
            by: approver(),
            correction: Some(l3),
        },
    );
    let q = check_request(c2, v2, &b.id);
    let request = put(&mut s, kinds::CHECK_REQUEST, &q);
    s.set_refs(vec![(
        format!("{}{request}", carried::CHECK_REF_PREFIX),
        request.0.to_vec(),
    )])
    .unwrap();
    let [pass, concern, fail] = [Verdict::Pass, Verdict::Concern, Verdict::Fail].map(|v| {
        put(
            &mut s,
            kinds::REVIEW,
            &check_review(request, q.case_hash, v, &b.id),
        )
    });
    let dispute_answer = s
        .put_answer(&answer(
            format!("dispute:{l2}"),
            AnswerAct::Dispute,
            vec![dispute],
        ))
        .unwrap();
    let resolve_answer = s
        .put_answer(&answer(
            format!("resolve:{dispute}"),
            AnswerAct::ResolveDispute,
            vec![resolution],
        ))
        .unwrap();
    let verdict_answer = s
        .put_answer(&answer(
            format!("verdict:{concern}"),
            AnswerAct::AcceptVerdict,
            vec![],
        ))
        .unwrap();
    let tag = s
        .put_tag(&Tag {
            name: TAG.into(),
            landing: l3,
            message: None,
            tagged_by: approver(),
        })
        .unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    drop(s);
    Carriers {
        dir,
        path,
        repo,
        b: b.id,
        policy,
        c2,
        v2,
        l1,
        l2,
        l3,
        tag,
        dismissal,
        assessment: assessment_hash,
        needs_person,
        dispute,
        resolution,
        request,
        pass,
        concern,
        fail,
        dispute_answer,
        resolve_answer,
        verdict_answer,
    }
}

impl Carriers {
    fn open(&self) -> Store {
        Store::open(&self.path).unwrap()
    }

    /// Every carried record: its key.
    fn keys(&self) -> Vec<ObjectKey> {
        vec![
            key(kinds::ASSESSMENT, self.assessment),
            key(kinds::ASSESSMENT, self.needs_person),
            key(kinds::DISPUTE, self.dispute),
            key(kinds::DISPUTE_RESOLUTION, self.resolution),
            key(kinds::CHECK_REQUEST, self.request),
            key(kinds::REVIEW, self.pass),
            key(kinds::REVIEW, self.concern),
            key(kinds::REVIEW, self.fail),
            key(kinds::LANDING, self.l2),
            key(kinds::ANSWER, self.dispute_answer),
            key(kinds::ANSWER, self.resolve_answer),
            key(kinds::ANSWER, self.verdict_answer),
        ]
    }

    /// The records of the tag's closure the holder takes: the answers that target neither a
    /// landing nor a tag stay out.
    fn closure_keys(&self) -> Vec<ObjectKey> {
        self.keys()
            .into_iter()
            .filter(|k| k.kind != kinds::ANSWER && k.hash != self.needs_person)
            .collect()
    }
}

/// The stored frame of `k`: its raw kind and payload.
fn frame_of(r: &StoreReader, k: ObjectKey) -> (u8, Vec<u8>) {
    r.get_kind_raw(k.kind, &k.hash).unwrap()
}

/// The frames of `keys` in `r`, by key.
fn frames(r: &StoreReader, keys: &[ObjectKey]) -> BTreeMap<ObjectKey, (u8, Vec<u8>)> {
    keys.iter().map(|k| (*k, frame_of(r, *k))).collect()
}

/// A load of the closed store at `src` into `to`: every sealed batch ingested at its place, as
/// a dump of this rdy's format carries it, then the store opened and checked.
fn load(src: &Path, to: &Path) -> Store {
    let s = Store::open(src).unwrap();
    let r = s.reader();
    let features = r.store_features().unwrap();
    let mut batches = vec![];
    r.for_each_batch(s.log_end(), |b| {
        batches.push(b);
        Ok(())
    })
    .unwrap();
    drop(r);
    drop(s);
    let mut i = Ingest::create(to).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(&format::DumpStore::Features(features)).unwrap();
    i.finish().unwrap();
    Store::open(to).unwrap()
}

/// The tag's closure as one sync batch file at `path`.
fn export(c: &Carriers, path: &Path) -> Vec<ObjectKey> {
    let s = c.open();
    let r = s.reader();
    let cl = closure(&r, c.tag, &BTreeSet::new()).unwrap();
    let header = StreamHeader::tag_history(
        r.store_features().unwrap(),
        c.repo,
        c.tag,
        cl.prerequisites.clone(),
    );
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    write_batch(
        &mut source,
        &header,
        &cl.records,
        File::create(path).unwrap(),
    )
    .unwrap();
    cl.records
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

/// A new holder of `c`'s repo at `name`, and the take of the tag's closure into it.
fn take(
    c: &Carriers,
    name: &str,
) -> (
    Store,
    Result<rdy_store::take_batch::BatchTaken, TakeError<StreamError>>,
) {
    let file = c.dir.path().join(format!("{name}.rdysync"));
    export(c, &file);
    let mut h = Store::create_holder(&c.dir.path().join(name), c.repo).unwrap();
    let target = UploadTarget {
        tag: TAG.into(),
        landing: c.l3,
    };
    let got = h.take_batches(&[File::open(&file).unwrap()], &target, incoming);
    (h, got)
}

/// Every ref of `s`, by name.
fn refs(s: &Store) -> BTreeMap<String, Vec<u8>> {
    s.reader()
        .refs_with_prefix("")
        .unwrap()
        .into_iter()
        .collect()
}

#[test]
fn ok4_zero_encoding_ids_survive_current_structs() {
    // an older rdy's records: encoding-0 frames of a change, a landing and both residues
    let c = change(vec![Op::Create {
        block: block("b-00009", "Old text."),
    }]);
    let old_change = postcard::to_allocvec(&LegacyChangeV0::from_current(&c).unwrap()).unwrap();
    let l = LandingRecord {
        proposal: h(1),
        prev: None,
        state: h(2),
        approved_by: approver(),
        changes: vec![Hash::of(&old_change)],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: Some(h(1)),
        rebased_from: vec![],
        evidence: None,
        assessment: None,
        policy: None,
        read_identity: None,
    };
    let old_landing = postcard::to_allocvec(&LegacyLandingV0::from_current(&l).unwrap()).unwrap();
    let old_block_residue =
        postcard::to_allocvec(&LegacyRedactionResidueV0::Block(LegacyBlockResidueV0 {
            version: h(3),
            id: id("b-00009"),
            block_type: "claim".into(),
            parent: None,
            order: "V".into(),
            content_hash: h(4),
            frozen: false,
        }))
        .unwrap();
    let mut residue = ChangeResidue::of(&c);
    residue.source_encoding = 0;
    let old_change_residue = postcard::to_allocvec(
        &LegacyRedactionResidueV0::from_current(&RedactionResidue::Change(residue)).unwrap(),
    )
    .unwrap();
    let old = [
        (kinds::CHANGE, old_change),
        (kinds::LANDING, old_landing),
        (kinds::REDACTION_RESIDUE, old_block_residue),
        (kinds::REDACTION_RESIDUE, old_change_residue),
    ];
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("s");
    drop(Store::create(&dir).unwrap());
    carried_fixture::append_raw(&dir, &old, true);
    let mut s = Store::open(&dir).unwrap();
    s.reindex().unwrap();
    for (kind, bytes) in &old {
        let k = key(*kind, Hash::of(bytes));
        let (raw, payload) = s.reader().get_kind_raw(k.kind, &k.hash).unwrap();
        assert_eq!(
            (raw, &payload),
            (*kind, bytes),
            "the frame is unmarked encoding 0"
        );
        let o = codec::split(raw, &payload).unwrap();
        assert_eq!(o.version, 0);
        match *kind {
            kinds::CHANGE => {
                let read: Change = s.reader().get_typed(kinds::CHANGE, &k.hash).unwrap();
                assert_eq!(read, c, "the current struct, its new options absent");
                assert_eq!(read.links.replayed_from, None);
                assert_eq!(read.links.briefing, None);
                assert_eq!(read.id(), k.hash, "its old id");
                assert_ne!(read.id(), c.id(), "a new change would be encoding 1");
                assert!(!read.is_projection());
                assert_eq!(
                    read.read_identity,
                    Some(ChangeReadIdentity::Stored(EncodedIdentity {
                        hash: k.hash,
                        version: 0
                    }))
                );
                let e = codec::encode(kinds::CHANGE, &read).unwrap();
                assert_eq!((e.raw_kind(), &e.payload), (*kind, bytes), "exact bytes");
                assert!(codec::is_canonical(&o, &read));
            }
            kinds::LANDING => {
                let read: LandingRecord = s.reader().get_typed(kinds::LANDING, &k.hash).unwrap();
                assert_eq!(read, l);
                assert_eq!((read.assessment, read.policy.clone()), (None, None));
                assert_eq!(read.id(), k.hash, "its old id");
                assert_ne!(read.id(), l.id());
                let e = codec::encode(kinds::LANDING, &read).unwrap();
                assert_eq!((e.raw_kind(), &e.payload), (*kind, bytes), "exact bytes");
            }
            _ => {
                let read: RedactionResidue = codec::decode_split(&o).unwrap();
                let source = match &read {
                    RedactionResidue::Block(b) => b.source_encoding,
                    RedactionResidue::Change(c) => c.source_encoding,
                };
                assert_eq!(source, 0, "an old residue names an encoding-0 source");
                assert_eq!(codec::residue_body(&read, 0).as_ref(), Some(bytes));
                assert!(codec::is_canonical(&o, &read));
                // the current struct never reads the short old body
                assert!(postcard::from_bytes::<RedactionResidue>(bytes).is_err());
            }
        }
        // refs_of reads the frozen shape too
        refs_of(raw, &payload).unwrap();
    }
    // a new change of the same content is a new object of encoding 1
    let latest = codec::encode(kinds::CHANGE, &c).unwrap();
    assert_eq!(latest.version, 1);
    assert_eq!(latest.raw_kind(), kinds::CHANGE | codec::VERSIONED);
    assert_eq!(latest.hash(), c.id());
    assert_eq!(&latest.payload[..2], &[1, 0]);
}

#[test]
fn ok4_new_shapes_have_fixed_ordinals_and_bytes() {
    let enc = |v: &dyn erased::Ser| v.bytes();
    // the four kind codes; 12 stays unassigned
    assert_eq!(
        (
            kinds::ASSESSMENT,
            kinds::DISPUTE,
            kinds::DISPUTE_RESOLUTION,
            kinds::CHECK_REQUEST
        ),
        (25, 26, 27, 28)
    );
    assert!(!rdy_store::object_refs::is_portable(12));
    // every appended enum index
    assert_eq!(enc(&PlanKind::Tension), [5]);
    assert_eq!(PlanKind::Tension.as_str(), "tension");
    assert_eq!(enc(&PlanAct::Dismiss), [3]);
    assert_eq!(PlanAct::Dismiss.as_str(), "dismiss");
    assert_eq!(enc(&PlanOutcome::Dismissed), [10]);
    assert_eq!(PlanOutcome::Dismissed.as_str(), "dismissed");
    for (act, n, name) in [
        (AnswerAct::Dispute, 14, "dispute"),
        (AnswerAct::ResolveDispute, 15, "resolve_dispute"),
        (AnswerAct::AcceptVerdict, 16, "accept_verdict"),
    ] {
        assert_eq!(enc(&act), [n]);
        assert_eq!(act.as_str(), name);
        assert_eq!(serde_json::to_value(act).unwrap(), serde_json::json!(name));
    }
    assert_eq!(
        enc(&RejectReaffirmProbe::last()),
        [13],
        "RejectReaffirm stays 13"
    );
    let check = ReviewSubject::Check { request: h(9) };
    assert_eq!(enc(&check), [&[3u8][..], &h(9).0].concat());
    for (v, n) in [
        (Verdict::Pass, 4),
        (Verdict::Concern, 5),
        (Verdict::Fail, 6),
    ] {
        assert_eq!(enc(&v), [n]);
    }
    assert_eq!(
        Review {
            subject: check,
            case_hash: None,
            verdict: Verdict::Pass,
            reason: String::new(),
            citations: vec![],
            provenance: writer(),
        }
        .vkey(),
        None
    );
    assert_eq!(enc(&AssessmentSupport::Carried), [1]);
    for (k, n) in [
        (ImpactKind::Assessment, 11),
        (ImpactKind::Dispute, 12),
        (ImpactKind::DisputeResolution, 13),
        (ImpactKind::CheckRequest, 14),
    ] {
        assert_eq!(enc(&k), [n]);
    }
    // preserved indices
    assert_eq!(
        [Assurance::Policy, Assurance::Relayed, Assurance::Local].map(|a| enc(&a)[0]),
        [0, 1, 2]
    );
    assert_eq!(
        [
            EvidenceKind::Elicited,
            EvidenceKind::Card,
            EvidenceKind::Text
        ]
        .map(|e| enc(&e)[0]),
        [0, 1, 2]
    );
    // the populated optional fields: the links' two options, the landing's two fields
    let links = ChangeLinks {
        replayed_from: Some(h(1)),
        briefing: Some(h(2)),
        ..ChangeLinks::default()
    };
    assert_eq!(
        enc(&links),
        [&[0u8, 0, 0, 0, 1][..], &h(1).0, &[1], &h(2).0].concat()
    );
    assert_eq!(LegacyChangeLinksV0::from_current(&links), None);
    let policy = PolicyRef {
        block: id("p-00001"),
        version: h(4),
    };
    let set = (Some(h(3)), Some(policy.clone()));
    assert_eq!(
        enc(&set),
        [&[1u8][..], &h(3).0, &[1, 7], b"p-00001", &h(4).0].concat()
    );
    // a carried record's frame: the marked kind, then the encoding-1 prefix
    let d = Dispute {
        landing: h(5),
        opened_by: approver(),
        reason: "r".into(),
        at_ms: 1,
    };
    let e = codec::encode(kinds::DISPUTE, &d).unwrap();
    assert_eq!(e.raw_kind(), 26 | 0x80);
    assert_eq!(&e.payload[..2], &[1, 0]);
    assert_eq!(&e.payload[2..], &postcard::to_allocvec(&d).unwrap()[..]);
    // an unmarked frame of a carried kind is no encoding: they start at 1
    for kind in 25..=28u8 {
        assert_eq!(codec::first(kind), 1);
        assert_eq!(codec::latest(kind), 1);
        assert!(codec::split(kind, &[0]).is_err(), "kind {kind}");
    }
    for (kind, v) in [
        (kinds::CHANGE, 1),
        (kinds::LANDING, 1),
        (kinds::REDACTION_RESIDUE, 1),
        (kinds::BLOCK, 0),
        (kinds::REVIEW, 0),
        (kinds::ANSWER, 0),
        (kinds::PLAN, 0),
    ] {
        assert_eq!(codec::latest(kind), v, "kind {kind}");
    }
}

/// `RejectReaffirm`, the base's last act, as a probe of the appended indices.
struct RejectReaffirmProbe;

impl RejectReaffirmProbe {
    fn last() -> AnswerAct {
        AnswerAct::RejectReaffirm
    }
}

/// A postcard encoder over a trait object, for the ordinal table.
mod erased {
    pub trait Ser {
        fn bytes(&self) -> Vec<u8>;
    }
    impl<T: serde::Serialize> Ser for T {
        fn bytes(&self) -> Vec<u8> {
            postcard::to_allocvec(self).unwrap()
        }
    }
}

#[test]
fn ok4_all_carriers_keep_every_field() {
    let c = carriers();
    let s = c.open();
    s.reader().check_objects().unwrap();
    let keys = c.keys();
    let want = frames(&s.reader(), &keys);
    // every field, read back
    let r = s.reader();
    let a: Assessment = r.get_typed(kinds::ASSESSMENT, &c.assessment).unwrap();
    assert_eq!(a.tensions_omitted, 7);
    assert_eq!(a.suspects_omitted, 5);
    assert_eq!(a.base, Some(c.l1));
    assert_eq!(a.policy.as_ref().unwrap().policy, Some(c.policy.clone()));
    let n: Assessment = r.get_typed(kinds::ASSESSMENT, &c.needs_person).unwrap();
    assert!(matches!(
        n.policy.as_ref().unwrap(),
        AssessedPolicy {
            policy: None,
            verdict: PolicyVerdict::NeedsPerson { .. }
        }
    ));
    let l: LandingRecord = r.get_typed(kinds::LANDING, &c.l2).unwrap();
    assert_eq!(l.assessment, Some(c.assessment));
    assert_eq!(l.policy, Some(c.policy.clone()));
    assert_eq!(l.assurance, Assurance::Policy);
    let q: CheckRequest = r.get_typed(kinds::CHECK_REQUEST, &c.request).unwrap();
    assert_eq!(q.subject.change, Some(c.c2));
    assert_eq!(q.subject.version, Some(c.v2));
    let res: DisputeResolution = r
        .get_typed(kinds::DISPUTE_RESOLUTION, &c.resolution)
        .unwrap();
    assert_eq!(res.correction, Some(c.l3));
    drop(r);
    drop(s);
    // dump/load: the same frames, and a loaded project that checks
    let loaded = load(&c.path, &c.dir.path().join("loaded"));
    assert_eq!(
        frames(&loaded.reader(), &keys),
        want,
        "the load keeps every byte"
    );
    loaded.reader().check_objects().unwrap();
    // reindex: the same frames and refs
    let mut loaded = loaded;
    let before = refs(&loaded);
    loaded.reindex().unwrap();
    assert_eq!(frames(&loaded.reader(), &keys), want);
    assert_eq!(refs(&loaded), before);
    loaded.reader().check_objects().unwrap();
    drop(loaded);
    // stream/check/take: every record the tag's closure holds, byte for byte, in a holder
    // that checks, with no carried ref
    let file = c.dir.path().join("check.rdysync");
    export(&c, &file);
    rdy_store::stream_source::CheckedStream::open(File::open(&file).unwrap()).unwrap();
    let (h, got) = take(&c, "holder");
    got.unwrap();
    let closure_keys = c.closure_keys();
    let want_closure: BTreeMap<_, _> = want
        .iter()
        .filter(|(k, _)| closure_keys.contains(k))
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    assert_eq!(frames(&h.reader(), &closure_keys), want_closure);
    h.reader().check_holder().unwrap();
    let mut h = h;
    h.reindex().unwrap();
    assert_eq!(frames(&h.reader(), &closure_keys), want_closure);
    h.reader().check_holder().unwrap();
}

#[test]
fn ok4_closure_takes_reverse_dispute_attachments() {
    let c = carriers();
    let s = c.open();
    let r = s.reader();
    let records: BTreeSet<ObjectKey> = closure(&r, c.tag, &BTreeSet::new())
        .unwrap()
        .records
        .into_iter()
        .collect();
    // from the tag: the landings' history (l3, l2, l1), l2's assessment, the dispute of l2, its
    // resolution, the correction l3, the check request of c2 and v2 and its three reviews
    for k in [
        key(kinds::TAG, c.tag),
        key(kinds::LANDING, c.l3),
        key(kinds::LANDING, c.l2),
        key(kinds::LANDING, c.l1),
        key(kinds::ASSESSMENT, c.assessment),
        key(kinds::REVIEW, c.dismissal),
        key(kinds::DISPUTE, c.dispute),
        key(kinds::DISPUTE_RESOLUTION, c.resolution),
        key(kinds::CHECK_REQUEST, c.request),
        key(kinds::REVIEW, c.pass),
        key(kinds::REVIEW, c.concern),
        key(kinds::REVIEW, c.fail),
        key(kinds::CHANGE, c.c2),
        key(kinds::PROPOSAL, c.v2),
    ] {
        assert!(records.contains(&k), "{k} is in the closure");
    }
    // the answers that target no landing or tag, and an unlanded assessment, are not attached
    for k in [
        key(kinds::ANSWER, c.dispute_answer),
        key(kinds::ANSWER, c.resolve_answer),
        key(kinds::ANSWER, c.verdict_answer),
        key(kinds::ASSESSMENT, c.needs_person),
    ] {
        assert!(!records.contains(&k), "{k} is not attached");
    }
    // the order puts each record after what it needs
    let order = closure(&r, c.tag, &BTreeSet::new()).unwrap().records;
    let at = |k: ObjectKey| order.iter().position(|x| *x == k).unwrap();
    assert!(at(key(kinds::LANDING, c.l2)) < at(key(kinds::DISPUTE, c.dispute)));
    assert!(at(key(kinds::DISPUTE, c.dispute)) < at(key(kinds::DISPUTE_RESOLUTION, c.resolution)));
    assert!(at(key(kinds::LANDING, c.l3)) < at(key(kinds::DISPUTE_RESOLUTION, c.resolution)));
    assert!(at(key(kinds::ASSESSMENT, c.assessment)) < at(key(kinds::LANDING, c.l2)));
    assert!(at(key(kinds::CHECK_REQUEST, c.request)) < at(key(kinds::REVIEW, c.concern)));
    drop(r);
    drop(s);
    // the holder keeps the records and their logical keys, and its refs stay closed
    let (h, got) = take(&c, "holder");
    got.unwrap();
    for k in &records {
        assert!(h.reader().has_kind(k.kind, &k.hash).unwrap(), "{k}");
    }
    for name in refs(&h).keys() {
        assert!(
            name.starts_with("tag/") || name.starts_with("meta/"),
            "a holder ref of a closed family: {name}"
        );
    }
    // a check ref is refused in a holder
    let mut h = h;
    let e = h
        .set_refs(vec![(format!("check/{}", c.request), c.request.0.to_vec())])
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        format!("a holder store cannot write ref check/{}", c.request)
    );
}

#[test]
fn ok4_reference_identities_are_not_object_edges() {
    let c = carriers();
    let s = c.open();
    let r = s.reader();
    let edges = |k: ObjectKey| -> BTreeSet<ObjectRef> {
        let (raw, payload) = frame_of(&r, k);
        refs_of(raw, &payload).unwrap().into_iter().collect()
    };
    let one = |kind: u8, hash: Hash| ObjectRef {
        kind: Some(kind),
        hash,
    };
    // the assessment: its proposal (once), its base, its policy block version and its
    // dismissal reviews; pack, config, card, tension ids and vkeys are identities
    assert_eq!(
        edges(key(kinds::ASSESSMENT, c.assessment)),
        BTreeSet::from([
            one(kinds::BLOCK, c.policy.version),
            one(kinds::LANDING, c.l1),
            one(kinds::PROPOSAL, c.v2),
            one(kinds::REVIEW, c.dismissal),
        ])
    );
    let landing = edges(key(kinds::LANDING, c.l2));
    assert!(landing.contains(&one(kinds::ASSESSMENT, c.assessment)));
    assert!(landing.contains(&one(kinds::BLOCK, c.policy.version)));
    assert_eq!(
        edges(key(kinds::DISPUTE_RESOLUTION, c.resolution)),
        BTreeSet::from([one(kinds::LANDING, c.l3), one(kinds::DISPUTE, c.dispute)])
    );
    assert_eq!(
        edges(key(kinds::CHECK_REQUEST, c.request)),
        BTreeSet::from([one(kinds::CHANGE, c.c2), one(kinds::PROPOSAL, c.v2)])
    );
    assert_eq!(
        edges(key(kinds::REVIEW, c.concern)),
        BTreeSet::from([one(kinds::CHECK_REQUEST, c.request)])
    );
    assert_eq!(
        edges(key(kinds::ANSWER, c.verdict_answer)),
        BTreeSet::from([one(kinds::REVIEW, c.concern)])
    );
    // no identity hash names an object the store must hold
    let identities = [
        Hash::of(b"pack digest"),
        Hash::of(b"config"),
        Hash::of(b"card"),
        Hash::of(b"tension id"),
        Hash::of(b"tension vkey"),
        Hash::of(b"case"),
    ];
    for k in c.keys() {
        for e in edges(k) {
            assert!(!identities.contains(&e.hash), "{k} names {}", e.hash);
        }
    }
    // a change's briefing is a token identity; its replay source is a reference, not a dep
    let mut replay = change(vec![]);
    replay.links.briefing = Some(Hash::of(b"briefing"));
    replay.links.replayed_from = Some(c.c2);
    let (raw, payload) = codec::item(kinds::CHANGE, &replay).unwrap();
    assert_eq!(
        refs_of(raw, &payload).unwrap(),
        vec![one(kinds::CHANGE, c.c2)]
    );
    assert!(replay.deps.is_empty());
    drop(r);
    drop(s);
    // and a whole-store check reads none of them as missing
    c.open().reader().check_objects().unwrap();
}

/// Which checks reach a corruption: a holder's take of the newest tag and the source's fsck,
/// or the source's fsck alone. A take carries only a tag's closure (`outside the tag closure`
/// otherwise): no `check/` ref and no answer that targets neither a landing nor a tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reach {
    Take,
    Fsck,
}

/// What one corruption does to the carriers' source before the take: the refusal's reason and
/// the checks that reach it.
type Corrupt = (&'static str, Reach, Box<dyn Fn(&Carriers, &mut Store)>);

fn corruptions() -> Vec<Corrupt> {
    fn review(s: &mut Store, c: &Carriers, r: Review) {
        let _ = c;
        s.put(kinds::REVIEW, &r).unwrap();
    }
    vec![
        (
            carried::CHECK_VERDICT,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                let mut r = check_review(c.request, Hash::of(b"case"), Verdict::Pass, &c.b);
                r.verdict = Verdict::Tension;
                review(s, c, r)
            }),
        ),
        (
            carried::CHECK_CASE,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                review(
                    s,
                    c,
                    check_review(c.request, Hash::of(b"another case"), Verdict::Fail, &c.b),
                )
            }),
        ),
        (
            carried::CHECK_CITATIONS,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                let mut r = check_review(c.request, Hash::of(b"case"), Verdict::Concern, &c.b);
                r.citations.clear();
                review(s, c, r)
            }),
        ),
        (
            carried::CHECK_REASON,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                let mut r = check_review(c.request, Hash::of(b"case"), Verdict::Fail, &c.b);
                r.reason = "  ".into();
                review(s, c, r)
            }),
        ),
        (
            "invalid dispute reason",
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                s.put(
                    kinds::DISPUTE,
                    &Dispute {
                        landing: c.l3,
                        opened_by: approver(),
                        reason: String::new(),
                        at_ms: 2,
                    },
                )
                .unwrap();
            }),
        ),
        (
            carried::DISPUTE_ITEM,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                // targets the landing, so the closure attaches it
                s.put(
                    kinds::ANSWER,
                    &answer("dispute:NOTHEX".into(), AnswerAct::Dispute, vec![c.l3]),
                )
                .unwrap();
            }),
        ),
        (
            carried::WRONG_KIND,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                s.put(
                    kinds::ANSWER,
                    &answer(format!("dispute:{}", c.l3), AnswerAct::Dispute, vec![c.l3]),
                )
                .unwrap();
            }),
        ),
        (
            carried::VERDICT_TARGETS,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                s.put(
                    kinds::ANSWER,
                    &answer(
                        format!("verdict:{}", c.concern),
                        AnswerAct::AcceptVerdict,
                        vec![c.l3],
                    ),
                )
                .unwrap();
            }),
        ),
        (
            carried::POLICY_FIELDS,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                // a policy landing with a relay: a new landing on canon, tagged instead
                let mut b = block("b-00002", "Relayed policy.");
                b.order = "W".into();
                s.commit_to_view("work", change(vec![Op::Create { block: b }]))
                    .unwrap();
                let v = s.propose("work", "relayed", writer()).unwrap();
                let named = c.policy.clone();
                let bad = land_altered(s, v, &move |l| {
                    l.policy = Some(named.clone());
                    l.assurance = Assurance::Policy;
                    l.relayed_by = Some(ActorRun {
                        actor: "agent:relay".into(),
                        run: None,
                    });
                    l.relay_evidence = Some(RelayEvidence {
                        quoted: "yes".into(),
                        session_ref: "s".into(),
                        question: None,
                    });
                });
                retag(s, bad);
            }),
        ),
        (
            carried::POLICY_BLOCK,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                let mut b = block("b-00003", "Wrong policy.");
                b.order = "X".into();
                s.commit_to_view("work", change(vec![Op::Create { block: b }]))
                    .unwrap();
                let v = s.propose("work", "wrong policy", writer()).unwrap();
                let wrong = PolicyRef {
                    block: c.b.clone(),
                    version: c.policy.version,
                };
                let bad = land_altered(s, v, &move |l| {
                    l.policy = Some(wrong.clone());
                    l.assurance = Assurance::Policy;
                });
                retag(s, bad);
            }),
        ),
        (
            carried::ASSESSMENT_PROPOSAL,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                let mut b = block("b-00004", "Wrong assessment.");
                b.order = "Y".into();
                s.commit_to_view("work", change(vec![Op::Create { block: b }]))
                    .unwrap();
                let v = s.propose("work", "wrong assessment", writer()).unwrap();
                let a = c.assessment;
                let bad = land_altered(s, v, &move |l| l.assessment = Some(a));
                retag(s, bad);
            }),
        ),
        (
            carried::ASSESSMENT_BASE,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                let mut b = block("b-00005", "Wrong base.");
                b.order = "Z".into();
                s.commit_to_view("work", change(vec![Op::Create { block: b }]))
                    .unwrap();
                let v = s.propose("work", "wrong base", writer()).unwrap();
                let a = assessment(
                    v,
                    Some(Hash::of(b"no landing")),
                    None,
                    c.dismissal,
                    vec![c.b.clone()],
                );
                // a base that is a landing, but not an ancestor: a landing of another chain
                let mut a = a;
                a.base = Some(c.dispute_landing_outside(s));
                let ah = s.put(kinds::ASSESSMENT, &a).unwrap();
                let bad = land_altered(s, v, &move |l| l.assessment = Some(ah));
                retag(s, bad);
            }),
        ),
        (
            carried::DISPUTE_TARGET,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                // two targets: the landing, so the closure attaches it, and its dispute
                s.put(
                    kinds::ANSWER,
                    &answer(
                        format!("dispute:{}", c.l3),
                        AnswerAct::Dispute,
                        vec![c.l3, c.dispute],
                    ),
                )
                .unwrap();
            }),
        ),
        (
            carried::DISPUTE_TARGET,
            Reach::Fsck,
            Box::new(|c: &Carriers, s: &mut Store| {
                // its one DISPUTE names another landing (l2) than its item (l3); the answer
                // writer refuses it before a byte is written
                let bad = answer(
                    format!("dispute:{}", c.l3),
                    AnswerAct::Dispute,
                    vec![c.dispute],
                );
                let end = s.log_end();
                let e = s.put_answer(&bad).unwrap_err();
                assert!(e.to_string().contains(carried::DISPUTE_TARGET), "{e}");
                assert_eq!(s.log_end(), end);
                s.put(kinds::ANSWER, &bad).unwrap();
            }),
        ),
        (
            carried::PLAN_ANSWER,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                // an answer-only plan of item h(1); the assessment's tension names item h(2)
                let plan = rdy_model::plan::Plan {
                    stable: None,
                    prev: None,
                    view: "side".into(),
                    proposal: None,
                    answers: vec![rdy_model::plan::PlanAnswer {
                        item: h(1),
                        act: PlanAct::Confirm,
                        reason: Some("it holds".into()),
                        subject: None,
                        cause: h(3),
                        by: writer(),
                    }],
                    heads: vec![],
                    provenance: writer(),
                    seal: None,
                };
                let plan = s.put_plan(&plan).unwrap();
                let mut b = block("b-00006", "Wrong plan answer.");
                b.order = "a".into();
                s.commit_to_view("work", change(vec![Op::Create { block: b }]))
                    .unwrap();
                let v = s.propose("work", "wrong plan answer", writer()).unwrap();
                let mut a = assessment(v, Some(c.l3), None, c.dismissal, vec![c.b.clone()]);
                a.tensions[0].status = AssessmentTensionStatus::Open;
                a.tensions[0].dismissal = None;
                a.tensions[0].answer = Some(AssessmentPlanAnswerRef { plan, item: h(2) });
                let ah = s.put(kinds::ASSESSMENT, &a).unwrap();
                let bad = land_altered(s, v, &move |l| l.assessment = Some(ah));
                retag(s, bad);
            }),
        ),
        (
            carried::DISMISS,
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                // a seal of a reaffirm item, and a Dismiss answer of that item
                let item = SealedItemSpec::new(&c.b, PlanKind::Reaffirm, PlanOutcome::Reaffirmed);
                let dismiss = rdy_model::plan::PlanAnswer {
                    item: item.id(),
                    act: PlanAct::Dismiss,
                    reason: Some("not real".into()),
                    subject: None,
                    cause: h(3),
                    by: writer(),
                };
                let bad = sealed_side(s, "b-00007", "b", item, vec![dismiss]);
                retag(s, bad);
            }),
        ),
        (
            // the plan record rules judge an outcome its kind does not allow, a tension's too:
            // the carrier adds no second rule (`invalid seal`)
            "invalid seal",
            Reach::Take,
            Box::new(|c: &Carriers, s: &mut Store| {
                let item = SealedItemSpec::new(&c.b, PlanKind::Tension, PlanOutcome::Deferred);
                let bad = sealed_side(s, "b-00008", "c", item, vec![]);
                retag(s, bad);
            }),
        ),
        (
            carried::CHECK_REF,
            Reach::Fsck,
            Box::new(|c: &Carriers, s: &mut Store| {
                // the request's ref names another hash
                s.set_refs(vec![(
                    format!("{}{}", carried::CHECK_REF_PREFIX, c.request),
                    c.pass.0.to_vec(),
                )])
                .unwrap();
            }),
        ),
    ]
}

/// A sealed item of `kind` and `outcome` on block `b`, caused by `D-00002`.
struct SealedItemSpec(rdy_model::plan::SealedItem);

impl SealedItemSpec {
    fn new(b: &BlockId, kind: PlanKind, outcome: PlanOutcome) -> SealedItemSpec {
        let cause = id("D-00002");
        SealedItemSpec(rdy_model::plan::SealedItem {
            id: rdy_model::plan::item_hash(&cause, b.as_str(), kind),
            kind,
            subject: b.to_string(),
            cause,
            edge: "reflects".into(),
            outcome,
            answer: None,
        })
    }

    fn id(&self) -> Hash {
        self.0.id
    }
}

/// A landing on canon of a new block `block_id` (sibling order `order`) and its unbound seal of
/// `item` with `answers`, in the landing's batch, as the landing composition writes a seal.
/// Returns the landing.
fn sealed_side(
    s: &mut Store,
    block_id: &str,
    order: &str,
    item: SealedItemSpec,
    answers: Vec<rdy_model::plan::PlanAnswer>,
) -> Hash {
    let mut b = block(block_id, "A sealed side.");
    b.order = order.into();
    s.commit_to_view("side", change(vec![Op::Create { block: b }]))
        .unwrap();
    let p = s.propose("side", "side", writer()).unwrap();
    s.set_held(true).unwrap();
    let l = s.land(&p, approver()).unwrap();
    let v = s.landing(&l).unwrap().proposal;
    let plan = rdy_model::plan::Plan {
        stable: None,
        prev: None,
        view: "side".into(),
        proposal: Some(v),
        answers,
        heads: vec![],
        provenance: approver(),
        seal: Some(rdy_model::plan::PlanSeal {
            landing: l,
            version: v,
            decisions: vec![item.0.cause.clone()],
            items: vec![item.0],
        }),
    };
    let frames = vec![
        codec::item_new(kinds::PLAN, &plan),
        (
            kinds::REF_UPDATE,
            postcard::to_allocvec(&rdy_store::kinds::RefUpdate {
                name: rdy_store::plan::plan_ref("side"),
                value: plan.id().0.to_vec(),
                at: rdy_store::segment::Location {
                    segment: 0,
                    offset: 0,
                },
            })
            .unwrap(),
        ),
    ];
    s.write_guarded_for_tests(frames).unwrap();
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    l
}

impl Carriers {
    /// A landing outside canon's chain: a holder-style orphan landing record the store keeps
    /// as a carrier.
    fn dispute_landing_outside(&self, s: &mut Store) -> Hash {
        let r = s.reader();
        let l: LandingRecord = r.get_typed(kinds::LANDING, &self.l1).unwrap();
        drop(r);
        let orphan = LandingRecord {
            prev: None,
            approved_by: prov("human:other", Role::Approver),
            read_identity: None,
            ..l
        };
        s.put(kinds::LANDING, &orphan).unwrap()
    }
}

/// Moves tag `v1`'s history to `landing` with a new tag of another name, so the take of `v1`'s
/// target is unchanged: the corruption is reached through the new tag's landing. Writes the new
/// tag only; the take uses the original `v1`.
fn retag(s: &mut Store, landing: Hash) {
    s.put_tag(&Tag {
        name: "v2".into(),
        landing,
        message: None,
        tagged_by: approver(),
    })
    .unwrap();
}

#[test]
fn ok4_bad_carriers_fail_before_take_seal() {
    for (reason, reach, corrupt) in corruptions() {
        let c = carriers();
        let mut s = c.open();
        corrupt(&c, &mut s);
        if reach == Reach::Fsck {
            drop(s);
            let e = c.open().reader().check_objects().unwrap_err();
            assert!(e.to_string().contains(reason), "{reason}: {e}");
            continue;
        }
        // the corruption is reached from the newest tag: v2 when one was made, else v1
        let v2 = s.reader().tags().unwrap().into_iter().find(|t| t.0 == "v2");
        let (tag_name, tag) = match &v2 {
            Some(t) => ("v2", t.1),
            None => (TAG, c.tag),
        };
        let landing: Tag = s.reader().get_typed(kinds::TAG, &tag).unwrap();
        let landing = landing.landing;
        let r = s.reader();
        let cl = closure(&r, tag, &BTreeSet::new()).unwrap();
        let header =
            StreamHeader::tag_history(r.store_features().unwrap(), c.repo, tag, cl.prerequisites);
        let file = c.dir.path().join("bad.rdysync");
        let mut source = StoreSource {
            reader: &r,
            end: END_OF_LOG,
        };
        write_batch(
            &mut source,
            &header,
            &cl.records,
            File::create(&file).unwrap(),
        )
        .unwrap();
        drop(r);
        drop(s);
        let mut h = Store::create_holder(&c.dir.path().join("h"), c.repo).unwrap();
        let end = h.log_end();
        let before = refs(&h);
        let segments = carried_fixture::log_bytes(&c.dir.path().join("h"));
        let target = UploadTarget {
            tag: tag_name.into(),
            landing,
        };
        let e = match h.take_batches(&[File::open(&file).unwrap()], &target, incoming) {
            Err(e) => e,
            Ok(t) => panic!("{reason}: the take passed: {t:?}"),
        };
        let text = match e {
            TakeError::Stream(e) => e.to_string(),
            other => format!("{other:?}"),
        };
        assert!(text.contains(reason), "{reason}: {text}");
        assert_eq!(h.log_end(), end, "{reason}: nothing appended");
        assert_eq!(refs(&h), before, "{reason}: refs unchanged");
        assert_eq!(
            carried_fixture::log_bytes(&c.dir.path().join("h")),
            segments,
            "{reason}: destination bytes unchanged"
        );
        // the same rule refuses the source's fsck
        let e = c.open().reader().check_objects().unwrap_err();
        assert!(e.to_string().contains(reason), "{reason}: {e}");
    }
}

#[test]
fn ok4_accept_verdict_answer_is_carried() {
    let c = carriers();
    let mut s = c.open();
    let fail_answer = s
        .put_answer(&answer(
            format!("verdict:{}", c.fail),
            AnswerAct::AcceptVerdict,
            vec![],
        ))
        .unwrap();
    drop(s);
    for h_ in [c.verdict_answer, fail_answer] {
        let s = c.open();
        let a: Answer = s.reader().get_typed(kinds::ANSWER, &h_).unwrap();
        assert_eq!(a.act, AnswerAct::AcceptVerdict);
        assert_eq!(postcard::to_allocvec(&a.act).unwrap(), [16]);
        assert_eq!(
            serde_json::to_value(a.act).unwrap(),
            serde_json::json!("accept_verdict")
        );
        assert!(a.targets.is_empty());
        // its ref is the existing `answer/<item>`
        assert_eq!(
            s.reader().answer_of(&a.item).unwrap().map(|(h, _)| h),
            Some(h_)
        );
    }
    let keys = [
        key(kinds::ANSWER, c.verdict_answer),
        key(kinds::ANSWER, fail_answer),
    ];
    let want = frames(&c.open().reader(), &keys);
    c.open().reader().check_objects().unwrap();
    // dump/load, reindex and fsck keep both
    let mut loaded = load(&c.path, &c.dir.path().join("loaded"));
    assert_eq!(frames(&loaded.reader(), &keys), want);
    loaded.reindex().unwrap();
    assert_eq!(frames(&loaded.reader(), &keys), want);
    loaded.reader().check_objects().unwrap();
    drop(loaded);
    // a stream of the tag's closure carries the reviews, not the answers (the landing closure
    // attaches none), and its take and check pass
    let (h, got) = take(&c, "holder");
    got.unwrap();
    assert!(h.reader().has_kind(kinds::REVIEW, &c.concern).unwrap());
    assert!(
        !h.reader()
            .has_kind(kinds::ANSWER, &c.verdict_answer)
            .unwrap()
    );
    h.reader().check_holder().unwrap();
    // the refusals, each before a byte is written
    let mut s = c.open();
    let end = s.log_end();
    let cases: [(String, Vec<Hash>, &str); 5] = [
        (
            format!("verdict:{}", c.pass),
            vec![],
            carried::VERDICT_REVIEW,
        ),
        (
            format!("verdict:{}", c.dismissal),
            vec![],
            carried::VERDICT_REVIEW,
        ),
        (
            format!("verdict:{}", Hash::of(b"missing")),
            vec![],
            carried::VERDICT_REVIEW,
        ),
        (
            format!("verdict:{}", c.concern.to_string().to_uppercase()),
            vec![],
            carried::VERDICT_ITEM,
        ),
        (
            format!("verdict:{}", c.concern),
            vec![c.l3],
            carried::VERDICT_TARGETS,
        ),
    ];
    for (item, targets, reason) in cases {
        let e = s
            .put_answer(&answer(item.clone(), AnswerAct::AcceptVerdict, targets))
            .unwrap_err();
        match e {
            StoreError::Decode(m) => {
                assert!(m.ends_with(&format!("(kind 10): {reason}")), "{item}: {m}")
            }
            other => panic!("{item}: {other:?}"),
        }
        assert_eq!(s.log_end(), end, "{item}: nothing written");
    }
}

/// The features of the log's `meta/store_features` ref.
fn log_manifest(s: &Store) -> StoreFeatures {
    let v = s.reader().get_ref(REF_STORE_FEATURES).unwrap().unwrap();
    StoreFeatures::parse(&v).unwrap()
}

fn names(f: &StoreFeatures) -> Vec<&str> {
    f.features.iter().map(|f| f.name.as_str()).collect()
}

/// The refs the last sealed batch of `s` wrote.
fn last_batch_refs(s: &Store) -> Vec<String> {
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
        .filter(|f| f.kind() == kinds::REF_UPDATE)
        .filter_map(|f| {
            f.present_payload()
                .and_then(|p| postcard::from_bytes::<kinds::RefUpdate>(p).ok())
        })
        .map(|u| u.name)
        .collect()
}

/// The release that carries the registered features is the one whose rdy first reads them: the
/// workspace version equals the version every registered feature requires. It fails until the
/// release's version commit sets the workspace to that version.
#[test]
fn ok4_merge_version_equals_feature_required_rdy() {
    assert_eq!(env!("CARGO_PKG_VERSION"), FEATURE_REQUIRED_RDY);
}

#[test]
fn ok4_features_activate_in_their_first_use_batch() {
    // every registered feature is incompatible and needs the release that carries them
    let registered = registered_features();
    assert_eq!(
        registered
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        [
            "assessment",
            "check_request",
            "content_hash_v2",
            "dispute",
            "object_encoding",
            "plan_tension",
            "view_replay"
        ]
    );
    assert_eq!(FEATURE_REQUIRED_RDY, "0.26.0");
    for f in &registered {
        assert_eq!(f.class, FeatureClass::Incompatible, "{}", f.name);
        assert_eq!(f.required_rdy, FEATURE_REQUIRED_RDY, "{}", f.name);
    }
    // a new store's manifest is exactly the baseline
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("new")).unwrap();
    assert_eq!(
        names(&log_manifest(&s)),
        ["content_hash_v2", "object_encoding"]
    );
    assert_eq!(baseline_features(), log_manifest(&s));
    drop(s);
    // each row's first record adds its feature in the same sealed batch, and only that one
    let c = carriers();
    let rows: Vec<(&str, ObjectKey)> = vec![
        ("assessment", key(kinds::ASSESSMENT, c.assessment)),
        ("dispute", key(kinds::DISPUTE, c.dispute)),
        ("dispute", key(kinds::DISPUTE_RESOLUTION, c.resolution)),
        ("dispute", key(kinds::ANSWER, c.dispute_answer)),
        ("dispute", key(kinds::ANSWER, c.resolve_answer)),
        ("check_request", key(kinds::CHECK_REQUEST, c.request)),
        ("check_request", key(kinds::REVIEW, c.pass)),
        ("check_request", key(kinds::ANSWER, c.verdict_answer)),
        ("assessment", key(kinds::LANDING, c.l2)),
    ];
    let src = c.open();
    for (i, (feature, k)) in rows.iter().enumerate() {
        let frame = frame_of(&src.reader(), *k);
        let want: Vec<String> = format::required_features(frame.0, &frame.1)
            .into_iter()
            .map(|f| f.name)
            .filter(|n| n != "object_encoding")
            .collect();
        assert_eq!(want, [*feature], "{k}");
        let dir = d.path().join(format!("row-{i}"));
        let mut s = Store::create(&dir).unwrap();
        s.write(vec![frame]).unwrap();
        let manifest = log_manifest(&s);
        let mut got = names(&manifest);
        got.retain(|n| !["content_hash_v2", "object_encoding"].contains(n));
        assert_eq!(got, [*feature], "{k}: only its feature");
        assert!(
            last_batch_refs(&s).contains(&REF_STORE_FEATURES.to_string()),
            "{k}: in the same sealed batch"
        );
        assert_eq!(
            format::read_format_file(&dir).unwrap(),
            Some(log_manifest(&s))
        );
    }
    drop(src);
    // a replayed change uses view_replay
    let mut replay = change(vec![]);
    replay.links.replayed_from = Some(h(1));
    let (raw, payload) = codec::item(kinds::CHANGE, &replay).unwrap();
    let got: Vec<String> = format::required_features(raw, &payload)
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(got, ["object_encoding", "view_replay"]);
    // a tension plan uses plan_tension: a sealed tension item, a dismissed outcome or a dismiss
    let mut plan = rdy_model::plan::Plan {
        stable: None,
        prev: None,
        view: "work".into(),
        proposal: None,
        answers: vec![rdy_model::plan::PlanAnswer {
            item: h(1),
            act: PlanAct::Dismiss,
            reason: Some("settled".into()),
            subject: None,
            cause: h(2),
            by: writer(),
        }],
        heads: vec![],
        provenance: writer(),
        seal: None,
    };
    let names_of = |p: &rdy_model::plan::Plan| -> Vec<String> {
        let (raw, payload) = codec::item_new(kinds::PLAN, p);
        format::required_features(raw, &payload)
            .into_iter()
            .map(|f| f.name)
            .collect()
    };
    assert_eq!(names_of(&plan), ["plan_tension"]);
    plan.answers[0].act = PlanAct::Confirm;
    assert!(names_of(&plan).is_empty());
    // an encoding-1 frame uses object_encoding; an encoding-0 one none
    let (raw, payload) = codec::item(kinds::CHANGE, &change(vec![])).unwrap();
    let got: Vec<String> = format::required_features(raw, &payload)
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(got, ["object_encoding"]);
    let old =
        postcard::to_allocvec(&LegacyChangeV0::from_current(&change(vec![])).unwrap()).unwrap();
    assert!(format::required_features(kinds::CHANGE, &old).is_empty());
    // a refused batch adds none: the first use and its FORMAT are put back
    let dir = d.path().join("refused");
    let mut s = Store::create(&dir).unwrap();
    let e = s.put_answer(&answer(
        format!("verdict:{}", Hash::of(b"missing")),
        AnswerAct::AcceptVerdict,
        vec![],
    ));
    assert!(e.is_err());
    assert_eq!(log_manifest(&s), baseline_features());
    assert_eq!(
        format::read_format_file(&dir).unwrap(),
        Some(baseline_features())
    );
    // a legacy load's final batch writes the baseline; a dump-4 load keeps its manifest
    let src = d.path().join("legacy-src");
    drop(Store::create(&src).unwrap());
    let s = Store::open(&src).unwrap();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    drop(s);
    let kept = StoreFeatures::of(vec![StoreFeature {
        name: "object_encoding".into(),
        class: FeatureClass::Incompatible,
        required_rdy: FEATURE_REQUIRED_RDY.into(),
    }]);
    let to = d.path().join("dump4");
    let mut i = Ingest::create(&to).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(&format::DumpStore::Features(baseline_features()))
        .unwrap();
    i.finish().unwrap();
    let loaded = Store::open(&to).unwrap();
    assert_eq!(log_manifest(&loaded), baseline_features());
    let _ = kept;
    drop(loaded);
    // a log whose records use a feature its manifest does not name: a raw batch past the
    // store's writers holds a plan with a Dismiss answer (plan_tension)
    let src = d.path().join("under-src");
    drop(Store::create(&src).unwrap());
    let mut plan = plan.clone();
    plan.answers[0].act = PlanAct::Dismiss;
    {
        let (mut log, q) = rdy_store::segment::SegmentLog::open_marked(
            &src.join("segments"),
            64 * 1024 * 1024,
            None,
        )
        .unwrap();
        assert!(q.is_empty());
        log.append_batch(&[codec::item_new(kinds::PLAN, &plan)])
            .unwrap();
        log.sync().unwrap();
    }
    let s = Store::open(&src).unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    drop(s);
    let ingest = |name: &str| {
        let mut i = Ingest::create(&d.path().join(name)).unwrap();
        for (n, b) in batches.iter().enumerate() {
            i.batch(n as u64 + 1, b).unwrap();
        }
        i
    };
    // a legacy load's final batch merges the features its records use into the baseline
    let mut i = ingest("legacy-under");
    assert!(i.upgrade(&format::DumpStore::Legacy(18)).unwrap());
    i.finish().unwrap();
    let loaded = Store::open(&d.path().join("legacy-under")).unwrap();
    assert_eq!(
        names(&log_manifest(&loaded)),
        ["content_hash_v2", "object_encoding", "plan_tension"]
    );
    assert_eq!(
        format::read_format_file(&d.path().join("legacy-under")).unwrap(),
        Some(log_manifest(&loaded))
    );
    drop(loaded);
    // a dump-4 load keeps its manifest only when the manifest names every feature its records
    // use: one that under-reports is refused, and the load appends nothing
    let mut i = ingest("dump4-under");
    let end = i.end();
    match i.upgrade(&format::DumpStore::Features(baseline_features())) {
        Err(StoreError::Dump { what: m, .. }) => assert_eq!(
            m,
            "the dump's records use feature plan_tension, which its manifest does not name"
        ),
        other => panic!("an under-reported manifest was kept: {other:?}"),
    }
    assert_eq!(i.end(), end);
}

#[test]
fn ok4_projection_guard_survives_encoding_metadata() {
    let c = carriers();
    let mut s = c.open();
    // a normal read is no projection, and writes back as the same object
    let read: Change = s.reader().get_typed(kinds::CHANGE, &c.c2).unwrap();
    assert!(!read.is_projection());
    assert_eq!(read.id(), c.c2);
    let e = codec::encode(kinds::CHANGE, &read).unwrap();
    assert_eq!(e.hash(), c.c2);
    // a redacted read cannot be written
    let residue = ChangeResidue::of(&read);
    let projection = residue
        .project(|id, version| -> Result<Block, StoreError> {
            Ok(Block::redacted(
                id.clone(),
                "claim".into(),
                None,
                "V".into(),
                rdy_model::redaction::RedactedBlockIdentity {
                    version,
                    content_hash: h(1),
                    frozen: false,
                    request: h(2),
                },
            ))
        })
        .unwrap();
    assert!(projection.is_projection());
    assert!(matches!(
        codec::encode(kinds::CHANGE, &projection),
        Err(StoreError::ProjectedWrite)
    ));
    let end = s.log_end();
    assert!(s.put(kinds::CHANGE, &projection).is_err());
    assert_eq!(s.log_end(), end);
    // an edited stored read keeps its identity, and is refused before append
    let mut edited = read.clone();
    edited.message = "edited".into();
    for e in [
        codec::encode(kinds::CHANGE, &edited).map(|_| ()),
        s.put(kinds::CHANGE, &edited).map(|_| ()),
    ] {
        match e {
            Err(StoreError::Refused(m)) => assert_eq!(m, codec::IDENTITY_MISMATCH),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(s.log_end(), end, "no refused write left a frame");
    let mut landing: LandingRecord = s.reader().get_typed(kinds::LANDING, &c.l1).unwrap();
    assert_eq!(landing.id(), c.l1);
    landing.checks.push(h(9));
    assert!(matches!(
        s.put(kinds::LANDING, &landing),
        Err(StoreError::Refused(m)) if m == codec::IDENTITY_MISMATCH
    ));
    // cleared, the edit is a new object of the latest encoding
    let mut cleared = edited;
    cleared.read_identity = None;
    let e = codec::encode(kinds::CHANGE, &cleared).unwrap();
    assert_eq!((e.version, e.hash()), (1, cleared.id()));
    // an encoding-0 stored read keeps its frozen encoding; an edit that needs encoding 1 is
    // refused while it keeps the old identity
    let old = change(vec![]);
    let bytes = postcard::to_allocvec(&LegacyChangeV0::from_current(&old).unwrap()).unwrap();
    let mut read = LegacyChangeV0::from_current(&old)
        .unwrap()
        .into_current(EncodedIdentity {
            hash: Hash::of(&bytes),
            version: 0,
        });
    assert!(!read.is_projection());
    assert_eq!(
        codec::encode(kinds::CHANGE, &read).unwrap().payload,
        bytes,
        "the frozen bytes"
    );
    read.links.briefing = Some(h(3));
    assert!(matches!(
        codec::encode(kinds::CHANGE, &read),
        Err(StoreError::Refused(m)) if m == codec::IDENTITY_MISMATCH
    ));
    let _ = encoded_hash(0, &bytes);
}
