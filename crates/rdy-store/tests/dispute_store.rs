//! ap2 (D-ZWP7Q, D-C8RZW, D-QQCZK, D-AYYD8, D-1HDVA; O22): the store writes a DISPUTE of a
//! landing and the DISPUTE_RESOLUTION of a dispute, each with the answer that relays it in the
//! same batch. Two derived tables answer a landing's disputes and a dispute's resolution in one
//! keyed range read; fsck checks them against the log, and an index without them is rebuilt.
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::*;
use rdy_store::format::{REF_STORE_FEATURES, StoreFeatures};
use rdy_store::index::Index;
use rdy_store::ingest::Ingest;
use rdy_store::kinds;
use rdy_store::plan::ProposeRequest;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

const DIFFER: &str = "the dispute tables differ from the dispute records; run rdy reindex";

fn prov(actor: &str, role: Role, at_ms: i64) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("run-1".into()),
        role,
        at_ms,
    }
}

fn writer() -> Provenance {
    prov("agent:w", Role::Writer, 3)
}

fn approver() -> Provenance {
    prov("human:alice", Role::Approver, 4)
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
        read_identity: None,
    }
}

fn create(i: &str) -> Change {
    Change {
        view: "work".into(),
        ops: vec![Op::Create {
            block: block(i, &format!("the {i} claim")),
        }],
        deps: vec![],
        provenance: writer(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Commits block `i` to view `work`, proposes the view with a summary and lands it.
fn land_one(s: &mut Store, i: &str) -> Hash {
    s.commit_to_view("work", create(i)).unwrap();
    let req = ProposeRequest {
        view: "work",
        title: "adds a claim",
        summary: Some("adds a claim"),
        by: writer(),
        changes: s.included("work").unwrap().as_ref().clone(),
        alternative: None,
        prev: None,
        rekey: None,
        answer: None,
    };
    let prepared = s.prepare_propose(req).unwrap();
    let v = prepared.version();
    s.commit_propose(prepared).unwrap();
    s.land(&v, approver()).unwrap()
}

/// A store at `<dir>/s` with two landings.
fn store(dir: &Path) -> (PathBuf, Store, Hash, Hash) {
    let path = dir.join("s");
    let mut s = Store::create(&path).unwrap();
    let l1 = land_one(&mut s, "b-00001");
    let l2 = land_one(&mut s, "b-00002");
    (path, s, l1, l2)
}

fn dispute(landing: Hash, reason: &str) -> Dispute {
    Dispute {
        landing,
        opened_by: approver(),
        reason: reason.into(),
        at_ms: 1_760_000_000_001,
    }
}

fn resolution(dispute: Hash, outcome: DisputeOutcome) -> DisputeResolution {
    DisputeResolution {
        dispute,
        outcome,
        by: approver(),
        correction: None,
    }
}

/// A person's local answer; the writer sets its act, item and target.
fn answer() -> Answer {
    Answer {
        item: "set by the writer".into(),
        act: AnswerAct::Confirm,
        option: None,
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

/// The same answer, relayed by an agent with the person's words.
fn relayed() -> Answer {
    Answer {
        relayed_by: Some(ActorRun {
            actor: "agent:w".into(),
            run: Some("run-1".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "the edit is wrong".into(),
            session_ref: "chat-1".into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Text),
        ..answer()
    }
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

/// The stored answer `h`.
fn stored_answer(s: &Store, h: &Hash) -> Answer {
    let (raw, payload) = s.reader().get_kind_raw(kinds::ANSWER, h).unwrap();
    rdy_store::codec::decode_record(raw, &payload).unwrap()
}

/// The hash a ref holds.
fn ref_hash(s: &Store, name: &str) -> Option<Hash> {
    s.reader()
        .get_ref(name)
        .unwrap()
        .map(|v| Hash(v.try_into().unwrap()))
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

#[test]
fn ap2_a_dispute_and_its_answer_commit_in_one_batch() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, l1, _) = store(d.path());
    let end = s.log_end();
    let (dh, ah) = s
        .put_dispute(&dispute(l1, "the edit is wrong"), Some(relayed()))
        .unwrap();
    let ah = ah.expect("the answer is written");
    assert_eq!(markers_since(&s, end), 1, "one batch, one marker");
    let batch = s.last_batch().to_vec();
    assert!(batch.contains(&(kinds::DISPUTE, dh)), "{batch:?}");
    assert!(batch.contains(&(kinds::ANSWER, ah)), "{batch:?}");
    let pos = |k: u8, h: Hash| batch.iter().position(|x| *x == (k, h)).unwrap();
    assert!(
        pos(kinds::DISPUTE, dh) < pos(kinds::ANSWER, ah),
        "the dispute first"
    );
    let a = stored_answer(&s, &ah);
    assert_eq!(a.item, format!("dispute:{l1}"));
    assert_eq!(a.act, AnswerAct::Dispute);
    assert_eq!(a.targets, vec![dh]);
    assert_eq!(a.relayed_by, relayed().relayed_by, "the relay is kept");
    assert_eq!(ref_hash(&s, &format!("answer/dispute:{l1}")), Some(ah));
    assert_eq!(s.disputes_of(&l1, s.log_end()).unwrap(), vec![dh]);
    assert!(s.disputes_of(&l1, end).unwrap().is_empty(), "not before it");
    // with no answer, the dispute alone
    let end = s.log_end();
    let (second, none) = s
        .put_dispute(&dispute(l1, "and another thing"), None)
        .unwrap();
    assert_eq!(none, None);
    assert_eq!(markers_since(&s, end), 1);
    assert_eq!(s.disputes_of(&l1, s.log_end()).unwrap(), vec![dh, second]);
    s.check_objects().unwrap();
}

#[test]
fn ap2_a_dispute_of_no_landing_or_with_a_bad_reason_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, l1, _) = store(d.path());
    let end = s.log_end();
    let missing = Hash([9; 32]);
    match s.put_dispute(&dispute(missing, "why"), Some(answer())) {
        Err(StoreError::NotFound(h)) => assert_eq!(h, missing),
        other => panic!("a dispute of no landing: {other:?}"),
    }
    match s.put_dispute(&dispute(l1, "   "), None) {
        Err(StoreError::Decode(m)) => assert!(m.ends_with("invalid dispute reason"), "{m}"),
        other => panic!("an empty reason: {other:?}"),
    }
    assert_eq!(s.log_end(), end, "nothing written");
}

#[test]
fn ap2_a_resolution_is_indexed_by_its_dispute() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, l1, l2) = store(d.path());
    let (dh, _) = s.put_dispute(&dispute(l1, "wrong"), None).unwrap();
    let (other, _) = s.put_dispute(&dispute(l2, "wrong too"), None).unwrap();
    let before = s.log_end();
    assert_eq!(s.resolution_of(&dh, before).unwrap(), None);
    let r = DisputeResolution {
        correction: Some(l2),
        ..resolution(dh, DisputeOutcome::Upheld)
    };
    let (rh, ah) = s.put_dispute_resolution(&r, Some(answer())).unwrap();
    let ah = ah.unwrap();
    assert_eq!(markers_since(&s, before), 1, "one batch");
    assert_eq!(s.resolution_of(&dh, s.log_end()).unwrap(), Some(rh));
    assert_eq!(s.resolution_of(&dh, before).unwrap(), None, "not before it");
    assert_eq!(s.resolution_of(&other, s.log_end()).unwrap(), None);
    let a = stored_answer(&s, &ah);
    assert_eq!(a.item, format!("resolve:{dh}"));
    assert_eq!(a.act, AnswerAct::ResolveDispute);
    assert_eq!(a.targets, vec![rh]);
    assert_eq!(ref_hash(&s, &format!("answer/resolve:{dh}")), Some(ah));
    // a dispute the store does not hold: nothing written
    let end = s.log_end();
    let missing = Hash([9; 32]);
    match s.put_dispute_resolution(&resolution(missing, DisputeOutcome::Dismissed), None) {
        Err(StoreError::NotFound(h)) => assert_eq!(h, missing),
        other => panic!("a resolution of no dispute: {other:?}"),
    }
    assert_eq!(s.log_end(), end);
    s.check_objects().unwrap();
}

#[test]
fn ap2_a_second_resolution_is_refused_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, l1, _) = store(d.path());
    let (dh, _) = s.put_dispute(&dispute(l1, "wrong"), None).unwrap();
    let (rh, _) = s
        .put_dispute_resolution(&resolution(dh, DisputeOutcome::Dismissed), None)
        .unwrap();
    let end = s.log_end();
    for outcome in [
        DisputeOutcome::Upheld,
        DisputeOutcome::Dismissed,
        DisputeOutcome::Withdrawn,
    ] {
        match s.put_dispute_resolution(&resolution(dh, outcome), Some(answer())) {
            Err(StoreError::Refused(m)) => {
                assert_eq!(m, format!("dispute {dh} already has a resolution"))
            }
            other => panic!("a second resolution: {other:?}"),
        }
    }
    assert_eq!(s.log_end(), end, "nothing written");
    assert_eq!(s.resolution_of(&dh, s.log_end()).unwrap(), Some(rh));
}

#[test]
fn ap2_disputes_of_lists_every_dispute_of_a_landing_in_log_order() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, l1, l2) = store(d.path());
    let mut of_l1 = vec![];
    let mut all = vec![];
    for (n, landing) in [l1, l2, l1, l1, l2, l1].into_iter().enumerate() {
        let (h, _) = s
            .put_dispute(&dispute(landing, &format!("reason {n}")), None)
            .unwrap();
        if landing == l1 {
            of_l1.push(h);
        }
        all.push(h);
    }
    let mut sorted = of_l1.clone();
    sorted.sort();
    assert_ne!(sorted, of_l1, "log order is not hash order here");
    let end = s.log_end();
    assert_eq!(s.disputes_of(&l1, end).unwrap(), of_l1);
    assert_eq!(s.disputes_of(&l2, end).unwrap(), vec![all[1], all[4]]);
    assert!(s.disputes_of(&Hash([9; 32]), end).unwrap().is_empty());
    // every dispute, in log order, at most `max`
    assert_eq!(s.all_disputes(end, 1_000).unwrap(), (all.clone(), 0));
    assert_eq!(s.all_disputes(end, 2).unwrap(), (all[..2].to_vec(), 4));
    assert_eq!(s.all_disputes(end, 0).unwrap(), (vec![], 6));
    s.reindex().unwrap();
    assert_eq!(s.disputes_of(&l1, end).unwrap(), of_l1, "a rebuild agrees");
    assert_eq!(s.all_disputes(end, 1_000).unwrap(), (all, 0));
}

