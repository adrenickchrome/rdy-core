//! The Windows side of `sys`, one child module for each part: `pipe` makes the transport,
//! `identity` reads the account at its other end, and `agent` reaches the caller's SSH agent. The
//! parts still to come have no stand-in here, so this crate does not build for Windows until the
//! last of them exists.

mod agent;
mod identity;
mod pipe;
pub use agent::*;
pub use identity::*;
pub use pipe::*;
