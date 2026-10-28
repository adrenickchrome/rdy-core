//! ok12 (D-C8RZW, D-TXFHR, QD1): the carried shapes through the adapter. A populated ASSESSMENT,
//! DISPUTE, DISPUTE_RESOLUTION and CHECK_REQUEST, and a Check REVIEW of each of the three
//! verdicts, are taken into a holder, held by a captured read snapshot, written in a dump and a
//! stream, and reopened, with every stored byte and full hash as the source wrote them. The
//! landing that names the assessment keeps its appended fields. The adapter adds no assessment or
//! dispute query and no writer: a record is read only as an object of a tag's history.
//!
//! The source is a bare store built through the store's own writers (no product path writes one
//! of these records); it is closed before any adapter handle opens a holder.
use rdy_adapter::maintenance;
use rdy_adapter::stream::{ObjectKey, check_batch};
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{Hash, RepoId, StoreHandle};
use rdy_model::assessment::*;
use rdy_model::check_request::{CheckCode, CheckMode, CheckRequest, CheckSubject};
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::{
    ActorRun, Assurance, Block, BlockId, Change, ChangeLinks, Citation, FieldValue, Op, Provenance,
    RelayEvidence, Review, ReviewSubject, Role, Tag, Verdict,
};
use rdy_store::carried_records as carried;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::kinds;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::{LandRequest, Store, StoreReader};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

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

