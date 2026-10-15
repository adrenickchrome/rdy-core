//! The structural layer of coverage (FW-CH1, FW-CV1): the Grounded, Connected, Complete and
//! Governed checks and issues of each scored block. Each block's entry reads only local graph
//! queries, so a carry can recompute any one block. The layer is a function of the scope's
//! build and the pack, memoised on the build.
use super::model::*;
use crate::brief::effective_stable;
use crate::lexicon::{Lexicon, is_content, is_tie, link_kind_for, linked_from_outside, tied};
use crate::tensions::index::{is_deprecated, is_normative};
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{DerivedStatus, EdgeKind, Graph};
use rdy_model::{Block, BlockId, FieldValue, decision, tree};
use rdy_schema::{Pack, TypeClass};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

thread_local! {
    static STRUCTURAL_BUILDS: Cell<u64> = const { Cell::new(0) };
    static GOVERNED_SCANS: Cell<u64> = const { Cell::new(0) };
}

/// Test hook: this thread's count of full structural builds (`structural_of` calls).
#[doc(hidden)]
pub fn structural_builds() -> u64 {
    STRUCTURAL_BUILDS.with(Cell::get)
}

/// Test hook: this thread's count of scans of an entity's incoming edges for a live `affects`
/// (the Governed check through the entities a block describes).
#[doc(hidden)]
pub fn governed_scans() -> u64 {
    GOVERNED_SCANS.with(Cell::get)
}

/// What one block's structural entry reads, with what its entries share, memoised per entity
/// for the life of the context (one build or one carry).
pub(crate) struct EntryCx<'a> {
    pub m: &'a Materialized,
    pub g: &'a Graph,
    pub lex: &'a Lexicon,
    pub pack: &'a Pack,
    /// Whether each entity read so far is deprecated in the scope.
    deprecated: RefCell<HashMap<BlockId, bool>>,
    /// Whether each block read so far has a live decision that affects it: read once per
    /// context, not once per block that describes it (final review I1).
    affected: RefCell<HashMap<BlockId, bool>>,
}

/// What an [`EntryCx`] memoised, kept between contexts on the same scope of the same state
/// (a briefing's batches of entries).
#[derive(Default)]
pub(crate) struct CxMemo {
    deprecated: HashMap<BlockId, bool>,
    affected: HashMap<BlockId, bool>,
}

impl<'a> EntryCx<'a> {
    pub(crate) fn new(
        m: &'a Materialized,
        g: &'a Graph,
        lex: &'a Lexicon,
        pack: &'a Pack,
    ) -> EntryCx<'a> {
        EntryCx::resume(m, g, lex, pack, CxMemo::default())
    }

    /// A context that starts from `memo`, which an earlier context on the same scope of the
    /// same state left ([`EntryCx::into_memo`]).
    pub(crate) fn resume(
        m: &'a Materialized,
        g: &'a Graph,
        lex: &'a Lexicon,
        pack: &'a Pack,
        memo: CxMemo,
    ) -> EntryCx<'a> {
        EntryCx {
            m,
            g,
            lex,
            pack,
            deprecated: RefCell::new(memo.deprecated),
            affected: RefCell::new(memo.affected),
        }
    }

    /// What this context memoised, for the next context on the same scope of the same state.
    pub(crate) fn into_memo(self) -> CxMemo {
        CxMemo {
            deprecated: self.deprecated.into_inner(),
            affected: self.affected.into_inner(),
        }
    }

    /// Whether a live decision affects `target` (an incoming `affects` edge).
    fn affected_live(&self, target: &BlockId) -> bool {
        if let Some(a) = self.affected.borrow().get(target) {
            return *a;
        }
        GOVERNED_SCANS.with(|c| c.set(c.get() + 1));
        let a = self
            .g
            .incoming(target)
            .iter()
            .any(|a| is_link(a, decision::AFFECTS) && is_live(self.g, &a.from));
        self.affected.borrow_mut().insert(target.clone(), a);
        a
    }

    /// Whether `entity` is deprecated in the scope, by the deprecated reference rule
    /// (`is_deprecated`, D-S46DE).
    fn deprecated(&self, entity: &BlockId) -> bool {
        if let Some(d) = self.deprecated.borrow().get(entity) {
            return *d;
        }
        let d = is_deprecated(self.m, self.g, entity);
        self.deprecated.borrow_mut().insert(entity.clone(), d);
        d
    }
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
fn is_governed(cx: &EntryCx<'_>, id: &BlockId) -> bool {
    if !cx.g.governing(id).is_empty() {
        return true;
    }
    cx.g.outgoing(id)
        .iter()
        .filter(|e| is_link(e, decision::DESCRIBES))
        .any(|e| cx.affected_live(&e.to))
}

