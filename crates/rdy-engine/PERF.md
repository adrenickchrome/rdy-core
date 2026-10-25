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
  should bring p99 near or under 5 ms. *Measured in "M2.0: one flush per write", subsection
  "Linux": the median met it, p99 did not.* Drives with power-loss protection flush in well under 0.1 ms,
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

### Linux

Measured 2026-10-04 on the **Linux** configuration of the table at the top (x86_64, 8 cores,
30 GB, Ubuntu 24.04.5 LTS, kernel 6.17, consumer NVMe SSD without power-loss protection, ext4),
release build, Rust 1.99.0, same commit and test as the macOS run above. The target on this class
is write p99 < 5 ms. Three consecutive runs of
`cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture`:

| run | write p50 | write p95 | write p99 | write max | flush p99 (4 KB `write_all_at` + `sync_data`, 50 runs) |
|---|---|---|---|---|---|
| 1 (first run after build) | 1.23 ms | 1.29 ms | 8.73 ms | 27.37 ms | 0.31 ms |
| 2 | 3.56 ms | 3.65 ms | 29.76 ms | 403.38 ms | 1.41 ms |
| 3 | 3.20 ms | 3.35 ms | 5.75 ms | 29.14 ms | 1.29 ms |

Verdict: **missed** on Linux in all three runs (write p99 8.73, 29.76 and 5.75 ms against
< 5 ms). The body meets it with room: p50 1.2-3.6 ms and p95 1.3-3.7 ms, with p50 to p95 under
0.2 ms in every run. The miss is in the tail: in each run between 2 and 9 of the 200 writes took
over 5 ms (p99 is the second-slowest write, p95 the tenth-slowest). Against the two-flush runs in
"Linux results" (p50 4.6-5.1 ms, p99 9.5-29.8 ms), the median dropped by between a quarter and
three quarters. The steady-run p99 dropped from about 10 ms to 5.8-8.7 ms. The
large maxima (27-403 ms, the 403 ms once, in run 2) recur on this drive and fit the occasional slow
flushes described there; they were not profiled in this run. The drive's own flush cost moved between runs
(flush p99 0.31 ms in run 1, 1.3-1.4 ms in runs 2 and 3), and the write median moved with it. The
assertion in `perf_budget` is kept, and no product code was changed for the miss. The other
targets were met in every run: cold start (median of 3) 0.46-0.47 s, query p99 2.3-8.0 us,
`version_position` p99 3.2-4.1 us.

`qa-perf` (`cargo qa --only perf --rebaseline`) recorded this class's first baseline in
`crates/rdy-qa/qa-baseline.json`. Medians in ms: `write_single` 2.628, `write_batch_200` 4.691,
`propose_land_500_flat` 89.492, `cold_open` 46.818, `cold_open_lagging` 58.126 (caught up 4,497
frames), `render` 1056.201, `export_import` 257.593.

## M2a1: store format

Measured 2026-10-05, release build, Rust 1.99.0, on the **macOS** class: macOS 26.6.2 / Apple M4 Pro /
APFS. Linux was not re-measured: its rows above and its `qa-baseline.json` class still hold the
M2.0 numbers. What changed for the write path: a view's record is now a chain of `VIEW_OP` frames
(one `Create` op on a view's first write, one `Commit` op per write) in place of one view record
rewritten whole on every write; a landing is one atomic batch; propose and land merge.
Reproduce: `cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture`
(the two ignored tests take turns, so they do not disturb each other's flushes) and
`cargo run -q -p rdy-qa --release --bin qa-perf` (or `cargo qa --only perf`).

### Log bytes per write (`chain1_log_bytes_per_write_stay_flat_to_10k`)

10,000 single-claim writes into one view through `Rdy::write` (a setup write that creates one doc
and 100 sections comes first; claims go round-robin into the sections). Bytes are the growth of the
log end per write.

| measurement | result |
|---|---|
| mean bytes per write, writes 900-1,000 | 480.0 |
| mean bytes per write, writes 9,900-10,000 | 486.0 |
| ratio (asserted <= 1.1) | 1.0125 |
| write time, first 100 / last 100 (mean) | 4.4 ms / 5.4 ms (second run: 4.9 ms / 8.8 ms) |
| `propose` of the 10,001-change view | 85.0 ms (second run: 161.9 ms) |
| `land` of that proposal | 121.3 ms (second run: 132.0 ms) |

M1's view record re-encoded the whole included set on every write: about 350 KB per write at 11k
changes, 1.6 GB for the 100k project. A write now appends about 480 bytes at any length (6 bytes more
at 10k, which was not investigated). The chained record removes the one size-dependent write cost
that "What would help" named. The runs differ in their timings (a third run, which printed slow
writes, saw eight consecutive writes over 20 ms around write 2,700); the byte counts were the same
in every run.

### `perf_budget` at 100k blocks (`spec_11_targets_at_100k_blocks`, 200 one-claim writes)

Three consecutive runs, each alone in its process. Target (M2.0's restated one, macOS): write p99 <
one durable flush p99 + 1 ms.

| run | write p50 | write p95 | write p99 | write max | flush p99 | write target |
|---|---|---|---|---|---|---|
| 1 | 5.01 ms | 6.19 ms | 17.08 ms | 46.45 ms | 5.27 ms | 6.27 ms |
| 2 | 4.59 ms | 5.92 ms | 17.38 ms | 45.70 ms | 4.49 ms | 5.49 ms |
| 3 | 4.87 ms | 6.28 ms | 17.23 ms | 41.50 ms | 81.49 ms (one slow flush in 50) | 82.49 ms |

Verdict: **missed** on macOS: write p99 17.1-17.4 ms against 5.5-6.3 ms (run 3's target is not
meaningful: one 4 KB flush in its 50 took tens of ms; against the other runs' 5.3 ms it also
misses). The body improved on M2.0 (p50 6.25 ms, p95 7.45 ms): p50 is 4.6-5.0 ms and p95 6.2 ms,
close to the flush p99 (4.5-5.3 ms here). The p99 got worse than M2.0's 10.90 ms, and the cause is
two writes in the 200, not the body. Two extra runs that printed every write over 9 ms found
exactly two: write 0 (19.0 and 19.7 ms) and write 199 (40.9 and 44.7 ms). In a 200-sample, p99 is
the second-slowest write.

- **Write 0** is the first write after the test has queried the project. It is not the view's
  first write (the generator has already made about 11k writes into view `work`), it writes no
  `Create` op, and it is not a chain cost. The test builds the 100k-block graph (`graph(&work)`)
  before the loop. `update_cache` (`crates/rdy-engine/src/lib.rs`) invalidates a view's cached graph
  on every write, and write 0 is the write that drops it: with timing printed inside `Rdy::write`
  (temporary, removed), write 0 spent 4.7-5.2 ms in `commit_to_view_at` (the same as writes 1-5)
  and 13-18 ms in `update_cache`, all of it in dropping the `Graph` (18.3 ms in the run that timed
  the drop alone; `apply` of the ops took 22-124 us). Writes 1-5 found no graph to drop and took
  4.1-4.9 ms. The cost follows the block count, not the chain: write 0 after open then `graph`
  took 3.9 ms at 1k blocks, 7.3 ms at 10k and 20-25 ms at 100k, while the store's part of the write
  stayed at about 4-6 ms. The invalidation is M1 code (`fc24bbb`), unchanged by M2a1, so M2.0 paid
  it too wherever a graph was cached before a write. This is a block-count-dependent cost that
  stays, not an intended one (see the concern in the Task 13 report).
- **Write 199** is a checkpoint. A write checkpoints the index once `CHECKPOINT_FRAMES` (1,000)
  frames are pending. A write now carries more frames than in M2.0 (the extra `VIEW_OP` frame), so
  the first checkpoint after the project is generated falls inside the 200 writes where M2.0's
  fell after them. In the 10,000-write run, writes over 20 ms recur every 400 writes (writes 2,778,
  3,178, 3,578): the same checkpoint, amortised to well under 0.2 ms per write. The checkpoint is
  the M2.0 design (index follows by checkpoint) plus the intended extra frame; the write 0 cost
  above is not intended: it is the inline drop of the cached graph, which grows with the block count.

The assertion in `perf_budget` is kept as specified, and no product code was changed for the miss.
Other targets, same three runs: cold start (median of 3) 0.69, 0.71 and 0.69 s; query p99 3.8, 7.1
and 5.2 us; `version_position` p99 1.96, 1.38 and 1.29 us (M2.0: 4.4 us; the cache of Task 11, one
walk only when canon's head, the view's head or the tag count changes); all met.

### `qa-perf` against the baseline

`cargo qa --only perf` passed: every operation is within the gate's budget against the baseline for
`macOS 26.6.2 / Apple M4 Pro / APFS` (the M2.0 baseline), so the gate was not rebaselined. Medians
in ms:

| op | baseline | M2a1 | change |
|---|---|---|---|
| `write_single` | 4.250 | 4.321 | +2% |
| `write_batch_200` | 7.039 | 8.064 | +15% |
| `propose_land_500_flat` | 117.674 | 122.313 | +4% |
| `context` | 40.504 | 40.477 | 0% |
| `impact` | 40.474 | 40.485 | 0% |
| `version_position` | 3.582 | 0.001 | cache hit (p95 4.9 ms is the first, uncached call) |
| `resolve_path` | 34.479 | 34.026 | -1% |
| `tree_full` | 1.379 | 1.238 | -10% |
| `render` | 1261.995 | 1220.097 | -3% |
| `export_import` | 562.855 | 547.997 | -3% |
| `cold_open` | 84.975 | 86.811 | +2% |
| `cold_open_lagging` | 91.471 | 110.650 | +21% |

`write_batch_200` (+15%) and `propose_land_500_flat` (+4%) carry the intended costs: one more frame
per write, and a merge in propose and land. `cold_open_lagging` is the largest move (+21%; a
standalone `qa-perf` run gave 105.96 ms, so it is not noise): it catches up 4,551 frames from the
log in place of 4,497, 1% more, yet takes about 15-19 ms longer. The likely cause is the index
entries a caught-up frame now makes (the typed (kind, hash) entry and the `view_ops` entry); that
was not profiled. It is inside the gate's budget and is still 18x under the < 2 s target, so it
was not investigated further and not rebaselined.

Rebaselined: nothing. `qa-baseline.json` is unchanged, for both classes.

## M2a2: snapshot engine (2026-10-05)

Measured 2026-10-05, release build, Rust 1.99.0, on the **macOS** class: macOS 26.6.2 / Apple M4 Pro
/ APFS. Linux was not re-measured here: its `qa-baseline.json` class still holds the M2.0 numbers, and
the `_50k` baseline is recorded on the isolated Linux bench at plan end. **The machine was busy for
every run below** (load average 4 to 21 while the runs were going; other work was running on it), so
the timings are noisy. Where a number could be a regression, an A/B against the pre-plan build under
the same load is given.

What changed for the read and write paths: reads go through an `Arc<ReadState>` published by the
writer, with `Materialized`, `Graph` and the full tree in lazily filled cells; a write seeds the
written view's entry with `Materialized::with` (O(ops)) and a landing seeds canon, so a read of the
written view after a write finds its materialization built.
Reproduce: `cargo test -p rdy-engine --release --test perf_budget -- --ignored --nocapture`,
`cargo test -p rdy-engine --release --test perf_folders -- --ignored --nocapture`,
`cargo run -q -p rdy-qa --release --bin qa-perf` and `RDY_QA_DEEP=1 cargo run -q -p rdy-qa --release --bin qa-perf`.

### `perf_budget` at 100k blocks (`spec_11_targets_at_100k_blocks`)

| measurement | target | result | status |
|---|---|---|---|
| cold start (open + materialize + graph), median of 3 | < 2 s | 795 ms (795, 793, 821 ms) | met; re-measured after M2.0 and M2a2 as spec §15.1 asks |
| write p50 / p95 / p99 / max (200 one-claim writes) | p99 < flush p99 + 1 ms (macOS, restated in M2.0) | 5.09 ms / 5.77 ms / 22.46 ms / 28.94 ms; flush p99 6.25 ms, so target 7.25 ms | **missed** (p99); p50 and p95 are within 1 ms of the flush p99 |
| query p99 (`context` + `impact`, 200 probes) | < 5 ms | 3.6 us (p50 2.3 us, p95 3.0 us) | met |
| `version_position` p99 (200 calls, cached) | < 5 ms | 3.1 us (p50 84 ns) | met |
| view materialize, cold (`invalidate`, then `materialized`), median of 3 | < 2 s here (inside the cold-start budget); the spec's cold figure is "< 50 ms" (M2 spec 15.1), which the 10k tier measures below at 36 to 38 ms | 535 ms (525, 535, 538 ms) | met against the 2 s budget; 50 ms is not claimed at 100k |
| view materialize after one write (the state is seeded) | - | 250 ns | informational |
| view materialize, incremental (`with` of one `Create` on the built view, 200 runs) | p99 < 1 ms | p50 3.75 us, p95 4.0 us, p99 12.2 us, max 36 us | met |

The write p99 is the second-slowest of 200 writes, as in the M2a1 section. The body did not move
(p50 5.09 ms against 4.6 to 5.0 ms in M2a1, in the same range under this load). Whether the
slow tail is still the first checkpoint falling inside the 200 writes was not profiled in this run.
The assertion stays as specified; no product code was changed for the miss. Cold materialize at 100k
is 535 ms: a view's full rebuild is the cost the snapshot engine takes off the write path
(a write seeds the next state in microseconds), so it is paid by the first read after `invalidate`,
`reindex` or a reopen only.

`chain1_log_bytes_per_write_stay_flat_to_10k` still passes: 480.0 bytes per write at writes 900 to
1,000 and 486.0 at writes 9,900 to 10,000; propose of 10,001 changes 85.5 ms, land 184.1 ms.

### Folders and the memoised tree (`perf_folders`, 1k folders / 10k docs / about 101k blocks)

| measurement | target | M2 baseline (addendum B) | result | status |
|---|---|---|---|---|
| full tree, first build (`tree_full`) | informational | 9.77 ms p99 (every call built it) | 9.23 ms (`tree first build`) | informational |
| full tree, memo hits, p99 of the 49 calls after the first | < 5 ms | 9.77 ms (**missed**) | 625 ns | **met** (was the only missed read target of addendum B) |
| `resolve_path` p99 (200 doc paths) | < 5 ms | 5.1 us | 8.0 us | met |

The tree is memoised per published state (M2 spec 15.1: per canon head), so a call after the first
returns an `Arc` clone. The first call of a state still builds it, at the cost the old per-call walk had.

### `qa-perf` (typical tier, about 10k blocks) against the M2a1 baseline

Medians in ms, same machine class; "pre-plan" is the build at 573c26b (before M2a2) run back to back
with this build, twice each, under the same load, because the stored baseline was taken on a quiet
machine. "no baseline: recorded" is expected for the new ops.

| op | stored baseline | this build (first run) | this build (A/B runs 1, 2) | pre-plan (A/B runs 1, 2) | note |
|---|---|---|---|---|---|
| `write_single` | 4.250 | 4.239 | 4.585, 4.169 | 5.126, 4.161 | same |
| `write_batch_200` | 7.039 | 9.001 | 9.568, 8.046 | 10.070, 9.057 | busy machine; same as pre-plan |
| `propose_land_500_flat` | 117.674 | 53.099 | 55.000, 54.651 | 144.993, 181.199 | about 3x faster: landing seeds canon in O(ops) |
| `version_position` | 3.582 | 0.000 | 0.000, 0.000 | 0.001, 0.001 | vpos cache hit, as M2a1 measured (p95 5.1 ms is the first, uncached call) |
| `version_position_cached` (new) | no baseline: recorded | 0.000 (200 runs) | 0.000, 0.000 | - | microseconds; judged on the Linux bench |
| `context` | 40.504 | 44.061 | 44.750, 45.764 | 43.435, 43.706 | cold read; same as pre-plan within 5% |
| `impact` | 40.474 | 44.172 | 45.432, 45.942 | 43.285, 44.512 | cold read; same |
| `resolve_path` | 34.479 | 38.511 | 40.438, 40.290 | 37.122, 40.596 | cold read; same |
| `tree_full` | 1.379 | 0.307 | 0.308, 0.315 | 1.260, 1.258 | drops: it is a clone of the memo now (see below) |
| `tree_full_memo` (new) | no baseline: recorded | 0.000 (20 runs) | 0.000, 0.000 | - | an `Arc` clone |
| `view_materialize_cold` (new) | no baseline: recorded | 36.321 | 38.385, 38.534 | - | against "< 50 ms cold" (spec 15.1) |
| `view_materialize_incremental` (new) | no baseline: recorded | 0.004 | 0.003, 0.004 | - | against "< 1 ms" |
| `render` | 1261.995 | 1329.521 | 1365.661, 1491.988 | 5115.126, 1319.804 | noise dominates |
| `export_import` | 562.855 | 596.576 | 585.961, 911.149 | 1134.618, 559.517 | noise dominates |
| `cold_open` | 84.975 | 95.261 | 94.901, 94.803 | 112.563, 87.770 | within noise |
| `cold_open_lagging` | 91.471 | 115.992 | 114.828, 111.547 | 141.666, 110.102 | see below |

`tree_full` (the existing op, `tree(Canon, None, None)`) dropping and `version_position` staying a
cache hit are not regressions, and neither was rebaselined.

**`cold_open_lagging` exceeds the gate's budget against the stored baseline** (116.0 ms against 91.5
ms is +27% and +24.5 ms, over the 25% margin and the 2 ms floor; the A/B runs read 114.8 and 111.5).
It was investigated first: the pre-plan build, run back to back under the same load, reads 141.7 and
110.1 ms. So the move from the stored 91.5 ms is machine drift, plus the +21% M2a1 already recorded
(110.65 ms), and not a cost of this plan: M2a2 does not touch index catch-up. It is 17x under the
< 2 s target. Not rebaselined. The orchestrator judges perf on the isolated Linux bench at plan end.

Nothing is rebaselined by this task: `qa-baseline.json` is unchanged. The three new ops and
`version_position_cached` are recorded in the baseline by the orchestrator's baseline commit.

### Large tier (50k), first measurement

`RDY_QA_DEEP=1 cargo run -q -p rdy-qa --release --bin qa-perf` builds a second project with
`generate_tree(50, 10, 97)` (about 50k blocks), lands it, adds 20 pending writes, and measures the
shared ops under the `_50k` names. First measurement on the busy Mac, medians in ms (not recorded in
the baseline: that is done on the Linux bench):

| op | median | p95 | runs |
|---|---|---|---|
| `write_single_50k` | 5.158 | 7.209 | 20 |
| `context_50k` | 392.947 | 498.048 | 10 |
| `impact_50k` | 402.263 | 593.252 | 10 |
| `resolve_path_50k` | 427.513 | 578.653 | 10 |
| `tree_full_50k` | 0.082 | 1.053 | 20 |
| `tree_full_memo_50k` | 0.000 | 0.000 | 20 |
| `view_materialize_cold_50k` | 430.887 | 442.671 | 5 |
| `view_materialize_incremental_50k` | 0.003 | 0.068 | 20 |
| `render_50k` | 455.365 | 911.582 | 3 |
| `cold_open_50k` | 465.709 | 605.638 | 3 |

The cold reads (`context`, `impact`, `resolve_path`) rebuild the graph from a cold state each run,
which is why they are 10x the typical tier's 40 ms for 5x the blocks (and the machine's load was 21
at the time); the write and the incremental step do not move with size.

### Intake items closed

- **Full tree, memoised:** `tree_full` is built once per published state and returned as an `Arc`;
  the 1k-folder tree p99 goes from 9.77 ms (missed) to 625 ns on the memo hits, with the first build
  at 9.23 ms.
- **`prospective_issues`, `rdy_read` suspects and `run_check` no longer clone canon:** canon and
  view materializations are persistent maps shared by `Arc`, so each takes the state's `Arc` and
  `with` applies the ops in O(ops x log n); at 100k blocks `with` of one op costs 3.75 us (p99 12.2
  us) where a clone of the old `BTreeMap` copied every block. These three paths were not timed
  separately in this task.
- **Cold-start first pass:** the three opens at 100k read 795, 793 and 821 ms in this run, so the
  first pass did not stand out against the 2 s target here (it was 2.06 s in the pre-QA run, taken
  while the OS flushed the freshly generated store). One run does not prove it is gone; the median
  of 3 is kept as the judged number.

## M2b: write through the daemon, daemon cold start (2026-10-05)

`qa-perf` (typical tier, about 10k blocks) gains three ops. `new_doc_single` is the in-process
comparator: one `new_doc` (a doc and its skeleton) on a `Rdy` handle. `daemon_write_single` is the
same write, `new_doc` on view `va`, over one `ctl` connection to `rdy_daemon::start` in this process
(30 runs each). `daemon_cold_start` is `start` to `ready` on the same project once its index is
current (3 runs), so it is the warm-up of canon plus the socket bind; a lagging index is
`cold_open_lagging`'s case. `write_single` is a one-claim write and is shown for reference only: the
two new rows are the like-for-like pair.

Medians and p95 in ms. Mac: macOS 26.6.2 / Apple M4 Pro / APFS, release build, two back-to-back runs
under a load average of about 4 (other builds on the machine, so not quiet). Linux: Ubuntu 24.04 /
AMD Ryzen 7 9800X3D / ext4 on consumer NVMe, isolated bench, release build, two `cargo qa --deep`
runs (run 1 at the plan's last task, run 3 after the final review's fix round; a run 2 between
them agreed with both).

| op | Mac run 1 median / p95 | Mac run 2 median / p95 | Linux run 1 median / p95 | Linux run 3 median / p95 |
|---|---|---|---|---|
| `write_single` (reference) | 8.523 / 11.204 | 4.356 / 5.348 | 2.478 / 2.520 | 2.537 / 25.158 |
| `new_doc_single` (in-process) | 6.014 / 8.317 | 6.311 / 7.170 | 3.636 / 3.686 | 3.662 / 3.705 |
| `daemon_write_single` | 4.562 / 8.281 | 4.982 / 5.792 | 2.606 / 2.705 | 2.651 / 2.729 |
| `daemon_cold_start` | 81.966 / 91.641 | 87.277 / 87.280 | 52.938 / 54.341 | 53.982 / 54.480 |
| `daemon_write_single_50k` | 5.151 / 6.277 (one run) | - | 2.733 / 2.856 | 2.741 / 2.893 |
| `daemon_cold_start_50k` | 258.337 / 264.108 (one run) | - | 193.934 / 195.657 | 193.128 / 193.380 |

`qa-baseline.json` records the Linux run 3 rows for the five new ops (run 2: `daemon_write_single`
2.771, `daemon_cold_start` 53.741, `new_doc_single` 3.880, `daemon_write_single_50k` 2.678,
`daemon_cold_start_50k` 193.742 ms median). On Linux the daemon's write is 2.6 to 2.8 ms median and
under 3 ms p95 in every run, so "< 5 ms p99 on Linux NVMe" holds as far as the tier measures (it
records p95, not p99, over 30 runs). The 25 ms `write_single` p95 in run 3 is an ext4 journal
stall in that op's runs, not the daemon's path.
`daemon_cold_start` is about 54 ms at 10k blocks and 193 ms at 50k, far under its "< 2 s at
100k" target; it tracks `cold_open` (about 51 ms and 165 ms in the same runs) plus the canon
warm-up. `new_doc_single` is measured after the cold opens (it writes 30 docs into view `vd`, which
the cold opens would otherwise open too). The cold-open medians drift between bench sessions by
about 8 ms (`cold_open`) and 12 ms (`cold_open_lagging`) for `main` and this branch alike, so the
`cold_open_lagging` row's +23% against its baseline is the machine, not the daemon.

The daemon adds one socket round trip and one thread hand-off per write, and on the Mac that is not
visible against the one durable flush that dominates the write: `daemon_write_single` reads within
the noise of the in-process `new_doc_single` (and below it in every run, Mac and Linux; `new_doc_single` writes into
a fresh view, `vd`, where the daemon writes into `va`, so the pair is the same shape and not the same
bytes). Reads take no lock. A connection's requests run one at a time in arrival order (a `ctl`
connection reads one line at a time; an MCP session's tool calls are stamped at the transport,
`rdy_mcp::order`), so one connection's writes queue behind each other and two connections' writes
queue behind the one writer thread, and the flush is the cost either way. On the Mac the flush is
the floor (`F_FULLFSYNC`, "Write p99: missed" above); on the Linux bench the daemon's write is
within "< 5 ms p99 on Linux NVMe" as above.

