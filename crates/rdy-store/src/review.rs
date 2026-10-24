//! Reviews in the store (plan M2d2, rvw1): a review is an object of kind `REVIEW`. A dismissal is
//! a review with verdict `Dismiss` of a tension; the ref `dismiss/<vkey>` holds its 32 bytes.
//! Nothing removes or rewrites a `dismiss/` ref (FW-CH7): a side's content change gives the
//! tension a new vkey, which no dismissal names.
//! A mention review (FW-CV4) is a review of a mention, with verdict `Dismiss` or `Restore`; the ref
//! `mention/<block>/<entity>` holds the latest one, and each review rewrites it (reversible).
//! A verdict (plan M2e1) is a review of a pair with verdict `Tension` or `NoTension`; the ref
//! `pair/<key>` names the one that settles the pair. A `tension` verdict also opens a semantic
//! tension (kind `TENSION`): `semantic/<key>` names its latest version, and the only later version
//! is a person's acceptance of the advice.
use crate::coord::key_hash;
use crate::error::StoreError;
use crate::index::hash_from;
use crate::{Store, StoreReader, kinds};
use rdy_model::{BlockId, Hash, Review, ReviewSubject, SemanticTension, Verdict, pair_key};
use std::collections::BTreeSet;

/// The ref prefix of dismissals: `dismiss/<vkey>` names the review that dismissed the tension.
pub const DISMISS_REF: &str = "dismiss/";

/// The ref prefix of mention reviews: `mention/<block>/<entity>` names the latest review of the
/// mention of `entity` in `block`.
pub const MENTION_REF: &str = "mention/";

/// The ref prefix of verdicts: `pair/<key>` names the review that settles the pair.
pub const PAIR_REF: &str = "pair/";

/// The ref prefix of semantic tensions: `semantic/<key>` names the latest version of the pair's
/// semantic tension.
pub const SEMANTIC_REF: &str = "semantic/";

/// A verdict or a version the store's checks refuse.
fn refuse(m: impl Into<String>) -> StoreError {
    StoreError::Refused(m.into())
}

/// The ref of the mention of `entity` in `block`: `mention/<block>/<entity>`.
pub fn mention_ref(block: &BlockId, entity: &BlockId) -> String {
    format!("{MENTION_REF}{block}/{entity}")
}

/// The (block, entity) a `mention/` ref key names.
pub(crate) fn mention_key(key: &str) -> Result<(BlockId, BlockId), StoreError> {
    key.strip_prefix(MENTION_REF)
        .and_then(|k| k.split_once('/'))
        .and_then(|(b, e)| Some((BlockId::parse(b).ok()?, BlockId::parse(e).ok()?)))
        .ok_or_else(|| {
            StoreError::Decode(format!(
                "ref {key:?} does not name a mention by its block and entity"
            ))
        })
}

impl Store {
    /// Writes a dismissal: the review and the ref `dismiss/<vkey>` to it, in one batch. Refuses a
    /// review whose verdict is not `Dismiss` or whose subject has no vkey. Refuses a vkey that
    /// another review already dismissed (FW-CH7: the ref is never rewritten). A refusal is
    /// `StoreError::Refused`. The same review again
    /// is an idempotent retry: it returns the id and writes nothing.
    pub fn put_dismissal(&mut self, r: &Review) -> Result<Hash, StoreError> {
        let (id, frames) = self.dismissal_frames(r)?;
        if let Some(items) = frames {
            self.write(items)?;
        }
        Ok(id)
    }

    /// The checks of `put_dismissal` for `r`: its id, and the frames that write it (the review
    /// and its `dismiss/<vkey>` ref), or `None` when the ref already names this review (an
    /// idempotent retry). A refusal is `StoreError::Refused`.
    #[allow(clippy::type_complexity)]
    fn dismissal_frames(
        &self,
        r: &Review,
    ) -> Result<(Hash, Option<Vec<(u8, Vec<u8>)>>), StoreError> {
        let vkey = match (r.verdict, r.vkey()) {
            (Verdict::Dismiss, Some(k)) => k,
            _ => {
                return Err(StoreError::Refused(
                    "a dismissal is a review with verdict dismiss of a tension".into(),
                ));
            }
        };
        let (kind, bytes) = crate::codec::item(kinds::REVIEW, r);
        let id = Hash::of(&bytes);
        let name = format!("{DISMISS_REF}{vkey}");
        if let Some(held) = self.get_ref(&name)? {
            let held = hash_from(&held, "dismissal ref")?;
            if held == id {
                return Ok((id, None));
            }
            return Err(StoreError::Refused(format!(
                "tension vkey {vkey} is already dismissed by review {held}"
            )));
        }
        let mut items = vec![(kind, bytes)];
        items.extend(self.ref_items_pub(vec![(name, id.0.to_vec())]));
        Ok((id, Some(items)))
    }

