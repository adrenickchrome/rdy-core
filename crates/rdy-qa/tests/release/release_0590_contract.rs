//! r5901 (D-TXFHR, D-E30RA, D-KQ4CA, D-8QV9G): the contract fixture of release 0.59.0.
//!
//! `rdy-0590-v1.json` is the current fixture. It started as a byte copy of the frozen
//! `hub-ports-v1.json`, and every selector names it. The first capture, taken with the integrated
//! agent text, wrote no leaf: the fixture holds shapes, codes and probes, and the changed tool
//! descriptions and refusal fixes sit outside them. Later tasks of the release add their own
//! leaves. The tests here keep every earlier fixture as it was and pin that the agent delta is
//! descriptive only: no parameter type, required set, code or result shape moves.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The predecessor of this plan's fixture, frozen from here on.
const PREDECESSOR: &str = "hub-ports-v1.json";

/// The sha256 of [`PREDECESSOR`], its final bytes. It never moves: a later contract change is a
/// successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "1ca4ac7975b5518125d3ff65dc119f7e3d46e8305fc018a5425752d22f51fcab";

/// This plan's fixture, which every selector names.
const SELECTED: &str = "rdy-0590-v1.json";

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

/// The fixtures that a selector named before this plan's fixture. No selector names one now.
/// (`release-1.json` is no earlier selection: the released baseline stays a named source.)
const PREVIOUSLY_SELECTED: [&str; 8] = [
    "repo-links-v1.json",
    "readme-from-rdy-v1.json",
    "disclosure-review-v1.json",
    "sync-client-v1.json",
    "sync-client-b-v1.json",
    "m5b-v1.json",
    "hub-forks-v1.json",
    "hub-ports-v1.json",
];

/// Every frozen contract fixture, with the sha256 of its bytes.
const FROZEN: &[(&str, &str)] = &[
    (
        "release-1.json",
        "635f63e24cb0a743892f52944b9b5182f4f013ca75f5491d9e05c120400a6ef6",
    ),
    (
        "repo-links-v1.json",
        "b975e0cad885386f134d9dbfab08bf7677ccdbbc9bdeb0c927289e9346f4ca27",
    ),
    (
        "mcp-http-v1.json",
        "13a8de668dedbbbd2591a6da4e24d65d7897de8368107ff90e9d89917e26a3de",
    ),
    (
        "m4a-v1.json",
        "49ca6b3f9cab4e97445c1de5df5a513fe97ceee3971d9ae17853307bc9922a71",
    ),
    (
        "m4bc-v1.json",
        "f8a229b41fff5337d52d8b614670a5b0edcbee534598b35c001064fa040d06c6",
    ),
    (
        "readme-from-rdy-v1.json",
        "883debf0ad6ba99e983662a92ddb8c7f044306cea027ca835994b39b5187dc16",
    ),
    (
        "disclosure-review-v1.json",
        "956044b4b5e6a51a9810f31a4c0c16ad81aa9a035d735ed4d8b9d7a1d9edf139",
    ),
    (
        "sd1-v1.json",
        "012ff9ed50f654618517e629e3dcfa6c8e28935bf311c880de2faaa40576aea2",
    ),
    (
        "sync-client-v1.json",
        "a8bc2d43a01a728f14d7926cb7ace8fd82d4312047f0c03f39720018efcae10c",
    ),
    (
        "sync-client-b-v1.json",
        "e65653b123aea1307a9e2ab9728f3b5d18e25b33b8f7ca8aa9fb2ff5a2dcb3e1",
    ),
    (
        "m5b-v1.json",
        "d00807ccc5ed37448eb81fec8addd240633596f73aa288b1dc54d26256d2fb41",
    ),
    (
        "hub-forks-v1.json",
        "5cff7aee1f5afb3e1e1e0364fc72fb7367a6acf103b1ad64040818cc13cd3252",
    ),
    (PREDECESSOR, PREDECESSOR_SHA),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts")
}

