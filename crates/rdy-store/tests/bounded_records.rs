//! Bounded record decoding (plan HA1 Task 1; D-D2NPN verify-on-receive, quality-gate tb-A4DAV):
//! outside bytes fail or stay bounded before any visitor allocates or recurses.
use rdy_model::{Hash, Provenance, Role, Tag};
use rdy_store::bounded::{
    self, COLLECTION_ELEMENTS, DECODED_VALUES, NESTING, RECORD_BYTES, RecordError, TEXT_BYTES,
};
use rdy_store::kinds::{BLOB_CHUNK, SIGNATURE, SNAPSHOT, TAG};
use rdy_store::object_refs::refs_of;
use serde::Deserialize;
use std::collections::BTreeMap;

/// A global allocator that counts the bytes this thread asks for.
mod counting_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    thread_local! {
        /// Bytes this thread allocated (and grew to) since the last reset.
        static BYTES: Cell<usize> = const { Cell::new(0) };
    }

    fn count(n: usize) {
        let _ = BYTES.try_with(|b| b.set(b.get().saturating_add(n)));
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count(layout.size());
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count(new_size);
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// `f`'s result and the bytes this thread allocated while it ran.
    pub fn measured<T>(f: impl FnOnce() -> T) -> (T, usize) {
        BYTES.with(|b| b.set(0));
        let out = f();
        (out, BYTES.with(|b| b.get()))
    }
}

#[global_allocator]
static ALLOC: counting_alloc::Counting = counting_alloc::Counting;

use counting_alloc::measured;

