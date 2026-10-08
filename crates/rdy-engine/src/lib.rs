pub mod accept;
pub mod alt;
mod areas;
pub mod brief;
pub mod code_brief;
pub mod code_index;
pub mod config_levels;
pub mod consequence;
pub mod coord_graph;
pub mod coverage;
mod decide;
pub mod engine;
mod error;
pub mod events;
pub mod factsheet;
mod gate;
mod history;
pub mod inflight;
pub mod intent;
mod io;
mod keys;
pub mod lexicon;
mod lint;
mod materialize;
mod paths;
pub mod proposal_show;
mod query;
mod read_state;
pub mod related;
pub mod search;
mod session;
pub mod suggest;
pub mod testkit;
mod tree_io;
mod version;
pub mod view_api;
pub mod view_query;
pub mod views;
pub mod wc;
mod write;
mod yaml_pos;

pub use areas::by_area;
pub use brief::{
    BriefRegistry, Briefing, Coverage, E_BRIEF_REQUIRED, E_BRIEF_STALE, W_BRIEF_STALE,
};
pub use decide::DecisionInput;
pub use error::*;
pub use events::{Event, EventKind, Events};
pub use gate::{
    LandInfo, LandOptions, LandingRow, ProposalInfo, ProposalRef, RebasedInfo, RelayedByRow,
};
pub use history::{Author, HISTORY_MAX, History, HistoryEntry, JustificationInfo, LandingRef};
pub use io::{ExportSummary, block_from_yaml, block_to_yaml, refuse_inflight_export};
pub use lint::check_path;
pub use materialize::{Materialized, PathMiss};
pub use paths::TreeNode;
pub use proposal_show::{
    ProposalChangeRow, ProposalShow, ProposalVersionRow, is_proposal_ref, proposal_text,
};
pub use query::{
    Context, ImpactItem, ImpactVia, code_lines, compact_summary, field_json, summary, summary_in,
};
pub use rdy_store::Relay;
pub use read_state::{CoordHeads, Delta, InflightSet, OpenIntent, ReadState};
pub use search::{SearchQuery, SearchResult};
pub use session::{
    HUMAN_RUN, INFLIGHT_SCOPE, PinnedClock, Scope, Session, SessionKind, classify, human_actor,
    human_actor_for, human_actor_for_env, now_ms, pin_clock, rfc3339, rfc3339_now,
};
pub use tree_io::Diagnostic;
pub use version::{
    AS_OF_CACHE_ENTRIES, VersionInfo, VersionPosition, VersionRef, ViewPosition, parse_version,
};
pub use view_query::{ChangeSummary, Compare, CompareEntry, OpSummary, ViewDetail, ViewSummary};
pub use views::{ConflictInfo, ConflictSide, OwnerInfo, ViewHeadInfo, ViewOpResult};
pub use wc::project::Projection;
pub use write::{
    LinkInput, OpInput, WriteRequest, WriteResult, editorial_class, json_to_field,
    parse_justification,
};

pub use engine::{Engine, Local, WriteFn};
pub use rdy_store::OpenReport;

use arc_swap::ArcSwap;
use rdy_schema::{AreaMatcher, Pack, PackSet};
use rdy_store::segment::Location;
use rdy_store::{Store, kinds, layout};
use read_state::{AsOfCache, Entry, InflightCell};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize)]
struct Config {
    pack: String,
    /// The project's name as a reader of its site sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// One sentence about the project, shown on the site's home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// How proposals are reviewed (FW-HB1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reviews: Option<ReviewsConfig>,
    /// How views are listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    views: Option<ViewsConfig>,
    /// Where decisions are recorded (FW-PR2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decisions: Option<DecisionsConfig>,
    #[serde(default, skip_serializing_if = "ServeSection::is_empty")]
    serve: ServeSection,
}

/// The `serve:` section of `.rdy/config.yaml`, in seconds.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ServeSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idle_timeout: Option<u64>,
}

impl ServeSection {
    fn is_empty(&self) -> bool {
        self.start_timeout.is_none() && self.idle_timeout.is_none()
    }

    fn resolve(&self) -> ServeConfig {
        ServeConfig {
            start_timeout: self
                .start_timeout
                .map_or(ServeConfig::DEFAULT.start_timeout, Duration::from_secs),
            idle_timeout: self
                .idle_timeout
                .map_or(ServeConfig::DEFAULT.idle_timeout, Duration::from_secs),
        }
    }
}

/// How long a client waits for a daemon to start and how long an idle daemon stays up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServeConfig {
    pub start_timeout: Duration,
    pub idle_timeout: Duration,
}

impl ServeConfig {
    pub const DEFAULT: ServeConfig = ServeConfig {
        start_timeout: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(600),
    };
}

/// The project's `serve:` settings, read without opening the store. A missing file, a missing
/// section or an unreadable value gives the default for what is missing and never an error: a
/// client must be able to start a daemon on a project whose config is broken, and the daemon
/// then reports the config error.
pub fn serve_config(project: &Path) -> ServeConfig {
    let merged =
        config_levels::effective_lenient(project, config_levels::user_file_from_env().as_deref())
            .merged;
    let section = merged.get("serve");
    let secs = |key: &str| {
        section
            .and_then(|s| s.get(key))
            .and_then(serde_yaml_ng::Value::as_u64)
    };
    ServeSection {
        start_timeout: secs("start_timeout"),
        idle_timeout: secs("idle_timeout"),
    }
    .resolve()
}

/// What one `stat` of a file says, for telling whether its bytes may have changed: length,
/// modification time, inode number and status-change time (`ctime`), all at full precision.
/// `ctime` is set by the kernel on every write, rename into place and `utimes`, and no tool can
/// set it back or ahead, so it is what "changed since" is judged by. On a non-unix target there
/// is no `ino` or `ctime`: `ino` is 0 and `ctime` is the mtime (a tool can set it: the weaker
/// rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub mtime: std::time::SystemTime,
    pub ino: u64,
    pub ctime: std::time::SystemTime,
}

