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
use crate::redaction::PreparedRedactionApply;
use crate::state::{PreparedProposal, open_ref, proposal_ref, view_ref};
use crate::store::encode;
use crate::view::{ViewHead, ViewOp, ViewOpKind, decode_head};
use rdy_model::plan::Plan;
use rdy_model::signature::Signature;
use rdy_model::{
    ActorRun, AltChoice, AltSet, AltStatus, ApprovalEvidence, Assurance, BlockId, Change,
    EvidenceKind, Hash, LandingRecord, Op, Proposal, Provenance, RelayEvidence,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

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

/// A landing built by [`Store::prepare_land`] and not yet written: its frames, its landing
/// record and hash, and the outcome its write reports. Its frames are private: only
/// [`Store::commit_land`] writes them, with the signatures of its landing.
#[derive(Debug)]
pub struct PreparedLanding {
    items: Vec<(u8, Vec<u8>)>,
    applies: Option<PreparedRedactionApply>,
    landing: LandingRecord,
    hash: Hash,
    outcome: LandOutcome,
    /// Whether [`Store::plan_landing`] added the landing's plans.
    planned: bool,
    /// A re-affirmation card's landing (D-CQMA4): its internal view's name and head after the
    /// landing's rebase, and whether [`Store::close_reaffirm_view`] closed it. None for every
    /// other landing.
    fresh: Option<FreshLanding>,
}

/// The internal view of a prepared re-affirmation landing.
#[derive(Debug)]
struct FreshLanding {
    view: String,
    head: ViewHead,
    closed: bool,
}

/// What a landing is built on: a proposal version the store holds, or the in-memory proposal of a
/// re-affirmation card ([`Store::prepare_reaffirm_propose`]), whose frames start the batch.
enum LandBase {
    Stored,
    Fresh(Box<PreparedProposal>),
}

impl PreparedLanding {
    /// The landing record the commit writes.
    pub fn landing(&self) -> &LandingRecord {
        &self.landing
    }

    /// The hash of that landing record.
    pub fn hash(&self) -> Hash {
        self.hash
    }

    /// The proposal version the landing lands: the request's, or the final version an
    /// acceptance change gives it.
    pub fn landed_version(&self) -> Hash {
        self.outcome.proposal
    }

    /// Whether the landing writes a final version of its own (an acceptance change).
    pub fn writes_final_version(&self) -> bool {
        self.outcome.accept.is_some()
    }
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
    /// landing names their requests in `checks` (sorted), and the store checks them before
    /// anything is written (`prepare_redaction_apply`): `checks` is exactly those requests, a
    /// person's approval (`RedactionHuman` otherwise), each carrier in the proposing view's history
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
        let prepared = self.prepare_land(req)?;
        self.commit_land(prepared, &[])
    }

    /// The landing `land_atomic` would write for `req`, prepared without writing anything: every
    /// check of `land_atomic` runs, and the frames, the landing record and the outcome are built
    /// against the store as it is now. [`Store::commit_land`] writes them. The preparation stays
    /// current only while the log does not move: a caller that releases the writer between the
    /// two checks the log position itself.
    pub fn prepare_land(&self, req: LandRequest) -> Result<PreparedLanding, StoreError> {
        self.prepare_land_with(req, &|_| {})
    }

    /// [`Store::prepare_land`] with `alter` run on the landing record before the redaction
    /// applies are checked: the store's own refusals of a forged record are then testable.
    #[doc(hidden)]
    pub fn prepare_land_altered_for_tests(
        &self,
        req: LandRequest,
        alter: &dyn Fn(&mut LandingRecord),
    ) -> Result<PreparedLanding, StoreError> {
        self.prepare_land_with(req, alter)
    }

    fn prepare_land_with(
        &self,
        req: LandRequest,
        alter: &dyn Fn(&mut LandingRecord),
    ) -> Result<PreparedLanding, StoreError> {
        self.land_frames(LandBase::Stored, req, alter)
    }

    /// The landing of a re-affirmation card (D-CQMA4, D-EJBPD), prepared without writing
    /// anything: the batch starts with `prepared`'s frames (the internal view's `Create` op, the
    /// members' change, its `Commit` op, the proposal and its bound plan), then the landing of
    /// that version as [`Store::prepare_land`] builds it, with every one of its checks (the
    /// evidence through the shared field rule), by `approver` with `relay` and `evidence`. The
    /// landing clears the proposal's `open/` ref in the same batch. [`Store::plan_landing`] then
    /// adds its seal and [`Store::close_reaffirm_view`] closes the view; [`Store::commit_land`]
    /// writes the batch only after both. `prepared` must come from
    /// [`Store::prepare_reaffirm_propose`] with its plan bound (`Refused` otherwise).
    pub fn prepare_reaffirm_land(
        &self,
        prepared: PreparedProposal,
        approver: Provenance,
        relay: Option<Relay>,
        evidence: Option<ApprovalEvidence>,
    ) -> Result<PreparedLanding, StoreError> {
        if prepared.fresh.is_none() || prepared.plan.is_none() {
            return Err(StoreError::Refused(
                "a re-affirmation landing takes a re-affirmation proposal with its bound plan"
                    .into(),
            ));
        }
        let req = LandRequest {
            proposal: prepared.version(),
            accept: None,
            approver,
            alt: None,
            relay,
            evidence,
        };
        self.land_frames(LandBase::Fresh(Box::new(prepared)), req, &|_| {})
    }

    /// Closes the internal view of the re-affirmation landing `prepared` after its seal
    /// (D-CQMA4): a `Close` op after the landing's rebase of the view, by `by`, and the view's
    /// head ref, the batch's only write of it. Refused for any other landing, before the seal, or
    /// twice. Writes nothing.
    pub fn close_reaffirm_view(
        &self,
        prepared: &mut PreparedLanding,
        by: &Provenance,
    ) -> Result<(), StoreError> {
        let planned = prepared.planned;
        let Some(fresh) = prepared.fresh.as_mut().filter(|f| !f.closed && planned) else {
            return Err(StoreError::Refused(
                "only a sealed re-affirmation landing closes its view, once".into(),
            ));
        };
        let (op, _, head) = self.close_part(&fresh.view, &fresh.head, REAFFIRM_CLOSE_REASON, by);
        prepared.items.push(op);
        prepared
            .items
            .extend(self.ref_items_pub(vec![(view_ref(&fresh.view), encode(&head))]));
        fresh.head = head;
        fresh.closed = true;
        Ok(())
    }

    /// The landing of `base` that `req` names: [`Store::prepare_land`]'s checks and frames.
    fn land_frames(
        &self,
        base: LandBase,
        req: LandRequest,
        alter: &dyn Fn(&mut LandingRecord),
    ) -> Result<PreparedLanding, StoreError> {
        // 1. the proposal, and the version check: the request names its stable's latest version.
        // A re-affirmation card's version is new: its batch writes its first ref
        let (p, stable, fresh) = match base {
            LandBase::Stored => {
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
                (p, stable, None)
            }
            LandBase::Fresh(prepared) => {
                let prepared = *prepared;
                (prepared.proposal.clone(), prepared.stable(), Some(prepared))
            }
        };
        // 1b. the relay and the evidence kind agree (FW-B9; D-CQMA4's local card evidence)
        check_evidence(req.relay.as_ref(), req.evidence.as_ref(), &p.view)?;
        // 1c. the alternative set: the version the approver saw is still its latest, and open
        let alt_prev = match &req.alt {
            Some(alt) => Some(self.alt_landing_check(alt)?),
            None => None,
        };
        // 2. the proposing view's head and included set
        let (head, included, mut items, fresh_change) = match fresh {
            None => (
                self.view_head(&p.view)?
                    .ok_or_else(|| StoreError::UnknownView(p.view.clone()))?,
                self.included(&p.view)?,
                vec![],
                None,
            ),
            Some(prepared) => {
                let f = prepared
                    .fresh
                    .expect("a re-affirmation landing's proposal has its view");
                (
                    f.head,
                    Arc::new(p.changes.clone()),
                    prepared.items,
                    Some(f.change),
                )
            }
        };
        // 3. a change lands once
        for c in &p.changes {
            if let Some(l) = self.landing_of(c)? {
                return Err(StoreError::Decode(format!(
                    "change {c} already landed in {l}"
                )));
            }
        }
        // 4. the acceptance change, its commit op and the final proposal version
        let mut latest_ref = None;
        let mut final_changes = p.changes.clone();
        let mut accept_change = None;
        let (tip, final_id, aid) = match req.accept {
            Some(mut accept) => {
                let (frames, pos) = self.change_frames(&mut accept, Some(&p.view), &[])?;
                let aid = Hash::of(&frames[pos].1);
                items.extend(frames);
                let by = &accept.provenance;
                let commit = crate::codec::item(
                    kinds::VIEW_OP,
                    &ViewOp {
                        view: p.view.clone(),
                        prev: Some(head.head),
                        op: ViewOpKind::Commit { change: aid },
                        actor: by.actor.clone(),
                        run: by.run.clone(),
                        at_ms: by.at_ms,
                    },
                );
                let tip = Hash::of(&commit.1);
                items.push(commit);
                final_changes.push(aid);
                let fin = crate::codec::item(
                    kinds::PROPOSAL,
                    &Proposal {
                        stable: Some(stable),
                        prev: Some(req.proposal),
                        title: p.title.clone(),
                        view: p.view.clone(),
                        changes: final_changes.clone(),
                        alternative: p.alternative.clone(),
                        provenance: p.provenance.clone(),
                        canon_at: p.canon_at,
                        summary: p.summary.clone(),
                    },
                );
                let final_id = Hash::of(&fin.1);
                items.push(fin);
                latest_ref = Some((proposal_ref(&stable), final_id.0.to_vec()));
                accept_change = Some(accept);
                (tip, final_id, Some(aid))
            }
            None => (head.head, req.proposal, None),
        };
        // 5. canon after the landing
        let mut changes = match &fresh_change {
            Some(c) => vec![c.clone()],
            None => p
                .changes
                .iter()
                .map(|c| self.change(c))
                .collect::<Result<Vec<_>, _>>()?,
        };
        changes.extend(accept_change.iter().cloned());
        let (prev_head, prev_state) = (self.canon_head()?, self.canon_state()?);
        let state = self.merge_checked(&prev_state, &changes)?.0.state;
        let snapshot = crate::codec::item(kinds::SNAPSHOT, &state);
        let sid = Hash::of(&snapshot.1);
        items.push(snapshot);
        // 6. the landing. A removal landing names the requests of the carriers it lands in
        // `checks`, sorted and without repeats: its hash, and so a signed tag, commits to the
        // exact removal set (D-CC90E), and no other request of the same carrier can reuse the
        // approval. A landing with no carrier names nothing.
        let mut checks = vec![];
        for c in &final_changes {
            if let Some(r) = self.redaction_for_carrier(c)? {
                checks.push(r);
            }
        }
        checks.sort();
        checks.dedup();
        let mut landing = LandingRecord {
            proposal: final_id,
            prev: self.canon_head()?,
            state: sid,
            approved_by: req.approver.clone(),
            changes: final_changes.clone(),
            chosen: req.alt.as_ref().map(|a| AltChoice {
                set: a.set,
                member: a.member.clone(),
            }),
            checks,
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
        alter(&mut landing);
        // 6a. the redaction applies of the carriers it lands, checked before anything is written
        let applies =
            self.prepare_redaction_apply(&p.view, &included, &landing, accept_change.as_ref())?;
        let lframe = crate::codec::item(kinds::LANDING, &landing);
        let lid = Hash::of(&lframe.1);
        let landing_record = landing;
        items.push(lframe);
        // 6b. the set's next version, chosen by this landing
        let mut alt_ref = None;
        let alt_version = match (&req.alt, alt_prev) {
            (Some(alt), Some(prev_version)) => {
                let next = crate::codec::item(
                    kinds::ALT_SET,
                    &AltSet {
                        stable: Some(alt.set),
                        prev: Some(alt.prev),
                        status: AltStatus::Chosen {
                            member: alt.member.clone(),
                            landing: lid,
                        },
                        provenance: req.approver.clone(),
                        ..prev_version
                    },
                );
                let id = Hash::of(&next.1);
                items.push(next);
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
        // the versions the acceptance change (or a re-affirmation card's change) writes have no
        // producer in the index until the write
        let unwritten = match (&accept_change, aid, &fresh_change) {
            (Some(a), Some(aid), _) => Some((a, aid)),
            (_, _, Some(c)) => p.changes.first().map(|cid| (c, *cid)),
            _ => None,
        };
        let accepted: HashMap<Hash, Hash> = match unwritten {
            Some((a, aid)) => a
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::Create { block } | Op::Update { block, .. } => {
                        Some((block.version_hash(), aid))
                    }
                    Op::Delete { .. } => None,
                })
                .collect(),
            None => HashMap::new(),
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
        // a re-affirmation card's internal view has no ref before this batch: it takes its place
        // in `view/` ref order, and its head ref is the close's to write
        let mut views = self.refs_with_prefix("view/")?;
        if fresh_change.is_some() {
            let name = view_ref(&p.view);
            let at = views.partition_point(|(n, _)| *n < name);
            views.insert(at, (name, vec![]));
        }
        let mut fresh_head = None;
        for (rname, value) in views {
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
                        items.push(op);
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
            let rebase = crate::codec::item(
                kinds::VIEW_OP,
                &ViewOp {
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
                },
            );
            let last_write_ms = match (&accept_change, proposing) {
                (Some(a), true) => a.provenance.at_ms,
                _ => vhead.last_write_ms,
            };
            let new_head = ViewHead {
                head: Hash::of(&rebase.1),
                base: Some(lid),
                len: remaining.len() as u64,
                owner: vhead.owner,
                pinned: vhead.pinned,
                closed: vhead.closed,
                last_write_ms,
            };
            items.push(rebase);
            if proposing && fresh_change.is_some() {
                fresh_head = Some(new_head);
            } else {
                view_refs.push((view_ref(&name), encode(&new_head)));
            }
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
        if fresh_change.is_some() || self.get_ref(&open)?.as_deref() == Some(&stable.0[..]) {
            refs.push((open, vec![]));
        }
        items.extend(self.ref_items_pub(refs));
        let fresh = match (fresh_change, fresh_head) {
            (Some(_), Some(head)) => Some(FreshLanding {
                view: p.view.clone(),
                head,
                closed: false,
            }),
            (Some(_), None) => {
                return Err(StoreError::Refused(
                    "the re-affirmation landing did not rebase its view".into(),
                ));
            }
            (None, _) => None,
        };
        Ok(PreparedLanding {
            fresh,
            planned: false,
            items,
            applies,
            landing: landing_record,
            hash: lid,
            outcome: LandOutcome {
                landing: lid,
                proposal: final_id,
                accept: aid,
                rebased,
                alt_version,
                closed,
            },
        })
    }

    /// Adds the landing's change plans to `prepared`'s batch (D-4JMPH, p-8D6ES), after its
    /// landing and refs and before its signatures: `bound`, the plan version bound to the final
    /// version an acceptance change gives the landing, then `seal`, the sealed version naming
    /// this landing, each with `plan/<view>` naming it. Each keeps every plan rule against the
    /// store and the batch before it (`Store::check_plan`): `bound` names the landed final
    /// version and has no seal (`plan <hash>: invalid proposal` or `invalid seal`); `seal` seals
    /// this landing (`invalid seal`). Writes nothing; a landing takes its plans once.
    pub fn plan_landing(
        &self,
        prepared: &mut PreparedLanding,
        bound: Option<&Plan>,
        seal: Option<&Plan>,
    ) -> Result<(), StoreError> {
        let refuse = |p: &Plan, reason: &str| {
            Err(StoreError::Refused(crate::plan::damage(
                &crate::plan::plan_hash(p),
                reason,
            )))
        };
        if let Some(b) = bound {
            if b.seal.is_some() {
                return refuse(b, crate::plan::INVALID_SEAL);
            }
            if prepared.planned
                || b.proposal != Some(prepared.outcome.proposal)
                || prepared.outcome.accept.is_none()
            {
                return refuse(b, crate::plan::INVALID_PROPOSAL);
            }
        }
        if let Some(sealed) = seal
            && (prepared.planned
                || sealed
                    .seal
                    .as_ref()
                    .is_none_or(|x| x.landing != prepared.hash))
        {
            return refuse(sealed, crate::plan::INVALID_SEAL);
        }
        let mut items = std::mem::take(&mut prepared.items);
        let mut add = |p: &Plan| -> Result<(), StoreError> {
            let (_, frames) = self.plan_frames(p, &items)?;
            items.extend(frames);
            Ok(())
        };
        let added = bound.into_iter().chain(seal).try_for_each(&mut add);
        prepared.items = items;
        added?;
        prepared.planned = bound.is_some() || seal.is_some();
        Ok(())
    }

    /// Writes the landing `prepared` holds, with `signatures` (each of its landing, D-1Z951) in
    /// the same batch: after the landing, the refs and its plans, before any REDACTION_APPLY. A
    /// failed write writes nothing. With no signatures this is the write `land_atomic` makes.
    pub fn commit_land(
        &mut self,
        prepared: PreparedLanding,
        signatures: &[Signature],
    ) -> Result<LandOutcome, StoreError> {
        let PreparedLanding {
            mut items,
            applies,
            hash: lid,
            outcome,
            fresh,
            ..
        } = prepared;
        // a re-affirmation card's landing is written only whole: sealed, its view closed
        if fresh.is_some_and(|f| !f.closed) {
            return Err(StoreError::Refused(
                "a re-affirmation landing is written only with its view closed".into(),
            ));
        }
        items.extend(crate::signature::frames(signatures)?);
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
                // the prepared frames hold only typed records: its plans are the plan writer's
                crate::redaction::refuse_redaction_kinds(&items)?;
                self.write_guarded(items)?;
            }
        }
        Ok(outcome)
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

/// The reason of the `Close` op of a re-affirmation card's internal view.
pub const REAFFIRM_CLOSE_REASON: &str = "the re-affirmation card landed";

/// A relayed landing carries a relay kind of evidence (card or text); an elicited approval is the
/// human's own answer and has no relay (FW-B9, D4). A landing with no relay records card evidence
/// only as a re-affirmation card's landing of the proposal of view `view`
/// ([`crate::local_card_evidence_allowed`], the field rule fsck shares).
fn check_evidence(
    relay: Option<&Relay>,
    evidence: Option<&ApprovalEvidence>,
    view: &str,
) -> Result<(), StoreError> {
    match (relay, evidence) {
        (None, Some(e)) if crate::local_card_evidence_allowed(e, Some(view)) => Ok(()),
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
