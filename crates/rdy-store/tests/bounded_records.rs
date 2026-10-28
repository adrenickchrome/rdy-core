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
        text(&StreamError::store_format(
            Some(0),
            0,
            &rdy_store::StoreError::Format {
                message: "store feature x needs rdy 9.0.0".into(),
                fix: "open the project with rdy 9.0.0 or later".into(),
            }
        )),
        (
            "E_STORE_FORMAT",
            "store feature x needs rdy 9.0.0".into(),
            "open the project with rdy 9.0.0 or later".into(),
            Some(0),
            0
        )
    );
    assert_eq!(
        text(&StreamError::sync_format_one(8)),
        (
            "E_STORE_FORMAT",
            "sync batch format 1 is not supported".into(),
            "write the stream with the pinned rdy build".into(),
            Some(0),
            8
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
        (
            Reason::UnlistedDependency(rdy_store::object_refs::ObjectRef {
                kind: Some(4),
                hash: Hash([0xab; 32]),
            }),
            "unlisted dependency",
        ),
        (
            Reason::RootTagNotListed,
            "root tag is not in the batch or its prerequisites",
        ),
        (Reason::UnneededPrerequisite(key), "unneeded prerequisite"),
    ];
    for (r, s) in reasons {
        assert_eq!(r.as_str(), s);
    }
}

#[test]
fn plb2_parameterized_reason_display_is_exact() {
    use rdy_store::object_refs::{ObjectKey, ObjectRef};
    use rdy_store::stream_error::{Reason, StreamError};
    let hex = "ab".repeat(32);
    let hash = Hash([0xab; 32]);
    let typed = Reason::UnlistedDependency(ObjectRef {
        kind: Some(4),
        hash,
    });
    // an untyped edge that resolved to nothing: its hash alone, no kind is inferred
    let untyped = Reason::UnlistedDependency(ObjectRef { kind: None, hash });
    let unneeded = Reason::UnneededPrerequisite(ObjectKey { kind: 22, hash });

    // the labels are static; Display adds the object
    assert_eq!(typed.as_str(), "unlisted dependency");
    assert_eq!(untyped.as_str(), "unlisted dependency");
    assert_eq!(unneeded.as_str(), "unneeded prerequisite");
    assert_eq!(
        Reason::RootTagNotListed.as_str(),
        "root tag is not in the batch or its prerequisites"
    );
    assert_eq!(typed.to_string(), format!("unlisted dependency 4:{hex}"));
    assert_eq!(untyped.to_string(), format!("unlisted dependency {hex}"));
    assert_eq!(
        unneeded.to_string(),
        format!("unneeded prerequisite 22:{hex}")
    );
    assert_eq!(
        Reason::RootTagNotListed.to_string(),
        "root tag is not in the batch or its prerequisites"
    );
    // a reason without an object prints its label alone
    assert_eq!(Reason::DependencyCycle.to_string(), "dependency cycle");

    // the message and fix of the error stay the shared ones
    let e = StreamError::bad_record(2, 160, untyped);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_RECORD",
            format!("stream record 2 at byte 160: unlisted dependency {hex}").as_str(),
            "request this stream again; run rdy fsck at its source"
        )
    );
    let e = StreamError::bad_record(0, 12, unneeded);
    assert_eq!(
        e.message,
        format!("stream record 0 at byte 12: unneeded prerequisite 22:{hex}")
    );
}

/// A field value of `levels` levels whose deepest value is the costliest leaf for the decoder:
/// an empty list (its enum and its sequence) or an empty map.
fn field_of_levels(levels: usize, map: bool) -> rdy_model::FieldValue {
    use rdy_model::FieldValue;
    let mut v = if map {
        FieldValue::Map(BTreeMap::new())
    } else {
        FieldValue::List(vec![])
    };
    for _ in 1..levels {
        v = if map {
            FieldValue::Map(BTreeMap::from([("k".to_string(), v)]))
        } else {
            FieldValue::List(vec![v])
        };
    }
    v
}

