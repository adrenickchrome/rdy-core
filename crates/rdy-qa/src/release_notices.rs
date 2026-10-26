//! Third-party notices and the retained MPL-2.0 sources of a release (D-CNNKS).
//!
//! [`for_target`] reads Cargo's locked graph of `rdy-cli` for one release target: the packages
//! that the release binary links (normal edges, no proc macros, no build or dev edges). For each
//! package it writes the declared license expression and every top-level license file into
//! `THIRD-PARTY-NOTICES.txt`'s bytes, and names the MPL-2.0-only packages, whose exact crates.io
//! crate files the release attaches. It never opens a crate file.
//!
//! [`release_sources`] (`qa-release-sources`) copies those crate files from Cargo's registry
//! cache into a new directory, checks each copy against `Cargo.lock`, and returns the release
//! notes part that names them. [`check_release_text`] checks a release's notes against that part,
//! and checks their changes part: in place, and naming any store or dump format move since the
//! previous release with a dump-and-load step only when the store format moves (D-7PQEF, D-MYEW8).
use crate::platform;
use crate::release_install::{self, TARGETS, valid_path, valid_target, valid_version};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The most packages `cargo metadata` may list.
pub const METADATA_PACKAGES_MAX: usize = 4096;
/// The most bytes one Cargo run may write to stdout and stderr together.
pub const CARGO_OUTPUT_MAX: usize = 16_777_216;
/// The largest `Cargo.lock` read.
pub const LOCK_MAX: u64 = 16_777_216;
/// The most license files taken from one package.
pub const LICENSE_FILES_MAX: usize = 128;
/// The largest license file taken.
pub const LICENSE_FILE_MAX: u64 = 65_536;
/// The longest path read, in bytes.
pub const PATH_MAX: usize = release_install::PATH_MAX;
/// The longest license expression parsed, in bytes.
pub const EXPRESSION_MAX: usize = 1024;
/// The deepest parentheses nesting of a license expression.
pub const EXPRESSION_DEPTH_MAX: usize = 64;
/// The largest crate file copied.
pub const CRATE_MAX: u64 = 67_108_864;
/// The chunk in which a crate file is copied and hashed.
pub const COPY_CHUNK: usize = 65_536;
/// The largest release notes file checked.
pub const NOTES_MAX: u64 = 1_048_576;
/// The largest notices file, as a length.
const NOTICES_LEN_MAX: usize = release_install::NOTICES_MAX as usize;
/// A hang guard for one Cargo run (not a speed check).
const CARGO_DEADLINE: Duration = Duration::from_secs(300);

/// The source of every package the notices take.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// The first line of the notices, before a blank line.
const HEADER: &str = "Third-party notices for rdy\n\n";
/// The heading of the release notes part.
pub const NOTES_HEADING: &str = "## Third-party source (MPL-2.0)";
/// Where a release's assets are downloaded from, before `v<V>/<file>`.
const RELEASE_DOWNLOADS: &str = "https://github.com/adrenickchrome/rdy-core/releases/download";

/// The refusal texts, one line each. Invalid arguments exit 2; the others exit 1.
pub const MSG_INVALID_ARGUMENTS: &str = "release sources: invalid arguments";
pub const MSG_GRAPH: &str = "release sources: locked dependency graph is unavailable";
pub const MSG_LOCKFILE: &str = "release sources: invalid lockfile";
pub const MSG_NOTICES: &str = "release sources: third-party notices are incomplete";
pub const MSG_SIZE_LIMIT: &str = "release sources: size limit exceeded";
pub const MSG_SOURCE: &str = "release sources: source archive does not match Cargo.lock";
pub const MSG_OUTPUT_EXISTS: &str = "release sources: output exists";
pub const MSG_FILE_OPERATION: &str = "release sources: file operation failed";
pub const MSG_NOTES: &str = "release sources: release notes do not match Cargo.lock";
pub const MSG_CHANGES: &str = "release sources: release notes lack the changes part";
pub const MSG_FORMAT_MOVE: &str = "release sources: release notes do not name the format move";

/// The published releases, oldest first: each version with the dump format and store format it
/// writes. The changes part of a release's notes covers everything since the newest row before
/// it (D-7PQEF). A release adds its row once it is published.
pub const RELEASED: &[(&str, u32, u32)] = &[("0.17.0", 3, 16), ("0.20.0", 3, 18)];
/// The groups of the changes part, in order; each is left out when empty (D-7PQEF).
pub const CHANGES_GROUPS: [&str; 5] = [
    "### New features",
    "### Changes",
    "### Fixes",
    "### Security",
    "### Project and build",
];

/// The notices of one release target: `THIRD-PARTY-NOTICES.txt`'s bytes and the MPL-2.0-only
/// packages whose crate files the release attaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notices {
    pub bytes: Vec<u8>,
    pub mpl: Vec<MplSource>,
}

/// An MPL-2.0-only package: its crate file's name and the SHA-256 that `Cargo.lock` records.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MplSource {
    pub name: String,
    pub version: String,
    pub filename: String,
    pub sha256: String,
}

/// Why notices or sources failed; each has one public text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Arguments,
    Graph,
    Lockfile,
    Notices,
    SizeLimit,
    Source,
    OutputExists,
    FileOperation,
    Notes,
    Changes,
    FormatMove,
}

impl Kind {
    fn message(self) -> &'static str {
        match self {
            Kind::Arguments => MSG_INVALID_ARGUMENTS,
            Kind::Graph => MSG_GRAPH,
            Kind::Lockfile => MSG_LOCKFILE,
            Kind::Notices => MSG_NOTICES,
            Kind::SizeLimit => MSG_SIZE_LIMIT,
            Kind::Source => MSG_SOURCE,
            Kind::OutputExists => MSG_OUTPUT_EXISTS,
            Kind::FileOperation => MSG_FILE_OPERATION,
            Kind::Notes => MSG_NOTES,
            Kind::Changes => MSG_CHANGES,
            Kind::FormatMove => MSG_FORMAT_MOVE,
        }
    }
}

/// A refusal and what it refused (a package, a line or a file), for tests; only the kind's
/// text reaches the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Refused {
    kind: Kind,
    at: String,
}

fn refused(kind: Kind, at: impl Into<String>) -> Refused {
    Refused {
        kind,
        at: at.into(),
    }
}

impl Refused {
    fn text(self) -> String {
        self.kind.message().to_string()
    }
}

// ---------------------------------------------------------------------------------------------
// The locked graph: `cargo tree` selects, `cargo metadata` and `Cargo.lock` describe.
// ---------------------------------------------------------------------------------------------

/// One package of `cargo metadata`, as the notices read it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MetaPackage {
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: String,
    license: Option<String>,
    /// A workspace member.
    member: bool,
}

impl MetaPackage {
    /// The directory that holds the manifest.
    fn dir(&self) -> Option<&str> {
        Path::new(&self.manifest_path).parent()?.to_str()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Metadata {
    packages: Vec<MetaPackage>,
}

#[derive(Deserialize)]
struct MetadataJson {
    packages: Vec<MetadataPackageJson>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct MetadataPackageJson {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: String,
    license: Option<String>,
}

/// Reads `cargo metadata --format-version 1` output: at most [`METADATA_PACKAGES_MAX`]
/// packages, each manifest path at most [`PATH_MAX`] bytes.
fn parse_metadata(json: &[u8]) -> Result<Metadata, Refused> {
    let parsed: MetadataJson =
        serde_json::from_slice(json).map_err(|e| refused(Kind::Graph, e.to_string()))?;
    if parsed.packages.len() > METADATA_PACKAGES_MAX {
        return Err(refused(
            Kind::SizeLimit,
            format!("{} metadata packages", parsed.packages.len()),
        ));
    }
    let members: BTreeSet<&str> = parsed
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let mut packages = Vec::with_capacity(parsed.packages.len());
    for p in &parsed.packages {
        if p.manifest_path.len() > PATH_MAX {
            return Err(refused(
                Kind::SizeLimit,
                format!("{} {} manifest path", p.name, p.version),
            ));
        }
        packages.push(MetaPackage {
            name: p.name.clone(),
            version: p.version.clone(),
            source: p.source.clone(),
            manifest_path: p.manifest_path.clone(),
            license: p.license.clone(),
            member: members.contains(p.id.as_str()),
        });
    }
    Ok(Metadata { packages })
}

/// One `cargo tree --format {p}` line: `<name> v<version>`, then optionally ` (<path>)`; one
/// trailing ` (*)` was taken off first. `None` for any other shape.
fn tree_line(line: &str) -> Option<(&str, &str, Option<&str>)> {
    let line = line.strip_suffix(" (*)").unwrap_or(line);
    let (name, rest) = line.split_once(' ')?;
    let rest = rest.strip_prefix('v')?;
    let (version, suffix) = match rest.split_once(' ') {
        Some((version, suffix)) => (version, Some(suffix)),
        None => (rest, None),
    };
    if name.is_empty() || version.is_empty() {
        return None;
    }
    let path = match suffix {
        None => None,
        Some(s) => Some(s.strip_prefix('(')?.strip_suffix(')')?),
    };
    Some((name, version, path))
}

/// The crates.io packages that `tree` (Cargo's selection) names, each once, matched in
/// `meta`. A line with a path is a workspace member (its name, version and manifest
/// directory), which is left out; any other suffix, an unmatched or ambiguous package, or a
/// source other than crates.io is refused.
fn select<'m>(tree: &str, meta: &'m Metadata) -> Result<Vec<&'m MetaPackage>, Refused> {
    let body = tree
        .strip_suffix('\n')
        .ok_or_else(|| refused(Kind::Graph, "the tree output does not end in LF"))?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for line in body.split('\n') {
        let (name, version, path) = tree_line(line).ok_or_else(|| refused(Kind::Graph, line))?;
        let found: Vec<usize> = meta
            .packages
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.name == name
                    && p.version == version
                    && match path {
                        Some(path) => p.member && p.source.is_none() && p.dir() == Some(path),
                        None => !p.member && p.source.as_deref() == Some(CRATES_IO),
                    }
            })
            .map(|(i, _)| i)
            .collect();
        let [i] = found[..] else {
            return Err(refused(Kind::Graph, line));
        };
        if path.is_none() && seen.insert(i) {
            out.push(&meta.packages[i]);
        }
    }
    Ok(out)
}

/// One `[[package]]` of `Cargo.lock`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

#[derive(Deserialize)]
struct LockFile {
    #[serde(default)]
    package: Vec<LockPackage>,
}

/// Reads the lockfile at `path` through a checked handle: a regular file of at most
/// [`LOCK_MAX`] bytes.
fn read_lock(path: &Path) -> Result<Vec<u8>, Refused> {
    let file = platform::open_input(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::InvalidInput => refused(Kind::Lockfile, "open"),
        _ => refused(Kind::FileOperation, "open Cargo.lock"),
    })?;
    read_checked(&file, LOCK_MAX).map_err(|kind| match kind {
        Kind::Notices => refused(Kind::Lockfile, "not a regular file"),
        kind => refused(kind, "Cargo.lock"),
    })
}

/// Reads the open `file` from its start: a regular file of at most `max` bytes, read with at
/// most `max + 1` bytes kept. A file that is not regular is `Kind::Notices` (the caller names
/// its own kind); a larger one `Kind::SizeLimit`.
fn read_checked(file: &File, max: u64) -> Result<Vec<u8>, Kind> {
    let meta = file.metadata().map_err(|_| Kind::FileOperation)?;
    if !meta.is_file() {
        return Err(Kind::Notices);
    }
    if meta.len() > max {
        return Err(Kind::SizeLimit);
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| Kind::FileOperation)?;
    if bytes.len() as u64 > max {
        return Err(Kind::SizeLimit);
    }
    Ok(bytes)
}

/// Parses `Cargo.lock` as TOML.
fn parse_lock(bytes: &[u8]) -> Result<Vec<LockPackage>, Refused> {
    let text = std::str::from_utf8(bytes).map_err(|_| refused(Kind::Lockfile, "not UTF-8"))?;
    let lock: LockFile =
        toml::from_str(text).map_err(|e| refused(Kind::Lockfile, e.to_string()))?;
    Ok(lock.package)
}

/// Whether `s` is a SHA-256 in 64 lowercase hex digits.
fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The checksum that exactly one lock entry gives `name` `version` from `source`.
fn checksum(
    lock: &[LockPackage],
    name: &str,
    version: &str,
    source: &str,
) -> Result<String, Refused> {
    let at = || format!("{name} {version}");
    let found: Vec<&LockPackage> = lock
        .iter()
        .filter(|p| p.name == name && p.version == version && p.source.as_deref() == Some(source))
        .collect();
    let [entry] = found[..] else {
        return Err(refused(Kind::Lockfile, at()));
    };
    match entry.checksum.as_deref() {
        Some(sum) if is_sha256(sum) => Ok(sum.to_string()),
        _ => Err(refused(Kind::Lockfile, at())),
    }
}

/// A selected package, its unpacked directory and its locked checksum.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Crate {
    name: String,
    version: String,
    license: Option<String>,
    dir: PathBuf,
    checksum: String,
}

