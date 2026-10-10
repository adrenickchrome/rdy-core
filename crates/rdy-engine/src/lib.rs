pub mod accept;
pub mod alt;
mod areas;
pub mod brief;
pub mod code_brief;
pub mod code_index;
mod config;
pub mod config_levels;
pub mod consequence;
pub mod coord_graph;
pub mod coverage;
mod decide;
pub mod engine;
mod error;
pub mod events;
pub mod factsheet;
mod fsck;
mod gate;
mod handle;
mod history;
pub mod inflight;
pub mod intent;
mod io;
mod keys;
pub mod lexicon;
mod lint;
mod materialize;
mod pack_load;
mod paths;
pub mod proposal_show;
mod publish;
mod query;
mod read_state;
pub mod related;
pub mod search;
mod session;
pub mod suggest;
pub mod tensions;
pub mod testkit;
mod tree_io;
mod version;
pub mod view_api;
pub mod view_query;
pub mod views;
pub mod wc;
mod wc_check;
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
    LandInfo, LandOptions, LandingRow, PROPOSAL_TENSIONS_MAX, ProposalInfo, ProposalRef,
    RebasedInfo, RelayedByRow,
};
pub use history::{Author, HISTORY_MAX, History, HistoryEntry, JustificationInfo, LandingRef};
pub use io::{EditBase, ExportSummary, block_from_yaml, block_to_yaml, refuse_inflight_export};
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
pub use read_state::{
    AltBinds, AltChosenBy, CoordHeads, Delta, InflightSet, OpenIntent, ReadState,
};
pub use search::{SearchQuery, SearchResult};
pub use session::{
    HUMAN_RUN, INFLIGHT_SCOPE, PinnedClock, Scope, Session, SessionKind, classify, human_actor,
    human_actor_for, human_actor_for_env, now_ms, pin_clock, rfc3339, rfc3339_now,
};
pub use tree_io::Diagnostic;
pub use version::{
    AS_OF_CACHE_ENTRIES, VersionInfo, VersionPosition, VersionRef, ViewPosition, parse_version,
};
pub use view_query::{
    ChangeSummary, Compare, CompareEntry, NonceLookup, OpSummary, ViewDetail, ViewSummary,
};
pub use views::{ConflictInfo, ConflictSide, OwnerInfo, ViewHeadInfo, ViewOpResult};
pub use wc::project::Projection;
pub use write::{
    LinkInput, OpInput, WriteRequest, WriteResult, editorial_class, json_to_field,
    parse_justification,
};

pub use config::{ServeConfig, serve_config};
pub use engine::{Engine, Local, WriteFn};
pub use fsck::FsckReport;
pub use pack_load::{
    FileStamp, PackFingerprint, ReloadOutcome, pack_fingerprint, pack_source_reads, project_pack,
    resolve_pack,
};
pub use rdy_store::OpenReport;
pub use rdy_store::fsutil::{sync_dir, write_durable};
pub use wc_check::{check_wc, lexical_path};

use arc_swap::ArcSwap;
use rdy_schema::{AreaMatcher, Pack};
use rdy_store::Store;
use rdy_store::segment::Location;
use read_state::AsOfCache;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    /// The config as it was parsed at open or by the last successful pack reload. A full
    /// publish reads the site identity and the review and view settings from it, never from the
    /// files again (#832).
    config: Arc<config::Config>,
    serve: ServeConfig,
    /// The registered briefings (in memory; one registry per open project).
    briefs: Arc<brief::BriefRegistry>,
    /// The tension detectors every published state runs: the registry, unless a test set others.
    detectors: &'static [&'static dyn tensions::Detector],
    /// The tension set the last `settle_tensions` recorded, and the publish (`publish_gen`) of
    /// its state.
    tension_seen: Option<(Arc<tensions::TensionSet>, u64)>,
    /// How many states `publish` has published since open: a settle keys on it, so a publish
    /// with no store write (a pack reload) is settled too.
    publish_gen: u64,
    /// The store's last batch end at the last settle (at open: the origin): a settle with no
    /// batch since names no `by`.
    settled_batch: Location,
    /// The state `publish` held before the first write since open, while nothing is settled:
    /// the first settle diffs against its set.
    tension_base: Option<Arc<ReadState>>,
    /// Test hook: how many of the next `Delta::Full` publishes panic (0 in every real run).
    full_publish_panics: u32,
}
