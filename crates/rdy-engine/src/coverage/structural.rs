//! The structural layer of coverage (FW-CH1, FW-CV1): the Grounded, Connected, Complete and
//! Governed checks and issues of each scored block. Each block's entry reads only local graph
//! queries and its doc's level, so a carry can recompute any one block. The layer is a function of the scope's
//! build and the pack, memoised on the build.
use super::model::*;
use crate::brief::effective_stable;
use crate::error::E_LIMIT;
use crate::lexicon::{Lexicon, is_content, is_tie, link_kind_for, linked_from_outside, tied};
use crate::query::is_entity_in;
use crate::tensions::index::{is_deprecated, is_normative};
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::{DerivedStatus, EdgeKind, GoverningCache, GoverningStatuses, Graph};
use rdy_model::{Block, BlockId, FieldValue, decision, tree};
use rdy_schema::supporting::{REFLECTS_LINK, SOURCE};
use rdy_schema::{Glob, Pack, REFLECTS_FIELD, TypeClass};
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

/// Test hook: this thread's count of scans of an entity's incoming edges for the decisions that
/// affect it (the Governed check through the entities a block describes): the scans each
/// check's [`GoverningCache`] makes, summed.
#[doc(hidden)]
pub fn governed_scans() -> u64 {
    GOVERNED_SCANS.with(Cell::get)
}

/// nb4: the most docs one level may hold directly before a coverage read refuses (`E_LIMIT`).
pub(crate) const MAX_LEVEL_DOCS: usize = 100_000;
/// nb4: the most tree entries the level counts of one structural build, carry or briefing's
/// local entries visit before a coverage read refuses (`E_LIMIT`).
pub(crate) const MAX_STRUCTURE_ENTRIES: usize = 1_000_000;

/// The work guards of the level counts: the most docs one level holds directly, and the most
/// tree entries one context visits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub docs: usize,
    pub entries: usize,
}

impl Limits {
    /// The fixed guards: [`MAX_LEVEL_DOCS`] and [`MAX_STRUCTURE_ENTRIES`].
    pub(crate) const FIXED: Limits = Limits {
        docs: MAX_LEVEL_DOCS,
        entries: MAX_STRUCTURE_ENTRIES,
    };
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
    /// rf6: the decisions that affect each entity read so far, for the graph's governing
    /// routes: scanned once per context, not once per block that describes it (final review I1).
    governing: RefCell<GoverningCache>,
    /// nb4: how many docs each level read so far holds directly, by parent (`None`: the top
    /// level), counted once per context.
    levels: RefCell<HashMap<Option<BlockId>, usize>>,
    /// nb4: how many tree entries the level counts visited.
    visited: Cell<usize>,
    limits: Limits,
}

/// What an [`EntryCx`] memoised, kept between contexts on the same scope of the same state
/// (a briefing's batches of entries).
#[derive(Default)]
pub(crate) struct CxMemo {
    deprecated: HashMap<BlockId, bool>,
    governing: GoverningCache,
    levels: HashMap<Option<BlockId>, usize>,
    visited: usize,
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
            governing: RefCell::new(memo.governing),
            levels: RefCell::new(memo.levels),
            visited: Cell::new(memo.visited),
            limits: Limits::FIXED,
        }
    }

    /// This context with work guards `limits` in place of the fixed ones.
    #[cfg(test)]
    pub(crate) fn with_limits(mut self, limits: Limits) -> EntryCx<'a> {
        self.limits = limits;
        self
    }

    /// What this context memoised, for the next context on the same scope of the same state.
    pub(crate) fn into_memo(self) -> CxMemo {
        CxMemo {
            deprecated: self.deprecated.into_inner(),
            governing: self.governing.into_inner(),
            levels: self.levels.into_inner(),
            visited: self.visited.get(),
        }
    }

    /// nb4: how many docs the level under `parent` (`None`: the top level) holds directly,
    /// counted once per context. `E_LIMIT` past the level or structure guard.
    fn level_docs(&self, parent: Option<&BlockId>) -> Result<usize, RdyError> {
        let key = parent.cloned();
        if let Some(n) = self.levels.borrow().get(&key) {
            return Ok(*n);
        }
        let n = count_docs(
            self.m.children(parent).into_iter(),
            &self.visited,
            self.limits,
            parent,
            &|p| self.m.path_of(p).unwrap_or_default(),
        )?;
        self.levels.borrow_mut().insert(key, n);
        Ok(n)
    }

    /// rf6: whether a live decision governs `id` by one of the graph's four routes. Counts the
    /// entity scans this query adds to [`governed_scans`].
    fn live_route(&self, id: &BlockId) -> bool {
        let mut cache = self.governing.borrow_mut();
        let before = cache.entity_scans();
        let found = !self
            .g
            .governing_routes(
                id,
                GoverningStatuses::Live,
                |e| is_entity_in(self.pack, self.m, e),
                &mut cache,
            )
            .is_empty();
        let scans = (cache.entity_scans() - before) as u64;
        GOVERNED_SCANS.with(|c| c.set(c.get() + scans));
        found
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

/// nb4: the docs among `children`, the entries of the level under `parent` (`None`: the top
/// level, else a folder whose path `path` gives), each child counted in `visited`. `E_LIMIT` at
/// the first doc past `limits.docs` or the first entry past `limits.entries`; nothing is
/// collected.
fn count_docs<'b>(
    children: impl Iterator<Item = &'b Block>,
    visited: &Cell<usize>,
    limits: Limits,
    parent: Option<&BlockId>,
    path: &dyn Fn(&BlockId) -> String,
) -> Result<usize, RdyError> {
    let mut docs = 0;
    for c in children {
        let seen = visited.get() + 1;
        if seen > limits.entries {
            return Err(RdyError::new(
                E_LIMIT,
                format!("coverage structure exceeds {} tree entries", limits.entries),
                "split the project into smaller projects",
            ));
        }
        visited.set(seen);
        if c.block_type == tree::DOC {
            docs += 1;
            if docs > limits.docs {
                let (subject, folder) = level_names(parent, path);
                return Err(RdyError::new(
                    E_LIMIT,
                    format!("coverage level {subject} exceeds {} docs", limits.docs),
                    fix_crowded_level(&folder),
                ));
            }
        }
    }
    Ok(docs)
}

