//! The source build id (D-CCC6E). `build.rs` and `tests/build_id.rs` both include this file,
//! so it uses no platform API and no rdy crate.
#![allow(dead_code)]

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const DOMAIN: &[u8] = b"rdy-source-build-v1\0";
const BUFFER_BYTES: usize = 65_536;

/// The closed bounds of one walk.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub files: u64,
    pub entries: u64,
    pub name_bytes: usize,
    pub content_bytes: u64,
}

pub const LIMITS: Limits = Limits {
    files: 100_000,
    entries: 100_000,
    name_bytes: 4_096,
    content_bytes: 1_073_741_824,
};

/// The reason a build id cannot be computed. `Display` gives the exact reason text.
#[derive(Debug, PartialEq, Eq)]
pub struct BuildIdError(pub String);

impl fmt::Display for BuildIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// When the walk calls the test hook for a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    BeforeRead,
    AfterRead,
}

/// The id and the paths Cargo must watch.
#[derive(Debug)]
pub struct BuildInputs {
    pub id: String,
    pub rerun: Vec<PathBuf>,
}

struct Entry {
    rel: String,
    abs: PathBuf,
    len: u64,
}

fn err(reason: impl Into<String>) -> BuildIdError {
    BuildIdError(reason.into())
}

fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<(), BuildIdError> {
    v.try_reserve(n)
        .map_err(|_| err("cannot allocate input metadata"))
}

/// The build id of the sources under `root`.
pub fn source_build_id(root: &Path) -> Result<String, BuildIdError> {
    Ok(source_build_inputs(root, &LIMITS, &mut |_, _| {})?.id)
}

/// The build id with explicit bounds, a hook for tests and the rerun paths.
pub fn source_build_inputs(
    root: &Path,
    limits: &Limits,
    hook: &mut dyn FnMut(&Path, Phase),
) -> Result<BuildInputs, BuildIdError> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut rerun: Vec<PathBuf> = Vec::new();
    let mut seen: u64 = 0;
    let mut files: u64 = 0;
    let mut total: u64 = 0;
    for name in ["Cargo.toml", "Cargo.lock"] {
        let abs = root.join(name);
        admit_file(
            name.to_string(),
            abs,
            limits,
            &mut seen,
            &mut files,
            &mut total,
            &mut entries,
            &mut rerun,
        )?;
    }
    let crates = root.join("crates");
    let mut stack: Vec<(String, PathBuf)> = Vec::new();
    reserve(&mut stack, 1)?;
    stack.push(("crates".to_string(), crates));
    while let Some((rel, dir)) = stack.pop() {
        reserve(&mut rerun, 1)?;
        rerun.push(dir.clone());
        let read = fs::read_dir(&dir).map_err(|_| err(format!("cannot read input: {rel}")))?;
        for item in read {
            let item = item.map_err(|_| err(format!("cannot read input: {rel}")))?;
            let name = item.file_name();
            let Some(name) = name.to_str() else {
                return Err(err("input path is not UTF-8"));
            };
            let child = format!("{rel}/{name}");
            if child.len() > limits.name_bytes {
                return Err(err(format!(
                    "input path exceeds {} bytes",
                    limits.name_bytes
                )));
            }
            let abs = item.path();
            let meta = fs::symlink_metadata(&abs)
                .map_err(|_| err(format!("cannot read input: {child}")))?;
            if meta.is_dir() {
                seen += 1;
                if seen > limits.entries {
                    return Err(err(format!("more than {} input entries", limits.entries)));
                }
                reserve(&mut stack, 1)?;
                stack.push((child, abs));
            } else {
                admit_file(
                    child,
                    abs,
                    limits,
                    &mut seen,
                    &mut files,
                    &mut total,
                    &mut entries,
                    &mut rerun,
                )?;
            }
        }
    }
    entries.sort_by(|a, b| a.rel.as_bytes().cmp(b.rel.as_bytes()));
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOMAIN);
    let mut buf = vec![0u8; BUFFER_BYTES];
    for e in &entries {
        hasher.update(&(e.rel.len() as u64).to_le_bytes());
        hasher.update(e.rel.as_bytes());
        hasher.update(&e.len.to_le_bytes());
        hook(&e.abs, Phase::BeforeRead);
        let changed = || err(format!("input changed while being read: {}", e.rel));
        let mut file =
            fs::File::open(&e.abs).map_err(|_| err(format!("cannot read input: {}", e.rel)))?;
        let mut read_total: u64 = 0;
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|_| err(format!("cannot read input: {}", e.rel)))?;
            if n == 0 {
                break;
            }
            read_total += n as u64;
            if read_total > e.len {
                return Err(changed());
            }
            hasher.update(&buf[..n]);
        }
        hook(&e.abs, Phase::AfterRead);
        let after = fs::symlink_metadata(&e.abs).map_err(|_| changed())?;
        if read_total != e.len || after.len() != e.len || !after.is_file() {
            return Err(changed());
        }
    }
    let digest = hasher.finalize().to_hex();
    Ok(BuildInputs {
        id: digest.as_str()[..16].to_string(),
        rerun,
    })
}

#[allow(clippy::too_many_arguments)]
fn admit_file(
    rel: String,
    abs: PathBuf,
    limits: &Limits,
    seen: &mut u64,
    files: &mut u64,
    total: &mut u64,
    entries: &mut Vec<Entry>,
    rerun: &mut Vec<PathBuf>,
) -> Result<(), BuildIdError> {
    *seen += 1;
    if *seen > limits.entries {
        return Err(err(format!("more than {} input entries", limits.entries)));
    }
    let meta = fs::symlink_metadata(&abs).map_err(|_| err(format!("cannot read input: {rel}")))?;
    if !meta.is_file() {
        return Err(err(format!("input is not a regular file: {rel}")));
    }
    *files += 1;
    if *files > limits.files {
        return Err(err(format!("more than {} input files", limits.files)));
    }
    let len = meta.len();
    *total = total.saturating_add(len);
    if *total > limits.content_bytes {
        return Err(err(format!(
            "input contents exceed {} bytes",
            limits.content_bytes
        )));
    }
    reserve(entries, 1)?;
    reserve(rerun, 1)?;
    rerun.push(abs.clone());
    entries.push(Entry { rel, abs, len });
    Ok(())
}