/// The selected packages with their lock checksums.
fn crates_of(selected: &[&MetaPackage], lock: &[LockPackage]) -> Result<Vec<Crate>, Refused> {
    selected
        .iter()
        .map(|p| {
            let dir = p
                .dir()
                .ok_or_else(|| refused(Kind::Graph, format!("{} {}", p.name, p.version)))?;
            Ok(Crate {
                name: p.name.clone(),
                version: p.version.clone(),
                license: p.license.clone(),
                dir: PathBuf::from(dir),
                checksum: checksum(lock, &p.name, &p.version, CRATES_IO)?,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The notices.
// ---------------------------------------------------------------------------------------------

/// The license text of a package that ships none: checked in, for one exact name and version.
#[derive(Clone, Copy, Debug)]
struct Override {
    name: &'static str,
    version: &'static str,
    /// The package's declared expression, which must stay this.
    license: &'static str,
    url: &'static str,
    text: &'static [u8],
}

/// rmcp 3.5.0 ships no license file; this is the complete upstream LICENSE at the commit its
/// crate was published from.
const RMCP_URL: &str = "https://raw.githubusercontent.com/modelcontextprotocol/rust-sdk/0cde3c5cf3e6aff0cc852ce6045f107e95991f48/LICENSE";

/// The checked-in license texts. An override that no package uses fails, so an upgrade cannot
/// keep an old text.
const OVERRIDES: [Override; 1] = [Override {
    name: "rmcp",
    version: "3.5.0",
    license: "Apache-2.0",
    url: RMCP_URL,
    text: include_bytes!("../licenses/rmcp-3.5.0.txt"),
}];

/// Whether the declared `expression` is MPL-2.0 only: at least one requirement, and every
/// requirement MPL-2.0 (with or without `+`) with no exception. Strict SPDX, with `/` read as
/// `OR`. An empty, oversized, too deeply nested or invalid expression is refused.
fn mpl_only(expression: &str) -> Result<bool, Kind> {
    if expression.is_empty() {
        return Err(Kind::Notices);
    }
    if expression.len() > EXPRESSION_MAX {
        return Err(Kind::SizeLimit);
    }
    let mut depth = 0usize;
    for b in expression.bytes() {
        match b {
            b'(' => {
                depth += 1;
                if depth > EXPRESSION_DEPTH_MAX {
                    return Err(Kind::SizeLimit);
                }
            }
            b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let mode = spdx::ParseMode {
        allow_slash_as_or_operator: true,
        ..spdx::ParseMode::STRICT
    };
    let parsed = spdx::Expression::parse_mode(expression, mode).map_err(|_| Kind::Notices)?;
    let mut any = false;
    for r in parsed.requirements() {
        any = true;
        let mpl =
            matches!(&r.req.license, spdx::LicenseItem::Spdx { id, .. } if id.name == "MPL-2.0");
        if !mpl || r.req.addition.is_some() {
            return Ok(false);
        }
    }
    Ok(any)
}

/// The prefixes of a license file's name, matched without ASCII case.
const LICENSE_PREFIXES: [&str; 4] = ["LICENSE", "LICENCE", "COPYING", "NOTICE"];

/// A directory entry's raw name: `Some` (as UTF-8) when it names a license file, `None` when it
/// does not; a matching name that is not UTF-8 is refused.
fn license_name(raw: &[u8]) -> Result<Option<&str>, Kind> {
    let matches = LICENSE_PREFIXES
        .iter()
        .any(|p| raw.len() >= p.len() && raw[..p.len()].eq_ignore_ascii_case(p.as_bytes()));
    if !matches {
        return Ok(None);
    }
    std::str::from_utf8(raw)
        .map(Some)
        .map_err(|_| Kind::Notices)
}

/// The license files at the top of the package directory `dir`, sorted bytewise: every regular
/// file whose name matches. Matching directories are skipped; a matching link or other file is
/// refused. Nothing is followed or recursed into.
fn license_files(dir: &Path) -> Result<Vec<String>, Kind> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|_| Kind::FileOperation)? {
        let entry = entry.map_err(|_| Kind::FileOperation)?;
        let raw = entry.file_name();
        let Some(name) = license_name(raw.as_encoded_bytes())? else {
            continue;
        };
        let kind = entry.file_type().map_err(|_| Kind::FileOperation)?;
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(Kind::Notices);
        }
        if dir.join(name).as_os_str().len() > PATH_MAX {
            return Err(Kind::SizeLimit);
        }
        names.push(name.to_string());
        if names.len() > LICENSE_FILES_MAX {
            return Err(Kind::SizeLimit);
        }
    }
    names.sort();
    Ok(names)
}

/// One license file's text, through a checked handle that never follows a link: a regular file
/// of at most [`LICENSE_FILE_MAX`] bytes of UTF-8.
fn read_license(path: &Path) -> Result<Vec<u8>, Kind> {
    let file = platform::open_input(path).map_err(|e| match e.kind() {
        io::ErrorKind::InvalidInput => Kind::Notices,
        _ => Kind::FileOperation,
    })?;
    let bytes = read_checked(&file, LICENSE_FILE_MAX)?;
    if std::str::from_utf8(&bytes).is_err() {
        return Err(Kind::Notices);
    }
    Ok(bytes)
}

/// `len + add` when it fits in `max`.
fn grown(len: usize, add: usize, max: usize) -> Option<usize> {
    len.checked_add(add).filter(|n| *n <= max)
}

/// The notices' bytes as they grow, never past the notices cap.
struct NoticesOut {
    bytes: Vec<u8>,
}

impl NoticesOut {
    fn push(&mut self, more: &[u8]) -> Result<(), Kind> {
        grown(self.bytes.len(), more.len(), NOTICES_LEN_MAX).ok_or(Kind::SizeLimit)?;
        self.bytes.extend_from_slice(more);
        Ok(())
    }

    /// A blank line, `--- <name> ---`, LF and the text, which ends in one LF unless it is empty.
    fn file(&mut self, name: &str, text: &[u8]) -> Result<(), Kind> {
        self.push(format!("\n--- {name} ---\n").as_bytes())?;
        self.push(text)?;
        if text.last().is_some_and(|b| *b != b'\n') {
            self.push(b"\n")?;
        }
        Ok(())
    }
}

/// Orders by name, then version bytes.
fn by_name_version(a: (&str, &str), b: (&str, &str)) -> std::cmp::Ordering {
    (a.0.as_bytes(), a.1.as_bytes()).cmp(&(b.0.as_bytes(), b.1.as_bytes()))
}

/// The notices of `crates`, sorted by name and version, with `overrides` for packages that ship
/// no license file.
fn notices_of(crates: &[Crate], overrides: &[Override]) -> Result<Notices, Refused> {
    let mut sorted: Vec<&Crate> = crates.iter().collect();
    sorted.sort_by(|a, b| by_name_version((&a.name, &a.version), (&b.name, &b.version)));
    let mut out = NoticesOut { bytes: Vec::new() };
    let at_start = |kind: Kind| refused(kind, "the notices header");
    out.push(HEADER.as_bytes()).map_err(at_start)?;
    let mut used = vec![false; overrides.len()];
    let mut mpl = Vec::new();
    for c in sorted {
        let at = |kind: Kind| refused(kind, format!("{} {}", c.name, c.version));
        let declared = c.license.as_deref().unwrap_or("");
        let only_mpl = mpl_only(declared).map_err(at)?;
        let files = license_files(&c.dir).map_err(at)?;
        let fallback = if files.is_empty() {
            let i = overrides
                .iter()
                .position(|o| o.name == c.name && o.version == c.version)
                .ok_or_else(|| at(Kind::Notices))?;
            let o = &overrides[i];
            if o.license != declared || std::str::from_utf8(o.text).is_err() {
                return Err(at(Kind::Notices));
            }
            used[i] = true;
            Some(o)
        } else {
            None
        };
        out.push(format!("{} {}\nLicense: {declared}\n", c.name, c.version).as_bytes())
            .map_err(at)?;
        if only_mpl {
            let filename = format!("{}-{}.crate", c.name, c.version);
            out.push(
                format!(
                    "Source: {filename}; SHA-256: {}\nThis release attaches {filename}.\n",
                    c.checksum
                )
                .as_bytes(),
            )
            .map_err(at)?;
            mpl.push(MplSource {
                name: c.name.clone(),
                version: c.version.clone(),
                filename,
                sha256: c.checksum.clone(),
            });
        }
        if let Some(o) = fallback {
            out.push(format!("Source URL: {}\n", o.url).as_bytes())
                .map_err(at)?;
            out.file("LICENSE", o.text).map_err(at)?;
        }
        for name in &files {
            // one file at a time: its buffer goes before the next is read
            let text = read_license(&c.dir.join(name)).map_err(at)?;
            out.file(name, &text).map_err(at)?;
        }
        out.push(b"\n").map_err(at)?;
    }
    if let Some(i) = used.iter().position(|u| !u) {
        let o = &overrides[i];
        return Err(refused(
            Kind::Notices,
            format!("unused override {} {}", o.name, o.version),
        ));
    }
    Ok(Notices {
        bytes: out.bytes,
        mpl,
    })
}

// ---------------------------------------------------------------------------------------------
// Running Cargo.
// ---------------------------------------------------------------------------------------------

/// What notices read besides the target; tests swap parts of it.
struct Sources<'a> {
    /// The workspace root: Cargo runs here, and `Cargo.lock` is read here.
    root: PathBuf,
    /// A fresh command for the Cargo executable.
    cargo: &'a dyn Fn() -> Command,
    overrides: &'a [Override],
}

/// The configured `CARGO` executable, else `cargo`.
fn cargo_command() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

impl Sources<'static> {
    /// The real release: this repository, its Cargo, the checked-in overrides.
    fn release() -> Self {
        Sources {
            root: release_install::repository_root(),
            cargo: &cargo_command,
            overrides: &OVERRIDES,
        }
    }
}

/// Runs Cargo with `args` (no shell) in the root, through the bounded runner; its stdout.
fn run_cargo(src: &Sources, args: &[&str]) -> Result<Vec<u8>, Refused> {
    let mut cmd = (src.cargo)();
    cmd.args(args).current_dir(&src.root);
    let at = || args.first().copied().unwrap_or("cargo").to_string();
    match platform::run_bounded(cmd, None, CARGO_OUTPUT_MAX, CARGO_DEADLINE) {
        Ok(out) if out.status.success() => Ok(out.stdout),
        Ok(_) => Err(refused(Kind::Graph, at())),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Err(refused(Kind::SizeLimit, at())),
        Err(_) => Err(refused(Kind::Graph, at())),
    }
}

/// The crates.io packages that the `rdy-cli` release binary links for `target`.
fn gather(target: &str, src: &Sources) -> Result<Vec<Crate>, Refused> {
    if !valid_target(target) {
        return Err(refused(Kind::Arguments, target));
    }
    // The lock is read, bounded and parsed before Cargo runs, so a bad lock is always the
    // lockfile refusal; its entries are kept for the graph's checksums below.
    let lock = parse_lock(&read_lock(&src.root.join("Cargo.lock"))?)?;
    let tree = run_cargo(
        src,
        &[
            "tree",
            "--locked",
            "--offline",
            "-p",
            "rdy-cli",
            "--target",
            target,
            "-e",
            "normal,no-proc-macro",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--color",
            "never",
        ],
    )?;
    let tree = String::from_utf8(tree).map_err(|_| refused(Kind::Graph, "tree output"))?;
    let meta = run_cargo(
        src,
        &[
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            target,
        ],
    )?;
    let meta = parse_metadata(&meta)?;
    let selected = select(&tree, &meta)?;
    crates_of(&selected, &lock)
}

/// The notices for `target` and the packages they cover.
fn inventory(target: &str, src: &Sources) -> Result<(Notices, Vec<Crate>), Refused> {
    let crates = gather(target, src)?;
    let notices = notices_of(&crates, src.overrides)?;
    Ok((notices, crates))
}

/// The notices of the `rdy-cli` release binary for `target`, one of the release targets:
/// `THIRD-PARTY-NOTICES.txt`'s bytes and the MPL-2.0-only packages. Cargo's locked, offline
/// graph selects the packages; it opens no crate file. The error is one of the `MSG_` texts.
pub fn for_target(target: &str) -> Result<Notices, String> {
    inventory(target, &Sources::release())
        .map(|(notices, _)| notices)
        .map_err(Refused::text)
}

// ---------------------------------------------------------------------------------------------
// The retained sources and the release notes.
// ---------------------------------------------------------------------------------------------

/// A crate file to attach: its name, its locked SHA-256 and where Cargo's cache holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    filename: String,
    sha256: String,
    cache: PathBuf,
}

/// Where Cargo's registry cache holds the crate file of the package unpacked at `dir`
/// (`<registry>/src/<index>/<name>-<version>`): `<registry>/cache/<index>/<name>-<version>.crate`.
fn cache_path(dir: &Path, name: &str, version: &str) -> Option<PathBuf> {
    let stem = format!("{name}-{version}");
    if dir.file_name()? != stem.as_str() {
        return None;
    }
    let index_dir = dir.parent()?;
    let index = index_dir.file_name()?;
    let src = index_dir.parent()?;
    if src.file_name()? != "src" {
        return None;
    }
    Some(
        src.parent()?
            .join("cache")
            .join(index)
            .join(format!("{stem}.crate")),
    )
}

/// The MPL-2.0-only packages of both release targets, each once (by name, version and
/// checksum), sorted, and their crate files.
fn union_sources(src: &Sources) -> Result<(Vec<MplSource>, Vec<Source>), Refused> {
    let mut all: BTreeMap<(String, String), (MplSource, PathBuf)> = BTreeMap::new();
    for target in TARGETS {
        let (notices, crates) = inventory(target, src)?;
        for m in notices.mpl {
            let at = || format!("{} {}", m.name, m.version);
            let c = crates
                .iter()
                .find(|c| c.name == m.name && c.version == m.version)
                .ok_or_else(|| refused(Kind::Graph, at()))?;
            let cache = cache_path(&c.dir, &c.name, &c.version)
                .ok_or_else(|| refused(Kind::Source, at()))?;
            match all.get(&(m.name.clone(), m.version.clone())) {
                Some((seen, _)) if seen.sha256 != m.sha256 => {
                    return Err(refused(Kind::Lockfile, at()));
                }
                Some(_) => {}
                None => {
                    all.insert((m.name.clone(), m.version.clone()), (m, cache));
                }
            }
        }
    }
    let mut mpl = Vec::new();
    let mut sources = Vec::new();
    for (_, (m, cache)) in all {
        sources.push(Source {
            filename: m.filename.clone(),
            sha256: m.sha256.clone(),
            cache,
        });
        mpl.push(m);
    }
    Ok((mpl, sources))
}

/// The release notes part for `version` and the MPL-2.0-only packages `mpl` (sorted by name
/// and version): the two-library paragraph, or the empty-set text. Other counts are refused
/// until a decision gives their text.
fn notes_part(version: &str, mpl: &[MplSource]) -> Result<String, Refused> {
    match mpl {
        [] => Ok(format!(
            "{NOTES_HEADING}\n\nThe binaries contain no libraries used under MPL-2.0.\n"
        )),
        [a, b] => {
            let mut pair = [a, b];
            pair.sort_by(|x, y| by_name_version((&x.name, &x.version), (&y.name, &y.version)));
            let labels: Vec<String> = pair
                .iter()
                .map(|m| format!("`{}` {}", m.name, m.version))
                .collect();
            let mut text = format!(
                "{NOTES_HEADING}\n\nrdy is licensed MIT OR Apache-2.0. The binaries also contain two libraries under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/): {}. This release attaches the exact source code of each, as its crates.io crate file. Each SHA-256 matches rdy's `Cargo.lock` at this release:\n\n",
                labels.join(" and ")
            );
            for m in pair {
                text.push_str(&format!(
                    "- [{f}]({RELEASE_DOWNLOADS}/v{version}/{f}) — SHA-256: `{}`\n",
                    m.sha256,
                    f = m.filename
                ));
            }
            text.push_str(
                "\nEach archive's `THIRD-PARTY-NOTICES.txt` gives the license of every library in the binary.\n",
            );
            Ok(text)
        }
        _ => Err(refused(Kind::Notes, format!("{} MPL packages", mpl.len()))),
    }
}

/// Checks release notes `text` against the generated `part`: exactly one heading line, after
/// the level-two heading `## Feedback`; from the heading, `part` exactly, then one extra LF and
/// the first `<details>` line; and exactly two `<details>` lines after the part.
fn check_notes(part: &str, text: &str) -> Result<(), Refused> {
    let mut starts = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if line.strip_suffix('\n').unwrap_or(line) == NOTES_HEADING {
            starts.push(offset);
        }
        offset += line.len();
    }
    let [at] = starts[..] else {
        return Err(refused(Kind::Notes, format!("{} headings", starts.len())));
    };
    let previous = text[..at].lines().rfind(|l| l.starts_with("## "));
    if previous != Some("## Feedback") {
        return Err(refused(Kind::Notes, "not after ## Feedback"));
    }
    let after = text[at..]
        .strip_prefix(part)
        .ok_or_else(|| refused(Kind::Notes, "the part differs"))?;
    if !after.starts_with("\n<details>\n") {
        return Err(refused(Kind::Notes, "no <details> after the part"));
    }
    if after.lines().filter(|l| *l == "<details>").count() != 2 {
        return Err(refused(Kind::Notes, "not two <details> after the part"));
    }
    Ok(())
}

