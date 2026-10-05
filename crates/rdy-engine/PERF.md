# Performance record (spec 11)

Measured 2026-10-03, release build, Rust 1.99.0, 100,000 blocks (claims in docs of 10 sections x
100 claims, written in batches by `testkit::generate`), on two configurations:

| config | CPU / RAM | OS | storage | durable flush cost (4 KB write + flush) |
|---|---|---|---|---|
| **macOS** | Apple M4 Pro | macOS 26 (Darwin 25.6) | internal SSD, APFS | `F_FULLFSYNC` 4.1 ms |
| **Linux** | x86_64, 8 cores, 30 GB | Ubuntu 24.04.5 LTS, kernel 6.17 | consumer NVMe SSD (no power-loss protection), ext4 | `fdatasync` p50 2.5 ms, p99 2.8 ms, max 5.9 ms |

The macOS numbers below are the detailed record (profiles, fixes, criterion); the Linux results have
their own section, "Linux results".
Reproduce: `cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture`
and `RDY_BENCH_BLOCKS=100000 cargo bench -p rdy-engine`.

Source column: **perf_budget** = `tests/perf_budget.rs` (cold-start row: the pre-QA run); **criterion** = `benches/core.rs`.

| measurement | target | before fixes | final (100k) | source | status |
|---|---|---|---|---|---|
| cold start (open + materialize + graph) | < 2 s | 2.02 s (first pass, right after generation) | 0.65 s median of 3 (2.06 s, 0.65 s, 0.64 s) | perf_budget | met |
| cold open + graph | - | - | 791 ms (10k blocks: 101 ms) | criterion | informational |
| query p99 (`context` + `impact`, 200 probes) | < 5 ms | not measured (`context` scanned every edge) | 3.5 us (p50 2.2 us, p95 3.1 us, max 7.1 us) | perf_budget | met |
| `context` / `impact` mean | - | - | 1.55 us / 373 ns (10k: 1.40 us / 324 ns) | criterion | informational |
| write p99 (one-claim commit, 200 writes) | < 5 ms | grows with view length, 18 ms avg at 8k blocks | 23.3 ms p99 (p50 10.6 ms, p95 11.5 ms, max 24.3 ms) | perf_budget | **missed** (macOS; Linux also missed, see below). Superseded by "M2.0: one flush per write" below |
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
unchanged. The Linux run (section "Linux results") also misses p99, by about 2x, on consumer NVMe.

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

## Linux results

Configuration: see the **Linux** row of the configuration table at the top. Same commit, toolchain
and test as macOS (`cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture`).
The full workspace suite also passes on this configuration (166 passed, 1 ignored).

| run | write p50 | write p95 | write p99 | write max | query p99 | cold start (median of 3) |
|---|---|---|---|---|---|---|
| 1 (first run after build) | 4.71 ms | 5.53 ms | 29.79 ms | 326.3 ms | 2.66 us | 0.48 s (0.71, 0.47, 0.48 s) |
| 2 | 4.64 ms | 4.78 ms | 9.51 ms | 30.1 ms | 2.78 us | 0.48 s (0.49, 0.48, 0.48 s) |
| 3 | 5.10 ms | 5.31 ms | 10.86 ms | 31.0 ms | 2.74 us | 0.47 s (0.49, 0.47, 0.47 s) |

Targets: cold start < 2 s **met**; query p99 < 5 ms **met**; write p99 < 5 ms **missed** (about 10 ms
on steady runs). Query p50 is about 1.65 us.

Reading:

- The disk sets the floor. A write does two durable flushes (segment log, then the redb index
  commit); at about 2.5 ms per `fdatasync` on this drive that is about 5 ms before any rdy work, which
  is why the write median sits right at the target.
- The tail (p99 about 2x the median, occasional ~30 ms maxima) matches occasional slow flushes from
  ext4 journal commits and redb page allocation landing on a write, not rdy CPU time. The 326 ms
  maximum appeared once, on the first run after the build, and did not recur.
- The view-record growth is not visible at this scale on Linux (first-100 vs last-100 write means:
  5.2 vs 8.1 ms on run 1, 4.9 vs 4.8 ms and 5.5 vs 4.9 ms on runs 2 and 3).
- With one flush per write (below), the floor on this configuration drops to about 2.5 ms, which
  should bring p99 near or under 5 ms. Drives with power-loss protection flush in well under 0.1 ms,
  where the target is comfortably reachable.

## What would help (M2 work, deferred, not done here)

- **One fsync per write instead of two.** *Superseded: done in M2.0, see "M2.0: one flush per write" below.* Commit the redb index at non-immediate durability and
  catch the index up from the log on open (the log is the source of truth). This changes the
  store's crash story and needs a design decision. On the Linux configuration it halves the flush
  floor (about 5 ms to about 2.5 ms) and is the change most likely to meet the target there. On macOS
  one 4.1 ms fsync plus about 2 ms is still about 6 ms, so it does not by itself meet 5 ms there.
- **Consider restating the target per hardware class** if p99 still misses on consumer NVMe after
  the one-flush change, e.g. "p99 < 5 ms on NVMe with power-loss protection; < 10 ms on consumer
  NVMe".
- **A chained view record.** The view record lists every change hash in the view, so each write
  re-encodes, hashes and appends O(changes) bytes (about 350 KB at 11k changes). Disk growth is
  quadratic in the number of changes: the 100k project is 1.6 GB. A chained or incremental record
  (with the incremental graph overlays) removes it; it is the only size-dependent write cost left.

