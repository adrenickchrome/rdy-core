//! Pack rule checks (D-JQD5X, D-ZRXY0): a rule judges each block that matches its `select`. A
//! filter rule fails on a block that does not match its `require`; a module rule runs its module
//! once on the block ([`judge_module`]), which answers pass or fail, and a module that cannot
//! answer fails the block. The filters are M3f's ([`rdy_schema::filter`]); the evaluator is
//! [`FilterContext`]. This file adds no parser and no evaluator: it runs each rule over the
//! blocks a stage names and words each failure. Both kinds give the same rows, codes and caps.
//!
//! - At `change` ([`change_checks`]) a rule judges the blocks a write or view op changed and the
//!   blocks one explicit link or pinned reference away, before or after. It never refuses.
//! - At `propose` (the readiness of `pending::queue`) a rule judges every block of the version's
//!   landing projection. A blocking failure on stable content is a readiness issue.
//!
//! [`rule_reach`] is the dependency walk that widens an assessment's `reach` (D-DSPMW, D-AJ0XQ):
//! the blocks a rule's result can read, so a landing that changes one of them stales the
//! assessment and nothing reruns a rule at landing.
//!
//! A loaded pack's rules are compiled once, by [`compile_rule_specs`], into [`CompiledRules`]: each
//! rule's filters parsed and each module rule's module compiled (D-QVTJ9). The handle travels with
//! the pack in every state published under it, and owns a [`VerdictCache`] of that pack's module
//! results. A reload compiles the new pack's rules into a new handle with an empty cache.
use crate::brief::effective_stable;
use crate::check::module_input::{ModuleState, encode_input, module_reads};
use crate::check::module_run::{Judgement, RuleBudget, judge_module};
use crate::check::stages::Stage;
use crate::error::{E_CONFIG, E_INTERNAL, E_RULE_CHECK, RdyError, W_RULE_CHECK};
use crate::filter::{FilterContext, FilterInputs};
use crate::materialize::Materialized;
use crate::tensions::index::duplicate_eligible;
use crate::{ReadState, Scope};
use rdy_check_wasm::{CheckResult, CompileFailure, CompiledModule, RunFailure, RunOutcome};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::BlockId;
use rdy_model::assessment::MAX_ROWS;
use rdy_schema::filter::{Dir, Expr, Field, Filter, LinkKind};
use rdy_schema::{
    ArgValue, CompiledRule, ModuleTest, RuleInput, RuleSeverity, RuleTestSpec, check_resolved_rules,
};
use rdy_schema::{Issue, Pack, Severity};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The most failure rows a scan keeps of each kind it keeps (at `propose`: blocking and
/// advisory each; at `change`: both together). The rest are counted.
pub const MAX_RULE_ROWS: usize = 256;

/// One failure of one rule on one block, as `on_change.checks` gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RuleIssue {
    pub rule: String,
    /// `E_RULE_CHECK` or `W_RULE_CHECK`.
    pub code: String,
    pub block: String,
    /// `rule <name> fails on <id>: <message>`.
    pub message: String,
    /// The rule's own `fix`.
    pub fix: String,
}

impl RuleIssue {
    /// The row as a readiness issue or warning, by its code.
    pub fn issue(&self) -> Issue {
        let blocking = self.code == E_RULE_CHECK;
        Issue {
            severity: if blocking {
                Severity::Error
            } else {
                Severity::Warning
            },
            code: if blocking { E_RULE_CHECK } else { W_RULE_CHECK },
            block: BlockId::parse(&self.block).ok(),
            message: self.message.clone(),
            fix: self.fix.clone(),
        }
    }
}

/// What one scan found: the kept rows by kind, in rule name then block id order, and how many
/// rows past the cut each kind left out. `read_blocks`: at `propose`, every block the failing
/// rows read (their complete rule inputs, the rows past the cut included), for the assessment's
/// reach; never stored as a record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleScan {
    pub blocking: Vec<RuleIssue>,
    pub warnings: Vec<RuleIssue>,
    pub blocking_omitted: usize,
    pub warnings_omitted: usize,
    pub read_blocks: BTreeSet<BlockId>,
}

impl RuleScan {
    /// The kept rows of both kinds in the stated order: rule name, then block id.
    pub fn rows(&self) -> Vec<RuleIssue> {
        let mut rows: Vec<RuleIssue> = self
            .blocking
            .iter()
            .chain(&self.warnings)
            .cloned()
            .collect();
        rows.sort_by(|a, b| (&a.rule, &a.block).cmp(&(&b.rule, &b.block)));
        rows
    }

    /// The rows left out, of both kinds.
    pub fn omitted(&self) -> usize {
        self.blocking_omitted.saturating_add(self.warnings_omitted)
    }
}

thread_local! {
    /// This thread's rule scans (see `rule_scans_for_tests`).
    static RULE_SCANS: Cell<u64> = const { Cell::new(0) };
    /// Tests only: the next change-stage report on this thread fails.
    static FAIL_NEXT_REPORT: Cell<bool> = const { Cell::new(false) };
}

/// Tests only: this thread's scans of [`evaluate_rules`] that judged at least one rule.
#[doc(hidden)]
pub fn rule_scans_for_tests() -> u64 {
    RULE_SCANS.with(Cell::get)
}

/// Tests only: the next change-stage rule report on this thread fails with `E_INTERNAL`, as a
/// read error would; the write stands.
#[doc(hidden)]
pub fn fail_next_rule_report_for_tests() {
    FAIL_NEXT_REPORT.with(|f| f.set(true));
}