## M2c: views (2026-10-06)

`qa-perf` gains seven ops, in both tiers (the `_50k` names at the large tier), all run last in their
tier on a reopened project so the views and landings they leave behind do not move the ops above
them. They share one fixture: `vr`, an unpinned view of 200 one-claim changes, made before the
typical tier's `propose_land_500_flat` landings so that they rebase it (the large tier lands one
extra view to rebase it).

| op | what is timed |
|---|---|
| `view_materialize_after_rebase` | `invalidate`, then `materialized` of `vr`: an unpinned view after landings rebased it (it reads on canon's head, through `Rebase` ops). The pinned `view_materialize_cold` no longer covers this |
| `include_200` | `view_include` of `vr`'s 200 changes into a fresh view (the view's creation, the deps check and the store write, one flush each for the create and the include) |
| `exclude_cold` | on a built 200-change view with canon built: one `view_exclude`, then the read that rebuilds the view |
| `view_cold` | `invalidate`, then `materialized` of a fresh 200-change view that no landing has rebased |
| `view_incremental` | one write into that view (untimed), then `materialized` of it: the cost of the read once the write's publish seeded the state. A regression that left the state unbuilt shows here as a cold rebuild |
| `land_rebase_50` | `propose` and `land` of one view while 50 other non-empty unpinned views are present, 5 of them with a change on the claim the landing edits (each landing records 5 conflicts). The fifty are `vr`, the views earlier ops left and `lv<k>` views of 4 one-doc changes; the scratch views of the ops above are closed first, since a closed view is not rebased. The landing view is the 51st |
| `view_ls_50` | `invalidate`, then `view_list(all = true, 14 days)` over those 50 views plus every closed and landed one |

Targets (spec 15.1): view materialization < 50 ms cold and < 1 ms incremental (exclude counts as
cold), landing with 50 views rebasing < 100 ms p99. The spec gives no figure for a 200-change
include or for the list, so `include_200` is read against the 50 ms cold figure and `view_ls_50`
has no target. `include_200`, `exclude_cold` and `land_rebase_50` end in durable flushes, so they
take the wider (50%) regression margin that the write ops do. Each op's target text is in
`spec_target` (`vperf1_the_view_ops_state_their_targets`), and `vperf1_the_view_ops_run_on_a_small_tree`
runs the whole set on a small tree.

### Judged figures: Linux bench

Added at plan end by the orchestrator (Linux / x86_64, 8 cores / NVMe; two agreeing
`PERF=1 rdy-bench.sh ... cargo qa --only perf` runs). Perf is judged there, not on the Mac.

| op | Linux run 1 median / p95 | Linux run 2 median / p95 |
|---|---|---|
| (to be filled in) | | |

### Mac figures (indicative: one smoke run, not quiet)

macOS 26.6.2 / Apple M4 Pro / APFS, release build, one `cargo qa --only perf --deep` run under a load
average of about 4 (a clippy run overlapped its first minute), so the figures are noisy and nothing
here is judged. The same run read the existing ops 36 to 62% over their M2.0 baseline (`context`,
`impact`, `resolve_path`, `export_import`, `cold_open`, `cold_open_lagging`), a uniform shift that
points at the load and not at one op.

| op | typical (10k) median / p95 ms | large (50k) median / p95 ms |
|---|---|---|
| `view_materialize_after_rebase` | 54.7 / 57.1 | 166.2 / 170.3 |
| `include_200` | 68.3 / 75.8 | 184.8 / 197.4 |
| `exclude_cold` | 63.2 / 66.2 | 179.8 / 192.0 |
| `view_cold` | 56.1 / 71.0 | 166.9 / 173.9 |
| `view_incremental` | 0.000 / 0.001 | 0.000 / 0.000 |
| `land_rebase_50` | 36.7 / 36.9 | 85.6 / 99.5 |
| `view_ls_50` | 567.6 / 586.3 | 1,623.2 / 1,692.0 |

