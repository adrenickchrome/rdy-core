//! rfr (README from rdy; D-TXFHR, D-3VVNC, D-69AY3): the public contract after the README plan.
//!
//! `contracts/readme-from-rdy-v1.json` is the current fixture: the `m4bc-v1.json` baseline plus
//! the additions of this plan, and every live comparison and capture uses it
//! (`release_contract`). The baselines before it (`release-1.json`, `repo-links-v1.json`,
//! `mcp-http-v1.json`, `m4a-v1.json`, `m4bc-v1.json`) are frozen and never recaptured. Each task
//! of the plan adds one test here that lists, by exact JSON pointer, what its task changed.
//!
//! Task 1 adds the optional boolean `collapsed` to the section block type, and its probe. The
//! pack's text is hashed into every landing id, so the ids and lines built from a landing id
//! change with it; the test shows those changes are id text only.
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

/// The sha256 of `m4bc-v1.json`, the predecessor of the current fixture. It is frozen: a later
/// contract change is a successor fixture, never an edit of this file.
const M4BC_V1_SHA: &str = "f8a229b41fff5337d52d8b614670a5b0edcbee534598b35c001064fa040d06c6";

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

/// `v` with every run of 12 to 64 lowercase hexadecimal digits, in strings and in keys, replaced
/// by `#`: the shape of the value without its derived identifiers.
fn without_ids(v: &Value) -> Value {
    fn mask(s: &str) -> String {
        let mut out = String::new();
        let mut run = String::new();
        let flush = |out: &mut String, run: &mut String| {
            if (12..=64).contains(&run.len()) {
                out.push('#');
            } else {
                out.push_str(run);
            }
            run.clear();
        };
        for c in s.chars() {
            if c.is_ascii_digit() || ('a'..='f').contains(&c) {
                run.push(c);
            } else {
                flush(&mut out, &mut run);
                out.push(c);
            }
        }
        flush(&mut out, &mut run);
        out
    }
    match v {
        Value::String(s) => Value::String(mask(s)),
        Value::Array(a) => Value::Array(a.iter().map(without_ids).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (mask(k), without_ids(x))).collect())
        }
        other => other.clone(),
    }
}

/// Task 1: the members of `readme-from-rdy-v1.json` that differ from `m4bc-v1.json`, as
/// `added|removed|changed <path>`. Closed: a member that is not here is byte-identical. The
/// fixtures' lists of identified entries are compared by identity. The schema field, its probe and
/// the derived identities are the whole change: the derived-identities scenario hashes the pack
/// into every landing id.
const RFR1_CHANGES: &[&str] = &[
    "added /schema_packs/rdy_standard/block_types/section/fields/collapsed",
    "added /schema_packs/validation/section_collapsed",
    "changed /derived_identities/scenario/inboxes/agent:a/a",
    "changed /derived_identities/scenario/logged_tensions",
    "removed /derived_identities/scenario/tensions/tn-31af126dccec",
    "removed /derived_identities/scenario/tensions/tn-c7804141f02d",
    "added /derived_identities/scenario/tensions/tn-16f2d490fc3b",
    "added /derived_identities/scenario/tensions/tn-38f39054fcf2",
];

#[test]
fn rfr1_contract_adds_only_collapsed() {
    // the baselines are frozen
    assert_eq!(sha("m4bc-v1.json"), M4BC_V1_SHA, "m4bc-v1.json is frozen");
    assert_eq!(
        sha("release-1.json"),
        "635f63e24cb0a743892f52944b9b5182f4f013ca75f5491d9e05c120400a6ef6"
    );
    assert_eq!(
        sha("repo-links-v1.json"),
        "b975e0cad885386f134d9dbfab08bf7677ccdbbc9bdeb0c927289e9346f4ca27"
    );
    assert_eq!(
        sha("mcp-http-v1.json"),
        "13a8de668dedbbbd2591a6da4e24d65d7897de8368107ff90e9d89917e26a3de"
    );
    assert_eq!(
        sha("m4a-v1.json"),
        "49ca6b3f9cab4e97445c1de5df5a513fe97ceee3971d9ae17853307bc9922a71"
    );

    let (old, new) = (read("m4bc-v1.json"), read("readme-from-rdy-v1.json"));

    // every changed member is listed, and none else moved
    let mut found = vec![];
    leaves(&keyed(&old), &keyed(&new), "", &mut found);
    let mut want: Vec<String> = RFR1_CHANGES.iter().map(|p| (*p).to_string()).collect();
    found.sort();
    want.sort();
    assert_eq!(
        found, want,
        "every changed path of readme-from-rdy-v1.json is listed, and none else"
    );

    // the schema change: an optional boolean on the section, and the section's other fields are
    // as they were
    let at = "/schema_packs/rdy_standard/block_types/section/fields";
    assert_eq!(
        new.pointer(&format!("{at}/collapsed")),
        Some(&serde_json::json!({"kind": "bool", "required": false, "values": []}))
    );
    let fields = |v: &Value| -> Map<String, Value> {
        let mut m = v
            .pointer(at)
            .and_then(Value::as_object)
            .expect("the section's fields")
            .clone();
        m.remove("collapsed");
        m
    };
    assert_eq!(fields(&old), fields(&new));
    for ty in ["prose", "claim", "note", "doc", "table", "figure"] {
        let p = format!("/schema_packs/rdy_standard/block_types/{ty}");
        assert_eq!(old.pointer(&p), new.pointer(&p), "{ty} is unchanged");
        assert!(
            new.pointer(&format!("{p}/fields/collapsed")).is_none(),
            "{ty} has no collapsed field"
        );
    }

    // its probe: absent, true and false are accepted, and another kind is refused
    let probe = new
        .pointer("/schema_packs/validation/section_collapsed")
        .expect("the collapsed probe");
    for case in ["absent", "true", "false"] {
        assert_eq!(probe[case], serde_json::json!([]), "{case}");
    }
    let bad = probe["bad_kind"].as_array().expect("the refusal");
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert_eq!(bad[0][0], "E_FIELD_KIND");
    assert_eq!(bad[0][1], "field \"collapsed\" must be Bool");

    // the derived identities change as id text only: with every digest masked the member is the
    // member of the baseline
    assert_eq!(
        without_ids(&old["derived_identities"]),
        without_ids(&new["derived_identities"]),
        "the derived identities differ in their ids alone"
    );
}
