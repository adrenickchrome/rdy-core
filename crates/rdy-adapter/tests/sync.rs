//! scc6 (D-WDKWQ, D-0WWYR, D-QMZ92, D-H0BZN, D-8RGWW, D-DZDB7, D-R80JP, D-4XHPH, D-47HVF): the hub
//! side of a sync through the adapter. A hub checks the batch files of a push into a checked
//! quarantine bound to one snapshot, reads its chain facts and the prospective minimum rdy,
//! chooses a move and makes it through the store's one writer, which compares the expected tip or
//! head and checks the move's chain rule. It checks a stored landing with the pack and settings it
//! passes, and reads captured view heads. A project captures the render inputs of a tag and the
//! gate inputs of a push.
//!
//! The source projects are built with core calls and closed before an adapter handle opens a
//! store. Their format-4 batches come from the adapter's own export. Each core call that a reply
//! stands for is an oracle only.

use rdy_adapter::landing_check::{CheckInputs, LandingCheck};
use rdy_adapter::rendering::RenderInputs;
use rdy_adapter::stream::{ObjectKey, StreamPurpose};
use rdy_adapter::sync::{CanonMove, ChainFacts, CheckedQuarantine, Moved, RdyVersion, ViewRead};
use rdy_adapter::{Hash, RdyError, RepoId, Snapshot, StoreHandle};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest};
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The writer's clock of every move (pinned on the calling thread).
const AT: i64 = 1_763_164_800_000;

// ---------------------------------------------------------------------------------------------
// source projects (core)

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write_in(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "scc6".into(),
        },
    )
    .unwrap()
}

/// Proposes view `view` and lands it; returns the landing.
fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), view, view, Some("test summary"))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"));
    r.land(&session(Role::Approver), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:#?}"))
        .parse()
        .unwrap()
}

/// A doc with a section and one prose block, written in view `view`.
fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) -> rdy_engine::WriteResult {
    write_in(
        r,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": title, "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": body}
        ]),
    )
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn project(dir: &Path) -> Rdy {
    std::fs::create_dir_all(dir).unwrap();
    drop(Rdy::init(dir, "rdy-standard").unwrap());
    Rdy::open(dir).unwrap()
}

