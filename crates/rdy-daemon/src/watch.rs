//! Whether the pack-defining files (`.rdy/config.yaml` and `.rdy/packs/*.yaml`) changed since
//! they were last seen, cheaply: a `stat` of each file and a listing of `.rdy/packs/`, and their
//! bytes only when a stat or the set of files differs, or a file is racily clean.
//!
//! Racily clean (as git calls it): a filesystem stamps mtimes at some granularity, so a second
//! write of the same length inside one tick leaves `(len, mtime)` as it was. A stat is trusted
//! only when its mtime is more than [`RACY`] before the moment its bytes were read; until a later
//! check reads the file again and sees its mtime that far in the past, every check reads it.
use rdy_engine::{PackFingerprint, pack_fingerprint};
use rdy_model::Hash;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

/// The widest mtime granularity assumed: 2 s covers FAT and other coarse filesystems, and the
/// lag of a coarse kernel clock behind the wall clock. Reading small pack files is cheap, so
/// this errs long.
const RACY: Duration = Duration::from_secs(2);

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

/// The fingerprint's files with their stats as it took them (each just before its bytes were
/// read), kept only where the mtime is more than [`RACY`] before `fp.at`, the clock read before
/// the first stat. A later write gets an mtime past `fp.at - RACY`, so it never matches a kept
/// stat; a racily clean file keeps `None` and is read on the next check.
fn table_of(fp: &PackFingerprint) -> Table {
    let files = fp
        .files
        .iter()
        .map(|(path, len, mtime)| {
            let safe = fp.at.duration_since(*mtime).is_ok_and(|age| age > RACY);
            (path.clone(), safe.then_some((*len, *mtime)))
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
            table: Mutex::new(table_of(loaded)),
            hashed: AtomicUsize::new(0),
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The pack-defining files as they stand, in fingerprint order: the config, then each
    /// `*.yaml` (and `*.yml`) of `.rdy/packs/` sorted by path.
    fn listing(&self) -> Vec<(PathBuf, Stat)> {
        let mut paths = vec![self.root.join(".rdy/config.yaml")];
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
    /// file's `(len, mtime)` is trusted (not racily clean) and matches, and the set of files
    /// matches; otherwise they are hashed, and equal hashes (a `touch`, or a racily clean file
    /// unchanged) refresh the stats, so the same files are not read again once their mtimes are
    /// safely past. A failed read counts as changed, so the writer's reload reports it.
    pub fn changed(&self) -> bool {
        let mut table = self.table();
        let now = self.listing();
        if table.files.iter().all(|(_, s)| s.is_some()) && now == table.files {
            return false;
        }
        self.hashed.fetch_add(1, Ordering::Relaxed);
        match pack_fingerprint(&self.root) {
            Ok(fp) if fp.hash == table.hash => {
                // the fingerprint's own stats, each taken before its bytes were read
                *table = table_of(&fp);
                false
            }
            Ok(_) | Err(_) => true,
        }
    }

    /// Records `fp` as seen: `changed` compares with it from now on. Its stats are the ones it
    /// took, never newer ones, so bytes written after it was read are never marked seen.
    pub fn seen(&self, fp: &PackFingerprint) {
        *self.table() = table_of(fp);
    }

    /// Test hook: how many times `changed` read the files' bytes.
    #[doc(hidden)]
    pub fn stats(&self) -> usize {
        self.hashed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const A: &str = "name: p\nversion: '1'\nextends: aaaa\n";
    const B: &str = "name: p\nversion: '1'\nextends: bbbb\n";

    /// A project whose files are stamped a minute ahead: racily clean for every check below,
    /// however slow the machine (a file written just now is too, but only for [`RACY`]).
    fn project() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".rdy/packs")).unwrap();
        std::fs::write(d.path().join(".rdy/config.yaml"), "pack: p\n").unwrap();
        let pack = d.path().join(".rdy/packs/p.yaml");
        std::fs::write(&pack, A).unwrap();
        let ahead = SystemTime::now() + Duration::from_secs(60);
        set_mtime(&d.path().join(".rdy/config.yaml"), ahead);
        set_mtime(&pack, ahead);
        (d, pack)
    }

    fn mtime(p: &Path) -> SystemTime {
        std::fs::metadata(p).unwrap().modified().unwrap()
    }

    fn set_mtime(p: &Path, t: SystemTime) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    /// Moves every pack-defining file's mtime `ago` into the past.
    fn backdate(root: &Path, ago: Duration) {
        let t = SystemTime::now() - ago;
        set_mtime(&root.join(".rdy/config.yaml"), t);
        set_mtime(&root.join(".rdy/packs/p.yaml"), t);
    }

    #[test]
    fn racy1_a_same_length_rewrite_in_the_same_mtime_tick_is_seen() {
        let (d, pack) = project();
        let w = PackWatch::new(d.path(), &pack_fingerprint(d.path()).unwrap());
        assert!(!w.changed());
        // a coarse filesystem gives the second write the first one's mtime
        let m = mtime(&pack);
        assert_eq!(A.len(), B.len());
        std::fs::write(&pack, B).unwrap();
        set_mtime(&pack, m);
        assert!(w.changed(), "same length, same mtime, other bytes");
    }

    #[test]
    fn racy1_a_rewrite_after_seen_in_the_same_mtime_tick_is_seen() {
        let (d, pack) = project();
        let fp = pack_fingerprint(d.path()).unwrap();
        let w = PackWatch::new(d.path(), &fp);
        w.seen(&fp);
        let m = mtime(&pack);
        std::fs::write(&pack, B).unwrap();
        set_mtime(&pack, m);
        assert!(w.changed());
    }

    #[test]
    fn racy1_files_safely_in_the_past_are_trusted_without_reading() {
        let (d, pack) = project();
        backdate(d.path(), Duration::from_secs(60));
        let w = PackWatch::new(d.path(), &pack_fingerprint(d.path()).unwrap());
        for _ in 0..5 {
            assert!(!w.changed());
        }
        assert_eq!(w.stats(), 0, "old stats that match are trusted");
        // a change of stat is still read, and found
        std::fs::write(&pack, B).unwrap();
        assert!(w.changed());
    }

    #[test]
    fn racy1_a_fresh_file_is_trusted_once_a_later_check_sees_it_safely_past() {
        let (d, _) = project();
        let w = PackWatch::new(d.path(), &pack_fingerprint(d.path()).unwrap());
        assert!(!w.changed());
        assert!(!w.changed());
        assert_eq!(w.stats(), 2, "racily clean: read on every check");
        // the same stats, now safely in the past: one more read, then none
        backdate(d.path(), Duration::from_secs(60));
        assert!(!w.changed());
        assert!(!w.changed());
        assert!(!w.changed());
        assert_eq!(w.stats(), 3);
    }
}
