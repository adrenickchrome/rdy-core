//! r5901 (D-TXFHR, D-E30RA, D-KQ4CA, D-8QV9G): the contract fixture of release 0.59.0.
//!
//! `rdy-0590-v1.json` is the current fixture. It started as a byte copy of the frozen
//! `hub-ports-v1.json`, and every selector names it. The first capture, taken with the integrated
//! agent text, wrote no leaf: the fixture holds shapes, codes and probes, and the changed tool
//! descriptions and refusal fixes sit outside them. Later tasks of the release add their own
//! leaves. M5a's `m5a-v1.json`, frozen when M5a merged into this release, is the predecessor
//! since: a re-capture on the merged code carries M5a's surfaces, and the delta from M5a is this
//! release's own. The tests here keep every earlier fixture as it was, pin that delta, and pin that
//! the agent delta is descriptive only: no parameter type, required set, code or result shape moves.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The predecessor of this plan's fixture: M5a's fixture, frozen from here on.
const PREDECESSOR: &str = "m5a-v1.json";

/// The sha256 of [`PREDECESSOR`] as M5a froze it. It never moves: a later contract change is a
/// successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "b18e1992465210f6af99eabafb0290fb9b12f2288db5d5ac83bfa4f1a44bc4ec";

/// The fixture this plan's fixture started from, and M5a's predecessor. It is frozen.
const STARTED_FROM: &str = "hub-ports-v1.json";

/// The sha256 of [`STARTED_FROM`], its final bytes.
const STARTED_FROM_SHA: &str = "1ca4ac7975b5518125d3ff65dc119f7e3d46e8305fc018a5425752d22f51fcab";

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
const PREVIOUSLY_SELECTED: [&str; 9] = [
    "repo-links-v1.json",
    "readme-from-rdy-v1.json",
    "disclosure-review-v1.json",
    "sync-client-v1.json",
    "sync-client-b-v1.json",
    "m5b-v1.json",
    "hub-forks-v1.json",
    "hub-ports-v1.json",
    "m5a-v1.json",
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
    (STARTED_FROM, STARTED_FROM_SHA),
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

/// Every member where `now` differs from `was`, keyed by its path, as `added|removed|changed`
/// with the member's value in `now` (null for a removal).
fn leaves(was: &Value, now: &Value, path: &str, out: &mut BTreeMap<String, (String, Value)>) {
    match (was, now) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for k in keys {
                let at = format!("{path}/{k}");
                match (a.get(k), b.get(k)) {
                    (None, Some(y)) => {
                        out.insert(at, ("added".into(), y.clone()));
                    }
                    (Some(_), None) => {
                        out.insert(at, ("removed".into(), Value::Null));
                    }
                    (Some(x), Some(y)) => leaves(x, y, &at, out),
                    (None, None) => unreachable!("the key is in one of the objects"),
                }
            }
        }
        _ if was != now => {
            out.insert(path.to_string(), ("changed".into(), now.clone()));
        }
        _ => {}
    }
}

/// The keyed delta from fixture `was` to fixture `now`.
fn delta(was: &str, now: &str) -> BTreeMap<String, (String, Value)> {
    let mut out = BTreeMap::new();
    leaves(&keyed(&read(was)), &keyed(&read(now)), "", &mut out);
    out
}

