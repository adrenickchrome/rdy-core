//! Task 2 of the kungfu plan: the bounded local count file and its worker.

use rdy_engine::kungfu_counts::{
    Case, Counts, Cycle, Day, FILE_NAME, Fault, Hooks, Key, LOCK_NAME, MAX_DEPTH, MAX_FILE_BYTES,
    MAX_PENDING_ROWS, MAX_ROWS, Recorder, Rows, cap_rows, parse_file, summarize, to_bytes,
};
use rdy_store::pio;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

const ALICE: &str = "human:alice";
const BOB: &str = "human:bob";
const AGENT: &str = "agent:builder";
/// 2026-11-06 12:00 UTC.
const NOON: i64 = 1_793_966_400;
const DAY_SECS: i64 = 86_400;
const WAIT: Duration = Duration::from_secs(60);

fn today() -> Day {
    Day::from_unix_secs(NOON)
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rdy = tmp.path().join(".rdy");
    fs::create_dir(&rdy).expect("rdy dir");
    (tmp, rdy)
}

fn run_dir(rdy: &Path) -> PathBuf {
    let run = rdy.join("run");
    if !run.exists() {
        pio::create_private_dir(&run).expect("run dir");
    }
    run
}

fn count_file(rdy: &Path) -> PathBuf {
    rdy.join("run").join(FILE_NAME)
}

fn key(day: Day, actor: &str, run: Option<&str>, ruleset: &str) -> Key {
    Key {
        day,
        actor: actor.to_owned(),
        run: run.map(str::to_owned),
        ruleset: ruleset.to_owned(),
    }
}

fn one(not_loaded: u64, lost: u64, wrong: u64) -> Counts {
    Counts {
        not_loaded,
        lost,
        wrong,
    }
}

struct Rig {
    rec: Recorder,
    rx: Receiver<Cycle>,
    clock: Arc<AtomicI64>,
    fault: Arc<Mutex<Fault>>,
}

fn hooks(clock: &Arc<AtomicI64>, fault: &Arc<Mutex<Fault>>, tx: mpsc::Sender<Cycle>) -> Hooks {
    let c = Arc::clone(clock);
    let f = Arc::clone(fault);
    Hooks {
        clock: Some(Arc::new(move || c.load(Ordering::SeqCst))),
        fault: Some(Arc::new(move || *f.lock().unwrap())),
        after_cycle: Some(Arc::new(move |cy| {
            let _ = tx.send(cy);
        })),
        retry: Duration::from_secs(3600),
        ..Hooks::default()
    }
}

fn rig(rdy: &Path) -> Rig {
    let clock = Arc::new(AtomicI64::new(NOON));
    let fault = Arc::new(Mutex::new(Fault::None));
    let (tx, rx) = mpsc::channel();
    let rec = Recorder::detached(rdy, hooks(&clock, &fault, tx)).expect("recorder");
    let first = rx.recv_timeout(WAIT).expect("first cycle");
    assert_eq!(first, Cycle::Loaded);
    Rig {
        rec,
        rx,
        clock,
        fault,
    }
}

/// Waits until nothing is pending, nudging the worker past a retry wait after a failure.
/// Waits for `want`. A worker reports a cycle after it clears its state, so a `settle` can return
/// with the last `Flushed` or `Loaded` report still to come: those two are skipped, and anything
/// else is the test's failure.
fn wait_for(rx: &mpsc::Receiver<Cycle>, want: Cycle) {
    loop {
        match rx.recv_timeout(WAIT).expect("a worker cycle") {
            cy if cy == want => return,
            Cycle::Flushed | Cycle::Loaded => {}
            cy => panic!("waiting for {want:?}, saw {cy:?}"),
        }
    }
}

fn settle(rig: &Rig) -> Vec<Cycle> {
    let mut seen = Vec::new();
    while !rig.rec.settled() {
        let cy = rig
            .rx
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("a worker cycle after {seen:?}"));
        seen.push(cy);
        if cy != Cycle::Flushed && cy != Cycle::Loaded {
            rig.rec.nudge();
        }
    }
    seen
}

