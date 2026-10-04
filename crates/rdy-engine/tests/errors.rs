use rdy_engine::RdyError;
use rdy_model::{BlockId, Hash};
use rdy_store::StoreError;

fn id() -> BlockId {
    BlockId::parse("c-AAAAA").unwrap()
}

#[test]
fn stale_base_with_actual_carries_full_hash() {
    let actual = Hash([7; 32]);
    let e: RdyError = StoreError::StaleBase { id: id(), expected: Hash([1; 32]), actual: Some(actual) }.into();
    assert_eq!(e.code, "E_STALE_BASE");
    assert!(e.fix.contains(&"07".repeat(32)));
}

#[test]
fn stale_base_deleted_mentions_deleted() {
    let e: RdyError = StoreError::StaleBase { id: id(), expected: Hash([1; 32]), actual: None }.into();
    assert_eq!(e.code, "E_STALE_BASE");
    assert!(e.fix.contains("deleted"));
    assert!(e.message.contains("deleted"));
}

#[test]
fn other_mappings() {
    assert_eq!(RdyError::from(StoreError::UnknownView("w".into())).code, "E_NOT_FOUND");
    assert_eq!(RdyError::from(StoreError::Conflict { id: id() }).code, "E_STALE_BASE");
    assert_eq!(RdyError::from(StoreError::Frozen(id())).code, "E_FROZEN");
    assert_eq!(RdyError::from(StoreError::Io(std::io::Error::other("x"))).code, "E_IO");
}

#[test]
fn damaged_log_hint_does_not_point_at_commands_that_need_an_open_store() {
    let e = RdyError::from(StoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "corruption at offset 0 ...")));
    assert_eq!(e.code, "E_IO");
    assert!(!e.fix.contains("rdy fsck"), "{}", e.fix);
    assert!(e.fix.contains("message"), "{}", e.fix);
}
