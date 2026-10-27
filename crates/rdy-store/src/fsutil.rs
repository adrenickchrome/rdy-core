//! Directory durability: a new file or directory entry survives a power loss only once its
//! parent directory is fsynced.
//!
//! Private and settings replacements reach every mode call through [`crate::pio`].
use crate::pio;
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

thread_local! {
    /// The active sync recorder of this thread, if any (see [`record_syncs`]).
    static RECORDER: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    /// The private-write probe of this thread, if any (see
    /// [`with_private_write_probe_for_tests`]).
    static PROBE: RefCell<ProbeState> = const { RefCell::new(ProbeState::Off) };
    /// The active zero recorder of this thread, if any (see [`record_zeroed`]).
    static ZEROED: RefCell<Option<Vec<ZeroedFile>>> = const { RefCell::new(None) };
}

/// The paths whose zeroing fails (see [`fail_zeroing_for_tests`]).
static FAIL_ZEROING: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Test hook: while the returned guard lives, zeroing the file `path` names (or named) fails
/// with an injected error, on any thread.
#[doc(hidden)]
pub fn fail_zeroing_for_tests(path: &Path) -> FailZeroing {
    FAIL_ZEROING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(path.to_path_buf());
    FailZeroing(path.to_path_buf())
}

/// The guard of [`fail_zeroing_for_tests`]; dropping it ends the failure.
#[doc(hidden)]
pub struct FailZeroing(PathBuf);

impl Drop for FailZeroing {
    fn drop(&mut self) {
        let mut all = FAIL_ZEROING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = all.iter().position(|p| *p == self.0) {
            all.remove(i);
        }
    }
}

/// The zero write's chunk: one write covers at most this many bytes.
const ZERO_CHUNK: u64 = 1 << 20;

/// A file [`zero_file`] overwrote, as the zero recorder saw it just before its unlink.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZeroedFile {
    pub path: PathBuf,
    pub len: u64,
    /// Every byte of the file read zero.
    pub all_zero: bool,
}

/// Runs `f` with a zero recorder turned on for this thread and returns its result with every
/// file zeroed while it ran, in order, each read back after its sync. The recorder is turned off
/// again even if `f` panics. Nested or concurrent recording is not supported.
#[doc(hidden)]
pub fn record_zeroed<T>(f: impl FnOnce() -> T) -> (T, Vec<ZeroedFile>) {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            ZEROED.with(|r| *r.borrow_mut() = None);
        }
    }
    ZEROED.with(|r| *r.borrow_mut() = Some(vec![]));
    let _off = Off;
    let out = f();
    let zeroed = ZEROED.with(|r| r.borrow_mut().take().unwrap_or_default());
    (out, zeroed)
}

/// Overwrites the regular file at `path` with zeros over its whole length, then syncs it
/// (D-6H0ZX). A link is never followed. It writes at most 1 MiB at a time, and as many bytes as
/// the file held when it was opened. A file with more than one link is not rdy's own copy: it is
/// not zeroed, a [`crate::notice`] says so, and the error says why.
pub fn zero_file(path: &Path) -> io::Result<()> {
    let f = pio::open_no_follow(OpenOptions::new().write(true).read(true), path)?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let links = crate::fileid::links(&meta);
    if links != 1 {
        let why = format!(
            "{} has more than one link ({links}), so it was not zeroed",
            path.display()
        );
        crate::notice(&format!("rdy: {why}"));
        return Err(io::Error::other(why));
    }
    zero_open_file(&f, path)
}

/// [`zero_file`] through `f`, an open handle on the file `path` names (or named).
pub fn zero_open_file(f: &File, path: &Path) -> io::Result<()> {
    if FAIL_ZEROING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|p| p == path)
    {
        return Err(io::Error::other("injected zeroing failure"));
    }
    let len = f.metadata()?.len();
    let chunk = vec![0u8; len.min(ZERO_CHUNK) as usize];
    let mut at = 0u64;
    while at < len {
        let n = (len - at).min(ZERO_CHUNK) as usize;
        pio::write_all_at(f, &chunk[..n], at)?;
        at += n as u64;
    }
    f.sync_all()?;
    record(path);
    ZEROED.with(|r| {
        if let Some(v) = r.borrow_mut().as_mut() {
            let mut all_zero = true;
            let mut buf = vec![0u8; len.min(ZERO_CHUNK) as usize];
            let mut at = 0u64;
            while at < len {
                let n = (len - at).min(ZERO_CHUNK) as usize;
                if pio::read_exact_at(f, &mut buf[..n], at).is_err()
                    || buf[..n].iter().any(|b| *b != 0)
                {
                    all_zero = false;
                    break;
                }
                at += n as u64;
            }
            v.push(ZeroedFile {
                path: path.to_path_buf(),
                len,
                all_zero,
            });
        }
    });
    Ok(())
}