fn disk_rows(rdy: &Path, at: Day) -> Rows {
    let bytes = fs::read(count_file(rdy)).expect("count file");
    parse_file(&bytes, at).expect("a valid count file")
}

fn file_with(rows: &Rows) -> Vec<u8> {
    to_bytes(rows)
}

#[test]
fn kfu2_daily_rows_expire_and_saturate() {
    let t = today();
    let at = |n: i64| t.minus(n);
    let mut rows = Rows::new();
    rows.insert(key(at(0), ALICE, None, "write"), one(1, 0, 0));
    rows.insert(key(at(29), ALICE, Some("r1"), "write"), one(0, 2, 0));
    rows.insert(key(at(30), ALICE, None, "write"), one(9, 9, 9));
    let future = Day::parse("2026-11-07").expect("day");
    rows.insert(key(future, ALICE, None, "write"), one(7, 7, 7));
    let read = parse_file(&file_with(&rows), t).expect("valid");
    assert_eq!(
        read.len(),
        2,
        "the expired day and the future day are dropped"
    );
    assert!(read.contains_key(&key(at(29), ALICE, Some("r1"), "write")));
    assert_eq!(Day::parse("2026-11-06"), Some(t));
    assert_eq!(t.to_string(), "2026-11-06");
    assert_eq!(Day::parse("2026-02-30"), None);
    assert_eq!(Day::parse("2026-2-3"), None);

    // totals and distinct sessions over the retained days
    let mut rows = Rows::new();
    rows.insert(key(at(0), ALICE, None, "write"), one(1, 0, 0));
    rows.insert(key(at(0), ALICE, None, "read"), one(0, 1, 0));
    rows.insert(key(at(0), ALICE, Some("r1"), "read"), one(0, 0, 1));
    rows.insert(key(at(1), ALICE, None, "read"), one(2, 0, 0));
    rows.insert(key(at(1), BOB, None, "read"), one(1, 1, 1));
    rows.insert(key(at(40), BOB, None, "read"), one(5, 5, 5));
    let s = summarize(&rows, t);
    assert_eq!(s.today, one(1, 1, 1));
    assert_eq!(s.total, one(4, 2, 2));
    assert_eq!(s.today_sessions, 2, "alice with no run, alice in r1");
    assert_eq!(
        s.sessions, 3,
        "bob adds one; alice's two sessions are not repeated"
    );

    // a recorder on a seeded file: the clock turns, old days leave the summary, counts saturate
    let (_tmp, rdy) = project();
    let run = run_dir(&rdy);
    let mut seeded = Rows::new();
    seeded.insert(key(t, ALICE, None, "write"), one(u64::MAX - 1, 0, 0));
    seeded.insert(key(at(29), BOB, None, "write"), one(0, 3, 0));
    fs::write(run.join(FILE_NAME), file_with(&seeded)).expect("seed");
    let rig = rig(&rdy);
    assert_eq!(
        rig.rec.summary().total,
        one(u64::MAX - 1, 3, 0),
        "the seeded file is read"
    );
    for _ in 0..5 {
        assert!(rig.rec.record(ALICE, None, "write", Case::NotLoaded));
    }
    assert!(rig.rec.record(ALICE, None, "write", Case::Lost));
    settle(&rig);
    let on_disk = disk_rows(&rdy, t);
    assert_eq!(on_disk[&key(t, ALICE, None, "write")], one(u64::MAX, 1, 0));
    // the next day: bob's row from 29 days ago is now 30 days old
    rig.clock.store(NOON + DAY_SECS, Ordering::SeqCst);
    let s = rig.rec.summary();
    assert_eq!(s.total, one(u64::MAX, 1, 0));
    assert_eq!(s.today, Counts::default());
    assert_eq!(s.sessions, 1);
    assert!(!rig.rec.rows().keys().any(|k| k.actor == BOB));
    // a record on the new day merges, and the write drops the expired row from the file
    assert!(rig.rec.record(AGENT, None, "read", Case::Wrong));
    settle(&rig);
    let next = Day::from_unix_secs(NOON + DAY_SECS);
    let on_disk = disk_rows(&rdy, next);
    assert!(!on_disk.keys().any(|k| k.actor == BOB));
    assert_eq!(on_disk[&key(next, AGENT, None, "read")], one(0, 0, 1));
}