#[test]
fn ap2_the_first_dispute_activates_the_dispute_feature() {
    let d = tempfile::tempdir().unwrap();
    let (_path, mut s, l1, _) = store(d.path());
    assert!(!manifest(&s).contains(&"dispute".to_string()));
    let end = s.log_end();
    s.put_dispute(&dispute(l1, "wrong"), Some(answer()))
        .unwrap();
    assert_eq!(markers_since(&s, end), 1, "the feature joins the batch");
    assert!(manifest(&s).contains(&"dispute".to_string()));
    assert!(
        s.reader()
            .store_features()
            .unwrap()
            .features
            .iter()
            .any(|f| f.name == "dispute")
    );
}

/// A closed store at `<dir>/s` with disputes of both landings and a resolution.
fn closed_with_disputes(dir: &Path) -> (PathBuf, Hash, Hash, Hash) {
    let (path, mut s, l1, l2) = store(dir);
    let (d1, _) = s
        .put_dispute(&dispute(l1, "wrong"), Some(answer()))
        .unwrap();
    s.put_dispute(&dispute(l2, "wrong too"), None).unwrap();
    let (r1, _) = s
        .put_dispute_resolution(&resolution(d1, DisputeOutcome::Withdrawn), Some(answer()))
        .unwrap();
    s.check_objects().unwrap();
    s.close().unwrap();
    (path, l1, d1, r1)
}

