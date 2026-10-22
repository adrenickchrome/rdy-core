//! cp3: change plans in the store (store format 17; D-4JMPH: "A new store kind PLAN, code 24";
//! p-CST21, p-FQZ2K, p-ZDH29, p-Z8ZHE). A plan version is a PLAN object; `plan/<view>` names the
//! latest; a landing's seal shares its batch. Four derived tables come from the frames alone,
//! and fsck, reindex, the closure and a holder's take keep them.
use rdy_model::plan::{
    Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem, item_hash,
};
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, closure_before, write_batch};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::object_refs::ObjectKey;
use rdy_store::plan::{AcceptedDeferral, plan_ref};
use rdy_store::segment::Location;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{StreamHeader, check_batch};
use rdy_store::take_batch::{TagChoice, UploadFacts, UploadTarget};
use rdy_store::transition::{Cause, SuspectMove, Transitions, Why};
use rdy_store::{Store, StoreError};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::PathBuf;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: 3,
    }
}

fn writer() -> Provenance {
    prov("agent:w", Role::Writer)
}

fn human() -> Provenance {
    prov("human:alice", Role::Approver)
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(i: &str, body: &str) -> Block {
    Block {
        id: id(i),
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

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: writer(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

fn ref_frame(name: &str, value: Vec<u8>) -> (u8, Vec<u8>) {
    (
        kinds::REF_UPDATE,
        enc(&RefUpdate {
            name: name.into(),
            value,
            at: ORIGIN,
        }),
    )
}

/// The PLAN frame of `p` and its `plan/<view>` ref, as the plan writers compose them.
fn plan_frames(p: &Plan) -> Vec<(u8, Vec<u8>)> {
    vec![
        (kinds::PLAN, enc(p)),
        ref_frame(&plan_ref(&p.view), p.id().0.to_vec()),
    ]
}

fn heads() -> Vec<PlanHead> {
    vec![
        PlanHead {
            repo: "app".into(),
            head: Some("a".repeat(40)),
        },
        PlanHead {
            repo: "web".into(),
            head: None,
        },
    ]
}

fn answer(item: &str, act: AnswerAct, targets: Vec<Hash>, note: Option<&str>) -> Answer {
    Answer {
        item: item.into(),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: human(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: note.map(str::to_string),
    }
}

/// The cause every fixture item names.
const CAUSE: &str = "D-00002";
/// A reaffirm item of the cause.
fn reaffirm_item() -> String {
    format!("reaffirm:{CAUSE}@0123456789ab")
}

fn defer_answer(item: Hash) -> PlanAnswer {
    PlanAnswer {
        item,
        act: PlanAct::Defer,
        reason: Some("the release owns it".into()),
        subject: Some(Hash([7; 32])),
        cause: Hash([8; 32]),
        by: writer(),
    }
}

/// A store with one plan chain of view `work`: a bound version written with its proposal, an
/// answer-only version through the typed writer that defers the subject's re-affirmation, the
/// person's acceptance of the deferral, and the landing with its seal in one batch.
struct Built {
    dir: tempfile::TempDir,
    path: PathBuf,
    s: Store,
    subject: Block,
    proposal: Hash,
    bound: Hash,
    answered: Hash,
    accept: Hash,
    landing: Hash,
    version: Hash,
    seal: Hash,
    item: Hash,
    /// The log end before the seal's batch, and after it.
    before_seal: Location,
    after_seal: Location,
}

fn cause() -> BlockId {
    id(CAUSE)
}

fn build() -> Built {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let subject = block("b-00001", "the subject claim");
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: subject.clone(),
            }],
        ),
    )
    .unwrap();
    // the proposal and its bound version, one batch
    s.set_held(true).unwrap();
    let proposal = s.propose("work", "first", writer()).unwrap();
    let v1 = Plan {
        stable: None,
        prev: None,
        view: "work".into(),
        proposal: Some(proposal),
        answers: vec![],
        heads: heads(),
        provenance: writer(),
        seal: None,
    };
    s.write_guarded_for_tests(plan_frames(&v1)).unwrap();
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    let bound = v1.id();
    // an answer-only version: the agent defers the subject's re-affirmation
    let item = item_hash(&cause(), subject.id.as_str(), PlanKind::Reaffirm);
    let v2 = Plan {
        stable: Some(bound),
        prev: Some(bound),
        proposal: None,
        answers: vec![defer_answer(item)],
        ..v1.clone()
    };
    let answered = s.put_plan(&v2).unwrap();
    assert_eq!(answered, v2.id());
    // the person accepts the deferral
    let accept = s
        .put_answer(&answer(
            "defer:P-abcd@1",
            AnswerAct::AcceptDeferrals,
            vec![],
            None,
        ))
        .unwrap();
    // the landing and its seal, one batch
    let before_seal = s.log_end();
    s.set_held(true).unwrap();
    let landing = s.land(&proposal, human()).unwrap();
    let version = s.landing(&landing).unwrap().proposal;
    let v3 = Plan {
        stable: Some(bound),
        prev: Some(answered),
        proposal: Some(version),
        seal: Some(PlanSeal {
            landing,
            version,
            decisions: vec![cause()],
            items: vec![SealedItem {
                id: item,
                kind: PlanKind::Reaffirm,
                subject: subject.id.to_string(),
                cause: cause(),
                edge: "reflects".into(),
                outcome: PlanOutcome::Deferred,
                answer: Some(accept),
            }],
        }),
        ..v2.clone()
    };
    s.write_guarded_for_tests(plan_frames(&v3)).unwrap();
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    let after_seal = s.log_end();
    Built {
        dir,
        path,
        s,
        subject,
        proposal,
        bound,
        answered,
        accept,
        landing,
        version,
        seal: v3.id(),
        item,
        before_seal,
        after_seal,
    }
}

fn plan_ref_of(s: &Store, view: &str) -> Option<Hash> {
    s.get_ref(&plan_ref(view))
        .unwrap()
        .map(|v| Hash(v.try_into().unwrap()))
}

#[test]
fn cp3_plan_chain_and_four_indexes_rebuild() {
    let mut b = build();
    let s = &mut b.s;
    assert_eq!(kinds::PLAN, 24);
    // the chain and its ref
    assert_eq!(plan_ref_of(s, "work"), Some(b.seal));
    let sealed = s.plan(&b.seal).unwrap();
    assert_eq!(
        (sealed.stable, sealed.prev),
        (Some(b.bound), Some(b.answered))
    );
    assert_eq!(sealed.proposal, Some(b.version));
    assert_eq!(s.plan(&b.answered).unwrap().answers.len(), 1);
    // the four tables, as of the end of the log
    let end = s.log_end();
    assert_eq!(s.plan_of_proposal(&b.proposal, end).unwrap(), Some(b.bound));
    assert_eq!(s.plan_of_landing(&b.landing, end).unwrap(), Some(b.seal));
    assert_eq!(s.plans_of_decision(&cause(), end).unwrap(), vec![b.seal]);
    let seal_at = match s
        .accepted_deferral(&b.subject.id, &cause(), end)
        .unwrap()
        .unwrap()
    {
        AcceptedDeferral::Plan {
            seal,
            item,
            answer,
            at,
        } => {
            assert_eq!((seal, item, answer), (b.seal, b.item, b.accept));
            at
        }
        other => panic!("a plan source: {other:?}"),
    };
    assert!(b.before_seal <= seal_at && seal_at < b.after_seal);
    // a seal never binds: the landed version has no bound plan unless it is the proposal
    if b.version != b.proposal {
        assert_eq!(s.plan_of_proposal(&b.version, end).unwrap(), None);
    }
    // an end before the seal's batch sees no seal, no decision row and no deferral
    let early = b.before_seal;
    assert_eq!(s.plan_of_landing(&b.landing, early).unwrap(), None);
    assert!(s.plans_of_decision(&cause(), early).unwrap().is_empty());
    assert_eq!(
        s.accepted_deferral(&b.subject.id, &cause(), early).unwrap(),
        None
    );
    assert_eq!(
        s.plan_of_proposal(&b.proposal, early).unwrap(),
        Some(b.bound)
    );
    // every table holds its row, and a reindex rebuilds every table byte for byte
    let dump = s.index_dump_for_tests().unwrap();
    for (name, rows) in [
        ("plan_seals", &dump.plan_seals),
        ("plan_decisions", &dump.plan_decisions),
        ("plan_deferrals", &dump.plan_deferrals),
        ("plan_proposals", &dump.plan_proposals),
    ] {
        assert_eq!(rows.len(), 1, "{name}");
    }
    s.check_objects().unwrap();
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), dump);
    // and so does an open that rebuilds the index from the whole log
    drop(b.s);
    std::fs::remove_file(b.path.join("index.redb")).unwrap();
    let s = Store::open(&b.path).unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), dump);
    s.check_objects().unwrap();
}

