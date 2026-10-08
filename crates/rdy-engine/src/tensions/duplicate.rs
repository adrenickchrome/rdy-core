//! Duplicate definition: two blocks define the same name, or have the same normative text.
//! The detector reports nothing yet.
use super::{DetectCx, Detector, Found, TensionKind};
use crate::RdyError;
use rdy_model::BlockId;
use std::collections::BTreeSet;

/// This kind's detector.
pub struct DuplicateDefinition;

impl Detector for DuplicateDefinition {
    fn kind(&self) -> TensionKind {
        TensionKind::DuplicateDefinition
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
