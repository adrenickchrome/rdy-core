//! sg6 (spec §15.2 crash tests): `kill -9` the daemon during a large include and during a landing
//! that rebases views. Each op is all or nothing: after the restart by the next client, fsck is
//! clean with no unlogged move, and the invariant checker passes.
use rdy_daemon::ServeOptions;
use rdy_daemon::{Attempt, Client, Hello, PROTO_VERSION, Proto, RunDir, connect};
use rdy_engine::{LandingPoint, Rdy, Scope, on_landing_at_for_tests};
use rdy_model::Hash;
use rdy_qa::daemon::{Served, wait_endpoint_live};
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::invariants::check_all;
use rdy_qa::mcp::{DEADLINE, McpClient};
use rdy_qa::tensions::ag;
use serde_json::{Value, json};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The changes view `src` holds.
const SRC_CHANGES: usize = 2_000;
/// The unpinned, non-empty views the landing rebases.
const VIEWS: usize = 20;
/// How many of those views also edit the seed claim the person edits.
const COLLIDING: usize = 3;
/// A hang guard for one approve, not a speed check.
const CHILD_BUDGET: Duration = Duration::from_secs(120);

fn lcg(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed >> 33
}

/// The attempts allowed per kill point that must fall in flight: a bound on the loop, not a rate
/// the test asserts.
const MAX_TRIES: usize = 20;

/// How many kill points hit an op in flight.
fn points() -> usize {
    if deep() { 10 } else { 3 }
}

/// Where a kill fell, as the client saw it.
#[derive(Debug, PartialEq, Eq)]
enum Hit {
    /// The op was answered before the kill.
    Answered,
    /// The kill came before the client reached the daemon; it ran on the successor.
    Early,
    /// The client got `E_DAEMON_GONE` for the op: the kill fell while it was in flight.
    InFlight,
}

/// The narrowest kill window of the include, in ms: the time an op takes varies from run to run,
/// so the window never closes to one point.
const MIN_WIDTH: u64 = 4;

/// A seeded kill delay in `[lo, hi]` (in the window's unit), and the window narrowed by where it
/// fell: a kill after the answer lowers `hi`, a kill before the op reached the daemon raises `lo`.
/// Under load the time an op takes drifts from attempt to attempt, so a window narrowed on earlier
/// attempts can sit beside the op: a miss in a window already at its narrowest (`min`) resets it
/// to the timed window `[0, full]`.
struct Window {
    lo: u64,
    hi: u64,
    /// The timed window's upper end: the un-killed op's time.
    full: u64,
    /// The narrowest width.
    min: u64,
    /// The unit, for the log: `ms` or `µs`.
    unit: &'static str,
    seed: u64,
}

impl Window {
    fn new(full: u64, min: u64, unit: &'static str, seed: u64) -> Window {
        Window {
            lo: 0,
            hi: full,
            full,
            min,
            unit,
            seed,
        }
    }
    fn draw(&mut self) -> u64 {
        self.lo + lcg(&mut self.seed) % (self.hi - self.lo + 1)
    }
    fn narrow(&mut self, d: u64, hit: &Hit) {
        if *hit != Hit::InFlight && self.hi - self.lo <= self.min {
            let u = self.unit;
            eprintln!(
                "sg6 window {}..{} {u} missed at {d} {u} ({hit:?}): reset to 0..{} {u}",
                self.lo, self.hi, self.full
            );
            (self.lo, self.hi) = (0, self.full);
            return;
        }
        match hit {
            Hit::Answered => self.hi = d.max(self.lo + self.min),
            Hit::Early => self.lo = d.min(self.hi.saturating_sub(self.min)),
            Hit::InFlight => {}
        }
    }
}

/// `rdy --json fsck` through the daemon (the next client restarts it): clean, no unlogged move.
/// Returns the report.
fn fsck_clean(s: &Served, what: &str) -> Value {
    let (ok, f) = s.json(&["--json", "fsck"]);
    assert!(ok, "{what}: fsck failed: {f}");
    assert_eq!(f["unlogged_moves"], 0, "{what}: {f}");
    f
}

