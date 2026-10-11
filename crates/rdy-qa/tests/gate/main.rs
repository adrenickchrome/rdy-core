//! The `gate` area of the rdy-qa suite: one test binary (`cargo qa --only gate`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod gate_alts;
mod gate_semantic;
mod gate_tensions;
