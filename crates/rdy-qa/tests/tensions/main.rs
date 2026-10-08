//! The `tensions` area of the rdy-qa suite: one test binary (`cargo qa --only tensions`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod tensions_collision;
mod tensions_core;
mod tensions_deprecated;
mod tensions_dismiss;
mod tensions_duplicate;
mod tensions_fork;
mod tensions_premise;
mod tensions_unjustified;