/// The deepest record that embeds a block: a CHANGE whose op carries the full block, with one
/// field of `levels` levels; an update when `update`, else a create.
fn embedding_change(levels: usize, map: bool, update: bool) -> Vec<u8> {
    use rdy_model::{Block, BlockId, Change, Op};
    let block = Block {
        id: BlockId::parse("c-00001").unwrap(),
        block_type: "table".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([("rows".to_string(), field_of_levels(levels, map))]),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    assert!(!field_of_levels(levels, map).nests_deeper_than(levels));
    assert!(field_of_levels(levels, map).nests_deeper_than(levels - 1));
    let op = if update {
        Op::Update {
            base: Hash([1; 32]),
            block,
        }
    } else {
        Op::Create { block }
    };
    let change = Change {
        view: "work".into(),
        ops: vec![op],
        deps: vec![],
        provenance: Provenance {
            actor: "human:a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1,
        },
        justification: None,
        message: String::new(),
        links: Default::default(),
        read_identity: None,
    };
    // the change's frame payload in CHANGE's latest encoding (ok4: the version prefix first)
    rdy_store::codec::item(rdy_store::kinds::CHANGE, &change)
        .unwrap()
        .1
}

/// The raw kind of a change frame in CHANGE's latest encoding.
fn change_raw() -> u8 {
    let kind = rdy_store::kinds::CHANGE;
    rdy_store::codec::raw_kind(kind, rdy_store::codec::latest(kind))
}

#[test]
fn ha1_field_nesting_bound_fits_the_deepest_embedding_record() {
    use rdy_model::MAX_FIELD_NESTING;
    let change = change_raw();
    // the write-side bound is what the stream decoder's own counting allows in the deepest
    // record that embeds a block: at MAX_FIELD_NESTING levels it decodes, one more is refused
    for map in [false, true] {
        for update in [true, false] {
            let at = embedding_change(MAX_FIELD_NESTING, map, update);
            assert!(
                refs_of(change, &at).is_ok(),
                "map {map} update {update}: {:?}",
                refs_of(change, &at)
            );
        }
        let over = embedding_change(MAX_FIELD_NESTING + 1, map, true);
        assert_eq!(
            unit(refs_of(change, &over)),
            limit(NESTING, 64),
            "map {map}"
        );
    }
}

/// The plan prefix of a version of view `work` with no chain and no proposal, up to its answers'
/// length.
fn plan_prefix() -> Vec<u8> {
    let mut out = vec![0u8, 0u8];
    out.extend(varint(4));
    out.extend(b"work");
    out.push(0);
    out
}

fn plan_answer(n: u32) -> rdy_model::plan::PlanAnswer {
    let mut item = [0u8; 32];
    item[..4].copy_from_slice(&n.to_be_bytes());
    rdy_model::plan::PlanAnswer {
        item: Hash(item),
        act: rdy_model::plan::PlanAct::Done,
        reason: None,
        subject: None,
        cause: Hash([3; 32]),
        by: Provenance {
            actor: "a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 0,
        },
    }
}

fn plan_of(answers: Vec<rdy_model::plan::PlanAnswer>) -> rdy_model::plan::Plan {
    rdy_model::plan::Plan {
        stable: None,
        prev: None,
        view: "work".into(),
        proposal: None,
        answers,
        heads: vec![],
        provenance: Provenance {
            actor: "a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 0,
        },
        seal: None,
    }
}

#[test]
fn cp3_hostile_plan_hints_do_not_allocate_from_the_hint() {
    use rdy_store::kinds::PLAN;
    use rdy_store::plan::decode_plan;
    let decode = |b: &[u8]| unit(decode_plan(b));
    // truncated huge sequences: a hint over the bound is refused before any visitor sees it, and
    // one within it over the bytes left never preallocates for it
    for (what, hint) in [("answers", 1u64 << 40), ("answers", u64::from(u32::MAX))] {
        let mut b = plan_prefix();
        b.extend(varint(hint));
        let (r, bytes) = measured(|| decode(&b));
        assert_eq!(r, limit(COLLECTION_ELEMENTS, 100_000), "{what}");
        assert!(
            bytes < 4096,
            "{what}: a refused hint allocated {bytes} bytes"
        );
        let (r, bytes) = measured(|| unit(refs_of(PLAN, &b)));
        assert_eq!(
            r,
            limit(COLLECTION_ELEMENTS, 100_000),
            "{what} through refs_of"
        );
        assert!(bytes < 4096, "{what}: refs_of allocated {bytes} bytes");
    }
    let mut short = plan_prefix();
    short.extend(varint(100_000));
    short.extend([7u8; 40]);
    let (r, bytes) = measured(|| decode(&short));
    assert_eq!(r, Err(RecordError::Decode));
    assert!(bytes < 64 * 1024, "a short hint allocated {bytes} bytes");
    // heads after no answers, and a seal's items, take the same bound
    let mut heads = plan_prefix();
    heads.push(0);
    heads.extend(varint(1 << 35));
    assert_eq!(decode(&heads), limit(COLLECTION_ELEMENTS, 100_000));
    // deep values: within the element bound, more decoded values than a record holds
    let many = plan_of((0..100_000).map(plan_answer).collect());
    let bytes = postcard::to_allocvec(&many).unwrap();
    assert_eq!(decode(&bytes), limit(DECODED_VALUES, 1_000_000));
    let fits = plan_of((0..1000).map(plan_answer).collect());
    assert!(decode_plan(&postcard::to_allocvec(&fits).unwrap()).is_ok());
    // overlong text: the view, with or without its bytes behind it
    let mut text = vec![0u8, 0u8];
    text.extend(varint(1_048_577));
    let (r, bytes) = measured(|| decode(&text));
    assert_eq!(r, limit(TEXT_BYTES, 1_048_576));
    assert!(bytes < 4096, "a refused text allocated {bytes} bytes");
    text.extend(vec![b'v'; 1_048_577]);
    assert_eq!(decode(&text), limit(TEXT_BYTES, 1_048_576));
    // invalid enum arms: a plan act, a plan kind and an outcome past their last variants
    let one = plan_of(vec![plan_answer(1)]);
    let good = postcard::to_allocvec(&one).unwrap();
    let act = plan_prefix().len() + 1 + 32;
    assert_eq!(good[act], 2, "the act byte of Done");
    let mut bad = good.clone();
    // past Dismiss (3, ok4)
    bad[act] = 4;
    assert_eq!(decode(&bad), Err(RecordError::Decode));
    let item = rdy_model::plan::SealedItem {
        id: Hash([9; 32]),
        kind: rdy_model::plan::PlanKind::UpdateImplementation,
        subject: "s".into(),
        cause: rdy_model::BlockId::parse("D-00002").unwrap(),
        edge: "e".into(),
        outcome: rdy_model::plan::PlanOutcome::Open,
        answer: None,
    };
    let sealed = rdy_model::plan::Plan {
        proposal: Some(Hash([5; 32])),
        seal: Some(rdy_model::plan::PlanSeal {
            landing: Hash([6; 32]),
            version: Hash([5; 32]),
            decisions: vec![],
            items: vec![item.clone()],
        }),
        ..plan_of(vec![])
    };
    let good = postcard::to_allocvec(&sealed).unwrap();
    assert!(decode_plan(&good).is_ok());
    let sub = postcard::to_allocvec(&item).unwrap();
    let at = good
        .windows(sub.len())
        .position(|w| w == &sub[..])
        .expect("the item's bytes");
    let kind = at + 32;
    assert_eq!(good[kind], 4, "the kind byte of update_implementation");
    let outcome = at + sub.len() - 2;
    assert_eq!(good[outcome], 9, "the outcome byte of open");
    // past Tension (5) and Dismissed (10), ok4
    for (i, v) in [(kind, 6u8), (outcome, 11u8)] {
        let mut bad = good.clone();
        bad[i] = v;
        assert_eq!(decode(&bad), Err(RecordError::Decode), "byte {i} = {v}");
    }
    // trailing bytes, and a varint that is not its canonical form
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(decode(&trailing), Err(RecordError::Decode));
    let mut wide = good.clone();
    let len_at = 2;
    assert_eq!(wide[len_at], 4, "the view length");
    wide[len_at] = 0x84;
    wide.insert(len_at + 1, 0);
    assert_eq!(decode(&wide), Err(RecordError::Decode));
    // through the store: a hostile PLAN never reaches the log
    let dir = tempfile::tempdir().unwrap();
    let mut s = rdy_store::Store::create(&dir.path().join("s")).unwrap();
    let end = s.log_end();
    for b in [&short, &bad, &trailing] {
        assert!(s.write_guarded_for_tests(vec![(PLAN, b.to_vec())]).is_err());
        assert_eq!(s.log_end(), end);
    }
}
