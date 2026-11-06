//! The kungfu values and tool table (D-Y18E1): the byte rule of a value, the closed table and the
//! delivered lines. The validator here is the one `build.rs` runs.
#[allow(dead_code)]
#[path = "../build/kungfu.rs"]
mod build;

use rdy_engine::embedded_guide::{BUNDLE, BUNDLE_SHA256, RULESETS};
use rdy_engine::instructions::{self, LoadRequest, Reply};
use rdy_engine::kungfu;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Barrier};

/// The owners and their tools, written out here so that no expectation comes from the table.
const OWNERS: [(&str, &str); 9] = [
    (
        "bootstrap",
        "rdy_brief, rdy_context, rdy_overview, rdy_pending, rdy_query, rdy_read, rdy_schema, rdy_search, rdy_site, rdy_suspects, rdy_tensions",
    ),
    (
        "write",
        "rdy_accept, rdy_asset, rdy_decide, rdy_intent, rdy_revoke, rdy_supersede, rdy_write",
    ),
    ("after-write", "rdy_plan, rdy_propose, rdy_resolve"),
    (
        "approvals",
        "rdy_answer, rdy_dispute, rdy_land, rdy_policy, rdy_tag",
    ),
    (
        "read",
        "rdy_coverage, rdy_diff, rdy_impact, rdy_inbox, rdy_log, rdy_tags, rdy_trace",
    ),
    (
        "publish",
        "rdy_auth, rdy_export, rdy_key_bind, rdy_key_retire, rdy_key_revoke, rdy_key_show, rdy_publication_check, rdy_publish, rdy_remote, rdy_render",
    ),
    ("review", "rdy_review, rdy_review_next, rdy_review_submit"),
    ("parallel", "rdy_alternatives, rdy_view"),
    (
        "project-ops",
        "rdy_clone, rdy_compact, rdy_dump, rdy_import, rdy_load, rdy_redact, rdy_sign, rdy_stream_check, rdy_upgrade",
    ),
];
const NON_OWNERS: [&str; 3] = ["as-built", "diagrams", "troubleshoot"];
const LINE_START: &str = "On every call to a tool in this ruleset, pass kungfu: \"";

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `kf-` and the first eight digest digits, computed apart from the code under test.
fn expected_value(text: &str) -> String {
    format!("kf-{}", &hex(text.as_bytes())[..8])
}

fn set(name: &str) -> &'static rdy_engine::embedded_guide::Ruleset {
    RULESETS
        .iter()
        .find(|s| s.name == name)
        .expect("a built-in ruleset")
}

fn source_of(name: &str) -> String {
    set(name).rows.iter().map(|r| r.text).collect()
}

fn line_of(name: &str, tools: &str) -> String {
    format!(
        "{LINE_START}{}\". Tools: {tools}.\n",
        expected_value(&source_of(name))
    )
}

fn table_text() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("kungfu-tools.json"))
        .expect("the table")
}

fn table_value() -> Vec<Value> {
    serde_json::from_str::<Value>(&table_text())
        .expect("JSON")
        .as_array()
        .expect("an array")
        .clone()
}

/// The refusal of `rows` as a table.
fn refusal(rows: &[Value]) -> String {
    build::validate(&serde_json::to_vec(rows).unwrap())
        .err()
        .expect("a refusal")
        .to_string()
}

fn refusal_of_bytes(bytes: &[u8]) -> String {
    build::validate(bytes).err().expect("a refusal").to_string()
}

const INVALID: &str = "kungfu catalogue: invalid table";

fn reply(load: &[&str], full: bool) -> Reply {
    let ids: Vec<String> = load.iter().map(|s| (*s).to_string()).collect();
    let load = LoadRequest::parse(&ids).unwrap().with_full(full);
    instructions::merge(vec![instructions::builtin(&load).unwrap()], &load).unwrap()
}

