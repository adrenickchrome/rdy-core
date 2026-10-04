# Index engine choice: redb or fjall

Decision (2026-10-03): **keep redb 4.** fjall 3 was not at least 2x faster on inserts, so the decision rule keeps redb. `index.rs` is unchanged and fjall stays a dev-dependency used only by the benchmark.

## Workload

`crates/rdy-store/benches/index_choice.rs` runs what rdy's index does:

- insert 100k entries (32-byte BLAKE3 hash -> 12-byte location), committed in batches of 10, each commit durable with fsync (redb: `Index::apply`, default immediate durability; fjall: `WriteBatch` with `PersistMode::SyncAll`);
- 100k point reads of those keys in the same pseudo-random order.

Run: `cargo bench -p rdy-store --bench index_choice` (criterion, 10 samples). The insert benchmark does 10,000 fsynced commits per iteration, so a run takes about 25 minutes.

## Result

Apple M4 Pro, macOS (Darwin 25.6), release build, fjall 3.1.12, redb 4.

| benchmark | redb | fjall | fjall vs redb |
|---|---|---|---|
| insert 100k, batches of 10 (fsync) | 53.06 s (52.56-53.52) | 45.43 s (45.13-45.77) | 1.17x faster |
| 100k point reads | 58.6 ms (57.7-59.7) | 52.5 ms (50.5-55.0) | 1.12x faster |

Insert time is 10,000 commits at about 4-5 ms each on both engines. That is the cost of one `F_FULLFSYNC` on this machine (measured separately at 4.1 ms), so the engines differ little on this workload; the fsync dominates. fjall's reads are about 12% quicker but that is not what the rule asks about.

## Rule applied

"Keep redb unless fjall is at least 2x faster on the insert benchmark and not slower on reads." fjall is 1.17x faster on inserts, below the 2x threshold, so redb stays. The `Index` API and its Task 7/8 tests are untouched.