fn block(id_: &str, parent: Option<&str>, order: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "claim".into(),
        parent: parent.map(id),
        order: order.into(),
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

/// An assessment of `proposal` on `base`, with every list populated, its policy accepted under a
/// named rule, and a tension dismissed by review `dismissal`.
fn assessment(
    proposal: Hash,
    base: Hash,
    policy: PolicyRef,
    dismissal: Hash,
    blocks: Vec<BlockId>,
) -> Assessment {
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
        base: Some(base),
        method: 1,
        pack: AssessmentPack {
            name: "rdy-qa".into(),
            version: "1.0.0".into(),
            digest: Hash::of(b"pack digest"),
        },
        config: Hash::of(b"config"),
        policy: Some(AssessedPolicy {
            policy: Some(policy),
            verdict: PolicyVerdict::Accepts {
                rule: "maintainers-approve".into(),
            },
        }),
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

fn put(s: &mut Store, kind: u8, v: &(impl serde::Serialize + 'static)) -> Hash {
    s.put(kind, v).unwrap()
}

/// The landing of `proposal` by the approver, with `alter` run on its record first.
fn land_altered(
    s: &mut Store,
    proposal: Hash,
    alter: &dyn Fn(&mut rdy_model::LandingRecord),
) -> Hash {
    let req = LandRequest {
        proposal,
        accept: None,
        approver: approver(),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    };
    let prepared = s.prepare_land_altered_for_tests(req, alter).unwrap();
    s.commit_land(prepared, &[]).unwrap().landing
}

/// A source store that holds every carried shape:
///
/// - landing `l1` of change `c1` (the policy block `p` and the claim `b`);
/// - proposal `v2` of change `c2`, its ASSESSMENT (base `l1`, the policy's version, a dismissed
///   tension), then landing `l2` of `v2`: a policy landing that names the assessment and the
///   policy;
/// - a DISPUTE of `l2`; landing `l3` of `c3`, the correction; its DISPUTE_RESOLUTION;
/// - a CHECK_REQUEST of `c2` and `v2` with its `check/` ref, and a Check REVIEW of each verdict;
/// - tag `v1.0.0` at `l3`, and tag `v2.0.0` at `l4`, a later edit.
struct Carriers {
    dir: tempfile::TempDir,
    path: PathBuf,
    repo: RepoId,
    l2: Hash,
    l3: Hash,
    l4: Hash,
    tags: [Hash; 2],
    assessment: Hash,
    dispute: Hash,
    resolution: Hash,
    request: Hash,
    pass: Hash,
    concern: Hash,
    fail: Hash,
}

const V1: &str = "v1.0.0";
const V2: &str = "v2.0.0";

fn carriers() -> Carriers {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    // a doc holds both blocks, so the tree is one a holder accepts
    let mut d = block("d-00001", None, "V", "");
    d.block_type = "doc".into();
    d.fields = BTreeMap::from([
        ("name".to_string(), FieldValue::Str("guide".into())),
        ("title".to_string(), FieldValue::Str("Guide".into())),
        ("doc_type".to_string(), FieldValue::Str("page".into())),
    ]);
    let p = block("p-00001", Some("d-00001"), "V", "The authority policy.");
    let b = block("b-00001", Some("d-00001"), "W", "A claim.");
    s.commit_to_view(
        "work",
        change(vec![
            Op::Create { block: d },
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
    let a = assessment(v2, l1, policy.clone(), dismissal, vec![b.id.clone()]);
    let assessment_hash = put(&mut s, kinds::ASSESSMENT, &a);
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
            block: b3.clone(),
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
    let t1 = s
        .put_tag(&Tag {
            name: V1.into(),
            landing: l3,
            message: None,
            tagged_by: approver(),
        })
        .unwrap();
    // a later edit, so a second upload exists beyond the first
    let mut b4 = b3.clone();
    b4.body = "A claim, later.".into();
    s.commit_to_view(
        "work",
        change(vec![Op::Update {
            base: b3.version_hash(),
            block: b4,
        }]),
    )
    .unwrap();
    let v4 = s.propose("work", "fourth", writer()).unwrap();
    let l4 = s.land(&v4, approver()).unwrap();
    let t2 = s
        .put_tag(&Tag {
            name: V2.into(),
            landing: l4,
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
        l2,
        l3,
        l4,
        tags: [t1, t2],
        assessment: assessment_hash,
        dispute,
        resolution,
        request,
        pass,
        concern,
        fail,
    }
}

impl Carriers {
    /// Every carried record, and the landing that names the assessment.
    fn keys(&self) -> Vec<ObjectKey> {
        vec![
            key(kinds::ASSESSMENT, self.assessment),
            key(kinds::DISPUTE, self.dispute),
            key(kinds::DISPUTE_RESOLUTION, self.resolution),
            key(kinds::CHECK_REQUEST, self.request),
            key(kinds::REVIEW, self.pass),
            key(kinds::REVIEW, self.concern),
            key(kinds::REVIEW, self.fail),
            key(kinds::LANDING, self.l2),
        ]
    }

    /// The tag `name`'s history as one sync batch file at `path`; returns its object keys.
    fn export(&self, name: &str, path: &Path) -> Vec<ObjectKey> {
        let s = Store::open(&self.path).unwrap();
        let r = s.reader();
        let tag = self.tags[usize::from(name == V2)];
        let cl = closure(&r, tag, &BTreeSet::new()).unwrap();
        let header = StreamHeader::tag_history(
            r.store_features().unwrap(),
            self.repo,
            tag,
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

    fn target(&self, name: &str) -> UploadTarget {
        UploadTarget {
            tag: name.into(),
            landing: if name == V1 { self.l3 } else { self.l4 },
        }
    }
}

/// The stored frame (raw kind and payload) of each key, read from the closed store at `dir`.
fn frames(dir: &Path, keys: &[ObjectKey]) -> BTreeMap<ObjectKey, (u8, Vec<u8>)> {
    let s = Store::open(dir).unwrap();
    let r = s.reader();
    keys.iter()
        .map(|k| (*k, r.get_kind_raw(k.kind, &k.hash).unwrap()))
        .collect()
}

fn holder_store(root: &Path) -> PathBuf {
    root.join(".rdy").join("store")
}

fn sorted(mut keys: Vec<ObjectKey>) -> Vec<ObjectKey> {
    keys.sort();
    keys.dedup();
    keys
}

/// The refs of the closed store at `dir` that a holder must not hold: a project's.
fn project_refs(dir: &Path) -> Vec<String> {
    let s = Store::open(dir).unwrap();
    let r: StoreReader = s.reader();
    r.refs_with_prefix("")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| {
            ["canon", "view/", "proposal/", "check/", "answer/", "plan/"]
                .iter()
                .any(|p| n.starts_with(p) && !n.starts_with("answer/tag-object:"))
        })
        .collect()
}

#[test]
fn ok12_carriers_survive_holder_take_and_snapshot() {
    let c = carriers();
    let base = c.dir.path();
    let carried_keys = c.keys();
    let source_frames = frames(&c.path, &carried_keys);
    // the source holds the whole populated set, and the landing keeps its appended fields
    assert_eq!(source_frames.len(), carried_keys.len());

    let mut files = vec![];
    for name in [V1, V2] {
        let path = base.join(format!("{name}.rdysync"));
        let keys = c.export(name, &path);
        for k in &carried_keys {
            assert!(keys.contains(k), "{name}: the closure holds {k:?}");
        }
        files.push((name, path, keys));
    }

    // the take: every carried record comes in its stored bytes, and no project ref with them
    let root = base.join("holder");
    let mut h = StoreHandle::create_holder(&root, c.repo).unwrap();
    let (name, path, v1_keys) = &files[0];
    h.take(&[File::open(path).unwrap()], c.target(name))
        .unwrap();

    // a snapshot captured now, before the second upload
    let early = h.snapshot().unwrap();
    let mut early_bytes = Vec::new();
    let early_report = early
        .write_stream(V1, &BTreeSet::new(), &mut early_bytes)
        .unwrap();
    assert_eq!(sorted(early_report.keys.clone()), sorted(v1_keys.clone()));
    for k in &carried_keys {
        assert!(early_report.keys.contains(k), "{k:?}");
    }
    // the exported stream is a valid batch of the same repo with the same objects
    let checked = check_batch(early_bytes.as_slice(), Some(c.repo)).unwrap();
    assert_eq!(checked.keys, early_report.keys);

    // a later take does not change what the snapshot captured
    let (name, path, v2_keys) = &files[1];
    h.take(&[File::open(path).unwrap()], c.target(name))
        .unwrap();
    assert_eq!(early.tags().unwrap().len(), 1);
    let e = early
        .write_stream(V2, &BTreeSet::new(), std::io::sink())
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:?}");
    let mut again = Vec::new();
    early
        .write_stream(V1, &BTreeSet::new(), &mut again)
        .unwrap();
    assert_eq!(
        again, early_bytes,
        "the snapshot is unchanged byte for byte"
    );

    // a new snapshot sees both, with the carriers in each history
    let late = h.snapshot().unwrap();
    assert_eq!(late.tags().unwrap().len(), 2);
    // the API reads the landing's appended fields as the source stored them
    let want = Store::open(&c.path)
        .unwrap()
        .reader()
        .landing(&c.l2)
        .unwrap();
    assert_eq!(want.assessment, Some(c.assessment));
    assert!(want.policy.is_some());
    assert_eq!(want.assurance, Assurance::Policy);
    let facts = late.landing_facts(c.l2).unwrap();
    assert_eq!(facts.hash, c.l2);
    assert_eq!(facts.record, want, "landing_facts reads the stored landing");
    let in_history = late.tag_history(V2).unwrap();
    let l2 = in_history
        .iter()
        .find(|f| f.hash == c.l2)
        .expect("the policy landing is in the tag's history");
    assert_eq!(l2.record.assessment, Some(c.assessment));
    assert_eq!(l2.record.policy, want.policy);
    assert_eq!(l2.record.assurance, Assurance::Policy);
    let late_report = late
        .write_stream(V2, &BTreeSet::new(), std::io::sink())
        .unwrap();
    assert_eq!(sorted(late_report.keys.clone()), sorted(v2_keys.clone()));
    for k in &carried_keys {
        assert!(late_report.keys.contains(k), "{k:?}");
    }
    drop(late);

    // the dump carries them too, and loads into a second holder
    let dump = base.join("holder.rdydump");
    h.dump(&dump).unwrap();
    assert!(h.fsck().is_ok());
    drop(early);
    h.close().unwrap();

    let store = holder_store(&root);
    assert_eq!(
        frames(&store, &carried_keys),
        source_frames,
        "the holder holds the source bytes"
    );
    assert!(
        project_refs(&store).is_empty(),
        "{:?}",
        project_refs(&store)
    );

    let loaded = base.join("loaded");
    maintenance::load(&dump, &loaded).unwrap();
    assert_eq!(
        frames(&holder_store(&loaded), &carried_keys),
        source_frames,
        "a load keeps them"
    );
    assert!(project_refs(&holder_store(&loaded)).is_empty());

    // reopen: the holder, and the loaded one, export the same histories and check clean
    for dir in [&root, &loaded] {
        let h = StoreHandle::open_holder(dir).unwrap();
        h.fsck().unwrap();
        let snap = h.snapshot().unwrap();
        for (name, _, keys) in &files {
            let report = snap
                .write_stream(name, &BTreeSet::new(), std::io::sink())
                .unwrap();
            assert_eq!(sorted(report.keys), sorted(keys.clone()), "{name}");
        }
        drop(snap);
        h.close().unwrap();
        assert_eq!(frames(&holder_store(dir), &carried_keys), source_frames);
    }
}