/// The `(major, minor, patch)` of a plain version.
fn version_key(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(a)), Some(Some(b)), Some(Some(c)), None) => Some((a, b, c)),
        _ => None,
    }
}

/// The release before `version` in `released`: the newest row with a lower version.
fn previous_release<'a>(
    version: &str,
    released: &[(&'a str, u32, u32)],
) -> Option<(&'a str, u32, u32)> {
    let key = version_key(version)?;
    released
        .iter()
        .copied()
        .filter(|(v, _, _)| version_key(v).is_some_and(|k| k < key))
        .max_by_key(|(v, _, _)| version_key(v))
}

/// The heading of the changes part, after the previous release `previous`.
pub fn changes_heading(previous: &str) -> String {
    format!("## What changed since {previous}")
}

/// The store a release writes: a scalar store of dump format and store format (every published
/// release so far), or a feature store of a dump format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrittenStore {
    Scalar(u32, u32),
    Features(u32),
}

impl From<(u32, u32)> for WrittenStore {
    fn from((dump, store): (u32, u32)) -> WrittenStore {
        WrittenStore::Scalar(dump, store)
    }
}

/// What this rdy writes: a feature store, in its dump format.
pub fn current_store() -> WrittenStore {
    WrittenStore::Features(rdy_store::dump::DUMP_FORMAT)
}

/// The line that the changes part's Changes group must hold when the formats move from the
/// previous release `previous` (dump format `pd`, store format `ps`) to `version` (`current`),
/// or `None` when neither moves. A move from a scalar store to feature stores is O18's line.
fn format_move_line(
    version: &str,
    (previous, pd, ps): (&str, u32, u32),
    current: impl Into<WrittenStore>,
) -> Option<String> {
    let (cd, cs) = match current.into() {
        WrittenStore::Scalar(cd, cs) => (cd, cs),
        WrittenStore::Features(cd) => {
            return Some(format!(
                "- **Breaking: the store format moves from {ps} to feature-based stores (dump pair ({pd}, {ps}) to dump format {cd}).** rdy {version} refuses a store made by rdy {previous} with `E_STORE_FORMAT`. To move a project, run `rdy dump --out <file>` with rdy {previous}, then `rdy load <file> <dir>` with rdy {version}, into a directory with no store."
            ));
        }
    };
    if (pd, ps) == (cd, cs) {
        return None;
    }
    if ps != cs {
        return Some(format!(
            "- **Breaking: the store format moves from {ps} to {cs} (dump pair ({pd}, {ps}) to ({cd}, {cs})).** rdy {version} refuses a store made by rdy {previous} with `E_STORE_FORMAT`. To move a project, run `rdy dump --out <file>` with rdy {previous}, then `rdy load <file> <dir>` with rdy {version}, into a directory with no store."
        ));
    }
    Some(format!(
        "- **Breaking: the dump format moves from {pd} to {cd} (dump pair ({pd}, {ps}) to ({cd}, {ps})).** rdy {version} opens a store made by rdy {previous} as it is, and loads every earlier dump. rdy {previous} cannot load a dump made by rdy {version}."
    ))
}

/// The format move line of `version` against [`RELEASED`] and the store this rdy writes
/// ([`current_store`]), or `None` when neither moves. `Err` when no published release comes
/// before `version`.
pub fn release_format_move_line(version: &str) -> Result<Option<String>, String> {
    let previous = previous_release(version, RELEASED).ok_or(MSG_INVALID_ARGUMENTS)?;
    Ok(format_move_line(version, previous, current_store()))
}

/// Checks the changes part of release notes `text` (D-7PQEF): exactly one `heading` line; the
/// level-two heading before it starts `## Try it` and the one after it is `## Feedback`; its
/// level-three headings are [`CHANGES_GROUPS`], at least one, in order, none twice; and when
/// `move_line` is given, its Changes group holds that line exactly.
fn check_changes(heading: &str, move_line: Option<&str>, text: &str) -> Result<(), Refused> {
    let lines: Vec<&str> = text.lines().collect();
    let at: Vec<usize> = (0..lines.len()).filter(|&i| lines[i] == heading).collect();
    let [at] = at[..] else {
        return Err(refused(Kind::Changes, format!("{} headings", at.len())));
    };
    let before = lines[..at].iter().rfind(|l| l.starts_with("## "));
    if !before.is_some_and(|l| l.starts_with("## Try it")) {
        return Err(refused(Kind::Changes, "not after ## Try it"));
    }
    let end = (at + 1..lines.len())
        .find(|&i| lines[i].starts_with("## "))
        .unwrap_or(lines.len());
    if lines.get(end) != Some(&"## Feedback") {
        return Err(refused(Kind::Changes, "not before ## Feedback"));
    }
    let body = &lines[at + 1..end];
    let groups: Vec<usize> = (0..body.len())
        .filter(|&i| body[i].starts_with("### "))
        .collect();
    let mut next = 0usize;
    for &g in &groups {
        let Some(k) = CHANGES_GROUPS[next..].iter().position(|h| *h == body[g]) else {
            return Err(refused(Kind::Changes, body[g]));
        };
        next += k + 1;
    }
    if groups.is_empty() {
        return Err(refused(Kind::Changes, "no group"));
    }
    if let Some(line) = move_line {
        let changes = groups.iter().position(|&g| body[g] == CHANGES_GROUPS[1]);
        let held = changes.is_some_and(|i| {
            let from = groups[i] + 1;
            let to = groups.get(i + 1).copied().unwrap_or(body.len());
            body[from..to].contains(&line)
        });
        if !held {
            return Err(refused(Kind::FormatMove, line));
        }
    }
    Ok(())
}

/// One run of source staging: OUT, open, and the files it made there.
struct SourceRun {
    parent: File,
    out_name: std::ffi::OsString,
    out_dir: File,
    made: Vec<(String, File)>,
    /// A descriptor held from before OUT was made, given back when the run finishes: the
    /// ownership checks of its cleanup open one entry at a time, so they still run when every
    /// other descriptor of this process is taken.
    spare: File,
}

impl SourceRun {
    /// Copies the checked `from` into the new file `source.filename` in OUT, hashing the bytes
    /// once as they pass; the copy stands only if its hash is the locked one.
    fn copy(
        &mut self,
        source: &Source,
        from: &File,
        during: &dyn Fn(&str, u64),
    ) -> Result<(), Refused> {
        let at = |kind: Kind| refused(kind, source.filename.clone());
        let file = platform::create_new_at(&self.out_dir, source.filename.as_ref())
            .map_err(|_| at(Kind::FileOperation))?;
        // owned from here on, before anything else can fail; copied through this one handle
        self.made.push((source.filename.clone(), file));
        let Some((_, made)) = self.made.last() else {
            return Err(at(Kind::FileOperation));
        };
        let mut to: &File = made;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; COPY_CHUNK];
        let mut total = 0u64;
        let mut reader = from;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(at(Kind::FileOperation)),
            };
            total = total
                .checked_add(n as u64)
                .filter(|t| *t <= CRATE_MAX)
                .ok_or_else(|| at(Kind::SizeLimit))?;
            hash.update(&buf[..n]);
            to.write_all(&buf[..n])
                .map_err(|_| at(Kind::FileOperation))?;
            during(&source.filename, total);
        }
        to.sync_all().map_err(|_| at(Kind::FileOperation))?;
        platform::set_file_mode(to, 0o644).map_err(|_| at(Kind::FileOperation))?;
        if format!("{:x}", hash.finalize()) != source.sha256 {
            return Err(at(Kind::Source));
        }
        Ok(())
    }

    /// A success stands only while OUT's path still names this run's OUT and each copy is still
    /// in it. On failure it removes each copy (only while its name still holds it) and OUT
    /// (only while OUT's path still names it). A failed removal is the result.
    fn finish(self, made: Result<(), Refused>) -> Result<(), Refused> {
        let SourceRun {
            parent,
            out_name,
            out_dir,
            made: files,
            spare,
        } = self;
        drop(spare);
        let in_place = platform::holds_file(&parent, &out_name, &out_dir).unwrap_or(false);
        let kept = files.iter().all(|(name, file)| {
            platform::holds_file(&out_dir, name.as_ref(), file).unwrap_or(false)
        });
        let made = match made {
            Ok(()) if in_place && kept => return Ok(()),
            Ok(()) => Err(refused(Kind::FileOperation, "OUT changed")),
            failed => failed,
        };
        let mut cleaned = true;
        for (name, file) in &files {
            cleaned &= match platform::holds_file(&out_dir, name.as_ref(), file) {
                Ok(true) => platform::remove_file_at(&out_dir, name.as_ref()).is_ok(),
                // gone, or another's: never removed
                Ok(false) => true,
                Err(_) => false,
            };
        }
        if in_place {
            cleaned &= platform::remove_dir_at(&parent, &out_name).is_ok();
        }
        if cleaned {
            made
        } else {
            Err(refused(Kind::FileOperation, "cleanup"))
        }
    }
}

/// Copies each of `sources` into the new directory `out`. Every source is checked first (a
/// regular file, not a link, of at most [`CRATE_MAX`] bytes) and kept open; only then is OUT
/// made, and each copy is read from its checked handle. Each output is owned from the moment it
/// is made, and one descriptor is held back for the cleanup. `during` runs after each chunk.
fn stage_sources(
    sources: &[Source],
    out: &Path,
    during: &dyn Fn(&str, u64),
) -> Result<(), Refused> {
    let mut held = Vec::with_capacity(sources.len());
    for s in sources {
        let at = |kind: Kind| refused(kind, s.filename.clone());
        let file = platform::open_input(&s.cache).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::InvalidInput
            | io::ErrorKind::InvalidFilename => at(Kind::Source),
            _ => at(Kind::FileOperation),
        })?;
        let meta = file.metadata().map_err(|_| at(Kind::FileOperation))?;
        if !meta.is_file() {
            return Err(at(Kind::Source));
        }
        if meta.len() > CRATE_MAX {
            return Err(at(Kind::SizeLimit));
        }
        held.push((s, file));
    }
    match std::fs::symlink_metadata(out) {
        Ok(_) => return Err(refused(Kind::OutputExists, "OUT")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(refused(Kind::FileOperation, "OUT")),
    }
    let (Some(parent), Some(out_name)) = (out.parent(), out.file_name()) else {
        return Err(refused(Kind::FileOperation, "OUT"));
    };
    let parent = platform::open_dir(parent).map_err(|_| refused(Kind::FileOperation, "OUT"))?;
    let spare = parent
        .try_clone()
        .map_err(|_| refused(Kind::FileOperation, "OUT"))?;
    let out_dir = match platform::create_private_dir_at(&parent, out_name) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(refused(Kind::OutputExists, "OUT"));
        }
        Err(_) => return Err(refused(Kind::FileOperation, "OUT")),
    };
    let mut run = SourceRun {
        parent,
        out_name: out_name.to_os_string(),
        out_dir,
        made: Vec::new(),
        spare,
    };
    let copied = held
        .iter()
        .try_for_each(|(s, file)| run.copy(s, file, during));
    run.finish(copied)
}

