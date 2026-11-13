**Your intent, ready for anyone who needs it.** rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents. Before an agent writes, rdy shows it what governs the change and what depends on it. After the write, rdy shows what the change affected. Nothing joins the accepted design until a person approves it.

This release adds what a README needs: collapsed sections, a dark image for a figure, and an export of one doc as one Markdown file. rdy-core's own README now comes from rdy this way. On a terminal, signing in to rdyhub is now one command. A project's first dark image turns on a new store feature, and older rdy then refuses that project. This release keeps the compatibility promise of Release 1. See "About Release 1" below.

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
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.51.0/rdy-0.51.0-aarch64-apple-darwin.tar.gz
tar -xzf rdy-0.51.0-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.51.0-aarch64-apple-darwin/rdy ~/.local/bin/
```

**Linux (x86_64):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.51.0/rdy-0.51.0-x86_64-unknown-linux-gnu.tar.gz
tar -xzf rdy-0.51.0-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.51.0-x86_64-unknown-linux-gnu/rdy ~/.local/bin/
```

**Windows (x86_64), in PowerShell:**

```powershell
Invoke-WebRequest -OutFile rdy-0.51.0-x86_64-pc-windows-msvc.tar.gz https://github.com/adrenickchrome/rdy-core/releases/download/v0.51.0/rdy-0.51.0-x86_64-pc-windows-msvc.tar.gz
tar.exe -xzf rdy-0.51.0-x86_64-pc-windows-msvc.tar.gz
New-Item -ItemType Directory -Force "$HOME\.local\bin" | Out-Null
Move-Item -Force rdy-0.51.0-x86_64-pc-windows-msvc\rdy.exe "$HOME\.local\bin\"
```

On macOS and Linux, if `~/.local/bin` is not on your PATH, add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile. On Windows, add `$HOME\.local\bin` to your PATH. Then check:

```sh
rdy --version
```

It prints `rdy 0.51.0`.

On macOS, if you downloaded the archive in a browser instead of with curl, macOS may block the binary. Run `xattr -d com.apple.quarantine ~/.local/bin/rdy` once.

## Release key

This release is signed by rdy's machine release key, the same key that signed 0.46.0, 0.47.0, 0.48.0, 0.49.0 and 0.50.0. To check the signature of your download, follow "Verify the download" in the README. The allowed-signers line for v0.51.0:

```text
rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8
```

Fingerprint: `SHA256:Tj8hfra0U07asaeSZkt1b5e1HGJ+w8vMzLrn+G6/+/M`.

Compare this line and fingerprint with the ones in the README at v0.51.0. Use the key only when both match.

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

   A session lasts as long as one `rdy mcp` process, and keeps one actor and one run. To start another run, start a new `rdy mcp` process. A client that starts a new process for each call must start a new session each time.

   The agent starts its session with `rdy_session`, which names its own `agent:` actor, such as `agent:claude`, and a run. rdy's guide tells it to. Approving stays with you: an agent cannot start an approver session over MCP.

3. **Set up rdy for your agents.** In the project folder, run:

   ```sh
   rdy setup
   ```

   It creates your own rules project, the meta project, if you have none. It also adds three lines at the top of `CLAUDE.md` and `AGENTS.md` that tell the agent to call `rdy_session`, then `rdy_instructions`, at the start of each task. That call gives the agent rdy's guide, so you download nothing.

4. **Ask your agent for something real.** For example:

   > Start an rdy session in your own view, then read its overview. Help me design retention for an AI support assistant. Our intent is to keep only the customer data we need, for as short a time as we can, and be able to explain the choices. Start with a decision to delete raw conversations after 30 days and keep anonymised metrics for 2 years. Create the storage docs and link them to the decision. Propose the first design for my approval.

   The agent first loads its rules with `rdy_instructions`. The rules give a `kungfu` value for each tool that they cover, and the agent passes that value on each call to the tool. A call without it still runs, with the warning `W_KUNGFU`. In a new project nothing has landed yet, so the first writes need no brief. A later write that touches landed content needs a token from `rdy_brief`, which takes block ids or a path. With neither, `rdy_brief` refuses with `E_BAD_INPUT`.

   A doc type can ask for sections: an `overview` doc needs Summary and Goals. Until the doc has them, a write gives the warning `W_SKELETON_MISSING`, and the agent adds them. It is a warning, not a refusal.