/// Tests only: the next module run on this thread faults inside the host, as a fault of rdy
/// would; the scan gives the rule read error ([`host_fault`]).
#[doc(hidden)]
pub fn host_fault_in_next_module_run_for_tests() {
    rdy_check_wasm::panic_in_next_run_for_tests(rdy_check_wasm::TestPanic::Check);
}

static RULE_COMPILATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static THREAD_RULE_COMPILATIONS: Cell<usize> = const { Cell::new(0) };
    static THREAD_MODULE_COMPILATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Tests only: how many times this process has compiled a set of rules ([`compile_rule_specs`]),
/// whatever the project.
#[doc(hidden)]
pub fn rule_compilations_for_tests() -> usize {
    RULE_COMPILATIONS.load(Ordering::Relaxed)
}

/// Tests only: [`rule_compilations_for_tests`] counted on the calling thread only.
#[doc(hidden)]
pub fn rule_compilations_on_this_thread_for_tests() -> usize {
    THREAD_RULE_COMPILATIONS.with(Cell::get)
}

/// Tests only: how many check modules the calling thread has compiled.
#[doc(hidden)]
pub fn module_compilations_on_this_thread_for_tests() -> usize {
    THREAD_MODULE_COMPILATIONS.with(Cell::get)
}

/// A module rule's test, compiled: the module, the SHA-256 of its decoded bytes, their length,
/// and the `args` and `depth` it runs with (an absent `args` is the empty mapping, an absent
/// `depth` [`rdy_schema::DEFAULT_DEPTH`]). The decoded bytes are not kept.
pub struct ModuleRule {
    module: CompiledModule,
    sha256: [u8; 32],
    bytes: usize,
    args: BTreeMap<String, ArgValue>,
    depth: u8,
}

impl ModuleRule {
    /// The compiled module.
    pub fn module(&self) -> &CompiledModule {
        &self.module
    }

    /// The SHA-256 of the decoded module bytes.
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// The decoded module's length in bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The interface number of the module's exports.
    pub fn interface(&self) -> u32 {
        self.module.interface()
    }

    /// The `args` the module receives.
    pub fn args(&self) -> &BTreeMap<String, ArgValue> {
        &self.args
    }

    /// The link steps of neighbours the module sees.
    pub fn depth(&self) -> u8 {
        self.depth
    }
}

/// What a compiled rule tests on each block its `select` matches.
pub enum CompiledRuleTest {
    /// The parsed `require` filter.
    Filter(Filter),
    Module(ModuleRule),
}

/// One rule, its filters parsed and its module compiled.
pub struct PreparedRule {
    pub name: String,
    pub select: Filter,
    pub test: CompiledRuleTest,
    pub severity: RuleSeverity,
    pub message: String,
    pub fix: String,
}

impl PreparedRule {
    /// The rule's filters: its `select`, and a filter rule's `require`.
    fn filters(&self) -> impl Iterator<Item = &Filter> {
        let require = match &self.test {
            CompiledRuleTest::Filter(require) => Some(require),
            CompiledRuleTest::Module(_) => None,
        };
        std::iter::once(&self.select).chain(require)
    }

    /// The most link steps the rule reads from a judged block (D-DSPMW): its deepest link test,
    /// and a module rule's `depth`.
    fn depth(&self) -> usize {
        let links = self
            .filters()
            .map(|f| link_depth(&f.expr))
            .max()
            .unwrap_or(0);
        match &self.test {
            CompiledRuleTest::Module(m) => links.max(usize::from(m.depth())),
            CompiledRuleTest::Filter(_) => links,
        }
    }
}

/// The rules of one loaded pack, compiled once ([`compile_rule_specs`]), in name order, with the
/// cache of that pack's module results. Every state published under the pack shares it; a
/// reload replaces it with the new pack's, and its cache is never carried over.
pub struct CompiledRules {
    rules: Vec<PreparedRule>,
    cache: VerdictCache,
}

impl CompiledRules {
    /// No rules: a holder's, or a pack's that has none.
    pub fn empty() -> CompiledRules {
        CompiledRules {
            rules: vec![],
            cache: VerdictCache::new(),
        }
    }

    /// Every rule, in name order.
    pub fn rules(&self) -> &[PreparedRule] {
        &self.rules
    }

    /// Whether there is no rule.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Every filter of every rule: each `select`, and each filter rule's `require`. What they
    /// need (`Filter::needs`) is every global fact the rules read: a module reads none.
    pub(crate) fn filters(&self) -> Vec<&Filter> {
        self.rules.iter().flat_map(PreparedRule::filters).collect()
    }

    /// The cache of this pack's module results.
    pub fn cache(&self) -> &VerdictCache {
        &self.cache
    }
}

/// The rules of the resolved `pack`, compiled ([`compile_rule_specs`]).
pub(crate) fn compile_pack_rules(pack: &Pack) -> Result<Arc<CompiledRules>, RdyError> {
    compile_rule_specs(pack, &pack.rules).map(Arc::new)
}

