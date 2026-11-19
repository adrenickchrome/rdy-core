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
    // the pull's refused lookup of a held push (D-H67EN)
    (
        "added /errors_warnings/codes/W_UPSTREAM_HELD_LOOKUP",
        "a pull whose lookup of the held push the hub refused keeps held and goes on (D-H67EN)",
    ),
    (
        "added /errors_warnings/error_rows/row=W_UPSTREAM_HELD_LOOKUP",
        "the errors reference lists the held-lookup warning (D-H67EN)",
    ),
    (
        "added /errors_warnings/templates/UPSTREAM_HELD_LOOKUP_FIX",
        "the fix of W_UPSTREAM_HELD_LOOKUP for a hub code with no fix",
    ),
    (
        "added /errors_warnings/templates/UPSTREAM_HELD_LOOKUP_MESSAGE",
        "the text of W_UPSTREAM_HELD_LOOKUP",
    ),
    (
        "added /mcp/result_fields/rdy_schema/ok/errors?/W_UPSTREAM_HELD_LOOKUP",
        "rdy_schema lists the held-lookup warning row",
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
/// warning code and its texts; then the pull's held-lookup warning (D-H67EN). No whole top-level section moves: each leaf is a member below a
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
        (
            "UPSTREAM_HELD_LOOKUP_MESSAGE",
            "the outcome of held push <push> could not be read: <code>",
        ),
        ("UPSTREAM_HELD_LOOKUP_FIX", "pull again later"),
    ] {
        assert_eq!(templates[name], format!("\"{text}\""), "{name}");
    }
}

// ---------------------------------------------------------------------------------------------
// Task 10: the freeze. The successor holds the union of Tasks 8 and 9 and nothing else.
// ---------------------------------------------------------------------------------------------

/// Every scalar and array member of `v` by its keyed path, so one leaf compares to the same leaf.
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

fn flattened(name: &str) -> std::collections::BTreeMap<String, Value> {
    let mut out = std::collections::BTreeMap::new();
    flat(&keyed(&read(name)), "", &mut out);
    out
}

/// The path of a change (`added /a/b` is `/a/b`).
fn path_of(change: &str) -> &str {
    change.split_once(' ').expect("a change has a verb").1
}

/// Every leaf that Tasks 8 and 9 own, with its reason.
fn owned() -> Vec<(&'static str, &'static str)> {
    SCC8_CHANGES.iter().chain(SCC9_CHANGES).copied().collect()
}

/// scc10: the successor differs from the predecessor in exactly the union of the owned leaves:
/// each owned leaf changed and no other, no leaf of the predecessor was dropped or moved, no owned
/// leaf is a whole section, and no leaf is owned twice.
#[test]
fn scc10_contract_delta_is_exactly_the_owned_leaves() {
    let owned = owned();
    let mut want: Vec<String> = owned.iter().map(|(c, _)| c.to_string()).collect();
    let distinct: BTreeSet<&String> = want.iter().collect();
    assert_eq!(distinct.len(), want.len(), "no leaf is owned by two tasks");
    want.sort();
    let found = changes();
    assert_eq!(found, want, "the delta is the owned leaves and no other");
    for (change, why) in &owned {
        assert!(why.len() > 10, "{change}: {why}");
        assert!(!change.starts_with("removed "), "{change}");
        // a leaf below a section, never the section or a whole row
        assert!(path_of(change).matches('/').count() >= 2, "{change}");
    }
    // every leaf that no owned change touches keeps its value
    let (was, now) = (flattened(PREDECESSOR), flattened(CURRENT));
    let touched: Vec<&str> = found.iter().map(|c| path_of(c)).collect();
    let mut kept = 0;
    for (leaf, value) in &was {
        if touched
            .iter()
            .any(|t| leaf == t || leaf.starts_with(&format!("{t}/")))
        {
            continue;
        }
        assert_eq!(now.get(leaf), Some(value), "{leaf} keeps its value");
        kept += 1;
    }
    assert!(kept > 1000, "the check saw the whole predecessor: {kept}");
    // no section moved: every top-level section still holds most of what it held
    let (Value::Object(a), Value::Object(b)) = (read(PREDECESSOR), read(CURRENT)) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for section in a.keys() {
        let prefix = format!("/{section}/");
        let before = was.keys().filter(|l| l.starts_with(&prefix)).count();
        let held = was
            .keys()
            .filter(|l| l.starts_with(&prefix))
            .filter(|l| now.get(*l) == was.get(*l))
            .count();
        assert!(
            before == 0 || held * 10 >= before * 9,
            "section {section} keeps its leaves: {held} of {before}"
        );
    }
}

/// The sha256 of each wire fixture, in one place: rdyhub recaptures a wire fixture when its core
/// pin moves, and then only these digests and the `rdy` strings change.
const WIRE_V1_SHA: &str = "18ade8e2c1a301d780270f20fe49fc116110a6aa512baf624d768ef5ba39711f";
const WIRE_V2_SHA: &str = "ddc3e17a39425ba7af5a7a4be8531fa89e484b2ff82cc4f4b12d8d37418e33e7";

