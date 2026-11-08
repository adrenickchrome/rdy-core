//! The native editor fixture for the CLI area (26b): the test executable itself, started as the
//! `EDITOR` in `edit` mode. It replaces every shell editor script, so the same bytes edit the
//! buffer on every platform.

#[path = "../../src/platform/child_fixture.rs"]
pub mod child_fixture;

use rdy_qa::daemon::Served;
use std::path::Path;
use std::process::Output;

/// `rdy --project <dir> --json edit <id>` as the human, with the editor `script` run by the
/// executable `exe` (this test executable when `None`). The project and its `rdy` are named for
/// the step that stops the daemon.
pub fn edit_with(s: &Served, exe: Option<&Path>, script: &str, id: &str) -> Output {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.arg("--project")
        .arg(s.dir.path())
        .args(["--json", "edit", id])
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(s.env_pairs());
    match exe {
        Some(exe) => child_fixture::set_editor_at(&mut cmd, exe, script),
        None => child_fixture::set_editor(&mut cmd, script),
    };
    child_fixture::set_editor_rdy(&mut cmd, &rdy_qa::fixtures::rdy_bin(), s.dir.path(), None);
    child_fixture::edit_output(&mut cmd).unwrap()
}
