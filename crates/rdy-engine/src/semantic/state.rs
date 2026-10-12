//! The verdicts and semantic tensions of one published state, read from the store's pair/ and semantic/ refs. A function of the log: reindex rebuilds it.
use crate::error::RdyError;
use imbl::OrdMap;
use rdy_model::{BlockId, Hash, PairItem, Review, ReviewSubject, SemanticTension, Verdict};
use rdy_store::review::{PAIR_REF, SEMANTIC_REF};
use rdy_store::{StoreError, StoreReader};
use std::sync::Arc;

/// The verdict that settles a pair: the review, its verdict, the pair it judged, and who judged it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub review: Hash,
    pub verdict: Verdict,
    pub items: Vec<PairItem>,
    /// The review's provenance actor.
    pub actor: String,
}

/// A pair's semantic tension: its latest version and that version's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenTension {
    pub version: Hash,
    pub record: SemanticTension,
}

/// The semantic tensions one item takes part in: by the other item's block, then its content,
/// the pair's key.
pub type Partners = OrdMap<BlockId, OrdMap<Hash, Hash>>;

/// The verdicts and semantic tensions of one published state, by pair key.
#[derive(Clone, Debug, Default)]
pub struct SemanticState {
    settled: OrdMap<Hash, Arc<Settled>>,
    tensions: OrdMap<Hash, Arc<OpenTension>>,
    /// The semantic tensions by item (FW-SW3): a block at a content, then the other item. A
    /// reader that knows the contents each block holds now reaches only the records on them, so
    /// records on contents a block no longer holds cost nothing however many there are.
    by_item: OrdMap<PairItem, Partners>,
    /// How many semantic tensions each block takes part in.
    blocks: OrdMap<BlockId, usize>,
}

impl SemanticState {
    /// Reads every `pair/` and `semantic/` ref.
    pub fn read(reader: &StoreReader) -> Result<SemanticState, RdyError> {
        let mut s = SemanticState::default();
        for (key, review) in reader.verdicts()? {
            s.settle(key, review, reader.review(&review)?)?;
        }
        for (key, version) in reader.semantic_tensions()? {
            s.open(key, version, reader.semantic_tension(&version)?);
        }
        Ok(s)
    }

    /// This state with the `pair/` and `semantic/` refs of each key in `keys` read again.
    pub(crate) fn reread(
        &self,
        reader: &StoreReader,
        keys: &[Hash],
    ) -> Result<SemanticState, RdyError> {
        let mut s = self.clone();
        for key in keys {
            s.settled.remove(key);
            s.close(key);
            if let Some(review) = ref_hash(reader, PAIR_REF, key, "verdict ref")? {
                s.settle(*key, review, reader.review(&review)?)?;
            }
            if let Some(version) = ref_hash(reader, SEMANTIC_REF, key, "semantic tension ref")? {
                s.open(*key, version, reader.semantic_tension(&version)?);
            }
        }
        Ok(s)
    }

    /// The verdict that settles the pair of key `key`, if any.
    pub fn settled(&self, key: &Hash) -> Option<&Arc<Settled>> {
        self.settled.get(key)
    }

    /// The settled pairs whose key's hex text starts with `hex` (6 to 64 lowercase hex digits;
    /// anything else matches nothing), sorted by key.
    pub fn settled_with_prefix(&self, hex: &str) -> Vec<(Hash, Arc<Settled>)> {
        if !(6..=64).contains(&hex.len())
            || !hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
        {
            return vec![];
        }
        // lowercase hex sorts as the bytes it encodes: the keys with this prefix are one range
        let bound = |pad: char| -> Option<Hash> {
            let mut text = hex.to_string();
            text.extend(std::iter::repeat_n(pad, 64 - hex.len()));
            text.parse().ok()
        };
        let (Some(lo), Some(hi)) = (bound('0'), bound('f')) else {
            return vec![];
        };
        self.settled
            .range(lo..=hi)
            .map(|(k, v)| (*k, Arc::clone(v)))
            .collect()
    }

    /// The semantic tension of the pair of key `key`, if any.
    pub fn tension(&self, key: &Hash) -> Option<&Arc<OpenTension>> {
        self.tensions.get(key)
    }

    /// Every semantic tension, by key.
    pub fn tensions(&self) -> impl Iterator<Item = &Arc<OpenTension>> {
        self.tensions.values()
    }

    /// The semantic tensions `item` takes part in, by the other item (see [`Partners`]).
    pub fn partners(&self, item: &PairItem) -> Option<&Partners> {
        self.by_item.get(item)
    }

    /// Whether block `b` takes part in a semantic tension, at any content.
    pub fn has_block(&self, b: &BlockId) -> bool {
        self.blocks.contains_key(b)
    }

    /// The blocks that take part in a semantic tension, at any content, by id.
    pub fn blocks(&self) -> impl Iterator<Item = &BlockId> {
        self.blocks.keys()
    }

    /// Whether any semantic tension exists.
    pub fn has_tensions(&self) -> bool {
        !self.tensions.is_empty()
    }

