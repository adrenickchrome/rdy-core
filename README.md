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