/// The `transitions` count of a clean `rdy --json fsck`.
fn transitions(s: &Served, what: &str) -> u64 {
    let f = fsck_clean(s, what);
    f["transitions"]
        .as_u64()
        .unwrap_or_else(|| panic!("{what}: {f}"))
}

/// Stops the daemon, opens the project in-process, runs `f` and `check_all`, and closes it.
fn in_process<T>(s: &Served, what: &str, f: impl FnOnce(&Rdy) -> T) -> T {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap_or_else(|e| panic!("{what}: open: {e}"));
    let out = f(&r);
    check_all(&r).unwrap_or_else(|v| panic!("{what}: {v:#?}"));
    r.close().unwrap();
    out
}

/// A served project whose canon holds the seed doc and whose view `src` holds `SRC_CHANGES`
/// changes, each one claim under the seed section; seeded in-process before any daemon runs.
fn large_src() -> (Served, Vec<String>) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let changes = (0..SRC_CHANGES)
        .map(|i| {
            write(
                &mut r,
                "src",
                json!([{"op": "create", "type": "claim", "parent": section, "body": format!("Claim {i}.")}]),
            )
            .unwrap()
            .change
        })
        .collect();
    r.close().unwrap();
    (s, changes)
}

/// The JSON-RPC request line of `rdy_view {op: "include", view, changes}`.
fn include_line(id: u64, view: &str, changes: &[String]) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "rdy_view",
        "arguments": {"op": "include", "view": view, "changes": changes}}})
    .to_string()
}

/// An agent session whose run (and so its own view) is `view`, over a proxy to the daemon.
fn owner_of(s: &Served, view: &str) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    let v = c
        .call(
            "rdy_session",
            json!({"actor": "agent:inc", "run": view, "role": "writer"}),
        )
        .ok();
    assert_eq!(v["view"], view, "{v}");
    c
}

/// The include of `changes` into `view` by its owner, with the daemon killed `delay` ms after the
/// request is written (`None`: not killed). Returns where the kill fell and how long the call took.
fn include(s: &Served, view: &str, changes: &[String], delay: Option<u64>) -> (Hit, Duration) {
    let mut c = owner_of(s, view);
    let id = c.next_id();
    let t = Instant::now();
    c.send_raw(&include_line(id, view, changes));
    if let Some(d) = delay {
        std::thread::sleep(Duration::from_millis(d));
        s.kill9();
    }
    let reply = c.recv(id, DEADLINE);
    let took = t.elapsed();
    if let Some(e) = reply.get("error") {
        assert_eq!(e["data"]["code"], "E_DAEMON_GONE", "{view}: {reply}");
        assert!(
            delay.is_some(),
            "{view}: an include with no kill failed: {reply}"
        );
        return (Hit::InFlight, took);
    }
    assert_ne!(
        reply["result"]["isError"],
        json!(true),
        "{view}: the include was refused: {reply}"
    );
    (Hit::Answered, took)
}

#[test]
fn sg6_a_kill_during_a_large_include_is_all_or_nothing() {
    let (s, changes) = large_src();
    // an include with no kill: it takes all the changes, and it times the window
    let (hit, took) = include(&s, "dst-cal", &changes, None);
    assert_eq!(hit, Hit::Answered);
    let n = in_process(&s, "dst-cal", |r| r.read().included("dst-cal").len());
    assert_eq!(n, SRC_CHANGES, "the include took every change");
    let mut w = Window::new(took.as_millis() as u64, MIN_WIDTH, "ms", 0x7367_3661);
    let (mut hits, mut attempt) = (0, 0);
    while hits < points() {
        assert!(
            attempt < points() * MAX_TRIES,
            "only {hits} of {} kills fell in flight in {attempt} attempts (window {}..{} ms)",
            points(),
            w.lo,
            w.hi
        );
        let view = format!("dst-{attempt}");
        let d = w.draw();
        let (hit, _) = include(&s, &view, &changes, Some(d));
        let what = format!("{view}, killed {d} ms after the request ({hit:?})");
        fsck_clean(&s, &what);
        let n = in_process(&s, &what, |r| r.read().included(&view).len());
        assert!(
            n == 0 || n == SRC_CHANGES,
            "{what}: the view holds {n} of {SRC_CHANGES} changes"
        );
        if hit == Hit::Answered {
            assert_eq!(n, SRC_CHANGES, "{what}: an answered include is lost");
        }
        if hit == Hit::InFlight {
            hits += 1;
        }
        eprintln!("sg6 include: {what}: {n} changes");
        w.narrow(d, &hit);
        attempt += 1;
    }
}