#[test]
fn kfu2_fixed_limits_bound_allocations() {
    let t = today();
    // the longest identity: a 135-byte actor, a 64-byte run and a 64-byte ruleset
    let name = "a".repeat(64);
    let actor = format!("agent:{name}/{name}");
    assert_eq!(actor.len(), 135);
    let run = "r".repeat(64);
    let ruleset = "s".repeat(64);
    let mut full = Rows::new();
    'fill: for d in 0..30 {
        for n in 0..1000 {
            let tail = format!("{n:04}");
            let actor = format!("{}{tail}", &actor[..actor.len() - 4]);
            let mut counts = one(u64::MAX, u64::MAX, u64::MAX);
            counts.lost = u64::MAX;
            full.insert(key(t.minus(d), &actor, Some(&run), &ruleset), counts);
            if full.len() == MAX_ROWS {
                break 'fill;
            }
        }
    }
    assert_eq!(full.len(), MAX_ROWS);
    let bytes = to_bytes(&full);
    assert!(
        (bytes.len() as u64) < MAX_FILE_BYTES,
        "16,384 maximum rows are {} bytes",
        bytes.len()
    );
    let back = parse_file(&bytes, t).expect("valid");
    assert_eq!(back.len(), MAX_ROWS);

    // 16,385 rows: the oldest day's last row in key order is dropped, deterministically
    let mut over = Rows::new();
    for n in 0..=MAX_ROWS {
        let day = t.minus(i64::try_from(n % 3).unwrap());
        over.insert(
            key(day, ALICE, Some(&format!("r{n}")), "write"),
            one(1, 0, 0),
        );
    }
    assert_eq!(over.len(), MAX_ROWS + 1);
    let read = parse_file(&to_bytes(&over), t).expect("valid");
    assert_eq!(read.len(), MAX_ROWS);
    let dropped: Vec<&Key> = over.keys().filter(|k| !read.contains_key(k)).collect();
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].day, t.minus(2), "the oldest day loses first");
    let last_old = over
        .keys()
        .filter(|k| k.day == t.minus(2))
        .max()
        .expect("old row");
    assert_eq!(dropped[0], last_old, "and its last row in key order");
    let mut again = over.clone();
    cap_rows(&mut again);
    assert_eq!(again, read);

    // the byte boundary, checked before the JSON is read
    let mut small = Rows::new();
    small.insert(key(t, ALICE, None, "write"), one(1, 0, 0));
    let mut padded = to_bytes(&small);
    padded.resize(usize::try_from(MAX_FILE_BYTES).unwrap(), b' ');
    assert!(parse_file(&padded, t).is_some(), "exactly 8 MiB is read");
    padded.push(b' ');
    assert!(parse_file(&padded, t).is_none(), "one byte over is damaged");

    // nesting: the closed file is three deep; a fourth level is the limit; a fifth is damage
    assert_eq!(MAX_DEPTH, 4);
    let deep = |n: usize| {
        format!(
            "{{\"schema\":1,\"rows\":{}{}}}",
            "[".repeat(n),
            "]".repeat(n)
        )
    };
    assert!(parse_file(deep(1).as_bytes(), t).is_some());
    assert!(
        parse_file(deep(2).as_bytes(), t).is_none(),
        "an array in rows is not a row"
    );
    let hostile = format!("{}{}", "[".repeat(1_000_000), "]".repeat(1_000_000));
    assert!(
        parse_file(hostile.as_bytes(), t).is_none(),
        "deep nesting never recurses"
    );

    // the pending bound: distinct rows past it are lost, a row already waiting still counts
    let (_tmp, rdy) = project();
    let gate_in = Arc::new(Barrier::new(2));
    let gate_out = Arc::new(Barrier::new(2));
    let (tx, rx) = mpsc::channel();
    let (i, o) = (Arc::clone(&gate_in), Arc::clone(&gate_out));
    let hooks = Hooks {
        clock: Some(Arc::new(|| NOON)),
        before_flush: Some(Arc::new(move || {
            i.wait();
            o.wait();
        })),
        after_cycle: Some(Arc::new(move |c| {
            let _ = tx.send(c);
        })),
        retry: Duration::from_secs(3600),
        ..Hooks::default()
    };
    let rec = Recorder::detached(&rdy, hooks).expect("recorder");
    assert_eq!(rx.recv_timeout(WAIT).expect("load"), Cycle::Loaded);
    assert!(rec.record(ALICE, None, "write", Case::Wrong));
    gate_in.wait(); // the worker holds a snapshot of one row and waits
    for n in 0..MAX_PENDING_ROWS {
        assert!(
            rec.record(ALICE, Some(&format!("run{n}")), "write", Case::Wrong),
            "row {n}"
        );
    }
    assert!(!rec.record(ALICE, Some("overflow"), "write", Case::Wrong));
    assert!(
        rec.record(ALICE, Some("run0"), "write", Case::Lost),
        "an existing row still counts"
    );
    assert_eq!(rec.dropped(), 1);
    assert!(!rec.record("not an actor", None, "write", Case::Lost));
    assert!(
        !rec.record(ALICE, Some("none"), "write", Case::Lost),
        "a reserved run is no run"
    );
    assert!(!rec.record(ALICE, None, "Bad Name", Case::Lost));
    gate_out.wait();
}

