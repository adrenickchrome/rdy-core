//! Reviews in the store (plan M2d2, rvw1): a review is an object of kind `REVIEW`. A dismissal is
//! a review with verdict `Dismiss` of a tension; the ref `dismiss/<vkey>` holds its 32 bytes.
//! Nothing removes or rewrites a `dismiss/` ref (FW-CH7): a side's content change gives the
//! tension a new vkey, which no dismissal names.
//! A mention review (FW-CV4) is a review of a mention, with verdict `Dismiss` or `Restore`; the ref
//! `mention/<block>/<entity>` holds the latest one, and each review rewrites it (reversible).
use crate::coord::key_hash;
use crate::error::StoreError;
use crate::index::hash_from;
use crate::store::encode;
use crate::{Store, StoreReader, kinds};
use rdy_model::{BlockId, Hash, Review, ReviewSubject, Verdict};

/// The ref prefix of dismissals: `dismiss/<vkey>` names the review that dismissed the tension.
pub const DISMISS_REF: &str = "dismiss/";

/// The ref prefix of mention reviews: `mention/<block>/<entity>` names the latest review of the
/// mention of `entity` in `block`.
pub const MENTION_REF: &str = "mention/";

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
    /// another review already dismissed (FW-CH7: the ref is never rewritten). The same review again
    /// is an idempotent retry: it returns the id and writes nothing.
    pub fn put_dismissal(&mut self, r: &Review) -> Result<Hash, StoreError> {
        let vkey = match (r.verdict, r.vkey()) {
            (Verdict::Dismiss, Some(k)) => k,
            _ => {
                return Err(StoreError::Decode(
                    "a dismissal is a review with verdict dismiss of a tension".into(),
                ));
            }
        };
        let bytes = encode(r);
        let id = Hash::of(&bytes);
        let name = format!("{DISMISS_REF}{vkey}");
        if let Some(held) = self.get_ref(&name)? {
            let held = hash_from(&held, "dismissal ref")?;
            if held == id {
                return Ok(id);
            }
            return Err(StoreError::Decode(format!(
                "tension vkey {vkey} is already dismissed by review {held}"
            )));
        }
        let mut items = vec![(kinds::REVIEW, bytes)];
        items.extend(self.ref_items_pub(vec![(name, id.0.to_vec())]));
        self.write(items)?;
        Ok(id)
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
                return Err(StoreError::Decode(
                    "a mention review is a review with verdict dismiss or restore of a mention"
                        .into(),
                ));
            }
        };
        let bytes = encode(r);
        let id = Hash::of(&bytes);
        if let Some(held) = self.get_ref(&name)?
            && hash_from(&held, "mention ref")? == id
        {
            return Ok(id);
        }
        let mut items = vec![(kinds::REVIEW, bytes)];
        items.extend(self.ref_items_pub(vec![(name, id.0.to_vec())]));
        self.write(items)?;
        Ok(id)
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
}
