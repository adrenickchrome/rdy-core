//! Whether the pack-defining files (`.rdy/config.yaml` and `.rdy/packs/*.yaml`) changed since
//! they were last seen, cheaply: a `stat` of each file and a listing of `.rdy/packs/`, and their
//! bytes only when a stat or the set of files differs.
use rdy_engine::config_levels::{LOCAL_FILE, PROJECT_FILE};
use rdy_engine::{PackFingerprint, pack_fingerprint};
use rdy_model::Hash;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

/// A file's length and modification time, at full precision. `None`: not known to match the
/// bytes last seen, so the next check reads them.
type Stat = Option<(u64, SystemTime)>;

/// What was last seen: the fingerprint's hash and a stat per file, in fingerprint order.
struct Table {
    hash: Hash,
    files: Vec<(PathBuf, Stat)>,
}

/// Watches a project's pack files between writes. Shared between the writer and the connection
/// tasks: `changed` may run on any thread.
pub struct PackWatch {
    root: PathBuf,
    table: Mutex<Table>,
    /// How many times `changed` read the files' bytes.
    hashed: AtomicUsize,
}

fn stat(path: &Path) -> Stat {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// A modification time in unix ms, as `pack_fingerprint` records it.
fn unix_ms(t: SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The fingerprint's files with `stat_of`'s stat for each, kept only where it agrees with what
/// the fingerprint recorded (a file changed in between is read again next time).
fn table_of(fp: &PackFingerprint, mut stat_of: impl FnMut(&Path) -> Stat) -> Table {
    let files = fp
        .files
        .iter()
        .map(|(path, len, ms)| {
            let s = stat_of(path).filter(|(l, t)| l == len && unix_ms(*t) == *ms);
            (path.clone(), s)
        })
        .collect();
    Table {
        hash: fp.hash,
        files,
    }
}

impl PackWatch {
    /// A watch that has seen `loaded`, the fingerprint of the pack in use.
    pub fn new(root: &Path, loaded: &PackFingerprint) -> PackWatch {
        PackWatch {
            root: root.to_path_buf(),
            table: Mutex::new(table_of(loaded, stat)),
            hashed: AtomicUsize::new(0),
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The pack-defining files as they stand, in fingerprint order: the config, the local
    /// settings when the file exists, then each
    /// `*.yaml` (and `*.yml`) of `.rdy/packs/` sorted by path.
    fn listing(&self) -> Vec<(PathBuf, Stat)> {
        let mut paths = vec![self.root.join(PROJECT_FILE)];
        let local = self.root.join(LOCAL_FILE);
        if local.exists() {
            paths.push(local);
        }
        if let Ok(dir) = std::fs::read_dir(self.root.join(".rdy/packs")) {
            let mut packs: Vec<PathBuf> = dir
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "yaml" || x == "yml"))
                .collect();
            packs.sort();
            paths.extend(packs);
        }
        paths
            .into_iter()
            .map(|p| {
                let s = stat(&p);
                (p, s)
            })
            .collect()
    }

    /// Whether the files' bytes differ from those last seen. No bytes are read while every
    /// file's `(len, mtime)` and the set of files match; otherwise they are hashed, and equal
    /// hashes (a `touch`) refresh the stats, so the same files are not read again. A failed
    /// read counts as changed, so the writer's reload reports it.
    pub fn changed(&self) -> bool {
        let mut table = self.table();
        let now = self.listing();
        if table.files.iter().all(|(_, s)| s.is_some()) && now == table.files {
            return false;
        }
        self.hashed.fetch_add(1, Ordering::Relaxed);
        match pack_fingerprint(&self.root) {
            Ok(fp) if fp.hash == table.hash => {
                // stats taken before the bytes were read: a later change still shows
                let stat_of = |p: &Path| now.iter().find(|(q, _)| q == p).and_then(|(_, s)| *s);
                *table = table_of(&fp, stat_of);
                false
            }
            Ok(_) | Err(_) => true,
        }
    }

    /// Records `fp` as seen: `changed` compares with it from now on.
    pub fn seen(&self, fp: &PackFingerprint) {
        *self.table() = table_of(fp, stat);
    }

    /// Test hook: how many times `changed` read the files' bytes.
    #[doc(hidden)]
    pub fn stats(&self) -> usize {
        self.hashed.load(Ordering::Relaxed)
    }
}