/// This release's closed contract delta so far, from its predecessor: span maps' (`dedup_spans`)
/// leaves, and plan deltas' (`plan_delta`): the feature, its activation by a PLAN of encoding 1,
/// its rule and its first-use dump and sync fixtures; and the refusal texts of the recovery task:
/// four error rows and two templates (`r5909_successor_contract_changes_only_refusal_text`); and
/// a partial history's codes ([`HISTORY_CODES`]); and the eight new templates of the overlapped
/// upload preparation and the canon input check, whose start or worker failures a push reports
/// (`UPLOAD_*` and `CAPTURE_*`); and a partial history's store feature (`partial_history`, in
/// the feature list and the registered names) and its sync history format 5: the canon-state
/// purpose, its root, its read format, its wire fixture v6 and its first-use stream fixture.
const RELEASE_DELTA: [&str; 51] = [
    "added /cli/runtime/probes/compact/stdout/deduplicated_bytes",
    "added /cli/runtime/probes/fsck/stdout/deduplicated_bytes",
    "added /cli/runtime/probes/status/stdout/deduplicated_bytes",
    "added /dump_load/fixture_digests/v4-f1-dedup-spans.expect.json",
    "added /dump_load/fixture_digests/v4-f1-dedup-spans.rdydump",
    "added /dump_load/fixture_digests/v4-f1-reduction-chain-1.rdydump",
    "added /dump_load/fixture_digests/v4-f1-reduction-chain-2.rdydump",
    "added /dump_load/fixture_digests/v4-f1-reduction-chain-full.rdydump",
    "added /dump_load/fixture_digests/v4-f1-reduction-chain.expect.json",
    "added /dump_load/fixture_digests/v4-f1-reduction.expect.json",
    "added /dump_load/fixture_digests/v4-f1-reduction.rdydump",
    "added /errors_warnings/templates/CAPTURE_FAILED_FIX",
    "added /errors_warnings/templates/CAPTURE_FAILED_MESSAGE",
    "added /errors_warnings/templates/CAPTURE_START_FIX",
    "added /errors_warnings/templates/CAPTURE_START_MESSAGE",
    "added /errors_warnings/templates/UPLOAD_FAILED_FIX",
    "added /errors_warnings/templates/UPLOAD_FAILED_MESSAGE",
    "added /errors_warnings/templates/UPLOAD_START_FIX",
    "added /errors_warnings/templates/UPLOAD_START_MESSAGE",
    "added /mcp/result_fields/rdy_compact/ok/deduplicated_bytes",
    "added /store/activation/kind 24 encoding 1",
    "added /store/creation_baseline/features/name=dedup_spans",
    "changed /store/creation_baseline_text",
    "added /store/feature_rules/dedup_spans",
    "added /store/feature_rules/plan_delta",
    "added /store/features/name=dedup_spans",
    "added /store/features/name=plan_delta",
    "changed /store/registered_names",
    "added /sync/fixture_digests/sync4-reduction.bin",
    "added /sync/fixture_digests/sync4-reduction.expect.json",
    "changed /errors_warnings/error_rows/row=E_CONFLICT_UNRESOLVED",
    "changed /errors_warnings/error_rows/row=E_DEFERRAL_OPEN",
    "changed /errors_warnings/error_rows/row=E_EXCEPTION_OPEN",
    "changed /errors_warnings/error_rows/row=E_PLAN_ITEM",
    "changed /errors_warnings/templates/HUMAN_RELAY_FIX",
    "changed /errors_warnings/templates/PLAN_ITEM_FIX",
    "added /errors_warnings/codes/E_HISTORY_PENDING",
    "added /errors_warnings/codes/W_HISTORY_PENDING",
    "added /errors_warnings/error_rows/row=E_HISTORY_PENDING",
    "added /errors_warnings/error_rows/row=W_HISTORY_PENDING",
    "added /errors_warnings/templates/HISTORY_PENDING_FIX",
    "changed /mcp/refusal_error_rows",
    "added /mcp/result_fields/rdy_schema/ok/errors?/E_HISTORY_PENDING",
    "added /mcp/result_fields/rdy_schema/ok/errors?/W_HISTORY_PENDING",
    "added /store/features/name=partial_history",
    "added /sync/fixture_digests/sync-client-wire-v6.json",
    "added /sync/fixture_digests/sync5-canon-state.bin",
    "added /sync/fixture_digests/sync5-canon-state.expect.json",
    "changed /sync/history/read_formats",
    "changed /sync/history/roots",
    "changed /sync/purposes",
];

/// The codes a partial history adds (D-GGTYA): E_HISTORY_PENDING and W_HISTORY_PENDING, each with
/// its code, its error row and its `rdy_schema` entry, and their one fix template. These are the
/// only codes the release adds.
const HISTORY_CODES: [&str; 2] = ["E_HISTORY_PENDING", "W_HISTORY_PENDING"];

