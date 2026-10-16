# rdy

rdy is a system for keeping decisions and documentation consistent while agents change them.

Agents should never change the accepted design blind. rdy computes. Agents write. People decide.

Documentation and the decisions behind it are stored as typed blocks in an append-only, content-addressed store. Agents read and write through MCP tools, with a briefing before they edit accepted content; people read the rendered site and approve proposals into canon. Local-first, deterministic, model-free, and built in Rust.

## Platforms

| Platform | Status |
|---|---|
| macOS (arm64) | Supported. The fast gate (`cargo qa --skip-perf --require-all`) passes. |
| Linux (x86_64, aarch64) | Supported. The fast gate passes on x86_64; aarch64 builds and runs its tests (one timing-bound test is being fixed). Perf baselines are Linux only. |
| Windows | Not supported yet: `rdy-store` and the daemon use Unix-only APIs, so the workspace does not compile. `rdy-model`, `rdy-schema` and `rdy-graph` build. Windows support is planned after M3. |
| Other Unix (the BSDs, illumos) | Untested. |

## Quick start

```bash
cargo install --path crates/rdy-cli
rdy init                      # creates .rdy/ and .rdy.yaml (pack rdy-standard)
rdy mcp                       # MCP server over stdio for your agent harness
rdy status
rdy propose --view run-1 --title "first pass"
rdy approve <proposal-id>     # lands it in canon; proposed decisions become accepted
rdy render --out site/
rdy new folder wallets            # folders group docs; paths work wherever ids do
rdy new doc wallets/transactions  # a doc built from its pack skeleton
rdy ls                            # the folder and doc tree
rdy check export/                 # lint exported (format 2) files
rdy tag v0.1.0 -m "first cut"  # freeze canon's head as a version (approver)
rdy tags                       # list versions
rdy show <id> --scope tag:v0.1.0   # read as of a version (also landing:<id>)
```

Agent sessions (`rdy mcp`) and the human commands share the project's daemon (see Daemon below), so `rdy status`, `rdy propose`, `rdy approve` and the rest work while an agent session runs.

A store written by a different `rdy` build is refused with `E_STORE_FORMAT` ("re-create it with this rdy").

Claude Code MCP config:

```json
{ "mcpServers": { "rdy": { "command": "rdy", "args": ["mcp", "--project", "/path/to/project"] } } }
```

or:

```bash
claude mcp add rdy -- rdy mcp --project /path/to/project
```

## Concepts

- **Blocks**: typed, id-stable units (claims, terms, decisions, sections...). Every edit makes a new content-addressed version.
- **Views**: where writes happen. An agent session writes to a view named after its `run` (e.g. `run-1`), which is why the quick start proposes `--view run-1`; the CLI default view is `work`. **Canon**: the accepted design, advanced only by landing a proposal.
- **Decisions**: immutable once accepted; superseded or revoked only by new decisions.
- **Schema packs**: the document skeleton and block types; `rdy-standard` is built in and replaceable.

## Working with agents

rdy computes. Agents write. People decide. The loop is: brief, write, consequences, propose, approved, land.

1. **Brief.** The agent calls `rdy_brief` for the blocks it will change. The briefing lists what governs them, what depends on them and what will go stale. It also lists open tensions and other changes in progress, and it gives a token.
2. **Write.** The agent writes to its own view and passes the token as `brief`. A write to stable content with no current token fails with `E_BRIEF_REQUIRED`. A token that is out of date fails with `E_BRIEF_STALE`.
3. **Consequences.** Every write, `rdy_view` include/exclude/undo and propose reply lists `consequences`: the blocks the change touches, why, and what to do (`update`, `confirm`, `reaffirm` or `resolve`). The agent answers each one before it proposes. The gate does not check the answers yet.
4. **Propose, approved, land.** The agent proposes the view. A person approves it, and the proposal lands in canon.

A **view** is a private place for one run's changes. Many views can be open at one time, and each lands on its own.

An **intent** (`rdy_intent`) tells other agents which area or blocks a run plans to change, and for how long.