/// Whether decision `d` is applied (D-ET9RX): a block that is neither a decision nor of the
/// supporting class links to it by `decided_by`, `implements` or a reference, or `d` affects a
/// block that is not a decision.
fn is_applied(m: &Materialized, g: &Graph, pack: &Pack, d: &BlockId) -> bool {
    let carries = g.incoming(d).iter().any(|e| {
        (e.kind == EdgeKind::References
            || is_link(e, decision::DECIDED_BY)
            || is_link(e, decision::IMPLEMENTS))
            && m.get(&e.from).is_some_and(|f| {
                f.block_type != decision::DECISION
                    && pack.class_of(&f.block_type) != TypeClass::Supporting
            })
    });
    carries
        || g.outgoing(d).iter().any(|e| {
            is_link(e, decision::AFFECTS)
                && m.get(&e.to)
                    .is_some_and(|t| t.block_type != decision::DECISION)
        })
}

/// Whether decision `b` says it needs no content (`needs_content: false`).
fn needs_no_content(b: &Block) -> bool {
    b.fields.get("needs_content") == Some(&FieldValue::Bool(false))
}

/// Whether decision `b`, once it is live, would be unapplied (cc16, FW-CH5): no content applies
/// it and it does not say it needs none. A proposed decision counts, as a landing accepts it; a
/// rejected, superseded or revoked one does not.
pub(crate) fn lands_unapplied(m: &Materialized, g: &Graph, pack: &Pack, b: &Block) -> bool {
    b.block_type == decision::DECISION
        && matches!(
            g.status(&b.id),
            Some(
                DerivedStatus::Proposed
                    | DerivedStatus::Live
                    | DerivedStatus::PartiallySuperseded { .. }
            )
        )
        && !needs_no_content(b)
        && !is_applied(m, g, pack, &b.id)
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

/// The entities `b`'s text names, as [`entry_of`] lists them, linked or not and whether or not
/// `b` can link: none for a presentational block (not scored). A carry reads it for a block's
/// version before the write.
pub(crate) fn named_in(pack: &Pack, lex: &Lexicon, b: &Block) -> Vec<BlockId> {
    if pack.is_presentational(&b.block_type) {
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
    // every scored block's mentions are listed (a fact sheet's neighbourhood); only a block
    // that can link has a Grounded check per mention
    let can_link = link_kind_for(cx.pack, b).is_some();
    for mention in cx.lex.mentions(b) {
        let Ok(entity) = BlockId::parse(&mention.entity) else {
            continue;
        };
        let linked = can_link && tied(cx.g, id, &entity);
        // an unlinked mention of a deprecated entity is no linking gap: the deprecated reference
        // tension reports it, and its fix is to reword (D-S46DE; final review I4)
        if can_link && (linked || !cx.deprecated(&entity)) {
            e.check(Dimension::Grounded);
            if !linked {
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
        }
        named.push(entity);
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
            if !is_applied(cx.m, cx.g, cx.pack, id) {
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
        if !is_governed(cx, id) {
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
    let m = st.materialized(scope)?;
    let g = st.graph(scope)?;
    let lex = st.lexicon(scope)?;
    Ok(structural_in(&m, &g, &lex, st.pack()))
}

/// The structural layer of materialization `m`, whose graph is `g` and whose lexicon is `lex`,
/// built in full (what [`structural_of`] builds for a scope). Counted by [`structural_builds`].
pub(crate) fn structural_in(m: &Materialized, g: &Graph, lex: &Lexicon, pack: &Pack) -> Structural {
    STRUCTURAL_BUILDS.with(|c| c.set(c.get() + 1));
    let cx = EntryCx::new(m, g, lex, pack);
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
    Structural {
        blocks: blocks.into_iter().collect(),
        mentions: mentions
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect::<imbl::OrdSet<BlockId>>()))
            .collect(),
    }
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