/// The contract fixtures before this plan, each with the sha256 of the bytes its release shipped.
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
    (PREDECESSOR, PREDECESSOR_SHA),
];

/// The expect files of the released sync streams, each with the sha256 of its bytes.
const STREAM_EXPECTS: &[(&str, &str)] = &[
    (
        "sync4-canon-history.expect.json",
        "be1dd377d2a59c88a1ab11ba6a216dc4e363ffbebbba46cd7c5687d5d3d22ebd",
    ),
    (
        "sync4-view-history.expect.json",
        "e2e80db199bc15b4f106b501a775bda1c45594ab9d3aae1d2b48e43835958350",
    ),
    (
        "v1-s15.expect.json",
        "a4f1e496b0d8ce5643111a4404f088a125f53c4652f4821eafe2c3c7f221cc64",
    ),
    (
        "v2-f1-e0.expect.json",
        "3c9dfd192617e3a908c620ef2dc2b8ca0d0531df69cd0d74edd7792eafd74e3a",
    ),
    (
        "v2-f1-e1-ck.expect.json",
        "751bdf941c45df13cc89b62cc3ea4578b865f3566f9b7c6520383b3af70692e0",
    ),
    (
        "v2-f1-e1-snapshot-delta.expect.json",
        "96d14c71b4eab9f813c6d93a7bb8a78aa4b4c1928839be1e9bfdabcfbb8d19da",
    ),
    (
        "v2-f1-e1.expect.json",
        "07330e0f1ab234f3356317aef18fe0f1e027492bbdcfeddd13add59dec1926bd",
    ),
    (
        "v3-f1-e1-link-delivery.expect.json",
        "7f080ab6c538250b8ce071ef8611daedc5310fe637993476d3044c60cdfae9e4",
    ),
];

fn sync_fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/sync")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// scc10: every earlier contract fixture, `sync-client-v1.json` included, and both wire fixtures
/// and the released stream expect files keep their bytes; the contracts folder holds these and
/// the successor alone, and the successor's wire digests name the wire files as they stand.
#[test]
fn scc10_predecessor_fixtures_keep_their_bytes() {
    for (name, want) in EARLIER {
        assert_eq!(&sha(&bytes(name)), want, "{name} is frozen");
    }
    assert_eq!(sha(&sync_fixture("sync-client-wire-v1.json")), WIRE_V1_SHA);
    assert_eq!(sha(&sync_fixture("sync-client-wire-v2.json")), WIRE_V2_SHA);
    for (name, want) in STREAM_EXPECTS {
        assert_eq!(&sha(&sync_fixture(name)), want, "{name} is frozen");
    }
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<&str> = EARLIER.iter().map(|(n, _)| *n).chain([CURRENT]).collect();
    want.sort();
    assert_eq!(
        names, want,
        "the folder holds the earlier fixtures and {CURRENT}"
    );
    let now = read(CURRENT);
    for (name, bytes) in [
        (
            "sync-client-wire-v1.json",
            sync_fixture("sync-client-wire-v1.json"),
        ),
        (
            "sync-client-wire-v2.json",
            sync_fixture("sync-client-wire-v2.json"),
        ),
    ] {
        assert_eq!(
            now["sync"]["fixture_digests"][name],
            format!("{} {}", bytes.len(), rdy_model::Hash::of(&bytes)),
            "{name}"
        );
    }
    // the predecessor's own digests are unchanged members of the successor
    let (was, now) = (flattened(PREDECESSOR), flattened(CURRENT));
    for (leaf, value) in was.iter().filter(|(l, _)| l.contains("fixture_digests")) {
        assert_eq!(now.get(leaf), Some(value), "{leaf}");
    }
}

