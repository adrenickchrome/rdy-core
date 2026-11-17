//! scc4 (D-4E32E, D-XNQ6M, D-3RSCW, D-47HVF): views that travel with a sync. A pushed view's
//! identity is its name guarded by its root; owner and pin stay per machine; a push carries
//! each op as it is; a pull moves, re-chains or takes each named view, or refuses that view
//! alone. The hub here is a project copy that takes each pushed chain as a fetch and moves its
//! view by the same pull, as the hub never rebases a view.
use rdy_engine::stream_export;
use rdy_engine::sync_fetch::{self, HubState};
use rdy_engine::sync_views::{self, HubView, ViewPullRequest, ViewPullStatus, ViewPulled};
use rdy_engine::view_api::OwnerInput;
use rdy_engine::*;
use rdy_model::{Hash, Role};
use rdy_qa::fixtures::*;
use rdy_store::hub_canon::{Replaced, ReplacedKind, replaced_ref};
use rdy_store::sync_state::{SyncStep, SyncView};
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use rdy_store::{ViewHead, ViewOpKind};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

const URL: &str = "https://127.0.0.1:9/r/demo";

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn sa() -> Session {
    as_agent("agent:a", "ra")
}

fn sb() -> Session {
    as_agent("agent:b", "rb")
}

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    A,
    B,
}

/// The hub's project and two machines, `a` and `b`, copies of one project after its seed
/// landing: a doc with section `section` and claim `claim`.
struct World {
    dir: tempfile::TempDir,
    hub: Rdy,
    a: Rdy,
    b: Rdy,
    section: String,
    claim: String,
    n: u32,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let hp = dir.path().join("hub");
    let mut hub = Rdy::init(&hp, "rdy-standard").unwrap();
    let (_doc, section, claim) = seed_doc(&mut hub, "seed");
    land(&mut hub, "seed").unwrap();
    drop(hub);
    copy_dir(&hp, &dir.path().join("a"));
    copy_dir(&hp, &dir.path().join("b"));
    World {
        hub: Rdy::open(&hp).unwrap(),
        a: Rdy::open(&dir.path().join("a")).unwrap(),
        b: Rdy::open(&dir.path().join("b")).unwrap(),
        dir,
        section,
        claim,
        n: 0,
    }
}

/// The view history of `view` at `head` in `from`, fetched into `to` as remote `origin`'s.
fn send(dir: &Path, n: &mut u32, from: &Rdy, to: &mut Rdy, view: &str, head: Hash) {
    let mut out = Vec::new();
    stream_export::write_history(
        &from.read(),
        StreamPurpose::ViewHistory,
        head,
        None,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    *n += 1;
    let path = dir.join(format!("batch-{n}"));
    std::fs::write(&path, out).unwrap();
    let target = FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::ViewHistory,
        root: head,
        view: Some(view.into()),
    };
    sync_fetch::fetch(to, &[File::open(&path).unwrap()], &target).unwrap();
}

fn hub_session() -> Session {
    writer()
}

/// The caller of a pull.
fn puller() -> Session {
    Session::human("human:puller".into(), Role::Writer).unwrap()
}

impl World {
    fn side(&mut self, s: Side) -> &mut Rdy {
        match s {
            Side::A => &mut self.a,
            Side::B => &mut self.b,
        }
    }

    fn side_ref(&self, s: Side) -> &Rdy {
        match s {
            Side::A => &self.a,
            Side::B => &self.b,
        }
    }

    fn hub_state(&self) -> HubState {
        let reader = self.hub.store().reader();
        HubState {
            repo_id: Some(reader.repo_id().unwrap().unwrap()),
            canon: reader.canon_head().unwrap(),
        }
    }

    /// Every view the hub holds, with its root and head.
    fn hub_views(&self) -> BTreeMap<String, HubView> {
        let st = self.hub.read();
        let reader = st.reader();
        let mut out = BTreeMap::new();
        for name in st.view_names() {
            let head = st.view_head(&name).unwrap().clone();
            let root = reader.view_chain_root(&name, &head.head).unwrap();
            out.insert(
                name,
                HubView {
                    root,
                    head: head.head,
                },
            );
        }
        out
    }

    fn head(&self, s: Side, view: &str) -> ViewHead {
        self.side_ref(s)
            .store()
            .reader()
            .view_head(view)
            .unwrap()
            .unwrap_or_else(|| panic!("{s:?} has view {view}"))
    }