#[test]
fn kfu2_concurrent_writers_merge_without_lost_updates() {
    let (_tmp, rdy) = project();
    let barrier = Arc::new(Barrier::new(2));
    let mut rigs = Vec::new();
    for _ in 0..2 {
        let clock = Arc::new(AtomicI64::new(NOON));
        let fault = Arc::new(Mutex::new(Fault::None));
        let (tx, rx) = mpsc::channel();
        let mut h = hooks(&clock, &fault, tx);
        let (b, first) = (Arc::clone(&barrier), Arc::new(AtomicBool::new(true)));
        h.before_flush = Some(Arc::new(move || {
            if first.swap(false, Ordering::SeqCst) {
                b.wait(); // both workers hold a snapshot before either writes
            }
        }));
        let rec = Recorder::detached(&rdy, h).expect("recorder");
        assert_eq!(rx.recv_timeout(WAIT).expect("load"), Cycle::Loaded);
        rigs.push(Rig {
            rec,
            rx,
            clock,
            fault,
        });
    }
    for _ in 0..3 {
        rigs[0].rec.record(ALICE, None, "write", Case::NotLoaded);
        rigs[1].rec.record(ALICE, None, "write", Case::NotLoaded);
    }
    rigs[0].rec.record(ALICE, None, "read", Case::Lost);
    rigs[1].rec.record(BOB, Some("r2"), "write", Case::Wrong);
    std::thread::scope(|s| {
        for r in rigs.iter_mut() {
            s.spawn(move || settle(&*r));
        }
    });
    let rows = disk_rows(&rdy, today());
    assert_eq!(rows[&key(today(), ALICE, None, "write")], one(6, 0, 0));
    assert_eq!(rows[&key(today(), ALICE, None, "read")], one(0, 1, 0));
    assert_eq!(rows[&key(today(), BOB, Some("r2"), "write")], one(0, 0, 1));
    // an outside holder of the same lock file keeps both out until it lets go
    let lock = File::open(rdy.join("run").join(LOCK_NAME)).expect("lock file");
    lock.lock().expect("lock");
    rigs[0].rec.record(AGENT, None, "read", Case::Lost);
    wait_for(&rigs[0].rx, Cycle::Contended);
    lock.unlock().expect("unlock");
    rigs[0].rec.nudge();
    settle(&rigs[0]);
    assert_eq!(
        disk_rows(&rdy, today())[&key(today(), AGENT, None, "read")],
        one(0, 1, 0)
    );
}

