//! The in-process adapter of one rdy store, for a host that embeds rdy core (D-WDKWQ).
//!
//! `StoreHandle` opens a project's store through the existing exclusive lock and hands out
//! captured read snapshots; no daemon and no second writer service. A record or a rule belongs to
//! core: this crate adds none. Every type its signatures use is re-exported here, so a consumer
//! depends on this crate alone. No raw `Local`, `Rdy`, `ReadState` or `StoreReader` escapes it.
//! `rendering` writes a render request for the host's child process, of a captured tag or of a
//! reached landing alone or against a second one (D-CST6K), and renders one page of a captured
//! tag; the renderer itself is core's (`rdy-render`). A store upgrade is an explicit
//! call (`maintenance::upgrade`): no open upgrades a store (D-WWAK7). `sync` checks a push into
//! a holder and makes the hub's canon and view moves; `landing_check` checks a stored landing
//! with the inputs the hub passes. `compare` compares and diffs any two scopes a snapshot reads,
//! and pages the diff as `rdy_diff` does; `version_tests` tests whether a chain of landings merges
//! onto a landing's state, and whether an approval carries from one request version to the next;
//! `replay_test` tests where a pull replay of a chain would stop, with each landing's gate result.
//! No product behaviour is promised stable across pins.

pub mod capabilities;
pub mod compare;
mod follower;
pub mod foreign_facts;
pub mod foreign_removal;
mod fork;
pub mod history;
pub mod landing_check;
pub mod link_take;
pub mod links;
pub mod maintenance;
pub mod publication;
pub mod publication_take;
pub mod reads;
pub mod rendering;
pub mod replay_test;
pub mod signatures;
mod store;
pub mod stream;
pub mod sync;
pub mod take;
mod value;
pub mod version_tests;

pub use compare::{Compare, CompareEntry, Diff, DiffChange, DiffCounts, DiffEntry, DiffQuery};
pub use follower::Follower;
pub use foreign_removal::{ForeignRemovalTaken, RemovalTaken};
pub use landing_check::LandingCheck;
pub use link_take::{DeliverySource, DeliveryTaken, LinkRevoked, LinkSource};
pub use maintenance::{
    ChainEnd, DumpStop, LoadedChainFile, Location, StoppedAt, UpgradePlan, UpgradeReceipt,
    UpgradeReport, UpgradeStep,
};
pub use maintenance::{FeatureClass, StoreFeature, StoreFeatures};
pub use publication::{Baseline, BaselineBinding, PreviousChoice, ProfileFacts, profile_facts};
pub use publication_take::{
    PublicationFacts, PublicationRefusal, PublicationTaken, PublicationTarget,
};
pub use rdy_engine::follower::FollowReport;
pub use rdy_engine::identity::{MAX_ACTOR_BYTES, is_actor};
pub use rdy_engine::search::SearchFilters;
pub use rdy_engine::{
    CarriedTargetIssue, CarriedWarning, CompactReport, KindBytes, RdyError, core_error_code,
};
pub use rdy_model::foreign::ForeignSet;
pub use rdy_model::{Block, BlockId, Hash, RepoId, Tag};
pub use rdy_store::holder::StoreMode;
pub use rdy_store::meta::Origin;
pub use reads::{ReadRequest, SearchRequest};
pub use replay_test::{ReplayStop, ReplayStopCause, ReplayTest, ReplayedCheck};
pub use store::{Identity, Snapshot, StoreHandle};
pub use sync::{
    CanonMove, ChainFacts, CheckedQuarantine, Moved, PushedView, QuarantineFacts, RdyVersion,
    ViewRead,
};
pub use take::{StorePosition, TakeOutcome, UploadTarget};
pub use value::{encode_block, hash_bytes};
pub use version_tests::{ConflictKind, MergeConflict, RebaseTest, VersionChain, VersionTests};
