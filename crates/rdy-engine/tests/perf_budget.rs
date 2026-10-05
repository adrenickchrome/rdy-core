//! Run with: cargo test -p rdy-engine --release --test perf_budget -- --ignored
//! Uses 100k blocks (spec 11). Ignored by default because generating the project takes a while.
use rdy_engine::{Rdy, Scope, Session, WriteRequest, testkit};
use rdy_model::Role;
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

/// p99 of 50 durable flushes of a 4 KB write on this platform (a `write_all_at` plus `sync_data`
/// on a temp file; `sync_data` is `F_FULLFSYNC` on macOS). The write target is stated against it.
fn flush_p99() -> Duration {
    let mut f = tempfile::tempfile().unwrap();
    f.write_all(&[0u8; 4096]).unwrap();
    f.sync_all().unwrap();
    let buf = [0xA5u8; 4096];
    let mut runs = vec![];
    for _ in 0..50 {
        let t = Instant::now();
        f.write_all_at(&buf, 0).unwrap();
        f.sync_data().unwrap();
        runs.push(t.elapsed());
    }
    p99(runs)
}

/// The write p99 target per hardware class: one durable flush plus 1 ms on macOS, where
/// `F_FULLFSYNC` is the floor; 5 ms elsewhere (spec 11).
fn write_target(flush_p99: Duration) -> Duration {
    if cfg!(target_os = "macos") {
        flush_p99 + Duration::from_millis(1)
    } else {
        Duration::from_millis(5)
    }
}

fn pct(v: &[Duration], p: usize) -> Duration {
    let mut v = v.to_vec();
    v.sort();
    v[(v.len() * p / 100).min(v.len() - 1)]
}

fn p99(v: Vec<Duration>) -> Duration {
    pct(&v, 99)
}

fn dist(v: &[Duration]) -> String {
    format!(
        "p50 {:?} p95 {:?} p99 {:?} max {:?}",
        pct(v, 50),
        pct(v, 95),
        pct(v, 99),
        v.iter().max().unwrap()
    )
}

#[test]
#[ignore]
fn spec_11_targets_at_100k_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let ids = testkit::generate(dir.path(), 100_000);

    // Cold start: open the project and build the graph. Run three times and judge the median, so one
    // pass that overlaps the OS flushing the freshly generated store cannot decide the result.
    let work = Scope::View("work".into());
    let mut colds = vec![];
    let mut r = None;
    for _ in 0..3 {
        drop(r.take());
        let t = Instant::now();
        let mut open = Rdy::open(dir.path()).unwrap();
        open.graph(&work).unwrap();
        colds.push(t.elapsed());
        r = Some(open);
    }
    let mut r = r.unwrap();
    let mut sorted = colds.clone();
    sorted.sort();
    let cold = sorted[1];

    let s = Session {
        actor: "agent:perf".into(),
        run: None,
        role: Role::Writer,
    };
    let parent = r.block(&work, &ids[0]).unwrap().parent.unwrap().to_string();
    let mut writes = vec![];
    for _ in 0..200 {
        let t = Instant::now();
        r.write(
            &s,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(
                    json!([{"op": "create", "type": "claim", "parent": parent, "body": "perf"}]),
                )
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
        writes.push(t.elapsed());
    }
    let write_p99 = p99(writes.clone());
    let flush = flush_p99();
    let write_target = write_target(flush);
    println!(
        "writes: {}; first 100 mean {:?}, last 100 mean {:?}",
        dist(&writes),
        writes[..100].iter().sum::<Duration>() / 100,
        writes[100..].iter().sum::<Duration>() / 100
    );

    r.graph(&work).unwrap();
    let mut queries = vec![];
    for id in ids.iter().step_by(500) {
        let t = Instant::now();
        r.context(&work, id).unwrap();
        r.impact(&work, id, 3).unwrap();
        queries.push(t.elapsed() / 2);
    }
    let query_p99 = p99(queries.clone());
    println!("queries: {}", dist(&queries));

    // The generator never lands, so land the work view once and version the head; then time the
    // version position (it walks landings back to the highest version at or before its base).
    let approver = Session {
        actor: "human:perf".into(),
        run: None,
        role: Role::Approver,
    };
    let p = r.propose(&s, "work", "seed").unwrap();
    r.land(&approver, &p.id).unwrap();
    r.tag(&approver, "v0.1.0", None, None).unwrap();
    let mut positions = vec![];
    for _ in 0..200 {
        let t = Instant::now();
        r.version_position(&Scope::Canon).unwrap();
        positions.push(t.elapsed());
    }
    let position_p99 = p99(positions.clone());
    println!("version_position: {}", dist(&positions));

    // Measure everything first so one run reports every number, then check each target.
    println!("cold start (median of 3) {cold:?} {colds:?} (target < 2 s)");
    println!("write p50/p95/p99/max: {}", dist(&writes));
    println!("flush p99 {flush:?}");
    println!("write target {write_target:?}");
    println!("write p99 {write_p99:?} (target < {write_target:?})");
    println!("query p99 {query_p99:?} (target < 5 ms)");
    println!("version_position p99 {position_p99:?} (target < 5 ms)");
    let mut misses = vec![];
    if cold >= Duration::from_secs(2) {
        misses.push(format!("cold start {cold:?} (target < 2 s)"));
    }
    if write_p99 >= write_target {
        misses.push(format!(
            "write p99 {write_p99:?} (target < {write_target:?}; flush p99 {flush:?})"
        ));
    }
    if query_p99 >= Duration::from_millis(5) {
        misses.push(format!("query p99 {query_p99:?} (target < 5 ms)"));
    }
    if position_p99 >= Duration::from_millis(5) {
        misses.push(format!(
            "version_position p99 {position_p99:?} (target < 5 ms)"
        ));
    }
    assert!(
        misses.is_empty(),
        "spec 11 targets missed: {}",
        misses.join("; ")
    );
}
