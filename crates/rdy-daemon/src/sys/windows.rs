//! The Windows side of `sys`, one child module for each part: `pipe` makes the transport,
//! `identity` reads the account at its other end, `agent` reaches the caller's SSH agent, and
//! `run_state` names the project's pipe and reads and writes the files of its run directory. The
//! parts still to come have no stand-in here, so this crate does not build for Windows until the
//! last of them exists.

mod agent;
mod identity;
mod pipe;
mod run_state;
pub use agent::*;
pub use identity::*;
pub use pipe::*;
pub use run_state::*;