/// postcard's varint of `n`.
fn varint(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn limit(name: &'static str, maximum: u64) -> Result<(), RecordError> {
    Err(RecordError::Limit { name, maximum })
}

/// `r` with its value dropped, so results of different types compare to one error.
fn unit<T>(r: Result<T, RecordError>) -> Result<(), RecordError> {
    r.map(|_| ())
}

fn tag() -> Tag {
    Tag {
        name: "v1.0.0".into(),
        landing: Hash([4; 32]),
        message: Some("first".into()),
        tagged_by: Provenance {
            actor: "human:nick".into(),
            run: None,
            role: Role::Approver,
            at_ms: 7,
        },
    }
}

#[test]
fn ha1_lengths_are_refused_before_visitors_allocate() {
    // A sequence length hint far over the element bound: refused before Vec's visitor sees it.
    let hostile = varint(u64::from(u32::MAX));
    let (r, bytes) = measured(|| unit(bounded::decode::<Vec<Hash>>(&hostile)));
    assert_eq!(r, limit(COLLECTION_ELEMENTS, 100_000));
    assert!(bytes < 4096, "a refused hint allocated {bytes} bytes");

    // A hint within the bound but over the bytes left: the visitor never preallocates for it.
    // (Unclamped, Vec<Hash> would reserve its cautious 1 MiB before failing.)
    let mut short = varint(100_000);
    short.extend([7u8; 32]);
    let (r, bytes) = measured(|| unit(bounded::decode::<Vec<Hash>>(&short)));
    assert_eq!(r, Err(RecordError::Decode));
    assert!(bytes < 16 * 1024, "a short hint allocated {bytes} bytes");

    // A map hint far over the bound.
    let hostile = varint(1 << 40);
    let (r, bytes) = measured(|| unit(bounded::decode::<BTreeMap<String, u8>>(&hostile)));
    assert_eq!(r, limit(COLLECTION_ELEMENTS, 100_000));
    assert!(bytes < 4096, "a refused map hint allocated {bytes} bytes");

    // A text length far over the bound, with no bytes behind it: Limit, not a truncation.
    let hostile = varint(1 << 30);
    let (r, bytes) = measured(|| unit(bounded::decode::<String>(&hostile)));
    assert_eq!(r, limit(TEXT_BYTES, 1_048_576));
    assert!(
        bytes < 4096,
        "a refused text length allocated {bytes} bytes"
    );

    // A complete text one byte over the bound: refused before a String owns it.
    let mut over = varint(1_048_577);
    over.extend(vec![b'a'; 1_048_577]);
    let (r, bytes) = measured(|| unit(bounded::decode::<String>(&over)));
    assert_eq!(r, limit(TEXT_BYTES, 1_048_576));
    assert!(bytes < 1_048_576, "a refused text allocated {bytes} bytes");
    let mut exact = varint(1_048_576);
    exact.extend(vec![b'a'; 1_048_576]);
    assert_eq!(
        bounded::decode::<String>(&exact).map(|s| s.len()),
        Ok(1_048_576)
    );
    // Byte strings take the same bound.
    assert_eq!(
        unit(bounded::decode::<serde_bytes_like::Bytes>(&over)),
        limit(TEXT_BYTES, 1_048_576)
    );

    // The element bound is inclusive.
    let mut at = varint(100_000);
    at.extend(vec![1u8; 100_000]);
    assert_eq!(
        bounded::decode::<Vec<u8>>(&at).map(|v| v.len()),
        Ok(100_000)
    );
    let mut past = varint(100_001);
    past.extend(vec![1u8; 100_001]);
    assert_eq!(
        unit(bounded::decode::<Vec<u8>>(&past)),
        limit(COLLECTION_ELEMENTS, 100_000)
    );

    // Through refs_of: a snapshot's map and a signature's statement bytes.
    let (r, bytes) = measured(|| unit(refs_of(SNAPSHOT, &varint(1 << 40))));
    assert_eq!(r, limit(COLLECTION_ELEMENTS, 100_000));
    assert!(bytes < 4096, "a refused snapshot allocated {bytes} bytes");
    let mut sig = vec![9u8; 32];
    sig.extend(varint(9));
    sig.extend(b"rdy-sig-1");
    sig.extend(varint(1 << 50));
    let (r, bytes) = measured(|| unit(refs_of(SIGNATURE, &sig)));
    assert_eq!(r, limit(COLLECTION_ELEMENTS, 100_000));
    assert!(bytes < 4096, "a refused signature allocated {bytes} bytes");
}

/// A value that only nests: each level is one sequence holding the next level or nothing.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct Chain(#[allow(dead_code)] Vec<Chain>);

/// The encoding of a chain `levels` sequences deep.
fn chain(levels: usize) -> Vec<u8> {
    let mut out = vec![1u8; levels - 1];
    out.push(0);
    out
}

/// A block whose one field nests `n` lists deep around a null.
fn nested_field_block(n: usize) -> Vec<u8> {
    use rdy_model::{Block, BlockId, FieldValue};
    let mut v = FieldValue::Null;
    for _ in 0..n {
        v = FieldValue::List(vec![v]);
    }
    let b = Block {
        id: BlockId::parse("c-00001").unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([("deep".to_string(), v)]),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    postcard::to_allocvec(&b).unwrap()
}

#[test]
fn ha1_depth_and_total_values_are_bounded() {
    // 64 nested containers decode; the 65th is refused before the decoder recurses into it.
    assert!(bounded::decode::<Chain>(&chain(64)).is_ok());
    assert_eq!(
        unit(bounded::decode::<Chain>(&chain(65))),
        limit(NESTING, 64)
    );
    // A hostile depth is refused without exhausting the stack.
    assert_eq!(
        unit(bounded::decode::<Chain>(&chain(100_000))),
        limit(NESTING, 64)
    );
    // In a stored record: a block holds its field 3 containers deep (struct, map, enum) and
    // each list adds two (the list and its element's enum), so 30 lists end at depth 63 and 31
    // at depth 65.
    assert_eq!(
        refs_of(rdy_store::kinds::BLOCK, &nested_field_block(30)),
        Ok(vec![])
    );
    assert_eq!(
        unit(refs_of(rdy_store::kinds::BLOCK, &nested_field_block(31))),
        limit(NESTING, 64)
    );

    // Values: one outer sequence, 10 inner ones and their absent options, 1,000,000 in all.
    let values = |last: usize| {
        let mut out = varint(10);
        for _ in 0..9 {
            out.extend(varint(100_000));
            out.extend(vec![0u8; 100_000]);
        }
        out.extend(varint(last as u64));
        out.extend(vec![0u8; last]);
        out
    };
    let at = values(99_989);
    let decoded = bounded::decode::<Vec<Vec<Option<u8>>>>(&at).unwrap();
    assert_eq!(
        1 + decoded.len() + decoded.iter().map(Vec::len).sum::<usize>(),
        1_000_000
    );
    assert_eq!(
        unit(bounded::decode::<Vec<Vec<Option<u8>>>>(&values(99_990))),
        limit(DECODED_VALUES, 1_000_000)
    );
}

/// The block id of entry `i`: prefix `c` and five Crockford digits.
fn entry_id(i: usize) -> rdy_model::BlockId {
    let body: String = (0..5)
        .rev()
        .map(|d| rdy_model::CROCKFORD[(i >> (5 * d)) & 31] as char)
        .collect();
    rdy_model::BlockId::parse(&format!("c-{body}")).unwrap()
}

/// A hash unique to `i`.
fn entry_hash(i: usize) -> Hash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&(i as u64).to_be_bytes());
    Hash(h)
}

