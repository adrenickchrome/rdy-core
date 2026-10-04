pub mod error;
pub mod index;
pub mod kinds;
pub mod segment;
mod store;

pub use error::StoreError;
pub use store::Store;
mod state;
pub use state::{apply_op, ViewRecord};
