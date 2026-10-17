//! `rdy clone` (plan Export and clone; D-9QV85, D-D2NPN, D-DAGNB, D-C8C70, RULING F3 and F4): a
//! new project from a local project or a dump, in a directory with no store. A clone is a dump
//! and a load through one stream: its log is the source's log byte for byte, so every hash,
//! landing, tag, view and review is kept, then one origin batch. A clone keeps the source's repo
//! id; a fork takes a new one. Both record where they came from in `meta/origin`.
//!
//! The steps (closed):
//! 1. `File`: a dump whose header names a store format before 12 is refused: it holds no repo id
//!    (O10). The check reads the header of the one stream the load then consumes. `State`: refuse
//!    a target in the source's resolved `.rdy` (final review C1), read the project files, then a
//!    spawned thread loads the pipe's reader while this thread writes the dump into the pipe's
//!    writer (O14).
//! 2. The load's steps 2 to 6, unchanged (`crate::load`).
//! 3. After the open under the load's lock: the loaded store's meta refs pass the store's check
//!    first, so a source that `rdy load` refuses is refused in its words, never written over
//!    (ruling task-5). Then read the loaded store's repo id (the source's), its newest version
//!    and canon's head (O8), and write the origin batch (`Store::put_origin`, a fork with a new
//!    repo id). Then the load's `fsck` and counts.
//! 4. Any failure undoes everything, as the load does.
//! 5. The loader thread is always joined. The clone returns the writer's error when the writer
//!    failed and the loader failed reading the stream; in every other case the loader's (O25).
//!
//! Platform (D-1RXEA): `std::io::pipe` and `std::thread::spawn` only.
use crate::Rdy;
use crate::config_levels::PROJECT_FILE;
use crate::dump::project_files;
use crate::error::{E_BAD_INPUT, E_DUMP, E_INTERNAL, E_IO, RdyError};
use crate::load::{LoadSummary, Opened, inside_source_rdy, load_opened, open_dump, open_stream};
use crate::read_state::ReadState;
use rdy_model::RepoId;
use rdy_store::layout::STORE_DIR;
use rdy_store::meta::{FIRST_REPO_ID_STORE_FORMAT, Origin};
use serde::Serialize;
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What a clone reads: an open project's published state, or a dump file.
pub enum CloneSource<'a> {
    State(&'a ReadState),
    File(&'a Path),
}

/// The clone source a path names (O7, closed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    File,
    Project,
}

/// Where a clone came from, as the replies show it: the source's repo id, its newest version and
/// its canon head, each `null` when absent.
#[derive(Clone, Debug, Serialize)]
pub struct OriginReply {
    pub repo_id: String,
    pub tag: Option<String>,
    pub landing: Option<String>,
}

impl From<&Origin> for OriginReply {
    fn from(o: &Origin) -> Self {
        OriginReply {
            repo_id: o.repo_id.to_string(),
            tag: o.tag.clone(),
            landing: o.landing.map(|h| h.to_string()),
        }
    }
}

/// What `rdy clone` did: the load's summary (its `next` names the clone), the clone's repo id,
/// and its origin: `origin` for a clone, `forked_from` for a fork.
#[derive(Clone, Debug, Serialize)]
pub struct CloneSummary {
    #[serde(flatten)]
    pub load: LoadSummary,
    pub repo_id: String,
    pub fork: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<OriginReply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<OriginReply>,
}

/// What the person does after a clone: it carries no working copy (O9).
const NEXT: &str = "run rdy checkout to bind a working copy; a clone carries none";

/// The clone source a path names (O7): a path that contains `://` is a URL, refused; a regular
/// file is a dump; a directory with `.rdy.yaml` and `.rdy/store` is a project; anything else is
/// refused.
pub fn source_kind(path: &Path) -> Result<SourceKind, RdyError> {
    let shown = path.display().to_string();
    if shown.contains("://") {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{shown} is a URL: cloning from a hub comes with the sync client"),
            "run rdy dump --out <file> in that project, then clone the file",
        ));
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => return Ok(SourceKind::File),
        Ok(m)
            if m.is_dir()
                && path.join(PROJECT_FILE).is_file()
                && path.join(".rdy").join(STORE_DIR).is_dir() =>
        {
            return Ok(SourceKind::Project);
        }
        _ => {}
    }
    Err(RdyError::new(
        E_BAD_INPUT,
        format!("{shown} is neither a dump file nor an rdy project"),
        "pass a dump file, or a directory with .rdy.yaml and .rdy/store",
    ))
}

/// Clones `source` into `dir`, a directory with no store. A refusal or failure leaves the target
/// as it was.
pub fn clone(source: CloneSource, dir: &Path, fork: bool) -> Result<CloneSummary, RdyError> {
    let (load, origin, repo_id) = match source {
        CloneSource::File(file) => from_file(file, dir, fork)?,
        CloneSource::State(st) => from_state(st, dir, fork)?,
    };
    let reply = OriginReply::from(&origin);
    let (origin, forked_from) = if fork {
        (None, Some(reply))
    } else {
        (Some(reply), None)
    };
    Ok(CloneSummary {
        load: LoadSummary {
            next: NEXT.to_string(),
            ..load
        },
        repo_id: repo_id.to_string(),
        fork,
        origin,
        forked_from,
    })
}

