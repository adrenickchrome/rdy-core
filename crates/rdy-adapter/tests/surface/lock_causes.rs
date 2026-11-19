//! sdc6: the adapter names the two causes of a refused open with their own codes.

use rdy_adapter::{RdyError, StoreHandle};
use std::path::Path;

const LOCKED_FIX: &str = "drop --no-daemon, or stop the daemon with rdy serve --stop";

/// An empty project on disk, closed again.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::Rdy::init(dir.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    dir
}

fn refused(path: &Path) -> RdyError {
    StoreHandle::open(path).err().expect("the open is refused")
}

#[test]
fn sdc6_adapter_a_second_live_open_is_e_locked() {
    let dir = project();
    let first = StoreHandle::open(dir.path()).unwrap();
    let e = refused(dir.path());
    assert_eq!(e.code, "E_LOCKED", "{e}");
    assert_eq!(e.fix, LOCKED_FIX, "{e}");
    drop(first);
}

#[test]
fn sdc6_adapter_a_live_snapshot_of_a_dropped_handle_is_e_held_by_reader() {
    let dir = project();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snapshot = handle.snapshot().unwrap();
    drop(handle);
    let e = refused(dir.path());
    assert_eq!(e.code, "E_HELD_BY_READER", "{e}");
    assert_eq!(
        e.message,
        format!(
            "the project store at {} is still open in this process through a read state of an earlier open",
            dir.path().join(".rdy").join("store").display()
        ),
        "{e}"
    );
    assert_eq!(e.fix, rdy_engine::HELD_BY_READER_FIX, "{e}");
    assert_eq!(e.fix, "release the earlier read state, then try again");
    drop(snapshot);
    StoreHandle::open(dir.path()).unwrap();
}