impl FileStamp {
    pub fn of(meta: &std::fs::Metadata) -> FileStamp {
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        #[cfg(unix)]
        let (ino, ctime) = {
            use std::os::unix::fs::MetadataExt;
            let ctime = u64::try_from(meta.ctime()).map_or(std::time::UNIX_EPOCH, |s| {
                std::time::UNIX_EPOCH + std::time::Duration::new(s, meta.ctime_nsec() as u32)
            });
            (meta.ino(), ctime)
        };
        #[cfg(not(unix))]
        let (ino, ctime) = (0, mtime);
        FileStamp {
            len: meta.len(),
            mtime,
            ino,
            ctime,
        }
    }
}

/// What the pack-defining files looked like at one moment: `.rdy/config.yaml`, then
/// `.rdy/local.yaml` when it exists, and every `.rdy/packs/*.yaml` (and `.yml`), sorted by path, each with the [`FileStamp`] taken just before
/// its bytes were read; `hash` is BLAKE3 over each file's path bytes, a NUL, its contents and a
/// NUL, in that order. Equal hashes mean equal bytes whatever the stamps say. `at` is this
/// machine's wall clock read before the first stat: a file whose `ctime` is not safely before
/// `at` can change again without a new stamp (it is "racily clean").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFingerprint {
    pub files: Vec<(PathBuf, FileStamp)>,
    pub hash: rdy_model::Hash,
    pub at: std::time::SystemTime,
}

pub fn pack_fingerprint(project: &Path) -> Result<PackFingerprint, RdyError> {
    read_sources(project).map(|s| s.fingerprint)
}

