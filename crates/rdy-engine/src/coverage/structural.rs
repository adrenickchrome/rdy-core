//! The structural layer of coverage (FW-CH1, FW-CV1): the Grounded, Connected, Complete and
//! Governed checks and issues of each scored block. Each block's entry reads only local graph
//! queries, so a carry can recompute any one block. The layer is a function of the scope's
//! build and the pack, memoised on the build.
use super::model::*;
use crate::brief::effective_stable;
use crate::lexicon::{Lexicon, is_content, is_tie, link_kind_for, linked_from_outside, tied};
use crate::tensions::index::is_normative;
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{DerivedStatus, EdgeKind, Graph};
use rdy_model::{Block, BlockId, FieldValue, decision, tree};
use rdy_schema::{Pack, TypeClass};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

thread_local! {
    static STRUCTURAL_BUILDS: Cell<u64> = const { Cell::new(0) };
}

/// Test hook: this thread's count of full structural builds (`structural_of` calls).
#[doc(hidden)]
pub fn structural_builds() -> u64 {
    STRUCTURAL_BUILDS.with(Cell::get)
}

/// What one block's structural entry reads.
pub(crate) struct EntryCx<'a> {
    pub m: &'a Materialized,
    pub g: &'a Graph,
    pub lex: &'a Lexicon,
    pub pack: &'a Pack,
}

/// Whether `id` is a live decision: accepted and not wholly superseded or revoked.
fn is_live(g: &Graph, id: &BlockId) -> bool {
    matches!(
        g.status(id),
        Some(DerivedStatus::Live | DerivedStatus::PartiallySuperseded { .. })
    )
}

/// Whether an edge is the link `name`.
fn is_link(e: &rdy_graph::Edge, name: &str) -> bool {
    matches!(&e.kind, EdgeKind::Link(n) if n == name)
}

/// Whether `id` is tied to a block that exists and is an entity: a reference, a `describes`
/// link or an `affects` link to one, among its own outgoing edges.
fn tied_to_entity(cx: &EntryCx<'_>, id: &BlockId) -> bool {
    cx.g.outgoing(id).iter().any(|e| {
        is_tie(&e.kind)
            && cx
                .m
                .get(&e.to)
                .is_some_and(|t| cx.lex.is_entity_type(&t.block_type))
    })
}

/// Whether `id` has a governing decision: a live decision by `decided_by` or `implements`, or a
/// direct `affects` (`Graph::governing`), or a live decision that affects an entity `id`
/// describes (D-TQD4D).
fn is_governed(g: &Graph, id: &BlockId) -> bool {
    if !g.governing(id).is_empty() {
        return true;
    }
    g.outgoing(id)
        .iter()
        .filter(|e| is_link(e, decision::DESCRIBES))
        .any(|e| {
            g.incoming(&e.to)
                .iter()
                .any(|a| is_link(a, decision::AFFECTS) && is_live(g, &a.from))
        })
}

/// Whether decision `d` is applied (D-ET9RX): a block that is neither a decision nor of the
/// supporting class links to it by `decided_by`, `implements` or a reference, or `d` affects a
/// block that is not a decision.
fn is_applied(cx: &EntryCx<'_>, d: &BlockId) -> bool {
    let carries = cx.g.incoming(d).iter().any(|e| {
        (e.kind == EdgeKind::References
            || is_link(e, decision::DECIDED_BY)
            || is_link(e, decision::IMPLEMENTS))
            && cx.m.get(&e.from).is_some_and(|f| {
                f.block_type != decision::DECISION
                    && cx.pack.class_of(&f.block_type) != TypeClass::Supporting
            })
    });
    carries
        || cx.g.outgoing(d).iter().any(|e| {
            is_link(e, decision::AFFECTS)
                && cx
                    .m
                    .get(&e.to)
                    .is_some_and(|t| t.block_type != decision::DECISION)
        })
}

/// Whether decision `b` says it needs no content (`needs_content: false`).
fn needs_no_content(b: &Block) -> bool {
    b.fields.get("needs_content") == Some(&FieldValue::Bool(false))
}

/// The required skeleton sections doc `b` lacks, by title, in skeleton order.
fn missing_sections(cx: &EntryCx<'_>, b: &Block) -> Vec<String> {
    let children = cx.m.children(Some(&b.id));
    let titles = crate::keys::section_titles(children.iter().copied());
    let have: Vec<&str> = titles.get(&b.id).cloned().unwrap_or_default();
    let Some(dt) = b
        .field_str("doc_type")
        .and_then(|t| cx.pack.doc_types.get(t))
    else {
        return vec![];
    };
    dt.skeleton
        .iter()
        .filter(|s| s.required && !have.contains(&s.title.as_str()))
        .map(|s| s.title.clone())
        .collect()
}

/// The entities `b`'s text names, as [`entry_of`] lists them: none for a presentational block
/// or one that takes no link. A carry reads it for a block's version before the write.
pub(crate) fn named_in(pack: &Pack, lex: &Lexicon, b: &Block) -> Vec<BlockId> {
    if pack.is_presentational(&b.block_type) || link_kind_for(pack, b).is_none() {
        return vec![];
    }
    lex.mentions(b)
        .iter()
        .filter_map(|m| BlockId::parse(&m.entity).ok())
        .collect()
}

