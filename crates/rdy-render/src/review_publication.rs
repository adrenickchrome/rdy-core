//! A store-less publication with review marks (D-AJ7T2): the whole static site of one
//! publication profile in one state, with the marks of a review page against a second state.
//! Both states arrive with their sources; no store, scope, view or proposal is read. The pages
//! carry no review name: the hub writes its notes in each page's notice slot, and core writes
//! nothing there.
use crate::profile_live::{OldSide, review_history};
use crate::site::Site;
use crate::{
    Budget, Mode, NotFoundOut, PublicationArtifact, Slots, artifact, build_site_from, publication,
};
use rdy_engine::publication::{
    IMPLICIT_ORG, PublicationSource, check_stable_selector, resolve_source,
};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::{Materialized, RdyError};
use rdy_model::BlockId;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Builds the publication of `selector` (`implicit:org` or a full profile id) in the state
/// `to_m`, read through `to`, in memory, with the marks of a review page against the state
/// `from_m`, read through `from` (D-AJ7T2). Each block that differs carries the mark a review
/// page gives it: New, Edited, or a removed line where the old text stood, and the pages offer
/// Show changes and Show diff. A pair with no change gives exactly [`crate::build_publication`]'s
/// site of `to_m` with no chosen tag. The profile is the same id in both states: a state that
/// holds no such block is the empty side. The site names no version (`chosen_label(None, ..)`);
/// `redacted` is the `to` state's removed-content mark. Every HTML page holds one empty notice
/// slot, at the offset its manifest entry records. The build keeps [`crate::build_publication`]'s
/// limits, gate and manifest recipe; the old side is never gated and no byte of its figures is
/// read.
pub fn build_review_publication(
    to: &dyn PublicationSource,
    to_m: &Materialized,
    from: &dyn PublicationSource,
    from_m: &Materialized,
    redacted: bool,
    selector: &str,
    inputs: &RenderInputs,
) -> Result<PublicationArtifact, RdyError> {
    crate::probe::hit();
    check_stable_selector(selector)?;
    publication::check_inputs(to, inputs)?;
    publication::check_inputs(from, inputs)?;
    let resolved = resolve_source(to, to_m, selector)?;
    // the same profile in the old state; a state without the profile block is the empty side
    let held = selector == IMPLICIT_ORG
        || BlockId::parse(selector).is_ok_and(|id| from_m.get(&id).is_some());
    let old = if held {
        Some(resolve_source(from, from_m, selector)?)
    } else {
        None
    };
    let mut prepared = publication::prepare_resolved(to, resolved, "", redacted, inputs)?;
    let old = OldSide::of(from, old)?;
    // the decisions whose references either side drops (D-7M0YF)
    let mut dropped: BTreeSet<BlockId> = prepared
        .profile
        .as_ref()
        .map(|p| p.dropped_decisions.clone())
        .unwrap_or_default();
    dropped.extend(old.dropped.iter().cloned());
    let m = Arc::clone(&prepared.m);
    let site = Site::for_profile(&m, &prepared.pack, &prepared.site_name, &dropped);
    let mut hist = review_history(
        &site,
        &old,
        (&m, &prepared.status),
        (&prepared.pack, &prepared.site_name, &dropped),
        true,
        |_| Ok(None),
    )?;
    drop(site);
    // a pair with no change is the plain publication
    if hist.listed.as_ref().is_some_and(|r| r.changed_blocks() > 0) {
        // an old side's removed figure reads as the notice on its removed line
        for (blob, request) in &old.removed {
            prepared.blobs.removed.entry(*blob).or_insert(*request);
        }
        if let Some(p) = prepared.profile.as_mut() {
            p.dropped_decisions = dropped;
        }
        hist.warnings = std::mem::take(&mut prepared.history.warnings);
        prepared.history = hist;
    }
    let budget = Budget::publication();
    // a publication's links stay relative: it has no generated not-found page
    artifact(build_site_from(
        prepared,
        Mode::Static,
        Slots::Notices,
        Some(&budget),
        NotFoundOut::None,
    )?)
}