    fn hub_head(&self, view: &str) -> Hash {
        self.hub_views()[view].head
    }

    fn push_plan(&self, s: Side, views: &[&str]) -> Result<Vec<sync_views::ViewPush>, RdyError> {
        let names: Vec<String> = views.iter().map(|v| v.to_string()).collect();
        sync_views::push_plan(&self.side_ref(s).read(), &names, &self.hub_views())
    }

    /// Pushes `views` from side `s`: the hub fetches each chain and moves its view to the
    /// pushed head, and `s` records the pushed head as fetched and applied.
    fn push(&mut self, s: Side, views: &[&str]) -> Vec<sync_views::ViewPush> {
        let plan = self.push_plan(s, views).unwrap();
        let repo = self.hub_state().repo_id.unwrap();
        for p in &plan {
            assert!(p.holds_hub_head, "{p:?}");
            let (dir, n) = (self.dir.path().to_path_buf(), &mut self.n);
            let from = match s {
                Side::A => &self.a,
                Side::B => &self.b,
            };
            send(&dir, n, from, &mut self.hub, &p.view, p.head);
            let hub = self.hub_state();
            let done = sync_views::pull_views(
                &mut self.hub,
                &hub_session(),
                &ViewPullRequest {
                    remote: "origin".into(),
                    hub,
                    views: vec![(p.view.clone(), Some(p.head))],
                    take: false,
                },
            )
            .unwrap();
            assert!(
                matches!(
                    done[0].status,
                    ViewPullStatus::New | ViewPullStatus::FastForward
                ),
                "the hub only extends a view: {done:?}"
            );
            let value = SyncView {
                repo_id: repo,
                url: URL.into(),
                root: p.root,
                fetched: Some(p.head),
                applied: Some(p.head),
            };
            self.side(s)
                .store_mut()
                .write_sync(
                    "origin",
                    vec![SyncStep::View {
                        name: p.view.clone(),
                        value,
                    }],
                )
                .unwrap();
        }
        plan
    }

    /// Fetches each of `views` the hub holds into side `s`, then pulls them.
    fn pull_req(
        &mut self,
        s: Side,
        views: &[&str],
        take: bool,
        hub: Option<HubState>,
    ) -> Result<Vec<ViewPulled>, RdyError> {
        let heads = self.hub_views();
        let mut req = vec![];
        for v in views {
            let head = heads.get(*v).map(|h| h.head);
            if let Some(head) = head {
                let (dir, n) = (self.dir.path().to_path_buf(), &mut self.n);
                let to = match s {
                    Side::A => &mut self.a,
                    Side::B => &mut self.b,
                };
                send(&dir, n, &self.hub, to, v, head);
            }
            req.push((v.to_string(), head));
        }
        let hub = hub.unwrap_or_else(|| self.hub_state());
        sync_views::pull_views(
            self.side(s),
            &puller(),
            &ViewPullRequest {
                remote: "origin".into(),
                hub,
                views: req,
                take,
            },
        )
    }

    fn pull(&mut self, s: Side, views: &[&str]) -> Vec<ViewPulled> {
        self.pull_req(s, views, false, None).unwrap()
    }

    fn write(&mut self, s: Side, who: &Session, view: &str, body: &str) -> WriteResult {
        let section = self.section.clone();
        write_as(self.side(s), who, view, claim(&section, body)).unwrap()
    }

    fn fsck_clean(&self, s: Side) {
        self.side_ref(s).store().check_objects().unwrap();
    }
}

/// The chain of view `view` at its head on side `s`, newest first.
fn chain(w: &World, s: Side, view: &str) -> Vec<(Hash, rdy_store::ViewOp)> {
    let head = w.head(s, view).head;
    w.side_ref(s)
        .store()
        .reader()
        .view_chain(view, &head)
        .unwrap()
}

fn included(w: &World, s: Side, view: &str) -> Vec<Hash> {
    w.side_ref(s)
        .store()
        .reader()
        .included(view)
        .unwrap()
        .to_vec()
}

