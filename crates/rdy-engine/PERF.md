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
