//! scc1 (D-TXFHR; D-DZDB7, D-WWAK7): the Sync client contract fixture. `sync-client-v1.json`
//! is the current fixture: it starts as a byte copy of the shipped `sd1-v1.json`, which is frozen
//! from here on, and adds only the holder's storage surfaces: the registered feature `hub_canon`
//! and its rule, and the digests of its first-use fixtures (the dump pair `v4-f1-e1-hub-canon`
//! and the native holder `store/hub-canon-1`). scc2 (D-KE83X, D-4XHPH, D-3RSCW) then adds sync
//! stream format 4, the minimum rule and the project's sync state: the registered feature
//! `sync_state` and its rule, the sync state refs and bounds, the history purposes and the
//! digests of the `sync4-*` stream fixtures, and the fix text `SYNC_LOCKED_FIX`. Every other
//! member is the predecessor's.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The shipped predecessor of the current fixture. It is frozen.
const PREDECESSOR: &str = "sd1-v1.json";

/// The sha256 of [`PREDECESSOR`], as rdy 0.52.0 shipped it. It never moves: a later contract
/// change is a successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "012ff9ed50f654618517e629e3dcfa6c8e28935bf311c880de2faaa40576aea2";

/// The current fixture, which every selector names.
const CURRENT: &str = "sync-client-v1.json";

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

/// The members of [`CURRENT`] that differ from [`PREDECESSOR`], as `added|changed <path>` in
/// the keyed paths of [`keyed`], each with the reason it moves. Closed: Task 1b adds the holder's
/// storage alone.
const SCC1_CHANGES: &[(&str, &str)] = &[
    (
        "added /dump_load/fixture_digests/v4-f1-e1-hub-canon.expect.json",
        "the first-use dump pair of hub_canon, frozen once",
    ),
    (
        "added /dump_load/fixture_digests/v4-f1-e1-hub-canon.rdydump",
        "the first-use dump of hub_canon, frozen once",
    ),
    (
        "added /store/feature_rules/hub_canon",
        "the activation rule of the feature, in words",
    ),
    (
        "added /store/features/name=hub_canon",
        "the registry gains the incompatible feature hub_canon at its minimum 0.53.0",
    ),
    (
        "added /store/holder_fixture",
        "the digests of the first-use native holder store/hub-canon-1",
    ),
    (
        "changed /store/registered_names",
        "the registry's names gain hub_canon",
    ),
    (
        "changed /store/sources",
        "the store row reads the native holder fixture",
    ),
];

/// The members that scc2 (Sync client A Task 2) changes, with the reason each moves. Closed:
/// Task 2 adds sync stream format 4, the minimum rule and the sync state alone.
const SCC2_CHANGES: &[(&str, &str)] = &[
    (
        "added /errors_warnings/templates/SYNC_LOCKED_FIX",
        "the fix text of E_LOCKED while a push or pull with the remote runs (D-3RSCW)",
    ),
    (
        "added /store/feature_rules/sync_state",
        "the activation rule of the feature, in words",
    ),
    (
        "added /store/features/name=sync_state",
        "the registry gains the incompatible feature sync_state at its minimum 0.53.0",
    ),
    (
        "added /store/sync_state",
        "the sync state refs of a remote, their value and name bounds and the write kinds",
    ),
    (
        "added /sync/fixture_digests/sync4-canon-history.bin",
        "the format-4 canon history fixture, frozen once",
    ),
    (
        "added /sync/fixture_digests/sync4-canon-history.expect.json",
        "the expect file of the canon history fixture, frozen once",
    ),
    (
        "added /sync/fixture_digests/sync4-view-history.bin",
        "the format-4 view history fixture, frozen once",
    ),
    (
        "added /sync/fixture_digests/sync4-view-history.expect.json",
        "the expect file of the view history fixture, frozen once",
    ),
    (
        "added /sync/history",
        "stream format 4: its read formats, each purpose's root kind and the minimum's classes",
    ),
    (
        "changed /store/registered_names",
        "the registry's names gain sync_state",
    ),
    (
        "changed /store/sources",
        "the store row reads rdy_store::sync_state",
    ),
    (
        "changed /sync/purposes",
        "the purposes gain canon_history (2) and view_history (3)",
    ),
];

/// The stream fixture files the sync row's digests gain in scc2 (F13: each leaf).
const SYNC4_FILES: [&str; 4] = [
    "sync4-canon-history.bin",
    "sync4-canon-history.expect.json",
    "sync4-view-history.bin",
    "sync4-view-history.expect.json",
];

/// The changed paths of scc1 and scc2 together, sorted and without repeats: both move
/// `/store/registered_names` and `/store/sources`.
fn all_changes() -> Vec<String> {
    let set: BTreeSet<String> = SCC1_CHANGES
        .iter()
        .chain(SCC2_CHANGES)
        .map(|(p, _)| (*p).to_string())
        .collect();
    set.into_iter().collect()
}

/// The files of the native holder whose digests the store row holds (F13: each leaf).
const HOLDER_FILES: [&str; 5] = [
    "FEATURES",
    "FORMAT",
    "expect.json",
    "lock",
    "segments/seg-000000.log",
];

