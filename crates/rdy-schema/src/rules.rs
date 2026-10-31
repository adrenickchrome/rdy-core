//! A pack's rules (D-JQD5X): each names the blocks it selects and the filter they must meet. This
//! file loads and validates them; running a rule belongs to the engine.
//!
//! Every text is bounded in bytes before its characters are counted, so a long text costs nothing
//! to refuse, and a text over the byte bound gets the same words as one over the character bound.

use crate::filter::{Expr, Field, Filter, parse};
use crate::pack::Pack;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The most rules a resolved pack holds, its parents' rules included.
pub const MAX_RULES: usize = 64;
/// The most characters of a rule's name, and the bytes that 64 of the widest characters take.
pub const MAX_RULE_NAME_CHARS: usize = 64;
const MAX_RULE_NAME_BYTES: usize = 256;
/// The most characters of a rule's message or fix, and of a decision's confirmation.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// The bytes that 2000 of the widest characters take.
pub const MAX_TEXT_BYTES: usize = 8_000;

/// The one field the landing sets on a decision: a rule that read it could pass at propose and
/// fail at landing, so it cannot name it.
const LANDING_CLOCK_FIELD: &str = "decided_at";

/// What a failing rule does. Nothing runs here: the engine gives the severity its meaning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSeverity {
    #[default]
    Advisory,
    Blocking,
}

/// One rule as the pack file writes it. `select` and `require` are filters in the query grammar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub select: String,
    pub require: String,
    #[serde(default)]
    pub severity: RuleSeverity,
    pub message: String,
    pub fix: String,
}

/// A rule with its two filters parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledRule {
    pub rule: Rule,
    pub select: Filter,
    pub require: Filter,
}

/// Whether `text` has 1 to 2000 characters once trimmed, and at most 8000 bytes.
pub(crate) fn text_fits(text: &str) -> bool {
    if text.len() > MAX_TEXT_BYTES {
        return false;
    }
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed.chars().nth(MAX_TEXT_CHARS).is_none()
}

/// The rules of a resolved `pack`, checked and parsed, in exact name order. Err: the detail of the
/// first refusal, in the pack's order of rules. `PackSet::resolve` runs this, so a pack that
/// resolves always compiles.
pub fn compile_rules(pack: &Pack) -> Result<Vec<CompiledRule>, String> {
    if pack.rules.len() > MAX_RULES {
        return Err(format!("a pack takes at most {MAX_RULES} rules"));
    }
    let mut names = BTreeSet::new();
    let mut compiled = Vec::with_capacity(pack.rules.len());
    for rule in &pack.rules {
        check_name(&rule.name)?;
        if !names.insert(rule.name.as_str()) {
            return Err(format!("rule {} is declared more than once", rule.name));
        }
        for (field, text) in [("message", &rule.message), ("fix", &rule.fix)] {
            if !text_fits(text) {
                return Err(format!(
                    "rule {}: {field} must have 1 to {MAX_TEXT_CHARS} characters",
                    rule.name
                ));
            }
        }
        compiled.push(CompiledRule {
            select: compile_filter(pack, rule, "select", &rule.select)?,
            require: compile_filter(pack, rule, "require", &rule.require)?,
            rule: rule.clone(),
        });
    }
    compiled.sort_by(|a, b| a.rule.name.cmp(&b.rule.name));
    Ok(compiled)
}

fn check_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if name.len() > MAX_RULE_NAME_BYTES
        || trimmed.is_empty()
        || trimmed.chars().nth(MAX_RULE_NAME_CHARS).is_some()
    {
        return Err(format!(
            "rule name must have 1 to {MAX_RULE_NAME_CHARS} characters"
        ));
    }
    if trimmed.len() != name.len() {
        return Err("rule name must have no leading or trailing space".to_string());
    }
    Ok(())
}

fn compile_filter(pack: &Pack, rule: &Rule, field: &str, text: &str) -> Result<Filter, String> {
    let filter = parse(text, pack).map_err(|e| format!("rule {} {field}: {e}", rule.name))?;
    if reads_landing_clock(&filter.expr) {
        return Err(format!(
            "rule {} {field}: a rule cannot read fields.{LANDING_CLOCK_FIELD}, because the landing sets it",
            rule.name
        ));
    }
    Ok(filter)
}

/// Whether a comparison or a `has` reads the field, at any nesting. The name is compared exactly,
/// as the evaluator reads it: another spelling reads no field.
fn reads_landing_clock(expr: &Expr) -> bool {
    let is_clock = |field: &Field| matches!(field, Field::Custom(n) if n == LANDING_CLOCK_FIELD);
    match expr {
        Expr::Or(all) | Expr::And(all) => all.iter().any(reads_landing_clock),
        Expr::Not(inner) => reads_landing_clock(inner),
        Expr::Linked { inner, .. } => inner.as_deref().is_some_and(reads_landing_clock),
        Expr::Compare { field, .. } | Expr::Has(field) => is_clock(field),
    }
}
