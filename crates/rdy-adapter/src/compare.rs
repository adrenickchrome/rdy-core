//! The compare and the diff of any two scopes a snapshot reads (D-2QX9W Compare), and the compare
//! page with the whole diff's counts (D-V2X3H).
//!
//! Each side is a scope text that [`Snapshot::read`] takes, resolved by the adapter's one scope
//! resolver, so a scope `read` refuses is refused here with the same code, message and fix.
//! `from` is optional in every call: with none, the older side is the empty state, labelled
//! `empty` (D-AJW0S). All answer from the snapshot's captured refs. [`Snapshot::compare`] is
//! `ReadState::compare(to, from)`: `a` labels `to` and `b` labels `from`, and `added` blocks are
//! in `to` only. [`Snapshot::diff`] is `ReadState::diff(from, to)`. [`Snapshot::diff_reply`] is
//! the body of `rdy_diff` for the two sides, without `reminder`: every page carries the whole
//! diff's `counts`, a cut page `truncated` and `omitted`, and `next`, which is passed back as
//! `page`. A cursor is a token of the two states and an offset, so it needs no session; a cursor
//! of another request is `E_PAGE_STALE`. A holder's pushed view at an op reads its own state, so
//! a pair with such a side compares the two sides' states, with each side's label and merge
//! conflicts as the engine gives them for that side alone. No call writes.

use crate::reads::Resolved;
use crate::store::Snapshot;
use rdy_engine::diff::diff_states;
use rdy_engine::diff_request::{DIFF_DEFAULT_TOKENS, DiffSide};
use rdy_engine::tool_reads::check_tokens;
use rdy_engine::view_query::compare_states;
use rdy_engine::{E_INTERNAL, Materialized, RdyError, ReadState};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

pub use rdy_engine::diff::DiffCounts;
pub use rdy_engine::{Compare, CompareEntry, Diff, DiffChange, DiffEntry};

/// The label of the empty side in a compare (D-AJW0S).
const EMPTY: &str = "empty";

/// What [`Snapshot::diff_reply`] takes: the parameters of `rdy_diff`, with `to` always named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffQuery {
    /// The older scope; None is the empty state.
    pub from: Option<String>,
    /// The newer scope. Both take the scope texts of [`Snapshot::read`].
    pub to: String,
    /// Limits both sides to one doc or folder: an id, a key or a path.
    pub doc: Option<String>,
    /// Adds field values and the word pieces of each changed body.
    pub detail: bool,
    /// The budget of the page, at most 100000 (default 4000).
    pub max_tokens: Option<usize>,
    /// `next` of the previous page, with the same query.
    pub page: Option<String>,
}

impl Resolved {
    /// The state the scope reads: the view's own, else the snapshot's.
    fn state<'a>(&'a self, snapshot: &'a ReadState) -> &'a ReadState {
        self.view.as_deref().unwrap_or(snapshot)
    }

    /// The side as the engine's compare gives it alone: its label and its merge conflicts.
    fn alone(&self, snapshot: &ReadState) -> Result<Compare, RdyError> {
        self.state(snapshot).compare(&self.scope, &self.scope)
    }
}

impl Snapshot {
    /// Both sides resolved, with the captured epoch checked before (D-WDKWQ). No `from` is the
    /// empty state.
    fn sides(
        &self,
        from: Option<&str>,
        to: &str,
    ) -> Result<(Option<Resolved>, Resolved), RdyError> {
        self.read_state().ensure_current_epoch()?;
        let from = from.map(|f| self.read_scope(Some(f))).transpose()?;
        Ok((from, self.read_scope(Some(to))?))
    }

    /// What differs from scope `from` (None: the empty state) to scope `to`, block by block
    /// (`rdy_view` op `compare`).
    pub fn compare(&self, from: Option<&str>, to: &str) -> Result<Compare, RdyError> {
        let (from, to) = self.sides(from, to)?;
        let state = self.read_state();
        let out = match from {
            Some(from) if from.view.is_none() && to.view.is_none() => {
                state.compare(&to.scope, &from.scope)?
            }
            from => {
                let older = match &from {
                    Some(f) => f.state(state).materialized(&f.scope)?,
                    None => Arc::new(Materialized::default()),
                };
                let mut out = compare_states(&*to.state(state).materialized(&to.scope)?, &older);
                let a = to.alone(state)?;
                let (b, b_conflicted) = match &from {
                    Some(f) => {
                        let b = f.alone(state)?;
                        (b.a, b.conflicted)
                    }
                    None => (EMPTY.to_string(), vec![]),
                };
                // each block once, `to`'s row first, sorted by id, as the engine's compare keeps them
                let mut conflicted: BTreeMap<String, CompareEntry> = BTreeMap::new();
                for e in a.conflicted.into_iter().chain(b_conflicted) {
                    conflicted.entry(e.id.clone()).or_insert(e);
                }
                out.a = a.a;
                out.b = b;
                out.conflicted = conflicted.into_values().collect();
                out
            }
        };
        state.ensure_current_epoch()?;
        Ok(out)
    }

    /// The block diff from scope `from` (None: the empty state) to scope `to` (the diff
    /// `rdy_diff` pages).
    pub fn diff(&self, from: Option<&str>, to: &str) -> Result<Diff, RdyError> {
        let (from, to) = self.sides(from, to)?;
        let state = self.read_state();
        let out = match from {
            Some(from) if from.view.is_none() && to.view.is_none() => {
                state.diff(&from.scope, &to.scope)?
            }
            from => {
                let older = match &from {
                    Some(f) => f.state(state).materialized(&f.scope)?,
                    None => Arc::new(Materialized::default()),
                };
                diff_states(&older, &*to.state(state).materialized(&to.scope)?)
            }
        };
        state.ensure_current_epoch()?;
        Ok(out)
    }

    /// The page of the diff that `query` asks for: the body `rdy_diff` returns for the same
    /// sides, without `reminder` (D-V2X3H). Cursor refusals name `diff_reply`.
    pub fn diff_reply(&self, query: &DiffQuery) -> Result<Value, RdyError> {
        check_tokens(query.max_tokens)?;
        let (from, to) = self.sides(query.from.as_deref(), &query.to)?;
        let state = self.read_state();
        let side = |text: &str, r: Resolved| r.state(state).diff_side_of(text, r.scope.clone());
        let from = match (query.from.as_deref(), from) {
            (Some(text), Some(r)) => side(text, r)?,
            _ => DiffSide::Empty,
        };
        let to = side(&query.to, to)?;
        let reply = state.diff_page_of(
            &from,
            &to,
            query.doc.as_deref(),
            query.detail,
            query.page.as_deref(),
            query.max_tokens.unwrap_or(DIFF_DEFAULT_TOKENS),
            "diff_reply",
        )?;
        state.ensure_current_epoch()?;
        serde_json::to_value(reply).map_err(|e| {
            RdyError::new(
                E_INTERNAL,
                format!("a diff reply did not serialize: {e}"),
                "report this as a bug in rdy",
            )
        })
    }
}