#[test]
fn cp3_public_writes_cannot_forge_plans_or_refs() {
    let mut b = build();
    let s = &mut b.s;
    let next = Plan {
        stable: None,
        prev: None,
        view: "other".into(),
        proposal: None,
        answers: vec![],
        heads: vec![],
        provenance: writer(),
        seal: None,
    };
    let end = s.log_end();
    let refs = s.refs_with_prefix("").unwrap();
    let plan_only = "PLAN is written only by the plan writer";
    let refs_only = "plan refs are written only by the plan writer";
    let refused = |r: Result<(), StoreError>, want: &str| match r {
        Err(StoreError::Refused(m)) => assert_eq!(m, want),
        other => panic!("refused with {want:?}: {other:?}"),
    };
    // put, write, seal and set_refs
    refused(s.put(kinds::PLAN, &next).map(|_| ()), plan_only);
    refused(
        s.write(vec![(kinds::PLAN, enc(&next))]).map(|_| ()),
        plan_only,
    );
    refused(s.write(plan_frames(&next)).map(|_| ()), plan_only);
    refused(
        s.write(vec![ref_frame("plan/other", vec![1; 32])])
            .map(|_| ()),
        refs_only,
    );
    refused(
        s.set_refs(vec![("plan/work".into(), b.bound.0.to_vec())]),
        refs_only,
    );
    refused(s.seal(vec![(kinds::PLAN, enc(&next))]), plan_only);
    // the typed writer writes an answer-only version only, and checks it before any append
    let bound = Plan {
        proposal: Some(b.proposal),
        ..next.clone()
    };
    refused(
        s.put_plan(&bound).map(|_| ()),
        &format!("plan {}: invalid proposal", bound.id()),
    );
    let sealed = s.plan(&b.seal).unwrap();
    refused(
        s.put_plan(&sealed).map(|_| ()),
        &format!("plan {}: invalid seal", sealed.id()),
    );
    // a sealed predecessor ends the chain
    let after_seal = Plan {
        stable: Some(b.bound),
        prev: Some(b.seal),
        view: "work".into(),
        ..next.clone()
    };
    refused(
        s.put_plan(&after_seal).map(|_| ()),
        &format!("plan {}: invalid chain", after_seal.id()),
    );
    // an answer-only version that skips the view's latest version
    let behind = Plan {
        stable: Some(b.bound),
        prev: Some(b.answered),
        view: "work".into(),
        ..next.clone()
    };
    refused(
        s.put_plan(&behind).map(|_| ()),
        &format!("plan {}: invalid chain", behind.id()),
    );
    let unsorted = Plan {
        answers: vec![defer_answer(Hash([2; 32])), defer_answer(Hash([1; 32]))],
        ..next.clone()
    };
    refused(
        s.put_plan(&unsorted).map(|_| ()),
        &format!("plan {}: unsorted answers", unsorted.id()),
    );
    // nothing reached the log or the refs
    assert_eq!(s.log_end(), end);
    assert_eq!(s.refs_with_prefix("").unwrap(), refs);
    assert_eq!(plan_ref_of(s, "other"), None);
    // the typed writer's own write is one batch: the version and its ref
    let h = s.put_plan(&next).unwrap();
    assert_eq!(
        s.last_batch().iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        [kinds::PLAN, kinds::REF_UPDATE]
    );
    assert_eq!(plan_ref_of(s, "other"), Some(h));
    s.check_objects().unwrap();
}

/// The frames of the log in `[from, to)`: (kind, hash).
fn frames(s: &Store, from: Location, to: Location) -> Vec<(u8, Hash)> {
    let mut out = vec![];
    s.reader()
        .for_each_frame_from(from, to, |_, kind, hash, _, _| {
            out.push((kind, hash));
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn cp3_seal_follows_its_landing_in_one_batch() {
    let b = build();
    let s = &b.s;
    let batch = frames(s, b.before_seal, b.after_seal);
    let at = |kind: u8, hash: Hash| {
        batch
            .iter()
            .position(|f| *f == (kind, hash))
            .unwrap_or_else(|| panic!("{kind}:{hash} is in the landing's batch"))
    };
    let landing = at(kinds::LANDING, b.landing);
    let seal = at(kinds::PLAN, b.seal);
    assert!(landing < seal, "the landing precedes its seal");
    // the seal's ref follows it, and one marker ends the batch
    assert_eq!(batch[seal + 1].0, kinds::REF_UPDATE);
    let markers = batch
        .iter()
        .filter(|(k, _)| *k == kinds::BATCH_COMMIT)
        .count();
    assert_eq!(markers, 1);
    assert_eq!(batch.last().unwrap().0, kinds::BATCH_COMMIT);
    s.check_objects().unwrap();

    // a seal in a batch of its own, after its landing's, is damage
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "x"),
            }],
        ),
    )
    .unwrap();
    let p = s.propose("work", "first", writer()).unwrap();
    let landing = s.land(&p, human()).unwrap();
    let version = s.landing(&landing).unwrap().proposal;
    let seal = unbound_seal("work", landing, version, vec![]);
    s.write_guarded_for_tests(plan_frames(&seal)).unwrap();
    assert_damage(&s, &seal.id(), "invalid seal");
}

/// An unbound proposal's seal: no earlier chain version.
fn unbound_seal(view: &str, landing: Hash, version: Hash, decisions: Vec<BlockId>) -> Plan {
    Plan {
        stable: None,
        prev: None,
        view: view.into(),
        proposal: Some(version),
        answers: vec![],
        heads: heads(),
        provenance: human(),
        seal: Some(PlanSeal {
            landing,
            version,
            decisions,
            items: vec![],
        }),
    }
}

fn assert_damage(s: &Store, h: &Hash, reason: &str) {
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert_eq!(m, format!("plan {h}: {reason}")),
        other => panic!("plan {h}: {reason}: {other:?}"),
    }
}

/// `forge`'s plan, written raw after the built chain, as fsck reads it.
fn damaged(forge: impl FnOnce(&mut Built) -> Hash, reason: &str) {
    let mut b = build();
    let h = forge(&mut b);
    assert_damage(&b.s, &h, reason);
}

/// Writes `p` raw, as no writer does; it must be a new record.
fn raw(s: &mut Store, p: &Plan) -> Hash {
    assert!(!s.has_kind(kinds::PLAN, &p.id()).unwrap(), "a new plan");
    s.write_guarded_for_tests(vec![(kinds::PLAN, enc(p))])
        .unwrap();
    p.id()
}

fn fresh(view: &str) -> Plan {
    Plan {
        stable: None,
        prev: None,
        view: view.into(),
        proposal: None,
        answers: vec![],
        heads: heads(),
        provenance: writer(),
        seal: None,
    }
}