For reference, the pinned `view_materialize_cold` read 48.1 ms in the same run (164.8 ms at 50k), so
a cold 200-change view costs the canon base plus a few ms of its own: on this Mac a cold
materialization is at or just over the 50 ms figure at 10k blocks, and the target is judged on
the bench. The landing is well inside its 100 ms at 10k (36.7 ms) and at 50k (85.6 ms median, 99.5 ms
p95). `view_ls_50` is the one large number: `view_list` reads each open view's conflicts, which
builds the view (closed views are skipped), so a cold list over the 50 live views is about 50 view
builds (11 ms each at 10k). It is cold by construction (`invalidate` first); after any read of the views the list is a cache hit per view.
It has no target, and a per-view cached conflict count would be the fix if `rdy view ls` on a
cold daemon turns out to matter.

Final review fix round 1 made that fix: `view_list` no longer builds a view to count its
conflicts. A built view gives its build's count; otherwise the count is a merge of the view's
changes onto its base restricted to the blocks they touch (`merge_on_blocks`, whose conflicts are
the full merge's), cached in the view's state entry, with each base state read once per list.
`vls1_a_cold_list_counts_conflicts_without_building_the_views` pins that the list builds no view.
The figures above predate it; the bench run at plan end measures the new cost.

### Landing with 50 views at 100k (`m2c_landing_with_50_views_at_100k`, ignored)

100,000 blocks (`testkit::generate`, landed), 50 views of 20 changes each (1,000 writes, 5.1 s),
the first 5 editing the five claims that five landings in turn edit; each landing is `propose` plus
`land` of one more view. Recorded, not gated: the test asserts p99 < 500 ms (5x the target).

| measurement | target | result |
|---|---|---|
| `propose` + `land` with 50 views present, 5 landings, p50 / p95 / p99 / max | < 100 ms p99 at the typical tier; 100k recorded | 159.1 ms / 184.1 ms / 184.1 ms / 184.1 ms (macOS, release, one run) |

(With 5 samples the p95, p99 and max are the same sample.)

### Rebase cost

The merge work at a landing is O(touched): `merge_on_blocks` runs on the intersection of the
landing's blocks and each view's blocks only (ruling "Rebase cost"). What is not O(touched) is
finding each view's touched blocks: the landing collects them per landing by reading each live
view's remaining changes once (a cached decode each), so the cost grows with the number of
in-flight changes in unpinned views, not with the size of canon. That is the term the 100k figure
reflects (1,000 in-flight changes there, against about 430 in the typical perf tier). If the bench misses the
100 ms target, the fix is an incremental per-view touched-block index, a fix-forward task for the
orchestrator to schedule.

## M2b3: the edit loop (2026-10-06)

`qa-perf` gains three ops, each at 10k (typical tier) and 50k (`_50k`), run in a fresh view `eb`
where the fixture's section is `stable`, so every write there is checked against the briefing
registry. `brief_doc` is `ReadState::brief` over the fixture's doc by path plus `register`, timed on
the read pool with the view's graph carried (an untimed briefed write creates the new state first).
`write_briefed` is one claim create that cites a current briefing: the check runs and hits.
`write_consequences` is a briefed `update` of a claim that another claim references, then
`ReadState::consequences` over the change, asserting the referrer comes back as one `suspect` entry
(the assert is outside the timer); the briefing of the claim is made untimed. `write_single` is
shown from the same runs as the like-for-like write without a briefing.

Medians and p95 in ms. Linux: Ubuntu 24.04.5 LTS / AMD Ryzen 7 9800X3D 8-Core Processor / ext4,
isolated bench, release build, two `cargo qa --only perf --deep` runs back to back (the bench idle
at the start of each, `PERF.lock` held by the helper); every new op's median differs by at most 4.3%
between the runs (`brief_doc` 4.3%, the rest under 1%), so both are recorded. The Mac was not
measured (the perf rows come from the bench only).

| op | 10k run 1 median / p95 | 10k run 2 median / p95 | 50k run 1 median / p95 | 50k run 2 median / p95 |
|---|---|---|---|---|
| `write_single` (reference, same runs) | 0.532 / 0.578 | 0.528 / 0.566 | 0.548 / 0.576 | 0.549 / 0.571 |
| `write_briefed` | 0.574 / 0.593 | 0.576 / 0.597 | 0.642 / 0.678 | 0.644 / 0.676 |
| `write_consequences` (write + consequences) | 0.568 / 0.587 | 0.569 / 0.601 | 0.624 / 0.651 | 0.628 / 0.656 |
| `brief_doc` | 0.201 / 0.228 | 0.209 / 0.229 | 0.227 / 0.284 | 0.226 / 0.299 |

Every row is far under "< 5 ms p99 on Linux NVMe" (writes) and "< 5 ms p99" (`brief_doc`) at both
sizes, so none is a miss. The briefing check adds about 0.04 ms at 10k and 0.09 ms at 50k to a write
(`write_briefed` against `write_single`): the check is one registry read and one version lookup per
recorded block on the writer, the registry read being a mutex-guarded hash-map lookup. A write plus
its consequences costs no more than the briefed write (the tier records p95, not p99, over 20 to 30
runs), because the consequences run on the read pool over the carried graph (Tasks 2 and 4), a graph
walk over the changed block's incoming edges and not a rebuild; a cold graph build is about 30 ms at
10k, and the carry is what keeps it off this path. `brief_doc` over a 97-claim doc is about 0.2 ms
at both sizes: its cost follows the doc, not the project.

The daemon rows, before (`main`'s baseline in `qa-baseline.json`, the M2b run) and after (this
branch, both runs):

| op | before median / p95 | after run 1 median / p95 | after run 2 median / p95 |
|---|---|---|---|
| `daemon_write_single` | 2.651 / 2.729 | 0.671 / 0.830 | 0.664 / 0.688 |
| `daemon_write_single_50k` | 2.741 / 2.893 | 0.672 / 0.793 | 0.687 / 0.799 |
| `daemon_cold_start` | 53.982 / 54.480 | 51.860 / 52.013 | 51.600 / 51.892 |
| `daemon_cold_start_50k` | 193.128 / 193.380 | 202.307 / 202.529 | 201.146 / 205.422 |

### The write floor is the drive's, and it moves (confirmed 2026-10-06)

The first two runs above read a write floor of about 0.53 ms (`write_single`) where the baseline's
old rows have 2.6 ms. That is not this branch and not the bench's setup; it is the drive, and it
moves between about 0.4 and 2.6 ms per flush on the same machine class.

- **`main` shows the same floor.** The pre-plan base (`f558724`) on the same bench, same command
  (`cargo qa --only perf --deep`), two runs: `write_single` 0.528 and 0.531 ms, `new_doc_single`
  1.684 and 1.681 ms, `daemon_write_single` 0.647 and 0.652 ms, `write_single_50k` 0.550 and 0.547
  ms, `daemon_write_single_50k` 0.664 and 0.657 ms, `daemon_cold_start` 49.5 and 49.7 ms (50k: 190.7
  and 190.2 ms). So the 4x drop of `daemon_write_single` against its old baseline is not the daemon
  work of this plan.
- **The temp dir is the same mount.** The perf tier's `tempfile::tempdir()` is `/tmp`, on the root
  ext4 filesystem on the NVMe (`/dev/nvme0n1p5`), the same one the machine-class string names; the
  kernel reports the device write cache as `write back` with FUA, so `fsync` issues a real flush.
  The block layer's own flush counter (`/sys/block/nvme0n1/stat`) went up by exactly 600 for 600
  `fsync`/`fdatasync` calls of a 4 KB-write micro-benchmark, at about 0.29 ms of device time each:
  every flush reaches the device, and the device answers fast in that state.
- **The device is bimodal over minutes.** The micro-benchmark (4 KB `pwrite` + `fdatasync`, 300
  runs) read p50 0.43 ms with the bench idle, then 2.4 to 2.6 ms (what "Linux" at the top of this
  file and the M2b baseline record) minutes after the perf runs, and 1.4 to 1.6 ms in between; it
  flipped within a single `qa-perf` run (a 10k tier at 2.6 ms, the 50k tier at 0.55 ms). It is
  reproducible on demand: 20 GB of sequential writes (`dd`, `conv=fsync`) took the idle 0.43 ms to
  2.6 ms, held for minutes. So the floor tracks recent write volume on the drive (a consumer NVMe
  without power-loss protection; the cause inside the drive is not established here). The M2.0 notes
  of this file saw the same swing (flush p99 0.31 ms in one run, 1.3 to 1.4 ms in the next).

Which gives the baseline rule (`perf.rs`: a row fails only when worse than its baseline by the
margin and by more than `FLOOR_MS`): every floor-bound Linux row is baselined on the **slow floor**
(about 2.6 ms per durable flush), where `write_single`, `write_single_50k`, `new_doc_single` and
`write_batch_200` already were. A row baselined on the fast floor fails when the drive is in the
slow state: the first baseline of this branch did, and the bench said so (`write_briefed` 2.62 ms
against 0.575 ms, `daemon_write_single` 2.68 ms against 0.668 ms: FAIL). On the slow floor a
fast-state run reads as an improvement and passes. The `write_briefed`, `write_consequences` and
`daemon_write_single` rows (and `_50k`) are the mean of two runs taken in the slow state:

| op | 10k run A / run B | 50k run A / run B |
|---|---|---|
| `write_briefed` | 2.615 / 2.639 | 2.802 / 2.806 |
| `write_consequences` | 2.590 / 2.630 | 2.782 / 2.793 |
| `daemon_write_single` | 2.678 / 2.721 | 2.839 / 2.818 |

(medians in ms; each pair agrees within 1.6%. The 10k pair is two runs with the drive left slow by
the preceding runs, the 50k pair two runs after a 20 GB `dd` to put it there. The floor also moves
inside a run, so the unchanged `write_single` of those same runs reads 2.5 to 2.8 ms in three of
the four and 1.5 to 1.7 ms in the others; the rows above are the pairs where the op itself ran on
the slow floor.) Against `write_single` in the same run the briefing check adds 0.01 to 0.1 ms in
either state; the briefed write is the flush plus that. `brief_doc` and the
two `daemon_cold_start` rows do not depend on the flush floor and stay as the first two runs
recorded them. The same baseline now holds one floor for every write row, so the other Linux write
rows need no change.

The cold start lost its fixed client poll (Task 6: `connect_backoff`, 1 ms doubling to 50 ms,
replaces the 100 ms `CONNECT_RETRY` between connects while a spawned daemon comes up), but this
tier cannot show it: `daemon_cold_start` starts the daemon in this process and connects once the
socket is there, so it never sleeps in that loop, and the row moves by the noise (54.0 to 51.7 ms
at 10k, -4%; 193.1 to 201.7 ms at 50k, +4%, inside the 8 to 12 ms cold-open drift between bench
sessions noted under M2b). The saving is on the paths that spawn a daemon and wait for its socket
(the CLI and the MCP proxy), which no `qa-perf` op times.

## M2b2: history, blame and log

The `qa-perf` tier gained four ops for the history reads. Each warms the read state outside the
timer (`materialized(&Scope::Canon)`) and times only the call, and each asserts its result size.

| op | what it times | project | runs | target |
|---|---|---|---|---|
| `history_100` | `history(Canon, claim, 101)`: every version of a block updated 100 times, each in its own landed view | `deep_block`, its own project (101 versions) | 20 | < 5 ms p99 (reads, spec 11) |
| `log_page_20` | `log_page(None, Some(20))`: the newest 20 rows of the log | `deep_history` (2000 landings) | 20 | < 5 ms p99 (reads, spec 11) |
| `log_full` | `log_rows()`: the whole log | `deep_history` (2000 landings) | 5 | recorded, no spec target |
| `last_changes_doc` | `last_changes(Canon, subtree(doc))`: the last change of every block of the fixture doc | the tier's project (`last_changes_doc_50k` at 50k, the same budget with the large-tier note) | 20 (10 at 50k) | < 5 ms p99 (reads, spec 11) |

What each cost grows with:

- `history` grows with the block's version count: each version costs one producer lookup, one
  change read and one landing lookup. `history_100` is the 101-version case; a block with a
  handful of versions costs a handful of those.
- `last_changes` grows with the number of distinct changes in the subtree, not with the number of
  blocks: blocks written by the same change share one memoised change read.
- `log_page` builds a row (one proposal read each) only for the rows it returns, while the
  landing list and the version tags are listed whole; `log_full` builds a row for every landing
  and grows with the length of the chain.

`deep_block` is built outside every timer in its own directory, so the workload project, and with
it `cold_open_lagging`'s frame count, is unchanged.

No laptop figures are recorded here: the Mac is shared while this plan runs, so the first timing
of these ops is the Linux bench's.

Linux bench rows: <orchestrator fills in (two agreeing runs)>

## M2c3: briefing in views (2026-10-06)

The `qa-perf` tier gained three ops at the end of `tier_views`, after `view_ls_50`. The views the
tier leaves behind are live while they run: the 50 landing views (5 of them touching the landing
claims), the `inc*` views of `include_200` and the closed ones. Each large-tier twin
(`brief_inflight_50_50k`, `view_consequences_200_50k`, `landing_consequences_50k`) carries the
typical tier's budget at 50k blocks.

| op | what it times | runs | target |
|---|---|---|---|
| `brief_inflight_50` | `brief(Canon, claims)` over the landing claims, with the 50 live views up. The touching views change those claims, so `inflight.views` is not empty (asserted) | `runs.read` | < 5 ms p99 at 10k with 50 live views (the read budget, spec 11; amendment §4.5 item 1) |
| `view_consequences_200` | `view_consequences` for a fresh 200-change include, on the read pool. The include itself (a store write) is outside the timer; the read states before and after it are captured outside too | `runs.view_ops` | no spec target (recorded for M2c3; read against the < 50 ms cold figure) |
| `landing_consequences` | `landing_consequences` of a new proposal (one claim updated in its own view), on the read pool. The write and the propose are outside the timer. Canon's graph is built once before the op | `runs.view_ops` | no spec target (recorded for M2c3) |

What each cost grows with:

- `brief_inflight_50` adds, to the briefing, one lookup per live view in a memoised set of the
  block ids that view's changes touch (`Entry.touched`, built once per view entry), and one pass
  over the open proposals. The first read of a view entry builds its set; a publish that changes
  the view makes a new entry.
- `view_consequences_200` compares the view's materialization before and after the op, only for
  the blocks the op's changes touch, then derives the consequences of the changed blocks from the
  view's graph. It grows with the number of changes the op added.
- `landing_consequences` merges the proposal's changes onto canon, applies the applied ops to
  canon's materialization and carries canon's graph across them. It grows with the number of ops
  in the version, not with the project.

Baseline: `qa-baseline.json` is not changed here.

Linux bench rows: (bench: the orchestrator fills these rows from two cargo qa --only perf --deep runs on the isolated Linux bench)

Deep gate (bench): <orchestrator fills in>

## M2d1: graph, areas and coordination (2026-10-06)

The `qa-perf` tier gained `tier_inflight` after `tier_views` in both tiers, and the typical tier
gained `tier_mcp` last. The fixture writes 1,000 single-op changes across 20 views (`inf0` to
`inf19`) on the 10k tree (the 50k tree with `--deep`): each is an update of a distinct claim of
canon (the first 1,000 claim ids by id, not the fixture's claim). `inf0` also updates the
fixture's claim, so its impact crosses in-flight edits. The tier first closes the views the
earlier tiers left open: the landing views edit claims a landing changed, so they leave the
in-flight state with conflicts, and a conflicted in-flight build is never carried. Each large-tier
twin carries a `_50k` suffix and the typical tier's budget at 50k blocks. The op names keep `1k`
whatever the fixture's size (the small-tree test runs 40 changes).

The intake found a spec gap: a cold `context` or `impact` costs 31 to 35 ms at 10k. These ops time
warm reads, because the graph is built once per published state. The cold build has its own ops
(`inflight_build_1k`, `coord_build_1k`).

| op | what it times | runs | target |
|---|---|---|---|
| `inflight_build_1k` | cold: `invalidate`, then `graph(Inflight)` (the merge of the 1,000 changes onto canon, the materialization and the graph) | `runs.materialize_cold` | no spec target: the in-flight state's cold build, once per published state (recorded for M2d1) |
| `inflight_carry` | one clean commit (a new claim) into `inf1` after the state was built, then `materialized(Inflight)`. The write and the first read are outside the timer. Asserts that the build was carried | `runs.incremental` | < 1 ms incremental (spec 15.1, view materialization) |
| `context_inflight_1k` | `context` of the fixture claim in `Scope::Inflight`, on a built state | `runs.read` | < 5 ms p99 with 1k in-flight changes (spec 15.1; the overlays must not regress them) |
| `impact_inflight_1k` | `impact(…, 3)` of the fixture claim in `Scope::Inflight`, on a built state | `runs.read` | same |
| `context_view_1k` | `context` of the fixture claim in `Scope::View("inf0")`, on a built state | `runs.read` | same |
| `impact_view_1k` | `impact(…, 3)` of the fixture claim in `Scope::View("inf0")`, on a built state | `runs.read` | same |
| `coord_build_1k` | cold: `invalidate`, then `coord()` (the coordination graph of the in-flight state) | `runs.materialize_cold` | no spec target: the coordination graph's cold build, once per published state (recorded for M2d1) |
| `mcp_params_9mib` (typical tier only) | `parse_params::<WriteParams>` alone on one `rdy_write` call of 2,002 ops, about 9.2 MiB. The arguments are built outside the timer | 3 | no spec target: the comparator for `mcp_write_9mib` |
| `mcp_write_9mib` (typical tier only) | `parse_params` and `rdy_write`, consequences included, with a fresh doc name each run. Ends in a durable flush, so it takes the wider margin | 3 | no spec target: recorded (intake: the `&Value` parse and the deep clone of each op cost about +30 ms per 9 MiB) |
| `write_expired_intents_1k` (typical tier only) | `write_and_report` (the MCP and CLI write, consequences and intent notes included) of an update of the fixture claim by `agent:b`, in a fresh small project (2 folders x 3 docs x 12 claims) where 1,000 intents on that claim have expired and 5 are open. Asserts the reply names the 5 open intents only. Ends in a durable flush | `runs.write` | the write target (spec 11): the intents must not cost a store read each |
| `brief_expired_intents_1k` (typical tier only) | a warm `brief` of the fixture claim in view `rb`, same project. Asserts 5 in-flight intents | `runs.read` | < 5 ms p99 (the read budget, spec 11) |
| `write_settled_alts_1k` (typical tier only) | the same write in another fresh small project with 1,000 abandoned alternative sets (2,000 closed member views) | `runs.write` | the write target (spec 11): the sets must not cost a store read each |
| `open_chosen_alts_100` (typical tier only) | `Rdy::open` of another fresh small project with 100 chosen alternative sets. In each set, the winner view wrote 50 changes after the set was created, then proposed and landed the choice. The last 10 sets are heavy: before the set was created, the loser view wrote 50 new claims (bound to it). A third member joined with `alt_add` holding 50 claims of its own (the joiner reads). The winner recorded a choice decision. The open derives every chosen set's bindings from history: it walks each member's chain from its head back to the set's creation, and builds the set there (msw20) | `runs.materialize_cold` | < 2 s (the cold-open budget, spec 11) |
| `propose_chosen_alts_100` (typical tier only) | `propose` in the same project of a view that holds 50 changes, plus one loser change of a heavy set and a decision that refines that set's choice decision. The include and the decision are outside the timer. The gate checks each change against every settled set and folds the proposal's decisions (msw20). Ends in a durable flush | `runs.write` | the write target (spec 11) |

What each cost grows with:

- `inflight_build_1k` and `coord_build_1k` grow with the number of in-flight changes and with the
  tree: the first merges every change onto canon and builds from scratch, as canon is not built
  after the `invalidate`. A published state pays it once, on its first in-flight read.
- `inflight_carry` grows with the ops of the one commit. A commit whose change depends on the last
  in-flight writer of every block it writes, onto a build with no conflicts, seeds the next state.
- The warm reads cost what a read of any built scope costs: a lookup in the graph.
- `mcp_params_9mib` grows with the size of the request. `mcp_write_9mib` adds the deep clone of
  every op and the write itself.
- The coordination ops (final-review fix round 1) do no store read per intent or set. No intent
  or set is ever deleted: an intent expires at read time and a set is settled. So
  the published state indexes the intents whose latest version is open (`open_intents`) and the
  open and the settled sets, and a publish reads only the intents and sets that moved. A write
  and a briefing filter the index by `now` in memory: an expired intent costs one comparison.

The `mcp_*` ops run last, on a reopened project: their 6,000 blocks move nothing above them.

Baseline: `qa-baseline.json` is not changed here.

Linux bench rows: (bench: the orchestrator fills these rows from two cargo qa --only perf --deep runs on the isolated Linux bench)

Deep gate (bench): <orchestrator fills in>

## M2d2: deterministic tensions (2026-10-06)

The `qa-perf` tier gained `tier_tensions`, run right after `tier_inflight` in both tiers, on the same
project. It reuses the 1,000 changes `tier_inflight` leaves in flight across `inf0` to `inf19` on the
10k tree (the 50k tree with `--deep`) and adds 50 citing claims written round-robin into those
views. Each citing claim cites a claim that another view updated, so premise shifts exist (the tier
asserts at least 50 tensions, untimed). Each large-tier twin carries a `_50k` suffix. The op names
keep `1k` whatever the fixture's size (the small-tree test runs 40 changes and 10 citing claims).

Eight ops (the two `on_change` ops for a delete and a view op were added in the final review's fix
round, I7):

| op | what it times | runs | target |
|---|---|---|---|
| `tension_carry_1k` | one update of a fresh canon claim into `inf0` after the state's tension set was built. The timer holds the whole write: the carry of the set, the moves between the logged state and the new one, and the seal of the batch with its record (plan M2e2, founder decision C). The read after it finds the set built. Asserts that the set was carried | `runs.incremental` | recorded, no spec target: one write that carries the tension set and logs its moves in its batch (plan M2e2, founder decision C) |
| `tension_log_close_50_1k` | outside the timer, as agent `tlog{i}` in view `tlog{i}`, 50 one-op updates of claims that an in-flight view also updates (each opens one `edit_collision`); timed: `view_close` of `tlog{i}`. Asserts that the close's batch ends with one record of at least 50 resolved moves | `runs.incremental` | recorded, no spec target: a view close that resolves 50 tensions, logged as one record (plan M2e2) |
| `tension_on_change_1k` | one citing claim into `inf1` between two captured states, then `tension_delta` and `delta::on_change` for view `inf1` (what the write response computes; the second state's set was carried inside the write, which logs its moves). The write is outside the timer. Asserts the write opened a tension | `runs.incremental` | < 10 ms p99 with 1k in-flight changes (M2 spec 15.1: `on_change` and tension detection, included in the write response) |
| `tension_on_change_delete_1k` | one delete of a fresh canon claim into `inf1` between two captured states, then `tension_delta` and `delta::on_change` for `inf1`. A delete never carries, so the in-flight build of the second state's set and the diff ran inside the write (it logs its moves), outside the timer; the timer holds the delta and the report. Asserts the set was rebuilt | `runs.incremental` | same as `tension_on_change_1k` |
| `tension_on_change_view_op_1k` | one of `inf2`'s changes included into a fresh view between two captured states, then `tension_delta` and `delta::on_change` for that view. The view op's write carries the set across the change that moved views and logs its moves, outside the timer; the timer holds the delta (a diff that skips the structure the two sets share) and the report | `runs.incremental` | same as `tension_on_change_1k` |
| `tensions_full_1k` | cold: `invalidate`, then `tensions_rebuilt()` (every detector over canon and every in-flight change) | `runs.materialize_cold` | no spec target: the cold build of every tension at 1k in-flight changes, once per landing or structural update (recorded for M2d2) |
| `tensions_canon` | cold: `invalidate`, then `canon_tensions_rebuilt()` (the canon scope only; FW-CH6). It runs once per landing: the canon part is memoised on canon's build, so a commit never pays it | `runs.materialize_cold` | no spec target: the canon-scope build of the tensions (FW-CH6), once per landing; recorded for Coverage and confidence |
| `gate_tensions_1k` | `proposal_tensions` for `inf2` with its changes, on a built set | `runs.read` | recorded: the land message's tension check on a built set (inside the landing budget, M2 spec 15.1) |

What each cost grows with:

- `tension_carry_1k` and `tension_on_change_1k` grow with the tensions around the seed blocks of
  the one change, not with the number of changes in flight: the carry removes and re-detects only
  those. For duplicate definition (FW-TN2: one tension per group) the carry also re-detects around
  every member of each group a seed was or is in, so it grows with the size of those groups. A commit that updates a structural block, moves a block or changes its lifecycle does not carry and
  pays the full build. A created doc, section or folder carries (perf fix before the M2d2 merge:
  `daemon_write_single` is a `new_doc`, and its settle on the writer did the full build before the
  next request). A commit whose state before was not read yet carries that state's set first, when
  it is published, so a run of commits that nobody reads in between never leaves the next read a
  full build (the first `brief_doc` run did).
- `tension_on_change_delete_1k` grows with the in-flight changes and their writes: a delete, a
  structural update, a move or a lifecycle write builds the in-flight part of the set again
  (`Writes::build` and every detector over the live writes). `tension_on_change_view_op_1k` grows
  with the diff of the two sets (the view op carries, see below). A
  delete does not carry by design (a deviation from the plan's carry rule, fix 118991c,
  confirmed in the final review's fix round): it takes a block out of reads that the seeds do not
  cover, for example an `affects` target's path, which gives its deciders their areas. The
  canon part is not in this cost: it is memoised on canon's build as one map that every set
  built on it shares (fix round I7), so a build does not copy it and a diff skips it. Before
  that fix a delete's `on_change` on the 10k tree took 53 to 56 ms on macOS, most of it copying
  and diffing 43,650 all-canon text duplicates.
- `tension_carry_1k` and `tension_log_close_50_1k` (plan M2e2, founder decision C) hold the
  moves of a write: the diff of the logged set and the new one, by id (the structure a carry
  shares is skipped), the suspect diff of canon's graph (only when the write moves canon or
  the pack: a commit into a view moves no suspect), and one record in the batch, before its
  one sync. `tension_settle_1k` is gone: the settle after the reply no longer
  exists, and its diff runs inside every write.
- Since every write logs its moves, every publish needs its state's set, so each kind of write
  carries it (M2e2 perf fix): a commit (as above); a view op (`include_200`, `exclude_cold`, a
  close, `tension_log_close_50_1k`), re-detecting around the changes whose membership or views
  moved, with their writes updated on their blocks only; and a landing (`land_rebase_50`,
  `propose_land_500_flat`), re-detecting in flight around the landed blocks and the changes that
  left flight, with the new canon's part carried for duplicate definition (the other canon kinds
  run in full) and the in-flight state built on the new canon. A step that takes a delete, a
  structural or lifecycle update, or a non-fresh create into or out of flight (or onto canon)
  builds the set in full, as a commit does; so does a step whose seeds are more than 1,000 and
  outnumber the blocks in flight before it (an import: `export_import` builds the imported set
  once). `Rdy::init` reconciles the empty project, and `invalidate` computes no moves when the
  published state is the logged one, so a cold read stays cold.
- `tensions_full_1k` grows with the in-flight changes and the tree; a landing or a structural update
  pays it once per published state, inside the write that logs its moves. A briefing after a carried commit and a proposal never pay it: a proposal lists
  its changes' tensions from the set when it is built or carried, else from the detectors seeded
  with what its changes write (no canon part); a briefing reads the sides of a tension row only for
  the rows a page shows with them, and measures each row once. It includes the cold build of the
  in-flight state, as canon is not built after the `invalidate`.
- `tensions_canon` grows with the tree (and with the all-canon tensions in it). It is the canon
  part of `tensions_full_1k`, paid once per canon build. FW-TN2: the generated tree's text
  duplicates are 970 groups (10 members each at 10k, 50 at 50k) instead of 43,650 pairs at 10k
  and about 1.19M at 50k; on macOS the canon build of the 10k tree went from 305 ms to 52 ms.
- `gate_tensions_1k` grows with the changes of the version and the tensions that involve them.

Baseline: `qa-baseline.json` is not changed here.

Linux bench rows: (bench: the orchestrator fills these rows from two cargo qa --only perf --deep runs on the isolated Linux bench)

Deep gate (bench): <orchestrator fills in>

M2 hardening (hd1): a briefing builds and measures only the tension rows its page can show.
Before, every row that involved a briefed block was built and serialized, and the cut then showed
a few (brief_doc_50k: 1.16 ms median on the bench, of which about 0.6 ms was rows).

## Agent ergonomics 2: search (2026-10-06)

The `qa-perf` tier gained one op for block-text search (FW-PR1). It runs in `tier_reads`, after
`tree_full_memo`, so the canon state is warm and the timer holds only the search.

| op | what it times | project | runs | target |
|---|---|---|---|---|
| `search_literal` | `search(Canon, "Claim", first page, 4000 tokens)`: a literal that every generated claim holds | the tier's project (about 10k blocks) | 20 | no spec target: a full scan of block text, first page (recorded for FW-PR1; semantic search is M5) |
| `search_literal_50k` | the same call at 50k blocks | the large tier's project | 10 | the same, with the large-tier note |

What the cost grows with:

- The walk visits every block in reading order and matches each line of the body and of every
  string field. The cost grows with the total text in the scope, not with the page size.
- Each matching block adds its id and version hash to the page token. The search hashes once
  over all matches, so a query that most blocks match costs more than a rare one.
- Every matching block gets its path, because the totals count hits per doc. Only the entries
  on the page are serialised for the token budget.
- Each search reads the scope's graph (built once per state, then cached), because a match
  inside a superseded, rejected or revoked decision is marked `history`.

The regex crate matches in linear time, so one search of a line costs one linear pass. Compile
size is capped at 1 MiB (`REGEX_SIZE_LIMIT`), and the lazy-DFA cache of one search at 16 MiB
(`REGEX_DFA_LIMIT`). A pattern that matches empty text restarts after each empty match, and a
restart can scan to the end of the line. Each restart after the first on a line is charged the
rest of the line, and a search whose charge passes 32 MiB (`EMPTY_RESCAN_LIMIT`) is `E_LIMIT`.
The charge is the worst case, so a pattern that matches empty text at each position (`q*`, `x?`)
is refused when the scope has one line over about 8 KiB, even though it is cheap.

No laptop figures are recorded here: the first timing of these ops is the Linux bench's.

Linux bench rows: (bench: the orchestrator fills these rows from two cargo qa --only perf --deep runs on the isolated Linux bench)

## Entities and fact sheets (2026-10-06)

The `qa-perf` typical tier gained four ops in `tier_entities`. It runs after `tier_views` and
before `tier_inflight` (which closes every open view, `ve` too, and adds 1k in-flight changes, so
the entity ops run on a clean state and the in-flight ops after them as before), on a fresh view
`ve`. `ve` holds the fixture's canon plus 50
entities (each with a name, one alias and a kind) and 200 claims. Each claim names two entities
and one of ten shared code literals. The ops carry no `write` prefix on purpose: `spec_target`
gives every op that starts with `write` the write budget, and `fsync_bound` widens its margin.

| op | what it times | runs | target |
|---|---|---|---|
| `lexicon_build` | `lexicon(ve)` after the engine caches are dropped (outside the timer): the materialization, the graph and one pass over the text of the scope | `runs.read` | no spec target: a cold build of the entity lexicon over a 10k view (recorded for FW-EN1) |
| `entity_search` | `search(ve, "Entity 7", first page, 4000 tokens)` with the lexicon and graph warm: the entity resolves, its fact sheet is built and the match list is filtered | `runs.read` | < 5 ms p99 (a read, spec 11): an entity search with its fact sheet, first page |
| `suggestions_after_write` | `suggestions(ve, change)` for the change of a one-claim write. The write is outside the timer. The write carries the lexicon over its own blocks | `runs.write` | no spec target: the suggestions of a one-claim write over a carried lexicon (recorded for FW-EN3; they ride on every write response) |
| `coverage_all` | `coverage_issues(ve)` after the engine caches are dropped (outside the timer): the materialization, the graph, the lexicon, the structural layer and the state layer, in every dimension | `runs.read` | no spec target: the cold coverage issues over a 10k view, every dimension (recorded for FW-PF2 D-V3JSY; rdy coverage, rdy status) |

What each cost grows with:

- `lexicon_build` grows with the total text of the scope. It is built once per scope, then
  carried across each commit and landing in time proportional to the text of the written blocks.
  Every publish now carries it once it is built.
- `entity_search` grows with the number of entities that match the query (at most 8 sheets) and
  with the edges of each. The match scan is `search_literal`'s, read in any case and spacing (a
  literal that names an entity matches every unlinked mention).
- `suggestions_after_write` grows with the text of the written blocks and with the holders of
  each code literal they contain. It does not grow with the project. Every write response
  computes it, so it adds to the write path (`write_and_report`) but not to the durable flush.
- `coverage_all` grows with the number of blocks: it checks every block in all six dimensions
  and reads the tension set and the suspects. It runs for `rdy status` and for `rdy_overview`.
  A write carries the structural layer (see Coverage and confidence below), so the op times the
  cold pass. The write response reads the coverage change from two memoised rollups (the score
  and dimensions, no entity kinds) and the touched pages' own blocks (`write_consequences` and
  `propose` do this work; cc16).

