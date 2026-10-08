//! Loading and reloading the project's pack, and the fingerprint of its files.

use crate::config::{parse_config, read_config};
use crate::config_levels;
use crate::handle::io;
use crate::{Delta, E_CONFIG, E_NO_PROJECT, Rdy, RdyError};
use rdy_schema::{Pack, PackSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What one `stat` of a file says, for telling whether its bytes may have changed: length,
/// modification time, inode number and status-change time (`ctime`), all at full precision.
/// `ctime` is set by the kernel on every write, rename into place and `utimes`, and no tool can
/// set it back or ahead, so it is what "changed since" is judged by. On a non-unix target there
/// is no `ino` or `ctime`: `ino` is 0 and `ctime` is the mtime (a tool can set it: the weaker
/// rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub mtime: std::time::SystemTime,
    pub ino: u64,
    pub ctime: std::time::SystemTime,
}

impl FileStamp {
    pub fn of(meta: &std::fs::Metadata) -> FileStamp {
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        #[cfg(unix)]
        let (ino, ctime) = {
            use std::os::unix::fs::MetadataExt;
            let ctime = u64::try_from(meta.ctime()).map_or(std::time::UNIX_EPOCH, |s| {
                std::time::UNIX_EPOCH + std::time::Duration::new(s, meta.ctime_nsec() as u32)
            });
            (meta.ino(), ctime)
        };
        #[cfg(not(unix))]
        let (ino, ctime) = (0, mtime);
        FileStamp {
            len: meta.len(),
            mtime,
            ino,
            ctime,
        }
    }
}

/// What the pack-defining files looked like at one moment: `.rdy/config.yaml`, then
/// `.rdy/local.yaml` when it exists, and every `.rdy/packs/*.yaml` (and `.yml`), sorted by path, each with the [`FileStamp`] taken just before
/// its bytes were read; `hash` is BLAKE3 over each file's path bytes, a NUL, its contents and a
/// NUL, in that order. Equal hashes mean equal bytes whatever the stamps say. `at` is this
/// machine's wall clock read before the first stat: a file whose `ctime` is not safely before
/// `at` can change again without a new stamp (it is "racily clean").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFingerprint {
    pub files: Vec<(PathBuf, FileStamp)>,
    pub hash: rdy_model::Hash,
    pub at: std::time::SystemTime,
}

pub fn pack_fingerprint(project: &Path) -> Result<PackFingerprint, RdyError> {
    read_sources(project).map(|s| s.fingerprint)
}

