//! `rdy dump` (plan Dump and load; D-69AY3, D-D2NPN, D-FR0C4): the whole project as one
//! stream of dump format 1 (`rdy_store::dump`). It holds the published log's sealed batches,
//! byte for byte, and the closed set of project files (F5): `.rdy.yaml`, `.rdyignore` and the
//! pack files in `.rdy/packs`, never `.rdy/local.yaml`.
//!
//! A dump is atomic at the target: it writes `<out>.partial-<pid>`, syncs it, and hard-links it
//! to `out`, so `out` is a complete dump or nothing. It never overwrites a file.
use crate::config_levels::PROJECT_FILE;
use crate::error::{E_ALREADY_EXISTS, E_BAD_INPUT, E_IO, RdyError};
use crate::handle::io;
use crate::pack_load::pack_files;
use crate::read_state::ReadState;
use crate::wc::ignore::IGNORE_FILE;
use rdy_store::StoreError;
use rdy_store::dump::{
    DUMP_FORMAT, DumpHeader, DumpTotals, DumpWriter, MAX_FILE_BYTES, MAX_FILES, is_project_file,
};
use rdy_store::format::STORE_FORMAT;
use rdy_store::fsutil::sync_dir;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{BufWriter, ErrorKind, Write};
use std::path::Path;

/// The directory of the pack files a dump carries, relative to the project root.
const PACKS_DIR: &str = ".rdy/packs";

/// What `rdy dump` wrote: the file, its formats, what it holds and its length.
#[derive(Clone, Debug, Serialize)]
pub struct DumpSummary {
    pub out: String,
    pub dump_format: u32,
    pub store_format: u32,
    pub batches: u64,
    pub frames: u64,
    pub files: Vec<String>,
    pub bytes: u64,
}

/// The project files a dump carries (F5, a closed set), with their bytes, in ascending path
/// order: `.rdy.yaml` (required), `.rdyignore` if present, and each `.rdy/packs` file that the
/// pack loader reads. A pack file whose name is outside the set is refused.
pub fn project_files(root: &Path) -> Result<Vec<(String, Vec<u8>)>, RdyError> {
    let mut files = vec![(
        PROJECT_FILE.to_string(),
        read_named(&root.join(PROJECT_FILE))?,
    )];
    match std::fs::read(root.join(IGNORE_FILE)) {
        Ok(bytes) => files.push((IGNORE_FILE.to_string(), bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(io_at(&root.join(IGNORE_FILE), e)),
    }
    for f in pack_files(&root.join(PACKS_DIR))? {
        let name = f.file_name().unwrap_or_default();
        let path = format!("{PACKS_DIR}/{}", name.to_string_lossy());
        if name.to_str().is_none() || !is_project_file(&path) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!(
                    "{path}: rdy dump carries pack files named with A-Z a-z 0-9 . _ - only, and not starting with a dot"
                ),
                "rename the pack file",
            ));
        }
        files.push((path, read_named(&f)?));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

impl ReadState {
    /// Writes this state as a dump to `out`: the project files, then every sealed batch of the
    /// log up to `seq()`. Refuses an `out` that exists, an `out` whose directory does not, and
    /// an `out` in the project's `.rdy` directory.
    pub fn dump(&self, out: &Path) -> Result<DumpSummary, RdyError> {
        if std::fs::symlink_metadata(out).is_ok() {
            return Err(exists(out));
        }
        let dir = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !dir.exists() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{} does not exist", dir.display()),
                "create the directory first, or pick another path",
            ));
        }
        inside_rdy(self.root(), dir, out)?;
        let files = project_files(self.root())?;
        let mut partial = out.as_os_str().to_owned();
        partial.push(format!(".partial-{}", std::process::id()));
        let partial = Path::new(&partial);
        // only a partial file this call created is ever removed
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(partial)
            .map_err(|e| match e.kind() {
                // never removed: it is another call's, or left by a dump that was killed
                ErrorKind::AlreadyExists => RdyError::new(
                    E_ALREADY_EXISTS,
                    format!(
                        "{} exists: another dump may be writing it",
                        partial.display()
                    ),
                    "remove it if no dump is running, then retry",
                ),
                _ => io_at(partial, e),
            })?;
        let totals = match self
            .write_dump(BufWriter::new(file), &files)
            .and_then(|(buf, t)| {
                let file = buf.into_inner().map_err(|e| io(e.into_error()))?;
                file.sync_all().map_err(io)?;
                Ok(t)
            })
            .and_then(|t| {
                std::fs::hard_link(partial, out).map_err(|e| match e.kind() {
                    ErrorKind::AlreadyExists => exists(out),
                    _ => io(e),
                })?;
                Ok(t)
            }) {
            Ok(totals) => totals,
            Err(e) => {
                let _ = std::fs::remove_file(partial);
                return Err(e);
            }
        };
        std::fs::remove_file(partial).map_err(io)?;
        sync_dir(dir).map_err(io)?;
        Ok(DumpSummary {
            out: out.display().to_string(),
            dump_format: DUMP_FORMAT,
            store_format: STORE_FORMAT,
            batches: totals.batches,
            frames: totals.frames,
            files: files.into_iter().map(|(path, _)| path).collect(),
            bytes: totals.bytes,
        })
    }

    /// Writes the whole dump into `w` and returns `w`, flushed.
    fn write_dump<W: Write>(
        &self,
        w: W,
        files: &[(String, Vec<u8>)],
    ) -> Result<(W, DumpTotals), RdyError> {
        let header = DumpHeader {
            store_format: STORE_FORMAT,
            log_end: self.seq(),
            // the workspace version: the header names the rdy that wrote the dump (O17)
            rdy: env!("CARGO_PKG_VERSION").to_string(),
        };
        check_limits(files)?;
        // the writer's i/o errors are the destination's (disk full, say), never the store's
        let mut w = DumpWriter::new(w, &header).map_err(dest)?;
        for (path, bytes) in files {
            w.file(path, bytes).map_err(dest)?;
        }
        let mut failed = None;
        let read = self.reader().for_each_batch(self.seq(), |b| {
            w.batch(&b).map_err(|e| {
                let stop = StoreError::Refused(e.to_string());
                failed = Some(dest(e));
                stop
            })
        });
        if let Some(e) = failed {
            return Err(e);
        }
        read?;
        w.finish().map_err(dest)
    }
}

