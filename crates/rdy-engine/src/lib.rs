mod decide;
mod error;
pub mod events;
mod gate;
mod io;
mod lint;
mod materialize;
mod paths;
mod query;
mod read_state;
mod session;
pub mod testkit;
mod tree_io;
mod version;
mod write;
mod yaml_pos;

pub use decide::DecisionInput;
pub use error::*;
pub use events::{Event, EventKind, Events};
pub use gate::{LandInfo, ProposalInfo};
pub use io::{ExportSummary, block_from_yaml, block_to_yaml};
pub use lint::check_path;
pub use materialize::{Materialized, PathMiss};
pub use paths::TreeNode;
pub use query::{Context, ImpactItem, ImpactVia, compact_summary, field_json, summary, summary_in};
pub use read_state::{Delta, ReadState};
pub use session::{Scope, Session, now_ms, rfc3339_now};
pub use tree_io::Diagnostic;
pub use version::{
    AS_OF_CACHE_ENTRIES, VersionInfo, VersionPosition, VersionRef, ViewPosition, parse_version,
};
pub use write::{LinkInput, OpInput, WriteRequest, WriteResult, json_to_field};

use arc_swap::ArcSwap;
use rdy_schema::{Pack, PackSet};
use rdy_store::{Store, layout};
use read_state::{AsOfCache, Entry};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(serde::Serialize, serde::Deserialize)]
struct Config {
    pack: String,
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
    store: Store,
    pack: Arc<Pack>,
    /// The project's pack followed by every pack it extends, nearest first.
    lineage: Vec<String>,
}

fn io(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check file permissions and disk space")
}

/// The named pack as the project at `project` resolves it (its .rdy/packs plus the built-ins).
pub fn resolve_pack(project: &Path, name: &str) -> Result<Pack, RdyError> {
    Ok(load_pack(project, name)?.0)
}

fn load_pack(root: &Path, name: &str) -> Result<(Pack, Vec<String>), RdyError> {
    let mut set = PackSet::with_builtin();
    let dir = root.join(".rdy/packs");
    if dir.exists() {
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map_err(io)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        files.sort();
        for f in files
            .into_iter()
            .filter(|f| f.extension().is_some_and(|x| x == "yaml" || x == "yml"))
        {
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
        let cfg = serde_yaml_ng::to_string(&Config {
            pack: pack.to_string(),
        })
        .expect("config yaml");
        rdy_store::fsutil::write_durable(&dot.join("config.yaml"), cfg.as_bytes()).map_err(io)?;
        Rdy::open(project)
    }

    pub fn open(project: &Path) -> Result<Rdy, RdyError> {
        let cfg_path = project.join(".rdy/config.yaml");
        if !cfg_path.exists() {
            return Err(RdyError::new(
                E_NO_PROJECT,
                format!("{} is not an rdy project", project.display()),
                "run rdy init in the project directory",
            ));
        }
        let text = std::fs::read_to_string(&cfg_path).map_err(io)?;
        let cfg: Config = serde_yaml_ng::from_str(&text)
            .map_err(|e| RdyError::new(E_CONFIG, e.to_string(), "fix .rdy/config.yaml"))?;
        let (pack, lineage) = load_pack(project, &cfg.pack)?;
        let store = layout::open_project_store(&project.join(".rdy"))?;
        let pack = Arc::new(pack);
        let as_of = Arc::new(AsOfCache::default());
        let vpos: Arc<Mutex<version::VposCache>> = Default::default();
        // a placeholder until the first publish below, which replaces all of it
        let state = ArcSwap::from_pointee(ReadState {
            seq: store.log_end(),
            canon_head: None,
            canon: Arc::new(Entry::new(None, None, None, Arc::new(vec![]))),
            views: imbl::HashMap::new(),
            tags: Arc::new(vec![]),
            pack: Arc::clone(&pack),
            reader: store.reader(),
            as_of: Arc::clone(&as_of),
            vpos: Arc::clone(&vpos),
            error: None,
        });
        let mut r = Rdy {
            root: project.to_path_buf(),
            state,
            as_of,
            vpos,
            store,
            pack,
            lineage,
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

    /// Publishes the state after a write (snap1), re-reading what `delta` names and carrying the
    /// rest over from the current state. Every write calls it before returning `Ok`. If reading
    /// fails, the published state holds the error, every read of it returns that error, and the
    /// next publish re-reads everything; the error is returned too.
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
                    reader: self.store.reader(),
                    as_of: Arc::clone(&self.as_of),
                    vpos: Arc::clone(&self.vpos),
                    error: Some(e.clone()),
                }));
                Err(e)
            }
        }
    }

    fn next_state(&self, prev: &ReadState, delta: Delta) -> Result<ReadState, RdyError> {
        let seq = self.store.log_end();
        let canon_head = self.store.canon_head()?;
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
                        // the entry the write read: the view's own, or canon's for a view with
                        // no ref yet (it floated on canon)
                        let pred = match prev.views.get(&view) {
                            Some(e) => e.built_ok(),
                            None if next.base == prev.canon_head => prev.canon.built_ok(),
                            None => None,
                        };
                        if let Some(b) = pred.filter(|b| b.conflicts.is_empty()) {
                            next.seed(read_state::Built::new(
                                Arc::new(b.m.with(&ops)),
                                Arc::clone(&b.conflicts),
                            ));
                        }
                        views.insert(view, Arc::new(next));
                    }
                    None => {
                        views.remove(&view);
                    }
                }
                (Arc::clone(&prev.canon), views, Arc::clone(&prev.tags))
            }
            Delta::Landed { landing, view, ops } => {
                let mut canon = Entry::new(None, Some(landing), None, Arc::new(vec![]));
                if let Some(b) = prev.canon.built_ok() {
                    canon.seed(read_state::Built::new(
                        Arc::new(b.m.with(&ops)),
                        Arc::new(vec![]),
                    ));
                }
                let mut views = prev.views.clone();
                match self.view_entry(&view)? {
                    Some(next) => {
                        views.insert(view, Arc::new(next));
                    }
                    None => {
                        views.remove(&view);
                    }
                }
                (Arc::new(canon), views, Arc::clone(&prev.tags))
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
            reader: self.store.reader(),
            as_of: Arc::clone(&self.as_of),
            vpos: Arc::clone(&self.vpos),
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

    /// Forwarder to `ReadState::materialized` (removed once every caller reads the state).
    pub fn materialized(&self, scope: &Scope) -> Result<Arc<Materialized>, RdyError> {
        self.read().materialized(scope)
    }

    /// Forwarder to `ReadState::graph` (removed once every caller reads the state).
    pub fn graph(&self, scope: &Scope) -> Result<Arc<rdy_graph::Graph>, RdyError> {
        self.read().graph(scope)
    }

    /// Test hook: whether `scope`'s materialization and graph are built in the current state.
    #[doc(hidden)]
    pub fn is_cached(&self, scope: &Scope) -> (bool, bool) {
        self.read().is_built(scope)
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
