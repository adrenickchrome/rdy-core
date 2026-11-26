//! vxs1 (D-TXFHR): the M5a contract fixture. `m5a-v1.json` is the current fixture: it starts as a
//! byte copy of the integrated predecessor `hub-ports-v1.json`, which is frozen from here on,
//! and later captures add M5a's surfaces to it. [`m5a_changes`] lists every member where the
//! successor differs from that predecessor, so a closed list of M5a's changes can be checked
//! against it; this file pins only where the successor starts: the predecessor's bytes, the
//! fixture lineage, the selectors, and that no member of the predecessor is removed.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The integrated predecessor of the current fixture. It is frozen.
const PREDECESSOR: &str = "hub-ports-v1.json";

/// The sha256 of [`PREDECESSOR`] as the base froze it. It never moves: a later contract change
/// is a successor fixture, never an edit of that file.
const HUB_PORTS_V1_SHA: &str = "1ca4ac7975b5518125d3ff65dc119f7e3d46e8305fc018a5425752d22f51fcab";

/// This plan's fixture, which every selector named until its successor [`SUCCESSOR`].
const CURRENT: &str = "m5a-v1.json";

/// The successor of [`CURRENT`], which every selector names now; [`CURRENT`] is frozen since.
const SUCCESSOR: &str = "rdy-0590-v1.json";

/// The contract fixtures in the order each succeeded the one before, ending in [`CURRENT`].
const LINEAGE: [&str; 14] = [
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
    "m5b-v1.json",
    "hub-forks-v1.json",
    PREDECESSOR,
    CURRENT,
];

/// The members of the predecessor the successor may remove, each named exactly.
const ALLOWED_REMOVALS: [&str; 2] = [
    // Task 7: like modes make query optional (D-5VGAS source-required grouping); the schema drops required.
    "removed /mcp/tools/rdy_search/input_schema/required",
    // Task 8: the CLI query is optional with --like or --like-text.
    "removed /cli/tree/subcommands/name=search/args/id=query/required",
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

/// vxs1: the successor starts from the integrated predecessor. `hub-ports-v1.json` keeps its
/// bytes, the contracts folder holds the lineage and its successor `rdy-0590-v1.json` and
/// nothing else, every selector names that successor and none names `m5a-v1.json` or the
/// predecessor, and `m5a-v1.json` removes no member or row of the predecessor.
#[test]
fn vxs1_successor_starts_from_the_integrated_fixture() {
    assert_eq!(
        sha(&bytes(PREDECESSOR)),
        HUB_PORTS_V1_SHA,
        "{PREDECESSOR} is frozen"
    );
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<&str> = LINEAGE.iter().copied().chain([SUCCESSOR]).collect();
    want.sort();
    assert_eq!(
        names, want,
        "the folder holds the fixture lineage and {SUCCESSOR} alone"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{SUCCESSOR}\"")),
            "{file} selects {SUCCESSOR}"
        );
        for earlier in [PREDECESSOR, CURRENT] {
            assert!(
                !text.contains(&format!("contracts/{earlier}\"")),
                "{file} no longer selects {earlier}"
            );
        }
    }
    let removed: Vec<String> = m5a_changes()
        .into_iter()
        .filter(|c| c.starts_with("removed "))
        .filter(|c| !ALLOWED_REMOVALS.contains(&c.as_str()))
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

// ---------------------------------------------------------------------------------------------
// vxs9: the freeze. The successor holds exactly the vector advice and search additions.
// ---------------------------------------------------------------------------------------------

/// The sha256 of [`CURRENT`] as this plan freezes it, after the exact-delta check below.
const M5A_V1_SHA: &str = "b18e1992465210f6af99eabafb0290fb9b12f2288db5d5ac83bfa4f1a44bc4ec";

