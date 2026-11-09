**Your intent, ready for anyone who needs it.** rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents. Before an agent writes, rdy shows it what governs the change and what depends on it. After the write, rdy shows what the change affected. Nothing joins the accepted design until a person approves it.

This is a **preview**: things will change before Release 1. See "About this preview" below.

## What you get

- **Docs and decisions as one project.** Write docs, requirements, terms and decisions as linked blocks, not loose files.
- **Agents work through MCP.** Claude Code, or any MCP client, reads and writes through rdy's tools. Each agent gets a briefing before it edits.
- **Instructions for agents.** rdy's own agent guide is built in. Keep your rules in rdy too, each with its reason. Every agent gets the rules it needs at the start of a task, and your own rules follow you to every project.
- **Proposals and approvals.** Agent work lands in a view. You see the impact, then approve it or not.
- **Docs that match the release.** Record which release ships each decision. A public site then shows only what has shipped, and a release check catches content that is ahead of the build.
- **It catches drift.** When a decision changes, rdy marks every page that rests on it. It also flags a page that states a different number for a recorded value.
- **A live docs site.** `rdy site` gives you a local site that refreshes as changes land.
- **Local and private.** Everything runs on your machine: no account, no hosted service, no model inside rdy.

## Install (about a minute)

**macOS (Apple silicon):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.46.0/rdy-0.46.0-preview-aarch64-apple-darwin.tar.gz
tar -xzf rdy-0.46.0-preview-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.46.0-preview-aarch64-apple-darwin/rdy ~/.local/bin/
```

**Linux (x86_64):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.46.0/rdy-0.46.0-preview-x86_64-unknown-linux-gnu.tar.gz
tar -xzf rdy-0.46.0-preview-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.46.0-preview-x86_64-unknown-linux-gnu/rdy ~/.local/bin/
```

If `~/.local/bin` is not on your PATH, add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile. Then check:

```sh
rdy --version
```

It prints `rdy 0.46.0 (preview)`.

On macOS, if you downloaded the archive in a browser instead of with curl, macOS may block the binary. Run `xattr -d com.apple.quarantine ~/.local/bin/rdy` once.

## Release key

This release is the first one that rdy's machine release key signs. The founder's YubiKey key signed v0.17.0 to v0.41.0. To check the signature of your download, follow "Verify the download" in the README. The allowed-signers line for v0.46.0:

```text
rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8
```

Fingerprint: `SHA256:Tj8hfra0U07asaeSZkt1b5e1HGJ+w8vMzLrn+G6/+/M`.

Compare this line and fingerprint with the ones in the README at v0.46.0. Use the key only when both match.

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

   The agent claims its own `agent:` actor, such as `agent:claude`, when it starts a session. rdy's guide tells it to. Approving stays with you: an agent cannot start an approver session over MCP.

3. **Set up rdy for your agents.**

   ```sh
   rdy setup
   ```

   It creates your own rules project, the meta project, if you have none. It also adds three lines at the top of `CLAUDE.md` and `AGENTS.md` that tell the agent to call `rdy_session`, then `rdy_instructions`, at the start of each task. That call gives the agent rdy's guide, so you download nothing.

4. **Ask your agent for something real.** For example:

   > Start an rdy session in your own view, then read its overview. Help me design retention for an AI support assistant. Our intent is to keep only the customer data we need, for as short a time as we can, and be able to explain the choices. Start with a decision to delete raw conversations after 30 days and keep anonymised metrics for 2 years. Create the storage docs and link them to the decision. Propose the first design for my approval.

5. **Review and approve it yourself.**

   ```sh
   rdy pending
   ```

   This shows what waits for you, with the impact of each proposal. Approve with `rdy approve P-<id>`, or tell your agent your answer.

