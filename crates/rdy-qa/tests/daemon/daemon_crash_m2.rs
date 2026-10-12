//! sg6 (spec §15.2 crash tests): `kill -9` the daemon during a large include and during a landing
//! that rebases views. Each op is all or nothing: after the restart by the next client, fsck is
//! clean with no unlogged move, and the invariant checker passes.
use rdy_engine::{Rdy, Scope};
use rdy_model::Hash;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check_all;
use rdy_qa::mcp::{DEADLINE, McpClient};
use rdy_qa::tensions::ag;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The changes view `src` holds.
const SRC_CHANGES: usize = 2_000;
/// The unpinned, non-empty views the landing rebases.
const VIEWS: usize = 20;
/// A hang guard for one child command, not a speed check.
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

/// The narrowest kill window, in ms: the time an op takes varies from run to run, so the window
/// never closes to one point.
const MIN_WIDTH: u64 = 4;

/// A seeded kill delay in `[lo, hi]` ms, and the window narrowed by where it fell: a kill after the
/// answer lowers `hi`, a kill before the op reached the daemon raises `lo`.
struct Window {
    lo: u64,
    hi: u64,
    seed: u64,
}

impl Window {
    fn draw(&mut self) -> u64 {
        self.lo + lcg(&mut self.seed) % (self.hi - self.lo + 1)
    }
    fn narrow(&mut self, d: u64, hit: &Hit) {
        match hit {
            Hit::Answered => self.hi = d.max(self.lo + MIN_WIDTH),
            Hit::Early => self.lo = d.min(self.hi.saturating_sub(MIN_WIDTH)),
            Hit::InFlight => {}
        }
    }
}

/// `rdy --json fsck` through the daemon (the next client restarts it): clean, no unlogged move.
fn fsck_clean(s: &Served, what: &str) {
    let (ok, f) = s.json(&["--json", "fsck"]);
    assert!(ok, "{what}: fsck failed: {f}");
    assert_eq!(f["unlogged_moves"], 0, "{what}: {f}");
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
    let mut w = Window {
        lo: 0,
        hi: took.as_millis() as u64,
        seed: 0x7367_3661,
    };
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
/// claim) read on canon's head; the person's view `work` edits the seed claim and is proposed.
/// Seeded in-process, then the daemon is started.
fn rebasing_landing() -> (Served, Landing) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let views: Vec<String> = (0..VIEWS).map(|i| format!("v{i:02}")).collect();
    for v in &views {
        write_as(
            &mut r,
            &ag(v),
            v,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("{v} adds this.")}]),
        )
        .unwrap();
    }
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": claim, "body": "The person's edit."}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "work", "the person's edit").unwrap();
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

/// Waits for `child` with a hang guard; returns its success and its JSON (stdout or stderr).
fn finish(mut child: std::process::Child) -> (bool, Value) {
    let end = Instant::now() + CHILD_BUDGET;
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "rdy approve hung");
        std::thread::sleep(Duration::from_millis(5));
    }
    let out = child.wait_with_output().unwrap();
    let ok = out.status.success();
    let text = String::from_utf8_lossy(if ok { &out.stdout } else { &out.stderr }).into_owned();
    let v = serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
        .unwrap_or(Value::String(text));
    (ok, v)
}

/// `rdy --json approve <proposal>` as the person, with the daemon killed `delay` ms after the
/// command starts (`None`: not killed). Returns where the kill fell, the reply, and the time.
fn approve(s: &Served, l: &Landing, delay: Option<u64>) -> (Hit, Value, Duration) {
    let t = Instant::now();
    let child = s.spawn(&["--json", "approve", &l.proposal]);
    let Some(d) = delay else {
        let (ok, v) = finish(child);
        assert!(ok, "an approve with no kill failed: {v}");
        return (Hit::Answered, v, t.elapsed());
    };
    let mut child = child;
    std::thread::sleep(Duration::from_millis(d));
    if child.try_wait().unwrap().is_some() {
        let (ok, v) = finish(child);
        assert!(ok, "the approve failed before the kill: {v}");
        return (Hit::Answered, v, t.elapsed());
    }
    s.kill9();
    let (ok, v) = finish(child);
    let took = t.elapsed();
    if ok {
        // answered just before the kill, or run on a successor the command started after it
        let hit = if s.daemon_pid().is_some() {
            Hit::Early
        } else {
            Hit::Answered
        };
        return (hit, v, took);
    }
    match v["code"].as_str() {
        Some("E_DAEMON_GONE") => (Hit::InFlight, v, took),
        Some("E_DAEMON_START") => (Hit::Early, v, took),
        _ => panic!("the killed approve failed with another error: {v}"),
    }
}

/// After the kill: the landing is in the log and every view reads on the new head, or it is
/// absent and every view keeps the old base. Returns whether it landed.
fn all_or_nothing(s: &Served, l: &Landing, what: &str) -> bool {
    fsck_clean(s, what);
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
        landed
    })
}

#[test]
fn sg6_a_kill_during_a_landing_with_rebases_is_all_or_nothing() {
    // an approve with no kill: it lands and rebases every view, and it times the window
    let (s, l) = rebasing_landing();
    let (_, v, took) = approve(&s, &l, None);
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
    assert!(all_or_nothing(&s, &l, "no kill"));
    drop(s);
    let mut w = Window {
        lo: 0,
        hi: took.as_millis() as u64,
        seed: 0x6c61_6e64,
    };
    let (mut hits, mut attempt) = (0, 0);
    while hits < points() {
        assert!(
            attempt < points() * MAX_TRIES,
            "only {hits} of {} kills fell in flight in {attempt} attempts (window {}..{} ms)",
            points(),
            w.lo,
            w.hi
        );
        let (s, l) = rebasing_landing();
        let d = w.draw();
        let (hit, v, _) = approve(&s, &l, Some(d));
        let what =
            format!("attempt {attempt}, killed {d} ms after the approve started ({hit:?}: {v})");
        let landed = all_or_nothing(&s, &l, &what);
        if v.get("landing").is_some() {
            assert!(landed, "{what}: an acknowledged landing is lost");
        }
        if hit == Hit::InFlight {
            hits += 1;
        }
        eprintln!("sg6 landing: attempt {attempt}, killed at {d} ms: {hit:?}, landed: {landed}");
        w.narrow(d, &hit);
        attempt += 1;
    }
}
