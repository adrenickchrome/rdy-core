//! Directory durability: a new file or directory entry survives a power loss only once its
//! parent directory is fsynced.
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Fsyncs a directory, so entries created in it survive a power loss.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
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