#[test]
fn scc4_names_and_roots_guard_identity() {
    let mut w = world();
    // a and b each start a view named v: two roots
    w.write(Side::A, &sa(), "v", "from a");
    w.write(Side::B, &sb(), "v", "from b");
    w.push(Side::A, &["v"]);
    let a_root = chain(&w, Side::A, "v").last().unwrap().0;
    let b_root = chain(&w, Side::B, "v").last().unwrap().0;
    assert_ne!(a_root, b_root);
    let b_head = w.head(Side::B, "v");
    // a pull meets b's v with another root: that view is refused, with the pull's fix
    let got = w.pull(Side::B, &["v"]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].status, ViewPullStatus::Refused);
    let e = got[0].refusal.clone().unwrap();
    assert_eq!(e.code, E_VIEW_EXISTS);
    assert_eq!(
        e.message,
        format!("view v has local root {b_root} and hub root {a_root}")
    );
    assert_eq!(e.fix, "run rdy pull --take --view v");
    assert_eq!(w.head(Side::B, "v"), b_head, "a refusal writes nothing");
    // a push of b's v is refused too, with the push's fix
    let e = w.push_plan(Side::B, &["v"]).unwrap_err();
    assert_eq!(e.code, E_VIEW_EXISTS);
    assert_eq!(
        e.message,
        format!("view v has local root {b_root} and hub root {a_root}")
    );
    assert_eq!(e.fix, "fork the view to another name, then push that view");
    // the name keeps its root: a pull into a third name is not possible, the view travels by
    // its name only; with take, b's v moves to the hub's chain and b's old chain stays readable
    let got = w.pull_req(Side::B, &["v"], true, None).unwrap();
    assert_eq!(got[0].status, ViewPullStatus::Taken);
    assert_eq!(chain(&w, Side::B, "v").last().unwrap().0, a_root);
    assert!(
        w.b.store()
            .reader()
            .view_op(&b_head.head)
            .unwrap()
            .view
            .eq("v")
    );
    w.fsck_clean(Side::B);
}

#[test]
fn scc4_owner_and_pin_stay_local() {
    let mut w = world();
    w.a.view_create(&sa(), "v", None, true).unwrap();
    w.write(Side::A, &sa(), "v", "one");
    w.push(Side::A, &["v"]);
    // a view new to b arrives with no owner and unpinned
    let got = w.pull(Side::B, &["v"]);
    assert_eq!(got[0].status, ViewPullStatus::New);
    let bh = w.head(Side::B, "v");
    assert_eq!(bh.head, w.head(Side::A, "v").head);
    assert_eq!((bh.owner.clone(), bh.pinned), (None, false));
    // b hands the view to agent:b; a writes on and pushes
    w.b.view_own(
        &writer(),
        "v",
        Some(&OwnerInput {
            actor: "agent:b".into(),
            run: Some("rb".into()),
        }),
    )
    .unwrap();
    w.write(Side::A, &sa(), "v", "two");
    w.push(Side::A, &["v"]);
    let got = w.pull(Side::B, &["v"]);
    assert_eq!(got[0].status, ViewPullStatus::Rechained, "{got:?}");
    let bh = w.head(Side::B, "v");
    assert_eq!(bh.owner.as_ref().unwrap().actor, "agent:b");
    assert!(!bh.pinned);
    // b pushes its Own op; a's owner and pin do not change when a pulls it
    w.push(Side::B, &["v"]);
    let got = w.pull(Side::A, &["v"]);
    assert_eq!(got[0].status, ViewPullStatus::FastForward, "{got:?}");
    let ah = w.head(Side::A, "v");
    assert_eq!(ah.head, w.head(Side::B, "v").head);
    assert_eq!(ah.owner.as_ref().unwrap().actor, "agent:a");
    assert!(ah.pinned);
    // the Own op travelled as it is
    assert!(chain(&w, Side::A, "v").iter().any(|(_, op)| matches!(
        &op.op,
        ViewOpKind::Own { owner: Some(o) } if o.actor == "agent:b"
    )));
    w.fsck_clean(Side::A);
    w.fsck_clean(Side::B);
}