/// The dump fixture files the dump row's digests gain (F13: each leaf).
const DUMP_FILES: [&str; 2] = [
    "v4-f1-e1-hub-canon.expect.json",
    "v4-f1-e1-hub-canon.rdydump",
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

/// scc1: the current fixture starts from the shipped predecessor: `sd1-v1.json` keeps the bytes
/// rdy 0.52.0 shipped, every selector names `sync-client-v1.json` and none names the
/// predecessor, and no member of the predecessor is removed.
#[test]
fn scc1_successor_starts_from_the_shipped_predecessor() {
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
    let (old, new) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(&keyed(&old), &keyed(&new), "", &mut found);
    let removed: Vec<&String> = found.iter().filter(|c| c.starts_with("removed ")).collect();
    assert!(removed.is_empty(), "{removed:?}");
    // the rows of the predecessor are all there, and only the store, dump, sync and error rows
    // move
    let (Value::Object(was), Value::Object(now)) = (&old, &new) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(
        was.keys().collect::<Vec<_>>(),
        now.keys().collect::<Vec<_>>()
    );
    for (row, v) in was {
        if !["store", "dump_load", "sync", "errors_warnings"].contains(&row.as_str()) {
            assert_eq!(&now[row], v, "row {row} moved");
        }
    }
}

/// scc1 (F13): the successor adds only the holder's storage and scc2's changes. Every changed
/// path is listed with its reason; the registered row of `hub_canon` is incompatible at 0.53.0; the store row holds
/// the digest of each file of the native holder, and the dump row of each file of its dump pair,
/// each as its length and blake3.
#[test]
fn scc1_contract_adds_only_holder_storage() {
    let (old, new) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(&keyed(&old), &keyed(&new), "", &mut found);
    let want = all_changes();
    found.sort();
    assert_eq!(
        found, want,
        "every changed path of {CURRENT} is listed with its reason, and none else"
    );
    let row = new["store"]["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "hub_canon")
        .expect("the registered feature");
    assert_eq!(row["class"], "incompatible");
    assert_eq!(
        row["required_rdy"],
        rdy_store::format::HUB_CANON_REQUIRED_RDY
    );
    assert_eq!(rdy_store::format::HUB_CANON_REQUIRED_RDY, "0.53.0");
    // each leaf of the new fixtures, with its bytes on disk
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let digest = |p: &Path| {
        let b = std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        format!("{} {}", b.len(), rdy_model::Hash::of(&b))
    };
    let holder = new["store"]["holder_fixture"].as_object().unwrap();
    assert_eq!(holder.keys().collect::<Vec<_>>(), HOLDER_FILES);
    for f in HOLDER_FILES {
        assert_eq!(
            holder[f],
            digest(&fixtures.join("store/hub-canon-1").join(f)),
            "{f}"
        );
    }
    let dumps = new["dump_load"]["fixture_digests"].as_object().unwrap();
    for f in DUMP_FILES {
        assert!(
            old["dump_load"]["fixture_digests"].get(f).is_none(),
            "{f} is new"
        );
        assert_eq!(dumps[f], digest(&fixtures.join("dump").join(f)), "{f}");
    }
}

/// scc2 (F13): the successor adds only sync stream format 4, the minimum rule and the sync state.
/// Every scc2 path is a changed path of the fixture; the registered row of `sync_state` is
/// incompatible at 0.53.0; the purposes gain bytes 2 and 3, each with its root kind; only the
/// incompatible and read-only-compatible classes raise the minimum; and the sync row holds the
/// digest of each `sync4-*` file, as its length and blake3.
#[test]
fn scc2_contract_adds_only_sync4_and_minimums() {
    let (old, new) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(&keyed(&old), &keyed(&new), "", &mut found);
    for (p, _) in SCC2_CHANGES {
        assert!(found.iter().any(|f| f == p), "{p} is a change of {CURRENT}");
    }
    found.sort();
    assert_eq!(found, all_changes(), "no change outside scc1 and scc2");
    let row = new["store"]["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "sync_state")
        .expect("the registered feature");
    assert_eq!(row["class"], "incompatible");
    assert_eq!(
        row["required_rdy"],
        rdy_store::format::SYNC_STATE_REQUIRED_RDY
    );
    assert_eq!(rdy_store::format::SYNC_STATE_REQUIRED_RDY, "0.53.0");
    assert_eq!(
        new["sync"]["purposes"],
        serde_json::json!([
            [0, "tag_history"],
            [1, "link_delivery"],
            [2, "canon_history"],
            [3, "view_history"]
        ])
    );
    let history = &new["sync"]["history"];
    assert_eq!(history["format"], 4);
    assert_eq!(history["read_formats"], serde_json::json!([2, 3, 4]));
    assert_eq!(
        history["roots"],
        serde_json::json!([[0, 2, 9], [1, 3, 9], [2, 4, 4], [3, 4, 13]])
    );
    assert_eq!(
        history["minimum_classes"],
        serde_json::json!(["read_only_compatible", "incompatible"])
    );
    assert_eq!(
        new["errors_warnings"]["templates"]["SYNC_LOCKED_FIX"],
        format!("\"{}\"", rdy_store::sync_state::SYNC_LOCKED_FIX)
    );
    let state = &new["store"]["sync_state"];
    assert_eq!(state["max_value_bytes"], 8192);
    assert_eq!(state["max_url_bytes"], 4096);
    assert_eq!(state["feature"], "sync_state");
    // each new stream fixture file, with its bytes on disk
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sync");
    let digest = |p: &Path| {
        let b = std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        format!("{} {}", b.len(), rdy_model::Hash::of(&b))
    };
    let digests = new["sync"]["fixture_digests"].as_object().unwrap();
    for f in SYNC4_FILES {
        assert!(
            old["sync"]["fixture_digests"].get(f).is_none(),
            "{f} is new"
        );
        assert_eq!(digests[f], digest(&fixtures.join(f)), "{f}");
    }
}