#[test]
fn kfu1_digest_is_byte_exact_and_excludes_suffix() {
    // known SHA-256 vectors: the first four digest bytes, in lowercase hexadecimal
    assert_eq!(build::value_of(["abc"]), "kf-ba7816bf");
    assert_eq!(build::value_of([""]), "kf-e3b0c442");
    assert_eq!(
        build::value_of(["Caf\u{e9} \u{65e5}\u{672c}\u{8a9e}\n"]),
        "kf-d5ddda5f"
    );
    // LF, CRLF and no final LF are three different texts
    assert_eq!(build::value_of(["a\nb\n"]), "kf-911169dd");
    assert_eq!(build::value_of(["a\r\nb\r\n"]), "kf-58055bdc");
    assert_eq!(build::value_of(["a\nb"]), "kf-7e18f737");
    assert_eq!(build::value_of(["abc"]).len(), build::VALUE_BYTES);
    // no separator between rows: a split at any byte gives the same value
    assert_eq!(build::value_of(["ab", "c"]), "kf-ba7816bf");
    assert_eq!(build::value_of(["", "a", "", "bc", ""]), "kf-ba7816bf");
    // a changed byte is a changed value; no text is trimmed or normalized
    assert_ne!(build::value_of(["abc"]), build::value_of(["abd"]));
    assert_ne!(build::value_of(["abc"]), build::value_of(["abc "]));
    assert_ne!(build::value_of(["abc"]), build::value_of(["abc\n"]));
    assert_ne!(
        build::value_of(["caf\u{e9}"]),
        build::value_of(["cafe\u{301}"])
    );

    // the real owners hash their whole source, the split bootstrap rows in order
    let boot = set("bootstrap");
    assert_eq!(boot.rows.len(), 3);
    let joined: String = boot.rows.iter().map(|r| r.text).collect();
    assert_eq!(
        kungfu::owner("bootstrap").unwrap().value,
        expected_value(&joined)
    );
    assert_eq!(
        kungfu::owner("bootstrap").unwrap().value,
        build::value_of(boot.rows.iter().map(|r| r.text))
    );
    for (name, _) in OWNERS {
        let owner = kungfu::owner(name).unwrap();
        let source = source_of(name);
        assert_eq!(owner.value, expected_value(&source), "{name}");
        // the delivered line is outside the hash, and adding it changes no source byte
        let last = set(name).rows.last().unwrap().text;
        let delivered = owner.delivered(last);
        assert!(delivered.starts_with(last), "{name}");
        let with_line = format!("{}{}", &source[..source.len() - last.len()], delivered);
        assert_ne!(expected_value(&with_line), owner.value, "{name}");
    }

    // one LF comes before the line only when the source's final byte is not LF
    assert_eq!(build::delivered("a\n", "L\n"), "a\nL\n");
    assert_eq!(build::delivered("a", "L\n"), "a\nL\n");
    assert_eq!(build::delivered("a\r\n", "L\n"), "a\r\nL\n");
    assert_eq!(build::delivered("a\r", "L\n"), "a\r\nL\n");
    assert_eq!(build::delivered("a\n\n", "L\n"), "a\n\nL\n");
    assert_eq!(build::added_bytes("a\n", "L\n"), 2);
    assert_eq!(build::added_bytes("a", "L\n"), 3);

    // the exact line, its tools in ascending order, with no backticks
    assert_eq!(
        build::suffix_of("kf-00000000", &["rdy_b", "rdy_a"]),
        "On every call to a tool in this ruleset, pass kungfu: \"kf-00000000\". Tools: rdy_a, rdy_b.\n"
    );
    // the compiled source and the export stay as the digest names them
    assert_eq!(BUNDLE_SHA256, hex(BUNDLE));
}

