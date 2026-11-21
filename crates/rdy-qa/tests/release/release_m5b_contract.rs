//! wck1 (D-TXFHR; D-QVTJ9): the contract fixture of the check modules, and the interpreter
//! boundary's place in the dependency graph.
//!
//! `m5b-v1.json` is the current fixture: it started as a byte copy of `sync-client-b-v1.json`,
//! which is frozen from here on, and every selector moves to it. The module rule capture
//! (wck6) is its first change, and the module metadata capture (wck7) its second. The
//! interpreter is linked through `rdy-check-wasm` alone: no other package of the workspace
//! depends on it, the boundary depends on no rdy crate, and the engine depends on the boundary.
use rdy_qa::fixtures::output_within;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The predecessor of the current fixture, frozen from here on.
const PREDECESSOR: &str = "sync-client-b-v1.json";

/// The sha256 of [`PREDECESSOR`] at this plan's execution base. It never moves: a later contract
/// change is a successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "e65653b123aea1307a9e2ab9728f3b5d18e25b33b8f7ca8aa9fb2ff5a2dcb3e1";

/// The current fixture, which every selector names.
const CURRENT: &str = "m5b-v1.json";

/// The sha256 of [`CURRENT`] since the example pack metadata capture (wck8).
const CURRENT_SHA: &str = "d00807ccc5ed37448eb81fec8addd240633596f73aa288b1dc54d26256d2fb41";

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bytes(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/contracts")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// wck1: the integrated predecessor keeps its bytes, the current fixture has its captured
/// bytes, and every selector names it and no earlier fixture.
#[test]
fn wck1_successor_starts_from_the_integrated_fixture() {
    let (old, new) = (bytes(PREDECESSOR), bytes(CURRENT));
    assert_eq!(sha(&old), PREDECESSOR_SHA, "{PREDECESSOR} is frozen");
    assert_eq!(sha(&new), CURRENT_SHA, "{CURRENT} has its captured bytes");
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{CURRENT}\"")),
            "{file} selects {CURRENT}"
        );
        assert!(
            !text.contains(&format!("contracts/{PREDECESSOR}\"")),
            "{file} no longer selects {PREDECESSOR}"
        );
    }
}

/// wck1: in the resolved graph, only `rdy-check-wasm` depends on the interpreter, with exactly
/// the `portable-dispatch`, `std` and `validate` features; the boundary depends on no rdy crate; and the engine
/// depends on the boundary.
#[test]
fn wck1_only_the_wrapper_depends_on_wasmi() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(root());
    let out = output_within(&mut cmd, Duration::from_secs(600)).expect("cargo metadata");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata JSON");
    let packages = meta["packages"].as_array().expect("packages");
    let name_of = |id: &str| -> String {
        packages
            .iter()
            .find(|p| p["id"] == id)
            .and_then(|p| p["name"].as_str())
            .unwrap_or_else(|| panic!("package {id}"))
            .to_string()
    };
    let nodes = meta["resolve"]["nodes"].as_array().expect("resolve nodes");
    // each package's name, the names it depends on (any kind), and its enabled features
    let resolved: Vec<(String, BTreeSet<String>, Vec<String>)> = nodes
        .iter()
        .map(|n| {
            let deps = n["deps"]
                .as_array()
                .expect("deps")
                .iter()
                .map(|d| name_of(d["pkg"].as_str().expect("pkg")))
                .collect();
            let features = n["features"]
                .as_array()
                .expect("features")
                .iter()
                .map(|f| f.as_str().expect("feature").to_string())
                .collect();
            (name_of(n["id"].as_str().expect("id")), deps, features)
        })
        .collect();
    let dependents = |target: &str| -> Vec<&str> {
        let mut d: Vec<&str> = resolved
            .iter()
            .filter(|(_, deps, _)| deps.contains(target))
            .map(|(name, _, _)| name.as_str())
            .collect();
        d.sort();
        d
    };

    assert_eq!(
        dependents("wasmi"),
        ["rdy-check-wasm"],
        "one package links wasmi"
    );
    let wasmi: Vec<&(String, BTreeSet<String>, Vec<String>)> =
        resolved.iter().filter(|(n, _, _)| n == "wasmi").collect();
    assert_eq!(wasmi.len(), 1, "one wasmi version");
    // portable dispatch: the interpreter's loop never grows the host stack per operator, in an
    // optimised build or not
    assert_eq!(
        wasmi[0].2,
        ["portable-dispatch", "std", "validate"],
        "wasmi's features"
    );
    let wrapper = resolved
        .iter()
        .find(|(n, _, _)| n == "rdy-check-wasm")
        .expect("rdy-check-wasm");
    assert_eq!(
        wrapper.1.iter().collect::<Vec<_>>(),
        ["wasmi"],
        "the boundary depends on the interpreter alone"
    );
    assert_eq!(
        dependents("rdy-check-wasm"),
        ["rdy-engine"],
        "the engine depends on the boundary"
    );
}