    fn settle(&mut self, key: Hash, review: Hash, r: Review) -> Result<(), RdyError> {
        let ReviewSubject::Pair { items, .. } = r.subject else {
            return Err(StoreError::Decode(format!(
                "verdict ref {PAIR_REF}{key} names review {review}, which is not a review of a pair"
            ))
            .into());
        };
        self.settled.insert(
            key,
            Arc::new(Settled {
                review,
                verdict: r.verdict,
                items,
                actor: r.provenance.actor,
            }),
        );
        Ok(())
    }

    fn open(&mut self, key: Hash, version: Hash, record: SemanticTension) {
        if let [a, b] = record.items.as_slice() {
            for (x, y) in [(a, b), (b, a)] {
                self.by_item
                    .entry(x.clone())
                    .or_default()
                    .entry(y.block.clone())
                    .or_default()
                    .insert(y.content, key);
            }
        }
        for item in &record.items {
            *self.blocks.entry(item.block.clone()).or_default() += 1;
        }
        self.tensions
            .insert(key, Arc::new(OpenTension { version, record }));
    }

    /// Drops the semantic tension of key `key` and its index entries.
    fn close(&mut self, key: &Hash) {
        let Some(old) = self.tensions.remove(key) else {
            return;
        };
        if let [a, b] = old.record.items.as_slice() {
            for (x, y) in [(a, b), (b, a)] {
                let Some(partners) = self.by_item.get_mut(x) else {
                    continue;
                };
                if let Some(by_content) = partners.get_mut(&y.block) {
                    by_content.remove(&y.content);
                    if by_content.is_empty() {
                        partners.remove(&y.block);
                    }
                }
                if partners.is_empty() {
                    self.by_item.remove(x);
                }
            }
        }
        for item in &old.record.items {
            if let Some(n) = self.blocks.get_mut(&item.block) {
                *n -= 1;
                if *n == 0 {
                    self.blocks.remove(&item.block);
                }
            }
        }
    }
}

/// The hash ref `<prefix><key>` holds, if the ref exists.
fn ref_hash(
    reader: &StoreReader,
    prefix: &str,
    key: &Hash,
    what: &str,
) -> Result<Option<Hash>, RdyError> {
    let Some(bytes) = reader.get_ref(&format!("{prefix}{key}"))? else {
        return Ok(None);
    };
    let h = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        StoreError::Decode(format!(
            "{what} is {} bytes, expected a 32-byte hash",
            bytes.len()
        ))
    })?;
    Ok(Some(Hash(h)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::{Provenance, Role};

    fn settled() -> Arc<Settled> {
        Arc::new(Settled {
            review: Hash([1; 32]),
            verdict: Verdict::NoTension,
            items: vec![],
            actor: "agent:r".into(),
        })
    }

    fn tension(key: Hash, a: &BlockId, b: &BlockId) -> SemanticTension {
        SemanticTension {
            key,
            items: vec![
                PairItem {
                    block: a.clone(),
                    content: Hash([2; 32]),
                },
                PairItem {
                    block: b.clone(),
                    content: Hash([3; 32]),
                },
            ],
            review: Hash([1; 32]),
            advice: None,
            blocking: false,
            relay: None,
            relayed_by: None,
            prev: None,
            provenance: Provenance {
                actor: "agent:r".into(),
                run: None,
                role: Role::Reviewer,
                at_ms: 1,
            },
        }
    }

    /// guard: a prefix is 6 to 64 lowercase hex digits and matches only the keys it starts.
    #[test]
    fn se3_settled_with_prefix_takes_6_to_64_lowercase_hex() {
        let mut s = SemanticState::default();
        let (k1, k2) = (Hash([0x12; 32]), Hash([0x13; 32]));
        s.settled.insert(k1, settled());
        s.settled.insert(k2, settled());
        let keys = |hex: &str| -> Vec<Hash> {
            s.settled_with_prefix(hex)
                .into_iter()
                .map(|(k, _)| k)
                .collect()
        };
        assert_eq!(keys("121212"), vec![k1]);
        assert_eq!(keys(&k2.to_string()), vec![k2]);
        assert!(keys("12121").is_empty());
        assert!(keys("1212AB").is_empty());
        assert!(keys(&format!("{k1}0")).is_empty());
        assert!(keys("14ffff").is_empty());
    }

    /// guard: a key opened again with other items is indexed under its new blocks only.
    #[test]
    fn se3_a_reopened_key_moves_its_block_index() {
        let (a, b, c) = (
            BlockId::new("blk"),
            BlockId::new("blk"),
            BlockId::new("blk"),
        );
        let key = Hash([7; 32]);
        let item = |b: &BlockId, n: u8| PairItem {
            block: b.clone(),
            content: Hash([n; 32]),
        };
        let mut s = SemanticState::default();
        s.open(key, Hash([8; 32]), tension(key, &a, &b));
        assert_eq!(s.partners(&item(&b, 3)).unwrap()[&a][&Hash([2; 32])], key);
        assert!(s.has_block(&b));
        s.close(&key);
        s.open(key, Hash([9; 32]), tension(key, &a, &c));
        assert!(s.partners(&item(&b, 3)).is_none() && !s.has_block(&b));
        assert_eq!(s.partners(&item(&a, 2)).unwrap()[&c][&Hash([3; 32])], key);
        assert_eq!(s.blocks().count(), 2);
        s.close(&key);
        assert!(!s.has_tensions() && s.by_item.is_empty() && s.blocks.is_empty());
    }
}
