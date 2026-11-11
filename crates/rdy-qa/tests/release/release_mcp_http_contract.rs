//! mht8 (MCP over HTTP; D-TXFHR, D-E30RA, D-69AY3, D-6M0AF): the public contract after MCP over
//! HTTP, and its freeze.
//!
//! Three fixtures hold the contract. `contracts/release-1.json` is the Release 1 baseline and
//! `contracts/repo-links-v1.json` the repo links baseline; both are released and never
//! recaptured. `contracts/mcp-http-v1.json` is the current fixture: the repo links baseline plus
//! the additions of this plan, and every live comparison and capture uses it (`release_contract`).
//! These tests hold the three together:
//!
//! - the current fixture differs from the repo links baseline in the closed list [`CHANGES`], each
//!   change owned by the task that made it, and its bytes are pinned, so a later change to it is
//!   a successor fixture and not an edit ([`mht8_http_contract_capture_is_complete`]);
//! - the current fixture keeps everything either baseline records, but for the few members
//!   [`REWORDED`] names with the reason ([`mht8_release_one_and_repo_links_remain_compatible`]).
//!
//! No tool, error code, warning code, store format or dump format is added: the MCP row is the
//! baseline's, so the tool inventory of Repo links (`rdy_links` included) is the one these
//! tests read.
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

const RELEASE_1: &str = "635f63e24cb0a743892f52944b9b5182f4f013ca75f5491d9e05c120400a6ef6";
const REPO_LINKS: &str = "61b01d9bd9d60fbe8894c32ffc58cab433c42a7c332de7fde8ba00414bf31848";
/// The frozen current fixture.
const MCP_HTTP: &str = "641aad26e8f5ddf8fc5e9b7a90a93638810f5f422c3a4ef1aa038821536622ed";

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

/// Every member where `now` differs from `was`, as `added|removed|changed <path>`, in path order.
/// A list that is not keyed is one member.
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

/// Every changed member of the current fixture over the repo links baseline, each owned by the
/// task that made it. Closed: a member that is not here is byte-identical. The
/// fixtures' lists of identified entries are compared by identity.
const CHANGES: &[(&str, &str)] = &[
    // mht7: `rdy mcp --http-config`, and its argument group
    (
        "added /cli/tree/subcommands/name=mcp/args/id=http_config",
        "mht7",
    ),
    ("added /cli/tree/subcommands/name=mcp/groups", "mht7"),
    // mht1: the `serve.mcp_http` key, its row in the key table and its line in each listing
    ("added /config/keys/key=serve.mcp_http", "mht1"),
    (
        "added /config/listing/configured/json/key=serve.mcp_http",
        "mht1",
    ),
    (
        "added /config/listing/configured/json_show_origin/key=serve.mcp_http",
        "mht1",
    ),
    (
        "added /config/listing/defaults/json/key=serve.mcp_http",
        "mht1",
    ),
    (
        "added /config/listing/defaults/json_show_origin/key=serve.mcp_http",
        "mht1",
    ),
    ("changed /config/listing/configured/text", "mht1"),
    (
        "changed /config/listing/configured/text_show_origin",
        "mht1",
    ),
    ("changed /config/listing/defaults/text", "mht1"),
    ("changed /config/listing/defaults/text_show_origin", "mht1"),
    // mht7: the `E_SITE` row names the two new cases of MCP over HTTP (C27)
    ("changed /errors_warnings/error_rows/row=E_SITE", "mht7"),
];

/// The members of the repo links baseline that the current fixture words differently, with the
/// reason. Each is also in [`CHANGES`]. Everything else the baseline records is kept.
const REWORDED: &[(&str, &str)] = &[
    (
        "/config/listing",
        "the listing gained the `serve.mcp_http` line, in each capture's text; mht1 pins the line",
    ),
    (
        "/errors_warnings/error_rows/row=E_SITE",
        "the `E_SITE` row names the two new cases of MCP over HTTP: off, or no valid token file; mht7 pins the row",
    ),
];

/// What Release 1 words differently in the repo links baseline, which `rpl34_` holds (its
/// `REWORDED`): the current fixture inherits each. They are named here so that the retention
/// check against `release-1.json` states every exemption it makes.
const RELEASE_ONE_REWORDED: &[&str] = &[
    "/derived_identities/scenario",
    "/errors_warnings/templates/RUN_FIX",
    "/schema_packs/validation/unknown_pack_key",
    "/mcp/tools/rdy_supersede/input_schema/properties/decision",
    "/mcp/tools/rdy_supersede/input_schema/required",
];

fn under(path: &str, listed: &str) -> bool {
    path == listed || path.starts_with(&format!("{listed}/"))
}

