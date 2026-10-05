//! Atomic landing (atom1): an optional acceptance change, the proposal's final version, the
//! snapshot, the landing, the proposing view's `Rebase` op and the `canon` and view refs go into
//! the log as one batch, so a kill or a failed write leaves either all of them or none.
use crate::Store;
use crate::error::StoreError;
use crate::kinds;
use crate::state::view_ref;
use crate::store::encode;
use crate::view::{ViewHead, ViewOp, ViewOpKind};
use rdy_model::{AltChoice, Assurance, Change, Hash, LandingRecord, Proposal, Provenance};
use std::collections::HashSet;

/// What to land: the proposal version the approver named, an optional acceptance change to add to
/// it, the approver, and the alternative chosen (if any).
#[derive(Clone, Debug)]
pub struct LandRequest {
    pub proposal: Hash,
    pub accept: Option<Change>,
    pub approver: Provenance,
    pub chosen: Option<AltChoice>,
}

/// What a landing wrote: the landing, the proposal version it landed (the request's when there is
/// no acceptance change), and the acceptance change's id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandOutcome {
    pub landing: Hash,
    pub proposal: Hash,
    pub accept: Option<Hash>,
}

impl Store {
    /// Lands `req.proposal` in one write (atom1). With an acceptance change, the change is
    /// committed to the proposal's view and a final proposal version (the request's changes then
    /// the acceptance) is what lands; `approved_version` stays the version the approver named.
    pub fn land_atomic(&mut self, req: LandRequest) -> Result<LandOutcome, StoreError> {
        // 1. the proposal, its view's head and included set
        let p = self.proposal(&req.proposal)?;
        let head = self
            .view_head(&p.view)?
            .ok_or_else(|| StoreError::UnknownView(p.view.clone()))?;
        let included = self.included(&p.view)?;
        // 2. a change lands once
        for c in &p.changes {
            if let Some(l) = self.landing_of(c)? {
                return Err(StoreError::Decode(format!(
                    "change {c} already landed in {l}"
                )));
            }
        }
        // 3. the acceptance change, its commit op and the final proposal version
        let mut items = vec![];
        let mut final_changes = p.changes.clone();
        let mut accept_change = None;
        let (tip, final_id, aid) = match req.accept {
            Some(mut accept) => {
                let (frames, pos) = self.change_frames(&mut accept, Some(&p.view))?;
                let aid = Hash::of(&frames[pos].1);
                items.extend(frames);
                let by = &accept.provenance;
                let commit = encode(&ViewOp {
                    view: p.view.clone(),
                    prev: Some(head.head),
                    op: ViewOpKind::Commit { change: aid },
                    actor: by.actor.clone(),
                    run: by.run.clone(),
                    at_ms: by.at_ms,
                });
                let tip = Hash::of(&commit);
                items.push((kinds::VIEW_OP, commit));
                final_changes.push(aid);
                let fin = encode(&Proposal {
                    stable: Some(p.stable.unwrap_or(req.proposal)),
                    prev: Some(req.proposal),
                    title: p.title.clone(),
                    view: p.view.clone(),
                    changes: final_changes.clone(),
                    alternative: p.alternative.clone(),
                    provenance: p.provenance.clone(),
                });
                let final_id = Hash::of(&fin);
                items.push((kinds::PROPOSAL, fin));
                accept_change = Some(accept);
                (tip, final_id, Some(aid))
            }
            None => (head.head, req.proposal, None),
        };
        // 4. canon after the landing
        let mut changes = p
            .changes
            .iter()
            .map(|c| self.change(c))
            .collect::<Result<Vec<_>, _>>()?;
        changes.extend(accept_change.iter().cloned());
        let state = self.merge_checked(&self.canon_state()?, &changes)?.0.state;
        let sid = state.id();
        items.push((kinds::SNAPSHOT, encode(&state)));
        // 5. the landing
        let landing = LandingRecord {
            proposal: final_id,
            prev: self.canon_head()?,
            state: sid,
            approved_by: req.approver.clone(),
            changes: final_changes.clone(),
            chosen: req.chosen,
            checks: vec![],
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: Some(req.proposal),
            rebased_from: vec![],
        };
        let lbytes = encode(&landing);
        let lid = Hash::of(&lbytes);
        items.push((kinds::LANDING, lbytes));
        // 6. the view checkpoints: the landed changes drop out, the rest remain
        let landed: HashSet<&Hash> = final_changes.iter().collect();
        let (dropped, remaining): (Vec<Hash>, Vec<Hash>) = included
            .iter()
            .chain(aid.iter())
            .partition(|c| landed.contains(c));
        let rebase = encode(&ViewOp {
            view: p.view.clone(),
            prev: Some(tip),
            op: ViewOpKind::Rebase {
                landing: lid,
                dropped,
                remaining: remaining.clone(),
            },
            actor: req.approver.actor.clone(),
            run: req.approver.run.clone(),
            at_ms: req.approver.at_ms,
        });
        // 7. the view's new head
        let new_head = ViewHead {
            head: Hash::of(&rebase),
            base: Some(lid),
            len: remaining.len() as u64,
            owner: head.owner,
            pinned: head.pinned,
            closed: head.closed,
            last_write_ms: accept_change
                .as_ref()
                .map_or(head.last_write_ms, |a| a.provenance.at_ms),
        };
        items.push((kinds::VIEW_OP, rebase));
        // 8. one write
        let refs = vec![
            ("canon".to_string(), lid.0.to_vec()),
            (view_ref(&p.view), encode(&new_head)),
        ];
        items.extend(self.ref_items_pub(refs));
        self.write(items)?;
        // 9.
        Ok(LandOutcome {
            landing: lid,
            proposal: final_id,
            accept: aid,
        })
    }
}
