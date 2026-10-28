//! Loading and reloading the project's pack, and the fingerprint of its files.

use crate::config::{Config, parse_config, read_config};
use crate::config_levels;
use crate::handle::io;
use crate::{Delta, E_CONFIG, Rdy, RdyError, ServeConfig};
use rdy_schema::{AreaMatcher, Issue, Pack, PackSet, Severity, TensionKind, TensionSeverity};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What one `stat` of a file says, for telling whether its bytes may have changed: length,
/// modification time, inode number and status-change time (`ctime`), all at full precision.
/// `ctime` is set by the kernel on every write, rename into place and `utimes`, and no tool can
/// set it back or ahead, so it is what "changed since" is judged by.
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
        let rdy_store::fileid::ChangeStamp { ino, ctime } = rdy_store::fileid::change_stamp(meta);
        FileStamp {
            len: meta.len(),
            mtime,
            ino,
            ctime,
        }
    }
}

/// What the pack-defining files looked like at one moment: `.rdy.yaml`, then
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

thread_local! {
    static THREAD_SOURCE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test hook: how many times this process has read the pack-defining files (open, reload and
/// the daemon's watch each read them through one function), whatever the project.
#[doc(hidden)]
pub fn pack_source_reads() -> usize {
    SOURCE_READS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Test hook: [`pack_source_reads`] counted on the calling thread only, so a test that calls the
/// engine on its own thread sees no read another test makes at the same time.
#[doc(hidden)]
pub fn pack_source_reads_on_this_thread() -> usize {
    THREAD_SOURCE_READS.with(std::cell::Cell::get)
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

/// Reads `.rdy.yaml`, `.rdy/local.yaml` when it exists, and the pack files, each stat'ed just before its bytes are read.
/// No config file is `E_NO_PROJECT`; any other failure is an I/O error.
pub(crate) fn read_sources(project: &Path) -> Result<Sources, RdyError> {
    SOURCE_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    THREAD_SOURCE_READS.with(|n| n.set(n.get().wrapping_add(1)));
    let at = std::time::SystemTime::now();
    let cfg_path = project.join(config_levels::PROJECT_FILE);
    if !cfg_path.exists() {
        return Err(config_levels::missing_project(project));
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

/// The pack the project at `project` names in `.rdy.yaml`, resolved without opening its
/// store (so it works while another process holds the store). A missing or unreadable config is
/// the same error `Rdy::open` gives.
pub fn project_pack(project: &Path) -> Result<Pack, RdyError> {
    let cfg = read_config(project)?;
    resolve_pack(project, &cfg.pack)
}

/// A pack lowers a tension kind that blocks in stable content (D-ME2HD); rdy keeps it blocking.
pub const W_PACK_SEVERITY: &str = "W_PACK_SEVERITY";

/// The `W_PACK_SEVERITY` warnings of `pack`: one per kind that blocks in stable content
/// ([`TensionKind::blocks_in_stable`]) that the pack sets to advisory.
pub fn pack_severity_warnings(pack: &Pack) -> Vec<Issue> {
    TensionKind::ALL
        .into_iter()
        .filter(|k| {
            k.blocks_in_stable() && pack.tensions.get(k) == Some(&TensionSeverity::Advisory)
        })
        .map(|k| Issue {
            severity: Severity::Warning,
            code: W_PACK_SEVERITY,
            block: None,
            message: format!(
                "pack {} lowers {k} in stable content; rdy keeps it blocking",
                pack.name
            ),
            fix: format!("remove the {k} severity from the pack"),
        })
        .collect()
}

/// The `.yaml` and `.yml` files of a packs directory, sorted; none if it does not exist.
pub(crate) fn pack_files(dir: &Path) -> Result<Vec<PathBuf>, RdyError> {
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
            "set `pack:` in .rdy.yaml to an available pack (rdy-standard is built in)",
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
    /// fingerprint, or a failed republish. A republish that fails or panics restores the
    /// previous pack, config and fingerprint before it returns its error or resumes its panic,
    /// so the previous pack stays loaded and the next reload sees the files as changed.
    ///
    /// A holder has no config or pack files (D-BPYN8): its reload reads nothing and is
    /// `Unchanged`, so its fixed context and fingerprint stay.
    pub fn reload_pack(&mut self) -> Result<ReloadOutcome, RdyError> {
        if self.is_holder() {
            return Ok(ReloadOutcome::Unchanged);
        }
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
        // what is loaded now, restored if the publish of the new pack fails or panics
        let previous = Loaded {
            areas: Arc::clone(&self.areas),
            pack: Arc::clone(&self.pack),
            lineage: self.lineage.clone(),
            serve: self.serve,
            fingerprint: self.fingerprint.clone(),
            config: Arc::clone(&self.config),
        };
        self.swap_loaded(Loaded {
            areas: Arc::new(pack.area_matcher()),
            pack: Arc::new(pack),
            lineage,
            serve: cfg.serve.resolve(),
            fingerprint: fp,
            config: Arc::new(cfg),
        });
        // the reload's moves are logged in a batch of its own (plan M2e2, D-Z0EMP)
        self.recheck = Some(rdy_store::transition::Cause::PackReload);
        let failed = match catch_unwind(AssertUnwindSafe(|| self.publish(Delta::Full))) {
            Ok(Ok(())) => return Ok(ReloadOutcome::Reloaded),
            Ok(Err(e)) => Ok(e),
            Err(p) => Err(p),
        };
        self.swap_loaded(previous);
        match failed {
            Ok(e) => {
                // the failed publish left a state that names the new pack: publish one under
                // the previous pack (it keeps its own error if the read fails again)
                self.invalidate();
                Err(e)
            }
            Err(p) => resume_unwind(p),
        }
    }

    /// Puts `loaded` in place of the loaded pack and config, and drops the cached entries
    /// built under the pack it replaces.
    fn swap_loaded(&mut self, loaded: Loaded) {
        self.store.set_tracking(loaded.pack.tracking());
        self.areas = loaded.areas;
        self.pack = loaded.pack;
        self.lineage = loaded.lineage;
        self.serve = loaded.serve;
        self.fingerprint = loaded.fingerprint;
        self.config = loaded.config;
        // entries built under the old pack must not serve the new one
        self.as_of.clear();
        *self.vpos.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
    }
}

/// The pack and config a project has loaded, with what is derived from them.
struct Loaded {
    areas: Arc<AreaMatcher>,
    pack: Arc<Pack>,
    lineage: Vec<String>,
    serve: ServeConfig,
    fingerprint: PackFingerprint,
    config: Arc<Config>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps3_file_stamp_reads_the_inode_and_ctime_of_the_seam() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.yaml");
        std::fs::write(&f, "x\n").unwrap();
        let m = std::fs::metadata(&f).unwrap();
        let seam = rdy_store::fileid::change_stamp(&m);
        let stamp = FileStamp::of(&m);
        assert_eq!(stamp.ino, seam.ino);
        assert_eq!(stamp.ctime, seam.ctime);
    }
}
