//! The `authority` area of the rdy-qa suite: one test binary (`cargo qa --only authority`). It
//! drives each built approval route (a relayed `rdy_answer`, an elicited answer, the simulated
//! person's local UI, `rdy answer` and `rdy approve`) over one fixture recipe and compares what
//! each route stores. One `mod` line per test file in this directory; a file without its line
//! never runs (`tlay1_` checks). Keep the list sorted.
mod authority_routes;
