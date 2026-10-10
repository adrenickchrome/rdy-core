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
///
/// The temp name is predictable, so whatever stands there is removed first, and the temp file is
/// created with `create_new` (`O_EXCL`): a link planted at the temp name is never followed, and
/// one raced in after the removal fails the write. Something that cannot be removed there (a
/// directory) fails the write with an error that names the temp path.
pub fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = parent_of(path);
    let tmp = temp_beside(path)?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    record(path);
    sync_dir(parent)
}

/// As [`write_durable`] for many files at once, with the same temp-name rules and the same crash
/// rule (each path ends with its old bytes or its new bytes, never a part). Every temp file is
/// written and fsynced first, then each is renamed into place, then each directory is fsynced
/// once: the parent of every path, and each of `extra_dirs` (a directory the caller just created).
/// A directory that holds many files costs one directory flush, not one per file. Records every
/// path after its rename, then each directory, in the sync recorder. The parents must exist.
/// On an error, the temp files not yet renamed are removed.
pub fn write_durable_batch(items: &[(PathBuf, &[u8])], extra_dirs: &[PathBuf]) -> io::Result<()> {
    let mut staged: Vec<(PathBuf, PathBuf)> = vec![]; // (temp, final)
    let result = stage_all(items, &mut staged).and_then(|()| {
        let mut dirs: std::collections::BTreeSet<PathBuf> = extra_dirs.iter().cloned().collect();
        for (tmp, path) in &staged {
            std::fs::rename(tmp, path)?;
            record(path);
            dirs.insert(parent_of(path).to_path_buf());
        }
        staged.clear();
        for d in &dirs {
            sync_dir(d)?;
        }
        Ok(())
    });
    for (tmp, _) in &staged {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// The temp path beside `path`, with whatever stood there removed (see [`write_durable`]).
fn temp_beside(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(".tmp");
    let tmp = parent_of(path).join(tmp_name);
    match std::fs::remove_file(&tmp) {
        // a directory (or anything else rdy cannot remove) at the temp name: the person moves it
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io::Error::new(
            e.kind(),
            format!(
                "cannot remove {} to write through it ({e}); move it aside",
                tmp.display()
            ),
        )),
        _ => Ok(tmp),
    }
}

fn stage_all(items: &[(PathBuf, &[u8])], staged: &mut Vec<(PathBuf, PathBuf)>) -> io::Result<()> {
    for (path, bytes) in items {
        let tmp = temp_beside(path)?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        staged.push((tmp, path.clone()));
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msw11_write_durable_never_writes_through_a_link_at_the_temp_name() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let path = dir.path().join("blob.png");
        std::os::unix::fs::symlink(&victim, dir.path().join(".blob.png.tmp")).unwrap();
        write_durable(&path, b"new bytes").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
        assert_eq!(std::fs::read(&path).unwrap(), b"new bytes");
        assert!(std::fs::symlink_metadata(dir.path().join(".blob.png.tmp")).is_err());
    }

    /// Final review M6: a directory at the temp name fails the write with an error that names
    /// the temp path, so a person knows what to move aside; the directory is kept.
    #[test]
    fn msw11_a_directory_at_the_temp_name_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.png");
        let tmp = dir.path().join(".blob.png.tmp");
        std::fs::create_dir(&tmp).unwrap();
        std::fs::write(tmp.join("kept.txt"), b"mine").unwrap();
        let e = write_durable(&path, b"new bytes").unwrap_err();
        assert!(
            e.to_string().contains(&tmp.display().to_string()),
            "the error names the temp path: {e}"
        );
        assert_eq!(std::fs::read(tmp.join("kept.txt")).unwrap(), b"mine");
        assert!(std::fs::symlink_metadata(&path).is_err());
    }

    #[test]
    fn mfb5_a_batch_flushes_each_directory_once_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("old.txt"), b"old").unwrap();
        let items: Vec<(PathBuf, &[u8])> = vec![
            (dir.path().join("a.txt"), b"a"),
            (dir.path().join("b.txt"), b"b"),
            (sub.join("old.txt"), b"new"),
            (sub.join("c.txt"), b"c"),
        ];
        let (r, synced) = record_syncs(|| write_durable_batch(&items, &[]));
        r.unwrap();
        for (p, b) in &items {
            assert_eq!(&std::fs::read(p).unwrap(), b);
        }
        let dirs: Vec<_> = synced.iter().filter(|p| p.is_dir()).collect();
        assert_eq!(dirs.len(), 2, "{synced:?}");
        assert_eq!(synced.len(), 6, "four files, two directories: {synced:?}");
        assert!(
            std::fs::read_dir(&sub)
                .unwrap()
                .all(|e| { !e.unwrap().file_name().to_string_lossy().ends_with(".tmp") })
        );
        // a failure leaves no temp file behind and the earlier files unchanged
        let bad: Vec<(PathBuf, &[u8])> = vec![
            (dir.path().join("x.txt"), b"x"),
            (dir.path().join("missing/y.txt"), b"y"),
        ];
        assert!(write_durable_batch(&bad, &[]).is_err());
        assert!(!dir.path().join(".x.txt.tmp").exists());
        assert!(!dir.path().join("x.txt").exists());
    }
}
