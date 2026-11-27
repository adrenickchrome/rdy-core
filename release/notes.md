## What you get

- **Docs and decisions as one project.** Write docs, requirements, terms and decisions as linked blocks, not loose files.
- **Agents work through MCP.** Claude Code, or any MCP client, reads and writes through rdy's tools, over stdio or a local HTTP endpoint. Each agent gets a briefing before it edits.
- **Instructions for agents.** rdy's own agent guide is built in. Keep your rules in rdy too, each with its reason. Every agent gets the rules it needs at the start of a task, and your own rules follow you to every project.
- **Warnings when work skips rdy.** Optional hooks warn when a commit changes decided code and cites no decision, or when an agent edits rdy's files directly. They never block.
- **Proposals and approvals.** Agent work lands in a view. You see the impact, then approve it or not.
- **Docs that match the release.** Record which release ships each decision. A public site then shows only what has shipped, and a release check catches content that is ahead of the build.
- **Find similar content.** Rank search hits by relevance, or find the blocks most like a block or a draft text. rdy scores the words and links that blocks share, with no model.
- **It catches drift.** When a decision changes, rdy marks every page that rests on it. It also flags a page that states a different number for a recorded value.
- **Linked projects.** Cite the decisions and instructions that another project delivers through rdyhub. Your project keeps the version it cites until a person adopts a newer one.
- **Bring your Markdown.** Import a folder of Markdown files as docs. A preview shows what maps, and what does not, before anything is written.
- **Share through a hub.** Push canon and views to a hub repo, pull them back, and clone a project from its hub URL. A clone opens at the canon tip and fetches the older history after it. Fork a project at one of its tags, and port landed changes from another project into a view.
- **Checks that you write.** A pack rule can test each block with a WebAssembly module, in a sandbox with fixed limits.
- **A live docs site.** `rdy site` gives you a local site that refreshes as changes land.
- **A README from your docs.** Export one doc as one Markdown file, with its images in a folder beside it.
- **Local and private.** Everything runs on your machine: no account, no hosted service, no model inside rdy. Only a push, pull or clone that you run talks to a hub.

## Install

To install from the archive instead:

**macOS (Apple silicon):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.59.0/rdy-0.59.0-aarch64-apple-darwin.tar.gz
tar -xzf rdy-0.59.0-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.59.0-aarch64-apple-darwin/rdy ~/.local/bin/
```

**Linux (x86_64):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.59.0/rdy-0.59.0-x86_64-unknown-linux-gnu.tar.gz
tar -xzf rdy-0.59.0-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.59.0-x86_64-unknown-linux-gnu/rdy ~/.local/bin/
```

**Windows (x86_64), in PowerShell:**

```powershell
Invoke-WebRequest -OutFile rdy-0.59.0-x86_64-pc-windows-msvc.tar.gz https://github.com/adrenickchrome/rdy-core/releases/download/v0.59.0/rdy-0.59.0-x86_64-pc-windows-msvc.tar.gz
tar.exe -xzf rdy-0.59.0-x86_64-pc-windows-msvc.tar.gz
New-Item -ItemType Directory -Force "$HOME\.local\bin" | Out-Null
Move-Item -Force rdy-0.59.0-x86_64-pc-windows-msvc\rdy.exe "$HOME\.local\bin\"
```

On macOS and Linux, if `~/.local/bin` is not on your PATH, add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile. On Windows, add `$HOME\.local\bin` to your PATH. Then check:

```sh
rdy --version
```

It prints `rdy 0.59.0`.

On macOS, if you downloaded the archive in a browser instead of with curl, macOS may block the binary. Run `xattr -d com.apple.quarantine ~/.local/bin/rdy` once.

**macOS (Apple silicon) and Linux (x86_64):**

```sh
curl -fsSL https://getrdy.dev/install.sh | sh
```

**Windows 11 (x64), in PowerShell:**

```powershell
irm https://getrdy.dev/install.ps1 | iex
```

Each installer checks the release signature and the archive's SHA-256 before it installs. It puts `rdy` in `~/.local/bin` for your user, and asks before it changes your PATH.

## Release key

This release is signed by rdy's machine release key, the same key that signed 0.46.0, 0.47.0, 0.48.0, 0.49.0, 0.50.0, 0.51.0, 0.52.0, 0.53.0, 0.54.0, 0.55.0, 0.56.0 and 0.57.0. To check the signature of your download, follow "Verify the download" in the README. The allowed-signers line for v0.59.0:

```text
rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8
```

Fingerprint: `SHA256:Tj8hfra0U07asaeSZkt1b5e1HGJ+w8vMzLrn+G6/+/M`.

Compare this line and fingerprint with the ones in the README at v0.59.0. Use the key only when both match.

## Try it: your first project

1. **Make a project.**

   ```sh
   rdy init ~/my-project && cd ~/my-project
   ```

2. **Set up rdy for your agents.** In the project folder, run:

   ```sh
   rdy setup
   ```

   It creates your own rules project, the meta project, if you have none. It also adds three lines at the top of `CLAUDE.md` and `AGENTS.md` that tell the agent to call `rdy_session`, then `rdy_instructions`, at the start of each task. That call gives the agent rdy's guide, so you download nothing.

3. **Connect your agent.** Run `rdy setup` before you connect the agent. Then connect it and start its reader or writer session. For Claude Code:

   ```sh
   claude mcp add rdy -- rdy mcp --project "$PWD"
   ```

   For another MCP client, add this server to its config (write the full path; most clients do not expand `~`):

   ```json
   { "mcpServers": { "rdy": { "command": "rdy", "args": ["mcp", "--project", "/home/you/my-project"] } } }
   ```

   If the client does not find `rdy`, give its full path as `command`, such as `/home/you/.local/bin/rdy`. On Windows, give the native path, such as `C:\Users\ana\my-project`, and write each backslash twice in JSON.

   A session lasts as long as one `rdy mcp` process, and keeps one actor and one run. To start another run, start a new `rdy mcp` process. On the same process, a second `rdy_session` with another actor or run gets `E_BAD_INPUT`. A client that starts a new process for each call must start a new session each time.

   The agent starts its session with `rdy_session`. The call takes `actor`, its own `agent:` actor such as `agent:claude`, `run`, a name for the work, and `role`, `reader` or `writer` (default `writer`). rdy's guide tells it to. Approving stays with you: an agent cannot start an approver session over MCP.

