//! The check writers and readers (D-JQD5X, D-AYYD8, D-1HDVA, D-9P62P): the first product writers
//! of a CHECK_REQUEST and a Check REVIEW, and bounded reads of them and of AcceptVerdict answers.
//!
//! - A request and its ref `check/<request>` are one batch. First use enables the `check_request`
//!   feature in that batch (`crate::format`).
//! - A Check review is a REVIEW with no ref of its own. A request takes one review from this
//!   writer. Imported history may hold more, and the reader exposes them all.
//! - An AcceptVerdict answer goes through `Store::put_answer`. The ref `answer/<item>` names only
//!   the latest answer, so holds for several versions are read from `for_each_verdict_answer`.
//!
//! The store checks only the carried shape (`crate::carried_records`). The check catalogue, the
//! authorization and the meaning of a card option are the engine's. Each reader streams one object
//! at a time, in ascending full hash order, as the log stood at the captured end. A record a
//! redaction removed is not visited.
use crate::carried_records::{self, CHECK_REF, CHECK_REF_PREFIX};
use crate::closure::END_OF_LOG;
use crate::error::StoreError;
use crate::index::hash_from;
use crate::segment::Location;
use crate::{Store, StoreReader, codec, kinds};
use rdy_model::check_request::CheckRequest;
use rdy_model::{Answer, AnswerAct, Hash, Review, ReviewSubject};
use serde::de::DeserializeOwned;
use std::collections::BTreeSet;

/// The most requests in one batch: a change's contradiction fan-out stays all or nothing.
const MAX_REQUEST_BATCH: usize = 20;

impl Store {
    /// Writes one request. See [`Store::put_check_requests`].
    pub fn put_check_request(&mut self, r: &CheckRequest) -> Result<Hash, StoreError> {
        let ids = self.put_check_requests(std::slice::from_ref(r))?;
        Ok(ids[0])
    }

    /// Writes 1 to 20 requests and their `check/<request>` refs in one batch, and returns their
    /// hashes in input order. Every row is checked before anything is written, with the carried
    /// record rules (`Store::check_carried`): one refusal writes nothing. A row given twice, or
    /// held already with its ref, writes nothing more, and with no new row there is no batch. A
    /// `check/` ref that does not name its held request refuses.
    pub fn put_check_requests(&mut self, rows: &[CheckRequest]) -> Result<Vec<Hash>, StoreError> {
        if !(1..=MAX_REQUEST_BATCH).contains(&rows.len()) {
            return Err(StoreError::Refused(format!(
                "a check request batch takes 1 to {MAX_REQUEST_BATCH} requests"
            )));
        }
        let mut ids = Vec::with_capacity(rows.len());
        let mut seen = BTreeSet::new();
        let mut objects = vec![];
        let mut refs = vec![];
        for row in rows {
            let item = codec::item_new(kinds::CHECK_REQUEST, row);
            let id = Hash::of(&item.1);
            self.check_carried::<CheckRequest>(&item, &[])?;
            ids.push(id);
            if !seen.insert(id) {
                continue;
            }
            let name = format!("{CHECK_REF_PREFIX}{id}");
            let object = self.has_kind(kinds::CHECK_REQUEST, &id)?;
            let held = self.get_ref(&name)?;
            // a ref that fsck refuses is damage: this writer never repairs it
            if let Some(value) = &held
                && (hash_from(value, "check ref")? != id || !object)
            {
                return Err(carried_records::refusal(
                    &id,
                    kinds::CHECK_REQUEST,
                    CHECK_REF,
                ));
            }
            if !object {
                objects.push(item);
            }
            if held.is_none() {
                refs.push((name, id.0.to_vec()));
            }
        }
        if objects.is_empty() && refs.is_empty() {
            return Ok(ids);
        }
        objects.extend(self.ref_items_pub(refs));
        self.write(objects)?;
        Ok(ids)
    }

    /// Writes a Check review of a request the store holds, with the carried record rules: the
    /// verdict, the case of its request, the reason and the citations. The same review again
    /// returns its hash and writes nothing. A request with another review refuses, naming the
    /// lowest hash when imported history holds several. Nothing overwrites a review, and no ref is
    /// written.
    pub fn put_check_review(&mut self, r: &Review) -> Result<Hash, StoreError> {
        let ReviewSubject::Check { request } = r.subject else {
            return Err(StoreError::Refused(
                "a check review is a review with verdict pass, concern or fail of a check request"
                    .into(),
            ));
        };
        let item = codec::item_new(kinds::REVIEW, r);
        let id = Hash::of(&item.1);
        self.check_carried::<Review>(&item, &[])?;
        if self.has_kind(kinds::REVIEW, &id)? {
            return Ok(id);
        }
        let mut lowest = None;
        self.for_each_check_review(END_OF_LOG, |h, held| {
            if lowest.is_none()
                && matches!(held.subject, ReviewSubject::Check { request: q } if q == request)
            {
                lowest = Some(h);
            }
            Ok(())
        })?;
        if let Some(held) = lowest {
            return Err(StoreError::Refused(format!(
                "check request {request} already has review {held}"
            )));
        }
        self.write(vec![item])?;
        Ok(id)
    }
}

impl StoreReader {
    /// The check request with hash `h`.
    pub fn check_request(&self, h: &Hash) -> Result<CheckRequest, StoreError> {
        self.get_typed(kinds::CHECK_REQUEST, h)
    }

    /// Calls `f` with every check request the log held before `end`.
    pub fn for_each_check_request(
        &self,
        end: Location,
        f: impl FnMut(Hash, CheckRequest) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.for_each_record_before(kinds::CHECK_REQUEST, end, f)
    }

    /// Calls `f` with every Check review the log held before `end`, whatever request it names.
    pub fn for_each_check_review(
        &self,
        end: Location,
        mut f: impl FnMut(Hash, Review) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.for_each_record_before(kinds::REVIEW, end, |h, r: Review| {
            if matches!(r.subject, ReviewSubject::Check { .. }) {
                f(h, r)
            } else {
                Ok(())
            }
        })
    }

    /// Calls `f` with every AcceptVerdict answer the log held before `end`, including those the
    /// ref `answer/<item>` no longer names.
    pub fn for_each_verdict_answer(
        &self,
        end: Location,
        mut f: impl FnMut(Hash, Answer) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.for_each_record_before(kinds::ANSWER, end, |h, a: Answer| {
            if a.act == AnswerAct::AcceptVerdict {
                f(h, a)
            } else {
                Ok(())
            }
        })
    }

    /// Calls `f` with each present object of `kind` before `end`, decoded, in ascending hash
    /// order: filtered by kind, then by the captured end, then read and decoded one at a time.
    fn for_each_record_before<T: DeserializeOwned + 'static>(
        &self,
        kind: u8,
        end: Location,
        mut f: impl FnMut(Hash, T) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.for_each_object_of(kind, |h| {
            if !self.has_kind_before(kind, &h, end)? {
                return Ok(());
            }
            match self.get_typed::<T>(kind, &h) {
                Ok(v) => f(h, v),
                Err(StoreError::Redacted { .. }) => Ok(()),
                Err(e) => Err(e),
            }
        })
    }
}
