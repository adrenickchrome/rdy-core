pub mod brief;
pub mod code_brief;
pub mod code_index;
pub mod consequence;
mod decide;
pub mod engine;
mod error;
pub mod events;
mod gate;
mod history;
pub mod inflight;
mod io;
mod keys;
mod lint;
mod materialize;
mod paths;
pub mod proposal_show;
mod query;
mod read_state;
pub mod search;
mod session;
pub mod testkit;
mod tree_io;
mod version;
pub mod view_api;
pub mod view_query;
pub mod views;
mod write;
mod yaml_pos;

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
pub use io::{ExportSummary, block_from_yaml, block_to_yaml};
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
pub use read_state::{Delta, ReadState};
pub use search::{SearchQuery, SearchResult};
pub use session::{
    HUMAN_RUN, PinnedClock, Scope, Session, SessionKind, classify, human_actor,
    human_actor_from_env, now_ms, pin_clock, rfc3339, rfc3339_now,
};
pub use tree_io::Diagnostic;
pub use version::{
    AS_OF_CACHE_ENTRIES, VersionInfo, VersionPosition, VersionRef, ViewPosition, parse_version,
};
pub use view_query::{ChangeSummary, Compare, CompareEntry, OpSummary, ViewDetail, ViewSummary};
pub use views::{ConflictInfo, ConflictSide, OwnerInfo, ViewHeadInfo, ViewOpResult};
pub use write::{LinkInput, OpInput, WriteRequest, WriteResult, json_to_field};

pub use engine::{Engine, Local, WriteFn};
pub use rdy_store::OpenReport;

use arc_swap::ArcSwap;
use rdy_schema::{Pack, PackSet};
use rdy_store::segment::Location;
use rdy_store::{Store, kinds, layout};
use read_state::{AsOfCache, Entry};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    let section = std::fs::read_to_string(project.join(".rdy/config.yaml"))
        .ok()
        .and_then(|t| serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&t).ok())
        .and_then(|v| v.get("serve").cloned());
    let secs = |key: &str| {
        section
            .as_ref()
            .and_then(|s| s.get(key))
            .and_then(serde_yaml_ng::Value::as_u64)
    };
    ServeSection {
        start_timeout: secs("start_timeout"),
        idle_timeout: secs("idle_timeout"),
    }
    .resolve()
}

/// What the pack-defining files looked like at one moment: `.rdy/config.yaml` and every
/// `.rdy/packs/*.yaml` (and `.yml`), sorted by path, each as `(path, length, mtime)` stat'ed just
/// before its bytes were read; `hash` is BLAKE3 over each file's path bytes, a NUL, its contents
/// and a NUL, in that order. Equal hashes mean equal bytes whatever the mtimes say. `at` is the
/// wall clock (the clock mtimes come from) read before the first stat: a file whose mtime is not
/// safely before `at` can change again without a new mtime (it is "racily clean").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFingerprint {
    pub files: Vec<(PathBuf, u64, std::time::SystemTime)>,
    pub hash: rdy_model::Hash,
    pub at: std::time::SystemTime,
}

pub fn pack_fingerprint(project: &Path) -> Result<PackFingerprint, RdyError> {
    let at = std::time::SystemTime::now();
    let mut paths = vec![project.join(".rdy/config.yaml")];
    paths.extend(pack_files(&project.join(".rdy/packs"))?);
    let mut files = Vec::with_capacity(paths.len());
    let mut buf = Vec::new();
    for path in paths {
        let meta = std::fs::metadata(&path).map_err(io)?;
        let bytes = std::fs::read(&path).map_err(io)?;
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        buf.extend_from_slice(path.as_os_str().as_encoded_bytes());
        buf.push(0);
        buf.extend_from_slice(&bytes);
        buf.push(0);
        files.push((path, meta.len(), mtime));
    }
    Ok(PackFingerprint {
        files,
        hash: rdy_model::Hash::of(&buf),
        at,
    })
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
    let cfg_path = root.join(".rdy/config.yaml");
    if !cfg_path.exists() {
        return Err(RdyError::new(
            E_NO_PROJECT,
            format!("{} is not an rdy project", root.display()),
            "run rdy init in the project directory",
        ));
    }
    let text = std::fs::read_to_string(&cfg_path).map_err(io)?;
    serde_yaml_ng::from_str(&text)
        .map_err(|e| RdyError::new(E_CONFIG, e.to_string(), "fix .rdy/config.yaml"))
}