fn repo_of(r: &Rdy) -> RepoId {
    r.store().reader().repo_id().unwrap().unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

// ---------------------------------------------------------------------------------------------
// the hub's side: adapter calls only

/// Writes the format-4 history of `purpose` at `root` (less `base`'s closure) of the project at
/// `dir` to `out`, through an adapter handle of the project.
fn export(dir: &Path, purpose: StreamPurpose, root: Hash, base: Option<Hash>, out: &Path) {
    let h = StoreHandle::open(dir).unwrap();
    let snap = h.snapshot().unwrap();
    let file = File::create(out).unwrap();
    snap.write_history(purpose, root, base, &BTreeSet::<ObjectKey>::new(), file)
        .unwrap();
    drop(snap);
    h.close().unwrap();
}

fn files(paths: &[&Path]) -> Vec<File> {
    paths.iter().map(|p| File::open(p).unwrap()).collect()
}

fn check(snap: &Snapshot, paths: &[&Path]) -> Result<CheckedQuarantine, RdyError> {
    snap.check_quarantine(&files(paths))
}

fn moved(tip: Hash) -> Moved {
    Moved::Moved { tip }
}

/// One world of pushes. Project P lands `p1` (view `w0`), then `p2` (view `w1`); Q, a copy of P
/// at `p1`, lands `q2` (a sibling of `p2`). P keeps an open view `draft` with work in flight
/// whose ops are `d1` and then `d2`.
struct World {
    dir: tempfile::TempDir,
    repo: RepoId,
    p1: Hash,
    p2: Hash,
    q2: Hash,
    d1: Hash,
    d2: Hash,
    draft_root: Hash,
    /// canon history of P at p1, of P at p2 less p1, and of Q at q2 less p1
    e1: PathBuf,
    e2: PathBuf,
    eq: PathBuf,
    /// view history of `draft` at d1, and at d2 less d1
    v1: PathBuf,
    v2: PathBuf,
}

impl World {
    fn p(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn holder(&self, name: &str) -> StoreHandle {
        StoreHandle::create_holder(&self.dir.path().join(name), self.repo).unwrap()
    }
}

fn view_head_of(r: &Rdy, name: &str) -> Hash {
    r.store().reader().view_head(name).unwrap().unwrap().head
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let qd = dir.path().join("q");
    let mut p = project(&pd);
    let repo = repo_of(&p);
    doc(&mut p, "w0", "Canon", "The hub keeps one canon.");
    let p1 = land(&mut p, "w0");
    p.close().unwrap();
    copy_dir(&pd, &qd);
    let mut p = Rdy::open(&pd).unwrap();
    doc(&mut p, "draft", "Draft", "Work in flight.");
    let d1 = view_head_of(&p, "draft");
    let mut draft_root = d1;
    while let Some(prev) = p.store().reader().view_op(&draft_root).unwrap().prev {
        draft_root = prev;
    }
    doc(&mut p, "w1", "Extend", "The canon moves forward.");
    let p2 = land(&mut p, "w1");
    doc(&mut p, "draft", "Draft two", "More work in flight.");
    let d2 = view_head_of(&p, "draft");
    p.close().unwrap();
    let mut q = Rdy::open(&qd).unwrap();
    doc(&mut q, "w2", "Held", "A chain the hub holds beside canon.");
    let q2 = land(&mut q, "w2");
    q.close().unwrap();
    let at = |n: &str| dir.path().join(format!("{n}.rdysync"));
    let canon = StreamPurpose::CanonHistory;
    let view = StreamPurpose::ViewHistory;
    export(&pd, canon, p1, None, &at("e1"));
    export(&pd, canon, p2, Some(p1), &at("e2"));
    export(&qd, canon, q2, Some(p1), &at("eq"));
    export(&pd, view, d1, None, &at("v1"));
    export(&pd, view, d2, Some(d1), &at("v2"));
    World {
        repo,
        p1,
        p2,
        q2,
        d1,
        d2,
        draft_root,
        e1: at("e1"),
        e2: at("e2"),
        eq: at("eq"),
        v1: at("v1"),
        v2: at("v2"),
        dir,
    }
}

/// Seeds `h` with `p1` from a fresh snapshot.
fn seed(h: &StoreHandle, w: &World) {
    let snap = h.snapshot().unwrap();
    let c = check(&snap, &[&w.e1]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Seed, None).unwrap(),
        moved(w.p1)
    );
}

// ---------------------------------------------------------------------------------------------
// the moves

/// D-47HVF through the adapter alone: seed, extend, hold, set a view head and force, each from a
/// checked quarantine the hub made of the adapter's own format-4 batches, and each move's refs
/// read back from a new snapshot.
#[test]
fn scc6_adapter_only_consumer_uses_all_moves() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    let empty = h.snapshot().unwrap();
    assert_eq!(empty.canon(), None);

    // seed: the facts name the push before any verdict
    let c1 = check(&empty, &[&w.e1]).unwrap();
    assert_eq!(c1.facts().repo_id, w.repo);
    assert_eq!(c1.facts().purpose, StreamPurpose::CanonHistory);
    assert_eq!(c1.facts().head, w.p1);
    assert!(c1.facts().view.is_none());
    assert!(!c1.facts().objects.is_empty());
    assert_eq!(
        empty.canon_chain(&c1).unwrap(),
        ChainFacts {
            head: None,
            entered: vec![w.p1],
            holds_expected: false,
            replaced: vec![],
        }
    );
    assert_eq!(
        h.move_canon(&c1, CanonMove::Seed, None).unwrap(),
        moved(w.p1)
    );

    // extend
    let s1 = h.snapshot().unwrap();
    assert_eq!(s1.canon(), Some(w.p1));
    let c2 = check(&s1, &[&w.e2]).unwrap();
    assert_eq!(
        s1.canon_chain(&c2).unwrap(),
        ChainFacts {
            head: Some(w.p1),
            entered: vec![w.p2],
            holds_expected: true,
            replaced: vec![],
        }
    );
    assert_eq!(
        h.move_canon(&c2, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );

    // hold: a sibling chain that holds p1 but not p2
    let s2 = h.snapshot().unwrap();
    assert_eq!(s2.canon(), Some(w.p2));
    let cq = check(&s2, &[&w.eq]).unwrap();
    assert_eq!(
        s2.canon_chain(&cq).unwrap(),
        ChainFacts {
            head: Some(w.p2),
            entered: vec![w.q2],
            holds_expected: false,
            replaced: vec![w.p2],
        }
    );
    assert_eq!(
        h.move_canon(&cq, CanonMove::Hold, Some(w.p2)).unwrap(),
        moved(w.q2)
    );
    assert_eq!(
        h.snapshot().unwrap().canon(),
        Some(w.p2),
        "a hold moves no canon"
    );

    // set view head: the facts name the pushed view and its root
    let s3 = h.snapshot().unwrap();
    let cv = check(&s3, &[&w.v1]).unwrap();
    assert_eq!(cv.facts().purpose, StreamPurpose::ViewHistory);
    let pushed = cv.facts().view.clone().expect("a view push names its view");
    assert_eq!((pushed.name.as_str(), pushed.root), ("draft", w.draft_root));
    assert!(s3.view_head("draft").unwrap().is_none());
    let set = CanonMove::SetViewHead {
        name: pushed.name.clone(),
        root: pushed.root,
    };
    assert_eq!(h.move_canon(&cv, set, None).unwrap(), moved(w.d1));
    let head = h.snapshot().unwrap().view_head("draft").unwrap().unwrap();
    assert_eq!((head.head, head.root), (w.d1, w.draft_root));

    // force: the held chain, with no new batch, replaces canon
    let s4 = h.snapshot().unwrap();
    let cf = s4.check_stored_head(w.q2).unwrap();
    assert!(cf.facts().objects.is_empty());
    assert_eq!(
        h.move_canon(&cf, CanonMove::Force, Some(w.p2)).unwrap(),
        moved(w.q2)
    );
    let s5 = h.snapshot().unwrap();
    assert_eq!(s5.canon(), Some(w.q2));
    // the replaced mark and the held head stay, as the store wrote them
    h.fsck().unwrap();
    drop((empty, c1, s1, c2, s2, cq, s3, cv, s4, cf, s5));
    h.close().unwrap();
    let raw = Rdy::open_holder(&w.dir.path().join("hub")).unwrap();
    let reader = raw.store().reader();
    assert!(
        reader
            .get_ref(&rdy_store::hub_canon::replaced_ref(&w.p2))
            .unwrap()
            .is_some()
    );
    assert!(
        reader
            .get_ref(&rdy_store::hub_canon::held_ref(&w.q2))
            .unwrap()
            .is_some()
    );
}

/// Ruling 2: a checked quarantine with no new batch names a head the holder stores (one a hold
/// kept). An extend from it checks the same chain rule and compare, and leaves `held/` as it is.
#[test]
fn scc6_extend_from_a_held_chain_with_no_new_batches() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    let s1 = h.snapshot().unwrap();
    let c2 = check(&s1, &[&w.e2]).unwrap();
    assert_eq!(
        h.move_canon(&c2, CanonMove::Hold, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
    assert_eq!(h.snapshot().unwrap().canon(), Some(w.p1));

    // a stored head with no batch: a head the holder lacks is refused
    let s2 = h.snapshot().unwrap();
    let e = s2.check_stored_head(w.q2).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:#?}");
    let stored = s2.check_stored_head(w.p2).unwrap();
    assert!(stored.facts().objects.is_empty());
    // fix round 1: a push beside the held chain checks before any move, in a scratch holder that
    // takes canon and the held chain; the holder stores none of it
    let sibling = check(&s2, &[&w.eq]).unwrap();
    let inputs = inputs_of(&w.p());
    let want = oracle(&Rdy::open(&w.dir.path().join("q")).unwrap(), w.q2, &inputs);
    assert_eq!(
        s2.check_pushed_landing(&sibling, w.q2, &inputs).unwrap(),
        want
    );
    assert!(holds_none(&h, &sibling.facts().objects));
    drop(sibling);
    assert_eq!(stored.facts().head, w.p2);
    assert_eq!(
        s2.canon_chain(&stored).unwrap(),
        ChainFacts {
            head: Some(w.p1),
            entered: vec![w.p2],
            holds_expected: true,
            replaced: vec![],
        }
    );
    // the compare still holds: a wrong expected tip is behind, and nothing moves
    assert_eq!(
        h.move_canon(&stored, CanonMove::Extend, Some(w.p2))
            .unwrap(),
        Moved::Behind { tip: Some(w.p1) }
    );
    assert_eq!(
        h.move_canon(&stored, CanonMove::Extend, Some(w.p1))
            .unwrap(),
        moved(w.p2)
    );
    assert_eq!(h.snapshot().unwrap().canon(), Some(w.p2));
    drop((s1, c2, s2, stored));
    h.close().unwrap();
    let raw = Rdy::open_holder(&w.dir.path().join("hub")).unwrap();
    assert_eq!(
        raw.store()
            .reader()
            .get_ref(&rdy_store::hub_canon::held_ref(&w.p2))
            .unwrap(),
        Some(vec![]),
        "the held ref stays as it is"
    );
}

/// Two checked quarantines of one snapshot both expect `p1`. The first move wins; the second is
/// behind, writes nothing and names the tip it found. A chain rule that fails is E_BAD_INPUT with
/// D-47HVF's exact reason, and the hub's verdict cannot skip the compare.
#[test]
fn scc6_quarantine_check_and_move_share_expected_state() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    let s1 = h.snapshot().unwrap();
    let a = check(&s1, &[&w.e2]).unwrap();
    let b = check(&s1, &[&w.eq]).unwrap();
    // both read the same captured tip before the verdict
    assert_eq!(s1.canon_chain(&a).unwrap().head, Some(w.p1));
    assert_eq!(s1.canon_chain(&b).unwrap().head, Some(w.p1));
    assert_eq!(
        h.move_canon(&a, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
    let before = h.snapshot().unwrap();
    assert_eq!(
        h.move_canon(&b, CanonMove::Extend, Some(w.p1)).unwrap(),
        Moved::Behind { tip: Some(w.p2) }
    );
    // nothing was written: the store's log end and refs are as they were
    let after = h.snapshot().unwrap();
    assert_eq!(after.canon(), Some(w.p2));
    assert!(
        !after
            .has_objects(&[ObjectKey {
                kind: rdy_store::kinds::LANDING,
                hash: w.q2
            }])
            .unwrap()[0]
            .present
    );
    assert_eq!(before.canon(), after.canon());
    // the captured snapshot still reads its own tip
    assert_eq!(s1.canon_chain(&b).unwrap().head, Some(w.p1));
    // the compare holds, the chain rule does not: D-47HVF's reason, nothing written
    let e = h.move_canon(&b, CanonMove::Extend, Some(w.p2)).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, rdy_store::hub_canon::extend_reason(&w.p2));
    let e = h.move_canon(&a, CanonMove::Force, Some(w.p2)).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, rdy_store::hub_canon::force_reason(&w.p2));
    assert_eq!(h.snapshot().unwrap().canon(), Some(w.p2));
    // a quarantine is read on the snapshot that checked it
    let e = after.canon_chain(&b).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:#?}");
    // a push of another repo is refused with the stream check's own error
    let other = tempfile::tempdir().unwrap();
    let mut r = project(other.path());
    doc(&mut r, "w0", "Other", "Another repo.");
    let o1 = land(&mut r, "w0");
    r.close().unwrap();
    let ofile = other.path().join("o1.rdysync");
    export(other.path(), StreamPurpose::CanonHistory, o1, None, &ofile);
    let e = check(&after, &[&ofile]).unwrap_err();
    assert_eq!(e.code, "E_REPO_MISMATCH", "{e:#?}");
    // two files of one push must name one root
    let e = check(&after, &[&w.e2, &w.eq]).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD", "{e:#?}");
    assert!(e.message.contains("batch header differs"), "{e:#?}");
    h.close().unwrap();
}

/// D-4XHPH: the minimum rdy before and after a move, and the preview the checked quarantine gives
/// before the verdict, agree with what a snapshot reads after the move.
#[test]
fn scc6_minimum_before_after_and_preview_agree() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    let s0 = h.snapshot().unwrap();
    let c1 = check(&s0, &[&w.e1]).unwrap();
    let hub_canon = RdyVersion::parse(rdy_store::format::HUB_CANON_REQUIRED_RDY).unwrap();
    assert_eq!(c1.facts().min_rdy_before, s0.min_rdy());
    assert!(s0.min_rdy() < Some(hub_canon), "{:?}", s0.min_rdy());
    assert_eq!(c1.facts().min_rdy_after, Some(hub_canon));
    assert_eq!(
        h.move_canon(&c1, CanonMove::Seed, None).unwrap(),
        moved(w.p1)
    );
    // the snapshot taken before keeps its own minimum; a new one reads the preview
    assert!(s0.min_rdy() < Some(hub_canon));
    let s1 = h.snapshot().unwrap();
    assert_eq!(s1.min_rdy(), c1.facts().min_rdy_after);
    // a later push raises nothing more
    let c2 = check(&s1, &[&w.e2]).unwrap();
    assert_eq!(c2.facts().min_rdy_before, s1.min_rdy());
    assert_eq!(c2.facts().min_rdy_after, s1.min_rdy());
    // a compare that fails writes nothing and moves no minimum
    assert_eq!(
        h.move_canon(&c2, CanonMove::Extend, Some(w.p2)).unwrap(),
        Moved::Behind { tip: Some(w.p1) }
    );
    assert_eq!(h.snapshot().unwrap().min_rdy(), s1.min_rdy());
    h.close().unwrap();
}

/// D-4E32E, D-47HVF: a holder's view heads are captured with the snapshot's refs and log
/// position, have no owner and no pin, and keep the base they were pushed with when canon
/// moves: the holder rebases nothing.
#[test]
fn scc6_view_heads_are_captured_and_never_rebased() {
    let _clock = rdy_engine::pin_clock(AT);
    let w = world();
    let h = w.holder("hub");
    seed(&h, &w);
    let s1 = h.snapshot().unwrap();
    let cv = check(&s1, &[&w.v1]).unwrap();
    let pushed = cv.facts().view.clone().unwrap();
    let set = |name: &str, root: Hash| CanonMove::SetViewHead {
        name: name.into(),
        root,
    };
    assert_eq!(
        h.move_canon(&cv, set(&pushed.name, pushed.root), None)
            .unwrap(),
        moved(w.d1)
    );
    let s2 = h.snapshot().unwrap();
    let first: ViewRead = s2.view_head("draft").unwrap().unwrap();
    assert_eq!(first.name, "draft");
    assert_eq!(first.root, w.draft_root);
    assert_eq!(first.head, w.d1);
    assert_eq!(first.base, Some(w.p1));
    assert_eq!(first.owner, None);
    assert!(!first.pinned);
    assert!(!first.closed);
    assert!(first.len > 0);
    assert_eq!(s2.view_heads().unwrap(), vec![first.clone()]);
    // canon moves; the view keeps its base
    let c2 = check(&s2, &[&w.e2]).unwrap();
    assert_eq!(
        h.move_canon(&c2, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
    let s3 = h.snapshot().unwrap();
    assert_eq!(s3.view_head("draft").unwrap().unwrap(), first);
    // the view moves on; the older snapshots keep the head they captured
    let cv2 = check(&s3, &[&w.v2]).unwrap();
    // a wrong root is refused with D-47HVF's reason
    let e = h
        .move_canon(&cv2, set("draft", w.d1), Some(w.d1))
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        h.move_canon(&cv2, set("draft", w.draft_root), Some(w.p1))
            .unwrap(),
        Moved::Behind { tip: Some(w.d1) }
    );
    assert_eq!(
        h.move_canon(&cv2, set("draft", w.draft_root), Some(w.d1))
            .unwrap(),
        moved(w.d2)
    );
    let s4 = h.snapshot().unwrap();
    let second = s4.view_head("draft").unwrap().unwrap();
    assert_eq!((second.head, second.root), (w.d2, w.draft_root));
    assert_eq!((second.owner.clone(), second.pinned), (None, false));
    let pushed_base = second.base;
    assert_eq!(s2.view_head("draft").unwrap().unwrap().head, w.d1);
    assert_eq!(s3.view_heads().unwrap(), vec![first]);
    assert_eq!(s4.view_heads().unwrap(), vec![second]);
    assert!(s4.view_head("other").unwrap().is_none());
    h.close().unwrap();

    // a project's view read keeps its owner and pin, as its head holds them
    let p = StoreHandle::open(&w.p()).unwrap();
    let snap = p.snapshot().unwrap();
    let draft = snap.view_head("draft").unwrap().unwrap();
    assert_eq!((draft.head, draft.root), (w.d2, w.draft_root));
    assert!(draft.owner.is_some());
    // the hub's base is the one the pushed chain gives, as the project's own head reads it
    assert_eq!(pushed_base, draft.base, "the holder rebases no view");
    drop(snap);
    p.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// the landing check

/// A pack with one rule on claims, `ok-claims`, of `severity`.
fn pack(severity: &str) -> String {
    format!(
        "\
name: scc6
version: \"1\"
extends: rdy-standard
block_types:
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      ok: {{ kind: bool }}
rules:
  - name: ok-claims
    select: type = claim
    require: fields.ok = true
    severity: {severity}
    message: ok-claims message
    fix: ok-claims fix
"
    )
}

fn use_pack(dir: &Path, severity: &str) {
    std::fs::write(dir.join(".rdy/packs/scc6.yaml"), pack(severity)).unwrap();
    std::fs::write(dir.join(".rdy.yaml"), "pack: scc6\n").unwrap();
}

/// A project on the advisory pack whose second landing `l2` puts a claim that fails `ok-claims`;
/// the gate inputs of the advisory and of the blocking pack, captured through the adapter.
struct Checked {
    dir: tempfile::TempDir,
    l1: Hash,
    l2: Hash,
    claim: String,
    advisory: CheckInputs,
    blocking: CheckInputs,
}

fn inputs_of(dir: &Path) -> CheckInputs {
    let h = StoreHandle::open(dir).unwrap();
    let inputs = h.snapshot().unwrap().capture_gate_inputs().unwrap();
    h.close().unwrap();
    inputs
}

/// Writes `ops` into view `view` with an editorial justification, briefed when the write
/// touches stable content.
fn write_j(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    let req = || WriteRequest {
        view: view.into(),
        ops: serde_json::from_value(ops.clone()).unwrap(),
        justification: Some(rdy_model::Justification::Editorial {
            class: rdy_model::EditorialClass::Typo,
            reason: "typo".into(),
        }),
        message: "scc6".into(),
    };
    match r.write_briefed(&session(Role::Writer), req(), None) {
        Err(e) if e.code == "E_BRIEF_REQUIRED" || e.code == "E_BRIEF_STALE" => {
            let e = rdy_engine::brief::expand_error(e, &r.read(), view, &r.briefs(), None);
            let token = e.briefing.as_ref().unwrap()["base"]["token"]
                .as_str()
                .unwrap()
                .to_string();
            r.write_briefed(&session(Role::Writer), req(), Some(token))
                .unwrap()
        }
        other => other.unwrap(),
    }
}

/// Answers each open item of view `view`'s change plan that readiness gates: each tension item
/// `confirm`, each non-code item on stable content by its first allowed act.
fn answer_items(r: &mut Rdy, view: &str) {
    use rdy_engine::plan::api::{ItemState, PlanAnswerInput, PlanRow};
    use rdy_engine::plan::compute::allowed_acts;
    use rdy_model::{PlanAct, PlanKind};
    let Ok(reply) = r.read().view_plan(view) else {
        return;
    };
    let inputs: Vec<PlanAnswerInput> = reply
        .items
        .iter()
        .filter_map(|row| {
            let PlanRow::Live(i) = row else { return None };
            if i.state != ItemState::Open {
                return None;
            }
            let act = match i.kind {
                PlanKind::Tension => PlanAct::Confirm,
                PlanKind::UpdateImplementation => return None,
                kind if i.stable => *allowed_acts(kind).first()?,
                _ => return None,
            };
            Some(PlanAnswerInput {
                item: i.id.clone(),
                act,
                reason: Some("test: checked".into()),
            })
        })
        .collect();
    if !inputs.is_empty() {
        r.answer_plan(&session(Role::Writer), view, &inputs)
            .unwrap();
    }
}

/// Answers `view`'s open items, then proposes and lands it.
fn land_stable(r: &mut Rdy, view: &str) -> Hash {
    answer_items(r, view);
    land(r, view)
}

fn checked() -> Checked {
    let dir = tempfile::tempdir().unwrap();
    let r = project(dir.path());
    r.close().unwrap();
    use_pack(dir.path(), "advisory");
    let mut r = Rdy::open(dir.path()).unwrap();
    let w = write_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}
        ]),
    );
    let l1 = land_stable(&mut r, "seed");
    let c = write_j(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": w.created["$s"], "body": "C holds.", "fields": {"ok": false}}]),
    );
    let l2 = land_stable(&mut r, "a");
    r.close().unwrap();
    let advisory = inputs_of(dir.path());
    use_pack(dir.path(), "blocking");
    let blocking = inputs_of(dir.path());
    use_pack(dir.path(), "advisory");
    Checked {
        l1,
        l2,
        claim: c.created["$c"].clone(),
        advisory,
        blocking,
        dir,
    }
}

