//! rg2 (D-ME2HD, D-SDSV2, D-EYRGM, D-AYYD8, D-1HDVA; O3): the store writes a version's
//! assessment in its propose batch or in a batch of its own, a landing records the assessment it
//! is given, and the index answers the newest assessment of a version and the log's first
//! briefed change. A reindex keeps every assessment byte for byte, and an index without the
//! assessment table is rebuilt from the log.
use rdy_model::assessment::*;
use rdy_model::*;
use rdy_store::carried_records::{self as carried, StoreObjects};
use rdy_store::format::{REF_STORE_FEATURES, StoreFeatures};
use rdy_store::index::Index;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::plan::ProposeRequest;
use rdy_store::segment::Location;
use rdy_store::{LandRequest, Store, StoreError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

fn approver() -> Provenance {
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

fn change(view: &str, ops: Vec<Op>, briefing: Option<Hash>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: writer(),
        justification: None,
        message: String::new(),
        links: ChangeLinks {
            briefing,
            ..ChangeLinks::default()
        },
        read_identity: None,
    }
}

fn create(view: &str, i: &str, briefing: Option<Hash>) -> Change {
    change(
        view,
        vec![Op::Create {
            block: block(i, &format!("the {i} claim")),
        }],
        briefing,
    )
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

/// A valid assessment of version `proposal` on canon `base`, which reaches `blocks`; `mark` makes
/// two assessments of one version differ.
fn assessment(proposal: Hash, base: Option<Hash>, blocks: Vec<BlockId>, mark: &str) -> Assessment {
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
        policy: Some(AssessedPolicy {
            policy: None,
            verdict: PolicyVerdict::NeedsPerson {
                role: None,
                reasons: vec!["no authority policy is set".into()],
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
            hash: Hash::of(mark.as_bytes()),
            facts: vec![format!("Changes: 1 ({mark})")],
        },
        scope: AssessmentScope { proposal, blocks },
        issues: vec![],
    }
}

/// A store at `<dir>/s` with block `b` committed to view `view`.
fn store_with(dir: &Path, view: &str, b: &str) -> (PathBuf, Store) {
    let path = dir.join("s");
    let mut s = Store::create(&path).unwrap();
    s.commit_to_view(view, create(view, b, None)).unwrap();
    (path, s)
}

/// The first version of `view`'s proposal, with a summary, holding the view's changes.
fn request<'a>(s: &Store, view: &'a str) -> ProposeRequest<'a> {
    ProposeRequest {
        view,
        title: "first",
        summary: Some("adds a claim"),
        by: writer(),
        changes: s.included(view).unwrap().as_ref().clone(),
        alternative: None,
        prev: None,
        rekey: None,
        answer: None,
    }
}

/// The features of the log's `meta/store_features` ref, by name.
fn manifest(s: &Store) -> Vec<String> {
    let v = s.reader().get_ref(REF_STORE_FEATURES).unwrap().unwrap();
    StoreFeatures::parse(&v)
        .unwrap()
        .features
        .iter()
        .map(|f| f.name.clone())
        .collect()
}

fn markers_since(s: &Store, from: Location) -> usize {
    let mut n = 0;
    s.for_each_frame_from(from, s.log_end(), |_, kind, _, _, _| {
        n += usize::from(kind == kinds::BATCH_COMMIT);
        Ok(())
    })
    .unwrap();
    n
}

/// The location of the frame of logical kind `kind` with hash `h`.
fn frame_at(s: &Store, kind: u8, h: Hash) -> Location {
    let mut at = None;
    s.for_each_frame_from(ORIGIN, s.log_end(), |loc, raw, hash, _, _| {
        if rdy_store::codec::logical_kind(raw) == kind && hash == h && at.is_none() {
            at = Some(loc);
        }
        Ok(())
    })
    .unwrap();
    at.expect("the frame is in the log")
}

/// A proposed version of view `work` with its assessment bound in the propose batch.
fn proposed_with_assessment(dir: &Path) -> (PathBuf, Store, Hash, Hash, Assessment) {
    let (path, mut s) = store_with(dir, "work", "b-00001");
    let mut prepared = s.prepare_propose(request(&s, "work")).unwrap();
    let v = prepared.version();
    let a = assessment(v, None, vec![id("b-00001")], "first");
    let h = s.bind_assessment(&mut prepared, &a).unwrap();
    let out = s.commit_propose(prepared).unwrap();
    assert_eq!(out.assessment, Some(h));
    (path, s, v, h, a)
}

#[test]
fn rg2_a_bound_assessment_is_written_in_the_propose_batch() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s) = store_with(d.path(), "work", "b-00001");
    assert!(!manifest(&s).contains(&"assessment".to_string()));
    let end = s.log_end();
    let mut prepared = s.prepare_propose(request(&s, "work")).unwrap();
    let v = prepared.version();
    let a = assessment(v, None, vec![id("b-00001")], "first");
    let h = s.bind_assessment(&mut prepared, &a).unwrap();
    assert_eq!(s.log_end(), end, "a bind writes nothing");
    let out = s.commit_propose(prepared).unwrap();
    assert_eq!(out.version, v);
    assert_eq!(out.assessment, Some(h));
    assert_eq!(markers_since(&s, end), 1, "one batch, one marker");
    assert!(
        s.last_batch().contains(&(kinds::ASSESSMENT, h)),
        "the assessment is in the propose batch: {:?}",
        s.last_batch()
    );
    assert_eq!(s.assessment_of_proposal(&v, s.log_end()).unwrap(), Some(h));
    assert_eq!(s.assessment_of_proposal(&v, end).unwrap(), None);
    assert_eq!(s.assessment(&h).unwrap(), a);
    assert!(manifest(&s).contains(&"assessment".to_string()));
    s.check_objects().unwrap();
}

