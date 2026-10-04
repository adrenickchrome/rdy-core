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
```

Claude Code MCP config:

```json
{ "mcpServers": { "rdy": { "command": "rdy", "args": ["mcp"], "cwd": "/path/to/project" } } }
```

## Concepts

- **Blocks**: typed, id-stable units (claims, terms, decisions, sections...). Every edit makes a new content-addressed version.
- **Views**: where writes happen. **Canon**: the accepted design, advanced only by landing a proposal.
- **Decisions**: immutable once accepted; superseded or revoked only by new decisions.
- **Schema packs**: the document skeleton and block types; `rdy-standard` is built in and replaceable.

Design spec: see `docs/superpowers/specs/2026-10-03-rdy-design.md` in the research folder.