4. **Ask your agent for something real.** For example:

   > Start an rdy session in your own view, then read its overview. Help me design retention for an AI support assistant. Our intent is to keep only the customer data we need, for as short a time as we can, and be able to explain the choices. Start with a decision to delete raw conversations after 30 days and keep anonymised metrics for 2 years. Create the storage docs and link them to the decision. Propose the first design for my approval.

   The agent first loads its rules with `rdy_instructions`. Each ruleset has its own `kungfu` value and names the tools it covers. The agent passes each tool the value of the ruleset that covers it. A call without the value, or with an old one, still runs, with the warning `W_KUNGFU`. A write to content whose `lifecycle` is `stable` must cite a brief token, in the write's `brief`. A write to draft content needs none, so the first writes in a new project need no brief. `rdy_brief` takes block ids or a path, and gives its token in `base.token`. For a write, pass the briefing token as `brief` beside `ops`. A brief reply holds it in `base.token`; an `E_BRIEF_REQUIRED` reply holds it in `briefing.base.token`. A list of decision ids is not a briefing token. Without ids or a path, `rdy_brief` refuses with `E_BAD_INPUT`. When `rdy_decide` or `rdy_write` creates blocks, its result also gives a token for those blocks, in `briefing.token`.

   A doc type can ask for sections: a doc with `doc_type: overview` needs Summary and Goals. Create the doc and its required sections in one write. If you split them, expect `W_SKELETON_MISSING` until the sections exist. It is a warning, not a refusal.

   A write is `rdy_write {ops: [...]}`. Each op gives `op`, such as `create` or `update`, with the block's `type`, `id`, `parent`, `fields`, `body` and `links`. This example uses four block types: `folder`, `doc`, `section` and `prose`. A section's `title` goes in `fields`, and its parent is the doc. `implements` is not a link: it is a field of the write, next to `ops`, and names the decisions that the whole write carries out. A doc's `title` and `doc_type` go in `fields`. A link reads `"links": {"decided_by": [{"target": "D-<id>"}]}`.

   A doc holds no text of its own: its opening paragraph is a prose block inside it. So a doc takes no `description`, and a folder takes one in `fields`. To link a doc to the decision, the agent puts `decided_by` on a block inside the doc. A folder and a doc in it can go in one `rdy_write`: the agent gives the new folder an `id` that starts with `$`, such as `$storage`, and uses that name as the doc's `parent`. The agent records a decision with `rdy_decide`, which takes the decision's text as `statement`.

5. **Review and approve it yourself.**

   ```sh
   rdy pending
   ```

   This shows what waits for you, with the impact of each proposal. Approve with `rdy approve P-<id>`, or tell your agent your answer. Without `@<n>`, `rdy approve` takes the proposal's latest version, which the card shows as `P-<id>@<n>`. If you answer a card in chat with Later, it stays hidden until the chat session changes, and `rdy pending` counts it as snoozed. When a proposal adds a decision, its option reads "Approve P-\<id\> and confirm D-\<id\>": approving it also confirms the decision.

6. **Read the result.**

   ```sh
   rdy site
   ```

   Open the link. The home page links to the folders, and each folder's page links to its docs. Each page's address ends in `.html`. An agent reads the same docs with `rdy_read`, with no browser. Now ask your agent: “For EU customers, raw conversations must be deleted after 7 days. Keep the 30-day rule elsewhere and the 2-year metrics window.” The agent briefs on the decision and its connected docs, follows the consequences, and brings you the proposal to approve.

   A decision gets a card of its own only in some cases. For example, it supersedes or revokes a live decision, it is `one_way`, or no content applies it yet. If the agent refines the first decision and applies the new one in the docs, you get one card. `rdy_decide` takes the decisions that a new one refines as a list, in `refines`. Before it proposes, the agent answers each open item of the change plan with `rdy_plan {answers: [{item, act, reason}]}`. A `confirm` item takes `act: "confirm"`. An update of the block does not close the item. The answer closes it. The agent first updates the blocks whose text the new decision changes. Then it answers each `confirm` item, also for a block that it did not change.

   If `rdy pending` shows more than one card, answer each one, or tell your agent your answers. `rdy approve` names any card that it waits for.

   To port landed work from another local project or its dump, run `rdy view replay <target-view> --from <project-or-dump> --brief <target-brief-token> <source-change-or-landing>`. In chat use `rdy_view` with op `replay`, `view`, `from_file` naming the source dump, `brief` and `changes`. Read the result's conflicts and consequences before proposing.

   New content shows a **Planned** label. A new decision starts planned, so the content that cites it does too. Set `built` on the decision when the work ships.

   On a remote machine, the link is on `127.0.0.1`. Open an SSH tunnel to the port in the link first, for example `ssh -L <port>:127.0.0.1:<port> <host>`.

   The site keeps running after `rdy site` exits. To stop it, close your agent's session first: quit the agent, or end the `rdy mcp` process that your client started. `rdy mcp` ends when its standard input closes. Then run `rdy serve --stop` in the project folder. An `rdy mcp` that is still open logs `E_DAEMON_GONE`. `rdy serve --stop` prints `stopped`, or `no daemon running` when none was running. Your meta project has its own daemon, which starts at the agent's first `rdy_instructions` call. Stop it with `rdy serve --stop --project <path>`. Use the path that `rdy setup` printed when it created the project. By default, the path is `.rdy/meta` in your home folder, such as `~/.rdy/meta`.

## What changed since 0.57.0

### New features

