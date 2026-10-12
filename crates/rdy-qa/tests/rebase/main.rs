//! The `rebase` area of the rdy-qa suite: one test binary (`cargo qa --only rebase`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod rebase_engine;
mod rebase_hostile;
mod rebase_resurrect;
mod rebase_store;
mod rebase_ties;