#[test]
fn scc4_push_keeps_op_bytes_and_requires_fork_source() {
    let mut w = world();
    w.write(Side::A, &sa(), "v", "one");
    w.a.view_fork(&sa(), "v", "f").unwrap();
    w.write(Side::A, &sa(), "f", "two");
    // a fork whose source the hub lacks and the call does not name is refused
    let e = w.push_plan(Side::A, &["f"]).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "view f forks from view v, which the hub does not hold"
    );
    assert_eq!(
        e.fix,
        "push the source view too, or start a view from canon and include the changes"
    );
    // named together, the source goes first whatever the call's order
    let plan = w.push(Side::A, &["f", "v"]);
    assert_eq!(
        plan.iter().map(|p| p.view.as_str()).collect::<Vec<_>>(),
        ["v", "f"]
    );
    assert_eq!(
        plan[0].base, None,
        "a first push sends the chain from its root"
    );
    // each op reached the hub as it is: same id, same record, owners included
    for v in ["v", "f"] {
        let local = chain(&w, Side::A, v);
        let hub_head = w.hub_head(v);
        let hub = w.hub.store().reader().view_chain(v, &hub_head).unwrap();
        assert_eq!(local, hub);
    }
    let first = chain(&w, Side::A, "f").last().unwrap().1.clone();
    assert!(matches!(
        first.op,
        ViewOpKind::Fork { ref from, owner: Some(ref o), .. } if from == "v" && o.actor == "agent:a"
    ));
    // with the source on the hub, the fork pushes alone
    w.write(Side::A, &sa(), "f", "three");
    let plan = w.push_plan(Side::A, &["f"]).unwrap();
    assert_eq!(plan[0].base, Some(w.hub_head("f")));
    assert!(plan[0].holds_hub_head);
}

#[test]
fn scc4_rechain_remaps_undo_and_drops_rebase() {
    let mut w = world();
    w.write(Side::A, &sa(), "v", "base");
    w.push(Side::A, &["v"]);
    w.pull(Side::B, &["v"]);
    // b: two unpushed commits, an undo of the second, then a local landing rebases v
    let c1 = w.write(Side::B, &sb(), "v", "b one").change;
    let c2 = w.write(Side::B, &sb(), "v", "b two").change;
    let o2 = w.head(Side::B, "v").head;
    let u = w.b.undo(&sb(), "v", Some(&o2.to_string()), false).unwrap();
    assert_eq!(h(&u.undone), o2);
    w.write(Side::B, &as_agent("agent:w", "rw"), "w", "landed on b");
    land(&mut w.b, "w").unwrap();
    let old = w.head(Side::B, "v").head;
    let rebase = chain(&w, Side::B, "v")[0].clone();
    assert!(matches!(rebase.1.op, ViewOpKind::Rebase { .. }));
    // a moves the hub's v
    let c3 = w.write(Side::A, &sa(), "v", "a two").change;
    w.push(Side::A, &["v"]);
    let got = w.pull(Side::B, &["v"]);
    let p = &got[0];
    assert_eq!(p.status, ViewPullStatus::Rechained, "{p:?}");
    assert_eq!(p.dropped_rebases, vec![rebase.0.to_string()]);
    assert_eq!(p.replaced.as_deref(), Some(old.to_string().as_str()));
    // b's claim of the view (its Own op), the two commits and the undo, oldest first
    let kinds: Vec<&str> = p
        .rechained
        .iter()
        .map(
            |r| match w.b.store().reader().view_op(&h(&r.source)).unwrap().op {
                ViewOpKind::Own { .. } => "own",
                ViewOpKind::Commit { .. } => "commit",
                ViewOpKind::Undo { .. } => "undo",
                _ => "other",
            },
        )
        .collect();
    assert_eq!(kinds, ["own", "commit", "commit", "undo"], "{p:?}");
    let map: BTreeMap<Hash, Hash> = p
        .rechained
        .iter()
        .map(|r| (h(&r.source), h(&r.op)))
        .collect();
    // the re-chained undo names the new id of the commit it undoes
    let new_undo = map[&h(&u.op)];
    let op = w.b.store().reader().view_op(&new_undo).unwrap();
    assert_eq!(op.op, ViewOpKind::Undo { target: map[&o2] });
    assert_eq!(op.actor, "agent:b", "a re-chained op keeps its author");
    // the chain holds the hub's head; the view rebase rule ran after the re-chain
    let ch = chain(&w, Side::B, "v");
    assert!(ch.iter().any(|(id, _)| *id == w.hub_head("v")));
    assert!(!ch.iter().any(|(id, _)| *id == rebase.0));
    assert!(p.rebase.is_some(), "b's canon is past the hub head's base");
    assert!(matches!(ch[0].1.op, ViewOpKind::Rebase { .. }));
    let inc = included(&w, Side::B, "v");
    assert!(inc.contains(&h(&c1)) && inc.contains(&h(&c3)));
    assert!(!inc.contains(&h(&c2)));
    // the old head stays readable under replaced/, kind view op
    let mark =
        w.b.store()
            .reader()
            .get_ref(&replaced_ref(&old))
            .unwrap()
            .map(|v| Replaced::decode(&v).unwrap())
            .unwrap();
    assert_eq!(mark.kind, ReplacedKind::ViewOp);
    assert_eq!(mark.by, ch[0].0);
    assert!(w.b.store().reader().view_op(&old).is_ok());
    let state =
        w.b.store()
            .reader()
            .sync_view("origin", "v")
            .unwrap()
            .unwrap();
    assert_eq!(state.applied, Some(w.hub_head("v")));
    w.fsck_clean(Side::B);
}

