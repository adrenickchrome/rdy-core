# Install a rdy preview

The managed installer downloads a published preview, verifies its signed checksums
with the embedded rdy release key, and installs it for your user. It never opens a
project, changes a project store, stops a daemon, or restarts one. The manual archive
instructions in the [README](../README.md) remain available.

This source introduces the installer. The existing v0.17.0 release has compatible
signed archives but does not include `install.sh`; do not assume every older release
has an installer asset. New release packets include the reviewed standalone script
and this document as `INSTALL.md`.

## Requirements and supported targets

| Host | Archive selected | Status |
| --- | --- | --- |
| Apple Silicon macOS, native shell | `aarch64-apple-darwin` | Published preview target |
| x86_64 GNU/Linux | `x86_64-unknown-linux-gnu` | Published preview target |
| Intel macOS, Linux ARM, musl/Alpine, Windows | None | Refused; no qualified archive offered |

The installer needs Python 3.9 or later, an HTTPS certificate store recognized by
Python, and OpenSSH `ssh-keygen` with SSHSIG verification (`-Y verify`) and support
for the pinned security-key signature. It reports a missing dependency rather than
installing one or falling back to an unsigned download. The complete Python program
is embedded in `install.sh`; no second helper program is fetched.

The published binaries were tested on macOS 26.6.2 ARM64 and Ubuntu 26.04.1 x86_64.
Those observations do **not** establish minimum supported OS or glibc versions.
Older-host qualification is a release acceptance job. The installer rejects the
wrong architecture/libc and runs the verified binary's bounded `--version` probe
before activation. A binary that cannot run leaves the prior installation active.
Running an Intel shell through Rosetta is refused; use a native Apple Silicon shell.

The initial archives are not Apple Developer ID signed or notarized. The rdy SSH
release signature authenticates the archive; it is not Apple notarization. This
installer does not remove quarantine, change Gatekeeper, or bypass OS warnings.

## Get the installer

Use `scripts/install.sh` from a reviewed checkout at an exact commit, or download
the `install.sh` asset from a release whose installer/source identity you have
reviewed. Save and inspect it before executing it. Do not pipe a moving branch or a
download directly into a shell. New release evidence records the installer digest
and its source commit separately from the two-archive `SHA256SUMS` format.

The trust boundary starts with that installer source: its embedded public key
authenticates the application archives, not its own download. A checksum fetched
beside an installer is not independent proof of the installer author's identity.
Prefer a reviewed immutable release or exact source commit, and retain that
installer when reproducibility matters.

From the reviewed checkout, install an exact existing preview:

```sh
sh scripts/install.sh --version 0.17.0
```

This creates `~/.local/bin/rdy` and retained versions under
`~/.local/share/rdy/versions/`. The prefix and its managed directories must be owned
by your user, must not be symlinks, and must not be writable by another user or
group. Do not use `sudo`. The installer leaves shell startup files alone; if needed,
add `~/.local/bin` to your shell's `PATH` yourself.

Choose another user-owned prefix without changing an existing installation:

```sh
sh scripts/install.sh --version 0.17.0 --prefix "$HOME/rdy-preview"
"$HOME/rdy-preview/bin/rdy" --version
```

An existing unmanaged `rdy` file or external symlink in that prefix is never
overwritten. Choose a separate prefix, or relocate your old executable deliberately.
The script refuses installation as root.

To choose the newest supported published preview from GitHub's first 100 releases:

```sh
sh scripts/install.sh --channel preview
```

The channel accepts only public, non-draft prereleases tagged `v0.N.0`, and selects
the greatest numeric `N`. It does not use GitHub's `/releases/latest`, which excludes
prereleases. It resolves once, prints the exact version, and then downloads the exact
tag's assets. Use `--version` for a reproducible choice. If GitHub API access is
rate-limited or a release is incomplete, installation fails without activation.
The installer sends no GitHub token and reads no credential cache.

## Signature and download behavior

The embedded signer is the founder-confirmed offline release key:

```text
principal:   rdy-release
namespace:   rdy-release-v1
fingerprint: SHA256:Baa52ZPJSzazbmd2KDEkzUM6OYTnl8mznJXsmnwvvJc
```

There are no `--skip-verify`, signer, or arbitrary-download-URL overrides. The
installer verifies `SHA256SUMS.sig` over the exact manifest, requires exactly the
two supported archive names, and hashes the selected archive before extracting or
executing it. Public-key rotation requires a reviewed installer update.

Downloads use certificate-verified HTTPS, bounded redirects to named GitHub asset
hosts, size limits, and timeouts. Archives have bounded expanded size and exactly
the expected binary, license, and preview files. Paths outside that layout,
duplicates, links, devices, setuid metadata, and unexpected extended metadata are
refused. Extraction does not use the archive's paths as arbitrary filesystem paths.

## Offline installation

Place these three files from the same exact release in one directory:

- `SHA256SUMS`
- `SHA256SUMS.sig`
- The archive for your host, for example
  `rdy-0.17.0-preview-aarch64-apple-darwin.tar.gz`

Then run:

```sh
sh scripts/install.sh --version 0.17.0 --offline-dir "$HOME/Downloads/rdy-packet"
```

Offline mode uses the same pinned signature, manifest, archive, platform, and
activation checks. It requires an exact version and makes no network requests.
It does not accept symlink inputs. The other target's archive is not needed on this
host, but both entries must remain in the signed checksum manifest.

## Upgrade and preview-store compatibility

Run the installer again with an explicit new preview version. It prepares and
checks the new binary in a private temporary directory, writes a local receipt,
retains the version, and atomically switches the managed command symlink. A lock
prevents two installer processes from changing the same prefix concurrently. A
failure before activation leaves the old command active. Retained version receipts
detect accidental binary changes; they are not protection from a compromised user
account that can rewrite both receipt and binary.

Preview project stores can change between releases. Before using a newer binary
with valuable work, use the **old** binary to dump the project, load the dump with
the **new** binary into a new project directory, and run `fsck`. Keep the old project
and binary until you verify the result. Later preview binaries must read released
dumps; an older binary is not promised to read a newer project store. Follow the
release notes for the precise supported dump/load commands and compatibility test.

The installer never tests against your project directories and never performs an
automatic store migration. Existing daemons keep their current process/binary;
stop or restart them deliberately through supported rdy commands after checking
your work and release compatibility.

Select a previously retained binary:

```sh
sh scripts/install.sh --activate 0.17.0
```

This verifies its stored receipt and binary hash, then changes the command link.
It does **not** roll project data back. Do not open a newer-format project with the
older binary. A separate old project or a compatible verified dump is required.

## Uninstall

```sh
sh scripts/install.sh --uninstall
```

This removes only the recognized managed command link. It retains downloaded
versions/receipts, all project directories, dumps, configuration, and daemons. It
prints the retained versions directory so you can remove that directory separately
when you no longer need the old binaries. Use the same `--prefix` if you installed
outside `~/.local`.

## Local verification

```sh
sh -n scripts/install.sh
python3 -m unittest discover -s tests/installer -v
```

The fixture suite uses a temporary test signing key only inside the imported test
module. The shipped CLI has no test-key option. Tests exercise actual OpenSSH
verification, tampering, malformed archives, unsafe prefixes, retained-version
activation, failure before activation, and concurrent locks without a network or
existing rdy installation. Native release acceptance additionally installs the
real signed candidate on each qualified target. A local macOS result alone does
not establish native Linux compatibility.
