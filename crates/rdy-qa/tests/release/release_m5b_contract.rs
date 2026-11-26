//! wck1 (D-TXFHR; D-QVTJ9): the contract fixture of the check modules, and the interpreter
//! boundary's place in the dependency graph.
//!
//! `m5b-v1.json` is the current fixture: it started as a byte copy of `sync-client-b-v1.json`,
//! which is frozen from here on, and every selector moves to it. The module rule capture
//! (wck6) is its first change, the module metadata capture (wck7) its second and the example
//! pack's metadata (wck8) its last: wck9 pins its delta from the predecessor to exactly those
//! additions, and every earlier fixture, the built-in pack, the store registry, the dump and
//! store fixtures and the command tree to their bytes. The interpreter is linked through `rdy-check-wasm` alone: no other package of the workspace
//! depends on it, the boundary depends on no rdy crate, and the engine depends on the boundary.
use rdy_qa::fixtures::output_within;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The predecessor of the current fixture, frozen from here on.
const PREDECESSOR: &str = "sync-client-b-v1.json";

/// The sha256 of [`PREDECESSOR`] at this plan's execution base. It never moves: a later contract
/// change is a successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "e65653b123aea1307a9e2ab9728f3b5d18e25b33b8f7ca8aa9fb2ff5a2dcb3e1";

/// This plan's fixture, which every selector named until its successor, the first of [`LATER`].
const CURRENT: &str = "m5b-v1.json";

/// The sha256 of [`CURRENT`] since the example pack metadata capture (wck8), its final bytes.
const CURRENT_SHA: &str = "d00807ccc5ed37448eb81fec8addd240633596f73aa288b1dc54d26256d2fb41";

/// The successors of [`CURRENT`] before [`SELECTED`], in order, each frozen since its own
/// successor, with the sha256 that its successor copied.
const LATER: [(&str, &str); 3] = [
    (
        "hub-forks-v1.json",
        "5cff7aee1f5afb3e1e1e0364fc72fb7367a6acf103b1ad64040818cc13cd3252",
    ),
    (
        "hub-ports-v1.json",
        "1ca4ac7975b5518125d3ff65dc119f7e3d46e8305fc018a5425752d22f51fcab",
    ),
    (
        "m5a-v1.json",
        "b18e1992465210f6af99eabafb0290fb9b12f2288db5d5ac83bfa4f1a44bc4ec",
    ),
];

/// The latest successor of [`CURRENT`], which every selector names now.
const SELECTED: &str = "rdy-0590-v1.json";

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

/// wck1: the integrated predecessor keeps its bytes, this plan's fixture has its captured
/// bytes, and every selector names its latest successor and no earlier fixture.
#[test]
fn wck1_successor_starts_from_the_integrated_fixture() {
    let (old, new) = (bytes(PREDECESSOR), bytes(CURRENT));
    assert_eq!(sha(&old), PREDECESSOR_SHA, "{PREDECESSOR} is frozen");
    assert_eq!(sha(&new), CURRENT_SHA, "{CURRENT} has its captured bytes");
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{SELECTED}\"")),
            "{file} selects {SELECTED}"
        );
        for earlier in [PREDECESSOR, CURRENT]
            .into_iter()
            .chain(LATER.map(|(n, _)| n))
        {
            assert!(
                !text.contains(&format!("contracts/{earlier}\"")),
                "{file} no longer selects {earlier}"
            );
        }
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

// ---------------------------------------------------------------------------------------------
// wck9: the freeze. The successor holds exactly the module-rule additions.
// ---------------------------------------------------------------------------------------------

/// Every leaf where `m5b-v1.json` differs from its predecessor, each with the task that captured
/// it and why.
const M5B_CHANGES: &[(&str, &str)] = &[
    (
        "added /cli/runtime/probes/pack check",
        "wck7: rdy pack check is probed, its module metadata included",
    ),
    (
        "removed /cli/runtime/not_probed_commands/pack",
        "wck7: pack leaves the list of commands with no probe, because it now has one",
    ),
    (
        "added /mcp/result_fields/rdy_schema checks.rules",
        "wck7: rdy_schema's checks.rules lists the module limits and the rule tests",
    ),
    (
        "added /mcp/result_fields/rdy_schema/ok/checks?/rules/module",
        "wck7: the module object of checks.rules and its fixed limits",
    ),
    (
        "added /mcp/result_fields/rdy_schema/ok/checks?/rules/tests",
        "wck7: the two rule tests, require and module",
    ),
    (
        "added /mcp/result_fields/rdy_write module rules",
        "wck6: a write's on_change.checks rows of a module rule",
    ),
    (
        "added /schema_packs/module_rules",
        "wck6 to wck8: module packs, their metadata and the example pack",
    ),
    (
        "added /schema_packs/validation/module_rules",
        "wck6: the closed pack-load refusals of module rules",
    ),
];

