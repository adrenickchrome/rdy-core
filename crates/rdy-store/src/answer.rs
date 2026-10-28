//! Answers in the store (plan Decision intake, O1, O2, D-JV6Z8, D-DGCH7): an answer is an object
//! of kind `ANSWER` (`rdy_model::Answer`). It records one human act that no other record holds:
//! an answer to an item of the "needs you" queue, or a person's words that an agent relays for a
//! tag, a dismissal or a view act.
//!
//! # The record
//!
//! The fields, in wire order (postcard; never re-encoded, D-F0904):
//! - `item`: the item ref (O3), such as `P-x@n`, `D-x@v`, `Q-x@v`, `advice:P-x@n`, a tension id,
//!   `tag:<name>`, `own:<view>`, `stale:<view>` or `propose:<view>`;
//! - `act`: what the person did (`AnswerAct`, a closed list);
//! - `option`: the id of the option chosen (O4), when the item offered options;
//! - `label`: that option's label, from rdy's own card;
//! - `options`: the labels shown, in order;
//! - `card_hash`: the hash of the card the person answered (O7, F6);
//! - `targets`: the objects the act wrote;
//! - `person`: the person, as the actor of the act;
//! - `relayed_by`: the agent that relayed the person's words, if any;
//! - `relay_evidence`: the person's words and where they came from, with a relay;
//! - `evidence_kind`: `card` or `text` with a relay; None for a local answer;
//! - `note`: the person's note, 1 to 2000 characters when present.
//!
//! # The two ref families
//!
//! Each ref holds the 32 bytes of the answer's hash.
//! - `answer/<item>` names the latest answer to the item, for every act except `later`.
//! - `pending/<person>/<item>` names the person's latest snooze (act `later`) of the item.
//!
//! A ref name keeps `:` and `@` as they are, and a person may hold `/` (O17). So nothing splits a
//! ref name: a check rebuilds the name from the record and compares. Only `Store::put_answer`
//! (and the store siblings that call `put_answer_with`) writes either family; `set_refs` refuses
//! both.
//!
//! # The fsck rules (closed)
//!
//! Each failure is a `StoreError::Decode` with the message `answer <h>: <rule>`.
//! - An `ANSWER` decodes.
//! - `relayed_by` and `relay_evidence` are both present or both absent.
//! - `evidence_kind` is `card` or `text`, and only with a relay.
//! - `card` evidence needs `card_hash`.
//! - Each `targets` hash is an object in the log before the answer.
//! - The plan acts (store format 17): KeepSuspect answers `reaffirm:<id>@<version12>` with a
//!   note of 1 to 2000 trimmed characters and a nonempty, sorted, distinct list of BLOCK
//!   versions; AcceptDeferrals and RefuseDeferrals answer `defer:P-x@n`, and RejectReaffirm a
//!   block version item (`<id>@<version12>`), each with no target. A break is `invalid plan
//!   answer item`, `invalid plan answer targets` or `invalid plan answer note`.
//! - A ref `answer/<item>` or `pending/<person>/<item>` names an `ANSWER` whose `item` is
//!   `<item>` (and, for `pending/`, whose `person.actor` is `<person>`).
//! - A `pending/` ref names act `later`; an `answer/` ref never does.
//!
//! `put_answer` refuses an answer that breaks a rule of the record with `StoreError::Refused`.
use crate::error::StoreError;
use crate::index::hash_from;
use crate::object_refs::ObjectKey;
use crate::store::Frames;
use crate::{Store, StoreReader, kinds};
use rdy_model::{Answer, AnswerAct, EvidenceKind, Hash};

/// The ref prefix of answers: `answer/<item>` names the latest answer to the item.
pub const ANSWER_REF: &str = "answer/";

/// The ref prefix of snoozes: `pending/<person>/<item>` names the person's latest snooze.
pub const PENDING_REF: &str = "pending/";

/// The ref of the latest answer to `item`: `answer/<item>`.
pub fn answer_ref(item: &str) -> String {
    format!("{ANSWER_REF}{item}")
}

/// The ref of `person`'s latest snooze of `item`: `pending/<person>/<item>`.
pub fn snooze_ref(person: &str, item: &str) -> String {
    format!("{PENDING_REF}{person}/{item}")
}

