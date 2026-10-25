//! Object encodings (D-WWAK7: "Each object records its encoding version"). A stored object is
//! its kind, its encoding version and its body. Version 0 is the original encoding: the stored
//! payload is the body alone, so every object written before versioned encodings keeps its
//! bytes and its hash. From version 1 on, the stored payload is `version` (two bytes, little
//! endian) followed by the body, and the object's hash covers both.
//!
//! An [`EncodedIdentity`] is metadata a reader keeps beside a decoded value: the hash and the
//! encoding version of the stored object it came from. It is never part of a postcard or JSON
//! encoding, so a historical body decoded into a newer type keeps its original identity.
use crate::{Block, BlockId, FieldValue, Hash, Link};
use std::collections::BTreeMap;

/// The identity of a stored object: the hash of its stored payload and its encoding version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EncodedIdentity {
    pub hash: Hash,
    pub version: u16,
}

/// The hash of an object of encoding `version` whose body is `body`: `Hash::of(body)` for
/// version 0, and `Hash::of(version.to_le_bytes() || body)` from version 1 on, the hash of the
/// stored payload in both cases.
pub fn encoded_hash(version: u16, body: &[u8]) -> Hash {
    if version == 0 {
        return Hash::of(body);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&version.to_le_bytes());
    hasher.update(body);
    Hash(*hasher.finalize().as_bytes())
}

/// The content-1 hash of `block`: the domain `rdy-content-1\0` and the content encoding that
/// kept the order key, with the derived and engine fields left out and no tracking fields. It
/// serves only the proof check of an encoding-0 redaction residue (D-69AY3), which an rdy
/// before content-2 wrote with this hash of the intact block. No pin, tension, dismissal, plan
/// answer, pair key or premise ever calls it: those are content-2 only.
#[doc(hidden)]
pub fn legacy_content_1_hash(block: &Block) -> Hash {
    #[derive(serde::Serialize)]
    struct Content1<'a> {
        id: &'a BlockId,
        block_type: &'a String,
        parent: &'a Option<BlockId>,
        order: &'a String,
        fields: Vec<(&'a String, &'a FieldValue)>,
        body: &'a String,
        links: &'a BTreeMap<String, Vec<Link>>,
    }
    let content = Content1 {
        id: &block.id,
        block_type: &block.block_type,
        parent: &block.parent,
        order: &block.order,
        fields: block
            .fields
            .iter()
            .filter(|(k, _)| !crate::block::DERIVED_FIELDS.contains(&k.as_str()))
            .filter(|(k, _)| !crate::engine_fields::is_engine_field(k))
            .collect(),
        body: &block.body,
        links: &block.links,
    };
    let mut bytes = b"rdy-content-1\0".to_vec();
    bytes.extend(
        postcard::to_allocvec(&content)
            .expect("postcard encoding of an in-memory value cannot fail"),
    );
    Hash::of(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok1_encoded_hash_covers_the_version_prefix() {
        let body = b"a body";
        assert_eq!(encoded_hash(0, body), Hash::of(body));
        let mut framed = 1u16.to_le_bytes().to_vec();
        framed.extend_from_slice(body);
        assert_eq!(encoded_hash(1, body), Hash::of(&framed));
        assert_ne!(encoded_hash(1, body), encoded_hash(2, body));
        assert_ne!(encoded_hash(1, body), Hash::of(body));
    }
}
