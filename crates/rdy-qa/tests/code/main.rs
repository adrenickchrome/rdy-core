//! The `code` area of the rdy-qa suite: one test binary (`cargo qa --only code`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod code_brief;
mod code_cli;
mod code_index;
mod code_mcp;
mod code_provisional;
mod code_reflects;
