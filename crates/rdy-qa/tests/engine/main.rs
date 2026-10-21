//! The `engine` area of the rdy-qa suite: one test binary (`cargo qa --only engine`), with writes,
//! the gate, keys, pins, sessions and merges. What the engine reports (search, graph, history,
//! snapshots, coordination state) is in the `engine_read` binary; `--only engine` runs both.
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod engine;
mod engine_alts;
mod engine_autokey;
mod engine_commute;
mod engine_decision_home;
mod engine_demo_write;
mod engine_eff;
mod engine_gate;
mod engine_gate_state;
mod engine_keys;
mod engine_keys_assign;
mod engine_merge;
mod engine_pending_inbox;
mod engine_pins;
mod engine_port;
mod engine_priv;
mod engine_pver;
mod engine_reaffirm;
mod engine_repin;
mod engine_retype;
mod engine_semantic_accept;
mod engine_session;
mod engine_signature_check;
mod engine_splitbrain;
mod engine_supersede;
mod engine_write;
mod engine_write_extras;
