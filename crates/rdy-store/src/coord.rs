//! Coordination objects in the store (plan M2d1, cfmt1): intents (kind `INTENT`) and alternative
//! sets (kind `ALT_SET`). Each is a chain of versions; the ref `<prefix><stable hex>` holds the
//! latest version's 32 bytes, and a write of a later version must name that latest version as
//! its `prev`.
use crate::error::StoreError;
use crate::index::hash_from;
use crate::store::encode;
use crate::{Store, StoreReader, kinds};
use rdy_model::{AltSet, Hash, Intent};

/// The ref prefix of intents: `intent/<stable>` names an intent's latest version.
pub const INTENT_REF: &str = "intent/";
/// The ref prefix of alternative sets: `alt/<stable>` names a set's latest version.
pub const ALT_REF: &str = "alt/";

fn bad(m: String) -> StoreError {
    StoreError::Decode(m)
}

/// The stable id a ref key under `prefix` names.
pub(crate) fn key_hash(key: &str, prefix: &str, what: &str) -> Result<Hash, StoreError> {
    key.strip_prefix(prefix)
        .and_then(|k| k.parse().ok())
        .ok_or_else(|| {
            bad(format!(
                "ref {key:?} does not name a {what} by its stable id"
            ))
        })
}

impl Store {
    /// Writes an intent version and moves `intent/<stable>` to it, in one batch.
    pub fn put_intent(&mut self, i: &Intent) -> Result<Hash, StoreError> {
        self.put_versioned(
            kinds::INTENT,
            INTENT_REF,
            "intent",
            i.stable,
            i.prev,
            encode(i),
        )
    }

    /// Writes an alternative set version and moves `alt/<stable>` to it, in one batch.
    pub fn put_alt_set(&mut self, s: &AltSet) -> Result<Hash, StoreError> {
        self.put_versioned(
            kinds::ALT_SET,
            ALT_REF,
            "alternative set",
            s.stable,
            s.prev,
            encode(s),
        )
    }

    /// The chain rule: a first version has neither `stable` nor `prev` and its ref must not
    /// name another version yet; a later one has both, and `prev` is the ref's current value.
    fn put_versioned(
        &mut self,
        kind: u8,
        prefix: &str,
        what: &str,
        stable: Option<Hash>,
        prev: Option<Hash>,
        bytes: Vec<u8>,
    ) -> Result<Hash, StoreError> {
        let id = Hash::of(&bytes);
        let (stable, expected) = match (stable, prev) {
            (None, None) => (id, None),
            (Some(s), Some(p)) => (s, Some(p)),
            _ => {
                return Err(bad(format!(
                    "a {what} version sets stable and prev together: both for a later version, \
                     neither for the first"
                )));
            }
        };
        let name = format!("{prefix}{stable}");
        let current = match self.get_ref(&name)? {
            Some(v) => Some(hash_from(&v, &format!("{what} ref"))?),
            None => None,
        };
        match (expected, current) {
            // the first version, written again before anything moved its ref
            (None, None) => {}
            (None, Some(c)) if c == id => {}
            (Some(p), Some(c)) if p == c => {}
            (_, Some(c)) => {
                return Err(bad(format!(
                    "{what} {stable} moved: its latest version is {c}; read it again"
                )));
            }
            (Some(_), None) => {
                return Err(bad(format!("{what} {stable} does not exist")));
            }
        }
        let mut items = vec![(kind, bytes)];
        items.extend(self.ref_items_pub(vec![(name, id.0.to_vec())]));
        self.write(items)?;
        Ok(id)
    }
}

impl StoreReader {
    /// The intent version with hash `h`.
    pub fn intent(&self, h: &Hash) -> Result<Intent, StoreError> {
        self.get_typed(kinds::INTENT, h)
    }

    /// The alternative set version with hash `h`.
    pub fn alt_set(&self, h: &Hash) -> Result<AltSet, StoreError> {
        self.get_typed(kinds::ALT_SET, h)
    }

    /// Every intent as (stable id, latest version), sorted by stable id.
    pub fn intent_heads(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.heads(INTENT_REF, "intent")
    }

    /// Every alternative set as (stable id, latest version), sorted by stable id.
    pub fn alt_heads(&self) -> Result<Vec<(Hash, Hash)>, StoreError> {
        self.heads(ALT_REF, "alternative set")
    }

    fn heads(&self, prefix: &str, what: &str) -> Result<Vec<(Hash, Hash)>, StoreError> {
        let ref_what = format!("{what} ref");
        let mut out = self
            .refs_with_prefix(prefix)?
            .into_iter()
            .map(|(key, value)| Ok((key_hash(&key, prefix, what)?, hash_from(&value, &ref_what)?)))
            .collect::<Result<Vec<_>, StoreError>>()?;
        out.sort();
        Ok(out)
    }
}
