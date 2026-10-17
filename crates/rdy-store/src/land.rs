//! Atomic landing (atom1): an optional acceptance change, the proposal's final version, the
//! snapshot, the landing, a `Rebase` op for every view the landing rebases (reb1) and the refs go
//! into the log as one batch, so a kill or a failed write leaves either all of them or none. A
//! landing that chooses an alternative (altw1) adds the set's chosen version and a `Close` op for
//! each other open member view to the same batch. A landing that lands redaction carriers ends
//! the same batch with one REDACTION_APPLY per request (D-CC90E: "It lands only with a human
//! approval"; D-600CW: the removal record stays in the log).
use crate::Store;
use crate::coord::{ALT_REF, chosen_reason};
use crate::error::StoreError;
use crate::kinds;
use crate::merge::{Conflict, MergeChange, merge_on_blocks};
use crate::state::{open_ref, proposal_ref, view_ref};
use crate::store::encode;
use crate::view::{ViewHead, ViewOp, ViewOpKind, decode_head};
use rdy_model::{
    ActorRun, AltChoice, AltSet, AltStatus, ApprovalEvidence, Assurance, BlockId, Change,
    EvidenceKind, Hash, LandingRecord, Op, Proposal, Provenance, RelayEvidence,
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Who relayed a human's approval, and what the human said (FW-B9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relay {
    pub by: ActorRun,
    pub evidence: RelayEvidence,
}

/// The alternative a landing chooses (altw1): the set's stable id, the set version the approver
/// saw (still its latest and open, or the landing is refused), the chosen member view, and the
/// other member views to close instead of rebasing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltLanding {
    pub set: Hash,
    pub prev: Hash,
    pub member: String,
    pub close: Vec<String>,
}

/// What to land: the proposal version the approver named, an optional acceptance change to add to
/// it, the approver, the alternative chosen (if any), the relay that carried the approval (if
/// any) and how the approval was obtained (if recorded).
#[derive(Clone, Debug)]
pub struct LandRequest {
    pub proposal: Hash,
    pub accept: Option<Change>,
    pub approver: Provenance,
    pub alt: Option<AltLanding>,
    pub relay: Option<Relay>,
    pub evidence: Option<ApprovalEvidence>,
}

/// One view a landing rebased: the changes that dropped out of it (they landed) and the conflicts
/// its remaining changes have with the new canon on the blocks the landing touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebasedView {
    pub view: String,
    pub dropped: Vec<Hash>,
    pub conflicts: Vec<Conflict>,
}

/// What a landing wrote: the landing, the proposal version it landed (the request's when there is
/// no acceptance change), the acceptance change's id, every view it rebased, in `view/` ref
/// order (the proposing view included), the alternative set's chosen version (with `alt`), and
/// the member views it closed, in `view/` ref order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandOutcome {
    pub landing: Hash,
    pub proposal: Hash,
    pub accept: Option<Hash>,
    pub rebased: Vec<RebasedView>,
    pub alt_version: Option<Hash>,
    pub closed: Vec<String>,
}

