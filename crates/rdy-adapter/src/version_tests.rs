//! The rebase test of a chain of landings onto one landing's state (D-2QX9W, D-N0NVF).
//!
//! The test is core's (`rdy_engine::version_tests`). This module checks the inputs, resolves
//! `onto` with the adapter's one scope resolver, pins the answer to the snapshot's captured refs
//! and returns core's result. It writes nothing.
//!
//! The inputs (D-N0NVF), checked in this order: at most 100,000 hashes (`E_LIMIT`, before any
//! read); each hash once (`E_BAD_INPUT`); `onto` is `canon`, `tag:<version>` or `landing:<id>`,
//! the state of one landing (`E_BAD_INPUT`); each hash names a landing that D-Y32MN's reach finds
//! from the captured refs (`E_NOT_FOUND`, with that reach's text). `onto` then resolves as a read
//! resolves it. The chain needs no contiguity, and an empty chain is clean.

use crate::reads::unreached;
use crate::store::Snapshot;
use rdy_engine::history_facts::{self, MAX_LANDINGS};
use rdy_engine::{E_BAD_INPUT, E_LIMIT, RdyError, Scope};
use rdy_model::Hash;
use std::collections::HashSet;

pub use rdy_engine::version_tests::RebaseTest;
pub use rdy_store::{ConflictKind, MergeConflict};

/// Whether `onto` names the state of one landing: `canon`, `tag:<version>` or `landing:<id>`.
fn names_a_landing(onto: &str) -> bool {
    onto == "canon" || onto.starts_with("tag:") || onto.starts_with("landing:")
}

/// `E_BAD_INPUT`: an `onto` whose state is not one landing's.
fn not_a_landing(onto: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("onto {onto:?} is not a landing's state"),
        "use canon, tag:<version> or landing:<id>",
    )
}

impl Snapshot {
    /// The checked inputs of a version test (see the module docs): the landing whose state is
    /// `onto` (None for an empty canon). The rebase test and the replay test both take their
    /// inputs through it.
    pub(crate) fn version_test_inputs(
        &self,
        chain: &[Hash],
        onto: &str,
    ) -> Result<Option<Hash>, RdyError> {
        if chain.len() > MAX_LANDINGS {
            return Err(RdyError::new(
                E_LIMIT,
                format!("version test chain exceeds {MAX_LANDINGS} landings"),
                format!("send at most {MAX_LANDINGS} landings"),
            ));
        }
        let mut seen: HashSet<Hash> = HashSet::with_capacity(chain.len());
        for h in chain {
            if !seen.insert(*h) {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("landing {h} appears twice in the chain"),
                    "name each landing of the chain once",
                ));
            }
        }
        if !names_a_landing(onto) {
            return Err(not_a_landing(onto));
        }
        let read = self.read_state();
        read.ensure_current_epoch()?;
        let found = history_facts::reached(read, &self.reach_heads(), chain)?;
        if let Some((h, _)) = chain.iter().zip(&found).find(|(_, f)| !**f) {
            return Err(unreached(*h));
        }
        let resolved = self.read_scope(Some(onto))?;
        match resolved.scope {
            Scope::Canon if resolved.view.is_none() => Ok(self.canon()),
            Scope::At(h) if resolved.view.is_none() => Ok(Some(h)),
            _ => Err(not_a_landing(onto)),
        }
    }

    /// Whether `chain`, oldest first, merges onto the state at `onto` with the merge rules of a
    /// landing: `Clean`, or the first landing that does not merge, with every conflicting block
    /// and its kind (D-2QX9W). The inputs are checked first (see the module docs). A removal
    /// since the snapshot is `E_REDACTED_STATE`.
    pub fn rebase_test(&self, chain: &[Hash], onto: &str) -> Result<RebaseTest, RdyError> {
        let onto = self.version_test_inputs(chain, onto)?;
        let read = self.read_state();
        let out = rdy_engine::version_tests::rebase_test(read, chain, onto)?;
        read.ensure_current_epoch()?;
        Ok(out)
    }
}
