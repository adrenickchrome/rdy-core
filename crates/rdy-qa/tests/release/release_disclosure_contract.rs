//! drv (disclosure review rows; D-TXFHR, D-RDH1C, D-69AY3): the public contract after the
//! disclosure review plan.
//!
//! `contracts/disclosure-review-v1.json` is the current fixture: the frozen
//! `readme-from-rdy-v1.json` plus the additions of this plan. Every live comparison and capture
//! uses it: the release capture (`release_contract`), the CLI tree (`rdy-cli`'s `contract`) and
//! the rule listing check (`cli_rules`). The baselines before it are frozen and never recaptured.
//!
//! Task 1c starts the fixture as a byte copy of its predecessor and moves the three selectors to
//! it together. It changes no surface, so its list of changed members is empty. A later task adds
//! only its own named members to this same closed comparison: Task 6 adds the review's members
//! of the publication check's probes and the two probes that fill every list of the review.
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

/// The current fixture.
const SUCCESSOR: &str = "disclosure-review-v1.json";

/// The predecessor of the current fixture, which every selector named before this plan.
const PREDECESSOR: &str = "readme-from-rdy-v1.json";

/// The sha256 of [`PREDECESSOR`]. It is frozen: a later contract change is a successor fixture,
/// never an edit of this file.
const PREDECESSOR_SHA: &str = "883debf0ad6ba99e983662a92ddb8c7f044306cea027ca835994b39b5187dc16";

/// Every frozen contract fixture before the current one, with its sha256.
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
    (PREDECESSOR, PREDECESSOR_SHA),
];

/// Task 1c: no member of the current fixture moves. The seam, the row model and the part
/// fixture add no output field yet.
const DRV1_CHANGES: &[&str] = &[];

/// The two prefixes of a first check's report: the chat tool's result and the command's stdout.
const FIRST: [&str; 2] = [
    "/mcp/result_fields/rdy_publication_check first/ok",
    "/cli/runtime/probes/publication check first/stdout",
];

/// The two prefixes of a comparison's report.
const AGAINST: [&str; 2] = [
    "/mcp/result_fields/rdy_publication_check against/ok",
    "/cli/runtime/probes/publication check against/stdout",
];

/// The members Task 6 adds to a changed doc's row (D-RDH1C).
const CHANGED_DOC_MEMBERS: [&str; 10] = [
    "title",
    "before_title",
    "page",
    "before_page",
    "counts",
    "record_only",
    "changes",
    "omitted_changes",
    "decisions",
    "omitted_decisions",
];

/// The members Task 6 adds to a removed doc's row (D-RDH1C).
const REMOVED_DOC_MEMBERS: [&str; 3] = ["title", "before_page", "size"];

/// The attachment arrays of a comparison: no keyed identity, so each is one member.
const ATTACHMENT_ARRAYS: [&str; 3] = [
    "added_attachments",
    "removed_attachments",
    "changed_attachments",
];

/// The two new probes, which fill every list of the review.
const REVIEW_PROBES: [&str; 2] = [
    "/mcp/result_fields/rdy_publication_check review rows",
    "/cli/runtime/probes/publication check review rows",
];

/// Task 6: the exact members the review adds to the probes of the publication check, on both
/// surfaces (the closed capture inventory). Nothing else in `mcp`, `cli` or any other row moves.
fn drv6_changes() -> Vec<String> {
    let mut out = vec![];
    for p in FIRST {
        out.push(format!("added {p}/included"));
        out.push(format!("added {p}/references/dropped_decisions"));
    }
    for p in AGAINST {
        out.push(format!("added {p}/included"));
        out.push(format!("added {p}/references/dropped_decisions"));
        out.push(format!("added {p}/disclosure/totals"));
        for m in CHANGED_DOC_MEMBERS {
            out.push(format!("added {p}/disclosure/changed_docs/id=string/{m}"));
        }
        for m in REMOVED_DOC_MEMBERS {
            out.push(format!("added {p}/disclosure/removed_docs/id=string/{m}"));
        }
        for a in ATTACHMENT_ARRAYS {
            out.push(format!("changed {p}/disclosure/{a}"));
        }
    }
    for p in REVIEW_PROBES {
        out.push(format!("added {p}"));
    }
    out
}

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

/// Every member where `now` differs from `was`, as `added|removed|changed <path>`. A list that is
/// not keyed is one member.
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

/// Every changed member of the current fixture against its predecessor is in one task's list,
/// and none else moved: a member that is not listed is equal.
fn assert_closed(old: &Value, new: &Value) {
    let mut found = vec![];
    leaves(&keyed(old), &keyed(new), "", &mut found);
    let mut want: Vec<String> = DRV1_CHANGES.iter().map(|p| (*p).to_string()).collect();
    want.extend(drv6_changes());
    found.sort();
    want.sort();
    assert_eq!(
        found, want,
        "every changed path of {SUCCESSOR} is listed, and none else"
    );
}

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

