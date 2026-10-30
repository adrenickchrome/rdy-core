//! The diff request (D-HZYK0): what `rdy_diff` and `rdy diff` ask for, and the frozen reply.
//!
//! [`crate::diff`] compares two states. This module says which two: the one-argument defaults,
//! the scope labels and the root. It then shapes the reply: compact or detail, and the page of it
//! that fits a token budget.
//!
//! **The sides.** With both scopes named, the diff reads them as given. With one, rdy supplies
//! the other: a view diffs from its effective base landing, canon from its previous landing, a
//! proposal version from its base landing, and a tag or a landing diffs to canon. A side with no
//! landing to read is the empty state. `inflight` has no single older side, so it needs both.
//!
//! **The root.** `doc` resolves in `to`, then in `from`, and it names a doc or a folder.
//!
//! **The cut.** A page fits when (wrapper + fixed bytes + entry bytes) / 4 is at most the budget.
//! The fixed bytes are the reply without entries, counted twice: F0 as a whole reply, and F1 with
//! `truncated`, `omitted` at the whole counts and the longest `next`. A cut removes bodies, last
//! entry first, then field values, last entry first, then keeps the longest run of entries from
//! the page's start. It counts each entry's bytes when it first needs that form, and it never
//! serializes the whole reply again. `counts` cover the whole diff, so every page computes it.

use std::sync::Arc;

use rdy_graph::Graph;
use rdy_model::{BlockId, Hash, tree};
use serde::Serialize;

use crate::cursor;
use crate::diff::{DiffChange, DiffCounts, DiffEntry, add_decision_status, diff_states_under};
use crate::error::*;
use crate::gate::LandingRow;
use crate::read_state::ReadState;
use crate::{Materialized, Scope};

/// What `rdy_diff` and `rdy diff` ask for.
#[derive(Clone, Debug, Default)]
pub struct DiffRequest {
    /// The older scope, or the one scope that rdy supplies the other side of.
    pub from: String,
    pub to: Option<String>,
    /// Limits both sides to one doc or folder: an id, a key or a path.
    pub doc: Option<String>,
    /// Adds field values and the word pieces of each changed body.
    pub detail: bool,
}

/// What a scope label names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelKind {
    Canon,
    View,
    Tag,
    Landing,
    Proposal,
    Inflight,
    Empty,
}

/// One side of a diff, as the reply names it. An absent member is left out of the JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScopeLabel {
    pub kind: LabelKind,
    /// A view's name, a tag's name as stored, or `P-<stable short>@<n>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The landing's 64 hex characters: canon's head, a tag's landing or a named landing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing: Option<String>,
    /// A proposal version's 64 hex characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ScopeLabel {
    fn of(kind: LabelKind) -> Self {
        Self {
            kind,
            name: None,
            landing: None,
            version: None,
        }
    }

    fn landing(id: &Hash) -> Self {
        Self {
            landing: Some(id.to_string()),
            ..Self::of(LabelKind::Landing)
        }
    }
}

/// The frozen reply: its key sets, its values, absent against `null` and the order of every array,
/// not the order of keys in the JSON object. rdy_schema calls this shape `Diff`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffReply {
    pub from: ScopeLabel,
    pub to: ScopeLabel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<BlockId>,
    pub entries: Vec<DiffEntry>,
    /// The whole diff's counts. A cut never changes them.
    pub counts: DiffCounts,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// The entries after this page, by change kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted: Option<DiffCounts>,
    /// Passed back as `page`, it reads the entries after this page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// One landing: its `rdy log` row and its compact diff from its previous landing.
#[derive(Clone, Debug, Serialize)]
pub struct LandingShow {
    pub landing: LandingRow,
    pub diff: DiffReply,
}

/// One side of a diff: a scope with its label, or the empty state.
pub(crate) enum DiffSide {
    Empty,
    Scope { scope: Scope, label: ScopeLabel },
}

impl DiffSide {
    fn label(&self) -> ScopeLabel {
        match self {
            DiffSide::Empty => ScopeLabel::of(LabelKind::Empty),
            DiffSide::Scope { label, .. } => label.clone(),
        }
    }
}

