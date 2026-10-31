//! The closed recipes (O4): which rows each check's case shows. Each reads the two states it is
//! given and the graph's existing rules: governing routes (D-VPATZ) and related decisions.
use super::case::{
    BuiltCase, CaseInputs, MAX_CONTEXT, Plan, Row, Selection, Visited, assemble, wrong_subject,
};
use super::catalogue::AgentCheck;
use super::code::{self, CodeSource};
use crate::lexicon::is_tie;
use crate::query::{governing_ids, is_entity_in};
use crate::related::{RelatedDecision, related_in};
use crate::{E_UNKNOWN_BLOCK, Materialized, RdyError};
use rdy_graph::{EdgeKind, GoverningCache, GoverningStatuses, Graph};
use rdy_model::check_request::CheckSubject;
use rdy_model::decision::{
    self, DECIDED_BY, DECISION, DEPENDS_ON, DecisionStatus, IMPLEMENTS, REFINES, REVOKES,
    SUPERSEDES,
};
use rdy_model::{Block, BlockId, Hash, Justification, Op};
use rdy_schema::{Trait, supporting};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most pairs one decision's contradiction check emits.
const MAX_PAIRS: usize = 20;

/// The links from a decision to the decisions it follows.
const PREDECESSOR_LINKS: [&str; 3] = [SUPERSEDES, REVOKES, REFINES];

/// A block the state does not hold: the resolver's refusal.
pub(super) fn unknown(id: &BlockId) -> RdyError {
    RdyError::new(
        E_UNKNOWN_BLOCK,
        format!("no block {id}"),
        "check the id with rdy_read",
    )
}

/// What a subject names, for a refusal, when it is not the shape a recipe takes.
fn other_subject(check: AgentCheck, s: &CheckSubject) -> RdyError {
    if let Some(v) = &s.version {
        return wrong_subject(check, &v.to_string(), "proposal version");
    }
    if let Some(c) = &s.change {
        return wrong_subject(check, &c.to_string(), "change");
    }
    if let Some(c) = &s.code
        && s.blocks.is_empty()
    {
        let range = format!("{}@{}..{}", c.repo, c.from, c.to);
        return wrong_subject(check, &range, "code range");
    }
    let list: Vec<String> = s.blocks.iter().map(BlockId::to_string).collect();
    wrong_subject(
        check,
        &list.join(", "),
        &format!("list of {} blocks", list.len()),
    )
}

/// The one block of a one-block subject, in `after`.
fn one_block<'a>(
    check: AgentCheck,
    s: &CheckSubject,
    after: &'a Materialized,
) -> Result<&'a Block, RdyError> {
    let ([id], None, None) = (s.blocks.as_slice(), &s.version, &s.change) else {
        return Err(other_subject(check, s));
    };
    after.get(id).ok_or_else(|| unknown(id))
}

fn graph_of(inputs: CaseInputs<'_>, m: &Materialized) -> Graph {
    Graph::build_with(m.iter(), Arc::new(inputs.st.pack().tracking()))
}

/// The related rows of decision `id` in the case's `after` state, and the count the cap dropped.
/// A relation another view holds is not in that state, so it cannot change this case.
fn related_of(inputs: CaseInputs<'_>, id: &BlockId) -> (Vec<RelatedDecision>, usize) {
    let pack = inputs.st.pack();
    let is_entity_type = |t: &str| {
        pack.block_types
            .get(t)
            .is_some_and(|t| t.traits.contains(&Trait::Defines))
    };
    let g = graph_of(inputs, inputs.after);
    related_in(inputs.after, &g, &is_entity_type, std::slice::from_ref(id))
}

fn primary<'a>(
    role: &'static str,
    id: &BlockId,
    before: Option<&'a Block>,
    after: Option<&'a Block>,
    selection: Selection,
) -> Row<'a> {
    Row {
        role,
        id: id.clone(),
        before,
        after,
        selection,
        contextual: false,
    }
}

fn contextual<'a>(role: &'static str, b: &'a Block, selection: Selection) -> Row<'a> {
    Row {
        role,
        id: b.id.clone(),
        before: None,
        after: Some(b),
        selection,
        contextual: true,
    }
}

fn sorted(mut ids: Vec<BlockId>) -> Vec<BlockId> {
    ids.sort();
    ids.dedup();
    ids
}