/// The ref that names `a`: `pending/…` for a snooze, else `answer/…`.
fn ref_of(a: &Answer) -> String {
    if a.act == AnswerAct::Later {
        snooze_ref(&a.person.actor, &a.item)
    } else {
        answer_ref(&a.item)
    }
}

/// The rule of the record `a` breaks, if any (the targets rule aside): the one list that
/// `put_answer`, fsck and the sync batch check apply.
pub(crate) fn invalid(a: &Answer) -> Option<String> {
    if a.relayed_by.is_some() != a.relay_evidence.is_some() {
        return Some("relayed_by and relay_evidence must both be set or both unset".into());
    }
    match a.evidence_kind {
        Some(k @ EvidenceKind::Elicited) => {
            Some(format!("evidence kind {} is not card or text", k.as_str()))
        }
        Some(k) if a.relayed_by.is_none() => {
            Some(format!("evidence kind {} needs a relay", k.as_str()))
        }
        Some(EvidenceKind::Card) if a.card_hash.is_none() => {
            Some("card evidence needs card_hash".into())
        }
        _ => plan_act_invalid(a).map(str::to_string),
    }
}

/// The damage reason of a plan act's item.
pub(crate) const PLAN_ITEM: &str = "invalid plan answer item";
/// The damage reason of a plan act's targets.
pub(crate) const PLAN_TARGETS: &str = "invalid plan answer targets";
/// The damage reason of a plan act's note.
pub(crate) const PLAN_NOTE: &str = "invalid plan answer note";

