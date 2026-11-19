//! Plan, proposal and landing tests: plans, gates, supporting docs, reflects links, briefs and
//! checks, grouped into one test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
#[allow(dead_code)]
#[path = "../support/plan_frames.rs"]
mod plan_frames;

mod brief_refines;
mod briefing_record;
mod check_cases;
mod coverage_gate_config;
mod decision_fields;
mod gate;
mod governing_routes;
mod landing_order;
mod plan_affects;
mod plan_api;
mod plan_asbuilt;
mod plan_group;
mod plan_tension;
mod reflects_history;
mod reflects_tensions;
mod reflects_write;
mod review_citations;
mod supporting_cards;
mod supporting_plan;
mod supporting_write;
mod trace;
