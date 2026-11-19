//! Config, rules and signing tests: policy, signatures, sessions, instructions and the agent guide,
//! grouped into one test binary (one module per former root file).

// Helpers the module files share; each file uses part of them.
#[allow(dead_code)]
#[path = "../../build/kungfu.rs"]
mod build;
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
#[allow(dead_code)]
#[path = "../../build/guide.rs"]
mod guide;
#[allow(dead_code)]
#[path = "../modes/mod.rs"]
mod modes;

mod config_edit;
mod embedded_guide;
mod instructions_config;
mod kungfu;
mod kungfu_counts;
mod lint;
mod policy_model;
mod policy_signing;
mod rule_types;
mod rules;
mod session_kind;
mod signature_check;
mod signing_crypto;