#[test]
fn ha1_values_count_containers_and_leaves_not_scalars() {
    // A hash is 2 values (its newtype and its tuple); its 32 bytes are scalars and do not count.
    // One outer sequence, 11 inner ones and 11 x 45,454 hashes: 1 + 11 + 999,988 = 1,000,000.
    let hashes = |last: usize| {
        let mut out = varint(11);
        for i in 0..11 {
            let n = if i == 10 { last } else { 45_454 };
            out.extend(varint(n as u64));
            out.extend(vec![7u8; 32 * n]);
        }
        out
    };
    assert_eq!(
        bounded::decode::<Vec<Vec<Hash>>>(&hashes(45_454))
            .map(|v| v.iter().map(Vec::len).sum::<usize>()),
        Ok(499_994)
    );
    assert_eq!(
        unit(bounded::decode::<Vec<Vec<Hash>>>(&hashes(45_455))),
        limit(DECODED_VALUES, 1_000_000)
    );

    // A snapshot of 100,000 blocks (the element bound) is 2 + 3 x 100,000 = 300,002 values,
    // within the values bound. Counting scalars too would make it about 3,500,000.
    let snapshot = rdy_model::StateSnapshot {
        blocks: (0..100_000).map(|i| (entry_id(i), entry_hash(i))).collect(),
    };
    assert_eq!(snapshot.blocks.len(), 100_000);
    let bytes = postcard::to_allocvec(&snapshot).unwrap();
    assert_eq!(
        bounded::decode::<rdy_model::StateSnapshot>(&bytes).as_ref(),
        Ok(&snapshot)
    );
    assert_eq!(refs_of(SNAPSHOT, &bytes).map(|r| r.len()), Ok(100_000));
}

#[test]
fn ha1_decode_requires_one_complete_record() {
    let full = postcard::to_allocvec(&tag()).unwrap();
    assert_eq!(bounded::decode::<Tag>(&full), Ok(tag()));
    assert_eq!(refs_of(TAG, &full).map(|r| r.len()), Ok(1));
    for cut in 0..full.len() {
        assert_eq!(
            unit(bounded::decode::<Tag>(&full[..cut])),
            Err(RecordError::Decode),
            "a tag cut at {cut} bytes"
        );
        assert_eq!(unit(refs_of(TAG, &full[..cut])), Err(RecordError::Decode));
    }
    let mut trailing = full.clone();
    trailing.push(0);
    assert_eq!(
        unit(bounded::decode::<Tag>(&trailing)),
        Err(RecordError::Decode)
    );
    assert_eq!(unit(refs_of(TAG, &trailing)), Err(RecordError::Decode));

    // An invalid enum discriminant, a malformed varint, invalid UTF-8 and a bad option tag.
    assert_eq!(
        unit(bounded::decode::<Role>(&[9])),
        Err(RecordError::Decode)
    );
    assert_eq!(
        unit(bounded::decode::<u64>(&[0xff; 11])),
        Err(RecordError::Decode)
    );
    assert_eq!(
        unit(bounded::decode::<String>(&[0x80; 11])),
        Err(RecordError::Decode)
    );
    assert_eq!(
        unit(bounded::decode::<String>(&[2, 0xc3, 0x28])),
        Err(RecordError::Decode)
    );
    assert_eq!(
        unit(bounded::decode::<Option<u8>>(&[2])),
        Err(RecordError::Decode)
    );

    // The record bound is checked first: a valid record padded past it is Limit, not Decode.
    let mut big = full.clone();
    big.resize(16_777_217, 0);
    assert_eq!(
        unit(bounded::decode::<Tag>(&big)),
        limit(RECORD_BYTES, 16_777_216)
    );
    assert_eq!(unit(refs_of(TAG, &big)), limit(RECORD_BYTES, 16_777_216));

    // A raw chunk has no typed decode, only its own bound.
    assert_eq!(refs_of(BLOB_CHUNK, &vec![0xab; 1_048_576]), Ok(vec![]));
    assert_eq!(
        unit(refs_of(BLOB_CHUNK, &vec![0xab; 1_048_577])),
        limit("chunk bytes", 1_048_576)
    );
}

/// A byte string read through serde's bytes path (`deserialize_byte_buf`), not a sequence.
mod serde_bytes_like {
    use serde::de::{Deserialize, Deserializer, Visitor};
    use std::fmt;

    pub struct Bytes;

    impl<'de> Deserialize<'de> for Bytes {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Bytes, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Bytes;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("bytes")
                }
                fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Bytes, E> {
                    let _owned = v.to_vec();
                    Ok(Bytes)
                }
            }
            d.deserialize_byte_buf(V)
        }
    }
}

