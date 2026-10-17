use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::Cell;
use std::fmt;

thread_local! {
    /// splitmix64 state for test-seeded ids on this thread; `None`: ids are random.
    static SEEDED: Cell<Option<u64>> = const { Cell::new(None) };
    /// The seeded thread's own splitmix64 state for `random_bytes` (repo ids), apart from
    /// `SEEDED` so a repo id never moves the `BlockId` stream (O19); `None`: random bytes.
    static SEEDED_BYTES: Cell<Option<u64>> = const { Cell::new(None) };
}

/// What `seed_ids_for_tests` mixes into its seed for the `random_bytes` stream ("REPOID_1").
const BYTES_SEED_SALT: u64 = 0x5245_504f_4944_5f31;

/// Tests only: make `BlockId::new` on this thread draw from a deterministic generator seeded
/// with `seed`, so a test's ids replay exactly. Undo with [`unseed_ids_for_tests`].
#[doc(hidden)]
pub fn seed_ids_for_tests(seed: u64) {
    SEEDED.with(|s| s.set(Some(seed)));
    SEEDED_BYTES.with(|s| s.set(Some(seed ^ BYTES_SEED_SALT)));
}

/// Tests only: return this thread's `BlockId::new` to random ids.
#[doc(hidden)]
pub fn unseed_ids_for_tests() {
    SEEDED.with(|s| s.set(None));
    SEEDED_BYTES.with(|s| s.set(None));
}

/// One step of splitmix64: moves `state` and returns the step's output.
fn splitmix64(state: &Cell<Option<u64>>, seed: u64) -> u64 {
    let next = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    state.set(Some(next));
    let mut z = next;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A digit in `0..32`: from the seeded splitmix64 stream if this thread has one, else random.
fn next_digit() -> usize {
    SEEDED.with(|s| match s.get() {
        None => rand::random_range(0..32),
        Some(state) => (splitmix64(s, state) >> 59) as usize,
    })
}

/// Fills `buf` with random bytes. On a seeded thread they come from the thread's own byte
/// stream, eight bytes per step, which never moves the `BlockId` stream (O19).
pub(crate) fn random_bytes(buf: &mut [u8]) {
    SEEDED_BYTES.with(|s| match s.get() {
        None => rand::fill(buf),
        Some(_) => {
            for chunk in buf.chunks_mut(8) {
                let state = s.get().expect("a seeded thread");
                let z = splitmix64(s, state).to_le_bytes();
                chunk.copy_from_slice(&z[..chunk.len()]);
            }
        }
    })
}

pub const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum IdError {
    #[error("bad id prefix in {0:?}: expected 1-3 ASCII letters then '-'")]
    BadPrefix(String),
    #[error("bad id body in {0:?}: expected 5 Crockford base32 characters")]
    BadBody(String),
    #[error("bad hash {0:?}: expected 64 hex characters")]
    BadHash(String),
    #[error("bad repo id {0:?}: expected 32 hex digits")]
    BadRepoId(String),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(String);

impl BlockId {
    /// The least id in order (it is not a valid id): the lower bound of a range read of a map
    /// keyed by (BlockId, ...).
    pub const MIN: BlockId = BlockId(String::new());

    pub fn new(prefix: &str) -> BlockId {
        assert!(valid_prefix(prefix), "invalid id prefix {prefix:?}");
        let body: String = (0..5).map(|_| CROCKFORD[next_digit()] as char).collect();
        BlockId(format!("{prefix}-{body}"))
    }

    pub fn parse(s: &str) -> Result<BlockId, IdError> {
        let (prefix, body) = s
            .split_once('-')
            .ok_or_else(|| IdError::BadPrefix(s.to_string()))?;
        if !valid_prefix(prefix) {
            return Err(IdError::BadPrefix(s.to_string()));
        }
        if body.len() != 5 || !body.bytes().all(|b| CROCKFORD.contains(&b)) {
            return Err(IdError::BadBody(s.to_string()));
        }
        Ok(BlockId(s.to_string()))
    }

    pub fn prefix(&self) -> &str {
        self.0.split_once('-').map(|(p, _)| p).unwrap_or("")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for BlockId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<BlockId, IdError> {
        BlockId::parse(s)
    }
}

fn valid_prefix(p: &str) -> bool {
    (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphabetic())
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A block id serializes as a newtype struct named `BlockId` around its string. Postcard, JSON
/// and YAML read through a newtype struct, so the encoded bytes are the plain string's; a typed
/// visitor can still tell an id from text by the name.
impl Serialize for BlockId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_newtype_struct(BLOCK_ID_NAME, self.0.as_str())
    }
}

/// The newtype name a block id serializes under.
const BLOCK_ID_NAME: &str = "BlockId";

impl<'de> Deserialize<'de> for BlockId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        BlockId::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A block id carries a type marker a typed visitor can see (`serialize_newtype_struct`), and
    /// postcard and YAML read through it: every stored byte and hash stays the plain string's.
    #[test]
    fn rdx5_a_block_id_encodes_as_its_plain_string() {
        let id = BlockId::parse("PIN-7Q2ZZ").unwrap();
        let s = "PIN-7Q2ZZ".to_string();
        let typed = (
            id.clone(),
            Some(id.clone()),
            vec![id.clone(), id.clone()],
            BTreeMap::from([(id.clone(), 1u8)]),
        );
        let plain = (
            s.clone(),
            Some(s.clone()),
            vec![s.clone(), s.clone()],
            BTreeMap::from([(s.clone(), 1u8)]),
        );
        assert_eq!(
            postcard::to_allocvec(&typed).unwrap(),
            postcard::to_allocvec(&plain).unwrap()
        );
        assert_eq!(
            postcard::to_allocvec(&id).unwrap(),
            [&[9u8][..], b"PIN-7Q2ZZ"].concat()
        );
        assert_eq!(
            serde_yaml_ng::to_string(&typed).unwrap(),
            serde_yaml_ng::to_string(&plain).unwrap()
        );
        let back: BlockId = postcard::from_bytes(&postcard::to_allocvec(&id).unwrap()).unwrap();
        assert_eq!(back, id);
    }
}