#[test]
fn cp3_bad_chain_or_seal_is_damage() {
    // invalid fields: a deferral with no reason, a reason past 2000 characters, an empty view,
    // a head that is no git hash, and a done answer with a reason
    for bad in [
        Plan {
            answers: vec![PlanAnswer {
                reason: None,
                ..defer_answer(Hash([1; 32]))
            }],
            ..fresh("next")
        },
        Plan {
            answers: vec![PlanAnswer {
                reason: Some("x".repeat(2001)),
                ..defer_answer(Hash([1; 32]))
            }],
            ..fresh("next")
        },
        Plan {
            answers: vec![PlanAnswer {
                reason: Some(" padded".into()),
                ..defer_answer(Hash([1; 32]))
            }],
            ..fresh("next")
        },
        fresh(""),
        Plan {
            heads: vec![PlanHead {
                repo: "app".into(),
                head: Some("ABC".into()),
            }],
            ..fresh("next")
        },
        Plan {
            answers: vec![PlanAnswer {
                act: PlanAct::Done,
                ..defer_answer(Hash([1; 32]))
            }],
            ..fresh("next")
        },
    ] {
        damaged(|b| raw(&mut b.s, &bad), "invalid fields");
    }
    // the lists' order
    damaged(
        |b| {
            let p = Plan {
                answers: vec![defer_answer(Hash([2; 32])), defer_answer(Hash([1; 32]))],
                ..fresh("next")
            };
            raw(&mut b.s, &p)
        },
        "unsorted answers",
    );
    damaged(
        |b| {
            let mut p = fresh("next");
            p.heads.reverse();
            raw(&mut b.s, &p)
        },
        "unsorted heads",
    );
    // a seal's lists: decisions, then items
    let sealed_with = |b: &mut Built, decisions: Vec<BlockId>, items: Vec<SealedItem>| {
        let mut p = b.s.plan(&b.seal).unwrap();
        let seal = p.seal.as_mut().unwrap();
        seal.decisions = decisions;
        seal.items = items;
        raw(&mut b.s, &p)
    };
    let item = |subject: &str| SealedItem {
        id: item_hash(&cause(), subject, PlanKind::Update),
        kind: PlanKind::Update,
        subject: subject.into(),
        cause: cause(),
        edge: "reflects".into(),
        outcome: PlanOutcome::Open,
        answer: None,
    };
    damaged(
        |b| sealed_with(b, vec![id("D-00003"), id("D-00002")], vec![]),
        "unsorted decisions",
    );
    damaged(
        |b| {
            let mut items = vec![item("b-00001"), item("b-00002")];
            items.sort_by_key(|i| std::cmp::Reverse(i.id));
            sealed_with(b, vec![], items)
        },
        "unsorted items",
    );
    // the chain: a stable with no prev, a prev that is no plan, a sealed predecessor, a stable
    // that is not the chain's first version, and a fork beside the view's latest version
    damaged(
        |b| {
            let p = Plan {
                stable: Some(b.bound),
                ..fresh("work")
            };
            raw(&mut b.s, &p)
        },
        "invalid chain",
    );
    damaged(
        |b| {
            let p = Plan {
                stable: Some(b.bound),
                prev: Some(b.accept),
                ..fresh("work")
            };
            raw(&mut b.s, &p)
        },
        "invalid chain",
    );
    damaged(
        |b| {
            let p = Plan {
                stable: Some(b.bound),
                prev: Some(b.seal),
                ..fresh("work")
            };
            raw(&mut b.s, &p)
        },
        "invalid chain",
    );
    damaged(
        |b| {
            let first = b.s.put_plan(&fresh("next")).unwrap();
            let second =
                b.s.put_plan(&Plan {
                    stable: Some(first),
                    prev: Some(first),
                    ..fresh("next")
                })
                .unwrap();
            let p = Plan {
                stable: Some(second),
                prev: Some(second),
                answers: vec![defer_answer(Hash([1; 32]))],
                ..fresh("next")
            };
            raw(&mut b.s, &p)
        },
        "invalid chain",
    );
    damaged(
        |b| {
            let first = b.s.put_plan(&fresh("next")).unwrap();
            let p = Plan {
                answers: vec![defer_answer(Hash([1; 32]))],
                ..fresh("next")
            };
            let _ = first;
            raw(&mut b.s, &p)
        },
        "invalid chain",
    );
    // the bound proposal: one from an earlier batch, one of another view, and a second binding
    damaged(
        |b| {
            let p = Plan {
                proposal: Some(b.proposal),
                provenance: human(),
                ..fresh("work")
            };
            raw(&mut b.s, &p)
        },
        "invalid proposal",
    );
    damaged(
        |b| {
            b.s.commit_to_view(
                "side",
                change(
                    "side",
                    vec![Op::Create {
                        block: block("b-00004", "side"),
                    }],
                ),
            )
            .unwrap();
            b.s.set_held(true).unwrap();
            let p = b.s.propose("side", "side", writer()).unwrap();
            let bad = Plan {
                proposal: Some(p),
                ..fresh("next")
            };
            let h = raw(&mut b.s, &bad);
            b.s.seal(vec![]).unwrap();
            h
        },
        "invalid proposal",
    );
    damaged(
        |b| {
            b.s.commit_to_view(
                "side",
                change(
                    "side",
                    vec![Op::Create {
                        block: block("b-00004", "side"),
                    }],
                ),
            )
            .unwrap();
            b.s.set_held(true).unwrap();
            let p = b.s.propose("side", "side", writer()).unwrap();
            let first = Plan {
                proposal: Some(p),
                ..fresh("side")
            };
            raw(&mut b.s, &first);
            let second = Plan {
                stable: Some(first.id()),
                prev: Some(first.id()),
                proposal: Some(p),
                ..fresh("side")
            };
            let h = raw(&mut b.s, &second);
            b.s.seal(vec![]).unwrap();
            h
        },
        "invalid proposal",
    );
    // the seal: its landing in an earlier batch, a version that is not the landed one, an
    // accepting answer of another act, and an item whose id is not its own
    damaged(
        |b| {
            let p = unbound_seal("next", b.landing, b.version, vec![]);
            raw(&mut b.s, &p)
        },
        "invalid seal",
    );
    let land_side = |b: &mut Built| -> (Hash, Hash) {
        b.s.commit_to_view(
            "side",
            change(
                "side",
                vec![Op::Create {
                    block: block("b-00004", "side"),
                }],
            ),
        )
        .unwrap();
        let p = b.s.propose("side", "side", writer()).unwrap();
        b.s.set_held(true).unwrap();
        let l = b.s.land(&p, human()).unwrap();
        let v = b.s.landing(&l).unwrap().proposal;
        (l, v)
    };
    damaged(
        |b| {
            let (l, _) = land_side(b);
            let p = unbound_seal("side", l, b.proposal, vec![]);
            let h = raw(&mut b.s, &p);
            b.s.seal(vec![]).unwrap();
            h
        },
        "invalid seal",
    );
    damaged(
        |b| {
            let (l, v) = land_side(b);
            let mut p = unbound_seal("side", l, v, vec![]);
            p.proposal = Some(b.proposal);
            let h = raw(&mut b.s, &p);
            b.s.seal(vec![]).unwrap();
            h
        },
        "invalid seal",
    );
    damaged(
        |b| {
            let (l, v) = land_side(b);
            let mut p = unbound_seal("side", l, v, vec![]);
            p.seal.as_mut().unwrap().items = vec![SealedItem {
                outcome: PlanOutcome::Deferred,
                answer: Some(b.bound),
                ..item("b-00004")
            }];
            let h = raw(&mut b.s, &p);
            b.s.seal(vec![]).unwrap();
            h
        },
        "invalid seal",
    );
    damaged(
        |b| {
            let (l, v) = land_side(b);
            let mut p = unbound_seal("side", l, v, vec![]);
            p.seal.as_mut().unwrap().items = vec![SealedItem {
                id: Hash([5; 32]),
                ..item("b-00004")
            }];
            let h = raw(&mut b.s, &p);
            b.s.seal(vec![]).unwrap();
            h
        },
        "invalid seal",
    );
    // two seals of one landing
    damaged(
        |b| {
            let (l, v) = land_side(b);
            raw(&mut b.s, &unbound_seal("side", l, v, vec![]));
            let h = raw(&mut b.s, &unbound_seal("side", l, v, vec![cause()]));
            b.s.seal(vec![]).unwrap();
            h
        },
        "duplicate landing seal",
    );
    // a version the view's ref does not name
    let mut b = build();
    let h = raw(&mut b.s, &fresh("next"));
    match b.s.check_objects() {
        Err(StoreError::Decode(m)) => assert_eq!(
            m,
            format!("plan ref \"plan/next\" does not name the latest plan {h} of its view")
        ),
        other => panic!("{other:?}"),
    }
}