#[test]
fn rg2_bind_refuses_an_assessment_of_another_version() {
    let d = tempfile::tempdir().unwrap();
    let (_path, s) = store_with(d.path(), "work", "b-00001");
    let end = s.log_end();
    let mut prepared = s.prepare_propose(request(&s, "work")).unwrap();
    let other = Hash([9; 32]);
    let a = assessment(other, None, vec![], "other");
    let h = Hash::of(&rdy_store::codec::item_new(kinds::ASSESSMENT, &a).1);
    match s.bind_assessment(&mut prepared, &a) {
        Err(e) => assert_eq!(
            e.to_string(),
            carried::refusal(&h, kinds::ASSESSMENT, carried::ASSESSMENT_PROPOSAL).to_string()
        ),
        Ok(h) => panic!("an assessment of another version was bound: {h}"),
    }
    assert!(
        carried::refusal(&h, kinds::ASSESSMENT, carried::ASSESSMENT_PROPOSAL)
            .to_string()
            .ends_with(&format!(
                "record {h} (kind 25): assessment proposal differs"
            ))
    );
    // a record its own rules refuse: its reason, in the same form
    let v = prepared.version();
    let bad = Assessment {
        method: 0,
        ..assessment(v, None, vec![], "bad")
    };
    let bh = Hash::of(&rdy_store::codec::item_new(kinds::ASSESSMENT, &bad).1);
    match s.bind_assessment(&mut prepared, &bad) {
        Err(e) => assert!(
            e.to_string()
                .ends_with(&format!("record {bh} (kind 25): invalid assessment method")),
            "{e}"
        ),
        Ok(h) => panic!("an invalid assessment was bound: {h}"),
    }
    // one per version batch
    let good = assessment(v, None, vec![], "good");
    s.bind_assessment(&mut prepared, &good).unwrap();
    assert!(
        s.bind_assessment(&mut prepared, &assessment(v, None, vec![], "again"))
            .is_err()
    );
    assert_eq!(s.log_end(), end, "nothing written");
}

