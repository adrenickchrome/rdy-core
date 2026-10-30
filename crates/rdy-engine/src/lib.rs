pub mod accept;
pub mod allowed_signers;
pub mod alt;
pub mod answer;
mod areas;
pub mod brief;
pub mod card_words;
pub mod carried_warning;
pub mod clone;
pub mod code_brief;
pub mod code_evidence;
pub mod code_index;
mod config;
pub mod config_edit;
pub mod config_levels;
pub mod consequence;
pub mod consequence_literals;
pub mod coord_graph;
pub mod coverage;
mod cursor;
mod decide;
pub mod decision_fields;
pub mod did_you_mean;
pub mod diff;
pub mod diff_request;
pub mod dispute;
pub mod dump;
pub mod dump_error;
pub mod dump_stop;
pub mod engine;
mod error;
pub mod events;
pub mod factsheet;
pub mod formats;
mod fsck;
mod gate;
mod handle;
pub use handle::{CompletionPhase, read_only_feature_for_tests, with_completion_hook_for_tests};
mod history;
pub mod history_facts;
mod holder;
pub mod hub_config;
pub mod hub_credentials;
pub mod hub_http;
pub mod hub_keys;
pub mod hub_publish;
pub mod hub_publish_files;
pub mod hub_publish_stream;
pub mod hub_publish_wire;
pub mod hub_remotes;
pub mod hub_types;
pub mod identity;
pub mod inbox;
pub mod inflight;
pub mod intent;
mod io;
mod keys;
pub mod landing_order;
pub mod lexicon;
mod lint;
pub mod load;
pub mod load_chain;
pub mod maintenance;
mod materialize;
pub mod mention_review;
mod pack_load;
mod paths;
pub mod pending;
pub mod plan;
pub mod policy;
pub mod policy_verdict;
pub mod proposal_show;
pub mod publication;
pub mod publication_manifest;
pub mod publication_take;
mod publish;
pub mod quarantine;
mod query;
mod read_state;
pub mod readiness;
pub mod reaffirm;
pub mod redaction;
pub mod reflects;
pub mod related;
pub mod render_inputs;
pub mod render_source;
pub mod repo;
pub mod search;
pub mod section;
pub mod semantic;
mod session;
pub mod show;
pub mod signature_check;
pub mod signing_act;
pub mod signing_config;
pub mod signing_crypto;
pub mod signing_read;
pub mod site_scope;
pub mod site_target;
pub mod stream_check;
pub mod stream_error;
pub mod stream_export;
pub mod stream_take;
pub mod suggest;
pub mod suspect_reply;
pub mod tensions;
pub mod testkit;
pub mod trace;
pub mod transitions;
mod tree_io;
mod version;
pub mod view_api;
pub mod view_query;
pub mod views;
pub mod wc;
mod wc_check;
pub mod worddiff;
mod write;
mod yaml_pos;

pub use areas::by_area;
pub use brief::{
    BriefRegistry, Briefing, Coverage, E_BRIEF_REQUIRED, E_BRIEF_STALE, W_BRIEF_STALE,
};
pub use carried_warning::{CarriedTargetIssue, CarriedWarning};
pub use coverage::{CoverageIssue, CoverageIssues, IssueCode};
pub use decide::DecisionInput;
pub use diff::{Diff, DiffChange, DiffEntry, LinkChange, StatusChange};
pub use diff_request::{DiffReply, DiffRequest, LandingShow, ScopeLabel};
pub use dump::DumpSummary;
pub use error::*;
pub use events::{Event, EventKind, Events};
pub use gate::{
    LandInfo, LandOptions, LandingRow, PROPOSAL_TENSIONS_MAX, PolicyRow, ProposalInfo, ProposalRef,
    RebasedInfo, RelayedByRow, run_land,
};
pub use history::{
    Author, HISTORY_MAX, History, HistoryEntry, JustificationInfo, LandingRef, canon_step_reads,
};
pub use io::{EditBase, ExportSummary, block_from_yaml, block_to_yaml, refuse_inflight_export};
pub use lint::check_path;
pub use load::{LoadSummary, LoadedChainFile, StoppedAt};
pub use materialize::{Materialized, PathMiss};
pub use mention_review::{MentionReviewInfo, MentionVerdict};
pub use paths::TreeNode;
pub use proposal_show::{
    ProposalChangeRow, ProposalShow, ProposalVersionRow, is_proposal_ref, proposal_text,
};
pub use query::{
    Context, ImpactItem, ImpactVia, code_lines, compact_summary, cut_line, field_json, summary,
    summary_in,
};
pub use query::{RedactionRef, source_body};
pub use rdy_store::Relay;
pub use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpKind};
pub use read_state::{
    AltBinds, AltChosenBy, CoordHeads, Delta, InflightSet, OpenIntent, ReadState, inflight_builds,
};
pub use search::{SearchQuery, SearchResult};
pub use session::{
    HUMAN_RUN, INFLIGHT_SCOPE, PinnedClock, Scope, Session, SessionKind, classify, human_actor,
    human_actor_for, human_actor_for_env, now_ms, pin_clock, rfc3339, rfc3339_now,
};
pub use suspect_reply::{DeferredReply, SuspectReply};
pub use tree_io::Diagnostic;
pub use version::{
    AS_OF_CACHE_ENTRIES, MAX_VERSION_BYTES, TagInfo, VersionInfo, VersionPosition, VersionRef,
    ViewPosition, parse_version,
};
pub use view_query::{
    ChangeSummary, Compare, CompareEntry, NonceLookup, OpSummary, ViewDetail, ViewSummary,
};
pub use views::{ConflictInfo, ConflictSide, OwnerInfo, ResolveResult, ViewHeadInfo, ViewOpResult};
pub use wc::project::Projection;
pub use write::{
    JsonFieldError, LINK_KEY_TAIL, LinkInput, MAX_REFLECTS_SELECTOR_CHARS, MAX_REFLECTS_SOURCES,
    OP_FIX, OpInput, ParsedJustification, ReflectsArg, ReflectsInput, WriteRequest, WriteResult,
    editorial_class, is_reserved_view, json_to_field, one_justification_error, parse_justification,
    parse_op, parse_ops, reserved_view_error,
};

