//! The `mcp` area of the rdy-qa suite: one test binary (`cargo qa --only mcp`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod mcp;
mod mcp_accept;
mod mcp_alts;
mod mcp_compact;
mod mcp_decision_home;
mod mcp_history;
mod mcp_instructions;
mod mcp_intents;
mod mcp_log;
mod mcp_mentions;
mod mcp_params;
mod mcp_people;
mod mcp_pool;
mod mcp_proposal;
mod mcp_proxy;
mod mcp_reach;
mod mcp_read;
mod mcp_read_paging;
mod mcp_ref;
mod mcp_schema;
mod mcp_schema_rows;
mod mcp_search;
mod mcp_search_brief;
mod mcp_state;
mod mcp_tensions;
mod mcp_view_brief;
mod mcp_views;