#[test]
fn rg2_put_assessment_replaces_the_newest_of_a_version() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, v, first, _) = proposed_with_assessment(d.path());
    let after_first = s.log_end();
    let fresh = assessment(v, None, vec![id("b-00001")], "fresh");
    let end = s.log_end();
    let second = s.put_assessment(&fresh).unwrap();
    assert_ne!(first, second);
    assert_eq!(markers_since(&s, end), 1, "a batch of its own");
    assert_eq!(
        s.assessment_of_proposal(&v, s.log_end()).unwrap(),
        Some(second)
    );
    // a reader whose log end is before the fresh one sees the first
    assert_eq!(
        s.assessment_of_proposal(&v, after_first).unwrap(),
        Some(first)
    );
    assert_eq!(s.assessment(&second).unwrap(), fresh);
    // the version must be held
    let missing = Hash([7; 32]);
    let end = s.log_end();
    assert!(matches!(
        s.put_assessment(&assessment(missing, None, vec![], "missing")),
        Err(StoreError::NotFound(h)) if h == missing
    ));
    assert_eq!(s.log_end(), end);
    s.check_objects().unwrap();
}

#[test]
fn rg2_a_landing_records_the_assessment_it_is_given() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s) = store_with(d.path(), "first", "b-00001");
    // an earlier landing, the base the assessment names
    let p0 = s.prepare_propose(request(&s, "first")).unwrap();
    let v0 = s.commit_propose(p0).unwrap().version;
    let base = s
        .land_atomic(LandRequest {
            proposal: v0,
            accept: None,
            approver: approver(),
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap()
        .landing;
    s.commit_to_view("work", create("work", "b-00002", None))
        .unwrap();
    let mut prepared = s.prepare_propose(request(&s, "work")).unwrap();
    let v = prepared.version();
    let a = assessment(v, Some(base), vec![id("b-00002")], "work");
    let h = s.bind_assessment(&mut prepared, &a).unwrap();
    s.commit_propose(prepared).unwrap();
    let req = |assessment| LandRequest {
        proposal: v,
        accept: None,
        approver: approver(),
        alt: None,
        relay: None,
        evidence: None,
        assessment,
    };
    // an assessment of another version is refused, with nothing written
    let end = s.log_end();
    let other = s
        .put_assessment(&assessment(v0, None, vec![], "v0"))
        .unwrap();
    let end_other = s.log_end();
    let refused = s.land_atomic(req(Some(other)));
    match refused {
        Err(e) => assert!(
            e.to_string().contains(&format!(
                "(kind {}): assessment proposal differs",
                kinds::LANDING
            )),
            "{e}"
        ),
        Ok(o) => panic!("a landing named another version's assessment: {o:?}"),
    }
    assert_eq!(s.log_end(), end_other);
    assert!(end < end_other);
    // the version's own
    let out = s.land_atomic(req(Some(h))).unwrap();
    let rec = s.landing(&out.landing).unwrap();
    assert_eq!(rec.assessment, Some(h));
    assert_eq!(rec.approved_version, Some(v));
    let reader = s.reader();
    let mut source = StoreObjects(&reader);
    assert_eq!(
        carried::validate_refs(
            &mut source,
            ObjectKey {
                kind: kinds::LANDING,
                hash: out.landing,
            }
        )
        .unwrap(),
        None
    );
    drop(reader);
    s.check_objects().unwrap();
}

#[test]
fn rg2_a_reindex_keeps_every_assessment_byte_for_byte() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, v, first, _) = proposed_with_assessment(d.path());
    let second = s
        .put_assessment(&assessment(v, None, vec![id("b-00001")], "fresh"))
        .unwrap();
    let bytes = |s: &Store, h: &Hash| s.reader().get_kind_raw(kinds::ASSESSMENT, h).unwrap();
    let before = (bytes(&s, &first), bytes(&s, &second));
    let dump = s.index_dump_for_tests().unwrap();
    assert_eq!(dump.assessment_proposals.len(), 2);
    let answer = s.assessment_of_proposal(&v, s.log_end()).unwrap();
    assert_eq!(answer, Some(second));
    s.reindex().unwrap();
    assert_eq!((bytes(&s, &first), bytes(&s, &second)), before);
    assert_eq!(Hash::of(&before.0.1), first);
    assert_eq!(Hash::of(&before.1.1), second);
    assert_eq!(s.index_dump_for_tests().unwrap(), dump, "the same index");
    assert_eq!(s.assessment_of_proposal(&v, s.log_end()).unwrap(), answer);
    s.check_objects().unwrap();
}

