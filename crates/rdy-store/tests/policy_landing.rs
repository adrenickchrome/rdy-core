//! ap2 (D-ZWP7Q, D-AYYD8; O25): the store writes a policy landing: `assurance: policy`, the
//! agent's provenance, `policy: {block, version}` and the assessment, and no relay fields. fsck
//! refuses a landing of encoding 1 or more with `assurance: policy` and no `policy`, in a project
//! and in a holder, and every released dump still loads and passes it.
use rdy_model::assessment::*;
use rdy_model::*;
use rdy_store::carried_records as carried;
use rdy_store::dump::{DumpItem, DumpReader};
use rdy_store::dump_chain::DumpKind;
use rdy_store::holder::StoreMode;
use rdy_store::ingest::Ingest;
use rdy_store::kinds;
use rdy_store::plan::ProposeRequest;
use rdy_store::{LandRequest, Relay, Store, StoreError};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

const REFUSED: &str = "a policy landing has no relay, no evidence, and an assessment";

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("run-1".into()),
        role,
        at_ms: 3,
    }
}

fn agent() -> Provenance {
    prov("agent:w", Role::Writer)
}

fn approver() -> Provenance {
    prov("human:alice", Role::Approver)
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(i: &str, ty: &str, body: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
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
        provenance: agent(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Proposes view `work` with a summary; returns the version.
fn propose(s: &mut Store, title: &str) -> Hash {
    let req = ProposeRequest {
        view: "work",
        title,
        summary: Some("edits a claim"),
        by: agent(),
        changes: s.included("work").unwrap().as_ref().clone(),
        alternative: None,
        prev: None,
        rekey: None,
        answer: None,
    };
    let prepared = s.prepare_propose(req).unwrap();
    let v = prepared.version();
    s.commit_propose(prepared).unwrap();
    v
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

/// An assessment of `proposal` on `base` that the policy `policy` accepts.
fn assessment(proposal: Hash, base: Hash, policy: PolicyRef, blocks: Vec<BlockId>) -> Assessment {
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
                rule: "drafts".into(),
            },
        }),
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
            hash: Hash::of(b"card"),
            facts: vec!["Changes: 1".into()],
        },
        scope: AssessmentScope { proposal, blocks },
        issues: vec![],
    }
}

/// A store with the policy block `p` and claim `b` landed by a person, then an agent's edit of
/// `b` proposed with an assessment the policy accepts.
struct Fixture {
    _dir: tempfile::TempDir,
    s: Store,
    policy: PolicyRef,
    claim: Hash,
    version: Hash,
    assessment: Hash,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let p = block("ap-00001", "authority_policy", "The authority policy.");
    let b = block("b-00001", "claim", "A claim.");
    s.commit_to_view(
        "work",
        change(vec![
            Op::Create { block: p.clone() },
            Op::Create { block: b.clone() },
        ]),
    )
    .unwrap();
    let v1 = propose(&mut s, "first");
    let l1 = s.land(&v1, approver()).unwrap();
    let mut b2 = b.clone();
    b2.body = "A claim, edited.".into();
    s.commit_to_view(
        "work",
        change(vec![Op::Update {
            base: b.version_hash(),
            block: b2,
        }]),
    )
    .unwrap();
    let version = propose(&mut s, "second");
    let policy = PolicyRef {
        block: p.id.clone(),
        version: p.version_hash(),
    };
    let assessment = s
        .put_assessment(&assessment(version, l1, policy.clone(), vec![b.id.clone()]))
        .unwrap();
    Fixture {
        _dir: dir,
        s,
        policy,
        claim: b.version_hash(),
        version,
        assessment,
    }
}

impl Fixture {
    /// The agent's request to land the version with its assessment.
    fn request(&self) -> LandRequest {
        LandRequest {
            proposal: self.version,
            accept: None,
            approver: agent(),
            alt: None,
            relay: None,
            evidence: None,
            assessment: Some(self.assessment),
        }
    }
}

fn relay() -> Relay {
    Relay {
        by: ActorRun {
            actor: "agent:w".into(),
            run: Some("run-1".into()),
        },
        evidence: RelayEvidence {
            quoted: "yes".into(),
            session_ref: "chat-1".into(),
            question: None,
        },
    }
}

