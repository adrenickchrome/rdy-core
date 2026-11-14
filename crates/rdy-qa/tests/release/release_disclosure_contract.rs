//! drv (disclosure review rows; D-TXFHR, D-RDH1C, D-69AY3): the public contract after the
//! disclosure review plan.
//!
//! `contracts/disclosure-review-v1.json` is the current fixture: the frozen
//! `readme-from-rdy-v1.json` plus the additions of this plan. Every live comparison and capture
//! uses it: the release capture (`release_contract`), the CLI tree (`rdy-cli`'s `contract`) and
//! the rule listing check (`cli_rules`). The baselines before it are frozen and never recaptured.
//!
//! Task 1c starts the fixture as a byte copy of its predecessor and moves the three selectors to
//! it together. It changes no surface, so its list of changed members is empty. A later task adds
//! only its own named members to this same closed comparison.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts")
}

fn bytes(name: &str) -> Vec<u8> {
    let path = contracts().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn read(name: &str) -> Value {
    serde_json::from_slice(&bytes(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn sha(name: &str) -> String {
    Sha256::digest(bytes(name))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The current fixture.
const SUCCESSOR: &str = "disclosure-review-v1.json";

/// The predecessor of the current fixture, which every selector named before this plan.
const PREDECESSOR: &str = "readme-from-rdy-v1.json";

/// The sha256 of [`PREDECESSOR`]. It is frozen: a later contract change is a successor fixture,
/// never an edit of this file.
const PREDECESSOR_SHA: &str = "883debf0ad6ba99e983662a92ddb8c7f044306cea027ca835994b39b5187dc16";

/// Every frozen contract fixture before the current one, with its sha256.
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
    (PREDECESSOR, PREDECESSOR_SHA),
];

/// Task 1c: no member of the current fixture moves. The seam, the row model and the part
/// fixture add no output field yet.
const DRV1_CHANGES: &[&str] = &[];

/// A list of identified entries as an object keyed by identity, so a change reads as the entry's
/// own path and an inserted entry shifts nothing. Every other list stays a list.
fn keyed(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), keyed(x))).collect()),
        Value::Array(a) => {
            let identity = |e: &Value| -> Option<String> {
                for k in ["key", "name", "id"] {
                    if let Some(s) = e.get(k).and_then(Value::as_str) {
                        return Some(format!("{k}={s}"));
                    }
                }
                e.as_array()?.first()?.as_str().map(|s| format!("row={s}"))
            };
            let ids: Option<Vec<String>> = a.iter().map(identity).collect();
            match ids {
                Some(ids)
                    if !a.is_empty() && ids.iter().collect::<BTreeSet<_>>().len() == ids.len() =>
                {
                    let mut m = Map::new();
                    for (id, e) in ids.into_iter().zip(a) {
                        m.insert(id, keyed(e));
                    }
                    Value::Object(m)
                }
                _ => Value::Array(a.iter().map(keyed).collect()),
            }
        }
        other => other.clone(),
    }
}

/// Every member where `now` differs from `was`, as `added|removed|changed <path>`. A list that is
/// not keyed is one member.
fn leaves(was: &Value, now: &Value, path: &str, out: &mut Vec<String>) {
    match (was, now) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for k in keys {
                let at = format!("{path}/{k}");
                match (a.get(k), b.get(k)) {
                    (None, Some(_)) => out.push(format!("added {at}")),
                    (Some(_), None) => out.push(format!("removed {at}")),
                    (Some(x), Some(y)) => leaves(x, y, &at, out),
                    (None, None) => unreachable!("the key is in one of the objects"),
                }
            }
        }
        _ if was != now => out.push(format!("changed {path}")),
        _ => {}
    }
}

/// Every changed member of the current fixture against its predecessor is in one task's list,
/// and none else moved: a member that is not listed is equal.
fn assert_closed(old: &Value, new: &Value) {
    let mut found = vec![];
    leaves(&keyed(old), &keyed(new), "", &mut found);
    let mut want: Vec<String> = DRV1_CHANGES.iter().map(|p| (*p).to_string()).collect();
    found.sort();
    want.sort();
    assert_eq!(
        found, want,
        "every changed path of {SUCCESSOR} is listed, and none else"
    );
}

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

#[test]
fn drv1_successor_preserves_predecessor_surfaces() {
    // the predecessors are frozen
    for (name, digest) in FROZEN {
        assert_eq!(sha(name), *digest, "{name} is frozen");
    }

    // at Task 1c the successor equals its predecessor: the closed list is empty
    let (old, new) = (read(PREDECESSOR), read(SUCCESSOR));
    assert!(DRV1_CHANGES.is_empty(), "Task 1c moves no contract member");
    assert_closed(&old, &new);

    // no surface of the predecessor is removed
    let mut found = vec![];
    leaves(&keyed(&old), &keyed(&new), "", &mut found);
    let removed: Vec<&String> = found.iter().filter(|c| c.starts_with("removed ")).collect();
    assert!(removed.is_empty(), "{removed:?}");
    for surface in ["cli", "mcp", "errors_warnings", "store", "dump_load"] {
        assert!(new.get(surface).is_some(), "{surface}");
    }

    // all three selectors name the successor, and none names the predecessor as its path
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{SUCCESSOR}\"")),
            "{file} selects {SUCCESSOR}"
        );
        assert!(
            !text.contains(&format!("contracts/{PREDECESSOR}\"")),
            "{file} no longer selects {PREDECESSOR}"
        );
    }
}
