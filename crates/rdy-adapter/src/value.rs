//! Hash and encoding helpers that reuse core's own functions.

use postcard::ser_flavors::Flavor;
use rdy_engine::{E_INTERNAL, E_LIMIT, RdyError};
use rdy_model::redaction::MAX_RECORD_BYTES;
use rdy_model::{Block, FieldValue, Hash};

/// How many lists and maps a field value may nest. The serializer recurses once per level, so a
/// hostile depth well below the byte limit would overflow the stack; YAML front matter, the only
/// source of field values, stops far short of this. Checked before the serializer runs.
const MAX_FIELD_DEPTH: usize = 128;

/// The hash of `bytes`, as core hashes every stored object (`Hash::of`).
pub fn hash_bytes(bytes: &[u8]) -> Hash {
    Hash::of(bytes)
}

/// The existing postcard encoding of a block, the bytes its `version_hash` hashes. At most
/// `MAX_RECORD_BYTES` (16,777,216): the output never grows past that. A redacted read projection
/// is refused with `E_REDACTED`: its text is gone, and its version is not the hash of any bytes
/// this could produce. Map entries encode in key order, as core encodes them. A field value that
/// nests more than `MAX_FIELD_DEPTH` (128) lists and maps is refused with `E_LIMIT` before any
/// recursion, and the check holds memory for the nesting depth only, never for the width.
pub fn encode_block(block: &Block) -> Result<Vec<u8>, RdyError> {
    if let Some(request) = block.redaction() {
        return Err(rdy_store::StoreError::Redacted { request }.into());
    }
    check_nesting(block, MAX_WALK_FRAMES).map_err(nesting_error)?;
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

/// The most walk frames the depth check ever holds: the block's fields plus one frame for each
/// of the `MAX_FIELD_DEPTH` levels a list or map may open.
const MAX_WALK_FRAMES: usize = MAX_FIELD_DEPTH + 1;

/// An iterator over the children of one block or container being walked.
enum Frame<'a> {
    Fields(std::collections::btree_map::Values<'a, String, FieldValue>),
    List(std::slice::Iter<'a, FieldValue>),
}

impl<'a> Iterator for Frame<'a> {
    type Item = &'a FieldValue;

    fn next(&mut self) -> Option<&'a FieldValue> {
        match self {
            Frame::Fields(it) => it.next(),
            Frame::List(it) => it.next(),
        }
    }
}

/// Why the nesting check stopped.
enum Nesting {
    TooDeep,
    NoMemory,
}

/// Maps `Nesting` to the refusal `encode_block` returns.
fn nesting_error(why: Nesting) -> RdyError {
    match why {
        Nesting::TooDeep => RdyError::new(
            E_LIMIT,
            format!("a field value nests deeper than {MAX_FIELD_DEPTH} levels"),
            "reduce the nesting of the field value to the stated limit",
        ),
        Nesting::NoMemory => RdyError::new(
            E_LIMIT,
            "the block could not be checked: out of memory".to_string(),
            "reduce the block to the stated limit",
        ),
    }
}

/// Checks that no field value nests past `MAX_FIELD_DEPTH`. Walks with an explicit stack of
/// iterator frames, one per open list or map, so the check never recurses and its memory is
/// bounded by the nesting depth, never by how many siblings a list or map holds. The stack is
/// reserved once, fallibly, for `frames` entries and never grown past that; a branch deeper than the bound is refused before
/// it is pushed, so `frames` of `MAX_WALK_FRAMES` is always enough.
fn check_nesting(block: &Block, frames: usize) -> Result<(), Nesting> {
    let mut stack: Vec<Frame<'_>> = Vec::new();
    stack
        .try_reserve_exact(frames)
        .map_err(|_| Nesting::NoMemory)?;
    stack.push(Frame::Fields(block.fields.values()));
    while let Some(top) = stack.last_mut() {
        let Some(value) = top.next() else {
            stack.pop();
            continue;
        };
        // The level of `value`: the top frame holds the children of the container at level
        // `stack.len() - 1`, so these children sit at level `stack.len()`.
        let frame = match value {
            FieldValue::List(items) => Frame::List(items.iter()),
            FieldValue::Map(map) => Frame::Fields(map.values()),
            _ => continue,
        };
        if stack.len() > MAX_FIELD_DEPTH {
            return Err(Nesting::TooDeep);
        }
        if stack.len() >= frames {
            return Err(Nesting::NoMemory);
        }
        stack.push(frame);
    }
    Ok(())
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
    use super::{MAX_WALK_FRAMES, Nesting, check_nesting};
    use crate::StoreHandle;
    use rdy_engine::Rdy;
    use rdy_model::{Block, FieldValue};

    fn block_with(value: FieldValue) -> Block {
        Block {
            id: rdy_model::BlockId::parse("d-00001").unwrap(),
            block_type: "doc".to_string(),
            parent: None,
            order: "a0".to_string(),
            fields: [("wide".to_string(), value)].into(),
            body: String::new(),
            links: Default::default(),
            pins: Default::default(),
            read_identity: None,
        }
    }

    /// The check holds frames for the nesting depth only: a list or map of any width needs two
    /// frames (the fields, then the container), so a width of a million costs no more than three.
    #[test]
    fn ha3_nesting_check_memory_follows_depth_not_width() {
        let list = FieldValue::List(vec![FieldValue::Null; 1_000_000]);
        let map = FieldValue::Map(
            (0..100_000)
                .map(|i| (format!("k{i:06}"), FieldValue::Int(i)))
                .collect(),
        );
        for wide in [list, map] {
            let b = block_with(wide);
            assert!(check_nesting(&b, 2).is_ok());
            assert!(check_nesting(&b, MAX_WALK_FRAMES).is_ok());
            // One frame holds the fields alone; opening the container needs a second.
            assert!(matches!(check_nesting(&b, 1), Err(Nesting::NoMemory)));
        }
    }

    /// A refused reservation is an error, never an abort.
    #[test]
    fn ha3_nesting_check_refuses_when_memory_is_refused() {
        let b = block_with(FieldValue::List(vec![FieldValue::Null]));
        assert!(matches!(
            check_nesting(&b, usize::MAX),
            Err(Nesting::NoMemory)
        ));
        let e = super::nesting_error(Nesting::NoMemory);
        assert_eq!(e.code, "E_LIMIT");
        assert_eq!(e.message, "the block could not be checked: out of memory");
    }

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