/// The rules `specs` of the resolved `pack`, the parent's first as the resolved pack lists them,
/// checked ([`check_resolved_rules`]), then each module compiled, in name order. Compiling
/// validates and translates every function body; it runs no module. Err: the first refusal, as
/// `E_CONFIG` `invalid pack: <detail>` with the fix `fix the pack file`; a panic inside the
/// interpreter is the host fault's `E_INTERNAL`.
pub fn compile_rule_specs(pack: &Pack, specs: &[RuleInput]) -> Result<CompiledRules, RdyError> {
    RULE_COMPILATIONS.fetch_add(1, Ordering::Relaxed);
    THREAD_RULE_COMPILATIONS.with(|n| n.set(n.get().wrapping_add(1)));
    let checked = check_resolved_rules(pack, specs)
        .map_err(|e| RdyError::new(E_CONFIG, e.to_string(), "fix the pack file"))?;
    let mut rules = Vec::with_capacity(checked.len());
    for CompiledRule {
        rule,
        select,
        require,
    } in checked
    {
        let RuleInput {
            name,
            test,
            severity,
            message,
            fix,
            ..
        } = rule;
        let test = match (test, require) {
            (RuleTestSpec::Filter { .. }, Some(require)) => CompiledRuleTest::Filter(require),
            (RuleTestSpec::Module(m), None) => {
                CompiledRuleTest::Module(compile_module_rule(&name, m)?)
            }
            _ => {
                return Err(RdyError::new(
                    E_INTERNAL,
                    format!("rule {name} was checked without its test"),
                    "report this as a bug in rdy",
                ));
            }
        };
        rules.push(PreparedRule {
            name,
            select,
            test,
            severity,
            message,
            fix,
        });
    }
    Ok(CompiledRules {
        rules,
        cache: VerdictCache::new(),
    })
}

/// The module rule `name`'s test `m`, compiled.
fn compile_module_rule(name: &str, m: ModuleTest) -> Result<ModuleRule, RdyError> {
    THREAD_MODULE_COMPILATIONS.with(|n| n.set(n.get().wrapping_add(1)));
    let module = rdy_check_wasm::compile_module(&m.module).map_err(|f| compile_refusal(name, f))?;
    Ok(ModuleRule {
        module,
        sha256: Sha256::digest(&m.module).into(),
        bytes: m.module.len(),
        args: m.args.unwrap_or_default(),
        depth: m.depth.unwrap_or(rdy_schema::DEFAULT_DEPTH),
    })
}

/// The refusal of rule `name`'s module (D-WV7ZS): `invalid pack: <detail>`, or the host fault.
fn compile_refusal(name: &str, failure: CompileFailure) -> RdyError {
    use rdy_check_wasm::config::{MEMORY_PAGES_MAX, TABLE_ELEMENTS_MAX};
    let detail = match failure {
        CompileFailure::Invalid(e) => format!("rule {name} module: {e}"),
        CompileFailure::Import { module, name: item } => {
            format!("rule {name} module imports {module}.{item}; a check module imports nothing")
        }
        CompileFailure::MissingExport { name: export, ty } => {
            format!("rule {name} module lacks the export {export} of type {ty}")
        }
        CompileFailure::StartFunction => {
            format!("rule {name} module has a start function; a check module has none")
        }
        CompileFailure::MemoryMinimum => {
            format!("rule {name} module memory starts above {MEMORY_PAGES_MAX} pages")
        }
        CompileFailure::Tables => format!("rule {name} module has more than one table"),
        CompileFailure::TableElements => {
            format!("rule {name} module table exceeds {TABLE_ELEMENTS_MAX} elements")
        }
        CompileFailure::HostFault => return host_fault(),
    };
    RdyError::new(
        E_CONFIG,
        format!("invalid pack: {detail}"),
        "fix the pack file",
    )
}

/// A caught fault of rdy inside the interpreter (D-WV7ZS): never a row, and no panic payload.
pub(crate) fn host_fault() -> RdyError {
    RdyError::new(
        E_INTERNAL,
        "the rule checks could not be read: check module host fault",
        "report this as a bug in rdy",
    )
}

/// The most bytes of entries a [`VerdictCache`] keeps.
pub const VERDICT_CACHE_BYTES: usize = 67_108_864;

/// What a cached module result is keyed by: the rule's name, the SHA-256 of its decoded module
/// and the SHA-256 of the block's canonical input.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerdictKey {
    pub rule: String,
    pub module: [u8; 32],
    pub input: [u8; 32],
}

/// A recorded module result.
enum Recorded {
    /// The run ended with a result, a trap or bad output, using `fuel`.
    Completed {
        result: Result<CheckResult, RunFailure>,
        fuel: u64,
    },
    /// The run used all of `allowance`.
    OutOfFuel { allowance: u64 },
}

struct CacheEntry {
    /// When the entry was retained: the oldest goes first.
    age: u64,
    recorded: Recorded,
    bytes: usize,
}

#[derive(Default)]
struct CacheInner {
    entries: BTreeMap<VerdictKey, CacheEntry>,
    by_age: BTreeMap<u64, VerdictKey>,
    next_age: u64,
    bytes: usize,
}

/// The module results of one loaded pack (the O6 ruling), each with the fuel it used. A replay
/// gives the result and fuel an uncached run would give under the current allowance, so rows,
/// causes, omitted counts and rule budgets are the same with or without it. It holds at most
/// [`VERDICT_CACHE_BYTES`] of entries ([`VerdictCache::entry_bytes`]) and evicts the oldest first;
/// an evicted result is run again, never judged differently. A host fault and an input over the
/// cap are never cached.
pub struct VerdictCache {
    inner: Mutex<CacheInner>,
}