fn load_pack(root: &Path, name: &str) -> Result<(Pack, Vec<String>), RdyError> {
    let mut set = PackSet::with_builtin();
    for f in pack_files(&root.join(".rdy/packs"))? {
        let text = std::fs::read_to_string(&f).map_err(io)?;
        set.add_yaml_from(&text, &f.display().to_string())
            .map_err(|e| match e {
                rdy_schema::SchemaError::Duplicate { .. } => RdyError::new(
                    E_CONFIG,
                    e.to_string(),
                    "give each pack its own name; to change a pack, write a differently named pack that extends it",
                ),
                e => RdyError::new(E_CONFIG, format!("{}: {e}", f.display()), "fix the pack file"),
            })?;
    }
    let config = |e: rdy_schema::SchemaError| {
        RdyError::new(
            E_CONFIG,
            e.to_string(),
            "set `pack:` in .rdy/config.yaml to an available pack (rdy-standard is built in)",
        )
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
        // the fingerprint before the config and pack are read: a file edited between the reads
        // is then loaded new and fingerprinted old, so the watch's next `changed()` reloads it
        // (never the other way, which would mark unloaded bytes as seen). The config's own
        // error comes first (not a project, a bad config); an unreadable pack file fails the
        // open before the store is locked.
        let fingerprint = pack_fingerprint(project);
        let cfg = read_config(project)?;
        let fingerprint = fingerprint?;
        let (pack, lineage) = load_pack(project, &cfg.pack)?;
        let store = layout::open_project_store(&project.join(".rdy"))?;
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
        let fp = pack_fingerprint(&self.root)?;
        if fp.hash == self.fingerprint.hash {
            return Ok(ReloadOutcome::Unchanged);
        }
        let loaded = read_config(&self.root)
            .and_then(|cfg| load_pack(&self.root, &cfg.pack).map(|(p, l)| (cfg, p, l)));
        let (cfg, pack, lineage) = match loaded {
            Ok(v) => v,
            Err(error) => {
                return Ok(ReloadOutcome::Invalid {
                    error: Box::new(error),
                    fingerprint: fp,
                });
            }
        };
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
        let (canon, views, tags) = match delta {
            Delta::Full => {
                let mut views = imbl::HashMap::new();
                for (name, _) in self.store.refs_with_prefix("view/")? {
                    let name = name.strip_prefix("view/").unwrap_or(&name).to_string();
                    if let Some(e) = self.view_entry(&name)? {
                        views.insert(name, Arc::new(e));
                    }
                }
                let canon = Entry::new(None, canon_head, None, Arc::new(vec![]));
                (Arc::new(canon), views, Arc::new(self.version_tags()?))
            }
            Delta::Commit { view, ops } => {
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
                        views.insert(view, Arc::new(next));
                    }
                    None => {
                        views.remove(&view);
                    }
                }
                (Arc::clone(&prev.canon), views, Arc::clone(&prev.tags))
            }
            Delta::Landed {
                landing,
                view,
                ops,
                rebased,
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
                (Arc::new(canon), views, Arc::clone(&prev.tags))
            }
            Delta::View { view } => {
                let mut views = prev.views.clone();
                match self.view_entry(&view)? {
                    Some(mut next) => {
                        // view ops that left the base and the set as they were (a close, an
                        // own) keep the build; any other change starts unbuilt
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
                (Arc::clone(&prev.canon), views, Arc::clone(&prev.tags))
            }
            Delta::Tags => (
                Arc::clone(&prev.canon),
                prev.views.clone(),
                Arc::new(self.version_tags()?),
            ),
            Delta::Seq => (
                Arc::clone(&prev.canon),
                prev.views.clone(),
                Arc::clone(&prev.tags),
            ),
        };
        Ok(ReadState {
            seq,
            canon_head,
            canon,
            views,
            tags,
            pack: Arc::clone(&self.pack),
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
            error: None,
        })
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
        })
    }
}

/// The config at `root` as it is on disk; any read or parse error reads as no config.
fn read_config_lenient(root: &Path) -> Option<Config> {
    let text = std::fs::read_to_string(root.join(".rdy/config.yaml")).ok()?;
    serde_yaml_ng::from_str(&text).ok()
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