5. **Review and approve it yourself.**

   ```sh
   rdy pending
   ```

   This shows what waits for you, with the impact of each proposal. Approve with `rdy approve P-<id>`, or tell your agent your answer. Without `@<n>`, `rdy approve` takes the proposal's latest version, which the card shows as `P-<id>@<n>`. If you answer a card in chat with Later, it stays hidden until the chat session changes, and `rdy pending` counts it as snoozed. When a proposal adds a decision, its option reads "Approve P-\<id\> and confirm D-\<id\>": approving it also confirms the decision.

6. **Read the result.**

   ```sh
   rdy site
   ```

   Open the link. The home page links to each page, and each page's address ends in `.html`. Now ask your agent: “For EU customers, raw conversations must be deleted after 7 days. Keep the 30-day rule elsewhere and the 2-year metrics window.” The agent briefs on the decision and its connected docs, follows the consequences, and brings you the proposal to approve.

   A decision gets a card of its own only in some cases. For example, it supersedes or revokes a live decision, it is `one_way`, or no content applies it yet. If the agent refines the first decision and applies the new one in the docs, you get one card.

   If `rdy pending` shows more than one card, answer each one. Until you confirm a decision's own card, `rdy approve` refuses with `E_EXCEPTION_OPEN` and names the card. Answer it with `rdy answer <ref> confirm`, where `<ref>` is the card's ref in `rdy pending`, such as `D-<id>@<hash>`. Then run `rdy approve P-<id>` again. Or tell your agent your answer.

   After an approval, `rdy approve` can list `consequences:`, with lines such as `dependent k-<id>: confirm (neighbor k-<id>)` and more lines under each. That list names nearby blocks that the change may touch. It is not a card, and nothing waits for you unless `rdy pending` shows it.

   New content shows a **Planned** label. A new decision starts planned, so the content that cites it does too. Set `built` on the decision when the work ships.

   On a remote machine, the link is on `127.0.0.1`. Open an SSH tunnel to its port first, for example `ssh -L <port>:127.0.0.1:<port> <host>`.

   The site keeps running after `rdy site` exits. To stop it, close your agent's session first, then run `rdy serve --stop` in the project folder. An `rdy mcp` that is still open logs `E_DAEMON_GONE`. `rdy serve --stop` prints `stopped`, or `no daemon running` when none was running. Your meta project has its own daemon, which starts at the agent's first `rdy_instructions` call. Its default place is `.rdy/meta` in your home folder, so stop it with `rdy serve --stop --project ~/.rdy/meta`.

## What changed since 0.50.0

### New features

- **Collapsed sections.** A section takes the field `collapsed: true`. The site shows it as a fold, closed when the page loads, with the section's heading as its label. A link, a search result, an "On this page" entry or the browser's history opens the target's fold and the folds around it. A review page opens a fold that holds a change. Search and the `llms` files read the text in a fold. A PDF and a single HTML file show the section open, and Markdown writes it as `<details>`.
- **A dark image for a figure.** A figure takes `dark_blob` and `dark_media_type`, set together, and its alt text describes both images. The site shows the dark image when its Auto, Light and Dark setting selects the dark theme. A PDF and a single HTML file show the light image. Markdown writes the pair as a `<picture>` element. The first dark image turns on a new store feature: see "Changes".
- **Images from another host in text.** Block text takes `![alt](url)`, alone or as the label of a link. The alt text must not be empty, and the URL must use HTTPS. The site, a PDF and a single HTML file show the alt text and load nothing from another host. Markdown keeps the image, so a README on GitHub shows it.
- **One doc as one Markdown file.** `rdy export --format md --layout single --out <file>` writes the one doc that the selection holds as one Markdown file, with no front matter. Select the doc with `--profile`. A ref to a block of the doc, outside a collapsed section, becomes a `#slug` link. Any other ref becomes its label, with a warning. A figure needs `--assets <dir>`, a folder below the folder of `--out`, and rdy writes each image there, named by its SHA-256. `rdy_export` takes the same `layout: "single"` and `assets`.
- **Paragraphs and code in list items.** A list item in block text can hold more paragraphs and fenced code. Indent each part past the item's marker, and put a blank line before a new paragraph. A fence of three backticks or tildes opens the code, with an optional language, and the same fence closes it. The code is literal, and it shows as a code block.
- **Sign in to rdyhub with one command.** On a terminal, `rdy auth sign-in --install <origin>` asks `Sign in to <origin> as <machine>? [y/N]` in place. After a yes, it prints the code and the link and offers to open the link in your browser. Then it waits until you are signed in, denied or expired. `--machine-name` is now optional, and its default is the host name, cut to 64 characters. rdy sends the machine name to the hub when the sign-in starts. The prompt shows it before you say yes, and `--machine-name` sets another. Without a terminal, or with `--json`, `--card-hash`, `--answer` or `--relayed-by`, the card and relay path stays as it was. In `rdy_auth`, `machine_name` is optional for `start` too.
- **A pending sign-in finishes on the next hub call.** A hub call that finds a pending sign-in whose next check is due checks it once, as `rdy auth state` does. So after you confirm in the browser, the next hub call works without a `state` call.