#[test]
fn kfu2_contention_and_io_failure_never_reach_the_caller() {
    let (_tmp, rdy) = project();
    run_dir(&rdy);
    let rig = rig(&rdy);
    // a first flush makes the lock file
    rig.rec.record(ALICE, None, "write", Case::NotLoaded);
    settle(&rig);
    let lock = File::open(rdy.join("run").join(LOCK_NAME)).expect("lock file");
    lock.lock().expect("lock");
    for _ in 0..50 {
        assert!(
            rig.rec.record(ALICE, None, "write", Case::Lost),
            "a miss is accepted"
        );
    }
    wait_for(&rig.rx, Cycle::Contended);
    assert!(!rig.rec.settled());
    assert_eq!(
        rig.rec.summary().total,
        one(1, 50, 0),
        "the miss is in memory meanwhile"
    );
    // an injected write failure
    *rig.fault.lock().unwrap() = Fault::BeforeReplace;
    lock.unlock().expect("unlock");
    rig.rec.record(ALICE, None, "write", Case::Wrong);
    rig.rec.nudge();
    loop {
        let cy = rig.rx.recv_timeout(WAIT).expect("cycle");
        if cy == Cycle::Failed {
            break;
        }
        assert_ne!(cy, Cycle::Flushed, "the injected failure keeps the counts");
    }
    assert_eq!(rig.rec.summary().total, one(1, 50, 1));
    assert!(rig.rec.record(BOB, None, "read", Case::Lost));
    *rig.fault.lock().unwrap() = Fault::None;
    rig.rec.nudge();
    settle(&rig);
    let rows = disk_rows(&rdy, today());
    assert_eq!(rows[&key(today(), ALICE, None, "write")], one(1, 50, 1));
    assert_eq!(rows[&key(today(), BOB, None, "read")], one(0, 1, 0));
    assert_eq!(rig.rec.summary().total, one(1, 51, 1));
}

#[test]
fn kfu2_damaged_file_recovers_without_touching_store() {
    let t = today();
    let valid = to_bytes(&{
        let mut r = Rows::new();
        r.insert(key(t, ALICE, None, "write"), one(1, 1, 1));
        r
    });
    let text = String::from_utf8(valid.clone()).expect("utf8");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated", valid[..valid.len() / 2].to_vec()),
        ("invalid", b"not json at all".to_vec()),
        ("duplicate row", {
            let row = text
                .split("\"rows\":[")
                .nth(1)
                .unwrap()
                .trim_end_matches("]}");
            format!("{{\"schema\":1,\"rows\":[{row},{row}]}}").into_bytes()
        }),
        (
            "duplicate key",
            text.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1)
                .into_bytes(),
        ),
        (
            "unknown key",
            text.replacen("\"lost\"", "\"extra\":1,\"lost\"", 1)
                .into_bytes(),
        ),
        (
            "missing run",
            text.replacen("\"run\":null,", "", 1).into_bytes(),
        ),
        (
            "wrong schema",
            text.replacen("\"schema\":1", "\"schema\":2", 1)
                .into_bytes(),
        ),
        (
            "bad day",
            text.replacen("2026-11-06", "2026-13-06", 1).into_bytes(),
        ),
        (
            "bad actor",
            text.replacen("human:alice", "alice", 1).into_bytes(),
        ),
        (
            "oversized",
            vec![b' '; usize::try_from(MAX_FILE_BYTES).unwrap() + 1],
        ),
    ];
    for (what, bytes) in cases {
        let (_tmp, rdy) = project();
        fs::write(rdy.join("log"), b"LOG").expect("log");
        fs::write(rdy.join("index"), b"INDEX").expect("index");
        let run = run_dir(&rdy);
        fs::write(run.join(FILE_NAME), &bytes).expect("damaged file");
        assert!(parse_file(&bytes, t).is_none(), "{what}: damaged");
        let rig = rig(&rdy);
        assert_eq!(
            rig.rec.summary(),
            Default::default(),
            "{what}: reads as empty"
        );
        assert_eq!(
            fs::read(run.join(FILE_NAME)).unwrap(),
            bytes,
            "{what}: not repaired by a read"
        );
        assert!(rig.rec.record(AGENT, None, "read", Case::Lost));
        settle(&rig);
        let rows = disk_rows(&rdy, t);
        assert_eq!(rows.len(), 1, "{what}: replaced with valid pending counts");
        assert_eq!(rows[&key(t, AGENT, None, "read")], one(0, 1, 0));
        assert_eq!(fs::read(rdy.join("log")).unwrap(), b"LOG");
        assert_eq!(fs::read(rdy.join("index")).unwrap(), b"INDEX");
        let mut names: Vec<String> = fs::read_dir(&rdy)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["index", "log", "run"],
            "{what}: nothing else in the store"
        );
        let mut run_names: Vec<String> = fs::read_dir(&run)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        run_names.sort();
        assert_eq!(
            run_names,
            [FILE_NAME, LOCK_NAME],
            "{what}: no temp file left"
        );
    }
}