6. **Read the result.**

   ```sh
   rdy site
   ```

   Open the link. Now ask your agent: “For EU customers, raw conversations must be deleted after 7 days. Keep the 30-day rule elsewhere and the 2-year metrics window.” The agent briefs on the decision and its connected docs, follows the consequences, and brings you the proposal to approve.

   New content shows a **Planned** label. A new decision starts planned, so the content that cites it does too. Set `built` on the decision when the work ships.

   On a remote machine, the link is on `127.0.0.1`. Open an SSH tunnel to its port first, for example `ssh -L <port>:127.0.0.1:<port> <host>`.

   The site keeps running after `rdy site` exits. To stop it, run `rdy serve --stop` in the project folder. Your meta project has its own daemon: stop it with `rdy serve --stop --project ~/.rdy/meta`.

## What changed since 0.41.0

### New features

- **The agent guide is inside rdy.** `rdy_instructions` always gives a built-in `rdy` layer first. It holds a short required bootstrap and a menu of rulesets, with ids such as `rdy:write` and `rdy:approvals`. The agent loads the rulesets that its task needs. `full: true` (CLI: `rdy instructions --full`) gives every built-in ruleset. Your project's rules and your own rules follow, and they win where they differ. The exception is the rule that an agent never approves on its own judgment. The release no longer has an `AGENTS.md` or `CLAUDE.md` file to download.
- **Save a rule from chat.** When you state a rule, your agent saves it with `rdy_rule`, in one call from any project. A rule for all your work goes to your meta project, and a rule for this project goes to the project. The agent asks only when it is unsure. It then tells you where the rule went and shows its wording. The rule lands on your words, with no card. An authority policy can set `rules_from_chat: card` to get a card instead. The CLI twin is `rdy rule add`, `edit`, `remove` and `answer`, with your agent's actor in `--relayed-by`.
- **`rdy setup` creates your meta project.** When you have no meta project, it creates an empty one at the configured path. It leaves an existing one alone. Outside a project, that is all it does. `rdy init` names `rdy setup`, and `rdy status` adds a line while the meta project is missing.
- **Agents show that they hold their rules.** Each built-in ruleset gives a `kungfu` value for its tools. The agent passes it on each call. A missing or wrong value never stops a call. The reply then carries `W_KUNGFU`, once per ruleset in a session, and tells the agent to load the ruleset again. `rdy status` and `rdy_overview` show local counts. rdy sends nothing anywhere.

### Changes

- **No store or dump format change since 0.41.0.** The dump format is still 4. Replace the binary, and keep your projects as they are. If a daemon of the old rdy still runs for a project, run `rdy serve --stop` there once.
- **One new pack field.** A project that this rdy creates has the policy field `rules_from_chat` in its pack. So its derived ids differ from those of a project made by 0.41.0. An existing project needs no move.
- **Shorter MCP server instructions.** The detailed guidance now comes from the built-in rulesets through `rdy_instructions`.
- **A daemon stops when its store goes.** The daemon also stops when `.rdy/store` or its lock file is gone or replaced, not only `.rdy`.
- **Line ends in working-copy files.** rdy reads CR LF line ends in a working-copy file as LF.
- **The setup lines name `rdy_session` first.** The lines that `rdy setup` writes now say to call `rdy_session`, then `rdy_instructions`. An agent needs a session before `rdy_instructions` answers. `rdy setup` replaces an old version of the lines in place.

### Fixes

- **A table's body shows.** A table's body now shows as a paragraph right after the table: on the site, in a print and in a Markdown export. Before, only `rdy_read` and the dump showed it. A table with no body looks the same as before.

### Project and build

- **Windows.** rdy's code now runs on Windows 11 on amd64. On a native Windows 11 machine, it passes rdy's full native gate as a standard user. The hosted Windows CI job is advisory and not yet green. This release has archives for macOS and Linux only. The first Windows archive comes with Release 1.
- **CodeQL** no longer scans rdy's test folders.

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
- When a project's store is replaced while its daemon is still writing, the daemon's last write can reach the new store. Run `rdy serve --stop` in the project before you replace its store.
- The tool schemas show example ids, such as `D-W2HXR`, `FW-E3` and `M2c3`, that look like real ids. They are only examples. 0.41.0 showed them too.
- Your meta project has its own daemon. `rdy serve --stop` in a project does not stop it.

## Third-party source (MPL-2.0)