No laptop figures are recorded here: the first timing of these ops is the Linux bench's.

Baseline: `qa-baseline.json` is not changed here.

Linux bench rows: (bench: the orchestrator fills these rows from two cargo qa --only perf --deep runs on the isolated Linux bench)

Deep gate (bench): <orchestrator fills in>

### Briefings and the entity work (enperf2)

The deep gate failed `brief_doc` (9.5 ms against 0.205 ms) and `brief_doc_50k` (51 ms against
0.226 ms). The cost grew with the project. The cause was `mentioned_by` (FW-EN3): for a covered
new doc, it read the text of every block of the scope. The fixture's doc ("Doc 0") is new, so
each briefing read 10k blocks (50k in the large tier). On the M4 Pro, that was 15.3 ms of a
16.1 ms briefing. Graph coverage of the covered blocks took 0.22 ms and related decisions
0.03 ms.

The fix:

- The lexicon holds a word index: each case-folded word and the blocks whose text holds it. A
  phrase can only be found in a block that holds each of its words, so `mentioned_by` reads only
  those blocks. The index is built with the lexicon and carried across writes with it.
- The mention scan of a set of phrases is kept in the scope's build and carried across a write
  (`MentionScan`). The next briefing reads only the blocks written since. A write that can move
  a block it does not write into another doc (a delete or a move of a block with children) drops
  the kept scans.
