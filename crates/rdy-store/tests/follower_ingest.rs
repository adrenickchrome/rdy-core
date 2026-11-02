//! The follow path's bounds and capability (D-AA4FD; tb-A4DAV: hostile input is an error or a
//! bounded allocation): each new limit passes at its bound and refuses one past it, before any
//! growth, and an ordinary store takes no follow.
use rdy_store::Store;
use rdy_store::dump_chain::{BatchMarker, ChainEnd};
use rdy_store::follower::{
    FollowError, FollowLimit, FollowerFailure, FollowerMark, check_follow_limit_for_tests,
    follow_limit, prepare_follow,
};
use rdy_store::format::{DumpStore, baseline_features};
use rdy_store::segment::Location;

#[test]
fn mh8_hostile_increment_bounds_precede_growth() {
    for (unit, limit) in [
        (FollowLimit::Batches, 1_000_000u64),
        (FollowLimit::Frames, 10_000_000),
        (FollowLimit::Segments, 65_536),
        (FollowLimit::PrefixFrames, 100_000_000),
    ] {
        assert_eq!(follow_limit(unit), limit);
        check_follow_limit_for_tests(unit, limit).unwrap();
        match check_follow_limit_for_tests(unit, limit + 1) {
            Err(FollowError::Limit { unit: got }) => assert_eq!(got, unit),
            other => panic!("{unit:?}: {other:?}"),
        }
    }
    // an ordinary store takes no follow, before the file is read
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("store");
    let s = Store::create(&dir).unwrap();
    let mark = FollowerMark {
        chain: ChainEnd {
            repo_id: rdy_model::RepoId::new(),
            store_format: DumpStore::Features(baseline_features()),
            end: Location {
                segment: 0,
                offset: 0,
            },
            end_batch: BatchMarker::ZERO,
            digest: rdy_model::Hash([0; 32]),
        },
        files: vec![],
    };
    match prepare_follow(&s, &mark, &d.path().join("absent")) {
        Err(FollowError::Follower(FollowerFailure::NotFollower)) => {}
        other => panic!("{:?}", other.map(|_| ())),
    }
}
