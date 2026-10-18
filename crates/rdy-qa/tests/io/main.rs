//! The `io` area of the rdy-qa suite: one test binary (`cargo qa --only io`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod io;
mod io_authors;
mod io_clone;
mod io_dump;
mod io_dump_fixture;
mod io_dump_fixture_s11;
mod io_dump_fixture_s12;
mod io_dump_fixture_s13;
mod io_imported_by;
mod io_keys;
mod io_link_keys;
mod io_load;
mod io_onboarding;
mod io_publication;
mod io_reader;
mod io_redaction_load;
mod io_tree;