/// What the landing test seeds: the person's proposal, canon's head, and the views it rebases.
struct Landing {
    proposal: String,
    old_head: Hash,
    views: Vec<String>,
}

/// A served project: canon holds the seed doc; `VIEWS` agent views (unpinned, each with one
/// claim; the first `COLLIDING` also edit the seed claim) read on canon's head; the person's
/// view `work` edits the seed claim and is proposed. Seeded in-process, then the daemon is
/// started.
fn rebasing_landing() -> (Served, Landing) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let views: Vec<String> = (0..VIEWS).map(|i| format!("v{i:02}")).collect();
    for (i, v) in views.iter().enumerate() {
        let mut ops = vec![
            json!({"op": "create", "type": "claim", "parent": section, "body": format!("{v} adds this.")}),
        ];
        // these views also edit the seed claim: each edit collides with the person's, so the
        // landing moves the open tensions and logs a TRANSITION record in its batch
        if i < COLLIDING {
            ops.push(json!({"op": "update", "id": claim, "body": format!("{v}'s edit.")}));
        }
        write_as(&mut r, &ag(v), v, Value::Array(ops)).unwrap();
    }
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": claim, "body": "The person's edit."}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "work", "the person's edit", Some(TEST_SUMMARY))
        .unwrap();
    let st = r.read();
    let old_head = st.canon_head().expect("a canon head");
    for v in &views {
        let h = st.view_head(v).expect("a view head");
        assert!(!h.pinned && h.base == Some(old_head), "{v}: {h:?}");
    }
    drop(st);
    r.close().unwrap();
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    (
        s,
        Landing {
            proposal: p.r#ref,
            old_head,
            views,
        },
    )
}

/// The person's ctl connection to the served daemon, its `session` op answered.
async fn person(s: &Served) -> Client {
    let hello = Hello {
        rdy: PROTO_VERSION,
        version: env!("CARGO_PKG_VERSION").into(),
        build: rdy_daemon::BUILD.to_string(),
        proto: Proto::Ctl,
        cwd: s.dir.path().into(),
        project: RunDir::of_project(s.dir.path())
            .project_id()
            .expect("the project's .rdy"),
    };
    let mut c = match connect(&RunDir::of_project(s.dir.path()), &hello).await {
        Attempt::Ready(c) => c,
        // a daemon that just bound is still building its state: wait for its `ready`
        Attempt::Starting(p) => p
            .wait(Instant::now() + CHILD_WAIT, &mut |_| {})
            .await
            .unwrap_or_else(|e| panic!("the daemon did not become ready: {e}")),
        Attempt::Exiting => panic!(
            "the daemon is of another build than this test: rebuild rdy with cargo build -p \
             rdy-cli, in the profile this test runs in"
        ),
        other => panic!("no daemon: {other:?}"),
    };
    c.ctl("session", json!({"actor": null, "run": null}))
        .await
        .expect("the person's session");
    c
}

/// The child daemon's project root; unset in a normal test run ([`sg6_landing_child`]).
const CHILD_PROJECT: &str = "RDY_SG6_CHILD_PROJECT";
/// The landing point the child daemon stops at: `appended` or `sealed`.
const CHILD_POINT: &str = "RDY_SG6_CHILD_POINT";
/// The file the child daemon creates when a landing reaches its point.
const CHILD_MARKER: &str = "RDY_SG6_CHILD_MARKER";
/// A hang guard for the child daemon to start and for its landing to reach its point.
const CHILD_WAIT: Duration = Duration::from_secs(120);

