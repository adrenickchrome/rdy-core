//! The `cli` area of the rdy-qa suite: one test binary (`cargo qa --only cli`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod cli;
mod cli_alts;
mod cli_blame;
mod cli_config;
mod cli_config_file;
mod cli_coverage;
mod cli_daemon;
mod cli_dump;
mod cli_entities;
mod cli_inbox;
mod cli_intake;
mod cli_intents;
mod cli_keys;
mod cli_log;
mod cli_output;
mod cli_proposal;
mod cli_quarantine;
mod cli_ref;
mod cli_relay;
mod cli_search;
mod cli_search_filters;
mod cli_semantic;
mod cli_site;
mod cli_state;
mod cli_status_views;
mod cli_suspects;
mod cli_tag_relay;
mod cli_tensions;
mod cli_views;
