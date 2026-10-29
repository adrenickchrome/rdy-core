//! The `gate` area of the rdy-qa suite: one test binary (`cargo qa --only gate`).
//! One `mod` line per test file in this directory; a file without its line never runs
//! (`tlay1_` checks). Keep the list sorted.
mod gate_alts;
mod gate_answer;
mod gate_card_assessment;
mod gate_exceptions;
mod gate_identity;
mod gate_intake_e2e;
mod gate_plan;
mod gate_plan_cards;
mod gate_readiness;
mod gate_reaffirm_cards;
mod gate_semantic;
mod gate_signing;
mod gate_tensions;
mod gate_withdrawn;
