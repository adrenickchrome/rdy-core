//! Reviews in the store (plan M2d2, rvw1): a review is an object of kind `REVIEW`. A dismissal is
//! a review with verdict `Dismiss` of a tension; the ref `dismiss/<vkey>` holds its 32 bytes.
//! Nothing removes or rewrites a `dismiss/` ref (FW-CH7): a side's content change gives the
//! tension a new vkey, which no dismissal names.
use crate::coord::key_hash;
use crate::error::StoreError;
use crate::index::hash_from;
use crate::store::encode;
use crate::{Store, StoreReader, kinds};
use rdy_model::{Hash, Review, Verdict};

/// The ref prefix of dismissals: `dismiss/<vkey>` names the review that dismissed the tension.
pub const DISMISS_REF: &str = "dismiss/";

impl Store {
    /// Writes a dismissal: the review and the ref `dismiss/<vkey>` to it, in one batch. Refuses a
    /// review whose verdict is not `Dismiss` or whose subject has no vkey.
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
        let mut items = vec![(kinds::REVIEW, bytes)];
        items.extend(self.ref_items_pub(vec![(format!("{DISMISS_REF}{vkey}"), id.0.to_vec())]));
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
}