#[test]
fn drv1_successor_preserves_predecessor_surfaces() {
    // the predecessors are frozen
    for (name, digest) in FROZEN {
        assert_eq!(sha(name), *digest, "{name} is frozen");
    }

    // at Task 1c the successor equals its predecessor: the closed list is empty
    let (old, new) = (read(PREDECESSOR), read(SUCCESSOR));
    assert!(DRV1_CHANGES.is_empty(), "Task 1c moves no contract member");
    assert_closed(&old, &new);

    // no surface of the predecessor is removed
    let mut found = vec![];
    leaves(&keyed(&old), &keyed(&new), "", &mut found);
    let removed: Vec<&String> = found.iter().filter(|c| c.starts_with("removed ")).collect();
    assert!(removed.is_empty(), "{removed:?}");
    for surface in ["cli", "mcp", "errors_warnings", "store", "dump_load"] {
        assert!(new.get(surface).is_some(), "{surface}");
    }

    // all three selectors name the successor, and none names the predecessor as its path
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{SUCCESSOR}\"")),
            "{file} selects {SUCCESSOR}"
        );
        assert!(
            !text.contains(&format!("contracts/{PREDECESSOR}\"")),
            "{file} no longer selects {PREDECESSOR}"
        );
    }
}

/// The value at the JSON pointer `path` of `v`, with `~1` and `~0` unescaped.
fn at<'v>(v: &'v Value, path: &str) -> &'v Value {
    v.pointer(path).unwrap_or(&Value::Null)
}

/// The sorted member names of a shape object, with the `?` of an optional member dropped.
fn names(v: &Value) -> Vec<String> {
    let mut n: Vec<String> = v
        .as_object()
        .map(|m| {
            m.keys()
                .map(|k| k.trim_end_matches('?').to_string())
                .collect()
        })
        .unwrap_or_default();
    n.sort();
    n
}

/// drv6 (D-TXFHR, D-RDH1C, D-MCH70): the successor changes only the review's members of the
/// publication check. Every other member of the predecessor is equal, no member is removed, the
/// old members of each enriched row stay, the input schema does not move, and both surfaces
/// answer with one shape.
#[test]
fn drv6_contract_changes_only_review_results() {
    let (old, new) = (read(PREDECESSOR), read(SUCCESSOR));
    assert_closed(&old, &new);

    // the attachment arrays are one member each: their old row members all stay
    for p in AGAINST {
        for a in ATTACHMENT_ARRAYS {
            let path = format!("{p}/disclosure/{a}/0");
            let (was, now) = (names(at(&old, &path)), names(at(&new, &path)));
            assert!(!was.is_empty(), "{path} is populated");
            for m in &was {
                assert!(now.contains(m), "{path} keeps {m}: {now:?}");
            }
            let added: Vec<&String> = now.iter().filter(|m| !was.contains(m)).collect();
            let labels: &[&str] = if a == "changed_attachments" {
                &["after_home_doc_title", "before_home_doc_title", "label"]
            } else {
                &["home_doc_title", "label"]
            };
            assert_eq!(added, labels.iter().collect::<Vec<_>>(), "{path}");
        }
    }

    // the chat tool's input schema stays byte for byte
    let schema = "/mcp/tools/rdy_publication_check/input_schema";
    assert_eq!(at(&old, schema), at(&new, schema));
    assert!(at(&new, schema).is_object());

    // both surfaces answer each probe with one shape
    let mcp = &new["mcp"]["result_fields"];
    let cli = &new["cli"]["runtime"]["probes"];
    for (tool, command) in [
        ("rdy_publication_check first", "publication check first"),
        ("rdy_publication_check against", "publication check against"),
        (
            "rdy_publication_check review rows",
            "publication check review rows",
        ),
    ] {
        assert_eq!(mcp[tool]["ok"], cli[command]["stdout"], "{tool}");
    }

    // the review probe fills every list but one: no other array in its shape is empty
    fn empty(path: &str, v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Array(a) if a.is_empty() => out.push(path.to_string()),
            Value::Array(a) => a.iter().for_each(|x| empty(&format!("{path}[]"), x, out)),
            Value::Object(m) => m
                .iter()
                .for_each(|(k, x)| empty(&format!("{path}/{k}"), x, out)),
            _ => {}
        }
    }
    let mut none = vec![];
    empty(
        "",
        &mcp["rdy_publication_check review rows"]["ok"],
        &mut none,
    );
    // an added row names no changed part, so the one added row outside docs has an empty `what`
    assert_eq!(none, ["/disclosure/|null/changed_outside_docs[]/what"]);
    // its first check and its comparison are both in the shape
    let review = &mcp["rdy_publication_check review rows"]["ok"];
    for member in ["included", "disclosure"] {
        assert!(
            review[member]["|null"].is_object(),
            "{member} is null in one call and an object in the other: {}",
            review[member]
        );
    }
}

/// drv6: every frozen fixture keeps its bytes, the predecessor included, and the successor is
/// the only fixture a selector names.
#[test]
fn drv6_predecessor_fixtures_keep_their_bytes() {
    for (name, digest) in FROZEN {
        assert_eq!(sha(name), *digest, "{name} is frozen");
    }
    assert_eq!(sha(PREDECESSOR), PREDECESSOR_SHA);
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<String> = FROZEN.iter().map(|(n, _)| (*n).to_string()).collect();
    want.push(SUCCESSOR.to_string());
    want.sort();
    assert_eq!(
        names, want,
        "no fixture is added or removed beside the successor"
    );
}