/// The shared fix of [`HISTORY_CODES`].
const HISTORY_FIX: &str = "wait for the clone to finish, or run rdy pull to resume it";

/// r5901: M5a's fixture is the predecessor of this plan's fixture. It keeps M5a's frozen bytes,
/// it follows the fixture this plan's fixture started from, every earlier fixture keeps its
/// bytes, and every selector still names this plan's fixture.
#[test]
fn r5901_m5a_is_the_successor_predecessor() {
    assert_eq!(PREDECESSOR, "m5a-v1.json");
    assert_eq!(
        sha(&bytes(PREDECESSOR)),
        PREDECESSOR_SHA,
        "{PREDECESSOR} is frozen"
    );
    assert_eq!(
        FROZEN.last().copied(),
        Some((PREDECESSOR, PREDECESSOR_SHA)),
        "the predecessor is the last frozen fixture"
    );
    assert_eq!(
        FROZEN[FROZEN.len() - 2],
        (STARTED_FROM, STARTED_FROM_SHA),
        "M5a follows the fixture this plan's fixture started from"
    );
    for (name, want) in FROZEN {
        assert_eq!(&sha(&bytes(name)), want, "{name} is frozen");
    }
    assert_ne!(
        sha(&bytes(SELECTED)),
        PREDECESSOR_SHA,
        "{SELECTED} is a successor"
    );
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{SELECTED}\"")),
            "{file} selects {SELECTED}"
        );
        assert!(
            !text.contains(&format!("contracts/{PREDECESSOR}\"")),
            "{file} no longer selects {PREDECESSOR}"
        );
    }
}