/// The name of each landing point in [`CHILD_POINT`].
fn point_name(p: LandingPoint) -> &'static str {
    match p {
        LandingPoint::Appended => "appended",
        LandingPoint::Sealed => "sealed",
    }
}

/// Not a test on its own: the daemon [`approve_killed_at`] starts, the real daemon of this build
/// (`rdy_daemon::start`) in a process of its own. Its landing hook stops every landing at the
/// point [`CHILD_POINT`] names: it creates the marker file and blocks on stdin, so the landing
/// stays there until the parent kills this process. A parent that dies first closes the pipe,
/// and the child exits there too, its landing unfinished.
#[test]
fn sg6_landing_child() {
    let (Some(project), Some(point), Some(marker)) = (
        std::env::var_os(CHILD_PROJECT),
        std::env::var(CHILD_POINT).ok(),
        std::env::var_os(CHILD_MARKER),
    ) else {
        return;
    };
    let point = [LandingPoint::Appended, LandingPoint::Sealed]
        .into_iter()
        .find(|p| point_name(*p) == point)
        .unwrap_or_else(|| panic!("{CHILD_POINT}={point:?} names no landing point"));
    let marker = std::path::PathBuf::from(marker);
    on_landing_at_for_tests(move |p| {
        if p != point {
            return;
        }
        std::fs::write(&marker, b"").expect("the marker file");
        // the parent kills this process here; EOF means the parent is gone
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
        std::process::exit(3);
    });
    let run = rdy_daemon::start(ServeOptions {
        project: project.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        detach: false,
        idle_timeout: Some(CHILD_WAIT),
        human_actor: Some("human:qa".into()),
        site_lan_override: None,
    })
    .unwrap_or_else(|e| panic!("sg6 child: start: {e}"));
    let code = match run.wait() {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("sg6 child: {e}");
            1
        }
    };
    // do not return into the test harness: the child's work is the daemon, now done
    std::process::exit(code);
}

/// The person's `approve` of the landing, over a connection that is open and has its session
/// before the request. Returns the reply.
fn approve(s: &Served, l: &Landing) -> Value {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut c = rt.block_on(person(s));
    let args = json!({"proposal": l.proposal});
    rt.block_on(async { tokio::time::timeout(CHILD_BUDGET, c.ctl("approve", args)).await })
        .expect("the approve hung")
        .unwrap_or_else(|e| panic!("an approve with no kill failed: {e:?}"))
}

