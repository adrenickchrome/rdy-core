//! The release README (D-81YJG, D-QGN3A; the O5 release assembly contract).
//!
//! Source capture puts three things in the release commit: `release/readme.md`, the exact export
//! of the `readme` profile; its SHA-256 sidecar `release/readme.sha256`; and the exported images
//! in `assets/readme/`. [`run`] rebuilds `README.md` from them: it inserts the four generated
//! parts (Archives, Release signing key, Release keys and Install) once, before the line that
//! closes the final verify fold. The parts come from the release targets and the authenticated
//! release-key records of the same root, never from authored text.
//!
//! Write mode replaces `README.md` only after every check passes. Check mode compares
//! `README.md` with the same assembly and writes nothing. Both modes read local bytes only. The
//! check proves reproducibility and the absence of corruption; it never proves that the source
//! was approved.
//!
//! `qa-release-readme` is the command.
use crate::release_install;
use crate::release_keys::ReleaseKeys;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

/// The largest source and the largest assembled README, in bytes (the README bound).
pub const README_MAX: u64 = 1_048_576;
/// The most entries in `assets/readme/`.
pub const ASSET_ENTRIES_MAX: usize = 100_000;
/// The largest image in `assets/readme/`, in bytes.
pub const IMAGE_MAX: u64 = 67_108_864;
/// The most distinct image bytes in `assets/readme/`.
pub const IMAGES_MAX: u64 = 268_435_456;
/// The largest workspace manifest read for the version, in bytes.
const MANIFEST_MAX: u64 = 1_048_576;

/// The source export, relative to the root.
pub const SOURCE_FILE: &str = "release/readme.md";
/// The source's SHA-256 sidecar, relative to the root.
pub const SIDECAR_FILE: &str = "release/readme.sha256";
/// The exported images, relative to the root. Every local image reference names a file here.
pub const ASSETS_DIR: &str = "assets/readme";
/// The repository README, relative to the root.
pub const README_FILE: &str = "README.md";
/// The authenticated release-key records, relative to the root.
pub const KEYS_DIR: &str = "release-keys";
/// The workspace manifest that holds the release version.
pub const WORKSPACE_FILE: &str = "Cargo.toml";
/// The new README's bytes before they replace `README.md`.
const TEMP_FILE: &str = ".README.md.qa-release-readme.tmp";

/// The title of the fold before the final one.
pub const ABOUT_TITLE: &str = "About Release 1";
/// The title of the final fold, which receives the generated parts.
pub const VERIFY_TITLE: &str = "Verify the download (optional)";
/// The headings of the generated parts, in order.
pub const GENERATED_HEADINGS: [&str; 4] = [
    "### Archives",
    "### Release signing key",
    "### Release keys",
    "### Install",
];
/// The extensions an exported image file may have (`rdy_render::assets::extension`).
const EXTENSIONS: [&str; 8] = ["png", "jpg", "gif", "svg", "webp", "pdf", "txt", "bin"];
/// The prefix of every local image reference.
const ASSET_PREFIX: &str = "assets/readme/";

/// The bounds of one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The largest source and assembled text, in bytes.
    pub text: u64,
    /// The most entries in the assets folder.
    pub entries: usize,
    /// The largest image, in bytes.
    pub image: u64,
    /// The most distinct image bytes.
    pub images: u64,
}

/// The release bounds.
pub const LIMITS: Limits = Limits {
    text: README_MAX,
    entries: ASSET_ENTRIES_MAX,
    image: IMAGE_MAX,
    images: IMAGES_MAX,
};

/// What a run does with the assembled text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Replace `README.md` with it.
    Write,
    /// Compare `README.md` with it, and write nothing.
    Check,
}