- **A clone by URL is ready at the canon tip.** When the hub sends the canon state (sync stream format 5), `rdy clone <url>` and `rdy_clone` take the state of canon's tip first, and you can work at once. The CLI prints `ready: work in <dir> now; history arriving: <n> of <m> records`. Then the older history arrives, and rdy checks it as it comes. The CLI fetches it in the foreground and serves your other commands and MCP sessions while it does. `rdy_clone` gives the fetch to the project's daemon. A stop pauses the fetch, and the next `rdy pull` that names no view, or a daemon start, resumes it. From a hub that does not send the canon state, a clone takes the full history first, with the note `this hub sends history only; the clone waits for all of it`.
- **While the history arrives.** A call that needs the older history refuses with `E_HISTORY_PENDING`, "the project's history is still arriving (<n> of <m> records)". The fix is "wait for the clone to finish, or run rdy pull to resume it". These calls refuse: `rdy push`, a pull that names views, `rdy push --restore` and its preview, `rdy publish` to a hub site, `rdy dump`, a local clone or fork of the project, a read of a version or landing before the history that the project holds, and a create with an explicit id that the view does not hold, unless the project holds a deletion of that id. Reads that leave out the older history give the warning `W_HISTORY_PENDING`: `rdy status`, `rdy_context`, `rdy_brief`, `rdy_read` in compact form, export, tensions and the log. While the history is incomplete, `rdy_log` replies `{items, warnings}`, and its last row is the oldest landing that the project holds. `rdy fsck` prints `history: partial (<n> of <m>)`, and its JSON gives `history` with `complete`, `held` and `total`. `rdy status --json` gives the same with `frontier`.
- **Smaller stores on disk.** rdy stores an exact repeated run of payload bytes once on disk, and gives the space of each copy back to the file system. Records, hashes, dumps and the bytes that sync sends do not change. A pass runs after a write when at least 64 MiB of the log has been written since the last pass. A load, a clone or a take maps nothing, so a cloned or loaded store keeps its repeated bytes until `rdy compact`. `rdy compact` and `rdy_compact` report `deduplicated_bytes`, and `rdy fsck` prints `deduplicated bytes:`. Space comes back only where the file system can punch holes. Otherwise compact notes "hole punching is not supported for <n> mapped spans; they keep their disk space".
- **Smaller change plans.** A new version of a change plan stores only the answers that changed since the version before it, when that is smaller. After 128 such versions in a row, rdy stores a whole version again.
- **Rank search hits.** `rdy_search` with `sort: "relevance"` orders the hits inside each tier by their score against the query, and adds `score` to each hit. `sort: "reading"` names the default order. On the CLI: `rdy search --sort relevance`. The MCP server's instructions say to use it for ranked text search.
- **Find blocks like a block or a text.** `rdy_search` with `like` (a block id) or `like_text` (a text of at most 16,384 bytes) ranks the blocks most like it, from a score of 0.10, highest first. Each hit gives `score`, and up to five shared `words` and `links`. Give exactly one of `query`, `like` and `like_text`. The ranked modes read canon or a view, not a past landing or a proposal version. On the CLI: `rdy search --like <ID>` and `rdy search --like-text <TEXT>`.
- **Similar blocks as advice.** A briefing that covers at most 20 content blocks lists `vector_neighbors`: for each covered block, up to 2 other live content blocks whose words or links are close to its own, with each block shown once. A wider briefing lists none, and `vector_neighbors_skipped` counts its covered content blocks. A write or an import that creates content blocks lists up to 3 rows for each of the first 10 of them. These rows are advice. They give no consequence, plan item or suspect, and they change no briefing token. A word of digits only, such as `2026`, never starts a match, but it still counts in a score.
- **Port changes from another project.** `rdy view replay <view> --from <project-or-dump> [--brief <token>] <change-or-landing>...` writes one new change into your open view for each change you name, or for each change of a landing you name. The source is another project on this machine, or a dump of one, and rdy opens it read-only. The changes must have landed there. The port is one act, and one undo removes it. Without `--from`, `rdy view replay` works as before.
- **What a port copies.** Each new change keeps the ops, the message and the justification of its source change, and the blobs they need. It drops `intent` and `alternative`. Local pins and block links come as data, with no pin closure. An update whose base your project lacks becomes a create of its result, so it conflicts with a block that the project holds or deleted. A delete that your project already satisfies is dropped, and so is a delete whose base your project lacks, of a block that your project lacks too. A conflict stays in the view, as in any view. The limits of replay apply: 1,000 changes, 10,000 ops and 16,777,216 encoded bytes.
- **Where a change came from.** Each ported change records its source in `ported_from`: the source project's repo id and the hash of the source change. `rdy view show` prints `ported from <repo_id> <change>` for it, with no `[wc]`. JSON view reads give `ported_from` on each ported change, and landing facts give `ported_changes`, in landing order.
- **A port needs a briefing.** A port that touches a block of your project needs a briefing of the target blocks, also in a draft view. When you leave out `--brief`, your CLI session briefs the target itself. An agent session passes the token from `rdy brief`, or the port gives `E_BRIEF_REQUIRED` and writes nothing. A port that touches no block of your project needs no token.
- **`rdy_view`** replay takes `from_file`, a dump of the source project on the server's machine, and `brief`, the target briefing token. `rdy_dump` makes the dump. `from` stays the input of a fork.
- **New refusals.**
    - A change that has not landed in the source gives `E_BAD_INPUT`, "change <hash> has not landed in <project>": land it there first, or replay it inside that project.
    - `from` and `from_file` together give `E_BAD_INPUT`, "replay takes only one source". `brief` with no source gives `E_BAD_INPUT`, "replay without a source does not take brief".
    - A delete whose base your project lacks, of a block that your project holds, gives `E_BAD_INPUT`, "the target line lacks the base of a ported delete". So does a port that would make two blocks each other's ancestors.
    - A port whose block needs a parent that your project lacks, or holds in conflict, gives `E_DEPS_MISSING`, "the port needs parent <id> present and clean in the target": carry the change that creates the parent first, or resolve its conflict.
    - A source blob that was removed gives `E_REDACTED_STATE`, "a source blob of the ported change was removed".
    - A foreign pin that your project holds no delivery for gives `E_FOREIGN_VERSION`.
