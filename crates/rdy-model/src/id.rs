use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

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
        let body: String = (0..5)
            .map(|_| CROCKFORD[rand::random_range(0..32)] as char)
            .collect();
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