#[test]
fn scc4_rechain_onto_current_base_drops_landed_changes() {
    let mut w = world();
    w.write(Side::A, &sa(), "v", "base");
    w.push(Side::A, &["v"]);
    w.pull(Side::B, &["v"]);
    // b commits c on v, then lands c through another view: b's rebase of v drops c
    let c = w.write(Side::B, &sb(), "v", "b lands this").change;
    w.b.view_include(&sb(), "w", std::slice::from_ref(&c))
        .unwrap();
    land(&mut w.b, "w").unwrap();
    assert!(!included(&w, Side::B, "v").contains(&h(&c)));
    let b_rebase = chain(&w, Side::B, "v")[0].0;
    // a is a copy of b with b's canon; a takes the hub's v, rebases it onto that canon and
    // writes on, so the hub's v has its own rebase and a base equal to b's canon
    let a2 = w.dir.path().join("a2");
    // Windows refuses to copy a project that is open: close b while it is copied
    let b_path = w.dir.path().join("b");
    let spare = Rdy::init(&w.dir.path().join("spare"), "rdy-standard").unwrap();
    drop(std::mem::replace(&mut w.b, spare));
    copy_dir(&b_path, &a2);
    w.b = Rdy::open(&b_path).unwrap();
    w.a = Rdy::open(&a2).unwrap();
    let got = w.pull_req(Side::A, &["v"], true, None).unwrap();
    assert_eq!(got[0].status, ViewPullStatus::Taken, "{got:?}");
    let by = sa().provenance();
    assert!(
        w.a.store_mut()
            .view_rebase_rechained("v", &by)
            .unwrap()
            .is_some()
    );
    // the copy keeps b's local owner of v
    w.write(Side::A, &sb(), "v", "a on");
    w.push(Side::A, &["v"]);
    let canon = w.b.store().reader().canon_head().unwrap();
    assert_eq!(w.head(Side::A, "v").base, canon);
    // b's pull re-chains its commit of c and drops its rebase; the base is canon already, yet
    // the view rebase rule still drops the landed c
    let got = w.pull(Side::B, &["v"]);
    let p = &got[0];
    assert_eq!(p.status, ViewPullStatus::Rechained, "{p:?}");
    assert_eq!(p.dropped_rebases, vec![b_rebase.to_string()]);
    assert!(p.rebase.is_some(), "the landed c needs a rebase: {p:?}");
    let ch = chain(&w, Side::B, "v");
    match &ch[0].1.op {
        ViewOpKind::Rebase {
            landing, dropped, ..
        } => {
            assert_eq!(Some(*landing), canon);
            assert_eq!(dropped, &vec![h(&c)]);
        }
        op => panic!("the head is a rebase: {op:?}"),
    }
    assert_eq!(w.head(Side::B, "v").base, canon);
    assert!(!included(&w, Side::B, "v").contains(&h(&c)));
    let inflight = w.b.read().inflight_set().unwrap();
    assert!(!inflight.changes.contains(&h(&c)));
    w.fsck_clean(Side::B);
}