#[test]
fn kfu2_a_missing_run_directory_is_made_private_and_the_files_are_private() {
    let (_tmp, rdy) = project();
    let r = rig(&rdy);
    r.rec.record(ALICE, None, "write", Case::Wrong);
    settle(&r);
    let run = rdy.join("run");
    for handle in [
        pio::open_dir(&run).unwrap(),
        File::open(count_file(&rdy)).unwrap(),
        File::open(run.join(LOCK_NAME)).unwrap(),
    ] {
        let privacy = pio::privacy(&handle).unwrap();
        assert!(privacy.mine && privacy.private, "{privacy:?}");
    }
}

/// Unix only: this test sets permission modes and makes symbolic links.
#[cfg(unix)]
#[test]
fn kfu2_private_paths_and_replaced_projects_are_safe() {
    let t = today();
    // a run directory that is a link is never followed
    let (tmp, rdy) = project();
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    pio::symlink(&elsewhere, rdy.join("run")).expect("link");
    let r = rig(&rdy);
    assert!(r.rec.record(ALICE, None, "write", Case::Wrong));
    wait_for(&r.rx, Cycle::Failed);
    assert_eq!(
        fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "nothing written through the link"
    );
    assert_eq!(r.rec.summary().total, one(0, 0, 1), "the miss is kept");
    drop(r);

    // a count file or lock file that is a link is never followed
    for victim in [FILE_NAME, LOCK_NAME] {
        let (tmp, rdy) = project();
        let run = run_dir(&rdy);
        let target = tmp.path().join("target");
        fs::write(&target, b"TARGET").unwrap();
        pio::symlink(&target, run.join(victim)).expect("link");
        let r = rig(&rdy);
        assert!(r.rec.record(ALICE, None, "write", Case::Wrong));
        assert_eq!(
            r.rx.recv_timeout(WAIT).expect("cycle"),
            Cycle::Failed,
            "{victim}"
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            b"TARGET",
            "{victim}: target untouched"
        );
    }

    // a run directory open to others disables the flush until it is private again
    let (_tmp, rdy) = project();
    let run = run_dir(&rdy);
    pio::set_mode(&run, 0o755).unwrap();
    let r = rig(&rdy);
    r.rec.record(ALICE, None, "write", Case::Wrong);
    wait_for(&r.rx, Cycle::Failed);
    assert!(!count_file(&rdy).exists());
    pio::set_mode(&run, 0o700).unwrap();
    r.rec.nudge();
    settle(&r);
    assert_eq!(
        disk_rows(&rdy, t)[&key(t, ALICE, None, "write")],
        one(0, 0, 1)
    );
    drop(r);

    // a lock file open to others is refused
    let (_tmp, rdy) = project();
    let run = run_dir(&rdy);
    File::create(run.join(LOCK_NAME)).unwrap();
    pio::set_mode(&run.join(LOCK_NAME), 0o666).unwrap();
    let r = rig(&rdy);
    r.rec.record(ALICE, None, "write", Case::Wrong);
    wait_for(&r.rx, Cycle::Failed);
    assert!(!count_file(&rdy).exists());
}

