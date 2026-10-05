//! Directory durability: a new file or directory entry survives a power loss only once its
//! parent directory is fsynced.
use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

thread_local! {
    /// The active sync recorder of this thread, if any (see [`record_syncs`]).
    static RECORDER: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
}

/// Appends `path` to this thread's active sync recorder, if there is one. Code that fsyncs a file
/// other than through [`write_durable`] calls it after a successful sync. Costs nothing when no
/// recorder is active.
#[doc(hidden)]
pub fn record(path: &Path) {
    RECORDER.with(|r| {
        if let Some(v) = r.borrow_mut().as_mut() {
            v.push(path.to_path_buf());
        }
    });
}

/// Runs `f` with a recorder turned on for this thread and returns its result with every path
/// recorded while it ran, in order. The recorder is turned off again even if `f` panics. Nested or
/// concurrent recording is not supported.
#[doc(hidden)]
pub fn record_syncs<T>(f: impl FnOnce() -> T) -> (T, Vec<PathBuf>) {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            RECORDER.with(|r| *r.borrow_mut() = None);
        }
    }
    RECORDER.with(|r| *r.borrow_mut() = Some(vec![]));
    let _off = Off;
    let out = f();
    let synced = RECORDER.with(|r| r.borrow_mut().take().unwrap_or_default());
    (out, synced)
}

/// Fsyncs a directory, so entries created in it survive a power loss.
///
/// This uses `File::sync_all`, never `sync_data`. std implements `sync_all` on Apple targets as
/// `fcntl(F_FULLFSYNC)` (a drive-cache flush, also on a directory descriptor), and on Linux as
/// `fsync`. So no platform code is needed here.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()?;
    record(dir);
    Ok(())
}

/// Makes `bytes` the durable content of `path`, as a replacement that is atomic against crashes:
/// writes `<parent>/.<file name>.tmp`, fsyncs it, renames it over `path`, then fsyncs the parent
/// directory (`.` for a bare file name). Records `path`, then the parent, in the sync recorder.
pub fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(".tmp");
    let tmp = parent.join(tmp_name);
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    record(path);
    sync_dir(parent)
}

/// Creates `dir` and any missing ancestors, then fsyncs the parent of each directory it created,
/// so a power loss can't drop a new directory entry. Returns the parents it fsynced, nearest the
/// root first; empty if `dir` already existed.
pub fn create_dir_durable(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut missing = vec![];
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.as_os_str().is_empty() || d.exists() {
            break;
        }
        missing.push(d.to_path_buf());
        cur = d.parent();
    }
    if missing.is_empty() {
        return Ok(vec![]);
    }
    std::fs::create_dir_all(dir)?;
    let mut synced = vec![];
    for d in &missing {
        let parent = match d.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        sync_dir(&parent)?;
        synced.push(parent);
    }
    synced.reverse();
    Ok(synced)
}