/// Why a run refused; each has one fixed line (the O5 table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadmeFailure {
    /// A missing, unknown or repeated argument, or a version that is not three numbers.
    Arguments,
    /// The version differs from the workspace version.
    Version,
    /// The release-key records are missing, refused, or select no signer.
    Keys,
    /// The source holds malformed UTF-8.
    Utf8,
    /// A CR, a missing final LF, or more than one final LF.
    LineEnds,
    /// The last two sections are not About Release 1 then the verify fold, or that fold is
    /// missing or malformed.
    VerifyFold,
    /// Zero or more than one insertion position, or a fold the reader cannot close.
    Insertion,
    /// A generated heading is already in the verify fold.
    Generated,
    /// The sidecar is not 64 lowercase hex digits and one LF.
    Sidecar,
    /// The source's SHA-256 differs from the sidecar.
    Digest,
    /// An extra, missing, nonregular or misnamed file, or a name that is not its bytes' SHA-256.
    Assets,
    /// A local image reference that is absolute, escapes the assets folder, or names no file.
    ImageRef,
    /// A text, entry or image bound.
    SizeLimit,
    /// Check mode: `README.md` differs from the assembled text.
    Mismatch,
    /// Any other read, write, rename or sync failure.
    FileOperation,
}

impl ReadmeFailure {
    /// The one line the command writes to stderr, without its LF.
    pub fn message(self) -> &'static str {
        match self {
            ReadmeFailure::Arguments => "release readme: invalid arguments",
            ReadmeFailure::Version => "release readme: version does not match the workspace",
            ReadmeFailure::Keys => "release readme: release keys are invalid",
            ReadmeFailure::Utf8 => "release readme: source is not valid UTF-8",
            ReadmeFailure::LineEnds => {
                "release readme: source must use LF line ends and end with one LF"
            }
            ReadmeFailure::VerifyFold => "release readme: source does not end with the verify fold",
            ReadmeFailure::Insertion => "release readme: source has no single insertion position",
            ReadmeFailure::Generated => "release readme: source already holds a generated part",
            ReadmeFailure::Sidecar => "release readme: readme.sha256 is malformed",
            ReadmeFailure::Digest => "release readme: source digest does not match readme.sha256",
            ReadmeFailure::Assets => "release readme: assets/readme does not match the source",
            ReadmeFailure::ImageRef => "release readme: a local image reference is invalid",
            ReadmeFailure::SizeLimit => "release readme: size limit exceeded",
            ReadmeFailure::Mismatch => {
                "release readme: README.md does not match the assembled text"
            }
            ReadmeFailure::FileOperation => "release readme: file operation failed",
        }
    }

    /// The command's exit code: 2 for invalid arguments, else 1.
    pub fn exit_code(self) -> u8 {
        if self == ReadmeFailure::Arguments {
            2
        } else {
            1
        }
    }
}

type Result<T> = std::result::Result<T, ReadmeFailure>;

/// Whether `version` is three dot-separated numbers of 1 to 9 ASCII digits each.
pub fn valid_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| (1..=9).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Rebuilds (`Mode::Write`) or checks (`Mode::Check`) the README of the repository at `root` for
/// `version`, under the release bounds.
pub fn run(root: &Path, version: &str, mode: Mode) -> Result<()> {
    run_with(root, version, mode, &LIMITS)
}

/// [`run`] under the bounds `limits`.
pub fn run_with(root: &Path, version: &str, mode: Mode, limits: &Limits) -> Result<()> {
    if !valid_version(version) {
        return Err(ReadmeFailure::Arguments);
    }
    if workspace_version(root)? != version {
        return Err(ReadmeFailure::Version);
    }
    let keys = release_keys(root)?;
    let parts = generated_parts(&keys, version)?;
    let source = read_bounded(&root.join(SOURCE_FILE), limits.text).map_err(|e| match e {
        ReadError::TooLarge => ReadmeFailure::SizeLimit,
        ReadError::Io => ReadmeFailure::FileOperation,
    })?;
    let sidecar = read_sidecar(root)?;
    if sha256_hex(&source) != sidecar {
        return Err(ReadmeFailure::Digest);
    }
    let text = insert(&source, &parts, limits.text)?;
    check_assets(root, &text, limits)?;
    match mode {
        Mode::Write => write_readme(root, &text),
        Mode::Check => compare_readme(root, &text, limits.text),
    }
}