- `find_phrases` joins each window once per word, not once per window size.
- `Lexicon::mentions` does not read text when the lexicon has no names.
- Related decisions check each edge for a tie once (it was once per pair of edges).

In the fixture, 873 blocks outside the doc name "Doc 0" (1,070 hold both words). A cold scan
reads those 1,070 blocks. A carried scan reads the one block the write added.

M4 Pro figures (the `brief_doc` loop, 10k): the briefing after a write took 16.1 ms before and
0.6 ms after; `mentioned_by` took 15.3 ms before and 0.05 ms after. A cold lexicon build over
the same 10k view took 2 ms before and 8.7 ms after (the word index).

Whole-scope graph coverage (`rdy status`, `rdy_overview`) is still memoised per build and not
carried, so the first read after a write pays the full pass. It is not made incremental here.
A per-block delta must also handle entity name changes (they change the mentions of any block)
and edges into a doc from anywhere (they change its unlinked state).

Linux bench rows (enperf2): <filled from two cargo qa --only perf runs on the bench>

## M2f-a: the working copy (2026-10-07)

The `qa-perf` typical tier gained two ops in `tier_wc`. It runs after `tier_code` and before
`tier_render`. No laptop figures are recorded here.

| op | what it times | runs | target |
|---|---|---|---|
| `wc_project_typical` | `ReadState::wc_project` over canon, the whole tree: every doc and folder as file text, with the block table | 10 | recorded, no spec budget (M2f-a): the working copy |
| `wc_plan_one_doc` | one staged doc file, planned for commit: `stage_put` then `plan_commit`. The checkout is written once into a temporary directory, outside the timer. Each run first edits the first body of one doc file (outside the timer) | 10 | recorded, no spec budget (M2f-a): the working copy |

`wc_plan_one_doc` grows with the size of the checkout, not of the commit: the planner builds the
commit tree from the base files and the staged copies, then lints and diffs the whole tree.

Baseline: `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>

## Coverage and confidence (2026-10)

Coverage has two layers. The structural layer holds the Grounded, Connected, Complete and
Governed checks of each scored block. The state layer adds the dismissed mentions, the tensions
and the suspects. Before this work, the structural layer was memoised per build and never
carried, so the first coverage read after a write paid a full pass over the scope (about 105 ms
at 10k blocks). The write response carries the coverage change (D-C6HHV), so every write would
pay it.

The structural layer is now carried across a write (`coverage::carry`, called from
`Built::after`) when the graph, the lexicon and the layer are built. The carry recomputes only
the blocks in the dirty set, each with the same per-block function a full build uses
(`entry_of`). The other blocks keep their entries.

The dirty set (closed):

1. every block an op touches, its parent, and its doc, before and after the ops;
2. every target of an outgoing edge of a touched block, old or new, and that target's doc;
3. for a touched block that appears, goes, changes type or (a decision) changes status: every
   source of an incoming edge to it in the new graph (refs that dangle or stop dangling;
   decisions applied or not; questions answered; decisions that become live or stop being
   live). A body or other field edit changes nothing a pointing block reads of its target, so
   it adds none of them (final review I1);
4. for a touched decision whose status or whose `affects`, `supersedes` and `revokes` edges
   change (or that appears or goes): the targets of its `affects` edges, old and new, and the
   sources of their incoming `describes` edges; the targets of its `supersedes` and `revokes`
   edges, with the sources of their incoming `decided_by` and `implements` edges, the targets
   of their `affects` edges and the sources of those targets' incoming `describes` edges. A
   statement edit adds none of them (final review I1);
5. for a touched entity that starts or stops being deprecated (the deprecated reference rule): every
   block whose text names it. An unlinked mention of a deprecated entity is no Grounded check
   (final review I4).

"Its doc" is every doc above the block, so a doc nested in a doc is reached too. A deleted
block leaves the map. The entity-to-blocks map drops each touched block from the entities its
old text named and adds it back under those its new text names.

The carry gives up, and the next read rebuilds, when an op:

- creates, deletes or renames an entity (its raw `name` or `aliases` field changes): every
  block's mentions can change, and an unlinked mention's fix names the raw name, so a rename
  that keeps the phrase ("Ledger" to "LEDGER") gives up too;
- changes the `lifecycle` of a block that has children, or moves a block that has children;
- deletes a block and leaves children under it;
- changes a doc's `doc_type`;
- creates, deletes, renames or moves a doc or a folder: the paths of other docs can change, and
  an unlinked-doc issue names its path.

The carry also does not run when the graph or the lexicon of the build was not carried.

Equality with a rebuild is pinned by `cc11_carried_equals_rebuilt_over_random_writes` (300
random writes, among them body edits of the term and of proposed decisions and moves of the
term in and out of a deprecated section), `cc11_dangling_refs_and_landed_decisions_carry_exactly`,
`cc11_a_rename_that_keeps_the_phrase_equals_a_rebuild` and
`cc11_tree_writes_and_doc_type_changes_rebuild` (a folder rename, a move of a section with
children, a `doc_type` change, a doc created and a doc moved). The counter `structural_builds`
pins that a plain write builds nothing in full and that those writes give up.

The `qa-perf` typical tier gained two ops in `tier_entities`, after `coverage_all`, on view
`ve`:

| op | what it times | runs | target |
|---|---|---|---|
| `coverage_carry` | What a write reply reads for its coverage after one single-claim write: the carry of the structural layer, then `coverage_change(ve → ve)` for the written blocks, as `write_and_report` calls it: the score on the state before (memoised), the score on the state after (its coverage issues carried from the state before), and the written doc's page rollup before and after. Outside the timer: the view's score on the state before the write (the previous reply read it), the write, and its ops read back. Timed: `coverage::carry_for_perf`, the call `Built::after` makes (`cc11_the_perf_hook_carries_as_a_write_does` pins that they give the same layer), and `coverage_change` | `runs.write` | < 5 ms p95: a write reply's coverage change after one write: the carry, the scores before and after, the touched page's rollups (FW-PF2; reference/performance tb-XBXR4 read budget) |
| `coverage_report_cold` | `coverage_report(ve)` after the engine caches are dropped (outside the timer): the materialization, the graph, the lexicon, the structural layer and the state layer | `runs.read` | no spec target: the cold coverage report over a 10k view, recorded (FW-PF2; rdy coverage) |

`coverage_carry` grows with the dirty set (the written blocks, their neighbours and their docs),
with the tensions and suspects the write changed, with the scope's rollup after the write, which
reads every scored block, and with the blocks of the written doc's page, read twice for its
rollups. The write goes to the fixture's first doc, which this tier fills with 250 more blocks,
so the doc's `linked_from_outside` walk of its whole subtree is in the timed path.
It rebuilds neither the structural layer nor the state layer: the coverage issues are carried
from the state before (fix round 2), and only the entries of the blocks whose structural entry,
counted tensions or suspects changed are rebuilt. It counts no entity kinds: the kinds are a
summary's (`rdy status`, `rdy_overview`), and a write reply reads only the rollup (fix round 3;
`kinds_builds` pins it in the op and in
`cc11_fix3_a_write_replys_coverage_change_counts_no_kinds`). `rdy coverage` (the full report) is
`coverage_report_cold`'s, not a write reply's.

No laptop figures are recorded here: the first timing of these ops is the Linux bench's.

Baseline: `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>