rdy is licensed MIT OR Apache-2.0. The binaries also contain two libraries under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/): `imbl` 7.0.2 and `imbl-sized-chunks` 0.2.0. This release attaches the exact source code of each, as its crates.io crate file. Each SHA-256 matches rdy's `Cargo.lock` at this release:

- [imbl-7.0.2.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.46.0/imbl-7.0.2.crate) — SHA-256: `46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec`
- [imbl-sized-chunks-0.2.0.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.46.0/imbl-sized-chunks-0.2.0.crate) — SHA-256: `2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6`

Each archive's `THIRD-PARTY-NOTICES.txt` gives the license of every library in the binary.

<details>
<summary><b>About this preview</b></summary>

This is a preview of rdy.
It comes before Release 1, the first release with a compatibility promise.

Until Release 1, any command, flag, tool, code or file format may change in the next preview, with no deprecation period.
One thing stays: every later rdy, Release 1 included, reads every dump that this rdy writes.

To move a project to a newer preview, run `rdy dump --out <file>` with this rdy.
Then run `rdy load <file> <dir>` with the new rdy, into a directory with no store.
A project made with a preview moves to Release 1 the same way.

Report problems at https://github.com/adrenickchrome/rdy-core/issues.

</details>

<details>
<summary><b>Verify the download (optional)</b></summary>

Every release is signed. `SHA256SUMS` lists both archives, and `SHA256SUMS.sig` is its OpenSSH signature (namespace `rdy-release-v1`, principal `rdy-release`). rdy's machine release key signs this release.

Release signing key:

```text
rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8
```

Fingerprint: SHA256:Tj8hfra0U07asaeSZkt1b5e1HGJ+w8vMzLrn+G6/+/M. It is the same as in the [README](https://github.com/adrenickchrome/rdy-core#release-signing-key).

This release also attaches `transition-v0.46.0.json` and `transition-v0.46.0.json.sig`. The historical root key signed this transition to the machine key. The README's "Release keys" section shows how to check it offline.

| Platform | Target | Tested on |
|---|---|---|
| macOS arm64 | `aarch64-apple-darwin` | macOS 26.6.2 |
| Linux amd64 | `x86_64-unknown-linux-gnu` | Ubuntu 26.04.1 LTS |

To check the signature and the hash before you install, download the archive, `SHA256SUMS` and `SHA256SUMS.sig` into one folder and run:

On macOS arm64, for an archive downloaded with curl:

```sh
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'shasum' '-a' '256' '-c' '--ignore-missing' 'SHA256SUMS' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.46.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.46.0-preview-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.46.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.46.0-preview-aarch64-apple-darwin/rdy' '/path/to/rdy-0.46.0/bin/rdy' &&
'/path/to/rdy-0.46.0/bin/rdy' '--version'
```

On Linux amd64:

```sh
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'sha256sum' '-c' '--ignore-missing' 'SHA256SUMS' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.46.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.46.0-preview-x86_64-unknown-linux-gnu.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.46.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.46.0-preview-x86_64-unknown-linux-gnu/rdy' '/path/to/rdy-0.46.0/bin/rdy' &&
'/path/to/rdy-0.46.0/bin/rdy' '--version'
```

On macOS arm64, for an archive downloaded with a web browser:

```sh
# In Safari, turn off “Open safe files after downloading” before downloading. Keep the original .tar.gz file.
# After both checks pass, remove com.apple.quarantine from the archive before extraction. Skip this command if the archive has no com.apple.quarantine attribute.
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'shasum' '-a' '256' '-c' '--ignore-missing' 'SHA256SUMS' &&
'xattr' '-d' 'com.apple.quarantine' '/path/to/download/rdy-0.46.0-preview-aarch64-apple-darwin.tar.gz' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.46.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.46.0-preview-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.46.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.46.0-preview-aarch64-apple-darwin/rdy' '/path/to/rdy-0.46.0/bin/rdy' &&
'/path/to/rdy-0.46.0/bin/rdy' '--version'
```

Add the version's bin directory to PATH in this shell.
Replace `<version-prefix>` with the version prefix, such as `/path/to/rdy-0.46.0`:

```sh
export PATH="<version-prefix>/bin:$PATH"
```

</details>