### Changes

- **Breaking for older rdy: rdy 0.51.0 adds the store feature `figure_dark` (class incompatible, minimum rdy 0.51.0); store format 19 and dump format 4 stay.** A store starts to use `figure_dark` only when its project first writes a figure with a dark image (in any view), and only a store or dump that uses it needs rdy 0.51.0: rdy 0.50.0 refuses one with `E_STORE_FORMAT` and names rdy 0.51.0. rdy 0.51.0 opens a store made by rdy 0.50.0 as it is, and loads every earlier dump.
- **`figure_dark` never turns off.** It stays on even when you remove that figure later. Once it is on, rdy 0.50.0 and older refuse the store and its dumps with `E_STORE_FORMAT`: `store feature figure_dark needs rdy 0.51.0`. A project with no dark image stays open to older rdy. So add no dark image while someone still uses an older rdy on the project. The sync stream formats stay too.
- **You do nothing to move.** If an older rdy wrote to the store after rdy 0.50.0, the first open with rdy 0.51.0 rebuilds the index once. That is not damage.
- **The schema pack changes.** The built-in pack adds the optional `collapsed` to `section` and the two dark fields to `figure`, so its digest changes. The ids already in a store do not move. New landings hash the new pack.
- **An indented paragraph in a list item is now its own paragraph.** Before, it joined the item's first paragraph. The stored text and its hashes do not change. Only the site, the prints and the exports change.
- **A fence in a list item now opens code.** Its text is literal: a ref, an escape or other markup in it shows as typed. A ref in that code is not checked and adds no link.
- **New refusals, with no new codes.** A figure's `dark_blob` gets the checks of `blob`, under its own name, such as `<id>'s dark_blob <value> is not a blob hash`. One dark field without the other is `E_VALIDATION`: `<id>'s dark_blob and dark_media_type are set together`. The single-file export refuses with `E_BAD_INPUT` when the selection holds more than one doc or a figure has no `--assets`. It also refuses an assets folder that is not empty or not below the folder of `--out`. `--assets` with another format or layout is `E_BAD_INPUT` too: `--assets applies to --format md --layout single only`.
- **The agent guide covers the new features.** rdy's built-in guide has new rules for them. They cover paragraphs and code in list items, collapsed sections, images from another host, dark figures, the single-file export and the optional machine name. The rule for dark figures tells the agent to ask the person before the first one.

### Fixes

- **Uploads from a long history.** An upload to rdyhub now counts only the records and edges that it sends against its limits of 1,000,000 each. Before, a project with a long history could fail even an upload of one landing with `stream exceeds 1000000 graph edges`. The upload's walk through the history also uses less memory.
- **Counts catch up after an older rdy.** When an rdy older than 0.50.0 wrote to a holder after rdy 0.50.0, the counts that a take checks fell behind the log. A take could then accept past the holder's limits, until a `rdy reindex`. rdy 0.51.0 sees counts that are behind the log at open, and rebuilds the index once.
- **Daemon memory with an open site.** The project daemon no longer grows while its live site is open. A site build held each figure in a buffer of up to twice the figure's size. On Linux with glibc, the freed figure memory also stayed with the process, so the daemon grew after each landing. rdy now reads each figure into one buffer of its size. On Linux with glibc, the daemon gives freed figure memory back to the system. No setting changes.