/// The stored landing `h`.
fn landing(s: &Store, h: &Hash) -> LandingRecord {
    let (raw, payload) = s.reader().get_kind_raw(kinds::LANDING, h).unwrap();
    rdy_store::codec::decode_record(raw, &payload).unwrap()
}

#[test]
fn ap2_a_policy_landing_records_policy_and_no_relay() {
    let mut f = fixture();
    let prepared =
        f.s.prepare_land_by_policy(f.request(), f.policy.clone())
            .unwrap();
    assert_eq!(prepared.landing().assurance, Assurance::Policy);
    let out = f.s.commit_land(prepared, &[]).unwrap();
    let rec = landing(&f.s, &out.landing);
    assert_eq!(rec.assurance, Assurance::Policy);
    assert_eq!(rec.policy, Some(f.policy.clone()));
    assert_eq!(rec.assessment, Some(f.assessment));
    assert_eq!(rec.approved_by, agent(), "the agent's provenance");
    assert_eq!(rec.approved_version, Some(f.version));
    assert_eq!((rec.relayed_by, rec.relay_evidence), (None, None));
    assert_eq!(rec.evidence, None);
    f.s.check_objects().unwrap();
}

#[test]
fn ap2_a_policy_landing_with_a_relay_is_refused() {
    let f = fixture();
    let end = f.s.log_end();
    let relayed = LandRequest {
        relay: Some(relay()),
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Text,
            options: vec![],
            choice: None,
            card_hash: None,
        }),
        ..f.request()
    };
    let with_evidence = LandRequest {
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Elicited,
            options: vec![],
            choice: None,
            card_hash: None,
        }),
        ..f.request()
    };
    for req in [relayed, with_evidence] {
        match f.s.prepare_land_by_policy(req, f.policy.clone()) {
            Err(StoreError::Refused(m)) => assert_eq!(m, REFUSED),
            other => panic!("a policy landing with a relay: {other:?}"),
        }
    }
    assert_eq!(f.s.log_end(), end, "nothing written");
}

#[test]
fn ap2_a_policy_landing_with_no_assessment_is_refused() {
    let f = fixture();
    let req = LandRequest {
        assessment: None,
        ..f.request()
    };
    match f.s.prepare_land_by_policy(req, f.policy.clone()) {
        Err(StoreError::Refused(m)) => assert_eq!(m, REFUSED),
        other => panic!("a policy landing with no assessment: {other:?}"),
    }
}

#[test]
fn ap2_a_policy_ref_to_another_blocks_version_is_refused() {
    let f = fixture();
    let end = f.s.log_end();
    let wrong = PolicyRef {
        block: f.policy.block.clone(),
        version: f.claim,
    };
    match f.s.prepare_land_by_policy(f.request(), wrong) {
        Err(StoreError::Decode(m)) => {
            assert!(m.ends_with(carried::POLICY_BLOCK), "{m}");
            assert!(m.contains(&format!("(kind {})", kinds::LANDING)), "{m}");
        }
        other => panic!("a policy ref to another block's version: {other:?}"),
    }
    // a policy version the store does not hold
    let missing = PolicyRef {
        block: f.policy.block.clone(),
        version: Hash([9; 32]),
    };
    match f.s.prepare_land_by_policy(f.request(), missing) {
        Err(StoreError::NotFound(h)) => assert_eq!(h, Hash([9; 32])),
        other => panic!("a policy version the store does not hold: {other:?}"),
    }
    assert_eq!(f.s.log_end(), end, "nothing written");
}

/// O25's text for landing `l`.
fn o25(l: &Hash) -> String {
    format!("landing {l}: assurance is policy but it names no policy")
}

#[test]
fn ap2_fsck_refuses_a_policy_landing_with_no_policy() {
    let mut f = fixture();
    f.s.check_objects().unwrap();
    // a forged encoding-1 record: assurance policy, its assessment, and no policy
    let prepared =
        f.s.prepare_land_altered_for_tests(f.request(), &|l| l.assurance = Assurance::Policy)
            .unwrap();
    let out = f.s.commit_land(prepared, &[]).unwrap();
    let rec = landing(&f.s, &out.landing);
    assert_eq!(
        (rec.assurance, rec.policy.clone()),
        (Assurance::Policy, None)
    );
    assert!(rec.read_identity.is_some_and(|i| i.version >= 1));
    match f.s.check_objects() {
        Err(StoreError::Decode(m)) => assert_eq!(m, o25(&out.landing)),
        other => panic!("fsck passed a policy landing with no policy: {other:?}"),
    }
}