/// A store with the built chain, tagged at the sealed landing: its repo, path and tag.
fn tagged() -> (Built, Hash) {
    let mut b = build();
    let tag =
        b.s.put_tag(&Tag {
            name: "v1".into(),
            landing: b.landing,
            message: None,
            tagged_by: human(),
        })
        .unwrap();
    (b, tag)
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn export(s: &Store, tag: Hash, end: Location, path: &std::path::Path) -> Vec<ObjectKey> {
    let r = s.reader();
    let c = closure_before(&r, tag, &BTreeSet::new(), end).unwrap();
    let header = StreamHeader::tag_history(s.repo_id().unwrap().unwrap(), tag, vec![]);
    let mut source = StoreSource { reader: &r, end };
    write_batch(
        &mut source,
        &header,
        &c.records,
        File::create(path).unwrap(),
    )
    .unwrap();
    c.records
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

#[test]
fn cp3_tag_closure_carries_its_seal_and_chain() {
    let (b, tag) = tagged();
    let s = &b.s;
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let got: BTreeSet<ObjectKey> = c.records.iter().copied().collect();
    // the seal joins its landing; its links bring the chain, the versions and the acceptance
    for k in [
        key(kinds::PLAN, b.seal),
        key(kinds::PLAN, b.answered),
        key(kinds::PLAN, b.bound),
        key(kinds::PROPOSAL, b.proposal),
        key(kinds::PROPOSAL, b.version),
        key(kinds::ANSWER, b.accept),
        key(kinds::LANDING, b.landing),
    ] {
        assert!(got.contains(&k), "{k} is in the closure");
    }
    let order = &c.records;
    let pos = |k: ObjectKey| order.iter().position(|x| *x == k).unwrap();
    assert!(pos(key(kinds::PLAN, b.bound)) < pos(key(kinds::PLAN, b.answered)));
    assert!(pos(key(kinds::PLAN, b.answered)) < pos(key(kinds::PLAN, b.seal)));
    assert!(pos(key(kinds::LANDING, b.landing)) < pos(key(kinds::PLAN, b.seal)));
    // the batch checks, and a holder takes it
    let path = b.dir.path().join("v1.rdysync");
    let exported = export(s, tag, END_OF_LOG, &path);
    assert_eq!(exported, c.records);
    let report = check_batch(File::open(&path).unwrap()).unwrap();
    assert_eq!(report.keys, c.records);
    let hpath = b.dir.path().join("h");
    let mut h = Store::create_holder(&hpath, s.repo_id().unwrap().unwrap()).unwrap();
    h.take_batches(
        &[File::open(&path).unwrap()],
        &UploadTarget {
            tag: "v1".into(),
            landing: b.landing,
        },
        incoming,
    )
    .unwrap();
    for k in [b.seal, b.answered, b.bound] {
        assert!(h.has_kind(kinds::PLAN, &k).unwrap());
    }
    let end = h.log_end();
    assert_eq!(h.plan_of_landing(&b.landing, end).unwrap(), Some(b.seal));
    h.check_holder().unwrap();

    // a batch's PLAN keeps the plan record rules, with fsck's own text
    let mut unsorted = s.plan(&b.answered).unwrap();
    unsorted.answers = vec![defer_answer(Hash([2; 32])), defer_answer(Hash([1; 32]))];
    let k = key(kinds::PLAN, unsorted.id());
    let mut source: BTreeMap<ObjectKey, rdy_store::sync_stream::StreamRecord> = BTreeMap::new();
    source.insert(
        k,
        rdy_store::sync_stream::StreamRecord::Present {
            key: k,
            payload: enc(&unsorted),
        },
    );
    let header = StreamHeader::tag_history(
        s.repo_id().unwrap().unwrap(),
        tag,
        vec![key(kinds::TAG, tag)],
    );
    let (bytes, _) = write_batch(&mut source, &header, &[k], Vec::new()).unwrap();
    let e = check_batch(&bytes[..]).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD");
    assert!(
        e.message
            .ends_with(&format!("plan {}: unsorted answers", unsorted.id())),
        "{}",
        e.message
    );

    // a captured end leaves out a seal written after it
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "x"),
            }],
        ),
    )
    .unwrap();
    let p = s.propose("work", "first", writer()).unwrap();
    let landing = s.land(&p, human()).unwrap();
    let version = s.landing(&landing).unwrap().proposal;
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing,
            message: None,
            tagged_by: human(),
        })
        .unwrap();
    let captured = s.log_end();
    let late = unbound_seal("work", landing, version, vec![]);
    s.write_guarded_for_tests(plan_frames(&late)).unwrap();
    let r = s.reader();
    let before = closure_before(&r, tag, &BTreeSet::new(), captured).unwrap();
    assert!(!before.records.contains(&key(kinds::PLAN, late.id())));
    let now = closure(&r, tag, &BTreeSet::new()).unwrap();
    assert!(now.records.contains(&key(kinds::PLAN, late.id())));
    assert_eq!(s.plan_of_landing(&landing, captured).unwrap(), None);
}

#[test]
fn cp3_holder_keeps_plans_without_project_refs() {
    let (b, tag) = tagged();
    let path = b.dir.path().join("v1.rdysync");
    export(&b.s, tag, END_OF_LOG, &path);
    let hpath = b.dir.path().join("h");
    let mut h = Store::create_holder(&hpath, b.s.repo_id().unwrap().unwrap()).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing: b.landing,
    };
    h.take_batches(&[File::open(&path).unwrap()], &target, incoming)
        .unwrap();
    // the same upload again appends nothing
    let end = h.log_end();
    h.take_batches(&[File::open(&path).unwrap()], &target, incoming)
        .unwrap();
    assert_eq!(h.log_end(), end);
    assert!(h.refs_with_prefix("plan/").unwrap().is_empty());
    assert_eq!(h.plan_of_landing(&b.landing, end).unwrap(), Some(b.seal));
    assert_eq!(h.plan_of_proposal(&b.proposal, end).unwrap(), Some(b.bound));
    assert_eq!(h.plans_of_decision(&cause(), end).unwrap(), vec![b.seal]);
    assert!(matches!(
        h.accepted_deferral(&b.subject.id, &cause(), end).unwrap(),
        Some(AcceptedDeferral::Plan { seal, .. }) if seal == b.seal
    ));
    h.check_holder().unwrap();
    let dump = h.index_dump_for_tests().unwrap();
    assert_eq!(dump.plan_seals.len(), 1);
    h.reindex().unwrap();
    assert_eq!(h.index_dump_for_tests().unwrap(), dump);
    h.check_holder().unwrap();
    // a holder never writes a plan ref: a write of one is refused, and a forged one is damage
    let named = vec![("plan/work".to_string(), b.seal.0.to_vec())];
    assert!(h.set_refs(named.clone()).is_err());
    h.set_refs_unchecked_for_tests(named).unwrap();
    assert!(
        h.check_holder().is_err(),
        "a plan ref in a holder is damage"
    );
}

