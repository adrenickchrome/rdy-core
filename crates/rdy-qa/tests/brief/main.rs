//! The `brief` area of the rdy-qa suite: one test binary (`cargo qa --only brief`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod brief_carry;
mod brief_cli;
mod brief_consequences;
mod brief_coverage;
mod brief_engine;
mod brief_entities;
mod brief_inflight;
mod brief_intents;
mod brief_mcp;
mod brief_paging;
mod brief_reach;
mod brief_rebase;
mod brief_tensions;
mod brief_write;
