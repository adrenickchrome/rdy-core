**Your intent, ready for anyone who needs it.** rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents. Before an agent writes, rdy shows it what governs the change and what depends on it. After the write, rdy shows what the change affected. Nothing joins the accepted design until a person approves it.

This release adds publication baselines. A baseline is a portable record of what a published version showed, so the next publication can be compared with it, also in a project that shares a fork origin. `rdy_suspects` now pages, and its result changes shape. rdy keeps an upgrade backup until you delete it, and `rdy status` lists it. The live site needs much less memory while review pages are open, and a block's "Why?" trail lists only the decisions that govern it. rdy 0.53.0 opens a 0.52.0 project as it is. This release keeps the compatibility promise of Release 1. See "About Release 1" below.

## What you get

- **Docs and decisions as one project.** Write docs, requirements, terms and decisions as linked blocks, not loose files.
- **Agents work through MCP.** Claude Code, or any MCP client, reads and writes through rdy's tools, over stdio or a local HTTP endpoint. Each agent gets a briefing before it edits.
- **Instructions for agents.** rdy's own agent guide is built in. Keep your rules in rdy too, each with its reason. Every agent gets the rules it needs at the start of a task, and your own rules follow you to every project.
- **Warnings when work skips rdy.** Optional hooks warn when a commit changes decided code and cites no decision, or when an agent edits rdy's files directly. They never block.
- **Proposals and approvals.** Agent work lands in a view. You see the impact, then approve it or not.
- **Docs that match the release.** Record which release ships each decision. A public site then shows only what has shipped, and a release check catches content that is ahead of the build.
- **It catches drift.** When a decision changes, rdy marks every page that rests on it. It also flags a page that states a different number for a recorded value.
- **Linked projects.** Cite the decisions and instructions that another project delivers through rdyhub. Your project keeps the version it cites until a person adopts a newer one.
- **Bring your Markdown.** Import a folder of Markdown files as docs. A preview shows what maps, and what does not, before anything is written.
- **A live docs site.** `rdy site` gives you a local site that refreshes as changes land.
- **A README from your docs.** Export one doc as one Markdown file, with its images in a folder beside it.
- **Local and private.** Everything runs on your machine: no account, no hosted service, no model inside rdy.

## Install

**macOS (Apple silicon) and Linux (x86_64):**

```sh
curl -fsSL https://getrdy.dev/install.sh | sh
```

**Windows 11 (x64), in PowerShell:**

```powershell
irm https://getrdy.dev/install.ps1 | iex
```

Each installer checks the release signature and the archive's SHA-256 before it installs. It puts `rdy` in `~/.local/bin` for your user, and asks before it changes your PATH.

To install from the archive instead:

**macOS (Apple silicon):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.53.0/rdy-0.53.0-aarch64-apple-darwin.tar.gz
tar -xzf rdy-0.53.0-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.53.0-aarch64-apple-darwin/rdy ~/.local/bin/
```

**Linux (x86_64):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.53.0/rdy-0.53.0-x86_64-unknown-linux-gnu.tar.gz
tar -xzf rdy-0.53.0-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.53.0-x86_64-unknown-linux-gnu/rdy ~/.local/bin/
```

**Windows (x86_64), in PowerShell:**

```powershell
Invoke-WebRequest -OutFile rdy-0.53.0-x86_64-pc-windows-msvc.tar.gz https://github.com/adrenickchrome/rdy-core/releases/download/v0.53.0/rdy-0.53.0-x86_64-pc-windows-msvc.tar.gz
tar.exe -xzf rdy-0.53.0-x86_64-pc-windows-msvc.tar.gz
New-Item -ItemType Directory -Force "$HOME\.local\bin" | Out-Null
Move-Item -Force rdy-0.53.0-x86_64-pc-windows-msvc\rdy.exe "$HOME\.local\bin\"
```

On macOS and Linux, if `~/.local/bin` is not on your PATH, add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile. On Windows, add `$HOME\.local\bin` to your PATH. Then check:

```sh
rdy --version
```

It prints `rdy 0.53.0`.

On macOS, if you downloaded the archive in a browser instead of with curl, macOS may block the binary. Run `xattr -d com.apple.quarantine ~/.local/bin/rdy` once.

## Release key

This release is signed by rdy's machine release key, the same key that signed 0.46.0, 0.47.0, 0.48.0, 0.49.0, 0.50.0, 0.51.0 and 0.52.0. To check the signature of your download, follow "Verify the download" in the README. The allowed-signers line for v0.53.0:

```text
rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8
```