/// r5901: M5a's surfaces survive the re-freeze. Every member where M5a's fixture differs from
/// the fixture it followed (the vector briefing, write and import rows and the search inputs,
/// modes and relevance) holds the same value in this plan's fixture, and the only delta from
/// M5a is [`RELEASE_DELTA`]: no input mode, output member, error code or format vanishes.
#[test]
fn r5901_m5a_surfaces_survive_refreeze() {
    let now = keyed(&read(SELECTED));
    let m5a = delta(STARTED_FROM, PREDECESSOR);
    assert!(!m5a.is_empty(), "M5a has a delta");
    for (path, (kind, value)) in &m5a {
        let here = path
            .split('/')
            .filter(|p| !p.is_empty())
            .try_fold(&now, |cur, k| cur.get(k));
        if kind == "removed" {
            assert!(here.is_none(), "{path} stays removed");
        } else {
            assert_eq!(here, Some(value), "{kind} {path} survives");
        }
    }
    let mut ours: Vec<String> = delta(PREDECESSOR, SELECTED)
        .into_iter()
        .map(|(path, (kind, _))| format!("{kind} {path}"))
        .collect();
    ours.sort();
    let mut want: Vec<String> = RELEASE_DELTA.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(ours, want, "the delta from M5a is this release's own");
    let (Value::Object(was), Value::Object(now)) = (read(PREDECESSOR), read(SELECTED)) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(
        was.keys().collect::<Vec<_>>(),
        now.keys().collect::<Vec<_>>(),
        "every row of {PREDECESSOR} is there, in order"
    );
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
/// in the compact report, a partial history's two codes (their refusal rows, `rdy_schema`
/// entries, codes, error rows and fix template), and the eight new upload and canon-input
/// templates. Nothing else may move.
const ADDED_LEAVES: &[&str] = &[
    "length /mcp/refusal_error_rows",
    "member /mcp/result_fields/rdy_compact/ok/deduplicated_bytes",
    "member /mcp/result_fields/rdy_schema/ok/errors?/E_HISTORY_PENDING",
    "member /mcp/result_fields/rdy_schema/ok/errors?/W_HISTORY_PENDING",
    "member /errors_warnings/codes/E_HISTORY_PENDING",
    "member /errors_warnings/codes/W_HISTORY_PENDING",
    "length /errors_warnings/error_rows",
    "member /errors_warnings/templates/CAPTURE_FAILED_FIX",
    "member /errors_warnings/templates/CAPTURE_FAILED_MESSAGE",
    "member /errors_warnings/templates/CAPTURE_START_FIX",
    "member /errors_warnings/templates/CAPTURE_START_MESSAGE",
    "member /errors_warnings/templates/HISTORY_PENDING_FIX",
    "member /errors_warnings/templates/UPLOAD_FAILED_FIX",
    "member /errors_warnings/templates/UPLOAD_FAILED_MESSAGE",
    "member /errors_warnings/templates/UPLOAD_START_FIX",
    "member /errors_warnings/templates/UPLOAD_START_MESSAGE",
];

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

/// The exact refusal text the fixture holds now where it differs from the predecessor: four
/// error rows (code, then fix) and two templates. The tool descriptions and the fixes that the
/// engine builds with their values are not in the fixture; their texts are pinned where they are
/// made (`r5909_*` in rdy-engine, the mcp and cli areas).
const REFUSAL_TEXT: [(&str, &str); 6] = [
    (
        "/errors_warnings/error_rows/row=E_CONFLICT_UNRESOLVED/2",
        "read the conflict with rdy_context; update an editable block with base, or take the landed side of a frozen decision with rdy_view resolve and landed true",
    ),
    (
        "/errors_warnings/error_rows/row=E_DEFERRAL_OPEN/2",
        "answer the deferral card first: <ref>. Then relay the approval.",
    ),
    (
        "/errors_warnings/error_rows/row=E_EXCEPTION_OPEN/2",
        "answer these exception cards first, in order: <refs>. Then relay the approval.",
    ),
    (
        "/errors_warnings/error_rows/row=E_PLAN_ITEM/2",
        "read the plan again after each write. Check counts.open and read every page before answering. An already_written consequence may have no item. One unmatched item or group refuses the whole call.",
    ),
    (
        "/errors_warnings/templates/HUMAN_RELAY_FIX",
        "\"if you are the person, drop the relay flags. If you relay for the person, set RDY_ACTOR=agent:<name> to your own actor and pass that actor in --relayed-by with the person's words in --evidence\"",
    ),
    (
        "/errors_warnings/templates/PLAN_ITEM_FIX",
        "\"read the plan again after each write. Check counts.open and read every page before answering. An already_written consequence may have no item. One unmatched item or group refuses the whole call.\"",
    ),
];

/// The value at `path` of the keyed fixture `v`.
fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('/')
        .filter(|p| !p.is_empty())
        .try_fold(v, |cur, k| match cur {
            Value::Array(items) => k.parse::<usize>().ok().and_then(|i| items.get(i)),
            other => other.get(k),
        })
}