### Final review fix round 1: hub entities and the canon drift history

**Hub entities (I1).** Before this round a body edit of an entity that N blocks describe made
all N describers dirty (rule 3 took every incoming source), and each describer's Governed check
scanned the entity's N incoming edges again, so the carry cost O(N²) on the writer. Measured on
the final review's MCP probe: 22, 45 and 130 ms per hub edit at 2,000, 4,000 and 8,000
describers. Two changes:

- rule 3 and rule 4 take the pointing blocks only when what they read can change (above);
- whether an entity has a live incoming `affects` is read once per build or carry and kept per
  entity (`EntryCx::affected_live`), so a cold build is linear in the edges too.

`cc11_a_body_edit_of_a_hub_recomputes_o1_entries` pins the count (the test hook
`carried_entries`): a body edit of a term that 1,000 claims describe, and a statement edit of a
proposed decision that affects it, each recompute at most 4 entries, and the landing that
accepts the decision still reaches every describer. `cc11_a_cold_build_reads_a_hubs_governance_once`
pins one scan of the hub per build (`governed_scans`).

Mac figures (indicative, release, in process; the write and the coverage read after it): 5.8,
7.0 and 10.4 ms at 2,000, 4,000 and 8,000 describers; a cold structural build 6.0, 10.4 and
25.3 ms.

**The canon form of restatement drift against history (I2).** Each canon build walked back
through every version of every edited long block down to the oldest newest-landing among the
long blocks, reading each version's step (the change that wrote it, its landing, its base) from
the store with a memo that lived for one build. The cost was one store step per version, not
per block, so it grew with the project's history on every landing reply. Measured on the final
review's engine probe: 400 blocks with unique text went from 4.3 to 50 ms over 60 landings.

The steps are now kept across canon builds (`history::StepCache`, shared by every state one
handle publishes, like `vpos`). A version's step is the change chosen among its producers: of
those landed at or before canon's head, the one landed last. History is append-only, so a later
landing changes the step only of a version that one of its changes writes again (a revert). The
cache holds the steps for one canon head; on a newer head it drops the steps of every version
the landings since then write (walking back at most 64 landings), and on any other head it
starts again empty. Only a step whose change has landed is kept, and a step whose change the
walk has already taken (a block written back to an earlier text) is read from the store. The
cache is bounded at 2^18 steps and cleared by a reindex.

`cc7_a_canon_build_after_a_landing_reads_only_the_new_versions` pins the reads (the test hook
`canon_step_reads`): after a warm build, one landing that edits 20 of 21 long blocks, 10
landings deep, costs at most 25 store steps (241 before).
`cc7_kept_canon_steps_equal_a_fresh_read_through_a_revert` pins that a revert drops the kept
step: a fresh handle reads the same drift.

Mac figures (indicative, release; the first canon build after each landing, the final review's
shape): 400 blocks with unique text 3.9 ms after 1 landing, 4.7 ms after 30, 5.9 ms after 60;
400 blocks with shared phrases 5.3, 6.3 and 7.3 ms. The walk is still one in-memory step per
version, so a small linear term in history depth stays.

Two `qa-perf` ops cover these shapes, each on a project of its own:

| op | what it times | runs | target |
|---|---|---|---|
| `coverage_carry_hub` | As `coverage_carry`, after a body edit of a term that 2,000 claims in a stable section describe (each governed through it by a live decision): `carry_for_perf` and `coverage_change` for the written block; it asserts the carry recomputed at most 8 entries | `runs.write` | < 5 ms p95: a write reply's coverage change after a body edit of a term 2,000 claims describe (FW-PF2; tb-XBXR4 read budget; final review I1) |
| `tensions_canon_drift_deep` | The canon-scope build of the tensions after one more edit landing (untimed) of `n / 2` prose blocks in one controlled-English pattern, 50 edit landings deep, with one long block that never changes (it keeps every edited block's history in reach); it asserts the build read at most two store steps per edited block | `runs.materialize_cold` | no spec target: the D-SES4J budget check of the canon form against history; compare with `tensions_canon_drift`. If the bench shows the canon form over its budget, the founder's D-SES4J fallback applies (drop the canon form; keep the in-flight and landing forms) |

`tension_value_1k` and `tension_drift_1k` time the tension delta and its `on_change` report,
not the whole write; their target text now says so (final review M6).

No laptop figures are judged here: the first timing of these ops is the Linux bench's.

Baseline: `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>

## M2e1: semantic tensions (2026-10-11)

The `qa-perf` tier gained `tier_reviews`, run right after `tier_tensions` in both tiers, on the
same project: the 1,000 in-flight changes in 20 views (`inf0` to `inf19`), the citing claims, and
the tension set `tier_tensions` leaves behind. The writer is `human:qa`. The reviewer is
`agent:perf-reviewer` with the reviewer role. Each large-tier twin carries a `_50k` suffix. The op
names keep `1k` whatever the fixture's size.

Seven ops:

| op | what it times | runs | target |
|---|---|---|---|
| `review_requests_all_1k` | `review_requests()` on a state whose tension set is built (outside the timer): every request of every in-flight change, cold. Asserts at least one request | `runs.read` | recorded, no spec target: the worst case of a queue scan |
| `review_next_1k` | `next_request` for the reviewer, then `case_of` and `case_hash` of that request: the first `rdy_review_next` | `runs.read` | recorded, no spec target |
| `review_submit_lookup_1k` | `find_request` for a request no lease and no settled pair holds: the read-side lookup of a lease-less `rdy_review_submit`, which walks the queue. The daemon runs it on a read, outside the writer | `runs.read` | recorded, no spec target |
| `review_submit_1k` | `submit_verdict` with a `tension` verdict, one finding and `severity: blocking` with a reason, then the read of the new state's tension set (the write logged its moves). The request is leased to the reviewer outside the timer, as `rdy_review_next` leaves it, so the submit finds it from the lease; its case hash and the built tension set are also read outside the timer. Asserts that `full_builds()` did not move (the verdict carried the set) and that the state holds one more semantic tension | `runs.incremental` | < 10 ms p99 with 1k in-flight changes (the tension-detection budget of M2 spec 15.1, applied to a verdict, carry and logged moves included) |
| `review_submit_nolease_1k` | The writer part of a lease-less `rdy_review_submit`, as the daemon runs it (`run_review_submit`): `submit_found` (it re-proves the pair on the writer; its write logs the moves), then the read of the tension set. The request is found with `find_request` outside the timer (`review_submit_lookup_1k` times that). Same asserts as `review_submit_1k` | `runs.incremental` | < 10 ms p99 with 1k in-flight changes (the `review_submit_1k` target; task-7 ruling) |
| `review_accept_1k` | `accept_advice` as `human:qa` with no relay, for the one tension a `review_submit_1k` run opened, then the read of the tension set (the write logged its moves). The view of the advised change is proposed outside the timer. Asserts that `full_builds()` did not move and that the accepted tension blocks | `runs.incremental` | recorded, no spec target: a person's acceptance of one advised tension of a proposal |
| `review_requests_sparse_area` | `review_requests()` on a project of its own, with the `rdy-qa` pack's areas: three docs under `payments` hold one claim each, 100 docs of 20 claims (400 in the large tier) are in no area, and 50 changes (100) in views of their own edit the three claims. The area holds fewer than 20 blocks, so the area rule walks every doc for each edited claim | `runs.read` | recorded, no spec target: the area rule's walk over every doc (final review M5) |

What each cost grows with:

- The semantic detector costs nothing measurable when the project holds no records. It returns at
  once, so `tension_carry_1k`, `tension_on_change_1k`, `tensions_full_1k` and `tensions_canon`
  stay where they were. With records it reads them by item (a block at a content, then the
  other item), only at the contents the blocks hold now: on the writer's pre-publish build it is
  O(the live records on the seed blocks), and a full build reads only the records whose two
  texts some side still holds. Records on texts a block no longer holds are never visited
  (final ruling 3, FW-SW3).
- `review_requests_all_1k` and `review_requests_sparse_area` grow with the in-flight changes and
  the candidate pairs of each; the second also with the corpus, for each edited block of a
  sparse area. The area of each block is read once per queue walk. The daemon never computes
  requests on the writer; they run on reads only.
- `review_next_1k` reads the requests of a change only when the walk reaches that change, and
  stops at the first request the reviewer may take. It also builds the case file of that request
  from the in-flight state.
- `review_submit_lookup_1k` is the whole queue walk. It runs on a read, never on the writer.
- `review_submit_1k`, `review_submit_nolease_1k` and `review_accept_1k` pay for the verdict
  carry: the new tension set is carried from the state before, with the pair's tension
  re-detected around its blocks. None of them walks the queue on the writer. `review_accept_1k`
  also reads the landing form of the proposal's semantic tensions (the stored kinds only).

No laptop figures are judged here: the first timing of these ops is the Linux bench's.

Baseline: `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>

## M2e2: inbox (2026-10-12)

The `qa-perf` tier gained `tier_inbox`, run right after `tier_reviews` in both tiers, on the same
project: the 1,000 in-flight changes in 20 views, the citing claims and the tension set
`tier_tensions` leaves behind. The pack is `rdy-standard`, which declares no areas, so no landing
matches by area. Each large-tier twin carries a `_50k` suffix. The op names keep `1k` whatever
the fixture's size.

Outside every timer, the tier writes the subscription that `subscribe` writes for a new session
(`agent:perf-inbox`, run `perf-inbox`, cursor at the head, no areas, role `writer`). That agent
then commits 20 one-op changes in view `perf-inbox`: updates of 20 canon claims that the in-flight
views also update, so the log holds `tension_opened` moves on the session's changes. Agent
`agent:perf-other` then posts 200 intents on one block those changes write, so the log holds 200
`intent_posted` events. The 20 changes and the intents stay in the project; `tier_mcp` reopens the
same project after the tier.

Three ops:

| op | what it times | runs | target |
|---|---|---|---|
| `inbox_fetch_200_1k` | `inbox_page(&sub, None, usize::MAX)` on a fresh state: the run's in-flight changes, then the log walked from the cursor. Asserts a page of 200 entries and `more`, with the logged `tension_opened` entries among them | `runs.read` | < 5 ms p99 (M2 spec 15.1: inbox fetch, 200 entries), at 1k in-flight changes |
| `inbox_in_flight_1k` | `run_in_flight` for the session: its in-flight changes, their blocks, citations, views and areas. Every page read pays this before it matches. Asserts the 20 changes | `runs.read` | recorded, no spec target |
| `inbox_ack_1k` | `ack_inbox` with a cursor one batch further each run (the batch ends are read from the log outside the timer). Asserts that the cursor moved | `runs.incremental` | recorded, no spec target: one inbox ack, a ref write with its fsync |

What each cost grows with:

- `inbox_fetch_200_1k` grows with the entries on the page and the batches walked to fill it, plus
  one `run_in_flight`. A page reads each batch's events and logged moves from the log; it keeps no
  cache. If the bench shows it over target, the remedy is an in-memory replay cache keyed by batch
  `seq`, held by the engine and dropped on reindex. It is a follow-up the orchestrator places
  (ledger O10); it is not built here.
- `inbox_in_flight_1k` grows with the changes in flight, since it walks the set's writes and
  keeps those of the run, and with the views of the project, since it checks each view's owner.
- `inbox_ack_1k` is one ref write: an append, a sync and a publish. It does not grow with the
  project. It moves with the drive's sync cost, as the write ops do.

No laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops
are PASS-NEW until the baseline holds them. Because the tier leaves 20 changes and 200 intents in
the 1k project, compare the `tier_mcp` ops that reopen it with the baseline.

Baseline: `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>

## M2g: eight writers and the checker (2026-10-12)

The M2 targets are p95, not p99 (D-ANNKF). The `spec_target` text of every op now says p95, and
the pinned tests assert it. rdy does not measure p99.

Three ops. The first two run on the typical tier only; `render_cold` runs on both tiers
(`render_cold_50k` on the large one):

| op | what it times | runs | target |
|---|---|---|---|
| `daemon_write_8_clients` | one `rdy_write` of one claim, its durable flush included, through an `rdy mcp` proxy while seven other proxies write one claim after another. The daemon is the real one, started by the first proxy, over a copy of the typical project. The op asserts that each of the seven wrote | `runs.write` | recorded, no spec target: the write target of M2 spec 15.1 is for one writer. Compare with `daemon_write_single` |
| `invariants_check_all` | `rdy_qa::invariants::check_all` on the typical workload after the view ops, so canon and every open view are checked from one snapshot, then `fsck` and the `logged` rule (D-XH24M). A violation is printed, not asserted | `runs.read` | recorded, no spec target |
| `render_cold` | `render_site` of canon on a fresh handle each run, with the project closed: canon is read outside the timer, so the median holds the canon tension read that the disagreements page needs (FW-SW2). `render` renders from one state, which memoises that read in the first run only, so its median cannot show that cost (final review M2) | 5 (3 at 50k) | recorded, no spec target. Compare with `render` |

What each cost grows with:

- `daemon_write_8_clients` ends in a durable flush, so it moves with the drive, like
  `daemon_write_single`. Eight writers queue on one writer, so the median is about the flush cost
  times the writers in line. The bench's fsync cost is bimodal between runs (POST-LAUNCH-LOG #14,
  #18): record the higher median of the two runs.
- `invariants_check_all` grows with the blocks of canon and of each open view, since each scope is
  materialized and walked once.
- `render_cold` less `render` is about the canon tension read of the page (compare with
  `tensions_canon`). The page read the whole set, in-flight part included; it now reads
  `ReadState::canon_tensions`, which shares canon's memo and never detects the in-flight part.
  The test `sg9_a_canon_render_builds_no_inflight_tension_set` pins this with the engine's
  full-build counter.
- `render_50k` went from about 550-577 ms (dfffa8f and before) to 877-915 ms (3a4fcf7). `render`
  keeps its state across runs, so the tension read is in its first run only, not its median. The
  cost in every run was the page itself: the 50k tree has 970 all-canon duplicate definitions with
  about 50 blocks each (48,500 links). `Site::set_disagreements` sorted each entry's blocks by a
  key that walked the block's doc chain at every comparison (laptop: about 245 ms a render), and
  the links read each block's place and doc again (about 145 ms). The fix reads each block's
  place once per entry and memoises `Site::url_of` and `Site::doc_of` per render; the permalink
  pages reuse those places. Pinned by `set_disagreements_reads_each_blocks_doc_once` (a count).

No laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops
are PASS-NEW until the baseline holds them. `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>

## The live site (2026-10-13)

Five ops, for D-N5NSN and D-FV6G9 (ledger F4). A view updates live as the agent writes, so the
build and the fan-out are costs of the hot path, and the founder asked for the latency from a write
to the page's event (2026-10-12). Every op runs on both tiers (`_50k` on the large one). The three
daemon ops run on a daemon over a copy of the tier's project (their landings would move the
project the later ops open); the copy is removed at the end.

