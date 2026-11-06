//! The agent guide this crate compiles in: exact export bytes, the closed catalogue, and every
//! refusal of the validator `build.rs` runs (D-GEKKY).
#[allow(dead_code)]
#[path = "../build/guide.rs"]
mod guide;

use rdy_engine::embedded_guide::{BUNDLE, BUNDLE_SHA256, DESIGN_LANDING, EDITION, RULESETS};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const FIX: &str = "re-export the guide and its digest from one frozen landing";
const APPROVAL: &str =
    "Never approve, tag or answer on your own judgment. Relay the person's exact words.";

const NAMES: [&str; 12] = [
    "bootstrap",
    "write",
    "after-write",
    "approvals",
    "as-built",
    "read",
    "publish",
    "review",
    "diagrams",
    "parallel",
    "project-ops",
    "troubleshoot",
];

const WHEN: [&str; 12] = [
    "start a task with rdy",
    "create or change content, structure, decisions or links",
    "handle consequences, re-affirmations, change plans or readiness",
    "show a card, relay an answer, or work under the authority policy or a dispute",
    "ship work, or set `built`, sources or code links",
    "explain why a block says something, or read changes",
    "check or publish a profile, or use a hub",
    "run or answer a review",
    "write a diagram",
    "use subagents or alternative sets",
    "dump, load, import, redact, sign or upgrade",
    "hit an actor, run or daemon error",
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A valid catalogue, built here so that no refusal depends on the committed guide's bytes.
fn good() -> Value {
    let rulesets: Vec<Value> = NAMES
        .iter()
        .zip(WHEN)
        .map(|(name, when)| {
            let rows = if *name == "bootstrap" {
                json!([
                    {"id": "rdy:bootstrap:before", "text": "# Start\n\nBefore.\n\n", "locked": false},
                    {"id": "rdy:bootstrap:approval", "text": APPROVAL, "locked": true},
                    {"id": "rdy:bootstrap:after", "text": "\n\nAfter.\n", "locked": false},
                ])
            } else {
                json!([{"id": format!("rdy:{name}:body"), "text": format!("# {name}\n\nText.\n"), "locked": false}])
            };
            json!({"name": name, "title": format!("Title {name}"), "when": when,
                   "required": *name == "bootstrap", "rows": rows})
        })
        .collect();
    json!({"schema": 1, "edition": "0.43.0", "design_landing": "a".repeat(64), "rulesets": rulesets})
}

fn encode(v: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec(v).unwrap();
    out.push(b'\n');
    out
}

fn sidecar_of(bytes: &[u8]) -> Vec<u8> {
    format!("{}\n", hex(bytes)).into_bytes()
}

fn checked(bytes: &[u8]) -> Result<guide::Catalogue, guide::Refusal> {
    guide::validate(bytes, &sidecar_of(bytes))
}

/// The whole diagnostic of `bytes`, under a matching sidecar.
fn refusal(bytes: &[u8]) -> String {
    match checked(bytes) {
        Ok(_) => panic!("accepted: {}", String::from_utf8_lossy(bytes)),
        Err(e) => e.to_string(),
    }
}

fn changed(f: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v = good();
    f(&mut v);
    encode(&v)
}

fn expect(bytes: &[u8], message: &str) {
    assert_eq!(refusal(bytes), format!("{message}; {FIX}"));
}

#[test]
fn agi1_catalogue_matches_export_bytes() {
    let root = workspace();
    let on_disk = std::fs::read(root.join("release/agent-guide.json")).unwrap();
    let sidecar = std::fs::read(root.join("release/agent-guide.sha256")).unwrap();
    assert_eq!(
        BUNDLE,
        on_disk.as_slice(),
        "the compiled bytes are the export"
    );
    assert_eq!(sidecar.len(), 65);
    assert_eq!(
        sidecar,
        sidecar_of(&on_disk),
        "the sidecar names the export"
    );
    assert_eq!(BUNDLE_SHA256, hex(&on_disk));
    assert_eq!(on_disk.last(), Some(&b'\n'));

    let parsed: Value = serde_json::from_slice(&on_disk).unwrap();
    assert_eq!(parsed["schema"], 1);
    assert_eq!(parsed["edition"], EDITION);
    assert_eq!(parsed["design_landing"], DESIGN_LANDING);
    let sets = parsed["rulesets"].as_array().unwrap();
    assert_eq!(sets.len(), RULESETS.len());
    for (json, compiled) in sets.iter().zip(RULESETS) {
        assert_eq!(json["name"], compiled.name);
        assert_eq!(json["title"], compiled.title);
        assert_eq!(json["when"], compiled.when);
        assert_eq!(json["required"], compiled.required);
        let rows = json["rows"].as_array().unwrap();
        assert_eq!(rows.len(), compiled.rows.len());
        for (j, c) in rows.iter().zip(compiled.rows) {
            assert_eq!(j["id"], c.id);
            assert_eq!(j["text"], c.text, "{} is byte for byte", c.id);
            assert_eq!(j["locked"], c.locked);
        }
    }

    // The three bootstrap slices rebuild one body, and it holds the approval paragraph once.
    let boot = &RULESETS[0];
    let joined: String = boot.rows.iter().map(|r| r.text).collect();
    let exported: String = sets[0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert_eq!(joined, exported);
    assert!(joined.starts_with("# "), "the body keeps its heading");
    assert!(joined.ends_with('\n'), "the body keeps its final newline");
    assert_eq!(joined.matches(APPROVAL).count(), 1);
    assert_eq!(boot.rows[1].text, APPROVAL);
    assert!(
        boot.rows[0].text.ends_with("\n\n") || boot.rows[0].text.ends_with("Planned "),
        "the separator stays in the slice before"
    );
    assert!(
        boot.rows[2].text.starts_with("\n\n"),
        "the separator stays in the slice after"
    );

    // The checked input of the build is what compiled.
    let catalogue = guide::load(&root).unwrap();
    assert_eq!(catalogue.bytes, on_disk);
    assert_eq!(catalogue.rulesets.len(), 12);
}

#[test]
fn agi1_bootstrap_has_no_backslash_escapes() {
    for row in RULESETS[0].rows {
        for c in ['\\', '`', '*', '_', '[', ']', '<', '>', '!'] {
            let escaped = format!("\\{c}");
            assert!(
                !row.text.contains(&escaped),
                "{} holds the exporter's escape {escaped}",
                row.id
            );
        }
    }
}

#[test]
fn agi1_delivered_rows_carry_no_anchor_lines() {
    for set in RULESETS {
        for row in set.rows {
            assert!(!row.text.contains("<a id=\""), "{} holds an anchor", row.id);
            assert!(
                !row.text.starts_with("---\n"),
                "{} holds front matter",
                row.id
            );
        }
    }
}

#[test]
fn agi1_only_the_approval_paragraph_is_locked() {
    let names: Vec<&str> = RULESETS.iter().map(|s| s.name).collect();
    assert_eq!(names, NAMES);
    let whens: Vec<&str> = RULESETS.iter().map(|s| s.when).collect();
    assert_eq!(whens, WHEN);
    assert!(RULESETS.iter().all(|s| !s.title.is_empty()));
    let required: Vec<&str> = RULESETS
        .iter()
        .filter(|s| s.required)
        .map(|s| s.name)
        .collect();
    assert_eq!(required, ["bootstrap"]);
    let rows: Vec<_> = RULESETS.iter().flat_map(|s| s.rows).collect();
    assert_eq!(rows.len(), 14);
    let locked: Vec<_> = rows.iter().filter(|r| r.locked).collect();
    assert_eq!(locked.len(), 1);
    assert_eq!(locked[0].id, "rdy:bootstrap:approval");
    assert_eq!(locked[0].text, APPROVAL);
    let mut ids: Vec<String> = vec![
        "rdy:bootstrap:before".to_string(),
        "rdy:bootstrap:approval".to_string(),
        "rdy:bootstrap:after".to_string(),
    ];
    ids.extend(NAMES[1..].iter().map(|n| format!("rdy:{n}:body")));
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), ids);

    // No stored reason anywhere: the delivered row has none to check, and a bundle cannot add one.
    let parsed: Value = serde_json::from_slice(BUNDLE).unwrap();
    for set in parsed["rulesets"].as_array().unwrap() {
        assert!(set.get("reason").is_none());
        for row in set["rows"].as_array().unwrap() {
            assert!(row.get("reason").is_none());
        }
    }
    let mut smuggled = good();
    smuggled["rulesets"][1]["rows"][0]["reason"] = json!([]);
    expect(
        &encode(&smuggled),
        "embedded guide: invalid rulesets[1].rows[0]: unknown key reason",
    );
}