/// Kills and reaps the child daemon on every way out of [`approve_killed_at`]: a failed attempt
/// leaves no daemon behind.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The person's `approve` of the landing, served by a child daemon of this build
/// ([`sg6_landing_child`]) that stops the landing at `point`; the child is killed with
/// `kill -9` once the landing is there. Returns where the kill fell and the error the client
/// got.
///
/// The kill waits for the landing to reach a known point, not for a drawn delay: a landing takes
/// a few milliseconds, and on a slow or loaded machine a delay drawn in that window fell before
/// or after the landing on most attempts.
fn approve_killed_at(s: &Served, l: &Landing, point: LandingPoint) -> (Hit, Value) {
    // the served daemon `rebasing_landing` started gives the project to the child
    let first = s.daemon_pid().expect("the served daemon");
    s.stop();
    s.wait_killed(first);
    let run = s.run_dir();
    run.ensure().expect("the run dir");
    let marker = s.dir.path().join("sg6-landing-point");
    let mut cmd = Command::new(std::env::current_exe().expect("the test binary"));
    cmd.args([
        "daemon_crash_m2::sg6_landing_child",
        "--exact",
        "--nocapture",
    ])
    .current_dir(s.dir.path());
    let home = empty_home();
    isolate_person(&mut cmd, home, &home.join(".config"));
    if let Some((k, v)) = coverage_env() {
        cmd.env(k, v);
    }
    let log = rdy_qa::platform::open_private_log(&run.log_file()).expect("the daemon log");
    cmd.env(CHILD_PROJECT, s.dir.path())
        .env(CHILD_POINT, point_name(point))
        .env(CHILD_MARKER, &marker)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone().expect("the daemon log")))
        .stderr(Stdio::from(log));
    let mut child = KillOnDrop(cmd.spawn().expect("start the child daemon"));
    wait_endpoint_live(&run);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut c = rt.block_on(person(s));
    assert_eq!(
        s.daemon_pid(),
        Some(child.0.id()),
        "the child daemon serves the project"
    );
    let args = json!({"proposal": l.proposal});
    let call = rt.spawn(async move { c.ctl("approve", args).await });
    let t0 = Instant::now();
    while !marker.exists() {
        assert!(
            !call.is_finished(),
            "the approve was answered before its landing reached {point:?}"
        );
        assert!(
            t0.elapsed() < CHILD_WAIT,
            "the landing never reached {point:?} in {} s",
            CHILD_WAIT.as_secs()
        );
        // test-hygiene: allow wait the poll interval of a wait on the marker file, under CHILD_WAIT
        std::thread::sleep(Duration::from_millis(5));
    }
    rdy_qa::platform::kill_hard(child.0.id());
    child.0.wait().expect("reap the killed child daemon");
    let reply = rt
        .block_on(async { tokio::time::timeout(CHILD_BUDGET, call).await })
        .expect("the approve hung")
        .expect("the approve task");
    match reply {
        Ok(v) => panic!("an approve whose daemon was killed in the landing was answered: {v}"),
        Err(e) => {
            let v = serde_json::to_value(&e).unwrap();
            (killed_hit(&v), v)
        }
    }
}

/// Where the kill fell, from the error of a killed approve. A client sends a `session` op before
/// `approve`, and a kill during either one gives `E_DAEMON_GONE`; only the one that names
/// `approve` fell while the landing was in flight.
fn killed_hit(v: &Value) -> Hit {
    let message = v["message"].as_str().unwrap_or("");
    match v["code"].as_str() {
        Some("E_DAEMON_GONE") if message.contains("during approve") => Hit::InFlight,
        Some("E_DAEMON_GONE") if message.contains("closed the connection during ") => Hit::Early,
        Some("E_DAEMON_START") => Hit::Early,
        _ => panic!("the killed approve failed with another error: {v}"),
    }
}

#[test]
fn sg6_a_narrowed_window_that_misses_resets_to_the_timed_window() {
    let mut w = Window::new(20, MIN_WIDTH, "ms", 1);
    w.narrow(12, &Hit::Early);
    w.narrow(16, &Hit::Answered);
    assert_eq!((w.lo, w.hi), (12, 16), "narrowed to MIN_WIDTH");
    // the op drifted later under load: a kill before it reached the daemon resets the window
    w.narrow(15, &Hit::Early);
    assert_eq!((w.lo, w.hi), (0, 20));
    w.narrow(4, &Hit::Answered);
    w.narrow(5, &Hit::InFlight);
    assert_eq!((w.lo, w.hi), (0, 4), "an in-flight kill keeps the window");
    w.narrow(3, &Hit::Answered);
    assert_eq!((w.lo, w.hi), (0, 20));
}

#[test]
fn sg6_only_a_kill_during_the_approve_op_counts_as_in_flight() {
    // the CLI sends `session` before `approve`: a kill during `session` is before the landing
    let gone = |op: &str| {
        json!({"code": "E_DAEMON_GONE",
            "message": format!("the daemon closed the connection during {op}")})
    };
    assert_eq!(killed_hit(&gone("approve")), Hit::InFlight);
    assert_eq!(killed_hit(&gone("session")), Hit::Early);
    assert_eq!(
        killed_hit(&json!({"code": "E_DAEMON_START", "message": "x"})),
        Hit::Early
    );
}