#[test]
fn kfu1_owners_have_distinct_values() {
    let owners = kungfu::owners();
    assert_eq!(owners.len(), 9);
    let mut values: Vec<&str> = owners.iter().map(|o| o.value.as_str()).collect();
    values.sort_unstable();
    values.dedup();
    assert_eq!(values.len(), 9);
    assert_eq!(
        owners.iter().map(|o| o.name).collect::<Vec<_>>(),
        OWNERS.map(|(name, _)| name)
    );

    // equal text is a collision: nothing salts it apart
    let same = build::check_values(&[("write", vec!["same"]), ("read", vec!["same"])]);
    assert_eq!(
        same.unwrap_err().to_string(),
        "kungfu catalogue: value collision rdy:write rdy:read"
    );
    // so is the same text under another row split
    let split = build::check_values(&[("write", vec!["ab", "c"]), ("read", vec!["abc"])]);
    assert_eq!(
        split.unwrap_err().to_string(),
        "kungfu catalogue: value collision rdy:write rdy:read"
    );
    // an injected collision of two different texts refuses, and the diagnostic has no value
    let injected = build::check_values_with(
        &[
            ("write", vec!["one"]),
            ("read", vec!["two"]),
            ("review", vec!["three"]),
        ],
        &|texts| {
            if texts == ["one"] || texts == ["three"] {
                "kf-00000000".to_string()
            } else {
                "kf-11111111".to_string()
            }
        },
    );
    let message = injected.unwrap_err().to_string();
    assert_eq!(
        message,
        "kungfu catalogue: value collision rdy:write rdy:review"
    );
    assert!(!message.contains("kf-"), "{message}");
    assert!(
        build::check_values(&[("write", vec!["one"]), ("read", vec!["two"])]).is_ok(),
        "different texts pass"
    );
}

