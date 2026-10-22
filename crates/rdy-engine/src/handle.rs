//! Opening, creating and closing a project: the `Rdy` handle's accessors.

use crate::config::{Config, ServeSection, parse_config, read_config, site_identity};
use crate::pack_load::{load_pack, load_pack_from, read_sources};
use crate::read_state::{AsOfCache, Entry};
use crate::{
    E_ALREADY_EXISTS, E_IO, Events, OpenReport, PackFingerprint, Rdy, RdyError, ReadState,
    ServeConfig, brief, inflight, tensions, version,
};
use arc_swap::ArcSwap;
use rdy_schema::Pack;
use rdy_store::holder::StoreMode;
use rdy_store::{Store, layout};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) fn io(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check file permissions and disk space")
}

/// The caches every state of one redaction epoch shares with the writer: the as-of and proposal
/// entries, the version positions, the blocks each proposal version writes, and canon's version
/// steps. A content removal starts a new set (plan Redaction, Task 10); no entry crosses epochs.
pub(crate) struct EpochCaches {
    pub(crate) as_of: Arc<AsOfCache>,
    pub(crate) vpos: Arc<Mutex<version::VposCache>>,
    pub(crate) proposal_touched: inflight::ProposalTouched,
    pub(crate) canon_step_cache: Arc<crate::history::StepCache>,
}

impl EpochCaches {
    /// An empty set.
    pub(crate) fn new() -> EpochCaches {
        EpochCaches {
            as_of: Arc::default(),
            vpos: Arc::default(),
            proposal_touched: inflight::ProposalTouched::default(),
            canon_step_cache: Arc::default(),
        }
    }
}

/// What a handle reads besides its store: the pack resolved for it with its lineage, the pack
/// files' fingerprint, and the config. A project reads them from its files (`project_context`);
/// a holder's are fixed values that no file supplies (`holder::holder_context`).
pub(crate) struct Context {
    pub(crate) pack: Pack,
    pub(crate) lineage: Vec<String>,
    pub(crate) fingerprint: PackFingerprint,
    pub(crate) config: Config,
}

/// A project's context, read from its files. Each file is read once: the config and pack are
/// loaded from the bytes the fingerprint hashed, so the watch's fingerprint is exactly what is
/// loaded. The config's own error comes first (not a project, a bad config); an unreadable pack
/// file fails the open before the store is locked. `.rdy.yaml` with no store is refused in the
/// words every path uses, after the config's and the pack's own errors.
fn project_context(project: &Path) -> Result<Context, RdyError> {
    let sources = match read_sources(project) {
        Ok(s) => s,
        Err(e) => {
            read_config(project)?;
            return Err(e);
        }
    };
    let config = parse_config(project, &sources.config, sources.local.as_deref())?;
    let (pack, lineage) = load_pack_from(&sources.packs, &config.pack)?;
    crate::config_levels::require_project(project)?;
    Ok(Context {
        pack,
        lineage,
        fingerprint: sources.fingerprint,
        config,
    })
}