/// What `rdy_schema` says of the reply.
pub const DIFF_DOC: &str = "rdy_diff returns {from, to, root?, entries, counts: {added, removed, updated, moved}, truncated?, omitted?, next?}. from and to are scope labels {kind, name?, landing?, version?}; kind is canon, view, tag, landing, proposal, inflight or empty. An entry is {id, block_type, doc, path, change, summary, from_version, to_version, moved?, type?, fields: [{name, from?, to?}], links: [{rel, added, removed}], decision_status?: {from, to}, repinned, body?: [{op, text}]}. change is added, removed, updated or moved; an entry that moved and changed is updated with moved set. moved is {from_parent, to_parent, from_path, to_path, reordered_only}. op is same, del or add. Only detail: true adds field values and body. A cut result carries next: pass it as page with the same request. The shape only grows.";

/// The token budget of a page when the caller names none.
pub const DIFF_DEFAULT_TOKENS: usize = 4_000;

/// The fix of both root refusals.
const ROOT_FIX: &str = "name a doc or folder that one side holds: rdy_search / rdy search";

impl ReadState {
    /// The two sides of `req` and their labels, resolved with the session's view.
    pub(crate) fn diff_sides(
        &self,
        req: &DiffRequest,
        own_view: Option<&str>,
    ) -> Result<(DiffSide, DiffSide), RdyError> {
        let named = self.labelled_side(&req.from, own_view)?;
        if let Some(to) = &req.to {
            return Ok((named, self.labelled_side(to, own_view)?));
        }
        let DiffSide::Scope { scope, .. } = &named else {
            return Err(internal("a named scope read as the empty state"));
        };
        let older = match scope {
            Scope::Inflight => {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "inflight needs both sides",
                    "name both scopes: rdy_diff {from, to} / rdy diff <from> <to>",
                ));
            }
            // a tag or a landing is the older side already
            Scope::At(_) => return Ok((named, self.labelled_side("canon", own_view)?)),
            Scope::View(name) => self.landing_side(self.effective_base(name)),
            Scope::Proposal(version) => self.landing_side(self.proposal_base(version)?),
            Scope::Canon => {
                let prev = match self.canon_head() {
                    Some(head) => self.reader().landing(&head)?.prev,
                    None => None,
                };
                self.landing_side(prev)
            }
        };
        Ok((older, named))
    }

    /// The scope `text` names and its label.
    fn labelled_side(&self, text: &str, own_view: Option<&str>) -> Result<DiffSide, RdyError> {
        let scope = self.resolve_read_scope(text, own_view)?;
        let label = match &scope {
            Scope::Canon => ScopeLabel {
                landing: self.canon_head().map(|h| h.to_string()),
                ..ScopeLabel::of(LabelKind::Canon)
            },
            Scope::View(name) => ScopeLabel {
                name: Some(name.clone()),
                ..ScopeLabel::of(LabelKind::View)
            },
            Scope::Inflight => ScopeLabel::of(LabelKind::Inflight),
            Scope::At(landing) => match text.strip_prefix("tag:") {
                Some(name) => ScopeLabel {
                    name: Some(self.stored_tag_name(name)?),
                    landing: Some(landing.to_string()),
                    ..ScopeLabel::of(LabelKind::Tag)
                },
                None => ScopeLabel::landing(landing),
            },
            Scope::Proposal(version) => {
                let spelled = text.strip_prefix("proposal:").unwrap_or(text);
                let pref = crate::gate::resolve_in(self, spelled)?;
                ScopeLabel {
                    name: Some(pref.to_string()),
                    version: Some(version.to_string()),
                    ..ScopeLabel::of(LabelKind::Proposal)
                }
            }
        };
        Ok(DiffSide::Scope { scope, label })
    }

    /// The name a tag holds in the store, for `tag:<name>` as a request spells it.
    fn stored_tag_name(&self, name: &str) -> Result<String, RdyError> {
        let want = crate::version::parse_version(name)?;
        self.version_tags()?
            .into_iter()
            .find(|(v, _)| v.cmp_precedence(&want).is_eq())
            .map(|(_, t)| t.name)
            .ok_or_else(|| internal("a tag that resolved has no stored name"))
    }

    /// A landing as a side, labelled `landing`, or the empty state. Canon's head reads through
    /// `Scope::Canon`, which is built already.
    fn landing_side(&self, landing: Option<Hash>) -> DiffSide {
        let Some(id) = landing else {
            return DiffSide::Empty;
        };
        let scope = if Some(id) == self.canon_head() {
            Scope::Canon
        } else {
            Scope::At(id)
        };
        DiffSide::Scope {
            scope,
            label: ScopeLabel::landing(&id),
        }
    }

    fn side_state(&self, side: &DiffSide) -> Result<Arc<Materialized>, RdyError> {
        match side {
            DiffSide::Empty => Ok(Arc::new(Materialized::default())),
            DiffSide::Scope { scope, .. } => self.materialized(scope),
        }
    }

    fn side_graph(&self, side: &DiffSide) -> Result<Arc<Graph>, RdyError> {
        match side {
            DiffSide::Empty => Ok(Arc::new(Graph::build_with(
                std::iter::empty(),
                Arc::clone(self.reader().tracking()),
            ))),
            DiffSide::Scope { scope, .. } => self.graph(scope),
        }
    }

    /// The whole diff of two sides under the root: the engine's diff, then the status pass, then
    /// compact unless `detail`. No cut.
    pub(crate) fn diff_between(
        &self,
        from: &DiffSide,
        to: &DiffSide,
        doc: Option<&str>,
        detail: bool,
    ) -> Result<DiffReply, RdyError> {
        let (mf, mt) = (self.side_state(from)?, self.side_state(to)?);
        let root = diff_root(doc, &mf, &mt)?;
        let mut diff = diff_states_under(&mf, &mt, root.as_ref());
        let (gf, gt) = (self.side_graph(from)?, self.side_graph(to)?);
        add_decision_status(&mut diff, &mf, &mt, &gf, &gt, root.as_ref());
        if !detail {
            for e in &mut diff.entries {
                strip_body(e);
                strip_values(e);
            }
        }
        Ok(DiffReply {
            from: from.label(),
            to: to.label(),
            root,
            entries: diff.entries,
            counts: diff.counts,
            truncated: false,
            omitted: None,
            next: None,
        })
    }

    /// `diff_sides` then `diff_between`: what `rdy diff` prints.
    pub fn diff_reply(
        &self,
        req: &DiffRequest,
        own_view: Option<&str>,
    ) -> Result<DiffReply, RdyError> {
        let (from, to) = self.diff_sides(req, own_view)?;
        self.diff_between(&from, &to, req.doc.as_deref(), req.detail)
    }

    /// The page of `req` that fits `max_tokens`, with no wrapper; cursors name `rdy_diff`.
    pub fn diff_page(
        &self,
        req: &DiffRequest,
        own_view: Option<&str>,
        page: Option<&str>,
        max_tokens: usize,
    ) -> Result<DiffReply, RdyError> {
        let (from, to) = self.diff_sides(req, own_view)?;
        self.diff_between_page(
            &from,
            &to,
            req.doc.as_deref(),
            req.detail,
            page,
            max_tokens,
            0,
            "rdy_diff",
        )
    }

    /// The cut of `diff_between`'s reply: `wrapper` is the caller's bytes around the diff, `tool`
    /// names the cursor's tool in its refusal.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn diff_between_page(
        &self,
        from: &DiffSide,
        to: &DiffSide,
        doc: Option<&str>,
        detail: bool,
        page: Option<&str>,
        max_tokens: usize,
        wrapper: usize,
        tool: &str,
    ) -> Result<DiffReply, RdyError> {
        let cursor = page.map(|p| cursor::parse(p, tool)).transpose()?;
        let (mf, mt) = (self.side_state(from)?, self.side_state(to)?);
        let token = page_token(&mf, &mt, diff_root(doc, &mf, &mt)?.as_ref(), detail);
        if cursor.is_some_and(|(t, _)| t != token) {
            return Err(RdyError::new(
                E_PAGE_STALE,
                "the diff behind this page changed since the first page, or the request differs",
                PAGE_STALE_FIX,
            ));
        }
        let reply = self.diff_between(from, to, doc, detail)?;
        let offset = match cursor {
            None => 0,
            Some((_, offset)) if offset >= reply.entries.len() => {
                return Err(bad_cursor(tool));
            }
            Some((_, offset)) => offset,
        };
        cut(reply, offset, &token, max_tokens, wrapper)
    }

    /// The landing's row and its compact diff from `prev` (or the empty state) to it. `max_tokens`
    /// None and no `page`: the whole diff (the CLI). Else the whole reply, the row and the wrapper
    /// included, fits the budget; cursors name `rdy_log`.
    pub fn landing_show(
        &self,
        id: &Hash,
        page: Option<&str>,
        max_tokens: Option<usize>,
    ) -> Result<LandingShow, RdyError> {
        let landing = self.log_row(id)?;
        let from = self.landing_side(self.reader().landing(id)?.prev);
        let to = self.landing_side(Some(*id));
        let diff = if page.is_none() && max_tokens.is_none() {
            self.diff_between(&from, &to, None, false)?
        } else {
            // the bytes of `{"landing":<row>,"diff":` and the closing `}`
            let wrapper = r#"{"landing":"#.len() + json_len(&landing)? + r#","diff":"#.len() + 1;
            self.diff_between_page(
                &from,
                &to,
                None,
                false,
                page,
                max_tokens.unwrap_or(DIFF_DEFAULT_TOKENS),
                wrapper,
                "rdy_log",
            )?
        };
        Ok(LandingShow { landing, diff })
    }
}