Fingerprint: `SHA256:Tj8hfra0U07asaeSZkt1b5e1HGJ+w8vMzLrn+G6/+/M`.

Compare this line and fingerprint with the ones in the README at v0.53.0. Use the key only when both match.

## Try it: your first project

1. **Make a project.**

   ```sh
   rdy init ~/my-project && cd ~/my-project
   ```

2. **Connect your agent.** For Claude Code:

   ```sh
   claude mcp add rdy -- rdy mcp --project "$PWD"
   ```

   For another MCP client, add this server to its config (write the full path; most clients do not expand `~`):

   ```json
   { "mcpServers": { "rdy": { "command": "rdy", "args": ["mcp", "--project", "/home/you/my-project"] } } }
   ```

   If the client does not find `rdy`, give its full path as `command`, such as `/home/you/.local/bin/rdy`. On Windows, give the native path, such as `C:\Users\ana\my-project`, and write each backslash twice in JSON.

   A session lasts as long as one `rdy mcp` process, and keeps one actor and one run. To start another run, start a new `rdy mcp` process. On the same process, a second `rdy_session` with another actor or run gets `E_BAD_INPUT`. A client that starts a new process for each call must start a new session each time.

   The agent starts its session with `rdy_session`. The call takes two fields: `actor`, its own `agent:` actor such as `agent:claude`, and `run`, a name for the work. rdy's guide tells it to. Approving stays with you: an agent cannot start an approver session over MCP.

3. **Set up rdy for your agents.** In the project folder, run:

   ```sh
   rdy setup
   ```

   It creates your own rules project, the meta project, if you have none. It also adds three lines at the top of `CLAUDE.md` and `AGENTS.md` that tell the agent to call `rdy_session`, then `rdy_instructions`, at the start of each task. That call gives the agent rdy's guide, so you download nothing.

4. **Ask your agent for something real.** For example:

   > Start an rdy session in your own view, then read its overview. Help me design retention for an AI support assistant. Our intent is to keep only the customer data we need, for as short a time as we can, and be able to explain the choices. Start with a decision to delete raw conversations after 30 days and keep anonymised metrics for 2 years. Create the storage docs and link them to the decision. Propose the first design for my approval.

   The agent first loads its rules with `rdy_instructions`. The rules give a `kungfu` value for each tool that they cover, and the agent passes that value on each call to the tool. A call without it still runs, with the warning `W_KUNGFU`. In a new project nothing has landed yet, so the first writes need no brief. A later write that touches landed content needs a brief token. `rdy_brief` takes block ids or a path, and gives its token in `base.token`. Without ids or a path, `rdy_brief` refuses with `E_BAD_INPUT`. When `rdy_decide` or `rdy_write` creates blocks, its result also gives a token for those blocks, in `briefing.token`.

   A doc type can ask for sections: a doc with `doc_type: overview` needs Summary and Goals. Until the doc has them, a write gives the warning `W_SKELETON_MISSING`, and the agent adds them. It is a warning, not a refusal.

   A doc holds no text of its own: its opening paragraph is a prose block inside it. To link a doc to the decision, the agent puts `decided_by` on a block inside the doc. A folder and a doc in it can go in one `rdy_write`: the agent gives the new folder an id such as `$storage`, and uses that name as the doc's `parent`. The agent records a decision with `rdy_decide`, which takes the decision's text as `statement`.

5. **Review and approve it yourself.**

   ```sh
   rdy pending
   ```

   This shows what waits for you, with the impact of each proposal. Approve with `rdy approve P-<id>`, or tell your agent your answer. Without `@<n>`, `rdy approve` takes the proposal's latest version, which the card shows as `P-<id>@<n>`. If you answer a card in chat with Later, it stays hidden until the chat session changes, and `rdy pending` counts it as snoozed. When a proposal adds a decision, its option reads "Approve P-\<id\> and confirm D-\<id\>": approving it also confirms the decision.