| op | what it times | runs | target |
|---|---|---|---|
| `site_build_live` | `rdy_render::live::build_live(&r.read(), &Scope::Canon, "/")` on the tier's project: the whole site built in memory, with each page's revision. Compare with `render`, which writes the same pages to disk | 5 (3 at 50k) | no spec target: the live site's full build (recorded; compare with render) |
| `site_event_after_land` | one subscription on the page of the doc the write edits, then a body edit of that doc written into a fresh view and landed through the writer. The time runs from just before the write to the subscription's `Reload` (the site worker builds and sends it). Each run waits for the worker to settle, outside the timer | `runs.write` | < 1 s p95 at 10k blocks; at 50k, the same budget (proposed, ledger F4; reported until the founder rules) |
| `site_event_after_view_write` | the same, with the subscription on the view's page and a commit into that view (no landing). Review mode, so the page carries the change marks | `runs.write` | < 1 s p95 at 10k blocks; at 50k, the same budget (proposed, ledger F4; reported until the founder rules) |
| `site_fanout_32` | the fan-out alone: two builds of canon on either side of a landing that changes one page, made outside the timer, then `rdy_render::live::diff` of the two and one send per stream to 32 bounded queues, with the events the site sends (`Reload` for the changed page, the new label for the others). Built from the public functions, so no hook in the site. A tree of fewer than 32 pages has several streams on a page. The op asserts that the diff is not empty and that the edited page got `Reload` | `runs.view_ops` | no spec target: the fan-out to 32 open pages (recorded) |
| `site_get_page` | `rdy_qa::http::get` of the longest doc page, from a warm build, over the daemon's loopback port. The op asserts status 200 | 50 | < 5 ms p95 (proposed, ledger F4; reported) |

What each cost grows with:

- `site_build_live` grows with the pages (one per doc and folder, the generated pages and the
  permalink pages), as `render` does, but writes no file. It is the cost of one build on the site
  worker, which a landing or a view commit starts only while a page of that scope is open.
- `site_event_after_land` is the landing (it ends in durable flushes, so it moves with the drive,
  like `daemon_write_single`), plus the build of canon, plus the diff of the two builds. It grows
  with the pages. `site_event_after_view_write` is a commit (one flush) plus the build of the
  view, which reads the review base besides.
- `site_fanout_32` is the diff of the two builds plus one send per open stream. It grows with the
  changed pages times the streams, and with the pages for the diff itself. It is a cost
  worth a look only if it comes close to `site_build_live`.
- `site_get_page` is the request head, the guard checks, the page bytes with the live script
  added, and a loopback round trip. It does not grow with the project, but with the length of the
  page.

Targets: none of the five is in `spec_ceiling_ms`, so none is enforced. The founder rules F4
after two bench runs agree on write-to-event and page-GET times (D-FV6G9); until then the targets
are proposed and reported. Like every target, they are the p95 of the timed runs (D-ANNKF): the
bench records the median and the p95 of each op, and rdy does not measure p99. The ledger
proposed these two as p99; the decision wins. The values are the ledger's proposal and stay the
founder's to rule (F4). The `_50k` event ops have a text of their own: the 10k-block budget,
applied at 50k blocks. `sgperf1_no_target_says_p99` holds every op to this, the site ops too.

No laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops
are PASS-NEW until the baseline holds them. `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>

## Site review (2026-10-14)

Three ops, for D-W40KZ and D-2F29G. A diff joins every render that has landings since the tag, and the
versions page counts every open view and proposal. These ops give the per-group checks their own numbers,
to explain a move in `render`. All three run on the 10k tier only: the 50k tier has no tag, so it has
no diff since a tag (F3). None is a spec ceiling.

| op | what it times | runs | target |
|---|---|---|---|
| `diff_since` | `r.read().diff(&Scope::At(<the v0.1.0 landing>), &Scope::Canon)` on the tier's project: the block and word diff from the tag to canon, which `render` builds among its pages. Placed right after `render` | 5 | no spec target: the block and word diff since the tier's tag (recorded; render builds it) |
| `site_versions_page` | `rdy_render::menu::build_versions` with a count of the docs each open view and proposal changes, and no cache. It runs after the view ops, so every open view of the tier is counted | 5 | no spec target: the live versions page with every open view and proposal counted, cold (recorded) |
| `site_versions_page_warm` | the same build with a cache filled by one build outside the timer, after one commit to one view (outside the timer). Only that view is recounted | 5 | no spec target: the versions page after one view commit, one view recounted (recorded) |


## Live profiles (2026-10-21)

Five ops, reported with no target and no spec ceiling (D-CWC91, D-DK03K; quality gate tb-A4DAV).
They run on both tiers (`_50k` on the large one), right after the live site ops. The ops measure
what choosing a profile adds to the live site: the build of a selected canon, the build of a
selected review, the versions page with its profile rows, the serving of a profile-only page and
the event after a landing. Everything is made outside every timer, on a copy of the closed
tier project: two public profiles in one landing (`perf-prs-in` selects the doc that the site
ops edit and the first half of the other docs in sorted id order, rounded up and capped at
1,024; `perf-prs-out` selects the same number of the other docs), then a view that changes the
doc's body, adds a decision under its section and swaps one doc of `perf-prs-in` for another, proposed and left open. The copy's
handle, the warmed states, the daemon, the profile-only listener, its first build and the page
stream are also made outside the timers. The copy is removed at the end.

| op | what it times | runs | target |
|---|---|---|---|
| `site_profile_build` | `build_profile_live` of `perf-prs-in` in canon with a fresh `ProfileHistoryMemo` each run: the selection, the reference check, every selected page, the page bytes and the history chip. The state is warm | 5 (3 at 50k) | no spec target: build a stored public profile of canon live, check and page bytes included (reported) |
| `site_profile_review` | the same in the open proposal, with a fresh memo each run: the profile in the proposal and in its base, both selected sides compared, the changed and the removed pages. The proposal's state is warm | 5 (3 at 50k) | no spec target: build a proposal's profile with a changed selection and body, old and new selected sides compared (reported) |
| `site_profile_menu` | `build_versions_for` the menu seen from the proposal and the profile: the bounded Profiles list with the selected doc count of each row, and the count of docs each proposal and view changes. The counts of proposals and views are the full-source counts; one build fills their cache outside the timer, as the site's cache holds them | 5 (3 at 50k) | no spec target: build the versions page seen from a proposal and a profile, profile rows and cached source counts included (reported) |
| `site_profile_get_page` | `rdy_qa::http::get` of the edited doc's page on the profile-only listener, over its loopback port. The listener, its first build and one request are outside the timer. The op asserts status 200 | 50 | no spec target: GET a warm profile-only page (reported) |
| `site_profile_event_after_land` | a body edit of that doc, written into a fresh view and landed through the writer, with one stream open on the page of the profile-only target. The time starts when the landing returns, which is after the writer has sealed, built, published and replied (the writer-completion boundary; `site_event_after_land` times from before the write, so the two are not comparable), and stops at the stream's `Reload`, which the site worker sends. The write and the writer's own work are outside the sample. Each run waits for the worker to settle, outside the timer | `runs.write` | no spec target: from the return of a landing through the writer to the profile page's reload event (reported) |

The daemon keeps a `ProfileHistoryMemo` for each profile target it serves, so the GET and event
ops build with the memo of the listener's own target, while the two cold builds start from an
empty one. The event is received from the existing stream, never after a sleep.

Three fixture facts are printed on standard error with the timings, one line for each tier
(`site_profile` and `site_profile_50k`): the docs `perf-prs-in` selects in canon, the bytes of
the files of its cold canon build, and the blocks the selected old and new sides of the proposal
differ in as the review counts them: the renderer's own `changed_blocks` of the selected old and
new sides, read from the cold review build's label (its header chip must show the same number).
The proposal also adds a decision under the edited section, which the review never counts, so
the block diff's raw entries are one more than the figure and the helper asserts the figure is
below them. The checks of the tiny harness fixtures read these facts and the names; they assert no duration.

What each cost grows with:

- `site_profile_build` grows with the selected docs and their blocks, the reference rows of the
  selection and the landings since the tag (the memo evaluates each once).
- `site_profile_review` is one selected build plus the profile resolved in the base and the
  comparison of the two selected states, so it grows with the selection on both sides and with
  the changed blocks.
- `site_profile_menu` grows with the number of profiles (capped by the list's bound) and with
  the selected docs counted for each row. The source counts are cached, so open views and
  proposals do not show here.
- `site_profile_get_page` does not grow with the project, but with the length of the page.
- `site_profile_event_after_land` is the build of the profile target after a publish plus the
  diff of the two builds. The landing itself is outside this time (compare with
  `site_event_after_land`, which includes it).

Nothing here sets a ceiling: no machine class is added and `qa-baseline.json` is not changed. A
baseline step records these ops only after two bench runs agree within 10%.

No laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops
are PASS-NEW until the baseline holds them.

Deep gate (bench): PASS-PERF-INFO at cd01f92 (2026-10-21; merged b44c7cc, v0.14.0): no FAIL row; dump +78%, site_event_after_view_write +42%, site_event_after_view_write_50k +28% (1.06 s against the proposed 1 s), all under 2x.


## Dump and load (2026-10-14)

Two ops, reported with no target. Both run on both tiers (`dump_50k` and `load_50k` on the large
one), right after `render_cold`, with the project closed:

| op | what it times | runs | target |
|---|---|---|---|
| `dump` | `ReadState::dump` of the whole project to a new file: the project files, then every sealed batch of the log. The handle is opened outside the timer. The size of the dump goes to stderr as `dump: N bytes` | 5 (3 at 50k) | recorded, no spec target: the whole project as one dump stream |
| `load` | `rdy_engine::load::load` of that dump into a new directory: the store rebuilt from the stream, the index build and `fsck` included. The loaded directory and the dump file are removed outside the timer | 5 (3 at 50k) | recorded, no spec target: rebuild from a dump, with the index build and fsck |

What each cost grows with:

- `dump` grows with the bytes of the log and the project files, read once and written once.
- `load` grows with the same bytes, plus the index build and `fsck` over the loaded store. Compare
  with `cold_open`, which also builds from the log, and with `export_import`.

No laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops
are PASS-NEW until the baseline holds them. `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>


## Dump chains (2026-10-21)

Three ops, reported with no target and no spec ceiling (quality gate k-CMC06). They run on both
tiers (`dump_increment_50k`, `load_chain_50k` and `load_chain_stop_50k` on the large one), right
after `load`, with the project closed. The cadence of shipping files is not measured here: the
ops time one export and two loads, never an interval.

All three work on one copy of the closed project, made and removed outside every timer. Per
tier, outside the timers: the copy is opened; one write creates the `chain` view; the full file
is written; then exactly 100 ordinary writes follow (100 new sealed batches). After the timed
exports, one more write and one approved removal of the tier's fixture claim (a block the full
file holds) land, and the second increment, which holds them, is written after the first. The
stop of `load_chain_stop` is the project's last landing before the copy was made, taken from the
copy before any write, never chosen by elapsed time. The removal comes after it, so a stopped
load finds the removed claim's payload in its retained part.

| op | what it times | runs | target |
|---|---|---|---|
| `dump_increment` | `ReadState::dump_after_file` of the full file: the predecessor's envelope and the walk that checks the marker of its end, the walk that finds the marker of the increment's own end, the project files, the 100 batches after it, and the write, the sync and the placement of the new file. The files of runs after the first are removed outside the timer; the first is kept as the first increment | 5 (3 at 50k) | no spec target: export 100 sealed batches after a full dump (reported) |
| `load_chain` | `rdy_engine::load::load_chain` of the full file and the two increments into a new directory, no stop: the pre-pass over every file, the ingest checks, the index build and `fsck`. The loaded directory is removed outside the timer | 5 (3 at 50k) | no spec target: validate and load a full dump and two increments (reported) |
| `load_chain_stop` | the same load with the stop above: the part after the stop is checked by the pre-pass and not written; the retained part is written with the carried records of the removal | 5 (3 at 50k) | no spec target: load a chain before removal with carried records (reported) |

Beside each op's timing, the run prints one line to stderr (from the first run of the op):

`<op>: bytes read N, bytes written N, source batches N, source frames N, carried spans N (N held)`

- Bytes read: for `dump_increment`, the bytes the export actually reads, in four parts: the
  project-file bytes; the bytes of the source log between the predecessor's end and the
  increment's end (segment files, framing included); the predecessor checks; and the walk that
  finds the marker of the increment's own end batch. The checks are the predecessor's envelope
  (the prelude and header, read twice, and the trailer; the body is not read) and the walk that
  finds the marker of its end. Each walk reads the segment of its end from offset zero up to the
  end, checking each frame and batch marker on the way. This is read volume, not source volume:
  the source volume of the increment is the log between the two ends, and both walks repeat on
  every export. For a load, the sum of the lengths of the three input
  files.
- Bytes written: for `dump_increment`, the length of the new file. For a load, the bytes of the
  segment files of the loaded store.
- Source batches and frames: what the new file holds, or what the load wrote into the store
  (the retained part when a stop is given).
