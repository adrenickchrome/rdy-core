//! scc8 (D-TXFHR; D-H0BZN, D-47HVF, D-4XHPH, D-0SC9C; ruling R-F2): Sync client B's contract
//! fixture. `sync-client-b-v1.json` is the current fixture: it starts as a byte copy of
//! `sync-client-v1.json`, which is frozen from here on. Task 8a1 adds only the sync wire's
//! fixture digest and the exchange's error values: the code `E_DIVERGED` and the texts of
//! `SYNC_DIVERGED`, `SYNC_REMOTE_MODE` and `SYNC_MIN_RDY`. Task 8a2 adds wire fixture v2's
//! digest and the texts of `SYNC_UNCERTAIN`. Every other member is the predecessor's.
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

/// The members Tasks 8a1 and 8a2 add to [`CURRENT`], each with the reason it moves.
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
        "added /errors_warnings/templates/SYNC_UNCERTAIN_FIX",
        "the fix of a sync write with no reply: find its outcome first (D-H0BZN, Task 8a2)",
    ),
    (
        "added /errors_warnings/templates/SYNC_UNCERTAIN_MESSAGE",
        "the text of a sync write with no reply (D-H0BZN, Task 8a2)",
    ),
    (
        "added /sync/fixture_digests/sync-client-wire-v1.json",
        "rdyhub's frozen sync wire fixture v1, a byte copy",
    ),
    (
        "added /sync/fixture_digests/sync-client-wire-v2.json",
        "rdyhub's frozen sync wire fixture v2, a byte copy (Task 8a2); v1 stays beside it",
    ),
];

/// The members Task 9a adds to [`CURRENT`] (D-47HVF, D-2VM48): the chat tools, the command
/// trees, the one error row, and the changed fix of an existing row; then the URL clone's
/// selector, warning and texts (D-D5J18, D-7PYYS). Each has the reason it moves. A list of names
/// that gains an entry changes as a list, one level under its section.
const SCC9_CHANGES: &[(&str, &str)] = &[
    (
        "added /cli/runtime/not_probed_commands/pull",
        "rdy pull needs a hub; cli_sync pins its replies",
    ),
    (
        "added /cli/runtime/not_probed_commands/push",
        "rdy push needs a hub; cli_sync pins its replies",
    ),
    (
        "added /cli/tree/subcommands/name=pull",
        "the pull command and its flags: remote, view, take, strict and the relay flags",
    ),
    (
        "added /cli/tree/subcommands/name=push",
        "the push command and its flags: remote, view, force, restore and the relay flags",
    ),
    (
        "added /errors_warnings/error_rows/row=E_DIVERGED",
        "the chat row of a push whose local canon does not hold the hub tip",
    ),
    (
        "added /mcp/result_fields/rdy_schema/ok/errors?/E_DIVERGED",
        "rdy_schema lists the new error row",
    ),
    (
        "added /mcp/result_fields_not_captured/rdy_pull",
        "rdy_pull needs a hub; mcp_sync pins its replies",
    ),
    (
        "added /mcp/result_fields_not_captured/rdy_push",
        "rdy_push needs a hub; mcp_sync pins its replies",
    ),
    (
        "added /mcp/tools/rdy_pull",
        "the pull tool: its closed schema and description",
    ),
    (
        "added /mcp/tools/rdy_push",
        "the push tool: its closed schema and description",
    ),
    (
        "changed /errors_warnings/error_rows/row=E_UPSTREAM_UNCERTAIN",
        "its fix says what to run after a sync write with no reply",
    ),
    (
        "changed /mcp/refusal_error_rows",
        "the list of refusal codes gains E_DIVERGED",
    ),
    (
        "changed /mcp/tool_names",
        "the list of tool names gains rdy_pull and rdy_push",
    ),
    // the URL clone (D-D5J18, D-7PYYS, D-47HVF, D-FYSG5)
    (
        "added /errors_warnings/codes/W_UPSTREAM_PACK_MISMATCH",
        "a URL clone whose hub repo holds gate settings for another pack uses rdy's default pack",
    ),
    (
        "added /errors_warnings/templates/CLONE_NO_CANON_FIX",
        "the fix of a URL clone of a hub repo with no canon",
    ),
    (
        "added /errors_warnings/templates/CLONE_NO_CANON_MESSAGE",
        "the text of a URL clone of a hub repo with no canon",
    ),
    (
        "added /errors_warnings/templates/CLONE_SETTING_MESSAGE",
        "the text of a hub setting this rdy cannot read, with its capturing rdy",
    ),
    (
        "added /errors_warnings/templates/CLONE_SETTING_NEWER_FIX",
        "the fix of a hub setting from a newer rdy",
    ),
    (
        "added /errors_warnings/templates/CLONE_SETTING_OLDER_FIX",
        "the fix of a hub setting from this rdy or an older one",
    ),
    (
        "added /errors_warnings/templates/CLONE_SETTING_UNNAMED_FIX",
        "the fix of a hub setting that names no capturing rdy",
    ),
    (
        "added /errors_warnings/templates/CLONE_SETTING_UNNAMED_MESSAGE",
        "the text of a hub setting that names no capturing rdy",
    ),
    (
        "added /errors_warnings/templates/CLONE_URL_FILE_FIX",
        "the fix of a clone that names both a hub URL and a dump file",
    ),
    (
        "added /errors_warnings/templates/CLONE_URL_FILE_MESSAGE",
        "the text of a clone that names both a hub URL and a dump file",
    ),
    (
        "added /errors_warnings/templates/CLONE_URL_FORK_FIX",
        "the fix of a fork from a hub URL",
    ),
    (
        "added /errors_warnings/templates/CLONE_URL_FORK_MESSAGE",
        "the text of a fork from a hub URL",
    ),
    (
        "added /errors_warnings/templates/UPSTREAM_PACK_MISMATCH_FIX",
        "the fix of W_UPSTREAM_PACK_MISMATCH",
    ),
    (
        "added /errors_warnings/templates/UPSTREAM_PACK_MISMATCH_MESSAGE",
        "the text of W_UPSTREAM_PACK_MISMATCH",
    ),
    (
        "added /mcp/tools/rdy_clone/input_schema/properties/url",
        "rdy_clone takes an https hub repo URL, exclusive with file",
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
    let later: Vec<&str> = SCC9_CHANGES.iter().map(|(c, _)| *c).collect();
    let found: Vec<String> = changes()
        .into_iter()
        .filter(|c| !later.contains(&c.as_str()))
        .collect();
    assert_eq!(found, want);
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
    let wire2 = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sync/sync-client-wire-v2.json"),
    )
    .unwrap();
    assert_eq!(
        sha(&wire2),
        "ddc3e17a39425ba7af5a7a4be8531fa89e484b2ff82cc4f4b12d8d37418e33e7"
    );
    assert_eq!(
        now["sync"]["fixture_digests"]["sync-client-wire-v2.json"],
        format!("{} {}", wire2.len(), rdy_model::Hash::of(&wire2))
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
        (
            "SYNC_UNCERTAIN_MESSAGE",
            "the sync write of push <push> may have completed",
        ),
        (
            "SYNC_UNCERTAIN_FIX",
            "run rdy push or rdy pull again: rdy finds the outcome of push <push> before it writes again",
        ),
    ] {
        assert_eq!(templates[name], format!("\"{text}\""), "{name}");
    }
}

