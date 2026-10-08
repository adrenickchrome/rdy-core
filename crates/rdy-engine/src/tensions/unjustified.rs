//! Unjustified change: a change edits stable normative content without a justification.
//! The detector reports nothing yet.
use super::{DetectCx, Detector, Found, TensionKind};
use crate::RdyError;
use rdy_model::BlockId;
use std::collections::BTreeSet;

/// This kind's detector.
pub struct UnjustifiedChange;

impl Detector for UnjustifiedChange {
    fn kind(&self) -> TensionKind {
        TensionKind::UnjustifiedChange
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
