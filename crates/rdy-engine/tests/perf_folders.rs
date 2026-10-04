//! Addendum B performance: paths and the tree at 1k folders / 10k docs / ~100k blocks.
//! Run: cargo test -p rdy-engine --release --test perf_folders -- --ignored --nocapture
use rdy_engine::*;
use std::time::{Duration, Instant};

fn p99(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[(v.len() * 99 / 100).min(v.len() - 1)]
}

#[test]
#[ignore]
fn paths_and_tree_at_1k_folders() {
    let dir = tempfile::tempdir().unwrap();
    let t = Instant::now();
    let paths = testkit::generate_tree(dir.path(), 1000, 10, 8);
    println!("generated {} docs in {:?}", paths.len(), t.elapsed());
    let mut r = Rdy::open(dir.path()).unwrap();
    let work = Scope::View("work".into());
    r.materialized(&work).unwrap();
    let mut tree = vec![];
    for _ in 0..50 {
        let t = Instant::now();
        r.tree(&work, None, None).unwrap();
        tree.push(t.elapsed());
    }
    let mut resolve = vec![];
    for p in paths.iter().step_by(paths.len() / 200) {
        let t = Instant::now();
        r.resolve_path(&work, p).unwrap();
        resolve.push(t.elapsed());
    }
    let (tree_p99, resolve_p99) = (p99(tree), p99(resolve));
    println!("tree (ls/overview) p99 {tree_p99:?} (target < 5 ms)");
    println!("resolve_path p99 {resolve_p99:?} (target < 5 ms)");
    let mut misses = vec![];
    if tree_p99 >= Duration::from_millis(5) {
        misses.push(format!("tree p99 {tree_p99:?}"));
    }
    if resolve_p99 >= Duration::from_millis(5) {
        misses.push(format!("resolve_path p99 {resolve_p99:?}"));
    }
    assert!(misses.is_empty(), "missed: {misses:?}");
}
