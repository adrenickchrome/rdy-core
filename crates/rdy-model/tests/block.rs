use rdy_model::{Block, BlockId, FieldValue, Link};
use std::collections::BTreeMap;

fn claim(body: &str) -> Block {
    Block {
        id: BlockId::parse("b-7KQ2M").unwrap(),
        block_type: "claim".into(),
        parent: Some(BlockId::parse("d-1DENT").unwrap()),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

#[test]
fn version_hash_is_deterministic_and_content_sensitive() {
    let a = claim("x");
    assert_eq!(a.version_hash(), claim("x").version_hash());
    assert_ne!(a.version_hash(), claim("y").version_hash());
    let mut moved = claim("x");
    moved.order = "W".into();
    assert_ne!(a.version_hash(), moved.version_hash());
}

#[test]
fn field_and_link_insertion_order_does_not_change_the_hash() {
    let mut a = claim("x");
    a.fields.insert("b".into(), FieldValue::Int(2));
    a.fields.insert("a".into(), FieldValue::Int(1));
    let mut b = claim("x");
    b.fields.insert("a".into(), FieldValue::Int(1));
    b.fields.insert("b".into(), FieldValue::Int(2));
    assert_eq!(a.version_hash(), b.version_hash());
}

#[test]
fn ref_tokens_come_from_body_and_string_fields() {
    let mut b = claim("settles on the {{ t-MEDGR }} per {{D-W2HXR}}; {{not an id}} and {single}");
    b.fields.insert("note".into(), FieldValue::List(vec![FieldValue::Str("see {{c-WXMET}}".into())]));
    let toks: Vec<String> = b.ref_tokens().into_iter().collect();
    assert_eq!(toks, vec!["D-W2HXR", "c-WXMET", "not an id", "t-MEDGR"]);
}

#[test]
fn unterminated_reference_is_not_a_token() {
    assert!(claim("broken {{t-MEDGR").ref_tokens().is_empty());
}

#[test]
fn blocks_round_trip_postcard() {
    let mut b = claim("ünïcødé\nsecond line");
    b.links.insert("decided_by".into(), vec![Link { target: BlockId::parse("D-W2HXR").unwrap(), part: None }]);
    let bytes = postcard::to_allocvec(&b).unwrap();
    assert_eq!(postcard::from_bytes::<Block>(&bytes).unwrap(), b);
}