#[test]
fn kfu1_table_is_closed_and_all_rows_warn() {
    // the checked-in table passes and is what the binary holds
    let rows = build::validate(table_text().as_bytes()).expect("the checked-in table");
    assert_eq!(rows.len(), 59);
    assert_eq!(kungfu::TOOLS.len(), 59);
    for (row, compiled) in rows.iter().zip(kungfu::TOOLS) {
        assert_eq!(row.tool, compiled.tool);
        assert_eq!(row.ruleset.as_deref(), compiled.ruleset);
        assert_eq!(
            format!("{:?}", row.response),
            format!("{:?}", compiled.response)
        );
        assert_eq!(compiled.response, kungfu::Response::Warn);
    }
    let exempt: Vec<&str> = kungfu::TOOLS
        .iter()
        .filter(|p| p.ruleset.is_none())
        .map(|p| p.tool)
        .collect();
    assert_eq!(exempt, ["rdy_instructions", "rdy_session"]);
    let mut names: Vec<&str> = kungfu::TOOLS.iter().map(|p| p.tool).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        kungfu::TOOLS.iter().map(|p| p.tool).collect::<Vec<_>>()
    );
    for (name, tools) in OWNERS {
        let handle = format!("rdy:{name}");
        let listed: Vec<&str> = kungfu::TOOLS
            .iter()
            .filter(|p| p.ruleset == Some(handle.as_str()))
            .map(|p| p.tool)
            .collect();
        assert_eq!(listed.join(", "), tools, "{name}");
        for tool in listed {
            assert_eq!(kungfu::policy(tool).unwrap().ruleset, Some(handle.as_str()));
        }
    }
    assert_eq!(kungfu::policy("rdy_session").unwrap().ruleset, None);
    assert!(kungfu::policy("rdy_nothing").is_none());
    assert!(kungfu::policy("").is_none());

    let good = table_value();
    let edited = |apply: &dyn Fn(&mut Vec<Value>)| {
        let mut rows = good.clone();
        apply(&mut rows);
        rows
    };
    let without =
        |ruleset: &'static str| edited(&move |rows| rows.retain(|r| r["ruleset"] != ruleset));

    // a missing owner, for each owner
    for (name, _) in OWNERS {
        let handle: &'static str = Box::leak(format!("rdy:{name}").into_boxed_str());
        assert_eq!(refusal(&without(handle)), INVALID, "{name}");
    }
    // exactly the two exemptions
    let lost = edited(&|rows| rows.retain(|r| r["tool"] != "rdy_session"));
    assert_eq!(refusal(&lost), INVALID);
    let owned = edited(&|rows| {
        for r in rows.iter_mut().filter(|r| r["tool"] == "rdy_session") {
            r["ruleset"] = json!("rdy:write");
        }
    });
    assert_eq!(refusal(&owned), INVALID);
    let extra = edited(&|rows| {
        for r in rows.iter_mut().filter(|r| r["tool"] == "rdy_write") {
            r["ruleset"] = Value::Null;
        }
    });
    assert_eq!(refusal(&extra), INVALID);

    // duplicate names, near or far apart
    let near = edited(&|rows| {
        let copy = rows[1].clone();
        rows.insert(1, copy);
    });
    assert_eq!(
        refusal(&near),
        "kungfu catalogue: duplicate tool rdy_alternatives"
    );
    let far = edited(&|rows| {
        let copy = rows[1].clone();
        rows.push(copy);
    });
    assert_eq!(
        refusal(&far),
        "kungfu catalogue: duplicate tool rdy_alternatives"
    );
    // tool-name order
    let swapped = edited(&|rows| rows.swap(0, 1));
    assert_eq!(refusal(&swapped), INVALID);

    // an unknown or non-owning ruleset
    for name in ["rdy:nothing", "rdy:as-built", "write"] {
        let bad = edited(&|rows| rows[0]["ruleset"] = json!(name));
        assert_eq!(
            refusal(&bad),
            format!("kungfu catalogue: unknown ruleset {name}")
        );
    }

    // every row warns: a refusal row needs a later decision
    for tool in [0, 58] {
        let bad = edited(&|rows| rows[tool]["response"] = json!("refuse"));
        let name = good[tool]["tool"].as_str().unwrap();
        assert_eq!(
            refusal(&bad),
            format!("kungfu catalogue: non-warn response {name}")
        );
    }

    // a malformed field or shape
    let long = "a".repeat(build::TOOL_MAX + 1);
    for (label, bad) in [
        (
            "no tool",
            edited(&|rows| {
                rows[0].as_object_mut().unwrap().remove("tool");
            }),
        ),
        (
            "no ruleset",
            edited(&|rows| {
                rows[0].as_object_mut().unwrap().remove("ruleset");
            }),
        ),
        (
            "no response",
            edited(&|rows| {
                rows[0].as_object_mut().unwrap().remove("response");
            }),
        ),
        (
            "unknown key",
            edited(&|rows| rows[0]["reason"] = json!("x")),
        ),
        ("tool type", edited(&|rows| rows[0]["tool"] = json!(1))),
        (
            "ruleset type",
            edited(&|rows| rows[0]["ruleset"] = json!(1)),
        ),
        (
            "response type",
            edited(&|rows| rows[0]["response"] = json!(true)),
        ),
        (
            "response word",
            edited(&|rows| rows[0]["response"] = json!("block")),
        ),
        (
            "response null",
            edited(&|rows| rows[0]["response"] = Value::Null),
        ),
        (
            "empty tool",
            edited(&|rows| {
                rows.insert(
                    0,
                    json!({
                        "tool": "", "ruleset": "rdy:write", "response": "warn",
                    }),
                )
            }),
        ),
        (
            "tool too long",
            edited(&|rows| {
                rows.insert(
                    0,
                    json!({
                        "tool": long.as_str(), "ruleset": "rdy:write", "response": "warn",
                    }),
                )
            }),
        ),
        (
            "tool not ASCII",
            edited(&|rows| {
                rows.push(json!({
                    "tool": "rdy_caf\u{e9}", "ruleset": "rdy:write", "response": "warn",
                }))
            }),
        ),
        (
            "tool with a comma",
            edited(&|rows| {
                rows.push(json!({
                    "tool": "rdy_a, rdy_b", "ruleset": "rdy:write", "response": "warn",
                }))
            }),
        ),
        (
            "row not an object",
            edited(&|rows| rows.push(json!("rdy_write"))),
        ),
        (
            "ruleset with a newline",
            edited(&|rows| rows[0]["ruleset"] = json!("rdy:a\nb")),
        ),
    ] {
        assert_eq!(refusal(&bad), INVALID, "{label}");
    }
    for (label, bytes) in [
        ("not JSON", &b"not json"[..]),
        ("empty", b""),
        ("an object", b"{\"rows\":[]}"),
        ("null", b"null"),
        ("trailing text", b"[] x"),
        ("not UTF-8", b"[\"\xff\"]"),
        (
            "duplicate key",
            b"[{\"tool\":\"a\",\"tool\":\"b\",\"ruleset\":null,\"response\":\"warn\"}]",
        ),
    ] {
        assert_eq!(refusal_of_bytes(bytes), INVALID, "{label}");
    }
    let deep = "[".repeat(build::TABLE_MAX);
    assert_eq!(refusal_of_bytes(deep.as_bytes()), INVALID);

    // the byte bound, exactly
    let pretty = table_text();
    let pad = build::TABLE_MAX - pretty.len();
    let at_bound = format!("{pretty}{}", " ".repeat(pad));
    assert_eq!(at_bound.len(), 32_768);
    assert!(build::validate(at_bound.as_bytes()).is_ok());
    let over = format!("{at_bound} ");
    assert_eq!(
        refusal_of_bytes(over.as_bytes()),
        "kungfu catalogue: table exceeds 32768 bytes"
    );

    // the row bound, exactly: 256 rows pass, the next one stops the parse
    let filled = |count: usize| {
        let mut rows = good.clone();
        for i in 0..count - good.len() {
            rows.push(json!({
                "tool": format!("rdy_z{i:03}"), "ruleset": "rdy:write", "response": "warn",
            }));
        }
        rows
    };
    let at_rows = filled(256);
    assert!(build::validate(&serde_json::to_vec(&at_rows).unwrap()).is_ok());
    assert_eq!(
        refusal(&filled(257)),
        "kungfu catalogue: table exceeds 256 rows"
    );

    // the generated rows hold the table, and an unreadable file refuses
    let generated = build::generate(&rows);
    assert!(generated.contains(
        "Policy { tool: \"rdy_write\", ruleset: Some(\"rdy:write\"), response: Response::Warn }"
    ));
    assert!(
        generated
            .contains("Policy { tool: \"rdy_session\", ruleset: None, response: Response::Warn }")
    );
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        build::load(empty.path())
            .err()
            .expect("a refusal")
            .to_string(),
        INVALID
    );
    let big = tempfile::tempdir().unwrap();
    let path = big.path().join(build::TABLE_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, " ".repeat(build::TABLE_MAX + 1)).unwrap();
    assert_eq!(
        build::load(big.path())
            .err()
            .expect("a refusal")
            .to_string(),
        "kungfu catalogue: table exceeds 32768 bytes"
    );
    std::fs::write(&path, &pretty).unwrap();
    assert_eq!(build::load(big.path()).ok().map(|r| r.len()), Some(59));
}