#[test]
fn agi1_build_refuses_invalid_catalogues() {
    assert!(checked(&encode(&good())).is_ok(), "the base case is valid");

    // Missing input, through the reader build.rs calls.
    let dir = tempfile::tempdir().unwrap();
    let missing = |d: &Path| guide::load(d).err().unwrap().to_string();
    assert_eq!(
        missing(dir.path()),
        format!("embedded guide: missing release/agent-guide.json; {FIX}")
    );
    std::fs::create_dir_all(dir.path().join("release")).unwrap();
    let bytes = encode(&good());
    std::fs::write(dir.path().join("release/agent-guide.json"), &bytes).unwrap();
    assert_eq!(
        missing(dir.path()),
        format!("embedded guide: missing release/agent-guide.sha256; {FIX}")
    );
    std::fs::write(
        dir.path().join("release/agent-guide.sha256"),
        sidecar_of(&bytes),
    )
    .unwrap();
    assert_eq!(guide::load(dir.path()).unwrap().bytes, bytes);

    // Oversized inputs refuse before the read allocates for them.
    let big = vec![b' '; 1_048_577];
    std::fs::write(dir.path().join("release/agent-guide.json"), &big).unwrap();
    assert_eq!(
        missing(dir.path()),
        format!("embedded guide: release/agent-guide.json exceeds 1048576 bytes; {FIX}")
    );
    std::fs::write(dir.path().join("release/agent-guide.json"), &bytes).unwrap();
    std::fs::write(
        dir.path().join("release/agent-guide.sha256"),
        vec![b'0'; 66],
    )
    .unwrap();
    assert_eq!(
        missing(dir.path()),
        format!("embedded guide: release/agent-guide.sha256 exceeds 65 bytes; {FIX}")
    );
    expect(
        &big,
        "embedded guide: release/agent-guide.json exceeds 1048576 bytes",
    );

    // Digest: wrong, or a sidecar of the wrong form.
    let wrong =
        "embedded guide: release/agent-guide.json does not match release/agent-guide.sha256";
    let e = guide::validate(&bytes, format!("{}\n", "0".repeat(64)).as_bytes());
    assert_eq!(e.err().unwrap().to_string(), format!("{wrong}; {FIX}"));
    let truncated = &bytes[..bytes.len() / 2];
    let e = guide::validate(truncated, &sidecar_of(&bytes));
    assert_eq!(e.err().unwrap().to_string(), format!("{wrong}; {FIX}"));
    for bad in [
        format!("{}\n", "A".repeat(64)),
        "0".repeat(65),
        format!("{}\n", "0".repeat(63)),
        String::new(),
    ] {
        let e = guide::validate(&bytes, bad.as_bytes());
        assert_eq!(
            e.err().unwrap().to_string(),
            format!(
                "embedded guide: invalid release/agent-guide.sha256: invalid provenance; {FIX}"
            ),
            "{bad:?}"
        );
    }

    // Text that is not one JSON document.
    let invalid_json = "embedded guide: invalid release/agent-guide.json: invalid JSON";
    expect(truncated, invalid_json);
    expect(b"", invalid_json);
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(b"{}");
    expect(&trailing, invalid_json);
    expect(
        b"\xff{}",
        "embedded guide: invalid release/agent-guide.json: invalid UTF-8",
    );
    expect(b"[1]", "embedded guide: invalid bundle: wrong type");

    // Duplicate keys, at every level, and the depth bound.
    let text = String::from_utf8(bytes.clone()).unwrap();
    let dup = "embedded guide: invalid release/agent-guide.json: duplicate key";
    expect(text.replacen("{", "{\"schema\":1,", 1).as_bytes(), dup);
    expect(
        text.replacen("\"locked\":", "\"locked\":false,\"locked\":", 1)
            .as_bytes(),
        dup,
    );
    let deep = format!("{}{}", "[".repeat(9), "]".repeat(9));
    expect(deep.as_bytes(), "embedded guide: depth exceeds 8");
    let deepest = "[".repeat(100_000);
    expect(deepest.as_bytes(), "embedded guide: depth exceeds 8");

    // Closed keys.
    let bad = changed(|v| v["surprise"] = json!(1));
    expect(&bad, "embedded guide: invalid bundle: unknown key surprise");
    let bad = changed(|v| {
        let set = v["rulesets"][2].as_object_mut().unwrap();
        let t = set.remove("title").unwrap();
        set.insert("titel".to_string(), t);
    });
    expect(
        &bad,
        "embedded guide: invalid rulesets[2]: unknown key titel; did you mean title?",
    );

    // Field types and values.
    let bad = changed(|v| v["schema"] = json!(2));
    expect(&bad, "embedded guide: invalid schema: unsupported schema");
    let bad = changed(|v| v["schema"] = json!("1"));
    expect(&bad, "embedded guide: invalid schema: unsupported schema");
    let bad = changed(|v| {
        v.as_object_mut().unwrap().remove("edition");
    });
    expect(&bad, "embedded guide: invalid edition: wrong type");
    for edition in ["", "v1", "0.43", "0.43.0-rc1", "0.43.0-dev-dev"] {
        let bad = changed(|v| v["edition"] = json!(edition));
        expect(&bad, "embedded guide: invalid edition: invalid provenance");
    }
    for landing in ["", "abc", &"A".repeat(64), &"a".repeat(65)] {
        let bad = changed(|v| v["design_landing"] = json!(landing));
        expect(
            &bad,
            "embedded guide: invalid design_landing: invalid provenance",
        );
    }
    let bad = changed(|v| v["rulesets"] = json!({}));
    expect(&bad, "embedded guide: invalid rulesets: wrong type");
    let bad = changed(|v| v["rulesets"][0]["required"] = json!("true"));
    expect(
        &bad,
        "embedded guide: invalid rulesets[0].required: wrong type",
    );
    let bad = changed(|v| v["rulesets"][0]["rows"][0]["locked"] = json!(null));
    expect(
        &bad,
        "embedded guide: invalid rulesets[0].rows[0].locked: wrong type",
    );

    // Names, order, and the closed row ids.
    for name in [
        "",
        "Write",
        "a b",
        "-write",
        "write-",
        "a--b",
        &"a".repeat(65),
    ] {
        let bad = changed(|v| v["rulesets"][1]["name"] = json!(name));
        let message = if name.len() > 64 {
            "embedded guide: rulesets[1].name exceeds 64 bytes".to_string()
        } else {
            "embedded guide: invalid rulesets[1].name: invalid name".to_string()
        };
        expect(&bad, &message);
    }
    let bad = changed(|v| v["rulesets"].as_array_mut().unwrap().swap(1, 2));
    expect(
        &bad,
        "embedded guide: invalid rulesets[1].name: unexpected order",
    );
    let bad = changed(|v| v["rulesets"][1]["name"] = json!("rules"));
    expect(
        &bad,
        "embedded guide: invalid rulesets[1].name: unexpected order",
    );
    let bad = changed(|v| v["rulesets"][2]["name"] = json!("write"));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].name: duplicate ruleset",
    );
    let bad = changed(|v| {
        v["rulesets"].as_array_mut().unwrap().pop();
    });
    expect(&bad, "embedded guide: invalid rulesets: unexpected order");
    let bad = changed(|v| {
        let extra = v["rulesets"][11].clone();
        v["rulesets"].as_array_mut().unwrap().push(extra);
    });
    expect(&bad, "embedded guide: rulesets exceeds 12");
    let bad = changed(|v| v["rulesets"][2]["rows"][0]["id"] = json!("rdy:write:body"));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].rows[0].id: duplicate row",
    );
    let bad = changed(|v| v["rulesets"][2]["rows"][0]["id"] = json!("rdy:after-write:other"));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].rows[0].id: unexpected order",
    );
    let bad = changed(|v| v["rulesets"][2]["rows"][0]["id"] = json!("a".repeat(97)));
    expect(
        &bad,
        "embedded guide: rulesets[2].rows[0].id exceeds 96 bytes",
    );
    let bad = changed(|v| v["rulesets"][2]["rows"][0]["id"] = json!("has space"));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].rows[0].id: invalid id",
    );
    let bad = changed(|v| {
        let extra = v["rulesets"][2]["rows"][0].clone();
        v["rulesets"][2]["rows"].as_array_mut().unwrap().push(extra);
    });
    expect(&bad, "embedded guide: rows exceeds 14");
    let bad = changed(|v| {
        v["rulesets"][0]["rows"].as_array_mut().unwrap().pop();
    });
    expect(
        &bad,
        "embedded guide: invalid rulesets[0].rows: unexpected order",
    );
    let bad = changed(|v| v["rulesets"][2]["rows"] = json!([]));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].rows: unexpected order",
    );

    // Text bounds and empty text.
    let bad = changed(|v| v["rulesets"][2]["title"] = json!("t".repeat(257)));
    expect(&bad, "embedded guide: rulesets[2].title exceeds 256 bytes");
    let bad = changed(|v| v["rulesets"][2]["title"] = json!(""));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].title: empty text",
    );
    let bad = changed(|v| v["rulesets"][2]["when"] = json!("w".repeat(1025)));
    expect(&bad, "embedded guide: rulesets[2].when exceeds 1024 bytes");
    let bad = changed(|v| v["rulesets"][2]["when"] = json!(""));
    expect(&bad, "embedded guide: invalid rulesets[2].when: empty text");
    let bad = changed(|v| v["rulesets"][2]["rows"][0]["text"] = json!("x".repeat(262_145)));
    expect(
        &bad,
        "embedded guide: rulesets[2].rows[0].text exceeds 262144 bytes",
    );
    for empty in ["", " \n\t\r\n"] {
        let bad = changed(|v| v["rulesets"][2]["rows"][0]["text"] = json!(empty));
        expect(
            &bad,
            "embedded guide: invalid rulesets[2].rows[0].text: empty text",
        );
    }

    // One required set, one lock, the reviewed paragraph.
    let bad = changed(|v| v["rulesets"][2]["required"] = json!(true));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].required: invalid required set",
    );
    let bad = changed(|v| v["rulesets"][0]["required"] = json!(false));
    expect(
        &bad,
        "embedded guide: invalid rulesets[0].required: invalid required set",
    );
    let bad = changed(|v| v["rulesets"][2]["rows"][0]["locked"] = json!(true));
    expect(
        &bad,
        "embedded guide: invalid rulesets[2].rows[0].locked: invalid approval lock",
    );
    let bad = changed(|v| v["rulesets"][0]["rows"][1]["locked"] = json!(false));
    expect(
        &bad,
        "embedded guide: invalid rulesets[0].rows[1].locked: invalid approval lock",
    );
    let bad = changed(|v| v["rulesets"][0]["rows"][1]["text"] = json!("Approve what you like."));
    expect(
        &bad,
        "embedded guide: invalid rulesets[0].rows[1].text: invalid approval lock",
    );
}