/// [`zero_file`], then removes the file (D-6H0ZX: a damaged index is zeroed before it goes).
pub fn zero_then_remove(path: &Path) -> io::Result<()> {
    zero_file(path)?;
    std::fs::remove_file(path)
}

/// A private-write probe, boxed.
type Probe = Box<dyn FnMut(PrivateWriteStage, &Path) -> io::Result<()>>;

/// This thread's probe: none, installed, or out on a call (so a probe that writes is not
/// probed again).
enum ProbeState {
    Off,
    On(Probe),
    Running,
}

/// A stage of a private or settings replacement, or of a config edit's new directory, after
/// which the test probe runs.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateWriteStage {
    /// A config edit made a directory, and has not opened it yet (see [`probe_dir_made`]).
    DirMade,
    /// The temp file exists, with its final mode and no bytes.
    Created,
    /// The bytes are written to the temp file.
    Written,
    /// The temp file is fsynced.
    Synced,
    /// The temp file is renamed over the target.
    Renamed,
    /// The parent directory is fsynced.
    DirSynced,
}

/// Runs `run` with `probe` called after each stage of every private or settings replacement on
/// this thread (with the temp path, then the target, then the parent directory), and after a
/// config edit makes a directory (with that directory's path). A probe error
/// stops the replacement as that stage's own failure would, with the same cleanup. The probe is
/// removed again even if `run` panics. A nested probe panics.
#[doc(hidden)]
pub fn with_private_write_probe_for_tests<T>(
    probe: impl FnMut(PrivateWriteStage, &Path) -> io::Result<()> + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            PROBE.with(|p| *p.borrow_mut() = ProbeState::Off);
        }
    }
    PROBE.with(|p| {
        let mut p = p.borrow_mut();
        assert!(
            matches!(*p, ProbeState::Off),
            "a private-write probe is already installed on this thread"
        );
        *p = ProbeState::On(Box::new(probe));
    });
    let _off = Off;
    run()
}

/// Calls this thread's probe, if there is one, with [`PrivateWriteStage::DirMade`] at `path`: a
/// config edit calls it after it makes the directory `path` and before it opens it. A probe error
/// is returned, to be handled as a failed open.
#[doc(hidden)]
pub fn probe_dir_made(path: &Path) -> io::Result<()> {
    probe(PrivateWriteStage::DirMade, path)
}

/// Calls this thread's probe, if there is one, after `stage` at `path`.
fn probe(stage: PrivateWriteStage, path: &Path) -> io::Result<()> {
    let taken = PROBE.with(|p| {
        let mut p = p.borrow_mut();
        match std::mem::replace(&mut *p, ProbeState::Running) {
            ProbeState::On(f) => Some(f),
            other => {
                *p = other;
                None
            }
        }
    });
    let Some(mut f) = taken else {
        return Ok(());
    };
    let r = f(stage, path);
    PROBE.with(|p| {
        let mut p = p.borrow_mut();
        if matches!(*p, ProbeState::Running) {
            *p = ProbeState::On(f);
        }
    });
    r
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
    let tmp = temp_beside(path)?;
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    record(path);
    sync_dir(parent_of(path))
}

/// As [`write_durable`], for a secret: the temp file is created private (mode 0o600, whatever
/// the umask) before any byte is written, so the target ends 0o600. A failure before the rename
/// removes the temp file and leaves the target as it was. A failure of the directory fsync after
/// the rename is an error, with the new bytes possibly in place. The parent directory is opened
/// once (links in its path are followed), and every later step goes through that handle, as in
/// [`replace_in_dir`].
pub fn write_private_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (dir, name) = open_parent(path)?;
    replace_in_dir(&dir, parent_of(path), name, bytes, None)
}

/// As [`write_private_durable`], for a settings file: an existing regular file at `path` keeps
/// its permissions (the temp file gets them before any byte is written); a new file is 0o600.
pub fn write_settings_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (dir, name) = open_parent(path)?;
    let keep = match pio::open_read_at(&dir, name) {
        Ok(f) => {
            let m = f.metadata()?;
            m.is_file().then(|| m.permissions())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound || pio::is_link_refusal(&e) => None,
        Err(e) => return Err(e),
    };
    replace_in_dir(&dir, parent_of(path), name, bytes, keep.as_ref())
}

/// The open parent directory of `path`, and `path`'s file name.
fn open_parent(path: &Path) -> io::Result<(File, &OsStr)> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    Ok((pio::open_dir(parent_of(path))?, name))
}

