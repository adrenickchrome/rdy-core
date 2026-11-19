//! The deep gate's memory soak: one served daemon takes a run of landings, and after each one the
//! harness reads the daemon's `RssAnon` and the memory report of `rdy status --json` (the
//! allocator's live bytes and the bytes the capped caches hold). `rdy_qa::soak::check` then
//! applies the 800 MiB limit and the no-growth rule after landing 5. Deep gate only; the
//! readings are Linux ones, so other platforms skip with a line that says so.
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::deep;
use rdy_qa::platform;
use rdy_qa::skip::{Why, skip, skip_line};
use rdy_qa::soak::{Sample, check};
use std::io::Write;

const NAME: &str = "soak1_daemon_memory_stays_bounded_over_landings";
/// The landings of one soak; `RDY_QA_SOAK_LANDINGS` sets another count.
const LANDINGS: u64 = 40;

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

/// Lands one new doc through the human's CLI: write, propose, approve.
fn land(s: &Served, n: u64) {
    let (name, title) = (format!("soak-{n}"), format!("Soak {n}"));
    ok(s, &["--json", "new", "doc", &name, "--title", &title]);
    let p = ok(
        s,
        &[
            "--json",
            "propose",
            "--title",
            &title,
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
    let s = Served::new();
    ok(&s, &["--json", "status"]);
    let pid = s.daemon_pid().expect("a served daemon");
    if platform::rss_anon_bytes(pid).is_none() {
        not_here("RssAnon is read from /proc/<pid>/status, on Linux only");
        s.stop();
        return;
    }
    let mut samples = vec![];
    for n in 1..=landings() {
        land(&s, n);
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
            landing: n,
            rss_anon,
            live,
            cache,
        };
        eprintln!(
            "soak landing {n}: RssAnon {rss_anon} live {live} cache {cache} net {}",
            sample.net()
        );
        samples.push(sample);
    }
    s.stop();
    if let Err(e) = check(&samples) {
        panic!("memory soak: {e}");
    }
}