/// The README for `version`: the source export `source` with the generated parts under `keys`
/// inserted before the final verify fold's close. The one pure assembly of both modes.
pub fn assemble(source: &[u8], keys: &ReleaseKeys, version: &str) -> Result<String> {
    let parts = generated_parts(keys, version)?;
    insert(source, &parts, LIMITS.text)
}

/// The four generated parts for `version`: the Archives part, then the selected release key,
/// the trust list and the install blocks, all from `keys` (`readme_ready_section`).
pub fn generated_parts(keys: &ReleaseKeys, version: &str) -> Result<String> {
    let ready = release_install::readme_ready_section(
        keys,
        version,
        &release_install::readme_notes_url(version),
    )
    .map_err(|_| ReadmeFailure::Keys)?;
    Ok(format!(
        "{}\n{ready}",
        release_install::readme_archives_section(version)
    ))
}

/// `source` with `parts` and one LF inserted at its single insertion position, within `max`
/// bytes.
fn insert(source: &[u8], parts: &str, max: u64) -> Result<String> {
    if source.len() as u64 > max {
        return Err(ReadmeFailure::SizeLimit);
    }
    let text = std::str::from_utf8(source).map_err(|_| ReadmeFailure::Utf8)?;
    if text.contains('\r') || !text.ends_with('\n') || text.ends_with("\n\n") {
        return Err(ReadmeFailure::LineEnds);
    }
    let at = insertion_point(text)?;
    if (text.len() + parts.len() + 1) as u64 > max {
        return Err(ReadmeFailure::SizeLimit);
    }
    let mut out = String::with_capacity(text.len() + parts.len() + 1);
    out.push_str(&text[..at]);
    out.push_str(parts);
    out.push('\n');
    out.push_str(&text[at..]);
    Ok(out)
}

// ---------------------------------------------------------------- the structural reader

/// An open code fence: its character and its length.
struct Fence {
    ch: u8,
    len: usize,
}

/// The fence `line` opens: three or more backticks or tildes after any spaces (a backtick
/// fence's info string holds no backtick).
fn fence_open(line: &str) -> Option<Fence> {
    let t = line.trim_start_matches(' ');
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = t.bytes().take_while(|&b| b == ch).count();
    if len < 3 || (ch == b'`' && t[len..].contains('`')) {
        return None;
    }
    Some(Fence { ch, len })
}

/// Whether `line` closes `fence`: at least as many of its character, then only blanks.
fn fence_closes(line: &str, fence: &Fence) -> bool {
    let t = line.trim_start_matches(' ');
    let n = t.bytes().take_while(|&b| b == fence.ch).count();
    n >= fence.len && t[n..].trim().is_empty()
}

/// One top-level part of the source.
enum Top<'a> {
    /// A fold: its title, the line that closes it, and whether a generated heading sits
    /// directly in it.
    Fold {
        title: &'a str,
        close: Option<usize>,
        generated: bool,
    },
    /// A heading, a paragraph, a list, a table or fenced code.
    Other,
}

