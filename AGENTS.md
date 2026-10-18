# AGENTS.md

## What rdy is

rdy is a system for keeping decisions and documentation consistent while agents change them.
Design decisions and the current-state docs live in the rdy-design project, in its own repository.
Read them through rdy's MCP tools.

## Crates

Each crate is in `crates/`.

- `rdy-model`: the core types, such as blocks, changes, proposals, decisions and the project tree.
- `rdy-store`: the append-only segment log and the derived index (`redb`).
- `rdy-schema`: schema packs, path globs and block validation.
- `rdy-graph`: the block graph and the coordination graph.
- `rdy-engine`: reads, writes, briefings, tensions, working copies and config.
- `rdy-adapter`: the in-process store handle and captured read snapshots for a host that embeds rdy core.
- `rdy-render`: renders a project as a static site.
- `rdy-mcp`: the MCP server and its tools.
- `rdy-daemon`: the per-project Unix socket that serves `ctl` and `mcp` connections.
- `rdy-cli`: the `rdy` binary.
- `rdy-qa`: the QA gate, the fixtures, the perf workload and the test binaries.

A crate depends only on crates listed before it.

- `rdy-model` depends on no rdy crate.
- `rdy-store`, `rdy-schema` and `rdy-graph` depend on `rdy-model`.
- `rdy-engine` depends on `rdy-model`, `rdy-schema`, `rdy-store` and `rdy-graph`.
- `rdy-adapter` depends on `rdy-model`, `rdy-store`, `rdy-schema` and `rdy-engine`.
- `rdy-render` depends on `rdy-model`, `rdy-engine`, `rdy-graph` and `rdy-schema`.
- `rdy-mcp` depends on `rdy-model`, `rdy-engine`, `rdy-schema` and `rdy-graph`.
- `rdy-daemon` depends on `rdy-engine`, `rdy-mcp`, `rdy-render` and `rdy-model`.
- `rdy-cli` depends on `rdy-model`, `rdy-engine`, `rdy-schema`, `rdy-render`, `rdy-mcp` and `rdy-daemon`.
- `rdy-qa` depends on `rdy-model`, `rdy-schema`, `rdy-store`, `rdy-engine`, `rdy-render`, `rdy-mcp` and `rdy-daemon`.

Do not add a dependency that points the other way.

## Persistence rules

- The log is the record. The index is derived from it and can always be rebuilt.
- Run `rdy reindex` to rebuild the index.
- Open also rebuilds an index that lost the store format ref.
- Fail closed. The store refuses a write that it cannot make durable.
- The store refuses a damaged log. It never repairs the log silently.
- Never lose the log. No code path deletes or rewrites acknowledged log data.
- Two paths cut the log, and both touch only unacknowledged bytes.
- Open quarantines a torn tail. It copies the bytes aside first.
- A failed write rolls back what it appended.
- `STORE_FORMAT` is in `crates/rdy-store/src/format.rs`. Change it only in a plan that says so.
- The store refuses a store in another format with `E_STORE_FORMAT`.
- Until rdy is published there is no migration. Fix forward.

## Inputs

- A mutation input refuses an unknown key. It names the nearest known key when one is close (`rdy_engine::did_you_mean`).
- A file that a person edits follows the same rule.
- A config file warns with `W_CONFIG_UNKNOWN` and keeps going.
- A new input type follows the same rules.

## Definition of done

- Run `cargo fmt --all` before each commit.
- Run `cargo clippy -p <crate> --all-targets -- -D warnings` for each crate you touch.
- Run the focused tests of the change.
- The gate is the authority. Run `cargo qa --skip-perf` for the fast gate. The deep gate, `cargo qa --deep`, runs on the bench at milestones.
- Perf runs only on the bench machine. Its machine class is the only class in `crates/rdy-qa/qa-baseline.json`.
- On any other machine, a perf run has no baseline and fails. Do not add a machine class to the file.
- Do not trust a summary of a run, because a summary can hide skips. Read the skip count.
- A merge records the base and the verified commit in its message.
- Write conventional commits, such as `fix(<area>): ...` and `perf(<area>): ...`.

## Tests

- Each rdy-qa area is one test binary, in `crates/rdy-qa/tests/<area>/`.
- Run one area with `cargo test -p rdy-qa --release --test <area> <filter>`.
- Build the release binary first with `cargo build --release -p rdy-cli`. `cargo qa` does this for you.
- Some tests need a prerequisite.
  - Node, for the render tests.
  - A non-root user, for the read-only directory tests.
  - git, for the code-index tests.
  - A case-insensitive filesystem, for three working-copy tests. On Linux these tests are ignored.
- `RDY_QA_REQUIRE_ALL=1` or `cargo qa --require-all` turns a missing prerequisite into a failure. Ignored tests do not count, so a passing `--require-all` run on Linux does not cover them.
- `RDY_QA_DEEP=1` runs the deep cases.
- The gate also takes `--only <area>`, `--skip-perf` and `--rebaseline`.
- Timing belongs in perf ops. Never assert time in a correctness test.
- Change a perf baseline only on the bench, with `--rebaseline`, after two runs that agree within 10%.
- CI (`.github/workflows/ci.yml`) runs the fast gate on `main`. It is a cheap check and not the merge authority.

## Shared engine modules

These files in `crates/rdy-engine/src/` carry rules that many features share:

- `gate.rs`
- `write.rs`
- `publish.rs`
- `read_state.rs`
- `brief.rs`
- `tensions/`

Only one change at a time touches them.
A change there names every pinned test it updates and says why.