pub use config::{
    CoverageGate, DEFAULT_LEASE_TTL_MINUTES, DEFAULT_PROPOSE_CHANGES, ProposeCadence, ServeConfig,
    serve_config,
};
pub use engine::{Engine, Local, ReadFn, WriteFn};
pub use fsck::FsckReport;
pub use pack_load::{
    FileStamp, PackFingerprint, ReloadOutcome, pack_fingerprint, pack_source_reads,
    pack_source_reads_on_this_thread, project_pack, resolve_pack,
};
pub use rdy_store::BlobState;
pub use rdy_store::OpenReport;
pub use rdy_store::fsutil::{sync_dir, write_durable};
pub use rdy_store::{CachedDrawing, DiagramCache};
/// The notice channel of the library crates: stderr, unless a test captures the notice it
/// provokes ([`capture_notices_for_tests`]).
pub use rdy_store::{NoticeCapture, capture_notices_for_tests, notice};
pub use wc_check::{check_wc, lexical_path};

use arc_swap::ArcSwap;
use rdy_schema::{AreaMatcher, Pack};
use rdy_store::Store;
pub use rdy_store::segment::Location;
use read_state::AsOfCache;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct Rdy {
    root: PathBuf,
    /// The published read state (snap1). Declared before `store`, with the shared caches, so a
    /// dropped `Rdy` releases its state's reader before the store closes.
    state: ArcSwap<ReadState>,
    /// The state the log's tension and suspect moves fold to (plan M2e2, founder decision C,
    /// D-Z0EMP): the next batch logs its moves against it. `None` until a batch reconciles the
    /// store's open-move tables after open. Declared before `store`, as `state` is.
    logged: Option<Arc<ReadState>>,
    /// The as-of entries, shared with every published state.
    as_of: Arc<AsOfCache>,
    /// version_position results, keyed by scope and valid for one (canon head, tag count);
    /// shared with every published state.
    vpos: Arc<Mutex<version::VposCache>>,
    /// The blocks each proposal version writes; shared with every published state.
    proposal_touched: inflight::ProposalTouched,
    /// Canon's version steps kept across canon builds; shared with every published state.
    canon_step_cache: Arc<history::StepCache>,
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
    /// The leases on review requests (in memory; one registry per open project).
    leases: Arc<semantic::lease::LeaseRegistry>,
    /// The tension detectors every published state runs: the registry, unless a test set others.
    detectors: &'static [&'static dyn tensions::Detector],
    /// What a publish logs when no write batch moved anything: `pack_reload` (a pack reload),
    /// `reopened` (`reconcile_transitions`) or `recheck` (`invalidate`, `reindex`). Cleared by
    /// every publish.
    recheck: Option<rdy_store::transition::Cause>,
    /// The error of the last batch whose moves could not be computed; cleared by the next
    /// success.
    transition_error: Option<RdyError>,
    /// Test hook: how many of the next `Delta::Full` publishes panic (0 in every real run).
    full_publish_panics: u32,
}
