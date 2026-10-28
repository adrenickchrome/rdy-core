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
        read_identity: None,
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
    // ok2: the content hash leaves out the order key
    assert_eq!(a.content_hash(), moved.content_hash());
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
    b.fields.insert(
        "note".into(),
        FieldValue::List(vec![FieldValue::Str("see {{c-WXMET}}".into())]),
    );
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
    b.links.insert(
        "decided_by".into(),
        vec![Link {
            target: BlockId::parse("D-W2HXR").unwrap(),
            part: None,
        }],
    );
    let bytes = postcard::to_allocvec(&b).unwrap();
    assert_eq!(postcard::from_bytes::<Block>(&bytes).unwrap(), b);
}

fn typed(block_type: &str, body: &str) -> Block {
    let mut b = claim(body);
    b.block_type = block_type.into();
    b
}

fn set(items: &[&str]) -> std::collections::BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn reference_scanning_is_type_aware_and_honours_escapes() {
    // code bodies are never scanned; their fields still are
    let mut code = typed("code", "image: {{ .Values.image }} {{t-MEDGR}}");
    assert!(code.ref_tokens().is_empty());
    code.fields
        .insert("caption".into(), FieldValue::Str("see {{t-MEDGR}}".into()));
    assert_eq!(code.ref_tokens(), set(&["t-MEDGR"]));
    // diagram bodies: only id-shaped tokens and placeholders count
    let dg = typed(
        "diagram",
        "graph TD\n  A{{Decide?}} --> B[{{t-MEDGR}}] --> C[{{$t}}] --> D{{ yes }}",
    );
    assert_eq!(dg.ref_tokens(), set(&["$t", "t-MEDGR"]));
    // prose: \{{ is a literal, everything else is strict
    assert_eq!(
        claim(r"Write \{{literal}} for braces; see {{t-MEDGR}}.").ref_tokens(),
        set(&["t-MEDGR"])
    );
    assert_eq!(claim("{{not an id}}").ref_tokens(), set(&["not an id"]));
    assert_eq!(typed("diagram", r"A\{{t-MEDGR}}").ref_tokens(), set(&[]));
}
