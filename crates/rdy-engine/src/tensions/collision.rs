//! Edit collision: two in-flight changes edit one block from one base, or an in-flight edit
//! against a canon version it did not build on.
//! The detector reports nothing yet.
use super::{DetectCx, Detector, Found, TensionKind};
use crate::RdyError;
use rdy_model::BlockId;
use std::collections::BTreeSet;

/// This kind's detector.
pub struct EditCollision;

impl Detector for EditCollision {
    fn kind(&self) -> TensionKind {
        TensionKind::EditCollision
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
