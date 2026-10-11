//! se2: the daemon frees a session's leases when the session ends.
use rdy_daemon::{SessionId, Shared};
use rdy_model::{BlockId, Hash};

#[test]
fn se2_a_session_end_frees_its_leases() {
    let d = tempfile::tempdir().unwrap();
    let r = rdy_engine::Rdy::init(d.path(), "rdy-standard").unwrap();
    let shared = Shared::new(r);
    let leases = shared.engine().leases();
    let pair = vec![(BlockId::parse("c-AAAAA").unwrap(), Hash([1; 32]))];
    assert!(leases.claim(Hash([5; 32]), pair.clone(), 7, 0, 600_000));
    assert!(leases.claim(Hash([6; 32]), pair, 9, 0, 600_000));
    shared.end_session(SessionId(7));
    assert_eq!(
        leases.leased(1).into_iter().collect::<Vec<_>>(),
        vec![Hash([6; 32])]
    );
    shared.shutdown().unwrap();
}
