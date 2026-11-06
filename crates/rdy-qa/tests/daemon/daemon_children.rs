//! The harness's native child fixtures in this binary: a child that hangs, by no tool of any one
//! platform (23c).

#[path = "../../src/platform/child_fixture.rs"]
pub mod child_fixture;

use child_fixture::{TARGET_ENV, entry_selector, fixture_command, fixture_command_for};
use std::time::Duration;

#[test]
fn wx23c_integration_binary_reuses_the_fixture_with_its_own_selector() {
    // this binary's entry, not the library's
    assert_eq!(
        entry_selector(),
        "daemon_children::child_fixture::fixture_entry"
    );
    let status = fixture_command("exit:9").status().expect("run the fixture");
    assert_eq!(status.code(), Some(9));

    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("edited");
    let status = fixture_command_for("editor", &file)
        .env(child_fixture::TEXT_ENV, "reused\n")
        .status()
        .expect("run the editor");
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "reused\n");
    assert_eq!(TARGET_ENV, "RDY_QA_CHILD_TARGET");

    // the bounded runner from the product library runs the fixture
    let out = rdy_qa::platform::run_bounded(
        fixture_command("exit:0"),
        None,
        1 << 16,
        Duration::from_secs(60),
    )
    .expect("bounded run");
    assert_eq!(out.status.code(), Some(0));
}