/// Removes every row of the index table `table` of the closed store at `path`.
fn clear_table(path: &Path, table: &str) {
    let raw = redb::Database::create(path.join("index.redb")).unwrap();
    let w = raw.begin_write().unwrap();
    {
        let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(table);
        let mut t = w.open_table(def).unwrap();
        let keys: Vec<Vec<u8>> = redb::ReadableTable::iter(&t)
            .unwrap()
            .map(|e| e.unwrap().0.value().to_vec())
            .collect();
        assert!(!keys.is_empty(), "{table} has rows");
        for k in keys {
            t.remove(&k[..]).unwrap();
        }
    }
    w.commit().unwrap();
}

#[test]
fn ap2_fsck_reports_a_dispute_table_that_differs() {
    for table in ["dispute_landings", "dispute_resolutions"] {
        let d = tempfile::tempdir().unwrap();
        let (path, l1, d1, r1) = closed_with_disputes(d.path());
        clear_table(&path, table);
        let mut s = Store::open(&path).unwrap();
        assert!(!s.open_report().index_rebuilt, "the index opens as it is");
        match s.check_objects() {
            Err(StoreError::Decode(m)) => assert_eq!(m, DIFFER, "{table}"),
            other => panic!("fsck passed a wrong {table}: {other:?}"),
        }
        s.reindex().unwrap();
        s.check_objects().unwrap();
        assert_eq!(s.disputes_of(&l1, s.log_end()).unwrap(), vec![d1]);
        assert_eq!(s.resolution_of(&d1, s.log_end()).unwrap(), Some(r1));
    }
}

