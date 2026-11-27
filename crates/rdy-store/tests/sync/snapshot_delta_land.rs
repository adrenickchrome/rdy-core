//! sdl6 (D-RP6SM, D-918YB, D-7K4Z0, D-8T7PA): the landing writer writes snapshot deltas. Every
//! landing (direct, relayed, with an acceptance change, of a replayed change, by policy, of a
//! re-affirmation card) writes exactly one SNAPSHOT_DELTA, after its landing and in its batch,
//! and no SNAPSHOT; a no-op landing has its own delta; the delta is made from the final landing
//! hash, every field of the record settled; in a store that predates snapshot deltas the first
//! delta turns the feature on in its own batch; and a new store or holder lists the feature from
//! its first batch, so a new holder takes a feature upload with no maintenance.
use rdy_model::assessment::*;
use rdy_model::plan::Plan;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::format::{
    self, FEATURE_SNAPSHOT_DELTA, FEATURES_FILE, REF_STORE_FEATURES, REF_UPGRADE,
    SNAPSHOT_DELTA_REQUIRED_RDY,
};
use rdy_store::plan::{ProposeRequest, ReaffirmProposeRequest};
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{TagChoice, UploadFacts, UploadTarget};
use rdy_store::{LandRequest, OpenPolicy, Relay, Store, StoreError, codec, kinds, upgrade};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

fn prov(actor: &str, role: Role, at: i64) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("run-1".into()),
        role,
        at_ms: at,
    }
}

fn agent() -> Provenance {
    prov("agent:w", Role::Writer, 1)
}