- Carried spans: the spans of the carried records in the loaded store, and how many of them still
  hold their payload. Counted after the timer, from the loaded store, in the first run only. It is
  zero for an export and for a load without a stop.

What each cost grows with:

- `dump_increment` grows with the batches after the predecessor and with the project files. It
  also pays one walk over the predecessor's segment, from its start to the predecessor's end, and
  one over its own end's segment, from its start to that end, so its read cost grows with the
  offsets of the two ends inside their segments (at most one segment each, never the whole log). Compare with `dump`.
- `load_chain` grows as `load` does, plus one pre-pass over each file before the first write.
- `load_chain_stop` pays the same pre-pass over every file; it writes only the part up to the
  stop, plus the carried records. Compare it with `load_chain` for the cost of the files after
  the stop.

No correctness test asserts a duration, a faster-than-full result or a shipping interval. No
laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops are
PASS-NEW until the baseline holds them. `qa-baseline.json` is not changed here.

Deep gate (bench): PASS-PERF-INFO at 362c808 (2026-10-21; merged 5290e61, v0.13.0): no FAIL row; dump +76%, site_event_after_view_write +39%, site_event_after_view_write_50k +29%, daemon_write_8_clients +393% (POST-LAUNCH 44).


## Publications (core) (2026-10-18)

Six ops, reported with no target and no spec ceiling (D-0VCQ5, D-5CVWB; quality gate k-CMC06).
They run on the typical tier only, right after `clone`. An org profile (the benchmark audience,
F4) selects the first half of the tier's doc ids in sorted order, rounded up and capped at 1,024.
It is made and landed in a copy of the closed project, and the copy's handle is opened and
warmed, outside every timer. `publication_diff` runs on a small fixture of its own: one folder
selected, two docs before and three after, the first doc's first prose block changed. Both
selections are resolved outside the timer, and each output is removed outside it.

| op | what it times | runs | target |
|---|---|---|---|
| `publication_resolve` | `publication::resolve` of the profile against the same warm `ReadState`: the selection, the induced graph, the digests and the reference check, not cached; no store is opened | 5 | no spec target: resolve a profile and check its references (reported) |
| `publication_diff` | `publication::disclosure_diff` of the two pre-resolved selections of the comparison fixture: the pure comparison only | 5 | no spec target: compare two resolved selections (reported) |
| `render_profile` | `render_profile_site` of the profile at a fresh path: resolution, check, model build, every page, and the final placement | 5 | no spec target: build and write a selected static site (reported) |
| `export_profile_md` | `print::export_profile` of the profile as a Markdown tree at a fresh path | 5 | no spec target: print selected docs as Markdown (reported) |
| `export_profile_html` | the same, as one HTML file | 5 | no spec target: print selected docs as HTML (reported) |
| `export_profile_pdf` | the same, laid out and written as one PDF file | 5 | no spec target: print selected docs as PDF (reported) |

What each cost grows with:

- `publication_resolve` grows with the retained blocks, the candidate graph edges and the
  reference occurrences in the selection, and with the attachment rows.
- `publication_diff` grows with the selected docs, folders, attachments and reference rows of
  the two selections. It reads no store.
- `render_profile` and the three exports grow with the selected blocks printed, plus one
  `publication_resolve`. The PDF also pays for the page layout and the font subsetting; Markdown
  pays for one file per page.

No laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops
are PASS-NEW until the baseline holds them. `qa-baseline.json` is not changed here.

Deep gate (bench): PASS-PERF-INFO at e0afcc3 (2026-10-18; merged 97479bc, v0.8.0): no FAIL row; render_cold +26%, site_event_after_view_write +33%, site_event_after_view_write_50k +30% (1.08 s against the proposed 1 s).


## Export and clone (2026-10-16)

Five ops, reported with no target (D-9QV85, D-WQSAY, O42). The three exports run on the typical
tier only, right after `dump` and `load`. `clone` runs on both tiers (`clone_50k` on the large
one), right after `dump`, with the project closed. The handle is opened outside every timer, and
each output is removed outside it.

| op | what it times | runs | target |
|---|---|---|---|
| `export_md` | `print::export` of canon as a Markdown tree at a new path | 5 | no spec target: the scope printed as md, written whole (reported) |
| `export_html` | the same, as one HTML file | 5 | no spec target: the scope printed as html, written whole (reported) |
| `export_pdf` | the same, laid out and written as one PDF file | 5 | no spec target: the scope laid out and written as PDF (reported) |
| `clone` | `rdy_engine::clone::clone` of the open project's state into a new directory: the dump stream, the load with its index build, the origin batch and `fsck` | 5 (3 at 50k) | no spec target: a clone of this project, with the index build, the origin and fsck (reported) |

What each cost grows with:

- The exports grow with the blocks printed. The PDF also pays for the page layout and the font
  subsetting; Markdown pays for one file per page.
- `clone` grows as `dump` plus `load` do, plus one origin batch.

No laptop figures are judged here: the first timing of these ops is the Linux bench's. The ops
are PASS-NEW until the baseline holds them. `qa-baseline.json` is not changed here.

Deep gate (bench): <orchestrator fills in>


## Decision intake (2026-10-15)

Two ops, reported with no target (D-JV6Z8, O33). Both run on both tiers (`pending_50k` and
`answer_later_50k` on the large one), right after the view ops. Two proposals with a summary are
made first, outside the timer, so the queue holds cards beside the proposals the view ops left
open. Neither is a spec ceiling.

| op | what it times | runs | target |
|---|---|---|---|
| `pending` | `ReadState::pending` for the project's person, with no chat session and a card budget of 2,000 tokens: the needs-you queue with every open proposal carded | 10 (5 at 50k) | no spec target: the needs-you queue with every open proposal carded (reported) |
| `answer_later` | `Rdy::answer` of the first item with option `later`, relayed by an agent session with the person's words: one `ANSWER` record in a batch of its own | 10 (5 at 50k) | no spec target: one relayed snooze, one ANSWER record (reported) |

What each cost grows with:

- `pending` grows with the number of open proposals, because each version is carded; with the
  decisions of each proposal, because the card lists each one and checks each for an exception; and
  with the advised tensions on each proposal, which the card shows beside its exceptions.
- `answer_later` grows with the same queue: it reads the queue to bind the answer to the item and its
  card hash, then writes one record and does one durable flush. It also reads the queue once more
  to name the next item.

No laptop figures are judged here, and `qa-baseline.json` is not changed (O33).

Deep gate (bench): <orchestrator fills in>

## Change plans and re-affirmation (2026-10-22)

Five ops, recorded with no spec target and no ceiling (D-4JMPH, D-EJBPD, D-01QX5; system/quality-gate
tb-A4DAV puts timing in perf, so no correctness test asserts a duration). They run on both tiers
(`_50k` on the large one), after the signature ops. No baseline changes here: `qa-baseline.json`
holds none of the ten names, and a rebaseline is the bench's.

Each tier works on one copy of its closed project, made and removed outside every timer. Outside
the timers, the copy gets four things:

- 300 claims that reference one claim A, so a view that changes A owes 300 confirm items;
- 300 accepted decisions that use one term T, so a view that changes T makes all 300 suspect;
- a small local Git checkout (one file, `src/m.rs`), linked from the copy only. Its first commit
  cites A, so a plan that changes A has one code item. No Git command reaches a network;
- 300 claims that reflect `src/*.rs`, landed after that commit, then one more commit that changes
  `src/m.rs`. All 300 reflects blocks are now suspect against the checkout, and they name one
  matched file.

Each writing sample starts from a view of its own, made and pinned outside the timer (a pinned
view is not rebased by the landings that follow). Setup, the answers that a seal needs, the
propose, the exception answers, cleanup and process startup are all outside the sample.

| op | what it times | runs | target |
|---|---|---|---|
| `plan_read` | `ReadState::change_plan` of a view that changes A, with the largest page: the candidates of the net change, the code evidence at the checkout's HEAD, the counts and the rows. The state is warm; one read is outside the timer | 5 (3 at 50k) | recorded, no spec target |
| `plan_answer_300` | `Rdy::answer_plan` with 300 confirm answers on a fresh view: the plan computed again, the request checked as a whole, one plan record, one ref update, one durable flush | `runs.write` | recorded, no spec target |
| `plan_seal` | `Rdy::land` of an approved version whose plan is bound and fully answered (no deferred item): the landing, and the seal of every item | `runs.write` | recorded, no spec target |
| `reaffirm_300` | one ordinary `write` with one bulk `reaffirm` op (`because` T) on a fresh view that changes T: the selection, the 300 metadata updates in one change, the durable flush | `runs.write` | recorded, no spec target |
| `reflects_suspects` | `ReadState::suspect_details` of canon: the structural rows, the deferral lookup and the 300 code rows against the linked checkout. The state is warm; one read is outside the timer | 5 (3 at 50k) | recorded, no spec target |

`plan_seal` does not use a deferred item, so it does not depend on the deferral card. `reaffirm_300`
uses the ordinary op, not the later queue card. `reflects_suspects` runs before the seal's landings
move canon, so canon's only suspects are the 300 code rows.

Beside the timings, the run prints one line to stderr for each tier (`plan ops` and `plan ops_50k`):

`plan ops: plan items N (code files N), answers N, sealed items N, re-affirmed decisions N, changed reflects blocks N (N matched file)`

At the typical size it reads 301 plan items (300 confirm and one code item), 1 code file, 300
answers, 300 or more sealed items, 300 re-affirmed decisions and 300 changed reflects blocks that
name 1 matched file.

What each cost grows with:

- Changed blocks. A plan compares the version's final state with its canon baseline and works
  on the net change, so intermediate edits cost nothing. `plan_read`, `plan_answer_300` and
  `plan_seal` all compute the whole plan first.
- Reached edges. Each changed block asks the consequence visitor for the blocks that reference
  it; a document item is one per reached (cause, subject) pair. 300 referrers of A give 300 items,
  and a block with more referrers gives more. `reaffirm_300` grows with the edges that make the
  decisions suspect, and with the decisions it writes (one update each, in one change).
- Plan answers. An answer request is checked in full before it is written. A cumulative plan
  keeps every earlier answer, so a view that answers in many calls pays for all of them in each
  call. `plan_seal` seals every item of the version, so it grows with the item count.
- Matched files. `reflects_suspects` compares the committed files that each glob matches with the
  snapshot a block recorded. It grows with the reflects blocks, their globs and the files in the
  committed tree at each linked HEAD. It does not read the worktree.
- Git history. The code evidence of `plan_read` reads the citations of the checkout's commits
  and, for each code item, the commits after the chain's first HEAD, so it grows with the history
  the checkout holds. The small fixture has two commits and does not show this; the code ops of
  an earlier section measure the history cost on their own checkout. `reflects_suspects` compares
  tree identities and does not walk history.

No correctness test asserts a duration. No laptop figures are judged here: the first timing of
these ops is the Linux bench's. The ops are PASS-NEW until the baseline holds them.

Deep gate (bench): <orchestrator fills in>

## Renderer: diagrams in a 10k-doc build (2026-10-24)

One measurement of the site build with native diagrams, Copy and the sidebar. It is a
measurement, not a gate: the result is "recorded once; no speed target or regression baseline."
`qa-baseline.json` is not changed, no machine class is added, and `compare` and the rebaseline
never see it.

Run it on the bench class only, once, on the final integrated commit:

```
cargo run -p rdy-qa --release --bin qa-perf -- --renderer-10k
```

`qa-perf` with no argument runs the gate workload as before. Any other argument list prints
`usage: qa-perf [--renderer-10k]` to stderr and exits 2. The pins are in `crates/rdy-qa/src/perf.rs`
(`dgm12_` tests), so `cargo qa` runs them.

| op | what it times | runs | target |
|---|---|---|---|
| `render_diagrams_10k` | `render_site` of canon into an empty output directory, with one `ReadState` captured before the timer: the complete durable operation, up to and including the placement of the output | 1 | none: recorded once |

Fixture (built with `testkit::generate_tree`, then landed, before the timer):

- 1000 folders with ten docs each: exactly 10,000 docs. Each doc has one section and three
  claims.
- Every tenth doc in tree order, starting with the first, also has one Mermaid diagram: exactly
  1000. The families cycle flowchart, class, sequence and state, 250 each. One label holds the
  doc number, so no two diagrams draw the same SVG.
- One shared figure blob. The first ten docs in tree order each hold a figure of it.
- No invalid or unsupported diagram. The labels are generic.

Timer boundary: a fresh render with the prepared read state and no earlier rendered-site cache.
The fixture build, the landing, the byte counts and the cleanup are outside it. The reported time
is that one duration; `median_ms` and `p95_ms` are both equal to it.

Before the final Measure JSON line on stdout, the run prints one stable line to stderr:

```
render_diagrams_10k: docs=<n> diagrams=<n> files=<n> bytes=<n> html_bytes=<n> copy_bytes=<n> svg_bytes=<n> sidebar_bytes=<n> copy_max_page_bytes=<n> sidebar_max_page_bytes=<n>
```

Every count is taken from the files in the output directory:

- `files` and `bytes`: every file, the `.rdy-site` marker included.
- `html_bytes`: the `.html` files.
- `copy_bytes`: the bytes inside each `<template data-copy-page hidden>` element as serialized
  in the HTML, entity escapes included and the template tags excluded. It is zero while a build
  has no Copy template.
- `svg_bytes`: the `.svg` files under `assets/diagrams/`.
- `sidebar_bytes`: the bytes of each sidebar `nav` element, its tags included.
- `copy_max_page_bytes` and `sidebar_max_page_bytes`: the largest single page's Copy payload and
  sidebar.

A second stderr line, `render_diagrams_10k first_doc: path=<doc path> copy_bytes=<n>
sidebar_bytes=<n>`, gives the same two counts for the first doc in tree order.

What the time grows with: the docs and pages written, the sidebar each page carries, the Copy
payload of each page, and one SVG drawing for each diagram. The figure blob is read once for the
build and written once.

No time is recorded here. The first timing is the bench's, taken on the final integrated commit.

Deep gate (bench): <orchestrator fills in>
