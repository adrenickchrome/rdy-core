//! `rdy dump` (plan Dump and load; D-69AY3, D-D2NPN, D-FR0C4): the whole project as one
//! stream of dump format 1 (`rdy_store::dump`). It holds the published log's sealed batches,
//! byte for byte, and the closed set of project files (F5): `.rdy.yaml`, `.rdyignore` and the
//! pack files in `.rdy/packs`, never `.rdy/local.yaml`.
//!
//! A dump is atomic at the target: it writes `<out>.partial-<pid>`, syncs it, and hard-links it
//! to `out`, so `out` is a complete dump or nothing. It never overwrites a file.
use crate::config_levels::PROJECT_FILE;
use crate::error::{E_ALREADY_EXISTS, E_BAD_INPUT, RdyError};
use crate::handle::io;
use crate::pack_load::pack_files;
use crate::read_state::ReadState;
use crate::wc::ignore::IGNORE_FILE;
use rdy_store::dump::{DUMP_FORMAT, DumpHeader, DumpTotals, DumpWriter, is_project_file};
use rdy_store::format::STORE_FORMAT;
use rdy_store::fsutil::sync_dir;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, ErrorKind};
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
        std::fs::read(root.join(PROJECT_FILE)).map_err(io)?,
    )];
    match std::fs::read(root.join(IGNORE_FILE)) {
        Ok(bytes) => files.push((IGNORE_FILE.to_string(), bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(io(e)),
    }
    for f in pack_files(&root.join(PACKS_DIR))? {
        let name = f.file_name().unwrap_or_default();
        let path = format!("{PACKS_DIR}/{}", name.to_string_lossy());
        if name.to_str().is_none() || !is_project_file(&path) {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{path}: rdy dump carries pack files named with A-Z a-z 0-9 . _ - only"),
                "rename the pack file",
            ));
        }
        files.push((path, std::fs::read(&f).map_err(io)?));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

impl ReadState {
    /// Writes this state as a dump to `out`: the project files, then every sealed batch of the
    /// log up to `seq()`. Refuses an `out` that exists and an `out` whose directory does not.
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
        let files = project_files(self.root())?;
        let mut partial = out.as_os_str().to_owned();
        partial.push(format!(".partial-{}", std::process::id()));
        let partial = Path::new(&partial);
        // only a partial file this call created is ever removed
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(partial)
            .map_err(io)?;
        let totals = match self.write_dump(file, &files).and_then(|t| {
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

    /// Writes the whole dump into `file` and syncs it.
    fn write_dump(&self, file: File, files: &[(String, Vec<u8>)]) -> Result<DumpTotals, RdyError> {
        let header = DumpHeader {
            store_format: STORE_FORMAT,
            log_end: self.seq(),
            // the workspace version: the header names the rdy that wrote the dump (O17)
            rdy: env!("CARGO_PKG_VERSION").to_string(),
        };
        let mut w = DumpWriter::new(BufWriter::new(file), &header)?;
        for (path, bytes) in files {
            w.file(path, bytes)?;
        }
        self.reader().for_each_batch(self.seq(), |b| w.batch(&b))?;
        let (buf, totals) = w.finish()?;
        let file = buf.into_inner().map_err(|e| io(e.into_error()))?;
        file.sync_all().map_err(io)?;
        Ok(totals)
    }
}

/// `out` exists: a dump never overwrites a file.
fn exists(out: &Path) -> RdyError {
    RdyError::new(
        E_ALREADY_EXISTS,
        format!("{} exists", out.display()),
        "pick a new file name; rdy dump never overwrites a file",
    )
}
