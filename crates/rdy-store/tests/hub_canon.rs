//! scc1 (D-DZDB7, D-R80JP, D-47HVF): a holder's canon. The feature `hub_canon` opens `canon`,
//! `view/`, `held/` and `replaced/` in a holder; five checked moves write them; a failed
//! compare writes nothing; each ref family is closed on write, replay, ingest and fsck.
//!
//! The pushed objects are built in a scratch holder (`Src`) by hand, as the holder check's own
//! tests build them, and copied frame by frame into a move batch.
use rdy_model::*;
use rdy_store::dump::DumpBatch;
use rdy_store::hub_canon::{
    self, CANON_REF, CanonMove, HOLD_REASON, Moved, Replaced, ReplacedKind, SEED_REASON, held_ref,
    replaced_ref,
};
use rdy_store::ingest::Ingest;
use rdy_store::segment::Location;
use rdy_store::view::{Owner, ViewHead, ViewOp, ViewOpKind};
use rdy_store::{Store, StoreError, codec, kinds};
use std::collections::BTreeMap;
use std::path::Path;

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// The writer's clock of every move here (injected).
const AT: i64 = 1_700_000_000_123;

fn prov(at: i64) -> Provenance {
    Provenance {
        actor: "human:a".into(),
        run: None,
        role: Role::Approver,
        at_ms: at,
    }
}

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "a".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn change(ops: Vec<Op>, deps: Vec<Hash>, message: &str) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps,
        provenance: prov(1),
        justification: None,
        message: message.into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn landing_record(
    proposal: Hash,
    prev: Option<Hash>,
    state: Hash,
    changes: &[Hash],
) -> LandingRecord {
    LandingRecord {
        proposal,
        prev,
        state,
        approved_by: prov(3),
        changes: changes.to_vec(),
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: None,
        rebased_from: vec![],
        evidence: None,
        assessment: None,
        policy: None,
        read_identity: None,
    }
}

fn holder(dir: &Path, name: &str) -> Store {
    Store::create_holder(&dir.join(name), RepoId([7; 16])).unwrap()
}

/// A scratch holder whose writes are pushed: each step's new frames are one move batch.
struct Src {
    s: Store,
    /// The blocks of each landing's state.
    states: BTreeMap<Hash, Vec<Block>>,
}

type Records = Vec<(u8, Vec<u8>)>;

impl Src {
    fn new(dir: &Path, name: &str) -> Src {
        Src {
            s: holder(dir, name),
            states: BTreeMap::new(),
        }
    }

    /// The sync frames `f` writes, in log order.
    fn capture(&mut self, f: impl FnOnce(&mut Src)) -> Records {
        let from = self.s.log_end();
        f(self);
        let mut out = vec![];
        self.s
            .reader()
            .for_each_frame_from(from, self.s.log_end(), |_, raw, _, payload, _| {
                if rdy_store::sync_stream::is_sync_kind(codec::logical_kind(raw)) {
                    out.push((raw, payload.to_vec()));
                }
                Ok(())
            })
            .unwrap();
        out
    }

    /// Lands one change that creates block `id` with `body` on `prev`: the block, the change,
    /// the snapshot, the proposal, the landing and its delta. Returns the landing and the
    /// frames.
    fn land(&mut self, prev: Option<Hash>, id: &str, body: &str) -> (Hash, Records) {
        let mut landing = Hash([0; 32]);
        let recs = self.capture(|src| {
            let b = block(id, body);
            let s = &mut src.s;
            s.put(kinds::BLOCK, &b).unwrap();
            let deps = prev
                .map(|p| s.reader().landing(&p).unwrap().changes)
                .unwrap_or_default();
            let c = s
                .put(
                    kinds::CHANGE,
                    &change(vec![Op::Create { block: b.clone() }], deps, body),
                )
                .unwrap();
            let mut blocks = prev.map(|p| src.states[&p].clone()).unwrap_or_default();
            blocks.push(b);
            let state = StateSnapshot {
                blocks: blocks
                    .iter()
                    .map(|b| (b.id.clone(), b.version_hash()))
                    .collect(),
            };
            let state = src.s.put(kinds::SNAPSHOT, &state).unwrap();
            let proposal = Proposal {
                stable: None,
                prev: None,
                title: body.into(),
                view: "work".into(),
                changes: vec![c],
                alternative: None,
                provenance: prov(2),
                canon_at: prev,
                summary: None,
            };
            let proposal = src.s.put(kinds::PROPOSAL, &proposal).unwrap();
            let rec = landing_record(proposal, prev, state, &[c]);
            landing = src.s.put_landing_for_tests(&rec).unwrap();
            src.states.insert(landing, blocks);
        });
        (landing, recs)
    }

