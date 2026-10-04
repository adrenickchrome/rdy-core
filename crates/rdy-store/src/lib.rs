pub mod error;
pub mod fsutil;
pub mod index;
pub mod kinds;
pub mod segment;
mod store;

pub use error::StoreError;
pub use store::{Store, lock_error};
mod state;
pub use state::{ViewRecord, apply_op};
