//! The render inputs a publication records (D-KDFZ3, D-QWSTV): [`capture_inputs`] takes the
//! source's settings (`rdy_engine::render_inputs::capture`: this rdy's version, the effective
//! pack, the theme, the title and the description) and the reader projection of the scope's
//! disagreements. A publication replays them: it never reads the source's current canon, its
//! tensions or its dismissals again, so a canon move or a dismissal after the capture changes no
//! byte of the artifact.
//!
//! The projection holds each disagreement's fixed sentence, its blocks and its lead block alone:
//! never a tension's summary, advice, dismissal reason, status, actor or time.
use crate::disagree::{self, Disagreement};
use rdy_engine::render_inputs::{self, DISAGREEMENT_SENTENCES, DisagreementInput, RenderInputs};
use rdy_engine::{E_BAD_INPUT, RdyError, ReadState, Scope};

/// The render inputs of `scope` in `read`: the source's settings, then the disagreements a page
/// of the scope lists ([`disagree::list`]: canon's open ones for canon and a tag at canon's head,
/// none for any other scope), in their order. The value is validated before it is returned.
pub fn capture_inputs(read: &ReadState, scope: &Scope) -> Result<RenderInputs, RdyError> {
    let mut inputs = render_inputs::capture(read)?;
    inputs.disagreements = disagree::list(read, scope)?
        .into_iter()
        .map(|d| DisagreementInput {
            sentence: d.sentence.to_string(),
            blocks: d.blocks,
            lead: d.lead,
        })
        .collect();
    render_inputs::validate_inputs(&inputs)?;
    Ok(inputs)
}

/// The disagreements of `inputs` as a page lists them, in their order: each row's sentence is the
/// closed table's own text. A row with a sentence the table lacks is refused (validated inputs
/// have none).
pub(crate) fn disagreements(inputs: &RenderInputs) -> Result<Vec<Disagreement>, RdyError> {
    inputs
        .disagreements
        .iter()
        .map(|row| {
            let sentence = DISAGREEMENT_SENTENCES
                .iter()
                .find(|(_, s)| *s == row.sentence)
                .map(|(_, s)| *s)
                .ok_or_else(|| {
                    RdyError::new(
                        E_BAD_INPUT,
                        "render disagreement inputs are invalid",
                        "capture the render inputs from the source again",
                    )
                })?;
            Ok(Disagreement {
                sentence,
                blocks: row.blocks.clone(),
                lead: row.lead.clone(),
            })
        })
        .collect()
}
