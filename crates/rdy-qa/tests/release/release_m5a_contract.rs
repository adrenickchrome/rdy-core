//! vxs1 (D-TXFHR): the M5a contract fixture. `m5a-v1.json` is the current fixture: it starts as a
//! byte copy of the integrated predecessor `m5b-v1.json`, which is frozen from here on,
//! and later captures add M5a's surfaces to it. [`m5a_changes`] lists every member where the
//! successor differs from that predecessor, so a closed list of M5a's changes can be checked
//! against it; this file pins only where the successor starts: the predecessor's bytes, the
//! fixture lineage, the selectors, and that no member of the predecessor is removed.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The integrated predecessor of the current fixture. It is frozen.
const PREDECESSOR: &str = "m5b-v1.json";

/// The sha256 of [`PREDECESSOR`] as the base froze it. It never moves: a later contract change
/// is a successor fixture, never an edit of that file.
const M5B_V1_SHA: &str = "d00807ccc5ed37448eb81fec8addd240633596f73aa288b1dc54d26256d2fb41";

/// The current fixture, which every selector names.
const CURRENT: &str = "m5a-v1.json";

/// The contract fixtures in the order each succeeded the one before, ending in [`CURRENT`].
const LINEAGE: [&str; 12] = [
    "release-1.json",
    "repo-links-v1.json",
    "mcp-http-v1.json",
    "m4a-v1.json",
    "m4bc-v1.json",
    "readme-from-rdy-v1.json",
    "disclosure-review-v1.json",
    "sd1-v1.json",
    "sync-client-v1.json",
    "sync-client-b-v1.json",
    PREDECESSOR,
    CURRENT,
];

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts")
}

fn bytes(name: &str) -> Vec<u8> {
    let p = contracts().join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn read(name: &str) -> Value {
    serde_json::from_slice(&bytes(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// A list of identified entries as an object keyed by identity (`key=`, `name=`, `id=` or the
/// first string of a row), so a change reads as the entry's own path. Other lists stay lists.
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

/// Every member where `now` differs from `was`, as `added|removed|changed <path>`.
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

/// The members of [`CURRENT`] that differ from the actual predecessor [`PREDECESSOR`], as
/// `added|removed|changed <path>` in the keyed paths of [`keyed`], sorted.
fn m5a_changes() -> Vec<String> {
    let mut found = vec![];
    leaves(
        &keyed(&read(PREDECESSOR)),
        &keyed(&read(CURRENT)),
        "",
        &mut found,
    );
    found.sort();
    found
}

/// vxs1: the successor starts from the integrated predecessor. `m5b-v1.json` keeps its
/// bytes, the contracts folder holds the lineage and nothing else, every selector names
/// `m5a-v1.json` and none names the predecessor, and the successor removes no member or row of
/// the predecessor. Later captures may add to the successor.
#[test]
fn vxs1_successor_starts_from_the_integrated_fixture() {
    assert_eq!(
        sha(&bytes(PREDECESSOR)),
        M5B_V1_SHA,
        "{PREDECESSOR} is frozen"
    );
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<&str> = LINEAGE.to_vec();
    want.sort();
    assert_eq!(names, want, "the folder holds the fixture lineage alone");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{CURRENT}\"")),
            "{file} selects {CURRENT}"
        );
        assert!(
            !text.contains(&format!("contracts/{PREDECESSOR}\"")),
            "{file} no longer selects {PREDECESSOR}"
        );
    }
    let removed: Vec<String> = m5a_changes()
        .into_iter()
        .filter(|c| c.starts_with("removed "))
        .collect();
    assert!(removed.is_empty(), "{removed:?}");
    let (Value::Object(was), Value::Object(now)) = (read(PREDECESSOR), read(CURRENT)) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(
        was.keys().collect::<Vec<_>>(),
        now.keys().collect::<Vec<_>>(),
        "every row of {PREDECESSOR} is there, in order"
    );
}