#[test]
fn scc4_close_and_undo_close_travel() {
    let mut w = world();
    w.write(Side::A, &sa(), "v", "one");
    w.push(Side::A, &["v"]);
    w.pull(Side::B, &["v"]);
    w.a.view_close(&sa(), &["v".to_string()], "done").unwrap();
    let close = w.head(Side::A, "v").head;
    w.push(Side::A, &["v"]);
    let got = w.pull(Side::B, &["v"]);
    assert_eq!(got[0].status, ViewPullStatus::FastForward);
    assert!(w.head(Side::B, "v").closed);
    w.a.undo(&sa(), "v", Some(&close.to_string()), false)
        .unwrap();
    assert!(!w.head(Side::A, "v").closed);
    w.push(Side::A, &["v"]);
    w.pull(Side::B, &["v"]);
    let bh = w.head(Side::B, "v");
    assert!(!bh.closed);
    assert_eq!(bh.head, w.head(Side::A, "v").head);
    w.fsck_clean(Side::B);
}

#[test]
fn scc4_rechain_records_conflicts() {
    let mut w = world();
    let c = w.claim.clone();
    w.write(Side::A, &sa(), "v", "start");
    w.push(Side::A, &["v"]);
    w.pull(Side::B, &["v"]);
    write_as(
        &mut w.a,
        &sa(),
        "v",
        json!([{"op": "update", "id": c, "body": "a says"}]),
    )
    .unwrap();
    write_as(
        &mut w.b,
        &sb(),
        "v",
        json!([{"op": "update", "id": c, "body": "b says"}]),
    )
    .unwrap();
    w.push(Side::A, &["v"]);
    let got = w.pull(Side::B, &["v"]);
    assert_eq!(got[0].status, ViewPullStatus::Rechained, "{got:?}");
    assert!(!got[0].conflicts.is_empty(), "{got:?}");
    let st = w.b.read();
    assert!(
        !st.conflicts(&Scope::View("v".into())).unwrap().is_empty(),
        "the view holds the conflict"
    );
}

#[test]
fn scc4_referenced_ops_and_open_proposals_refuse_only_one_view() {
    let mut w = world();
    for v in ["v", "x", "y"] {
        w.write(Side::A, &sa(), v, &format!("{v} start"));
    }
    w.push(Side::A, &["v", "x", "y"]);
    w.pull(Side::B, &["v", "x", "y"]);
    // b: unpushed work in each view; another view forks v after its unpushed op, and x has
    // an open proposal
    for v in ["v", "x", "y"] {
        w.write(Side::B, &sb(), v, &format!("{v} on b"));
    }
    w.b.view_fork(&sb(), "v", "vf").unwrap();
    rdy_qa::intake::answer_open_items(&mut w.b, &sb(), "x");
    let p =
        w.b.propose(&sb(), "x", "x", Some(rdy_qa::intake::TEST_SUMMARY))
            .unwrap();
    // the hub moves all three
    for v in ["v", "x", "y"] {
        w.write(Side::A, &sa(), v, &format!("{v} on a"));
    }
    w.push(Side::A, &["v", "x", "y"]);
    let before = (w.head(Side::B, "v"), w.head(Side::B, "x"));
    let got = w.pull(Side::B, &["v", "x", "y"]);
    let status: Vec<_> = got.iter().map(|g| g.status).collect();
    assert_eq!(
        status,
        [
            ViewPullStatus::Refused,
            ViewPullStatus::Refused,
            ViewPullStatus::Rechained
        ]
    );
    let e = got[0].refusal.clone().unwrap();
    assert_eq!(e.code, E_VIEW_DIVERGED);
    assert_eq!(
        e.message,
        "view v cannot re-chain because view vf names its unpushed work"
    );
    assert_eq!(
        e.fix,
        "push the view first if the hub head has not moved, or run rdy pull --take --view v"
    );
    let e = got[1].refusal.clone().unwrap();
    assert_eq!(e.code, E_VIEW_DIVERGED);
    let pref = w.b.resolve_proposal(&p.id).unwrap().to_string();
    assert!(pref.starts_with("P-"), "{pref}");
    assert_eq!(
        e.message,
        format!("view x cannot re-chain because {pref} names its unpushed work")
    );
    assert_eq!((w.head(Side::B, "v"), w.head(Side::B, "x")), before);
    // the refused views keep their old state; y's records the hub's head
    let reader = w.b.store().reader();
    assert_ne!(
        reader.sync_view("origin", "v").unwrap().unwrap().applied,
        Some(w.hub_head("v"))
    );
    assert_eq!(
        reader.sync_view("origin", "y").unwrap().unwrap().applied,
        Some(w.hub_head("y"))
    );
    w.fsck_clean(Side::B);
}