impl VerdictCache {
    fn new() -> VerdictCache {
        VerdictCache {
            inner: Mutex::new(CacheInner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cached outcome of `key` under `allowance`, the fuel the block may use. A completed
    /// result that used F fuel applies when `allowance` is at least F, and charges F. A run that
    /// used all of its allowance A applies when `allowance` is at most A, and charges all of
    /// `allowance`. Otherwise `None`: the module runs again.
    pub fn replay(&self, key: &VerdictKey, allowance: u64) -> Option<RunOutcome> {
        let inner = self.lock();
        match &inner.entries.get(key)?.recorded {
            Recorded::Completed { result, fuel } if allowance >= *fuel => Some(RunOutcome {
                result: result.clone(),
                fuel: *fuel,
            }),
            Recorded::OutOfFuel { allowance: at } if allowance <= *at => Some(RunOutcome {
                result: Err(RunFailure::OutOfFuel),
                fuel: allowance,
            }),
            _ => None,
        }
    }

    /// Records the `outcome` of `key`, run under `allowance`, in place of any entry `key` had,
    /// evicting the oldest entries until it fits. Whether it was kept: never for a host fault,
    /// an input over the cap, or an entry larger than the whole cache. The size is checked before
    /// anything is copied.
    pub fn record(&self, key: &VerdictKey, allowance: u64, outcome: &RunOutcome) -> bool {
        let Some(bytes) = VerdictCache::entry_bytes(key, outcome) else {
            return false;
        };
        if bytes > VERDICT_CACHE_BYTES {
            return false;
        }
        let mut inner = self.lock();
        if let Some(old) = inner.entries.remove(key) {
            inner.by_age.remove(&old.age);
            inner.bytes -= old.bytes;
        }
        while inner.bytes + bytes > VERDICT_CACHE_BYTES {
            let Some((_, oldest)) = inner.by_age.pop_first() else {
                break;
            };
            if let Some(old) = inner.entries.remove(&oldest) {
                inner.bytes -= old.bytes;
            }
        }
        let recorded = match &outcome.result {
            Err(RunFailure::OutOfFuel) => Recorded::OutOfFuel { allowance },
            result => Recorded::Completed {
                result: result.clone(),
                fuel: outcome.fuel,
            },
        };
        let age = inner.next_age;
        inner.next_age += 1;
        inner.by_age.insert(age, key.clone());
        inner.entries.insert(
            key.clone(),
            CacheEntry {
                age,
                recorded,
                bytes,
            },
        );
        inner.bytes += bytes;
        true
    }

    /// The bytes an entry of `key` and `outcome` counts against [`VERDICT_CACHE_BYTES`]: the
    /// rule name's bytes, the two 32-byte digests, and the bytes of the texts the outcome holds
    /// (a failure's message and fix, a trap's words, a bad output's detail). `None`: the outcome
    /// is never cached.
    pub fn entry_bytes(key: &VerdictKey, outcome: &RunOutcome) -> Option<usize> {
        let text = match &outcome.result {
            Err(RunFailure::HostFault | RunFailure::InputTooLarge) => return None,
            Ok(CheckResult::Pass) | Err(RunFailure::OutOfFuel | RunFailure::OutputTooLarge) => 0,
            Ok(CheckResult::Fail { message, fix }) => {
                message.as_ref().map_or(0, String::len) + fix.as_ref().map_or(0, String::len)
            }
            Err(RunFailure::Trap(t) | RunFailure::BadOutput(t)) => t.len(),
        };
        Some(key.rule.len() + 64 + text)
    }

    /// The bytes of the entries kept.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    /// The entries kept.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether no entry is kept.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Runs every rule of `pack` on each of `ids` that `inputs.m` holds, at `stage`, its rules
/// compiled for this call ([`compile_rule_specs`]). The scan is [`evaluate`]'s. Rules never run
/// at `land`.
pub fn evaluate_rules(
    pack: &Pack,
    inputs: FilterInputs<'_>,
    ids: impl Iterator<Item = BlockId>,
    stage: Stage,
) -> Result<RuleScan, RdyError> {
    refuse_land(stage)?;
    evaluate(&*compile_pack_rules(pack)?, inputs, ids, stage)
}

/// Rules never run at `land`.
fn refuse_land(stage: Stage) -> Result<(), RdyError> {
    if stage == Stage::Land {
        return Err(RdyError::new(
            E_INTERNAL,
            "pack rules run at change and propose only",
            "report this as a bug in rdy",
        ));
    }
    Ok(())
}

/// One rule's answer on one block that fails it: the row's message and fix.
struct Failure {
    message: String,
    fix: String,
}

/// Rule `rule`'s answer on block `id`, which its `select` matched: `None` when the block passes.
/// A module rule runs once on the block, charging `budget` ([`judge_module`]); its input comes
/// from `st`. Err: a host fault, the rule read error.
fn judge(
    rule: &PreparedRule,
    ctx: &FilterContext<'_>,
    st: &ModuleState<'_>,
    cache: &VerdictCache,
    budget: &mut RuleBudget,
    id: &BlockId,
) -> Result<Option<Failure>, RdyError> {
    let fails_on = |message: &str| format!("rule {} fails on {id}: {message}", rule.name);
    let m = match &rule.test {
        CompiledRuleTest::Filter(require) => {
            return Ok((!ctx.matches(require, id)).then(|| Failure {
                message: fails_on(&rule.message),
                fix: rule.fix.clone(),
            }));
        }
        CompiledRuleTest::Module(m) => m,
    };
    let input = || encode_input(st, &rule.name, m.args(), id, m.depth()).ok();
    Ok(match judge_module(&rule.name, m, cache, budget, input)? {
        Judgement::Pass => None,
        Judgement::Fail { message, fix } => Some(Failure {
            message: fails_on(message.as_deref().unwrap_or(&rule.message)),
            fix: fix.unwrap_or_else(|| rule.fix.clone()),
        }),
        Judgement::CannotJudge(cause) => Some(Failure {
            message: format!("rule {} could not judge {id}: {}", rule.name, cause.words()),
            fix: format!(
                "fix the module of rule {} in the pack file, or make the rule advisory",
                rule.name
            ),
        }),
    })
}

/// Runs each of the compiled `rules`, in name order, on each of `ids` that `inputs.m` holds and
/// the rule's `select` matches, in block id order, at `stage`. A filter rule fails a block that
/// its `require` does not match; a module rule runs its module once on the block, with a new
/// fuel budget for each rule in each scan, and fails a block it answers fail on or cannot judge.
/// A row's code is `E_RULE_CHECK` when the rule is blocking and the block is in effective stable
/// content of `inputs.m`, else `W_RULE_CHECK`. At `propose` the first [`MAX_RULE_ROWS`] blocking
/// rows and the first [`MAX_RULE_ROWS`] warning rows are kept; at `change`, the first
/// [`MAX_RULE_ROWS`] rows of both. Every selected block is judged; the rows past the cut are
/// counted. One [`FilterContext`] and one module state judge the one state; they live inside
/// this call only. Err: a host fault in a module, the rule read error. Rules never run at `land`.
pub(crate) fn evaluate(
    rules: &CompiledRules,
    inputs: FilterInputs<'_>,
    ids: impl Iterator<Item = BlockId>,
    stage: Stage,
) -> Result<RuleScan, RdyError> {
    refuse_land(stage)?;
    let mut scan = RuleScan::default();
    if rules.is_empty() {
        return Ok(scan);
    }
    RULE_SCANS.with(|n| n.set(n.get() + 1));
    let ids: BTreeSet<BlockId> = ids.collect();
    let (pack, m, graph, areas) = (inputs.pack, inputs.m, inputs.graph, inputs.areas);
    let st = ModuleState::new(pack, m, graph, areas);
    let ctx = FilterContext::from_parts(inputs);
    let mut failing: Vec<(usize, BlockId)> = vec![];
    for (at, rule) in rules.rules().iter().enumerate() {
        let mut budget = RuleBudget::new();
        for id in &ids {
            if !ctx.matches(&rule.select, id) {
                continue;
            }
            let Some(failure) = judge(rule, &ctx, &st, rules.cache(), &mut budget, id)? else {
                continue;
            };
            let blocking = rule.severity == RuleSeverity::Blocking && effective_stable(m, Some(id));
            let row = RuleIssue {
                rule: rule.name.clone(),
                code: if blocking { E_RULE_CHECK } else { W_RULE_CHECK }.to_string(),
                block: id.to_string(),
                message: failure.message,
                fix: failure.fix,
            };
            let room = match stage {
                Stage::Change => scan.blocking.len() + scan.warnings.len() < MAX_RULE_ROWS,
                _ if blocking => scan.blocking.len() < MAX_RULE_ROWS,
                _ => scan.warnings.len() < MAX_RULE_ROWS,
            };
            match (room, blocking) {
                (true, true) => scan.blocking.push(row),
                (true, false) => scan.warnings.push(row),
                (false, true) => scan.blocking_omitted += 1,
                (false, false) => scan.warnings_omitted += 1,
            }
            // the cut bounds the rows shown, never the reach: a row past it is read too
            if stage == Stage::Propose {
                failing.push((at, id.clone()));
            }
        }
    }
    // the complete inputs of each failing row, whatever its predicates short-circuited (D-AJ0XQ)
    if stage == Stage::Propose {
        let mut read = Bounded::unbounded();
        for (at, id) in &failing {
            // an unbounded set never refuses
            let _ = inputs_of(
                std::slice::from_ref(&rules.rules()[*at]),
                m,
                graph,
                id,
                &mut read,
            );
        }
        scan.read_blocks = read.set;
    }
    Ok(scan)
}

/// The change stage's rule checks of a write or a view op in `view`, from the state `before`
/// to the state `after`: the blocks `changed` names that the view's materialization changed,
/// and the blocks one explicit link or pinned reference away from them in either state, in
/// either direction (parent and child placement is no link), judged in `after`. A deleted block
/// gives no row; its neighbours can. `full`: the op moved the view so that only a full diff
/// tells what changed. A pack with no rules returns an empty scan and reads nothing.
pub(crate) fn change_checks(
    before: &ReadState,
    after: &ReadState,
    view: &str,
    changed: &BTreeSet<BlockId>,
    full: bool,
) -> Result<RuleScan, RdyError> {
    if FAIL_NEXT_REPORT.with(|f| f.replace(false)) {
        return Err(RdyError::new(
            E_INTERNAL,
            "the rule checks could not be read (test)",
            "report this as a bug in rdy",
        ));
    }
    let rules = after.compiled_rules()?;
    if rules.is_empty() {
        return Ok(RuleScan::default());
    }
    let scope = Scope::View(view.to_string());
    let (bm, am) = (before.materialized(&scope)?, after.materialized(&scope)?);
    let differs = |id: &BlockId| bm.version_of(id) != am.version_of(id);
    let written: BTreeSet<BlockId> = if full {
        bm.ids()
            .chain(am.ids())
            .filter(|id| differs(id))
            .cloned()
            .collect()
    } else {
        changed.iter().filter(|id| differs(id)).cloned().collect()
    };
    if written.is_empty() {
        return Ok(RuleScan::default());
    }
    let (bg, ag) = (before.graph(&scope)?, after.graph(&scope)?);
    let mut ids = written.clone();
    for g in [&*bg, &*ag] {
        for id in &written {
            ids.extend(
                g.outgoing(id)
                    .iter()
                    .filter(|e| is_link(&e.kind))
                    .map(|e| e.to.clone()),
            );
            ids.extend(
                g.incoming(id)
                    .iter()
                    .filter(|e| is_link(&e.kind))
                    .map(|e| e.from.clone()),
            );
        }
    }
    let filters = rules.filters();
    let inputs = after.scope_inputs(&scope, &filters)?;
    evaluate(&rules, inputs.inputs(after), ids.into_iter(), Stage::Change)
}

/// An explicit pack link or a pinned reference: what a rule's one-link reach counts.
fn is_link(kind: &EdgeKind) -> bool {
    matches!(kind, EdgeKind::Link(_) | EdgeKind::References)
}

/// A set of block ids that refuses its `cap + 1`th distinct id.
pub(crate) struct Bounded {
    pub(crate) set: BTreeSet<BlockId>,
    cap: usize,
}

impl Bounded {
    pub(crate) fn new(cap: usize) -> Bounded {
        Bounded {
            set: BTreeSet::new(),
            cap,
        }
    }

    fn unbounded() -> Bounded {
        Bounded::new(usize::MAX)
    }

    /// Adds `id`; Err when it is new and the set is full. Membership is checked first.
    pub(crate) fn insert(&mut self, id: &BlockId) -> Result<bool, ()> {
        if self.set.contains(id) {
            return Ok(false);
        }
        if self.set.len() >= self.cap {
            return Err(());
        }
        self.set.insert(id.clone());
        Ok(true)
    }
}

/// The deepest nesting of link tests in `expr`: 0 to 3 (the parser's bound).
fn link_depth(expr: &Expr) -> usize {
    match expr {
        Expr::Or(all) | Expr::And(all) => all.iter().map(link_depth).max().unwrap_or(0),
        Expr::Not(inner) => link_depth(inner),
        Expr::Linked { inner, .. } => 1 + inner.as_deref().map_or(0, link_depth),
        Expr::Compare { .. } | Expr::Has(_) => 0,
    }
}

/// `b`'s ancestors, nearest first, at most as many as `m` holds.
fn ancestors<'m>(m: &'m Materialized, b: &BlockId) -> Vec<&'m BlockId> {
    let mut out = vec![];
    let mut cur = m.get(b).and_then(|x| x.parent.as_ref());
    while let Some(p) = cur {
        if out.len() > m.len() {
            break;
        }
        out.push(p);
        cur = m.get(p).and_then(|x| x.parent.as_ref());
    }
    out
}

/// The blocks one explicit link, pinned reference or placement edge away from `b`, in either
/// direction.
fn neighbours<'g>(m: &'g Materialized, g: &'g Graph, b: &BlockId) -> Vec<&'g BlockId> {
    labelled_neighbours(m, g, b).map(|(_, n)| n).collect()
}

/// One step from a block to a neighbour: placement, or an explicit link or pinned reference in
/// either direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step<'g> {
    /// The neighbour is a child of the near block.
    Child,
    /// The neighbour is the near block's parent.
    Parent,
    /// An edge of this kind leads from the near block to the neighbour.
    Out(&'g EdgeKind),
    /// An edge of this kind leads from the neighbour to the near block.
    In(&'g EdgeKind),
}

impl Step<'_> {
    /// The step's words: `child`, `parent`, `out <kind>` or `in <kind>`, where a pinned
    /// reference's kind is `references`.
    pub fn label(&self) -> String {
        match self {
            Step::Child => "child".into(),
            Step::Parent => "parent".into(),
            Step::Out(kind) => format!("out {}", kind.name()),
            Step::In(kind) => format!("in {}", kind.name()),
        }
    }
}

/// [`neighbours`], each with the step that reaches it, in the same order: the children, the
/// parent, the edges out, then the edges in. A block can appear more than once.
pub(crate) fn labelled_neighbours<'g>(
    m: &'g Materialized,
    g: &'g Graph,
    b: &BlockId,
) -> impl Iterator<Item = (Step<'g>, &'g BlockId)> + 'g {
    let children = m
        .children(Some(b))
        .into_iter()
        .map(|c| (Step::Child, &c.id));
    let parent = m
        .get(b)
        .and_then(|x| x.parent.as_ref())
        .map(|p| (Step::Parent, p));
    let out = g
        .outgoing(b)
        .iter()
        .filter(|e| is_link(&e.kind))
        .map(|e| (Step::Out(&e.kind), &e.to));
    let inc = g
        .incoming(b)
        .iter()
        .filter(|e| is_link(&e.kind))
        .map(|e| (Step::In(&e.kind), &e.from));
    children.chain(parent).chain(out).chain(inc)
}

