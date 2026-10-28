//! The in-process adapter of one rdy store, for a host that embeds rdy core (D-WDKWQ).
//!
//! `StoreHandle` opens a project's store through the existing exclusive lock and hands out
//! captured read snapshots; no daemon and no second writer service. A record or a rule belongs to
//! core: this crate adds none. Every type its signatures use is re-exported here, so a consumer
//! depends on this crate alone. No raw `Local`, `Rdy`, `ReadState` or `StoreReader` escapes it.
//! `rendering` writes a render request for the host's child process and renders one page of a
//! captured tag; the renderer itself is core's (`rdy-render`). A store upgrade is an explicit
//! call (`maintenance::upgrade`): no open upgrades a store (D-WWAK7).
//! No product behaviour is promised stable across pins.

pub mod history;
pub mod maintenance;
pub mod publication;
pub mod publication_take;
pub mod rendering;
pub mod signatures;
mod store;
pub mod stream;
pub mod take;
mod value;

pub use maintenance::{
    ChainEnd, DumpStop, LoadedChainFile, StoppedAt, UpgradePlan, UpgradeReceipt, UpgradeReport,
    UpgradeStep,
};
pub use publication::{ProfileFacts, profile_facts};
pub use publication_take::{
    PublicationFacts, PublicationRefusal, PublicationTaken, PublicationTarget,
};
pub use rdy_engine::{CarriedTargetIssue, CarriedWarning, RdyError, core_error_code};
pub use rdy_model::{Block, BlockId, Hash, RepoId, Tag};
pub use rdy_store::holder::StoreMode;
pub use rdy_store::meta::Origin;
pub use store::{Identity, Snapshot, StoreHandle};
pub use take::{StorePosition, TakeOutcome, UploadTarget};
pub use value::{encode_block, hash_bytes};
