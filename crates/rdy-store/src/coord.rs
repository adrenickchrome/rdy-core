//! Coordination objects in the store (plan M2d1, cfmt1): intents (kind `INTENT`) and alternative
//! sets (kind `ALT_SET`). Each is a chain of versions; the ref `<prefix><stable hex>` holds the
//! latest version's 32 bytes, and a write of a later version must name that latest version as
//! its `prev`.
use crate::error::StoreError;
use crate::index::hash_from;
use crate::state::view_ref;
use crate::store::encode;
use crate::{Store, StoreReader, kinds};
use rdy_model::{AltSet, Hash, Intent, Provenance};

/// The ref prefix of intents: `intent/<stable>` names an intent's latest version.
pub const INTENT_REF: &str = "intent/";
/// The ref prefix of alternative sets: `alt/<stable>` names a set's latest version.
pub const ALT_REF: &str = "alt/";

/// What `Store::alt_set_write` wrote: the set's new version and the views it closed, in the
/// order asked (an already-closed view is not listed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltWrite {
    pub version: Hash,
    pub closed: Vec<String>,
}

/// The close reason of a member view that lost when `member` was chosen from set `set`.
pub fn chosen_reason(set: &Hash, member: &str) -> String {
    format!("alternative chosen: as-{} chose {member}", set.short())
}

/// The close reason of a member view of set `set`, abandoned for `reason`.
pub fn abandoned_reason(set: &Hash, reason: &str) -> String {
    format!("alternative set abandoned: as-{}: {reason}", set.short())
}

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
            INTENT_REF,
            "intent",
            i.stable,
            i.prev,
            crate::codec::item(kinds::INTENT, i),
        )
    }

    /// Writes an alternative set version and moves `alt/<stable>` to it, in one batch.
    pub fn put_alt_set(&mut self, s: &AltSet) -> Result<Hash, StoreError> {
        self.put_versioned(
            ALT_REF,
            "alternative set",
            s.stable,
            s.prev,
            crate::codec::item(kinds::ALT_SET, s),
        )
    }

    /// Writes alternative set version `next` and closes, in `close` order, each named view that
    /// is still open (a `Close { reason }` op by `by` and its closed head), all in one batch
    /// (altw1). The version follows the chain rule of `put_alt_set`. A name with no view is
    /// `UnknownView` and nothing is written; an already-closed view is skipped.
    pub fn alt_set_write(
        &mut self,
        next: &AltSet,
        close: &[String],
        reason: &str,
        by: &Provenance,
    ) -> Result<AltWrite, StoreError> {
        let (kind, bytes) = crate::codec::item(kinds::ALT_SET, next);
        let id = Hash::of(&bytes);
        let name = self.chain_check(ALT_REF, "alternative set", next.stable, next.prev, id)?;
        let mut items = vec![(kind, bytes)];
        let mut refs = vec![(name, id.0.to_vec())];
        let mut closed: Vec<String> = vec![];
        for view in close {
            let head = self
                .view_head(view)?
                .ok_or_else(|| StoreError::UnknownView(view.clone()))?;
            if head.closed || closed.contains(view) {
                continue;
            }
            let (op, _, new_head) = self.close_part(view, &head, reason, by);
            items.push(op);
            refs.push((view_ref(view), encode(&new_head)));
            closed.push(view.clone());
        }
        items.extend(self.ref_items_pub(refs));
        self.write(items)?;
        Ok(AltWrite {
            version: id,
            closed,
        })
    }

    /// Writes a version (`bytes`, of `kind`) and moves `<prefix><stable>` to it, in one batch.
    fn put_versioned(
        &mut self,
        prefix: &str,
        what: &str,
        stable: Option<Hash>,
        prev: Option<Hash>,
        item: (u8, Vec<u8>),
    ) -> Result<Hash, StoreError> {
        let id = Hash::of(&item.1);
        let name = self.chain_check(prefix, what, stable, prev, id)?;
        let mut items = vec![item];
        items.extend(self.ref_items_pub(vec![(name, id.0.to_vec())]));
        self.write(items)?;
        Ok(id)
    }

    /// The chain rule for version `id`: a first version has neither `stable` nor `prev` and its
    /// ref must not name another version yet; a later one has both, and `prev` is the ref's
    /// current value. Returns the ref's name. Writes nothing.
    fn chain_check(
        &self,
        prefix: &str,
        what: &str,
        stable: Option<Hash>,
        prev: Option<Hash>,
        id: Hash,
    ) -> Result<String, StoreError> {
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
        Ok(name)
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