An **alternative set** (`rdy_alternatives`) holds two or more views that answer one question. Landing one member closes the others.

A **tension** is a disagreement that rdy finds: between changes, between a change and canon, or inside canon. Some are found by rules (a fork, a duplicate definition, a value that differs). Others come from a reviewer's verdict on a pair of blocks (`rdy_review_next`, `rdy_review_submit`). Anyone who writes may dismiss an advisory tension. A blocking one needs a reviewer or a person of another actor than its authors. A blocking tension stops a landing that would bring all its sides into canon. A reviewer may advise that a semantic tension should block. That advice gates nothing until a person accepts it.

`rdy_coverage` scores each block on six dimensions and lists each failed check with its fix. Fact sheets, search hits and briefings carry a confidence level (high, medium or low) with its reasons; confidence never blocks.

The **inbox** tells an agent what changed around its changes: `rdy_inbox` reads it, and the `rdy://inbox` resource sends best-effort notifications. `rdy_inbox {person: true}` reads the person's inbox so the agent can show it in chat.

Chat comes first. A person works through their agent, and the agent calls the tools for them. The CLI is a fallback. No tool approves yet: the agent relays the person's answer with `rdy approve <P-x@n> --relayed-by <agent> --evidence <their words> --question <what you asked>`. `rdy_schema` has one key for each topic: `edit_loop`, `brief`, `consequences`, `views`, `intents`, `alternatives`, `tensions`, `reviews` and `inbox`.

## Daemon

Each project has one daemon that holds its store; every `rdy` command is a client of it. The
first command run in a project starts it in the background (stderr shows `starting: ...` while
it opens the store and builds canon), and later commands reuse it.

```bash
rdy serve              # run the daemon in the foreground (never idles out; Ctrl-C stops it cleanly)
rdy serve --detach     # start it in the background and return once it is ready
rdy serve --stop       # stop it ("no daemon running" if there is none)
```

Its files live in `.rdy/run/`: `rdy.sock` (the socket), `daemon.pid`, `daemon.log` (a
background daemon's output; an `E_DAEMON_START` error quotes its last lines) and `start.lock`
(held by the one client that starts the daemon). When `.rdy/run/rdy.sock` is too long for the
system, the socket is `/tmp/rdy-<uid>/<hash>.sock` instead, in a directory only you can read.
The daemon writes its socket's path to `.rdy/run/daemon.sock`, and clients connect there, so
every spelling of the project's path reaches one daemon. The record also names the project's
`.rdy` directory, so a copy of the project never talks to the original's daemon.
Two settings in `.rdy.yaml`, in seconds:

```yaml
serve:
  start_timeout: 30   # how long a command waits for a daemon to become ready
  idle_timeout: 600   # a background daemon with no connections exits after this long
```

The same file links the code that implements the docs: `code_repos: [<path to a git checkout's
top folder>, …]` (absolute or relative to the project; full clones; at most 16).
`rdy brief --code <file>` reads their `Implements:` trailers with `git log`.

`--no-daemon` (or `RDY_NO_DAEMON=1`) opens the store in the command's own process instead. Only
one process can hold a store: a `--no-daemon` command while the daemon runs fails with
`E_LOCKED` ("drop --no-daemon, or stop the daemon with rdy serve --stop"), and a daemon cannot
start while a `--no-daemon` command holds the store. After rebuilding rdy, run
`rdy serve --stop` so the next command starts the new build's daemon.

Who is asking: the human actor is `actor:` in `~/.config/rdy/config.yaml` (or
`$XDG_CONFIG_HOME/rdy/config.yaml`), else `human:$USER`. A command run with no `RDY_ACTOR`, or
with the human's own, is the human; its writes default to the view `work` and it may approve,
tag and import. An agent sets `RDY_ACTOR` (and optionally `RDY_RUN`): its writes default to its
run's view, else `work-<actor>` (`:` becomes `-`, e.g. `work-agent-x`), and approving, tagging
and importing are refused with `E_PERMISSION`.

rdy checks the user config as it checks the project's. If the file does not parse, is not a
mapping of settings, or gives a known key a value of the wrong type, every command in every
project fails with `E_CONFIG`, and the error names the file. rdy never skips the file, because
that would change who the human is. A misspelt key, or a key that belongs in another config file,
is a `W_CONFIG_UNKNOWN` warning that names the file. rdy ignores that key and keeps going.

View ownership: the first commit into a view records its owner (the actor and run that made it),
and `rdy status` shows each view's owner. An agent session may write only into a view it owns or
a new one; writing into another session's view (the human's `work`, or the same actor's view
from another run) is `E_VIEW_OWNER`, and the fix names the agent's own view.