#[test]
fn rg2_the_first_briefed_change_is_the_lowest_location() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s) = store_with(d.path(), "work", "b-00001");
    assert_eq!(s.first_briefed_change().unwrap(), None);
    s.commit_to_view("work", create("work", "b-00002", None))
        .unwrap();
    assert_eq!(s.first_briefed_change().unwrap(), None);
    let one = s
        .commit_to_view("work", create("work", "b-00003", Some(Hash([1; 32]))))
        .unwrap();
    let two = s
        .commit_to_view("other", create("other", "b-00004", Some(Hash([2; 32]))))
        .unwrap();
    let at = frame_at(&s, kinds::CHANGE, one);
    assert!(at < frame_at(&s, kinds::CHANGE, two));
    assert_eq!(s.first_briefed_change().unwrap(), Some(at));
    s.reindex().unwrap();
    assert_eq!(s.first_briefed_change().unwrap(), Some(at));
}

#[test]
fn rg2_an_index_without_the_assessment_table_is_rebuilt() {
    let d = tempfile::tempdir().unwrap();
    let (path, mut s, v, _, _) = proposed_with_assessment(d.path());
    let second = s
        .put_assessment(&assessment(v, None, vec![id("b-00001")], "fresh"))
        .unwrap();
    s.commit_to_view("work", create("work", "b-00002", Some(Hash([1; 32]))))
        .unwrap();
    let end = s.log_end();
    let answers = (
        s.assessment_of_proposal(&v, end).unwrap(),
        s.first_briefed_change().unwrap(),
    );
    assert_eq!(answers.0, Some(second));
    assert!(answers.1.is_some());
    let dump = s.index_dump_for_tests().unwrap();
    s.close().unwrap();
    // the table dropped from the index file
    let file = path.join("index.redb");
    {
        let raw = redb::Database::create(&file).unwrap();
        let w = raw.begin_write().unwrap();
        let def: redb::TableDefinition<&[u8], &[u8]> =
            redb::TableDefinition::new("assessment_proposals");
        assert!(w.delete_table(def).unwrap(), "the table was there");
        w.commit().unwrap();
    }
    match Index::open(&file) {
        Err(StoreError::Db(m)) => assert_eq!(
            m,
            "the index has no assessment table; it is rebuilt from the log"
        ),
        Err(e) => panic!("another refusal: {e}"),
        Ok(_) => panic!("an index without the assessment table opened"),
    }
    let s = Store::open(&path).unwrap();
    assert!(s.open_report().index_rebuilt, "a missing table is rebuilt");
    assert_eq!(
        (
            s.assessment_of_proposal(&v, end).unwrap(),
            s.first_briefed_change().unwrap(),
        ),
        answers
    );
    assert_eq!(s.index_dump_for_tests().unwrap(), dump);
}

/// A tension row whose answer names item `item` of plan `plan`, with an optional dismissal.
fn tension(plan: Option<Hash>, dismissal: Option<Hash>) -> AssessmentTension {
    AssessmentTension {
        id: Hash([3; 32]),
        kind: "fork".into(),
        severity: AssessmentSeverity::Advisory,
        status: AssessmentTensionStatus::Open,
        answer: plan.map(|plan| AssessmentPlanAnswerRef {
            plan,
            item: Hash([4; 32]),
        }),
        dismissal,
        summary: String::new(),
        blocks: vec![],
        vkey: Hash([5; 32]),
    }
}

/// The unsealed plan of view `view` bound to version `v`, with no answers.
fn bare_plan(view: &str, v: Hash) -> rdy_model::plan::Plan {
    rdy_model::plan::Plan {
        stable: None,
        prev: None,
        view: view.into(),
        proposal: Some(v),
        answers: vec![],
        heads: vec![rdy_model::plan::PlanHead {
            repo: "app".into(),
            head: Some("a".repeat(40)),
        }],
        provenance: writer(),
        seal: None,
    }
}