/// The explicit link and reference neighbours of `b`, in either direction.
fn linked<'g>(g: &'g Graph, b: &BlockId) -> impl Iterator<Item = &'g BlockId> {
    g.outgoing(b)
        .iter()
        .filter(|e| is_link(&e.kind))
        .map(|e| &e.to)
        .chain(
            g.incoming(b)
                .iter()
                .filter(|e| is_link(&e.kind))
                .map(|e| &e.from),
        )
}

/// Adds to `out` the complete inputs of evaluating each of `rules` at `b` in `(m, g)`, whether
/// it passes or fails, and whether its `select` matches: `b`, its ancestors for an inherited
/// field (lifecycle, path, doc, area), every candidate a link test inspects and those
/// candidates' own inputs, and the sources of derived inputs: a status's incoming links
/// (supersedes, revokes), a suspect's pinned targets, and a tension's or a coverage score's
/// linked neighbours, parent and ancestors. Every branch of `and`, `or` and `not` is walked, so
/// a short-circuited evaluation leaves nothing out. A module rule adds every block its module's
/// input reads ([`module_reads`]): the neighbours to its `depth`, those the input's cap drops
/// too, and their derived values' inputs. Err: `out` is full.
fn inputs_of(
    rules: &[PreparedRule],
    m: &Materialized,
    g: &Graph,
    b: &BlockId,
    out: &mut Bounded,
) -> Result<(), ()> {
    out.insert(b)?;
    for rule in rules {
        // a blocking rule's severity reads the effective lifecycle, inherited from the ancestors
        if rule.severity == RuleSeverity::Blocking {
            for a in ancestors(m, b) {
                out.insert(a)?;
            }
        }
        for f in rule.filters() {
            expr_inputs(&f.expr, m, g, b, out)?;
        }
        if let CompiledRuleTest::Module(module) = &rule.test {
            module_reads(m, g, b, module.depth(), out)?;
        }
    }
    Ok(())
}