    /// Writes view op `op` of view `view` after `prev`; returns its id and frames.
    fn op(
        &mut self,
        view: &str,
        prev: Option<Hash>,
        op: ViewOpKind,
        actor: &str,
    ) -> (Hash, Records) {
        let mut id = Hash([0; 32]);
        let recs = self.capture(|src| {
            let o = ViewOp {
                view: view.into(),
                prev,
                op,
                actor: actor.into(),
                run: None,
                at_ms: 40,
            };
            id = src.s.put(kinds::VIEW_OP, &o).unwrap();
        });
        (id, recs)
    }
}

fn cat(parts: &[&Records]) -> Records {
    parts.iter().flat_map(|r| r.iter().cloned()).collect()
}

/// Checks `records` with `head` against `s` and moves.
fn mv(
    s: &mut Store,
    records: Records,
    head: Hash,
    m: CanonMove,
    expected: Option<Hash>,
) -> Result<Moved, StoreError> {
    let batch = s.reader().check_move_batch(records, head)?;
    s.move_canon(&batch, &m, expected, AT)
}

fn refused(r: Result<Moved, StoreError>) -> String {
    match r {
        Err(StoreError::Refused(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Every ref of `s` and its log end: what a move that writes nothing leaves.
/// A store's log end, refs and feature names.
type State = (Location, Vec<(String, Vec<u8>)>, Vec<String>);

fn state(s: &Store) -> State {
    let features = s
        .features()
        .features
        .iter()
        .map(|f| f.name.clone())
        .collect();
    (
        s.log_end(),
        s.reader().refs_with_prefix("").unwrap(),
        features,
    )
}

fn has_hub(s: &Store) -> bool {
    s.features().has(rdy_store::format::FEATURE_HUB_CANON)
}

fn format_has_hub(dir: &Path) -> bool {
    rdy_store::format::read_format_file(dir)
        .unwrap()
        .unwrap()
        .has(rdy_store::format::FEATURE_HUB_CANON)
}

#[test]
fn scc1_feature_first_ref_and_rollback_are_one_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = Src::new(dir.path(), "src");
    let (l1, r1) = src.land(None, "b-00001", "one");
    let mut h = holder(dir.path(), "h");
    let hdir = dir.path().join("h");
    // an upload alone never lists the feature
    h.write(r1.clone()).unwrap();
    assert!(!has_hub(&h));
    assert!(!format_has_hub(&hdir));
    // a move whose sync fails leaves objects, refs, features, FORMAT and the index as they were
    let before = state(&h);
    let index = h.index_dump_for_tests().unwrap();
    let batch = h.reader().check_move_batch(vec![], l1).unwrap();
    h.fail_next_sync();
    assert!(h.move_canon(&batch, &CanonMove::Seed, None, AT).is_err());
    assert_eq!(state(&h), before);
    assert_eq!(h.index_dump_for_tests().unwrap(), index);
    assert!(!has_hub(&h));
    assert!(!format_has_hub(&hdir));
    assert_eq!(h.canon_head().unwrap(), None);
    // the move that lands: the feature ref and canon in the one batch it writes
    let from = h.log_end();
    let moved = h.move_canon(&batch, &CanonMove::Seed, None, AT).unwrap();
    assert_eq!(moved, Moved::Moved { tip: l1 });
    assert_eq!(h.canon_head().unwrap(), Some(l1));
    assert!(has_hub(&h));
    assert!(format_has_hub(&hdir));
    let mut frames = vec![];
    h.reader()
        .for_each_frame_from(from, h.log_end(), |_, raw, _, payload, _| {
            frames.push((codec::logical_kind(raw), payload.to_vec()));
            Ok(())
        })
        .unwrap();
    let names: Vec<String> = frames
        .iter()
        .filter(|(k, _)| *k == kinds::REF_UPDATE)
        .map(|(_, p)| postcard::from_bytes::<kinds::RefUpdate>(p).unwrap().name)
        .collect();
    assert_eq!(names, [CANON_REF, rdy_store::format::REF_STORE_FEATURES]);
    let markers = frames
        .iter()
        .filter(|(k, _)| *k == kinds::BATCH_COMMIT)
        .count();
    assert_eq!(markers, 1, "one batch");
    // a public write of a hub ref is refused in a holder
    let w = h.set_refs(vec![(CANON_REF.into(), l1.0.to_vec())]);
    assert!(
        matches!(&w, Err(StoreError::Refused(m)) if m == "a holder store cannot write ref canon"),
        "{w:?}"
    );
    // fsck and a reindex agree
    h.check_holder().unwrap();
    h.reindex().unwrap();
    assert_eq!(h.canon_head().unwrap(), Some(l1));
    // a later move lists nothing new
    let (l2, r2) = src.land(Some(l1), "b-00002", "two");
    let from = h.log_end();
    mv(&mut h, r2, l2, CanonMove::Extend, Some(l1)).unwrap();
    let mut features_refs = 0;
    h.reader()
        .for_each_frame_from(from, h.log_end(), |_, raw, _, payload, _| {
            if codec::logical_kind(raw) == kinds::REF_UPDATE
                && postcard::from_bytes::<kinds::RefUpdate>(payload)
                    .unwrap()
                    .name
                    == rdy_store::format::REF_STORE_FEATURES
            {
                features_refs += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(features_refs, 0);
}

/// Every batch of `s`'s log.
fn batches(s: &Store) -> Vec<DumpBatch> {
    let mut out = vec![];
    s.reader()
        .for_each_batch_after(ORIGIN, s.log_end(), &mut |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

/// The text of the first refusal an ingest of `s`'s batches meets.
fn ingest_refusal(s: &Store, to: &Path) -> String {
    let mut i = Ingest::create(to).unwrap();
    for (n, b) in batches(s).iter().enumerate() {
        if let Err(e) = i.batch(n as u64, b) {
            return e.to_string();
        }
    }
    panic!("the ingest took every batch");
}

#[test]
fn scc1_holder_ref_families_stay_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = Src::new(dir.path(), "src");
    let (l1, r1) = src.land(None, "b-00001", "one");
    let (l2, r2) = src.land(Some(l1), "b-00002", "two");
    let (create, rc) = src.op(
        "main",
        None,
        ViewOpKind::Create {
            base: None,
            owner: None,
            pinned: false,
        },
        "human:a",
    );
    // each case: a holder with canon at l1 (the feature listed), then one forged ref
    let mark = |kind: u8, by: Hash, tail: &[u8]| {
        let mut v = postcard::to_allocvec(&(kind, by, AT)).unwrap();
        v.extend_from_slice(tail);
        v
    };
    let cases: Vec<(&str, String, Vec<u8>, String)> = vec![
        (
            "a nonempty held value",
            held_ref(&l2),
            vec![1],
            "the value of a held ref is empty, not 1 bytes".into(),
        ),
        (
            "a held name that is not hex",
            format!("held/{}", "G".repeat(64)),
            vec![],
            "the name is not 64 lowercase hex digits".into(),
        ),
        (
            "a malformed mark",
            replaced_ref(&l1),
            vec![0, 1, 2],
            "not a replaced mark".into(),
        ),
        (
            "an unknown kind",
            replaced_ref(&l1),
            mark(2, l2, &[]),
            "kind 2 is not landing (0) or view op (1)".into(),
        ),
        (
            "trailing bytes",
            replaced_ref(&l1),
            mark(0, l2, &[9]),
            "1 trailing bytes after the replaced mark".into(),
        ),
        (
            "a mark whose target is of another kind",
            replaced_ref(&l1),
            mark(1, create, &[]),
            format!("it names view op {l1}, which is not stored"),
        ),
        (
            "a mark whose replacement is not stored",
            replaced_ref(&l1),
            mark(0, Hash([3; 32]), &[]),
            format!("it names landing {}, which is not stored", Hash([3; 32])),
        ),
        (
            "a canon that is no landing",
            CANON_REF.into(),
            create.0.to_vec(),
            format!("it names landing {create}, which is not stored"),
        ),
        (
            "a view head with an owner",
            "view/main".into(),
            postcard::to_allocvec(&ViewHead {
                head: create,
                base: None,
                len: 0,
                owner: Some(Owner {
                    actor: "human:a".into(),
                    run: None,
                }),
                pinned: false,
                closed: false,
                last_write_ms: 40,
            })
            .unwrap(),
            "a holder's view head has no owner and no pin".into(),
        ),
    ];
    for (i, (what, name, value, want)) in cases.into_iter().enumerate() {
        let mut h = holder(dir.path(), &format!("h{i}"));
        h.write(cat(&[&r1, &r2, &rc])).unwrap();
        mv(&mut h, vec![], l1, CanonMove::Seed, None).unwrap();
        h.set_refs_unchecked_for_tests(vec![(name.clone(), value)])
            .unwrap();
        // fsck
        let text = h.check_holder().unwrap_err().to_string();
        assert!(
            text.contains(&name) && text.contains(&want),
            "{what}: fsck: {text}"
        );
        // ingest of the same log
        let text = ingest_refusal(&h, &dir.path().join(format!("in{i}")));
        assert!(
            text.contains(&name) && text.contains(&want),
            "{what}: ingest: {text}"
        );
        // replay: a reindex
        let text = h.reindex().unwrap_err().to_string();
        assert!(
            text.contains(&name) && text.contains(&want),
            "{what}: replay: {text}"
        );
    }
    // a hub ref in a holder that never listed the feature
    let mut h = holder(dir.path(), "plain");
    h.write(r1.clone()).unwrap();
    h.set_refs_unchecked_for_tests(vec![(held_ref(&l1), vec![])])
        .unwrap();
    let want = format!("holder ref {} is not allowed", held_ref(&l1));
    let text = h.check_holder().unwrap_err().to_string();
    assert!(text.contains(&want), "fsck: {text}");
    let text = ingest_refusal(&h, &dir.path().join("in-plain"));
    assert!(text.contains(&want), "ingest: {text}");
    let text = h.reindex().unwrap_err().to_string();
    assert!(text.contains(&want), "replay: {text}");
    // a well-formed mark decodes back, and its encoding is the postcard of its three fields
    let m = Replaced {
        kind: ReplacedKind::ViewOp,
        by: create,
        at_ms: AT,
    };
    assert_eq!(m.encode(), mark(1, create, &[]));
    assert_eq!(Replaced::decode(&m.encode()).unwrap(), m);
    assert!(hub_canon::is_hub_ref("view/main") && !hub_canon::is_hub_ref("tag/v1"));
}

#[test]
fn scc1_seed_extend_force_hold_and_view_check_chains() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = Src::new(dir.path(), "src");
    let (l1, r1) = src.land(None, "b-00001", "one");
    let (l2, r2) = src.land(Some(l1), "b-00002", "two");
    let (l2b, r2b) = src.land(Some(l1), "b-00003", "two-b");
    let (f1, rf1) = src.land(None, "b-00004", "other");
    let mut h = holder(dir.path(), "h");
    // seed: a complete chain, from an upload history the holder already holds
    h.write(r1.clone()).unwrap();
    assert_eq!(
        mv(&mut h, vec![], l1, CanonMove::Seed, None).unwrap(),
        Moved::Moved { tip: l1 }
    );
    // extend: the chain holds the tip
    assert_eq!(
        mv(&mut h, r2.clone(), l2, CanonMove::Extend, Some(l1)).unwrap(),
        Moved::Moved { tip: l2 }
    );
    assert_eq!(h.canon_head().unwrap(), Some(l2));
    // hold: a chain that holds a landing of canon; only held/<head> is written
    let before = h.canon_head().unwrap();
    assert_eq!(
        mv(&mut h, r2b.clone(), l2b, CanonMove::Hold, Some(l2)).unwrap(),
        Moved::Moved { tip: l2b }
    );
    assert_eq!(h.canon_head().unwrap(), before);
    assert_eq!(h.reader().get_ref(&held_ref(&l2b)).unwrap(), Some(vec![]));
    // force: a chain that does not hold the tip; the tip is marked replaced, at the writer's clock
    assert_eq!(
        mv(&mut h, rf1.clone(), f1, CanonMove::Force, Some(l2)).unwrap(),
        Moved::Moved { tip: f1 }
    );
    assert_eq!(h.canon_head().unwrap(), Some(f1));
    let mark = h.reader().get_ref(&replaced_ref(&l2)).unwrap().unwrap();
    assert_eq!(
        Replaced::decode(&mark).unwrap(),
        Replaced {
            kind: ReplacedKind::Landing,
            by: f1,
            at_ms: AT
        }
    );
    // a view: a new view's chain from its root, then an extension of the hub's head
    let (create, rc) = src.op(
        "main",
        None,
        ViewOpKind::Create {
            base: None,
            owner: None,
            pinned: false,
        },
        "human:a",
    );
    let (close, rclose) = src.op(
        "main",
        Some(create),
        ViewOpKind::Close {
            reason: "done".into(),
        },
        "human:a",
    );
    let set = |root| CanonMove::SetViewHead {
        name: "main".into(),
        root,
    };
    assert_eq!(
        mv(&mut h, cat(&[&rc, &rclose]), close, set(create), None).unwrap(),
        Moved::Moved { tip: close }
    );
    let head = h.view_head("main").unwrap().unwrap();
    assert_eq!(
        head,
        ViewHead {
            head: close,
            base: None,
            len: 0,
            owner: None,
            pinned: false,
            closed: true,
            last_write_ms: 40,
        }
    );
    let (undo, rundo) = src.op(
        "main",
        Some(close),
        ViewOpKind::Undo { target: close },
        "human:a",
    );
    assert_eq!(
        mv(&mut h, rundo, undo, set(create), Some(close)).unwrap(),
        Moved::Moved { tip: undo }
    );
    let head = h.view_head("main").unwrap().unwrap();
    assert_eq!((head.head, head.closed), (undo, false));
    // every root stays: fsck, then a reindex that gives the same index
    h.check_holder().unwrap();
    let index = h.index_dump_for_tests().unwrap();
    h.reindex().unwrap();
    assert_eq!(h.index_dump_for_tests().unwrap(), index);
    h.check_holder().unwrap();
}

#[test]
fn scc1_two_moves_from_one_base_have_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = Src::new(dir.path(), "src");
    let (l1, r1) = src.land(None, "b-00001", "one");
    let (a, ra) = src.land(Some(l1), "b-00002", "a");
    let (b, rb) = src.land(Some(l1), "b-00003", "b");
    let mut h = holder(dir.path(), "h");
    mv(&mut h, r1, l1, CanonMove::Seed, None).unwrap();
    // both checked against the same base
    let pa = h.reader().check_move_batch(ra, a).unwrap();
    let pb = h.reader().check_move_batch(rb, b).unwrap();
    assert_eq!(
        h.move_canon(&pa, &CanonMove::Extend, Some(l1), AT).unwrap(),
        Moved::Moved { tip: a }
    );
    // the loser's compare fails under the writer: no error, nothing written, the tip it found
    let before = state(&h);
    assert_eq!(
        h.move_canon(&pb, &CanonMove::Extend, Some(l1), AT).unwrap(),
        Moved::Behind { tip: Some(a) }
    );
    assert_eq!(state(&h), before);
    assert!(!h.reader().has_kind(kinds::LANDING, &b).unwrap());
    // a seed against a canon that is there is behind too
    assert_eq!(
        h.move_canon(&pb, &CanonMove::Seed, None, AT).unwrap(),
        Moved::Behind { tip: Some(a) }
    );
    assert_eq!(state(&h), before);
    // the loser goes on from the tip it was given: a force onto it wins
    assert_eq!(
        h.move_canon(&pb, &CanonMove::Force, Some(a), AT).unwrap(),
        Moved::Moved { tip: b }
    );
    h.check_holder().unwrap();
}

#[test]
fn scc1_move_chain_refusal_reasons_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    let mut src = Src::new(dir.path(), "src");
    let (l1, r1) = src.land(None, "b-00001", "one");
    let (l2, r2) = src.land(Some(l1), "b-00002", "two");
    let (f1, rf1) = src.land(None, "b-00004", "other");
    let mut h = holder(dir.path(), "h");
    h.write(cat(&[&r1, &rf1])).unwrap();
    // seed: a stored landing whose prev the holder does not hold (an upload history cut short)
    let orphan = {
        let rec = landing_record(
            src.s.reader().landing(&l1).unwrap().proposal,
            Some(Hash([9; 32])),
            src.s.reader().landing(&l1).unwrap().state,
            &[],
        );
        h.put(kinds::LANDING, &rec).unwrap()
    };
    let before = state(&h);
    assert_eq!(
        refused(mv(&mut h, vec![], orphan, CanonMove::Seed, None)),
        SEED_REASON
    );
    assert_eq!(state(&h), before);
    // F8: a pushed landing without its delta
    let no_delta: Records = r2
        .iter()
        .filter(|(k, _)| codec::logical_kind(*k) != kinds::SNAPSHOT_DELTA)
        .cloned()
        .collect();
    let text = match h.reader().check_move_batch(no_delta, l2) {
        Err(StoreError::Refused(m)) => m,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        text,
        format!("landing {l2}: expected exactly one snapshot delta")
    );
    mv(&mut h, vec![], l1, CanonMove::Seed, None).unwrap();
    // extend: a chain that does not hold the tip
    assert_eq!(
        refused(mv(&mut h, vec![], f1, CanonMove::Extend, Some(l1))),
        hub_canon::extend_reason(&l1)
    );
    // force: a chain that holds the tip
    let (l3, r3) = src.land(Some(l1), "b-00003", "three");
    assert_eq!(
        refused(mv(&mut h, r3.clone(), l3, CanonMove::Force, Some(l1))),
        hub_canon::force_reason(&l1)
    );
    // hold: a chain with no landing of canon
    assert_eq!(
        refused(mv(&mut h, vec![], f1, CanonMove::Hold, Some(l1))),
        HOLD_REASON
    );
    // the compare comes first: a stale expected tip is behind, whatever the chain
    let before = state(&h);
    assert_eq!(
        mv(&mut h, vec![], f1, CanonMove::Extend, Some(f1)).unwrap(),
        Moved::Behind { tip: Some(l1) }
    );
    assert_eq!(state(&h), before);
    // views: the hub's head is not held, and a root that is not the view's
    let op = |src: &mut Src, prev, op, actor: &str| src.op("main", prev, op, actor);
    let create_op = || ViewOpKind::Create {
        base: None,
        owner: None,
        pinned: false,
    };
    let (create, rc) = op(&mut src, None, create_op(), "human:a");
    let (close, rclose) = op(
        &mut src,
        Some(create),
        ViewOpKind::Close { reason: "x".into() },
        "human:a",
    );
    let (own, rown) = op(
        &mut src,
        Some(create),
        ViewOpKind::Own { owner: None },
        "human:a",
    );
    let (other, rother) = op(&mut src, None, create_op(), "human:b");
    let set = |root| CanonMove::SetViewHead {
        name: "main".into(),
        root,
    };
    mv(&mut h, cat(&[&rc, &rclose]), close, set(create), None).unwrap();
    let before = state(&h);
    assert_eq!(
        refused(mv(&mut h, rown, own, set(create), Some(close))),
        hub_canon::view_head_reason(&close)
    );
    assert_eq!(
        refused(mv(&mut h, rother.clone(), other, set(create), Some(close))),
        hub_canon::view_root_reason(&other, &create, "main")
    );
    assert_eq!(
        refused(mv(&mut h, rother, other, set(other), Some(close))),
        hub_canon::view_root_reason(&other, &create, "main")
    );
    assert_eq!(state(&h), before);
    // a view push whose expected head is stale is behind
    assert_eq!(
        mv(&mut h, vec![], close, set(create), None).unwrap(),
        Moved::Behind { tip: Some(close) }
    );
    // every refusal is E_BAD_INPUT at the engine (pinned in the QA suite); here: `Refused`
    let _ = r1;
    let _ = l2;
}
