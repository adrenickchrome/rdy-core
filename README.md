# rdy

Version control for agent-aided design. Design content and the decisions behind it are stored as typed blocks in an append-only, content-addressed store. Agents write through MCP; humans read the rendered site and approve proposals into canon.

## Quick start

```bash
cargo install --path crates/rdy-cli
rdy init                      # creates .rdy/ with the rdy-standard pack
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

In M1, `rdy mcp` holds the project's store while it runs, so run the human commands (`rdy status`, `rdy propose`, `rdy approve`, ...) after the agent session ends. A shared multi-client daemon arrives in M2.

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

## Daemon

Each project has one daemon that holds its store; every `rdy` command is a client of it. The
first command run in a project starts it in the background (stderr shows `starting: ...` while
it opens the store and builds canon), and later commands reuse it.

```bash
rdy serve              # run the daemon in the foreground (never idles out)
rdy serve --detach     # start it in the background and return once it is ready
rdy serve --stop       # stop it ("no daemon running" if there is none)
```

Its files live in `.rdy/run/`: `rdy.sock` (the socket), `daemon.pid`, `daemon.log` (a
background daemon's output; an `E_DAEMON_START` error quotes its last lines) and `start.lock`
(held by the one client that starts the daemon). Two settings in `.rdy/config.yaml`, in seconds:

```yaml
serve:
  start_timeout: 30   # how long a command waits for a daemon to become ready
  idle_timeout: 600   # a background daemon with no connections exits after this long
```

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

View ownership: the first commit into a view records its owner (the actor and run that made it),
and `rdy status` shows each view's owner. An agent session may write only into a view it owns or
a new one; writing into another session's view (the human's `work`, or the same actor's view
from another run) is `E_VIEW_OWNER`, and the fix names the agent's own view.

## QA gate

`cargo qa` runs rdy's full regression and hardening gate: fmt, clippy, the workspace tests, the
`rdy-qa` hardening suite (model, store, schema, engine, io, mcp, cli, render) and a perf-regression
tier, and writes `target/qa/report.md`. Flags: `--deep` (before a milestone), `--only <area>`,
`--skip-perf`, `--rebaseline` (after an intended performance change; commit the baseline diff).

`--only perf` runs the release build and the perf tier only. `--only <area>` for any other area
keeps fmt, clippy, the workspace tests, the `rdy-qa` unit tests and the perf tier (unless
`--skip-perf`) and narrows only the hardening suite to that area's test files.