/// What the engine's own evaluator gives for landing `l` on the published state of `r`.
fn oracle(r: &Rdy, l: Hash, inputs: &CheckInputs) -> LandingCheck {
    let st = r.read();
    let record = st.reader().landing(&l).unwrap();
    rdy_engine::landing_check::check_landing(&st, &record, inputs).unwrap()
}

/// D-0WWYR: the check judges the landing on its own `prev` with the pack and settings the caller
/// passes, in a project and in a holder that holds the landing; bad inputs are refused.
#[test]
fn scc6_check_landing_uses_prev_pack_and_settings() {
    let _clock = rdy_engine::pin_clock(AT);
    let c = checked();
    // the advisory pack is the project's own; the blocking one gives one row on the claim
    let r = Rdy::open(c.dir.path()).unwrap();
    let want_adv = oracle(&r, c.l2, &c.advisory);
    let want_blk = oracle(&r, c.l2, &c.blocking);
    assert!(want_adv.pass, "{want_adv:#?}");
    assert!(!want_blk.pass);
    assert_eq!(
        want_blk
            .rows
            .iter()
            .map(|i| (i.code, i.block.as_ref().map(|b| b.to_string())))
            .collect::<Vec<_>>(),
        vec![(rdy_engine::E_RULE_CHECK, Some(c.claim.clone()))]
    );
    assert_eq!(oracle(&r, c.l1, &c.blocking).rows, vec![]);
    let repo = repo_of(&r);
    // a later landing over the claim: the check still judges l2 on l1
    drop(r);
    let mut r = Rdy::open(c.dir.path()).unwrap();
    write_j(
        &mut r,
        "later",
        json!([{"op": "update", "id": c.claim, "fields": {"ok": true}}]),
    );
    let l3 = land_stable(&mut r, "later");
    assert_eq!(oracle(&r, l3, &c.blocking).rows, vec![]);
    r.close().unwrap();

    let p = StoreHandle::open(c.dir.path()).unwrap();
    let snap = p.snapshot().unwrap();
    assert_eq!(snap.check_landing(c.l2, &c.advisory).unwrap(), want_adv);
    assert_eq!(snap.check_landing(c.l2, &c.blocking).unwrap(), want_blk);
    assert!(snap.check_landing(l3, &c.blocking).unwrap().pass);
    // inputs out of their ranges are refused before any check
    let mut bad = c.blocking.clone();
    bad.coverage_target = 101;
    let e = snap.check_landing(c.l2, &bad).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "coverage target 101 is outside 0 to 100",
            "pass a coverage target from 0 to 100"
        )
    );
    let mut bad = c.blocking.clone();
    bad.propose_changes = 0;
    let e = snap.check_landing(c.l2, &bad).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "propose changes 0 is outside 1 to 1000",
            "pass a propose changes count from 1 to 1000"
        )
    );
    let mut bad = c.blocking.clone();
    bad.decisions_home = Some(" ".into());
    let e = snap.check_landing(c.l2, &bad).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "decisions home is empty",
            "pass none for no decisions home"
        )
    );
    // a landing the snapshot does not hold
    let e = snap
        .check_landing(Hash::of(b"no such landing"), &c.advisory)
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e:#?}");
    // the push of l1..l3 to a holder: the holder's check equals the project's
    let file = c.dir.path().join("canon.rdysync");
    let out = File::create(&file).unwrap();
    snap.write_history(StreamPurpose::CanonHistory, l3, None, &BTreeSet::new(), out)
        .unwrap();
    drop(snap);
    p.close().unwrap();
    let hub = tempfile::tempdir().unwrap();
    let h = StoreHandle::create_holder(hub.path(), repo).unwrap();
    let s0 = h.snapshot().unwrap();
    let q = check(&s0, &[&file]).unwrap();
    assert_eq!(h.move_canon(&q, CanonMove::Seed, None).unwrap(), moved(l3));
    let hs = h.snapshot().unwrap();
    assert_eq!(hs.check_landing(c.l2, &c.advisory).unwrap(), want_adv);
    assert_eq!(hs.check_landing(c.l2, &c.blocking).unwrap(), want_blk);
    // a first landing checks in the holder too
    assert!(hs.check_landing(c.l1, &c.blocking).unwrap().pass);
    // a holder captures no gate inputs: they are the pushing project's
    assert_eq!(hs.capture_gate_inputs().unwrap_err().code, "E_BAD_INPUT");
    h.close().unwrap();
}