#[test]
fn cp3_keep_suspect_members_rebuild_from_answer_targets() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let one = block("b-00001", "member one");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: one.clone() }]),
    )
    .unwrap();
    // a second member's version first written in the answer's own batch
    let two = block("b-00003", "member two");
    let mut targets = vec![one.version_hash(), two.version_hash()];
    targets.sort();
    let keep = answer(
        &reaffirm_item(),
        AnswerAct::KeepSuspect,
        targets,
        Some("it still holds"),
    );
    s.write_guarded_for_tests(vec![(kinds::BLOCK, enc(&two)), (kinds::ANSWER, enc(&keep))])
        .unwrap();
    let end = s.log_end();
    for member in [&one.id, &two.id] {
        match s.accepted_deferral(member, &cause(), end).unwrap() {
            Some(AcceptedDeferral::KeepSuspect { answer, .. }) => assert_eq!(answer, keep.id()),
            other => panic!("{member}: {other:?}"),
        }
    }
    // only the recorded members and cause
    assert_eq!(
        s.accepted_deferral(&id("b-00005"), &cause(), end).unwrap(),
        None
    );
    assert_eq!(
        s.accepted_deferral(&one.id, &id("D-00009"), end).unwrap(),
        None
    );
    s.check_objects().unwrap();
    let dump = s.index_dump_for_tests().unwrap();
    assert_eq!(dump.plan_deferrals.len(), 2);
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), dump);
    // the answer rules: the item, the note and the targets
    let refused = |s: &mut Store, a: &Answer, reason: &str| match s.put_answer(a) {
        Err(StoreError::Refused(m)) => assert_eq!(m, format!("answer {}: {reason}", a.id())),
        other => panic!("{reason}: {other:?}"),
    };
    let base = answer(
        &reaffirm_item(),
        AnswerAct::KeepSuspect,
        vec![one.version_hash()],
        Some("it holds"),
    );
    refused(
        &mut s,
        &Answer {
            item: format!("{CAUSE}@0123456789ab"),
            ..base.clone()
        },
        "invalid plan answer item",
    );
    refused(
        &mut s,
        &Answer {
            note: None,
            ..base.clone()
        },
        "invalid plan answer note",
    );
    refused(
        &mut s,
        &Answer {
            note: Some(" untrimmed".into()),
            ..base.clone()
        },
        "invalid plan answer note",
    );
    refused(
        &mut s,
        &Answer {
            targets: vec![],
            ..base.clone()
        },
        "invalid plan answer targets",
    );
    let landing_like = s.put_blob(b"not a block").unwrap();
    refused(
        &mut s,
        &Answer {
            targets: vec![landing_like],
            ..base.clone()
        },
        "invalid plan answer targets",
    );
    for (act, item) in [
        (AnswerAct::AcceptDeferrals, "P-abcd@1"),
        (AnswerAct::RefuseDeferrals, "defer:P-abcd"),
        (AnswerAct::RejectReaffirm, "reaffirm:D-00002@0123456789ab"),
    ] {
        refused(
            &mut s,
            &answer(item, act, vec![], None),
            "invalid plan answer item",
        );
    }
    for (act, item) in [
        (AnswerAct::AcceptDeferrals, "defer:P-abcd@1"),
        (AnswerAct::RefuseDeferrals, "defer:P-abcd@2"),
        (AnswerAct::RejectReaffirm, "D-00002@0123456789ab"),
    ] {
        refused(
            &mut s,
            &answer(item, act, vec![one.version_hash()], None),
            "invalid plan answer targets",
        );
        s.put_answer(&answer(item, act, vec![], None)).unwrap();
    }
    // fsck keeps the same rules on a forged answer
    let forged = Answer {
        targets: vec![landing_like],
        ..base
    };
    s.write_guarded_for_tests(vec![(kinds::ANSWER, enc(&forged))])
        .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert_eq!(
            m,
            format!("answer {}: invalid plan answer targets", forged.id())
        ),
        other => panic!("{other:?}"),
    }
}

fn suspect(block: &str, reason: &str) -> SuspectMove {
    SuspectMove {
        block: id(block),
        target: cause(),
        reason: reason.into(),
        writers: vec![],
        why: Why {
            cause: Cause::Recheck,
            id: None,
            actor: None,
            run: None,
        },
    }
}

fn moves(gone: Vec<SuspectMove>, new: Vec<SuspectMove>) -> (u8, Vec<u8>) {
    Transitions {
        at_ms: 1,
        resolved: vec![],
        opened: vec![],
        gone,
        new,
    }
    .frame()
}

#[test]
fn cp3_latest_deferral_preserves_captured_reads_and_expires_on_clear() {
    let mut b = build();
    let subject = b.subject.id.clone();
    // the built chain's seal is the first acceptance; its batch ends at `after_seal`
    let e0 = b.before_seal;
    let e1 = b.after_seal;
    let s = &mut b.s;
    // the second acceptance: a KeepSuspect of the same pair
    let keep = answer(
        &reaffirm_item(),
        AnswerAct::KeepSuspect,
        vec![b.subject.version_hash()],
        Some("it still holds"),
    );
    let keep_id = s.put_answer(&keep).unwrap();
    let e2 = s.log_end();
    // the suspect opens, then goes: the clear; then it reopens
    s.seal(vec![moves(vec![], vec![suspect("b-00001", "r1")])])
        .unwrap();
    let e3 = s.log_end();
    s.seal(vec![moves(vec![suspect("b-00001", "r1")], vec![])])
        .unwrap();
    let e4 = s.log_end();
    s.seal(vec![moves(vec![], vec![suspect("b-00001", "r1")])])
        .unwrap();
    let e5 = s.log_end();
    // a third acceptance, then a reason-only replacement in one batch: no clear
    let again = answer(
        &reaffirm_item(),
        AnswerAct::KeepSuspect,
        vec![b.subject.version_hash()],
        Some("it holds again"),
    );
    let again_id = s.put_answer(&again).unwrap();
    let e6 = s.log_end();
    s.seal(vec![moves(
        vec![suspect("b-00001", "r1")],
        vec![suspect("b-00001", "r2")],
    )])
    .unwrap();
    let e7 = s.log_end();
    let source = |s: &Store, end: Location| s.accepted_deferral(&subject, &cause(), end).unwrap();
    let plan = |a: Option<AcceptedDeferral>| matches!(a, Some(AcceptedDeferral::Plan { seal, .. }) if seal == b.seal);
    let kept = |a: Option<AcceptedDeferral>, h: Hash| matches!(a, Some(AcceptedDeferral::KeepSuspect { answer, .. }) if answer == h);
    let check = |s: &Store| {
        // one row: the latest acceptance
        assert_eq!(s.index_dump_for_tests().unwrap().plan_deferrals.len(), 1);
        assert_eq!(source(s, e0), None, "before every acceptance");
        assert!(plan(source(s, e1)), "after the seal");
        assert!(kept(source(s, e2), keep_id), "after the KeepSuspect");
        assert!(kept(source(s, e3), keep_id), "an opening keeps it");
        assert_eq!(source(s, e4), None, "the clear expires it");
        assert_eq!(source(s, e5), None, "a reopening does not revive it");
        assert!(kept(source(s, e6), again_id), "a later acceptance");
        assert!(kept(source(s, e7), again_id), "a reason-only replacement");
        // two pairs at once, one folded and one read from its row
        let both = s
            .accepted_deferrals(&[(subject.clone(), cause()), (id("b-00009"), cause())], e1)
            .unwrap();
        assert!(plan(both[0]));
        assert_eq!(both[1], None);
    };
    check(s);
    s.check_objects().unwrap();
    s.reindex().unwrap();
    check(s);
}

