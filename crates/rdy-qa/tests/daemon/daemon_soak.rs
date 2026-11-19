//! The deep gate's memory soak: one served daemon, on a project of about 20,000 blocks, takes a
//! run of landings in the shape of agent traffic. Four views stay open at once: each cycle
//! proposes a new one, lands the oldest, and reads every other open view and its proposal
//! version. Those views sit on bases behind canon: the scopes the old engine copied whole. The
//! daemon is read once before any landing (landing 0) and after each cycle: its `RssAnon` and the
//! memory report of `rdy status --json` (the allocator's live bytes and the bytes the capped caches
//! hold). `rdy_qa::soak::check` then applies the 800 MiB limit and the growth rule. Deep gate only;
//! the readings are Linux ones, so other platforms skip with a line that says so.
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::deep;
use rdy_qa::platform;
use rdy_qa::skip::{Why, skip, skip_line};
use rdy_qa::soak::{Sample, check};
use std::io::Write;

const NAME: &str = "soak1_daemon_memory_stays_bounded_over_landings";
/// The landings of one soak; `RDY_QA_SOAK_LANDINGS` sets another count.
const LANDINGS: u64 = 40;
/// The seeded project: 20 folders of 10 docs, each with 97 claims (about 20,000 blocks), landed
/// on canon before the daemon starts.
const FOLDERS: usize = 20;
const DOCS_PER_FOLDER: usize = 10;
const CLAIMS_PER_DOC: usize = 97;

fn seed(project: &std::path::Path) {
    rdy_engine::testkit::generate_tree(project, FOLDERS, DOCS_PER_FOLDER, CLAIMS_PER_DOC);
    let mut r = rdy_engine::Rdy::open(project).expect("open the seeded project");
    rdy_qa::fixtures::land_with_a_tie(&mut r, "work");
}

fn landings() -> u64 {
    std::env::var("RDY_QA_SOAK_LANDINGS")
        .ok()
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("RDY_QA_SOAK_LANDINGS: {v:?} is not a count"))
        })
        .unwrap_or(LANDINGS)
}

/// A skip that never fails the run: the platform has no such reading, nothing is missing.
fn not_here(reason: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{}", skip_line(NAME, reason));
}

fn ok(s: &Served, args: &[&str]) -> serde_json::Value {
    let (ok, v) = s.json(args);
    assert!(ok, "rdy {args:?}: {v}");
    v
}

/// The views kept open at once: each cycle proposes one more and lands the oldest, so every
/// open view sits on a base behind canon.
const OPEN_VIEWS: usize = 4;

/// Writes one new doc in view `name` and proposes the view, as the human; returns the version.
fn propose_view(s: &Served, name: &str) -> String {
    ok(
        s,
        &[
            "--json", "new", "doc", name, "--title", name, "--view", name,
        ],
    );
    let p = ok(
        s,
        &[
            "--json",
            "propose",
            "--title",
            name,
            "--view",
            name,
            "--summary",
            "Adds one doc.",
        ],
    );
    p["ref"].as_str().expect("a proposal ref").to_string()
}

/// Approves proposal version `pref`, as the human.
fn approve(s: &Served, pref: &str) {
    let o = s.rdy(&["approve", pref]);
    assert!(
        o.status.success(),
        "approve {pref}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// An agent's reads of an open view and its proposal version: the scopes the old engine copied
/// whole once the view's base fell behind canon.
fn read_open(s: &Served, view: &str, pref: &str) {
    ok(s, &["--json", "suspects", "--scope", view]);
    ok(s, &["--json", "plan", "--view", view]);
    ok(s, &["--json", "diff", "canon", &format!("proposal:{pref}")]);
}

/// One soak cycle: propose a new view; past [`OPEN_VIEWS`] open, land the oldest (one landing);
/// then read every other open view and its proposal. Returns whether the cycle landed.
fn cycle(s: &Served, open: &mut Vec<(String, String)>, n: u64) -> bool {
    let view = format!("soak-{n}");
    let pref = propose_view(s, &view);
    open.push((view, pref));
    let landed = open.len() > OPEN_VIEWS;
    if landed {
        let (_, oldest) = open.remove(0);
        approve(s, &oldest);
    }
    for (view, pref) in &open[..open.len() - 1] {
        read_open(s, view, pref);
    }
    landed
}

/// One reading of the daemon after `landing` landings: `RssAnon` and the status memory report.
/// `None` when the daemon reports no live bytes (glibc's counters only).
fn sample(s: &Served, pid: u32, landing: u64) -> Option<Sample> {
    let v = ok(s, &["--json", "status"]);
    let mem = &v["memory"];
    let live = mem["live_bytes"].as_u64()?;
    let cache = mem["capped_cache_bytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("status has no capped_cache_bytes: {v}"));
    assert_eq!(s.daemon_pid(), Some(pid), "the same daemon served the soak");
    let rss_anon = platform::rss_anon_bytes(pid).expect("RssAnon of the daemon");
    let sample = Sample {
        landing,
        rss_anon,
        live,
        cache,
    };
    eprintln!(
        "soak landing {landing}: RssAnon {rss_anon} live {live} cache {cache} net {}",
        sample.net()
    );
    Some(sample)
}

#[test]
fn soak1_daemon_memory_stays_bounded_over_landings() {
    if !deep() {
        skip(
            NAME,
            Why::DeepOnly,
            "runs in a deep gate only (RDY_QA_DEEP=1)",
        );
        return;
    }
    let s = Served::seeded(seed);
    // the served status carries the daemon's memory report on every platform
    let first = ok(&s, &["--json", "status"]);
    assert!(
        first["memory"]["capped_cache_bytes"].is_u64(),
        "the served status has no memory report: {first}"
    );
    assert_eq!(
        first["memory"]["live_bytes"].is_u64(),
        cfg!(all(target_os = "linux", target_env = "gnu")),
        "{first}"
    );
    let pid = s.daemon_pid().expect("a served daemon");
    if platform::rss_anon_bytes(pid).is_none() {
        not_here("RssAnon is read from /proc/<pid>/status, on Linux only");
        s.stop();
        return;
    }
    // landing 0: canon built and read, no landing yet
    ok(&s, &["--json", "suspects"]);
    let Some(zero) = sample(&s, pid, 0) else {
        not_here("the daemon reports no live bytes (glibc's allocator counters only)");
        s.stop();
        return;
    };
    let mut samples = vec![zero];
    let (mut open, mut landed, mut n) = (vec![], 0, 0);
    while landed < landings() {
        n += 1;
        if cycle(&s, &mut open, n) {
            landed += 1;
        }
        samples.extend(sample(&s, pid, landed));
    }
    s.stop();
    if let Err(e) = check(&samples) {
        panic!("memory soak: {e}");
    }
}