/// Whether `item` is `<block id>@<12 lowercase hex digits>`: a block version item.
fn block_version_item(item: &str) -> bool {
    item.split_once('@').is_some_and(|(id, v)| {
        rdy_model::BlockId::is_valid(id)
            && v.len() == 12
            && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Whether `item` is `defer:P-<4 to 64 hex digits>@<n>`: an explicit proposal version.
fn defer_item(item: &str) -> bool {
    let Some(rest) = item.strip_prefix("defer:P-") else {
        return false;
    };
    rest.split_once('@').is_some_and(|(id, n)| {
        (4..=64).contains(&id.len())
            && id.bytes().all(|b| b.is_ascii_hexdigit())
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The rule of the four plan acts that `a` breaks, if any: KeepSuspect answers a re-affirmation
/// item (`reaffirm:<id>@<version12>`) with a note and its member BLOCK versions (sorted and
/// distinct) as targets; AcceptDeferrals and RefuseDeferrals answer a deferral card
/// (`defer:P-x@n`) and RejectReaffirm a block version item, each with no target. The typed
/// rule of KeepSuspect's targets (each a BLOCK) is the target check's.
fn plan_act_invalid(a: &Answer) -> Option<&'static str> {
    let ascending = |t: &[Hash]| t.windows(2).all(|w| w[0] < w[1]);
    match a.act {
        AnswerAct::KeepSuspect => {
            if crate::plan::reaffirm_cause(&a.item).is_none() {
                return Some(PLAN_ITEM);
            }
            if !a.note.as_deref().is_some_and(crate::plan::valid_reason) {
                return Some(PLAN_NOTE);
            }
            if a.targets.is_empty() || !ascending(&a.targets) {
                return Some(PLAN_TARGETS);
            }
            None
        }
        AnswerAct::AcceptDeferrals | AnswerAct::RefuseDeferrals => {
            if !defer_item(&a.item) {
                return Some(PLAN_ITEM);
            }
            (!a.targets.is_empty()).then_some(PLAN_TARGETS)
        }
        AnswerAct::RejectReaffirm => {
            if !block_version_item(&a.item) {
                return Some(PLAN_ITEM);
            }
            (!a.targets.is_empty()).then_some(PLAN_TARGETS)
        }
        _ => None,
    }
}

/// The typed target rule of `a` (store format 17), if it breaks it: a KeepSuspect's targets are
/// member BLOCK versions. `is_block` says whether a hash names a BLOCK version that `a` may name
/// (written before it). The one rule that `put_answer`, fsck and the receive checks
/// (`crate::stream_source`) apply; the generic reference edges of the targets stay kind-None.
pub(crate) fn invalid_targets<E>(
    a: &Answer,
    mut is_block: impl FnMut(&Hash) -> Result<bool, E>,
) -> Result<Option<&'static str>, E> {
    if a.act != AnswerAct::KeepSuspect {
        return Ok(None);
    }
    for t in &a.targets {
        if !is_block(t)? {
            return Ok(Some(PLAN_TARGETS));
        }
    }
    Ok(None)
}

/// The text of the rule `rule` that answer `h` breaks: `answer <h>: <rule>`.
pub(crate) fn damage(h: &Hash, rule: impl std::fmt::Display) -> String {
    format!("answer {h}: {rule}")
}

impl Store {
    /// Writes the answer `a` and its ref in one batch: `pending/<person>/<item>` for a snooze
    /// (act `later`), else `answer/<item>`. Returns the answer's id. Refuses, with
    /// `StoreError::Refused` and nothing written, an answer that breaks a rule of the record or
    /// names a target that is not an object in the log.
    pub fn put_answer(&mut self, a: &Answer) -> Result<Hash, StoreError> {
        self.put_answer_with(a, vec![])
    }

    /// `put_answer` with `extra` frames in the same batch, before the answer (O14): a store
    /// sibling writes the record its act writes and the answer that holds the relay in one
    /// batch. A target may name an object of `extra` or one the log holds already.
    pub(crate) fn put_answer_with(
        &mut self,
        a: &Answer,
        extra: Vec<(u8, Vec<u8>)>,
    ) -> Result<Hash, StoreError> {
        let (id, items) = self.answer_items(a, extra)?;
        self.write(items)?;
        Ok(id)
    }

    /// The answer's id and the frames `put_answer_with` writes: `extra`, then the answer and its
    /// ref. Checks the answer's rules and its targets as `put_answer_with` does; writes nothing.
    pub(crate) fn answer_items(
        &self,
        a: &Answer,
        extra: Vec<(u8, Vec<u8>)>,
    ) -> Result<(Hash, Frames), StoreError> {
        let (kind, bytes) = crate::codec::item_new(kinds::ANSWER, a);
        let id = Hash::of(&bytes);
        if let Some(rule) = invalid(a) {
            return Err(StoreError::Refused(damage(&id, rule)));
        }
        for t in &a.targets {
            let in_batch = extra.iter().any(|(_, p)| Hash::of(p) == *t);
            if !in_batch && self.index().object(t)?.is_none() {
                return Err(StoreError::Refused(damage(
                    &id,
                    format!("target {t} is not an object in the log"),
                )));
            }
        }
        let is_block = |t: &Hash| -> Result<bool, StoreError> {
            Ok(extra
                .iter()
                .any(|(k, p)| crate::codec::logical_kind(*k) == kinds::BLOCK && Hash::of(p) == *t)
                || self.has_kind(kinds::BLOCK, t)?)
        };
        if let Some(rule) = invalid_targets(a, is_block)? {
            return Err(StoreError::Refused(damage(&id, rule)));
        }
        // a carried act's item, targets and bindings (`crate::carried_records`), before a byte
        // is written; its record is the store's (the batch's earlier frames are not read)
        let key = ObjectKey {
            kind: kinds::ANSWER,
            hash: id,
        };
        if let Err(reason) = crate::carried_records::validate_local(kind, &bytes) {
            return Err(crate::carried_records::refusal(&id, kinds::ANSWER, reason));
        }
        let mut source = PendingAnswer {
            store: crate::carried_records::StoreObjects(self),
            key,
            body: &bytes,
        };
        if let Some(reason) = crate::carried_records::validate_refs(&mut source, key)? {
            return Err(crate::carried_records::refusal(&id, kinds::ANSWER, reason));
        }
        if a.act == AnswerAct::AcceptVerdict
            && let Some(r) = crate::carried_records::verdict_item(&a.item)
            && !self.has_kind(kinds::REVIEW, &r)?
        {
            return Err(crate::carried_records::refusal(
                &id,
                kinds::ANSWER,
                crate::carried_records::VERDICT_REVIEW,
            ));
        }
        let mut items = extra;
        items.push((kind, bytes));
        items.extend(self.ref_items_pub(vec![(ref_of(a), id.0.to_vec())]));
        Ok((id, items))
    }
}

/// The store's objects and one answer not written yet, as the carried checks read them.
struct PendingAnswer<'a> {
    store: crate::carried_records::StoreObjects<'a>,
    key: ObjectKey,
    body: &'a [u8],
}

impl crate::carried_records::CarriedSource for PendingAnswer<'_> {
    fn object(&mut self, key: ObjectKey) -> Result<Option<(u16, Vec<u8>)>, StoreError> {
        if key == self.key {
            return Ok(Some((0, self.body.to_vec())));
        }
        self.store.object(key)
    }
}

impl StoreReader {
    /// The answer `name` holds, decoded; a ref that names no stored answer is an error.
    fn answer_at(&self, name: &str, value: &[u8]) -> Result<(Hash, Answer), StoreError> {
        let h = hash_from(value, &format!("answer ref {name:?}"))?;
        let a = self.get_typed(kinds::ANSWER, &h)?;
        Ok((h, a))
    }

    fn answer_named(&self, name: &str) -> Result<Option<(Hash, Answer)>, StoreError> {
        self.get_ref(name)?
            .map(|v| self.answer_at(name, &v))
            .transpose()
    }

    /// The latest answer to `item` (the ref `answer/<item>`), if any.
    pub fn answer_of(&self, item: &str) -> Result<Option<(Hash, Answer)>, StoreError> {
        self.answer_named(&answer_ref(item))
    }

    /// `person`'s latest snooze of `item` (the ref `pending/<person>/<item>`), if any.
    pub fn snooze_of(
        &self,
        person: &str,
        item: &str,
    ) -> Result<Option<(Hash, Answer)>, StoreError> {
        self.answer_named(&snooze_ref(person, item))
    }

    /// Every ref whose name starts with `prefix` (an `answer/` or `pending/` name or a start of
    /// one) and the answer it names, in ref-name order.
    pub fn answers_with_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<(String, Hash, Answer)>, StoreError> {
        self.refs_with_prefix(prefix)?
            .into_iter()
            .map(|(name, v)| {
                let (h, a) = self.answer_at(&name, &v)?;
                Ok((name, h, a))
            })
            .collect()
    }
}

impl StoreReader {
    /// The fsck rules of the answers (closed; see the module docs): every `ANSWER` in the log,
    /// in log order up to what the index holds, keeps the record rules and names targets
    /// written before it; every `answer/` and `pending/` ref names an answer of its own item
    /// and family. Returns how many `ANSWER` records the log holds.
    pub(crate) fn check_answers(&self) -> Result<usize, StoreError> {
        let upto = self.index().indexed_upto()?;
        let origin = crate::segment::Location {
            segment: 0,
            offset: 0,
        };
        let mut n = 0;
        self.for_each_record_from(origin, upto, |at, frame, _| {
            let (h, payload) = match frame {
                crate::redaction::StoredFrame::Present {
                    kind: kinds::ANSWER,
                    hash,
                    payload,
                } => (hash, payload),
                // an answer is never removable: a dropped one is damage
                crate::redaction::StoredFrame::Dropped {
                    kind: kinds::ANSWER,
                    ..
                } => return Err(StoreError::corrupt_at(at)),
                _ => return Ok(()),
            };
            let a: Answer = crate::codec::decode_record(kinds::ANSWER, payload)
                .map_err(|_| StoreError::Decode(damage(&h, "it does not decode")))?;
            if let Some(rule) = invalid(&a) {
                return Err(StoreError::Decode(damage(&h, rule)));
            }
            for t in &a.targets {
                if !self.index().object(t)?.is_some_and(|l| l < at) {
                    return Err(StoreError::Decode(damage(
                        &h,
                        format!("target {t} is not an object in the log before it"),
                    )));
                }
            }
            let is_block = |t: &Hash| -> Result<bool, StoreError> {
                Ok(self.index().typed(kinds::BLOCK, t)?.is_some_and(|l| l < at))
            };
            if let Some(rule) = invalid_targets(&a, is_block)? {
                return Err(StoreError::Decode(damage(&h, rule)));
            }
            // an accepted verdict's review is in the log before it, as a target is (O17)
            if a.act == AnswerAct::AcceptVerdict
                && let Some(r) = crate::carried_records::verdict_item(&a.item)
                && !self
                    .index()
                    .typed(kinds::REVIEW, &r)?
                    .is_some_and(|l| l < at)
            {
                return Err(crate::carried_records::refusal(
                    &h,
                    kinds::ANSWER,
                    crate::carried_records::VERDICT_REVIEW,
                ));
            }
            n += 1;
            Ok(())
        })?;
        for (name, value) in self.refs_with_prefix(ANSWER_REF)? {
            // a holder names its relay answers by tag object (O11), which the holder check
            // checks: their name is not their item's. A mode that does not read is the meta
            // check's to report.
            if name.starts_with(crate::holder::ANSWER_TAG_OBJECT_REF)
                && matches!(self.mode(), Ok(crate::holder::StoreMode::Holder))
            {
                continue;
            }
            let (h, a) = self.answer_checked(&name, &value)?;
            if answer_ref(&a.item) != name {
                return Err(bad_ref(&h, &name, "names an answer to another item"));
            }
            if a.act == AnswerAct::Later {
                return Err(bad_ref(&h, &name, "names act later"));
            }
        }
        for (name, value) in self.refs_with_prefix(PENDING_REF)? {
            let (h, a) = self.answer_checked(&name, &value)?;
            if a.act != AnswerAct::Later {
                let act = a.act.as_str();
                return Err(bad_ref(&h, &name, &format!("names act {act}, not later")));
            }
            if snooze_ref(&a.person.actor, &a.item) != name {
                return Err(bad_ref(
                    &h,
                    &name,
                    "names a snooze of another person or item",
                ));
            }
        }
        Ok(n)
    }

    /// The answer the ref `name` holds, for fsck: a value that is not a hash or names no stored
    /// answer fails.
    fn answer_checked(&self, name: &str, value: &[u8]) -> Result<(Hash, Answer), StoreError> {
        let h = hash_from(value, &format!("answer ref {name:?}"))?;
        match self.get_typed(kinds::ANSWER, &h) {
            Ok(a) => Ok((h, a)),
            Err(StoreError::NotFound(_)) => Err(bad_ref(&h, name, "names no stored answer")),
            Err(e) => Err(e),
        }
    }
}

fn bad_ref(h: &Hash, name: &str, rule: &str) -> StoreError {
    StoreError::Decode(damage(h, format!("ref {name:?} {rule}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::{ActorRun, Provenance, RelayEvidence, Role};

    /// O14: the extra frames, the answer and its ref are one batch, in that order.
    #[test]
    fn di1_put_answer_with_writes_one_batch() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::create(&d.path().join("s")).unwrap();
        let extra = vec![
            (kinds::BLOB_CHUNK, b"one".to_vec()),
            (kinds::BLOB_CHUNK, b"two".to_vec()),
        ];
        let hashes: Vec<Hash> = extra.iter().map(|(_, p)| Hash::of(p)).collect();
        let a = Answer {
            item: "tag:v1".into(),
            act: AnswerAct::Tag,
            option: None,
            label: None,
            options: vec![],
            card_hash: None,
            targets: hashes.clone(),
            person: Provenance {
                actor: "human:nick".into(),
                run: None,
                role: Role::Approver,
                at_ms: 7,
            },
            relayed_by: Some(ActorRun {
                actor: "agent:a".into(),
                run: None,
            }),
            relay_evidence: Some(RelayEvidence {
                quoted: "tag it".into(),
                session_ref: "s-1".into(),
                question: None,
            }),
            evidence_kind: Some(EvidenceKind::Text),
            note: None,
        };
        let writes = s.write_count();
        let id = s.put_answer_with(&a, extra).unwrap();
        assert_eq!(id, a.id());
        assert_eq!(s.write_count(), writes + 1, "one write");
        let batch = s.last_batch().to_vec();
        let kinds_of: Vec<u8> = batch.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds_of,
            [
                kinds::BLOB_CHUNK,
                kinds::BLOB_CHUNK,
                kinds::ANSWER,
                kinds::REF_UPDATE
            ]
        );
        assert_eq!(batch[0].1, hashes[0]);
        assert_eq!(batch[1].1, hashes[1]);
        assert_eq!(batch[2].1, id);
        let r: kinds::RefUpdate = s.get_typed(kinds::REF_UPDATE, &batch[3].1).unwrap();
        assert_eq!(r.name, "answer/tag:v1");
        assert_eq!(r.value, id.0.to_vec());
        assert_eq!(s.answer_of("tag:v1").unwrap(), Some((id, a)));
        assert_eq!(s.check_objects().unwrap().answers, 1);
    }
}