- **A new store feature.** The first ported change in a project turns on the store feature `ported_change`. From then on, an rdy before 0.58.0 refuses the project with `E_STORE_FORMAT`, "store feature ported_change needs rdy 0.58.0". A removal of the ported change keeps the feature on. A dump of the project records it, and an older rdy refuses to load that dump by the same rule.
- **For hub integrators (rdy-adapter):**
    - `Snapshot::port_bundle(landings)` on a hub holder gives a `PortBundle`, form `rdy-port-bundle-1`, of 1 to 1,000 landings of the source hub canon, with at most 1,000 changes and 64 MiB. It holds no epoch, removal set or expiry: the hub makes it again for each attempt and keeps none.
    - `StoreHandle::port_into_view(bundle, view, author)` writes a new pushed view on hub canon's tip, with one new change for each source change, under the run `hub-carry` and the person's actor. Canon, tags and held refs do not move, and the carry makes no landing and no request. A person pulls the view, lands it and pushes it, and that push opens the landing request. If the view exists, the call returns `PortOutcome::Behind { head }` and writes nothing.
    - A carry copies foreign links and pins as written and checks no delivery for them. The delivery check runs when a person lands the view.
    - A carry's refusals are `E_BAD_INPUT`, `E_DEPS_MISSING`, `E_REDACTED_STATE` and `E_BRIEF_REQUIRED`. It checks in a fixed order, and the first fault wins: length, family, same store, author, target hub canon, view head, target removals, ops, briefing.
    - `Snapshot::compare_size(from, to, limit)` counts the rows of a compare up to `limit`, builds no entry, and refuses what `compare` refuses.
    - `Snapshot::prepare_canon_state(root)` reads the state of the canon tip once, as stream format 5, and gives a `PreparedCanonState`. A root that the snapshot does not hold gives `E_UNRESOLVED_REF`.
    - `Snapshot::prepare_pull` and `prepare_pull_with_base` prepare a first or an incremental pull with one walk. `prepare_pull_reply` and `prepare_pull_reply_with_base` give a `PullReply`, with `page(after, limit)` and `write_keys_encoded(keys, accept_zstd)`. Another purpose gives `E_STREAM_PURPOSE`, and a key outside the selection gives `E_UNRESOLVED_REF`.
    - `Snapshot::check_quarantine_checked`, with `CheckedBatchFile` and `check_batch_file`, reuses core's check of each put. A file that changes during the take gives `E_BAD_RECORD`, "input changed during take".
    - The client asks for get replies with `Accept-Encoding: zstd`, and reads a reply with no coding or with one zstd stream. It asks for the canon state only when the hub lists stream format 5. When a get refusal `E_HUB_VALIDATION` holds `args.get_batch_bytes`, the client splits the get's keys in two and asks again.

### Changes

- **Many refusals name the next step.** The codes and the cases that raise them do not change. The fix texts now say what to do. Some examples:
    - A plan answer with an act that the item does not take names the acts it takes, such as "confirm items take: confirm or defer". `E_PLAN_ITEM` now says to read the plan again after each write, check `counts.open` and read every page, and gives the number of open items.
    - A wrong card option lists the options of that card with their labels: "this <kind> card takes: <id> (<label>), .... Copy an option id from the current card; put the person's exact words in evidence."
    - `E_EXCEPTION_OPEN` lists the exception cards to answer first, in order. `E_DEFERRAL_OPEN` names the deferral card. `E_CONFLICT_UNRESOLVED` says how to resolve each kind of block.
    - A delete of a parent lists its children. A block that only an open view holds names that view. A frozen decision lists the ways to change it.
    - A call with no MCP session says "state rdy/actor and rdy/run in _meta, or call rdy_session with your actor, run and role, then resend the call".
    - A list in a fix text holds at most 32 entries, each of at most 256 characters, and says when it holds only part of the list.
- **Tool descriptions with examples.** `rdy_brief` says that the token is in `base.token`, or in `briefing.base.token` in an `E_BRIEF_REQUIRED` or `E_BRIEF_STALE` refusal. `rdy_answer` lists the options of each kind of card. `rdy_plan` lists the acts that each kind of item takes. `rdy_view` gives the parameters of each op. `rdy_decide`, `rdy_supersede`, `rdy_propose`, `rdy_accept`, `rdy_links`, `rdy_resolve`, `rdy_tag` and `rdy_write` give an example call, and `rdy_read` gives its limits. The key rule in `rdy_schema` gives the length and the characters that a key can hold.
- **Search texts.** A `sort` that is not valid gives "sort takes coverage, relevance or reading, not <value>". A `rdy_brief` page cursor from an earlier rdy is refused with "page is not a cursor from rdy_brief's next".
- **Faster push and first pull.** A push prepares the next upload batch while the current one uploads, and checks the canon inputs while it walks canon. A pull and a clone walk the history once for each operation, decode each record once, and ask the hub for zstd get replies. A clone runs its last check on the store that took canon. When a hub is busy after a lost reply, rdy waits and asks for the outcome again, for at most 120 seconds, and never sends the write again. New refusals: `E_IO`, "the upload preparation could not start" or "the canon input check could not start", with the fix "retry the push", and `E_INTERNAL`, "the upload preparation failed" or "the canon input check failed". On a test project of 57,618 records, over the local loopback, a first push took 7.1 s, where it took 15.4 s before this release. A clone of the whole history took 13.3 s, where it took 57.2 s, and a clone by URL was ready for work after 2.6 s. A push of a small change took 2.3 s (3.4 s before), and a pull of 10 new landings took 3.0 s (5.5 s before).
- **Sync stream format 5.** rdy reads sync stream formats 2, 3, 4 and 5. Format 5 holds the state of the canon tip, and rdy asks for it only when the hub lists it.
- **The schema texts for ports.** The `rdy_schema` edit-loop text says that a port needs a briefing of the target blocks, also on drafts. The view schema lists `from_file` and `brief` for replay.
- **Breaking for older rdy: rdy 0.59.0 adds the store feature `dedup_spans` (class incompatible, minimum rdy 0.59.0); store format 19 and dump format 4 stay.** Every store rdy 0.59.0 creates uses `dedup_spans` from the start. The first time rdy 0.59.0 opens a store made by rdy 0.57.0, even to read it, it upgrades the store to use `dedup_spans`, after a backup beside the store, and maps the repeated byte runs already in it. From then on rdy 0.57.0 refuses that store, and a dump of it, with `E_STORE_FORMAT` and names rdy 0.59.0. The way back is a dump made with rdy 0.57.0 before the first rdy 0.59.0 open. rdy 0.59.0 loads every earlier dump.
- **Breaking for older rdy: rdy 0.59.0 adds the store feature `partial_history` (class incompatible, minimum rdy 0.59.0); store format 19 and dump format 4 stay.** A store starts to use `partial_history` only when its project first takes a canon state in a URL clone, before its history arrives, and only a store or dump that uses it needs rdy 0.59.0: rdy 0.57.0 refuses one with `E_STORE_FORMAT` and names rdy 0.59.0. rdy 0.59.0 opens a store made by rdy 0.57.0 as it is, and loads every earlier dump.
- **Breaking for older rdy: rdy 0.59.0 adds the store feature `plan_delta` (class incompatible, minimum rdy 0.59.0); store format 19 and dump format 4 stay.** A store starts to use `plan_delta` only when its project first writes a plan with this rdy (every plan version it writes uses it), and only a store or dump that uses it needs rdy 0.59.0: rdy 0.57.0 refuses one with `E_STORE_FORMAT` and names rdy 0.59.0. rdy 0.59.0 opens a store made by rdy 0.57.0 as it is, and loads every earlier dump.
- **Breaking for older rdy: rdy 0.59.0 adds the store feature `ported_change` (class incompatible, minimum rdy 0.58.0); store format 19 and dump format 4 stay.** A store starts to use `ported_change` only when its project first stores a ported change or a removed port's residue, and only a store or dump that uses it needs rdy 0.58.0: rdy 0.57.0 refuses one with `E_STORE_FORMAT` and names rdy 0.58.0. rdy 0.59.0 opens a store made by rdy 0.57.0 as it is, and loads every earlier dump.
- **Moving to 0.59.0.** You need no step of your own. The first time rdy 0.59.0 opens a store from rdy 0.57.0, it upgrades the store, and it loads every earlier dump. Store format 19 and dump format 4 stay. You can also dump the project with rdy 0.57.0 and load the dump with rdy 0.59.0. The `partial_history`, `plan_delta` and `ported_change` lines below say that rdy 0.59.0 opens a store made by rdy 0.57.0 as it is. That is true for those features. The `dedup_spans` upgrade above still runs on every store from rdy 0.57.0. rdy 0.58.0 was not published. Where a refusal names rdy 0.58.0, use rdy 0.59.0.
- **A hub must support this release.** Before you push or publish a project with rdy 0.59.0, make sure that your hub supports the store features `dedup_spans`, `plan_delta` and `partial_history`.