fn approver() -> Provenance {
    prov("human:alice", Role::Approver, 2)
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
        foreign: Default::default(),
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: agent(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Proposes view `view` with a summary; returns the version.
fn propose(s: &mut Store, view: &str, title: &str) -> Hash {
    let req = ProposeRequest {
        view,
        title,
        summary: Some("edits claims"),
        by: agent(),
        changes: s.included(view).unwrap().as_ref().clone(),
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

fn request(proposal: Hash) -> LandRequest {
    LandRequest {
        proposal,
        accept: None,
        approver: approver(),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
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

/// The frames of the store's log, read from its files: `(raw kind, hash, payload)` in log
/// order, each batch ended by its marker.
fn frames(dir: &Path) -> Vec<(u8, Hash, Vec<u8>)> {
    let mut names: Vec<_> = std::fs::read_dir(dir.join("segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("seg-"))
        .collect();
    names.sort();
    let mut out = vec![];
    for p in names {
        let bytes = std::fs::read(&p).unwrap();
        let mut at = 0usize;
        while at + 37 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            if at + 37 + len > bytes.len() {
                break;
            }
            let hash = Hash(bytes[at + 5..at + 37].try_into().unwrap());
            out.push((bytes[at + 4], hash, bytes[at + 37..at + 37 + len].to_vec()));
            at += 37 + len;
        }
    }
    out
}

/// The log's batches: the frames of each, its marker left out.
fn batches(dir: &Path) -> Vec<Vec<(u8, Hash, Vec<u8>)>> {
    let mut out = vec![];
    let mut cur = vec![];
    for f in frames(dir) {
        if f.0 == kinds::BATCH_COMMIT {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(f);
        }
    }
    assert!(cur.is_empty(), "the log ends with a marker");
    out
}

/// Every frame of logical kind `kind` the log holds.
fn count_kind(dir: &Path, kind: u8) -> usize {
    frames(dir)
        .iter()
        .filter(|(k, _, _)| codec::logical_kind(*k) == kind)
        .count()
}

/// The landing `l`'s batch holds exactly one delta, after the landing, and no SNAPSHOT; the
/// delta names `l`, its `full` is the landing's `state`, its `base` is the delta of the
/// landing's `prev`, its hash is its stored bytes' (encoding 1, raw 158), and the map it folds
/// to is the map the store reads. Returns the delta.
fn one_delta(s: &Store, dir: &Path, l: Hash, what: &str) -> SnapshotDelta {
    let batch = batches(dir)
        .into_iter()
        .find(|b| {
            b.iter()
                .any(|(k, h, _)| codec::logical_kind(*k) == kinds::LANDING && *h == l)
        })
        .unwrap_or_else(|| panic!("{what}: no batch holds the landing"));
    assert!(
        !batch
            .iter()
            .any(|(k, _, _)| codec::logical_kind(*k) == kinds::SNAPSHOT),
        "{what}: no SNAPSHOT"
    );
    let deltas: Vec<usize> = batch
        .iter()
        .enumerate()
        .filter(|(_, (k, _, _))| codec::logical_kind(*k) == kinds::SNAPSHOT_DELTA)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(deltas.len(), 1, "{what}: exactly one delta in the batch");
    let at = deltas[0];
    let landing_at = batch.iter().position(|(_, h, _)| *h == l).unwrap();
    assert!(at > landing_at, "{what}: the delta follows its landing");
    let (raw, hash, payload) = &batch[at];
    assert_eq!(*raw, 158, "{what}: kind 30 at encoding 1");
    let d: SnapshotDelta = codec::decode_canonical(*raw, payload).unwrap();
    assert_eq!(
        d.id(),
        *hash,
        "{what}: the delta's hash is its stored bytes'"
    );
    assert_eq!(Hash::of(payload), *hash);
    assert_eq!(d.landing, l, "{what}: the delta names the landing");
    let rec = s.landing(&l).unwrap();
    assert_eq!(d.full, rec.state, "{what}: full is the landing's state");
    let base = rec
        .prev
        .map(|p| s.reader().snapshot_delta_of(&p).unwrap().unwrap().0);
    assert_eq!(d.base, base, "{what}: base is the previous landing's delta");
    assert_eq!(
        s.reader().snapshot_delta_of(&l).unwrap().map(|(h, _)| h),
        Some(*hash),
        "{what}: the index maps the landing to it"
    );
    let map = s.snapshot_at(&l).unwrap().blocks;
    assert_eq!(snapshot_map_id(&map), rec.state, "{what}: the fold");
    let before = rec
        .prev
        .map(|p| s.snapshot_at(&p).unwrap().blocks)
        .unwrap_or_default();
    assert_eq!(
        diff_snapshots(l, base, &before, &map),
        d,
        "{what}: the merge's difference"
    );
    d
}

/// A store with the policy block `ap-00001` and claims landed by a person.
struct Fx {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    s: Store,
    policy: PolicyRef,
    landings: Vec<Hash>,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let p = block("ap-00001", "authority_policy", "The authority policy.");
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![
                Op::Create { block: p.clone() },
                Op::Create {
                    block: block("b-00001", "claim", "One."),
                },
            ],
        ),
    )
    .unwrap();
    let v = propose(&mut s, "work", "first");
    let l = s.land_atomic(request(v)).unwrap().landing;
    Fx {
        _dir: dir,
        path,
        s,
        policy: PolicyRef {
            block: p.id.clone(),
            version: p.version_hash(),
        },
        landings: vec![l],
    }
}

impl Fx {
    /// Commits `ops` to view `work` and proposes it.
    fn next(&mut self, ops: Vec<Op>, title: &str) -> Hash {
        self.s.commit_to_view("work", change("work", ops)).unwrap();
        propose(&mut self.s, "work", title)
    }

    fn claim(&self, i: &str) -> Hash {
        *self.s.canon_state().unwrap().blocks.get(&id(i)).unwrap()
    }
}

#[test]
fn sdl6_all_landing_paths_emit_one_delta_and_no_snapshot() {
    let mut f = fixture();
    let mut what = vec!["direct (first landing)"];
    // relayed, with text evidence
    let v = f.next(
        vec![Op::Create {
            block: block("b-00002", "claim", "Two."),
        }],
        "relayed",
    );
    let l =
        f.s.land_atomic(LandRequest {
            relay: Some(relay()),
            evidence: Some(ApprovalEvidence {
                kind: EvidenceKind::Text,
                options: vec![],
                choice: None,
                card_hash: None,
            }),
            ..request(v)
        })
        .unwrap()
        .landing;
    f.landings.push(l);
    what.push("relayed");
    // with an acceptance change
    let v = f.next(
        vec![Op::Create {
            block: block("b-00003", "claim", "Three."),
        }],
        "accepted",
    );
    let base = f.claim("b-00001");
    let accept = Change {
        provenance: approver(),
        ..change(
            "work",
            vec![Op::Update {
                base,
                block: block("b-00001", "claim", "One, accepted."),
            }],
        )
    };
    let out =
        f.s.land_atomic(LandRequest {
            accept: Some(accept),
            ..request(v)
        })
        .unwrap();
    assert!(out.accept.is_some());
    f.landings.push(out.landing);
    what.push("with an acceptance change");
    // a replayed change (a change whose links name the change it replays)
    let source =
        f.s.commit_to_view(
            "other",
            change(
                "other",
                vec![Op::Create {
                    block: block("b-00004", "claim", "Four."),
                }],
            ),
        )
        .unwrap();
    let replay = Change {
        links: ChangeLinks {
            replayed_from: Some(source),
            ..ChangeLinks::default()
        },
        ..change(
            "work",
            vec![Op::Create {
                block: block("b-00004", "claim", "Four."),
            }],
        )
    };
    f.s.commit_to_view("work", replay).unwrap();
    let v = propose(&mut f.s, "work", "replayed");
    f.landings
        .push(f.s.land_atomic(request(v)).unwrap().landing);
    what.push("of a replayed change");
    // by policy: the record's assurance, policy and assessment are set before its hash
    let base = f.claim("b-00002");
    let v = f.next(
        vec![Op::Update {
            base,
            block: block("b-00002", "claim", "Two, by policy."),
        }],
        "by policy",
    );
    let canon = f.s.canon_head().unwrap().unwrap();
    let a =
        f.s.put_assessment(&assessment(v, canon, f.policy.clone(), vec![id("b-00002")]))
            .unwrap();
    let prepared =
        f.s.prepare_land_by_policy(
            LandRequest {
                approver: agent(),
                assessment: Some(a),
                ..request(v)
            },
            f.policy.clone(),
        )
        .unwrap();
    let l = f.s.commit_land(prepared, &[]).unwrap().landing;
    assert_eq!(f.s.landing(&l).unwrap().assurance, Assurance::Policy);
    f.landings.push(l);
    what.push("by policy");
    // a re-affirmation card's isolated landing: its own proposal, bound plan and seal, and its
    // view closed, in one batch
    let view = rdy_store::reaffirm_view(&Hash::of(b"a re-affirmation card"));
    let base = f.claim("b-00003");
    let mut prepared =
        f.s.prepare_reaffirm_propose(ReaffirmProposeRequest {
            view: &view,
            canon_at: f.s.canon_head().unwrap(),
            change: Change {
                provenance: approver(),
                ..change(
                    &view,
                    vec![Op::Update {
                        base,
                        block: block("b-00003", "claim", "Three, re-affirmed."),
                    }],
                )
            },
            title: "re-affirm",
            summary: "re-affirms a claim",
            by: approver(),
        })
        .unwrap();
    let version = prepared.version();
    let bound = Plan {
        stable: None,
        prev: None,
        view: view.clone(),
        proposal: Some(version),
        answers: vec![],
        heads: vec![],
        provenance: approver(),
        seal: None,
        read_identity: None,
    };
    let bound_id = f.s.bind_plan(&mut prepared, &bound).unwrap();
    let mut landing =
        f.s.prepare_reaffirm_land(
            prepared,
            approver(),
            None,
            Some(ApprovalEvidence {
                kind: EvidenceKind::Card,
                options: vec![rdy_store::REAFFIRM_HOLD_LABEL.into()],
                choice: Some(rdy_store::REAFFIRM_HOLD_LABEL.into()),
                card_hash: Some(Hash::of(b"a re-affirmation card")),
            }),
        )
        .unwrap();
    let seal = Plan {
        stable: Some(bound_id),
        prev: Some(bound_id),
        seal: Some(rdy_model::plan::PlanSeal {
            landing: landing.hash(),
            version,
            decisions: vec![],
            items: vec![],
        }),
        ..bound.clone()
    };
    f.s.plan_landing(&mut landing, None, Some(&seal)).unwrap();
    f.s.close_reaffirm_view(&mut landing, &approver()).unwrap();
    let l = f.s.commit_land(landing, &[]).unwrap().landing;
    f.landings.push(l);
    what.push("of a re-affirmation card");

    let dir = f.path.clone();
    let mut deltas = vec![];
    for (l, w) in f.landings.iter().zip(&what) {
        deltas.push(one_delta(&f.s, &dir, *l, w));
    }
    assert_eq!(deltas[0].base, None, "the first delta has no base");
    assert_eq!(
        deltas[0].set.len(),
        2,
        "the first delta sets the whole first map"
    );
    // the log holds no SNAPSHOT at all, and one delta per landing
    assert_eq!(count_kind(&dir, kinds::SNAPSHOT), 0);
    assert_eq!(count_kind(&dir, kinds::SNAPSHOT_DELTA), f.landings.len());
    // the store's check, a reindex and a reopen agree
    f.s.check_objects().unwrap();
    f.s.reindex().unwrap();
    f.s.check_objects().unwrap();
    let maps: Vec<StateSnapshot> = f
        .landings
        .iter()
        .map(|l| f.s.snapshot_at(l).unwrap())
        .collect();
    f.s.close().unwrap();
    let s = Store::open(&dir).unwrap();
    for (l, m) in f.landings.iter().zip(&maps) {
        assert_eq!(s.snapshot_at(l).unwrap(), *m);
    }
}

#[test]
fn sdl6_noop_landing_has_its_own_delta() {
    let mut f = fixture();
    let before = f.s.canon_state().unwrap();
    // two no-op landings in a row: each creates a block that its next change deletes
    for (i, bid) in ["n-00001", "n-00002"].iter().enumerate() {
        let b = block(bid, "claim", "Gone again.");
        f.s.commit_to_view(
            "work",
            change("work", vec![Op::Create { block: b.clone() }]),
        )
        .unwrap();
        f.s.commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Delete {
                    base: b.version_hash(),
                    id: b.id.clone(),
                }],
            ),
        )
        .unwrap();
        let v = propose(&mut f.s, "work", &format!("no-op {i}"));
        f.landings
            .push(f.s.land_atomic(request(v)).unwrap().landing);
    }
    assert_eq!(f.s.canon_state().unwrap(), before, "the map did not move");
    let dir = f.path.clone();
    let d: Vec<SnapshotDelta> = f
        .landings
        .iter()
        .map(|l| one_delta(&f.s, &dir, *l, "a landing"))
        .collect();
    for (i, x) in d.iter().enumerate().skip(1) {
        assert!(x.set.is_empty() && x.drop.is_empty(), "no-op {i}: empty");
        assert_eq!(x.full, d[0].full, "no-op {i}: the same full hash");
        assert_eq!(x.base, Some(d[i - 1].id()), "no-op {i}: its own base");
    }
    // identical full maps, landing-specific deltas
    let ids: BTreeSet<Hash> = d.iter().map(SnapshotDelta::id).collect();
    assert_eq!(ids.len(), d.len(), "every landing has its own delta");
    assert_eq!(count_kind(&dir, kinds::SNAPSHOT), 0);
    f.s.check_objects().unwrap();
}

#[test]
fn sdl6_policy_fields_are_final_before_delta_identity() {
    let mut f = fixture();
    let base = f.claim("b-00001");
    let v = f.next(
        vec![Op::Update {
            base,
            block: block("b-00001", "claim", "One, by policy."),
        }],
        "by policy",
    );
    let canon = f.s.canon_head().unwrap().unwrap();
    let a =
        f.s.put_assessment(&assessment(v, canon, f.policy.clone(), vec![id("b-00001")]))
            .unwrap();
    let req = LandRequest {
        approver: agent(),
        assessment: Some(a),
        ..request(v)
    };
    // the plain record the same request makes, before the policy is set
    let plain = f.s.prepare_land(req.clone()).unwrap();
    let prepared = f.s.prepare_land_by_policy(req, f.policy.clone()).unwrap();
    assert_ne!(
        prepared.hash(),
        plain.hash(),
        "the policy fields move the hash"
    );
    let rec = prepared.landing().clone();
    assert_eq!(rec.assurance, Assurance::Policy);
    assert_eq!(rec.policy.as_ref(), Some(&f.policy));
    assert_eq!(rec.assessment, Some(a));
    let lid = f.s.commit_land(prepared, &[]).unwrap().landing;
    // the stored record is the prepared one, and the delta names its final hash
    let (raw, payload) = f.s.reader().get_kind_raw(kinds::LANDING, &lid).unwrap();
    let stored: LandingRecord = codec::decode_record(raw, &payload).unwrap();
    assert_eq!(stored, rec);
    assert_eq!(Hash::of(&payload), lid);
    let d = one_delta(&f.s, &f.path.clone(), lid, "by policy");
    assert_eq!(d.landing, lid);
    assert_ne!(d.landing, plain.hash());
    // the evidence and the relay fields too: a relayed landing's delta names the record with
    // them
    let v = f.next(
        vec![Op::Create {
            block: block("b-00009", "claim", "Nine."),
        }],
        "relayed",
    );
    let evidence = ApprovalEvidence {
        kind: EvidenceKind::Card,
        options: vec!["Approve".into(), "Reject".into()],
        choice: Some("Approve".into()),
        card_hash: Some(Hash::of(b"card")),
    };
    let prepared =
        f.s.prepare_land(LandRequest {
            relay: Some(relay()),
            evidence: Some(evidence.clone()),
            ..request(v)
        })
        .unwrap();
    assert_eq!(prepared.landing().evidence.as_ref(), Some(&evidence));
    assert_eq!(prepared.landing().assurance, Assurance::Relayed);
    let lid = f.s.commit_land(prepared, &[]).unwrap().landing;
    let d = one_delta(&f.s, &f.path.clone(), lid, "relayed with card evidence");
    // the base is the previous landing's delta, never a landing hash: no circular dependency
    let prev = f.s.landing(&lid).unwrap().prev.unwrap();
    assert_ne!(d.base, Some(prev));
    assert_eq!(
        d.base,
        Some(f.s.reader().snapshot_delta_of(&prev).unwrap().unwrap().0)
    );
    f.s.check_objects().unwrap();
}

/// The features `FORMAT` mirrors at `dir`.
fn mirror(dir: &Path) -> format::StoreFeatures {
    format::read_format_file(dir).unwrap().unwrap()
}

/// The features refs the batch `b` sets.
fn features_refs(b: &[(u8, Hash, Vec<u8>)]) -> Vec<format::StoreFeatures> {
    b.iter()
        .filter(|(k, _, _)| *k == kinds::REF_UPDATE)
        .filter_map(|(_, _, p)| postcard::from_bytes::<kinds::RefUpdate>(p).ok())
        .filter(|u| u.name == REF_STORE_FEATURES)
        .map(|u| format::StoreFeatures::parse(&u.value).unwrap())
        .collect()
}

#[test]
fn sdl6_first_delta_activates_the_feature_atomically() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("s");
    // an explicitly legacy, empty store, opened with no upgrade: the writer's open does not
    // turn the feature on first
    let s = Store::create_legacy_for_tests(&dir).unwrap();
    assert_eq!(s.features(), &format::legacy_baseline_features());
    s.close().unwrap();
    let mut s = Store::open_with_policy(&dir, OpenPolicy::RequireCurrent).unwrap();
    assert!(!s.features().has(FEATURE_SNAPSHOT_DELTA));
    assert!(!mirror(&dir).has(FEATURE_SNAPSHOT_DELTA));
    let before = batches(&dir).len();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "claim", "One."),
            }],
        ),
    )
    .unwrap();
    let v = propose(&mut s, "work", "first");
    let after_propose = batches(&dir).len();
    assert_eq!(after_propose, before + 2, "a commit and a proposal");
    let l = s.land_atomic(request(v)).unwrap().landing;
    // one batch: the landing, its delta and the features ref that turns the feature on
    let all = batches(&dir);
    assert_eq!(all.len(), after_propose + 1, "the landing is one batch");
    let last = all.last().unwrap();
    assert!(last.iter().any(|(_, h, _)| *h == l));
    let refs = features_refs(last);
    assert_eq!(refs.len(), 1, "the feature turns on in the landing's batch");
    let f = refs[0]
        .features
        .iter()
        .find(|f| f.name == FEATURE_SNAPSHOT_DELTA)
        .expect("the first delta's batch lists the feature")
        .clone();
    assert_eq!(f.class, format::FeatureClass::Incompatible);
    assert_eq!(f.required_rdy, SNAPSHOT_DELTA_REQUIRED_RDY);
    assert_eq!(
        refs[0],
        format::legacy_baseline_features().merged(std::slice::from_ref(&f)),
        "every other feature is kept"
    );
    // no earlier batch names it
    for b in &all[..all.len() - 1] {
        for m in features_refs(b) {
            assert!(!m.has(FEATURE_SNAPSHOT_DELTA));
        }
    }
    let delta = one_delta(&s, &dir, l, "the first landing");
    assert_eq!(delta.base, None);
    assert!(s.features().has(FEATURE_SNAPSHOT_DELTA));
    assert!(mirror(&dir).has(FEATURE_SNAPSHOT_DELTA), "FORMAT follows");
    // no upgrade ran: no receipt, no backup
    assert!(s.get_ref(REF_UPGRADE).unwrap().is_none());
    s.check_objects().unwrap();
    s.close().unwrap();
    // no snapshot-delta step remains; span maps keep their own optional step
    assert_eq!(
        upgrade::inspect(&dir).unwrap().steps,
        [upgrade::DEDUP_SPANS_STEP]
    );

    // a legacy store whose head has no delta takes no landing until it is upgraded: the writer
    // refuses before anything is written
    let d2 = tempfile::tempdir().unwrap();
    let dir2 = d2.path().join("s");
    {
        let _legacy = format::legacy_writer_for_tests();
        let mut s = Store::create(&dir2).unwrap();
        s.commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Create {
                    block: block("b-00001", "claim", "One."),
                }],
            ),
        )
        .unwrap();
        let v = propose(&mut s, "work", "first");
        s.land(&v, approver()).unwrap();
        s.close().unwrap();
    }
    let mut s = Store::open_with_policy(&dir2, OpenPolicy::RequireCurrent).unwrap();
    assert!(!s.features().has(FEATURE_SNAPSHOT_DELTA));
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00002", "claim", "Two."),
            }],
        ),
    )
    .unwrap();
    let v = propose(&mut s, "work", "second");
    let end = s.log_end();
    assert!(matches!(
        s.land_atomic(request(v)),
        Err(StoreError::UpgradeRequired)
    ));
    assert_eq!(s.log_end(), end, "nothing written");
    assert!(!s.features().has(FEATURE_SNAPSHOT_DELTA));
    assert_eq!(count_kind(&dir2, kinds::SNAPSHOT_DELTA), 0);
}

