**Your intent, ready for anyone who needs it.** rdy stores what you intend and decided, and gives it to whoever needs it in a form they can use: a docs site for people, briefings and instructions for agents. Before an agent writes, rdy shows it what governs the change and what depends on it. After the write, rdy shows what the change affected. Nothing joins the accepted design until a person approves it.

This is **Release 1**, the first release of rdy with a compatibility promise. See "About Release 1" below.

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
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.47.0/rdy-0.47.0-aarch64-apple-darwin.tar.gz
tar -xzf rdy-0.47.0-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.47.0-aarch64-apple-darwin/rdy ~/.local/bin/
```

**Linux (x86_64):**

```sh
curl -LO https://github.com/adrenickchrome/rdy-core/releases/download/v0.47.0/rdy-0.47.0-x86_64-unknown-linux-gnu.tar.gz
tar -xzf rdy-0.47.0-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin && mv rdy-0.47.0-x86_64-unknown-linux-gnu/rdy ~/.local/bin/
```

**Windows (x86_64), in PowerShell:**

```powershell
Invoke-WebRequest -OutFile rdy-0.47.0-x86_64-pc-windows-msvc.tar.gz https://github.com/adrenickchrome/rdy-core/releases/download/v0.47.0/rdy-0.47.0-x86_64-pc-windows-msvc.tar.gz
tar.exe -xzf rdy-0.47.0-x86_64-pc-windows-msvc.tar.gz
New-Item -ItemType Directory -Force "$HOME\.local\bin" | Out-Null
Move-Item -Force rdy-0.47.0-x86_64-pc-windows-msvc\rdy.exe "$HOME\.local\bin\"
```

On macOS and Linux, if `~/.local/bin` is not on your PATH, add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile. On Windows, add `$HOME\.local\bin` to your PATH. Then check:

```sh
rdy --version
```

It prints `rdy 0.47.0`.

On macOS, if you downloaded the archive in a browser instead of with curl, macOS may block the binary. Run `xattr -d com.apple.quarantine ~/.local/bin/rdy` once.

## Release key

This release is signed by rdy's machine release key, the same key that signed 0.46.0. To check the signature of your download, follow "Verify the download" in the README. The allowed-signers line for v0.47.0:

```text
rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8
```

Fingerprint: `SHA256:Tj8hfra0U07asaeSZkt1b5e1HGJ+w8vMzLrn+G6/+/M`.

Compare this line and fingerprint with the ones in the README at v0.47.0. Use the key only when both match.

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

   On Windows, give the native path, such as `C:\Users\ana\my-project`, and write each backslash twice in JSON.

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

## What changed since 0.46.0

### New features

- **Windows.** rdy runs on Windows 11 on amd64, and this release has a native Windows archive. The daemon listens on a named pipe that only your account can open. Give native paths, such as `rdy mcp --project C:\Users\ana\shop`, and write each backslash twice in JSON. To sign, set `SSH_AUTH_SOCK` to the pipe of the Windows OpenSSH agent, `\\.\pipe\openssh-ssh-agent`. `rdy edit` runs `%VISUAL%`, else `%EDITOR%`, else `notepad.exe`.
- **`rdy config` lists every key.** It shows each key with its value, or `unset`, and the levels that may set it: local, project or user. Before, it showed only the keys that had a value.
- **The release check shows what a profile hides.** `rdy publication check` lists the docs whose planned blocks a public profile hides, in `hidden_planned`. With `--against`, it also lists the blocks that are newly hidden.
- **The live site always gets a port.** When all 16 candidate ports of the site refuse, the site takes a port that the operating system assigns. The daemon log names it: `site: ports <first>..<last> refused; using OS-assigned port <port>`. A port that you set with `serve.site_port` has no fallback.

### Changes

- **Release 1.** The compatibility promise starts with this release. See "About Release 1" below.
- **No preview mark.** `rdy --version` prints `rdy 0.47.0`. The archive names have no `-preview` part.
- **No store or dump format change since 0.46.0.** The dump format is still 4. Replace the binary, and keep your projects as they are. If a daemon of the old rdy still runs for a project, run `rdy serve --stop` there once.
- **Plain paths in messages.** On Windows, every message shows a plain drive path, such as `C:\Users\ana\shop`, never the `\\?\` form.
- **No console window for git.** On Windows, a background daemon runs git with no console window of its own, so no window opens.
- **An unreadable pack file is named.** When rdy cannot open or read a file that defines the pack, the `E_IO` message names the file: `could not open <path>: <error>` or `could not read <path>: <error>`.

### Fixes

- **The old daemon cannot write to a replaced store.** Before, when a project's store was replaced while its daemon still wrote, the daemon's last write could reach the new store. Now `rdy load` and a new daemon wait until the old daemon's last write ends.
- **The live site starts on Windows when ports are reserved.** Hyper-V, WSL or Docker can reserve the site's candidate ports, and the site then did not start. Now it takes a port that the operating system assigns.

### Project and build

- **rdy-adapter** re-exports the format capability checks and the actor form check (`is_actor`, `MAX_ACTOR_BYTES`), for tools that build on it, such as rdyhub.

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

## Third-party source (MPL-2.0)

rdy is licensed MIT OR Apache-2.0. The binaries also contain two libraries under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/): `imbl` 7.0.2 and `imbl-sized-chunks` 0.2.0. This release attaches the exact source code of each, as its crates.io crate file. Each SHA-256 matches rdy's `Cargo.lock` at this release:

- [imbl-7.0.2.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.47.0/imbl-7.0.2.crate) — SHA-256: `46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec`
- [imbl-sized-chunks-0.2.0.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.47.0/imbl-sized-chunks-0.2.0.crate) — SHA-256: `2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6`

Each archive's `THIRD-PARTY-NOTICES.txt` gives the license of every library in the binary.

<details>
<summary><b>About Release 1</b></summary>

This is Release 1, the first release of rdy with a compatibility promise.
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

Every release is signed. `SHA256SUMS` lists the three archives, and `SHA256SUMS.sig` is its OpenSSH signature (namespace `rdy-release-v1`, principal `rdy-release`). rdy's machine release key signs this release.

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
'mkdir' '/path/to/unpack' '/path/to/rdy-0.47.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.47.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.47.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.47.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.47.0/bin/rdy' &&
'/path/to/rdy-0.47.0/bin/rdy' '--version'
```