/// The byte offset of the line that closes the final verify fold.
///
/// The reader ignores fenced code. A fold is a `<details>` line, then a
/// `<summary><b>…</b></summary>` line, and it ends at its `</details>` line. The last two
/// top-level parts are the About Release 1 fold and the verify fold, which ends the source with
/// a blank line before its close. The source holds one verify fold, and no generated heading
/// sits directly in it.
fn insertion_point(text: &str) -> Result<usize> {
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in text.split_terminator('\n') {
        lines.push((offset, line));
        offset += line.len() + 1;
    }
    let mut fence: Option<Fence> = None;
    let mut depth = 0usize;
    let mut tops: Vec<Top> = Vec::new();
    let mut verify_folds = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].1;
        i += 1;
        if let Some(f) = &fence {
            if fence_closes(line, f) {
                fence = None;
            }
            continue;
        }
        if let Some(f) = fence_open(line) {
            fence = Some(f);
            if depth == 0 {
                tops.push(Top::Other);
            }
            continue;
        }
        if line == "<details>" {
            let title = lines
                .get(i)
                .and_then(|(_, s)| {
                    s.strip_prefix("<summary><b>")?
                        .strip_suffix("</b></summary>")
                })
                .ok_or(ReadmeFailure::VerifyFold)?;
            i += 1;
            if title == VERIFY_TITLE {
                verify_folds += 1;
            }
            if depth == 0 {
                tops.push(Top::Fold {
                    title,
                    close: None,
                    generated: false,
                });
            }
            depth += 1;
            continue;
        }
        if line == "</details>" {
            if depth == 0 {
                return Err(ReadmeFailure::Insertion);
            }
            depth -= 1;
            if depth == 0
                && let Some(Top::Fold { close, .. }) = tops.last_mut()
            {
                *close = Some(i - 1);
            }
            continue;
        }
        if depth == 0 {
            if !line.trim().is_empty() {
                tops.push(Top::Other);
            }
        } else if depth == 1
            && GENERATED_HEADINGS.contains(&line)
            && let Some(Top::Fold { generated, .. }) = tops.last_mut()
        {
            *generated = true;
        }
    }
    if depth > 0 {
        return Err(ReadmeFailure::Insertion);
    }
    let n = tops.len();
    let (about, verify) = match n {
        0 | 1 => return Err(ReadmeFailure::VerifyFold),
        _ => (&tops[n - 2], &tops[n - 1]),
    };
    let (close, generated) = match (about, verify) {
        (
            Top::Fold {
                title: ABOUT_TITLE,
                close: Some(_),
                ..
            },
            Top::Fold {
                title: VERIFY_TITLE,
                close: Some(close),
                generated,
            },
        ) if *close == lines.len() - 1 => (*close, *generated),
        _ => return Err(ReadmeFailure::VerifyFold),
    };
    if verify_folds != 1 {
        return Err(ReadmeFailure::Insertion);
    }
    if !lines[close - 1].1.is_empty() {
        return Err(ReadmeFailure::VerifyFold);
    }
    if generated {
        return Err(ReadmeFailure::Generated);
    }
    Ok(lines[close].0)
}

// ---------------------------------------------------------------- local images

