//! The adapter's public contract: a consumer imports everything from `rdy_adapter` alone.

use rdy_adapter::{
    Block, BlockId, Hash, Identity, Origin, RdyError, RepoId, Snapshot, StoreHandle, Tag,
    encode_block, hash_bytes,
};
use std::collections::BTreeMap;
use std::path::Path;

/// An empty project on disk, closed again.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::Rdy::init(dir.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    dir
}

fn open(path: &Path) -> Result<StoreHandle, RdyError> {
    StoreHandle::open(path)
}

/// A block whose fields, links and pins are filled in a deliberately unsorted order.
fn block() -> Block {
    use rdy_model::{FieldValue, Link};
    let mut fields = BTreeMap::new();
    for key in ["zeta", "alpha", "mid"] {
        fields.insert(key.to_string(), FieldValue::Str(format!("{key} value")));
    }
    let mut inner = BTreeMap::new();
    inner.insert("z".to_string(), FieldValue::Int(-3));
    inner.insert(
        "a".to_string(),
        FieldValue::List(vec![FieldValue::Bool(true)]),
    );
    fields.insert("map".to_string(), FieldValue::Map(inner));
    let mut links = BTreeMap::new();
    for key in ["refs", "blocks"] {
        links.insert(
            key.to_string(),
            vec![Link {
                target: BlockId::parse("d-00001").unwrap(),
                part: Some("p".to_string()),
            }],
        );
    }
    let mut pins = BTreeMap::new();
    pins.insert(BlockId::parse("d-00003").unwrap(), hash_bytes(b"three"));
    pins.insert(BlockId::parse("d-00002").unwrap(), hash_bytes(b"two"));
    Block {
        id: BlockId::parse("d-00009").unwrap(),
        block_type: "doc".to_string(),
        parent: None,
        order: "a0".to_string(),
        fields,
        body: "A body.".to_string(),
        links,
        pins,
        read_identity: None,
    }
}

#[test]
fn ha3_a_consumer_needs_only_the_adapter_crate() {
    let dir = project();
    let handle: StoreHandle = open(dir.path()).unwrap();
    let snap: Snapshot = handle.snapshot().unwrap();
    let identity: &Identity = snap.identity();
    let _id: RepoId = identity.repo_id;
    let origin: &Option<Origin> = &identity.origin;
    assert!(origin.is_none());
    let canon: Option<Hash> = snap.canon();
    assert!(canon.is_none());
    let tags: Vec<(Hash, Tag)> = snap.tags().unwrap();
    assert!(tags.is_empty());
    let bytes: Vec<u8> = encode_block(&block()).unwrap();
    let h: Hash = hash_bytes(&bytes);
    assert_eq!(h, Hash::of(&bytes));
    let _e: Option<RdyError> = None;
    handle.close().unwrap();
}

#[test]
fn ha3_open_uses_the_existing_exclusive_lock() {
    let dir = project();
    let first = open(dir.path()).unwrap();
    let second = open(dir.path()).err().expect("a second open is refused");
    assert_eq!(second.code, "E_LOCKED", "{second}");
    drop(first);
}

#[test]
fn ha3_close_releases_the_lock() {
    let dir = project();
    open(dir.path()).unwrap().close().unwrap();
    open(dir.path()).unwrap().close().unwrap();
}

/// The bytes of [`block`] under the existing postcard block encoding (pinned).
const BLOCK_BYTES: &[u8] = &[
    7, 100, 45, 48, 48, 48, 48, 57, 3, 100, 111, 99, 0, 2, 97, 48, 4, 5, 97, 108, 112, 104, 97, 3,
    11, 97, 108, 112, 104, 97, 32, 118, 97, 108, 117, 101, 3, 109, 97, 112, 5, 2, 1, 97, 4, 1, 1,
    1, 1, 122, 2, 5, 3, 109, 105, 100, 3, 9, 109, 105, 100, 32, 118, 97, 108, 117, 101, 4, 122,
    101, 116, 97, 3, 10, 122, 101, 116, 97, 32, 118, 97, 108, 117, 101, 7, 65, 32, 98, 111, 100,
    121, 46, 2, 6, 98, 108, 111, 99, 107, 115, 1, 7, 100, 45, 48, 48, 48, 48, 49, 1, 1, 112, 4,
    114, 101, 102, 115, 1, 7, 100, 45, 48, 48, 48, 48, 49, 1, 1, 112, 2, 7, 100, 45, 48, 48, 48,
    48, 50, 220, 119, 15, 255, 83, 245, 8, 53, 248, 204, 149, 126, 1, 192, 213, 115, 29, 60, 46,
    213, 68, 195, 117, 73, 58, 40, 192, 155, 229, 224, 151, 99, 7, 100, 45, 48, 48, 48, 48, 51,
    219, 236, 14, 104, 159, 182, 59, 215, 41, 20, 119, 39, 18, 157, 133, 78, 157, 89, 10, 182, 32,
    161, 139, 187, 203, 51, 23, 210, 104, 214, 253, 114,
];
/// The hash of those bytes (pinned).
const BLOCK_HASH: &str = "8619d6f1a53b3803f185581997429479393fa43e066b46bcb8cf47b21e34ce30";