/// The bytes of the store's segments, by file name.
fn segments(path: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(path.join("segments"))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn cp3_failed_plan_append_keeps_acknowledged_bytes() {
    let mut b = build();
    let bytes = segments(&b.path);
    let end = b.s.log_end();
    let dump = b.s.index_dump_for_tests().unwrap();
    let next = fresh("next");
    // a torn append and a failed index update each leave the acknowledged log as it was
    b.s.fail_next_append_after(10);
    assert!(b.s.put_plan(&next).is_err());
    assert_eq!(b.s.log_end(), end);
    assert_eq!(segments(&b.path), bytes);
    b.s.fail_next_index_apply();
    assert!(b.s.put_plan(&next).is_err());
    assert_eq!(b.s.log_end(), end);
    assert_eq!(segments(&b.path), bytes);
    assert_eq!(b.s.index_dump_for_tests().unwrap(), dump);
    assert_eq!(plan_ref_of(&b.s, "next"), None);
    // the retry writes it; the bytes before it are the acknowledged ones
    let h = b.s.put_plan(&next).unwrap();
    assert_eq!(plan_ref_of(&b.s, "next"), Some(h));
    let after = segments(&b.path);
    for (name, before) in &bytes {
        assert_eq!(&after[name][..before.len()], &before[..], "{name}");
    }
    drop(b.s);
    let s = Store::open(&b.path).unwrap();
    assert_eq!(plan_ref_of(&s, "next"), Some(h));
    s.check_objects().unwrap();
}

// -------------------------------------------------------------------------------------------
// fix round 1: cumulative answers, and the take's plan and plan-answer context

#[test]
fn cp3_successor_plans_keep_every_earlier_answer() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let (a, b) = (Hash([1; 32]), Hash([2; 32]));
    let first = Plan {
        answers: vec![defer_answer(a)],
        ..fresh("solo")
    };
    let first = s.put_plan(&first).unwrap();
    let next = |prev: Hash, answers: Vec<PlanAnswer>| Plan {
        stable: Some(first),
        prev: Some(prev),
        answers,
        ..fresh("solo")
    };
    // a successor that drops an earlier answer is refused, with nothing written
    let end = s.log_end();
    let dropped = next(first, vec![]);
    match s.put_plan(&dropped) {
        Err(StoreError::Refused(m)) => {
            assert_eq!(m, format!("plan {}: invalid chain", dropped.id()))
        }
        other => panic!("a dropped answer: {other:?}"),
    }
    let other_item = next(first, vec![defer_answer(b)]);
    assert!(matches!(
        s.put_plan(&other_item),
        Err(StoreError::Refused(m)) if m == format!("plan {}: invalid chain", other_item.id())
    ));
    assert_eq!(s.log_end(), end);
    assert_eq!(plan_ref_of(&s, "solo"), Some(first));
    // a successor may replace an item's answer and add new items
    let replaced = PlanAnswer {
        act: PlanAct::Confirm,
        reason: Some("it holds after all".into()),
        ..defer_answer(a)
    };
    let second = s.put_plan(&next(first, vec![replaced.clone()])).unwrap();
    let third = s
        .put_plan(&next(second, vec![replaced, defer_answer(b)]))
        .unwrap();
    assert_eq!(plan_ref_of(&s, "solo"), Some(third));
    s.check_objects().unwrap();
    // fsck reads a successor that drops an answer as damage
    let forged = next(third, vec![defer_answer(b)]);
    s.write_guarded_for_tests(plan_frames(&forged)).unwrap();
    assert_damage(&s, &forged.id(), "invalid chain");
}

/// The closure of `tag` in `s`, as records by key and in order.
fn closure_records(
    s: &Store,
    tag: Hash,
) -> (
    BTreeMap<ObjectKey, rdy_store::sync_stream::StreamRecord>,
    Vec<ObjectKey>,
) {
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    let mut records = BTreeMap::new();
    for k in &c.records {
        records.insert(*k, source.record(*k).unwrap().unwrap());
    }
    (records, c.records)
}

/// A stream of the built tag's closure that `edit` changes, written to `name`: a batch check
/// of the file alone passes.
fn edited_upload(
    b: &Built,
    tag: Hash,
    name: &str,
    edit: impl FnOnce(
        &mut BTreeMap<ObjectKey, rdy_store::sync_stream::StreamRecord>,
        &mut Vec<ObjectKey>,
    ),
) -> PathBuf {
    let (mut records, mut order) = closure_records(&b.s, tag);
    edit(&mut records, &mut order);
    let header = StreamHeader::tag_history(b.s.repo_id().unwrap().unwrap(), tag, vec![]);
    let path = b.dir.path().join(name);
    write_batch(&mut records, &header, &order, File::create(&path).unwrap()).unwrap();
    check_batch(File::open(&path).unwrap()).unwrap();
    path
}

fn present<T: serde::Serialize>(
    kind: u8,
    v: &T,
) -> (ObjectKey, rdy_store::sync_stream::StreamRecord) {
    let payload = enc(v);
    let k = key(kind, Hash::of(&payload));
    (
        k,
        rdy_store::sync_stream::StreamRecord::Present { key: k, payload },
    )
}

/// Adds the record of `kind` holding `v` at the end of the upload; returns its hash.
fn add_record<T: serde::Serialize>(
    records: &mut BTreeMap<ObjectKey, rdy_store::sync_stream::StreamRecord>,
    order: &mut Vec<ObjectKey>,
    kind: u8,
    v: &T,
) -> Hash {
    let (k, r) = present(kind, v);
    records.insert(k, r);
    order.push(k);
    k.hash
}

/// Puts the record of PLAN `p` in the place of the upload's seal `old`.
fn replace_seal(
    records: &mut BTreeMap<ObjectKey, rdy_store::sync_stream::StreamRecord>,
    order: &mut [ObjectKey],
    old: Hash,
    p: &Plan,
) {
    let old = key(kinds::PLAN, old);
    records.remove(&old);
    let (k, r) = present(kinds::PLAN, p);
    records.insert(k, r);
    let at = order.iter().position(|x| *x == old).unwrap();
    order[at] = k;
}

/// What a holder holds: its log end, its refs and its derived tables.
fn holder_state(
    h: &Store,
) -> (
    Location,
    Vec<(String, Vec<u8>)>,
    rdy_store::index::IndexDump,
) {
    (
        h.log_end(),
        h.refs_with_prefix("").unwrap(),
        h.index_dump_for_tests().unwrap(),
    )
}

/// The upload at `path` is refused, before anything is written, with E_BAD_RECORD ending
/// `rule`: by a take into `h`, and by a checked stream with nothing held when `alone`.
fn refused_upload(h: &mut Store, b: &Built, path: &std::path::Path, rule: &str, alone: bool) {
    let before = holder_state(h);
    let target = UploadTarget {
        tag: "v1".into(),
        landing: b.landing,
    };
    match h.take_batches(&[File::open(path).unwrap()], &target, incoming) {
        Err(rdy_store::take_batch::TakeError::Stream(e)) => {
            assert_eq!(e.code, "E_BAD_RECORD", "{}", e.message);
            assert!(e.message.ends_with(rule), "{}: {rule}", e.message);
        }
        Err(e) => panic!("{rule}: {e}"),
        Ok(_) => panic!("{rule}: the take wrote the upload"),
    }
    assert_eq!(holder_state(h), before, "{rule}: the holder is unchanged");
    h.check_holder().unwrap();
    if alone {
        match rdy_store::stream_source::CheckedStream::open(File::open(path).unwrap()) {
            Err(e) => assert!(e.message.ends_with(rule), "{}: {rule}", e.message),
            Ok(_) => panic!("{rule}: the checked stream passed"),
        }
    }
}