/// r5909: against the frozen predecessor, the successor keeps every tool name, every argument and
/// required set and every error code, and its result fields differ only by the compact report's
/// `deduplicated_bytes`. The text that changed is the exact table above; the plan-delta and
/// span-map rows are those of [`RELEASE_DELTA`]. Every predecessor digest stays (checked by
/// `r5901_successor_keeps_every_frozen_fixture`).
#[test]
fn r5909_successor_contract_changes_only_refusal_text() {
    let (was, now) = (keyed(&read(PREDECESSOR)), keyed(&read(SELECTED)));
    // the tool names, the argument names and required sets of every tool
    assert_eq!(at(&was, "/mcp/tool_names"), at(&now, "/mcp/tool_names"));
    let (tools_was, tools_now) = (
        at(&was, "/mcp/tools").unwrap(),
        at(&now, "/mcp/tools").unwrap(),
    );
    assert_eq!(
        tools_was.as_object().unwrap().keys().collect::<Vec<_>>(),
        tools_now.as_object().unwrap().keys().collect::<Vec<_>>()
    );
    for (name, tool) in tools_was.as_object().unwrap() {
        let other = &tools_now[name];
        let args = |t: &Value| -> Vec<String> {
            t["input_schema"]["properties"]
                .as_object()
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default()
        };
        assert_eq!(args(tool), args(other), "{name}: argument names");
        assert_eq!(
            tool["input_schema"]["required"], other["input_schema"]["required"],
            "{name}: required set"
        );
    }
    // every error code stays, and the only new ones are a partial history's two
    let mut codes = at(&was, "/errors_warnings/codes").unwrap().clone();
    for code in HISTORY_CODES {
        codes[code] = Value::String(format!("{code:?}"));
    }
    assert_eq!(Some(&codes), at(&now, "/errors_warnings/codes"));
    let mut refusals: Vec<Value> = at(&was, "/mcp/refusal_error_rows")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    let after = refusals
        .iter()
        .position(|c| c == "E_STORE_READ_ONLY")
        .expect("the read-only row")
        + 1;
    for (i, code) in HISTORY_CODES.iter().enumerate() {
        refusals.insert(after + i, Value::String(code.to_string()));
    }
    assert_eq!(
        Some(&Value::Array(refusals)),
        at(&now, "/mcp/refusal_error_rows")
    );
    for code in HISTORY_CODES {
        let row = at(&now, &format!("/errors_warnings/error_rows/row={code}")).unwrap();
        assert_eq!(row[2], HISTORY_FIX, "{code}");
    }
    assert_eq!(
        at(&now, "/errors_warnings/templates/HISTORY_PENDING_FIX"),
        Some(&Value::String(format!("{HISTORY_FIX:?}")))
    );
    // the result fields: the compact report's span-map byte count and the two codes' schema
    // entries are new
    let fields: Vec<String> = delta(PREDECESSOR, SELECTED)
        .into_iter()
        .filter(|(path, _)| path.starts_with("/mcp/result_fields"))
        .map(|(path, (kind, _))| format!("{kind} {path}"))
        .collect();
    assert_eq!(
        fields,
        [
            "added /mcp/result_fields/rdy_compact/ok/deduplicated_bytes",
            "added /mcp/result_fields/rdy_schema/ok/errors?/E_HISTORY_PENDING",
            "added /mcp/result_fields/rdy_schema/ok/errors?/W_HISTORY_PENDING",
        ]
    );
    // the text that changed is exactly the table, and every other leaf of the three subtrees
    // that differs is one the release's own rows name
    let changed: Vec<(String, Value)> = delta(PREDECESSOR, SELECTED)
        .into_iter()
        .filter(|(path, (kind, _))| kind == "changed" && path.starts_with("/errors_warnings"))
        .map(|(path, (_, value))| (path, value))
        .collect();
    let rows: Vec<&str> = changed.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        rows,
        [
            "/errors_warnings/error_rows/row=E_CONFLICT_UNRESOLVED",
            "/errors_warnings/error_rows/row=E_DEFERRAL_OPEN",
            "/errors_warnings/error_rows/row=E_EXCEPTION_OPEN",
            "/errors_warnings/error_rows/row=E_PLAN_ITEM",
            "/errors_warnings/templates/HUMAN_RELAY_FIX",
            "/errors_warnings/templates/PLAN_ITEM_FIX",
        ]
    );
    for (path, want) in REFUSAL_TEXT {
        let got = at(&now, path).unwrap_or_else(|| panic!("{path}"));
        assert_eq!(got, &Value::String(want.into()), "{path}");
        // the message of a row, and the code, are the predecessor's
        if path.ends_with("/2") {
            let row = path.trim_end_matches("/2");
            assert_eq!(at(&was, &format!("{row}/0")), at(&now, &format!("{row}/0")));
            assert_eq!(at(&was, &format!("{row}/1")), at(&now, &format!("{row}/1")));
        }
    }
    // the plan-delta and span-map rows are the release's own: the fixture's delta is the list
    let mut ours: Vec<String> = delta(PREDECESSOR, SELECTED)
        .into_iter()
        .map(|(path, (kind, _))| format!("{kind} {path}"))
        .collect();
    ours.sort();
    let mut want: Vec<String> = RELEASE_DELTA.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(ours, want);
    assert!(
        RELEASE_DELTA.iter().any(|d| d.contains("plan_delta"))
            && RELEASE_DELTA.iter().any(|d| d.contains("dedup_spans"))
    );
}
