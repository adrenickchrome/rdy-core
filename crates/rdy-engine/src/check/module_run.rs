//! Judging one block with a module rule (D-QVTJ9, D-WV7ZS): the block's allowance, a replay from
//! the pack's verdict cache or a fresh run, the fuel charged to the rule, and the answer.
//!
//! - [`RuleBudget`]: the fuel of one rule in one change or propose scan,
//!   [`RULE_FUEL`]. Each scan makes a new one.
//! - [`judge_module`]: one selected block. Its allowance is the smaller of
//!   [`BLOCK_FUEL`] and the rule's remaining fuel. Both exported calls use it, and what
//!   they use is charged to the rule. A run out of fuel is charged its whole allowance. When the
//!   allowance was below the block's cap, the cause is the rule's fuel; at equal budgets it is the
//!   block's. Once the rule has no fuel left, every later block fails with the rule's cause, and
//!   no module runs and no input is made for it.
//!
//! A cached result replays the fuel it recorded, so the answer and the charge are an uncached
//! run's ([`VerdictCache::replay`]). A host fault is never an answer: it is the rule read error.
use crate::check::rules::{ModuleRule, VerdictCache, VerdictKey, host_fault};
use crate::error::RdyError;
use rdy_check_wasm::config::{BLOCK_FUEL, RULE_FUEL};
use rdy_check_wasm::{CheckResult, RunFailure, run_block};
use sha2::{Digest, Sha256};
use std::cell::Cell;

/// The fuel one rule has left in one change or propose scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleBudget {
    remaining: u64,
}

impl RuleBudget {
    /// A new scan's budget: [`RULE_FUEL`].
    pub fn new() -> RuleBudget {
        RuleBudget {
            remaining: RULE_FUEL,
        }
    }

    /// The fuel left.
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// The fuel the next block may use: the smaller of [`BLOCK_FUEL`] and the fuel left.
    pub fn allowance(&self) -> u64 {
        self.remaining.min(BLOCK_FUEL)
    }
}

impl Default for RuleBudget {
    fn default() -> RuleBudget {
        RuleBudget::new()
    }
}

/// Why a module could not judge a block: the closed causes of D-WV7ZS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The module trapped, in the interpreter's words.
    Trap(String),
    /// The block used all of [`BLOCK_FUEL`].
    BlockFuel,
    /// The rule used all of [`RULE_FUEL`] in this scan.
    RuleFuel,
    /// The judged block alone makes the input larger than [`rdy_check_wasm::config::INPUT_BYTES_MAX`].
    InputTooLarge,
    /// The output is larger than [`rdy_check_wasm::config::OUTPUT_BYTES_MAX`].
    OutputTooLarge,
    /// The output is not a check result: why.
    BadOutput(String),
}

impl Cause {
    /// The cause's words, as `rule <name> could not judge <id>: <cause>` ends.
    pub fn words(&self) -> String {
        match self {
            Cause::Trap(t) => format!("the module trapped: {t}"),
            Cause::BlockFuel => format!("the module used all of its {BLOCK_FUEL} fuel"),
            Cause::RuleFuel => format!("the rule used all of its {RULE_FUEL} fuel in this run"),
            Cause::InputTooLarge => "the input is over 1 MiB".to_string(),
            Cause::OutputTooLarge => "the output is over 64 KiB".to_string(),
            Cause::BadOutput(d) => format!("the output is not a check result: {d}"),
        }
    }
}

/// A module rule's answer for one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Judgement {
    Pass,
    /// The block fails, with the module's message and fix when it gave them; the rule's own
    /// stand in for those it left out.
    Fail {
        message: Option<String>,
        fix: Option<String>,
    },
    /// The module could not judge the block, which fails it.
    CannotJudge(Cause),
}

thread_local! {
    static MODULE_RUNS: Cell<u64> = const { Cell::new(0) };
}

/// Tests only: how many module runs the calling thread has started, cache replays not counted.
#[doc(hidden)]
pub fn module_runs_on_this_thread_for_tests() -> u64 {
    MODULE_RUNS.with(Cell::get)
}

/// Judges one selected block with rule `rule`'s module `m`, charging `budget`. `input` makes the
/// block's input envelope (`None`: the judged block alone is over 1 MiB); it is called only when
/// the rule has fuel left. A result is taken from `cache` when it replays under the block's
/// allowance, else the module runs and its result is recorded. Err: a host fault, the rule read
/// error; nothing is charged or recorded for it.
pub fn judge_module(
    rule: &str,
    m: &ModuleRule,
    cache: &VerdictCache,
    budget: &mut RuleBudget,
    input: impl FnOnce() -> Option<Vec<u8>>,
) -> Result<Judgement, RdyError> {
    if budget.remaining == 0 {
        return Ok(Judgement::CannotJudge(Cause::RuleFuel));
    }
    let Some(bytes) = input() else {
        return Ok(Judgement::CannotJudge(Cause::InputTooLarge));
    };
    let allowance = budget.allowance();
    let key = VerdictKey {
        rule: rule.to_string(),
        module: *m.sha256(),
        input: Sha256::digest(&bytes).into(),
    };
    let outcome = match cache.replay(&key, allowance) {
        Some(outcome) => outcome,
        None => {
            MODULE_RUNS.with(|n| n.set(n.get().wrapping_add(1)));
            let outcome = run_block(m.module(), &bytes, allowance);
            if outcome.result == Err(RunFailure::HostFault) {
                return Err(host_fault());
            }
            cache.record(&key, allowance, &outcome);
            outcome
        }
    };
    budget.remaining = budget.remaining.saturating_sub(outcome.fuel.min(allowance));
    Ok(match outcome.result {
        Ok(CheckResult::Pass) => Judgement::Pass,
        Ok(CheckResult::Fail { message, fix }) => Judgement::Fail { message, fix },
        Err(RunFailure::OutOfFuel) if allowance < BLOCK_FUEL => {
            Judgement::CannotJudge(Cause::RuleFuel)
        }
        Err(RunFailure::OutOfFuel) => Judgement::CannotJudge(Cause::BlockFuel),
        Err(RunFailure::Trap(t)) => Judgement::CannotJudge(Cause::Trap(t)),
        Err(RunFailure::InputTooLarge) => Judgement::CannotJudge(Cause::InputTooLarge),
        Err(RunFailure::OutputTooLarge) => Judgement::CannotJudge(Cause::OutputTooLarge),
        Err(RunFailure::BadOutput(d)) => Judgement::CannotJudge(Cause::BadOutput(d)),
        Err(RunFailure::HostFault) => return Err(host_fault()),
    })
}
