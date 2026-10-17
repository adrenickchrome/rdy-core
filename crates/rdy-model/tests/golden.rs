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
    fields.insert(
        "e_list".into(),
        FieldValue::List(vec![FieldValue::Int(1), FieldValue::Str("x".into())]),
    );
    fields.insert(
        "f_map".into(),
        FieldValue::Map(BTreeMap::from([("k".to_string(), FieldValue::Bool(false))])),
    );
    Block {
        id: BlockId::parse("c-7KQ2M").unwrap(),
        block_type: "claim".into(),
        parent: Some(BlockId::parse("s-1DENT").unwrap()),
        order: "V".into(),
        fields,
        body: "Balances live on the {{t-MEDGR}}.".into(),
        links: BTreeMap::from([(
            "describes".to_string(),
            vec![Link {
                target: BlockId::parse("t-MEDGR").unwrap(),
                part: Some("balance".into()),
            }],
        )]),
        pins: BTreeMap::from([(BlockId::parse("t-MEDGR").unwrap(), Hash::of(b"pinned"))]),
        read_identity: None,
    }
}

fn change(justification: Option<Justification>) -> Change {
    let b = block();
    Change {
        view: "work".into(),
        ops: vec![
            Op::Create { block: b.clone() },
            Op::Update {
                base: b.version_hash(),
                block: b.clone(),
            },
            Op::Delete {
                base: b.version_hash(),
                id: b.id.clone(),
            },
        ],
        deps: vec![Hash::of(b"dep")],
        provenance: Provenance {
            actor: "agent:designer".into(),
            run: Some("run-1".into()),
            role: Role::Approver,
            at_ms: 1_759_449_600_000,
        },
        justification,
        message: "golden".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

#[test]
fn canonical_encoding_is_frozen() {
    assert_eq!(
        block().version_hash().to_string(),
        "3900f27b4ae5bff77eb9b5e6313e5f30b77df8fb3274ce3d56e1b8074356d71e"
    );
    // M2a1: Change gained links (three absent options, 00 00 00); the hash was recomputed
    // cc1: ChangeLinks gained imported_by (a fourth absent option); the hash was recomputed
    assert_eq!(
        change(None).id().to_string(),
        "072c20746baee55920394bab70945e4d633355a06d05dd1794425fafb53d380b"
    );
    // M2c3 llst1: Justification::Implements holds a list; the hash was recomputed
    // cc1: ChangeLinks gained imported_by (a fourth absent option); the hash was recomputed
    assert_eq!(
        change(Some(Justification::Implements(vec![
            BlockId::parse("D-W2HXR").unwrap()
        ])))
        .id()
        .to_string(),
        "4ed4a3a527c7de49e091c77dbbdd05230dd9d66f538ebcc48528cf9ed18beda6"
    );
    // cc1: ChangeLinks gained imported_by (a fourth absent option); the hash was recomputed
    assert_eq!(
        change(Some(Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: "wording".into()
        }))
        .id()
        .to_string(),
        "b453311fb47f908f16c4e00923bb9cb7f63a590e6807b62082b083f9fb379b3c"
    );
}
