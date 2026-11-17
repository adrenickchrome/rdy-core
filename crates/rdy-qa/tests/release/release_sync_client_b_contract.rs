//! scc8 (D-TXFHR; D-H0BZN, D-47HVF, D-4XHPH, D-0SC9C; ruling R-F2): Sync client B's contract
//! fixture. `sync-client-b-v1.json` is the current fixture: it starts as a byte copy of
//! `sync-client-v1.json`, which is frozen from here on, and Task 8a1 adds only the sync wire's
//! fixture digest and the exchange's error values: the code `E_DIVERGED` and the texts of
//! `SYNC_DIVERGED`, `SYNC_REMOTE_MODE` and `SYNC_MIN_RDY`. Every other member is the
//! predecessor's.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The predecessor of the current fixture. It is frozen.
const PREDECESSOR: &str = "sync-client-v1.json";

/// The sha256 of [`PREDECESSOR`] at Sync client B's execution base. It never moves: a later
/// contract change is a successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "a8bc2d43a01a728f14d7926cb7ace8fd82d4312047f0c03f39720018efcae10c";

/// The current fixture, which every selector names.
const CURRENT: &str = "sync-client-b-v1.json";

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

/// The members Task 8a1 adds to [`CURRENT`], each with the reason it moves.
const SCC8_CHANGES: &[(&str, &str)] = &[
    (
        "added /errors_warnings/codes/E_DIVERGED",
        "a push whose local canon does not hold the hub tip (D-47HVF)",
    ),
    (
        "added /errors_warnings/templates/SYNC_DIVERGED_FIX",
        "the fix of E_DIVERGED",
    ),
    (
        "added /errors_warnings/templates/SYNC_DIVERGED_MESSAGE",
        "the text of E_DIVERGED",
    ),
    (
        "added /errors_warnings/templates/SYNC_MIN_RDY_FIX",
        "the fix of the hub's minimum rdy (D-4XHPH)",
    ),
    (
        "added /errors_warnings/templates/SYNC_MIN_RDY_MESSAGE",
        "the text of the hub's minimum rdy (D-4XHPH)",
    ),
    (
        "added /errors_warnings/templates/SYNC_REMOTE_MODE_FIX",
        "the fix of a remote that is not a share remote",
    ),
    (
        "added /errors_warnings/templates/SYNC_REMOTE_MODE_MESSAGE",
        "the text of a remote that is not a share remote",
    ),
    (
        "added /sync/fixture_digests/sync-client-wire-v1.json",
        "rdyhub's frozen sync wire fixture v1, a byte copy",
    ),
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

fn changes() -> Vec<String> {
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

/// scc8 (brief addition): the current fixture starts from the shipped predecessor:
/// `sync-client-v1.json` keeps its bytes, every selector names `sync-client-b-v1.json` and none
/// names the predecessor, and no member of the predecessor is removed.
#[test]
fn scc8_successor_starts_from_the_shipped_predecessor() {
    assert_eq!(
        sha(&bytes(PREDECESSOR)),
        PREDECESSOR_SHA,
        "{PREDECESSOR} is frozen"
    );
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
    let removed: Vec<String> = changes()
        .into_iter()
        .filter(|c| c.starts_with("removed "))
        .collect();
    assert!(removed.is_empty(), "{removed:?}");
    let (Value::Object(was), Value::Object(now)) = (read(PREDECESSOR), read(CURRENT)) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(
        was.keys().collect::<Vec<_>>(),
        now.keys().collect::<Vec<_>>()
    );
}

/// scc8: the successor adds only the sync wire fixture's digest and the exchange's error values
/// ([`SCC8_CHANGES`]); no whole section moves. The digest is the length and blake3 of the
/// copied fixture, whose sha256 is rdyhub's.
#[test]
fn scc8_contract_adds_only_sync_wire_and_errors() {
    let want: Vec<String> = SCC8_CHANGES.iter().map(|(c, _)| c.to_string()).collect();
    assert_eq!(changes(), want);
    for (change, why) in SCC8_CHANGES {
        assert!(!why.is_empty(), "{change} has a reason");
        // a leaf at least three levels down: never a whole top-level section or row
        let path = change.split_once(' ').unwrap().1;
        assert!(path.matches('/').count() >= 3, "{change}");
    }
    let wire = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sync/sync-client-wire-v1.json"),
    )
    .unwrap();
    assert_eq!(
        sha(&wire),
        "18ade8e2c1a301d780270f20fe49fc116110a6aa512baf624d768ef5ba39711f"
    );
    let now = read(CURRENT);
    assert_eq!(
        now["sync"]["fixture_digests"]["sync-client-wire-v1.json"],
        format!("{} {}", wire.len(), rdy_model::Hash::of(&wire))
    );
    assert_eq!(
        now["errors_warnings"]["codes"]["E_DIVERGED"],
        "\"E_DIVERGED\""
    );
    let templates = &now["errors_warnings"]["templates"];
    for (name, text) in [
        (
            "SYNC_DIVERGED_MESSAGE",
            "local canon does not hold hub tip <tip>",
        ),
        (
            "SYNC_DIVERGED_FIX",
            "run rdy pull to replay local landings, then push again",
        ),
        (
            "SYNC_REMOTE_MODE_MESSAGE",
            "remote <name> is not a share remote",
        ),
        (
            "SYNC_REMOTE_MODE_FIX",
            "set its mode to share, or choose a share remote",
        ),
        (
            "SYNC_MIN_RDY_MESSAGE",
            "hub repo <name> needs rdy <version>",
        ),
        ("SYNC_MIN_RDY_FIX", "update rdy to <version> or later"),
    ] {
        assert_eq!(templates[name], format!("\"{text}\""), "{name}");
    }
}
