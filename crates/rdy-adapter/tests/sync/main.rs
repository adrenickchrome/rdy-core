//! Sync tests: sync, takes, streams, links, followers, the hub contract and upgrades, grouped into
//! one test binary (one module per former root file).

mod compare_size;
mod dump_after;
mod export_walk_known;
mod follower_follow;
mod follower_seed;
mod fork_at;
mod hub_compare;
mod hub_contract;
mod hub_ports;
mod hub_renders;
mod hub_scopes;
mod hub_tests;
mod hub_world;
mod link_export;
mod link_take;
mod load_chain;
mod move_removal_reads;
mod repo_links_journey;
mod stream;
mod sync;
mod take;
mod upgrade;