/// The source of a probe file, from the workspace root.
fn source(file: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(file);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The files that probe the sync surfaces at run time: the command line and chat tests that run
/// the real binary against a loopback hub, and the two-machine process test.
const CLI_PROBES: [&str; 2] = [
    "crates/rdy-qa/tests/cli/cli_sync.rs",
    "crates/rdy-adapter/tests/sync_e2e.rs",
];
const MCP_PROBES: [&str; 2] = [
    "crates/rdy-qa/tests/mcp/mcp_sync.rs",
    "crates/rdy-adapter/tests/sync_e2e.rs",
];

fn probed(files: &[&str], needle: &str) -> bool {
    files.iter().any(|f| source(f).contains(needle))
}

/// scc10: each new tool, command, flag, parameter and refusal row of this plan has a runtime
/// probe: a test that runs it through the real binary. The probes are text-checked here, so a
/// surface a later task adds without a probe fails this test. The dump-fetch forms wait for the
/// dump task (their probes join then).
#[test]
fn scc10_every_new_surface_has_a_runtime_probe() {
    let now = keyed(&read(CURRENT));
    let mut seen = 0;
    // tools: called over stdio and over HTTP by the two-machine test, and pinned in mcp_sync
    for (change, _) in SCC9_CHANGES {
        let Some(tool) = path_of(change).strip_prefix("/mcp/tools/") else {
            continue;
        };
        if tool.contains('/') {
            continue;
        }
        let name = format!("\"{tool}\"");
        assert!(probed(&MCP_PROBES[..1], &name), "mcp_sync probes {tool}");
        assert!(
            source(MCP_PROBES[1]).contains(&name),
            "the two-machine test probes {tool}"
        );
        // every parameter of the tool is passed by some probe
        let props = now["mcp"]["tools"][tool]["input_schema"]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{tool} has a schema"));
        for p in props.keys() {
            assert!(
                probed(&MCP_PROBES, &format!("\"{p}\"")),
                "{tool} parameter {p} has a probe"
            );
            seen += 1;
        }
    }
    // commands and every flag of each
    for (change, _) in SCC9_CHANGES {
        let Some(cmd) = path_of(change).strip_prefix("/cli/tree/subcommands/name=") else {
            continue;
        };
        assert!(
            probed(&CLI_PROBES[..1], &format!("\"{cmd}\"")),
            "cli_sync probes {cmd}"
        );
        assert!(
            source(CLI_PROBES[1]).contains(&format!("\"{cmd}\"")),
            "the two-machine test probes {cmd}"
        );
        let args = &now["cli"]["tree"]["subcommands"][format!("name={cmd}")]["args"];
        // `keyed` turns the argument rows into an object by their ids
        let args: Vec<&Value> = match args {
            Value::Object(m) => m.values().collect(),
            Value::Array(a) => a.iter().collect(),
            other => panic!("{cmd} has arguments: {other}"),
        };
        for a in args {
            let (Some(long), false) = (a["long"].as_str(), a["global"] == true) else {
                continue;
            };
            if long == "help" {
                continue;
            }
            assert!(
                probed(&CLI_PROBES, &format!("\"--{long}\"")),
                "{cmd} --{long} has a probe"
            );
            seen += 1;
        }
    }
    // refusal rows and codes: a test pins the code
    for (change, _) in SCC8_CHANGES.iter().chain(SCC9_CHANGES) {
        let path = path_of(change);
        let code = path
            .strip_prefix("/errors_warnings/codes/")
            .or_else(|| path.strip_prefix("/errors_warnings/error_rows/row="));
        let Some(code) = code else { continue };
        assert!(
            probed(&CLI_PROBES, code) || probed(&MCP_PROBES, code),
            "{code} has a probe"
        );
        seen += 1;
    }
    // the clone's URL selector, over both clients
    assert!(source("crates/rdy-qa/tests/mcp/mcp_sync.rs").contains("\"url\""));
    assert!(source("crates/rdy-qa/tests/cli/cli_sync.rs").contains("clone"));
    assert!(seen > 20, "the probes were checked: {seen}");
}

/// scc10: the released streams and dumps still read: every released dump fixture reads through
/// to its trailer, and every released sync stream fixture this rdy takes passes the batch check
/// (format 1, refused as released, stays refused).
#[test]
fn scc10_released_streams_and_dumps_remain_readable() {
    use rdy_store::dump::DumpReader;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut dumps = 0;
    for entry in std::fs::read_dir(root.join("dump")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rdydump") {
            continue;
        }
        let (mut reader, _, _) = DumpReader::open(std::fs::File::open(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        while reader
            .next()
            .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()))
            .is_some()
        {}
        assert!(
            reader.trailer().is_some(),
            "{} reads to its trailer",
            path.display()
        );
        dumps += 1;
    }
    assert!(dumps >= 50, "the dump fixtures were read: {dumps}");
    // the sync streams this rdy reads: format 2 tag uploads (before and after snapshot deltas)
    // and format 4 canon and view histories. Format 1 is refused before any record, and a link
    // delivery holds no root tag to check; each has its own pin.
    let mut streams = 0;
    for name in [
        "v2-f1-e0.rdysync",
        "v2-f1-e1-ck.rdysync",
        "v2-f1-e1.rdysync",
        "v2-f1-e1-snapshot-delta.rdysync",
        "sync4-canon-history.bin",
        "sync4-view-history.bin",
    ] {
        let path = root.join("sync").join(name);
        let report =
            rdy_engine::stream_check::check_batch(std::fs::File::open(&path).unwrap(), None)
                .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
        assert!(!report.keys.is_empty(), "{name} holds records");
        streams += 1;
    }
    let one = rdy_engine::stream_check::check_batch(
        std::fs::File::open(root.join("sync/v1-s15.rdysync")).unwrap(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        one.code, "E_STORE_FORMAT",
        "format 1 stays refused as released"
    );
    assert!(streams == 6, "the stream fixtures were read: {streams}");
}