/// Every member where [`CURRENT`] differs from [`PREDECESSOR`], each with the task that captured
/// it and why. Only briefing, write (and import) receipt and search members: no review member.
const M5A_CHANGES: &[(&str, &str)] = &[
    // the briefing (vxs3, vxs11): vector rows, the skipped count, and the paged reply the cut
    // vector probe gives
    (
        "added /cli/runtime/probes/brief/stdout/docs?",
        "vxs3: the vector probe's briefing lists its docs",
    ),
    (
        "added /cli/runtime/probes/brief/stdout/next?",
        "vxs3: the vector probe's cut briefing has a next page",
    ),
    (
        "added /cli/runtime/probes/brief/stdout/omitted?",
        "vxs3: the cut briefing counts its omitted vector rows",
    ),
    (
        "added /cli/runtime/probes/brief/stdout/truncated?",
        "vxs3: the cut briefing says it is truncated",
    ),
    (
        "added /cli/runtime/probes/brief/stdout/vector_neighbors?",
        "vxs3: briefing vector rows {for, block, score, words, links, doc, summary}",
    ),
    (
        "added /cli/runtime/probes/brief/stdout/vector_neighbors_skipped?",
        "vxs11: a briefing of more than 20 content blocks counts them skipped",
    ),
    (
        "added /mcp/result_fields/rdy_brief/ok/docs?",
        "vxs3: the vector probe's briefing lists its docs",
    ),
    (
        "added /mcp/result_fields/rdy_brief/ok/next?",
        "vxs3: the vector probe's cut briefing has a next page",
    ),
    (
        "added /mcp/result_fields/rdy_brief/ok/omitted?",
        "vxs3: the cut briefing counts its omitted vector rows",
    ),
    (
        "added /mcp/result_fields/rdy_brief/ok/truncated?",
        "vxs3: the cut briefing says it is truncated",
    ),
    (
        "added /mcp/result_fields/rdy_brief/ok/vector_neighbors?",
        "vxs3: briefing vector rows {for, block, score, words, links, doc, summary}",
    ),
    (
        "added /mcp/result_fields/rdy_brief/ok/vector_neighbors_skipped?",
        "vxs11: a briefing of more than 20 content blocks counts them skipped",
    ),
    // the write receipt (vxs4): three rows for each of the first ten created content blocks
    (
        "added /mcp/result_fields/rdy_import md/ok/vector_neighbors",
        "vxs4: an import's receipt lists the vector rows of its created blocks",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op0?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op1?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op2?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op3?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op4?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op5?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op6?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op7?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op8?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op9?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/created/op10?",
        "vxs4: the probe's eleven created claims, op0 to op10",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/vector_neighbors?",
        "vxs4: write vector rows {for, block, score, words, links, doc}",
    ),
    (
        "added /mcp/result_fields/rdy_write/ok/vector_neighbors_skipped?",
        "vxs4: the created content blocks past ten count as skipped",
    ),
    // search (vxs6 to vxs8): relevance, like and like_text, and their refusals
    (
        "added /mcp/result_fields/rdy_search relevance",
        "vxs6: sort relevance gives each hit and each also member a score",
    ),
    (
        "added /mcp/result_fields/rdy_search refused",
        "vxs6: a relevance regex and an unknown sort value are refused",
    ),
    (
        "added /mcp/result_fields/rdy_search like",
        "vxs7: like hits with score, words and links",
    ),
    (
        "added /mcp/result_fields/rdy_search like_text",
        "vxs7: like_text hits with score, words and links",
    ),
    (
        "added /mcp/result_fields/rdy_search like refused",
        "vxs7: the like-mode refusals E_BAD_INPUT, E_LIMIT and E_UNKNOWN_BLOCK",
    ),
    (
        "added /mcp/tools/rdy_search/input_schema/properties/like",
        "vxs7: rdy_search takes like",
    ),
    (
        "added /mcp/tools/rdy_search/input_schema/properties/like_text",
        "vxs7: rdy_search takes like_text",
    ),
    (
        "changed /mcp/tools/rdy_search/input_schema/properties/query/description",
        "vxs7: query says to give exactly one of query, like and like_text",
    ),
    (
        "changed /mcp/tools/rdy_search/input_schema/properties/query/type",
        "vxs7: query may be left out, so it may be null",
    ),
    (
        "changed /mcp/tools/rdy_search/input_schema/properties/sort/description",
        "vxs6: sort names relevance, coverage and reading, with query only",
    ),
    (
        "removed /mcp/tools/rdy_search/input_schema/required",
        "vxs7: no source parameter is required on its own",
    ),
    (
        "added /cli/runtime/probes/search relevance",
        "vxs8: rdy search --sort relevance",
    ),
    (
        "added /cli/runtime/probes/search like",
        "vxs8: rdy search --like",
    ),
    (
        "added /cli/runtime/probes/search like_text",
        "vxs8: rdy search --like-text",
    ),
    (
        "added /cli/runtime/probes/search like refused",
        "vxs8: the like-mode refusals at the command line",
    ),
    (
        "added /cli/tree/subcommands/name=search/args/id=like",
        "vxs8: the --like flag",
    ),
    (
        "added /cli/tree/subcommands/name=search/args/id=like_text",
        "vxs8: the --like-text flag",
    ),
    (
        "changed /cli/tree/subcommands/name=search/groups/id=SearchArgs/args",
        "vxs8: the search arguments group holds like and like_text",
    ),
    (
        "removed /cli/tree/subcommands/name=search/args/id=query/required",
        "vxs8: the query is optional with --like or --like-text",
    ),
];

