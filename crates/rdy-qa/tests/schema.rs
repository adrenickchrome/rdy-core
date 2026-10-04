use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use rdy_model::{Block, BlockId, FieldValue};
use rdy_qa::fixtures::cases;
use rdy_schema::*;

#[test]
fn schema_hostile_packs_are_refused() {
    let packs = [
        "name: a\nversion: \"1\"\nextends: b\n---\n", // unknown parent once split below
        "name: x\nversion: \"1\"\nblock_types:\n  folder: { prefix: zz }\n",
        "name: y\nversion: \"1\"\nblock_types:\n  t: { prefix: q, fields: { e: { kind: enum } } }\n",
        "name: z\nversion: \"1\"\nbogus_key: 1\n",
        "name: w\nversion: \"1\"\nblock_types:\n  t1: { prefix: q }\n  t2: { prefix: q }\n",
    ];
    for p in packs {
        let mut set = PackSet::with_builtin();
        let r = set.add_yaml(p).and_then(|n| set.resolve(&n).map(|_| ()));
        assert!(r.is_err(), "{p}");
    }
    let mut set = PackSet::with_builtin();
    set.add_yaml("name: a\nversion: \"1\"\nextends: b\n")
        .unwrap();
    set.add_yaml("name: b\nversion: \"1\"\nextends: a\n")
        .unwrap();
    assert!(set.resolve("a").is_err(), "a cycle is refused");
    // a large pack resolves in bounded time
    let mut big = String::from("name: big\nversion: \"1\"\nblock_types:\n");
    for i in 0..2000 {
        big.push_str(&format!("  t{i}: {{ prefix: {} }}\n", prefix(i)));
    }
    let mut set = PackSet::with_builtin();
    let t = std::time::Instant::now();
    let n = set.add_yaml(&big).unwrap();
    let _ = set.resolve(&n);
    assert!(t.elapsed() < std::time::Duration::from_secs(2));
}

fn prefix(i: usize) -> String {
    let a = b"abcdefghijklmnopqrstuvwxyz";
    format!(
        "{}{}{}",
        a[i % 26] as char,
        a[(i / 26) % 26] as char,
        a[(i / 676) % 26] as char
    )
}

fn value() -> impl Strategy<Value = FieldValue> {
    let leaf = prop_oneof![
        Just(FieldValue::Null),
        any::<bool>().prop_map(FieldValue::Bool),
        any::<i64>().prop_map(FieldValue::Int),
        "\\PC{0,20}".prop_map(FieldValue::Str)
    ];
    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(FieldValue::List),
            proptest::collection::btree_map("[a-z_]{1,8}", inner, 0..4).prop_map(FieldValue::Map),
        ]
    })
}

proptest! {
    #![proptest_config(Config { cases: cases(512), rng_seed: RngSeed::Fixed(0x005c_4e3a), failure_persistence: None, ..Config::default() })]
    #[test]
    fn schema_validate_block_never_panics(
        ty in prop_oneof![Just("doc"), Just("folder"), Just("section"), Just("claim"), Just("decision"), Just("figure"), Just("nonsense"), Just("")],
        parent_ty in prop_oneof![Just("doc"), Just("folder"), Just("section"), Just("claim"), Just("nonsense")],
        has_parent in any::<bool>(),
        order in "\\PC{0,30}",
        fields in proptest::collection::btree_map("[a-z_]{1,10}", value(), 0..6),
        body in "\\PC{0,40}",
    ) {
        let pack = PackSet::with_builtin().resolve("rdy-standard").unwrap();
        let mk = |ty: &str, parent: Option<BlockId>| Block { id: BlockId::parse("c-AAAAA").unwrap(), block_type: ty.into(), parent, order: order.clone(), fields: fields.clone(), body: body.clone(), links: Default::default(), pins: Default::default() };
        let p = mk(parent_ty, None);
        let b = mk(ty, has_parent.then(|| BlockId::parse("s-AAAAA").unwrap()));
        let _ = validate_block(&pack, &b, has_parent.then_some(&p));
    }
}