#[test]
fn agi1_marks_are_for_a_development_bundle_only() {
    let marked = [
        "# T\n\n**Planned**\n\nText.\n",
        "# T\n\n> **Planned**\n>\n> Note.\n",
        "# T\n\nPlanned Load this ruleset.\n",
        "# T\n\nText.\n\nPlanned ",
    ];
    for text in marked {
        let bad = changed(|v| v["rulesets"][2]["rows"][0]["text"] = json!(text));
        expect(
            &bad,
            "embedded guide: invalid rulesets[2].rows[0].text: invalid provenance",
        );
        let ok = changed(|v| {
            v["rulesets"][2]["rows"][0]["text"] = json!(text);
            v["edition"] = json!("0.43.0-dev");
        });
        let c = checked(&ok).unwrap();
        assert_eq!(c.edition, "0.43.0-dev");
    }
    // Prose that only starts with the word later in a body is not a mark.
    let ok = changed(|v| {
        v["rulesets"][2]["rows"][0]["text"] = json!("# T\n\nText.\n\nPlanned text is hidden.\n");
    });
    assert!(checked(&ok).is_ok());
    // A mark across the bootstrap's split is still found.
    let bad = changed(|v| v["rulesets"][0]["rows"][0]["text"] = json!("# T\n\n**Planned**\n\n"));
    expect(
        &bad,
        "embedded guide: invalid rulesets[0].rows[0].text: invalid provenance",
    );
}

#[test]
fn agi1_generated_source_keeps_every_byte() {
    let text = "Quote \" slash \\ brace { line\r\nnul-free \u{2028} caf\u{e9} \u{1F600}\n";
    let bytes = changed(|v| v["rulesets"][2]["rows"][0]["text"] = json!(text));
    let source = guide::generate(&checked(&bytes).unwrap());
    assert!(source.contains(&format!("text: {text:?}")));
    assert!(source.contains("include_bytes!(\"agent-guide.json\")"));
}