/// Writes the canon history of the project at `dir` at `root`, less `base`'s closure, to `out`.
fn export_canon(dir: &Path, root: Hash, base: Option<Hash>, out: &Path) {
    export(dir, StreamPurpose::CanonHistory, root, base, out);
}

/// Whether the snapshot of `h` holds none of `keys`.
fn holds_none(h: &StoreHandle, keys: &[ObjectKey]) -> bool {
    h.snapshot()
        .unwrap()
        .has_objects(keys)
        .unwrap()
        .iter()
        .all(|p| !p.present)
}

/// D-0WWYR (fix round 1): a hub checks the landings of a push while they are only in its checked
/// quarantine, before any move: the first push to an empty holder, and a push that extends a
/// seeded one. The check equals the project's own, a failing check stores nothing and moves no
/// canon, and the guarded move still compares the expected tip afterwards.
#[test]
fn scc6_check_landing_of_a_quarantined_push_before_intake() {
    let _clock = rdy_engine::pin_clock(AT);
    let c = checked();
    let r = Rdy::open(c.dir.path()).unwrap();
    let want_adv = oracle(&r, c.l2, &c.advisory);
    let want_blk = oracle(&r, c.l2, &c.blocking);
    let want_l1 = oracle(&r, c.l1, &c.blocking);
    let repo = repo_of(&r);
    r.close().unwrap();
    assert!(want_adv.pass && !want_blk.pass && want_l1.pass);
    let first = c.dir.path().join("first.rdysync");
    export_canon(c.dir.path(), c.l2, None, &first);
    let at_l1 = c.dir.path().join("l1.rdysync");
    export_canon(c.dir.path(), c.l1, None, &at_l1);
    let next = c.dir.path().join("next.rdysync");
    export_canon(c.dir.path(), c.l2, Some(c.l1), &next);

    // the first push to an empty holder
    let hub = tempfile::tempdir().unwrap();
    let h = StoreHandle::create_holder(hub.path(), repo).unwrap();
    let s0 = h.snapshot().unwrap();
    let q = check(&s0, &[&first]).unwrap();
    // the stored-only check does not see the quarantine
    assert_eq!(
        s0.check_landing(c.l2, &c.advisory).unwrap_err().code,
        "E_NOT_FOUND"
    );
    assert_eq!(
        s0.check_pushed_landing(&q, c.l2, &c.blocking).unwrap(),
        want_blk
    );
    assert_eq!(
        s0.check_pushed_landing(&q, c.l1, &c.blocking).unwrap(),
        want_l1
    );
    assert_eq!(
        s0.check_pushed_landing(&q, c.l2, &c.advisory).unwrap(),
        want_adv
    );
    // a landing neither the push nor the holder holds
    assert_eq!(
        s0.check_pushed_landing(&q, Hash::of(b"no such landing"), &c.advisory)
            .unwrap_err()
            .code,
        "E_NOT_FOUND"
    );
    // inputs out of range are refused first
    let mut bad = c.advisory.clone();
    bad.coverage_target = 101;
    assert_eq!(
        s0.check_pushed_landing(&q, c.l2, &bad).unwrap_err().code,
        "E_BAD_INPUT"
    );
    // the failing check stored nothing and moved no canon
    assert_eq!(h.snapshot().unwrap().canon(), None);
    assert!(holds_none(&h, &q.facts().objects));
    // the passing check, then the guarded move: a wrong expectation is still Behind
    assert_eq!(
        h.move_canon(&q, CanonMove::Seed, Some(c.l1)).unwrap(),
        Moved::Behind { tip: None }
    );
    assert_eq!(
        h.move_canon(&q, CanonMove::Seed, None).unwrap(),
        moved(c.l2)
    );
    let s1 = h.snapshot().unwrap();
    assert_eq!(s1.check_landing(c.l2, &c.advisory).unwrap(), want_adv);
    h.close().unwrap();

    // a push that extends a seeded holder
    let hub = tempfile::tempdir().unwrap();
    let h = StoreHandle::create_holder(hub.path(), repo).unwrap();
    let s0 = h.snapshot().unwrap();
    let q1 = check(&s0, &[&at_l1]).unwrap();
    assert_eq!(
        h.move_canon(&q1, CanonMove::Seed, None).unwrap(),
        moved(c.l1)
    );
    let s1 = h.snapshot().unwrap();
    let q2 = check(&s1, &[&next]).unwrap();
    // a quarantine checked on another snapshot is refused
    assert_eq!(
        s1.check_pushed_landing(&q1, c.l1, &c.advisory)
            .unwrap_err()
            .code,
        "E_BAD_INPUT"
    );
    assert_eq!(
        s1.check_landing(c.l2, &c.blocking).unwrap_err().code,
        "E_NOT_FOUND"
    );
    assert_eq!(
        s1.check_pushed_landing(&q2, c.l2, &c.blocking).unwrap(),
        want_blk
    );
    assert_eq!(
        s1.check_pushed_landing(&q2, c.l2, &c.advisory).unwrap(),
        want_adv
    );
    // a landing the holder stores checks as the stored check does
    assert_eq!(
        s1.check_pushed_landing(&q2, c.l1, &c.blocking).unwrap(),
        s1.check_landing(c.l1, &c.blocking).unwrap()
    );
    assert_eq!(h.snapshot().unwrap().canon(), Some(c.l1));
    assert!(holds_none(
        &h,
        &[ObjectKey {
            kind: rdy_store::kinds::LANDING,
            hash: c.l2
        }]
    ));
    assert_eq!(
        h.move_canon(&q2, CanonMove::Extend, Some(c.l1)).unwrap(),
        moved(c.l2)
    );
    h.close().unwrap();
}

