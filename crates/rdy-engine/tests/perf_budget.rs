//! Run with: cargo test -p rdy-engine --release --test perf_budget -- --ignored
//! Uses 100k blocks (spec 11). Ignored by default because generating the project takes a while.
use rdy_engine::{Rdy, Scope, Session, WriteRequest, testkit};
use rdy_model::{Block, BlockId, Op, Role};
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The ignored tests measure time, so they take turns: run together in one process they would
/// disturb each other's flushes.
static SERIAL: Mutex<()> = Mutex::new(());

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
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
        let open = Rdy::open(dir.path()).unwrap();
        open.read().graph(&work).unwrap();
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
        kind: rdy_engine::SessionKind::Human,
    };
    let parent = r
        .read()
        .block(&work, &ids[0])
        .unwrap()
        .parent
        .unwrap()
        .to_string();
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

    r.read().graph(&work).unwrap();
    let mut queries = vec![];
    for id in ids.iter().step_by(500) {
        let t = Instant::now();
        r.read().context(&work, id).unwrap();
        r.read().impact(&work, id, 3).unwrap();
        queries.push(t.elapsed() / 2);
    }
    let query_p99 = p99(queries.clone());
    println!("queries: {}", dist(&queries));

    // View materialization. Cold: the state is dropped (`invalidate`) and the read rebuilds the
    // view; three runs, judged on the median. Incremental: one write, then the work a commit's
    // publish does to seed the next state, `with` of one Create, on the built view.
    let mut colds_view = vec![];
    for _ in 0..3 {
        r.invalidate();
        let t = Instant::now();
        r.read().materialized(&work).unwrap();
        colds_view.push(t.elapsed());
    }
    colds_view.sort();
    let materialize_cold = colds_view[1];
    r.write(
        &s,
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(
                json!([{"op": "create", "type": "claim", "parent": parent, "body": "after invalidate"}]),
            )
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let t = Instant::now();
    let built = r.read().materialized(&work).unwrap();
    let after_write = t.elapsed();
    let section = BlockId::parse(&parent).unwrap();
    let op = [Op::Create {
        block: Block {
            id: BlockId::new("c"),
            block_type: "claim".into(),
            parent: Some(section),
            order: "V".into(),
            fields: Default::default(),
            body: "incremental".into(),
            links: Default::default(),
            pins: Default::default(),
        },
    }];
    let mut incremental = vec![];
    for _ in 0..200 {
        let t = Instant::now();
        let next = built.with(&op);
        incremental.push(t.elapsed());
        assert_eq!(next.len(), built.len() + 1);
    }
    let incremental_p99 = p99(incremental.clone());
    println!("view materialize cold (median of 3) {materialize_cold:?} {colds_view:?}");
    println!("view materialize after one write (seeded) {after_write:?}");
    println!("view materialize incremental: {}", dist(&incremental));

    // The generator never lands, so land the work view once and version the head; then time the
    // version position (it walks landings back to the highest version at or before its base).
    let approver = Session {
        actor: "human:perf".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    };
    let p = r.propose(&s, "work", "seed", None).unwrap();
    r.land(&approver, &p.id).unwrap();
    r.tag(&approver, "v0.1.0", None, None).unwrap();
    let mut positions = vec![];
    for _ in 0..200 {
        let t = Instant::now();
        r.read().version_position(&Scope::Canon).unwrap();
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
    println!("view materialize cold {materialize_cold:?} (budget < 2 s)");
    println!("view materialize incremental p99 {incremental_p99:?} (target < 1 ms)");
    println!("version_position p99 {position_p99:?} (target < 5 ms)");
    let mut misses = vec![];
    if cold >= Duration::from_secs(2) {
        misses.push(format!("cold start {cold:?} (target < 2 s)"));
    }
    if materialize_cold >= Duration::from_secs(2) {
        misses.push(format!(
            "view materialize cold {materialize_cold:?} (budget < 2 s, inside the cold-start target)"
        ));
    }
    if incremental_p99 >= Duration::from_millis(1) {
        misses.push(format!(
            "view materialize incremental p99 {incremental_p99:?} (target < 1 ms)"
        ));
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

/// chain1: the view record is a chain of ops, so the log bytes one write appends do not depend on
/// how long the view is. M1's view record was rewritten whole on every write and grew quadratically
/// (1.6 GB at 100k blocks). Prints the mean bytes per write over writes 900-1,000 and over writes
/// 9,900-10,000, and the time to propose and land the 10,000-change view.
/// Run with: cargo test -p rdy-engine --release --test perf_budget chain1 -- --ignored --nocapture
#[test]
#[ignore]
fn chain1_log_bytes_per_write_stay_flat_to_10k() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const WRITES: usize = 10_000;
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let s = Session {
        actor: "agent:perf".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    let write = |r: &mut Rdy, ops: serde_json::Value| {
        r.write(
            &s,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap()
    };

    // One doc with 100 sections, so no parent holds more than 100 claims. This setup write is not
    // one of the 10,000 measured writes.
    let mut setup = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Doc", "doc_type": "constraints"}}),
    ];
    for sec in 0..100 {
        setup.push(json!({"op": "create", "type": "section", "id": format!("$s{sec}"), "parent": "$d", "fields": {"title": format!("Part {sec}")}}));
    }
    let res = write(&mut r, json!(setup));
    let parents: Vec<String> = (0..100)
        .map(|sec| res.created[&format!("$s{sec}")].clone())
        .collect();

    // Log bytes appended by each write, taken from the end of the log. A write that rolled into a
    // new segment has no byte count and is skipped (the segment size is far above 10,000 writes).
    let mut bytes: Vec<Option<u64>> = vec![];
    let mut times = vec![];
    for i in 0..WRITES {
        let before = r.store().log_end();
        let t = Instant::now();
        write(
            &mut r,
            json!([{"op": "create", "type": "claim", "parent": parents[i % 100], "body": format!("claim {i}")}]),
        );
        times.push(t.elapsed());
        let after = r.store().log_end();
        bytes.push((after.segment == before.segment).then(|| after.offset - before.offset));
    }
    let mean = |lo: usize, hi: usize| -> f64 {
        let v: Vec<u64> = bytes[lo..hi].iter().flatten().copied().collect();
        assert!(!v.is_empty(), "no same-segment writes in {lo}..{hi}");
        v.iter().sum::<u64>() as f64 / v.len() as f64
    };
    let early = mean(900, 1_000);
    let late = mean(9_900, 10_000);
    println!(
        "log bytes per write: writes 900-1,000 mean {early:.1}; writes 9,900-10,000 mean {late:.1}"
    );
    println!(
        "write time: first 100 mean {:?}, last 100 mean {:?}; {}",
        times[..100].iter().sum::<Duration>() / 100,
        times[WRITES - 100..].iter().sum::<Duration>() / 100,
        dist(&times)
    );

    let approver = Session {
        actor: "human:perf".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    };
    let t = Instant::now();
    let p = r.propose(&s, "work", "chain1", None).unwrap();
    let propose = t.elapsed();
    let t = Instant::now();
    r.land(&approver, &p.id).unwrap();
    let land = t.elapsed();
    println!("propose of {WRITES}+1 changes {propose:?}; land {land:?}");

    assert!(
        late <= early * 1.1,
        "log bytes per write grew: {early:.1} at writes 900-1,000, {late:.1} at 9,900-10,000"
    );
}

/// m2c: landing with 50 non-empty unpinned views present, at 100k blocks. Fifty views of 20
/// changes each (the first five edit the claims the landings edit, so each landing records five
/// conflicts); five landings of one view each, timed over propose and land. The target is
/// "< 100 ms p99" (M2 spec 15.1) at the typical tier; the 100k figure is recorded, not gated, so
/// the assertion is 5x that. The perf tier's `land_rebase_50` is the judged measure.
/// Run with: cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture m2c_
#[test]
#[ignore]
fn m2c_landing_with_50_views_at_100k() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    const VIEWS: usize = 50;
    const CHANGES: usize = 20;
    const TOUCHING: usize = 5;
    const LANDINGS: usize = 5;
    let dir = tempfile::tempdir().unwrap();
    let ids = testkit::generate(dir.path(), 100_000);
    let mut r = Rdy::open(dir.path()).unwrap();
    let writer = Session::human("human:perf".into(), Role::Writer);
    let approver = Session::human("human:perf".into(), Role::Approver);
    let put = |r: &mut Rdy, view: &str, ops: serde_json::Value| {
        r.write(
            &writer,
            WriteRequest {
                view: view.into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    };
    // the generator never lands: land `work`, so every view below reads on the 100k canon
    let p = r.propose(&writer, "work", "seed", None).unwrap();
    r.land(&approver, &p.id).unwrap();

    let claims: Vec<String> = ids[..LANDINGS].iter().map(|c| c.to_string()).collect();
    let t = Instant::now();
    for k in 0..VIEWS {
        let view = format!("v{k}");
        let mut first = vec![];
        if k < TOUCHING {
            for c in &claims {
                first.push(json!({"op": "update", "id": c, "body": format!("Mine {k} {c}.")}));
            }
            put(&mut r, &view, json!(first));
        }
        for d in 0..(CHANGES - usize::from(k < TOUCHING)) {
            put(
                &mut r,
                &view,
                json!([{"op": "create", "type": "doc", "fields": {"name": format!("v-{k}-{d}"), "title": format!("V {k} {d}"), "doc_type": "constraints"}}]),
            );
        }
    }
    println!(
        "{VIEWS} views of {CHANGES} changes written in {:?}",
        t.elapsed()
    );

    let mut lands = vec![];
    for (i, claim) in claims.iter().enumerate() {
        let view = format!("land{i}");
        put(
            &mut r,
            &view,
            json!([{"op": "update", "id": claim, "body": format!("Landed {i}.")}]),
        );
        let t = Instant::now();
        let p = r.propose(&writer, &view, "m2c", None).unwrap();
        r.land(&approver, &p.id).unwrap();
        lands.push(t.elapsed());
    }
    let land_p99 = p99(lands.clone());
    println!(
        "landing with {VIEWS} views (propose + land, {LANDINGS} landings): {}",
        dist(&lands)
    );
    println!(
        "landing p99 {land_p99:?} (target < 100 ms at 10k; recorded at 100k; asserted < 500 ms)"
    );
    assert!(
        !r.read()
            .conflicts(&Scope::View("v0".into()))
            .unwrap()
            .is_empty(),
        "the touching views carry the landings' conflicts"
    );
    assert!(
        land_p99 < Duration::from_millis(500),
        "landing with {VIEWS} views at 100k: p99 {land_p99:?} (5x the 100 ms target)"
    );
}