fn new_holder(b: &Built, name: &str) -> Store {
    Store::create_holder(&b.dir.path().join(name), b.s.repo_id().unwrap().unwrap()).unwrap()
}

#[test]
fn cp3_holder_take_refuses_conflicting_or_broken_seals() {
    let (b, tag) = tagged();
    let seal = b.s.plan(&b.seal).unwrap();
    // a second seal of the landing in the same upload
    let other = Plan {
        provenance: prov("human:bob", Role::Approver),
        ..seal.clone()
    };
    let path = edited_upload(&b, tag, "two-seals.rdysync", |r, o| {
        add_record(r, o, kinds::PLAN, &other);
    });
    let later = b.seal.max(other.id());
    let mut h = new_holder(&b, "h-two");
    refused_upload(
        &mut h,
        &b,
        &path,
        &format!("plan {later}: duplicate landing seal"),
        true,
    );
    // the seal names a proposal version that is not its landing's
    let another = Proposal {
        title: "another version".into(),
        ..b.s.proposal(&b.version).unwrap()
    };
    let (pk, pr) = present(kinds::PROPOSAL, &another);
    let mismatch = Plan {
        proposal: Some(pk.hash),
        seal: Some(PlanSeal {
            version: pk.hash,
            ..seal.seal.clone().unwrap()
        }),
        ..seal.clone()
    };
    let path = edited_upload(&b, tag, "mismatch.rdysync", |r, o| {
        replace_seal(r, o, b.seal, &mismatch);
        let at = o.iter().position(|k| k.hash == mismatch.id()).unwrap();
        o.insert(at, pk);
        r.insert(pk, pr);
    });
    let rule = format!("plan {}: invalid seal", mismatch.id());
    refused_upload(&mut h, &b, &path, &rule, true);
    // a seal whose chain is not its predecessor's
    let unchained = Plan {
        stable: Some(b.answered),
        ..seal.clone()
    };
    let path = edited_upload(&b, tag, "unchained.rdysync", |r, o| {
        replace_seal(r, o, b.seal, &unchained)
    });
    let rule = format!("plan {}: invalid chain", unchained.id());
    refused_upload(&mut h, &b, &path, &rule, true);
    // (a seal that drops an answer of the approved version's binding: cp10's landing test; this
    // chain's binding has none, and a seal may leave out the answer-only version's, D-4JMPH)
    // the valid upload is taken, and taken again
    let valid = b.dir.path().join("valid.rdysync");
    export(&b.s, tag, END_OF_LOG, &valid);
    let target = UploadTarget {
        tag: "v1".into(),
        landing: b.landing,
    };
    h.take_batches(&[File::open(&valid).unwrap()], &target, incoming)
        .unwrap();
    let end = h.log_end();
    h.take_batches(&[File::open(&valid).unwrap()], &target, incoming)
        .unwrap();
    assert_eq!(h.log_end(), end);
    // a later upload with another seal of the held landing conflicts with the holder's seal
    let path = edited_upload(&b, tag, "conflict.rdysync", |r, o| {
        replace_seal(r, o, b.seal, &other)
    });
    let rule = format!("plan {}: duplicate landing seal", other.id());
    refused_upload(&mut h, &b, &path, &rule, false);
    assert_eq!(h.plan_of_landing(&b.landing, end).unwrap(), Some(b.seal));
}

#[test]
fn cp3_holder_take_refuses_a_keep_suspect_of_no_block() {
    let (b, tag) = tagged();
    // a grammatically valid KeepSuspect that targets the closure's landing, not a BLOCK version
    let keep = answer(
        &reaffirm_item(),
        AnswerAct::KeepSuspect,
        vec![b.landing],
        Some("the claim still holds"),
    );
    let mut k = Hash([0; 32]);
    let path = edited_upload(&b, tag, "keep.rdysync", |r, o| {
        k = add_record(r, o, kinds::ANSWER, &keep);
    });
    let mut h = new_holder(&b, "h-keep");
    let rule = format!("answer {k}: invalid plan answer targets");
    refused_upload(&mut h, &b, &path, &rule, true);
    // the store's own writer refuses it too
    let mut s = b.s;
    assert!(matches!(
        s.put_answer(&keep),
        Err(StoreError::Refused(m)) if m == rule
    ));
}

// ---- cp10: the landing's plans and the first binding after an unbound version ----

/// A store whose view `work` holds one created block (`b-00001`) and its proposal `P`, bound in
/// its own batch to `B`, which defers item 1; then an answer-only version `A` that replaces the
/// defer and defers item 2: (store, P, B, A).
fn answered_after_propose() -> (tempfile::TempDir, Store, Hash, Plan, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "x"),
            }],
        ),
    )
    .unwrap();
    let (one, two) = (Hash([1; 32]), Hash([2; 32]));
    s.set_held(true).unwrap();
    let p = s.propose("work", "first", writer()).unwrap();
    let b = Plan {
        proposal: Some(p),
        answers: vec![defer_answer(one)],
        ..fresh("work")
    };
    s.write_guarded_for_tests(plan_frames(&b)).unwrap();
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    let replaced = PlanAnswer {
        act: PlanAct::Confirm,
        reason: Some("it holds after all".into()),
        ..defer_answer(one)
    };
    let a = s
        .put_plan(&Plan {
            stable: Some(b.id()),
            prev: Some(b.id()),
            answers: vec![replaced, defer_answer(two)],
            ..fresh("work")
        })
        .unwrap();
    (dir, s, p, b, a)
}

/// The seal of `landing`, which lands `version`, after `prev` in the chain first `stable`, with
/// `answers`.
fn seal_after(
    stable: Hash,
    prev: Hash,
    landing: Hash,
    version: Hash,
    answers: Vec<PlanAnswer>,
) -> Plan {
    Plan {
        stable: Some(stable),
        prev: Some(prev),
        proposal: Some(version),
        answers,
        provenance: human(),
        seal: Some(PlanSeal {
            landing,
            version,
            decisions: vec![],
            items: vec![],
        }),
        ..fresh("work")
    }
}

fn land_request(p: Hash, accept: Option<Change>) -> rdy_store::LandRequest {
    rdy_store::LandRequest {
        proposal: p,
        accept,
        approver: human(),
        alt: None,
        relay: None,
        evidence: None,
    }
}

