//! Plan versions stored as answer deltas (D-3PNRK, D-4JMPH, D-WWAK7): a version after its
//! predecessor stores only how its answers differ, folds back to the same logical plan, keeps
//! its prepared frame's hash as its id, and reads within fixed bounds. Also the one
//! proposal-to-landing map of a checked batch.
use rdy_model::encoding::{PLAN_DELTA_ENCODING, encoded_hash};
use rdy_model::plan::{
    MAX_PLAN_ANSWERS, MAX_PLAN_BYTES, Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome,
    PlanSeal, SealedItem, item_hash,
};
use rdy_model::plan_encoding::{
    MAX_PLAN_DELTA_DEPTH, PlanDeltaV1, PlanRecordV1, PlanV0, delta_body, full_body, v0_body,
};
use rdy_model::*;
use rdy_store::codec::{self, VERSIONED};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::object_refs::{ObjectRef, refs_of};
use rdy_store::plan::{
    INVALID_CHAIN, INVALID_FIELDS, batch_landing_cap_for_tests, plan_ref,
    take_landing_decodes_for_tests,
};
use rdy_store::plan_record::{self, FoldError, PlanRecord};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};

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

fn block(i: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(i).unwrap(),
        block_type: "claim".into(),
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
        provenance: writer(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn heads() -> Vec<PlanHead> {
    vec![PlanHead {
        repo: "app".into(),
        head: Some("a".repeat(40)),
    }]
}

/// An answer to item `n` with `act` and `reason`.
fn ans(n: u8, act: PlanAct, reason: &str) -> PlanAnswer {
    PlanAnswer {
        item: Hash([n; 32]),
        act,
        reason: (act != PlanAct::Done).then(|| reason.to_string()),
        subject: (act != PlanAct::Done).then_some(Hash([0x51; 32])),
        cause: Hash([0x52; 32]),
        by: writer(),
    }
}

/// A plan version of `view` with `answers` (sorted by item here) after `stable`/`prev`.
fn version(view: &str, stable: Option<Hash>, prev: Option<Hash>, answers: &[PlanAnswer]) -> Plan {
    let mut answers = answers.to_vec();
    answers.sort_by_key(|a| a.item);
    Plan {
        stable,
        prev,
        view: view.into(),
        proposal: None,
        answers,
        heads: heads(),
        provenance: writer(),
        seal: None,
        read_identity: None,
    }
}

/// A store with one block in view `work`.
fn store() -> (tempfile::TempDir, Store) {
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
    (dir, s)
}

/// The stored record `h`, decoded from its raw frame.
fn record(s: &Store, h: &Hash) -> PlanRecord {
    let (version, body) = s.get_versioned_body(kinds::PLAN, h).unwrap();
    let raw = if version == 0 {
        kinds::PLAN
    } else {
        kinds::PLAN | VERSIONED
    };
    let payload = if version == 0 {
        body
    } else {
        [&version.to_le_bytes()[..], &body].concat()
    };
    let (r, id) = plan_record::decode(raw, &payload).unwrap();
    assert_eq!(id.hash, *h, "a stored id is its frame's hash");
    r
}

fn plan_ref_of(s: &Store, view: &str) -> Option<Hash> {
    s.get_ref(&plan_ref(view))
        .unwrap()
        .map(|v| Hash(v.try_into().unwrap()))
}

/// The answer-only chain of `view` from nothing through each answer list of `steps`, written
/// by the typed writer: the ids and the drafts.
fn chain(s: &mut Store, view: &str, steps: &[Vec<PlanAnswer>]) -> Vec<(Hash, Plan)> {
    let mut out: Vec<(Hash, Plan)> = vec![];
    for answers in steps {
        let (stable, prev) = match (out.first(), out.last()) {
            (Some((f, _)), Some((l, _))) => (Some(*f), Some(*l)),
            _ => (None, None),
        };
        let p = version(view, stable, prev, answers);
        let want = s.select_plan_frame(&p, &[]).unwrap().hash();
        let id = s.put_plan(&p).unwrap();
        assert_eq!(id, want, "the writer stores the frame it prepared");
        assert_eq!(plan_ref_of(s, view), Some(id));
        out.push((id, p));
    }
    out
}

/// The logical fields of `p`, without its read identity.
fn logical(p: &Plan) -> Plan {
    Plan {
        read_identity: None,
        ..p.clone()
    }
}

/// The landing, after an answer-only chain, of a proposal that bound answer 1 only: the seal
/// keeps the approved binding's answers and so removes the answers recorded after it. Also
/// the plan folds the landing composition made.
fn seal_after_later_answers(later: usize) -> (tempfile::TempDir, Store, Hash, Plan, u64) {
    let (d, mut s) = store();
    s.set_held(true).unwrap();
    let p = s.propose("work", "first", writer()).unwrap();
    let mut bound = version("work", None, None, &[ans(1, PlanAct::Defer, "one")]);
    bound.proposal = Some(p);
    s.prepare_propose_for_held(p, &bound);
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    let b = plan_ref_of(&s, "work").unwrap();
    let mut prev = b;
    let mut answers = bound.answers.clone();
    for n in 0..later {
        answers.push(ans(10 + n as u8, PlanAct::Confirm, "later"));
        prev = s
            .put_plan(&version("work", Some(b), Some(prev), &answers))
            .unwrap();
    }
    let mut prepared = s
        .prepare_land(rdy_store::LandRequest {
            proposal: p,
            accept: None,
            approver: human(),
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    let seal = Plan {
        stable: Some(b),
        prev: Some(prev),
        proposal: Some(prepared.landed_version()),
        provenance: human(),
        seal: Some(PlanSeal {
            landing: prepared.hash(),
            version: prepared.landed_version(),
            decisions: vec![],
            items: vec![],
        }),
        ..bound.clone()
    };
    let id = s.prepare_landing_plan_id(&prepared, &seal).unwrap();
    plan_record::reset_counts_for_tests();
    s.plan_landing(&mut prepared, None, Some(&seal)).unwrap();
    let folds = plan_record::counts_for_tests().folds;
    s.commit_land(prepared, &[]).unwrap();
    assert_eq!(plan_ref_of(&s, "work"), Some(id));
    (d, s, id, seal, folds)
}

/// A proposal's bound plan, written beside it as the propose composition does.
trait BindHeld {
    fn prepare_propose_for_held(&mut self, p: Hash, bound: &Plan) -> Hash;
}

impl BindHeld for Store {
    fn prepare_propose_for_held(&mut self, _p: Hash, bound: &Plan) -> Hash {
        let frame = self.select_plan_frame(bound, &[]).unwrap();
        let id = frame.hash();
        let r = RefUpdate {
            name: plan_ref(&bound.view),
            value: id.0.to_vec(),
            at: Location {
                segment: 0,
                offset: 0,
            },
        };
        self.write_guarded_for_tests(vec![
            frame.into_item(),
            (kinds::REF_UPDATE, postcard::to_allocvec(&r).unwrap()),
        ])
        .unwrap();
        id
    }
}

#[test]
fn r5906_delta_folds_to_the_same_plan() {
    let (_d, mut s) = store();
    // all four acts, a replacement, unchanged answers and a new one, version by version
    let steps = vec![
        vec![ans(1, PlanAct::Defer, "the release owns it")],
        vec![
            ans(1, PlanAct::Defer, "the release owns it"),
            ans(2, PlanAct::Confirm, "it holds"),
        ],
        vec![
            ans(1, PlanAct::Confirm, "it holds after all"),
            ans(2, PlanAct::Confirm, "it holds"),
            ans(3, PlanAct::Done, ""),
        ],
        vec![
            ans(1, PlanAct::Confirm, "it holds after all"),
            ans(2, PlanAct::Confirm, "it holds"),
            ans(3, PlanAct::Done, ""),
            ans(4, PlanAct::Dismiss, "settled elsewhere"),
        ],
    ];
    let written = chain(&mut s, "work", &steps);
    assert!(record(&s, &written[0].0).is_full(), "a chain starts whole");
    for (n, (id, draft)) in written.iter().enumerate() {
        let read = s.plan(id).unwrap();
        assert_eq!(read, *draft, "version {n} folds to its logical plan");
        assert_eq!(read.answers, draft.answers, "the same answer order");
        assert_eq!(read.id(), *id, "a read keeps its stored id");
        assert_eq!(read.read_identity.unwrap().version, PLAN_DELTA_ENCODING);
    }
    // the later versions are deltas that hold only what changed
    for (n, want_set) in [(1, vec![2u8]), (2, vec![1, 3]), (3, vec![4])] {
        match record(&s, &written[n].0) {
            PlanRecord::Delta(d) => {
                let set: Vec<u8> = d.answers_set.iter().map(|a| a.item.0[0]).collect();
                assert_eq!(set, want_set, "version {n}");
                assert!(d.answers_remove.is_empty());
                assert_eq!(d.prev, written[n - 1].0);
                assert_eq!(d.stable, written[0].0);
            }
            PlanRecord::Full(_) => panic!("version {n} is a delta"),
        }
    }
    // a hydrated delta is never re-encoded whole under its id
    let latest = s.plan(&written[3].0).unwrap();
    match codec::encode(kinds::PLAN, &latest) {
        Err(StoreError::Refused(m)) => assert_eq!(m, codec::IDENTITY_MISMATCH),
        other => panic!("a hydrated delta re-encoded: {other:?}"),
    }
    // a hydrated whole version re-encodes to its own bytes
    let first = s.plan(&written[0].0).unwrap();
    assert_eq!(
        codec::encode(kinds::PLAN, &first).unwrap().hash(),
        written[0].0
    );
    // the draft id is the whole frame's hash, never the delta's
    assert_ne!(written[3].1.id(), written[3].0);
    assert_eq!(
        written[3].1.id(),
        encoded_hash(PLAN_DELTA_ENCODING, &full_body(&written[3].1))
    );
    s.check_objects().unwrap();

    // the approved-binding exception: a seal keeps the binding's answers, which removes the
    // answers recorded after it, and still folds to what it carries
    let (_d2, s2, seal_id, seal, _) = seal_after_later_answers(3);
    match record(&s2, &seal_id) {
        PlanRecord::Delta(d) => {
            let removed: Vec<u8> = d.answers_remove.iter().map(|h| h.0[0]).collect();
            assert_eq!(removed, [10, 11, 12]);
            assert!(d.answers_set.is_empty());
        }
        PlanRecord::Full(_) => panic!("the seal is a delta"),
    }
    assert_eq!(logical(&s2.plan(&seal_id).unwrap()), logical(&seal));
    s2.check_objects().unwrap();
    // a normal answer-only removal is still refused
    let (_d3, mut s3) = store();
    let w = chain(
        &mut s3,
        "work",
        &[vec![
            ans(1, PlanAct::Defer, "one"),
            ans(2, PlanAct::Defer, "two"),
        ]],
    );
    let dropped = version(
        "work",
        Some(w[0].0),
        Some(w[0].0),
        &[ans(1, PlanAct::Defer, "one")],
    );
    let want = s3.select_plan_frame(&dropped, &[]).unwrap().hash();
    match s3.put_plan(&dropped) {
        Err(StoreError::Refused(m)) => assert_eq!(m, format!("plan {want}: {INVALID_CHAIN}")),
        other => panic!("a dropped answer: {other:?}"),
    }

    // dedup: the whole checkpoint after 128 deltas repeats the answers its predecessor delta
    // set; their bytes map onto the delta's, and the logical plans and ids survive the punch
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup");
    let mut s = Store::create(&path).unwrap();
    let long = |n: u8| -> String {
        (0..2000u32)
            .map(|i| char::from(b'a' + ((i * 7919 + u32::from(n) * 31) % 26) as u8))
            .collect()
    };
    let mut steps: Vec<Vec<PlanAnswer>> = vec![vec![ans(1, PlanAct::Defer, "one")]];
    for n in 2..=MAX_PLAN_DELTA_DEPTH as u8 {
        let mut next = steps.last().unwrap().clone();
        next.push(ans(n, PlanAct::Defer, "small"));
        steps.push(next);
    }
    let mut big = steps.last().unwrap().clone();
    big.extend((0xF0..0xF5u8).map(|n| ans(n, PlanAct::Defer, &long(n))));
    steps.push(big.clone());
    big.push(ans(0xC8, PlanAct::Done, ""));
    steps.push(big);
    let written = chain(&mut s, "work", &steps);
    let last = written.len() - 1;
    assert!(!record(&s, &written[last - 1].0).is_full());
    assert!(record(&s, &written[last].0).is_full(), "the checkpoint");
    let before: Vec<Plan> = written.iter().map(|(h, _)| s.plan(h).unwrap()).collect();
    let r = s.compact(false).unwrap();
    assert!(r.deduplicated_bytes >= 4096, "{}", r.deduplicated_bytes);
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    for ((h, draft), was) in written.iter().zip(&before) {
        let now = s.plan(h).unwrap();
        assert_eq!(now, *was);
        assert_eq!(now, *draft);
        assert_eq!(now.id(), *h);
    }
    s.check_objects().unwrap();
}

#[test]
fn r5906_full_checkpoint_and_size_choice() {
    let (_d, mut s) = store();
    // a whole version, then exactly 128 deltas; the next version is whole again
    let mut answers = vec![ans(0, PlanAct::Defer, "zero")];
    let first = s.put_plan(&version("work", None, None, &answers)).unwrap();
    let mut prev = first;
    for n in 1..=MAX_PLAN_DELTA_DEPTH {
        answers.push(ans(n as u8, PlanAct::Confirm, "ok"));
        prev = s
            .put_plan(&version("work", Some(first), Some(prev), &answers))
            .unwrap();
        assert!(!record(&s, &prev).is_full(), "version {n} is a delta");
        assert_eq!(s.plan_delta_depth_for_tests(&prev).unwrap(), Some(n));
    }
    answers.push(ans(200, PlanAct::Confirm, "ok"));
    let next = s
        .put_plan(&version("work", Some(first), Some(prev), &answers))
        .unwrap();
    assert!(record(&s, &next).is_full(), "the 129th is whole");
    assert_eq!(s.plan_delta_depth_for_tests(&next).unwrap(), Some(0));
    // the whole checkpoint keeps its prev: the history is not cut
    assert_eq!(record(&s, &next).prev(), Some(prev));
    answers.push(ans(201, PlanAct::Confirm, "ok"));
    let after = s
        .put_plan(&version("work", Some(first), Some(next), &answers))
        .unwrap();
    assert!(
        !record(&s, &after).is_full(),
        "a delta follows the checkpoint"
    );
    assert_eq!(s.plan(&after).unwrap().answers.len(), answers.len());
    s.check_objects().unwrap();

    // the size rule, on the frames the writer would choose (unchecked): a smaller delta is
    // chosen; an equal or larger one is not
    let (_d, mut s) = store();
    let base = s
        .put_plan(&version(
            "work",
            None,
            None,
            &[ans(1, PlanAct::Defer, "one")],
        ))
        .unwrap();
    let base_answers = s.plan(&base).unwrap().answers;
    let sizes = |p: &Plan| {
        let d = PlanDeltaV1::between(base, base, &base_answers, p);
        (delta_body(&d).len(), full_body(p).len())
    };
    let smaller = version(
        "work",
        Some(base),
        Some(base),
        &[ans(1, PlanAct::Confirm, "c")],
    );
    let (dl, fl) = sizes(&smaller);
    assert!(dl < fl);
    assert_eq!(
        s.select_plan_frame(&smaller, &[]).unwrap().version,
        PLAN_DELTA_ENCODING
    );
    assert!(!is_full(
        &s.select_plan_frame(&smaller, &[]).unwrap().payload
    ));
    // a delta that replaces every answer under other items is larger
    let larger = version(
        "work",
        Some(base),
        Some(base),
        &[ans(9, PlanAct::Defer, "nine")],
    );
    let (dl, fl) = sizes(&larger);
    assert!(dl > fl);
    assert!(is_full(&s.select_plan_frame(&larger, &[]).unwrap().payload));
    // an equal size: the removed items' hashes cost what the unchanged answer saves; the
    // number removed and the actor's length tune the sizes until both frames are the same length
    let mut equal = None;
    'search: for removed in 1..8u8 {
        for len in 1..200 {
            let by = prov(&"x".repeat(len), Role::Writer);
            let keep = |n: u8| PlanAnswer {
                by: by.clone(),
                ..ans(n, PlanAct::Defer, "k")
            };
            let mut base_set: Vec<PlanAnswer> = (0..removed).map(|n| keep(20 + n)).collect();
            base_set.push(keep(40));
            let next_set: Vec<PlanAnswer> = vec![keep(40)];
            let p = version("work", Some(Hash([1; 32])), Some(Hash([2; 32])), &next_set);
            let pd = PlanDeltaV1::between(Hash([1; 32]), Hash([2; 32]), &base_set, &p);
            if delta_body(&pd).len() == full_body(&p).len() {
                equal = Some((base_set, next_set));
                break 'search;
            }
        }
    }
    let (base_set, next_set) = equal.expect("an equal-size pair");
    let (_d, mut s) = store();
    let b = s.put_plan(&version("work", None, None, &base_set)).unwrap();
    let p = version("work", Some(b), Some(b), &next_set);
    let d = PlanDeltaV1::between(b, b, &s.plan(&b).unwrap().answers, &p);
    assert_eq!(delta_body(&d).len(), full_body(&p).len(), "equal frames");
    assert!(
        is_full(&s.select_plan_frame(&p, &[]).unwrap().payload),
        "an equal-size delta is not chosen"
    );
}

/// Whether the encoding-1 PLAN payload is a whole version.
fn is_full(payload: &[u8]) -> bool {
    assert_eq!(&payload[..2], &PLAN_DELTA_ENCODING.to_le_bytes());
    payload[2] == 0
}

#[test]
fn r5906_bound_and_sealed_ids_are_prepared_ids() {
    // an acceptance: the proposal's binding, then the final version's binding and the seal in
    // the landing's batch
    let (_d, mut s) = store();
    s.set_held(true).unwrap();
    let p = s.propose("work", "first", writer()).unwrap();
    let mut bound = version("work", None, None, &[ans(1, PlanAct::Defer, "one")]);
    bound.proposal = Some(p);
    let b = s.prepare_propose_for_held(p, &bound);
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    assert_eq!(s.plan_of_proposal(&p, s.log_end()).unwrap(), Some(b));
    let a = s
        .put_plan(&version(
            "work",
            Some(b),
            Some(b),
            &[
                ans(1, PlanAct::Defer, "one"),
                ans(2, PlanAct::Confirm, "two"),
            ],
        ))
        .unwrap();
    let request = || rdy_store::LandRequest {
        proposal: p,
        accept: Some(change(
            "work",
            vec![Op::Create {
                block: block("b-00002", "accepted"),
            }],
        )),
        approver: human(),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    };
    let mut prepared = s.prepare_land(request()).unwrap();
    assert!(prepared.writes_final_version());
    let (l, fin) = (prepared.hash(), prepared.landed_version());
    let fplan = Plan {
        stable: Some(b),
        prev: Some(a),
        proposal: Some(fin),
        provenance: human(),
        ..bound.clone()
    };
    let fid = s.prepare_landing_plan_id(&prepared, &fplan).unwrap();
    assert_ne!(fid, fplan.id(), "the final binding is stored as a delta");
    let seal = Plan {
        stable: Some(b),
        prev: Some(fid),
        seal: Some(PlanSeal {
            landing: l,
            version: fin,
            decisions: vec![],
            items: vec![],
        }),
        ..fplan.clone()
    };
    // a seal that names the draft id is refused, with nothing written and no ref moved
    let end = s.log_end();
    let wrong = Plan {
        prev: Some(fplan.id()),
        ..seal.clone()
    };
    assert!(
        s.plan_landing(&mut prepared, Some(&fplan), Some(&wrong))
            .is_err()
    );
    assert_eq!(s.log_end(), end);
    assert_eq!(plan_ref_of(&s, "work"), Some(a));
    let mut prepared = s.prepare_land(request()).unwrap();
    s.plan_landing(&mut prepared, Some(&fplan), Some(&seal))
        .unwrap();
    s.commit_land(prepared, &[]).unwrap();
    // one durable batch: the landing, both plans and the ref to the seal
    let batch = s.last_batch().to_vec();
    let at = |k: u8, h: Hash| {
        batch
            .iter()
            .position(|x| codec::logical_kind(x.0) == k && x.1 == h)
    };
    let (li, fi) = (at(kinds::LANDING, l), at(kinds::PLAN, fid));
    let sealed = plan_ref_of(&s, "work").unwrap();
    let si = at(kinds::PLAN, sealed);
    assert!(li.is_some() && fi.is_some() && si.is_some(), "{batch:?}");
    assert!(li < fi && fi < si);
    let end = s.log_end();
    assert_eq!(s.plan_of_proposal(&fin, end).unwrap(), Some(fid));
    assert_eq!(s.plan_of_landing(&l, end).unwrap(), Some(sealed));
    assert_eq!(
        s.plan(&sealed).unwrap().prev,
        Some(fid),
        "seal.prev is the stored id"
    );
    assert_eq!(record(&s, &sealed).prev(), Some(fid));
    s.check_objects().unwrap();
}

/// A reader over `frames` that counts its reads.
struct Frames {
    frames: HashMap<Hash, (u16, Vec<u8>)>,
    reads: Cell<usize>,
}

impl Frames {
    fn new() -> Self {
        Frames {
            frames: HashMap::new(),
            reads: Cell::new(0),
        }
    }

    fn add_full(&mut self, p: &Plan) -> Hash {
        let body = full_body(p);
        let h = encoded_hash(PLAN_DELTA_ENCODING, &body);
        self.frames.insert(h, (PLAN_DELTA_ENCODING, body));
        h
    }

    fn add_delta(&mut self, d: &PlanDeltaV1) -> Hash {
        let body = delta_body(d);
        let h = encoded_hash(PLAN_DELTA_ENCODING, &body);
        self.frames.insert(h, (PLAN_DELTA_ENCODING, body));
        h
    }

    fn fold(&self, h: &Hash) -> Result<Plan, FoldError> {
        self.reads.set(0);
        let read = |x: &Hash| -> Result<Option<(u16, Vec<u8>)>, StoreError> {
            self.reads.set(self.reads.get() + 1);
            Ok(self.frames.get(x).cloned())
        };
        plan_record::fold(h, &read)
    }
}

fn delta(stable: Hash, prev: Hash, set: Vec<PlanAnswer>, remove: Vec<Hash>) -> PlanDeltaV1 {
    PlanDeltaV1 {
        stable,
        prev,
        view: "work".into(),
        proposal: None,
        answers_set: set,
        answers_remove: remove,
        heads: heads(),
        provenance: writer(),
        seal: None,
    }
}

fn broken(r: Result<Plan, FoldError>, at: Hash, reason: &str) {
    match r {
        Err(FoldError::Broken { plan, reason: why }) => {
            assert_eq!((plan, why), (at, reason));
        }
        other => panic!("{at}: {reason}: {other:?}"),
    }
}

#[test]
fn r5906_hostile_delta_is_bounded() {
    let mut f = Frames::new();
    let base = version(
        "work",
        None,
        None,
        &[ans(1, PlanAct::Defer, "one"), ans(2, PlanAct::Defer, "two")],
    );
    let full = f.add_full(&base);
    // a sound delta folds
    let ok = f.add_delta(&delta(full, full, vec![ans(3, PlanAct::Done, "")], vec![]));
    assert_eq!(f.fold(&ok).unwrap().answers.len(), 3);
    // each list rule, and the chain rules, are invalid chain at the delta that breaks them
    let cases: Vec<(&str, PlanDeltaV1)> = vec![
        (
            "missing prev",
            delta(full, Hash([0xEE; 32]), vec![], vec![]),
        ),
        (
            "stable mismatch",
            delta(
                Hash([0xEF; 32]),
                full,
                vec![ans(3, PlanAct::Done, "")],
                vec![],
            ),
        ),
        (
            "redundant set",
            delta(full, full, vec![ans(1, PlanAct::Defer, "one")], vec![]),
        ),
        (
            "missing remove",
            delta(full, full, vec![], vec![Hash([9; 32])]),
        ),
        (
            "overlap",
            delta(
                full,
                full,
                vec![ans(1, PlanAct::Confirm, "c")],
                vec![Hash([1; 32])],
            ),
        ),
        (
            "unsorted set",
            delta(
                full,
                full,
                vec![ans(4, PlanAct::Done, ""), ans(3, PlanAct::Done, "")],
                vec![],
            ),
        ),
        (
            "duplicate remove",
            delta(full, full, vec![], vec![Hash([1; 32]), Hash([1; 32])]),
        ),
    ];
    for (name, d) in cases {
        let h = f.add_delta(&d);
        broken(f.fold(&h), h, INVALID_CHAIN);
        assert!(f.reads.get() <= 4, "{name}: {} reads", f.reads.get());
    }
    // a missing target is not found; a frame under another hash (no cycle can form with
    // content hashes) is invalid fields at that hash, without a loop
    assert!(matches!(
        f.fold(&Hash([0xAB; 32])),
        Err(FoldError::Store(StoreError::NotFound(_)))
    ));
    let mut liar = Frames::new();
    let real = liar.add_full(&base);
    let fake = Hash([0xCD; 32]);
    let body = delta_body(&delta(real, fake, vec![], vec![]));
    liar.frames.insert(fake, (PLAN_DELTA_ENCODING, body));
    broken(liar.fold(&fake), fake, INVALID_FIELDS);
    // depth: 128 deltas after a whole version fold; the 129th is invalid chain, read within
    // 258 records
    let mut f = Frames::new();
    let first = f.add_full(&version("work", None, None, &[]));
    let mut prev = first;
    let mut ids = vec![];
    for n in 0..=MAX_PLAN_DELTA_DEPTH {
        prev = f.add_delta(&delta(
            first,
            prev,
            vec![ans((n % 250) as u8 + 1, PlanAct::Defer, &format!("r{n}"))],
            vec![],
        ));
        ids.push(prev);
    }
    let at_128 = ids[MAX_PLAN_DELTA_DEPTH - 1];
    assert_eq!(f.fold(&at_128).unwrap().id(), at_128);
    assert!(
        f.reads.get() <= 2 * (MAX_PLAN_DELTA_DEPTH + 1),
        "{}",
        f.reads.get()
    );
    let at_129 = ids[MAX_PLAN_DELTA_DEPTH];
    broken(f.fold(&at_129), at_129, INVALID_CHAIN);
    assert!(f.reads.get() <= 2 * (MAX_PLAN_DELTA_DEPTH + 1));
    // the count bound: exactly MAX_PLAN_ANSWERS answers fold; one more is invalid fields
    let item = |i: usize| {
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&(i as u64).to_be_bytes());
        Hash(h)
    };
    // (one frame holds at most a fixed number of decoded values, so the answers arrive in
    // ten frames of 10,000)
    let done = |i: usize| PlanAnswer {
        item: item(i),
        ..ans(0, PlanAct::Done, "")
    };
    let chunk = MAX_PLAN_ANSWERS / 10;
    let mut f = Frames::new();
    let first: Vec<PlanAnswer> = (0..chunk).map(done).collect();
    let big = f.add_full(&version("work", None, None, &first));
    let mut at_cap = big;
    for k in 1..10 {
        at_cap = f.add_delta(&delta(
            big,
            at_cap,
            (k * chunk..(k + 1) * chunk).map(done).collect(),
            vec![],
        ));
    }
    assert_eq!(f.fold(&at_cap).unwrap().answers.len(), MAX_PLAN_ANSWERS);
    let past = f.add_delta(&delta(big, at_cap, vec![done(MAX_PLAN_ANSWERS)], vec![]));
    broken(f.fold(&past), past, INVALID_FIELDS);
    // a replacement at the cap keeps the count
    let replaced = PlanAnswer {
        act: PlanAct::Defer,
        reason: Some("r".into()),
        ..done(5)
    };
    let same = f.add_delta(&delta(big, at_cap, vec![replaced], vec![]));
    assert_eq!(f.fold(&same).unwrap().answers.len(), MAX_PLAN_ANSWERS);
    drop(f);
    // the byte bound: a reconstruction past MAX_PLAN_BYTES is invalid fields before it grows
    let reason = "r".repeat(1 << 20);
    let heavy: Vec<PlanAnswer> = (0..15u8)
        .map(|n| ans(n + 1, PlanAct::Defer, &reason))
        .collect();
    let mut f = Frames::new();
    let heavy_full = f.add_full(&version("work", None, None, &heavy));
    let more = f.add_delta(&delta(
        heavy_full,
        heavy_full,
        vec![
            ans(100, PlanAct::Defer, &reason),
            ans(101, PlanAct::Defer, &reason),
        ],
        vec![],
    ));
    assert!(full_body(&version("work", None, None, &heavy)).len() < MAX_PLAN_BYTES);
    broken(f.fold(&more), more, INVALID_FIELDS);
    // a declared list longer than the bounds is refused at decode, without allocating it
    let mut hostile = vec![1u8]; // Delta
    hostile.extend(postcard::to_allocvec(&(Hash([1; 32]), Hash([2; 32]), "w")).unwrap());
    hostile.push(0); // proposal None
    hostile.extend([0xFF, 0xFF, 0xFF, 0xFF, 0x0F]); // answers_set: 2^32 - 1 entries
    assert!(plan_record::decode_body(PLAN_DELTA_ENCODING, &hostile).is_err());
    // an unknown encoding refuses
    assert!(plan_record::decode_body(2, &full_body(&base)).is_err());

    // through a store: a raw delta after a missing or non-plan predecessor is damage, and a
    // read refuses it without a panic
    for non_plan in [false, true] {
        let (_d, mut s) = store();
        let w = chain(&mut s, "work", &[vec![ans(1, PlanAct::Defer, "one")]]);
        let prev = if non_plan {
            s.included("work").unwrap()[0]
        } else {
            Hash([0xEE; 32])
        };
        let d = delta(w[0].0, prev, vec![ans(2, PlanAct::Done, "")], vec![]);
        let body = delta_body(&d);
        let h = encoded_hash(PLAN_DELTA_ENCODING, &body);
        s.write_guarded_for_tests(vec![(
            kinds::PLAN | VERSIONED,
            [&PLAN_DELTA_ENCODING.to_le_bytes()[..], &body].concat(),
        )])
        .unwrap();
        match s.plan(&h) {
            Err(StoreError::Decode(m)) => assert_eq!(m, format!("plan {h}: {INVALID_CHAIN}")),
            other => panic!("{prev}: {other:?}"),
        }
        match s.check_objects() {
            Err(StoreError::Decode(m)) => assert_eq!(m, format!("plan {h}: {INVALID_CHAIN}")),
            other => panic!("fsck: {other:?}"),
        }
        // the typed writer refuses a version after it, with nothing written
        let end = s.log_end();
        let after = version(
            "work",
            Some(w[0].0),
            Some(h),
            &[ans(1, PlanAct::Defer, "one")],
        );
        assert!(s.put_plan(&after).is_err());
        assert_eq!(s.log_end(), end);
    }
}

#[test]
fn r5906_wire_refs_do_not_hydrate() {
    let (_d, mut s) = store();
    let p = s.propose("work", "first", writer()).unwrap();
    let w = chain(
        &mut s,
        "work",
        &[
            vec![ans(1, PlanAct::Defer, "one")],
            vec![ans(1, PlanAct::Defer, "one"), ans(2, PlanAct::Done, "")],
        ],
    );
    let (encoding, body) = s.get_versioned_body(kinds::PLAN, &w[1].0).unwrap();
    assert_eq!(encoding, PLAN_DELTA_ENCODING);
    let payload = [&encoding.to_le_bytes()[..], &body].concat();
    // a delta's edges: stable and prev (one key, sorted and without repeats), and a seal's
    plan_record::reset_counts_for_tests();
    let refs = refs_of(s.features(), kinds::PLAN | VERSIONED, &payload).unwrap();
    assert_eq!(
        refs,
        [ObjectRef {
            kind: Some(kinds::PLAN),
            hash: w[0].0
        }]
    );
    let sealed = PlanDeltaV1 {
        proposal: Some(p),
        seal: Some(PlanSeal {
            landing: Hash([0x11; 32]),
            version: p,
            decisions: vec![],
            items: vec![SealedItem {
                id: item_hash(
                    &BlockId::parse("D-00002").unwrap(),
                    "b-00001",
                    PlanKind::Reaffirm,
                ),
                kind: PlanKind::Reaffirm,
                subject: "b-00001".into(),
                cause: BlockId::parse("D-00002").unwrap(),
                edge: "reflects".into(),
                outcome: PlanOutcome::Deferred,
                answer: Some(Hash([0x22; 32])),
            }],
        }),
        answers_remove: vec![Hash([1; 32])],
        ..delta(w[0].0, w[1].0, vec![ans(3, PlanAct::Done, "")], vec![])
    };
    let payload = [&PLAN_DELTA_ENCODING.to_le_bytes()[..], &delta_body(&sealed)].concat();
    let mut want = vec![
        ObjectRef {
            kind: Some(kinds::PLAN),
            hash: w[0].0,
        },
        ObjectRef {
            kind: Some(kinds::PLAN),
            hash: w[1].0,
        },
        ObjectRef {
            kind: Some(kinds::PROPOSAL),
            hash: p,
        },
        ObjectRef {
            kind: Some(kinds::LANDING),
            hash: Hash([0x11; 32]),
        },
        ObjectRef {
            kind: Some(kinds::ANSWER),
            hash: Hash([0x22; 32]),
        },
    ];
    want.sort_by_key(|r| (r.kind, r.hash));
    assert_eq!(
        refs_of(s.features(), kinds::PLAN | VERSIONED, &payload).unwrap(),
        want
    );
    assert_eq!(plan_record::counts_for_tests().folds, 0, "no edge hydrates");

    // a long chain across several whole checkpoints: metadata walks decode each visited
    // frame once and fold nothing; a read folds once within its bound
    let (_d, mut s) = store();
    let mut answers = vec![ans(0, PlanAct::Defer, "zero")];
    let first = s.put_plan(&version("work", None, None, &answers)).unwrap();
    let mut prev = first;
    let mut ids = vec![first];
    for n in 1..300usize {
        let i = n as u64;
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&i.to_be_bytes());
        answers.push(PlanAnswer {
            item: Hash(h),
            ..ans(0, PlanAct::Confirm, "ok")
        });
        prev = s
            .put_plan(&version("work", Some(first), Some(prev), &answers))
            .unwrap();
        ids.push(prev);
    }
    let fulls = ids.iter().filter(|h| record(&s, h).is_full()).count();
    assert!(fulls >= 3, "{fulls} whole versions in 300");
    let latest = s.plan(&prev).unwrap();
    plan_record::reset_counts_for_tests();
    let depth = s.plan_delta_depth_for_tests(&prev).unwrap().unwrap();
    let c = plan_record::counts_for_tests();
    assert_eq!(c.folds, 0);
    assert_eq!(c.decodes, depth as u64 + 1, "each visited header once");
    plan_record::reset_counts_for_tests();
    assert_eq!(s.last_bound_proposal(&latest).unwrap(), None);
    let c = plan_record::counts_for_tests();
    assert_eq!(c.folds, 0, "last_bound_proposal folds nothing");
    assert_eq!(c.decodes, ids.len() as u64 - 1, "each earlier frame once");
    plan_record::reset_counts_for_tests();
    assert_eq!(s.plan(&prev).unwrap(), latest);
    let c = plan_record::counts_for_tests();
    assert_eq!(c.folds, 1);
    assert!(c.decodes <= 2 * (depth as u64 + 1) && c.decodes <= 258);
    // the next write's checks (its chain rule and the 128-rule count) hydrate within a fixed
    // number of folds, however long the chain
    answers.push(ans(255, PlanAct::Confirm, "ok"));
    plan_record::reset_counts_for_tests();
    s.put_plan(&version("work", Some(first), Some(prev), &answers))
        .unwrap();
    let c = plan_record::counts_for_tests();
    assert!(c.folds <= 3, "{c:?}");
    assert!(c.decodes <= 4 * 258, "{c:?}");

    // a seal after an answer-only chain: the approved binding's search folds only the binding
    // it selects, so the landing composition's folds do not grow with the chain
    let (_d2, _s2, _, _, few) = seal_after_later_answers(2);
    let (_d3, s3, _, _, many) = seal_after_later_answers(24);
    assert_eq!(few, many, "no fold for an intervening version");
    assert!(many <= 4, "{many} folds");
    s3.check_objects().unwrap();
}

/// `n` views, each with a bound proposal written in its own batch: (view, proposal, binding).
fn bound_views(s: &mut Store, n: usize) -> Vec<(String, Hash, Hash)> {
    let mut out = vec![];
    for i in 0..n {
        let view = format!("v{i}");
        s.commit_to_view(
            &view,
            change(
                &view,
                vec![Op::Create {
                    block: block(&format!("b-1000{i}"), "y"),
                }],
            ),
        )
        .unwrap();
        s.set_held(true).unwrap();
        let p = s.propose(&view, "t", writer()).unwrap();
        let mut b = version(&view, None, None, &[]);
        b.proposal = Some(p);
        let id = s.prepare_propose_for_held(p, &b);
        s.seal(vec![]).unwrap();
        s.set_held(false).unwrap();
        out.push((view, p, id));
    }
    out
}

/// The acceptance request of view `view`'s proposal `p`.
fn acceptance(view: &str, p: Hash) -> rdy_store::LandRequest {
    rdy_store::LandRequest {
        proposal: p,
        accept: Some(change(
            view,
            vec![Op::Create {
                block: block(&format!("b-2{}", view[1..].repeat(4)), "accepted"),
            }],
        )),
        approver: human(),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    }
}

/// Lands `p` with an acceptance and its final version's binding after `b`; `before` runs
/// first with the landing record the commit will write. Returns (landing, final version).
fn accept_one(
    s: &mut Store,
    view: &str,
    p: Hash,
    b: Hash,
    before: impl FnOnce(&mut Store, &LandingRecord),
) -> (Hash, Hash) {
    let probe = s.prepare_land(acceptance(view, p)).unwrap();
    let record = probe.landing().clone();
    drop(probe);
    before(s, &record);
    let mut prepared = s.prepare_land(acceptance(view, p)).unwrap();
    let fin = prepared.landed_version();
    let fplan = Plan {
        stable: Some(b),
        prev: Some(b),
        proposal: Some(fin),
        provenance: human(),
        ..version(view, None, None, &[])
    };
    let fid = s.prepare_landing_plan_id(&prepared, &fplan).unwrap();
    s.plan_landing(&mut prepared, Some(&fplan), None).unwrap();
    let l = prepared.hash();
    s.commit_land(prepared, &[]).unwrap();
    LAST_BINDING.with(|c| c.set(Some((b, fid))));
    (l, fin)
}

thread_local! {
    /// The chain start and the final binding id of the last [`accept_one`] on this thread.
    static LAST_BINDING: Cell<Option<(Hash, Hash)>> = const { Cell::new(None) };
}

/// `n` bound views, then every view's acceptance landing with its final binding, in one held
/// batch when `one_batch`: each binding asks for its landing.
fn acceptances(n: usize, one_batch: bool) -> (tempfile::TempDir, Store, Vec<(Hash, Hash)>) {
    acceptances_then(n, one_batch, |_, _| {})
}

/// [`acceptances`], with `then` run before the batch seals.
fn acceptances_then(
    n: usize,
    one_batch: bool,
    then: impl FnOnce(&mut Store, &[(Hash, Hash)]),
) -> (tempfile::TempDir, Store, Vec<(Hash, Hash)>) {
    let (d, mut s) = store();
    let bound = bound_views(&mut s, n);
    if one_batch {
        s.set_held(true).unwrap();
    }
    let mut out = vec![];
    for (view, p, b) in bound {
        out.push(accept_one(&mut s, &view, p, b, |_, _| {}));
    }
    then(&mut s, &out);
    if one_batch {
        s.seal(vec![]).unwrap();
        s.set_held(false).unwrap();
    }
    (d, s, out)
}

/// fsck's outcome on `s`, as text.
fn fsck(s: &Store) -> Result<(), String> {
    s.check_objects().map(|_| ()).map_err(|e| e.to_string())
}

/// fsck's outcome on `s` with the map; the same outcome with every lookup scanning as before
/// (a cap of zero maps nothing) and with small caps.
fn same_as_scan(s: &Store) -> Result<(), String> {
    let with_map = fsck(s);
    for cap in [0usize, 1, 2] {
        let _cap = batch_landing_cap_for_tests(cap);
        assert_eq!(fsck(s), with_map, "cap {cap}");
    }
    with_map
}

#[test]
fn r5906_batch_landing_lookup_decodes_each_landing_once() {
    let n = 6;
    let (_d, s, landed) = acceptances(n, true);
    let batch = s.last_batch().to_vec();
    let landings = batch
        .iter()
        .filter(|(k, _)| codec::logical_kind(*k) == kinds::LANDING)
        .count();
    assert_eq!(landings, n, "one batch holds every landing");
    let end = s.log_end();
    for (l, fin) in &landed {
        assert!(batch.iter().any(|(_, h)| h == l));
        assert!(s.plan_of_proposal(fin, end).unwrap().is_some());
    }
    take_landing_decodes_for_tests();
    fsck(&s).unwrap();
    assert_eq!(
        take_landing_decodes_for_tests(),
        n as u64,
        "one decode per landing of the batch, not one per earlier landing per plan"
    );
    // a cap of two: a binding found as it is met needs no scan, and the outcome is the same
    let cap = batch_landing_cap_for_tests(2);
    fsck(&s).unwrap();
    assert_eq!(take_landing_decodes_for_tests(), n as u64);
    drop(cap);
    // a later lookup of a version past the cap falls back to the scan: a second binding of
    // the last final version, raw, at the end of the batch (damage either way)
    let (_d, s, _) = acceptances_then(n, true, |s, landed| {
        let (_, fin) = landed[landed.len() - 1];
        let (b, fid) = LAST_BINDING.with(Cell::get).unwrap();
        let again = Plan {
            stable: Some(b),
            prev: Some(fid),
            proposal: Some(fin),
            provenance: prov("human:bob", Role::Approver),
            ..version(&format!("v{}", landed.len() - 1), None, None, &[])
        };
        let frame = s.select_plan_frame(&again, &[]).unwrap();
        s.write_guarded_for_tests(vec![frame.into_item()]).unwrap();
    });
    take_landing_decodes_for_tests();
    let with_map = fsck(&s);
    let mapped = take_landing_decodes_for_tests();
    let cap = batch_landing_cap_for_tests(2);
    assert_eq!(fsck(&s), with_map);
    let scanned = take_landing_decodes_for_tests();
    drop(cap);
    assert!(scanned > mapped, "the fallback scans: {scanned} > {mapped}");
    // the map is cleared at each batch: landings in their own batches decode once each too
    let (_d, s, _) = acceptances(3, false);
    take_landing_decodes_for_tests();
    fsck(&s).unwrap();
    assert_eq!(take_landing_decodes_for_tests(), 3);
}

#[test]
fn r5906_batch_landing_lookup_preserves_bad_seal() {
    // sound: the final bindings follow their landings, in one batch or across batches
    let (_d, s, _) = acceptances(3, true);
    same_as_scan(&s).unwrap();
    let (_d, s, _) = acceptances(3, false);
    same_as_scan(&s).unwrap();
    // a binding of a version that no landing of its batch lands is refused before append
    let (_d, mut s) = store();
    let bound = bound_views(&mut s, 1);
    let (view, p, b) = &bound[0];
    let end = s.log_end();
    let mut prepared = s.prepare_land(acceptance(view, *p)).unwrap();
    let stray = Plan {
        stable: Some(*b),
        prev: Some(*b),
        proposal: Some(Hash([0x77; 32])),
        provenance: human(),
        ..version(view, None, None, &[])
    };
    assert!(s.plan_landing(&mut prepared, Some(&stray), None).is_err());
    drop(prepared);
    assert_eq!(s.log_end(), end);
    same_as_scan(&s).unwrap();
    // a duplicate landing of one final version, raw, earlier in the batch: the writer's
    // lookup takes the first landing of the version, as before, and refuses; fsck of what
    // stands reads the same under the map and the scan
    let (_d, mut s) = store();
    let bound = bound_views(&mut s, 2);
    s.set_held(true).unwrap();
    let (v0, p0, b0) = &bound[0];
    accept_one(&mut s, v0, *p0, *b0, |_, _| {});
    let (v1, p1, b1) = &bound[1];
    let probe = s.prepare_land(acceptance(v1, *p1)).unwrap();
    let dup = LandingRecord {
        approved_by: prov("human:bob", Role::Approver),
        ..probe.landing().clone()
    };
    drop(probe);
    s.write_guarded_for_tests(vec![codec::item_new(kinds::LANDING, &dup)])
        .unwrap();
    let mut prepared = s.prepare_land(acceptance(v1, *p1)).unwrap();
    let fplan = Plan {
        stable: Some(*b1),
        prev: Some(*b1),
        proposal: Some(prepared.landed_version()),
        provenance: human(),
        ..version(v1, None, None, &[])
    };
    assert!(s.plan_landing(&mut prepared, Some(&fplan), None).is_err());
    drop(prepared);
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    let duplicate = same_as_scan(&s);
    // a malformed LANDING earlier in the batch: the lookup skips it, as the scan does, and
    // fsck reports the same first damage
    let (_d, mut s) = store();
    let bound = bound_views(&mut s, 2);
    s.set_held(true).unwrap();
    let (v0, p0, b0) = &bound[0];
    let mut wrote = false;
    accept_one(&mut s, v0, *p0, *b0, |s, _| {
        wrote = s
            .write_guarded_for_tests(vec![(kinds::LANDING, vec![0xFF, 0x01])])
            .is_ok();
    });
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    let malformed = same_as_scan(&s);
    assert!(!wrote || malformed.is_err(), "{malformed:?}");
    let _ = duplicate;
}

#[test]
fn r5906_holder_landing_fallback_is_unchanged() {
    // a holder takes a tag's closure: its acceptance landing and final binding arrive in a
    // fetched batch; the map and the holder's first-carrier fallback give the same outcome
    let (d, mut s, landed) = acceptances(2, true);
    let (l, _) = landed[1];
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: l,
            message: None,
            tagged_by: human(),
        })
        .unwrap();
    let path = d.path().join("v1.rdysync");
    {
        let r = s.reader();
        let end = s.log_end();
        let c = rdy_store::closure::closure_before(&r, tag, &Default::default(), end).unwrap();
        let header = rdy_store::sync_stream::StreamHeader::tag_history(
            rdy_store::format::baseline_features(),
            s.repo_id().unwrap().unwrap(),
            tag,
            vec![],
        );
        let mut source = rdy_store::closure::StoreSource { reader: &r, end };
        rdy_store::closure::write_batch(
            &mut source,
            &header,
            &c.records,
            std::fs::File::create(&path).unwrap(),
        )
        .unwrap();
    }
    let mut h = Store::create_holder(&d.path().join("h"), s.repo_id().unwrap().unwrap()).unwrap();
    let target = rdy_store::take_batch::UploadTarget {
        tag: "v1".into(),
        landing: l,
    };
    h.take_batches(&[std::fs::File::open(&path).unwrap()], &target, |_, _| {
        Ok::<_, rdy_store::stream_error::StreamError>(rdy_store::take_batch::TagChoice::Incoming)
    })
    .unwrap();
    h.check_holder().unwrap();
    same_as_scan(&h).unwrap();
    // a project store reads its later binding of a landed version outside the batch the same
    // way under the map and the scan
    let (_d, mut s) = store();
    let bound = bound_views(&mut s, 1);
    let (view, p, b) = &bound[0];
    accept_one(&mut s, view, *p, *b, |_, _| {});
    same_as_scan(&s).unwrap();
}

