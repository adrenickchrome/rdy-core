//! The `release` area of the rdy-qa suite: one test binary (`cargo qa --only release`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod release_contract;
mod release_install;
mod release_install_unix;
mod release_keys;
mod release_notices;
mod release_notices_unix;
mod release_one;
mod release_package;
mod release_package_unix;
mod release_repo_links_contract;
mod release_repo_links_formats;
mod release_sample;
