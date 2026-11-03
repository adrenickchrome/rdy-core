//! The `checks` area of the rdy-qa suite: one test binary (`cargo qa --only checks`). It holds
//! the M3 gate's conformance tests of the check contract (D-JQD5X, D-2GT0Z): the deterministic
//! stages, the eight agent-check cases, source facts, governing routes, neighbor consequences,
//! the read tools and the Grounded method. One `mod` line per test file in this directory; a file
//! without its line never runs (`tlay1_` checks). Keep the list sorted.
mod checks_m3;