#[test]
fn ap2_an_index_without_the_dispute_tables_is_rebuilt_at_open() {
    for table in ["dispute_landings", "dispute_resolutions"] {
        let d = tempfile::tempdir().unwrap();
        let (path, l1, d1, r1) = closed_with_disputes(d.path());
        let dump = Store::open(&path).unwrap().index_dump_for_tests().unwrap();
        let file = path.join("index.redb");
        {
            let raw = redb::Database::create(&file).unwrap();
            let w = raw.begin_write().unwrap();
            let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(table);
            assert!(w.delete_table(def).unwrap(), "the table was there");
            w.commit().unwrap();
        }
        match Index::open(&file) {
            Err(StoreError::Db(m)) => assert_eq!(
                m,
                "the index has no dispute tables; it is rebuilt from the log"
            ),
            Err(e) => panic!("another refusal: {e}"),
            Ok(_) => panic!("an index without {table} opened"),
        }
        let s = Store::open(&path).unwrap();
        assert!(s.open_report().index_rebuilt, "a missing table is rebuilt");
        assert_eq!(s.index_dump_for_tests().unwrap(), dump);
        assert_eq!(s.disputes_of(&l1, s.log_end()).unwrap(), vec![d1]);
        assert_eq!(s.resolution_of(&d1, s.log_end()).unwrap(), Some(r1));
        s.check_objects().unwrap();
    }
}

/// A load of the closed store at `src` into `to`: every sealed batch ingested at its place, as
/// a dump of this rdy's format carries it, then the store opened.
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
    i.upgrade(&rdy_store::format::DumpStore::Features(features))
        .unwrap();
    i.finish().unwrap();
    Store::open(to).unwrap()
}

#[test]
fn ap2_a_dump_and_load_keep_the_disputes_and_their_index() {
    let d = tempfile::tempdir().unwrap();
    let (path, l1, d1, r1) = closed_with_disputes(d.path());
    let src = Store::open(&path).unwrap();
    let want = src.index_dump_for_tests().unwrap();
    let all = src.all_disputes(src.log_end(), 1_000).unwrap();
    drop(src);
    let loaded = load(&path, &d.path().join("loaded"));
    loaded.check_objects().unwrap();
    let got = loaded.index_dump_for_tests().unwrap();
    assert_eq!(got.dispute_landings.len(), 2);
    assert_eq!(got.dispute_resolutions.len(), 1);
    assert_eq!(got.dispute_landings, want.dispute_landings);
    assert_eq!(got.dispute_resolutions, want.dispute_resolutions);
    let end = loaded.log_end();
    assert_eq!(loaded.disputes_of(&l1, end).unwrap(), vec![d1]);
    assert_eq!(loaded.resolution_of(&d1, end).unwrap(), Some(r1));
    assert_eq!(loaded.all_disputes(end, 1_000).unwrap(), all);
    assert!(loaded.disputes_of(&l1, ORIGIN).unwrap().is_empty());
}
