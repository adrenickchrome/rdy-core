//! The deep gate's memory soak: one served daemon, on a project of about 20,000 blocks, takes a
//! run of landings, two per cycle. Each comes from a view of its own, and every second one from a
//! view on a base one landing behind canon (the scope the old engine copied whole, and kept once
//! the view closed), so a scope that keeps its own copy of the project shows as growth. After each landing the harness
//! reads the daemon's `RssAnon` and the memory report of `rdy status --json` (the allocator's
//! live bytes and the bytes the capped caches hold). `rdy_qa::soak::check` then applies the
//! 800 MiB limit and the relative growth rule after landing 5. Deep gate only; the readings are
//! Linux ones, so other platforms skip with a line that says so.
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

/// Writes one new doc `name` in view `name`, as the human.
fn write_doc(s: &Served, name: &str) {
    ok(
        s,
        &[
            "--json", "new", "doc", name, "--title", name, "--view", name,
        ],
    );
}

/// Proposes view `name` and approves it, as the human.
fn land_view(s: &Served, name: &str) {
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
    let pref = p["ref"].as_str().expect("a proposal ref").to_string();
    let o = s.rdy(&["approve", &pref]);
    assert!(
        o.status.success(),
        "approve {pref}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// One soak cycle, two landings: view `late-<n>` is written first, view `early-<n>` lands, then
/// `late-<n>` lands on a base one landing behind canon. A view on an older base is the scope the
/// old engine copied whole, and its closed build was then kept.
fn cycle(s: &Served, n: u64) {
    let (late, early) = (format!("late-{n}"), format!("early-{n}"));
    write_doc(s, &late);
    write_doc(s, &early);
    land_view(s, &early);
    land_view(s, &late);
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
    let mut samples = vec![];
    for n in 1..=landings() / 2 {
        cycle(&s, n);
        let v = ok(&s, &["--json", "status"]);
        let mem = &v["memory"];
        let Some(live) = mem["live_bytes"].as_u64() else {
            not_here("the daemon reports no live bytes (glibc's allocator counters only)");
            s.stop();
            return;
        };
        let cache = mem["capped_cache_bytes"]
            .as_u64()
            .unwrap_or_else(|| panic!("status has no capped_cache_bytes: {v}"));
        assert_eq!(s.daemon_pid(), Some(pid), "the same daemon served the soak");
        let rss_anon = platform::rss_anon_bytes(pid).expect("RssAnon of the daemon");
        let sample = Sample {
            landing: 2 * n,
            rss_anon,
            live,
            cache,
        };
        eprintln!(
            "soak landing {}: RssAnon {rss_anon} live {live} cache {cache} net {}",
            sample.landing,
            sample.net()
        );
        samples.push(sample);
    }
    s.stop();
    if let Err(e) = check(&samples) {
        panic!("memory soak: {e}");
    }
}
