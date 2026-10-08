//! Deprecated reference: a write adds a reference or a link to deprecated content.
//! The detector reports nothing yet.
use super::{DetectCx, Detector, Found, TensionKind};
use crate::RdyError;
use rdy_model::BlockId;
use std::collections::BTreeSet;

/// This kind's detector.
pub struct DeprecatedReference;

impl Detector for DeprecatedReference {
    fn kind(&self) -> TensionKind {
        TensionKind::DeprecatedReference
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
