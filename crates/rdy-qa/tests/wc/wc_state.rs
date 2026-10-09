//! wcst1: the working copy's local state lives in .rdy/wc/ (or a worktree's state directory) and is
//! never a store object (spec §12.1, D-QWJ6J). wclk1: every working-copy command takes its lock.
use rdy_engine::wc::state::*;
use rdy_engine::*;
use rdy_model::BlockId;
use std::time::Duration;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

#[test]
fn wcst1_an_unbound_place_is_e_wc_not_bound_and_state_round_trips() {
    let t = tempfile::tempdir().unwrap();
    drop(Rdy::init(t.path(), "rdy-standard").unwrap());
    std::fs::create_dir_all(t.path().join("sub/dir")).unwrap();
    let place = Place::locate(&t.path().join("sub/dir")).expect("a project above");
    assert_eq!(place.root, t.path());
    assert_eq!(place.state, t.path().join(WC_DIR));
    let mut st = WcState::load(&place).unwrap();
    assert_eq!(st.require_bound().unwrap_err().code, E_WC_NOT_BOUND);
    st.bound = Some(Bound {
        view: "work".into(),
        head: "canon:".into(),
        at_ms: 7,
    });
    st.base.blocks.insert(
        id("p-AAAAA"),
        BaseBlock {
            version: "ab".repeat(32),
            file: "intro.yaml".into(),
            hidden: Default::default(),
        },
    );
    st.base.files.insert("intro.yaml".into(), id("d-AAAAA"));
    st.stage.deleted.insert("old.yaml".into());
    st.conflicts.push(ConflictEntry {
        path: "intro.yaml".into(),
        kind: ConflictKind::Blocks,
        blocks: vec![id("p-AAAAA")],
        theirs_versions: Default::default(),
        local_name: None,
        store_name: None,
        store_parent: None,
    });
    st.pending = Some(Pending {
        nonce: new_nonce(),
        view: "work".into(),
        assigned: vec![],
        rewrite: vec![id("d-AAAAA")],
        files: vec!["intro.yaml".into()],
        at_ms: 9,
    });
    st.record_brief(BriefEntry {
        token: "t".into(),
        ids: vec![id("p-AAAAA")],
        view: "work".into(),
        at_ms: 1,
    });
    st.save(&place).unwrap();
    let back = WcState::load(&place).unwrap();
    assert_eq!(back, st);
    assert_eq!(back.require_bound().unwrap().view, "work");
    for f in [
        "bound.yaml",
        "base/blocks.json",
        "stage/deleted.yaml",
        "conflicts.yaml",
        "pending.yaml",
        "briefs.yaml",
    ] {
        assert!(place.state.join(f).exists(), "{f}");
    }
    st.pending = None;
    st.save(&place).unwrap();
    assert!(
        !place.state.join("pending.yaml").exists(),
        "an empty journal has no file"
    );
}

#[test]
fn wcst1_nonces_are_32_hex_and_distinct() {
    let a = new_nonce();
    let b = new_nonce();
    assert_eq!(a.len(), 32);
    assert!(
        a.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_ne!(a, b);
}

#[test]
fn wcst1_briefs_keep_the_newest_64() {
    let mut st = WcState::default();
    for i in 0..70 {
        st.record_brief(BriefEntry {
            token: format!("t{i}"),
            ids: vec![],
            view: "work".into(),
            at_ms: i,
        });
    }
    assert_eq!(st.briefs.len(), MAX_BRIEFS);
    assert_eq!(st.briefs[0].token, "t6");
    assert_eq!(st.briefs[63].token, "t69");
}

#[test]
fn wcst1_a_worktree_pointer_names_its_project_and_state() {
    let p = tempfile::tempdir().unwrap();
    drop(Rdy::init(p.path(), "rdy-standard").unwrap());
    let w = tempfile::tempdir().unwrap();
    std::fs::write(
        w.path().join(POINTER_FILE),
        format!("project: {}\nname: side\n", p.path().display()),
    )
    .unwrap();
    let place = Place::locate(w.path()).unwrap();
    assert_eq!(place.root, w.path());
    assert_eq!(place.project, p.path());
    assert_eq!(place.state, p.path().join(WORKTREES_DIR).join("side"));
}

#[test]
fn wclk1_a_second_holder_waits_then_gets_e_wc_locked() {
    let t = tempfile::tempdir().unwrap();
    let state = t.path().join("wc");
    let held = WcLock::take(&state, Duration::ZERO).unwrap();
    let e = WcLock::take(&state, Duration::from_millis(200)).unwrap_err();
    assert_eq!(e.code, E_WC_LOCKED, "{e}");
    drop(held);
    WcLock::take(&state, Duration::ZERO).unwrap();
}

#[test]
fn wclk1_a_waiter_gets_the_lock_once_it_is_released() {
    let t = tempfile::tempdir().unwrap();
    let state = t.path().join("wc");
    let held = WcLock::take(&state, Duration::ZERO).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let s2 = state.clone();
    let waiter = std::thread::spawn(move || {
        tx.send(()).unwrap();
        WcLock::take(&s2, Duration::from_secs(10)).map(|_| ())
    });
    rx.recv().unwrap();
    drop(held);
    waiter.join().unwrap().unwrap();
}

#[test]
fn msw10_a_relative_project_in_the_pointer_is_joined_to_its_directory() {
    let top = tempfile::tempdir().unwrap();
    let p = top.path().join("proj");
    std::fs::create_dir_all(&p).unwrap();
    drop(Rdy::init(&p, "rdy-standard").unwrap());
    let w = top.path().join("side");
    std::fs::create_dir_all(w.join("sub")).unwrap();
    std::fs::write(w.join(POINTER_FILE), "project: ../proj\nname: side\n").unwrap();
    let place = Place::locate(&w.join("sub")).unwrap();
    assert_eq!(place.root, w);
    assert_eq!(place.project, w.join("../proj"));
    assert!(place.project.join(".rdy").is_dir());
    assert_eq!(
        place.state,
        w.join("../proj").join(WORKTREES_DIR).join("side")
    );
}