/// scc9: the successor adds only the sync surfaces of Task 9 ([`SCC9_CHANGES`]) beside Task 8's:
/// the two tools, the two command trees and one error row; then the clone's `url` selector, its
/// warning code and its texts. No whole top-level section moves: each leaf is a member below a
/// section, and no member of an earlier task changes.
#[test]
fn scc9_contract_adds_only_sync_surfaces() {
    let mut want: Vec<String> = SCC8_CHANGES
        .iter()
        .chain(SCC9_CHANGES)
        .map(|(c, _)| c.to_string())
        .collect();
    want.sort();
    assert_eq!(changes(), want);
    for (change, why) in SCC9_CHANGES {
        assert!(!why.is_empty(), "{change} has a reason");
        let path = change.split_once(' ').unwrap().1;
        assert!(path.matches('/').count() >= 2, "{change}");
        assert!(!change.starts_with("removed "), "{change}");
    }
    let now = read(CURRENT);
    // the tools and their schemas
    let tools = now["mcp"]["tool_names"].as_array().unwrap();
    for t in ["rdy_pull", "rdy_push"] {
        assert!(tools.iter().any(|n| n == t), "{t}");
        assert!(now["mcp"]["tools"][t].is_object(), "{t}");
    }
    // the command trees
    let keyed_now = keyed(&now);
    let subs = &keyed_now["cli"]["tree"]["subcommands"];
    for c in ["pull", "push"] {
        assert_eq!(subs[format!("name={c}")]["name"], c);
    }
    // the URL clone: its selector and its texts, exactly
    assert!(now["mcp"]["tools"]["rdy_clone"]["input_schema"]["properties"]["url"].is_object());
    assert_eq!(
        now["errors_warnings"]["codes"]["W_UPSTREAM_PACK_MISMATCH"],
        "\"W_UPSTREAM_PACK_MISMATCH\""
    );
    let templates = &now["errors_warnings"]["templates"];
    for (name, text) in [
        (
            "CLONE_NO_CANON_MESSAGE",
            "hub repo <url> has no canon to clone",
        ),
        (
            "CLONE_NO_CANON_FIX",
            "make the project with rdy init, add this hub repo as a share remote, then push",
        ),
        (
            "CLONE_URL_FORK_MESSAGE",
            "a fork starts from a project or a dump, not from a hub URL",
        ),
        (
            "CLONE_URL_FORK_FIX",
            "clone the hub repo, then run rdy clone --fork <that project> <dir>",
        ),
        (
            "CLONE_URL_FILE_MESSAGE",
            "file is not used by clone with url",
        ),
        ("CLONE_URL_FILE_FIX", "remove file or url"),
        (
            "CLONE_SETTING_MESSAGE",
            "the <member> of hub repo <url> comes from rdy <captured> and does not fit rdy <this>",
        ),
        (
            "CLONE_SETTING_NEWER_FIX",
            "update rdy to <captured> or later, then clone again",
        ),
        (
            "CLONE_SETTING_OLDER_FIX",
            "have the hub repo's next push come from rdy <this>, then clone again",
        ),
        (
            "CLONE_SETTING_UNNAMED_MESSAGE",
            "the <member> of hub repo <url> does not fit rdy <this>",
        ),
        (
            "CLONE_SETTING_UNNAMED_FIX",
            "update rdy, or have the hub repo's next push come from rdy <this>",
        ),
        (
            "UPSTREAM_PACK_MISMATCH_MESSAGE",
            "hub repo <url> holds gate settings for another pack, so this clone uses rdy's default pack",
        ),
        (
            "UPSTREAM_PACK_MISMATCH_FIX",
            "push the project to the hub repo again from a machine that has its pack, then clone again",
        ),
    ] {
        assert_eq!(templates[name], format!("\"{text}\""), "{name}");
    }
}
