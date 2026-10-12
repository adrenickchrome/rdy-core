//! A run's in-flight changes and what they touch (plan M2e2, D-TPZ0W).
use crate::{RdyError, ReadState, Scope};
use rdy_model::{BlockId, Hash};
use rdy_store::view::Owner;
use std::collections::BTreeSet;

/// One run's in-flight changes, the blocks they write and cite, the views the run owns (open or closed) and the areas of its blocks (plan M2e2, D-TPZ0W).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunInFlight {
    pub changes: BTreeSet<Hash>,
    pub blocks: BTreeSet<BlockId>,
    pub cited: BTreeSet<BlockId>,
    pub views: BTreeSet<String>,
    pub areas: Vec<String>,
}

impl ReadState {
    /// The in-flight changes of run `run` of `actor`, with the blocks they write (live writes
    /// only), the blocks those writes cite (not the run's own), the views the run owns and the
    /// areas of the written blocks.
    pub fn run_in_flight(&self, actor: &str, run: Option<&str>) -> Result<RunInFlight, RdyError> {
        let mut out = RunInFlight::default();
        let tensions = self.tensions()?;
        let writes = tensions.writes();
        for facts in writes.changes() {
            if facts.actor != actor || facts.run.as_deref() != run {
                continue;
            }
            out.changes.insert(facts.id);
            for w in writes.of_change(&facts.id) {
                let (true, Some(new)) = (w.live, w.new.as_ref()) else {
                    continue;
                };
                out.blocks.insert(w.block.clone());
                out.cited
                    .extend(rdy_graph::edges_of(new).into_iter().map(|e| e.to));
            }
        }
        out.cited.retain(|b| !out.blocks.contains(b));
        let me = Owner {
            actor: actor.to_string(),
            run: run.map(str::to_string),
        };
        // the views map by reference: no name is cloned but the run's own
        for (name, entry) in &self.views {
            if entry.head.as_ref().and_then(|h| h.owner.as_ref()) == Some(&me) {
                out.views.insert(name.clone());
            }
        }
        let mut areas = BTreeSet::new();
        for b in &out.blocks {
            match self.areas_of(&Scope::Inflight, b) {
                Ok(a) => areas.extend(a),
                Err(e) if e.code == "E_UNKNOWN_BLOCK" => {}
                Err(e) => return Err(e),
            }
        }
        out.areas = areas.into_iter().collect();
        Ok(out)
    }

    /// The events of every committed batch in the log from `from` to this state's `seq`, in log
    /// order. An older state never sees a later batch.
    pub fn replay(
        &self,
        from: rdy_store::segment::Location,
    ) -> Result<Vec<crate::Event>, RdyError> {
        crate::events::replay(self.reader(), from, self.seq())
    }
}
