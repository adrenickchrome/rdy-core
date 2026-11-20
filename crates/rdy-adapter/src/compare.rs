//! The compare and the diff of any two scopes a snapshot reads (D-2QX9W Compare).
//!
//! Each side is a scope text that [`Snapshot::read`] takes, resolved by the adapter's one scope
//! resolver, so a scope `read` refuses is refused here with the same code, message and fix.
//! Both answer from the snapshot's captured refs. [`Snapshot::compare`] is
//! `ReadState::compare(to, from)`: `a` labels `to` and `b` labels `from`, and `added` blocks are
//! in `to` only. [`Snapshot::diff`] is `ReadState::diff(from, to)`. A holder's pushed view at an
//! op reads its own state, so a pair with such a side compares the two sides' states, with each
//! side's label and merge conflicts as the engine gives them for that side alone. Neither call
//! writes.

use crate::reads::Resolved;
use crate::store::Snapshot;
use rdy_engine::diff::diff_states;
use rdy_engine::view_query::compare_states;
use rdy_engine::{RdyError, ReadState};
use std::collections::BTreeMap;

pub use rdy_engine::diff::DiffCounts;
pub use rdy_engine::{Compare, CompareEntry, Diff, DiffChange, DiffEntry};

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
    /// Both sides resolved, with the captured epoch checked before (D-WDKWQ).
    fn sides(&self, from: &str, to: &str) -> Result<(Resolved, Resolved), RdyError> {
        self.read_state().ensure_current_epoch()?;
        Ok((self.read_scope(Some(from))?, self.read_scope(Some(to))?))
    }

    /// What differs from scope `from` to scope `to`, block by block (`rdy_view` op `compare`).
    pub fn compare(&self, from: &str, to: &str) -> Result<Compare, RdyError> {
        let (from, to) = self.sides(from, to)?;
        let state = self.read_state();
        let out = if from.view.is_none() && to.view.is_none() {
            state.compare(&to.scope, &from.scope)?
        } else {
            let mut out = compare_states(
                &*to.state(state).materialized(&to.scope)?,
                &*from.state(state).materialized(&from.scope)?,
            );
            let (a, b) = (to.alone(state)?, from.alone(state)?);
            // each block once, `to`'s row first, sorted by id, as the engine's compare keeps them
            let mut conflicted: BTreeMap<String, CompareEntry> = BTreeMap::new();
            for e in a.conflicted.into_iter().chain(b.conflicted) {
                conflicted.entry(e.id.clone()).or_insert(e);
            }
            out.a = a.a;
            out.b = b.a;
            out.conflicted = conflicted.into_values().collect();
            out
        };
        state.ensure_current_epoch()?;
        Ok(out)
    }

    /// The block diff from scope `from` to scope `to` (the diff `rdy_diff` pages).
    pub fn diff(&self, from: &str, to: &str) -> Result<Diff, RdyError> {
        let (from, to) = self.sides(from, to)?;
        let state = self.read_state();
        let out = if from.view.is_none() && to.view.is_none() {
            state.diff(&from.scope, &to.scope)?
        } else {
            diff_states(
                &*from.state(state).materialized(&from.scope)?,
                &*to.state(state).materialized(&to.scope)?,
            )
        };
        state.ensure_current_epoch()?;
        Ok(out)
    }
}