/// The release notes part and staged sources for `version` into `out`.
fn sources_with(
    version: &str,
    out: &Path,
    src: &Sources,
    during: &dyn Fn(&str, u64),
) -> Result<String, Refused> {
    if !(valid_version(version) && valid_path(out)) {
        return Err(refused(Kind::Arguments, version));
    }
    let (mpl, sources) = union_sources(src)?;
    let part = notes_part(version, &mpl)?;
    stage_sources(&sources, out, during)?;
    Ok(part)
}

/// Copies the crate file of each MPL-2.0-only package of both release targets from Cargo's
/// registry cache into the new directory `out` (0644 each, nothing else), checks each copy's
/// SHA-256 against `Cargo.lock`, and returns the exact release notes part for `version`. On
/// failure only this run's own outputs are removed. The error is one of the `MSG_` texts.
pub fn release_sources(version: &str, out: &Path) -> Result<String, String> {
    sources_with(version, out, &Sources::release(), &|_, _| {}).map_err(Refused::text)
}

/// Checks release notes `text` for `version` against the part [`release_sources`] prints. The
/// error is one of the `MSG_` texts.
pub fn check_release_text(version: &str, text: &str) -> Result<(), String> {
    check_with(version, text, &Sources::release()).map_err(Refused::text)
}

fn check_with(version: &str, text: &str, src: &Sources) -> Result<(), Refused> {
    if !valid_version(version) {
        return Err(refused(Kind::Arguments, version));
    }
    let previous = previous_release(version, RELEASED)
        .ok_or_else(|| refused(Kind::Arguments, "no earlier release"))?;
    let (mpl, _) = union_sources(src)?;
    check_notes(&notes_part(version, &mpl)?, text)?;
    let line = format_move_line(version, previous, current_store());
    check_changes(&changes_heading(previous.0), line.as_deref(), text)
}

/// [`check_release_text`] of the release notes file `notes`: a regular file (not a link) of at
/// most [`NOTES_MAX`] bytes of UTF-8.
pub fn check_release_file(version: &str, notes: &Path) -> Result<(), String> {
    check_file_with(version, notes, &Sources::release())
}

fn check_file_with(version: &str, notes: &Path, src: &Sources) -> Result<(), String> {
    if !(valid_version(version) && valid_path(notes)) {
        return Err(MSG_INVALID_ARGUMENTS.to_string());
    }
    let file = platform::open_input(notes).map_err(|_| MSG_FILE_OPERATION.to_string())?;
    let bytes = read_checked(&file, NOTES_MAX).map_err(|kind| match kind {
        Kind::Notices => MSG_FILE_OPERATION.to_string(),
        kind => kind.message().to_string(),
    })?;
    let text = String::from_utf8(bytes).map_err(|_| MSG_NOTES.to_string())?;
    check_with(version, &text, src).map_err(Refused::text)
}

// ---------------------------------------------------------------------------------------------
// The source tool.
// ---------------------------------------------------------------------------------------------

#[derive(clap::Parser)]
#[command(
    name = "qa-release-sources",
    about = "Stage the MPL-2.0 crate files of a release and print its third-party notes part"
)]
struct ToolArgs {
    /// The plain release version, `0.<minor>.0`.
    #[arg(long, value_name = "V")]
    version: String,
    /// The output directory (an absolute path that does not exist yet).
    #[arg(long, value_name = "DIR")]
    out: String,
    /// A release notes file (an absolute path) to check before anything is staged.
    #[arg(long = "check-notes", value_name = "FILE")]
    check_notes: Option<String>,
}

/// `qa-release-sources` with `args` (the program name first): with `--check-notes`, checks
/// that release notes file first; then [`release_sources`], whose notes part goes to `stdout`.
/// A refusal is one line on `stderr` and nothing on `stdout`. The exit status: 0 on success,
/// 2 for invalid arguments, 1 for every other failure.
pub fn sources_tool(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    tool_with(args, &Sources::release(), stdout, stderr)
}

