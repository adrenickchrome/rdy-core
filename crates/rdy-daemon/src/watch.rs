//! Whether the pack-defining files (`.rdy/config.yaml` and `.rdy/packs/*.yaml`) changed since
//! they were last seen, cheaply: a `stat` of each file and a listing of `.rdy/packs/`, and their
//! bytes only when a stat or the set of files differs, or a file is racily clean.
//!
//! A stat is a [`FileStamp`]: length, mtime, inode and `ctime`. A file renamed into place has a
//! new inode, and every write, rename or `utimes` moves `ctime`, which the kernel sets and no tool
//! can set back or ahead (an mtime can be set to anything, `touch -d`, `cp -p`, `rsync -t`).
//!
//! Racily clean (as git calls it): a filesystem stamps times at some granularity, so a second
//! write inside one tick can leave the whole stamp as it was. A stamp is trusted only when its
//! `ctime` is more than [`RACY`] before the moment its bytes were read; until a later check reads
//! the file again and sees its `ctime` that far in the past, every check reads it. A future mtime
//! does not matter: the `ctime` of that `utimes` call ages like any other. On a non-unix target
//! there is no `ctime`, and the rule falls back to the mtime, which a tool can set.
//!
//! The limit: the moment of the read is this machine's clock, and the stamps are the file
//! server's on NFS or SMB. A server clock running more than [`RACY`] behind this one makes a
//! fresh stamp look old, so a same-tick rewrite there can be missed. (git compares with its index
//! file's own mtime, stamped by the same server; a marker file under `.rdy/run` would do the same
//! here, at the price of a write per check.)
use rdy_engine::{FileStamp, PackFingerprint, pack_fingerprint};
use rdy_model::Hash;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// The widest timestamp granularity assumed: 2 s covers FAT and other coarse filesystems, and the
/// lag of a coarse kernel clock behind the wall clock. Reading small pack files is cheap, so
/// this errs long.
pub const RACY: Duration = Duration::from_secs(2);

/// A file's stamp. `None`: not known to match the bytes last seen, so the next check reads them.
type Stat = Option<FileStamp>;

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
    /// The racy window in ns: [`RACY`], except in this module's tests.
    window_ns: AtomicU64,
}

fn stat(path: &Path) -> Stat {
    std::fs::metadata(path).ok().map(|m| FileStamp::of(&m))
}