#[test]
fn r5906_v0_records_read_and_name_their_old_ids() {
    // a plan stored before plan deltas keeps its bytes and its id, and a delta may follow it
    let (_d, mut s) = store();
    let old = version("work", None, None, &[ans(1, PlanAct::Defer, "one")]);
    let old_id = Hash::of(&v0_body(&old));
    assert_eq!(
        postcard::to_allocvec(&PlanV0::of(&old)).unwrap(),
        v0_body(&old)
    );
    let r = RefUpdate {
        name: plan_ref("work"),
        value: old_id.0.to_vec(),
        at: Location {
            segment: 0,
            offset: 0,
        },
    };
    s.write_guarded_for_tests(vec![
        (kinds::PLAN, v0_body(&old)),
        (kinds::REF_UPDATE, postcard::to_allocvec(&r).unwrap()),
    ])
    .unwrap();
    let read = s.plan(&old_id).unwrap();
    assert_eq!(read, old);
    assert_eq!(read.id(), old_id);
    assert_eq!(read.read_identity.unwrap().version, 0);
    let next = s
        .put_plan(&version(
            "work",
            Some(old_id),
            Some(old_id),
            &[ans(1, PlanAct::Defer, "one"), ans(2, PlanAct::Done, "")],
        ))
        .unwrap();
    assert!(!record(&s, &next).is_full());
    assert_eq!(s.plan(&next).unwrap().answers.len(), 2);
    s.check_objects().unwrap();
    let _ = PlanRecordV1::Full(PlanV0::of(&old));
    let _ = MAX_PLAN_BYTES;
}