    /// Writes the dismissals `reviews` and the answer `a` that holds the person's relay, in one
    /// batch (O14, D-562QR): each review and its `dismiss/<vkey>` ref, with the checks of
    /// `put_dismissal`, then the answer and its ref (`put_answer_with`). The answer's `targets`
    /// are set to the reviews, in order. An empty list, and two reviews of one vkey, are refused.
    /// Returns the reviews' ids and the answer's id. A refusal writes nothing.
    pub fn put_dismissals_answered(
        &mut self,
        reviews: &[Review],
        mut a: rdy_model::Answer,
    ) -> Result<(Vec<Hash>, Hash), StoreError> {
        if reviews.is_empty() {
            return Err(StoreError::Refused(
                "an answered dismissal names at least one review".into(),
            ));
        }
        let mut ids = vec![];
        let mut vkeys = BTreeSet::new();
        let mut extra = vec![];
        for r in reviews {
            let (id, frames) = self.dismissal_frames(r)?;
            if let Some(vkey) = r.vkey()
                && !vkeys.insert(vkey)
            {
                return Err(StoreError::Refused(format!(
                    "tension vkey {vkey} is dismissed twice in one batch"
                )));
            }
            extra.extend(frames.into_iter().flatten());
            ids.push(id);
        }
        a.targets.clone_from(&ids);
        let answer = self.put_answer_with(&a, extra)?;
        Ok((ids, answer))
    }

    /// Writes a mention review: the review and the ref `mention/<block>/<entity>` to it, in one
    /// batch. Refuses a review whose subject is not a mention or whose verdict is not `Dismiss` or
    /// `Restore`. The ref is rewritten by each review (FW-CV4: reversible); a review the ref
    /// already holds is an idempotent retry: it returns the id and writes nothing.
    pub fn put_mention_review(&mut self, r: &Review) -> Result<Hash, StoreError> {
        let name = match (&r.subject, r.verdict) {
            (ReviewSubject::Mention { block, entity }, Verdict::Dismiss | Verdict::Restore) => {
                mention_ref(block, entity)
            }
            _ => {
                return Err(StoreError::Refused(
                    "a mention review is a review with verdict dismiss or restore of a mention"
                        .into(),
                ));
            }
        };
        let (kind, bytes) = crate::codec::item(kinds::REVIEW, r);
        let id = Hash::of(&bytes);
        if let Some(held) = self.get_ref(&name)?
            && hash_from(&held, "mention ref")? == id
        {
            return Ok(id);
        }
        let mut items = vec![(kind, bytes)];
        items.extend(self.ref_items_pub(vec![(name, id.0.to_vec())]));
        self.write(items)?;
        Ok(id)
    }

    /// Writes a verdict: the review, the ref `pair/<key>` to it and, for a `tension` verdict, the
    /// semantic tension's first version `t` and the ref `semantic/<key>` to it, in one batch.
    /// Returns the review's id and the version's. A `tension` verdict settles the pair for good;
    /// a `no_tension` verdict settles it until a `tension` verdict rewrites the ref. The same review
    /// again is an idempotent retry: it returns its ids and writes nothing.
    pub fn put_verdict(
        &mut self,
        r: &Review,
        t: Option<&SemanticTension>,
    ) -> Result<(Hash, Option<Hash>), StoreError> {
        let (key, items) = match (&r.subject, r.verdict) {
            (ReviewSubject::Pair { key, items }, Verdict::Tension | Verdict::NoTension) => {
                (*key, items)
            }
            _ => {
                return Err(refuse(
                    "a verdict is a review with verdict tension or no_tension of a pair",
                ));
            }
        };
        if items.len() != 2 || items[0].block == items[1].block || key != pair_key(items) {
            return Err(refuse(format!(
                "pair key {key} does not match its two items"
            )));
        }
        let tension = r.verdict == Verdict::Tension;
        if tension != t.is_some() {
            return Err(refuse(
                "a tension verdict writes the tension's first version, and only a tension verdict does",
            ));
        }
        let (kind, bytes) = crate::codec::item(kinds::REVIEW, r);
        let id = Hash::of(&bytes);
        let first = match t {
            Some(t) => {
                if t.key != key
                    || &t.items != items
                    || t.review != id
                    || t.prev.is_some()
                    || t.blocking
                    || t.relay.is_some()
                    || t.relayed_by.is_some()
                {
                    return Err(refuse(
                        "the first version of a semantic tension names its verdict, has no prev, \
                         is not blocking and carries no relay",
                    ));
                }
                let tframe = crate::codec::item(kinds::TENSION, t);
                Some((Hash::of(&tframe.1), tframe))
            }
            None => None,
        };
        let pair = format!("{PAIR_REF}{key}");
        if let Some(held) = self.get_ref(&pair)? {
            let held = hash_from(&held, "verdict ref")?;
            if held == id {
                match &first {
                    None => return Ok((id, None)),
                    Some((tid, _)) if self.has_kind(kinds::TENSION, tid)? => {
                        return Ok((id, Some(*tid)));
                    }
                    Some(_) => {}
                }
            }
            if held == id || !tension || self.review(&held)?.verdict != Verdict::NoTension {
                return Err(refuse(format!("pair {key} is settled by review {held}")));
            }
        }
        let mut items = vec![(kind, bytes)];
        let mut refs = vec![(pair, id.0.to_vec())];
        let tid = first.map(|(tid, tframe)| {
            items.push(tframe);
            refs.push((format!("{SEMANTIC_REF}{key}"), tid.0.to_vec()));
            tid
        });
        items.extend(self.ref_items_pub(refs));
        self.write(items)?;
        Ok((id, tid))
    }