fn expr_inputs(
    expr: &Expr,
    m: &Materialized,
    g: &Graph,
    b: &BlockId,
    out: &mut Bounded,
) -> Result<(), ()> {
    match expr {
        Expr::Or(all) | Expr::And(all) => {
            for e in all {
                expr_inputs(e, m, g, b, out)?;
            }
        }
        Expr::Not(inner) => expr_inputs(inner, m, g, b, out)?,
        Expr::Compare { field, .. } | Expr::Has(field) => field_inputs(field, m, g, b, out)?,
        Expr::Linked {
            dir, kind, inner, ..
        } => {
            let candidates: Vec<&BlockId> = match (kind, dir) {
                (LinkKind::Contains, Dir::Out) => {
                    m.children(Some(b)).iter().map(|c| &c.id).collect()
                }
                (LinkKind::Contains, Dir::In) => m
                    .get(b)
                    .and_then(|x| x.parent.as_ref())
                    .into_iter()
                    .collect(),
                (_, Dir::Out) => g
                    .outgoing(b)
                    .iter()
                    .filter(|e| edge_is(kind, &e.kind))
                    .map(|e| &e.to)
                    .collect(),
                (_, Dir::In) => g
                    .incoming(b)
                    .iter()
                    .filter(|e| edge_is(kind, &e.kind))
                    .map(|e| &e.from)
                    .collect(),
            };
            for c in candidates {
                out.insert(c)?;
                if let Some(inner) = inner.as_deref() {
                    expr_inputs(inner, m, g, c, out)?;
                }
            }
        }
    }
    Ok(())
}

