//! Premise shift: an in-flight change alters or retires a block that another change's content
//! cites with a pin.
//! The detector reports nothing yet.
use super::{DetectCx, Detector, Found, TensionKind};
use crate::RdyError;
use rdy_model::BlockId;
use std::collections::BTreeSet;

/// This kind's detector.
pub struct PremiseShift;

impl Detector for PremiseShift {
    fn kind(&self) -> TensionKind {
        TensionKind::PremiseShift
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
