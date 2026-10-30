//! ck2 (D-JQD5X, D-AYYD8, D-1HDVA, D-9P62P): the first product writers of the carried check
//! records. A request and its `check/<request>` ref are one durable batch, first use enables the
//! `check_request` feature, a Check review passes the carried validator and stays the only review
//! of its request, and an AcceptVerdict answer keeps its own history. The readers stream one
//! object at a time and never see a record appended after their captured end.
use rdy_model::check_request::{CheckCode, CheckMode, CheckRequest, CheckSubject};
use rdy_model::*;
use rdy_store::carried_records as carried;
use rdy_store::closure::closure;
use rdy_store::codec;
use rdy_store::format::{self, FEATURE_REQUIRED_RDY, FEATURE_STORE_FORMAT, REF_STORE_FEATURES};
use rdy_store::ingest::Ingest;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError, StoreReader};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CHECK_REQUEST_FEATURE: &str = "check_request";

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

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
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

/// A store with one landed change: its change, version, landing and block.
struct Project {
    _dir: tempfile::TempDir,
    path: PathBuf,
    change: Hash,
    version: Hash,
    landing: Hash,
    block: BlockId,
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let b = block("b-00001", "A claim.");
    let change = s
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Create { block: b.clone() }],
                deps: vec![],
                provenance: writer(),
                justification: None,
                message: "m".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .unwrap();
    let version = s.propose("work", "first", writer()).unwrap();
    let landing = s.land(&version, approver()).unwrap();
    Project {
        _dir: dir,
        path,
        change,
        version,
        landing,
        block: b.id,
    }
}

impl Project {
    fn open(&self) -> Store {
        Store::open(&self.path).unwrap()
    }

    /// The request of number `n`: every optional part present, with its own case.
    fn request(&self, n: u32) -> CheckRequest {
        CheckRequest {
            check: "meaning_preservation".into(),
            subject: CheckSubject {
                blocks: vec![self.block.clone()],
                change: Some(self.change),
                version: Some(self.version),
                code: Some(CheckCode {
                    repo: "app".into(),
                    from: "a".repeat(40),
                    to: "b".repeat(40),
                }),
            },
            scope: "docs".into(),
            case_hash: case(n),
            mode: CheckMode::Independent,
            reason: Some(format!("a second look {n}")),
            requested_by: writer(),
            relayed_by: None,
            relay: None,
        }
    }
}

fn case(n: u32) -> Hash {
    Hash::of(&n.to_le_bytes())
}