// ---------------------------------------------------------------- contradiction

/// A two-block subject that no view holds together.
pub(super) fn no_form(a: &BlockId, b: &BlockId) -> RdyError {
    wrong_subject(
        AgentCheck::Contradiction,
        &a.to_string(),
        &format!("block no view holds with {b}"),
    )
}

/// A decision with no pair.
pub(super) fn no_pairs(s: &CheckSubject) -> RdyError {
    let who: Vec<String> = s.blocks.iter().map(BlockId::to_string).collect();
    wrong_subject(
        AgentCheck::Contradiction,
        &who.join(", "),
        "decision with no related decisions",
    )
}

/// The pairs of a contradiction subject (each sorted by id), the related rows the query's cap
/// dropped, and whether the subject was two named blocks.
pub(super) fn contradiction_pairs(
    s: &CheckSubject,
    inputs: CaseInputs<'_>,
) -> Result<(Vec<[BlockId; 2]>, usize, bool), RdyError> {
    let check = AgentCheck::Contradiction;
    if s.version.is_some() || s.change.is_some() || s.code.is_some() {
        return Err(other_subject(check, s));
    }
    match s.blocks.as_slice() {
        [a, b] if a != b => {
            for id in [a, b] {
                inputs.after.get(id).ok_or_else(|| unknown(id))?;
            }
            let (a, b) = if a < b { (a, b) } else { (b, a) };
            Ok((vec![[a.clone(), b.clone()]], 0, true))
        }
        [id] => {
            let d = inputs.after.get(id).ok_or_else(|| unknown(id))?;
            if d.block_type != DECISION {
                return Err(wrong_subject(check, &id.to_string(), &d.block_type));
            }
            let (rows, dropped) = related_of(inputs, id);
            let mut seen = BTreeSet::new();
            let mut pairs = vec![];
            for row in rows {
                let other = BlockId::parse(&row.decision).map_err(|_| {
                    RdyError::new(
                        crate::E_INTERNAL,
                        format!("related row names {}", row.decision),
                        "report this as a bug in rdy",
                    )
                })?;
                if other == *id || inputs.after.get(&other).is_none() || !seen.insert(other.clone())
                {
                    continue;
                }
                let pair = if *id < other {
                    [id.clone(), other]
                } else {
                    [other, id.clone()]
                };
                pairs.push(pair);
            }
            let past = pairs.len().saturating_sub(MAX_PAIRS);
            pairs.truncate(MAX_PAIRS);
            Ok((pairs, dropped + past, false))
        }
        _ => Err(other_subject(check, s)),
    }
}

// ---------------------------------------------------------------- the fold of selected changes

/// A change or proposal version subject: its label, and `change` or `proposal version`.
fn selected(
    check: AgentCheck,
    s: &CheckSubject,
    after: &Materialized,
) -> Result<(String, &'static str), RdyError> {
    match (s.blocks.as_slice(), &s.change, &s.version, &s.code) {
        ([], Some(c), None, None) => Ok((c.to_string(), "change")),
        ([], None, Some(v), None) => Ok((v.to_string(), "proposal version")),
        ([id], None, None, None) => {
            let b = after.get(id).ok_or_else(|| unknown(id))?;
            Err(wrong_subject(check, &id.to_string(), &b.block_type))
        }
        _ => Err(other_subject(check, s)),
    }
}

/// The selected changes folded in landing order: each block they write, at its first write, with
/// its last effective justification. A change with no justification does not end an earlier one's
/// (the rule the unjustified-change tension reads, FW-JU1).
struct Fold<'a> {
    order: Vec<BlockId>,
    last: BTreeMap<BlockId, &'a Justification>,
    /// Each written block's version before the selected changes' first write of it and after
    /// their last: the net the selected changes make by themselves, whatever the view did since.
    ends: BTreeMap<BlockId, (Option<Hash>, Option<Hash>)>,
}

