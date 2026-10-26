//! The `release` area of the rdy-qa suite: one test binary (`cargo qa --only release`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod release_contract;
mod release_install;
mod release_notices;
mod release_package;