#[test]
fn rg2_an_assessment_bound_before_its_plan_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s) = store_with(d.path(), "work", "b-00001");
    let end = s.log_end();
    let mut prepared = s.prepare_propose(request(&s, "work")).unwrap();
    let v = prepared.version();
    let plan = bare_plan("work", v);
    let a = Assessment {
        tensions: vec![tension(Some(plan.id()), None)],
        ..assessment(v, None, vec![id("b-00001")], "early")
    };
    // the plan its answer names is not in the batch yet: refused, nothing bound
    match s.bind_assessment(&mut prepared, &a) {
        Err(StoreError::NotFound(h)) => assert_eq!(h, plan.id()),
        other => panic!("an assessment bound before its plan: {other:?}"),
    }
    s.bind_plan(&mut prepared, &plan).unwrap();
    // with the plan bound, the answer it does not hold is the plan answer refusal
    let ah = Hash::of(&rdy_store::codec::item_new(kinds::ASSESSMENT, &a).1);
    match s.bind_assessment(&mut prepared, &a) {
        Err(e) => assert_eq!(
            e.to_string(),
            carried::refusal(&ah, kinds::ASSESSMENT, carried::PLAN_ANSWER).to_string()
        ),
        Ok(h) => panic!("an answer the plan does not hold was bound: {h}"),
    }
    assert_eq!(s.log_end(), end, "nothing written");
    let out = s.commit_propose(prepared).unwrap();
    assert_eq!(out.assessment, None);
    assert_eq!(s.assessment_of_proposal(&v, s.log_end()).unwrap(), None);
    s.check_objects().unwrap();
}

#[test]
fn rg2_an_assessment_naming_an_object_not_held_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, v, _, _) = proposed_with_assessment(d.path());
    let end = s.log_end();
    let base = assessment(v, None, vec![id("b-00001")], "dangling");
    let cases = [
        (
            "a tension answer's plan",
            Hash([8; 32]),
            Assessment {
                tensions: vec![tension(Some(Hash([8; 32])), None)],
                ..base.clone()
            },
        ),
        (
            "a tension dismissal's review",
            Hash([6; 32]),
            Assessment {
                tensions: vec![tension(None, Some(Hash([6; 32])))],
                ..base.clone()
            },
        ),
        (
            "the base landing",
            Hash([2; 32]),
            assessment(v, Some(Hash([2; 32])), vec![id("b-00001")], "dangling"),
        ),
    ];
    for (what, missing, a) in cases {
        match s.put_assessment(&a) {
            Err(StoreError::NotFound(h)) => assert_eq!(h, missing, "{what}"),
            other => panic!("{what} that is not held was written: {other:?}"),
        }
        // the next version's batch: the same refusal
        let mut p = s
            .prepare_propose(ProposeRequest {
                title: "next",
                prev: Some(v),
                ..request(&s, "work")
            })
            .unwrap();
        let a = Assessment {
            proposal: p.version(),
            scope: AssessmentScope {
                proposal: p.version(),
                blocks: a.scope.blocks.clone(),
            },
            ..a.clone()
        };
        match s.bind_assessment(&mut p, &a) {
            Err(StoreError::NotFound(h)) => assert_eq!(h, missing, "{what}"),
            other => panic!("{what} that is not held was bound: {other:?}"),
        }
        assert_eq!(s.log_end(), end, "{what}: nothing written");
    }
    s.check_objects().unwrap();
}

