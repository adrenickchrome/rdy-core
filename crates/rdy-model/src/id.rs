use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::Cell;
use std::fmt;

thread_local! {
    /// splitmix64 state for test-seeded ids on this thread; `None`: ids are random.
    static SEEDED: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Tests only: make `BlockId::new` on this thread draw from a deterministic generator seeded
/// with `seed`, so a test's ids replay exactly. Undo with [`unseed_ids_for_tests`].
#[doc(hidden)]
pub fn seed_ids_for_tests(seed: u64) {
    SEEDED.with(|s| s.set(Some(seed)));
}

/// Tests only: return this thread's `BlockId::new` to random ids.
#[doc(hidden)]
pub fn unseed_ids_for_tests() {
    SEEDED.with(|s| s.set(None));
}

/// A digit in `0..32`: from the seeded splitmix64 stream if this thread has one, else random.
fn next_digit() -> usize {
    SEEDED.with(|s| match s.get() {
        None => rand::random_range(0..32),
        Some(state) => {
            let state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            s.set(Some(state));
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 59) as usize
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
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(String);

impl BlockId {
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

impl Serialize for BlockId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BlockId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        BlockId::parse(&s).map_err(serde::de::Error::custom)
    }
}
