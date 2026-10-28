//! The `invariants` area of the rdy-qa suite: one test binary (`cargo qa --only invariants`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod invariants;
mod invariants_inbox;
mod invariants_landing;
mod invariants_outside_doc;
mod invariants_redaction;
mod invariants_reindex;
mod invariants_wc;