6. **Read the result.**

   ```sh
   rdy site
   ```

   Open the link. The home page links to the folders, and each folder's page links to its docs. Each page's address ends in `.html`. Now ask your agent: “For EU customers, raw conversations must be deleted after 7 days. Keep the 30-day rule elsewhere and the 2-year metrics window.” The agent briefs on the decision and its connected docs, follows the consequences, and brings you the proposal to approve.

   A decision gets a card of its own only in some cases. For example, it supersedes or revokes a live decision, it is `one_way`, or no content applies it yet. If the agent refines the first decision and applies the new one in the docs, you get one card. `rdy_decide` takes the decisions that a new one refines as a list, in `refines`. Before it proposes, the agent answers each open item of the change plan with `rdy_plan {answers: [{item, act, reason}]}`.

   If `rdy pending` shows more than one card, answer each one, or tell your agent your answers. `rdy approve` names any card that it waits for.

   New content shows a **Planned** label. A new decision starts planned, so the content that cites it does too. Set `built` on the decision when the work ships.

   On a remote machine, the link is on `127.0.0.1`. Open an SSH tunnel to its port first, for example `ssh -L <port>:127.0.0.1:<port> <host>`.

   The site keeps running after `rdy site` exits. To stop it, close your agent's session first: quit the agent, or end the `rdy mcp` process that your client started. Then run `rdy serve --stop` in the project folder. An `rdy mcp` that is still open logs `E_DAEMON_GONE`. `rdy serve --stop` prints `stopped`, or `no daemon running` when none was running. Your meta project has its own daemon, which starts at the agent's first `rdy_instructions` call. Stop it with `rdy serve --stop --project <path>`. The path is `.rdy/meta` in your home folder by default, such as `~/.rdy/meta`, and `rdy setup` prints it when it creates the project.

## What changed since 0.52.0

### New features

- **Publication baselines.** `rdy publication baseline --profile <profile> --scope tag:<name> [--out <file>]` and `rdy_publication_baseline {profile, scope, out?}` make the baseline of a publication profile at a tag. A baseline holds ids, versions, hashes, titles, pages and labels, and no other block text. Its format is `rdy-publication-baseline-1`: one JSON object in RFC 8785 canonical form. With `--out`, rdy writes it to a new file outside `.rdy`. Without it, rdy prints the baseline.
- **Compare with a baseline file.** `rdy publication check --against-baseline <file>` and `against_baseline` on `rdy_publication_check` take a baseline file in place of `--against` and `against`. The file can come from this project, or from a project that shares a fork origin with it. It must hold the same profile id and audience. The review rows keep their form, but the baseline's side gives labels with no excerpt or word diff. The result adds `disclosure.baseline`, `{repo_id, tag, landing, baseline_hash}`, and the text output adds the line `Compared with baseline of tag <tag> (<hash>).` `against` then reads `{kind: "baseline", landing, tag}`, and `disclosure.redactions` gives `{candidate, baseline}`, one list for each side. Over MCP, a relative `out` or `against_baseline` is under the project folder. On the command line, a relative `--out` or `--against-baseline` is under the current folder. `rdy_publication_baseline` is in the `rdy:publish` ruleset.
- **`changes_total` on review doc rows.** Each changed, added and removed doc row of the publication review gives `changes_total`. For a changed doc, it is the sum of the row's `counts`: blocks added, removed, updated and moved, plus decisions added, removed and updated. An added or removed doc gives 1 for the doc, plus its blocks and decisions. A row with `record_only` true gives 0. It also counts the changes that a cap leaves out of the row's lists. It enters no hash.
- **`rdy_suspects` pages.** `rdy_suspects` takes `page`, and gives `{items, total, next?}`. When `max_tokens` (4,000 by default) cuts whole items from the end, pass `next` as `page` with the same scope until `next` is absent. `total` counts every suspect.
- **Upgrade backups are listed.** `rdy status` prints one line for each upgrade backup of the store: `upgrade backup <name> (<n> bytes): rdy's rollback for the last upgrade; you may delete it after checking the store`. `rdy status --json` and `rdy_overview` add `upgrade_backups`, `[{name, bytes}]`, while the store has a backup.

### Changes

