//! The Windows side of [`crate::platform`] (FW-XP1 Phase 4). A child module of this one makes each
//! part: `windows_process` makes process liveness, the Job Object that owns a child's tree and the
//! bounded runner; `windows_fs` makes the filesystem fixtures (read-only directories, links, file
//! identities, native names); `windows_accounts` makes the second-account fixture; `windows_agent`
//! makes the fake SSH agents on named pipes. The release facts are not here yet, so `crate::platform` keeps its compile error.

pub use windows_accounts::*;
pub use windows_agent::*;
pub use windows_fs::*;
pub use windows_process::*;
// siblings: this file is not a mod.rs, so a plain `mod` would look in platform/windows/
#[path = "windows_accounts.rs"]
mod windows_accounts;
#[path = "windows_agent.rs"]
mod windows_agent;
#[path = "windows_fs.rs"]
mod windows_fs;
#[path = "windows_process.rs"]
mod windows_process;