fn bytes(name: &str) -> Vec<u8> {
    let path = contracts().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read(name: &str) -> Value {
    serde_json::from_slice(&bytes(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// r5901: every selector names this plan's fixture and no earlier one, every earlier fixture keeps
/// its bytes, and the folder holds those and this plan's fixture and no other.
#[test]
fn r5901_successor_keeps_every_frozen_fixture() {
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{SELECTED}\"")),
            "{file} selects {SELECTED}"
        );
        for earlier in PREVIOUSLY_SELECTED {
            assert!(
                !text.contains(&format!("contracts/{earlier}\"")),
                "{file} no longer selects {earlier}"
            );
        }
    }
    for (name, want) in FROZEN {
        assert_eq!(&sha(&bytes(name)), want, "{name} is frozen");
    }
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<&str> = FROZEN.iter().map(|(n, _)| *n).chain([SELECTED]).collect();
    want.sort();
    assert_eq!(
        names, want,
        "the folder holds every frozen fixture and {SELECTED}"
    );
}

/// The subtrees of the fixture that hold the agent text: the CLI tree, the MCP tool schemas and
/// refusals, and the error codes and texts.
const AGENT_SUBTREES: [&str; 3] = ["cli/tree", "mcp", "errors_warnings"];

/// Whether the leaf at `path` is words for a reader: a description, a message or a fix. Anything
/// else (a type, a required set, a code, a result field) is shape.
fn descriptive(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let last = parts.last().copied().unwrap_or("");
    // the text of a tool, a parameter, a command, a flag or an argument
    if ["description", "about", "message", "fix", "help"].contains(&last) {
        return true;
    }
    // a named message or fix template of the error rows
    if parts.starts_with(&["errors_warnings", "templates"]) {
        return true;
    }
    // an error row is [code, message, fix]: only the message and the fix are words
    parts.starts_with(&["errors_warnings", "error_rows"])
        && matches!(last, "1" | "2")
        && parts.len() == 4
}

/// The shape leaves later tasks of the release add to the agent subtrees: span maps' byte count
/// in the compact report. Nothing else may move.
const ADDED_LEAVES: &[&str] = &["member /mcp/result_fields/rdy_compact/ok/deduplicated_bytes"];

/// Every difference between `was` and `now` below `path`: a changed string leaf, or a change of
/// shape (a missing member, another type, another list length, a changed non-string leaf).
fn compare(was: &Value, now: &Value, path: &str, text: &mut Vec<String>, shape: &mut Vec<String>) {
    match (was, now) {
        (Value::Object(a), Value::Object(b)) => {
            for k in a.keys().chain(b.keys().filter(|k| !a.contains_key(*k))) {
                let at = format!("{path}/{k}");
                match (a.get(k), b.get(k)) {
                    (Some(x), Some(y)) => compare(x, y, &at, text, shape),
                    _ => shape.push(format!("member {at}")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                shape.push(format!("length {path}"));
                return;
            }
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                compare(x, y, &format!("{path}/{i}"), text, shape);
            }
        }
        (Value::String(a), Value::String(b)) => {
            if a != b {
                if descriptive(path) {
                    text.push(path.to_string());
                } else {
                    shape.push(format!("value {path}"));
                }
            }
        }
        _ if was != now => shape.push(format!("value {path}")),
        _ => {}
    }
}

/// r5901: the agent delta is descriptive only. In the CLI tree, the MCP schemas and the error
/// subtree, the successor differs from its predecessor in descriptions and in message and fix
/// text alone: every parameter type, required set, code and result shape is the predecessor's.
#[test]
fn r5901_agent_delta_is_descriptive_only() {
    let (was, now) = (read(PREDECESSOR), read(SELECTED));
    let mut text = vec![];
    let mut shape = vec![];
    for subtree in AGENT_SUBTREES {
        let at = |v: &Value| -> Value {
            subtree
                .split('/')
                .fold(v, |cur, k| {
                    cur.get(k)
                        .unwrap_or_else(|| panic!("no member {k} on the way to {subtree}"))
                })
                .clone()
        };
        compare(
            &at(&was),
            &at(&now),
            &format!("/{subtree}"),
            &mut text,
            &mut shape,
        );
    }
    assert_eq!(
        shape, ADDED_LEAVES,
        "only words and the added leaves differ"
    );
    // the words that differ are listed, so a failure names them
    for leaf in &text {
        assert!(descriptive(leaf), "{leaf} is words");
    }
}