#[test]
fn kfu2_replaced_projects_never_get_old_counts() {
    let t = today();
    let tmp = tempfile::tempdir().unwrap();
    let rdy = tmp.path().join(".rdy");
    fs::create_dir(&rdy).unwrap();
    let (tx, rx) = mpsc::channel();
    let clock = Arc::new(AtomicI64::new(NOON));
    let fault = Arc::new(Mutex::new(Fault::None));
    let old = Recorder::for_project_with(&rdy, hooks(&clock, &fault, tx)).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Cycle::Loaded);
    let again = Recorder::for_project(&rdy).unwrap();
    assert!(
        old.same_recorder(&again),
        "one recorder per project in a process"
    );
    drop(again);
    // the directory is replaced: moved aside and made again
    let moved = tmp.path().join(".rdy-old");
    fs::rename(&rdy, &moved).unwrap();
    fs::create_dir(&rdy).unwrap();
    let (tx2, rx2) = mpsc::channel();
    let new = Recorder::for_project_with(&rdy, hooks(&clock, &fault, tx2)).unwrap();
    assert!(
        !old.same_recorder(&new),
        "a replaced directory is a new project"
    );
    assert_eq!(rx2.recv_timeout(WAIT).unwrap(), Cycle::Loaded);
    old.record(ALICE, None, "write", Case::Lost);
    new.record(BOB, None, "read", Case::Wrong);
    while !old.settled() {
        rx.recv_timeout(WAIT).unwrap();
    }
    while !new.settled() {
        rx2.recv_timeout(WAIT).unwrap();
    }
    assert_eq!(old.summary().total, one(0, 1, 0));
    assert_eq!(new.summary().total, one(0, 0, 1));
    let old_rows = disk_rows(&moved, t);
    assert!(
        old_rows.keys().all(|k| k.actor == ALICE),
        "the old file holds only old counts"
    );
    let new_rows = disk_rows(&rdy, t);
    assert!(
        new_rows.keys().all(|k| k.actor == BOB),
        "the new file holds only new counts"
    );
    // dropping every handle lets the entry expire; a later lookup makes a fresh recorder
    drop(old);
    drop(new);
    let fresh = Recorder::for_project(&rdy).unwrap();
    assert!(fresh.record(AGENT, None, "read", Case::Lost));
}

#[test]
fn kfu2_uncertain_replace_does_not_double_count() {
    let t = today();
    for landed in [true, false] {
        let (_tmp, rdy) = project();
        let r = rig(&rdy);
        r.rec.record(ALICE, None, "write", Case::NotLoaded);
        r.rec.record(ALICE, None, "write", Case::NotLoaded);
        settle(&r);
        // a replacement whose outcome is unknown: the bytes are in place, or they are not
        *r.fault.lock().unwrap() = if landed {
            Fault::AfterReplace
        } else {
            Fault::LostReplace
        };
        r.rec.record(ALICE, None, "write", Case::NotLoaded);
        r.rec.record(BOB, None, "read", Case::Wrong);
        wait_for(&r.rx, Cycle::Uncertain);
        let between = if landed { 3 } else { 2 };
        assert_eq!(
            disk_rows(&rdy, t)[&key(t, ALICE, None, "write")].not_loaded,
            between,
            "landed={landed}"
        );
        assert!(
            !r.rec.settled(),
            "the batch is kept until the file is read back"
        );
        // a miss meanwhile, then the retry: read back first, never add the batch twice
        *r.fault.lock().unwrap() = Fault::None;
        r.rec.record(ALICE, None, "write", Case::NotLoaded);
        r.rec.nudge();
        settle(&r);
        let rows = disk_rows(&rdy, t);
        assert_eq!(
            rows[&key(t, ALICE, None, "write")],
            one(4, 0, 0),
            "landed={landed}"
        );
        assert_eq!(
            rows[&key(t, BOB, None, "read")],
            one(0, 0, 1),
            "landed={landed}"
        );
        assert_eq!(r.rec.summary().total, one(4, 0, 1), "landed={landed}");
    }
}

