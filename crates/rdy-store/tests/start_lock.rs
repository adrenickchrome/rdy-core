//! The project's start lock and its handoff to a launched daemon (the store's half of Task 8a).
//! One process plays both sides here: two handles of one file exclude each other on every
//! platform (the isolated probe in `probes/start-lock-transfer`), so no case needs a timer.
use rdy_store::layout::{RUN_DIR, START_LOCK_FILE};
use rdy_store::start_lock::{
    ClaimOutcome, FileLock, HandoffToken, RESERVATION_FILE, ReservationState, StartClaim,
    StartGuard,
};
use std::path::{Path, PathBuf};

const PID: u32 = 4242;

/// A project's `.rdy`, with no run directory yet.
fn project() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let rdy = d.path().join(".rdy");
    std::fs::create_dir(&rdy).unwrap();
    (d, rdy)
}

fn take(rdy: &Path) -> StartGuard {
    StartGuard::try_take(rdy)
        .unwrap()
        .expect("the lock is free")
}

fn claim(rdy: &Path, token: &HandoffToken, pid: u32) -> StartClaim {
    match StartClaim::try_claim(rdy, token, pid).unwrap() {
        ClaimOutcome::Claimed(c) => c,
        ClaimOutcome::Busy => panic!("busy"),
        ClaimOutcome::NotOurs => panic!("not ours"),
    }
}

fn is_not_ours(out: ClaimOutcome) -> bool {
    matches!(out, ClaimOutcome::NotOurs)
}

fn reservation(rdy: &Path) -> Vec<u8> {
    std::fs::read(rdy.join(RUN_DIR).join(RESERVATION_FILE)).unwrap_or_default()
}

fn raw_lock_free(rdy: &Path) -> bool {
    FileLock::try_take(&rdy.join(RUN_DIR).join(START_LOCK_FILE))
        .unwrap()
        .is_some()
}

#[test]
fn wx8a_guard_excludes_and_each_release_frees_only_its_own_lock() {
    let (_d, rdy) = project();
    let first = take(&rdy);
    assert!(rdy.join(RUN_DIR).is_dir(), "the guard makes the run dir");
    assert!(StartGuard::try_take(&rdy).unwrap().is_none(), "held: busy");
    drop(first);
    let _second = take(&rdy);
    // another project's guard is not affected
    let (_e, other) = project();
    drop(take(&other));
    assert!(StartGuard::try_take(&rdy).unwrap().is_none(), "still held");
}

#[test]
fn wx8a_guard_identifies_its_project() {
    let (_d, rdy) = project();
    let (_e, other) = project();
    let guard = take(&rdy);
    assert!(guard.guards(&rdy));
    assert!(!guard.guards(&other), "another project's .rdy");
    // Windows refuses to rename a directory with an open file in it
    if cfg!(windows) {
        return;
    }
    // the same name now names another directory: a restored project is another project
    let aside = rdy.with_extension("aside");
    std::fs::rename(&rdy, &aside).unwrap();
    std::fs::create_dir(&rdy).unwrap();
    assert!(!guard.guards(&rdy), "a replaced .rdy");
    assert!(guard.guards(&aside), "the directory itself, renamed");
}

#[test]
fn wx8a_a_guard_needs_a_project_directory_and_invents_none() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join(".rdy");
    assert!(StartGuard::try_take(&missing).is_err());
    assert!(!missing.exists(), "a guard never makes .rdy");
}

#[test]
fn wx8a_claim_needs_the_token_and_the_launched_process() {
    let (_d, rdy) = project();
    let guard = take(&rdy);
    let token = HandoffToken::new();
    let reserved = guard.reserve(PID, &token).unwrap().expect("reserved");
    let before = reservation(&rdy);
    assert_eq!(reserved.state().unwrap(), ReservationState::Unclaimed);

    let other = HandoffToken::new();
    assert!(is_not_ours(
        StartClaim::try_claim(&rdy, &other, PID).unwrap()
    ));
    assert!(is_not_ours(
        StartClaim::try_claim(&rdy, &token, PID + 1).unwrap()
    ));
    assert_eq!(reservation(&rdy), before, "a refused claim writes nothing");
    assert_eq!(reserved.state().unwrap(), ReservationState::Unclaimed);

    let held = claim(&rdy, &token, PID);
    assert_eq!(reserved.state().unwrap(), ReservationState::Claimed);
    assert!(
        matches!(
            StartClaim::try_claim(&rdy, &token, PID).unwrap(),
            ClaimOutcome::Busy
        ),
        "the claim's holder excludes a second claim"
    );
    drop(held);
    assert_eq!(reserved.state().unwrap(), ReservationState::Ended);
    assert!(
        is_not_ours(StartClaim::try_claim(&rdy, &token, PID).unwrap()),
        "a token is good for one claim"
    );
    assert_eq!(reservation(&rdy), b"", "the claim's end clears it");
}

