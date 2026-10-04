# Performance record (spec 11)

Measured 2026-10-03 on an Apple M4 Pro (macOS, Darwin 25.6, APFS), release build, 100,000 blocks
(claims in docs of 10 sections x 100 claims, written in batches by `testkit::generate`).
Reproduce: `cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture`
and `RDY_BENCH_BLOCKS=100000 cargo bench -p rdy-engine`.

Source column: **perf_budget** = `tests/perf_budget.rs` (final run, perf-run4.log); **criterion** = `benches/core.rs`.

| measurement | target | before fixes | final (100k) | source | status |
|---|---|---|---|---|---|
| cold start (open + materialize + graph) | < 2 s | 2.02 s (first pass, right after generation) | 0.63 s median of 3 (1.40 s, 0.63 s, 0.63 s) | perf_budget | met |
| cold open + graph | - | - | 791 ms (10k blocks: 101 ms) | criterion | informational |
| query p99 (`context` + `impact`, 200 probes) | < 5 ms | not measured (`context` scanned every edge) | 3.5 us (p50 2.2 us, p95 3.1 us, max 7.1 us) | perf_budget | met |
| `context` / `impact` mean | - | - | 1.55 us / 373 ns (10k: 1.40 us / 324 ns) | criterion | informational |
| write p99 (one-claim commit, 200 writes) | < 5 ms | grows with view length, 18 ms avg at 8k blocks | 23.3 ms p99 (p50 10.6 ms, p95 11.5 ms, max 24.3 ms) | perf_budget | **missed on macOS** |
| write mean | - | - | 11.7 ms (10k: 11.0 ms) | criterion | informational |

## Fixes made

1. `perf(store)`: `commit_to_view` replayed every change in the view (reading and decoding each
   one from the log) to validate a new write. `write` now checks against the engine's cached view
   state (`Store::commit_to_view_at`). Write time was growing linearly with view length (11 ms at
   2k blocks, 18 ms at 8k); it is now flat (9.3 ms at 2k, 9.5 ms at 8k).
2. `perf(engine)`: `context` computed suspects for the whole scope and filtered to one block. It now
   asks the graph for that block's own edges (`Graph::suspects_of`); `context` is about 1.5 us at 100k.

## Write p99: missed; the floor is fsync on macOS

**The 5 ms write p99 target is unattainable on macOS with ack-after-`F_FULLFSYNC`, even with one
fsync per write**: one 4.1 ms fsync plus about 2 ms of other work is about 6 ms mean. The target is
unchanged. **A Linux/NVMe run is required before spec 11 can be considered met**; it has not been done.

Profile of a single-claim write at 100k blocks (macOS `sample`, 2019 samples in `Rdy::write`):

- 1688 samples (84%) in `__fcntl`, which is `F_FULLFSYNC` (Rust's `File::sync_all` on macOS). A write
  does two: the segment log `sync` (822) and the redb index commit (890).
- the remainder is about 2 ms: BLAKE3 hashing and postcard encoding of the view record, `pwrite`,
  a few comparisons.

`F_FULLFSYNC` costs 4.1 ms here (measured with Python on a fresh file), so two are about 8.3 ms
before any rdy code. Plain `fsync` is 0.03 ms on this machine because it does not flush the drive
cache, but it would not meet the durability constraint.

Latency distribution of the 200 writes in the final run: p50 10.6 ms, p95 11.5 ms, p99 23.3 ms,
max 24.3 ms. The body is tight (p50 to p95 is under 1 ms), so the p99 is a few outliers at
about 2x the median: fsync variance (two fsyncs, each occasionally slow), not rdy work. The growing
view record is visible but small: mean of the first 100 writes 10.5 ms, last 100 writes 11.0 ms,
about 0.5 ms over 200 writes.

## What would help (M2 work, deferred, not done here)

- **One fsync per write instead of two.** Commit the redb index at non-immediate durability and
  catch the index up from the log on open (the log is the source of truth). This changes the
  store's crash story and needs a design decision. Even so, one 4.1 ms fsync plus about 2 ms is
  about 6 ms on macOS, so it does not by itself meet 5 ms there.
- **A chained view record.** The view record lists every change hash in the view, so each write
  re-encodes, hashes and appends O(changes) bytes (about 350 KB at 11k changes). Disk growth is
  quadratic in the number of changes: the 100k project is 1.6 GB. A chained or incremental record
  (with the incremental graph overlays) removes it; it is the only size-dependent write cost left.

## Cold start

0.6-0.7 s when the OS is idle (final run: 1.40 s, 0.63 s, 0.63 s). The first pass right after generating the project, while the OS is
still flushing 1.6 GB, took 1.9-2.0 s (the first run of this test saw 2.02 s). The test therefore
judges the median of three opens and prints all three. Profile top frames of a cold start: `pread`
(reading each block frame), `memcmp` and `redb` point lookups (`Index::object` per block),
BLAKE3 hash of every block read, `Graph::build`, `BTreeMap::insert`.