### Fixes

- **A land checks every pin.** Before, a land of a pulled view could land a pin to a foreign version that the project held no delivery for. A land now checks the pins of every change that it writes, and refuses such a pin with `E_FOREIGN_VERSION`. The land works once the delivery is in the project.
- **Linux on older CPUs.** The rdy 0.56.0 and 0.57.0 Linux archives needed an x86-64-v3 CPU, with AVX2, BMI2 and MOVBE (about 2013 and later). On an older CPU, rdy died with SIGILL at its first TLS contact with a hub, for example in `rdy auth sign-in`. The C code of ring and zstd in the Linux x86_64 archive is now built for baseline x86-64. ring and zstd still choose fast code by the CPU at run time.
- **A clone of a hub project passes its check.** Before, `rdy clone <hub url>` of a project with intents, alternative sets or plan chains from other views failed its fsck with `E_IO`, "intent ... does not exist". A later fsck or upgrade after a full-history pull failed the same way. The store check now accepts these fetched records.
- **A pull downloads each object once.** Before, a full-history pull or a clone by URL asked for every object in one get, dropped a reply that was past the hub's batch limit, and asked again for half. So a large history came down two or three times. A fetch now sizes each get from the bytes of each kind that earlier replies held.
- **Reaffirm cards for a landing's own decisions.** Before, a proposal that confirmed decisions and edited the blocks that carry them out could open a reaffirm card for those decisions after it landed. rdy now leaves out a decision when the same landing wrote the change and the block's current version. Edits from another view still open the card. Suspects stay readable in `rdy_suspects` and in briefings.
- **Daemon start on Windows.** About one daemon start in five failed when Windows refused to replace the socket record `daemon.sock` with "Access is denied". The daemon now tries the replace up to 20 times, 50 ms apart.
- **A daemon of another version.** On Windows, a client that restarted the daemon with another version of rdy could reach the old daemon and lose its session with `E_DAEMON_GONE`. A daemon that is being replaced now takes no new session.
- **A removal on Windows.** A stop during a removal could leave the project with no search index. The next open now builds the index again.

### Project and build

- **Build flags.** `.cargo/config.toml` sets `CFLAGS_x86_64_unknown_linux_gnu` and `CXXFLAGS_x86_64_unknown_linux_gnu` to `-march=x86-64 -mtune=generic`. The same variable set in your environment wins.
- **A baseline CPU check.** The release candidate runs the packaged Linux binary in `qemu-x86_64` with a baseline x86-64 CPU: `--version`, then a load, a check and a dump of the sample project.
- **Gate evidence.** A gate pass that needed a retry counts for a release only when each retried test binary names a flake issue: `qa-gate --retry-failed --flake-links <binary>=<issue id>,...`. The gate report names the failed tests and the panic text of each failed test binary.
- **New performance rows.** `sync_push_500mb` and `sync_pull_full_history_500mb` measure a push and a full-history pull of a 500 MB project, with no target.
- No new dependency.

## Feedback

This is early. Tell us what worked and what did not: https://github.com/adrenickchrome/rdy-core/issues

**Known issues in this release:**

