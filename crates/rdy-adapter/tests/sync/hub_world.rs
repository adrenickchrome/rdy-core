//! The holder world of the sync tests, shared by the hub scope tests: project P lands `p1` and
//! `p2`, a copy Q lands `q2` beside `p2`, and P keeps a view `draft` (D-47HVF). The source
//! projects are built with core calls and closed before an adapter handle opens a store. Later
//! scope tests read the view half, so an item one module does not use is not dead.
#![allow(dead_code)]

use rdy_adapter::stream::{ObjectKey, StreamPurpose};
use rdy_adapter::sync::{CanonMove, CheckedQuarantine, Moved};
use rdy_adapter::{Hash, RdyError, RepoId, Snapshot, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The writer's clock of every move (pinned on the calling thread).
pub const AT: i64 = 1_763_164_800_000;

// ---------------------------------------------------------------------------------------------
// source projects (core)

pub fn session(role: Role) -> Session {
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

pub fn write_in(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
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
pub fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), view, view, Some("test summary"))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"));
    r.land(&session(Role::Approver), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:#?}"))
        .parse()
        .unwrap()
}

/// A doc with a section and one prose block, written in view `view`.
pub fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) -> rdy_engine::WriteResult {
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

pub fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

pub fn project(dir: &Path) -> Rdy {
    std::fs::create_dir_all(dir).unwrap();
    drop(Rdy::init(dir, "rdy-standard").unwrap());
    Rdy::open(dir).unwrap()
}

pub fn repo_of(r: &Rdy) -> RepoId {
    r.store().reader().repo_id().unwrap().unwrap()
}

pub fn copy_dir(from: &Path, to: &Path) {
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

// ---------------------------------------------------------------------------------------------
// the hub's side: adapter calls only

/// Writes the format-4 history of `purpose` at `root` (less `base`'s closure) of the project at
/// `dir` to `out`, through an adapter handle of the project.
pub fn export(dir: &Path, purpose: StreamPurpose, root: Hash, base: Option<Hash>, out: &Path) {
    let h = StoreHandle::open(dir).unwrap();
    let snap = h.snapshot().unwrap();
    let file = File::create(out).unwrap();
    snap.write_history(purpose, root, base, &BTreeSet::<ObjectKey>::new(), file)
        .unwrap();
    drop(snap);
    h.close().unwrap();
}

pub fn files(paths: &[&Path]) -> Vec<File> {
    paths.iter().map(|p| File::open(p).unwrap()).collect()
}

pub fn check(snap: &Snapshot, paths: &[&Path]) -> Result<CheckedQuarantine, RdyError> {
    snap.check_quarantine(&files(paths))
}

pub fn moved(tip: Hash) -> Moved {
    Moved::Moved { tip }
}

/// One world of pushes. Project P lands `p1` (view `w0`), then `p2` (view `w1`); Q, a copy of P
/// at `p1`, lands `q2` (a sibling of `p2`). P keeps an open view `draft` with work in flight
/// whose ops are `d1` and then `d2`.
pub struct World {
    pub dir: tempfile::TempDir,
    pub repo: RepoId,
    pub p1: Hash,
    pub p2: Hash,
    pub q2: Hash,
    pub d1: Hash,
    pub d2: Hash,
    pub draft_root: Hash,
    /// canon history of P at p1, of P at p2 less p1, and of Q at q2 less p1
    pub e1: PathBuf,
    pub e2: PathBuf,
    pub eq: PathBuf,
    /// view history of `draft` at d1, and at d2 less d1
    pub v1: PathBuf,
    pub v2: PathBuf,
}

impl World {
    pub fn p(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    pub fn holder(&self, name: &str) -> StoreHandle {
        StoreHandle::create_holder(&self.dir.path().join(name), self.repo).unwrap()
    }
}

pub fn view_head_of(r: &Rdy, name: &str) -> Hash {
    r.store().reader().view_head(name).unwrap().unwrap().head
}

pub fn world() -> World {
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
pub fn seed(h: &StoreHandle, w: &World) {
    let snap = h.snapshot().unwrap();
    let c = check(&snap, &[&w.e1]).unwrap();
    assert_eq!(
        h.move_canon(&c, CanonMove::Seed, None).unwrap(),
        moved(w.p1)
    );
}

/// Moves view `draft` of the hub `h` to the head the view history at `path` pushes.
pub fn set_draft(h: &StoreHandle, w: &World, path: &std::path::Path, from: Option<Hash>, to: Hash) {
    let s = h.snapshot().unwrap();
    let c = check(&s, &[path]).unwrap();
    let set = CanonMove::SetViewHead {
        name: "draft".into(),
        root: w.draft_root,
    };
    assert_eq!(h.move_canon(&c, set, from).unwrap(), moved(to));
}

/// The hub of `w` seeded with `p1`, view `draft` pushed at `d1`, canon extended to `p2` and
/// `draft` moved on to `d2`.
pub fn pushed_draft(w: &World) -> StoreHandle {
    let h = w.holder("hub");
    seed(&h, w);
    set_draft(&h, w, &w.v1, None, w.d1);
    let s = h.snapshot().unwrap();
    let c2 = check(&s, &[&w.e2]).unwrap();
    assert_eq!(
        h.move_canon(&c2, CanonMove::Extend, Some(w.p1)).unwrap(),
        moved(w.p2)
    );
    drop((s, c2));
    set_draft(&h, w, &w.v2, Some(w.d1), w.d2);
    h
}

// ---------------------------------------------------------------------------------------------
// a clean replay

/// A hub whose canon holds a clean replay. The hub lands `p1` and then `p2`; L, a copy of P at
/// `p1`, lands `l` with no tag, pulls `p2` with a clean replay to `l2` (whose `rebased_from`
/// names `l`) and pushes `l2`. The hub has taken the push, so canon's chain reaches `l` through
/// `rebased_from` only.
pub struct Replayed {
    pub dir: tempfile::TempDir,
    pub hub: StoreHandle,
    pub p2: Hash,
    pub l: Hash,
    pub l2: Hash,
}

/// The URL of L's sync state with the hub.
const HUB_URL: &str = "https://127.0.0.1:9/r/demo";

pub fn replayed() -> Replayed {
    use rdy_engine::sync_fetch::{self, HubState};
    use rdy_engine::sync_replay::{self, PULLED, PullRequest};
    use rdy_store::sync_state::{SyncCanon, SyncStep};
    use rdy_store::take_batch::FetchTarget;
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let ld = dir.path().join("l");
    let mut p = project(&pd);
    let repo = repo_of(&p);
    doc(&mut p, "w0", "Canon", "The hub keeps one canon.");
    let p1 = land(&mut p, "w0");
    p.close().unwrap();
    copy_dir(&pd, &ld);
    let mut p = Rdy::open(&pd).unwrap();
    doc(&mut p, "w1", "Extend", "The canon moves forward.");
    let p2 = land(&mut p, "w1");
    p.close().unwrap();
    let at = |n: &str| dir.path().join(format!("{n}.rdysync"));
    let canon = StreamPurpose::CanonHistory;
    export(&pd, canon, p1, None, &at("e1"));
    export(&pd, canon, p2, Some(p1), &at("e2"));
    let hub = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    let s0 = hub.snapshot().unwrap();
    let c1 = check(&s0, &[&at("e1")]).unwrap();
    assert_eq!(
        hub.move_canon(&c1, CanonMove::Seed, None).unwrap(),
        moved(p1)
    );
    let s1 = hub.snapshot().unwrap();
    let c2 = check(&s1, &[&at("e2")]).unwrap();
    assert_eq!(
        hub.move_canon(&c2, CanonMove::Extend, Some(p1)).unwrap(),
        moved(p2)
    );
    drop((s0, c1, s1, c2));

    // L: a local landing on p1, with no tag
    let mut l = Rdy::open(&ld).unwrap();
    l.store_mut()
        .write_sync(
            "origin",
            vec![SyncStep::Canon(SyncCanon {
                repo_id: repo,
                url: HUB_URL.into(),
                fetched: Some(p1),
                applied: Some(p1),
                held: None,
            })],
        )
        .unwrap();
    doc(
        &mut l,
        "w2",
        "Local",
        "A local landing the hub has not seen.",
    );
    let lh = land(&mut l, "w2");

    // fetch the hub's canon from the holder, then pull with a clean replay
    let snap = hub.snapshot().unwrap();
    snap.write_history(
        canon,
        p2,
        Some(p1),
        &BTreeSet::<ObjectKey>::new(),
        File::create(at("fetch")).unwrap(),
    )
    .unwrap();
    drop(snap);
    let target = FetchTarget {
        remote: "origin".into(),
        url: HUB_URL.into(),
        purpose: canon,
        root: p2,
        view: None,
    };
    sync_fetch::fetch(&mut l, &files(&[&at("fetch")]), &target).unwrap();
    let puller = Session {
        actor: "human:puller".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    };
    let out = sync_replay::pull(
        &mut l,
        &puller,
        &PullRequest {
            remote: "origin".into(),
            hub: HubState {
                repo_id: Some(repo),
                canon: Some(p2),
            },
            take: false,
            strict: false,
        },
    )
    .unwrap();
    assert_eq!(out.status, PULLED, "{out:?}");
    let l2 = l.store().reader().canon_head().unwrap().unwrap();
    let rec = l.store().reader().landing(&l2).unwrap();
    assert_eq!(rec.prev, Some(p2));
    assert_eq!(rec.rebased_from, [lh]);
    l.close().unwrap();
    let push = at("push");
    export(&ld, canon, l2, Some(p2), &push);

    // the hub takes the push
    let s = hub.snapshot().unwrap();
    let c = check(&s, &[&push]).unwrap();
    assert_eq!(
        hub.move_canon(&c, CanonMove::Extend, Some(p2)).unwrap(),
        moved(l2)
    );
    drop((s, c));
    Replayed {
        dir,
        hub,
        p2,
        l: lh,
        l2,
    }
}
