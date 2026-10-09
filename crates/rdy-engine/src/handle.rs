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
use rdy_store::{Store, layout};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) fn io(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check file permissions and disk space")
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
            tensions: Arc::default(),
            dismissals: Arc::default(),
            detectors: &tensions::DETECTORS,
            error: None,
        });
        let settled_batch = store.last_batch_end();
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
            detectors: &tensions::DETECTORS,
            tension_seen: None,
            publish_gen: 0,
            settled_batch,
            tension_base: None,
            full_publish_panics: 0,
        };
        r.invalidate();
        // that publish replaced the placeholder state: the first write records the base
        r.tension_base = None;
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
