//! The Windows side of `sys`, one child module for each part: `pipe` makes the transport,
//! `identity` reads the account at its other end, `agent` reaches the caller's SSH agent,
//! `run_state` names the project's pipe and reads and writes the files of its run directory,
//! `process` starts and looks at processes, and `stop` holds the project's stop event and ends a
//! daemon under proof. The parts still to come have no stand-in here, so this crate does not build
//! for Windows until the last of them exists.

mod agent;
mod identity;
mod pipe;
mod process;
mod run_state;
mod stop;
pub use agent::*;
pub use identity::*;
pub use pipe::*;
pub use process::*;
pub use run_state::*;
pub use stop::*;
