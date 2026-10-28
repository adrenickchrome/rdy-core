//! The `io` area of the rdy-qa suite: one test binary (`cargo qa --only io`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod io;
mod io_authors;
mod io_clone;
mod io_decision_basis;
mod io_dump;
mod io_dump_chain_surfaces;
mod io_dump_fixture;
mod io_dump_fixture_s11;
mod io_dump_fixture_s12;
mod io_dump_fixture_s13;
mod io_dump_fixture_s14;
mod io_dump_fixture_s15;
mod io_dump_fixture_s16;
mod io_dump_fixture_s17;
mod io_dump_fixture_s18;
mod io_dump_names;
mod io_feature_fixture;
mod io_imported_by;
mod io_keys;
mod io_link_keys;
mod io_load;
mod io_load_limit;
mod io_onboarding;
mod io_plan_metadata;
mod io_publication;
mod io_reader;
mod io_redaction_load;
mod io_sync_fixture;
mod io_tree;
