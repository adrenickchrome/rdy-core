//! scc1 (D-TXFHR; D-DZDB7, D-WWAK7): the Sync client contract fixture. `sync-client-v1.json`
//! is the current fixture: it starts as a byte copy of the shipped `sd1-v1.json`, which is frozen
//! from here on, and adds only the holder's storage surfaces: the registered feature `hub_canon`
//! and its rule, and the digests of its first-use fixtures (the dump pair `v4-f1-e1-hub-canon`
//! and the native holder `store/hub-canon-1`). scc2 (D-KE83X, D-4XHPH, D-3RSCW) then adds sync
//! stream format 4, the minimum rule and the project's sync state: the registered feature
//! `sync_state` and its rule, the sync state refs and bounds, the history purposes and the
//! digests of the `sync4-*` stream fixtures, and the fix text `SYNC_LOCKED_FIX`. scc7 (D-CDQF7) also adds the error values of the
//! portable publication baseline: the code `E_PUBLICATION_BASELINE` and its texts. scc3
//! (D-8ZEHP, D-9YAQS) adds the facts of a signature that a pull replay carried, in the signatures
//! row. scc4 (D-4E32E, D-47HVF) adds the rules of views that travel with a sync: the code
//! `E_VIEW_DIVERGED`, the texts of the view refusals and the sync row's `views` probe. Every
//! other member is the predecessor's.
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

/// The members Task 7 adds to [`CURRENT`], each with the reason it moves: the error values of
/// the portable publication baseline (D-CDQF7) and nothing else.
const SCC7_CHANGES: &[(&str, &str)] = &[
    (
        "added /errors_warnings/codes/E_PUBLICATION_BASELINE",
        "a baseline that does not verify, bind or share a lineage",
    ),
    (
        "added /errors_warnings/templates/BASELINE_BINDING_FIX",
        "the fix of a binding mismatch",
    ),
    (
        "added /errors_warnings/templates/BASELINE_BINDING_MESSAGE",
        "the text of a binding mismatch",
    ),
    (
        "added /errors_warnings/templates/BASELINE_LINEAGE_FIX",
        "the fix of a baseline outside the lineage",
    ),
    (
        "added /errors_warnings/templates/BASELINE_LINEAGE_MESSAGE",
        "the text of a baseline outside the lineage",
    ),
    (
        "added /errors_warnings/templates/BASELINE_TAG_FIX",
        "the fix of a baseline make with no tag",
    ),
    (
        "added /errors_warnings/templates/BASELINE_TAG_MESSAGE",
        "the text of a baseline make with no tag",
    ),
    (
        "added /errors_warnings/templates/BASELINE_VERIFY_FIX",
        "the fix of every baseline reader failure",
    ),
    (
        "added /errors_warnings/templates/BASELINE_VERIFY_MESSAGE",
        "the text of every baseline reader failure",
    ),
];

/// The members Task 3b adds to [`CURRENT`], each with the reason it moves: the carried-signature
/// result fields (D-8ZEHP, D-9YAQS) and nothing else.
const SCC3_CHANGES: &[(&str, &str)] = &[(
    "added /signatures/carried",
    "a signature a pull replay carried: its carried_from member, its statement rule and its CLI line",
)];