- **Breaking for older rdy: rdy 0.53.0 adds the store feature `hub_canon` (class incompatible, minimum rdy 0.53.0); store format 19 and dump format 4 stay.** A store starts to use `hub_canon` only when its project first uses it, and only a store or dump that uses it needs rdy 0.53.0: rdy 0.52.0 refuses one with `E_STORE_FORMAT` and names rdy 0.53.0. rdy 0.53.0 opens a store made by rdy 0.52.0 as it is, and loads every earlier dump.
- **Breaking for older rdy: rdy 0.53.0 adds the store feature `sync_state` (class incompatible, minimum rdy 0.53.0); store format 19 and dump format 4 stay.** A store starts to use `sync_state` only when its project first uses it, and only a store or dump that uses it needs rdy 0.53.0: rdy 0.52.0 refuses one with `E_STORE_FORMAT` and names rdy 0.53.0. rdy 0.53.0 opens a store made by rdy 0.52.0 as it is, and loads every earlier dump.
- **Your projects do not use them.** `hub_canon` is for rdyhub's holders, and nothing that you run in this release writes `sync_state`. So you do nothing to move: rdy 0.53.0 opens a store made by rdy 0.52.0 with no upgrade step and no dump and load, and the store stays readable by rdy 0.52.0. When rdy's index of landings is behind the log, rdy rebuilds it from the log at the first open.
- **Breaking: the result of `rdy_suspects`.** It is now `{items, total, next?}`. The members `truncated` and `omitted` are gone. `total` is on every result, so a result that fits is not the same as in rdy 0.52.0. A `page` past the end, or a text that is not a page cursor, is refused with `E_BAD_INPUT`. A cursor from another list of suspects, another scope or another tool is refused with `E_PAGE_STALE`. A `max_tokens` too small for one suspect is refused with `E_LIMIT`. `rdy suspects` still prints every suspect.
- **The backup from the 0.52.0 upgrade.** When rdy 0.52.0 upgraded a 0.51.0 project, it left a backup folder in the project's `.rdy` folder, beside the store: `.store-upgrade-<hash>-<hash>`. It is the rollback for that upgrade. rdy 0.53.0 keeps it until you delete it, and `rdy status` names it with its size. After you check the store, for example with `rdy fsck`, you may delete the folder.
- **Upgrade backups and removals.** A content removal now overwrites every upgrade backup of the store with zeros, and then deletes it. A backup from before the removal holds the removed text. A later upgrade deletes the backups of earlier upgrades after its checks pass, so a store keeps at most one. rdy never deletes the backup of an upgrade whose completion is pending.
- **A shorter "Why?" trail.** A block's trail on the site lists only the decisions that govern the block itself: through `decided_by`, `implements` or a direct `affects`, and their supersessions. A decision that reaches the block only through an entity that the block describes is no longer a row. The trail ends with one line instead, such as `Also decided about: Account and Install.`, with each entity linked. The line names at most five, then `and <n> more`. On one large project, the site's HTML fell from 374 MB to 33 MB.
- **New refusals for baselines.** A baseline file that does not verify is refused with the new code `E_PUBLICATION_BASELINE` and `The publication baseline does not verify.` A baseline from a project outside this project's lineage gets the same code and `The publication baseline comes from a project outside this project's lineage.` A baseline with a scope that is not a tag is refused with `E_BAD_INPUT` and `A publication baseline requires a tag.` `against` with `against_baseline` is refused with `E_BAD_INPUT`. A baseline path that is not a file is refused with `E_BAD_INPUT`, and a file that rdy cannot read with `E_IO`.
- **Replayed changes read landed.** When a change that you replayed into another view lands, the original change now reads landed too. A proposal of the original view leaves it out. Before, the landing refused that proposal as already landed.

### Fixes

- **Daemon memory with review pages open.** The site build that an open page holds now counts in the site's memory cap of 256 MiB, so the daemon's memory no longer grows with each open review page. On Linux with glibc, the daemon also returns the memory of a dropped build to the system. On a Linux test project with 590 landings and 9 review pages open, the daemon's memory went from 4.6 GB in rdy 0.52.0 to 1.2 GB.

### Project and build

- **rdy-adapter.** rdy-adapter adds the calls that rdyhub needs for its next steps. They change nothing in a project that rdy runs.
- **Dependencies.** No crate is new to the build, and no dependency changes.

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
- After each landing, the live site rebuilds every open page's scope before it answers the next request. With many review pages open, a review link can take minutes to answer.
- Suspects:
  - A suspect on a superseded decision suggests "update <block>, or repin it to <decision>". A repin does not clear such a suspect: update the block, or point it at the decision that supersedes it.
  - A suspect whose premise was superseded reads only `superseded`. It names neither the decision that supersedes it nor what changed.
  - `rdy_trace` can show no premises for a decision that has a `depends_on` link.
  - `rdy_view create` with a bare landing hash as `base` gives a fix about forking a view. Write the base as `landing:<hash>`.
- A content removal keeps the upgrade backup that a pending upgrade completion names, so that backup can still hold the removed text until the completion ends. Only a removal that finishes at an open after a crash can meet this.
- Snapshot deltas and the daemon:
  - An upgrade that rdy refuses, for example on a snapshot whose bytes are not canonical, keeps its backup. Every later open to write then refuses with `E_UPGRADE_BACKUP`. Move the backup folder aside, then open the project again. Readers still open the project. The refusal of the upgrade gives the fix "run rdy fsck", which does not help in this case.
  - The upgrade, and the load of a dump without the feature, keep all new snapshot deltas in memory. A very long history of very large projects, such as 2,500 landings of 100,000 blocks each, can run out of memory. Nothing is written in that case.
  - `rdy fsck` accepts a snapshot delta that sets a block version that the store does not hold, when no removal took that version.
  - `rdy fsck`, dump and load accept snapshot delta bytes that are not canonical. A take refuses them.
  - The snapshot cache can hold more than its budget of 64 MiB. One test held about 97 MB.
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
  - On a slow or busy Windows machine, a code check can end with `W_HOOK_UNAVAILABLE` (deadline), because it has 2 seconds. The commit still goes ahead.
- Markdown import:
  - A local link whose text has formatting keeps the raw Markdown: `[**bold**](a.md)` shows `**bold**`. The report has no row for it.
  - Front matter that rdy does not support becomes a YAML code block, not plain text.
  - In a file whose lines end with a carriage return only, rdy does not read the front matter, and the report gives wrong line numbers.
  - When rdy converts a reference link, it removes the link's definition even when an image still uses it.
  - A source folder that is a symbolic link is imported. If the folder is replaced while rdy reads it, rdy can read the old folder.
  - A folder with several hundred empty subfolders can fail with `E_IO` "Too many open files" on a system that allows 256 open files.
  - When the first write of an import fails and rdy cannot undo it, the refusal reads `E_IO` "index error", not "import outcome is indeterminate". Reopen the project before you retry.
- Holders:
  - A take counts a holder's objects and edges as upper bounds. These counts grow faster than the holder check's counts, by about 3 objects for each signed release. Near the limit of 1,000,000, a take can refuse a holder that the holder check would pass.
  - In a take's check, a held landing whose previous landing is not in the index counts as depth zero. Only a damaged log can hold such a landing.
  - The holder check's limit of 1,000,000 objects is the next limit for a large project. A holder of a project with large landings can reach it after about 7,900 landings.

## Third-party source (MPL-2.0)

rdy is licensed MIT OR Apache-2.0. The binaries also contain two libraries under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/): `imbl` 7.0.2 and `imbl-sized-chunks` 0.2.0. This release attaches the exact source code of each, as its crates.io crate file. Each SHA-256 matches rdy's `Cargo.lock` at this release:

- [imbl-7.0.2.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.53.0/imbl-7.0.2.crate) — SHA-256: `46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec`
- [imbl-sized-chunks-0.2.0.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.53.0/imbl-sized-chunks-0.2.0.crate) — SHA-256: `2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6`

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
'mkdir' '/path/to/unpack' '/path/to/rdy-0.53.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.53.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.53.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.53.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.53.0/bin/rdy' &&
'/path/to/rdy-0.53.0/bin/rdy' '--version'
```