#[test]
fn rg2_bind_assessment_refuses_a_reaffirmation_proposal() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s) = store_with(d.path(), "first", "b-00001");
    let p0 = s.prepare_propose(request(&s, "first")).unwrap();
    let v0 = s.commit_propose(p0).unwrap().version;
    s.land_atomic(LandRequest {
        proposal: v0,
        accept: None,
        approver: approver(),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    })
    .unwrap();
    let view = "reaffirm-000000000001";
    let canon = s.canon_state().unwrap();
    let current = *canon.blocks.get(&id("b-00001")).unwrap();
    let mut prepared = s
        .prepare_reaffirm_propose(rdy_store::plan::ReaffirmProposeRequest {
            view,
            canon_at: s.canon_head().unwrap(),
            change: change(
                view,
                vec![Op::Update {
                    base: current,
                    block: block("b-00001", "the b-00001 claim, re-affirmed"),
                }],
                None,
            ),
            title: "re-affirm",
            summary: "re-affirms a claim",
            by: approver(),
        })
        .unwrap();
    let before = format!("{prepared:?}");
    let end = s.log_end();
    let a = assessment(prepared.version(), None, vec![id("b-00001")], "reaffirm");
    match s.bind_assessment(&mut prepared, &a) {
        Err(StoreError::Refused(m)) => assert_eq!(
            m,
            "a re-affirmation proposal is written only with its landing"
        ),
        other => panic!("a re-affirmation proposal took an assessment: {other:?}"),
    }
    assert_eq!(format!("{prepared:?}"), before, "the batch is unchanged");
    assert_eq!(s.log_end(), end);
}

/// A store whose log holds an assessment and a briefed change, closed, with its index file.
fn closed_with_rows(dir: &Path) -> (PathBuf, Hash, Location) {
    let (path, mut s, v, _, _) = proposed_with_assessment(dir);
    let c = s
        .commit_to_view("work", create("work", "b-00002", Some(Hash([1; 32]))))
        .unwrap();
    let at = frame_at(&s, kinds::CHANGE, c);
    assert_eq!(s.first_briefed_change().unwrap(), Some(at));
    s.check_objects().unwrap();
    s.close().unwrap();
    (path, v, at)
}

/// fsck refuses `s` with `text`; `rdy reindex` clears it.
fn fsck_refuses_until_reindex(path: &Path, text: &str) -> Store {
    let mut s = Store::open(path).unwrap();
    assert!(!s.open_report().index_rebuilt, "the index opens as it is");
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert_eq!(m, text),
        other => panic!("fsck passed a wrong index: {other:?}"),
    }
    s.reindex().unwrap();
    s.check_objects().unwrap();
    s
}

#[test]
fn rg2_fsck_reports_a_missing_first_briefed_change() {
    let d = tempfile::tempdir().unwrap();
    let (path, _, at) = closed_with_rows(d.path());
    {
        let raw = redb::Database::create(path.join("index.redb")).unwrap();
        let w = raw.begin_write().unwrap();
        {
            let def: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("meta");
            let mut t = w.open_table(def).unwrap();
            assert!(t.remove("first_briefed_change").unwrap().is_some());
        }
        w.commit().unwrap();
    }
    let s = fsck_refuses_until_reindex(
        &path,
        "the first briefed change differs from the change records; run rdy reindex",
    );
    assert_eq!(s.first_briefed_change().unwrap(), Some(at));
}

#[test]
fn rg2_fsck_reports_missing_assessment_rows() {
    let d = tempfile::tempdir().unwrap();
    let (path, v, _) = closed_with_rows(d.path());
    {
        let raw = redb::Database::create(path.join("index.redb")).unwrap();
        let w = raw.begin_write().unwrap();
        {
            let def: redb::TableDefinition<&[u8], &[u8]> =
                redb::TableDefinition::new("assessment_proposals");
            let mut t = w.open_table(def).unwrap();
            let keys: Vec<Vec<u8>> = redb::ReadableTable::iter(&t)
                .unwrap()
                .map(|e| e.unwrap().0.value().to_vec())
                .collect();
            assert!(!keys.is_empty());
            for k in keys {
                t.remove(&k[..]).unwrap();
            }
        }
        w.commit().unwrap();
    }
    let s = fsck_refuses_until_reindex(
        &path,
        "the assessment table differs from the assessment records; run rdy reindex",
    );
    assert!(s.assessment_of_proposal(&v, s.log_end()).unwrap().is_some());
}
