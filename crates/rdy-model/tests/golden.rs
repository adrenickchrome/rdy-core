//! Golden hashes: fixed values must keep these exact version hashes. A failure means the
//! canonical encoding changed (a field or enum variant was added, removed or reordered), which
//! would rename every stored object: bump the format deliberately or undo the change.
use rdy_model::*;
use std::collections::BTreeMap;

fn block() -> Block {
    let mut fields = BTreeMap::new();
    fields.insert("a_null".into(), FieldValue::Null);
    fields.insert("b_bool".into(), FieldValue::Bool(true));
    fields.insert("c_int".into(), FieldValue::Int(-42));
    fields.insert("d_str".into(), FieldValue::Str("Ünïcødé".into()));
    fields.insert("e_list".into(), FieldValue::List(vec![FieldValue::Int(1), FieldValue::Str("x".into())]));
    fields.insert("f_map".into(), FieldValue::Map(BTreeMap::from([("k".to_string(), FieldValue::Bool(false))])));
    Block {
        id: BlockId::parse("c-7KQ2M").unwrap(),
        block_type: "claim".into(),
        parent: Some(BlockId::parse("s-1DENT").unwrap()),
        order: "V".into(),
        fields,
        body: "Balances live on the {{t-MEDGR}}.".into(),
        links: BTreeMap::from([("describes".to_string(), vec![Link { target: BlockId::parse("t-MEDGR").unwrap(), part: Some("balance".into()) }])]),
        pins: BTreeMap::from([(BlockId::parse("t-MEDGR").unwrap(), Hash::of(b"pinned"))]),
    }
}

fn change(justification: Option<Justification>) -> Change {
    let b = block();
    Change {
        view: "work".into(),
        ops: vec![
            Op::Create { block: b.clone() },
            Op::Update { base: b.version_hash(), block: b.clone() },
            Op::Delete { base: b.version_hash(), id: b.id.clone() },
        ],
        deps: vec![Hash::of(b"dep")],
        provenance: Provenance { actor: "agent:designer".into(), run: Some("run-1".into()), role: Role::Approver, at_ms: 1_759_449_600_000 },
        justification,
        message: "golden".into(),
    }
}

#[test]
fn canonical_encoding_is_frozen() {
    assert_eq!(block().version_hash().to_string(), "3900f27b4ae5bff77eb9b5e6313e5f30b77df8fb3274ce3d56e1b8074356d71e");
    assert_eq!(change(None).id().to_string(), "2d24ead4674748aa8b8d0788ec49ee427f3adba8e33eabf49078271aadd8215b");
    assert_eq!(change(Some(Justification::Implements(BlockId::parse("D-W2HXR").unwrap()))).id().to_string(), "b6e750bf9649c363e3be5e6c022b6bea296ef80e2d7188d09fdb76df49a818ff");
    assert_eq!(change(Some(Justification::Editorial { class: EditorialClass::Clarify, reason: "wording".into() })).id().to_string(), "be552e118939bf6bdce4cc62db8ead08cb5c8ce833b25bd6c274d71e3f434282");
}