- A dispute, or a dispute resolution, made after a removal request still makes the request stale. Make the removal request again.
- A ruleset's "when" text can show two paragraphs when the first block of the ruleset holds two.
- When the project and your meta project each hold close to 5,000 rulesets, the menu can pass its 5,000-entry limit.
- When rdy cannot read the folder of your meta project, it treats the meta project as missing. Your own rules are then left out with no error.
- A validation error from your meta project loses its list of issues.
- `rdy setup` can overwrite a change that another tool saves to `CLAUDE.md` or `AGENTS.md` while it runs. Run it when no other tool is saving those files.
- A malformed `load` value is refused with a different message from the documented one.
- `instructions.person` set outside the user config warns only on the CLI's standard error, not in the tool reply or the CLI's JSON.
- The CLI's plain-text briefing shows neither the as-built status nor `W_AS_BUILT_OVERRIDE`. `rdy show` and the MCP tools show both.
- A review page does not show the Planned or since label on removed content.
- A confirm can silence `W_AS_BUILT_OVERRIDE` even when another as-built item of the same proposal was updated, not confirmed.
- When a decision's `built` changes and then changes back, an earlier confirm silences `W_AS_BUILT_OVERRIDE` again.
- `rdy publication check --against` can miss what a shown section reveals when a planned decision outside the selection becomes built.
- In a project with very many keys in one doc, a key change can land without a new assessment.
- A rule that you remove twice and then state a third time, with the same text, selectors, ruleset and reason, stops as not ready. Give it a different reason.
- The tool schemas show example ids, such as `D-W2HXR`, `FW-E3` and `M2c3`, that look like real ids. They are only examples. Earlier releases showed them too.
- On Windows, the checksum step of the install guide, `Get-FileHash`, fails with "not recognized" when another program starts Windows PowerShell 5.1 from PowerShell 7. Run the step in a normal PowerShell 5.1 or 7 window.
- Your meta project has its own daemon. `rdy serve --stop` in a project does not stop it.
- A misspelt or unknown kind under a pack's `tensions_across` is ignored with no warning. The docs say it warns with `W_CONFIG_UNKNOWN`.
- A publish that is already running fails with `E_UPSTREAM_AUTH` when another sign-in for the same install becomes due. Run `rdy auth state`, then publish again.
- In a single-file export, a doc whose title holds two spaces in a row, such as `A  B`, gets a link to itself that misses. The heading is `# A B`, but the link is `#a--b`.
- In a multiline field that is not a prose body, fenced code in a list item is also read as code. A ref in that code stays as typed, is not checked and adds no link. The rule is for prose bodies only.
- In a project of about 50,000 blocks, writes became slower after rdy 0.46. On the test machine a write takes about 1 ms, where it took 0.55 ms, and about 1.5 ms when it returns consequences. This release does not change it.
- After a landing, an open review page waits until the daemon has rendered all the open pages again. With many review pages open, this can take several seconds.
- Live site and review pages:
  - With many review targets in the site's memory, rdy can drop its build of canon or a tag to make room. The next request for that page then builds it again once.
  - The history that a review through a profile keeps is not counted in the site's memory cap. When rdy drops such a target while one of its pages is open, the history stays.
  - rdy counts some parts that review targets share more than once, so it can drop a review page before it must.
  - Each review page that is not yet rendered builds the navigation again, so the first view of a page costs more time than it needs. The times above include this.
  - The first view of a text page in a review through a profile reads the target's selected image files first.
  - In a review through a profile, an image with no dark version can count as two files. A target at exactly 32,000 files can then be refused with `E_LIMIT`.
- Suspects:
  - A suspect on a superseded decision suggests "update <block>, or repin it to <decision>". A repin does not clear such a suspect: update the block, or point it at the decision that supersedes it.
  - A suspect whose premise was superseded reads only `superseded`. It names neither the decision that supersedes it nor what changed.
  - `rdy_trace` can show no premises for a decision that has a `depends_on` link.
  - `rdy_view create` with a bare landing hash as `base` gives a fix about forking a view. Write the base as `landing:<hash>`.
- A content removal keeps the upgrade backup that a pending upgrade completion names, so that backup can still hold the removed text until the completion ends. Only a removal that finishes at an open after a crash can meet this.
- Sync with a hub:
  - A stopped publish's push id has no age. The next publish of the same tag resumes under it, also after the hub's quarantine for that upload has ended.
  - A pull with replay walks the project's retained history for each operation, also when nothing is left to replay. Its time and memory grow with that history.
  - A forced push, a restore and a take of hub canon read the state again after you confirm the card. If canon moves between the card and the run, the move can remove landings that the card did not name.
  - A push of a fork alone treats its source project as absent from the hub, also when the hub has it.
  - After local history moves on, a push that resumes an upload can send a different batch from its first try, and the hub can refuse it.
  - A restore can use a held hub tip as its history base when that tip descends from the dump's canon. The hub then refuses the restore.
  - `rdy clone <url>` with no target folder, run in an empty current folder, replaces that folder. Give a target folder.
  - When the project description that a push sent holds a newline or a tab, `rdy clone <url>` of that hub repo fails with `E_BAD_INPUT`. A new push does not fix it.
  - A clone refuses a hub pack whose custom parent pack this rdy does not have, with `E_BAD_INPUT`.
  - When the last flush of the folder fails after the clone is in place, the clone returns `E_IO` but leaves the project there.
  - A pull reply does not list the warnings about removals that a publication does not cover. `rdy status` and `rdy_overview` show them in `uncovered_warnings`.
- Forks at a tag:
  - When the last sync of the folder fails after a fork is in place, the fork returns `E_IO` but leaves a complete project there. A retry then gives `E_ALREADY_EXISTS`. Check the project with `rdy fsck`, or remove the folder and fork again.
  - In a fork, a search in the scope of a proposal, by its full hash, can read a correction made after the tag.
  - On Windows, a file or folder that another program makes at a clone or fork destination, while rdy moves the new project into place, can be replaced.
- A clone by URL with the history to come:
  - `rdy status` in text form does not print the notes of the byte report. `rdy status --json` gives them.
  - The `rdy_clone` tool description does not yet say that a clone can be ready before its history.
  - After a clone by URL is ready, its history takes longer to complete than a clone of the whole history: about 24 s, where a whole-history clone took 13.3 s, on the test project above. rdy checks the history more than once after the fetch.
  - While the history is incomplete, its completion reads the held records before its size limits apply, so a very large project can use much memory there.
- Disk space:
  - Each compact counts the mapped spans that cannot punch holes again, so the count in its note can include spans from earlier passes.
  - When the file system cannot punch holes, the pass that runs after a write records no note. The next `rdy compact` reports it.
  - Your own writes made before a pull or a clone into the project wait for `rdy compact` too. The pass after a later write does not count them.
  - `rdy fsck` accepts a `Received` row in the span map that points past the end of the log. Only store files that do not match can make one.
  - In rdy-adapter, a hub move with no removals writes no `Received` row, so the next write on that holder maps the moved history.
- Sync with a hub, from this release:
  - A dump download refuses a reply with any `Content-Encoding` other than identity.
  - A get that a hub refuses for its size, with no `args.get_batch_bytes` in the refusal, ends the clone or pull with the hub's error.
- Search:
  - In a `like` or `like_text` search, `ignore_case` has no effect. `like` takes a block id, not a key or a path.
  - An edit can change which lines a `like` hit shows, and the page cursor then goes stale with `E_PAGE_STALE`. Search again from the first page.