/// How a level is named (O6): its subject and messages give the folder's path, or `/` for the
/// top level; its fixes give the folder's path, or `the top level`.
fn level_names(parent: Option<&BlockId>, path: &dyn Fn(&BlockId) -> String) -> (String, String) {
    match parent {
        Some(p) => {
            let path = path(p);
            (path.clone(), path)
        }
        None => ("/".to_string(), "the top level".to_string()),
    }
}

/// nb4 (D-YXBXH, O6): whether `body`, trimmed, is one question: nonempty, ending in `?`, with no
/// earlier `.`, `!` or `?` that Unicode whitespace follows.
fn is_one_question(body: &str) -> bool {
    let t = body.trim();
    let Some(head) = t.strip_suffix('?') else {
        return false;
    };
    let mut chars = head.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '.' | '!' | '?') && chars.peek().is_some_and(|n| n.is_whitespace()) {
            return false;
        }
    }
    true
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

/// Whether block `b` is governed (rf6, D-X4RYX): a live decision governs it by one of the
/// graph's four routes (D-VPATZ, D-TQD4D), it has a `reflects` link to a source the scope holds,
/// or its `reflects` field is a nonempty list of valid path globs (counted without a linked
/// checkout and without `reflects_at`). A source is no governing decision: it only grounds.
fn is_governed(cx: &EntryCx<'_>, b: &Block) -> bool {
    cx.live_route(&b.id) || reflects_held_source(cx, &b.id) || reflects_globs(b)
}

/// rf6: whether `id` has a `reflects` link to a block the scope holds that is a source. A
/// dangling target or one of another type does not count.
fn reflects_held_source(cx: &EntryCx<'_>, id: &BlockId) -> bool {
    cx.g.outgoing(id).iter().any(|e| {
        is_link(e, REFLECTS_LINK) && cx.m.get(&e.to).is_some_and(|t| t.block_type == SOURCE)
    })
}

/// rf6: whether `b`'s `reflects` field is a nonempty list whose every entry is a path glob that
/// compiles. An empty list, or one with an entry that is no valid glob, does not count.
fn reflects_globs(b: &Block) -> bool {
    match b.fields.get(REFLECTS_FIELD) {
        Some(FieldValue::List(items)) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|i| i.as_str().is_some_and(|g| Glob::compile(g).is_ok()))
        }
        _ => false,
    }
}

