//! The Windows side of [`crate::pio`] (FW-XP1 Phase 4). Each part that is not built yet stops the
//! build with a `compile_error!` that names what it lacks, so no call can reach a body that
//! compiles without the platform call, and no answer is made up. A child module of this one
//! replaces each placeholder with its implementation: `windows_io` makes the positioned reads and
//! writes, hole punches and allocated-bytes queries; `windows_security` makes private objects and
//! no-follow opens; `windows_dir` makes the opens, entry kinds and listings anchored to an open
//! directory.

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

compile_error!(
    "rdy_store::pio has no Windows implementation yet of creates and removals anchored to an open directory (FW-XP1 Phase 4)"
);

compile_error!(
    "rdy_store::pio has no Windows implementation yet of renames anchored to an open directory or of a child process that starts in one (FW-XP1 Phase 4)"
);

compile_error!(
    "rdy_store::pio has no Windows implementation yet of directory flushes (FlushFileBuffers) or write-through renames (MoveFileExW) (FW-XP1 Phase 4)"
);