static SOURCE_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test hook: how many times this process has read the pack-defining files (open, reload and
/// the daemon's watch each read them through one function), whatever the project.
#[doc(hidden)]
pub fn pack_source_reads() -> usize {
    SOURCE_READS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The pack-defining files read once: their fingerprint and the very bytes it hashed, so what is
/// loaded is exactly what is fingerprinted (an edit landing between two reads cannot split them).
pub(crate) struct Sources {
    pub(crate) fingerprint: PackFingerprint,
    pub(crate) config: Vec<u8>,
    /// `.rdy/local.yaml`'s bytes when it exists.
    pub(crate) local: Option<Vec<u8>>,
    /// Each `.rdy/packs` file and its bytes, in fingerprint order.
    pub(crate) packs: Vec<(PathBuf, Vec<u8>)>,
}

/// Reads `.rdy/config.yaml`, `.rdy/local.yaml` when it exists, and the pack files, each stat'ed just before its bytes are read.
/// No config file is `E_NO_PROJECT`; any other failure is an I/O error.
pub(crate) fn read_sources(project: &Path) -> Result<Sources, RdyError> {
    SOURCE_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let at = std::time::SystemTime::now();
    let cfg_path = project.join(config_levels::PROJECT_FILE);
    if !cfg_path.exists() {
        return Err(no_project(project));
    }
    let mut paths = vec![cfg_path];
    let local = project.join(config_levels::LOCAL_FILE);
    let has_local = local.exists();
    if has_local {
        paths.push(local);
    }
    paths.extend(pack_files(&project.join(".rdy/packs"))?);
    let mut files = Vec::with_capacity(paths.len());
    let mut contents = Vec::with_capacity(paths.len());
    let mut buf = Vec::new();
    for path in paths {
        let meta = std::fs::metadata(&path).map_err(io)?;
        let bytes = std::fs::read(&path).map_err(io)?;
        buf.extend_from_slice(path.as_os_str().as_encoded_bytes());
        buf.push(0);
        buf.extend_from_slice(&bytes);
        buf.push(0);
        files.push((path.clone(), FileStamp::of(&meta)));
        contents.push((path, bytes));
    }
    let mut contents = contents.into_iter();
    let (_, config) = contents.next().expect("the config is always first");
    let local = if has_local {
        contents.next().map(|(_, b)| b)
    } else {
        None
    };
    Ok(Sources {
        fingerprint: PackFingerprint {
            files,
            hash: rdy_model::Hash::of(&buf),
            at,
        },
        config,
        local,
        packs: contents.collect(),
    })
}

fn no_project(root: &Path) -> RdyError {
    RdyError::new(
        E_NO_PROJECT,
        format!("{} is not an rdy project", root.display()),
        "run rdy init in the project directory",
    )
}

/// What `Rdy::reload_pack` did.
#[derive(Debug)]
pub enum ReloadOutcome {
    /// The pack files and the config are byte-for-byte what is loaded.
    Unchanged,
    /// The config and pack were re-read and the state republished with the new pack.
    Reloaded,
    /// The files changed but do not resolve; the loaded pack stays. `fingerprint` is the files
    /// as this reload read them, taken before their bytes were evaluated: recording it as seen
    /// never marks newer, unevaluated bytes as seen.
    Invalid {
        /// Boxed: a `RdyError` with its optional payloads outweighs the other variants.
        error: Box<RdyError>,
        fingerprint: PackFingerprint,
    },
}

/// The named pack as the project at `project` resolves it (its .rdy/packs plus the built-ins).
pub fn resolve_pack(project: &Path, name: &str) -> Result<Pack, RdyError> {
    Ok(load_pack(project, name)?.0)
}

/// The pack the project at `project` names in `.rdy/config.yaml`, resolved without opening its
/// store (so it works while another process holds the store). A missing or unreadable config is
/// the same error `Rdy::open` gives.
pub fn project_pack(project: &Path) -> Result<Pack, RdyError> {
    let cfg = read_config(project)?;
    resolve_pack(project, &cfg.pack)
}

/// The `.yaml` and `.yml` files of a packs directory, sorted; none if it does not exist.
fn pack_files(dir: &Path) -> Result<Vec<PathBuf>, RdyError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(io)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    files.sort();
    files.retain(|f| f.extension().is_some_and(|x| x == "yaml" || x == "yml"));
    Ok(files)
}

pub(crate) fn load_pack(root: &Path, name: &str) -> Result<(Pack, Vec<String>), RdyError> {
    let mut packs = vec![];
    for f in pack_files(&root.join(".rdy/packs"))? {
        let bytes = std::fs::read(&f).map_err(io)?;
        packs.push((f, bytes));
    }
    load_pack_from(&packs, name)
}

/// The named pack resolved from `packs` (each `.rdy/packs` file and its bytes) plus the
/// built-ins, and its lineage.
pub(crate) fn load_pack_from(
    packs: &[(PathBuf, Vec<u8>)],
    name: &str,
) -> Result<(Pack, Vec<String>), RdyError> {
    let mut set = PackSet::with_builtin();
    for (f, bytes) in packs {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        set.add_yaml_from(text, &f.display().to_string())
            .map_err(|e| match e {
                rdy_schema::SchemaError::Duplicate { .. } => RdyError::new(
                    E_CONFIG,
                    e.to_string(),
                    "give each pack its own name; to change a pack, write a differently named pack that extends it",
                ),
                e => RdyError::new(E_CONFIG, format!("{}: {e}", f.display()), "fix the pack file"),
            })?;
    }
    // pkfix1: only an unknown pack name is fixed in the config; a pack that exists but does not
    // resolve (an invalid area, a cycle) is fixed in its file.
    let config = |e: rdy_schema::SchemaError| match e {
        rdy_schema::SchemaError::UnknownPack(_) => RdyError::new(
            E_CONFIG,
            e.to_string(),
            "set `pack:` in .rdy/config.yaml to an available pack (rdy-standard is built in)",
        ),
        e => RdyError::new(E_CONFIG, e.to_string(), "fix the pack file"),
    };
    let pack = set.resolve(name).map_err(config)?;
    let lineage = set.lineage(name).map_err(config)?;
    Ok((pack, lineage))
}

impl Rdy {
    /// Re-reads the config and packs if their bytes changed since they were loaded. A config or
    /// pack that does not resolve is `Invalid`: the loaded pack, lineage and fingerprint stay, so
    /// a later fix counts as a change and a still-invalid state is reported again. On success the
    /// state is republished with the new pack. `Err` is only an I/O error reading the
    /// fingerprint (or a failed republish).
    pub fn reload_pack(&mut self) -> Result<ReloadOutcome, RdyError> {
        let sources = read_sources(&self.root)?;
        let fp = sources.fingerprint;
        if fp.hash == self.fingerprint.hash {
            return Ok(ReloadOutcome::Unchanged);
        }
        // parsed from the bytes `fp` hashed: what is loaded (or found invalid) is what is seen
        let loaded = parse_config(&self.root, &sources.config, sources.local.as_deref())
            .and_then(|cfg| load_pack_from(&sources.packs, &cfg.pack).map(|(p, l)| (cfg, p, l)));
        let (cfg, pack, lineage) = match loaded {
            Ok(v) => v,
            Err(error) => {
                return Ok(ReloadOutcome::Invalid {
                    error: Box::new(error),
                    fingerprint: fp,
                });
            }
        };
        self.areas = Arc::new(pack.area_matcher());
        self.store.set_tracking(pack.tracking());
        self.pack = Arc::new(pack);
        self.lineage = lineage;
        self.serve = cfg.serve.resolve();
        self.fingerprint = fp;
        // entries built under the old pack must not serve the new one
        self.as_of
            .lru
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self.vpos.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
        self.publish(Delta::Full)?;
        Ok(ReloadOutcome::Reloaded)
    }
}