/// D-0WWYR (fix round 2): the `prev` of a pushed landing may be one the holder stores only
/// through a tag. A holder that took tag `v1.0.0` at `l1`, and has no canon, checks the
/// incremental canon push of `l2` (less `l1`'s closure) before its first canon move: the check
/// equals the project's own, stores nothing and moves no canon. The check reads the records the
/// quarantine checked, not the batch file again: a file emptied after the check changes nothing.
#[test]
fn scc6_check_landing_of_a_push_onto_a_tagged_base() {
    let _clock = rdy_engine::pin_clock(AT);
    let c = checked();
    let mut r = Rdy::open(c.dir.path()).unwrap();
    r.tag(
        &session(Role::Approver),
        "v1.0.0",
        Some(&c.l1.to_string()),
        None,
    )
    .unwrap();
    let v1 = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap()
        .1;
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), v1, &BTreeSet::new(), &mut out).unwrap();
    let t1 = c.dir.path().join("t1.rdysync");
    std::fs::write(&t1, out).unwrap();
    let want_adv = oracle(&r, c.l2, &c.advisory);
    let want_blk = oracle(&r, c.l2, &c.blocking);
    let want_l1 = oracle(&r, c.l1, &c.blocking);
    let repo = repo_of(&r);
    r.close().unwrap();
    assert!(want_adv.pass && !want_blk.pass && want_l1.pass);
    let next = c.dir.path().join("next.rdysync");
    export_canon(c.dir.path(), c.l2, Some(c.l1), &next);

    let hub = tempfile::tempdir().unwrap();
    let h = StoreHandle::create_holder(hub.path(), repo).unwrap();
    h.take(
        &[File::open(&t1).unwrap()],
        rdy_adapter::take::UploadTarget {
            tag: "v1.0.0".into(),
            landing: c.l1,
        },
    )
    .unwrap();
    let s0 = h.snapshot().unwrap();
    assert_eq!(s0.canon(), None);
    let q = check(&s0, &[&next]).unwrap();
    assert_eq!(
        s0.canon_chain(&q).unwrap(),
        ChainFacts {
            head: None,
            entered: vec![c.l1, c.l2],
            holds_expected: false,
            replaced: vec![],
        }
    );
    // the hub's file is emptied after the check: the quarantine keeps its records
    std::fs::write(&next, b"").unwrap();
    assert_eq!(
        s0.check_pushed_landing(&q, c.l2, &c.blocking).unwrap(),
        want_blk
    );
    assert_eq!(
        s0.check_pushed_landing(&q, c.l2, &c.advisory).unwrap(),
        want_adv
    );
    // the tagged base checks as the stored check does
    assert_eq!(
        s0.check_pushed_landing(&q, c.l1, &c.blocking).unwrap(),
        want_l1
    );
    // the checks stored nothing and moved no canon
    assert_eq!(h.snapshot().unwrap().canon(), None);
    assert!(holds_none(
        &h,
        &[ObjectKey {
            kind: rdy_store::kinds::LANDING,
            hash: c.l2
        }]
    ));
    // the guarded first move, over the tagged base
    assert_eq!(
        h.move_canon(&q, CanonMove::Seed, Some(c.l1)).unwrap(),
        Moved::Behind { tip: None }
    );
    assert_eq!(
        h.move_canon(&q, CanonMove::Seed, None).unwrap(),
        moved(c.l2)
    );
    let s1 = h.snapshot().unwrap();
    assert_eq!(s1.check_landing(c.l2, &c.blocking).unwrap(), want_blk);
    h.close().unwrap();
}