#[test]
fn sdl6_new_store_and_holder_start_with_the_feature() {
    let want = format::StoreFeatures::of(vec![
        format::registered_features()
            .into_iter()
            .find(|f| f.name == format::FEATURE_CONTENT_HASH_V2)
            .unwrap(),
        format::registered_features()
            .into_iter()
            .find(|f| f.name == format::FEATURE_DEDUP_SPANS)
            .unwrap(),
        format::registered_features()
            .into_iter()
            .find(|f| f.name == format::FEATURE_OBJECT_ENCODING)
            .unwrap(),
        format::registered_features()
            .into_iter()
            .find(|f| f.name == FEATURE_SNAPSHOT_DELTA)
            .unwrap(),
    ]);
    assert_eq!(format::baseline_features(), want);
    assert_eq!(
        format::legacy_baseline_features()
            .features
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            format::FEATURE_CONTENT_HASH_V2,
            format::FEATURE_OBJECT_ENCODING
        ]
    );
    let d = tempfile::tempdir().unwrap();
    let project = d.path().join("p");
    let holder = d.path().join("h");
    let s = Store::create(&project).unwrap();
    let h = Store::create_holder(&holder, RepoId::new()).unwrap();
    for (dir, s, what) in [(&project, &s, "store"), (&holder, &h, "holder")] {
        // before any landing or delta: the first batch already lists the feature
        let all = batches(dir);
        assert_eq!(all.len(), 1, "{what}: one first batch");
        assert_eq!(features_refs(&all[0]), vec![want.clone()], "{what}");
        assert_eq!(s.features(), &want, "{what}");
        assert_eq!(mirror(dir), want, "{what}: FORMAT");
        assert!(
            std::fs::read_to_string(dir.join(FEATURES_FILE))
                .unwrap()
                .contains(FEATURE_SNAPSHOT_DELTA),
            "{what}"
        );
        assert_eq!(count_kind(dir, kinds::LANDING), 0, "{what}: no landing");
        assert_eq!(
            count_kind(dir, kinds::SNAPSHOT_DELTA),
            0,
            "{what}: no delta"
        );
        assert!(
            s.get_ref(REF_UPGRADE).unwrap().is_none(),
            "{what}: no receipt"
        );
    }
    drop((s, h));
    // and no upgrade is planned for either
    for dir in [&project, &holder] {
        let plan = upgrade::inspect(dir).unwrap();
        assert!(plan.steps.is_empty(), "{plan:?}");
        assert_eq!(plan.from, want);
    }
    // the first landing of the new store adds no features ref: the feature is on already
    let mut s = Store::open(&project).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "claim", "One."),
            }],
        ),
    )
    .unwrap();
    let v = propose(&mut s, "work", "first");
    let l = s.land_atomic(request(v)).unwrap().landing;
    let all = batches(&project);
    assert!(features_refs(all.last().unwrap()).is_empty());
    one_delta(&s, &project, l, "the first landing");
}