/// After the kill: the landing is in the log and every view reads on the new head, or it is
/// absent and every view keeps the old base. Returns whether it landed and fsck's count of
/// TRANSITION records.
fn all_or_nothing(s: &Served, l: &Landing, what: &str) -> (bool, u64) {
    let moves = transitions(s, what);
    let (ok, log) = s.json(&["--json", "log"]);
    assert!(ok, "{what}: {log}");
    let in_log = log
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["approved"] == l.proposal.as_str());
    in_process(s, what, |r| {
        let st = r.read();
        let head = st.canon_head().unwrap();
        let landed = head != l.old_head;
        assert_eq!(
            in_log, landed,
            "{what}: the log and canon's head disagree about the landing"
        );
        let want = if landed { head } else { l.old_head };
        for v in &l.views {
            let h = st.view_head(v).unwrap();
            assert_eq!(
                h.base,
                Some(want),
                "{what}: view {v} (landed: {landed}) reads on the wrong base"
            );
        }
        // the person's edit is in canon exactly when the landing is
        let read = st.materialized(&Scope::Canon).unwrap();
        let edited = read.iter().any(|b| b.body == "The person's edit.");
        assert_eq!(edited, landed, "{what}: canon's content and head disagree");
        (landed, moves)
    })
}

#[test]
fn sg6_a_kill_during_a_landing_with_rebases_is_all_or_nothing() {
    // an approve with no kill: it lands and rebases every view
    let (s, l) = rebasing_landing();
    let before = transitions(&s, "before the approve");
    let v = approve(&s, &l);
    let after = transitions(&s, "after the approve");
    assert!(
        after > before,
        "the landing logs no TRANSITION record ({before} before, {after} after), so the fold \
         check after a kill checks nothing"
    );
    let rebased: Vec<&str> = v["rebased"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["view"].as_str().unwrap())
        .collect();
    for view in &l.views {
        assert!(
            rebased.contains(&view.as_str()),
            "the landing rebases {view}: {v}"
        );
    }
    assert_eq!(all_or_nothing(&s, &l, "no kill"), (true, after));
    drop(s);
    // each kill falls at a known point of the landing: with its frames in the log before the
    // seal (nothing may survive), or sealed before the answer (all of it must)
    for attempt in 0..points() {
        let point = if attempt % 2 == 0 {
            LandingPoint::Appended
        } else {
            LandingPoint::Sealed
        };
        let (s, l) = rebasing_landing();
        let (hit, v) = approve_killed_at(&s, &l, point);
        let what = format!("attempt {attempt}, killed at {point:?} ({hit:?}: {v})");
        assert_eq!(hit, Hit::InFlight, "{what}");
        let (landed, moves) = all_or_nothing(&s, &l, &what);
        assert_eq!(landed, point == LandingPoint::Sealed, "{what}");
        // the landing's TRANSITION record is logged with it, in its batch, or not at all
        let want = if landed { after } else { before };
        assert_eq!(
            moves, want,
            "{what}: {moves} TRANSITION records (landed: {landed}; {before} before the landing, \
             {after} after)"
        );
        eprintln!(
            "sg6 landing: attempt {attempt}, killed at {point:?}: landed: {landed}, \
             transitions: {moves}"
        );
    }
}

/// Final review M6 (minors items 13 and 14): the wait after a `kill -9` ends when the killed
/// process is gone, even when a client has already started its successor and the socket answers
/// again. A wait for "no daemon answers" would run out its budget and panic here.
#[test]
fn sg6_the_kill_wait_ends_when_a_successor_already_answers() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    let first = s.daemon_pid().expect("a daemon");
    rdy_qa::platform::kill_hard(first);
    // the next client starts a successor before the wait begins
    let (ok, v) = s.json(&["--json", "status"]);
    assert!(ok, "{v}");
    let second = s.daemon_pid().expect("a successor");
    assert_ne!(first, second);
    s.wait_killed(first);
    assert_eq!(s.daemon_pid(), Some(second), "the successor still runs");
}