fn edge_is(kind: &LinkKind, edge: &EdgeKind) -> bool {
    match (kind, edge) {
        (LinkKind::References, EdgeKind::References) => true,
        (LinkKind::Link(want), EdgeKind::Link(have)) => want == have,
        _ => false,
    }
}

pub(super) fn field_inputs(
    field: &Field,
    m: &Materialized,
    g: &Graph,
    b: &BlockId,
    out: &mut Bounded,
) -> Result<(), ()> {
    let add_ancestors = |out: &mut Bounded| -> Result<(), ()> {
        for a in ancestors(m, b) {
            out.insert(a)?;
        }
        Ok(())
    };
    match field {
        Field::Lifecycle => add_ancestors(out)?,
        // a path reads the names on the chain, and a derived name its siblings' names
        Field::Path | Field::Doc => {
            add_ancestors(out)?;
            path_inputs(m, b, out)?;
        }
        // a decision is also in the areas of the paths that its `affects` links target
        Field::Area => {
            add_ancestors(out)?;
            path_inputs(m, b, out)?;
            if let Some(x) = m
                .get(b)
                .filter(|x| x.block_type == rdy_model::decision::DECISION)
            {
                for l in x
                    .links
                    .get(rdy_model::decision::AFFECTS)
                    .into_iter()
                    .flatten()
                {
                    out.insert(&l.target)?;
                    for a in ancestors(m, &l.target) {
                        out.insert(a)?;
                    }
                    path_inputs(m, &l.target, out)?;
                }
            }
        }
        Field::Status => {
            for e in g.incoming(b).iter().filter(|e| is_link(&e.kind)) {
                out.insert(&e.from)?;
            }
        }
        Field::Suspect => {
            for s in g.suspects_of(b) {
                out.insert(&s.target)?;
            }
        }
        Field::Tension | Field::Coverage => {
            for n in linked(g, b) {
                out.insert(n)?;
            }
            add_ancestors(out)?;
        }
        // ab2: the as-built status reads the block's direct decisions and its containers
        Field::Planned | Field::Since => {
            for e in g.outgoing(b).iter().filter(|e| {
                matches!(&e.kind, EdgeKind::Link(k)
                    if k == rdy_model::decision::DECIDED_BY || k == rdy_model::decision::IMPLEMENTS)
            }) {
                out.insert(&e.to)?;
            }
            add_ancestors(out)?;
        }
        Field::Id | Field::Key | Field::Type | Field::Trait | Field::Custom(_) => {}
    }
    Ok(())
}

/// Adds the blocks whose names the path of `b` reads beyond its chain: for each doc or folder on
/// the chain whose parent holds a doc or folder with no stored name, every doc and folder under
/// that parent, since a derived name is de-duplicated against its siblings' names.
fn path_inputs(m: &Materialized, b: &BlockId, out: &mut Bounded) -> Result<(), ()> {
    let chain = std::iter::once(b).chain(ancestors(m, b));
    for at in chain {
        let Some(x) = m.get(at) else { continue };
        if !rdy_model::tree::is_tree_type(&x.block_type) || !m.has_nameless_under(x.parent.as_ref())
        {
            continue;
        }
        for sibling in m.children(x.parent.as_ref()) {
            if rdy_model::tree::is_tree_type(&sibling.block_type) {
                out.insert(&sibling.id)?;
            }
        }
    }
    Ok(())
}