/// The members a listed change may sit under: the briefing, write, import and search surfaces.
const M5A_SURFACES: [&str; 8] = [
    "/cli/runtime/probes/brief/",
    "/cli/runtime/probes/search ",
    "/cli/tree/subcommands/name=search/",
    "/mcp/result_fields/rdy_brief/",
    "/mcp/result_fields/rdy_write/",
    "/mcp/result_fields/rdy_import md/",
    "/mcp/result_fields/rdy_search ",
    "/mcp/tools/rdy_search/",
];

/// Every scalar and array member of `v` by its keyed path.
fn flat(v: &Value, path: &str, out: &mut std::collections::BTreeMap<String, Value>) {
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

/// vxs9: the current fixture differs from its predecessor in exactly the listed members, and
/// each is a briefing, write receipt or search member. The two removals are the ones
/// [`ALLOWED_REMOVALS`] names; the four changes are the search schema's and the command's. No
/// member names a review, every other leaf keeps its value, no section moved, and the store,
/// dump, sync, identity and error sections are the predecessor's.
#[test]
fn vxs9_m5a_contract_delta_is_exactly_the_listed_additions() {
    let found = m5a_changes();
    let mut want: Vec<String> = M5A_CHANGES.iter().map(|(c, _)| c.to_string()).collect();
    let distinct: BTreeSet<&String> = want.iter().collect();
    assert_eq!(distinct.len(), want.len(), "no member is listed twice");
    want.sort();
    assert_eq!(found, want, "the delta is the listed members and no other");
    let removed: BTreeSet<&str> = M5A_CHANGES
        .iter()
        .map(|(c, _)| *c)
        .filter(|c| c.starts_with("removed "))
        .collect();
    assert_eq!(
        removed,
        ALLOWED_REMOVALS.into_iter().collect::<BTreeSet<_>>(),
        "the removals are the allowed ones"
    );
    let changed = M5A_CHANGES
        .iter()
        .filter(|(c, _)| c.starts_with("changed "))
        .count();
    assert_eq!(changed, 4, "the search schema and the command's group");
    for (change, why) in M5A_CHANGES {
        assert!(why.len() > 10, "{change}: {why}");
        let path = change.split_once(' ').expect("a verb").1;
        let surface = M5A_SURFACES.iter().any(|s| path.starts_with(s));
        assert!(surface, "a briefing, write or search member: {change}");
        assert!(
            !path.to_ascii_lowercase().contains("review"),
            "no vector review member: {change}"
        );
    }
    // every leaf that no listed member touches keeps its value
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    let (mut a, mut b) = (Default::default(), Default::default());
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
    for section in [
        "store",
        "dump_load",
        "sync",
        "derived_identities",
        "inventory_version",
        "errors_warnings",
        "schema_packs",
        "signatures",
        "site_urls",
        "working_copy",
        "markdown_exports",
        "config",
    ] {
        assert_eq!(was[section], now[section], "{section}");
    }
}

/// Every earlier contract fixture, each with the sha256 of its bytes.
const EARLIER: [(&str, &str); 13] = [
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
    (PREDECESSOR, HUB_PORTS_V1_SHA),
];

/// The inherited goldens and released fixtures by folder under the rdy-qa crate, each with the
/// file count and digest ([`tree_digest`]) that the integrated predecessor holds. M5a leaves them
/// as they were; its own goldens are `tests/golden/mcp-m5a` and `fixtures/vectors`.
const KEPT_TREES: [(&str, usize, &str); 12] = [
    (
        "tests/golden/disclosure_review",
        5,
        "dff9652a32b3d52da3938b82c223daefe888a59ff66bf02c03f2c3ed88b50e87",
    ),
    // each page's cursor only: the span maps' manifest entries and the plan answer deltas
    // (0.59.0) moved the log end; every entry is unchanged
    (
        "tests/golden/inbox",
        2,
        "c2a0a6417df7aa76f544c94fba2452834be8f7792139fc1c30df10be5585b830",
    ),
    // less the four goldens that span maps (0.59.0) moved, [`LATER_OWNED`]
    (
        "tests/golden/mcp",
        28,
        "98c0ac186c72928d7d43fc052044cdadaa75b97e1933c5e4fec6d70690db6bf9",
    ),
    (
        "tests/golden/mcp-wasm",
        2,
        "44237f58088959c6fec6a4dff9e47cd28962fc64709fb1ce89eab29289399f1d",
    ),
    (
        "tests/golden/review",
        2,
        "4e032beb5de595f855591fca0b0f9f12e4105742dc3bf11cd78c7a05e1529968",
    ),
    (
        "tests/golden/site",
        10,
        "d151c47db80e1155976b92597b8862ee07e7d06dc1de80079d0cbad4fb767cdc",
    ),
    (
        "tests/golden/site_http",
        12,
        "7aab0c5aec198927d709c1796a1feecca0861d3c666b67db7d6c96a6c3c2a579",
    ),
    (
        "tests/golden/snapshot-delta",
        1,
        "8ac9356f74c0cd39d3e41b472fdb6759228f535b8fb4325df4550a19f9e84220",
    ),
    (
        "tests/golden/wasm",
        1,
        "08946cf1358381bc12b5151927d3db9a9b93048b1397474e8f187cf983e5214e",
    ),
    (
        "fixtures/dump",
        88,
        "9f9a17f7872c45051f73003449c73d07df9a0e86355d0307813622b96d091ea4",
    ),
    (
        "fixtures/store",
        90,
        "974cec98b1ae6594f6e6aba6f15dd4b7ec5c6a35bea0930967d68d7c98010499",
    ),
    (
        "fixtures/sync",
        20,
        "0458f9fae8ba129573af1468590b22d463a13295835d7a097c60dfb72cf2b07d",
    ),
];

/// The paths under a kept tree that a later plan owns, by prefix: span maps' (0.59.0) first-use
/// fixtures of `dedup_spans`, plan deltas' (0.59.0) first-use fixtures of `plan_delta` and the
/// MCP goldens whose hashes, byte counts and cursors plan deltas moved, and the
/// four MCP goldens whose bytes span maps moved (the compact
/// report's `ref_update` and `log_bytes`, and the inbox cursor).
const LATER_OWNED: [&str; 22] = [
    "v4-f1-dedup-spans",
    "dedup-spans-1",
    "v4-f1-reduction",
    "reduction-1",
    "sync4-reduction",
    "rdy_compact--dry-run.json",
    "rdy_compact--unsupported.json",
    "rdy_inbox--page.json",
    "rdy_inbox--person.json",
    "rdy_diff--detail-doc.json",
    "rdy_diff--view.json",
    "rdy_log--page.json",
    "rdy_propose--with-advice.json",
    "rdy_propose--with-consequences.json",
    "rdy_resolve--accept.json",
    "rdy_tensions--canon.json",
    "rdy_view--create.json",
    "rdy_view--exclude.json",
    "rdy_view--fork.json",
    "rdy_view--include.json",
    "rdy_view--show-with-conflict.json",
    "rdy_view--undo.json",
];

/// The files under `dir` less [`LATER_OWNED`], and the sha256 of one line per file in byte order
/// of its `/` path: `<path> <sha256 of its bytes>`.
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
    files.retain(|(path, _)| !LATER_OWNED.iter().any(|later| path.starts_with(later)));
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let lines: String = files
        .iter()
        .map(|(path, bytes)| format!("{path} {}\n", sha(bytes)))
        .collect();
    (files.len(), sha(lines.as_bytes()))
}