impl Rdy {
    pub fn init(project: &Path, pack: &str) -> Result<Rdy, RdyError> {
        let dot = project.join(".rdy");
        let config = project.join(crate::config_levels::PROJECT_FILE);
        // init never touches an existing .rdy.yaml: without a store beside it, the refusal is
        // the text every other path gives for that state
        if crate::config_levels::is_project(project) {
            return Err(RdyError::new(
                E_ALREADY_EXISTS,
                format!("{} is already an rdy project", project.display()),
                "open the existing project, or init a different directory",
            ));
        }
        if config.exists() {
            return Err(crate::config_levels::missing_project(project));
        }
        // a store with no .rdy.yaml (the old layout): init never adopts it and never guesses
        // its pack. What is not a current-format store keeps its own refusal (fmt3), read only.
        if crate::config_levels::store_has_entry(project) {
            rdy_store::Store::check_existing(&dot.join(layout::STORE_DIR))?;
            return Err(RdyError::new(
                E_ALREADY_EXISTS,
                format!("{} has an rdy store but no .rdy.yaml", project.display()),
                "write .rdy.yaml with this project's settings (pack, name, decisions.home); rdy init does not adopt an existing store",
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
            coverage: None,
            propose: None,
        })
        .expect("config yaml");
        rdy_store::fsutil::write_durable(&config, cfg.as_bytes()).map_err(io)?;
        let mut r = Rdy::open(project)?;
        // plan M2e2: an empty store folds to no move and its state holds none, so the log is
        // reconciled here at no cost, and the first write logs only its own moves
        r.reconcile_transitions()?;
        Ok(r)
    }

    pub fn open(project: &Path) -> Result<Rdy, RdyError> {
        Rdy::open_with(project, None)
    }

    /// Opens `project` under `lock`, the store lock the caller took and keeps (a load, from
    /// before the rename of its store until it ends), in the mode its store holds: a holder as
    /// [`Rdy::open_holder`] does, with no config or pack file read; a project as [`Rdy::open`]
    /// does, with its config read once the mode is known. A lock that is not this project's
    /// store's is `E_LOCKED`.
    pub(crate) fn open_locked(
        project: &Path,
        lock: &rdy_store::StoreLock,
    ) -> Result<Rdy, RdyError> {
        let store = layout::open_project_store_locked(&project.join(".rdy"), lock)?;
        let context = match store.mode()? {
            StoreMode::Holder => crate::holder::holder_context()?,
            StoreMode::Project => project_context(project)?,
        };
        Rdy::from_store(project, store, context)
    }

    fn open_with(project: &Path, lock: Option<&rdy_store::StoreLock>) -> Result<Rdy, RdyError> {
        let context = project_context(project)?;
        let dot = project.join(".rdy");
        let store = match lock {
            Some(lock) => layout::open_project_store_locked(&dot, lock)?,
            None => layout::open_project_store(&dot)?,
        };
        // a holder is never read as a project: it has no canon, config or pack (D-BPYN8)
        if store.mode()? == StoreMode::Holder {
            return Err(crate::holder::is_a_holder());
        }
        Rdy::from_store(project, store, context)
    }

    /// The handle over `store`, already open, for the project or holder at `project` with
    /// `context`: the pack, its lineage and fingerprint, and the config that fill the handle's
    /// fields. The first publish reads the store's state.
    pub(crate) fn from_store(
        project: &Path,
        mut store: Store,
        context: Context,
    ) -> Result<Rdy, RdyError> {
        let Context {
            pack,
            lineage,
            fingerprint,
            config: cfg,
        } = context;
        // trk1: the freeze checks and every content hash use the pack's tracking fields
        store.set_tracking(pack.tracking());
        // plan M2e2 (D-Z0EMP): every write joins the open batch, and `publish` seals it with
        // the batch's tension and suspect moves last: one marker, one sync
        store.set_held(true)?;
        let areas = Arc::new(pack.area_matcher());
        let pack = Arc::new(pack);
        let EpochCaches {
            as_of,
            vpos,
            proposal_touched,
            canon_step_cache,
        } = EpochCaches::new();
        let (name, description) = site_identity(project, &cfg);
        let require_up_to_date = cfg.require_up_to_date();
        let idle_days = cfg.idle_days();
        let coverage_target = cfg.coverage_target();
        let (propose_cadence, propose_changes) = (cfg.propose_cadence(), cfg.propose_changes());
        let decisions_home = cfg.decisions_home();
        let lease_ttl_ms = i64::from(cfg.lease_ttl_minutes()) * 60_000;
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
            coverage_target,
            propose_cadence,
            propose_changes,
            decisions_home,
            reader: store.reader(),
            as_of: Arc::clone(&as_of),
            vpos: Arc::clone(&vpos),
            proposal_touched: Arc::clone(&proposal_touched),
            canon_step_cache: Arc::clone(&canon_step_cache),
            inflight_cell: Arc::default(),
            coord_heads: Arc::default(),
            plan_heads: Arc::default(),
            coord: OnceLock::new(),
            tensions: Arc::default(),
            dismissals: Arc::default(),
            mention_dismissals: Arc::default(),
            semantic: Arc::default(),
            lease_ttl_ms,
            coverage_memo: Default::default(),
            detectors: &tensions::DETECTORS,
            error: None,
        });
        let mut r = Rdy {
            root: project.to_path_buf(),
            state,
            logged: None,
            as_of,
            vpos,
            proposal_touched,
            canon_step_cache,
            events: Events::new(),
            last_emitted: store.log_end(),
            emit_error: None,
            store,
            pack,
            areas,
            lineage,
            fingerprint,
            serve: cfg.serve.resolve(),
            config: Arc::new(cfg),
            briefs: Arc::new(brief::BriefRegistry::new(brief::BRIEF_REGISTRY_CAP)),
            leases: Arc::new(crate::semantic::lease::LeaseRegistry::new()),
            detectors: &tensions::DETECTORS,
            recheck: None,
            transition_error: None,
            full_publish_panics: 0,
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

    /// The project's lease registry for review requests: the same one for as long as this `Rdy`
    /// is open.
    pub fn leases(&self) -> Arc<crate::semantic::lease::LeaseRegistry> {
        Arc::clone(&self.leases)
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

    pub fn store(&self) -> &Store {
        &self.store
    }
    /// The store, to write to it directly. A raw write through store_mut() joins the open
    /// batch; the next publish or seal_open seals it.
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// Makes the store's index durable up to everything written so far (see `Store::checkpoint`).
    /// An open batch is sealed first.
    pub fn checkpoint(&mut self) -> Result<(), RdyError> {
        self.seal_open()?;
        Ok(self.store.checkpoint()?)
    }

    /// Closes the project's store cleanly (see `Store::close`) and returns a failed checkpoint's
    /// error. An open batch is sealed first, with its moves. Dropping an `Rdy` closes it too,
    /// but can only report a failure on stderr.
    pub fn close(mut self) -> Result<(), RdyError> {
        let sealed = self.seal_open();
        let Rdy {
            state,
            logged,
            store,
            ..
        } = self;
        drop(logged);
        drop(state);
        let closed = store.close();
        sealed?;
        Ok(closed?)
    }

    /// Checkpoints if writes are waiting and the store has been idle long enough; true if it did.
    pub fn checkpoint_if_idle(&mut self) -> Result<bool, RdyError> {
        Ok(self.store.checkpoint_if_idle(std::time::Instant::now())?)
    }

    /// The current published state: what every read reads. Cheap; a held state never changes.
    pub fn read(&self) -> Arc<ReadState> {
        self.state.load_full()
    }

    /// The caches the writer shares with the states it publishes in the current epoch.
    pub(crate) fn epoch_caches(&self) -> EpochCaches {
        EpochCaches {
            as_of: Arc::clone(&self.as_of),
            vpos: Arc::clone(&self.vpos),
            proposal_touched: Arc::clone(&self.proposal_touched),
            canon_step_cache: Arc::clone(&self.canon_step_cache),
        }
    }

    /// Starts a new, empty set of shared caches: after a content removal (or its failure past
    /// authorization) no state of the new epoch reuses an entry built from the old one.
    pub(crate) fn renew_epoch_caches(&mut self) {
        let EpochCaches {
            as_of,
            vpos,
            proposal_touched,
            canon_step_cache,
        } = EpochCaches::new();
        self.as_of = as_of;
        self.vpos = vpos;
        self.proposal_touched = proposal_touched;
        self.canon_step_cache = canon_step_cache;
    }

    /// Test hook: every state published from now on runs `d` in place of the registry's
    /// detectors. Republishes everything (`Delta::Full`); a failed publish leaves its error in
    /// the published state.
    #[doc(hidden)]
    pub fn set_detectors_for_tests(&mut self, d: &'static [&'static dyn tensions::Detector]) {
        self.detectors = d;
        let _ = self.publish(crate::Delta::Full);
    }

    /// Test hook: the next `n` `Delta::Full` publishes (a pack reload's, `invalidate`'s) panic
    /// before they change anything.
    #[doc(hidden)]
    pub fn panic_full_publishes_for_tests(&mut self, n: u32) {
        self.full_publish_panics = n;
    }

    /// Days without a write after which an open view is idle (`views.idle_days`, default 14).
    pub fn idle_days(&self) -> u32 {
        self.read().idle_days()
    }
}
