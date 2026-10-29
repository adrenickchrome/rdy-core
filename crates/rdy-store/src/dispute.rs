//! The dispute writers (D-ZWP7Q, D-C8RZW): a person's DISPUTE of a landing and the
//! DISPUTE_RESOLUTION of a dispute, each in one batch with the ANSWER that records the act when
//! the caller gives one (a relayed act always does). The store sets the answer's act, its item
//! (`dispute:<landing>` or `resolve:<dispute>`, `crate::carried_records`) and its one target
//! (the record), so the answer always binds to the record it writes.
//!
//! The readers are `StoreReader::disputes_of`, `StoreReader::resolution_of` and
//! `StoreReader::all_disputes`, on the two dispute tables of the index. No gate, readiness,
//! freshness or landing path reads a dispute.
use crate::error::StoreError;
use crate::store::Frames;
use crate::{Store, kinds};
use rdy_model::dispute::{Dispute, DisputeResolution};
use rdy_model::{Answer, AnswerAct, Hash};

impl Store {
    /// One batch: the DISPUTE, then `answer` (its one target the dispute) when given, and the
    /// answer's ref. The dispute's reason and its landing are checked first, with the carried
    /// record rules (`Store::check_carried`): a reason out of bounds is `record <hash> (kind 26):
    /// invalid dispute reason`, a landing the store does not hold `NotFound`. Nothing is written
    /// on a refusal. Returns the dispute's hash and the answer's.
    pub fn put_dispute(
        &mut self,
        d: &Dispute,
        answer: Option<Answer>,
    ) -> Result<(Hash, Option<Hash>), StoreError> {
        let item = crate::codec::item_new(kinds::DISPUTE, d);
        let id = Hash::of(&item.1);
        self.check_carried::<Dispute>(&item, &[])?;
        let answer = answer.map(|a| Answer {
            act: AnswerAct::Dispute,
            item: format!("{}{}", crate::carried_records::DISPUTE_PREFIX, d.landing),
            targets: vec![id],
            ..a
        });
        self.write_with_answer(item, answer, id)
    }

    /// The same for a resolution: the DISPUTE_RESOLUTION, then `answer` (act ResolveDispute, item
    /// `resolve:<dispute>`, its one target the resolution) when given, and the answer's ref.
    /// `NotFound` for a dispute the store does not hold, and `Refused("dispute <hash> already has
    /// a resolution")` when one is indexed, both before anything is written. Returns the
    /// resolution's hash and the answer's.
    pub fn put_dispute_resolution(
        &mut self,
        r: &DisputeResolution,
        answer: Option<Answer>,
    ) -> Result<(Hash, Option<Hash>), StoreError> {
        if !self.has_kind(kinds::DISPUTE, &r.dispute)? {
            return Err(StoreError::NotFound(r.dispute));
        }
        if self.resolution_of(&r.dispute, self.log_end())?.is_some() {
            return Err(StoreError::Refused(format!(
                "dispute {} already has a resolution",
                r.dispute
            )));
        }
        let item = crate::codec::item_new(kinds::DISPUTE_RESOLUTION, r);
        let id = Hash::of(&item.1);
        self.check_carried::<DisputeResolution>(&item, &[])?;
        let answer = answer.map(|a| Answer {
            act: AnswerAct::ResolveDispute,
            item: format!("{}{}", crate::carried_records::RESOLVE_PREFIX, r.dispute),
            targets: vec![id],
            ..a
        });
        self.write_with_answer(item, answer, id)
    }

    /// Writes `record` (hash `id`) and, when given, `answer` with its ref, in one batch. The
    /// answer's rules are checked against the record first (`Store::answer_items`).
    fn write_with_answer(
        &mut self,
        record: (u8, Vec<u8>),
        answer: Option<Answer>,
        id: Hash,
    ) -> Result<(Hash, Option<Hash>), StoreError> {
        let (aid, items): (Option<Hash>, Frames) = match answer {
            Some(a) => {
                let (aid, items) = self.answer_items(&a, vec![record])?;
                (Some(aid), items)
            }
            None => (None, vec![record]),
        };
        self.write(items)?;
        Ok((id, aid))
    }
}
