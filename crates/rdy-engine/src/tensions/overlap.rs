//! Area overlap: two in-flight decisions affect one area and touch the same blocks; at read
//! time, an open intent covers a block another view's change edits.
//! The detector reports nothing yet.
use super::{DetectCx, Detector, Found, Tension, TensionKind};
use crate::{RdyError, ReadState};
use rdy_model::BlockId;
use std::collections::BTreeSet;

/// This kind's detector.
pub struct AreaOverlap;

impl Detector for AreaOverlap {
    fn kind(&self) -> TensionKind {
        TensionKind::AreaOverlap
    }

    fn detect(
        &self,
        _cx: &DetectCx<'_>,
        _seeds: Option<&BTreeSet<BlockId>>,
        _out: &mut Vec<Found>,
    ) -> Result<(), RdyError> {
        Ok(())
    }
}

impl ReadState {
    /// The read-time area overlaps at `now_ms`: each open, unexpired intent against the live
    /// writes of other views' changes on the blocks it covers. Never in the event-derived set.
    pub fn intent_tensions(&self, _now_ms: i64) -> Result<Vec<Tension>, RdyError> {
        Ok(vec![])
    }
}
