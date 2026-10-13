//! The `views` area of the rdy-qa suite: one test binary (`cargo qa --only views`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod views_conflicts;
mod views_consequences;
mod views_create;
mod views_cycles;
mod views_demo_consequences;
mod views_gate;
mod views_gate_consequences;
mod views_include;
mod views_invariants;
mod views_model;
mod views_own;
mod views_query;
mod views_undo;
