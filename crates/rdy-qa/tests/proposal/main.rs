//! The `proposal` area of the rdy-qa suite: one test binary (`cargo qa --only proposal`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod proposal_coverage;
mod proposal_pending;
mod proposal_summary_show;