impl Store {
    /// Lands `req.proposal` in one write (atom1). The version must be its proposal's latest
    /// (`proposal/<stable>`), or it is `ProposalMoved`. With an acceptance change, the change is
    /// committed to the proposal's view and a final proposal version (the request's changes then
    /// the acceptance) is what lands; `approved_version` stays the version the approver named.
    /// The same batch rebases the proposing view (always, pinned or closed or not) and every other
    /// view that is open, unpinned and non-empty onto the landing (reb1). With `req.alt`, the set
    /// version named must still be the set's latest and open; the landing records the choice, the
    /// set's next version is `Chosen` by it, and each member view in `alt.close` other than the
    /// proposing one is closed (when open, pinned or empty too) instead of rebased (altw1).
    ///
    /// When the landing lands redaction carriers (changes the carrier-to-request map names), the
    /// store checks them before anything is written (`prepare_redaction_apply`): a person's
    /// approval (`RedactionHuman` otherwise), each carrier in the proposing view's history
    /// (`Refused`), each request's targets against its frozen manifest (`Refused`), each span and
    /// residue (`RedactionCorrupt`), and every physical copy of the selected content covered: an
    /// unlisted copy before the request is `RedactionCorrupt`, one written after it
    /// `RedactionStale`. The batch then ends with one REDACTION_APPLY per request, in
    /// request-hash order, after the landing and the refs. Unheld, the checked apply writer seals
    /// it and finishes the removal before this returns; a failure after its sync never truncates
    /// the batch and leaves the handle pending. Held, the frames join the open batch, which
    /// takes no more writes; no mask is active and nothing is erased until its seal, which may
    /// append the TRANSITION last. A pre-seal preview reads through
    /// `Store::redaction_preview_reader`.
    pub fn land_atomic(&mut self, req: LandRequest) -> Result<LandOutcome, StoreError> {
        // 1. the proposal, and the version check: the request names its stable's latest version
        let p = self.proposal(&req.proposal)?;
        let stable = p.stable.unwrap_or(req.proposal);
        let latest = match self.get_ref(&proposal_ref(&stable))? {
            Some(b) => crate::index::hash_from(&b, "proposal ref")?,
            None => {
                return Err(StoreError::Decode(format!(
                    "proposal {stable} has no ref; run rdy reindex"
                )));
            }
        };
        if latest != req.proposal {
            return Err(StoreError::ProposalMoved { stable, latest });
        }
        // 1b. the relay and the evidence kind agree (FW-B9)
        check_evidence(req.relay.as_ref(), req.evidence.as_ref())?;
        // 1c. the alternative set: the version the approver saw is still its latest, and open
        let alt_prev = match &req.alt {
            Some(alt) => Some(self.alt_landing_check(alt)?),
            None => None,
        };
        // 2. the proposing view's head and included set
        let head = self
            .view_head(&p.view)?
            .ok_or_else(|| StoreError::UnknownView(p.view.clone()))?;
        let included = self.included(&p.view)?;
        // 3. a change lands once
        for c in &p.changes {
            if let Some(l) = self.landing_of(c)? {
                return Err(StoreError::Decode(format!(
                    "change {c} already landed in {l}"
                )));
            }
        }
        // 4. the acceptance change, its commit op and the final proposal version
        let mut items = vec![];
        let mut latest_ref = None;
        let mut final_changes = p.changes.clone();
        let mut accept_change = None;
        let (tip, final_id, aid) = match req.accept {
            Some(mut accept) => {
                let (frames, pos) = self.change_frames(&mut accept, Some(&p.view), &[])?;
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
                    stable: Some(stable),
                    prev: Some(req.proposal),
                    title: p.title.clone(),
                    view: p.view.clone(),
                    changes: final_changes.clone(),
                    alternative: p.alternative.clone(),
                    provenance: p.provenance.clone(),
                    canon_at: p.canon_at,
                    summary: p.summary.clone(),
                });
                let final_id = Hash::of(&fin);
                items.push((kinds::PROPOSAL, fin));
                latest_ref = Some((proposal_ref(&stable), final_id.0.to_vec()));
                accept_change = Some(accept);
                (tip, final_id, Some(aid))
            }
            None => (head.head, req.proposal, None),
        };
        // 5. canon after the landing
        let mut changes = p
            .changes
            .iter()
            .map(|c| self.change(c))
            .collect::<Result<Vec<_>, _>>()?;
        changes.extend(accept_change.iter().cloned());
        let (prev_head, prev_state) = (self.canon_head()?, self.canon_state()?);
        let state = self.merge_checked(&prev_state, &changes)?.0.state;
        let sid = state.id();
        items.push((kinds::SNAPSHOT, encode(&state)));
        // 6. the landing
        let landing = LandingRecord {
            proposal: final_id,
            prev: self.canon_head()?,
            state: sid,
            approved_by: req.approver.clone(),
            changes: final_changes.clone(),
            chosen: req.alt.as_ref().map(|a| AltChoice {
                set: a.set,
                member: a.member.clone(),
            }),
            checks: vec![],
            assurance: if req.relay.is_some() {
                Assurance::Relayed
            } else {
                Assurance::Local
            },
            relayed_by: req.relay.as_ref().map(|r| r.by.clone()),
            relay_evidence: req.relay.map(|r| r.evidence),
            approved_version: Some(req.proposal),
            rebased_from: vec![],
            evidence: req.evidence,
        };
        // 6a. the redaction applies of the carriers it lands, checked before anything is written
        let applies =
            self.prepare_redaction_apply(&p.view, &included, &landing, accept_change.as_ref())?;
        let lbytes = encode(&landing);
        let lid = Hash::of(&lbytes);
        items.push((kinds::LANDING, lbytes));
        // 6b. the set's next version, chosen by this landing
        let mut alt_ref = None;
        let alt_version = match (&req.alt, alt_prev) {
            (Some(alt), Some(prev_version)) => {
                let next = encode(&AltSet {
                    stable: Some(alt.set),
                    prev: Some(alt.prev),
                    status: AltStatus::Chosen {
                        member: alt.member.clone(),
                        landing: lid,
                    },
                    provenance: req.approver.clone(),
                    ..prev_version
                });
                let id = Hash::of(&next);
                items.push((kinds::ALT_SET, next));
                alt_ref = Some((format!("{ALT_REF}{}", alt.set), id.0.to_vec()));
                Some(id)
            }
            _ => None,
        };
        // 7. the scope: the proposing view, and every other open, unpinned, non-empty view. Each
        // drops the landed changes and records its conflicts with the new canon on the blocks
        // both it and the landing touch.
        let landed: HashSet<&Hash> = final_changes.iter().collect();
        let touched_l: BTreeSet<&BlockId> = changes
            .iter()
            .flat_map(|c| c.ops.iter().map(|op| op.block_id()))
            .collect();
        // the versions the acceptance change writes have no producer in the index until the write
        let accepted: HashMap<Hash, Hash> = match (&accept_change, aid) {
            (Some(a), Some(aid)) => a
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::Create { block } | Op::Update { block, .. } => {
                        Some((block.version_hash(), aid))
                    }
                    Op::Delete { .. } => None,
                })
                .collect(),
            _ => HashMap::new(),
        };
        // A version's producer as the index will answer after this write: when none of the
        // version's producers has landed yet, the ones this landing lands (the acceptance change
        // included) win, smallest hash first, as `Store::producer` breaks ties within a landing.
        // Otherwise an earlier landing's producer already wins, before the write and after it.
        // A read error fails the landing: read as "no producer", it would settle canon's side.
        // `rebase_conflicts` asks this only for the versions its merge needs, and keeps each
        // answer in `producers` for the next view.
        let producer = |v: &Hash| -> Result<Option<Hash>, StoreError> {
            let all = self.index().producers(v)?;
            let mut landed_before = false;
            for c in &all {
                if self.landing_of(c)?.is_some() {
                    landed_before = true;
                    break;
                }
            }
            if !landed_before {
                let mine = all
                    .iter()
                    .filter(|c| landed.contains(c))
                    .chain(accepted.get(v))
                    .min()
                    .copied();
                if mine.is_some() {
                    return Ok(mine);
                }
            }
            Ok(self.producer(v)?.or_else(|| accepted.get(v).copied()))
        };
        let mut producers: HashMap<Hash, Option<Hash>> = HashMap::new();
        // The delete that removed each block this landing touches and the new canon lacks, as
        // `deleters_at` will answer after this write: for a block canon held until now, this
        // landing's delete of it (smallest hash first, as `deleters_at` breaks ties within a
        // landing); otherwise the one that removed it as of the previous head.
        let mut rows = self.index().deleter_rows(
            touched_l
                .iter()
                .copied()
                .filter(|b| !state.blocks.contains_key(*b)),
        )?;
        let mut removed: HashMap<BlockId, Hash> = HashMap::new();
        rows.retain(|b, deletes| {
            if !prev_state.blocks.contains_key(b) {
                return true;
            }
            if let Some(d) = deletes
                .iter()
                .map(|(d, _)| d)
                .filter(|d| landed.contains(d))
                .min()
            {
                removed.insert(b.clone(), *d);
            }
            false
        });
        if let Some(h) = &prev_head {
            removed.extend(self.removers(rows, h)?);
        }
        let deleter = |b: &BlockId| removed.get(b).copied();
        let mut rebased = vec![];
        let mut closed = vec![];
        let mut view_refs = vec![];
        for (rname, value) in self.refs_with_prefix("view/")? {
            let name = rname.strip_prefix("view/").unwrap_or(&rname).to_string();
            let proposing = name == p.view;
            let (vhead, vincluded) = if proposing {
                let mut set = included.as_ref().clone();
                set.extend(aid);
                (head.clone(), set)
            } else {
                let h = decode_head(&name, &value)?;
                // a member the landing did not choose is closed, not rebased (altw1)
                if let Some(alt) = &req.alt
                    && alt.close.contains(&name)
                {
                    if !h.closed {
                        let reason = chosen_reason(&alt.set, &alt.member);
                        let (op, _, new_head) = self.close_part(&name, &h, &reason, &req.approver);
                        items.push((kinds::VIEW_OP, op));
                        view_refs.push((view_ref(&name), encode(&new_head)));
                        closed.push(name);
                    }
                    continue;
                }
                if h.closed || h.pinned || h.len == 0 {
                    continue;
                }
                let set = self.included_at(&name, &h)?.as_ref().clone();
                (h, set)
            };
            let (dropped, remaining): (Vec<Hash>, Vec<Hash>) =
                vincluded.into_iter().partition(|c| landed.contains(c));
            let conflicts = self.rebase_conflicts(
                &state,
                &remaining,
                &touched_l,
                &producer,
                &mut producers,
                &deleter,
            )?;
            let rebase = encode(&ViewOp {
                view: name.clone(),
                prev: Some(if proposing { tip } else { vhead.head }),
                op: ViewOpKind::Rebase {
                    landing: lid,
                    dropped: dropped.clone(),
                    remaining: remaining.clone(),
                    conflicts: conflicts.clone(),
                },
                actor: req.approver.actor.clone(),
                run: req.approver.run.clone(),
                at_ms: req.approver.at_ms,
            });
            let last_write_ms = match (&accept_change, proposing) {
                (Some(a), true) => a.provenance.at_ms,
                _ => vhead.last_write_ms,
            };
            let new_head = ViewHead {
                head: Hash::of(&rebase),
                base: Some(lid),
                len: remaining.len() as u64,
                owner: vhead.owner,
                pinned: vhead.pinned,
                closed: vhead.closed,
                last_write_ms,
            };
            items.push((kinds::VIEW_OP, rebase));
            view_refs.push((view_ref(&name), encode(&new_head)));
            rebased.push(RebasedView {
                view: name,
                dropped,
                conflicts,
            });
        }
        // 8. the refs: canon, the stable's latest version (step 4), the rebased and closed heads,
        // the alternative set's chosen version, and the view's open proposal closed when it is
        // this one
        let mut refs = vec![("canon".to_string(), lid.0.to_vec())];
        refs.extend(latest_ref);
        refs.extend(view_refs);
        refs.extend(alt_ref);
        let open = open_ref(&p.view);
        if self.get_ref(&open)?.as_deref() == Some(&stable.0[..]) {
            refs.push((open, vec![]));
        }
        items.extend(self.ref_items_pub(refs));
        // 9. one write. With applies, one REDACTION_APPLY per request (in request-hash order)
        // ends the data frames. Unheld, the checked apply writer validates the batch with the log,
        // seals it and finishes the removal before it returns; held, the frames join the open
        // batch without activating their masks, and its seal (which may add the TRANSITION last)
        // validates, activates and finishes.
        match applies {
            Some(applies) => {
                items.extend(applies.frames(lid));
                if self.held() {
                    self.write_guarded(items)?;
                } else {
                    self.seal_apply_batch(items)?;
                }
            }
            None => {
                self.write(items)?;
            }
        }
        Ok(LandOutcome {
            landing: lid,
            proposal: final_id,
            accept: aid,
            rebased,
            alt_version,
            closed,
        })
    }

    /// The set version a landing that chooses `alt` builds on: `alt.prev` must be the latest
    /// version of `alt/<set>` and open. Writes nothing.
    fn alt_landing_check(&self, alt: &AltLanding) -> Result<AltSet, StoreError> {
        let moved = || {
            StoreError::Decode(format!(
                "alternative set {} moved or is no longer open; read it again",
                alt.set
            ))
        };
        let latest = match self.get_ref(&format!("{ALT_REF}{}", alt.set))? {
            Some(b) => crate::index::hash_from(&b, "alternative set ref")?,
            None => return Err(moved()),
        };
        if latest != alt.prev {
            return Err(moved());
        }
        let version = self.alt_set(&alt.prev)?;
        if version.status != AltStatus::Open {
            return Err(moved());
        }
        Ok(version)
    }

    /// The conflicts a view's `remaining` changes have with canon's new `state`, on the blocks
    /// both they and the landing (`touched_l`) touch: none when they share no block, so the merge
    /// work is O(touched). `producer` reads a version's producer, and `producers` keeps the
    /// answers across views; a read error fails the call.
    fn rebase_conflicts(
        &self,
        state: &rdy_model::StateSnapshot,
        remaining: &[Hash],
        touched_l: &BTreeSet<&BlockId>,
        producer: &dyn Fn(&Hash) -> Result<Option<Hash>, StoreError>,
        producers: &mut HashMap<Hash, Option<Hash>>,
        deleter: &dyn Fn(&BlockId) -> Option<Hash>,
    ) -> Result<Vec<Conflict>, StoreError> {
        if remaining.is_empty() {
            return Ok(vec![]);
        }
        let rest = remaining
            .iter()
            .map(|c| self.change(c))
            .collect::<Result<Vec<_>, _>>()?;
        let isect: BTreeSet<BlockId> = rest
            .iter()
            .flat_map(|c| c.ops.iter().map(|op| op.block_id()))
            .filter(|b| touched_l.contains(b))
            .cloned()
            .collect();
        if isect.is_empty() {
            return Ok(vec![]);
        }
        let input: Vec<MergeChange<'_>> = rest
            .iter()
            .zip(remaining)
            .map(|(c, id)| MergeChange {
                id: *id,
                deps: &c.deps,
                ops: &c.ops,
            })
            .collect();
        // the merge asks only for canon's versions of `isect`: read them first, so a read error
        // fails here and is never read as "no producer"
        for v in isect.iter().filter_map(|b| state.blocks.get(b)) {
            if !producers.contains_key(v) {
                producers.insert(*v, producer(v)?);
            }
        }
        let known = |v: &Hash| producers.get(v).copied().flatten();
        Ok(merge_on_blocks(state, &input, &known, deleter, &isect).conflicts)
    }
}

/// A relayed landing carries a relay kind of evidence (card or text); an elicited approval is the
/// human's own answer and has no relay (FW-B9, D4).
fn check_evidence(
    relay: Option<&Relay>,
    evidence: Option<&ApprovalEvidence>,
) -> Result<(), StoreError> {
    match (relay, evidence) {
        (Some(_), None) => Err(StoreError::Decode(
            "a relayed landing needs its evidence kind (card or text)".into(),
        )),
        (Some(_), Some(e)) if e.kind == EvidenceKind::Elicited => Err(StoreError::Decode(
            "an elicited approval is the human's own answer: it has no relay".into(),
        )),
        (None, Some(e)) if e.kind != EvidenceKind::Elicited => Err(StoreError::Decode(format!(
            "evidence kind {} needs a relay",
            e.kind.as_str()
        ))),
        _ => Ok(()),
    }
}
