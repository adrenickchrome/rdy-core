//! `rdy fsck`: the store's consistency report.

use crate::{Rdy, RdyError};

#[derive(Clone, Debug, serde::Serialize)]
pub struct FsckReport {
    pub frames: usize,
    pub landings: usize,
    pub versions: usize,
    pub views: usize,
    pub landed_changes: usize,
    /// The intents, each with its version chain checked.
    pub intents: usize,
    /// The alternative sets, each with its version chain checked.
    pub alt_sets: usize,
    /// The dismissals, each a `dismiss/` ref naming a review of its own tension.
    pub reviews: usize,
    /// The TRANSITION records, each folded in log order into the open-move tables.
    pub transitions: usize,
    /// The moves a reconcile would log now: not zero when the pack or the rdy build changed
    /// since the last write. Not an error.
    pub unlogged_moves: usize,
}

impl Rdy {
    pub fn fsck(&self) -> Result<FsckReport, RdyError> {
        let frames = self.store.verify()?;
        let landings = self.store.landings()?;
        for (_, rec) in &landings {
            self.store.get::<rdy_model::StateSnapshot>(&rec.state)?;
        }
        let chain: Vec<rdy_model::Hash> = landings.iter().map(|(h, _)| *h).collect();
        let versions = self.check_tags(&chain)?;
        let objects = self.store.check_objects()?;
        let unlogged = crate::transitions::reconcile_moves(&self.read(), None, &[], None)?;
        Ok(FsckReport {
            frames,
            landings: landings.len(),
            versions,
            views: objects.views,
            landed_changes: objects.landed_changes,
            intents: objects.intents,
            alt_sets: objects.alt_sets,
            reviews: objects.reviews,
            transitions: objects.transitions,
            unlogged_moves: crate::transitions::count(&unlogged),
        })
    }
}