/// D-4JMPH, p-8D6ES: a landing's plans follow the view's chain (after an answer-only version
/// written since the propose) and carry the approved version's bound answers, which may leave
/// out answers given after it: the seal of a version that lands as is, and the final version's
/// binding and seal of an acceptance. A landing plan that drops an answer of the approved
/// binding is still refused, and fsck and a holder's take accept what the writer wrote.
#[test]
fn cp10_landing_plans_carry_the_approved_binding_after_later_answers() {
    // the approved version lands as is
    let (_d, mut s, p, b, a) = answered_after_propose();
    let mut prepared = s.prepare_land(land_request(p, None)).unwrap();
    let (l, v) = (prepared.hash(), prepared.landed_version());
    assert_eq!(v, p);
    let dropped = seal_after(b.id(), a, l, v, vec![]);
    match s.plan_landing(&mut prepared, None, Some(&dropped)) {
        Err(StoreError::Refused(m)) => {
            assert_eq!(m, format!("plan {}: invalid chain", dropped.id()))
        }
        other => panic!("a seal that drops a bound answer: {other:?}"),
    }
    let mut prepared = s.prepare_land(land_request(p, None)).unwrap();
    let sealed = seal_after(b.id(), a, l, v, b.answers.clone());
    s.plan_landing(&mut prepared, None, Some(&sealed)).unwrap();
    s.commit_land(prepared, &[]).unwrap();
    assert_eq!(plan_ref_of(&s, "work"), Some(sealed.id()));
    s.check_objects().unwrap();
    // an unsealed answer-only version still keeps every answer of its predecessor
    let other = Plan {
        stable: None,
        prev: None,
        answers: vec![defer_answer(Hash([3; 32]))],
        ..fresh("other")
    };
    let first = s.put_plan(&other).unwrap();
    let forged = Plan {
        stable: Some(first),
        prev: Some(first),
        answers: vec![],
        ..fresh("other")
    };
    assert!(matches!(
        s.put_plan(&forged),
        Err(StoreError::Refused(m)) if m == format!("plan {}: invalid chain", forged.id())
    ));

    // an acceptance: the final version binds the approved answers, after the answer-only one
    let (_d, mut s, p, b, a) = answered_after_propose();
    let accept = || {
        change(
            "work",
            vec![Op::Create {
                block: block("b-00002", "accepted"),
            }],
        )
    };
    let mut prepared = s.prepare_land(land_request(p, Some(accept()))).unwrap();
    let (l, fin) = (prepared.hash(), prepared.landed_version());
    assert!(prepared.writes_final_version());
    assert_ne!(fin, p);
    let bound_final = |answers: Vec<PlanAnswer>| Plan {
        stable: Some(b.id()),
        prev: Some(a),
        proposal: Some(fin),
        answers,
        provenance: human(),
        ..fresh("work")
    };
    let dropped = bound_final(vec![]);
    match s.plan_landing(&mut prepared, Some(&dropped), None) {
        Err(StoreError::Refused(m)) => {
            assert_eq!(m, format!("plan {}: invalid chain", dropped.id()))
        }
        other => panic!("a final binding that drops a bound answer: {other:?}"),
    }
    let mut prepared = s.prepare_land(land_request(p, Some(accept()))).unwrap();
    assert_eq!(prepared.hash(), l);
    let fplan = bound_final(b.answers.clone());
    let sealed = seal_after(b.id(), fplan.id(), l, fin, b.answers.clone());
    s.plan_landing(&mut prepared, Some(&fplan), Some(&sealed))
        .unwrap();
    s.commit_land(prepared, &[]).unwrap();
    assert_eq!(plan_ref_of(&s, "work"), Some(sealed.id()));
    let end = s.log_end();
    assert_eq!(s.plan_of_proposal(&fin, end).unwrap(), Some(fplan.id()));
    assert_eq!(s.plan_of_landing(&l, end).unwrap(), Some(sealed.id()));
    s.check_objects().unwrap();
    // a holder takes the landing's closure, its chain and both plans
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: l,
            message: None,
            tagged_by: human(),
        })
        .unwrap();
    let path = _d.path().join("v1.rdysync");
    let records = export(&s, tag, END_OF_LOG, &path);
    for h in [a, fplan.id(), sealed.id()] {
        assert!(records.contains(&key(kinds::PLAN, h)), "{h} in the closure");
    }
    let mut h = Store::create_holder(&_d.path().join("h"), s.repo_id().unwrap().unwrap()).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing: l,
    };
    // an upload whose seal drops the approved binding's answer is refused, before anything is
    // written, by the take and by a checked stream
    let forgetful = seal_after(b.id(), fplan.id(), l, fin, vec![]);
    let (mut records, mut order) = closure_records(&s, tag);
    replace_seal(&mut records, &mut order, sealed.id(), &forgetful);
    let bad = _d.path().join("forgetful.rdysync");
    let header = StreamHeader::tag_history(s.repo_id().unwrap().unwrap(), tag, vec![]);
    write_batch(&mut records, &header, &order, File::create(&bad).unwrap()).unwrap();
    let rule = format!("plan {}: invalid chain", forgetful.id());
    let before = holder_state(&h);
    match h.take_batches(&[File::open(&bad).unwrap()], &target, incoming) {
        Err(rdy_store::take_batch::TakeError::Stream(e)) => {
            assert_eq!(e.code, "E_BAD_RECORD", "{}", e.message);
            assert!(e.message.ends_with(&rule), "{}: {rule}", e.message);
        }
        Err(e) => panic!("{rule}: {e}"),
        Ok(_) => panic!("{rule}: the take wrote the upload"),
    }
    assert_eq!(holder_state(&h), before);
    match rdy_store::stream_source::CheckedStream::open(File::open(&bad).unwrap()) {
        Err(e) => assert!(e.message.ends_with(&rule), "{}: {rule}", e.message),
        Ok(_) => panic!("{rule}: the checked stream passed"),
    }
    // the written upload is taken
    h.take_batches(&[File::open(&path).unwrap()], &target, incoming)
        .unwrap();
    let end = h.log_end();
    assert_eq!(h.plan_of_landing(&l, end).unwrap(), Some(sealed.id()));
    assert_eq!(h.plan_of_proposal(&fin, end).unwrap(), Some(fplan.id()));
    h.check_holder().unwrap();
}

/// O19, p-AEYTC: the binding of a version that follows an unbound one starts a chain, also
/// where an answer-only chain stands in the view; a binding that continues that chain is
/// refused, and fsck agrees.
#[test]
fn cp10_binding_after_an_unbound_version_starts_a_chain() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "x"),
            }],
        ),
    )
    .unwrap();
    // the store-16 version, then an answer-only chain in the view
    let old = s.propose("work", "first", writer()).unwrap();
    let chain = s
        .put_plan(&Plan {
            answers: vec![defer_answer(Hash([1; 32]))],
            ..fresh("work")
        })
        .unwrap();
    let changes = s.included("work").unwrap().as_ref().clone();
    let request = || rdy_store::plan::ProposeRequest {
        view: "work",
        title: "first",
        summary: None,
        by: writer(),
        changes: changes.clone(),
        alternative: None,
        prev: Some(old),
        rekey: None,
        answer: None,
    };
    // continuing the answer-only chain is refused, with nothing written
    let end = s.log_end();
    let mut prepared = s.prepare_propose(request()).unwrap();
    let v = prepared.version();
    let continued = Plan {
        stable: Some(chain),
        prev: Some(chain),
        proposal: Some(v),
        answers: vec![defer_answer(Hash([1; 32]))],
        ..fresh("work")
    };
    match s.bind_plan(&mut prepared, &continued) {
        Err(StoreError::Refused(m)) => {
            assert_eq!(m, format!("plan {}: invalid chain", continued.id()))
        }
        other => panic!("a binding that continues the chain: {other:?}"),
    }
    assert_eq!(s.log_end(), end);
    // the first binding: no earlier chain version, no inherited answer
    let mut prepared = s.prepare_propose(request()).unwrap();
    let first = Plan {
        proposal: Some(v),
        ..fresh("work")
    };
    s.bind_plan(&mut prepared, &first).unwrap();
    s.commit_propose(prepared).unwrap();
    assert_eq!(plan_ref_of(&s, "work"), Some(first.id()));
    assert_eq!(s.plan_of_proposal(&old, s.log_end()).unwrap(), None);
    s.check_objects().unwrap();
    // fsck reads a continuing binding as damage
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "x"),
            }],
        ),
    )
    .unwrap();
    let old = s.propose("work", "first", writer()).unwrap();
    let chain = s.put_plan(&fresh("work")).unwrap();
    s.set_held(true).unwrap();
    let v = s.propose_version(&old, &[]).unwrap();
    let forged = Plan {
        stable: Some(chain),
        prev: Some(chain),
        proposal: Some(v),
        ..fresh("work")
    };
    s.write_guarded_for_tests(plan_frames(&forged)).unwrap();
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    assert_damage(&s, &forged.id(), "invalid chain");
}