- Ports:
  - `rdy_view` replay takes a project directory as `from_file` or `from`, although it is meant to take only a dump.
  - A port that cites an unknown or stale briefing token gives `E_BRIEF_STALE` with no new briefing. Run `rdy brief` again, then retry with the new token.
  - A project can hold canon landings, taken from a hub, with foreign pins that it holds no delivery for, while its own land of the same content is refused.
- Hub reads in rdy-adapter:
  - A pushed view whose included change was redacted does not render at an op. The holder and the render process refuse it. A read of the same scope works.
  - `rebase_test` and `replay_test` read deletions as of `onto`. A block that was deleted before `onto`, and that the chain re-creates, deletes and re-creates again, can show a false `deleted_since`. A real pull of the same chain is clean.
  - On a chain where one landing deletes a block and a later landing re-creates its id, a real pull stops at the same landing as the replay test. The cause of the real stop can be the gate or a conflict. The replay test always says `conflict`.
  - The replay test's check of each landing reports `assessment_matches` for the stored assessment of the source landing, not for the new assessment of the replayed landing.
- Check modules:
  - Fuel counts come from wasmi 2.0.2's default operator costs. A later wasmi can change them, so a module near a fuel limit can pass in one release of rdy and fail in the next.
  - A module rule's reach can be slow when a written block has very many referencers. In a debug build, a block with 100,000 referencers did not finish in 7 minutes.
  - Two proposals that each add one block to a module rule's selection can both land on their old assessments. A later scan of the whole selection can then fail on the rule's fuel.
  - A memory growth that fails for a lack of system memory returns -1 to the module. rdy then keeps the result as a trap of the module, not as a fault of rdy.
  - The verdict cache counts key and text bytes, not heap bytes. In one measurement, a full cache held about 474 MiB of heap.
  - The example module's JSON reader stops at the judged block's closing brace, and accepts some text that is not valid JSON. rdy always sends valid JSON, so the example reads its input right. Use a full JSON parser in your own module.
- Snapshot deltas and the daemon:
  - An upgrade that rdy refuses, for example on a snapshot whose bytes are not canonical, keeps its backup. Every later open to write then refuses with `E_UPGRADE_BACKUP`. Move the backup folder aside, then open the project again. Readers still open the project. The refusal of the upgrade gives the fix "run rdy fsck", which does not help in this case.
  - The upgrade, and the load of a dump without the feature, keep all new snapshot deltas in memory. A very long history of very large projects, such as 2,500 landings of 100,000 blocks each, can run out of memory. Nothing is written in that case.
  - `rdy fsck` accepts a snapshot delta that sets a block version that the store does not hold, when no removal took that version.
  - `rdy fsck`, dump and load accept snapshot delta bytes that are not canonical. A take refuses them.
  - A removal report over a long history can be slow: it folds shared history again for each landing.
- Publication review:
  - A first check's `included.rows` give no `path`, `digest` or `redacted`, although an added doc row gives them.
  - Excerpts of decisions lose code backticks: "Keep `ledger` rows" reads "Keep ledger rows".
  - A decision outside the publication that a doc only records shows no title, status or excerpt.
  - When a figure with one attachment becomes prose, the review counts one update and no removed figure.
  - A diff piece that uses exactly the budget of 300 characters drops the pieces after it, with no ellipsis.
  - The text output shows `blocks: 0 added, 0 removed, 0 updated` for a doc where only decisions changed.
  - The text output prints `Some rows are omitted.` also when it cut only text inside a row.
  - After an approved removal of an attachment, the alt text of a figure outside the selection can stay in a reference row's `target_label` or `source_label`.
- Repo links. All but the first of these need deliveries from rdyhub:
  - Two views that each add a repo link with the same alias, for different repos, can both land. Reads then use the link with the lower block id.
  - A misspelt link kind on a cited target, such as `decided_byy`, is accepted with no warning.
  - A new project refuses a whole delivery with `E_UNRESOLVED_REF` when the delivery holds a version that was already redacted.
  - When two links deliver the same version, a removal proof taken through one link also erases the other link's copy. `rdy fsck` then fails for the other link.
  - With two links to one project, adopting a new version of its instructions through one link also moves the other link's baseline.
  - An adopted instruction set takes its ruleset doc and its `required` flag from the latest delivery, but its rule text from the adopted version.
  - After a link is changed to another repo, a delivery equal to the last one is taken as unchanged, and still names the old repo.
- MCP over HTTP:
  - A power loss soon after the first start with MCP over HTTP on can lose the token file. Saved client settings then stop working. Run `rdy mcp --http-config` again, and update the client.
- Warning hooks:
  - A real Claude Code edit on Windows is not yet proven. A real Claude Code edit on macOS, the native Windows hook tests and a real commit in PowerShell through the code hook stand in for it.
  - The code hook does not see a decision that is only proposed, in a view that has not landed. It reports the commit as checked, with no warning.
  - When the code hook must visit more than 5,000 blocks to reach a cited block's decision, it reports the commit as checked, with no warning.
  - The edit hook can warn about a doc that the working copy leaves out, when the doc's folder has a `_folder.yaml` that is not valid.
  - When an editor saves `.claude/settings.local.json` while rdy installs or removes the edit hook, the editor's change can be lost with no `W_HOOK_CONFLICT`. Install or remove the hook when no editor is saving that file.
  - If you change the arguments of rdy's entry in `.claude/settings.local.json` and install the edit hook again, the file gets a second `PostToolUse` entry.
  - When the `commit-msg` launcher has lost its executable bit, installing the code hook again keeps it with no warning, and Git ignores it.
  - With `rdy mcp --no-daemon`, `rdy_hook` cannot install or remove a hook. Start `rdy mcp` without `--no-daemon`.

## Third-party source (MPL-2.0)

rdy is licensed MIT OR Apache-2.0. The binaries also contain two libraries under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/): `imbl` 7.0.2 and `imbl-sized-chunks` 0.2.0. This release attaches the exact source code of each, as its crates.io crate file. Each SHA-256 matches rdy's `Cargo.lock` at this release:

- [imbl-7.0.2.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.59.0/imbl-7.0.2.crate) — SHA-256: `46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec`
- [imbl-sized-chunks-0.2.0.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.59.0/imbl-sized-chunks-0.2.0.crate) — SHA-256: `2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6`

Each archive's `THIRD-PARTY-NOTICES.txt` gives the license of every library in the binary.