fn internal(message: &str) -> RdyError {
    RdyError::new(E_INTERNAL, message, "report this as a bug in rdy")
}

/// The refusal of a cursor past the end: `cursor::parse`'s own, from text it never takes.
fn bad_cursor(tool: &str) -> RdyError {
    match cursor::parse("", tool) {
        Err(e) => e,
        Ok(_) => internal("an empty cursor parsed"),
    }
}

/// The block `doc` names on one side, with its type. `None`: this side does not hold it.
fn held_root(m: &Materialized, doc: &str) -> Result<Option<(BlockId, String)>, RdyError> {
    match crate::keys::resolve_text(m, doc) {
        Ok(id) => Ok(m.get(&id).map(|b| (id, b.block_type.clone()))),
        Err(e) if e.code == E_UNKNOWN_BLOCK => Ok(None),
        Err(e) => Err(e),
    }
}

/// `doc` resolved in `to`, then in `from`; it names a doc or a folder.
fn diff_root(
    doc: Option<&str>,
    from: &Materialized,
    to: &Materialized,
) -> Result<Option<BlockId>, RdyError> {
    let Some(doc) = doc else {
        return Ok(None);
    };
    let found = match held_root(to, doc)? {
        Some(found) => Some(found),
        None => held_root(from, doc)?,
    };
    let Some((id, block_type)) = found else {
        return Err(RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("{doc} is on neither side"),
            ROOT_FIX,
        ));
    };
    if !tree::is_tree_type(&block_type) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{doc} is a {block_type}, not a doc or a folder"),
            ROOT_FIX,
        ));
    }
    Ok(Some(id))
}