#[test]
fn kfu1_delivery_adds_one_suffix_to_each_owner() {
    let line = |name: &str| {
        let (_, tools) = OWNERS.iter().find(|(n, _)| *n == name).unwrap();
        line_of(name, tools)
    };
    // every row of `reply` against the catalogue: at most the final row of an owner changes
    let check = |r: &Reply, names: &[&str]| {
        let wanted: Vec<&str> = RULESETS
            .iter()
            .filter(|s| names.contains(&s.name))
            .flat_map(|s| s.rows.iter().map(|row| row.id))
            .collect();
        assert_eq!(
            r.instructions
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            wanted
        );
        for got in &r.instructions {
            let (set, index, row) = RULESETS
                .iter()
                .find_map(|s| {
                    let i = s.rows.iter().position(|row| row.id == got.id)?;
                    Some((s, i, &s.rows[i]))
                })
                .unwrap();
            assert_eq!(got.ruleset, format!("rdy:{}", set.name));
            assert_eq!(got.locked, row.locked, "{}", got.id);
            assert!(got.reason.is_empty());
            let is_owner = OWNERS.iter().any(|(n, _)| *n == set.name);
            if is_owner && index + 1 == set.rows.len() {
                assert_eq!(
                    got.text,
                    format!("{}{}", row.text, line(set.name)),
                    "{}",
                    got.id
                );
                assert_eq!(got.text.matches(LINE_START).count(), 1, "{}", got.id);
            } else {
                assert_eq!(got.text, row.text, "{}", got.id);
                assert!(!got.text.contains(LINE_START), "{}", got.id);
            }
        }
        // the menu counts what would be delivered, loaded or not
        assert_eq!(r.menu.len(), RULESETS.len());
        for (entry, set) in r.menu.iter().zip(RULESETS) {
            let source: usize = set.rows.iter().map(|row| row.text.len()).sum();
            let is_owner = OWNERS.iter().any(|(n, _)| *n == set.name);
            let size = if is_owner {
                source + line(set.name).len()
            } else {
                source
            };
            assert_eq!(entry.tokens, size.div_ceil(4) as u64, "{}", set.name);
        }
    };

    // the bootstrap alone: its final row, after the approval row
    let base = reply(&[], false);
    assert_eq!(base.instructions.len(), 3);
    assert_eq!(base.instructions[0].text, set("bootstrap").rows[0].text);
    assert_eq!(base.instructions[1].text, set("bootstrap").rows[1].text);
    assert!(base.instructions[1].locked);
    assert_eq!(base.instructions[2].id, "rdy:bootstrap:after");
    assert!(base.instructions[2].text.ends_with(&line("bootstrap")));
    check(&base, &["bootstrap"]);

    // the bootstrap named, an owner loaded, and a load named twice: one line each
    check(&reply(&["rdy:bootstrap"], false), &["bootstrap"]);
    for (name, _) in OWNERS {
        let handle = format!("rdy:{name}");
        check(&reply(&[&handle], false), &["bootstrap", name]);
        check(&reply(&[&handle, &handle], false), &["bootstrap", name]);
    }

    // three rulesets own no tool: no value and no line, and their menu sizes do not move
    for name in NON_OWNERS {
        assert!(kungfu::owner(name).is_none(), "{name}");
        let handle = format!("rdy:{name}");
        check(&reply(&[&handle], false), &["bootstrap", name]);
    }

    // full: all fourteen rows, with nine lines
    let every: Vec<&str> = RULESETS.iter().map(|s| s.name).collect();
    let full = reply(&[], true);
    assert_eq!(full.instructions.len(), 14);
    check(&full, &every);
    let lines: usize = full
        .instructions
        .iter()
        .map(|i| i.text.matches(LINE_START).count())
        .sum();
    assert_eq!(lines, 9);
    check(&reply(&["rdy:write", "rdy:write"], true), &every);

    // the source is what it was: the export's bytes and its digest hold no line
    assert_eq!(BUNDLE_SHA256, hex(BUNDLE));
    let export: Value = serde_json::from_slice(BUNDLE).unwrap();
    for (json, compiled) in export["rulesets"].as_array().unwrap().iter().zip(RULESETS) {
        for (row, row_compiled) in json["rows"].as_array().unwrap().iter().zip(compiled.rows) {
            assert_eq!(row["text"], row_compiled.text);
            assert!(!row_compiled.text.contains(LINE_START));
        }
    }
}

#[test]
fn kfu1_cache_initializes_once() {
    let callers = 16;
    let barrier = Arc::new(Barrier::new(callers));
    let threads: Vec<_> = (0..callers)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                kungfu::owners().as_ptr() as usize
            })
        })
        .collect();
    let pointers: Vec<usize> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(pointers.iter().all(|p| *p == pointers[0]));
    assert_eq!(kungfu::owners().as_ptr() as usize, pointers[0]);
    assert_eq!(kungfu::owner("write").map(|o| o.name), Some("write"));
    // the counter sits inside the derivation, so every other test in this binary counts too
    assert_eq!(kungfu::derivations_for_tests(), 1);
}