fn read(name: &str) -> Value {
    serde_json::from_slice(&bytes(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
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

/// Every scalar and array member of `v` by its keyed path.
fn flat(v: &Value, path: &str, out: &mut BTreeMap<String, Value>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, x) in m {
                flat(x, &format!("{path}/{k}"), out);
            }
        }
        other => {
            out.insert(path.to_string(), other.clone());
        }
    }
}

/// wck9: the current fixture differs from its predecessor in exactly the listed members: each
/// listed member changed and no other; the one removal is `pack` leaving the list of commands
/// with no probe; every other leaf keeps its value; no section moved; and the command tree is the
/// predecessor's.
#[test]
fn wck9_m5b_contract_delta_is_exactly_the_listed_additions() {
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(&keyed(&was), &keyed(&now), "", &mut found);
    found.sort();
    let mut want: Vec<String> = M5B_CHANGES.iter().map(|(c, _)| c.to_string()).collect();
    let distinct: BTreeSet<&String> = want.iter().collect();
    assert_eq!(distinct.len(), want.len(), "no member is listed twice");
    want.sort();
    assert_eq!(found, want, "the delta is the listed members and no other");
    for (change, why) in M5B_CHANGES {
        assert!(why.len() > 10, "{change}: {why}");
        let path = change.split_once(' ').expect("a verb").1;
        assert!(
            path.matches('/').count() >= 2,
            "a member below a section: {change}"
        );
        if change.starts_with("removed ") {
            assert_eq!(path, "/cli/runtime/not_probed_commands/pack");
            assert!(
                found.contains(&"added /cli/runtime/probes/pack check".to_string()),
                "pack has a probe in its place"
            );
        } else {
            assert!(change.starts_with("added "), "{change}");
        }
    }
    // every leaf that no listed member touches keeps its value
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    flat(&keyed(&was), "", &mut a);
    flat(&keyed(&now), "", &mut b);
    let touched: Vec<&str> = found
        .iter()
        .map(|c| c.split_once(' ').expect("a verb").1)
        .collect();
    let mut kept = 0;
    for (leaf, value) in &a {
        if touched
            .iter()
            .any(|t| leaf == t || leaf.starts_with(&format!("{t}/")))
        {
            continue;
        }
        assert_eq!(b.get(leaf), Some(value), "{leaf} keeps its value");
        kept += 1;
    }
    assert!(kept > 1000, "the check saw the whole predecessor: {kept}");
    let (Value::Object(x), Value::Object(y)) = (&was, &now) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(x.keys().collect::<Vec<_>>(), y.keys().collect::<Vec<_>>());
    // the command tree is the predecessor's: M5b adds no command or flag
    assert_eq!(was["cli"]["tree"], now["cli"]["tree"], "the CLI tree");
    // the store, dump, sync and identity sections are the predecessor's
    for section in [
        "store",
        "dump_load",
        "sync",
        "derived_identities",
        "inventory_version",
    ] {
        assert_eq!(was[section], now[section], "{section}");
    }
}

/// Every earlier contract fixture, each with the sha256 of its bytes.
const EARLIER: &[(&str, &str)] = &[
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
    (PREDECESSOR, PREDECESSOR_SHA),
];

/// The sha256 of the built-in pack's file, which M5b leaves as it was.
const BUILTIN_PACK_SHA: &str = "20f0fb8fdaee1aa8b910226740154dc39408c4beca924a5399d4f9325c74cfce";

/// The digest of the released dump fixtures and of the native store fixtures ([`tree_digest`]),
/// which M5b leaves as they were.
const DUMP_FIXTURES_DIGEST: (usize, &str) = (
    86,
    "232ec8e9db5e8f4b582e82c3a4543c42743cdd59d82a36b3d39f75f7fe6a5d8e",
);
const STORE_FIXTURES_DIGEST: (usize, &str) = (
    85,
    "e5c48b33394612a9d4acc0f8325eaaa880953eb9dfa3ae723527b89853443fc8",
);