<details>
<summary><b>About Release 1</b></summary>

Release 1 (rdy 0.47.0) is the first release of rdy with a compatibility promise.
rdy stays below 1.0 during the beta, and the promise holds anyway.

From this release on, a change that breaks a contract surface ships with its upgrade path, in the same release. The contract surfaces are:

- the store, dump, sync batch and working-copy formats, and signatures;
- the CLI's commands, arguments, flags, exit codes and `--json` output;
- the MCP tools: their names, parameters and result fields;
- the `E_*` and `W_*` codes;
- the config keys and the pack format;
- the Markdown export layouts and the site's URLs.

A renamed name keeps working, with a warning, for two minor releases and at least 30 days, whichever ends later.
Text for people, such as messages, fix texts and help, can change in any release. Match a code, never its text.
Content that the docs mark as planned is outside the promise.

Every later rdy reads every dump that this rdy writes.
If this rdy refuses the store of a project from a preview with `E_STORE_FORMAT`, move the project with dump and load.
Run `rdy dump --out <file>` with the old rdy. Then run `rdy load <file> <dir>` with this rdy, into a directory with no store.

Report problems at https://github.com/adrenickchrome/rdy-core/issues.

</details>

<details>
<summary><b>Verify the download (optional)</b></summary>

Every release is signed. `SHA256SUMS` lists the three archives and the sample project `tally.rdydump`, and `SHA256SUMS.sig` is its OpenSSH signature (namespace `rdy-release-v1`, principal `rdy-release`). rdy's machine release key signs this release.

Release signing key:

```text
rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8
```

Fingerprint: SHA256:Tj8hfra0U07asaeSZkt1b5e1HGJ+w8vMzLrn+G6/+/M. It is the same as in the [README](https://github.com/adrenickchrome/rdy-core#release-signing-key).

| Platform | Target | Tested on |
|---|---|---|
| macOS arm64 | `aarch64-apple-darwin` | macOS 26.6.2 |
| Linux amd64 | `x86_64-unknown-linux-gnu` | Ubuntu 26.04.1 LTS |
| Windows amd64 | `x86_64-pc-windows-msvc` | Windows 11 Pro 25H2 |

To check the signature and the hash before you install, download the archive, `SHA256SUMS` and `SHA256SUMS.sig` into one folder and run:

On macOS arm64, for an archive downloaded with curl:

```sh
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'shasum' '-a' '256' '-c' '--ignore-missing' 'SHA256SUMS' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.59.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.59.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.59.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.59.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.59.0/bin/rdy' &&
'/path/to/rdy-0.59.0/bin/rdy' '--version'
```

On Linux amd64:

```sh
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'sha256sum' '-c' '--ignore-missing' 'SHA256SUMS' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.59.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.59.0-x86_64-unknown-linux-gnu.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.59.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.59.0-x86_64-unknown-linux-gnu/rdy' '/path/to/rdy-0.59.0/bin/rdy' &&
'/path/to/rdy-0.59.0/bin/rdy' '--version'
```

On Windows amd64, in PowerShell:

```powershell
& {
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath 'C:\path\to\download'
$s = [IO.File]::Open('C:\path\to\download\release-allowed-signers', [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::Write, [IO.FileShare]'ReadWrite, Delete'); $s.SetLength(0); $b = [Text.Encoding]::UTF8.GetBytes('rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' + [char]10); $s.Write($b, 0, $b.Length); $s.Close()
& 'cmd.exe' '/d' '/c' 'ssh-keygen.exe -Y verify -f release-allowed-signers -I rdy-release -n rdy-release-v1 -s SHA256SUMS.sig < SHA256SUMS'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
$n = 0; foreach ($line in [IO.File]::ReadAllLines('C:\path\to\download\SHA256SUMS')) { $want = $line.Substring(0, 64); $name = $line.Substring(66); $file = [IO.Path]::Combine('C:\path\to\download', $name); if (Test-Path -LiteralPath $file) { $got = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLower(); if ($got -cne $want) { throw ($name + ': FAILED') }; [Console]::Out.Write($name + ': OK' + [char]10); $n++ } }; if ($n -eq 0) { throw 'no archive was checked' }
foreach ($dir in @('C:\path\to\unpack', 'C:\path\to\rdy-0.59.0')) { if (Test-Path -LiteralPath $dir) { throw ($dir + ' exists') }; [void][IO.Directory]::CreateDirectory($dir) }
& (Join-Path $env:SystemRoot 'System32\tar.exe') '-xzf' 'C:\path\to\download\rdy-0.59.0-x86_64-pc-windows-msvc.tar.gz' '-C' 'C:\path\to\unpack'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
[void][IO.Directory]::CreateDirectory('C:\path\to\rdy-0.59.0\bin')
[IO.File]::Copy('C:\path\to\unpack\rdy-0.59.0-x86_64-pc-windows-msvc\rdy.exe', 'C:\path\to\rdy-0.59.0\bin\rdy.exe')
& 'C:\path\to\rdy-0.59.0\bin\rdy.exe' '--version'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
}
# Add the version's bin directory to PATH in this session:
# $env:Path = 'C:\path\to\rdy-0.59.0\bin;' + $env:Path
# rdy needs SSH_AUTH_SOCK, even when ssh works without it. Set it to the agent pipe:
# $env:SSH_AUTH_SOCK = '\\.\pipe\openssh-ssh-agent'
```

On macOS arm64, for an archive downloaded with a web browser:

```sh
# In Safari, turn off “Open safe files after downloading” before downloading. Keep the original .tar.gz file.
# After both checks pass, remove com.apple.quarantine from the archive before extraction. Skip this command if the archive has no com.apple.quarantine attribute.
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'shasum' '-a' '256' '-c' '--ignore-missing' 'SHA256SUMS' &&
'xattr' '-d' 'com.apple.quarantine' '/path/to/download/rdy-0.59.0-aarch64-apple-darwin.tar.gz' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.59.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.59.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.59.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.59.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.59.0/bin/rdy' &&
'/path/to/rdy-0.59.0/bin/rdy' '--version'
```

Add the version's bin directory to PATH in this shell.
Replace `<version-prefix>` with the version prefix, such as `/path/to/rdy-0.59.0`:

```sh
export PATH="<version-prefix>/bin:$PATH"
```

</details>