/// Holds everything of `was` in `now`: an object keeps every member (an optional member `k?`
/// stands for `k`), a list keeps every element (the current one may hold more, in any place),
/// and a scalar stays the same. A path in `exempt` is skipped; what fails is listed in `bad`.
fn retains(
    was: &Value,
    now: &Value,
    path: &str,
    exempt: &dyn Fn(&str) -> bool,
    bad: &mut Vec<String>,
) {
    if exempt(path) {
        return;
    }
    match (was, now) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, x) in a {
                let bare = k.trim_end_matches('?');
                let found = b
                    .get(k)
                    .or_else(|| b.get(bare))
                    .or_else(|| b.get(&format!("{bare}?")));
                match found {
                    Some(y) => retains(x, y, &format!("{path}/{k}"), exempt, bad),
                    None => bad.push(format!("{path}/{k}: dropped")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for (i, x) in a.iter().enumerate() {
                let held = b.iter().any(|y| {
                    let mut inner = vec![];
                    retains(x, y, path, exempt, &mut inner);
                    inner.is_empty()
                });
                if !held {
                    bad.push(format!(
                        "{path}[{i}]: no element of the current fixture holds it"
                    ));
                }
            }
        }
        _ if was != now => bad.push(format!("{path}: changed")),
        _ => {}
    }
}

/// The help text of a tool's parameter is not the contract.
fn help_text(path: &str) -> bool {
    path.starts_with("/mcp/tools/") && path.ends_with("/description")
}

#[test]
fn mht8_http_contract_capture_is_complete() {
    let was = keyed(&read("repo-links-v1.json"));
    let now = keyed(&read("mcp-http-v1.json"));
    let mut found = vec![];
    leaves(&was, &now, "", &mut found);
    let mut want: Vec<String> = CHANGES.iter().map(|(p, _)| (*p).to_string()).collect();
    want.sort();
    found.sort();
    assert_eq!(
        found, want,
        "every changed path of the current fixture is listed with its task, and none else"
    );
    for (path, owner) in CHANGES {
        assert!(["mht1", "mht7"].contains(owner), "{path}: owner {owner}");
    }
    for (path, _) in REWORDED {
        assert!(
            CHANGES.iter().any(|(c, _)| {
                let at = c.split_once(' ').map_or(*c, |(_, p)| p);
                under(at, path)
            }),
            "{path} is reworded, so it is also a change"
        );
    }
    // the members the plan promised not to move: tools, codes, formats, runtime probes
    let (old, new) = (read("repo-links-v1.json"), read("mcp-http-v1.json"));
    for member in [
        "/mcp",
        "/errors_warnings/codes",
        "/errors_warnings/templates",
        "/schema_packs",
        "/derived_identities",
    ] {
        assert_eq!(new.pointer(member), old.pointer(member), "{member} moved");
    }
    assert_eq!(
        new["cli"]["runtime"], old["cli"]["runtime"],
        "no probe of the release binary moved"
    );
    // the inventory is Repo links': 61 tools, `rdy_links` among them, none added here
    let tools: BTreeSet<&str> = new["mcp"]["tools"]
        .as_object()
        .expect("tools")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(tools.len(), 61);
    assert!(tools.contains("rdy_links"));
    assert_eq!(new["mcp"]["tools"], old["mcp"]["tools"]);
    // frozen: a later change is a successor fixture, never an edit of this one
    assert_eq!(
        sha("mcp-http-v1.json"),
        MCP_HTTP,
        "the final current fixture is frozen"
    );
}

#[test]
fn mht8_release_one_and_repo_links_remain_compatible() {
    // the released baselines keep their bytes
    assert_eq!(sha("release-1.json"), RELEASE_1);
    assert_eq!(sha("repo-links-v1.json"), REPO_LINKS);
    let current = keyed(&read("mcp-http-v1.json"));
    // the current fixture keeps everything the repo links baseline records, but for REWORDED
    let mut bad = vec![];
    let exempt = |p: &str| REWORDED.iter().any(|(r, _)| under(p, r)) || help_text(p);
    retains(
        &keyed(&read("repo-links-v1.json")),
        &current,
        "",
        &exempt,
        &mut bad,
    );
    assert!(bad.is_empty(), "repo links baseline: {bad:#?}");
    // and everything Release 1 records, but for what repo links reworded and this plan's rewording
    let mut bad = vec![];
    let exempt = |p: &str| {
        RELEASE_ONE_REWORDED.iter().any(|r| under(p, r))
            || REWORDED.iter().any(|(r, _)| under(p, r))
            || help_text(p)
    };
    retains(
        &keyed(&read("release-1.json")),
        &current,
        "",
        &exempt,
        &mut bad,
    );
    assert!(bad.is_empty(), "Release 1 baseline: {bad:#?}");
    // the exemptions are real: each reworded member differs, so none hides a drop
    let was = keyed(&read("repo-links-v1.json"));
    for (path, _) in REWORDED {
        assert_ne!(was.pointer(path), current.pointer(path), "{path}");
    }
}
