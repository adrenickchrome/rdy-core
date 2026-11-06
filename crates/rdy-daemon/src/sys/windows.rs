//! The Windows side of `sys`, one child module for each part: `pipe` makes the transport. The
//! parts still to come have no stand-in here, so this crate does not build for Windows until the
//! last of them exists.

mod agent;
mod pipe;
pub use agent::*;
pub use pipe::*;
