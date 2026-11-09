//! Signals across projects (repo links, D-Y49PX): the suspects a citing project computes from
//! its foreign copy, with today's rules.
//!
//! - The foreign nodes are the current delivered view of each cited repo, keyed by repo and
//!   block id ([`rdy_graph::foreign::ForeignNodes`]). A repo with a live source reads the newest
//!   version across its live sources ([`crate::ForeignState::current`]). A repo whose every
//!   source is revoked reads the frozen last views of those sources, newest in log order: a
//!   revocation freezes what was delivered, it deletes nothing.
//! - A block's foreign pins never move here: a delivery changes only the nodes, so its suspects
//!   come from the foreign cut alone (D-MEMMQ §6).
//! - A superseded node names a successor only when its repo's view holds it.
//! - A block whose foreign part is empty has no foreign edge and costs one check. A store that
//!   never took a delivery has no node and no work.
use crate::{RdyError, ReadState, Scope};
use rdy_graph::foreign::{ForeignNodes, ForeignSuspect, suspects_of};
use rdy_model::RepoId;
use rdy_store::foreign::HeldVersion;
use std::collections::BTreeMap;

/// The foreign nodes of `st`'s foreign cut.
pub(crate) fn foreign_nodes(st: &ReadState) -> Result<ForeignNodes, RdyError> {
    let foreign = st.foreign()?;
    let mut nodes = ForeignNodes::default();
    if foreign.is_empty() {
        return Ok(nodes);
    }
    for repo in foreign.repos() {
        if let Some(view) = foreign.current(repo) {
            nodes.add_selection(*repo, view.values().map(|h| &h.version));
        }
    }
    // a repo whose every source is revoked keeps its sources' frozen last views
    let mut frozen: BTreeMap<RepoId, BTreeMap<_, &HeldVersion>> = BTreeMap::new();
    for (_, view) in foreign.sources() {
        if !view.revoked || foreign.current(&view.repo).is_some() {
            continue;
        }
        let repo = frozen.entry(view.repo).or_default();
        for (id, held) in &view.members {
            match repo.get(id) {
                Some(have) if have.at >= held.at => {}
                _ => {
                    repo.insert(id, held);
                }
            }
        }
    }
    for (repo, view) in frozen {
        nodes.add_selection(repo, view.values().map(|h| &h.version));
    }
    Ok(nodes)
}

impl ReadState {
    /// The suspects of the blocks of `scope` on cited blocks (repo links, D-Y49PX), in block
    /// then target order: a reference or a suspect-link edge whose foreign pin differs from the
    /// current delivered content (`changed`) or that no delivery selects (`deleted`), and any
    /// edge to a superseded or revoked cited block. A superseded target names its successor when
    /// the cited repo's current view holds it.
    pub fn foreign_suspects(&self, scope: &Scope) -> Result<Vec<ForeignSuspect>, RdyError> {
        let mut out = vec![];
        // no delivery, no citation: a write pins only what a delivery holds
        if self.foreign()?.is_empty() {
            return Ok(out);
        }
        let nodes = foreign_nodes(self)?;
        let m = self.materialized(scope)?;
        for b in m.iter().filter(|b| !b.foreign.is_empty()) {
            out.extend(suspects_of(b, &nodes));
        }
        Ok(out)
    }
}
