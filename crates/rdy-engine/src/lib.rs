mod decide;
mod error;
mod gate;
mod io;
mod materialize;
mod query;
mod session;
pub mod testkit;
mod version;
mod write;

pub use decide::DecisionInput;
pub use error::*;
pub use gate::ProposalInfo;
pub use io::{ExportSummary, block_from_yaml, block_to_yaml};
pub use materialize::Materialized;
pub use query::{Context, ImpactItem, field_json, summary, summary_in};
pub use session::{Scope, Session, now_ms, rfc3339_now};
pub use version::{
    AS_OF_CACHE_ENTRIES, VersionInfo, VersionPosition, VersionRef, ViewPosition, parse_version,
};
pub use write::{LinkInput, OpInput, WriteRequest, WriteResult, json_to_field};

use rdy_model::Op;
use rdy_schema::{Pack, PackSet};
use rdy_store::Store;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[derive(serde::Serialize, serde::Deserialize)]
struct Config {
    pack: String,
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
}

fn io(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check file permissions and disk space")
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
            set.add_yaml(&text).map_err(|e| {
                RdyError::new(
                    E_CONFIG,
                    format!("{}: {e}", f.display()),
                    "fix the pack file",
                )
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
        std::fs::create_dir_all(dot.join("packs")).map_err(io)?;
        load_pack(project, pack)?;
        let cfg = serde_yaml_ng::to_string(&Config {
            pack: pack.to_string(),
        })
        .expect("config yaml");
        std::fs::write(dot.join("config.yaml"), cfg).map_err(io)?;
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
        let store = Store::open(&project.join(".rdy/store"))?;
        Ok(Rdy {
            root: project.to_path_buf(),
            store,
            pack,
            lineage,
            cache: HashMap::new(),
            graphs: HashMap::new(),
            as_of_lru: VecDeque::new(),
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
                    match self.store.get_raw(h) {
                        Ok((k, _)) if k == rdy_store::kinds::LANDING => {}
                        Ok(_) | Err(rdy_store::StoreError::NotFound(_)) => {
                            return Err(not_landing());
                        }
                        Err(e) => return Err(e.into()),
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
        Ok(FsckReport {
            frames,
            landings: landings.len(),
            versions,
        })
    }
}