/// Whether decision `d` is applied (D-ET9RX): a block that is neither a decision nor of the
/// supporting class links to it by `decided_by`, `implements` or a reference, or `d` affects a
/// block that is neither a decision nor of the supporting class (rf6, D-Z8P09).
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
                && m.get(&e.to).is_some_and(|t| {
                    t.block_type != decision::DECISION
                        && pack.class_of(&t.block_type) != TypeClass::Supporting
                })
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
/// queries: the block's own edges, its children, its doc's subtree for `unlinked_doc`, the
/// incoming edges of the entities it describes, the blocks its `reflects` links name, and (nb4)
/// a doc's level, counted once per context.
///
/// nb4: `E_LIMIT` when a doc's level, or the levels this context counted, pass a work guard
/// ([`MAX_LEVEL_DOCS`], [`MAX_STRUCTURE_ENTRIES`]).
pub(crate) fn entry_of(
    cx: &EntryCx<'_>,
    id: &BlockId,
) -> Result<Option<(BlockEntry, Vec<BlockId>)>, RdyError> {
    let Some(b) = cx.m.get(id) else {
        return Ok(None);
    };
    if cx.pack.is_presentational(&b.block_type) {
        return Ok(None);
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
    for located in cx.lex.mentions_located(b) {
        let mention = &located.mention;
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
                let mut issue = CoverageIssue::new(
                    id,
                    IssueCode::UnlinkedMention,
                    Some(entity.to_string()),
                    fix_unlinked_mention(&name, &entity),
                );
                issue.text = Some(located.text.clone());
                e.issues.push(issue);
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
    // a citation an accepted decision keeps after its target's deletion is not dangling: the one
    // predicate write, proposal and landing ask (p-QMGB4; task-5 R1, final-round1)
    let dangling: BTreeSet<&BlockId> =
        cx.g.outgoing(id)
            .iter()
            .filter(|x| {
                x.kind != EdgeKind::Contains
                    && cx.m.get(&x.to).is_none()
                    && !rdy_graph::keeps_deleted_citation(b, &x.to)
            })
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
        // nb4: the doc's level holds too many docs directly; the same check
        if cx.level_docs(b.parent.as_ref())? > cx.pack.max_docs_per_level() as usize {
            let (subject, folder) =
                level_names(b.parent.as_ref(), &|p| cx.m.path_of(p).unwrap_or_default());
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::CrowdedLevel,
                Some(subject),
                fix_crowded_level(&folder),
            ));
        }
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
    } else if b.block_type == PROSE || b.block_type == NOTE {
        // nb4: a prose or note block that is one question
        e.check(Dimension::Complete);
        if is_one_question(&b.body) {
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::QuestionAsProse,
                None,
                fix_question_as_prose(id),
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
        if !is_governed(cx, b) {
            e.issues.push(CoverageIssue::new(
                id,
                IssueCode::Ungrounded,
                None,
                fix_ungrounded(id),
            ));
        }
    }

    sort_issues(&mut e.issues, &BTreeMap::new());
    Ok(Some((e, named)))
}

/// The types whose body a question written as prose can be.
const PROSE: &str = "prose";
const NOTE: &str = "note";

/// The structural layer of `scope`: [`entry_of`] for every block. A full build: counted by
/// [`structural_builds`].
pub(crate) fn structural_of(st: &ReadState, scope: &Scope) -> Result<Structural, RdyError> {
    let m = st.materialized(scope)?;
    let g = st.graph(scope)?;
    let lex = st.lexicon(scope)?;
    structural_in(&m, &g, &lex, st.pack())
}

/// The structural layer of materialization `m`, whose graph is `g` and whose lexicon is `lex`,
/// built in full (what [`structural_of`] builds for a scope). Counted by [`structural_builds`].
/// `E_LIMIT` past a work guard: no layer at all, never part of one.
pub(crate) fn structural_in(
    m: &Materialized,
    g: &Graph,
    lex: &Lexicon,
    pack: &Pack,
) -> Result<Structural, RdyError> {
    STRUCTURAL_BUILDS.with(|c| c.set(c.get() + 1));
    build_layer(&EntryCx::new(m, g, lex, pack))
}