fn fold<'a>(inputs: CaseInputs<'a>, visited: &mut Visited) -> Result<Fold<'a>, RdyError> {
    let mut order = vec![];
    let mut seen = BTreeSet::new();
    let mut last = BTreeMap::new();
    let mut ends: BTreeMap<BlockId, (Option<Hash>, Option<Hash>)> = BTreeMap::new();
    for i in inputs.st.landing_order(inputs.changes) {
        let change = &inputs.changes[i];
        for op in &change.ops {
            let id = op.block_id();
            if seen.insert(id.clone()) {
                visited.visit()?;
                order.push(id.clone());
            }
            let written = match op {
                Op::Create { block } | Op::Update { block, .. } => Some(block.version_hash()),
                Op::Delete { .. } => None,
            };
            ends.entry(id.clone())
                .and_modify(|e| e.1 = written)
                .or_insert((op.base(), written));
            if let Some(j) = &change.justification {
                last.insert(id.clone(), j);
            }
        }
    }
    Ok(Fold { order, last, ends })
}

/// The before and after rows of `id`; `None` when the selected changes by themselves leave it as
/// they found it (a write they undo is net zero, whatever later view edits do to it), or when
/// neither state shows it. Membership reads the selected changes only; the rows show the states.
fn net<'a>(
    inputs: CaseInputs<'a>,
    folded: &Fold<'_>,
    id: &BlockId,
) -> Option<(Option<&'a Block>, Option<&'a Block>)> {
    let (first, last) = folded.ends.get(id)?;
    if first == last {
        return None;
    }
    let rows = (inputs.before.get(id), inputs.after.get(id));
    (rows.0.is_some() || rows.1.is_some()).then_some(rows)
}

pub(super) fn meaning(s: &CheckSubject, inputs: CaseInputs<'_>) -> Result<BuiltCase, RdyError> {
    let check = AgentCheck::MeaningPreservation;
    let (who, kind) = selected(check, s, inputs.after)?;
    let mut visited = Visited::default();
    let folded = fold(inputs, &mut visited)?;
    let mut rows = vec![];
    let mut subject_blocks = vec![];
    for id in &folded.order {
        if !matches!(folded.last.get(id), Some(Justification::Editorial { .. })) {
            continue;
        }
        if let Some((before, after)) = net(inputs, &folded, id) {
            rows.push(primary("subject", id, before, after, Selection::Shown));
            subject_blocks.push(id.clone());
        }
    }
    if rows.is_empty() {
        return Err(wrong_subject(
            check,
            &who,
            &format!("{kind} without editorial blocks"),
        ));
    }
    // the live decisions that govern each row, where the row's block is
    let pack = inputs.st.pack();
    let g_after = graph_of(inputs, inputs.after);
    let g_before = rows
        .iter()
        .any(|r| r.after.is_none())
        .then(|| graph_of(inputs, inputs.before));
    let mut cache_after = GoverningCache::default();
    let mut cache_before = GoverningCache::default();
    let mut governing = BTreeSet::new();
    for r in &rows {
        let found = match (&r.after, &g_before) {
            (None, Some(g)) => governing_ids(
                pack,
                inputs.before,
                g,
                &r.id,
                GoverningStatuses::Live,
                &mut cache_before,
            ),
            _ => governing_ids(
                pack,
                inputs.after,
                &g_after,
                &r.id,
                GoverningStatuses::Live,
                &mut cache_after,
            ),
        };
        governing.extend(found);
    }
    for d in governing {
        if subject_blocks.contains(&d) {
            continue;
        }
        visited.visit()?;
        // a row shows the decision in the state that still holds it
        match (inputs.after.get(&d), inputs.before.get(&d)) {
            (Some(b), _) => rows.push(contextual("decision", b, Selection::Decision)),
            (None, Some(b)) => rows.push(Row {
                before: Some(b),
                after: None,
                ..contextual("decision", b, Selection::Decision)
            }),
            (None, None) => {}
        }
    }
    assemble(
        check,
        inputs,
        Plan {
            rows,
            context_cap: None,
            subject_blocks: sorted(subject_blocks),
            code: CodeSource::None,
        },
    )
}