#[test]
fn ha1_stream_errors_use_the_exact_texts() {
    use rdy_store::object_refs::ObjectKey;
    use rdy_store::stream_error::{LIMIT_TOTAL_BYTES, Reason, StreamError};
    let key = ObjectKey {
        kind: 4,
        hash: Hash([0xab; 32]),
    };
    let hex = "ab".repeat(32);
    let text = |e: &StreamError| (e.code, e.message.clone(), e.fix.clone(), e.record, e.offset);

    let bad = StreamError::bad_record(3, 120, Reason::HashMismatch).with_object(Some(key));
    assert_eq!(
        text(&bad),
        (
            "E_BAD_RECORD",
            "stream record 3 at byte 120: hash mismatch".into(),
            "request this stream again; run rdy fsck at its source".into(),
            Some(3),
            120
        )
    );
    assert_eq!(bad.object, Some(key));
    let missing = StreamError::unresolved(2, 64, key);
    assert_eq!(
        text(&missing),
        (
            "E_UNRESOLVED_REF",
            format!("stream record 2 needs 4:{hex}"),
            "send the missing object with its dependencies".into(),
            Some(2),
            64
        )
    );
    assert_eq!(
        text(&StreamError::truncated(Some(9), 900)),
        (
            "E_STREAM_TRUNCATED",
            "stream trailer is missing or does not match".into(),
            "request the complete stream again".into(),
            Some(9),
            900
        )
    );
    assert_eq!(
        text(&StreamError::purpose(Some(0), 0, 7)),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 7 does not match tag_history".into(),
            "request a tag_history stream".into(),
            Some(0),
            0
        )
    );
    assert_eq!(
        text(&StreamError::store_format(Some(0), 0, 14, 15)),
        (
            "E_STORE_FORMAT",
            "stream store format 14 does not match 15".into(),
            "write the stream with the pinned rdy build".into(),
            Some(0),
            0
        )
    );
    assert_eq!(
        text(&StreamError::limit(None, 10, LIMIT_TOTAL_BYTES)),
        (
            "E_LIMIT",
            "stream exceeds 2147483648 total bytes".into(),
            "reduce the stream to the stated limit".into(),
            None,
            10
        )
    );
    assert_eq!(
        text(&StreamError::allocation(Some(1), 5)),
        (
            "E_LIMIT",
            "stream allocation failed".into(),
            "retry with more available memory".into(),
            Some(1),
            5
        )
    );
    let io = std::io::Error::other("disk gone");
    assert_eq!(
        text(&StreamError::io(Some(4), 70, &io)),
        (
            "E_IO",
            "stream input: disk gone".into(),
            "check that the stream file can be read".into(),
            Some(4),
            70
        )
    );

    // Record errors map to the stream table: unknown kind, does not decode, the record limit.
    let from = |e| text(&StreamError::from_record(5, 300, &e));
    assert_eq!(
        from(RecordError::Kind).1,
        "stream record 5 at byte 300: unknown kind"
    );
    assert_eq!(
        from(RecordError::Decode).1,
        "stream record 5 at byte 300: record does not decode"
    );
    assert_eq!(
        from(RecordError::Limit {
            name: TEXT_BYTES,
            maximum: 1_048_576
        }),
        (
            "E_LIMIT",
            "stream exceeds 1048576 text bytes".into(),
            "reduce the stream to the stated limit".into(),
            Some(5),
            300
        )
    );

    // Every reason, exactly.
    let reasons = [
        (Reason::UnknownKind, "unknown kind"),
        (Reason::HashMismatch, "hash mismatch"),
        (Reason::RecordDoesNotDecode, "record does not decode"),
        (Reason::TrailingBytes, "trailing bytes"),
        (Reason::InvalidHeader, "invalid header"),
        (Reason::InvalidDroppedRecord, "invalid dropped record"),
        (Reason::AmbiguousObjectKind, "ambiguous object kind"),
        (Reason::DependencyCycle, "dependency cycle"),
        (Reason::ObjectIsRepeated, "object is repeated"),
        (Reason::NoBatchFiles, "no batch files"),
        (Reason::BatchHeaderDiffers, "batch header differs"),
        (Reason::InputChangedDuringTake, "input changed during take"),
        (Reason::InvalidRemovalProof, "invalid removal proof"),
        (
            Reason::OutsideTagClosure,
            "object is outside the tag closure",
        ),
    ];
    for (r, s) in reasons {
        assert_eq!(r.as_str(), s);
    }
}
