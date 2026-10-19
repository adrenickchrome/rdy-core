//! The in-process adapter of one rdy store, for a host that embeds rdy core (D-WDKWQ).
//!
//! `StoreHandle` opens a project's store through the existing exclusive lock and hands out
//! captured read snapshots; no daemon and no second writer service. A record or a rule belongs to
//! core: this crate adds none. Every type its signatures use is re-exported here, so a consumer
//! depends on this crate alone. No raw `Local`, `Rdy`, `ReadState` or `StoreReader` escapes it.
//! No product behaviour is promised stable across pins.

pub mod history;
pub mod maintenance;
pub mod publication;
mod store;
mod value;

pub use rdy_engine::RdyError;
pub use rdy_model::{Block, BlockId, Hash, RepoId, Tag};
pub use rdy_store::meta::Origin;
pub use store::{Identity, Snapshot, StoreHandle};
pub use value::{encode_block, hash_bytes};