/// vxs9: M5a changes no released format or stored identity. Every earlier contract fixture keeps
/// its bytes and [`CURRENT`] has its frozen bytes; the inherited MCP, review, site and module
/// goldens, the released dump fixtures, the native store fixtures and the sync vectors keep
/// theirs; the goldens folder holds those and M5a's own and no other; the vector golden holds no
/// review member; and the contract's store registry, formats and derived identities are the
/// predecessor's.
#[test]
fn vxs9_released_fixtures_and_stored_identities_keep_their_bytes() {
    for (name, want) in EARLIER {
        assert_eq!(sha(&bytes(name)), want, "{name} is frozen");
    }
    assert_eq!(
        sha(&bytes(CURRENT)),
        M5A_V1_SHA,
        "{CURRENT} has its final bytes"
    );
    assert_eq!(
        EARLIER
            .iter()
            .map(|(n, _)| *n)
            .chain([CURRENT])
            .collect::<Vec<_>>(),
        LINEAGE,
        "the hashes follow the lineage"
    );
    let qa = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (tree, count, digest) in KEPT_TREES {
        let got = tree_digest(&qa.join(tree));
        assert_eq!((got.0, got.1.as_str()), (count, digest), "{tree}");
    }
    let mut goldens: Vec<String> = std::fs::read_dir(qa.join("tests/golden"))
        .unwrap()
        .map(|e| format!("tests/golden/{}", e.unwrap().file_name().to_string_lossy()))
        .collect();
    goldens.sort();
    let mut want: Vec<&str> = KEPT_TREES
        .iter()
        .map(|(t, _, _)| *t)
        .filter(|t| t.starts_with("tests/golden/"))
        .chain(["tests/golden/mcp-m5a"])
        .collect();
    want.sort();
    assert_eq!(goldens, want, "the goldens folder");
    // M5a's own goldens: the briefing and the write refusal, no review reply
    let mut own: Vec<String> = std::fs::read_dir(qa.join("tests/golden/mcp-m5a"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    own.sort();
    assert_eq!(
        own,
        [
            "rdy_brief--doc.json",
            "rdy_write--refused-brief-required.json"
        ]
    );
    // the vector golden: weights, constants and store replies only
    let vectors: Value =
        serde_json::from_slice(&std::fs::read(qa.join("fixtures/vectors/m5a-v1.json")).unwrap())
            .unwrap();
    let keys: Vec<&str> = vectors
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "blocks",
            "constants",
            "description",
            "expected",
            "log2",
            "store"
        ]
    );
    let store: Vec<&str> = vectors["store"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(store, ["about", "advice", "like", "relevance"]);
    assert!(
        !vectors.to_string().to_ascii_lowercase().contains("review"),
        "no review member in the vector golden"
    );
    // the store registry, the formats and the derived identities
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    assert_eq!(now["derived_identities"], was["derived_identities"]);
    // the registry less span maps' `dedup_spans` and plan deltas' `plan_delta` (0.59.0), which
    // came later, is what this fixture lists, and they are the only additions
    let live: Vec<&str> = rdy_store::format::REGISTERED_FEATURES
        .iter()
        .copied()
        .filter(|n| *n != "dedup_spans" && *n != "plan_delta")
        .collect();
    assert_eq!(
        now["store"]["registered_names"],
        serde_json::json!(live),
        "the store registers what the contract lists, dedup_spans and plan_delta"
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
    assert_eq!(live.len(), 13);
    assert_eq!(
        rdy_store::format::REGISTERED_FEATURES.len(),
        live.len() + 2,
        "dedup_spans and plan_delta are the only additions"
    );
}

/// The keys of the object at `v`, sorted.
fn keys_of(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v
        .as_object()
        .unwrap_or_else(|| panic!("an object: {v}"))
        .keys()
        .map(String::as_str)
        .collect();
    k.sort();
    k
}

/// vxs9: the captured vector surfaces are populated: the briefing's rows have all seven fields
/// with words and links on both surfaces and its skipped count is a number; the write and import
/// receipts' rows have six, without a summary; `like`, `like_text` and relevance replies carry
/// their scores, and the like modes their words and links; the like-mode refusals are captured
/// with the three codes on MCP and an exit and error on the command line; and no captured member
/// of a review surface holds a vector field.
#[test]
fn vxs9_populated_vector_surfaces_and_refusals_are_captured() {
    let now = read(CURRENT);
    let rf = &now["mcp"]["result_fields"];
    let probes = &now["cli"]["runtime"]["probes"];
    let brief_row = ["block", "doc", "for", "links", "score", "summary", "words"];
    for brief in [&rf["rdy_brief"]["ok"], &probes["brief"]["stdout"]] {
        let row = &brief["vector_neighbors?"][0];
        assert_eq!(keys_of(row), brief_row, "{brief}");
        assert_eq!(row["links"], serde_json::json!(["string"]));
        assert_eq!(row["words"], serde_json::json!(["string"]));
        assert_eq!(row["score"], "number");
        assert_eq!(brief["vector_neighbors_skipped?"], "number");
        assert!(brief["omitted?"].is_object(), "a cut page: {brief}");
    }
    let write_row = &rf["rdy_write"]["ok"]["vector_neighbors?"][0];
    assert_eq!(
        keys_of(write_row),
        ["block", "doc", "for", "links", "score", "words"]
    );
    assert_eq!(write_row["links"], serde_json::json!(["string"]));
    assert_eq!(rf["rdy_write"]["ok"]["vector_neighbors_skipped?"], "number");
    let import_row = &rf["rdy_import md"]["ok"]["vector_neighbors"][0];
    assert_eq!(
        keys_of(import_row),
        ["block", "doc", "for", "links", "score", "words"]
    );
    // the search modes, on both surfaces
    for (name, reply) in [
        ("rdy_search like", &rf["rdy_search like"]["ok"]),
        ("rdy_search like_text", &rf["rdy_search like_text"]["ok"]),
        ("search like", &probes["search like"]["stdout"]),
        ("search like_text", &probes["search like_text"]["stdout"]),
    ] {
        let hit = &reply["matches"][0];
        assert_eq!(hit["score"], "number", "{name}");
        assert_eq!(hit["words"], serde_json::json!(["string"]), "{name}");
        assert_eq!(hit["links"], serde_json::json!(["string"]), "{name}");
        assert_eq!(reply["query"], "string", "{name}: query stays a string");
        let echo = if name.ends_with("like_text") {
            "like_text"
        } else {
            "like"
        };
        assert_eq!(reply[echo], "string", "{name} echoes its source");
    }
    let ranked = &rf["rdy_search relevance"]["ok"]["matches"][0];
    assert_eq!(ranked["score"], "number");
    assert_eq!(ranked["also"][0]["score"], "number");
    assert!(ranked.get("words").is_none(), "relevance has no reasons");
    assert_eq!(
        probes["search relevance"]["stdout"]["matches"][0]["score"],
        "number"
    );
    // the refusals
    assert_eq!(
        rf["rdy_search like refused"]["refusal_codes"],
        serde_json::json!(["E_BAD_INPUT", "E_LIMIT", "E_UNKNOWN_BLOCK"])
    );
    assert_eq!(
        rf["rdy_search refused"]["refusal_codes"],
        serde_json::json!(["E_BAD_INPUT"])
    );
    for refused in [
        &rf["rdy_search like refused"]["refused"],
        &rf["rdy_search refused"]["refused"],
    ] {
        assert_eq!(keys_of(refused), ["code", "fix", "issues", "message"]);
        assert_eq!(refused["issues"], serde_json::json!([]));
    }
    let cli = &probes["search like refused"];
    assert_eq!(cli["exits"], serde_json::json!([1]));
    assert_eq!(keys_of(&cli["stderr"]), ["code", "fix", "message"]);
    // the inputs
    let props = &now["mcp"]["tools"];
    let search = props
        .as_array()
        .and_then(|t| t.iter().find(|t| t["name"] == "rdy_search"))
        .unwrap_or(&props["rdy_search"]);
    let p = &search["input_schema"]["properties"];
    for k in ["query", "like", "like_text", "sort"] {
        assert!(p.get(k).is_some(), "rdy_search takes {k}");
    }
    assert!(search["input_schema"].get("required").is_none());
    // no review surface holds a vector field
    let mut all = Default::default();
    flat(&keyed(&now), "", &mut all);
    let vector_review: Vec<&String> = all
        .keys()
        .filter(|k| k.to_ascii_lowercase().contains("review"))
        .filter(|k| {
            let k = k.to_ascii_lowercase();
            k.contains("vector") || k.contains("like") && k.contains("score")
        })
        .collect();
    assert!(vector_review.is_empty(), "{vector_review:?}");
}