/// The token of a diff's pages: a changed state, root or `detail` stales the cursor.
fn page_token(
    from: &Materialized,
    to: &Materialized,
    root: Option<&BlockId>,
    detail: bool,
) -> Hash {
    let mut bytes = b"rdy-diff-1\0".to_vec();
    bytes.extend_from_slice(&from.snapshot_id().0);
    bytes.extend_from_slice(&to.snapshot_id().0);
    if let Some(root) = root {
        bytes.extend_from_slice(root.to_string().as_bytes());
    }
    bytes.push(0);
    bytes.push(u8::from(detail));
    Hash::of(&bytes)
}

fn json_len<T: Serialize>(v: &T) -> Result<usize, RdyError> {
    serde_json::to_vec(v)
        .map(|b| b.len())
        .map_err(|e| internal(&format!("a diff reply did not serialize: {e}")))
}

/// Removes the body; whether there was one.
fn strip_body(e: &mut DiffEntry) -> bool {
    e.body.take().is_some()
}

/// Removes every field row's values; whether there were any.
fn strip_values(e: &mut DiffEntry) -> bool {
    let mut any = false;
    for f in &mut e.fields {
        any |= f.from.take().is_some();
        any |= f.to.take().is_some();
    }
    any
}

/// How many entries of each kind `entries` holds.
fn counts_of(entries: &[DiffEntry]) -> DiffCounts {
    let mut counts = DiffCounts::default();
    for e in entries {
        match e.change {
            DiffChange::Added => counts.added += 1,
            DiffChange::Removed => counts.removed += 1,
            DiffChange::Updated => counts.updated += 1,
            DiffChange::Moved => counts.moved += 1,
        }
    }
    counts
}