/// What the origin step recorded: the origin and the clone's repo id.
type Recorded = Option<(Origin, RepoId)>;

/// Step 3: the origin batch, written into the loaded project `r`. The load runs this step only
/// after the store's meta check passed, which requires a repo id; a store with none would write
/// nothing, and the load's `fsck` would refuse it.
fn record_origin(r: &mut Rdy, fork: bool) -> Result<Recorded, RdyError> {
    let Some(source_id) = r.store().reader().repo_id()? else {
        return Ok(None);
    };
    let st = r.read();
    let origin = Origin {
        fork,
        repo_id: source_id,
        tag: st.versions()?.last().map(|v| v.name.clone()),
        landing: st.canon_head(),
    };
    drop(st);
    let new_id = fork.then(RepoId::new);
    let after = r.store_mut().put_origin(&origin, new_id)?;
    Ok(Some((origin, after)))
}

/// Runs the load of the opened stream (named `source` in its texts) into `dir` with the origin
/// step after the open.
fn load_recording<R: Read>(
    source: &Path,
    opened: Opened<R>,
    dir: &Path,
    fork: bool,
) -> Result<(LoadSummary, Origin, RepoId), RdyError> {
    let mut got: Recorded = None;
    let mut step = |r: &mut Rdy| {
        got = record_origin(r, fork)?;
        Ok(())
    };
    let load = load_opened(source, opened, dir, Some(&mut step))?;
    // the load's fsck requires a repo id, so a load that passed recorded the origin
    let (origin, repo_id) = got.ok_or_else(|| {
        RdyError::new(
            E_INTERNAL,
            format!(
                "the clone of {} passed fsck with no origin recorded",
                source.display()
            ),
            "report this as a bug",
        )
    })?;
    Ok((load, origin, repo_id))
}

/// Step 1 for a dump file: opens it once and reads its header, refuses a dump older than store
/// format 12 (O10), then loads that same stream: a file replaced after the check is never read.
/// A file that does not open or a header that does not read is refused in the load's words.
fn from_file(
    file: &Path,
    dir: &Path,
    fork: bool,
) -> Result<(LoadSummary, Origin, RepoId), RdyError> {
    let opened = open_stream(file, BufReader::new(open_dump(file)?))?;
    let format = opened.header.store_format;
    if format < FIRST_REPO_ID_STORE_FORMAT {
        let file = file.display();
        return Err(RdyError::new(
            E_DUMP,
            format!(
                "{file} is a dump of store format {format}, which has no repo id, so rdy clone cannot record its origin"
            ),
            format!("load it with rdy load {file} <dir>, then clone that project"),
        ));
    }
    load_recording(file, opened, dir, fork)
}

/// Step 1 for an open project (O14): the loader runs on a spawned thread that owns the pipe's
/// reader; this thread writes the dump into the pipe's writer. The texts name the project's root
/// (O24).
fn from_state(
    st: &ReadState,
    dir: &Path,
    fork: bool,
) -> Result<(LoadSummary, Origin, RepoId), RdyError> {
    // the target must not be in the source's own .rdy, wherever a symlink puts it (C1)
    inside_source_rdy(&st.root().join(".rdy"), dir)?;
    let files = project_files(st.root())?;
    let (reader, writer) = std::io::pipe().map_err(|e| {
        RdyError::new(
            E_IO,
            format!("rdy clone cannot open a pipe: {e}"),
            "close other programs or raise the open file limit, then clone again",
        )
    })?;
    let ended = Arc::new(AtomicBool::new(false));
    let seen = Seen {
        r: reader,
        ended: ended.clone(),
    };
    let source: PathBuf = st.root().to_path_buf();
    let target: PathBuf = dir.to_path_buf();
    let loader = std::thread::spawn(move || {
        let opened = open_stream(&source, BufReader::new(seen))?;
        load_recording(&source, opened, &target, fork)
    });
    // the writer is dropped here on every path, so the loader always sees the stream end
    let wrote = st.write_dump(BufWriter::new(writer), &files).map(drop);
    let loaded = match loader.join() {
        Ok(loaded) => loaded,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    match (wrote, loaded) {
        // the loader failed because the stream stopped short: the writer's error says why
        (Err(w), Err(_)) if ended.load(Ordering::SeqCst) => Err(w),
        // every other case is the loader's: a loader that passed read the trailer, so the
        // writer had written it
        (_, loaded) => loaded,
    }
}

/// A reader that notes when its stream ended: at its end of file or at an error.
struct Seen<R> {
    r: R,
    ended: Arc<AtomicBool>,
}

impl<R: Read> Read for Seen<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(buf);
        let ended = match &n {
            Ok(0) => !buf.is_empty(),
            Ok(_) => false,
            Err(e) => e.kind() != std::io::ErrorKind::Interrupted,
        };
        if ended {
            self.ended.store(true, Ordering::SeqCst);
        }
        n
    }
}
