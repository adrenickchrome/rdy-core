//! The `wc` area of the rdy-qa suite: one test binary (`cargo qa --only wc`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod wc_brief;
mod wc_checkout;
mod wc_commit;
mod wc_figures;
mod wc_ignore;
mod wc_layout;
mod wc_new_check;
mod wc_plan;
mod wc_plan_metadata;
mod wc_reply;
mod wc_state;
mod wc_status;
mod wc_update;
mod wc_worktree;