### Project and build

- **The README comes from rdy.** rdy-core's README is now one doc in rdy, exported as one Markdown file at the landing that each release freezes for its notes. The release step adds the archives, the signing key, the release keys and the install commands to its last section. `qa-release-readme` checks the result byte for byte.
- **rdy-store** gives tools that build on it, such as rdyhub: `closure_within` with `WalkBounds`, `FEATURE_FIGURE_DARK`, `FIGURE_DARK_REQUIRED_RDY`, `FIGURE_TYPE`, `DARK_BLOB_FIELD`, `is_dark_figure`, `counts_behind` and `pio::host_name`. `REGISTERED_FEATURES` now lists nine features, and `BatchReport` has the new field `record_features`. The test hook `closure_kept_links_for_tests` is public in release builds too. Do not call it outside tests.
- **rdy-adapter** gives `AttachmentVariant`, and `AttachmentEntry` has the new field `variant`.
- **Dependencies.** No crate is new to the build. rdy-render now uses `sha2`, already in the workspace, for the names of exported images. rdy-store turns on the `windows-sys` feature `Win32_System_SystemInformation`, for the host name on Windows.
- **Build.** The website notice skips a `docs-v` release instead of failing. The end-to-end hook tests retry under load.

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

- [imbl-7.0.2.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.51.0/imbl-7.0.2.crate) — SHA-256: `46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec`
- [imbl-sized-chunks-0.2.0.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.51.0/imbl-sized-chunks-0.2.0.crate) — SHA-256: `2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6`

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
'mkdir' '/path/to/unpack' '/path/to/rdy-0.51.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.51.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.51.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.51.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.51.0/bin/rdy' &&
'/path/to/rdy-0.51.0/bin/rdy' '--version'
```

On Linux amd64:

```sh
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'sha256sum' '-c' '--ignore-missing' 'SHA256SUMS' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.51.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.51.0-x86_64-unknown-linux-gnu.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.51.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.51.0-x86_64-unknown-linux-gnu/rdy' '/path/to/rdy-0.51.0/bin/rdy' &&
'/path/to/rdy-0.51.0/bin/rdy' '--version'
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
foreach ($dir in @('C:\path\to\unpack', 'C:\path\to\rdy-0.51.0')) { if (Test-Path -LiteralPath $dir) { throw ($dir + ' exists') }; [void][IO.Directory]::CreateDirectory($dir) }
& (Join-Path $env:SystemRoot 'System32\tar.exe') '-xzf' 'C:\path\to\download\rdy-0.51.0-x86_64-pc-windows-msvc.tar.gz' '-C' 'C:\path\to\unpack'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
[void][IO.Directory]::CreateDirectory('C:\path\to\rdy-0.51.0\bin')
[IO.File]::Copy('C:\path\to\unpack\rdy-0.51.0-x86_64-pc-windows-msvc\rdy.exe', 'C:\path\to\rdy-0.51.0\bin\rdy.exe')
& 'C:\path\to\rdy-0.51.0\bin\rdy.exe' '--version'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
}
# Add the version's bin directory to PATH in this session:
# $env:Path = 'C:\path\to\rdy-0.51.0\bin;' + $env:Path
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
'xattr' '-d' 'com.apple.quarantine' '/path/to/download/rdy-0.51.0-aarch64-apple-darwin.tar.gz' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.51.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.51.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.51.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.51.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.51.0/bin/rdy' &&
'/path/to/rdy-0.51.0/bin/rdy' '--version'
```

Add the version's bin directory to PATH in this shell.
Replace `<version-prefix>` with the version prefix, such as `/path/to/rdy-0.51.0`:

```sh
export PATH="<version-prefix>/bin:$PATH"
```

</details>