pub(super) fn fidelity(s: &CheckSubject, inputs: CaseInputs<'_>) -> Result<BuiltCase, RdyError> {
    let check = AgentCheck::ImplementationFidelity;
    let (who, kind) = selected(check, s, inputs.after)?;
    let mut visited = Visited::default();
    let folded = fold(inputs, &mut visited)?;
    // each decision once, at its first place; each changed block under the first decision it
    // implements
    let mut decisions: Vec<&BlockId> = vec![];
    let mut under: BTreeMap<&BlockId, Vec<&BlockId>> = BTreeMap::new();
    let mut subject_blocks = vec![];
    for id in &folded.order {
        let Some(Justification::Implements(list)) = folded.last.get(id) else {
            continue;
        };
        if net(inputs, &folded, id).is_none() {
            continue;
        }
        subject_blocks.push(id.clone());
        for d in list {
            if !decisions.contains(&d) {
                visited.visit()?;
                decisions.push(d);
            }
        }
        if let Some(first) = list.first() {
            under.entry(first).or_default().push(id);
        }
    }
    if subject_blocks.is_empty() {
        return Err(wrong_subject(
            check,
            &who,
            &format!("{kind} without implements"),
        ));
    }
    let mut rows = vec![];
    for d in decisions {
        let (b, a) = (inputs.before.get(d), inputs.after.get(d));
        if a.is_some() {
            rows.push(primary("decision", d, None, a, Selection::Decision));
        } else if b.is_some() {
            rows.push(primary("decision", d, b, None, Selection::Decision));
        }
        for id in under.get(d).into_iter().flatten() {
            if let Some((before, after)) = net(inputs, &folded, id) {
                rows.push(primary("subject", id, before, after, Selection::Shown));
            }
        }
    }
    assemble(
        check,
        inputs,
        Plan {
            rows,
            context_cap: None,
            subject_blocks: sorted(subject_blocks),
            code: CodeSource::None,
        },
    )
}

// ---------------------------------------------------------------- decision review

pub(super) fn decision_review(
    s: &CheckSubject,
    inputs: CaseInputs<'_>,
) -> Result<BuiltCase, RdyError> {
    let check = AgentCheck::DecisionReview;
    let d = one_block(check, s, inputs.after)?;
    let who = d.id.to_string();
    if d.block_type != DECISION {
        return Err(wrong_subject(check, &who, &d.block_type));
    }
    if decision::status(d) != Some(DecisionStatus::Proposed) {
        let status = d.field_str("status").unwrap_or("unset");
        return Err(wrong_subject(
            check,
            &who,
            &format!("decision with status {status}"),
        ));
    }
    let mut visited = Visited::default();
    visited.visit()?;
    let mut rows = vec![primary("subject", &d.id, None, Some(d), Selection::Review)];
    let mut seen = BTreeSet::from([d.id.clone()]);
    for l in d.links.get(DEPENDS_ON).into_iter().flatten() {
        visited.visit()?;
        if let Some(f) = inputs.after.get(&l.target)
            && f.block_type == supporting::FINDING
            && seen.insert(f.id.clone())
        {
            rows.push(primary("finding", &f.id, None, Some(f), Selection::Shown));
        }
    }
    let (related, _) = related_of(inputs, &d.id);
    for r in related {
        visited.visit()?;
        if let Ok(id) = BlockId::parse(&r.decision)
            && let Some(b) = inputs.after.get(&id)
            && seen.insert(id)
        {
            rows.push(contextual("related", b, Selection::Decision));
        }
    }
    assemble(
        check,
        inputs,
        Plan {
            rows,
            context_cap: None,
            subject_blocks: vec![d.id.clone()],
            code: CodeSource::None,
        },
    )
}

// ---------------------------------------------------------------- entity and prose

/// Sorts contextual blocks: `home`'s doc first, then doc path with no doc last, then id.
fn sort_context(m: &Materialized, home: &BlockId, blocks: &mut Vec<&Block>) {
    let doc_of = |id: &BlockId| m.doc_of(id).map(|d| d.id.clone());
    let home = doc_of(home);
    let mut keyed: Vec<_> = blocks
        .drain(..)
        .map(|b| {
            let doc = doc_of(&b.id);
            let same = doc.is_some() && doc == home;
            let path = doc.as_ref().and_then(|d| m.path_of(d));
            ((!same, path.is_none(), path), b)
        })
        .collect();
    keyed.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.id.cmp(&y.id)));
    blocks.extend(keyed.into_iter().map(|(_, b)| b));
}

