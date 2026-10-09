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
        Ok(FsckReport {
            frames,
            landings: landings.len(),
            versions,
            views: objects.views,
            landed_changes: objects.landed_changes,
            intents: objects.intents,
            alt_sets: objects.alt_sets,
            reviews: objects.reviews,
        })
    }
}