static SOURCE_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test hook: how many times this process has read the pack-defining files (open, reload and
/// the daemon's watch each read them through one function), whatever the project.
#[doc(hidden)]
pub fn pack_source_reads() -> usize {
    SOURCE_READS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The pack-defining files read once: their fingerprint and the very bytes it hashed, so what is
/// loaded is exactly what is fingerprinted (an edit landing between two reads cannot split them).
struct Sources {
    fingerprint: PackFingerprint,
    config: Vec<u8>,
    /// `.rdy/local.yaml`'s bytes when it exists.
    local: Option<Vec<u8>>,
    /// Each `.rdy/packs` file and its bytes, in fingerprint order.
    packs: Vec<(PathBuf, Vec<u8>)>,
}

/// Reads `.rdy/config.yaml`, `.rdy/local.yaml` when it exists, and the pack files, each stat'ed just before its bytes are read.
/// No config file is `E_NO_PROJECT`; any other failure is an I/O error.
fn read_sources(project: &Path) -> Result<Sources, RdyError> {
    SOURCE_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let at = std::time::SystemTime::now();
    let cfg_path = project.join(config_levels::PROJECT_FILE);
    if !cfg_path.exists() {
        return Err(no_project(project));
    }
    let mut paths = vec![cfg_path];
    let local = project.join(config_levels::LOCAL_FILE);
    let has_local = local.exists();
    if has_local {
        paths.push(local);
    }
    paths.extend(pack_files(&project.join(".rdy/packs"))?);
    let mut files = Vec::with_capacity(paths.len());
    let mut contents = Vec::with_capacity(paths.len());
    let mut buf = Vec::new();
    for path in paths {
        let meta = std::fs::metadata(&path).map_err(io)?;
        let bytes = std::fs::read(&path).map_err(io)?;
        buf.extend_from_slice(path.as_os_str().as_encoded_bytes());
        buf.push(0);
        buf.extend_from_slice(&bytes);
        buf.push(0);
        files.push((path.clone(), FileStamp::of(&meta)));
        contents.push((path, bytes));
    }
    let mut contents = contents.into_iter();
    let (_, config) = contents.next().expect("the config is always first");
    let local = if has_local {
        contents.next().map(|(_, b)| b)
    } else {
        None
    };
    Ok(Sources {
        fingerprint: PackFingerprint {
            files,
            hash: rdy_model::Hash::of(&buf),
            at,
        },
        config,
        local,
        packs: contents.collect(),
    })
}

fn no_project(root: &Path) -> RdyError {
    RdyError::new(
        E_NO_PROJECT,
        format!("{} is not an rdy project", root.display()),
        "run rdy init in the project directory",
    )
}

/// What `Rdy::reload_pack` did.
#[derive(Debug)]
pub enum ReloadOutcome {
    /// The pack files and the config are byte-for-byte what is loaded.
    Unchanged,
    /// The config and pack were re-read and the state republished with the new pack.
    Reloaded,
    /// The files changed but do not resolve; the loaded pack stays. `fingerprint` is the files
    /// as this reload read them, taken before their bytes were evaluated: recording it as seen
    /// never marks newer, unevaluated bytes as seen.
    Invalid {
        /// Boxed: a `RdyError` with its optional payloads outweighs the other variants.
        error: Box<RdyError>,
        fingerprint: PackFingerprint,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ViewsConfig {
    /// Days without a write after which `rdy view ls` flags an open view as idle.
    idle_days: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DecisionsConfig {
    /// The doc (path, key or id) a decision given no parent is recorded in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<String>,
}

/// `views.idle_days` when the config does not set it.
const DEFAULT_IDLE_DAYS: u32 = 14;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ReviewsConfig {
    /// A proposal lands only on the canon head it was proposed on.
    #[serde(default)]
    require_up_to_date: bool,
}

impl Config {
    fn require_up_to_date(cfg: Option<&Config>) -> bool {
        cfg.and_then(|c| c.reviews.as_ref())
            .is_some_and(|r| r.require_up_to_date)
    }

    fn idle_days(cfg: Option<&Config>) -> u32 {
        cfg.and_then(|c| c.views.as_ref())
            .map_or(DEFAULT_IDLE_DAYS, |v| v.idle_days)
    }

    /// `decisions.home`, trimmed; an empty text counts as unset.
    fn decisions_home(cfg: Option<&Config>) -> Option<Arc<str>> {
        cfg.and_then(|c| c.decisions.as_ref())
            .and_then(|d| d.home.as_deref())
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(Arc::from)
    }
}

pub struct Rdy {
    root: PathBuf,
    /// The published read state (snap1). Declared before `store`, with the shared caches, so a
    /// dropped `Rdy` releases its state's reader before the store closes.
    state: ArcSwap<ReadState>,
    /// The as-of entries, shared with every published state.
    as_of: Arc<AsOfCache>,
    /// version_position results, keyed by scope and valid for one (canon head, tag count);
    /// shared with every published state.
    vpos: Arc<Mutex<version::VposCache>>,
    /// The blocks each proposal version writes; shared with every published state.
    proposal_touched: inflight::ProposalTouched,
    /// The in-process event bus (evt1); `publish` feeds it.
    events: Events,
    /// The end of the last batch whose events were published to `events` (or whose events could
    /// not be read: see `emit_error`).
    last_emitted: Location,
    /// The error of the latest batch whose events could not be read, so were never emitted live.
    emit_error: Option<RdyError>,
    store: Store,
    pack: Arc<Pack>,
    /// `pack`'s areas, compiled once (arn1).
    areas: Arc<AreaMatcher>,
    /// The project's pack followed by every pack it extends, nearest first.
    lineage: Vec<String>,
    /// The pack files as they were when `pack` was last loaded.
    fingerprint: PackFingerprint,
    serve: ServeConfig,
    /// The registered briefings (in memory; one registry per open project).
    briefs: Arc<brief::BriefRegistry>,
}

fn io(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check file permissions and disk space")
}

/// The named pack as the project at `project` resolves it (its .rdy/packs plus the built-ins).
pub fn resolve_pack(project: &Path, name: &str) -> Result<Pack, RdyError> {
    Ok(load_pack(project, name)?.0)
}

/// The pack the project at `project` names in `.rdy/config.yaml`, resolved without opening its
/// store (so it works while another process holds the store). A missing or unreadable config is
/// the same error `Rdy::open` gives.
pub fn project_pack(project: &Path) -> Result<Pack, RdyError> {
    let cfg = read_config(project)?;
    resolve_pack(project, &cfg.pack)
}

/// Lints the working tree under `root` against `view` (the bound view's materialization): the
/// tree read with the project's `.rdyignore`, then `lint_tree` with references resolved in the
/// view, then one `W_WC_UNTRACKED` warning for each file the scan does not read as a doc file.
/// With `path` (absolute, or relative to `root`), only the diagnostics in files under it.
pub fn check_wc(
    pack: &Pack,
    view: &Materialized,
    root: &Path,
    path: Option<&Path>,
) -> Result<Vec<Diagnostic>, RdyError> {
    use wc::layout::read_tree;
    let ignore = wc::ignore::Ignore::load(root)?;
    let mut tree = read_tree(root, &ignore, pack);
    let mut diags = wc::lint::lint_tree(&mut tree, pack, Some(view), Some(root));
    for u in &tree.untracked {
        diags.push(Diagnostic {
            file: u.path.clone(),
            line: 1,
            column: 1,
            code: W_WC_UNTRACKED.into(),
            message: format!("{}: {}", u.path, u.reason),
            fix: format!("rdy check {}", u.path),
            severity: rdy_schema::Severity::Warning,
        });
    }
    if let Some(under) = path.map(|p| relative_to(root, p)).transpose()?.flatten() {
        let prefix = format!("{under}/");
        diags.retain(|d| d.file == under || d.file.starts_with(&prefix));
    }
    diags.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
    Ok(diags)
}

/// `path` with `.` and `..` resolved by the words alone. `None` for a relative path whose `..`
/// climbs above its start. On an absolute path, `..` at the filesystem root stays there.
pub fn lexical_path(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let climbs = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if climbs {
                    out.pop();
                } else if !out.has_root() {
                    return None;
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    Some(out)
}

/// `path` as a `/`-separated path under `root`; `Ok(None)` for the root itself. `..` is resolved
/// first (`Path::starts_with` compares components, so `/root/../x` would start with `/root`); a
/// path that is not under `root` is `E_BAD_INPUT`, never a path read as if it were.
fn relative_to(root: &Path, path: &Path) -> Result<Option<String>, RdyError> {
    use std::path::Component;
    let outside = || {
        RdyError::new(
            E_BAD_INPUT,
            format!("{} is not under {}", path.display(), root.display()),
            "pass a path inside the working tree",
        )
    };
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let rel = if path.is_absolute() {
        let flat = lexical_path(path).ok_or_else(outside)?;
        let (root_flat, root_canon) = (
            lexical_path(root).unwrap_or_else(|| root.to_path_buf()),
            canon(root),
        );
        let real = canon(&flat);
        flat.strip_prefix(&root_flat)
            .or_else(|_| flat.strip_prefix(&root_canon))
            .or_else(|_| real.strip_prefix(&root_canon))
            .map(Path::to_path_buf)
            .map_err(|_| outside())?
    } else {
        lexical_path(path).ok_or_else(outside)?
    };
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// The `.yaml` and `.yml` files of a packs directory, sorted; none if it does not exist.
fn pack_files(dir: &Path) -> Result<Vec<PathBuf>, RdyError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(io)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    files.sort();
    files.retain(|f| f.extension().is_some_and(|x| x == "yaml" || x == "yml"));
    Ok(files)
}

fn read_config(root: &Path) -> Result<Config, RdyError> {
    let effective = config_levels::effective(root, config_levels::user_file_from_env().as_deref())?;
    config_of(effective)
}

/// The config from the bytes `read_sources` read (the project and local files; the user config
/// is read from the environment), so what is loaded is what the fingerprint hashed.
fn parse_config(root: &Path, config: &[u8], local: Option<&[u8]>) -> Result<Config, RdyError> {
    let effective = config_levels::effective_from(
        root,
        config_levels::user_file_from_env().as_deref(),
        config_levels::GivenFiles {
            project: config,
            local,
        },
    )?;
    config_of(effective)
}

fn config_of(effective: config_levels::Effective) -> Result<Config, RdyError> {
    serde_yaml_ng::from_value(serde_yaml_ng::Value::Mapping(effective.merged)).map_err(|e| {
        RdyError::new(
            E_CONFIG,
            e.to_string(),
            format!("fix {}", config_levels::PROJECT_FILE),
        )
    })
}

fn load_pack(root: &Path, name: &str) -> Result<(Pack, Vec<String>), RdyError> {
    let mut packs = vec![];
    for f in pack_files(&root.join(".rdy/packs"))? {
        let bytes = std::fs::read(&f).map_err(io)?;
        packs.push((f, bytes));
    }
    load_pack_from(&packs, name)
}

/// The named pack resolved from `packs` (each `.rdy/packs` file and its bytes) plus the
/// built-ins, and its lineage.
fn load_pack_from(
    packs: &[(PathBuf, Vec<u8>)],
    name: &str,
) -> Result<(Pack, Vec<String>), RdyError> {
    let mut set = PackSet::with_builtin();
    for (f, bytes) in packs {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        set.add_yaml_from(text, &f.display().to_string())
            .map_err(|e| match e {
                rdy_schema::SchemaError::Duplicate { .. } => RdyError::new(
                    E_CONFIG,
                    e.to_string(),
                    "give each pack its own name; to change a pack, write a differently named pack that extends it",
                ),
                e => RdyError::new(E_CONFIG, format!("{}: {e}", f.display()), "fix the pack file"),
            })?;
    }
    // pkfix1: only an unknown pack name is fixed in the config; a pack that exists but does not
    // resolve (an invalid area, a cycle) is fixed in its file.
    let config = |e: rdy_schema::SchemaError| match e {
        rdy_schema::SchemaError::UnknownPack(_) => RdyError::new(
            E_CONFIG,
            e.to_string(),
            "set `pack:` in .rdy/config.yaml to an available pack (rdy-standard is built in)",
        ),
        e => RdyError::new(E_CONFIG, e.to_string(), "fix the pack file"),
    };
    let pack = set.resolve(name).map_err(config)?;
    let lineage = set.lineage(name).map_err(config)?;
    Ok((pack, lineage))
}

impl Rdy {
    pub fn init(project: &Path, pack: &str) -> Result<Rdy, RdyError> {
        let dot = project.join(".rdy");
        if dot.join("config.yaml").exists() {
            return Err(RdyError::new(
                E_ALREADY_EXISTS,
                format!("{} is already an rdy project", project.display()),
                "open the existing project, or init a different directory",
            ));
        }
        // dur1: .rdy and .rdy/packs are fsynced into their parents (the store adds .rdy/store)
        rdy_store::fsutil::create_dir_durable(&dot.join("packs")).map_err(io)?;
        load_pack(project, pack)?;
        // the store first: a directory there that is not empty and not a current-format store is
        // refused before the config makes this a project (fmt3)
        layout::create_project_store(&dot)?.close()?;
        let cfg = serde_yaml_ng::to_string(&Config {
            pack: pack.to_string(),
            name: None,
            description: None,
            reviews: None,
            views: None,
            decisions: None,
            serve: ServeSection::default(),
        })
        .expect("config yaml");
        rdy_store::fsutil::write_durable(&dot.join("config.yaml"), cfg.as_bytes()).map_err(io)?;
        Rdy::open(project)
    }

    pub fn open(project: &Path) -> Result<Rdy, RdyError> {
        // each file read once: the config and pack are loaded from the bytes the fingerprint
        // hashed, so the watch's fingerprint is exactly what is loaded. The config's own error
        // comes first (not a project, a bad config); an unreadable pack file fails the open
        // before the store is locked.
        let sources = match read_sources(project) {
            Ok(s) => s,
            Err(e) => {
                read_config(project)?;
                return Err(e);
            }
        };
        let cfg = parse_config(project, &sources.config, sources.local.as_deref())?;
        let fingerprint = sources.fingerprint;
        let (pack, lineage) = load_pack_from(&sources.packs, &cfg.pack)?;
        let mut store = layout::open_project_store(&project.join(".rdy"))?;
        // trk1: the freeze checks and every content hash use the pack's tracking fields
        store.set_tracking(pack.tracking());
        let areas = Arc::new(pack.area_matcher());
        let pack = Arc::new(pack);
        let as_of = Arc::new(AsOfCache::default());
        let vpos: Arc<Mutex<version::VposCache>> = Default::default();
        let proposal_touched = inflight::ProposalTouched::default();
        let (name, description) = site_identity(project, Some(&cfg));
        let require_up_to_date = Config::require_up_to_date(Some(&cfg));
        let idle_days = Config::idle_days(Some(&cfg));
        let decisions_home = Config::decisions_home(Some(&cfg));
        // a placeholder until the first publish below, which replaces all of it
        let state = ArcSwap::from_pointee(ReadState {
            seq: store.log_end(),
            canon_head: None,
            canon: Arc::new(Entry::new(None, None, None, Arc::new(vec![]))),
            views: imbl::HashMap::new(),
            tags: Arc::new(vec![]),
            pack: Arc::clone(&pack),
            areas: Arc::clone(&areas),
            root: Arc::from(project),
            name,
            description,
            require_up_to_date,
            idle_days,
            decisions_home,
            reader: store.reader(),
            as_of: Arc::clone(&as_of),
            vpos: Arc::clone(&vpos),
            proposal_touched: Arc::clone(&proposal_touched),
            inflight_cell: Arc::default(),
            coord_heads: Arc::default(),
            coord: OnceLock::new(),
            error: None,
        });
        let mut r = Rdy {
            root: project.to_path_buf(),
            state,
            as_of,
            vpos,
            proposal_touched,
            events: Events::new(),
            last_emitted: store.log_end(),
            emit_error: None,
            store,
            pack,
            areas,
            lineage,
            fingerprint,
            serve: cfg.serve.resolve(),
            briefs: Arc::new(brief::BriefRegistry::new(brief::BRIEF_REGISTRY_CAP)),
        };
        r.invalidate();
        Ok(r)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn pack(&self) -> &Pack {
        &self.pack
    }
    /// The `serve:` settings the config had when it was last loaded.
    pub fn serve_config(&self) -> ServeConfig {
        self.serve
    }

    /// The project's briefing registry: the same one for as long as this `Rdy` is open.
    pub fn briefs(&self) -> Arc<brief::BriefRegistry> {
        Arc::clone(&self.briefs)
    }

    /// What the last `Store::open` did (a clone of the store's report).
    pub fn open_report(&self) -> OpenReport {
        self.store.open_report().clone()
    }

    /// The pack files as they were when the loaded pack was read: at open and after each
    /// successful reload.
    pub fn pack_fingerprint(&self) -> &PackFingerprint {
        &self.fingerprint
    }

    /// Re-reads the config and packs if their bytes changed since they were loaded. A config or
    /// pack that does not resolve is `Invalid`: the loaded pack, lineage and fingerprint stay, so
    /// a later fix counts as a change and a still-invalid state is reported again. On success the
    /// state is republished with the new pack. `Err` is only an I/O error reading the
    /// fingerprint (or a failed republish).
    pub fn reload_pack(&mut self) -> Result<ReloadOutcome, RdyError> {
        let sources = read_sources(&self.root)?;
        let fp = sources.fingerprint;
        if fp.hash == self.fingerprint.hash {
            return Ok(ReloadOutcome::Unchanged);
        }
        // parsed from the bytes `fp` hashed: what is loaded (or found invalid) is what is seen
        let loaded = parse_config(&self.root, &sources.config, sources.local.as_deref())
            .and_then(|cfg| load_pack_from(&sources.packs, &cfg.pack).map(|(p, l)| (cfg, p, l)));
        let (cfg, pack, lineage) = match loaded {
            Ok(v) => v,
            Err(error) => {
                return Ok(ReloadOutcome::Invalid {
                    error: Box::new(error),
                    fingerprint: fp,
                });
            }
        };
        self.areas = Arc::new(pack.area_matcher());
        self.store.set_tracking(pack.tracking());
        self.pack = Arc::new(pack);
        self.lineage = lineage;
        self.serve = cfg.serve.resolve();
        self.fingerprint = fp;
        // entries built under the old pack must not serve the new one
        self.as_of
            .lru
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self.vpos.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
        self.publish(Delta::Full)?;
        Ok(ReloadOutcome::Reloaded)
    }

    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// Makes the store's index durable up to everything written so far (see `Store::checkpoint`).
    pub fn checkpoint(&mut self) -> Result<(), RdyError> {
        Ok(self.store.checkpoint()?)
    }

    /// Closes the project's store cleanly (see `Store::close`) and returns a failed checkpoint's
    /// error. Dropping an `Rdy` closes it too, but can only report a failure on stderr.
    pub fn close(self) -> Result<(), RdyError> {
        let Rdy { state, store, .. } = self;
        drop(state);
        Ok(store.close()?)
    }

    /// Checkpoints if writes are waiting and the store has been idle long enough; true if it did.
    pub fn checkpoint_if_idle(&mut self) -> Result<bool, RdyError> {
        Ok(self.store.checkpoint_if_idle(std::time::Instant::now())?)
    }

    /// The current published state: what every read reads. Cheap; a held state never changes.
    pub fn read(&self) -> Arc<ReadState> {
        self.state.load_full()
    }

    /// Days without a write after which an open view is idle (`views.idle_days`, default 14).
    pub fn idle_days(&self) -> u32 {
        self.read().idle_days()
    }

    /// Publishes the state after a write (snap1), re-reading what `delta` names and carrying the
    /// rest over from the current state. Every write calls it before returning `Ok`. If reading
    /// fails, the published state holds the error, every read of it returns that error, and the
    /// next publish re-reads everything; the error is returned too.
    ///
    /// Then, after the state swap, it emits the events of the store's last batch if that batch
    /// has not been emitted yet (evt1): a `Full` publish from `invalidate` or `open` emits
    /// nothing new, and a batch that was fully deduplicated has no frames and emits nothing.
    /// A batch whose events cannot be read is not the write's failure (the write is durable): its
    /// error is recorded in `last_emit_error` and the batch counts as emitted, so a later batch
    /// never masks it; a replay over it returns the same error.
    /// Known gap: a write made through `store_mut()` is emitted live only if it is the last batch
    /// before the next publish; an earlier one is only in `replay_events`. M2b routes every
    /// write through the engine.
    pub(crate) fn publish(&mut self, delta: Delta) -> Result<(), RdyError> {
        let prev = self.state.load_full();
        // a state that holds an error has nothing to carry over
        let delta = if prev.error.is_some() {
            Delta::Full
        } else {
            delta
        };
        match self.next_state(&prev, delta) {
            Ok(next) => {
                self.state.store(Arc::new(next));
                self.emit_last_batch();
                Ok(())
            }
            Err(e) => {
                self.state.store(Arc::new(ReadState {
                    seq: self.store.log_end(),
                    canon_head: prev.canon_head,
                    canon: Arc::clone(&prev.canon),
                    views: prev.views.clone(),
                    tags: Arc::clone(&prev.tags),
                    pack: Arc::clone(&self.pack),
                    areas: Arc::clone(&self.areas),
                    root: Arc::clone(&prev.root),
                    name: Arc::clone(&prev.name),
                    description: prev.description.clone(),
                    require_up_to_date: prev.require_up_to_date,
                    idle_days: prev.idle_days,
                    decisions_home: prev.decisions_home.clone(),
                    reader: self.store.reader(),
                    as_of: Arc::clone(&self.as_of),
                    vpos: Arc::clone(&self.vpos),
                    proposal_touched: Arc::clone(&self.proposal_touched),
                    inflight_cell: Arc::clone(&prev.inflight_cell),
                    coord_heads: Arc::clone(&prev.coord_heads),
                    coord: OnceLock::new(),
                    error: Some(e.clone()),
                }));
                Err(e)
            }
        }
    }

    fn emit_last_batch(&mut self) {
        let end = self.store.last_batch_end();
        if end > self.last_emitted {
            match events::events_for_batch(&self.store.reader(), end, self.store.last_batch()) {
                Ok(events) => {
                    for e in events {
                        self.events.publish(Arc::new(e));
                    }
                }
                Err(e) => self.emit_error = Some(e),
            }
            self.last_emitted = end;
        }
    }

    /// A receiver of every event emitted from now on (evt1). Events committed before the
    /// subscription are not sent; `replay_events` covers them.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<Arc<Event>> {
        self.events.subscribe()
    }

    /// The events of every committed batch in the log from `from` to its end, in log order: what
    /// a subscriber would have received had it been listening since `from`. The first batch
    /// whose events cannot be read ends the walk with its error.
    pub fn replay_events(&self, from: Location) -> Result<Vec<Event>, RdyError> {
        let reader = self.store.reader();
        let mut out = Vec::new();
        let mut group: Vec<(u8, rdy_model::Hash)> = Vec::new();
        let mut failure: Option<RdyError> = None;
        let walk =
            reader.for_each_frame_from(from, self.store.log_end(), |_, kind, hash, _, next| {
                if kind == kinds::BATCH_COMMIT {
                    match events::events_for_batch(&reader, next, &group) {
                        Ok(events) => out.extend(events),
                        Err(e) => {
                            failure = Some(e);
                            // stops the walk; the batch's own error is returned below
                            return Err(rdy_store::StoreError::Db("replay stopped".into()));
                        }
                    }
                    group.clear();
                } else {
                    group.push((kind, hash));
                }
                Ok(())
            });
        if let Some(e) = failure {
            return Err(e);
        }
        walk?;
        Ok(out)
    }

    /// The error of the latest batch since open whose events could not be read and so were never
    /// emitted live; later batches that emit do not clear it. A replay over that batch returns
    /// the same error.
    pub fn last_emit_error(&self) -> Option<&RdyError> {
        self.emit_error.as_ref()
    }

    /// Test hook: the end of the last batch whose events were emitted.
    #[doc(hidden)]
    pub fn last_emitted_seq(&self) -> Location {
        self.last_emitted
    }

    fn next_state(&self, prev: &ReadState, delta: Delta) -> Result<ReadState, RdyError> {
        let seq = self.store.log_end();
        let canon_head = self.store.canon_head()?;
        // the config is re-read only by a full publish (open, invalidate, reindex); every other
        // publish carries the previous state's name, description and review rules
        let (name, description, require_up_to_date, idle_days, decisions_home) = match delta {
            Delta::Full => {
                let cfg = read_config_lenient(&prev.root);
                let (name, description) = site_identity(&prev.root, cfg.as_ref());
                (
                    name,
                    description,
                    Config::require_up_to_date(cfg.as_ref()),
                    Config::idle_days(cfg.as_ref()),
                    Config::decisions_home(cfg.as_ref()),
                )
            }
            _ => (
                Arc::clone(&prev.name),
                prev.description.clone(),
                prev.require_up_to_date,
                prev.idle_days,
                prev.decisions_home.clone(),
            ),
        };
        // the coordination refs: re-read by a publish that may have moved them, else carried. A
        // landing moves them only when it closed members of a set or chose an alternative (its
        // record says so; altg1)
        let coord_moved = match &delta {
            Delta::Full | Delta::Coord { .. } => true,
            Delta::Landed {
                landing, closed, ..
            } => !closed.is_empty() || self.store.landing(landing)?.chosen.is_some(),
            _ => false,
        };
        let coord_heads = if coord_moved {
            Arc::new(self.coord_heads(&prev.coord_heads)?)
        } else {
            Arc::clone(&prev.coord_heads)
        };
        let (canon, views, tags, inflight_cell) = match delta {
            Delta::Full => {
                let mut views = imbl::HashMap::new();
                for (name, _) in self.store.refs_with_prefix("view/")? {
                    let name = name.strip_prefix("view/").unwrap_or(&name).to_string();
                    if let Some(e) = self.view_entry(&name)? {
                        views.insert(name, Arc::new(e));
                    }
                }
                let canon = Entry::new(None, canon_head, None, Arc::new(vec![]));
                (
                    Arc::new(canon),
                    views,
                    Arc::new(self.version_tags()?),
                    Arc::default(),
                )
            }
            Delta::Commit { view, change, ops } => {
                let mut views = prev.views.clone();
                match self.view_entry(&view)? {
                    Some(mut next) => {
                        // the build the write applied to: the view's own on the same base; or
                        // canon's, for a view that read canon (no ref yet, or empty) and now
                        // sits on canon's head (the float catch-up may have moved its base).
                        // A build on an old base never seeds the write after a catch-up.
                        let pred = match prev.views.get(&view) {
                            Some(e) if e.base == next.base && e.built_ok().is_some() => {
                                e.built_ok()
                            }
                            Some(e) if e.included.is_empty() && next.base == prev.canon_head => {
                                prev.canon.built_ok()
                            }
                            None if next.base == prev.canon_head => prev.canon.built_ok(),
                            _ => None,
                        };
                        if let Some(b) = pred.filter(|b| b.conflicts.is_empty()) {
                            next.seed(b.after(&ops));
                        }
                        views.insert(view.clone(), Arc::new(next));
                    }
                    None => {
                        views.remove(&view);
                    }
                }
                let inflight =
                    self.inflight_after_commit(prev, &views, &view, change, &ops, canon_head);
                (
                    Arc::clone(&prev.canon),
                    views,
                    Arc::clone(&prev.tags),
                    inflight,
                )
            }
            Delta::Landed {
                landing,
                view,
                ops,
                rebased,
                closed,
            } => {
                let mut canon = Entry::new(None, Some(landing), None, Arc::new(vec![]));
                if let Some(b) = prev.canon.built_ok() {
                    canon.seed(b.after(&ops));
                }
                // the proposing view and every view the landing rebased (it is one of them) start
                // unbuilt, on the landing: a stale entry would seed the next commit into it from
                // the old base
                let mut views = prev.views.clone();
                let mut names = rebased;
                if !names.contains(&view) {
                    names.push(view);
                }
                for name in names {
                    match self.view_entry(&name)? {
                        Some(next) => {
                            views.insert(name, Arc::new(next));
                        }
                        None => {
                            views.remove(&name);
                        }
                    }
                }
                // the members the landing closed (not rebased): their heads re-read, their
                // builds kept
                for name in closed {
                    self.reread_view(prev, &mut views, name)?;
                }
                (
                    Arc::new(canon),
                    views,
                    Arc::clone(&prev.tags),
                    Arc::default(),
                )
            }
            Delta::View { view } => {
                let mut views = prev.views.clone();
                self.reread_view(prev, &mut views, view)?;
                let inflight = self.inflight_after_views(prev, &views, canon_head)?;
                (
                    Arc::clone(&prev.canon),
                    views,
                    Arc::clone(&prev.tags),
                    inflight,
                )
            }
            Delta::Coord { closed } => {
                let mut views = prev.views.clone();
                for view in closed {
                    self.reread_view(prev, &mut views, view)?;
                }
                let inflight = self.inflight_after_views(prev, &views, canon_head)?;
                (
                    Arc::clone(&prev.canon),
                    views,
                    Arc::clone(&prev.tags),
                    inflight,
                )
            }
            Delta::Tags => (
                Arc::clone(&prev.canon),
                prev.views.clone(),
                Arc::new(self.version_tags()?),
                Arc::clone(&prev.inflight_cell),
            ),
            Delta::Seq => (
                Arc::clone(&prev.canon),
                prev.views.clone(),
                Arc::clone(&prev.tags),
                Arc::clone(&prev.inflight_cell),
            ),
        };
        Ok(ReadState {
            seq,
            canon_head,
            canon,
            views,
            tags,
            pack: Arc::clone(&self.pack),
            areas: Arc::clone(&self.areas),
            root: Arc::clone(&prev.root),
            name,
            description,
            require_up_to_date,
            idle_days,
            decisions_home,
            reader: self.store.reader(),
            as_of: Arc::clone(&self.as_of),
            vpos: Arc::clone(&self.vpos),
            proposal_touched: Arc::clone(&self.proposal_touched),
            inflight_cell,
            coord_heads,
            coord: OnceLock::new(),
            error: None,
        })
    }

    /// Re-reads view `view`'s entry into `views` (`Delta::View`, and each view `Delta::Coord`
    /// closed). View ops that left the base and the set as they were (a close, an own) keep the
    /// build; any other change starts unbuilt.
    fn reread_view(
        &self,
        prev: &ReadState,
        views: &mut imbl::HashMap<String, Arc<Entry>>,
        view: String,
    ) -> Result<(), RdyError> {
        match self.view_entry(&view)? {
            Some(mut next) => {
                if let Some(e) = prev.views.get(&view)
                    && e.base == next.base
                    && e.included == next.included
                    && let Some(b) = e.built_ok()
                {
                    next.seed_shared(Arc::clone(b));
                }
                views.insert(view, Arc::new(next));
            }
            None => {
                views.remove(&view);
            }
        }
        Ok(())
    }

    /// The coordination refs as the store holds them now, with the open-set index carried from
    /// `prev` for every set that did not move (`CoordHeads::read`).
    fn coord_heads(&self, prev: &CoordHeads) -> Result<CoordHeads, RdyError> {
        let reader = self.store.reader();
        CoordHeads::read(
            &reader,
            prev,
            reader.intent_heads()?.into_iter().collect(),
            reader.alt_heads()?.into_iter().collect(),
        )
    }

    /// insc1: the in-flight cell after `change` (with `ops`) was committed to `view`. When the
    /// previous set was computed and `view` is open, the set is the previous one plus `change`
    /// (held by `view` alone). The previous build is carried, with `ops` applied, when it is
    /// computed and has no conflicts, every op applies cleanly to its materialization, and the
    /// change depends on the last writer (in the merge's order) of every block it writes: a new
    /// change is depended on by none, so it then merges after every writer of its blocks and a
    /// rebuild equals the carry. Anything else starts unbuilt.
    fn inflight_after_commit(
        &self,
        prev: &ReadState,
        views: &imbl::HashMap<String, Arc<Entry>>,
        view: &str,
        change: rdy_model::Hash,
        ops: &[rdy_model::Op],
        canon_head: Option<rdy_model::Hash>,
    ) -> Arc<InflightCell> {
        let Some(before) = prev.inflight_cell.set_ok() else {
            return Arc::default();
        };
        let open = views
            .get(view)
            .and_then(|e| e.head.as_ref())
            .is_some_and(|h| !h.closed);
        if !open || prev.canon_head != canon_head || before.views_of.contains_key(&change) {
            return Arc::default();
        }
        let mut set = before.as_ref().clone();
        let at = set.changes.partition_point(|c| *c < change);
        set.changes.insert(at, change);
        set.views_of.insert(change, vec![view.to_string()]);
        let cell = InflightCell::with_set(Arc::new(set));
        let carry = match (prev.inflight_cell.built_ok(), prev.inflight_cell.last.get()) {
            (Some(b), Some(last))
                if b.conflicts.is_empty() && read_state::applies_cleanly(&b.m, ops) =>
            {
                self.merges_last(change, ops, last).then_some((b, last))
            }
            _ => None,
        };
        if let Some((b, last)) = carry {
            let set = cell.set_ok().expect("set above");
            let mut entry = read_state::inflight_entry(canon_head, set);
            entry.seed(b.after(ops));
            let _ = cell.entry.set(Ok(Arc::new(entry)));
            let mut last = last.as_ref().clone();
            for op in ops {
                last.insert(op.block_id().clone(), change);
            }
            let _ = cell.last.set(Arc::new(last));
        }
        Arc::new(cell)
    }

    /// Whether `change` depends on the last in-flight writer of every block `ops` write; an
    /// unreadable change never carries.
    fn merges_last(
        &self,
        change: rdy_model::Hash,
        ops: &[rdy_model::Op],
        last: &read_state::LastWriters,
    ) -> bool {
        let writers: Vec<&rdy_model::Hash> = ops
            .iter()
            .filter_map(|op| last.get(op.block_id()))
            .collect();
        if writers.is_empty() {
            return true;
        }
        let Ok(c) = self.store.reader().change(&change) else {
            return false;
        };
        writers.iter().all(|w| c.deps.contains(w))
    }

    /// insc1: the in-flight cell after view ops (`Delta::View`, `Delta::Coord`). When the
    /// previous set was computed, the set is recomputed from `views`: an equal set keeps the
    /// previous cell, the same changes held by other views keep its build, anything else starts
    /// unbuilt. A previous set never computed leaves the new cell to compute on first read.
    fn inflight_after_views(
        &self,
        prev: &ReadState,
        views: &imbl::HashMap<String, Arc<Entry>>,
        canon_head: Option<rdy_model::Hash>,
    ) -> Result<Arc<InflightCell>, RdyError> {
        let Some(before) = prev.inflight_cell.set_ok() else {
            return Ok(Arc::default());
        };
        let set = read_state::inflight_set_of(&self.store.reader(), views)?;
        let same_base = prev.canon_head == canon_head;
        let entry_ok = !matches!(prev.inflight_cell.entry.get(), Some(Err(_)));
        if same_base && entry_ok && **before == set {
            return Ok(Arc::clone(&prev.inflight_cell));
        }
        let same_changes = same_base && before.changes == set.changes;
        let cell = InflightCell::with_set(Arc::new(set));
        if same_changes && let Some(Ok(entry)) = prev.inflight_cell.entry.get() {
            let _ = cell.entry.set(Ok(Arc::clone(entry)));
            // the shared entry may finish building through the previous state, which then
            // records `last` there only: this cell then lacks it and never carries the build
            if let Some(last) = prev.inflight_cell.last.get() {
                let _ = cell.last.set(Arc::clone(last));
            }
        }
        Ok(Arc::new(cell))
    }

    /// An unbuilt entry for view `name` as the store holds it now (head and set read together);
    /// None if the view has no ref.
    fn view_entry(&self, name: &str) -> Result<Option<Entry>, RdyError> {
        let Some(head) = self.store.view_head(name)? else {
            return Ok(None);
        };
        let included = self.store.included_at(name, &head)?;
        Ok(Some(Entry::new(
            Some(name.to_string()),
            head.base,
            Some(head),
            included,
        )))
    }

    /// Republishes a state with every cell empty (`Delta::Full`). Never fails: an error in
    /// reading the heads is kept in the state and returned by its reads. As-of builds are shared
    /// across states and kept. Call it after writing to the store outside the engine.
    pub fn invalidate(&mut self) {
        let _ = self.publish(Delta::Full);
    }

    /// Rebuilds the store's index from its log and republishes the state on the new index.
    pub fn reindex(&mut self) -> Result<(), RdyError> {
        self.store.reindex()?;
        self.publish(Delta::Full)
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FsckReport {
    pub frames: usize,
    pub landings: usize,
    pub versions: usize,
    pub views: usize,
    pub landed_changes: usize,
    /// The intents, each with its version chain checked.
    pub intents: usize,
    /// The alternative sets, each with its version chain checked.
    pub alt_sets: usize,
}

impl Rdy {
    pub fn fsck(&self) -> Result<FsckReport, RdyError> {
        let frames = self.store.verify()?;
        let landings = self.store.landings()?;
        for (_, rec) in &landings {
            self.store.get::<rdy_model::StateSnapshot>(&rec.state)?;
        }
        let chain: Vec<rdy_model::Hash> = landings.iter().map(|(h, _)| *h).collect();
        let versions = self.check_tags(&chain)?;
        let objects = self.store.check_objects()?;
        Ok(FsckReport {
            frames,
            landings: landings.len(),
            versions,
            views: objects.views,
            landed_changes: objects.landed_changes,
            intents: objects.intents,
            alt_sets: objects.alt_sets,
        })
    }
}

/// The config at `root` as it is on disk; any read or parse error reads as no config.
fn read_config_lenient(root: &Path) -> Option<Config> {
    let merged =
        config_levels::effective_lenient(root, config_levels::user_file_from_env().as_deref())
            .merged;
    serde_yaml_ng::from_value(serde_yaml_ng::Value::Mapping(merged)).ok()
}

/// The project's name and description as a site shows them, parsed once when a state is
/// published (a state is a snapshot). The name is the config's `name`, else the project
/// directory's name (`-` and `_` turned into spaces, the first letter upper-cased), else
/// `Design docs`; the description is the config's non-empty `description`.
fn site_identity(root: &Path, cfg: Option<&Config>) -> (Arc<str>, Option<Arc<str>>) {
    let named = cfg
        .and_then(|c| c.name.as_deref())
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let name = match named {
        Some(n) => n.to_string(),
        None => {
            let dir = root
                .file_name()
                .map(|n| n.to_string_lossy().replace(['-', '_'], " "))
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty());
            match dir {
                Some(d) => {
                    let mut chars = d.chars();
                    let first = chars.next().expect("non-empty").to_uppercase();
                    first.chain(chars).collect()
                }
                None => "Design docs".to_string(),
            }
        }
    };
    let description = cfg
        .and_then(|c| c.description.as_deref())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(Arc::from);
    (Arc::from(name), description)
}

impl ReadState {
    /// The name of the project as a site shows it, as parsed when this state was published.
    pub fn project_name(&self) -> String {
        self.name.to_string()
    }

    /// The config's `description`, when it had a non-empty one when this state was published.
    pub fn project_description(&self) -> Option<String> {
        self.description.as_deref().map(str::to_string)
    }
}

#[cfg(test)]
mod relative_to_tests {
    use super::{E_BAD_INPUT, lexical_path, relative_to};
    use std::path::Path;

    /// Fix round 1 (wcck1): `..` is resolved, and a path that leaves the root is an error, not
    /// a path with its `..` dropped.
    #[test]
    fn wcck1_relative_to_resolves_dotdot_and_refuses_a_path_outside() {
        let root = Path::new("/nonexistent-root/tree");
        let rel = |p: &str| relative_to(root, Path::new(p));
        assert_eq!(rel("/nonexistent-root/tree").unwrap(), None);
        assert_eq!(
            rel("/nonexistent-root/tree/a/../b/c.yaml")
                .unwrap()
                .as_deref(),
            Some("b/c.yaml")
        );
        assert_eq!(rel("a/./b.yaml").unwrap().as_deref(), Some("a/b.yaml"));
        for outside in [
            "/nonexistent-root/tree/../elsewhere/x.yaml",
            "/nonexistent-root/elsewhere",
            "../elsewhere",
            "a/../../elsewhere",
        ] {
            let e = rel(outside).unwrap_err();
            assert_eq!(e.code, E_BAD_INPUT, "{outside}");
        }
        assert_eq!(
            lexical_path(Path::new("/a/../..")).as_deref(),
            Some(Path::new("/"))
        );
        assert_eq!(lexical_path(Path::new("../a")), None);
    }
}