#[test]
fn scc4_take_view_keeps_old_chain_and_local_owner() {
    let mut w = world();
    w.write(Side::A, &sa(), "v", "start");
    w.push(Side::A, &["v"]);
    w.pull(Side::B, &["v"]);
    let mine = w.write(Side::B, &sb(), "v", "b only").change;
    w.b.view_create(&writer(), "keep", None, true).unwrap();
    let old = w.head(Side::B, "v");
    assert_eq!(old.owner.as_ref().unwrap().actor, "agent:b");
    w.write(Side::A, &sa(), "v", "a only");
    w.push(Side::A, &["v"]);
    let got = w.pull_req(Side::B, &["v"], true, None).unwrap();
    assert_eq!(got[0].status, ViewPullStatus::Taken);
    assert_eq!(
        got[0].replaced.as_deref(),
        Some(old.head.to_string().as_str())
    );
    let now = w.head(Side::B, "v");
    assert_eq!(now.head, w.hub_head("v"));
    assert_eq!((now.owner, now.pinned), (old.owner, old.pinned));
    assert!(!included(&w, Side::B, "v").contains(&h(&mine)));
    let mark =
        w.b.store()
            .reader()
            .get_ref(&replaced_ref(&old.head))
            .unwrap()
            .map(|v| Replaced::decode(&v).unwrap())
            .unwrap();
    assert_eq!((mark.kind, mark.by), (ReplacedKind::ViewOp, now.head));
    // the old chain stays readable and its change stays available to include
    assert!(w.b.store().reader().view_op(&old.head).is_ok());
    w.b.view_include(&sb(), "v", std::slice::from_ref(&mine))
        .unwrap();
    assert!(included(&w, Side::B, "v").contains(&h(&mine)));
    w.fsck_clean(Side::B);
}

#[test]
fn scc4_undo_own_refuses_after_hub_head() {
    let mut w = world();
    let to_b = OwnerInput {
        actor: "agent:b".into(),
        run: Some("rb".into()),
    };
    // before any push, an undo of an Own op is allowed
    w.write(Side::A, &sa(), "u", "local only");
    w.a.view_own(&writer(), "u", Some(&to_b)).unwrap();
    let own = w.head(Side::A, "u").head;
    w.a.undo(&writer(), "u", Some(&own.to_string()), false)
        .unwrap();
    assert_eq!(w.head(Side::A, "u").owner.unwrap().actor, "agent:a");
    // after a push the view has a hub head: the same undo is refused
    w.write(Side::A, &sa(), "v", "pushed");
    w.push(Side::A, &["v"]);
    w.a.view_own(&writer(), "v", Some(&to_b)).unwrap();
    let own = w.head(Side::A, "v").head;
    let before = w.head(Side::A, "v");
    let e =
        w.a.undo(&writer(), "v", Some(&own.to_string()), false)
            .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "view v has a hub head; undo cannot change its owner"
    );
    assert_eq!(e.fix, "use rdy view own");
    assert_eq!(w.head(Side::A, "v"), before);
    // an undo that does not touch the owner still works there
    let c = w.write(Side::A, &sb(), "v", "by b").change;
    let _ = c;
    let commit = w.head(Side::A, "v").head;
    w.a.undo(&sb(), "v", Some(&commit.to_string()), false)
        .unwrap();
}

#[test]
fn scc4_named_views_are_unique_and_exist_on_the_selected_side() {
    let mut w = world();
    w.write(Side::A, &sa(), "v", "one");
    // push: a name twice, and a view this machine lacks
    let e = w.push_plan(Side::A, &["v", "v"]).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_BAD_INPUT, "view v is named twice", "name each view once")
    );
    let e = w.push_plan(Side::A, &["nope"]).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND);
    assert_eq!(e.message, "no view \"nope\"");
    assert_eq!(e.fix, "list views with rdy view ls (MCP: rdy_view list)");
    w.push(Side::A, &["v"]);
    // pull: a name twice, and a view the hub lacks; neither writes
    let before = w.b.store().reader().view_head("v").unwrap();
    let e = w.pull_req(Side::B, &["v", "v"], false, None).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (E_BAD_INPUT, "view v is named twice", "name each view once")
    );
    let e = w
        .pull_req(Side::B, &["v", "nope"], false, None)
        .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND);
    assert_eq!(e.message, "view nope is not on remote origin");
    assert_eq!(
        e.fix,
        "check the name, or push the view from a machine that has it"
    );
    assert_eq!(w.b.store().reader().view_head("v").unwrap(), before);
}