## Working copy

For a person, a view can be a folder of YAML files. You edit them in your own editor. rdy puts
your edits back into the view as one change. Agents do not use the files: they read and write
through the tools (the MCP server and the commands above).

```bash
rdy checkout [<view>]        # write the view as files and bind this folder to it (default: work)
rdy status                   # what changed in the files, what is staged, what is in conflict
rdy add notes.yaml guides/   # stage files or folders (a deleted file stages its deletion)
rdy commit -m "reword setup" # record the staged files as one change in the bound view
rdy update                   # bring in changes made elsewhere; edits of yours stay
rdy brief guides/intro.yaml  # the briefing for a file, a folder or a block
rdy check                    # lint the files against the bound view
rdy worktree add ../review work-review   # a second folder, bound to its own view
```

- **Files.** Each doc is `<path>.yaml`, a list of block entries. Each folder is
  `<path>/_folder.yaml`. A new entry needs no `id`: rdy gives it one at commit. The order of the
  entries in the file is the order of the blocks. Decisions and questions are not in the files;
  they live in the store, and so do the links to them (`decided_by`, `implements`). A commit
  keeps them.
- **Commit rewrites the files it commits in canonical form, and drops their YAML comments.**
  Keep notes in a block, not in a comment.
- **Briefing.** When a person commits a change to accepted content, rdy shows the briefing
  first, then commits. `rdy brief` shows it earlier.
- **Conflicts.** If a block changed in the view and in your file, `rdy update` keeps your file and
  writes the view's side next to it as `<name>.theirs.yaml`. Fix the file, delete the
  `.theirs.yaml` file, then run `rdy add` on the file.
- **New docs.** `rdy new doc` and `rdy new folder` also write the file into a folder bound to
  the same view. A file already at that path is never overwritten: rdy warns and leaves it.
- **Images.** Name an image file in the `blob` field of an entry whose type is an asset type
  (such as `figure`), as a path from the doc file's folder. Commit stores the image and copies
  it to `_blobs/<hash>.<ext>`. Checkout and update write the view's images into `_blobs/`
  first, so a checkout or update that is then refused can leave images there. Each name is the
  image's hash, so such a file is never wrong and the next checkout uses it.
- **Ignored files.** `.rdyignore` (a subset of the gitignore patterns) keeps files out of the working copy.
- **Where it lives.** The state is local, in `.rdy/wc/` (a worktree has its own). It is never
  part of the project's history.

## QA gate

`cargo qa` runs rdy's full regression and hardening gate: fmt, clippy, the workspace tests, the
`rdy-qa` hardening suite (model, store, schema, engine, io, mcp, cli, render) and a perf-regression
tier, and writes `target/qa/report.md`. Flags: `--deep` (before a milestone), `--only <area>`,
`--skip-perf`, `--rebaseline` (after an intended performance change; commit the baseline diff).

`--only perf` runs the release build and the perf tier only. `--only <area>` for any other area
keeps fmt, clippy, the workspace tests, the `rdy-qa` unit tests and the perf tier (unless
`--skip-perf`) and narrows only the hardening suite to that area's test files. The `swarm` area runs eight scripted agents and one person against one daemon, through MCP first. It checks for lost writes, unreported tensions, landings past the gate and inboxes that differ from the log. It also checks that every stable write cites a briefing.
