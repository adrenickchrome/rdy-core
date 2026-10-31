//! Pack rule checks (D-JQD5X): a rule fails on a block that matches its `select` and does not
//! match its `require`. The filters are M3f's ([`rdy_schema::filter`]); the evaluator is
//! [`FilterContext`]. This file adds no parser and no evaluator: it runs each rule over the
//! blocks a stage names and words each failure.
//!
//! - At `change` ([`change_checks`]) a rule judges the blocks a write or view op changed and the
//!   blocks one explicit link or pinned reference away, before or after. It never refuses.
//! - At `propose` (the readiness of `pending::queue`) a rule judges every block of the version's
//!   landing projection. A blocking failure on stable content is a readiness issue.
//!
//! [`rule_reach`] is the dependency walk that widens an assessment's `reach` (D-DSPMW, D-AJ0XQ):
//! the blocks a rule's result can read, so a landing that changes one of them stales the
//! assessment and nothing reruns a rule at landing.
use crate::brief::effective_stable;
use crate::check::stages::Stage;
use crate::error::{E_CONFIG, E_INTERNAL, E_RULE_CHECK, RdyError, W_RULE_CHECK};
use crate::filter::{FilterContext, FilterInputs};
use crate::materialize::Materialized;
use crate::{ReadState, Scope};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::BlockId;
use rdy_model::assessment::MAX_ROWS;
use rdy_schema::filter::{Dir, Expr, Field, Filter, LinkKind};
use rdy_schema::{CompiledRule, RuleSeverity, compile_rules};
use rdy_schema::{Issue, Pack, Severity};
use serde::Serialize;
use std::cell::Cell;
use std::collections::BTreeSet;

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

/// The rules of `pack`, compiled, in name order. A pack that resolved always compiles; an error
/// here is the pack's `E_CONFIG`.
pub(crate) fn compiled(pack: &Pack) -> Result<Vec<CompiledRule>, RdyError> {
    if pack.rules.is_empty() {
        return Ok(vec![]);
    }
    compile_rules(pack)
        .map_err(|e| RdyError::new(E_CONFIG, format!("invalid pack: {e}"), "fix the pack file"))
}