/// The fingerprint's files with their stamps as it took them (each just before its bytes were
/// read), kept only where the `ctime` is more than `window` before `fp.at`, the clock read before
/// the first stat. A later write, rename or `utimes` gets a `ctime` past `fp.at - window`, so it
/// never matches a kept stamp; a racily clean file keeps `None` and is read on the next check.
fn table_of(fp: &PackFingerprint, window: Duration) -> Table {
    let files = fp
        .files
        .iter()
        .map(|(path, stamp)| {
            let safe = fp
                .at
                .duration_since(stamp.ctime)
                .is_ok_and(|age| age > window);
            (path.clone(), safe.then_some(*stamp))
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
            table: Mutex::new(table_of(loaded, RACY)),
            hashed: AtomicUsize::new(0),
            window_ns: AtomicU64::new(RACY.as_nanos() as u64),
        }
    }

    fn window(&self) -> Duration {
        Duration::from_nanos(self.window_ns.load(Ordering::Relaxed))
    }

    /// Test hook: sets the racy window (a long one keeps every file racy, zero trusts any file
    /// whose `ctime` is before the read).
    #[cfg(test)]
    fn set_window(&self, window: Duration) {
        self.window_ns
            .store(window.as_nanos() as u64, Ordering::Relaxed);
        let mut table = self.table();
        for (_, s) in table.files.iter_mut() {
            *s = None;
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
    /// file's stamp is trusted (not racily clean) and matches, and the set of files
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
                *table = table_of(&fp, self.window());
                false
            }
            Ok(_) | Err(_) => true,
        }
    }

    /// Records `fp` as seen: `changed` compares with it from now on. Its stats are the ones it
    /// took, never newer ones, so bytes written after it was read are never marked seen.
    pub fn seen(&self, fp: &PackFingerprint) {
        *self.table() = table_of(fp, self.window());
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
    use std::time::SystemTime;

    const A: &str = "name: p\nversion: '1'\nextends: aaaa\n";
    const B: &str = "name: p\nversion: '1'\nextends: bbbb\n";
    /// Far past any check below: with it, every file is racily clean however slow the machine.
    const LONG: Duration = Duration::from_secs(3600);

    fn project() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".rdy/packs")).unwrap();
        std::fs::write(d.path().join(".rdy/config.yaml"), "pack: p\n").unwrap();
        let pack = d.path().join(".rdy/packs/p.yaml");
        std::fs::write(&pack, A).unwrap();
        (d, pack)
    }

    /// A watch over `root` with the racy window `window`.
    fn watch(root: &Path, window: Duration) -> PackWatch {
        let w = PackWatch::new(root, &pack_fingerprint(root).unwrap());
        w.set_window(window);
        w
    }

    /// Lets every `ctime` stamped so far fall before the next read, coarse clocks included,
    /// for a watch whose window is zero.
    fn settle() {
        std::thread::sleep(Duration::from_millis(50));
    }

    fn mtime(p: &Path) -> SystemTime {
        std::fs::metadata(p).unwrap().modified().unwrap()
    }

    fn set_mtime(p: &Path, t: SystemTime) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn racy1_a_same_length_rewrite_in_the_same_mtime_tick_is_seen() {
        let (d, pack) = project();
        let w = watch(d.path(), LONG);
        assert!(!w.changed());
        // a coarse filesystem gives the second write the first one's stamps
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
        w.set_window(LONG);
        w.seen(&fp);
        let m = mtime(&pack);
        std::fs::write(&pack, B).unwrap();
        set_mtime(&pack, m);
        assert!(w.changed());
    }

    #[test]
    fn racy1_files_safely_in_the_past_are_trusted_without_reading() {
        let (d, pack) = project();
        settle();
        let w = watch(d.path(), Duration::ZERO);
        assert!(
            !w.changed(),
            "the first check reads (the stamps were dropped)"
        );
        let first = w.stats();
        for _ in 0..5 {
            assert!(!w.changed());
        }
        assert_eq!(w.stats(), first, "old stamps that match are trusted");
        // a change of stamp is still read, and found
        std::fs::write(&pack, B).unwrap();
        assert!(w.changed());
    }

    #[test]
    fn racy1_a_fresh_file_is_trusted_once_a_later_check_sees_it_safely_past() {
        let (d, _) = project();
        let w = watch(d.path(), LONG);
        assert!(!w.changed());
        assert!(!w.changed());
        assert_eq!(w.stats(), 2, "racily clean: read on every check");
        // the same stamps, now safely past: one more read, then none
        settle();
        w.set_window(Duration::ZERO);
        assert!(!w.changed());
        assert!(!w.changed());
        assert!(!w.changed());
        assert_eq!(w.stats(), 3);
    }

    #[test]
    fn racy1_a_same_length_pack_renamed_into_place_with_its_mtime_kept_is_seen() {
        let (d, pack) = project();
        settle();
        let w = watch(d.path(), Duration::ZERO);
        assert!(!w.changed());
        let trusted = w.stats();
        assert!(!w.changed());
        assert_eq!(w.stats(), trusted, "the pack's stamp is trusted");
        // an editor's save: write a sibling, carry the old mtime over, rename it into place
        let m = mtime(&pack);
        let tmp = d.path().join(".rdy/packs/.p.yaml.tmp");
        std::fs::write(&tmp, B).unwrap();
        set_mtime(&tmp, m);
        std::fs::rename(&tmp, &pack).unwrap();
        assert_eq!(std::fs::metadata(&pack).unwrap().len(), A.len() as u64);
        assert_eq!(mtime(&pack), m);
        assert!(w.changed(), "same length and mtime, new inode and bytes");
    }

    #[test]
    fn racy1_a_future_mtime_is_trusted_once_its_ctime_is_past() {
        let (d, pack) = project();
        set_mtime(&pack, SystemTime::now() + Duration::from_secs(3600));
        settle();
        let w = watch(d.path(), Duration::ZERO);
        assert!(!w.changed());
        let trusted = w.stats();
        for _ in 0..5 {
            assert!(!w.changed());
        }
        assert_eq!(w.stats(), trusted, "a future mtime alone forces no read");
        // and a rewrite that keeps that mtime is still seen
        let m = mtime(&pack);
        std::fs::write(&pack, B).unwrap();
        set_mtime(&pack, m);
        assert!(w.changed());
    }
}