/// The members Task 4 adds to [`CURRENT`], each with the reason it moves: the rules of views
/// that travel with a sync (D-4E32E, D-47HVF) and nothing else.
const SCC4_CHANGES: &[(&str, &str)] = &[
    (
        "added /errors_warnings/codes/E_VIEW_DIVERGED",
        "a pull cannot re-chain a view whose unpushed work a record names",
    ),
    (
        "added /errors_warnings/templates/SYNC_FORK_SOURCE_FIX",
        "the fix of a push of a fork whose source the hub does not hold",
    ),
    (
        "added /errors_warnings/templates/SYNC_FORK_SOURCE_MESSAGE",
        "the text of a push of a fork whose source the hub does not hold",
    ),
    (
        "added /errors_warnings/templates/SYNC_UNDO_OWNER_FIX",
        "the fix of an undo that would change the owner of a view with a hub head",
    ),
    (
        "added /errors_warnings/templates/SYNC_UNDO_OWNER_MESSAGE",
        "the text of an undo that would change the owner of a view with a hub head",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_DIVERGED_FIX",
        "the fix of a re-chain that a record or an open proposal blocks",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_DIVERGED_MESSAGE",
        "the text of a re-chain that a record or an open proposal blocks",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_EXISTS_FIX",
        "the fix of a pull that meets a local view with another root",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_EXISTS_MESSAGE",
        "the text of a pull that meets a local view with another root",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_MISSING_FIX",
        "the fix of a pull of a view the hub lacks",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_MISSING_MESSAGE",
        "the text of a pull of a view the hub lacks",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_ROOT_FIX",
        "the fix of a push of a view whose root is not the hub view's",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_ROOT_MESSAGE",
        "the text of a push of a view whose root is not the hub view's",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_TWICE_FIX",
        "the fix of a push or pull that names a view twice",
    ),
    (
        "added /errors_warnings/templates/SYNC_VIEW_TWICE_MESSAGE",
        "the text of a push or pull that names a view twice",
    ),
    (
        "added /sync/views",
        "the synced-view probe: the state ref, the replaced kind, the pull statuses, the push order and each view refusal",
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

/// The changed paths of scc1, scc2, scc3, scc4 and scc7 together, sorted and without repeats: scc1 and
/// scc2 both move `/store/registered_names` and `/store/sources`.
fn all_changes() -> Vec<String> {
    let set: BTreeSet<String> = SCC1_CHANGES
        .iter()
        .chain(SCC2_CHANGES)
        .chain(SCC7_CHANGES)
        .chain(SCC3_CHANGES)
        .chain(SCC4_CHANGES)
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
    // the rows of the predecessor are all there, and only the store, dump, sync, error and
    // signature rows move
    let (Value::Object(was), Value::Object(now)) = (&old, &new) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(
        was.keys().collect::<Vec<_>>(),
        now.keys().collect::<Vec<_>>()
    );
    for (row, v) in was {
        if ![
            "store",
            "dump_load",
            "sync",
            "errors_warnings",
            "signatures",
        ]
        .contains(&row.as_str())
        {
            assert_eq!(&now[row], v, "row {row} moved");
        }
    }
}

/// scc1 (F13): the successor adds only the holder's storage, scc2's changes and the baseline's
/// error values ([`SCC7_CHANGES`]). Every changed path is listed with its reason; the registered row of `hub_canon` is incompatible at 0.53.0; the store row holds
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
    assert_eq!(found, all_changes(), "no change outside the listed tasks");
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

/// scc7: the successor adds only the baseline's error values: the code and the texts of its
/// four refusals, each the engine's own constant.
#[test]
fn scc7_contract_adds_only_baseline_values() {
    use rdy_engine::{
        BASELINE_BINDING_FIX, BASELINE_BINDING_MESSAGE, BASELINE_LINEAGE_FIX,
        BASELINE_LINEAGE_MESSAGE, BASELINE_TAG_FIX, BASELINE_TAG_MESSAGE, BASELINE_VERIFY_FIX,
        BASELINE_VERIFY_MESSAGE, E_PUBLICATION_BASELINE,
    };
    let (old, new) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(
        &keyed(&old["errors_warnings"]),
        &keyed(&new["errors_warnings"]),
        "/errors_warnings",
        &mut found,
    );
    found.sort();
    // the error row also gains scc2's SYNC_LOCKED_FIX and scc4's view refusals
    let want: Vec<String> = all_changes()
        .into_iter()
        .filter(|p| p.contains(" /errors_warnings/"))
        .collect();
    for (p, _) in SCC7_CHANGES {
        assert!(want.iter().any(|w| w == p), "{p}");
    }
    assert_eq!(
        found, want,
        "the error row gains the baseline's values, scc2's fix text and scc4's view refusals alone"
    );
    let e = &new["errors_warnings"];
    // the row holds each constant's source literal, quotes included
    let literal = |x: &str| Value::String(format!("\"{x}\""));
    assert_eq!(
        e["codes"]["E_PUBLICATION_BASELINE"],
        literal(E_PUBLICATION_BASELINE)
    );
    for (name, text) in [
        ("BASELINE_VERIFY_MESSAGE", BASELINE_VERIFY_MESSAGE),
        ("BASELINE_VERIFY_FIX", BASELINE_VERIFY_FIX),
        ("BASELINE_BINDING_MESSAGE", BASELINE_BINDING_MESSAGE),
        ("BASELINE_BINDING_FIX", BASELINE_BINDING_FIX),
        ("BASELINE_LINEAGE_MESSAGE", BASELINE_LINEAGE_MESSAGE),
        ("BASELINE_LINEAGE_FIX", BASELINE_LINEAGE_FIX),
        ("BASELINE_TAG_MESSAGE", BASELINE_TAG_MESSAGE),
        ("BASELINE_TAG_FIX", BASELINE_TAG_FIX),
    ] {
        assert_eq!(e["templates"][name], literal(text), "{name}");
    }
    assert_eq!(
        BASELINE_VERIFY_MESSAGE,
        "The publication baseline does not verify."
    );
}

/// scc3 (D-8ZEHP, D-9YAQS, ruling task-3b): the successor adds only the facts of a carried
/// signature to the signatures row: `carried_from` holds the full hash of the landing on the
/// replay chain whose base the statement names, the statement matches, and the CLI line names
/// the first 12 characters. Every other member of the row is the predecessor's.
#[test]
fn scc3_contract_adds_only_replay_signature_facts() {
    let (old, new) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(
        &keyed(&old["signatures"]),
        &keyed(&new["signatures"]),
        "/signatures",
        &mut found,
    );
    let want: Vec<String> = SCC3_CHANGES.iter().map(|(p, _)| (*p).to_string()).collect();
    assert_eq!(
        found, want,
        "the signatures row gains the carried facts alone"
    );
    let carried = &new["signatures"]["carried"];
    assert_eq!(carried["result_member"], "carried_from");
    // ruling task-3b: the check follows the whole replay chain
    assert!(carried.get("levels").is_none());
    assert!(
        carried["chain"]
            .as_str()
            .unwrap()
            .starts_with("the subject's rebased_from, then each held source's own"),
        "{carried:#}"
    );
    let check = &carried["check"];
    let from = check["carried_from"].as_str().unwrap();
    assert_eq!(from.len(), 64, "the full hash");
    assert_eq!(check["statement"], "match");
    assert_eq!(check["signed"], "valid");
    assert!(check["code"].is_null());
    assert!(
        carried["cli"]
            .as_str()
            .unwrap()
            .ends_with(&format!(", carried from {}", &from[..12])),
        "{carried:#}"
    );
    // an ordinary vector has no carried_from member
    for v in new["signatures"]["vectors"].as_object().unwrap().values() {
        assert!(v.get("carried_from").is_none());
    }
}

/// scc4 (D-4E32E, D-47HVF): the successor adds only the rules of views that travel with a sync:
/// the code `E_VIEW_DIVERGED`, the texts of the view refusals, each the engine's own constant,
/// and the sync row's `views` probe. Every other member of the error and sync rows is the
/// predecessor's or another listed task's.
#[test]
fn scc4_contract_adds_only_synced_view_rules() {
    use rdy_engine::*;
    let (old, new) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(&keyed(&old), &keyed(&new), "", &mut found);
    for (p, _) in SCC4_CHANGES {
        assert!(found.iter().any(|f| f == p), "{p} is a change of {CURRENT}");
    }
    let scc4: Vec<&String> = found
        .iter()
        .filter(|f| {
            f.contains("E_VIEW_DIVERGED") || f.contains("/SYNC_") || f.contains("/sync/views")
        })
        .filter(|f| !f.ends_with("/SYNC_LOCKED_FIX"))
        .collect();
    let want: Vec<&str> = SCC4_CHANGES.iter().map(|(p, _)| *p).collect();
    assert_eq!(scc4, want, "scc4 adds these members alone");
    let e = &new["errors_warnings"];
    let literal = |x: &str| Value::String(format!("\"{x}\""));
    assert_eq!(e["codes"]["E_VIEW_DIVERGED"], literal(E_VIEW_DIVERGED));
    for (name, text) in [
        ("SYNC_VIEW_EXISTS_MESSAGE", SYNC_VIEW_EXISTS_MESSAGE),
        ("SYNC_VIEW_EXISTS_FIX", SYNC_VIEW_EXISTS_FIX),
        ("SYNC_VIEW_ROOT_MESSAGE", SYNC_VIEW_ROOT_MESSAGE),
        ("SYNC_VIEW_ROOT_FIX", SYNC_VIEW_ROOT_FIX),
        ("SYNC_VIEW_DIVERGED_MESSAGE", SYNC_VIEW_DIVERGED_MESSAGE),
        ("SYNC_VIEW_DIVERGED_FIX", SYNC_VIEW_DIVERGED_FIX),
        ("SYNC_FORK_SOURCE_MESSAGE", SYNC_FORK_SOURCE_MESSAGE),
        ("SYNC_FORK_SOURCE_FIX", SYNC_FORK_SOURCE_FIX),
        ("SYNC_UNDO_OWNER_MESSAGE", SYNC_UNDO_OWNER_MESSAGE),
        ("SYNC_UNDO_OWNER_FIX", SYNC_UNDO_OWNER_FIX),
        ("SYNC_VIEW_TWICE_MESSAGE", SYNC_VIEW_TWICE_MESSAGE),
        ("SYNC_VIEW_TWICE_FIX", SYNC_VIEW_TWICE_FIX),
        ("SYNC_VIEW_MISSING_MESSAGE", SYNC_VIEW_MISSING_MESSAGE),
        ("SYNC_VIEW_MISSING_FIX", SYNC_VIEW_MISSING_FIX),
    ] {
        assert_eq!(e["templates"][name], literal(text), "{name}");
    }
    let views = &new["sync"]["views"];
    assert_eq!(views["state_ref"], "meta/sync/<remote>/view/<name>");
    assert_eq!(views["replaced_kind"], 1);
    assert_eq!(views["push_order"]["sent"], serde_json::json!(["v", "f"]));
    let r = &views["refusals"];
    assert_eq!(
        r["diverged_proposal"][1],
        "view v cannot re-chain because P-0123456789ab@1 names its unpushed work"
    );
    assert_eq!(r["pull_root"][2], "run rdy pull --take --view v");
    assert_eq!(
        r["push_root"][2],
        "fork the view to another name, then push that view"
    );
    assert_eq!(
        r["push_unknown"],
        serde_json::json!([
            "E_NOT_FOUND",
            "no view \"nope\"",
            "list views with rdy view ls (MCP: rdy_view list)"
        ])
    );
}