/// D-0WWYR: work in flight in a view changes nothing, and `assessment_matches` says whether the
/// landing's stored assessment names the pack and config digests of the inputs.
#[test]
fn scc6_check_landing_ignores_views_and_reports_digest_match() {
    let _clock = rdy_engine::pin_clock(AT);
    let c = checked();
    let p = StoreHandle::open(c.dir.path()).unwrap();
    let before = p.snapshot().unwrap();
    let adv = before.check_landing(c.l2, &c.advisory).unwrap();
    let blk = before.check_landing(c.l2, &c.blocking).unwrap();
    assert!(adv.assessment_matches, "{adv:#?}");
    assert!(!blk.assessment_matches, "{blk:#?}");
    // another setting changes only the config digest
    let mut strict = c.advisory.clone();
    strict.require_up_to_date = !strict.require_up_to_date;
    let other = before.check_landing(c.l2, &strict).unwrap();
    assert_eq!(other.rows, adv.rows);
    assert!(!other.assessment_matches);
    drop(before);
    p.close().unwrap();
    // a view puts the claim right, in flight: the check reads no view
    let mut r = Rdy::open(c.dir.path()).unwrap();
    write_j(
        &mut r,
        "fix",
        json!([{"op": "update", "id": c.claim, "fields": {"ok": true}}]),
    );
    r.close().unwrap();
    let p = StoreHandle::open(c.dir.path()).unwrap();
    let after = p.snapshot().unwrap();
    assert!(after.view_head("fix").unwrap().is_some());
    assert_eq!(after.check_landing(c.l2, &c.advisory).unwrap(), adv);
    assert_eq!(after.check_landing(c.l2, &c.blocking).unwrap(), blk);
    p.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// render and gate inputs

/// The facts of gate inputs, to compare them.
fn gate_facts(i: &CheckInputs) -> String {
    format!(
        "{} {} {:?} {:?} {:?} {} {}",
        i.pack.name,
        i.pack.version,
        i.coverage_gate,
        i.coverage_target,
        i.decisions_home,
        i.propose_changes,
        i.require_up_to_date
    )
}

/// D-QMZ92, D-H0BZN, D-8RGWW: the render inputs of a requested tag are captured explicitly, with
/// the tag's own disagreement list: a tag at canon's head carries canon's open disagreements, a
/// tag at a past landing carries none.
#[test]
fn scc6_render_inputs_include_tag_disagreements() {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path());
    let spec = write_in(
        &mut r,
        "spec",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "The rule holds."}
        ]),
    );
    land(&mut r, "spec");
    tag(&mut r, "v1.0.0");
    let target = spec.created["$c"].clone();
    write_in(
        &mut r,
        "notes",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Reading"}},
            {"op": "create", "type": "claim", "parent": "$s", "body": format!("Readers rely on {{{{{target}}}}}.")}
        ]),
    );
    land(&mut r, "notes");
    write_in(
        &mut r,
        "retire",
        json!([{"op": "update", "id": spec.created["$s"], "fields": {"lifecycle": "deprecated"}}]),
    );
    let head = land(&mut r, "retire");
    tag(&mut r, "v2.0.0");
    // the oracles: the renderer's capture at each tag's landing
    let want_v2: RenderInputs =
        rdy_render::render_input::capture_inputs(&r.read(), &Scope::At(head)).unwrap();
    assert!(!want_v2.disagreements.is_empty(), "{want_v2:#?}");
    let l1 = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .map(|(_, h, _)| r.store().read_tag(&h).unwrap().landing)
        .unwrap();
    let want_v1 = rdy_render::render_input::capture_inputs(&r.read(), &Scope::At(l1)).unwrap();
    assert!(want_v1.disagreements.is_empty());
    let want_gate = gate_facts(&CheckInputs::of_state(&r.read()));
    r.close().unwrap();

    let h = StoreHandle::open(dir.path()).unwrap();
    let snap = h.snapshot().unwrap();
    assert_eq!(snap.capture_render_inputs("v2.0.0").unwrap(), want_v2);
    // `2.0.0` finds `v2.0.0`
    assert_eq!(snap.capture_render_inputs("2.0.0").unwrap(), want_v2);
    assert_eq!(snap.capture_render_inputs("v1.0.0").unwrap(), want_v1);
    assert_eq!(
        snap.capture_render_inputs("v3.0.0").unwrap_err().code,
        "E_NOT_FOUND"
    );
    // the gate inputs of the push are the project's, as the engine reads them
    let gate = snap.capture_gate_inputs().unwrap();
    assert_eq!(gate_facts(&gate), want_gate);
    h.close().unwrap();
}