/// Runs every rule of `pack` on each of `ids` that `inputs.m` holds, at `stage`. A rule fails
/// when its `select` matches and its `require` does not. The row's code is `E_RULE_CHECK` when
/// the rule is blocking and the block is in effective stable content of `inputs.m`, else
/// `W_RULE_CHECK`. The rows come in rule name, then block id order, each pair once. At
/// `propose` the first [`MAX_RULE_ROWS`] blocking rows and the first [`MAX_RULE_ROWS`] warning
/// rows are kept; at `change`, the first [`MAX_RULE_ROWS`] rows of both. The rest are counted.
/// One [`FilterContext`] judges the one state; it lives inside this call only. Rules never run
/// at `land`.
pub fn evaluate_rules(
    pack: &Pack,
    inputs: FilterInputs<'_>,
    ids: impl Iterator<Item = BlockId>,
    stage: Stage,
) -> Result<RuleScan, RdyError> {
    if stage == Stage::Land {
        return Err(RdyError::new(
            E_INTERNAL,
            "pack rules run at change and propose only",
            "report this as a bug in rdy",
        ));
    }
    let rules = compiled(pack)?;
    let mut scan = RuleScan::default();
    if rules.is_empty() {
        return Ok(scan);
    }
    RULE_SCANS.with(|n| n.set(n.get() + 1));
    let ids: BTreeSet<BlockId> = ids.collect();
    let (m, graph) = (inputs.m, inputs.graph);
    let ctx = FilterContext::from_parts(inputs);
    let mut failing: Vec<(usize, BlockId)> = vec![];
    for (at, rule) in rules.iter().enumerate() {
        for id in &ids {
            if !ctx.matches(&rule.select, id) || ctx.matches(&rule.require, id) {
                continue;
            }
            let blocking =
                rule.rule.severity == RuleSeverity::Blocking && effective_stable(m, Some(id));
            let row = RuleIssue {
                rule: rule.rule.name.clone(),
                code: if blocking { E_RULE_CHECK } else { W_RULE_CHECK }.to_string(),
                block: id.to_string(),
                message: format!(
                    "rule {} fails on {id}: {}",
                    rule.rule.name, rule.rule.message
                ),
                fix: rule.rule.fix.clone(),
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
            let _ = inputs_of(std::slice::from_ref(&rules[*at]), m, graph, id, &mut read);
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
    let pack = after.pack();
    let rules = compiled(pack)?;
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
    let filters: Vec<&Filter> = rules.iter().flat_map(|r| [&r.select, &r.require]).collect();
    let inputs = after.scope_inputs(&scope, &filters)?;
    evaluate_rules(pack, inputs.inputs(after), ids.into_iter(), Stage::Change)
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
    let mut out: Vec<&BlockId> = m.children(Some(b)).iter().map(|c| &c.id).collect();
    if let Some(p) = m.get(b).and_then(|x| x.parent.as_ref()) {
        out.push(p);
    }
    out.extend(
        g.outgoing(b)
            .iter()
            .filter(|e| is_link(&e.kind))
            .map(|e| &e.to),
    );
    out.extend(
        g.incoming(b)
            .iter()
            .filter(|e| is_link(&e.kind))
            .map(|e| &e.from),
    );
    out
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
/// it passes or fails: `b`, its ancestors for an inherited field (lifecycle, path, doc, area),
/// every candidate a link test inspects and those candidates' own inputs, and the sources of
/// derived inputs: a status's incoming links (supersedes, revokes), a suspect's pinned targets,
/// and a tension's or a coverage score's linked neighbours, parent and ancestors. Every branch
/// of `and`, `or` and `not` is walked, so a short-circuited evaluation leaves nothing out.
/// Err: `out` is full.
fn inputs_of(
    rules: &[CompiledRule],
    m: &Materialized,
    g: &Graph,
    b: &BlockId,
    out: &mut Bounded,
) -> Result<(), ()> {
    out.insert(b)?;
    for rule in rules {
        for f in [&rule.select, &rule.require] {
            expr_inputs(&f.expr, m, g, b, out)?;
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

fn field_inputs(
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
        Field::Lifecycle | Field::Path | Field::Doc | Field::Area => add_ancestors(out)?,
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
        Field::Id | Field::Key | Field::Type | Field::Trait | Field::Custom(_) => {}
    }
    Ok(())
}

/// The reach a version's rules add to its assessment (TASK-946, D-DSPMW, D-AJ0XQ): empty when
/// there are no rules. Otherwise, from each block the version writes (`written`, deleted ones
/// included), in canon before (`before`) and in the version's landing projection (`projected`):
/// the block, its descendants, and the blocks up to the deepest link nesting of any rule (0 to
/// 3) away through placement edges, explicit links and pinned references, in either direction.
/// Then, once and without expanding again, the complete rule inputs of every block of that
/// expansion in both states ([`inputs_of`]), and every block a stored rule row reads
/// (`stored_row_reads`). Sorted and distinct. Err: the walk reached more than [`MAX_ROWS`]
/// distinct blocks, the "too large to assess" issue of `pref`.
pub(crate) fn rule_reach(
    pref: &crate::ProposalRef,
    rules: &[CompiledRule],
    before: (&Materialized, &Graph),
    projected: (&Materialized, &Graph),
    written: &BTreeSet<BlockId>,
    stored_row_reads: &BTreeSet<BlockId>,
) -> Result<BTreeSet<BlockId>, Issue> {
    if rules.is_empty() {
        return Ok(BTreeSet::new());
    }
    let too_large = || crate::readiness::assessment::too_large(pref);
    let depth = rules
        .iter()
        .flat_map(|r| [&r.select, &r.require])
        .map(|f| link_depth(&f.expr))
        .max()
        .unwrap_or(0);
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