/// Tests only: the blocks that a filter test of `field` at `b` reads besides link candidates,
/// as rule reach records them.
#[doc(hidden)]
pub fn field_inputs_for_tests(
    field: &Field,
    m: &Materialized,
    g: &Graph,
    b: &BlockId,
) -> BTreeSet<BlockId> {
    let mut out = Bounded::unbounded();
    let _ = field_inputs(field, m, g, b, &mut out);
    out.set
}

/// Tests only: the reach `pack`'s rules add to version `pref`, which writes `written`, from
/// `before` to `projected`, with no stored rule rows ([`rule_reach`]). Err: the rules do not
/// compile.
#[doc(hidden)]
pub fn rule_reach_for_tests(
    pref: &crate::ProposalRef,
    pack: &Pack,
    before: (&Materialized, &Graph),
    projected: (&Materialized, &Graph),
    written: &BTreeSet<BlockId>,
) -> Result<Result<BTreeSet<BlockId>, Issue>, RdyError> {
    let rules = compile_pack_rules(pack)?;
    Ok(rule_reach(
        pref,
        pack,
        rules.rules(),
        before,
        projected,
        written,
        &BTreeSet::new(),
    ))
}

/// The reach a version's rules add to its assessment (TASK-946, D-DSPMW, D-AJ0XQ): empty when
/// there are no rules. Otherwise, from each block the version writes (`written`, deleted ones
/// included), in canon before (`before`) and in the version's landing projection (`projected`):
/// the block, its descendants, and the blocks up to the deepest link nesting of any rule, or a
/// module rule's `depth` (0 to 3), away through placement edges, explicit links and pinned
/// references, in either direction.
/// When a rule reads `tension` and a block of that walk can take part in a duplicate-definition
/// group, also every block that can (the partners no link joins). Then, once and without expanding again, the complete rule inputs of every block of that
/// expansion in both states ([`inputs_of`]), and every block a stored rule row reads
/// (`stored_row_reads`). Sorted and distinct. Err: the walk reached more than [`MAX_ROWS`]
/// distinct blocks, the "too large to assess" issue of `pref`.
pub(crate) fn rule_reach(
    pref: &crate::ProposalRef,
    pack: &Pack,
    rules: &[PreparedRule],
    before: (&Materialized, &Graph),
    projected: (&Materialized, &Graph),
    written: &BTreeSet<BlockId>,
    stored_row_reads: &BTreeSet<BlockId>,
) -> Result<BTreeSet<BlockId>, Issue> {
    if rules.is_empty() {
        return Ok(BTreeSet::new());
    }
    let too_large = || crate::readiness::assessment::too_large(pref);
    let depth = rules.iter().map(PreparedRule::depth).max().unwrap_or(0);
    let mut expansion = Bounded::new(MAX_ROWS);
    for (m, g) in [before, projected] {
        for w in written {
            expansion.insert(w).map_err(|_| too_large())?;
            if m.get(w).is_none() {
                continue;
            }
            // the descendants, walked iteratively
            let mut stack: Vec<&BlockId> = vec![w];
            while let Some(at) = stack.pop() {
                for c in m.children(Some(at)) {
                    if expansion.insert(&c.id).map_err(|_| too_large())? {
                        stack.push(&c.id);
                    }
                }
            }
            // the neighbours, level by level
            let mut seen: BTreeSet<&BlockId> = BTreeSet::from([w]);
            let mut level: Vec<&BlockId> = vec![w];
            for _ in 0..depth {
                let mut next = vec![];
                for at in level {
                    for n in neighbours(m, g, at) {
                        expansion.insert(n).map_err(|_| too_large())?;
                        if seen.insert(n) {
                            next.push(n);
                        }
                    }
                }
                level = next;
            }
        }
    }
    // a tension reads duplicate partners, which no link joins (row 166): the blocks that share
    // a name key or a text key with a block of the expansion, in either state. A module reads
    // no tension, so only a filter can.
    let reads_tension = rules
        .iter()
        .flat_map(PreparedRule::filters)
        .any(|f| f.needs().tension);
    if reads_tension
        && [before.0, projected.0]
            .iter()
            .any(|m| expansion.set.iter().any(|id| can_pair(pack, m, id)))
    {
        for (m, _) in [before, projected] {
            for b in m.iter().filter(|b| duplicate_eligible(pack, b)) {
                expansion.insert(&b.id).map_err(|_| too_large())?;
            }
        }
    }
    let mut reach = Bounded::new(MAX_ROWS);
    for id in &expansion.set {
        reach.insert(id).map_err(|_| too_large())?;
    }
    for id in &expansion.set {
        for (m, g) in [before, projected] {
            if m.get(id).is_some() {
                inputs_of(rules, m, g, id, &mut reach).map_err(|_| too_large())?;
            }
        }
    }
    for id in stored_row_reads {
        reach.insert(id).map_err(|_| too_large())?;
    }
    Ok(reach.set)
}

/// Whether block `id` of `m` can take part in a duplicate-definition group. A partner is made by
/// a competing landing that gives a block the same name or text, and no link joins it, so the
/// walk cannot know it: reach reads every block that can take part (row 166).
fn can_pair(pack: &Pack, m: &Materialized, id: &BlockId) -> bool {
    m.get(id).is_some_and(|b| duplicate_eligible(pack, b))
}