On Linux amd64:

```sh
cd '/path/to/download' &&
'printf' '%s\n' 'rdy-release namespaces="rdy-release-v1" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIUyk+U70yjuD/Ixg86ti9BBo0Kc/NL0ps3KAOAIVC/8' > '/path/to/download/release-allowed-signers' &&
'ssh-keygen' '-Y' 'verify' '-f' 'release-allowed-signers' '-I' 'rdy-release' '-n' 'rdy-release-v1' '-s' 'SHA256SUMS.sig' < '/path/to/download/SHA256SUMS' &&
'sha256sum' '-c' '--ignore-missing' 'SHA256SUMS' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.47.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.47.0-x86_64-unknown-linux-gnu.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.47.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.47.0-x86_64-unknown-linux-gnu/rdy' '/path/to/rdy-0.47.0/bin/rdy' &&
'/path/to/rdy-0.47.0/bin/rdy' '--version'
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
foreach ($dir in @('C:\path\to\unpack', 'C:\path\to\rdy-0.47.0')) { if (Test-Path -LiteralPath $dir) { throw ($dir + ' exists') }; [void][IO.Directory]::CreateDirectory($dir) }
& (Join-Path $env:SystemRoot 'System32\tar.exe') '-xzf' 'C:\path\to\download\rdy-0.47.0-x86_64-pc-windows-msvc.tar.gz' '-C' 'C:\path\to\unpack'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
[void][IO.Directory]::CreateDirectory('C:\path\to\rdy-0.47.0\bin')
[IO.File]::Copy('C:\path\to\unpack\rdy-0.47.0-x86_64-pc-windows-msvc\rdy.exe', 'C:\path\to\rdy-0.47.0\bin\rdy.exe')
& 'C:\path\to\rdy-0.47.0\bin\rdy.exe' '--version'
if ($LASTEXITCODE -ne 0) { throw 'a step failed' }
}
# Add the version's bin directory to PATH in this session:
# $env:Path = 'C:\path\to\rdy-0.47.0\bin;' + $env:Path
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
'xattr' '-d' 'com.apple.quarantine' '/path/to/download/rdy-0.47.0-aarch64-apple-darwin.tar.gz' &&
'mkdir' '/path/to/unpack' '/path/to/rdy-0.47.0' &&
'tar' '-xzf' '/path/to/download/rdy-0.47.0-aarch64-apple-darwin.tar.gz' '-C' '/path/to/unpack' &&
'mkdir' '-p' '/path/to/rdy-0.47.0/bin' &&
'install' '-m' '755' '/path/to/unpack/rdy-0.47.0-aarch64-apple-darwin/rdy' '/path/to/rdy-0.47.0/bin/rdy' &&
'/path/to/rdy-0.47.0/bin/rdy' '--version'
```

Add the version's bin directory to PATH in this shell.
Replace `<version-prefix>` with the version prefix, such as `/path/to/rdy-0.47.0`:

```sh
export PATH="<version-prefix>/bin:$PATH"
```

</details>