pub(super) fn entity(s: &CheckSubject, inputs: CaseInputs<'_>) -> Result<BuiltCase, RdyError> {
    let check = AgentCheck::EntityDivergence;
    let e = one_block(check, s, inputs.after)?;
    let who = e.id.to_string();
    let pack = inputs.st.pack();
    if !is_entity_in(pack, inputs.after, &e.id) {
        return Err(wrong_subject(check, &who, &e.block_type));
    }
    let g = graph_of(inputs, inputs.after);
    let mut visited = Visited::default();
    visited.visit()?;
    let mut seen = BTreeSet::from([e.id.clone()]);
    let mut tied: Vec<&Block> = vec![];
    for edge in g.incoming(&e.id) {
        if is_tie(&edge.kind) && seen.insert(edge.from.clone()) {
            visited.visit()?;
            if let Some(b) = inputs.after.get(&edge.from) {
                tied.push(b);
            }
        }
    }
    if tied.is_empty() {
        return Err(wrong_subject(check, &who, &e.block_type));
    }
    sort_context(inputs.after, &e.id, &mut tied);
    let mut rows = vec![primary("subject", &e.id, None, Some(e), Selection::Shown)];
    rows.extend(
        tied.into_iter()
            .map(|b| contextual("entity", b, Selection::Shown)),
    );
    assemble(
        check,
        inputs,
        Plan {
            rows,
            context_cap: Some(MAX_CONTEXT),
            subject_blocks: vec![e.id.clone()],
            code: CodeSource::None,
        },
    )
}

/// The decisions `d` directly follows, in link order: supersedes, revokes, refines.
fn predecessors(d: &Block) -> impl Iterator<Item = &BlockId> {
    PREDECESSOR_LINKS
        .iter()
        .flat_map(|name| d.links.get(*name).into_iter().flatten())
        .map(|l| &l.target)
}

pub(super) fn prose(s: &CheckSubject, inputs: CaseInputs<'_>) -> Result<BuiltCase, RdyError> {
    let check = AgentCheck::ProseDrift;
    let mut visited = Visited::default();
    let folded = fold(inputs, &mut visited)?;
    // what the selected changes leave changed: a write undone later is unchanged prose
    let written: BTreeSet<&BlockId> = folded
        .order
        .iter()
        .filter(|id| net(inputs, &folded, id).is_some())
        .collect();
    // the new decisions, and the name a refusal gives the subject when its prose is empty
    let (news, who, kind): (Vec<&Block>, String, String) = if let Some(v) = &s.version {
        if !s.blocks.is_empty() || s.change.is_some() || s.code.is_some() {
            return Err(other_subject(check, s));
        }
        let news: Vec<&Block> = folded
            .order
            .iter()
            .filter(|id| written.contains(id))
            .filter_map(|id| inputs.after.get(id))
            .filter(|b| b.block_type == DECISION && predecessors(b).next().is_some())
            .collect();
        if news.is_empty() {
            return Err(wrong_subject(
                check,
                &v.to_string(),
                "proposal version without supersedes, revokes or refines",
            ));
        }
        (news, v.to_string(), "proposal version".to_string())
    } else {
        let d = one_block(check, s, inputs.after)?;
        if d.block_type != DECISION {
            return Err(wrong_subject(check, &d.id.to_string(), &d.block_type));
        }
        (vec![d], d.id.to_string(), d.block_type.clone())
    };
    let new_ids: BTreeSet<&BlockId> = news.iter().map(|b| &b.id).collect();
    let mut rows = vec![];
    let mut decisions: Vec<&BlockId> = vec![];
    for d in &news {
        visited.visit()?;
        rows.push(primary(
            "subject",
            &d.id,
            None,
            Some(*d),
            Selection::Decision,
        ));
        decisions.push(&d.id);
    }
    let mut seen_old = BTreeSet::new();
    for d in &news {
        for old in predecessors(d) {
            if new_ids.contains(old) || !seen_old.insert(old) {
                continue;
            }
            visited.visit()?;
            if let Some(b) = inputs.after.get(old) {
                rows.push(primary("decision", old, None, Some(b), Selection::Decision));
                decisions.push(old);
            }
        }
    }
    // the informative blocks these decisions govern by the four routes, at any status
    let pack = inputs.st.pack();
    let g = graph_of(inputs, inputs.after);
    let mut cache = GoverningCache::default();
    let mut seen = BTreeSet::new();
    let mut governed: Vec<&Block> = vec![];
    for d in &decisions {
        let reached = g.governed_through(
            d,
            GoverningStatuses::All,
            |e| is_entity_in(pack, inputs.after, e),
            &mut cache,
        );
        for id in reached {
            if !seen.insert(id.clone()) {
                continue;
            }
            visited.visit()?;
            let Some(b) = inputs.after.get(&id) else {
                continue;
            };
            let informative = pack
                .block_types
                .get(&b.block_type)
                .is_some_and(|t| t.traits.contains(&Trait::Informative));
            if informative && !written.contains(&id) {
                governed.push(b);
            }
        }
    }
    if governed.is_empty() {
        return Err(wrong_subject(check, &who, &kind));
    }
    sort_context(inputs.after, &news[0].id, &mut governed);
    rows.extend(
        governed
            .into_iter()
            .map(|b| contextual("governed", b, Selection::Shown)),
    );
    assemble(
        check,
        inputs,
        Plan {
            rows,
            context_cap: Some(MAX_CONTEXT),
            subject_blocks: sorted(news.iter().map(|b| b.id.clone()).collect()),
            code: CodeSource::None,
        },
    )
}

