//! Hash and encoding helpers that reuse core's own functions.

use postcard::ser_flavors::Flavor;
use rdy_engine::{E_INTERNAL, E_LIMIT, RdyError};
use rdy_model::redaction::MAX_RECORD_BYTES;
use rdy_model::{Block, Hash};

/// The hash of `bytes`, as core hashes every stored object (`Hash::of`).
pub fn hash_bytes(bytes: &[u8]) -> Hash {
    Hash::of(bytes)
}

/// The existing postcard encoding of a block, the bytes its `version_hash` hashes. At most
/// `MAX_RECORD_BYTES` (16,777,216): the output never grows past that. A redacted read projection
/// is refused with `E_REDACTED`: its text is gone, and its version is not the hash of any bytes
/// this could produce. Map entries encode in key order, as core encodes them.
pub fn encode_block(block: &Block) -> Result<Vec<u8>, RdyError> {
    if let Some(request) = block.redaction() {
        return Err(rdy_store::StoreError::Redacted { request }.into());
    }
    match postcard::serialize_with_flavor(block, Capped(Vec::new())) {
        Ok(bytes) => Ok(bytes),
        Err(postcard::Error::SerializeBufferFull) => Err(RdyError::new(
            E_LIMIT,
            format!("block encoding exceeds {MAX_RECORD_BYTES} bytes"),
            "reduce the block to the stated limit",
        )),
        Err(e) => Err(RdyError::new(
            E_INTERNAL,
            format!("a block did not encode: {e}"),
            "report this as a bug in rdy",
        )),
    }
}

/// A postcard output that refuses to grow past `MAX_RECORD_BYTES` and reserves fallibly.
struct Capped(Vec<u8>);

impl Flavor for Capped {
    type Output = Vec<u8>;

    fn try_push(&mut self, data: u8) -> postcard::Result<()> {
        self.try_extend(&[data])
    }

    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        if self.0.len().saturating_add(data.len()) > MAX_RECORD_BYTES {
            return Err(postcard::Error::SerializeBufferFull);
        }
        self.0
            .try_reserve(data.len())
            .map_err(|_| postcard::Error::SerializeBufferFull)?;
        self.0.extend_from_slice(data);
        Ok(())
    }

    fn finalize(self) -> postcard::Result<Vec<u8>> {
        Ok(self.0)
    }
}

#[cfg(test)]
mod tests {
    use crate::StoreHandle;
    use rdy_engine::Rdy;

    /// Sibling modules read a snapshot through its crate-private accessors, never its fields.
    #[test]
    fn ha3_sibling_modules_use_snapshot_accessors() {
        let dir = tempfile::tempdir().unwrap();
        Rdy::init(dir.path(), "rdy-standard")
            .unwrap()
            .close()
            .unwrap();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        assert_eq!(snap.read_state().canon_head(), snap.canon());
        assert!(snap.captured_tags().is_empty());
        assert_eq!(snap.captured_tag("1.0.0").unwrap_err().code, "E_NOT_FOUND");
        assert_eq!(
            snap.captured_tag("not a version").unwrap_err().code,
            "E_BAD_TAG_NAME"
        );
    }
}