/// Refuses an `out` in the project's `.rdy` directory or below it: a file there can break the
/// store (a stray pack file, a segment name). Compares resolved paths, so a symlink or `..`
/// does not pass.
fn inside_rdy(root: &Path, dir: &Path, out: &Path) -> Result<(), RdyError> {
    let rdy = root.join(".rdy");
    let rdy = std::fs::canonicalize(&rdy).map_err(|e| io_at(&rdy, e))?;
    let dir = std::fs::canonicalize(dir).map_err(|e| io_at(dir, e))?;
    if dir.starts_with(&rdy) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} is inside the project .rdy directory", out.display()),
            "dump to a path outside .rdy",
        ));
    }
    Ok(())
}

/// Refuses more project files than a dump carries, and a file larger than a dump carries, each
/// with its own fix: a retry never passes these.
fn check_limits(files: &[(String, Vec<u8>)]) -> Result<(), RdyError> {
    if files.len() > MAX_FILES as usize {
        let others = files
            .iter()
            .filter(|(p, _)| !p.starts_with(&format!("{PACKS_DIR}/")))
            .count();
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "the project has {} files to dump, and rdy dump carries at most {MAX_FILES}",
                files.len()
            ),
            format!("keep at most {} pack files", MAX_FILES as usize - others),
        ));
    }
    if let Some((path, _)) = files.iter().find(|(_, b)| b.len() > MAX_FILE_BYTES) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{path} is larger than 16 MiB"),
            "make the file 16 MiB or smaller",
        ));
    }
    Ok(())
}

/// An error of the dump writer: its i/o errors are the destination's.
fn dest(e: StoreError) -> RdyError {
    match e {
        StoreError::Io(e) => io(e),
        e => e.into(),
    }
}

/// Reads one project file; an error names it.
fn read_named(path: &Path) -> Result<Vec<u8>, RdyError> {
    std::fs::read(path).map_err(|e| io_at(path, e))
}

/// An i/o error at `path`, named.
fn io_at(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check file permissions and disk space",
    )
}

/// `out` exists: a dump never overwrites a file.
fn exists(out: &Path) -> RdyError {
    RdyError::new(
        E_ALREADY_EXISTS,
        format!("{} exists", out.display()),
        "pick a new file name; rdy dump never overwrites a file",
    )
}

#[cfg(test)]
mod tests {
    use crate::Rdy;
    use crate::error::E_IO;
    use std::io::Write;

    /// A sink that takes `room` bytes, then fails as a full disk does.
    #[derive(Debug)]
    struct Full {
        room: usize,
    }

    impl Write for Full {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.room == 0 {
                return Err(std::io::Error::from_raw_os_error(28));
            }
            let n = buf.len().min(self.room);
            self.room -= n;
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn dl5_a_full_disk_is_an_io_error_of_the_destination_not_of_the_store() {
        let dir = tempfile::tempdir().unwrap();
        crate::testkit::generate(dir.path(), 40);
        let r = Rdy::open(dir.path()).unwrap();
        let st = r.read();
        let files = super::project_files(dir.path()).unwrap();
        let (whole, totals) = st.write_dump(Vec::new(), &files).unwrap();
        assert!(totals.batches > 1, "a log of several batches");
        // the header, a project file, the batches, and the trailer
        for room in [0, 20, 200, whole.len() / 2, whole.len() - 1] {
            let e = st.write_dump(Full { room }, &files).unwrap_err();
            assert_eq!(e.code, E_IO, "room {room}: {e:?}");
            assert_eq!(
                e.fix, "check file permissions and disk space",
                "room {room}: {e:?}"
            );
        }
    }
}