// ---------------------------------------------------------------- the code cases

/// The blocks that implement `d`: an `implements` or a `decided_by` link to it, by id.
fn implementations<'a>(
    g: &Graph,
    m: &'a Materialized,
    d: &BlockId,
    visited: &mut Visited,
) -> Result<Vec<&'a Block>, RdyError> {
    let mut ids = BTreeSet::new();
    for e in g.incoming(d) {
        if matches!(&e.kind, EdgeKind::Link(n) if n == IMPLEMENTS || n == DECIDED_BY)
            && ids.insert(e.from.clone())
        {
            visited.visit()?;
        }
    }
    Ok(ids.iter().filter_map(|id| m.get(id)).collect())
}

pub(super) fn diff(s: &CheckSubject, inputs: CaseInputs<'_>) -> Result<BuiltCase, RdyError> {
    let check = AgentCheck::DiffContradicts;
    let ([id], None, None) = (s.blocks.as_slice(), &s.version, &s.change) else {
        return Err(other_subject(check, s));
    };
    let d = inputs.after.get(id).ok_or_else(|| unknown(id))?;
    if d.block_type != DECISION {
        return Err(wrong_subject(check, &id.to_string(), &d.block_type));
    }
    let Some(range) = &s.code else {
        return Err(wrong_subject(
            check,
            &id.to_string(),
            "decision without a code range",
        ));
    };
    let g = graph_of(inputs, inputs.after);
    let mut visited = Visited::default();
    visited.visit()?;
    let implementing = implementations(&g, inputs.after, id, &mut visited)?;
    let mut causes = vec![id.clone()];
    causes.extend(implementing.iter().map(|b| b.id.clone()));
    let source = code::diff_source(check, inputs.st.root(), range, inputs.after, &causes)?;
    assemble(
        check,
        inputs,
        Plan {
            rows: vec![primary("subject", id, None, Some(d), Selection::Decision)],
            context_cap: None,
            subject_blocks: vec![id.clone()],
            code: source,
        },
    )
}

pub(super) fn confirmation(
    s: &CheckSubject,
    inputs: CaseInputs<'_>,
) -> Result<BuiltCase, RdyError> {
    let check = AgentCheck::Confirmation;
    let d = one_block(check, s, inputs.after)?;
    let who = d.id.to_string();
    if d.block_type != DECISION {
        return Err(wrong_subject(check, &who, &d.block_type));
    }
    if d.field_str("confirmation")
        .is_none_or(|t| t.trim().is_empty())
    {
        return Err(wrong_subject(
            check,
            &who,
            "decision without confirmation text",
        ));
    }
    let g = graph_of(inputs, inputs.after);
    let mut visited = Visited::default();
    visited.visit()?;
    let implementing = implementations(&g, inputs.after, &d.id, &mut visited)?;
    let mut causes = vec![d.id.clone()];
    causes.extend(implementing.iter().map(|b| b.id.clone()));
    let source = code::excerpt_source(check, inputs.st.root(), inputs.after, &causes)?;
    let mut rows = vec![primary(
        "subject",
        &d.id,
        None,
        Some(d),
        Selection::Confirmation,
    )];
    rows.extend(
        implementing
            .into_iter()
            .map(|b| primary("implementation", &b.id, None, Some(b), Selection::Shown)),
    );
    assemble(
        check,
        inputs,
        Plan {
            rows,
            context_cap: None,
            subject_blocks: vec![d.id.clone()],
            code: source,
        },
    )
}