/// One block's structural entry and the entities its text names, by the closed table (ledger
/// Q2): `None` for a block that is absent or presentational (not scored). Reads only local
/// queries: the block's own edges, its children, its doc's subtree for `unlinked_doc`, and the
/// incoming edges of the entities it describes.
pub(crate) fn entry_of(cx: &EntryCx<'_>, id: &BlockId) -> Option<(BlockEntry, Vec<BlockId>)> {
    let b = cx.m.get(id)?;
    if cx.pack.is_presentational(&b.block_type) {
        return None;
    }
    let mut e = BlockEntry {
        doc: cx.m.doc_of(id).map(|d| d.id.clone()),
        checks: [0; 6],
        issues: vec![],
    };
    let mut named: Vec<BlockId> = vec![];

    // Grounded
    if is_content(cx.pack, b) {
        e.check(Dimension::Grounded);
        if !tied_to_entity(cx, id) {
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::UntiedContent,
                None,
                fix_untied_content(id),
            ));
        }
    }
    if link_kind_for(cx.pack, b).is_some() {
        for mention in cx.lex.mentions(b) {
            let Ok(entity) = BlockId::parse(&mention.entity) else {
                continue;
            };
            e.check(Dimension::Grounded);
            if !tied(cx.g, id, &entity) {
                let name =
                    cx.m.get(&entity)
                        .and_then(|t| t.field_str("name"))
                        .unwrap_or(&mention.text)
                        .to_string();
                e.issues.push(CoverageIssue::new(
                    id,
                    IssueCode::UnlinkedMention,
                    Some(entity.to_string()),
                    fix_unlinked_mention(&name, &entity),
                ));
            }
            named.push(entity);
        }
    }

    // Consistent and Current: one check each; their issues are the state layer's
    e.check(Dimension::Consistent);
    e.check(Dimension::Current);

    // Connected
    e.check(Dimension::Connected);
    if b.block_type == tree::DOC && !linked_from_outside(cx.g, cx.m, id) {
        let path = cx.m.path_of(id).unwrap_or_default();
        e.issues.push(CoverageIssue::new(
            id,
            IssueCode::UnlinkedDoc,
            Some(path.clone()),
            fix_unlinked_doc(&path),
        ));
    }
    let dangling: BTreeSet<&BlockId> =
        cx.g.outgoing(id)
            .iter()
            .filter(|x| x.kind != EdgeKind::Contains && cx.m.get(&x.to).is_none())
            .map(|x| &x.to)
            .collect();
    for target in dangling {
        e.issues.push(CoverageIssue::new(
            id,
            IssueCode::DanglingRef,
            Some(target.to_string()),
            fix_dangling_ref(target),
        ));
    }

    // Complete
    if b.block_type == tree::DOC {
        e.check(Dimension::Complete);
        for title in missing_sections(cx, b) {
            let fix = fix_missing_section(&title);
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::MissingSection,
                Some(title),
                fix,
            ));
        }
    } else if b.block_type == "section" {
        e.check(Dimension::Complete);
        let stub =
            cx.m.children(Some(id))
                .iter()
                .all(|c| c.block_type == "section");
        if stub {
            let title = b.field_str("title").unwrap_or_default().to_string();
            let fix = fix_stub_section(&title);
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::StubSection,
                Some(title),
                fix,
            ));
        }
    } else if b.block_type == decision::QUESTION {
        e.check(Dimension::Complete);
        let answered =
            cx.g.incoming(id)
                .iter()
                .any(|x| is_link(x, decision::ANSWERS));
        if !answered {
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::OpenQuestion,
                None,
                fix_open_question(id),
            ));
        }
    }

    // Governed
    if b.block_type == decision::DECISION {
        if is_live(cx.g, id) && !needs_no_content(b) {
            e.check(Dimension::Governed);
            if !is_applied(cx, id) {
                e.issues.push(CoverageIssue::new(
                    id,
                    IssueCode::UnappliedDecision,
                    None,
                    fix_unapplied_decision(id),
                ));
            }
        }
    } else if is_normative(cx.pack, b) && effective_stable(cx.m, Some(id)) {
        e.check(Dimension::Governed);
        if !is_governed(cx.g, id) {
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::Ungrounded,
                None,
                fix_ungrounded(id),
            ));
        }
    }

    sort_issues(&mut e.issues, &BTreeMap::new());
    Some((e, named))
}

/// The structural layer of `scope`: [`entry_of`] for every block. A full build: counted by
/// [`structural_builds`].
pub(crate) fn structural_of(st: &ReadState, scope: &Scope) -> Result<Structural, RdyError> {
    STRUCTURAL_BUILDS.with(|c| c.set(c.get() + 1));
    let m = st.materialized(scope)?;
    let g = st.graph(scope)?;
    let lex = st.lexicon(scope)?;
    let cx = EntryCx {
        m: &m,
        g: &g,
        lex: &lex,
        pack: st.pack(),
    };
    let mut blocks: BTreeMap<BlockId, BlockEntry> = BTreeMap::new();
    let mut mentions: BTreeMap<BlockId, BTreeSet<BlockId>> = BTreeMap::new();
    for b in m.iter() {
        let Some((entry, named)) = entry_of(&cx, &b.id) else {
            continue;
        };
        for entity in named {
            mentions.entry(entity).or_default().insert(b.id.clone());
        }
        blocks.insert(b.id.clone(), entry);
    }
    Ok(Structural {
        blocks: blocks.into_iter().collect(),
        mentions: mentions
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect::<imbl::OrdSet<BlockId>>()))
            .collect(),
    })
}

/// Test hook: a full structural build of `scope` that ignores the memo.
#[doc(hidden)]
pub fn rebuilt_for_tests(st: &ReadState, scope: &Scope) -> Result<Structural, RdyError> {
    structural_of(st, scope)
}

/// Test hook: the memoised structural layer of `scope`, carried across writes or built.
#[doc(hidden)]
pub fn structural_for_tests(
    st: &ReadState,
    scope: &Scope,
) -> Result<std::sync::Arc<Structural>, RdyError> {
    st.structural(&st.coverage_scope(scope)?)
}