On Linux amd64:

```sh
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'sha256sum' '-c' '--ignore-missing' 'SHA256SUMS' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.53.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.53.0-x86_64-unknown-linux-gnu.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.53.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.53.0-x86_64-unknown-linux-gnu/rdy' '/path/to/rdy-0.53.0/bin/rdy' &&
'/path/to/rdy-0.53.0/bin/rdy' '--version'
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
foreach ($dir in @('C:\path\to\unpack', 'C:\path\to\rdy-0.53.0')) { if (Test-Path -LiteralPath $dir) { throw ($dir + ' exists') }; [void][IO.Directory]::CreateDirectory($dir) }
& (Join-Path $env:SystemRoot 'System32\tar.exe') '-xzf' 'C:\path\to\download\rdy-0.53.0-x86_64-pc-windows-msvc.tar.gz' '-C' 'C:\path\to\unpack'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
[void][IO.Directory]::CreateDirectory('C:\path\to\rdy-0.53.0\bin')
[IO.File]::Copy('C:\path\to\unpack\rdy-0.53.0-x86_64-pc-windows-msvc\rdy.exe', 'C:\path\to\rdy-0.53.0\bin\rdy.exe')
& 'C:\path\to\rdy-0.53.0\bin\rdy.exe' '--version'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
}
# Add the version's bin directory to PATH in this session:
# $env:Path = 'C:\path\to\rdy-0.53.0\bin;' + $env:Path
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
'xattr' '-d' 'com.apple.quarantine' '/path/to/download/rdy-0.53.0-aarch64-apple-darwin.tar.gz' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.53.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.53.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.53.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.53.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.53.0/bin/rdy' &&
'/path/to/rdy-0.53.0/bin/rdy' '--version'
```

Add the version's bin directory to PATH in this shell.
Replace `<version-prefix>` with the version prefix, such as `/path/to/rdy-0.53.0`:

```sh
export PATH="<version-prefix>/bin:$PATH"
```

</details>
