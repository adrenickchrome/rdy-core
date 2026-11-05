//! The Windows side of [`crate::pio`] (FW-XP1 Phase 4). No part of it is built yet. Each part
//! below stops the build with a `compile_error!` that names what it lacks, so no call can reach a
//! body that compiles without the platform call, and no answer is made up. A child module of this
//! one replaces each placeholder with its implementation.

compile_error!(
    "rdy_store::pio has no Windows implementation yet of positioned reads and writes, hole punching (FSCTL_SET_ZERO_DATA on a sparse file) or allocated bytes (FW-XP1 Phase 4)"
);

compile_error!(
    "rdy_store::pio has no Windows implementation yet of privacy (a protected DACL with one allow ACE for this account) or no-follow opens (FW-XP1 Phase 4)"
);

compile_error!(
    "rdy_store::pio has no Windows implementation yet of opens, entry kinds and listings anchored to an open directory (FW-XP1 Phase 4)"
);

compile_error!(
    "rdy_store::pio has no Windows implementation yet of creates and removals anchored to an open directory (FW-XP1 Phase 4)"
);

compile_error!(
    "rdy_store::pio has no Windows implementation yet of renames anchored to an open directory or of a child process that starts in one (FW-XP1 Phase 4)"
);

compile_error!(
    "rdy_store::pio has no Windows implementation yet of directory flushes (FlushFileBuffers) or write-through renames (MoveFileExW) (FW-XP1 Phase 4)"
);
