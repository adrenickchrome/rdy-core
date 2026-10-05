use rdy_model::decision::{self, DecisionStatus};
use rdy_model::*;
use std::collections::BTreeMap;

fn block(id: &str, ty: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn prov() -> Provenance {
    Provenance {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        at_ms: 1,
    }
}

#[test]
fn change_id_covers_ops_and_provenance() {
    let c = Change {
        view: "work".into(),
        ops: vec![Op::Create {
            block: block("b-7KQ2M", "claim"),
        }],
        deps: vec![],
        provenance: prov(),
        justification: None,
        message: "m".into(),
        links: ChangeLinks::default(),
    };
    let mut c2 = c.clone();
    assert_eq!(c.id(), c2.id());
    c2.provenance.actor = "agent:other".into();
    assert_ne!(c.id(), c2.id());
}

#[test]
fn op_accessors() {
    let b = block("b-7KQ2M", "claim");
    let base = b.version_hash();
    assert_eq!(Op::Create { block: b.clone() }.base(), None);
    assert_eq!(
        Op::Update {
            base,
            block: b.clone()
        }
        .base(),
        Some(base)
    );
    assert_eq!(
        Op::Delete {
            base,
            id: b.id.clone()
        }
        .block_id(),
        &b.id
    );
}

#[test]
fn roles() {
    assert!(!Role::Reader.can_write());
    assert!(Role::Writer.can_write() && !Role::Writer.can_land());
    assert!(Role::Approver.can_land());
}

#[test]
fn decision_status_and_frozen() {
    let mut d = block("D-W2HXR", decision::DECISION);
    assert_eq!(decision::status(&d), None);
    d.fields
        .insert("status".into(), FieldValue::Str("proposed".into()));
    assert_eq!(decision::status(&d), Some(DecisionStatus::Proposed));
    assert!(!decision::is_frozen(&d));
    d.fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    assert!(decision::is_frozen(&d));
    let mut claim = block("b-7KQ2M", "claim");
    claim
        .fields
        .insert("status".into(), FieldValue::Str("accepted".into()));
    assert!(!decision::is_frozen(&claim));
}

#[test]
fn snapshot_id_is_order_independent() {
    let mut a = StateSnapshot {
        blocks: BTreeMap::new(),
    };
    a.blocks
        .insert(BlockId::parse("b-AAAAA").unwrap(), Hash::of(b"1"));
    a.blocks
        .insert(BlockId::parse("b-BBBBB").unwrap(), Hash::of(b"2"));
    let mut b = StateSnapshot {
        blocks: BTreeMap::new(),
    };
    b.blocks
        .insert(BlockId::parse("b-BBBBB").unwrap(), Hash::of(b"2"));
    b.blocks
        .insert(BlockId::parse("b-AAAAA").unwrap(), Hash::of(b"1"));
    assert_eq!(a.id(), b.id());
}