#[test]
fn ap2_holder_fsck_refuses_a_policy_landing_with_no_policy() {
    for policy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::create_holder(&dir.path().join("h"), RepoId([7; 16])).unwrap();
        let p = block("ap-00001", "authority_policy", "The authority policy.");
        let pv = s.put(kinds::BLOCK, &p).unwrap();
        let c = s
            .put(
                kinds::CHANGE,
                &change(vec![Op::Create { block: p.clone() }]),
            )
            .unwrap();
        let state = s
            .put(
                kinds::SNAPSHOT,
                &StateSnapshot {
                    blocks: [(p.id.clone(), pv)].into_iter().collect(),
                },
            )
            .unwrap();
        let proposal = s
            .put(
                kinds::PROPOSAL,
                &Proposal {
                    stable: None,
                    prev: None,
                    title: "first".into(),
                    view: "work".into(),
                    changes: vec![c],
                    alternative: None,
                    provenance: agent(),
                    canon_at: None,
                    summary: Some("adds the policy".into()),
                },
            )
            .unwrap();
        // the forged record of the project test, with the policy it lacks when `policy`
        let rec = LandingRecord {
            proposal,
            prev: None,
            state,
            approved_by: agent(),
            changes: vec![c],
            chosen: None,
            checks: vec![],
            assurance: Assurance::Policy,
            relayed_by: None,
            relay_evidence: None,
            approved_version: None,
            rebased_from: vec![],
            evidence: None,
            assessment: None,
            policy: policy.then(|| PolicyRef {
                block: p.id.clone(),
                version: pv,
            }),
            read_identity: None,
        };
        let l = s.put(kinds::LANDING, &rec).unwrap();
        let tag = s
            .put(
                kinds::TAG,
                &Tag {
                    name: "v1".into(),
                    landing: l,
                    message: None,
                    tagged_by: approver(),
                },
            )
            .unwrap();
        s.set_refs(vec![("tag/v1".into(), tag.0.to_vec())]).unwrap();
        match (policy, s.check_holder()) {
            (true, Ok(_)) => {}
            (false, Err(StoreError::Decode(m))) => assert_eq!(m, o25(&l)),
            (_, other) => panic!("holder fsck, policy {policy}: {other:?}"),
        }
    }
}

/// The released dumps that load alone: every fixture whose header is not an increment (an
/// increment loads only after its chain's full dump, which is a fixture of its own).
fn released_dumps() -> Vec<(PathBuf, u32)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-qa/fixtures/dump");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "rdydump"))
        .collect();
    files.sort();
    assert!(files.len() >= 40, "the released fixtures: {}", files.len());
    files
        .into_iter()
        .filter_map(|p| {
            let (_, format, header) =
                DumpReader::open(BufReader::new(File::open(&p).unwrap())).unwrap();
            let increment = header.chain.is_some_and(|c| c.kind == DumpKind::Increment);
            (!increment).then_some((p, format))
        })
        .collect()
}

/// The store a load of dump `file` makes at `to`: its batches ingested in order, then the
/// store's format, as `rdy load` ingests them.
fn load(file: &Path, to: &Path) -> Store {
    let (mut reader, _, header) =
        DumpReader::open(BufReader::new(File::open(file).unwrap())).unwrap();
    let mut i = Ingest::create(to).unwrap();
    let mut n = 0u64;
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            n += 1;
            i.batch(n, &b).unwrap();
        }
    }
    assert!(n > 0, "{}: no batches", file.display());
    i.upgrade(&header.store).unwrap();
    i.finish().unwrap();
    Store::open(to).unwrap()
}

#[test]
fn ap2_fsck_passes_every_released_fixture() {
    let dumps = released_dumps();
    assert!(dumps.len() >= 20, "{} loadable dumps", dumps.len());
    for (n, (file, _format)) in dumps.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let s = load(file, &dir.path().join(format!("l{n}")));
        let checked = match s.reader().mode().unwrap() {
            StoreMode::Holder => s.check_holder(),
            StoreMode::Project => s.check_objects(),
        };
        if let Err(e) = checked {
            panic!("{}: fsck refused the load: {e}", file.display());
        }
    }
}
