//! wx8c: the project entry points take the start lock once at the outer call and borrow it
//! inside. A direct open (project, holder, follower), a daemon start (which takes the same
//! guard) and a load or seed never reach the gap between a store's close and its reopen. A
//! cancelled entry point lets its guard go and leaves no run state it made.
use rdy_engine::follower::{FollowerCore, seed};
use rdy_engine::load::{PlaceStep, load, with_placement_hook_for_tests};
use rdy_engine::{E_LOCKED, Rdy, RdyError, Session, WriteRequest};
use rdy_model::{RepoId, Role};
use rdy_store::start_lock::StartGuard;
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn session(role: Role) -> Session {
    Session::human("human:founder".into(), role).unwrap()
}

/// A landed project in `base/p`, closed, and its dump.
fn dumped(base: &Path) -> PathBuf {
    let mut r = Rdy::init(&base.join("p"), "rdy-standard").unwrap();
    let ops =
        json!([{"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "page"}}]);
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    let dump = base.join("p.rdydump");
    r.read().dump(&dump).unwrap();
    r.close().unwrap();
    dump
}

fn refused<T>(what: &str, r: Result<T, RdyError>) -> RdyError {
    match r {
        Ok(_) => panic!("{what}: the entry point passed"),
        Err(e) => e,
    }
}

/// Whether the start lock of the project at `root` is held (what a daemon start tries first).
fn start_held(root: &Path) -> bool {
    StartGuard::try_take(&root.join(".rdy")).unwrap().is_none()
}

#[test]
fn wx8c_direct_open_and_daemon_start_cannot_steal_the_gap() {
    let base = tempfile::tempdir().unwrap();
    let dump = dumped(base.path());
    let to = base.path().join("loaded");
    let at = to.clone();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&seen);
    let hook = move |step: PlaceStep| {
        log.borrow_mut().push(step);
        // a daemon start takes this guard before it opens anything
        assert!(start_held(&at), "{step:?}: the start lock is free");
        let project = refused(&format!("{step:?} open"), Rdy::open(&at));
        let holder = refused(&format!("{step:?} holder"), Rdy::open_holder(&at));
        let follower = refused(&format!("{step:?} follower"), FollowerCore::open(&at));
        if step != PlaceStep::Closed {
            // before the rename the project has no store to open and says so first
            assert_eq!(project.code, E_LOCKED, "{step:?}: {project:?}");
        }
        assert_eq!(holder.code, E_LOCKED, "{step:?}: {holder:?}");
        assert_eq!(follower.code, E_LOCKED, "{step:?}: {follower:?}");
        Ok(())
    };
    with_placement_hook_for_tests(hook, || load(&dump, &to).unwrap());
    assert_eq!(
        *seen.borrow(),
        [
            PlaceStep::Closed,
            PlaceStep::Renamed,
            PlaceStep::Reopened,
            PlaceStep::Checked
        ]
    );
    // the load is over: nothing holds the lock, and the store opens
    assert!(!start_held(&to));
    Rdy::open(&to).unwrap().close().unwrap();
}

#[test]
fn wx8c_holder_and_follower_seed_borrow_the_install_guard() {
    let base = tempfile::tempdir().unwrap();
    let dump = dumped(base.path());

    // a follower seed installs its store under one guard held through every step
    let follower = base.path().join("f");
    let at = follower.clone();
    let steps = Rc::new(RefCell::new(0));
    let count = Rc::clone(&steps);
    let hook = move |step: PlaceStep| {
        *count.borrow_mut() += 1;
        assert!(start_held(&at), "{step:?}: the seed's start lock is free");
        Ok(())
    };
    with_placement_hook_for_tests(hook, || seed(&dump, &follower).unwrap());
    assert_eq!(*steps.borrow(), 4);
    assert!(!start_held(&follower));

    // a caller that holds the guard opens a follower and a holder under it
    let holder = base.path().join("h");
    Rdy::create_holder(&holder, RepoId([3; 16]))
        .unwrap()
        .close()
        .unwrap();
    for root in [&follower, &holder] {
        let start = StartGuard::try_take(&root.join(".rdy")).unwrap().unwrap();
        let plain = if root == &follower {
            refused("follower", FollowerCore::open(root))
        } else {
            refused("holder", Rdy::open_holder(root))
        };
        assert_eq!(plain.code, E_LOCKED, "{plain:?}");
        if root == &follower {
            let core = FollowerCore::open_under_start(root, &start).unwrap();
            drop(core);
        } else {
            Rdy::open_holder_under_start(root, &start)
                .unwrap()
                .close()
                .unwrap();
        }
        // the borrow took nothing: the guard still stands, then frees
        assert!(start.guards(&root.join(".rdy")));
        drop(start);
        assert!(!start_held(root));
    }

    // the guard of another project borrows nowhere
    let other = StartGuard::try_take(&follower.join(".rdy"))
        .unwrap()
        .unwrap();
    let e = refused("holder", Rdy::open_holder_under_start(&holder, &other));
    assert_eq!(e.code, E_LOCKED, "{e:?}");
    let e = refused("follower", FollowerCore::open_under_start(&holder, &other));
    assert_eq!(e.code, E_LOCKED, "{e:?}");
}

#[test]
fn wx8c_cancelled_entry_point_releases_its_guard() {
    let base = tempfile::tempdir().unwrap();
    let dump = dumped(base.path());
    let project = base.path().join("p");
    let holder = base.path().join("h");
    Rdy::create_holder(&holder, RepoId([4; 16]))
        .unwrap()
        .close()
        .unwrap();
    let follower = base.path().join("f");
    seed(&dump, &follower).unwrap();

    let no_run = |root: &Path| !root.join(".rdy/run").exists();
    // each refusal comes after the entry point took its guard
    let cases: Vec<(&str, PathBuf, RdyError)> = vec![
        ("holder open of a project", project.clone(), {
            refused("holder open", Rdy::open_holder(&project))
        }),
        ("follower open of a project", project.clone(), {
            refused("follower open", FollowerCore::open(&project))
        }),
        ("holder create over a store", project.clone(), {
            refused("create", Rdy::create_holder(&project, RepoId([5; 16])))
        }),
        ("holder create over a holder", holder.clone(), {
            refused("create", Rdy::create_holder(&holder, RepoId([6; 16])))
        }),
        ("follower open of a holder", holder.clone(), {
            refused("follower open", FollowerCore::open(&holder))
        }),
        ("project init over a project", project.clone(), {
            refused("init", Rdy::init(&project, "rdy-standard"))
        }),
    ];
    for (what, root, e) in cases {
        assert_ne!(
            e.code, E_LOCKED,
            "{what}: refused for the wrong reason: {e:?}"
        );
        assert!(no_run(&root), "{what}: the refused entry left run state");
        assert!(
            !start_held(&root),
            "{what}: the refused entry kept its guard"
        );
        std::fs::remove_dir_all(root.join(".rdy/run")).unwrap();
    }
    // the stores are untouched and open as before
    Rdy::open(&project).unwrap().close().unwrap();
    Rdy::open_holder(&holder).unwrap().close().unwrap();
    drop(FollowerCore::open(&follower).unwrap());
}