## Cold start

0.6-0.7 s when the OS is idle (pre-QA run: 2.06 s, 0.65 s, 0.64 s; median 0.65 s). The first pass (2.06 s) exceeds the 2 s target: it is taken while the OS is still
flushing the ~1.6 GB just generated (2.02 s was seen on the first pass before any M1 fixes). The test therefore
judges the median of three opens and prints all three. Open now verifies the index's page checksums (redb `check_integrity`) and rebuilds a damaged index instead of trusting it; with that check the median is 0.65 s. The check was measured at ~31 ms on a warm 34 MB / 300k-object index; it is not yet measured on a cold page cache (QA follow-up). Profile top frames of a cold start: `pread`
(reading each block frame), `memcmp` and `redb` point lookups (`Index::object` per block),
BLAKE3 hash of every block read, `Graph::build`, `BTreeMap::insert`.

## Folders and paths (addendum B)

Measured 2026-10-04, release build, Rust 1.99.0, on the **macOS** configuration of the table at the
top (Apple M4 Pro, macOS 26.6, internal SSD, APFS).

Shape (`testkit::generate_tree(1000, 10, 8)`): 1,000 folders (10 top level, 990 spread evenly under
them), 10,000 docs (10 per folder), each with one `Constraints` section of 8 claims, in view `work`:
about 101,000 blocks (1,000 folders + 10,000 docs + 10,000 sections + 80,000 claims). Generation is
one write per doc and took 134 s.
Reproduce: `cargo test -p rdy-engine --release --test perf_folders -- --ignored --nocapture` and
`cargo test -p rdy-render --release --test render site_size_at_1k_folders -- --ignored --nocapture`.

| measurement | target | result | status |
|---|---|---|---|
| `tree` p99 (whole tree, no root, no depth limit; 50 calls) | < 5 ms | 9.77 ms | **missed** |
| `resolve_path` p99 (200 doc paths, 2 or 3 segments deep) | < 5 ms | 5.1 us | met |
| site render (full, `render_site`) | < 2 s | 4.18 s | **missed** |
| site size | - | 11,001 pages, 121,760,413 bytes (about 116 MiB) | informational |

- `tree` misses 5 ms: a full, unlimited-depth walk of 1,000 folders and 10,000 docs takes about 10 ms
  at p99. `rdy_overview` collapses levels under a token budget (Task 9), so the tool a client
  normally calls returns far fewer nodes than this measurement; the budgeted call was not measured
  here. No code was changed to chase the number.
- The full site render takes 4.18 s against the 2 s target, writing 11,001 pages (one per doc, one
  per folder, plus the index) and about 116 MiB. No code was changed to chase the number.
- `resolve_path` is a map lookup and is well under target.

## M2.0: one flush per write (2026-10-04)

Measured 2026-10-04, release build, Rust 1.99.0, 100,000 blocks, on the **macOS** configuration of
the table at the top (Apple M4 Pro, macOS 26.6, internal SSD, APFS). A write is now one marked batch
in the segment log with one durable flush; the index follows by checkpoint and is caught up from the
log on open. This section supersedes the write row of the table at the top and the "One fsync per
write instead of two" bullet under "What would help".

Restated target (spec 11, per hardware class): write p99 < 5 ms on Linux NVMe; on macOS, one
durable flush + < 1 ms, where the flush is measured in the test (p99 of 50 runs of a 4 KB
`write_all_at` plus `sync_data`, which is `F_FULLFSYNC` on macOS).
Reproduce: `cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture`.

| measurement (perf_budget, 200 one-claim writes) | result |
|---|---|
| write p50 | 6.25 ms |
| write p95 | 7.45 ms |
| write p99 | 10.90 ms |
| write max | 21.44 ms |
| flush p99 (4 KB `write_all_at` + `sync_data`, 50 runs) | 5.34 ms |
| write target (flush p99 + 1 ms) | 6.34 ms |

Verdict: **missed** on macOS. Write p99 is 10.90 ms against the 6.34 ms target. The median is
within the target (p50 6.25 ms, about 0.9 ms over the flush p99), and the body is tight (p50 to p95
is 1.2 ms); the miss is in the tail, where occasional writes cost about two flushes. Writes went
from about 10.6 ms p50 and 23.3 ms p99 (two flushes) to 6.25 ms and 10.90 ms. The assertion in
`perf_budget` is kept as specified, and no product code was changed for the miss. The other
targets in the same run: cold start (median of 3) 0.71 s, query p99 4.4 us, `version_position` p99
4.4 us, all met. The remaining non-flush write cost is the O(view length) view record, which M2a1
removes.

`qa-perf` (`cargo qa --only perf`), medians in ms, before = the baseline recorded before this
change:

| op | before | after | change |
|---|---|---|---|
| `write_single` | 9.400 | 5.097 | -46% |
| `write_batch_200` | 15.850 | 8.227 | -48% |
| `propose_land_500_flat` | 129.974 | 124.220 | -4% |
| `cold_open` | 85.969 | 87.475 | +2% |
| `cold_open_lagging` | - (new) | 91.219 | - |

`cold_open_lagging` restores an index checkpointed before the timed writes and the landings, then
times `Rdy::open` plus the first read of canon: it caught up 4,497 frames from the log in that
time, against the < 2 s target at 100k (M2 spec 15.1, index catch-up included). The project is the
perf tier's ~10k-block workload, not 100k blocks.
