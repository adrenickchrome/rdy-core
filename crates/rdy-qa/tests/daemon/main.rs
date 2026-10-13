//! The `daemon` area of the rdy-qa suite: one test binary (`cargo qa --only daemon`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod daemon_concurrent;
mod daemon_crash;
mod daemon_crash_m2;
mod daemon_followups;
mod daemon_notify;
mod daemon_pack_reads;
mod daemon_reload;
mod daemon_serve;
mod daemon_short_socket;
mod daemon_transport;
