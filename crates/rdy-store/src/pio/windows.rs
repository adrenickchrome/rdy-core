//! The Windows side of [`crate::pio`] (FW-XP1 Phase 4). A child module of this one makes each
//! part: `windows_io` makes the positioned reads and writes, hole punches and allocated-bytes
//! queries; `windows_security` makes private objects and no-follow opens; `windows_dir` makes the
//! opens, entry kinds, listings, creates, removals, renames and child processes anchored to an
//! open directory; `windows_durable` makes the directory flushes and write-through renames.
//! Permission-mode changes (`set_mode_at`) have no Windows form: Windows has no permission modes,
//! and a private Windows create already gives the object its private descriptor.

pub use windows_io::*;
// a sibling file: this file is not a mod.rs, so a plain `mod` would look in pio/windows/
#[path = "windows_io.rs"]
mod windows_io;

pub use windows_security::*;
#[path = "windows_security.rs"]
mod windows_security;

pub use windows_dir::*;
#[path = "windows_dir.rs"]
mod windows_dir;

pub use windows_durable::*;
#[path = "windows_durable.rs"]
mod windows_durable;