    /// Writes later versions of semantic tensions, one per key, and moves each `semantic/<key>` to
    /// its version, in one batch. A later version is a person's acceptance of the advice: its `prev`
    /// is the latest version, which is not blocking, and it is blocking, has advice, and keeps the
    /// latest's pair, verdict and advice. Every version is checked before any is written: one
    /// refusal refuses the batch. Returns the versions' ids, in order.
    pub fn put_tension_versions(
        &mut self,
        ts: &[SemanticTension],
    ) -> Result<Vec<Hash>, StoreError> {
        let mut seen = BTreeSet::new();
        let mut items = vec![];
        let mut refs = vec![];
        let mut ids = vec![];
        for t in ts {
            let key = t.key;
            if !seen.insert(key) {
                return Err(refuse(format!(
                    "semantic tension {key} appears twice in one batch"
                )));
            }
            let name = format!("{SEMANTIC_REF}{key}");
            let Some(held) = self.get_ref(&name)? else {
                return Err(refuse(format!("semantic tension {key} has no version yet")));
            };
            let held = hash_from(&held, "semantic tension ref")?;
            if t.prev != Some(held) {
                let prev = t.prev.map_or_else(|| "none".to_string(), |p| p.to_string());
                return Err(refuse(format!(
                    "semantic tension {key}: version {prev} is not the latest ({held})"
                )));
            }
            let latest = self.semantic_tension(&held)?;
            if latest.blocking {
                return Err(refuse(format!(
                    "semantic tension {key} is accepted already"
                )));
            }
            if !t.blocking
                || t.advice.is_none()
                || t.key != latest.key
                || t.items != latest.items
                || t.review != latest.review
                || t.advice != latest.advice
            {
                return Err(refuse(format!(
                    "semantic tension {key}: a later version accepts the advice of the first: \
                     blocking, with the same pair, verdict and advice"
                )));
            }
            let (kind, bytes) = crate::codec::item(kinds::TENSION, t);
            let id = Hash::of(&bytes);
            ids.push(id);
            items.push((kind, bytes));
            refs.push((name, id.0.to_vec()));
        }
        if ids.is_empty() {
            return Ok(ids);
        }
        items.extend(self.ref_items_pub(refs));
        self.write(items)?;
        Ok(ids)
    }
}

impl StoreReader {
    /// The review with hash `h`.
    pub fn review(&self, h: &Hash) -> Result<Review, StoreError> {
        self.get_typed(kinds::REVIEW, h)
    }

    /// Every dismissal as (vkey, review), sorted by vkey.
    pub fn dismissals(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        let mut out = self
            .refs_with_prefix(DISMISS_REF)?
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    key_hash(&key, DISMISS_REF, "dismissal")?,
                    hash_from(&value, "dismissal ref")?,
                ))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        out.sort();
        Ok(out)
    }

    /// Every mention review ref as (block, entity, latest review), sorted.
    pub fn mention_reviews(&self) -> Result<Vec<(BlockId, BlockId, Hash)>, StoreError> {
        let mut out = self
            .refs_with_prefix(MENTION_REF)?
            .into_iter()
            .map(|(key, value)| {
                let (block, entity) = mention_key(&key)?;
                Ok((block, entity, hash_from(&value, "mention ref")?))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        out.sort();
        Ok(out)
    }

    /// Every verdict ref as (pair key, review), sorted by key.
    pub fn verdicts(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.hash_refs(PAIR_REF, "verdict")
    }

    /// Every semantic tension as (pair key, latest version), sorted by key.
    pub fn semantic_tensions(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.hash_refs(SEMANTIC_REF, "semantic tension")
    }

    /// The semantic tension version with hash `h`.
    pub fn semantic_tension(&self, h: &Hash) -> Result<SemanticTension, StoreError> {
        self.get_typed(kinds::TENSION, h)
    }

    /// Every ref under `prefix` as (key hash, value hash), sorted by key.
    fn hash_refs(&self, prefix: &str, what: &str) -> Result<Vec<(Hash, Hash)>, StoreError> {
        let mut out = self
            .refs_with_prefix(prefix)?
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    key_hash(&key, prefix, what)?,
                    hash_from(&value, &format!("{what} ref"))?,
                ))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        out.sort();
        Ok(out)
    }
}