fn tool_with(
    args: &[OsString],
    src: &Sources,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    use clap::Parser;
    let refuse = |stderr: &mut dyn Write, message: &str| {
        let _ = writeln!(stderr, "{message}");
        if message == MSG_INVALID_ARGUMENTS {
            2
        } else {
            1
        }
    };
    let args = match ToolArgs::try_parse_from(args) {
        Ok(args) => args,
        Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => {
            return match write!(stdout, "{}", e.render()).and_then(|()| stdout.flush()) {
                Ok(()) => 0,
                Err(_) => 1,
            };
        }
        Err(_) => return refuse(stderr, MSG_INVALID_ARGUMENTS),
    };
    let out = Path::new(&args.out);
    let notes = args.check_notes.as_deref().map(Path::new);
    if !(valid_version(&args.version) && valid_path(out) && notes.is_none_or(valid_path)) {
        return refuse(stderr, MSG_INVALID_ARGUMENTS);
    }
    if let Some(notes) = notes
        && let Err(e) = check_file_with(&args.version, notes, src)
    {
        return refuse(stderr, &e);
    }
    match sources_with(&args.version, out, src, &|_, _| {}) {
        Ok(part) => match stdout
            .write_all(part.as_bytes())
            .and_then(|()| stdout.flush())
        {
            Ok(()) => 0,
            Err(_) => 1,
        },
        Err(e) => refuse(stderr, &e.text()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::shell_script;
    use crate::platform::test_support::{fifo, unix_socket};
    use crate::release_install::{MANIFEST_FILE, SIGNATURE_FILE, archive_name};
    use std::cell::Cell;
    use std::fs;

    const IMBL_SHA: &str = "46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec";
    const CHUNKS_SHA: &str = "2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6";

    fn member(name: &str, version: &str, manifest: &str) -> MetaPackage {
        MetaPackage {
            name: name.into(),
            version: version.into(),
            source: None,
            manifest_path: manifest.into(),
            license: Some("MIT OR Apache-2.0".into()),
            member: true,
        }
    }

    fn registry(name: &str, version: &str, license: &str) -> MetaPackage {
        MetaPackage {
            name: name.into(),
            version: version.into(),
            source: Some(CRATES_IO.into()),
            manifest_path: format!("/registry/src/index/{name}-{version}/Cargo.toml"),
            license: Some(license.into()),
            member: false,
        }
    }

    fn lock_entry(name: &str, version: &str, checksum: &str) -> LockPackage {
        LockPackage {
            name: name.into(),
            version: version.into(),
            source: Some(CRATES_IO.into()),
            checksum: Some(checksum.into()),
        }
    }

    fn named(selected: &[&MetaPackage]) -> Vec<String> {
        selected
            .iter()
            .map(|p| format!("{} {}", p.name, p.version))
            .collect()
    }

    /// A package directory under `root` holding `files` (name, bytes).
    fn package_dir(root: &Path, stem: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let dir = root.join(stem);
        fs::create_dir_all(&dir).expect("mkdir");
        for (name, bytes) in files {
            fs::write(dir.join(name), bytes).expect("write");
        }
        dir
    }

    fn krate(name: &str, version: &str, license: &str, dir: &Path) -> Crate {
        Crate {
            name: name.into(),
            version: version.into(),
            license: Some(license.into()),
            dir: dir.to_path_buf(),
            checksum: "ab".repeat(32),
        }
    }

    /// Sources with the Cargo command `cargo`, rooted at `root`, with no overrides.
    fn with_cargo<'a>(root: &Path, cargo: &'a dyn Fn() -> Command) -> Sources<'a> {
        Sources {
            root: root.to_path_buf(),
            cargo,
            overrides: &[],
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    /// The MPL-2.0-only packages of the locked graph, as the plan's pre-flight recorded them.
    fn locked_mpl() -> Vec<MplSource> {
        vec![
            MplSource {
                name: "imbl".into(),
                version: "7.0.2".into(),
                filename: "imbl-7.0.2.crate".into(),
                sha256: IMBL_SHA.into(),
            },
            MplSource {
                name: "imbl-sized-chunks".into(),
                version: "0.2.0".into(),
                filename: "imbl-sized-chunks-0.2.0.crate".into(),
                sha256: CHUNKS_SHA.into(),
            },
        ]
    }

    /// The entry headings (`<name> <version>`) of notices `bytes`, in order.
    fn headings(bytes: &[u8]) -> Vec<String> {
        let text = std::str::from_utf8(bytes).expect("utf-8");
        let lines: Vec<&str> = text.split('\n').collect();
        lines
            .windows(2)
            .filter(|w| w[1].starts_with("License: "))
            .map(|w| w[0].to_string())
            .collect()
    }

    #[test]
    fn plx7_notices_follow_the_linked_target_graph() {
        // Cargo's tree grammar: members carry their manifest directory, repeats carry (*).
        let meta = Metadata {
            packages: vec![
                member("rdy-cli", "0.18.0", "/workspace/Cargo.toml"),
                member(
                    "rdy-model",
                    "0.18.0",
                    "/workspace/crates/rdy-model/Cargo.toml",
                ),
                registry("imbl", "7.0.2", "MPL-2.0+"),
            ],
        };
        let tree = concat!(
            "rdy-cli v0.18.0 (/workspace)\n",
            "imbl v7.0.2\n",
            "imbl v7.0.2 (*)\n",
            "rdy-model v0.18.0 (/workspace/crates/rdy-model) (*)\n",
        );
        assert_eq!(
            named(&select(tree, &meta).expect("select")),
            ["imbl 7.0.2"],
            "one imbl entry, no members"
        );
        for bad in [
            "imbl v7.0.2 (*) (*)",
            "imbl v7.0.2 (/elsewhere/imbl)",
            "imbl v7.0.2 (https://github.com/bodil/imbl#0123abcd)",
            "imbl v7.0.2 extra",
            "imbl v7.0.2 (registry `other`)",
            "imbl v7.0.2 (proc-macro)",
            "imbl 7.0.2",
            "imbl v",
            "unknown v1.0.0",
            "imbl v7.0.3",
            "rdy-model v0.18.0 (/workspace/crates/other)",
            "rdy-model v0.18.0",
            "",
        ] {
            let tree = format!("rdy-cli v0.18.0 (/workspace)\n{bad}\n");
            assert_eq!(
                select(&tree, &meta).map(|s| named(&s)),
                Err(refused(Kind::Graph, bad)),
                "{bad:?}"
            );
        }
        for bad in [
            "",
            "imbl v7.0.2",
            "rdy-cli v0.18.0 (/workspace)\nimbl v7.0.2",
        ] {
            assert_eq!(
                select(bad, &meta).map(|s| named(&s)).map_err(|r| r.kind),
                Err(Kind::Graph),
                "{bad:?}: no final LF"
            );
        }
        // An ambiguous package and one from another source.
        let mut twice = Metadata {
            packages: meta.packages.clone(),
        };
        twice.packages.push(registry("imbl", "7.0.2", "MPL-2.0+"));
        assert_eq!(
            select("imbl v7.0.2\n", &twice).map(|s| named(&s)),
            Err(refused(Kind::Graph, "imbl v7.0.2"))
        );
        let mut git = Metadata {
            packages: meta.packages.clone(),
        };
        git.packages[2].source = Some("git+https://github.com/bodil/imbl#0123abcd".into());
        assert_eq!(
            select("imbl v7.0.2\n", &git).map(|s| named(&s)),
            Err(refused(Kind::Graph, "imbl v7.0.2"))
        );

        // Edges: the tree Cargo prints for `-e normal,no-proc-macro` names the normal linked
        // packages only, each package reached through a normal edge once. Build-only, dev-only,
        // proc-macro, proc-macro-only and other-target packages in the metadata stay out.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut packages = vec![
            member("rdy-cli", "0.18.0", "/workspace/Cargo.toml"),
            member(
                "rdy-model",
                "0.18.0",
                "/workspace/crates/rdy-model/Cargo.toml",
            ),
        ];
        let normal = [
            ("anyhow", "1.0.0"),
            ("both", "1.0.0"),
            ("winnow", "0.7.15"),
            ("winnow", "1.0.4"),
            ("zeta", "10.0.0"),
            ("zeta", "9.0.0"),
        ];
        let left_out = [
            ("cc", "1.0.0"),
            ("tempfile", "3.0.0"),
            ("serde_derive", "1.0.0"),
            ("syn", "2.0.0"),
            ("openssl-probe", "0.1.6"),
        ];
        let mut lock = Vec::new();
        for (name, version) in normal.iter().chain(&left_out) {
            let stem = format!("{name}-{version}");
            let d = package_dir(
                dir.path(),
                &stem,
                &[("LICENSE", format!("{stem} license\n").as_bytes())],
            );
            let mut p = registry(name, version, "MIT");
            p.manifest_path = d.join("Cargo.toml").to_str().expect("utf-8").into();
            packages.push(p);
            lock.push(lock_entry(name, version, &"0".repeat(64)));
        }
        let meta = Metadata { packages };
        let tree = concat!(
            "rdy-cli v0.18.0 (/workspace)\n",
            "zeta v9.0.0\n",
            "winnow v1.0.4\n",
            "both v1.0.0\n",
            "anyhow v1.0.0\n",
            "both v1.0.0 (*)\n",
            "winnow v0.7.15\n",
            "zeta v10.0.0\n",
            "rdy-model v0.18.0 (/workspace/crates/rdy-model)\n",
            "anyhow v1.0.0 (*)\n",
            "both v1.0.0 (*)\n",
        );
        let selected = select(tree, &meta).expect("select");
        assert_eq!(
            selected.len(),
            6,
            "each package once: {:?}",
            named(&selected)
        );
        let crates = crates_of(&selected, &lock).expect("lock");
        let notices = notices_of(&crates, &[]).expect("notices");
        let want: Vec<String> = normal.iter().map(|(n, v)| format!("{n} {v}")).collect();
        assert_eq!(headings(&notices.bytes), want, "name, then version bytes");
        let mut reversed = crates.clone();
        reversed.reverse();
        assert_eq!(
            notices_of(&reversed, &[]).expect("notices").bytes,
            notices.bytes,
            "the same packages give the same bytes"
        );
        assert_eq!(
            notices_of(&crates, &[]).expect("notices"),
            notices,
            "repeatable"
        );
        assert!(notices.mpl.is_empty());

        // The real locked graph for both release targets, without running either binary.
        let release = Sources::release();
        let root = release.root.to_str().expect("utf-8 root").to_string();
        for target in TARGETS {
            let (notices, crates) = inventory(target, &release).expect("inventory");
            let names: BTreeSet<&str> = crates.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(crates.len(), 178, "{target}: linked crates.io packages");
            for absent in [
                // build-only and proc-macro (or proc-macro-only) packages
                "autocfg",
                "cc",
                "version_check",
                "serde_derive",
                "thiserror-impl",
                "rmcp-macros",
                "bytemuck_derive",
                "syn",
                "proc-macro2",
                "quote",
                // QA-only and dev-only packages
                "toml",
                "spdx",
                "winnow",
                "proptest",
                "rusty-fork",
                // the replaced font parser
                "ttf-parser",
            ] {
                assert!(!names.contains(absent), "{target} links no {absent}");
            }
            let mac = target == TARGETS[0];
            for (name, on_mac) in [
                ("core-foundation", true),
                ("security-framework", true),
                ("openssl-probe", false),
                ("linux-raw-sys", false),
            ] {
                assert_eq!(names.contains(name), mac == on_mac, "{target}: {name}");
            }
            assert_eq!(notices.mpl, locked_mpl(), "{target}");
            assert_eq!(
                headings(&notices.bytes).len(),
                178,
                "{target}: one entry each"
            );
            let text = String::from_utf8(notices.bytes.clone()).expect("utf-8");
            assert!(text.starts_with(HEADER));
            for local in [root.as_str(), "registry/src", "/Users/", "/home/"] {
                assert!(!text.contains(local), "{target}: names {local}");
            }
            let (again, _) = inventory(target, &release).expect("inventory");
            assert_eq!(again, notices, "{target}: the same bytes again");
            assert_eq!(for_target(target), Ok(notices), "{target}: the public call");
        }
        assert_eq!(
            for_target("x86_64-apple-darwin"),
            Err(MSG_INVALID_ARGUMENTS.to_string())
        );
    }

    /// The runtime packages the pinned diagram crate adds to the release binary, sorted.
    const DIAGRAM_RUNTIME: [(&str, &str, &str); 6] = [
        ("mmdflux", "2.6.1", "MIT"),
        ("pest", "2.9.4", "MIT OR Apache-2.0"),
        ("psm", "0.1.24", "MIT OR Apache-2.0"),
        ("stacker", "0.1.25", "MIT OR Apache-2.0"),
        ("ucd-trie", "0.1.7", "MIT OR Apache-2.0"),
        ("unicode-width", "0.2.2", "MIT OR Apache-2.0"),
    ];

    #[test]
    fn dgm1_release_notices_cover_the_resolved_runtime_graph() {
        let release = Sources::release();
        for target in TARGETS {
            let (notices, crates) = inventory(target, &release).expect("inventory");
            // every linked package once, sorted by name and then version bytes
            let mut covered: Vec<(&str, &str)> = crates
                .iter()
                .map(|c| (c.name.as_str(), c.version.as_str()))
                .collect();
            covered.sort_by(|a, b| by_name_version(*a, *b));
            covered.dedup();
            assert_eq!(covered.len(), crates.len(), "{target}: each package once");
            let want: Vec<String> = covered.iter().map(|(n, v)| format!("{n} {v}")).collect();
            assert_eq!(headings(&notices.bytes), want, "{target}: sorted coverage");
            // the diagram crate's runtime closure, each with its own license files
            let text = String::from_utf8(notices.bytes.clone()).expect("utf-8");
            for (name, version, license) in DIAGRAM_RUNTIME {
                let c = crates
                    .iter()
                    .find(|c| c.name == name && c.version == version)
                    .unwrap_or_else(|| panic!("{target}: links {name} {version}"));
                assert_eq!(c.license.as_deref(), Some(license), "{target}: {name}");
                let files = license_files(&c.dir).expect("license files");
                assert!(!files.is_empty(), "{target}: {name} ships a license file");
                assert!(
                    text.contains(&format!("{name} {version}\nLicense: {license}\n")),
                    "{target}: {name}'s entry"
                );
                for file in &files {
                    assert!(
                        text.contains(&format!("\n--- {file} ---\n")),
                        "{name}: {file}"
                    );
                }
            }
            // build-only and proc-macro additions are not linked; valuable is not in the graph
            let names: BTreeSet<&str> = crates.iter().map(|c| c.name.as_str()).collect();
            for absent in ["pest_derive", "pest_generator", "pest_meta", "valuable"] {
                assert!(!names.contains(absent), "{target} links no {absent}");
            }
            // no license text is checked in for them, and the MPL set is unchanged
            assert!(
                OVERRIDES
                    .iter()
                    .all(|o| DIAGRAM_RUNTIME.iter().all(|(n, _, _)| o.name != *n))
            );
            assert_eq!(notices.mpl, locked_mpl(), "{target}: the MPL set");
        }
    }

    #[test]
    fn plx7_notices_include_all_license_files_and_exact_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let alpha = package_dir(
            dir.path(),
            "alpha-1.0.0",
            &[
                ("LICENSE-MIT", b"MIT text\n"),
                ("license-APACHE.txt", b"Apache text"),
                ("NOTICE", b"notice\n"),
                ("COPYING", b""),
                ("README.md", b"not a license\n"),
            ],
        );
        // a matching directory is skipped, and nothing nested is read
        package_dir(&alpha, "LICENSES", &[("LICENSE", b"nested\n")]);
        package_dir(&alpha, "src", &[("LICENSE", b"nested\n")]);
        let mpl = package_dir(dir.path(), "mplpkg-2.0.0", &[("LICENSE", b"MPL text\n")]);
        let rmcp = package_dir(dir.path(), "rmcp-3.5.0", &[("README.md", b"rmcp\n")]);
        let crates = [
            krate("rmcp", "3.5.0", "Apache-2.0", &rmcp),
            krate("alpha", "1.0.0", "MIT OR Apache-2.0", &alpha),
            krate("mplpkg", "2.0.0", "MPL-2.0", &mpl),
        ];
        // no crate file exists anywhere here: the generator never needs one
        let notices = notices_of(&crates, &OVERRIDES).expect("notices");
        let hash = "ab".repeat(32);
        let mut want = Vec::new();
        want.extend_from_slice(b"Third-party notices for rdy\n\n");
        want.extend_from_slice(b"alpha 1.0.0\nLicense: MIT OR Apache-2.0\n");
        want.extend_from_slice(b"\n--- COPYING ---\n");
        want.extend_from_slice(b"\n--- LICENSE-MIT ---\nMIT text\n");
        want.extend_from_slice(b"\n--- NOTICE ---\nnotice\n");
        want.extend_from_slice(b"\n--- license-APACHE.txt ---\nApache text\n");
        want.extend_from_slice(b"\n");
        want.extend_from_slice(b"mplpkg 2.0.0\nLicense: MPL-2.0\n");
        want.extend_from_slice(format!("Source: mplpkg-2.0.0.crate; SHA-256: {hash}\n").as_bytes());
        want.extend_from_slice(b"This release attaches mplpkg-2.0.0.crate.\n");
        want.extend_from_slice(b"\n--- LICENSE ---\nMPL text\n");
        want.extend_from_slice(b"\n");
        want.extend_from_slice(b"rmcp 3.5.0\nLicense: Apache-2.0\n");
        want.extend_from_slice(b"Source URL: https://raw.githubusercontent.com/modelcontextprotocol/rust-sdk/0cde3c5cf3e6aff0cc852ce6045f107e95991f48/LICENSE\n");
        want.extend_from_slice(b"\n--- LICENSE ---\n");
        want.extend_from_slice(OVERRIDES[0].text);
        want.extend_from_slice(b"\n");
        assert_eq!(
            String::from_utf8_lossy(&notices.bytes),
            String::from_utf8_lossy(&want)
        );
        assert_eq!(notices.bytes, want);
        assert_eq!(
            notices.mpl,
            [MplSource {
                name: "mplpkg".into(),
                version: "2.0.0".into(),
                filename: "mplpkg-2.0.0.crate".into(),
                sha256: hash.clone(),
            }]
        );

        // rmcp's checked-in text: the complete upstream LICENSE at the published commit.
        let o = &OVERRIDES[0];
        assert_eq!(
            (o.name, o.version, o.license),
            ("rmcp", "3.5.0", "Apache-2.0")
        );
        assert_eq!(
            o.url,
            "https://raw.githubusercontent.com/modelcontextprotocol/rust-sdk/0cde3c5cf3e6aff0cc852ce6045f107e95991f48/LICENSE"
        );
        assert_eq!(o.text.len(), 12_227);
        assert_eq!(
            sha256_hex(o.text),
            "0382b0057770ca05e9c350a50aa3b1c1fea84da0bc81d723bf00b9aa841be58a"
        );
        let text = std::str::from_utf8(o.text).expect("utf-8");
        assert!(text.starts_with("The MCP project is undergoing a licensing transition"));
        assert!(text.contains("                                 Apache License\n"));
        assert!(text.ends_with('\n'));

        // No license text and no override; an override for another version; one left unused;
        // one whose package ships a file; one whose package declares another license.
        let beta = package_dir(dir.path(), "beta-2.0.0", &[]);
        let rmcp_next = package_dir(dir.path(), "rmcp-3.5.1", &[]);
        let rmcp_file = package_dir(dir.path(), "rmcp-file", &[("LICENSE", b"x\n")]);
        let cases: Vec<(&str, Vec<Crate>, &str)> = vec![
            (
                "absent text",
                vec![krate("beta", "2.0.0", "MIT", &beta)],
                "beta 2.0.0",
            ),
            (
                "an override for another version",
                vec![krate("rmcp", "3.5.1", "Apache-2.0", &rmcp_next)],
                "rmcp 3.5.1",
            ),
            (
                "an unused override",
                vec![krate("alpha", "1.0.0", "MIT", &alpha)],
                "unused override rmcp 3.5.0",
            ),
            (
                "an override beside a license file",
                vec![krate("rmcp", "3.5.0", "Apache-2.0", &rmcp_file)],
                "unused override rmcp 3.5.0",
            ),
            (
                "an override of another declared license",
                vec![krate("rmcp", "3.5.0", "MIT", &rmcp)],
                "rmcp 3.5.0",
            ),
        ];
        for (what, crates, at) in cases {
            assert_eq!(
                notices_of(&crates, &OVERRIDES),
                Err(refused(Kind::Notices, at)),
                "{what}"
            );
        }
        let mut absent = krate("alpha", "1.0.0", "MIT", &alpha);
        absent.license = None;
        let mut empty = absent.clone();
        empty.license = Some(String::new());
        for c in [absent, empty] {
            assert_eq!(
                notices_of(&[c], &[]),
                Err(refused(Kind::Notices, "alpha 1.0.0"))
            );
        }

        // Links, sockets, FIFOs and text that is not UTF-8 refuse.
        type Spoil = fn(&Path);
        let spoils: [(&str, Spoil); 5] = [
            ("a symlinked file", |d| {
                fs::write(d.join("real"), b"x\n").expect("write");
                platform::symlink(&d.join("real"), &d.join("LICENSE-link")).expect("symlink");
            }),
            ("a symlinked directory", |d| {
                fs::create_dir(d.join("real")).expect("mkdir");
                platform::symlink(&d.join("real"), &d.join("LICENSES")).expect("symlink");
            }),
            ("a FIFO", |d| fifo(&d.join("LICENSE.fifo"))),
            ("a socket", |d| drop(unix_socket(&d.join("NOTICE.sock")))),
            ("text that is not UTF-8", |d| {
                fs::write(d.join("COPYING"), b"\xff\xfe").expect("write")
            }),
        ];
        for (i, (what, spoil)) in spoils.into_iter().enumerate() {
            // under the short root, so a socket path fits
            let pkg = tempfile::Builder::new()
                .prefix("plx7")
                .tempdir_in(crate::platform::short_tmp_root())
                .expect("tempdir");
            let p = package_dir(pkg.path(), &format!("p{i}"), &[("LICENSE", b"ok\n")]);
            spoil(&p);
            assert_eq!(
                notices_of(&[krate("spoiled", "1.0.0", "MIT", &p)], &[]),
                Err(refused(Kind::Notices, "spoiled 1.0.0")),
                "{what}"
            );
        }
        // A matching name that is not UTF-8 refuses; others are ignored.
        assert_eq!(license_name(b"LICENSE-\xff"), Err(Kind::Notices));
        assert_eq!(license_name(b"notice\xff"), Err(Kind::Notices));
        assert_eq!(license_name(b"README-\xff"), Ok(None));
        for (raw, want) in [
            (&b"licence.txt"[..], Some("licence.txt")),
            (b"Copying", Some("Copying")),
            (b"NOTICE", Some("NOTICE")),
            (b"LICENSE", Some("LICENSE")),
            (b"LICENS", None),
            (b"XLICENSE", None),
            (b"UNLICENSE", None),
            (b"", None),
        ] {
            assert_eq!(
                license_name(raw),
                Ok(want),
                "{:?}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn plx7_mpl_selection_and_checksums_come_from_the_lock() {
        for (expression, want) in [
            ("MPL-2.0", Ok(true)),
            ("MPL-2.0+", Ok(true)),
            ("(MPL-2.0)", Ok(true)),
            ("(MPL-2.0 OR MPL-2.0+)", Ok(true)),
            ("MPL-2.0 AND (MPL-2.0+ OR MPL-2.0)", Ok(true)),
            ("MIT OR MPL-2.0", Ok(false)),
            ("MPL-2.0 AND MIT", Ok(false)),
            ("(MIT OR Apache-2.0) AND MPL-2.0", Ok(false)),
            ("MIT/Apache-2.0", Ok(false)),
            ("MPL-2.0/Apache-2.0", Ok(false)),
            ("MPL-2.0-no-copyleft-exception", Ok(false)),
            ("MIT", Ok(false)),
            ("Apache-2.0 WITH LLVM-exception", Ok(false)),
            ("MPL-2.0 WITH LLVM-exception", Ok(false)),
            ("MPL 2.0", Err(Kind::Notices)),
            ("mpl-2.0", Err(Kind::Notices)),
            ("Foo-1.0", Err(Kind::Notices)),
            ("MIT AND", Err(Kind::Notices)),
            ("(MIT", Err(Kind::Notices)),
            ("", Err(Kind::Notices)),
        ] {
            assert_eq!(mpl_only(expression), want, "{expression:?}");
        }
        // strict SPDX alone would refuse the slash, which two linked crates declare
        assert!(spdx::Expression::parse_mode("MIT/Apache-2.0", spdx::ParseMode::STRICT).is_err());

        // The current lock: names, versions and both hashes.
        let root = release_install::repository_root();
        let lock = parse_lock(&read_lock(&root.join("Cargo.lock")).expect("read")).expect("parse");
        assert_eq!(
            checksum(&lock, "imbl", "7.0.2", CRATES_IO),
            Ok(IMBL_SHA.into())
        );
        assert_eq!(
            checksum(&lock, "imbl-sized-chunks", "0.2.0", CRATES_IO),
            Ok(CHUNKS_SHA.into())
        );
        for target in TARGETS {
            assert_eq!(
                for_target(target).map(|n| n.mpl),
                Ok(locked_mpl()),
                "{target}"
            );
        }
        // The notices carry the lock's checksum, whatever it is.
        let dir = tempfile::tempdir().expect("tempdir");
        let d = package_dir(dir.path(), "imbl-7.0.2", &[("LICENSE.md", b"MPL\n")]);
        let mut c = krate("imbl", "7.0.2", "MPL-2.0+", &d);
        c.checksum = "cd".repeat(32);
        let n = notices_of(&[c], &[]).expect("notices");
        assert_eq!(n.mpl[0].sha256, "cd".repeat(32));
        assert!(
            String::from_utf8(n.bytes)
                .expect("utf-8")
                .contains(&format!(
                    "Source: imbl-7.0.2.crate; SHA-256: {}\n",
                    "cd".repeat(32)
                ))
        );

        // Malformed locks and entries.
        for bad in [
            &b"\xff"[..],
            b"[[package]]\nname = 1\n",
            b"not toml =",
            b"package = 3\n",
        ] {
            assert_eq!(
                parse_lock(bad).map_err(|r| r.kind),
                Err(Kind::Lockfile),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        let good = lock_entry("imbl", "7.0.2", IMBL_SHA);
        let mut no_sum = good.clone();
        no_sum.checksum = None;
        let mut upper = good.clone();
        upper.checksum = Some(IMBL_SHA.to_uppercase());
        let mut short = good.clone();
        short.checksum = Some(IMBL_SHA[1..].into());
        let mut other_source = good.clone();
        other_source.source = Some("registry+https://example.com/index".into());
        let mut other_version = good.clone();
        other_version.version = "7.0.1".into();
        for (what, lock) in [
            ("no checksum", vec![no_sum]),
            ("an uppercase checksum", vec![upper]),
            ("a 63-digit checksum", vec![short]),
            ("two entries", vec![good.clone(), good.clone()]),
            ("another source", vec![other_source]),
            ("another version", vec![other_version]),
            ("no entry", vec![]),
        ] {
            assert_eq!(
                checksum(&lock, "imbl", "7.0.2", CRATES_IO),
                Err(refused(Kind::Lockfile, "imbl 7.0.2")),
                "{what}"
            );
        }
        let p = registry("imbl", "7.0.2", "MPL-2.0+");
        assert_eq!(
            crates_of(&[&p], &[]),
            Err(refused(Kind::Lockfile, "imbl 7.0.2"))
        );
        // A substituted checksum fails at the copy, against the cached bytes.
        let cache = dir.path().join("cache");
        fs::create_dir(&cache).expect("mkdir");
        fs::write(cache.join("imbl-7.0.2.crate"), b"the cached crate").expect("write");
        let source = Source {
            filename: "imbl-7.0.2.crate".into(),
            sha256: sha256_hex(b"another crate"),
            cache: cache.join("imbl-7.0.2.crate"),
        };
        let out = dir.path().join("out");
        assert_eq!(
            stage_sources(&[source], &out, &|_, _| {}),
            Err(refused(Kind::Source, "imbl-7.0.2.crate"))
        );
        assert!(fs::symlink_metadata(&out).is_err(), "no OUT remains");
    }

    #[test]
    fn plx7_notice_bounds_refuse_before_output() {
        // Cargo's output: N bytes pass, N+1 is the size limit; a failure is the graph refusal.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = |s: &'static str| move || shell_script(s);
        let at_cap = script("head -c 16777216 /dev/zero");
        let over = script("head -c 16777217 /dev/zero");
        let split = script("head -c 8388608 /dev/zero; head -c 8388609 /dev/zero >&2");
        let fails = script("exit 101");
        let not_utf8 = script("case \"$0\" in tree) printf '\\377\\n';; esac");
        assert_eq!(
            run_cargo(&with_cargo(dir.path(), &at_cap), &["tree"]).map(|o| o.len()),
            Ok(CARGO_OUTPUT_MAX)
        );
        assert_eq!(
            run_cargo(&with_cargo(dir.path(), &over), &["tree"]),
            Err(refused(Kind::SizeLimit, "tree"))
        );
        assert_eq!(
            run_cargo(&with_cargo(dir.path(), &split), &["metadata"]),
            Err(refused(Kind::SizeLimit, "metadata")),
            "stdout and stderr count together"
        );
        assert_eq!(
            run_cargo(&with_cargo(dir.path(), &fails), &["tree"]),
            Err(refused(Kind::Graph, "tree"))
        );
        // a well-formed (empty) lock, which is read before Cargo runs
        fs::write(dir.path().join("Cargo.lock"), b"version = 4\n").expect("write");
        assert_eq!(
            gather(TARGETS[0], &with_cargo(dir.path(), &not_utf8)),
            Err(refused(Kind::Graph, "tree output"))
        );
        assert_eq!(
            gather(
                "riscv64gc-unknown-linux-gnu",
                &with_cargo(dir.path(), &fails)
            ),
            Err(refused(Kind::Arguments, "riscv64gc-unknown-linux-gnu"))
        );

        // Metadata: 4096 packages and a 4096-byte manifest path pass; one more of either fails.
        let json = |count: usize, path_len: usize| {
            let packages: Vec<serde_json::Value> = (0..count)
                .map(|i| {
                    let manifest = format!("/{}", "m".repeat(path_len - 1));
                    serde_json::json!({
                        "id": format!("p{i}"), "name": format!("p{i}"), "version": "1.0.0",
                        "source": null, "manifest_path": manifest, "license": "MIT"
                    })
                })
                .collect();
            serde_json::to_vec(&serde_json::json!({
                "packages": packages, "workspace_members": []
            }))
            .expect("json")
        };
        assert_eq!(
            parse_metadata(&json(METADATA_PACKAGES_MAX, 20)).map(|m| m.packages.len()),
            Ok(4096)
        );
        assert_eq!(
            parse_metadata(&json(METADATA_PACKAGES_MAX + 1, 20)).map_err(|r| r.kind),
            Err(Kind::SizeLimit)
        );
        assert_eq!(
            parse_metadata(&json(1, PATH_MAX)).map(|m| m.packages.len()),
            Ok(1)
        );
        assert_eq!(
            parse_metadata(&json(1, PATH_MAX + 1)),
            Err(refused(Kind::SizeLimit, "p0 1.0.0 manifest path"))
        );
        assert_eq!(
            parse_metadata(b"{\"packages\": 1}").map_err(|r| r.kind),
            Err(Kind::Graph)
        );

        // Cargo.lock: N bytes are read, N+1 is refused before any parse.
        for (len, want) in [(LOCK_MAX, None), (LOCK_MAX + 1, Some(Kind::SizeLimit))] {
            let lock = dir.path().join(format!("lock-{len}"));
            File::create(&lock)
                .and_then(|f| f.set_len(len))
                .expect("a sparse lockfile");
            let got = read_lock(&lock);
            match want {
                None => assert_eq!(got.map(|b| b.len() as u64), Ok(LOCK_MAX)),
                Some(kind) => assert_eq!(got.map(|_| ()), Err(refused(kind, "Cargo.lock"))),
            }
        }

        // License files: 128 per package and 65536 bytes each pass; one more of either fails.
        for (count, want) in [
            (LICENSE_FILES_MAX, None),
            (LICENSE_FILES_MAX + 1, Some(Kind::SizeLimit)),
        ] {
            let d = package_dir(dir.path(), &format!("many-{count}"), &[]);
            for i in 0..count {
                fs::write(d.join(format!("LICENSE-{i:03}")), b"x\n").expect("write");
            }
            let got = notices_of(&[krate("many", "1.0.0", "MIT", &d)], &[]);
            match want {
                None => assert_eq!(headings(&got.expect("notices").bytes), ["many 1.0.0"]),
                Some(kind) => assert_eq!(got, Err(refused(kind, "many 1.0.0"))),
            }
        }
        for (len, want) in [
            (LICENSE_FILE_MAX, None),
            (LICENSE_FILE_MAX + 1, Some(Kind::SizeLimit)),
        ] {
            let d = package_dir(dir.path(), &format!("big-{len}"), &[]);
            fs::write(d.join("LICENSE"), vec![b'a'; len as usize]).expect("write");
            let got = notices_of(&[krate("big", "1.0.0", "MIT", &d)], &[]);
            match want {
                None => assert!(got.expect("notices").bytes.len() > LICENSE_FILE_MAX as usize),
                Some(kind) => assert_eq!(got, Err(refused(kind, "big 1.0.0"))),
            }
        }

        // License expressions: 1024 bytes and 64 levels pass; one more of either fails.
        let long = |n: usize| format!("LicenseRef-{}", "a".repeat(n - "LicenseRef-".len()));
        assert_eq!(mpl_only(&long(EXPRESSION_MAX)), Ok(false));
        assert_eq!(mpl_only(&long(EXPRESSION_MAX + 1)), Err(Kind::SizeLimit));
        let nested = |n: usize| format!("{}MPL-2.0{}", "(".repeat(n), ")".repeat(n));
        assert_eq!(mpl_only(&nested(EXPRESSION_DEPTH_MAX)), Ok(true));
        assert_eq!(
            mpl_only(&nested(EXPRESSION_DEPTH_MAX + 1)),
            Err(Kind::SizeLimit)
        );

        // Checked arithmetic.
        assert_eq!(grown(usize::MAX, 1, usize::MAX), None);
        assert_eq!(
            grown(NOTICES_LEN_MAX, 0, NOTICES_LEN_MAX),
            Some(NOTICES_LEN_MAX)
        );
        assert_eq!(grown(NOTICES_LEN_MAX, 1, NOTICES_LEN_MAX), None);

        // The notices: exactly 16777216 bytes pass, one more fails. 255 full license files
        // and a last one sized to land on the cap.
        let notices_root = dir.path().join("notices");
        let mut crates = Vec::new();
        let full = {
            let mut v = vec![b'n'; LICENSE_FILE_MAX as usize - 1];
            v.push(b'\n');
            v
        };
        for i in 0..256 {
            let d = package_dir(&notices_root, &format!("p{i:03}"), &[]);
            fs::write(d.join("LICENSE"), if i < 255 { &full[..] } else { b"\n" }).expect("write");
            crates.push(krate(&format!("p{i:03}"), "1.0.0", "MIT", &d));
        }
        let base = notices_of(&crates, &[]).expect("notices").bytes.len();
        let last = 1 + NOTICES_LEN_MAX - base;
        assert!((1..=LICENSE_FILE_MAX as usize).contains(&last), "{last}");
        for (len, want) in [(last, None), (last + 1, Some(Kind::SizeLimit))] {
            let mut text = vec![b'n'; len - 1];
            text.push(b'\n');
            fs::write(crates[255].dir.join("LICENSE"), &text).expect("write");
            let got = notices_of(&crates, &[]);
            match want {
                None => assert_eq!(got.map(|n| n.bytes.len()), Ok(NOTICES_LEN_MAX)),
                Some(kind) => assert_eq!(got, Err(refused(kind, "p255 1.0.0"))),
            }
        }

        // A crate file: 64 MiB is copied and hashed; one byte more is refused before OUT.
        let cache = dir.path().join("cache");
        fs::create_dir(&cache).expect("mkdir");
        let zeros = {
            let mut h = Sha256::new();
            let chunk = vec![0u8; COPY_CHUNK];
            for _ in 0..(CRATE_MAX as usize / COPY_CHUNK) {
                h.update(&chunk);
            }
            format!("{:x}", h.finalize())
        };
        for (len, want) in [(CRATE_MAX, None), (CRATE_MAX + 1, Some(Kind::SizeLimit))] {
            let path = cache.join(format!("big-{len}.crate"));
            File::create(&path)
                .and_then(|f| f.set_len(len))
                .expect("a sparse crate file");
            let source = Source {
                filename: "big-1.0.0.crate".into(),
                sha256: zeros.clone(),
                cache: path,
            };
            let out = dir.path().join(format!("out-{len}"));
            let got = stage_sources(&[source], &out, &|_, _| {});
            match want {
                None => {
                    assert_eq!(got, Ok(()));
                    assert_eq!(
                        fs::metadata(out.join("big-1.0.0.crate"))
                            .expect("copy")
                            .len(),
                        CRATE_MAX
                    );
                }
                Some(kind) => {
                    assert_eq!(got, Err(refused(kind, "big-1.0.0.crate")));
                    assert!(fs::symlink_metadata(&out).is_err(), "no OUT");
                }
            }
        }
        // The release notes file: 1048576 bytes are read, one more is refused.
        for (len, want) in [(NOTES_MAX, MSG_NOTES), (NOTES_MAX + 1, MSG_SIZE_LIMIT)] {
            let notes = dir.path().join(format!("notes-{len}"));
            fs::write(&notes, vec![b'x'; len as usize]).expect("write");
            let got = check_release_file("0.19.0", &notes);
            assert_eq!(got, Err(want.to_string()), "{len}");
        }
        let notes = dir.path().join("notes-utf8");
        fs::write(&notes, b"\xff").expect("write");
        assert_eq!(
            check_release_file("0.19.0", &notes),
            Err(MSG_NOTES.to_string())
        );
    }

    /// Two cached crate files (one of several chunks) and their sources.
    fn cached(dir: &Path) -> (Vec<Source>, Vec<Vec<u8>>) {
        let cache = dir.join("registry/cache/index");
        fs::create_dir_all(&cache).expect("mkdir");
        let small = b"a small crate\n".to_vec();
        let large: Vec<u8> = (0..(3 * COPY_CHUNK + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let mut sources = Vec::new();
        for (name, bytes) in [("alpha-1.0.0.crate", &small), ("beta-2.0.0.crate", &large)] {
            fs::write(cache.join(name), bytes).expect("write");
            sources.push(Source {
                filename: name.into(),
                sha256: sha256_hex(bytes),
                cache: cache.join(name),
            });
        }
        (sources, vec![small, large])
    }

    /// The sorted names in `dir`.
    fn listed(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn plx7_sources_are_exact_unsigned_release_assets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (sources, bytes) = cached(dir.path());
        let out = dir.path().join("out");
        let chunks = Cell::new(0);
        stage_sources(&sources, &out, &|_, _| chunks.set(chunks.get() + 1)).expect("stage");
        assert_eq!(chunks.get(), 1 + 4, "one hash pass, chunk by chunk");
        assert_eq!(listed(&out), ["alpha-1.0.0.crate", "beta-2.0.0.crate"]);
        for (s, b) in sources.iter().zip(&bytes) {
            let copy = out.join(&s.filename);
            assert_eq!(&fs::read(&copy).expect("copy"), b);
            assert_eq!(sha256_hex(b), s.sha256);
            assert_eq!(platform::mode_of(&copy).expect("mode"), 0o644);
            assert_eq!(
                &fs::read(&s.cache).expect("cache"),
                b,
                "the cache is unchanged"
            );
        }
        for unsigned in [MANIFEST_FILE, SIGNATURE_FILE] {
            assert!(!out.join(unsigned).exists(), "no {unsigned}");
        }
        // OUT is never reused or overwritten.
        assert_eq!(
            stage_sources(&sources, &out, &|_, _| {}),
            Err(refused(Kind::OutputExists, "OUT"))
        );
        assert_eq!(listed(&out), ["alpha-1.0.0.crate", "beta-2.0.0.crate"]);
        for (s, b) in sources.iter().zip(&bytes) {
            assert_eq!(&fs::read(out.join(&s.filename)).expect("copy"), b);
        }

        // A missing, linked or substituted nonregular source fails before OUT.
        type Spoil = fn(&Path);
        let spoils: [(&str, Spoil); 4] = [
            ("missing", |p| fs::remove_file(p).expect("rm")),
            ("a symlink", |p| {
                let real = p.with_extension("real");
                fs::rename(p, &real).expect("mv");
                platform::symlink(&real, p).expect("symlink");
            }),
            ("a FIFO", |p| {
                fs::remove_file(p).expect("rm");
                fifo(p);
            }),
            ("a directory", |p| {
                fs::remove_file(p).expect("rm");
                fs::create_dir(p).expect("mkdir");
            }),
        ];
        for (what, spoil) in spoils {
            let d = tempfile::tempdir().expect("tempdir");
            let (sources, _) = cached(d.path());
            spoil(&sources[1].cache);
            let out = d.path().join("out");
            assert_eq!(
                stage_sources(&sources, &out, &|_, _| {}),
                Err(refused(Kind::Source, "beta-2.0.0.crate")),
                "{what}"
            );
            assert!(fs::symlink_metadata(&out).is_err(), "{what}: no OUT");
        }

        // Wrong bytes fail at the copy's hash; this run's copies and OUT go again.
        let d = tempfile::tempdir().expect("tempdir");
        let (mut sources, bytes) = cached(d.path());
        sources[1].sha256 = sha256_hex(b"other bytes");
        let out = d.path().join("out");
        assert_eq!(
            stage_sources(&sources, &out, &|_, _| {}),
            Err(refused(Kind::Source, "beta-2.0.0.crate"))
        );
        assert!(fs::symlink_metadata(&out).is_err(), "no OUT remains");
        assert_eq!(fs::read(&sources[1].cache).expect("cache"), bytes[1]);

        // A change in place during the copy (signalled after the first chunk) fails the hash.
        let d = tempfile::tempdir().expect("tempdir");
        let (sources, bytes) = cached(d.path());
        let out = d.path().join("out");
        let changed = Cell::new(false);
        let target = sources[1].cache.clone();
        let during = |name: &str, copied: u64| {
            if name == "beta-2.0.0.crate" && copied == COPY_CHUNK as u64 && !changed.get() {
                use std::io::{Seek, SeekFrom};
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .open(&target)
                    .expect("open");
                f.seek(SeekFrom::Start(2 * COPY_CHUNK as u64))
                    .expect("seek");
                f.write_all(b"changed in place").expect("write");
                changed.set(true);
            }
        };
        assert_eq!(
            stage_sources(&sources, &out, &during),
            Err(refused(Kind::Source, "beta-2.0.0.crate"))
        );
        assert!(changed.get(), "the change happened mid-copy");
        assert!(fs::symlink_metadata(&out).is_err(), "no OUT remains");
        assert_eq!(fs::read(&sources[0].cache).expect("cache"), bytes[0]);

        // A replacement put in OUT during the run is never deleted or overwritten: the run
        // fails, removes only its own copies, and leaves OUT holding the replacement.
        let d = tempfile::tempdir().expect("tempdir");
        let (mut sources, _) = cached(d.path());
        sources[1].sha256 = sha256_hex(b"other bytes");
        let out = d.path().join("out");
        let out_path = out.clone();
        let swapped = Cell::new(false);
        let during = |name: &str, _: u64| {
            if name == "beta-2.0.0.crate" && !swapped.get() {
                fs::rename(out_path.join("alpha-1.0.0.crate"), out_path.join("moved"))
                    .expect("move the copy");
                fs::write(out_path.join("alpha-1.0.0.crate"), b"someone else's\n")
                    .expect("replace");
                swapped.set(true);
            }
        };
        assert_eq!(
            stage_sources(&sources, &out, &during),
            Err(refused(Kind::FileOperation, "cleanup"))
        );
        assert_eq!(listed(&out), ["alpha-1.0.0.crate", "moved"]);
        assert_eq!(
            fs::read(out.join("alpha-1.0.0.crate")).expect("replacement"),
            b"someone else's\n"
        );
    }

    /// Set in the child that [`plx7_source_staging_owns_its_outputs_when_descriptors_run_out`]
    /// re-executes under a 128-descriptor limit.
    const DESCRIPTOR_CHILD: &str = "RDY_PLX7_DESCRIPTOR_CHILD";

    /// Source staging owns each output from the moment it exists, and its cleanup still has
    /// the descriptor it needs. The staging runs in a re-executed child under a 128-descriptor
    /// limit, where filling every free descriptor touches no other test; after the first
    /// source's first chunk, `during` takes every free descriptor (or all but one).
    #[test]
    fn plx7_source_staging_owns_its_outputs_when_descriptors_run_out() {
        let name =
            crate::test_name!("plx7_source_staging_owns_its_outputs_when_descriptors_run_out");
        if std::env::var_os(DESCRIPTOR_CHILD).is_none() {
            let mut child = shell_script("ulimit -n 128 && exec \"$0\" \"$@\"");
            child
                .arg(std::env::current_exe().expect("current exe"))
                .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
                .env(DESCRIPTOR_CHILD, "1");
            let out =
                crate::fixtures::output_within(&mut child, std::time::Duration::from_secs(600))
                    .expect("run the child");
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && text.contains("test result: ok. 1 passed"),
                "the child ran this one test and it passed: {out:?}"
            );
            return;
        }
        let base = tempfile::tempfile().expect("a handle to copy");
        let held: std::cell::RefCell<Vec<File>> = std::cell::RefCell::new(Vec::new());
        // Copies `base` until no descriptor is free, then gives `free` of them back.
        let exhaust = |free: usize| {
            let mut held = held.borrow_mut();
            while held.len() < 4096 {
                match base.try_clone() {
                    Ok(f) => held.push(f),
                    Err(_) => break,
                }
            }
            assert!(held.len() < 4096, "the 128-descriptor limit holds");
            for _ in 0..free {
                held.pop();
            }
        };

        // No descriptor free: the second output cannot be made. The run fails, and its
        // cleanup still removes the first copy and OUT.
        let d = tempfile::tempdir().expect("tempdir");
        let (sources, bytes) = cached(d.path());
        let out = d.path().join("out");
        let filled = Cell::new(false);
        let during = |name: &str, _: u64| {
            if name == "alpha-1.0.0.crate" && !filled.replace(true) {
                exhaust(0);
            }
        };
        let got = stage_sources(&sources, &out, &during);
        held.borrow_mut().clear();
        assert!(filled.get(), "the descriptors ran out mid-run");
        assert_eq!(got, Err(refused(Kind::FileOperation, "beta-2.0.0.crate")));
        assert!(
            fs::symlink_metadata(&out).is_err(),
            "no owned output remains"
        );
        for (s, b) in sources.iter().zip(&bytes) {
            assert_eq!(
                &fs::read(&s.cache).expect("cache"),
                b,
                "the cache is unchanged"
            );
        }
        // A retry with descriptors to spare finds no OUT in its way.
        stage_sources(&sources, &out, &|_, _| {}).expect("the retry stages");
        assert_eq!(listed(&out), ["alpha-1.0.0.crate", "beta-2.0.0.crate"]);

        // One descriptor free: the second output takes it and is copied through that one
        // handle; the run succeeds with both exact copies.
        let d = tempfile::tempdir().expect("tempdir");
        let (sources, bytes) = cached(d.path());
        let out = d.path().join("out");
        let filled = Cell::new(false);
        let during = |name: &str, _: u64| {
            if name == "alpha-1.0.0.crate" && !filled.replace(true) {
                exhaust(1);
            }
        };
        let got = stage_sources(&sources, &out, &during);
        held.borrow_mut().clear();
        assert!(filled.get(), "the descriptors ran out mid-run");
        assert_eq!(got, Ok(()));
        assert_eq!(listed(&out), ["alpha-1.0.0.crate", "beta-2.0.0.crate"]);
        for (s, b) in sources.iter().zip(&bytes) {
            let copy = out.join(&s.filename);
            assert_eq!(&fs::read(&copy).expect("copy"), b);
            assert_eq!(platform::mode_of(&copy).expect("mode"), 0o644);
        }
    }

    /// The tool reads, bounds and parses `Cargo.lock` before Cargo runs: a malformed lock is the
    /// lockfile refusal (one line on stderr, no stdout, no OUT), with or without `--check-notes`,
    /// and Cargo never runs.
    #[test]
    fn plx7_source_tool_refuses_a_malformed_lock_before_cargo() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("Cargo.lock"), b"[[package]\n").expect("write");
        let notes = dir.path().join("notes.md");
        fs::write(&notes, notes_with("")).expect("write");
        let runs = Cell::new(0);
        let cargo = || {
            runs.set(runs.get() + 1);
            cargo_command()
        };
        let src = with_cargo(dir.path(), &cargo);
        let out = dir.path().join("out");
        let plain: Vec<OsString> = vec![
            "qa-release-sources".into(),
            "--version".into(),
            "0.19.0".into(),
            "--out".into(),
            out.clone().into(),
        ];
        let mut checked = plain.clone();
        checked.extend(["--check-notes".into(), notes.clone().into()]);
        for args in [plain, checked] {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let code = tool_with(&args, &src, &mut stdout, &mut stderr);
            assert_eq!(code, 1, "{args:?}");
            assert_eq!(
                String::from_utf8_lossy(&stderr),
                format!("{MSG_LOCKFILE}\n"),
                "{args:?}"
            );
            assert!(stdout.is_empty(), "{args:?}: no stdout");
            assert!(fs::symlink_metadata(&out).is_err(), "{args:?}: no OUT");
        }
        assert_eq!(runs.get(), 0, "Cargo never runs");
    }

    /// The changes part of 0.19.0's test notes: after 0.17.0, with the format move line.
    fn changes_part() -> String {
        let line = release_format_move_line("0.19.0")
            .expect("a release before 0.19.0")
            .expect("the formats moved since 0.17.0");
        format!(
            "## What changed since 0.17.0\n\n### New features\n\n- A feature.\n\n### Changes\n\n{line}\n- Another change.\n\n### Fixes\n\n- A fix.\n\n"
        )
    }

    /// Release notes with the changes part after Try it, then `part` after Feedback and before
    /// the two collapsed sections.
    fn notes_with(part: &str) -> String {
        format!(
            "## Try it\n\nSteps.\n\n{}## Feedback\n\nTell us.\n\n{part}\n<details>\n<summary><b>About this preview</b></summary>\n\nText.\n\n</details>\n\n<details>\n<summary><b>Verify the download (optional)</b></summary>\n\nMore.\n\n</details>\n",
            changes_part()
        )
    }

    /// The published 0.20.0 row is the previous release of later versions (TASK-540).
    #[test]
    fn plz2_published_020_is_the_previous_release() {
        assert_eq!(
            previous_release("0.21.0", RELEASED),
            Some(("0.20.0", 3, 18))
        );
        assert_eq!(
            previous_release("0.20.0", RELEASED),
            Some(("0.17.0", 3, 16))
        );
        assert_eq!(
            previous_release("0.19.0", RELEASED),
            Some(("0.17.0", 3, 16))
        );
        assert_eq!(previous_release("0.17.0", RELEASED), None);
    }

    /// The changes part (D-7PQEF): in place after Try it and before Feedback, with known groups
    /// in order, and the format move line in its Changes group when the formats moved since the
    /// previous release.
    #[test]
    fn plx7_notes_changes_part_names_the_format_move() {
        assert_eq!(
            MSG_CHANGES,
            "release sources: release notes lack the changes part"
        );
        assert_eq!(
            MSG_FORMAT_MOVE,
            "release sources: release notes do not name the format move"
        );

        // The move line, for each kind of move, and none without one.
        assert_eq!(
            format_move_line("0.20.0", ("0.17.0", 3, 16), (3, 18)).as_deref(),
            Some(
                "- **Breaking: the store format moves from 16 to 18 (dump pair (3, 16) to (3, 18)).** rdy 0.20.0 refuses a store made by rdy 0.17.0 with `E_STORE_FORMAT`. To move a project, run `rdy dump --out <file>` with rdy 0.17.0, then `rdy load <file> <dir>` with rdy 0.20.0, into a directory with no store."
            )
        );
        assert_eq!(
            format_move_line("0.21.0", ("0.20.0", 3, 18), (4, 18)).as_deref(),
            Some(
                "- **Breaking: the dump format moves from 3 to 4 (dump pair (3, 18) to (4, 18)).** rdy 0.21.0 opens a store made by rdy 0.20.0 as it is, and loads every earlier dump. rdy 0.20.0 cannot load a dump made by rdy 0.21.0."
            )
        );
        assert_eq!(
            format_move_line("0.21.0", ("0.17.0", 3, 16), (4, 18)).as_deref(),
            Some(
                "- **Breaking: the store format moves from 16 to 18 (dump pair (3, 16) to (4, 18)).** rdy 0.21.0 refuses a store made by rdy 0.17.0 with `E_STORE_FORMAT`. To move a project, run `rdy dump --out <file>` with rdy 0.17.0, then `rdy load <file> <dir>` with rdy 0.21.0, into a directory with no store."
            )
        );
        assert_eq!(format_move_line("0.21.0", ("0.20.0", 3, 18), (3, 18)), None);
        // O18: from the last scalar store to feature-based stores
        assert_eq!(
            format_move_line("0.26.0", ("0.20.0", 3, 18), current_store()).as_deref(),
            Some(
                "- **Breaking: the store format moves from 18 to feature-based stores (dump pair (3, 18) to dump format 4).** rdy 0.26.0 refuses a store made by rdy 0.20.0 with `E_STORE_FORMAT`. To move a project, run `rdy dump --out <file>` with rdy 0.20.0, then `rdy load <file> <dir>` with rdy 0.26.0, into a directory with no store."
            )
        );
        assert_eq!(current_store(), WrittenStore::Features(4));

        // The previous release: the newest published row before the version.
        let table = [("0.17.0", 3, 16), ("0.20.0", 3, 18)];
        assert_eq!(previous_release("0.21.0", &table), Some(("0.20.0", 3, 18)));
        assert_eq!(previous_release("0.20.0", &table), Some(("0.17.0", 3, 16)));
        assert_eq!(previous_release("0.19.0", &table), Some(("0.17.0", 3, 16)));
        assert_eq!(previous_release("0.17.0", &table), None);
        assert_eq!(previous_release("0.100.0", &table), Some(("0.20.0", 3, 18)));
        assert_eq!(
            release_format_move_line("0.17.0"),
            Err(MSG_INVALID_ARGUMENTS.to_string())
        );

        // Good notes pass.
        let v = "0.19.0";
        let good = notes_with(&notes_part(v, &locked_mpl()).expect("part"));
        assert_eq!(check_release_text(v, &good), Ok(()));
        let line = release_format_move_line(v).expect("ok").expect("a move");
        let heading = changes_heading("0.17.0");
        assert_eq!(check_changes(&heading, Some(&line), &good), Ok(()));
        let only_changes = good.replacen("### New features\n\n- A feature.\n\n", "", 1);
        assert_eq!(
            check_release_text(v, &only_changes),
            Ok(()),
            "groups may be left out"
        );
        assert_eq!(
            check_changes(&heading, None, &good.replacen(&format!("{line}\n"), "", 1)),
            Ok(()),
            "no move, no line"
        );

        // Notes without the part, or with it out of place or out of shape, refuse.
        let part = changes_part();
        let without = good.replacen(&part, "", 1);
        let wrong_release = good.replacen(&heading, "## What changed since 0.16.0", 1);
        let twice = good.replacen(&part, &format!("{part}{part}"), 1);
        let before_try =
            good.replacen(&part, "", 1)
                .replacen("## Try it\n", &format!("{part}## Try it\n"), 1);
        let after_feedback =
            good.replacen(&part, "", 1)
                .replacen("Tell us.\n\n", &format!("Tell us.\n\n{part}"), 1);
        let between = good.replacen("### Fixes", "## Other\n\n### Fixes", 1);
        let unknown_group = good.replacen("### Fixes", "### Improvements", 1);
        let out_of_order = good.replacen("### New features", "### Security", 1);
        let repeated = good.replacen("### Fixes", "### Changes", 1);
        let no_group = good
            .replacen("### New features\n\n", "", 1)
            .replacen("### Changes\n\n", "", 1)
            .replacen("### Fixes\n\n", "", 1);
        for (what, text) in [
            ("no part", &without),
            ("the wrong previous release", &wrong_release),
            ("two parts", &twice),
            ("before Try it", &before_try),
            ("another heading before Feedback", &between),
            ("an unknown group", &unknown_group),
            ("groups out of order", &out_of_order),
            ("a group twice", &repeated),
            ("no group", &no_group),
        ] {
            assert_eq!(
                check_changes(&heading, Some(&line), text).map_err(|r| r.kind),
                Err(Kind::Changes),
                "{what}"
            );
            assert_eq!(
                check_release_text(v, text),
                Err(MSG_CHANGES.to_string()),
                "{what}"
            );
        }

        // After Feedback, the part also moves the MPL part off Feedback: the notes refuse on that
        // first.
        assert_eq!(
            check_changes(&heading, Some(&line), &after_feedback).map_err(|r| r.kind),
            Err(Kind::Changes)
        );
        assert_eq!(
            check_release_text(v, &after_feedback),
            Err(MSG_NOTES.to_string())
        );

        // Notes without the format move line, or with it outside the Changes group, refuse.
        let no_line = good.replacen(&format!("{line}\n"), "", 1);
        // The edit names another previous store format; it must change the line whatever the
        // current store is (a scalar pair or feature-based stores).
        let edited = good.replacen(&line, &line.replace("from 16 to", "from 17 to"), 1);
        assert_ne!(edited, good, "the edit changes the line");
        let in_fixes = good.replacen(&format!("{line}\n"), "", 1).replacen(
            "- A fix.\n",
            &format!("- A fix.\n{line}\n"),
            1,
        );
        let no_changes_group = good
            .replacen(&format!("{line}\n"), "", 1)
            .replacen("### Changes", "### Fixes", 1)
            .replacen("### Fixes\n\n- A fix.", "### Security\n\n- A fix.", 1)
            .replacen("- A feature.\n", &format!("- A feature.\n{line}\n"), 1);
        for (what, text) in [
            ("no line", &no_line),
            ("an edited line", &edited),
            ("the line under Fixes", &in_fixes),
            ("no Changes group", &no_changes_group),
        ] {
            assert_eq!(
                check_changes(&heading, Some(&line), text).map_err(|r| r.kind),
                Err(Kind::FormatMove),
                "{what}"
            );
            assert_eq!(
                check_release_text(v, text),
                Err(MSG_FORMAT_MOVE.to_string()),
                "{what}"
            );
        }

        // A version with no published release before it is an argument error.
        assert_eq!(
            check_release_text("0.17.0", &good),
            Err(MSG_INVALID_ARGUMENTS.to_string())
        );
    }

    #[test]
    fn plx7_notes_and_readme_match_the_locked_mpl_set() {
        let v = "0.19.0";
        let want = concat!(
            "## Third-party source (MPL-2.0)\n",
            "\n",
            "rdy is licensed MIT OR Apache-2.0. The binaries also contain two libraries under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/): `imbl` 7.0.2 and `imbl-sized-chunks` 0.2.0. This release attaches the exact source code of each, as its crates.io crate file. Each SHA-256 matches rdy's `Cargo.lock` at this release:\n",
            "\n",
            "- [imbl-7.0.2.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.19.0/imbl-7.0.2.crate) — SHA-256: `46bad832b9b463ed9398b8506488cc2e3b897a9d44f13af115f382aac71f4fec`\n",
            "- [imbl-sized-chunks-0.2.0.crate](https://github.com/adrenickchrome/rdy-core/releases/download/v0.19.0/imbl-sized-chunks-0.2.0.crate) — SHA-256: `2a0813be332553f857953298749fa19549e8b61b80589757c29b4e2a804fa9c6`\n",
            "\n",
            "Each archive's `THIRD-PARTY-NOTICES.txt` gives the license of every library in the binary.\n",
        );
        let (mpl, sources) = union_sources(&Sources::release()).expect("both targets");
        assert_eq!(mpl, locked_mpl(), "read from the lock");
        let names: BTreeSet<&str> = mpl.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, BTreeSet::from(["imbl", "imbl-sized-chunks"]));
        assert_eq!(
            sources
                .iter()
                .map(|s| s.filename.as_str())
                .collect::<Vec<_>>(),
            ["imbl-7.0.2.crate", "imbl-sized-chunks-0.2.0.crate"]
        );
        assert_eq!(notes_part(v, &mpl).as_deref(), Ok(want));
        let mut reversed = mpl.clone();
        reversed.reverse();
        assert_eq!(notes_part(v, &reversed).as_deref(), Ok(want), "sorted");
        assert_eq!(
            notes_part(v, &[]).as_deref(),
            Ok(
                "## Third-party source (MPL-2.0)\n\nThe binaries contain no libraries used under MPL-2.0.\n"
            )
        );
        let mut three = mpl.clone();
        three.push(MplSource {
            name: "third".into(),
            version: "1.0.0".into(),
            filename: "third-1.0.0.crate".into(),
            sha256: "ab".repeat(32),
        });
        for set in [&mpl[..1], &three[..]] {
            assert_eq!(
                notes_part(v, set).map_err(|r| r.kind),
                Err(Kind::Notes),
                "{} entries",
                set.len()
            );
        }

        // The checker: the generated part, in place, passes; four broken sections do not.
        let good = notes_with(want);
        assert_eq!(check_notes(want, &good), Ok(()));
        assert_eq!(check_release_text(v, &good), Ok(()));
        let without = notes_with("").replacen("Tell us.\n\n\n", "Tell us.\n\n", 1);
        let duplicate = notes_with(&format!("{want}\n{want}"));
        let stale = notes_with(&want.replace(IMBL_SHA, &"0".repeat(64)));
        let misplaced = format!(
            "## What changed\n\n{want}\n<details>\nx\n</details>\n\n## Feedback\n\nTell us.\n\n<details>\ny\n</details>\n"
        );
        let after_details = notes_with("").replacen(
            "</details>\n\n<details>",
            &format!("</details>\n\n{want}\n<details>"),
            1,
        );
        let no_gap = notes_with(want).replacen(
            &format!("{want}\n<details>"),
            &format!("{want}<details>"),
            1,
        );
        for (what, text) in [
            ("missing", &without),
            ("duplicate", &duplicate),
            ("a stale hash", &stale),
            ("misplaced before Feedback", &misplaced),
            ("misplaced after the first details", &after_details),
            ("no blank line before details", &no_gap),
        ] {
            assert_eq!(
                check_notes(want, text).map_err(|r| r.kind),
                Err(Kind::Notes),
                "{what}"
            );
            assert_eq!(
                check_release_text(v, text),
                Err(MSG_NOTES.to_string()),
                "{what}"
            );
        }
        assert_eq!(
            check_release_text("0.19", &good),
            Err(MSG_INVALID_ARGUMENTS.to_string())
        );

        // The README's License section, exactly.
        let readme = fs::read_to_string(release_install::repository_root().join("README.md"))
            .expect("README");
        let license = readme
            .split_once("\n## License\n")
            .and_then(|(_, rest)| rest.split_once("\n## "))
            .map(|(section, _)| section)
            .expect("the License section");
        assert_eq!(
            license,
            concat!(
                "\n",
                "rdy is available under either [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your choice.\n",
                "\n",
                "rdy's binaries also contain open-source libraries under their own licenses. Two of them, `imbl` and `imbl-sized-chunks`, use the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/). Each release archive holds `THIRD-PARTY-NOTICES.txt` with every library's license. Each release also attaches the exact source code of the MPL-2.0 libraries.\n",
            )
        );

        // The release's eight assets; its signed manifest lists only the two archives.
        let archives: Vec<String> = TARGETS.iter().map(|t| archive_name(v, t)).collect();
        let mut assets: Vec<String> = archives.clone();
        assets.extend([MANIFEST_FILE, SIGNATURE_FILE, "AGENTS.md", "CLAUDE.md"].map(String::from));
        assets.extend(mpl.iter().map(|m| m.filename.clone()));
        let unique: BTreeSet<&String> = assets.iter().collect();
        assert_eq!((assets.len(), unique.len()), (8, 8), "{assets:?}");
        let manifest: Vec<&String> = assets.iter().filter(|a| a.ends_with(".tar.gz")).collect();
        assert_eq!(manifest, archives.iter().collect::<Vec<_>>());
        assert!(manifest.iter().all(|a| !a.ends_with(".crate")));
    }
}