fn incoming(
    _: &mut dyn ObjectSource,
    _: &UploadFacts,
) -> Result<TagChoice, rdy_store::stream_error::StreamError> {
    Ok(TagChoice::Incoming)
}

#[test]
fn sdl6_new_holder_takes_a_feature_upload_without_maintenance() {
    let mut f = fixture();
    let v = f.next(
        vec![Op::Create {
            block: block("b-00002", "claim", "Two."),
        }],
        "second",
    );
    f.landings
        .push(f.s.land_atomic(request(v)).unwrap().landing);
    let tag =
        f.s.put_tag(&Tag {
            name: "v1".into(),
            landing: f.landings[1],
            message: None,
            tagged_by: approver(),
        })
        .unwrap();
    let repo = f.s.repo_id().unwrap().unwrap();
    // the upload: the tag's closure, with its deltas and no SNAPSHOT
    let r = f.s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    assert_eq!(
        c.records
            .iter()
            .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
            .count(),
        2
    );
    assert!(!c.records.iter().any(|k| k.kind == kinds::SNAPSHOT));
    let features = r.store_features().unwrap();
    let header = StreamHeader::tag_history(features, repo, tag, vec![]);
    let file = f._dir.path().join("up.rdysync");
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    write_batch(
        &mut source,
        &header,
        &c.records,
        File::create(&file).unwrap(),
    )
    .unwrap();
    let report = rdy_store::sync_stream::check_batch(File::open(&file).unwrap()).unwrap();
    assert!(report.record_features.has(FEATURE_SNAPSHOT_DELTA));
    drop(r);
    // a new holder takes it at once: no backup, no maintenance, no receipt
    let hdir = f._dir.path().join("holder");
    let mut h = Store::create_holder(&hdir, repo).unwrap();
    let taken = h
        .take_batches(
            &[File::open(&file).unwrap()],
            &UploadTarget {
                tag: "v1".into(),
                landing: f.landings[1],
            },
            incoming,
        )
        .unwrap();
    assert_eq!(taken.new_objects as usize, c.records.len());
    for l in &f.landings {
        assert!(h.reader().snapshot_delta_of(l).unwrap().is_some());
        assert_eq!(h.snapshot_at(l).unwrap(), f.s.snapshot_at(l).unwrap());
    }
    assert!(h.get_ref(REF_UPGRADE).unwrap().is_none());
    h.reader().check_holder().unwrap();
    h.close().unwrap();
    let backups: Vec<String> = std::fs::read_dir(f._dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
        .collect();
    assert!(backups.is_empty(), "{backups:?}");
    assert!(upgrade::inspect(&hdir).unwrap().steps.is_empty());
}