#[test]
fn kfu2_worker_flush_and_stop_are_bounded() {
    let t = today();
    let (_tmp, rdy) = project();
    // the worker stops inside a flush; the reply path goes on
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let gate_rx = Mutex::new(gate_rx);
    let reached_tx = Mutex::new(reached_tx);
    let (tx, rx) = mpsc::channel();
    let hooks = Hooks {
        clock: Some(Arc::new(|| NOON)),
        before_flush: Some(Arc::new(move || {
            let _ = reached_tx.lock().unwrap().send(());
            let _ = gate_rx.lock().unwrap().recv();
        })),
        after_cycle: Some(Arc::new(move |c| {
            let _ = tx.send(c);
        })),
        retry: Duration::from_secs(3600),
        ..Hooks::default()
    };
    let rec = Recorder::detached(&rdy, hooks).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Cycle::Loaded);
    rec.record(ALICE, None, "write", Case::NotLoaded);
    reached_rx
        .recv_timeout(WAIT)
        .expect("the worker is in its flush");
    // many replies and the end of the last handle, all while the disk work is stuck
    let (done_tx, done_rx) = mpsc::channel();
    let handle = rec.clone();
    std::thread::spawn(move || {
        for _ in 0..1000 {
            handle.record(ALICE, None, "write", Case::NotLoaded);
        }
        let _ = handle.summary();
        drop(handle);
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(WAIT)
        .expect("record, summary and drop never wait for the disk");
    assert_eq!(rec.summary().total, one(1001, 0, 0));
    drop(rec); // the last handle ends while the worker is still inside the flush
    // let the stuck flush go; the stop requests one more, and the worker ends
    gate_tx.send(()).unwrap();
    gate_tx.send(()).unwrap(); // and the final flush's gate
    let mut outcomes = Vec::new();
    loop {
        match rx.recv_timeout(WAIT) {
            Ok(c) => outcomes.push(c),
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => panic!("the worker did not end: {outcomes:?}"),
        }
    }
    assert_eq!(outcomes.last(), Some(&Cycle::Stopped));
    assert_eq!(outcomes.iter().filter(|c| **c == Cycle::Stopped).count(), 1);
    assert_eq!(
        disk_rows(&rdy, t)[&key(t, ALICE, None, "write")],
        one(1001, 0, 0),
        "the final flush wrote everything"
    );

    // a final flush that meets the lock gives up once and ends
    let (_tmp, rdy) = project();
    let r = rig(&rdy);
    r.rec.record(ALICE, None, "write", Case::Lost);
    settle(&r);
    let lock = File::open(rdy.join("run").join(LOCK_NAME)).unwrap();
    lock.lock().unwrap();
    let Rig { rec, rx, .. } = r;
    rec.record(ALICE, None, "write", Case::Lost);
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), Cycle::Contended);
    drop(rec);
    let mut tail = Vec::new();
    loop {
        match rx.recv_timeout(WAIT) {
            Ok(c) => tail.push(c),
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => panic!("no end: {tail:?}"),
        }
    }
    assert_eq!(
        tail,
        [Cycle::Contended, Cycle::Stopped],
        "one try, then the worker ends"
    );
    lock.unlock().unwrap();
    assert_eq!(
        disk_rows(&rdy, t)[&key(t, ALICE, None, "write")],
        one(0, 1, 0),
        "the lost count stays lost"
    );
}

#[test]
fn kfu2_idle_recorder_observes_another_recorders_flush() {
    let (_tmp, rdy) = project();
    let (tx_a, rx_a) = mpsc::channel();
    let a = Recorder::detached(
        &rdy,
        Hooks {
            clock: Some(Arc::new(|| NOON)),
            after_cycle: Some(Arc::new(move |c| {
                let _ = tx_a.send(c);
            })),
            poll: Duration::from_millis(20),
            retry: Duration::from_secs(3600),
            ..Hooks::default()
        },
    )
    .expect("recorder a");
    assert_eq!(rx_a.recv_timeout(WAIT).expect("load"), Cycle::Loaded);
    assert_eq!(a.summary().total, one(0, 0, 0));
    let b = Recorder::detached(
        &rdy,
        Hooks {
            clock: Some(Arc::new(|| NOON)),
            poll: Duration::from_secs(3600),
            retry: Duration::from_secs(3600),
            ..Hooks::default()
        },
    )
    .expect("recorder b");
    assert!(b.record(ALICE, None, "write", Case::Lost));
    let start = std::time::Instant::now();
    while !b.settled() {
        assert!(start.elapsed() < WAIT, "b never flushed");
        std::thread::sleep(Duration::from_millis(5));
    }
    // a records nothing; its own poll must pick the flush up, with summary() never reading disk
    let start = std::time::Instant::now();
    loop {
        if a.summary().total == one(0, 1, 0) {
            break;
        }
        assert!(start.elapsed() < WAIT, "a never saw b's flush");
        std::thread::sleep(Duration::from_millis(10));
    }
}