/// `line` with each inline code span replaced by one space.
fn without_code_spans(line: &str) -> String {
    let b = line.as_bytes();
    let mut out = String::new();
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'`' {
            i += 1;
            continue;
        }
        let n = b[i..].iter().take_while(|&&c| c == b'`').count();
        let mut j = i + n;
        let mut end = None;
        while j < b.len() {
            if b[j] == b'`' {
                let m = b[j..].iter().take_while(|&&c| c == b'`').count();
                if m == n {
                    end = Some(j + m);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match end {
            Some(e) => {
                out.push_str(&line[last..i]);
                out.push(' ');
                last = e;
                i = e;
            }
            None => i += n,
        }
    }
    out.push_str(&line[last..]);
    out
}

/// The values of the HTML attribute `attr` (as `name=`) in `line`, double or single quoted and
/// after a blank.
fn attribute_values<'a>(line: &'a str, attr: &str, out: &mut Vec<&'a str>) {
    let mut from = 0;
    while let Some(k) = line[from..].find(attr) {
        let at = from + k;
        from = at + attr.len();
        if at == 0 || !line.as_bytes()[at - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = &line[from..];
        let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let value = &rest[1..];
        let end = value.find(q).unwrap_or(value.len());
        out.push(&value[..end]);
        from += 1 + end;
    }
}

/// Every image target in `text` outside fenced code and inline code: Markdown images, `src`
/// values and each `srcset` candidate (an empty `srcset` is one empty target).
fn image_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fence: Option<Fence> = None;
    for line in text.split_terminator('\n') {
        if let Some(f) = &fence {
            if fence_closes(line, f) {
                fence = None;
            }
            continue;
        }
        if let Some(f) = fence_open(line) {
            fence = Some(f);
            continue;
        }
        let line = without_code_spans(line);
        // Markdown images: `![alt](target)` or `![alt](<target>)`
        let mut rest = line.as_str();
        while let Some(i) = rest.find("![") {
            rest = &rest[i + 2..];
            let Some(j) = rest.find("](") else { break };
            let after = &rest[j + 2..];
            let target = match after.strip_prefix('<') {
                Some(a) => a.split('>').next().unwrap_or(""),
                None => after
                    .split(|c: char| c == ')' || c.is_ascii_whitespace())
                    .next()
                    .unwrap_or(""),
            };
            out.push(target.to_string());
            rest = after;
        }
        let mut values = Vec::new();
        attribute_values(&line, "src=", &mut values);
        out.extend(values.drain(..).map(str::to_string));
        attribute_values(&line, "srcset=", &mut values);
        for value in values {
            for candidate in value.split(',') {
                out.push(
                    candidate
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
    }
    out
}

/// The file in the assets folder that the local target `target` names: `assets/readme/<name>`
/// with one plain name.
fn asset_of(target: &str) -> Result<&str> {
    let name = target
        .strip_prefix(ASSET_PREFIX)
        .ok_or(ReadmeFailure::ImageRef)?;
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
        return Err(ReadmeFailure::ImageRef);
    }
    Ok(name)
}

/// The SHA-256 and extension of an exported image's file name: 64 lowercase hex digits, a dot
/// and a known extension.
fn parse_asset_name(name: &str) -> Option<&str> {
    let (hash, ext) = name.split_once('.')?;
    (hash.len() == 64
        && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && EXTENSIONS.contains(&ext))
    .then_some(hash)
}

/// Checks the assets folder against the local image references of `text`.
fn check_assets(root: &Path, text: &str, limits: &Limits) -> Result<()> {
    let dir = root.join(ASSETS_DIR);
    let mut files: Vec<(String, String, u64)> = Vec::new();
    let missing = match fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(_) => return Err(ReadmeFailure::FileOperation),
        Ok(m) if !m.is_dir() => return Err(ReadmeFailure::Assets),
        Ok(_) => false,
    };
    if !missing {
        let mut hashes = BTreeSet::new();
        let mut total = 0u64;
        let mut count = 0usize;
        for entry in fs::read_dir(&dir).map_err(|_| ReadmeFailure::FileOperation)? {
            let entry = entry.map_err(|_| ReadmeFailure::FileOperation)?;
            count += 1;
            if count > limits.entries {
                return Err(ReadmeFailure::SizeLimit);
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ReadmeFailure::Assets)?;
            let meta =
                fs::symlink_metadata(entry.path()).map_err(|_| ReadmeFailure::FileOperation)?;
            if !meta.is_file() {
                return Err(ReadmeFailure::Assets);
            }
            let hash = parse_asset_name(&name)
                .ok_or(ReadmeFailure::Assets)?
                .to_string();
            let size = meta.len();
            if size > limits.image {
                return Err(ReadmeFailure::SizeLimit);
            }
            if hashes.insert(hash.clone()) {
                total += size;
                if total > limits.images {
                    return Err(ReadmeFailure::SizeLimit);
                }
            }
            files.push((name, hash, size));
        }
        for (name, hash, size) in &files {
            if stream_sha256(&dir.join(name), *size)? != *hash {
                return Err(ReadmeFailure::Assets);
            }
        }
    }
    let mut used = BTreeSet::new();
    for target in image_targets(text) {
        if target.starts_with("http://") || target.starts_with("https://") {
            continue;
        }
        used.insert(asset_of(&target)?.to_string());
    }
    if missing && !used.is_empty() {
        return Err(ReadmeFailure::Assets);
    }
    let names: BTreeSet<&str> = files.iter().map(|(n, _, _)| n.as_str()).collect();
    if used.iter().any(|n| !names.contains(n.as_str())) {
        return Err(ReadmeFailure::ImageRef);
    }
    if names.iter().any(|n| !used.contains(*n)) {
        return Err(ReadmeFailure::Assets);
    }
    Ok(())
}

/// The SHA-256 of the file `path` of `size` bytes, read in bounded chunks.
fn stream_sha256(path: &Path, size: u64) -> Result<String> {
    let file = File::open(path).map_err(|_| ReadmeFailure::FileOperation)?;
    let mut reader = file.take(size + 1);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65_536];
    let mut read = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|_| ReadmeFailure::FileOperation)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        hasher.update(&buf[..n]);
    }
    if read != size {
        return Err(ReadmeFailure::FileOperation);
    }
    Ok(hex(&hasher.finalize()))
}