#[test]
fn wx8a_a_reservation_is_private_and_at_most_256_bytes() {
    let (_d, rdy) = project();
    let guard = take(&rdy);
    let token = HandoffToken::new();
    let _reserved = guard.reserve(u32::MAX, &token).unwrap().unwrap();
    let text = reservation(&rdy);
    assert!(!text.is_empty() && text.len() <= 256, "{}", text.len());
    let shown = String::from_utf8(text).unwrap();
    let plain = String::from_utf8(token.text().to_vec()).unwrap();
    assert!(!shown.contains(&plain), "the file holds no token");
    assert!(shown.contains(&u32::MAX.to_string()), "{shown}");
    let meta = rdy_store::pio::privacy(
        &std::fs::File::open(rdy.join(RUN_DIR).join(RESERVATION_FILE)).unwrap(),
    )
    .unwrap();
    assert!(meta.mine, "this account owns it");
}

/// The window this exists for: the parent has let start.lock go and the daemon has not taken it
/// yet. A rival finds the OS lock free and still gets no guard.
#[test]
fn wx8a_a_live_claim_excludes_every_ordinary_taker() {
    let (_d, rdy) = project();
    let parent = take(&rdy);
    let token = HandoffToken::new();
    let reserved = parent.reserve(PID, &token).unwrap().unwrap();
    let held = claim(&rdy, &token, PID);
    assert_eq!(reserved.state().unwrap(), ReservationState::Claimed);
    // the parent's lock is still its own until it lets go: the claim cannot take it
    let held = match held.try_into_guard().unwrap() {
        Err(c) => c,
        Ok(_) => panic!("took the lock its parent holds"),
    };
    drop(reserved);
    drop(parent);

    assert!(raw_lock_free(&rdy), "the OS lock is free in the window");
    assert!(
        StartGuard::try_take(&rdy).unwrap().is_none(),
        "a rival cannot steal the handoff"
    );

    let owner = match held.try_into_guard().unwrap() {
        Ok(g) => g,
        Err(_) => panic!("the lock is free"),
    };
    assert!(owner.guards(&rdy));
    assert!(StartGuard::try_take(&rdy).unwrap().is_none(), "owned");
    drop(owner);
    assert_eq!(reservation(&rdy), b"", "ownership's end clears it");
    let _rival = take(&rdy);
}

/// A parent that died (or gave up) before its child claimed: the next taker clears the
/// reservation, and the child it launched finds nothing to claim.
#[test]
fn wx8a_a_dead_parents_reservation_ends_with_the_next_taker() {
    let (_d, rdy) = project();
    let parent = take(&rdy);
    let token = HandoffToken::new();
    drop(parent.reserve(PID, &token).unwrap().unwrap());
    drop(parent);
    assert!(!reservation(&rdy).is_empty());

    let next = take(&rdy);
    assert_eq!(reservation(&rdy), b"");
    assert!(is_not_ours(
        StartClaim::try_claim(&rdy, &token, PID).unwrap()
    ));
    drop(next);
}

