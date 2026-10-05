mod decide;
mod error;
mod gate;
mod io;
mod keys;
mod lint;
mod materialize;
mod paths;
mod query;
mod session;
pub mod testkit;
mod tree_io;
mod version;
mod write;
mod yaml_pos;

pub use decide::DecisionInput;
pub use error::*;
pub use gate::{LandInfo, ProposalInfo};
pub use io::{ExportSummary, block_from_yaml, block_to_yaml};
pub use lint::check_path;
pub use materialize::{Materialized, PathMiss};
pub use paths::TreeNode;
pub use query::{Context, ImpactItem, ImpactVia, compact_summary, field_json, summary, summary_in};
pub use session::{Scope, Session, now_ms, rfc3339_now};
pub use tree_io::Diagnostic;
pub use version::{
    AS_OF_CACHE_ENTRIES, VersionInfo, VersionPosition, VersionRef, ViewPosition, parse_version,
};
pub use write::{LinkInput, OpInput, WriteRequest, WriteResult, json_to_field};

use rdy_model::Op;
use rdy_schema::{Pack, PackSet};
use rdy_store::{Store, layout};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[derive(serde::Serialize, serde::Deserialize)]
struct Config {
    pack: String,
    /// The project's name as a reader of its site sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// One sentence about the project, shown on the site's home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

pub struct Rdy {
    root: PathBuf,
    store: Store,
    pack: Pack,
    /// The project's pack followed by every pack it extends, nearest first.
    lineage: Vec<String>,
    cache: HashMap<Scope, Materialized>,
    graphs: HashMap<Scope, rdy_graph::Graph>,
    /// As-of scopes in least-recently-used order (front = oldest); at most AS_OF_CACHE_ENTRIES.
    as_of_lru: VecDeque<rdy_model::Hash>,
    /// version_position results, keyed by scope and valid for one (canon head, tag count).
    vpos: std::sync::Mutex<version::VposCache>,
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
            name: None,
            description: None,
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
        Ok(Rdy {
            root: project.to_path_buf(),
            store,
            pack,
            lineage,
            cache: HashMap::new(),
            graphs: HashMap::new(),
            as_of_lru: VecDeque::new(),
            vpos: Default::default(),
        })
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
        Ok(self.store.close()?)
    }

    /// Checkpoints if writes are waiting and the store has been idle long enough; true if it did.
    pub fn checkpoint_if_idle(&mut self) -> Result<bool, RdyError> {
        Ok(self.store.checkpoint_if_idle(std::time::Instant::now())?)
    }

    pub fn materialized(&mut self, scope: &Scope) -> Result<&Materialized, RdyError> {
        if !self.cache.contains_key(scope) {
            let snapshot = match scope {
                Scope::Canon => self.store.canon_state()?,
                Scope::View(v) => self.store.view_state(v)?,
                Scope::At(h) => {
                    let not_landing = || {
                        RdyError::new(
                            E_NOT_FOUND,
                            format!("{} is not a landing", h.short()),
                            "read as of a version (tag:<version>) or a landing in rdy log (landing:<id>)",
                        )
                    };
                    if !self.store.has_kind(rdy_store::kinds::LANDING, h)? {
                        return Err(not_landing());
                    }
                    self.store.snapshot_at(h)?
                }
            };
            let blocks = self.store.blocks(&snapshot)?;
            self.cache
                .insert(scope.clone(), Materialized::build(snapshot, blocks));
        }
        if let Scope::At(h) = scope {
            self.touch_as_of(*h);
        }
        Ok(&self.cache[scope])
    }

    /// Marks an as-of scope as used, evicting the least recently used beyond the cap (with its graph).
    fn touch_as_of(&mut self, h: rdy_model::Hash) {
        self.as_of_lru.retain(|x| *x != h);
        self.as_of_lru.push_back(h);
        while self.as_of_lru.len() > AS_OF_CACHE_ENTRIES {
            let old = self.as_of_lru.pop_front().expect("over the cap");
            self.cache.remove(&Scope::At(old));
            self.graphs.remove(&Scope::At(old));
        }
    }

    /// Drops cached canon and view state. As-of snapshots never change, so they are kept.
    pub fn invalidate(&mut self) {
        self.cache.retain(|s, _| matches!(s, Scope::At(_)));
        self.graphs.retain(|s, _| matches!(s, Scope::At(_)));
    }

    /// Test hook: whether `scope`'s materialization and graph are cached.
    #[doc(hidden)]
    pub fn is_cached(&self, scope: &Scope) -> (bool, bool) {
        (
            self.cache.contains_key(scope),
            self.graphs.contains_key(scope),
        )
    }

    pub(crate) fn update_cache(&mut self, view: &str, ops: &[Op]) {
        self.graphs.remove(&Scope::View(view.to_string()));
        if let Some(m) = self.cache.get_mut(&Scope::View(view.to_string())) {
            m.apply(ops);
        }
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

impl Rdy {
    /// The config as it is on disk now; any read or parse error reads as no config.
    fn config_now(&self) -> Option<Config> {
        let text = std::fs::read_to_string(self.root().join(".rdy/config.yaml")).ok()?;
        serde_yaml_ng::from_str(&text).ok()
    }

    /// The name of the project as a site shows it: the config's `name`, else the project
    /// directory's name (`-` and `_` turned into spaces, the first letter upper-cased), else
    /// `Design docs`.
    pub fn project_name(&self) -> String {
        let named = self
            .config_now()
            .and_then(|c| c.name)
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        if let Some(n) = named {
            return n;
        }
        let dir = self
            .root()
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

    /// The config's `description`, when it has a non-empty one.
    pub fn project_description(&self) -> Option<String> {
        self.config_now()
            .and_then(|c| c.description)
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
    }
}