/// A removal applied after a snapshot invalidates every check bound to it: the move refuses with
/// E_REDACTED_STATE and writes nothing, and the snapshot's reads refuse too.
#[test]
fn scc6_removal_invalidates_captured_check() {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(&dir.path().join("p"));
    let repo = repo_of(&r);
    let w = doc(&mut r, "work", "Intro", "Public words.");
    let secret = write_in(
        &mut r,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": w.created["$s"], "body": "The launch code is ZEBRA-7731."}]),
    );
    let l1 = land(&mut r, "work");
    let v1 = tag(&mut r, "v1.0.0");
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), v1, &BTreeSet::new(), &mut out).unwrap();
    let t1 = dir.path().join("t1.rdysync");
    std::fs::write(&t1, out).unwrap();
    // the canon push, made before the removal
    let mut out = Vec::new();
    rdy_engine::stream_export::write_history(
        &r.read(),
        StreamPurpose::CanonHistory,
        l1,
        None,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    let e1 = dir.path().join("e1.rdysync");
    std::fs::write(&e1, out).unwrap();

    let h = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let up = |p: &Path, tag: &str, landing: Hash| {
        h.take(
            &[File::open(p).unwrap()],
            rdy_adapter::take::UploadTarget {
                tag: tag.into(),
                landing,
            },
        )
        .unwrap();
    };
    up(&t1, "v1.0.0", l1);
    let snap = h.snapshot().unwrap();
    let checked = check(&snap, &[&e1]).unwrap();
    assert!(snap.canon_chain(&checked).is_ok());
    // the push's landing checks before any move
    assert!(
        snap.check_pushed_landing(
            &checked,
            l1,
            &rdy_engine::landing_check::CheckInputs::of_state(&r.read())
        )
        .is_ok()
    );
    assert!(
        snap.check_landing(
            l1,
            &rdy_engine::landing_check::CheckInputs::of_state(&r.read())
        )
        .is_ok()
    );

    // the project removes the secret; the holder takes the removal with the next tag
    let target = secret.created["$x"].clone();
    remove(&mut r, &target);
    let v2 = tag(&mut r, "v2.0.0");
    let l2 = r.store().read_tag(&v2).unwrap().landing;
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), v2, &BTreeSet::new(), &mut out).unwrap();
    let t2 = dir.path().join("t2.rdysync");
    std::fs::write(&t2, out).unwrap();
    let inputs = rdy_engine::landing_check::CheckInputs::of_state(&r.read());
    r.close().unwrap();
    up(&t2, "v2.0.0", l2);

    // the captured check is stale: no move, and nothing written
    let e = h.move_canon(&checked, CanonMove::Seed, None).unwrap_err();
    assert_eq!(e.code, "E_REDACTED_STATE", "{e:#?}");
    let fresh = h.snapshot().unwrap();
    assert_eq!(fresh.canon(), None);
    assert_eq!(fresh.min_rdy(), snap.min_rdy());
    // the snapshot's own reads refuse as well
    assert_eq!(
        snap.canon_chain(&checked).unwrap_err().code,
        "E_REDACTED_STATE"
    );
    assert_eq!(
        snap.check_landing(l1, &inputs).unwrap_err().code,
        "E_REDACTED_STATE"
    );
    assert_eq!(
        snap.check_pushed_landing(&checked, l1, &inputs)
            .unwrap_err()
            .code,
        "E_REDACTED_STATE"
    );
    assert_eq!(snap.view_heads().unwrap_err().code, "E_REDACTED_STATE");
    h.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// the contract

