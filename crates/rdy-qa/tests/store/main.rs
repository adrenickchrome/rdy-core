//! The `store` area of the rdy-qa suite: one test binary (`cargo qa --only store`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod store;
mod store_collide;
mod store_crash;
mod store_flush;
mod store_format;
mod store_fsck;
mod store_fsck3;
mod store_kinds;
mod store_land;
mod store_open;
mod store_pver;
mod store_reader;
mod store_resolve;
mod store_viewops;
