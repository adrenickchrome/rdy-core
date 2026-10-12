//! The `engine_read` half of the engine area: one test binary, run by `cargo qa --only engine` with
//! the `engine` binary. It holds what the engine reports: search, graph, history, snapshots,
//! coordination state. The engine area is two binaries so that one test edit rebuilds half of it.
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod engine_areas;
mod engine_before;
mod engine_confidence;
mod engine_coord;
mod engine_coverage_carry;
mod engine_coverage_issues;
mod engine_coverage_score;
mod engine_entity_search;
mod engine_events;
mod engine_graph;
mod engine_history;
mod engine_impact;
mod engine_inbox_inflight;
mod engine_inflight;
mod engine_intents;
mod engine_lexicon;
mod engine_links;
mod engine_mention_reviews;
mod engine_pmap;
mod engine_proposal_show;
mod engine_related;
mod engine_review_queue;
mod engine_scope;
mod engine_search;
mod engine_search_coverage;
mod engine_search_filters;
mod engine_search_mentions;
mod engine_search_results;
mod engine_search_tensions;
mod engine_semantic_state;
mod engine_snapshot_threads;
mod engine_snapshots;
mod engine_suggestions;
mod engine_suspects;
mod engine_tracking;
mod engine_tree;
mod engine_values;
mod engine_view_api;
mod engine_vpos;