#[test]
fn wx8a_withdraw_touches_only_its_own_reservation() {
    let (_d, rdy) = project();
    let guard = take(&rdy);
    let (mine, theirs) = (HandoffToken::new(), HandoffToken::new());
    let reserved = guard.reserve(PID, &mine).unwrap().unwrap();
    // a later start's record replaces it (the first start's owner is gone)
    let later = guard.reserve(PID + 1, &theirs).unwrap().unwrap();
    let their_text = reservation(&rdy);
    assert!(!reserved.withdraw().unwrap(), "no longer its own");
    assert_eq!(reservation(&rdy), their_text);
    // a claim in progress is not withdrawn
    let held = claim(&rdy, &theirs, PID + 1);
    assert_eq!(later.state().unwrap(), ReservationState::Claimed);
    assert!(!later.withdraw().unwrap(), "claimed: its owner ends it");
    drop(held);
    // its own, unclaimed: cleared
    let third = HandoffToken::new();
    let again = guard.reserve(PID, &third).unwrap().unwrap();
    assert!(again.withdraw().unwrap());
    assert_eq!(reservation(&rdy), b"");
    assert!(is_not_ours(
        StartClaim::try_claim(&rdy, &third, PID).unwrap()
    ));
}

/// A claim bound to one project does not open another's handoff.
#[test]
fn wx8a_a_token_binds_one_project() {
    let (_d, rdy) = project();
    let (_e, other) = project();
    let guard = take(&rdy);
    let token = HandoffToken::new();
    let _reserved = guard.reserve(PID, &token).unwrap().unwrap();
    // the same token and process against another project's files
    let _other = take(&other);
    assert!(is_not_ours(
        StartClaim::try_claim(&other, &token, PID).unwrap()
    ));
    // the same bytes copied to another project: its identity differs
    std::fs::copy(
        rdy.join(RUN_DIR).join(RESERVATION_FILE),
        other.join(RUN_DIR).join(RESERVATION_FILE),
    )
    .unwrap();
    assert!(is_not_ours(
        StartClaim::try_claim(&other, &token, PID).unwrap()
    ));
}

/// What stands at the reservation's name but is not a record of this start claims nothing and
/// never crashes: garbage and an oversized file are no one's.
#[test]
fn wx8a_hostile_reservation_bytes_are_no_ones() {
    let (_d, rdy) = project();
    drop(take(&rdy));
    let path = rdy.join(RUN_DIR).join(RESERVATION_FILE);
    let token = HandoffToken::new();
    for bytes in [vec![0xffu8; 16], vec![b'x'; 4096], b"\n\n\n".to_vec()] {
        std::fs::write(&path, &bytes).unwrap();
        assert!(is_not_ours(
            StartClaim::try_claim(&rdy, &token, PID).unwrap()
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "left as found");
        drop(take(&rdy));
        assert_eq!(std::fs::read(&path).unwrap(), b"", "the next taker clears");
    }
}

/// A daemon that dies holding its claim and the lock leaves neither behind (the probe's case 5,
/// through this module).
#[test]
fn wx8a_a_dead_owner_leaves_nothing_held() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let (_d, rdy) = project();
    let parent = take(&rdy);
    let token = HandoffToken::new();
    // the reservation names the child's pid, which exists only after the spawn: the child waits
    // for the go line on its stdin, as a daemon waits for its token
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "wx8a_child_after_go",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("WX8A_GO_RDY", &rdy)
        .env("WX8A_GO_TOKEN", std::str::from_utf8(token.text()).unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let reserved = parent.reserve(child.id(), &token).unwrap().unwrap();
    drop(reserved);
    drop(parent);
    drop(child.stdin.take()); // the go: end of file
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    assert!(
        lines.any(|l| l.unwrap().contains("owned")),
        "the child owns the start"
    );
    assert!(StartGuard::try_take(&rdy).unwrap().is_none(), "owned");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(raw_lock_free(&rdy), "the OS released the lock");
    let _next = take(&rdy);
    assert_eq!(reservation(&rdy), b"", "the next taker clears the record");
}

/// The child case of the test above.
#[test]
fn wx8a_child_after_go() {
    let Some(rdy) = std::env::var_os("WX8A_GO_RDY") else {
        return;
    };
    use std::io::{Read, Write};
    let mut rest = Vec::new();
    std::io::stdin().read_to_end(&mut rest).unwrap();
    let token = HandoffToken::from_text(std::env::var("WX8A_GO_TOKEN").unwrap().as_bytes())
        .expect("a token");
    let rdy = PathBuf::from(rdy);
    let held = claim(&rdy, &token, std::process::id());
    let owner = match held.try_into_guard().unwrap() {
        Ok(g) => g,
        Err(_) => panic!("the parent let go before the go line"),
    };
    println!("owned");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
        let _ = &owner;
    }
}