/// [`entry_of`] for every block of `cx`'s materialization.
fn build_layer(cx: &EntryCx<'_>) -> Result<Structural, RdyError> {
    let mut blocks: BTreeMap<BlockId, BlockEntry> = BTreeMap::new();
    let mut mentions: BTreeMap<BlockId, BTreeSet<BlockId>> = BTreeMap::new();
    for b in cx.m.iter() {
        let Some((entry, named)) = entry_of(cx, &b.id)? else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_schema::PackSet;

    fn pack() -> Pack {
        PackSet::with_builtin().resolve("rdy-standard").unwrap()
    }

    fn block(id: &str, block_type: &str, parent: Option<&str>, name: &str) -> Block {
        let mut fields = BTreeMap::new();
        if !name.is_empty() {
            fields.insert("name".to_string(), FieldValue::Str(name.to_string()));
            fields.insert("title".to_string(), FieldValue::Str(name.to_string()));
        }
        Block {
            id: BlockId::parse(id).unwrap(),
            block_type: block_type.to_string(),
            parent: parent.map(|p| BlockId::parse(p).unwrap()),
            order: id.to_string(),
            fields,
            body: String::new(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            read_identity: None,
        }
    }

    /// A doc id with body `n` (Crockford digits only).
    fn doc_id(n: usize) -> String {
        format!("d-{n:05}")
    }

    /// Folder `f-AREA0` holding `in_folder` docs, and `at_top` docs at the top level.
    fn state(at_top: usize, in_folder: usize) -> Materialized {
        let mut ops = vec![rdy_model::Op::Create {
            block: block("f-AREA0", tree::FOLDER, None, "area"),
        }];
        for i in 0..at_top + in_folder {
            let parent = (i >= at_top).then_some("f-AREA0");
            let id = doc_id(i);
            ops.push(rdy_model::Op::Create {
                block: block(&id, tree::DOC, parent, &format!("doc{i}")),
            });
        }
        Materialized::default().with(&ops)
    }

    /// The layer of `m` built with the level and structure limits `limits`.
    fn build(m: &Materialized, limits: Limits) -> Result<Structural, RdyError> {
        let p = pack();
        let g = Graph::build_from(m.iter());
        let lex = Lexicon::build(&p, m.iter());
        let cx = EntryCx::new(m, &g, &lex, &p).with_limits(limits);
        build_layer(&cx)
    }

    #[test]
    fn nb4_structure_bounds_fail_without_partial_scores() {
        // the fixed bounds
        assert_eq!(MAX_LEVEL_DOCS, 100_000);
        assert_eq!(MAX_STRUCTURE_ENTRIES, 1_000_000);
        let fixed = Limits::FIXED;
        assert_eq!((fixed.docs, fixed.entries), (100_000, 1_000_000));

        // the counter at each fixed bound, over that many entries without a copy of any
        let doc = block("d-00000", tree::DOC, None, "doc");
        let folder = block("f-00000", tree::FOLDER, None, "area");
        let visited = Cell::new(0);
        let n = count_docs(
            std::iter::repeat_n(&doc, 100_000),
            &visited,
            fixed,
            None,
            &|_| "area".to_string(),
        )
        .unwrap();
        assert_eq!(n, 100_000, "at the level bound");
        let visited = Cell::new(0);
        let e = count_docs(
            std::iter::repeat_n(&doc, 100_001),
            &visited,
            fixed,
            None,
            &|_| "area".to_string(),
        )
        .unwrap_err();
        assert_eq!(e.code, crate::error::E_LIMIT);
        assert_eq!(e.message, "coverage level / exceeds 100000 docs");
        assert_eq!(e.fix, "group the docs of the top level into folders");
        assert_eq!(visited.get(), 100_001, "it stops at the first excess");
        let at = BlockId::parse("f-00000").unwrap();
        let e = count_docs(
            std::iter::repeat_n(&doc, 100_001),
            &Cell::new(0),
            fixed,
            Some(&at),
            &|_| "area/sub".to_string(),
        )
        .unwrap_err();
        assert_eq!(e.message, "coverage level area/sub exceeds 100000 docs");
        assert_eq!(e.fix, "group the docs of area/sub into folders");
        // entries that are no docs count toward the structure bound only
        let visited = Cell::new(0);
        let n = count_docs(
            std::iter::repeat_n(&folder, 1_000_000),
            &visited,
            fixed,
            None,
            &|_| String::new(),
        )
        .unwrap();
        assert_eq!((n, visited.get()), (0, 1_000_000), "at the structure bound");
        let e = count_docs(std::iter::once(&folder), &visited, fixed, None, &|_| {
            String::new()
        })
        .unwrap_err();
        assert_eq!(e.code, crate::error::E_LIMIT);
        assert_eq!(e.message, "coverage structure exceeds 1000000 tree entries");
        assert_eq!(e.fix, "split the project into smaller projects");

        // a build that passes a bound fails whole: no partial layer, no score
        let m = state(3, 4);
        let ok = build(
            &m,
            Limits {
                docs: 4,
                entries: 8,
            },
        )
        .unwrap();
        assert_eq!(ok.blocks.len(), 8, "the folder and seven docs");
        let e = build(
            &m,
            Limits {
                docs: 3,
                entries: 8,
            },
        )
        .unwrap_err();
        assert_eq!(e.message, "coverage level area exceeds 3 docs");
        assert_eq!(e.fix, "group the docs of area into folders");
        let e = build(
            &m,
            Limits {
                docs: 4,
                entries: 7,
            },
        )
        .unwrap_err();
        assert_eq!(e.message, "coverage structure exceeds 7 tree entries");
        let m = state(4, 0);
        let e = build(
            &m,
            Limits {
                docs: 3,
                entries: 100,
            },
        )
        .unwrap_err();
        assert_eq!(e.message, "coverage level / exceeds 3 docs");
        assert_eq!(e.fix, "group the docs of the top level into folders");
    }
}