/// The entries of one page and the bytes they take, joined by one comma each.
struct PageBytes {
    entries: Vec<DiffEntry>,
    sizes: Vec<usize>,
    total: usize,
}

impl PageBytes {
    fn new(entries: Vec<DiffEntry>) -> Result<Self, RdyError> {
        let sizes = entries
            .iter()
            .map(json_len)
            .collect::<Result<Vec<_>, _>>()?;
        let commas = entries.len().saturating_sub(1);
        let total = sizes.iter().fold(commas, |sum, n| sum.saturating_add(*n));
        Ok(Self {
            entries,
            sizes,
            total,
        })
    }

    /// Applies `strip` one entry at a time, last entry first, until `fits` holds. Only an entry
    /// that `strip` changed is counted again. Whether the page now fits.
    fn shrink(
        &mut self,
        strip: fn(&mut DiffEntry) -> bool,
        fits: &dyn Fn(usize) -> bool,
    ) -> Result<bool, RdyError> {
        for i in (0..self.entries.len()).rev() {
            if !strip(&mut self.entries[i]) {
                continue;
            }
            let size = json_len(&self.entries[i])?;
            self.total = self
                .total
                .saturating_sub(self.sizes[i].saturating_sub(size));
            self.sizes[i] = size;
            if fits(self.total) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// How many entries from the page's start fit.
    fn longest_run(&self, fits: &dyn Fn(usize) -> bool) -> usize {
        let mut run = 0usize;
        let mut kept = 0;
        for (i, size) in self.sizes.iter().enumerate() {
            let comma = usize::from(i > 0);
            run = run.saturating_add(*size).saturating_add(comma);
            if !fits(run) {
                break;
            }
            kept = i + 1;
        }
        kept
    }
}

/// The page of `reply` that starts at `offset` and fits the budget (the module docs give the rule).
fn cut(
    mut reply: DiffReply,
    offset: usize,
    token: &Hash,
    max_tokens: usize,
    wrapper: usize,
) -> Result<DiffReply, RdyError> {
    let whole_count = reply.entries.len();
    let remaining = reply.entries.split_off(offset);
    reply.entries.clear();

    // `reply` holds no entries here: it is F0
    let f0 = json_len(&reply)?;
    let f1 = json_len(&DiffReply {
        truncated: true,
        omitted: Some(reply.counts.clone()),
        next: Some(cursor::make(token, whole_count)),
        ..reply.clone()
    })?;
    let fits_with = |fixed: usize, entries: usize| {
        wrapper.saturating_add(fixed).saturating_add(entries) / 4 <= max_tokens
    };

    let mut page = PageBytes::new(remaining)?;
    if fits_with(f0, page.total) {
        reply.entries = page.entries;
        return Ok(reply);
    }
    if !fits_with(f1, 0) {
        return Err(RdyError::new(
            E_LIMIT,
            "the token budget cannot hold the reply metadata",
            "increase max_tokens",
        ));
    }
    let fits = |entries: usize| fits_with(f1, entries);
    let mut kept = page.entries.len();
    if !page.shrink(strip_body, &fits)? && !page.shrink(strip_values, &fits)? {
        kept = page.longest_run(&fits);
        if kept == 0 {
            return Err(RdyError::new(
                E_LIMIT,
                "the token budget cannot hold one diff entry",
                "increase max_tokens, or narrow the diff with doc",
            ));
        }
    }
    let after = page.entries.split_off(kept);
    reply.entries = page.entries;
    reply.truncated = true;
    reply.omitted = Some(counts_of(&after));
    if !after.is_empty() {
        reply.next = Some(cursor::make(token, offset + kept));
    }
    Ok(reply)
}