#[test]
fn ha3_block_encoding_matches_core_exactly() {
    let b = block();
    let bytes = encode_block(&b).unwrap();
    assert_eq!(bytes, postcard::to_allocvec(&b).unwrap());
    assert_eq!(hash_bytes(&bytes), b.version_hash());
    assert_eq!(hash_bytes(&bytes), rdy_model::hash_of(&b));
    assert_eq!(bytes, BLOCK_BYTES, "{bytes:?}");
    assert_eq!(hash_bytes(&bytes).to_string(), BLOCK_HASH);
    // map ordering: the same entries inserted in the opposite order encode to the same bytes
    let mut again = b.clone();
    again.fields = b.fields.clone().into_iter().rev().collect();
    again.links = b.links.clone().into_iter().rev().collect();
    again.pins = b.pins.clone().into_iter().rev().collect();
    assert_eq!(encode_block(&again).unwrap(), bytes);
}

#[test]
fn ha3_block_encoding_limit_is_exact() {
    let mut b = block();
    b.body = String::new();
    let base = encode_block(&b).unwrap().len();
    // a body this long has a four-byte length prefix instead of one byte
    let fits = rdy_model::redaction::MAX_RECORD_BYTES - base - 3;
    b.body = "x".repeat(fits);
    assert_eq!(
        encode_block(&b).unwrap().len(),
        rdy_model::redaction::MAX_RECORD_BYTES
    );
    b.body.push('x');
    let e = encode_block(&b).unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
    assert_eq!(e.message, "block encoding exceeds 16777216 bytes");
    assert_eq!(e.fix, "reduce the block to the stated limit");
}

#[test]
fn ha3_projection_encoding_is_refused() {
    let request = hash_bytes(b"request");
    let projection = Block::redacted(
        BlockId::parse("d-00009").unwrap(),
        "doc".to_string(),
        None,
        "a0".to_string(),
        rdy_model::RedactedBlockIdentity {
            version: hash_bytes(b"version"),
            content_hash: hash_bytes(b"content"),
            frozen: false,
            request,
        },
    );
    let e = encode_block(&projection).unwrap_err();
    assert_eq!(e.code, "E_REDACTED");
    assert_eq!(
        e.message,
        format!("Content removed by redaction {request}.")
    );
    assert!(!e.message.contains(&projection.body), "{e}");
}

/// `depth` nested singleton lists (or maps) around one scalar, built without recursion.
fn nested(depth: usize, maps: bool) -> rdy_model::FieldValue {
    use rdy_model::FieldValue;
    let mut v = FieldValue::Int(1);
    for _ in 0..depth {
        v = if maps {
            FieldValue::Map(BTreeMap::from([("k".to_string(), v)]))
        } else {
            FieldValue::List(vec![v])
        };
    }
    v
}

/// Takes a nested value apart level by level, so dropping it does not recurse.
fn dismantle(mut v: rdy_model::FieldValue) {
    use rdy_model::FieldValue;
    loop {
        v = match v {
            FieldValue::List(mut items) => match items.pop() {
                Some(inner) => inner,
                None => return,
            },
            FieldValue::Map(mut m) => match m.pop_first() {
                Some((_, inner)) => inner,
                None => return,
            },
            _ => return,
        };
    }
}

#[test]
fn ha3_nesting_depth_is_bounded_before_encoding() {
    // The deepest nesting that encodes: bytes equal core's own encoding.
    for maps in [false, true] {
        let mut b = block();
        b.fields.insert("deep".to_string(), nested(128, maps));
        assert_eq!(
            encode_block(&b).unwrap(),
            postcard::to_allocvec(&b).unwrap()
        );
        let before = b.fields.remove("deep").unwrap();
        dismantle(before);
        // One level more, and a hostile depth far below the byte limit, are refused with no
        // recursion into the serializer (65,536 levels overflowed the stack before this bound).
        for depth in [129, 65_536, 1_000_000] {
            let mut b = block();
            b.fields.insert("deep".to_string(), nested(depth, maps));
            let e = encode_block(&b).unwrap_err();
            assert_eq!(e.code, "E_LIMIT", "depth {depth}: {e}");
            assert_eq!(e.message, "a field value nests deeper than 128 levels");
            assert_eq!(
                e.fix,
                "reduce the nesting of the field value to the stated limit"
            );
            dismantle(b.fields.remove("deep").unwrap());
        }
    }
}

#[test]
fn ha3_wide_values_encode_as_core_encodes_them() {
    use rdy_model::FieldValue;
    // A shallow value of any width is within the depth bound: bytes equal core's encoding.
    let wide_list = FieldValue::List(vec![FieldValue::Null; 1_000_000]);
    let wide_map = FieldValue::Map(
        (0..100_000)
            .map(|i| (format!("k{i:06}"), FieldValue::Int(i)))
            .collect(),
    );
    for wide in [wide_list, wide_map] {
        let mut b = block();
        b.fields.insert("wide".to_string(), wide);
        assert_eq!(
            encode_block(&b).unwrap(),
            postcard::to_allocvec(&b).unwrap()
        );
    }
    // A wide level above a too-deep one is still refused.
    let mut b = block();
    b.fields.insert(
        "wide".to_string(),
        FieldValue::List(vec![FieldValue::Null; 100_000]),
    );
    b.fields.insert("deep".to_string(), nested(129, false));
    assert_eq!(encode_block(&b).unwrap_err().code, "E_LIMIT");
    dismantle(b.fields.remove("deep").unwrap());
}