fn id_of(kind: u8, v: &(impl serde::Serialize + 'static)) -> Hash {
    Hash::of(&codec::item_new(kind, v).1)
}

fn verdict_review(request: Hash, case: Hash, verdict: Verdict, on: &BlockId) -> Review {
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

fn verdict_answer(review: Hash, version: &str) -> Answer {
    Answer {
        item: format!("verdict:{review}"),
        act: AnswerAct::AcceptVerdict,
        option: Some(format!("accept:{version}")),
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: approver(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

fn check_ref(request: Hash) -> String {
    format!("{}{request}", carried::CHECK_REF_PREFIX)
}

fn batches(s: &Store) -> usize {
    let mut n = 0;
    s.reader()
        .for_each_batch(s.log_end(), |_| {
            n += 1;
            Ok(())
        })
        .unwrap();
    n
}

/// The refs and the object frames of the last sealed batch of `s`.
fn last_batch(s: &Store) -> (BTreeSet<String>, Vec<u8>) {
    let mut last = None;
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            last = Some(b);
            Ok(())
        })
        .unwrap();
    let last = last.unwrap();
    let refs = last
        .frames
        .iter()
        .filter(|f| f.kind() == kinds::REF_UPDATE)
        .filter_map(|f| {
            f.present_payload()
                .and_then(|p| postcard::from_bytes::<kinds::RefUpdate>(p).ok())
        })
        .map(|u| u.name)
        .collect();
    let objects = last
        .frames
        .iter()
        .map(|f| codec::logical_kind(f.kind()))
        .filter(|k| *k != kinds::REF_UPDATE)
        .collect();
    (refs, objects)
}

fn names_check_request(s: &Store) -> bool {
    s.features()
        .features
        .iter()
        .any(|f| f.name == CHECK_REQUEST_FEATURE)
}

fn refused(e: StoreError) -> String {
    match e {
        StoreError::Refused(m) => m,
        e => panic!("not a refusal: {e:?}"),
    }
}

fn requests(r: &StoreReader, end: Location) -> Vec<Hash> {
    let mut out = vec![];
    r.for_each_check_request(end, |h, _| {
        out.push(h);
        Ok(())
    })
    .unwrap();
    out
}

fn reviews(r: &StoreReader, end: Location) -> Vec<Hash> {
    let mut out = vec![];
    r.for_each_check_review(end, |h, _| {
        out.push(h);
        Ok(())
    })
    .unwrap();
    out
}

fn answers(r: &StoreReader, end: Location) -> Vec<Hash> {
    let mut out = vec![];
    r.for_each_verdict_answer(end, |h, _| {
        out.push(h);
        Ok(())
    })
    .unwrap();
    out
}

fn sorted(mut v: Vec<Hash>) -> Vec<Hash> {
    v.sort();
    v
}

#[test]
fn ck2_request_and_ref_are_one_durable_batch() {
    let p = project();
    let mut s = p.open();
    assert!(!names_check_request(&s));
    let q = p.request(1);
    let before = batches(&s);
    let id = s.put_check_request(&q).unwrap();
    assert_eq!(id, id_of(kinds::CHECK_REQUEST, &q));
    // one batch: the request, its ref and the features ref of its first use
    assert_eq!(batches(&s), before + 1);
    let (refs, objects) = last_batch(&s);
    assert_eq!(
        refs,
        BTreeSet::from([check_ref(id), REF_STORE_FEATURES.to_string()])
    );
    assert_eq!(objects, [kinds::CHECK_REQUEST]);
    // a byte-identical retry writes nothing
    let end = s.log_end();
    assert_eq!(s.put_check_request(&q).unwrap(), id);
    assert_eq!(s.log_end(), end);
    assert_eq!(batches(&s), before + 1);
    drop(s);

    // reopen finds both; the feature is on, and the carried constants have not moved
    let s = p.open();
    let r = s.reader();
    assert_eq!(r.check_request(&id).unwrap(), q);
    assert_eq!(r.get_ref(&check_ref(id)).unwrap(), Some(id.0.to_vec()));
    r.check_objects().unwrap();
    let feature = s
        .features()
        .features
        .iter()
        .find(|f| f.name == CHECK_REQUEST_FEATURE)
        .unwrap()
        .clone();
    assert_eq!(feature.required_rdy, "0.26.0");
    assert_eq!(FEATURE_REQUIRED_RDY, "0.26.0");
    assert_eq!(FEATURE_STORE_FORMAT, 19);
    assert_eq!(kinds::CHECK_REQUEST, 28);
    let (raw, payload) = r.get_kind_raw(kinds::CHECK_REQUEST, &id).unwrap();
    let split = codec::split(raw, &payload).unwrap();
    assert_eq!((split.kind, split.version), (28, 1));
    drop(r);
    drop(s);

    // the plural writer: 1 and 20 rows are one batch each, with the hashes in input order
    let mut s = p.open();
    let one = p.request(100);
    let before = batches(&s);
    assert_eq!(
        s.put_check_requests(std::slice::from_ref(&one)).unwrap(),
        [id_of(kinds::CHECK_REQUEST, &one)]
    );
    assert_eq!(batches(&s), before + 1);
    let twenty: Vec<CheckRequest> = (200..220).map(|n| p.request(n)).collect();
    let want: Vec<Hash> = twenty
        .iter()
        .map(|q| id_of(kinds::CHECK_REQUEST, q))
        .collect();
    assert_ne!(want, sorted(want.clone()), "the input is not in hash order");
    let before = batches(&s);
    assert_eq!(s.put_check_requests(&twenty).unwrap(), want);
    assert_eq!(batches(&s), before + 1);
    let (refs, objects) = last_batch(&s);
    assert_eq!(
        refs,
        want.iter().map(|h| check_ref(*h)).collect::<BTreeSet<_>>()
    );
    assert_eq!(objects, [kinds::CHECK_REQUEST; 20]);
    for (h, q) in want.iter().zip(&twenty) {
        assert_eq!(s.reader().check_request(h).unwrap(), *q);
        assert_eq!(
            s.reader().get_ref(&check_ref(*h)).unwrap(),
            Some(h.0.to_vec())
        );
    }

    // 0 and 21 rows refuse, and write nothing
    let end = s.log_end();
    let text = "a check request batch takes 1 to 20 requests";
    assert_eq!(refused(s.put_check_requests(&[]).unwrap_err()), text);
    let many: Vec<CheckRequest> = (300..321).map(|n| p.request(n)).collect();
    assert_eq!(refused(s.put_check_requests(&many).unwrap_err()), text);
    assert_eq!(s.log_end(), end);

    // a bad last row refuses the whole batch
    let mut bad: Vec<CheckRequest> = (400..420).map(|n| p.request(n)).collect();
    bad[19].scope = String::new();
    let first = id_of(kinds::CHECK_REQUEST, &bad[0]);
    let bad_id = id_of(kinds::CHECK_REQUEST, &bad[19]);
    assert_eq!(
        s.put_check_requests(&bad).unwrap_err().to_string(),
        carried::refusal(&bad_id, kinds::CHECK_REQUEST, "invalid check scope").to_string()
    );
    assert_eq!(s.log_end(), end);
    assert!(!s.reader().has_kind(kinds::CHECK_REQUEST, &first).unwrap());
    assert_eq!(s.reader().get_ref(&check_ref(first)).unwrap(), None);

    // a row held already, and a row given twice, keep their places; only the new row is written
    let new = p.request(500);
    let new_id = id_of(kinds::CHECK_REQUEST, &new);
    let rows = [new.clone(), q.clone(), new];
    let before = batches(&s);
    assert_eq!(s.put_check_requests(&rows).unwrap(), [new_id, id, new_id]);
    assert_eq!(batches(&s), before + 1);
    let (refs, objects) = last_batch(&s);
    assert_eq!(refs, BTreeSet::from([check_ref(new_id)]));
    assert_eq!(objects, [kinds::CHECK_REQUEST]);
    // every row held: no batch at all
    let end = s.log_end();
    assert_eq!(s.put_check_requests(&rows).unwrap(), [new_id, id, new_id]);
    assert_eq!(s.log_end(), end);
    s.reader().check_objects().unwrap();

    // a request held with no ref gains only its ref; a ref that names no held request is damage,
    // and the writer does not repair it
    let bare = p.request(600);
    let bare_id = s.put(kinds::CHECK_REQUEST, &bare).unwrap();
    assert_eq!(s.put_check_request(&bare).unwrap(), bare_id);
    let (refs, objects) = last_batch(&s);
    assert_eq!(refs, BTreeSet::from([check_ref(bare_id)]));
    assert!(objects.is_empty());
    let dangling = p.request(601);
    let dangling_id = id_of(kinds::CHECK_REQUEST, &dangling);
    s.set_refs(vec![(check_ref(dangling_id), dangling_id.0.to_vec())])
        .unwrap();
    assert_eq!(
        s.put_check_request(&dangling).unwrap_err().to_string(),
        carried::refusal(&dangling_id, kinds::CHECK_REQUEST, carried::CHECK_REF).to_string()
    );
}

#[test]
fn ck2_refused_or_failed_request_leaves_no_bytes() {
    let p = project();
    let mut s = p.open();
    let end = s.log_end();

    // a malformed shape, with the carried validator's own text
    let mut bad = p.request(1);
    bad.check = String::new();
    let bad_id = id_of(kinds::CHECK_REQUEST, &bad);
    assert_eq!(
        s.put_check_request(&bad).unwrap_err().to_string(),
        carried::refusal(&bad_id, kinds::CHECK_REQUEST, "invalid check name").to_string()
    );
    assert_eq!(s.log_end(), end);

    // a subject object the store does not hold
    let absent = Hash::of(b"no such change");
    let mut gone = p.request(2);
    gone.subject.change = Some(absent);
    let gone_id = id_of(kinds::CHECK_REQUEST, &gone);
    assert!(matches!(
        s.put_check_request(&gone).unwrap_err(),
        StoreError::NotFound(h) if h == absent
    ));
    assert_eq!(s.log_end(), end);
    assert!(!names_check_request(&s));

    // an injected append failure: nothing acknowledged survives, and the first use rolls back
    let q = p.request(3);
    let id = id_of(kinds::CHECK_REQUEST, &q);
    s.fail_next_append_after(40);
    s.put_check_request(&q).unwrap_err();
    assert!(!s.reader().has_kind(kinds::CHECK_REQUEST, &id).unwrap());
    drop(s);

    let mut s = p.open();
    assert_eq!(s.log_end(), end);
    for h in [bad_id, gone_id, id] {
        assert!(!s.reader().has_kind(kinds::CHECK_REQUEST, &h).unwrap());
        assert_eq!(s.reader().get_ref(&check_ref(h)).unwrap(), None);
    }
    assert!(!names_check_request(&s));
    s.reader().check_objects().unwrap();

    // and the same request then writes whole
    assert_eq!(s.put_check_request(&q).unwrap(), id);
    assert_eq!(
        s.reader().get_ref(&check_ref(id)).unwrap(),
        Some(id.0.to_vec())
    );
    assert!(names_check_request(&s));
}

#[test]
fn ck2_check_review_uses_the_carried_validator() {
    let p = project();
    let mut s = p.open();
    let [q1, q2, q3] = [1, 2, 3].map(|n| s.put_check_request(&p.request(n)).unwrap());
    let end = s.log_end();

    // a concern without citations
    let mut no_cites = verdict_review(q1, case(1), Verdict::Concern, &p.block);
    no_cites.citations.clear();
    let want = carried::refusal(
        &id_of(kinds::REVIEW, &no_cites),
        kinds::REVIEW,
        carried::CHECK_CITATIONS,
    );
    assert_eq!(
        s.put_check_review(&no_cites).unwrap_err().to_string(),
        want.to_string()
    );

    // a case that is not the request's case
    let other = verdict_review(q1, case(9), Verdict::Pass, &p.block);
    let want = carried::refusal(
        &id_of(kinds::REVIEW, &other),
        kinds::REVIEW,
        carried::CHECK_CASE,
    );
    assert_eq!(
        s.put_check_review(&other).unwrap_err().to_string(),
        want.to_string()
    );

    // a request the store does not hold
    let absent = Hash::of(b"no such request");
    assert!(matches!(
        s.put_check_review(&verdict_review(absent, case(1), Verdict::Pass, &p.block))
            .unwrap_err(),
        StoreError::NotFound(h) if h == absent
    ));

    // a review of anything else
    let not_check = Review {
        subject: ReviewSubject::Mention {
            block: p.block.clone(),
            entity: p.block.clone(),
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: String::new(),
        citations: vec![],
        provenance: prov("agent:checker", Role::Reviewer),
    };
    assert_eq!(
        refused(s.put_check_review(&not_check).unwrap_err()),
        "a check review is a review with verdict pass, concern or fail of a check request"
    );
    assert_eq!(s.log_end(), end);

    // each verdict writes; an exact retry writes nothing; another review refuses exactly
    let pass = verdict_review(q1, case(1), Verdict::Pass, &p.block);
    let concern = verdict_review(q2, case(2), Verdict::Concern, &p.block);
    let fail = verdict_review(q3, case(3), Verdict::Fail, &p.block);
    for (q, r) in [(q1, &pass), (q2, &concern), (q3, &fail)] {
        let before = batches(&s);
        let id = s.put_check_review(r).unwrap();
        assert_eq!(id, id_of(kinds::REVIEW, r));
        assert_eq!(batches(&s), before + 1);
        let (refs, objects) = last_batch(&s);
        assert!(refs.is_empty(), "no ref: {refs:?}");
        assert_eq!(objects, [kinds::REVIEW]);

        let end = s.log_end();
        assert_eq!(s.put_check_review(r).unwrap(), id);
        assert_eq!(s.log_end(), end);

        let mut second = r.clone();
        second.provenance.actor = "agent:second".into();
        assert_eq!(
            refused(s.put_check_review(&second).unwrap_err()),
            format!("check request {q} already has review {id}")
        );
        assert_eq!(s.log_end(), end);
        assert_eq!(s.reader().review(&id).unwrap(), *r);
    }

    // imported history holds two reviews of one request: the reader exposes both, the refusal
    // names the lowest hash, and an exact retry of either still returns its hash
    let q4 = s.put_check_request(&p.request(4)).unwrap();
    let mut imported = vec![];
    for actor in ["agent:one", "agent:two"] {
        let mut r = verdict_review(q4, case(4), Verdict::Pass, &p.block);
        r.provenance.actor = actor.into();
        let h = s.put(kinds::REVIEW, &r).unwrap();
        imported.push((h, r));
    }
    imported.sort_by_key(|(h, _)| *h);
    let (low, high) = (imported[0].0, imported[1].0);
    let mut of_q4 = vec![];
    s.reader()
        .for_each_check_review(s.log_end(), |h, r| {
            if matches!(r.subject, ReviewSubject::Check { request } if request == q4) {
                of_q4.push(h);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(of_q4, [low, high]);
    let mut third = verdict_review(q4, case(4), Verdict::Pass, &p.block);
    third.provenance.actor = "agent:three".into();
    assert_eq!(
        refused(s.put_check_review(&third).unwrap_err()),
        format!("check request {q4} already has review {low}")
    );
    let end = s.log_end();
    for (h, r) in &imported {
        assert_eq!(s.put_check_review(r).unwrap(), *h);
    }
    assert_eq!(s.log_end(), end);
    s.reader().check_objects().unwrap();
}

#[test]
fn ck2_captured_read_excludes_later_records() {
    let p = project();
    let mut s = p.open();
    // no carried record yet
    let empty = s.log_end();
    let first: Vec<Hash> = s
        .put_check_requests(&(1..=5).map(|n| p.request(n)).collect::<Vec<_>>())
        .unwrap();
    let review = s
        .put_check_review(&verdict_review(
            first[0],
            case(1),
            Verdict::Concern,
            &p.block,
        ))
        .unwrap();
    let answer = s.put_answer(&verdict_answer(review, "P-a@1")).unwrap();
    // reviews and answers of other kinds never show
    let dismissal = Review {
        subject: ReviewSubject::Mention {
            block: p.block.clone(),
            entity: p.block.clone(),
        },
        case_hash: None,
        verdict: Verdict::Dismiss,
        reason: String::new(),
        citations: vec![],
        provenance: prov("human:alice", Role::Reviewer),
    };
    s.put_mention_review(&dismissal).unwrap();
    s.put_answer(&Answer {
        item: "P-a@1".into(),
        act: AnswerAct::RejectProposal,
        option: None,
        ..verdict_answer(review, "P-a@1")
    })
    .unwrap();
    let old = s.log_end();

    let later_request = s.put_check_request(&p.request(6)).unwrap();
    let later_review = s
        .put_check_review(&verdict_review(
            later_request,
            case(6),
            Verdict::Fail,
            &p.block,
        ))
        .unwrap();
    let later_answer = s
        .put_answer(&verdict_answer(later_review, "P-a@2"))
        .unwrap();
    let now = s.log_end();

    let r = s.reader();
    assert_eq!(requests(&r, empty), []);
    assert_eq!(reviews(&r, empty), []);
    assert_eq!(answers(&r, empty), []);
    // ascending full object hashes, as the log stood at the old end
    assert_eq!(requests(&r, old), sorted(first.clone()));
    assert_eq!(reviews(&r, old), [review]);
    assert_eq!(answers(&r, old), [answer]);
    let mut all = first;
    all.push(later_request);
    assert_eq!(requests(&r, now), sorted(all));
    assert_eq!(reviews(&r, now), sorted(vec![review, later_review]));
    assert_eq!(answers(&r, now), sorted(vec![answer, later_answer]));
    // an error of the visitor stops the walk and comes back
    let mut seen = 0;
    let stopped = r.for_each_check_request(now, |_, _| {
        seen += 1;
        Err(StoreError::Refused("stop".into()))
    });
    assert_eq!(refused(stopped.unwrap_err()), "stop");
    assert_eq!(seen, 1);
}

#[test]
fn ck2_verdict_answers_keep_each_version_in_history() {
    let p = project();
    let mut s = p.open();
    let q = s.put_check_request(&p.request(1)).unwrap();
    let review = s
        .put_check_review(&verdict_review(q, case(1), Verdict::Concern, &p.block))
        .unwrap();
    let one = verdict_answer(review, "P-a@1");
    let two = verdict_answer(review, "P-a@2");
    assert_eq!(one.item, two.item);
    let h1 = s.put_answer(&one).unwrap();
    let h2 = s.put_answer(&two).unwrap();
    assert_ne!(h1, h2);
    // the answer ref names only the latest, so the history comes from the iterator
    assert_eq!(
        s.reader().answer_of(&one.item).unwrap().map(|(h, _)| h),
        Some(h2)
    );
    let held = |s: &Store| {
        let mut got = BTreeMap::new();
        s.reader()
            .for_each_verdict_answer(s.log_end(), |h, a| {
                got.insert(h, a);
                Ok(())
            })
            .unwrap();
        got
    };
    let want = BTreeMap::from([(h1, one.clone()), (h2, two.clone())]);
    assert_eq!(held(&s), want);
    drop(s);
    let mut s = p.open();
    assert_eq!(held(&s), want);
    s.reindex().unwrap();
    assert_eq!(held(&s), want);
    for a in want.values() {
        assert!(a.targets.is_empty());
        assert_eq!(a.item, format!("verdict:{review}"));
    }
    s.reader().check_objects().unwrap();
}

/// A load of the closed store at `src` into `to`: every sealed batch ingested at its place, as
/// a dump of this rdy's format carries it, then the store opened.
fn load(src: &Path, to: &Path) -> Store {
    let s = Store::open(src).unwrap();
    let r = s.reader();
    let features = r.store_features().unwrap();
    let mut got = vec![];
    r.for_each_batch(s.log_end(), |b| {
        got.push(b);
        Ok(())
    })
    .unwrap();
    drop(r);
    drop(s);
    let mut i = Ingest::create(to).unwrap();
    for (n, b) in got.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(&format::DumpStore::Features(features)).unwrap();
    i.finish().unwrap();
    Store::open(to).unwrap()
}

#[test]
fn ck2_checks_survive_dump_load_and_closure() {
    let p = project();
    let mut s = p.open();
    let q = s.put_check_request(&p.request(1)).unwrap();
    let review = s
        .put_check_review(&verdict_review(q, case(1), Verdict::Concern, &p.block))
        .unwrap();
    let answer = s.put_answer(&verdict_answer(review, "P-a@1")).unwrap();
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: p.landing,
            message: None,
            tagged_by: approver(),
        })
        .unwrap();
    let keys = [
        ObjectKey {
            kind: kinds::CHECK_REQUEST,
            hash: q,
        },
        ObjectKey {
            kind: kinds::REVIEW,
            hash: review,
        },
        ObjectKey {
            kind: kinds::ANSWER,
            hash: answer,
        },
    ];
    let frames = |r: &StoreReader| -> Vec<(u8, Vec<u8>)> {
        keys.iter()
            .map(|k| r.get_kind_raw(k.kind, &k.hash).unwrap())
            .collect()
    };
    let want = frames(&s.reader());
    let manifest = s.reader().store_features().unwrap();

    // a closure of the tag takes the request and its review; an answer that targets neither a
    // landing nor a tag stays out, as every carried AcceptVerdict does (ok4)
    let got = closure(&s.reader(), tag, &BTreeSet::new()).unwrap();
    assert!(got.records.contains(&keys[0]));
    assert!(got.records.contains(&keys[1]));
    assert!(!got.records.contains(&keys[2]));
    assert!(
        got.records.iter().position(|k| *k == keys[0])
            < got.records.iter().position(|k| *k == keys[1])
    );
    drop(s);

    // a dump and load keep all three byte for byte, the ref and the first-use feature
    let to = tempfile::tempdir().unwrap();
    let loaded = load(&p.path, &to.path().join("loaded"));
    let r = loaded.reader();
    assert_eq!(frames(&r), want);
    assert_eq!(r.store_features().unwrap(), manifest);
    assert!(names_check_request(&loaded));
    assert_eq!(r.get_ref(&check_ref(q)).unwrap(), Some(q.0.to_vec()));
    assert_eq!(requests(&r, loaded.log_end()), [q]);
    assert_eq!(reviews(&r, loaded.log_end()), [review]);
    assert_eq!(answers(&r, loaded.log_end()), [answer]);
    r.check_objects().unwrap();
}
