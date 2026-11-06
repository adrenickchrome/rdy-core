//! The Windows side of [`crate::platform`] (FW-XP1 Phase 4). A child module of this one makes each
//! part: `windows_process` makes process liveness, the Job Object that owns a child's tree and the
//! bounded runner. The child fixtures, the filesystem and account fixtures and the release facts
//! are not here yet, so `crate::platform` keeps its compile error.

pub use windows_process::*;
// a sibling file: this file is not a mod.rs, so a plain `mod` would look in platform/windows/
#[path = "windows_process.rs"]
mod windows_process;