/// Makes `bytes` the durable content of the entry `name` of the open directory `dir`: the
/// private and settings replacement. Every step names an entry of `dir`, never a path, so a
/// directory moved or replaced after `dir` was opened cannot redirect the write.
///
/// Whatever stands at the temp name `.<name>.tmp` is removed (a directory there fails the write),
/// then the temp file is created new (`O_EXCL`, never through a link) and set to `perms` (an
/// existing settings file's permissions) or to 0o600, before any byte is written. It is written,
/// fsynced and renamed over `name`, then `dir` is fsynced. A failure before the rename removes the
/// temp file and leaves the target as it was; a failure of the directory fsync is an error, with
/// the new bytes possibly in place. `dir_path` names `dir` for the probe and the sync recorder only.
pub fn replace_in_dir(
    dir: &File,
    dir_path: &Path,
    name: &OsStr,
    bytes: &[u8],
    perms: Option<&Permissions>,
) -> io::Result<()> {
    let tmp_name = temp_name(name);
    let tmp = dir_path.join(&tmp_name);
    let target = dir_path.join(name);
    match pio::remove_file_at(dir, &tmp_name) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return Err(io::Error::new(
                e.kind(),
                format!(
                    "cannot remove {} to write through it ({e}); move it aside",
                    tmp.display()
                ),
            ));
        }
        _ => {}
    }
    let f = pio::create_private_at(dir, &tmp_name)?;
    // from here the temp file is this call's: a failure before the rename removes it
    let staged = stage_private(f, dir, &tmp, &tmp_name, name, bytes, perms);
    if let Err(e) = staged {
        let _ = pio::remove_file_at(dir, &tmp_name);
        return Err(e);
    }
    record(&target);
    probe(PrivateWriteStage::Renamed, &target)?;
    dir.sync_all()?;
    record(dir_path);
    probe(PrivateWriteStage::DirSynced, dir_path)
}

/// Sets the temp file's mode, writes and fsyncs it, then renames it over `name`.
fn stage_private(
    mut f: File,
    dir: &File,
    tmp: &Path,
    tmp_name: &OsStr,
    name: &OsStr,
    bytes: &[u8],
    perms: Option<&Permissions>,
) -> io::Result<()> {
    match perms {
        Some(keep) => f.set_permissions(keep.clone())?,
        None => pio::make_private_file(&f)?,
    }
    probe(PrivateWriteStage::Created, tmp)?;
    f.write_all(bytes)?;
    probe(PrivateWriteStage::Written, tmp)?;
    f.sync_all()?;
    probe(PrivateWriteStage::Synced, tmp)?;
    drop(f);
    pio::rename_at(dir, tmp_name, name)
}

/// As [`write_durable`] for many files at once, with the same temp-name rules and the same crash
/// rule (each path ends with its old bytes or its new bytes, never a part). Every temp file is
/// written and fsynced first, then each is renamed into place, then each directory is fsynced
/// once: the parent of every path, and each of `extra_dirs` (a directory the caller just created).
/// A directory that holds many files costs one directory flush, not one per file. Records every
/// path after its rename, then each directory, in the sync recorder. The parents must exist.
/// On an error, the temp files not yet renamed are removed, and the error comes with the path
/// that failed: the file whose temp write or rename failed, or the directory whose flush failed.
pub fn write_durable_batch(
    items: &[(PathBuf, &[u8])],
    extra_dirs: &[PathBuf],
) -> Result<(), (PathBuf, io::Error)> {
    let mut staged: Vec<(PathBuf, PathBuf)> = vec![]; // (temp, final)
    let result = stage_all(items, &mut staged).and_then(|()| {
        let mut dirs: std::collections::BTreeSet<PathBuf> = extra_dirs.iter().cloned().collect();
        for (tmp, path) in &staged {
            std::fs::rename(tmp, path).map_err(|e| (path.clone(), e))?;
            record(path);
            dirs.insert(parent_of(path).to_path_buf());
        }
        staged.clear();
        for d in &dirs {
            sync_dir(d).map_err(|e| (d.clone(), e))?;
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
    let tmp = parent_of(path).join(temp_name(name));
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

/// `.<name>.tmp`: the temp name of a replacement of `name`.
fn temp_name(name: &OsStr) -> OsString {
    let mut tmp = OsString::from(".");
    tmp.push(name);
    tmp.push(".tmp");
    tmp
}

fn stage_all(
    items: &[(PathBuf, &[u8])],
    staged: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), (PathBuf, io::Error)> {
    for (path, bytes) in items {
        stage_one(path, bytes, staged).map_err(|e| (path.clone(), e))?;
    }
    Ok(())
}

fn stage_one(path: &Path, bytes: &[u8], staged: &mut Vec<(PathBuf, PathBuf)>) -> io::Result<()> {
    let tmp = temp_beside(path)?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    staged.push((tmp, path.to_path_buf()));
    f.write_all(bytes)?;
    f.sync_all()
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
        let (at, _) = write_durable_batch(&bad, &[]).unwrap_err();
        assert_eq!(at, dir.path().join("missing/y.txt"), "the path that failed");
        assert!(!dir.path().join(".x.txt.tmp").exists());
        assert!(!dir.path().join("x.txt").exists());
    }
}