#[test]
fn scc4_view_refusal_codes_messages_and_fixes_are_exact() {
    let (l, r) = (h(&"1".repeat(64)), h(&"2".repeat(64)));
    let cases = [
        (
            sync_views::view_exists("v", &l, &r),
            E_VIEW_EXISTS,
            format!("view v has local root {l} and hub root {r}"),
            "run rdy pull --take --view v",
        ),
        (
            sync_views::view_root("v", &l, &r),
            E_VIEW_EXISTS,
            format!("view v has local root {l} and hub root {r}"),
            "fork the view to another name, then push that view",
        ),
        (
            sync_views::view_diverged("v", "answer 1234567"),
            E_VIEW_DIVERGED,
            "view v cannot re-chain because answer 1234567 names its unpushed work".into(),
            "push the view first if the hub head has not moved, or run rdy pull --take --view v",
        ),
        (
            sync_views::fork_source("f", "v"),
            E_BAD_INPUT,
            "view f forks from view v, which the hub does not hold".into(),
            "push the source view too, or start a view from canon and include the changes",
        ),
        (
            sync_views::undo_owner("v"),
            E_BAD_INPUT,
            "view v has a hub head; undo cannot change its owner".into(),
            "use rdy view own",
        ),
        (
            sync_views::view_twice("v"),
            E_BAD_INPUT,
            "view v is named twice".into(),
            "name each view once",
        ),
        (
            sync_views::view_missing("v", "origin"),
            E_NOT_FOUND,
            "view v is not on remote origin".into(),
            "check the name, or push the view from a machine that has it",
        ),
    ];
    for (e, code, message, fix) in cases {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code, message.as_str(), fix)
        );
    }
    assert_eq!(core_error_code("E_VIEW_DIVERGED"), Some(E_VIEW_DIVERGED));
}

#[test]
fn scc4_applied_head_is_scoped_to_remote_and_repo() {
    let mut w = world();
    w.write(Side::A, &sa(), "v", "one");
    w.push(Side::A, &["v"]);
    w.pull(Side::B, &["v"]);
    let reader = w.b.store().reader();
    let state = reader.sync_view("origin", "v").unwrap().unwrap();
    assert_eq!(state.applied, Some(w.hub_head("v")));
    assert_eq!(reader.sync_view("backup", "v").unwrap(), None);
    // a hub reply with another repo id: the state is not usable, and the pull says so
    w.write(Side::B, &sb(), "v", "b two");
    w.write(Side::A, &sa(), "v", "a two");
    w.push(Side::A, &["v"]);
    let mut other = w.hub_state();
    let o = Rdy::init(&w.dir.path().join("other"), "rdy-standard").unwrap();
    other.repo_id = Some(o.store().reader().repo_id().unwrap().unwrap());
    let got = w.pull_req(Side::B, &["v"], false, Some(other)).unwrap();
    assert!(got[0].no_sync_state, "{got:?}");
    // the newest op both chains hold bounds the re-chain: only b's op moves
    assert_eq!(got[0].status, ViewPullStatus::Rechained);
    let reader = w.b.store().reader();
    let authors: Vec<String> = got[0]
        .rechained
        .iter()
        .map(|r| reader.view_op(&h(&r.source)).unwrap().actor)
        .collect();
    assert_eq!(
        authors,
        ["agent:b", "agent:b"],
        "b's claim and commit: {got:?}"
    );
    // the state written is this remote's, under the reply's repo
    let reader = w.b.store().reader();
    let state = reader.sync_view("origin", "v").unwrap().unwrap();
    assert_eq!(state.repo_id, other.repo_id.unwrap());
    assert_eq!(reader.sync_view("backup", "v").unwrap(), None);
    // a pull with the hub's repo again: the state is not usable until this pull rewrites it
    let got = w.pull(Side::B, &["v"]);
    assert_eq!(got[0].status, ViewPullStatus::UpToDate, "{got:?}");
    let reader = w.b.store().reader();
    let state = reader.sync_view("origin", "v").unwrap().unwrap();
    assert_eq!(state.repo_id, w.hub_state().repo_id.unwrap());
    w.fsck_clean(Side::B);
}
