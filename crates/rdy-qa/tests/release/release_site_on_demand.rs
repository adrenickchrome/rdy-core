//! sod3 (D-TXFHR): Site on demand changes no contract surface. The contract fixture it found
//! keeps its bytes, and the plan adds no successor fixture, so the contracts folder holds no file
//! for it. Which fixture the selectors name is pinned by the plan that adds the next one
//! (`release_sync_client_b_contract.rs`), not here.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The current fixture when Site on demand started. It is the predecessor of this change, and
/// this change adds no successor: it stays the current fixture.
const PREDECESSOR: &str = "sync-client-v1.json";

/// The sha256 of [`PREDECESSOR`]. It never moves here: a contract change is a successor fixture
/// with its own plan, never an edit of this file.
const PREDECESSOR_SHA: &str = "a8bc2d43a01a728f14d7926cb7ace8fd82d4312047f0c03f39720018efcae10c";

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts")
}

#[test]
fn sod3_contract_surfaces_remain_unchanged() {
    let bytes = std::fs::read(contracts().join(PREDECESSOR)).expect("the current fixture");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        PREDECESSOR_SHA,
        "{PREDECESSOR} keeps its bytes"
    );
    // no successor: the folder holds no fixture of this change
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(
        !names.iter().any(|n| n.contains("site-on-demand")),
        "no successor fixture: {names:?}"
    );
    assert!(names.iter().any(|n| n == PREDECESSOR), "{names:?}");
}