/// FW-UP1a, D-KE83X, D-47HVF: the adapter's capabilities name the sync facts this build reads
/// and the moves it makes, from core's registries, and agree with the public contract's stream
/// facts.
#[test]
fn scc6_contract_reports_sync_capabilities() {
    let sync = &rdy_adapter::capabilities::sync_capabilities();
    assert_eq!(
        sync.stream_formats,
        rdy_store::sync_stream::READ_FORMATS.to_vec()
    );
    assert_eq!(sync.history_format, rdy_store::sync_stream::HISTORY_FORMAT);
    assert_eq!(
        sync.history_purposes,
        vec![StreamPurpose::CanonHistory, StreamPurpose::ViewHistory]
    );
    assert_eq!(
        sync.moves,
        vec!["seed", "extend", "force", "hold", "set_view_head"]
    );
    assert_eq!(sync.hub_canon.name, rdy_store::format::FEATURE_HUB_CANON);
    assert_eq!(
        sync.hub_canon.required_rdy,
        rdy_store::format::HUB_CANON_REQUIRED_RDY
    );
    assert_eq!(sync.hub_canon.required_rdy, "0.53.0");
    // the current public contract states the same stream facts
    let contract: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../rdy-qa/fixtures/contracts/sync-client-v1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let history = &contract["sync"]["history"];
    assert_eq!(history["format"], json!(sync.history_format));
    assert_eq!(history["read_formats"], json!(sync.stream_formats));
    // and the capability serializes as a host reads it
    let v = serde_json::to_value(sync).unwrap();
    assert_eq!(
        v["history_purposes"],
        json!(["canon_history", "view_history"])
    );
    assert_eq!(v["hub_canon"]["required_rdy"], json!("0.53.0"));
    assert_eq!(v["hub_canon"]["class"], json!("incompatible"));
    // the format capabilities keep their own members
    let formats = serde_json::to_value(rdy_adapter::capabilities::format_capabilities()).unwrap();
    assert!(formats.get("sync").is_none());
}