// ---------------------------------------------------------------- files

/// Why a bounded read failed.
enum ReadError {
    /// The file is over its bound.
    TooLarge,
    /// Any other failure, or no regular file.
    Io,
}

/// The bytes of the regular file `path`, at most `max` of them; the size is checked before
/// allocation.
fn read_bounded(path: &Path, max: u64) -> std::result::Result<Vec<u8>, ReadError> {
    let file = File::open(path).map_err(|_| ReadError::Io)?;
    let meta = file.metadata().map_err(|_| ReadError::Io)?;
    if !meta.is_file() {
        return Err(ReadError::Io);
    }
    if meta.len() > max {
        return Err(ReadError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReadError::Io)?;
    if bytes.len() as u64 > max {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}

/// The version of the workspace manifest at `root`: `workspace.package.version`.
fn workspace_version(root: &Path) -> Result<String> {
    let bytes = read_bounded(&root.join(WORKSPACE_FILE), MANIFEST_MAX)
        .map_err(|_| ReadmeFailure::FileOperation)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ReadmeFailure::Version)?;
    let table: toml::Table = toml::from_str(text).map_err(|_| ReadmeFailure::Version)?;
    table
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or(ReadmeFailure::Version)
}

/// The authenticated release-key records at `root`; a missing folder refuses.
fn release_keys(root: &Path) -> Result<ReleaseKeys> {
    let dir = root.join(KEYS_DIR);
    match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => ReleaseKeys::load(&dir).map_err(|_| ReadmeFailure::Keys),
        _ => Err(ReadmeFailure::Keys),
    }
}

/// The sidecar's digest: exactly 64 lowercase hex digits and one LF.
fn read_sidecar(root: &Path) -> Result<String> {
    let bytes = read_bounded(&root.join(SIDECAR_FILE), 65).map_err(|e| match e {
        ReadError::TooLarge => ReadmeFailure::Sidecar,
        ReadError::Io => ReadmeFailure::FileOperation,
    })?;
    match bytes.split_last() {
        Some((b'\n', digest))
            if digest.len() == 64
                && digest
                    .iter()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
        {
            Ok(String::from_utf8_lossy(digest).into_owned())
        }
        _ => Err(ReadmeFailure::Sidecar),
    }
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Replaces `README.md` with `text`: a new file beside it, synced, then renamed over it. A
/// failure removes the new file and leaves the old README.
fn write_readme(root: &Path, text: &str) -> Result<()> {
    let temp = root.join(TEMP_FILE);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| ReadmeFailure::FileOperation)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    match written.and_then(|()| fs::rename(&temp, root.join(README_FILE))) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = fs::remove_file(&temp);
            Err(ReadmeFailure::FileOperation)
        }
    }
}

/// Compares `README.md` with `text`; a missing README, or one over the bound, differs.
fn compare_readme(root: &Path, text: &str, max: u64) -> Result<()> {
    match read_bounded(&root.join(README_FILE), max) {
        Ok(bytes) if bytes == text.as_bytes() => Ok(()),
        Ok(_) | Err(ReadError::TooLarge) => Err(ReadmeFailure::Mismatch),
        Err(ReadError::Io) => match fs::symlink_metadata(root.join(README_FILE)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(ReadmeFailure::Mismatch),
            _ => Err(ReadmeFailure::FileOperation),
        },
    }
}
