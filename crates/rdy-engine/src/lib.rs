mod decide;
mod io;
mod error;
mod gate;
mod materialize;
mod query;
mod session;
mod write;
pub mod testkit;

pub use decide::DecisionInput;
pub use error::*;
pub use gate::ProposalInfo;
pub use io::{block_from_yaml, block_to_yaml, ExportSummary};
pub use materialize::Materialized;
pub use query::{field_json, summary, summary_in, Context, ImpactItem};
pub use session::{now_ms, rfc3339_now, Scope, Session};
pub use write::{json_to_field, LinkInput, OpInput, WriteRequest, WriteResult};

use rdy_model::Op;
use rdy_schema::{Pack, PackSet};
use rdy_store::Store;
use std::collections::HashMap;
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
}

fn io(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check file permissions and disk space")
}

fn load_pack(root: &Path, name: &str) -> Result<(Pack, Vec<String>), RdyError> {
    let mut set = PackSet::with_builtin();
    let dir = root.join(".rdy/packs");
    if dir.exists() {
        let mut files: Vec<_> = std::fs::read_dir(&dir).map_err(io)?.filter_map(Result::ok).map(|e| e.path()).collect();
        files.sort();
        for f in files.into_iter().filter(|f| f.extension().is_some_and(|x| x == "yaml" || x == "yml")) {
            let text = std::fs::read_to_string(&f).map_err(io)?;
            set.add_yaml(&text).map_err(|e| RdyError::new(E_CONFIG, format!("{}: {e}", f.display()), "fix the pack file"))?;
        }
    }
    let config = |e: rdy_schema::SchemaError| RdyError::new(E_CONFIG, e.to_string(), "set `pack:` in .rdy/config.yaml to an available pack (rdy-standard is built in)");
    let pack = set.resolve(name).map_err(config)?;
    let lineage = set.lineage(name).map_err(config)?;
    Ok((pack, lineage))
}

impl Rdy {
    pub fn init(project: &Path, pack: &str) -> Result<Rdy, RdyError> {
        let dot = project.join(".rdy");
        if dot.join("config.yaml").exists() {
            return Err(RdyError::new(E_ALREADY_EXISTS, format!("{} is already an rdy project", project.display()), "open the existing project, or init a different directory"));
        }
        std::fs::create_dir_all(dot.join("packs")).map_err(io)?;
        load_pack(project, pack)?;
        let cfg = serde_yaml_ng::to_string(&Config { pack: pack.to_string() }).expect("config yaml");
        std::fs::write(dot.join("config.yaml"), cfg).map_err(io)?;
        Rdy::open(project)
    }

    pub fn open(project: &Path) -> Result<Rdy, RdyError> {
        let cfg_path = project.join(".rdy/config.yaml");
        if !cfg_path.exists() {
            return Err(RdyError::new(E_NO_PROJECT, format!("{} is not an rdy project", project.display()), "run rdy init in the project directory"));
        }
        let text = std::fs::read_to_string(&cfg_path).map_err(io)?;
        let cfg: Config = serde_yaml_ng::from_str(&text).map_err(|e| RdyError::new(E_CONFIG, e.to_string(), "fix .rdy/config.yaml"))?;
        let (pack, lineage) = load_pack(project, &cfg.pack)?;
        let store = Store::open(&project.join(".rdy/store"))?;
        Ok(Rdy { root: project.to_path_buf(), store, pack, lineage, cache: HashMap::new(), graphs: HashMap::new() })
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
            };
            let blocks = self.store.blocks(&snapshot)?;
            self.cache.insert(scope.clone(), Materialized::build(snapshot, blocks));
        }
        Ok(&self.cache[scope])
    }

    pub fn invalidate(&mut self) {
        self.cache.clear();
        self.graphs.clear();
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
}

impl Rdy {
    pub fn fsck(&self) -> Result<FsckReport, RdyError> {
        let frames = self.store.verify()?;
        let landings = self.store.landings()?;
        for (_, rec) in &landings {
            self.store.get::<rdy_model::StateSnapshot>(&rec.state)?;
        }
        Ok(FsckReport { frames, landings: landings.len() })
    }
}
