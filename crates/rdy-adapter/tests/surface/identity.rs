//! The actor check through the adapter (TASK-1690): the adapter exports the engine's items, with
//! no wrapper and no copied grammar.

/// Compiles only when both arguments have one function-item type: a function item has a type of
/// its own, so a wrapper or a copy would not unify.
fn same_item<T>(_: T, _: T) {}

#[test]
fn relc3_actor_exports_are_the_engine_items() {
    same_item(rdy_adapter::is_actor, rdy_engine::identity::is_actor);
    assert_eq!(
        rdy_adapter::MAX_ACTOR_BYTES,
        rdy_engine::identity::MAX_ACTOR_BYTES
    );
    assert_eq!(rdy_adapter::MAX_ACTOR_BYTES, 135);
    let check: fn(&str) -> bool = rdy_adapter::is_actor;
    for ok in [
        "human:founder",
        "agent:a",
        "agent:handle/name",
        "ci:0123456789abcdef0123456789abcdef/job-1",
    ] {
        assert!(check(ok), "{ok}");
    }
    for bad in [
        "",
        "human:",
        "human:Founder",
        "agent:a/",
        "agent:/a",
        "ci:abc/job",
        "ci:0123456789abcdef0123456789abcdef",
        "robot:a",
        " human:a",
    ] {
        assert!(!check(bad), "{bad:?}");
    }
    // the byte limit: 6 + 64 + 1 + 64 bytes is the longest actor
    let name = "a".repeat(64);
    let longest = format!("agent:{name}/{name}");
    assert_eq!(longest.len(), rdy_adapter::MAX_ACTOR_BYTES);
    assert!(rdy_adapter::is_actor(&longest));
    assert!(!rdy_adapter::is_actor(&format!("{longest}a")));
}