/// The files under `dir`, and the sha256 of one line per file in byte order of its `/` path:
/// `<path> <sha256 of its bytes>`.
fn tree_digest(dir: &Path) -> (usize, String) {
    fn walk(dir: &Path, at: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if at.is_empty() {
                name
            } else {
                format!("{at}/{name}")
            };
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), &rel, out);
            } else {
                out.push((rel, std::fs::read(e.path()).unwrap()));
            }
        }
    }
    let mut files = vec![];
    walk(dir, "", &mut files);
    // the first-use fixtures of `ported_change` (hub ports), `dedup_spans` (span maps) and
    // `plan_delta` (plan deltas) came after this plan's release
    files.retain(|(path, _)| {
        ![
            "v4-f1-e1-ported-change",
            "ported-change-1",
            "v4-f1-dedup-spans",
            "dedup-spans-1",
            "v4-f1-reduction",
            "reduction-1",
        ]
        .iter()
        .any(|later| path.starts_with(later))
    });
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let lines: String = files
        .iter()
        .map(|(path, bytes)| format!("{path} {}\n", sha(bytes)))
        .collect();
    (files.len(), sha(lines.as_bytes()))
}

/// wck9: M5b changes no released format: every earlier contract fixture keeps its bytes and this
/// plan's fixture has its final bytes, the folder holds those and its successors and no other; the
/// built-in pack's file, the released dump fixtures and the native store fixtures keep their bytes; the store
/// registers the same features at the same formats; and the built-in pack still has no rule.
#[test]
fn wck9_old_packs_formats_and_contract_hashes_stay_equal() {
    for (name, want) in EARLIER {
        assert_eq!(&sha(&bytes(name)), want, "{name} is frozen");
    }
    assert_eq!(
        sha(&bytes(CURRENT)),
        CURRENT_SHA,
        "{CURRENT} has its final bytes"
    );
    for (name, want) in LATER {
        assert_eq!(sha(&bytes(name)), want, "{name} is frozen");
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<&str> = EARLIER
        .iter()
        .chain(LATER.iter())
        .map(|(n, _)| *n)
        .chain([CURRENT, SELECTED])
        .collect();
    want.sort();
    assert_eq!(
        names, want,
        "the folder holds the earlier fixtures, {CURRENT}, its successors and {SELECTED}"
    );

    // the built-in pack, its bytes and the pack the project resolves from them
    let pack_file = root().join("crates/rdy-schema/packs/rdy-standard.yaml");
    assert_eq!(sha(&std::fs::read(&pack_file).unwrap()), BUILTIN_PACK_SHA);
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    assert!(pack.rules.is_empty(), "the built-in pack has no rule");
    let now = read(CURRENT);
    assert_eq!(
        now["schema_packs"]["builtin_packs"],
        read(PREDECESSOR)["schema_packs"]["builtin_packs"]
    );

    // the released dump fixtures and the native store fixtures
    let qa = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dump = tree_digest(&qa.join("fixtures/dump"));
    assert_eq!(
        (dump.0, dump.1.as_str()),
        DUMP_FIXTURES_DIGEST,
        "the dump fixtures"
    );
    let store = tree_digest(&qa.join("fixtures/store"));
    assert_eq!(
        (store.0, store.1.as_str()),
        STORE_FIXTURES_DIGEST,
        "the store fixtures"
    );

    // the store registry and the formats: no feature, store format or dump format is new, but
    // the ones later plans registered (hub ports, `ported_change`; span maps, `dedup_spans`;
    // plan deltas, `plan_delta`; canon states, `partial_history`):
    // the registry less them is what this fixture lists, and they are the only additions
    let live: Vec<&str> = rdy_store::format::REGISTERED_FEATURES
        .iter()
        .copied()
        .filter(|n| {
            *n != "ported_change"
                && *n != "dedup_spans"
                && *n != "plan_delta"
                && *n != "partial_history"
        })
        .collect();
    assert_eq!(
        now["store"]["registered_names"],
        serde_json::json!(live),
        "the store registers what the contract lists, and the later additions"
    );
    assert_eq!(
        live.len() + 4,
        rdy_store::format::REGISTERED_FEATURES.len(),
        "ported_change, dedup_spans, plan_delta and partial_history are the only additions"
    );
    assert_eq!(
        now["dump_load"]["dump_format"],
        rdy_store::dump::DUMP_FORMAT
    );
    assert_eq!(
        (
            rdy_store::format::FEATURE_STORE_FORMAT,
            rdy_store::format::MANIFEST_VERSION,
            rdy_store::dump::DUMP_FORMAT,
        ),
        (19, 1, 4),
        "the store and dump formats"
    );
    assert_eq!(live.len(), 12);
}
