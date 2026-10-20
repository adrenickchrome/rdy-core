//! sdc6: the MCP error encoding names the two causes of a refused open with their own codes.

use rdy_engine::{HELD_BY_READER_FIX, Rdy, RdyError};
use rdy_mcp::tools::{ERROR_ROWS, error_json};
use std::path::Path;

const LOCKED_FIX: &str = "drop --no-daemon, or stop the daemon with rdy serve --stop";

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    Rdy::init(dir.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    dir
}

fn refused(path: &Path) -> RdyError {
    Rdy::open(path).err().expect("the open is refused")
}

#[test]
fn sdc6_mcp_encodes_e_locked_for_a_second_live_open() {
    let dir = project();
    let first = Rdy::open(dir.path()).unwrap();
    let v = error_json(&refused(dir.path()));
    assert_eq!(v["code"], "E_LOCKED", "{v}");
    assert_eq!(v["fix"], LOCKED_FIX, "{v}");
    drop(first);
}

#[test]
fn sdc6_mcp_encodes_e_held_by_reader_and_lists_its_row() {
    let dir = project();
    let r = Rdy::open(dir.path()).unwrap();
    let held = r.read();
    drop(r);
    let v = error_json(&refused(dir.path()));
    assert_eq!(v["code"], "E_HELD_BY_READER", "{v}");
    assert_eq!(v["fix"], HELD_BY_READER_FIX, "{v}");
    drop(held);

    let row = (
        "E_HELD_BY_READER",
        "the project store is still open in this process through a read state of an earlier open",
        HELD_BY_READER_FIX,
    );
    let at: Vec<usize> = ERROR_ROWS
        .iter()
        .enumerate()
        .filter(|(_, r)| r.0 == "E_HELD_BY_READER")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(at.len(), 1, "one row");
    assert_eq!(ERROR_ROWS[at[0]], row);
    assert_eq!(
        at[0] + 3,
        ERROR_ROWS.len(),
        "directly before the two publication rows"
    );
    assert_eq!(ERROR_ROWS[at[0] + 1].0, "E_PUBLICATION_REFERENCE");
    assert_eq!(ERROR_ROWS[at[0] + 2].0, "W_PUBLICATION_REFERENCE");
}
