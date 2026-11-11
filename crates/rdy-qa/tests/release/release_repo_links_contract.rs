//! rpl34 (repo links; D-TXFHR, D-E30RA, D-69AY3, D-C94DV): the public contract after repo links.
//!
//! Two fixtures hold the contract. `contracts/release-1.json` is the Release 1 baseline, frozen at
//! the Release 1 tag. `contracts/repo-links-v1.json` is the current fixture: the baseline plus the
//! additions of repo links, and every live comparison and capture uses it (`release_contract`).
//! These tests hold the two together:
//!
//! - the current fixture differs from the baseline in a closed list of members, and nowhere else
//!   ([`rpl34_contract_additions_are_closed`]);
//! - everything the baseline records is still in the current fixture, but for the few members
//!   that [`REWORDED`] names with the reason ([`rpl34_release_one_contract_remains_compatible`]);
//! - every tool of the current fixture has its kungfu row ([`rpl34_new_tool_cannot_omit_kungfu`]);
//! - a constant that shares a template name with another, with other text, is refused, so a later
//!   constant can never shadow a released template in the capture
//!   ([`rpl34_a_second_constant_cannot_shadow_a_released_template`]).
//!
//! A task that changes the public contract adds its members to [`ADDITIONS`] in the same commit;
//! the orchestrator captures the fixture, never a task. The adapter surface of repo links has its
//! own pin in rdy-adapter (`rpl34_adapter_surface_is_complete`).
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts")
}

fn read(name: &str) -> Value {
    let path = contracts().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; capture it with RDY_CONTRACT_CAPTURE=1",
            path.display()
        )
    });
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every member where `now` differs from `was`, as `added|removed|changed <path>`, in path order.
/// An array is one member: a list that changed is `changed`, and the tests below say how.
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

/// The members where the current fixture differs from the Release 1 baseline: the contract
/// additions of repo links, one entry each, and the reworded members of [`REWORDED`]. Closed: a
/// member that is not here is byte-identical in the two fixtures.
const ADDITIONS: &[&str] = &[
    "added /cli/runtime/not_probed_commands/adopt-upstream",
    "added /cli/runtime/not_probed_commands/new repo-link",
    "added /cli/runtime/probes/links",
    "added /cli/runtime/probes/links refs",
    "added /cli/runtime/probes/links show",
    "changed /cli/tree/subcommands",
    "changed /derived_identities/scenario/inboxes/agent:a/a",
    "changed /derived_identities/scenario/logged_tensions",
    "removed /derived_identities/scenario/tensions/tn-4cfebfe48e4d",
    "removed /derived_identities/scenario/tensions/tn-91ff37c35dde",
    "added /derived_identities/scenario/tensions/tn-b193a13b5413",
    "added /derived_identities/scenario/tensions/tn-bc2501dbc52a",
    "added /dump_load/fixture_digests/v4-f1-e1-foreign-copy.expect.json",
    "added /dump_load/fixture_digests/v4-f1-e1-foreign-copy.rdydump",
    "added /errors_warnings/codes/E_FOREIGN_READ_ONLY",
    "added /errors_warnings/codes/E_FOREIGN_VERSION",
    "added /errors_warnings/codes/E_INSTRUCTION_LOCKED",
    "changed /errors_warnings/error_rows",
    "added /errors_warnings/templates/ADOPT_CHANGE_FIX",
    "added /errors_warnings/templates/DEVIATION_ON_FIX",
    "added /errors_warnings/templates/LINKS_OP_FIX",
    "added /errors_warnings/templates/LOCKED_FIX",
    "changed /errors_warnings/templates/RUN_FIX",
    "added /errors_warnings/templates/TARGET_FIX",
    "changed /mcp/refusal_error_rows",
    "added /mcp/result_fields/rdy_links",
    "added /mcp/result_fields/rdy_links refs",
    "added /mcp/result_fields/rdy_links show",
    "added /mcp/result_fields/rdy_schema/ok/errors?/E_FOREIGN_READ_ONLY",
    "added /mcp/result_fields/rdy_schema/ok/errors?/E_FOREIGN_VERSION",
    "added /mcp/result_fields/rdy_schema/ok/errors?/E_INSTRUCTION_LOCKED",
    "added /mcp/result_fields/rdy_schema/ok/errors?/E_RESERVED_VIEW",
    "changed /mcp/tool_names",
    "changed /mcp/tools/rdy_answer/input_schema/properties/option/description",
    "changed /mcp/tools/rdy_context/input_schema/properties/id/description",
    "added /mcp/tools/rdy_decide/input_schema/properties/deviates_from",
    "changed /mcp/tools/rdy_impact/input_schema/properties/id/description",
    "added /mcp/tools/rdy_links",
    "changed /mcp/tools/rdy_read/input_schema/properties/id/description",
    "changed /mcp/tools/rdy_redact/input_schema/properties/targets/description",
    "added /mcp/tools/rdy_supersede/input_schema/$defs/DecideParams/properties/deviates_from",
    "added /mcp/tools/rdy_supersede/input_schema/properties/adopted_as",
    "added /mcp/tools/rdy_supersede/input_schema/properties/brief",
    "removed /mcp/tools/rdy_supersede/input_schema/properties/decision/$ref",
    "added /mcp/tools/rdy_supersede/input_schema/properties/decision/anyOf",
    "changed /mcp/tools/rdy_supersede/input_schema/properties/decision/description",
    "changed /mcp/tools/rdy_supersede/input_schema/properties/part/description",
    "added /mcp/tools/rdy_supersede/input_schema/properties/view",
    "changed /mcp/tools/rdy_supersede/input_schema/required",
    "changed /schema_packs/rdy_standard/block_types/doc/children",
    "added /schema_packs/rdy_standard/block_types/repo_link",
    "changed /schema_packs/store_only/fields",
    "changed /schema_packs/validation/unknown_pack_key",
    "added /store/activation/kind 1 encoding 1",
    "added /store/activation/kind 10 encoding 1",
    "added /store/activation/kind 17 encoding 1",
    "added /store/activation/kind 18 encoding 1",
    "added /store/activation/kind 19 encoding 1",
    "added /store/activation/kind 20 encoding 1",
    "added /store/encoding_versions/first/FOREIGN",
    "added /store/encoding_versions/latest/FOREIGN",
    "added /store/encoding_versions/optional",
    "changed /store/encoding_versions/registered",
    "added /store/feature_rules/foreign_copy",
    "changed /store/features",
    "added /store/kind_ids/FOREIGN",
    "added /store/ordinals/AnswerAct/Adopt",
    "added /store/ordinals/AnswerAct/KeepPin",
    "changed /store/registered_names",
    "added /sync/fixture_digests/v3-f1-e1-link-delivery.expect.json",
    "added /sync/fixture_digests/v3-f1-e1-link-delivery.rdysync",
    "changed /sync/purposes",
];

/// The baseline members that the current fixture holds in other words, and why. Each is also a
/// member of [`ADDITIONS`]; every other baseline member is held as it is, or as a superset.
const REWORDED: &[(&str, &str)] = &[
    (
        "/derived_identities/scenario",
        "the ids of proposals, landings and later tensions follow the digest of the pack, and the pack gained the repo_link block type; the fixed inputs above it (tension, pair, conflict, consequence) keep every identity",
    ),
    (
        "/errors_warnings/templates/RUN_FIX",
        "the reserved run names gained `adopt-...` (the adopt answer's own run); the words before it are the released ones",
    ),
    (
        "/schema_packs/validation/unknown_pack_key",
        "the refusal lists the keys a pack takes, and the list gained `tensions_across`",
    ),
    (
        "/mcp/tools/rdy_supersede/input_schema/properties/decision",
        "`decision` took the second branch of the call (`adopted_as`): its schema is a reference or null, with the same reference",
    ),
    (
        "/mcp/tools/rdy_supersede/input_schema/required",
        "`decision` is no longer required alone: the call names `decision` or `adopted_as`",
    ),
];

fn exempt(path: &str) -> bool {
    REWORDED.iter().any(|(p, _)| path == *p || path.starts_with(&format!("{p}/")))
        // the words of a parameter's description are help text, not the contract
        || (path.starts_with("/mcp/tools/") && path.ends_with("/description"))
}

/// Holds everything of `was` in `now`: an object keeps every member (an optional member `k?`
/// stands for `k`), an array keeps every element (the current one may hold more, in any place),
/// and a scalar stays the same. What fails is listed in `bad`.
fn retains(was: &Value, now: &Value, path: &str, bad: &mut Vec<String>) {
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
                    Some(y) => retains(x, y, &format!("{path}/{k}"), bad),
                    None => bad.push(format!("{path}/{k}: dropped")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for (i, x) in a.iter().enumerate() {
                let held = b.iter().any(|y| {
                    let mut inner = vec![];
                    retains(x, y, path, &mut inner);
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

fn names(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .expect("a list of names")
        .iter()
        .map(|n| n.as_str().expect("a name").to_string())
        .collect()
}

/// The paths of the commands of a `cli.tree`, as `rdy a b`.
fn command_paths(node: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    for sub in node["subcommands"].as_array().into_iter().flatten() {
        let path = format!("{prefix} {}", sub["name"].as_str().expect("a command name"));
        out.insert(path.clone());
        command_paths(sub, &path, out);
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn digest(v: &Value) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(serde_json::to_vec(v).expect("encode"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// rpl34: the current fixture is the baseline plus the closed list of repo-links additions. A
/// member outside [`ADDITIONS`] changes in neither direction, and each kind of addition is
/// named: the tool, the commands, the codes, the store feature.
#[test]
fn rpl34_contract_additions_are_closed() {
    let (was, now) = (read("release-1.json"), read("repo-links-v1.json"));
    let mut found = vec![];
    leaves(&was, &now, "", &mut found);
    let want: Vec<String> = ADDITIONS.iter().map(|s| (*s).to_string()).collect();
    let extra: Vec<&String> = found.iter().filter(|f| !want.contains(f)).collect();
    let gone: Vec<&String> = want.iter().filter(|w| !found.contains(w)).collect();
    assert!(
        extra.is_empty() && gone.is_empty(),
        "members that moved and are not in ADDITIONS: {extra:#?}; members of ADDITIONS that did not move: {gone:#?}"
    );
    let mut sorted = ADDITIONS.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ADDITIONS.len(), "no repeat");
    // the unrelated rows keep their bytes
    for row in [
        "config",
        "markdown_exports",
        "signatures",
        "site_urls",
        "working_copy",
    ] {
        assert_eq!(digest(&was[row]), digest(&now[row]), "row {row}");
    }
    assert_eq!(was["inventory_version"], now["inventory_version"]);
    // MCP: one tool more, none fewer
    let (old_tools, new_tools) = (
        names(&was["mcp"]["tool_names"]),
        names(&now["mcp"]["tool_names"]),
    );
    assert_eq!(
        new_tools
            .difference(&old_tools)
            .cloned()
            .collect::<BTreeSet<_>>(),
        set(&["rdy_links"])
    );
    assert!(old_tools.is_subset(&new_tools));
    // CLI: five commands more, none fewer
    let (mut old_cmds, mut new_cmds) = (BTreeSet::new(), BTreeSet::new());
    command_paths(&was["cli"]["tree"], "rdy", &mut old_cmds);
    command_paths(&now["cli"]["tree"], "rdy", &mut new_cmds);
    assert_eq!(
        new_cmds
            .difference(&old_cmds)
            .cloned()
            .collect::<BTreeSet<_>>(),
        set(&[
            "rdy adopt-upstream",
            "rdy links",
            "rdy links refs",
            "rdy links show",
            "rdy new repo-link",
        ])
    );
    assert!(old_cmds.is_subset(&new_cmds));
    // errors: three codes more, with their templates; no code gone and none with other text
    let (old_codes, new_codes) = (
        was["errors_warnings"]["codes"].as_object().expect("codes"),
        now["errors_warnings"]["codes"].as_object().expect("codes"),
    );
    let added: BTreeSet<String> = new_codes
        .keys()
        .filter(|k| !old_codes.contains_key(*k))
        .cloned()
        .collect();
    assert_eq!(
        added,
        set(&[
            "E_FOREIGN_READ_ONLY",
            "E_FOREIGN_VERSION",
            "E_INSTRUCTION_LOCKED"
        ])
    );
    for (code, text) in old_codes {
        assert_eq!(
            &new_codes[code], text,
            "{code}: a released code keeps its text"
        );
    }
    // the store: one feature more, with its rule; sync: one purpose more
    let features = |v: &Value| -> BTreeSet<String> {
        v["store"]["features"]
            .as_array()
            .expect("features")
            .iter()
            .map(|f| f["name"].as_str().expect("a feature name").to_string())
            .collect()
    };
    assert_eq!(
        features(&now)
            .difference(&features(&was))
            .cloned()
            .collect::<BTreeSet<_>>(),
        set(&["foreign_copy"])
    );
    assert!(now["store"]["feature_rules"]["foreign_copy"].is_string());
    assert_eq!(
        now["sync"]["purposes"],
        serde_json::json!([[0, "tag_history"], [1, "link_delivery"]])
    );
}

/// rpl34: Release 1 stays compatible. The current fixture holds everything the baseline holds:
/// every tool with every parameter and every refusal, every command with every argument, every
/// code with its text, every template with its text (but the two that [`REWORDED`] names), every
/// derived identity of fixed input, every released fixture digest, every store kind, feature
/// and ordinal. The baseline file itself is the Release 1 baseline: this test never writes it.
#[test]
fn rpl34_release_one_contract_remains_compatible() {
    let (was, now) = (read("release-1.json"), read("repo-links-v1.json"));
    let mut bad = vec![];
    retains(&was, &now, "", &mut bad);
    assert!(
        bad.is_empty(),
        "the current fixture lost baseline members: {bad:#?}"
    );
    // every exemption is a member that really moved, and its reason is written
    for (path, why) in REWORDED {
        assert!(
            ADDITIONS.iter().any(|a| a
                .split_once(' ')
                .is_some_and(|(_, at)| at.starts_with(path))),
            "{path} did not move"
        );
        assert!(!why.is_empty());
    }
    // the fixed inputs keep every derived identity
    for member in [
        "domains",
        "tension",
        "pair_key",
        "conflict_id",
        "consequence_id",
    ] {
        assert_eq!(
            was["derived_identities"][member], now["derived_identities"][member],
            "derived_identities.{member}"
        );
    }
    let (was_scenario, now_scenario) = (
        was["derived_identities"]["scenario"]
            .as_object()
            .expect("scenario"),
        now["derived_identities"]["scenario"]
            .as_object()
            .expect("scenario"),
    );
    assert_eq!(
        was_scenario.keys().collect::<Vec<_>>(),
        now_scenario.keys().collect::<Vec<_>>(),
        "the scenario keeps its parts"
    );
    for part in ["conflicts", "logged_suspects", "transitions", "events"] {
        assert_eq!(was_scenario[part], now_scenario[part], "scenario.{part}");
    }
    // the released fixtures keep their length and digest, and the new ones are added beside them
    for (row, member) in [
        ("dump_load", "fixture_digests"),
        ("sync", "fixture_digests"),
    ] {
        for (name, recorded) in was[row][member].as_object().expect("digests") {
            assert_eq!(&now[row][member][name], recorded, "{row}: {name}");
        }
    }
    // a reworded text keeps its released words
    let run_fix = |v: &Value| {
        v["errors_warnings"]["templates"]["RUN_FIX"]
            .as_str()
            .expect("RUN_FIX")
            .trim_matches('"')
            .to_string()
    };
    let words = "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight";
    assert!(run_fix(&was).starts_with(words));
    assert!(run_fix(&now).starts_with(words));
    assert!(run_fix(&now).contains("adopt-"));
    let unknown = |v: &Value| {
        v["schema_packs"]["validation"]["unknown_pack_key"]
            .as_str()
            .expect("the refusal")
            .to_string()
    };
    assert!(unknown(&now).contains("`tensions`, `tensions_across`, `duplicate_text_min_words`"));
    let supersede = |v: &Value| v["mcp"]["tools"]["rdy_supersede"]["input_schema"].clone();
    assert_eq!(
        supersede(&was)["required"],
        serde_json::json!(["decision", "old"])
    );
    assert_eq!(supersede(&now)["required"], serde_json::json!(["old"]));
    assert_eq!(
        supersede(&was)["properties"]["decision"]["$ref"],
        supersede(&now)["properties"]["decision"]["anyOf"][0]["$ref"],
        "decision keeps its reference"
    );
    // the template `OP_FIX` is the released text again: the links tool's own constant has its own
    // name (a collector that keys templates by name once let it replace the released one)
    let template = |v: &Value, name: &str| v["errors_warnings"]["templates"][name].clone();
    assert_eq!(template(&now, "OP_FIX"), template(&was, "OP_FIX"));
    assert!(
        template(&was, "OP_FIX")
            .as_str()
            .expect("OP_FIX")
            .starts_with("\"each op needs")
    );
    assert_eq!(
        template(&now, "LINKS_OP_FIX"),
        serde_json::json!("\"use op list, show or refs\""),
        "a template is the quoted text of its constant"
    );
    // the tool lists: a released tool keeps its description, and no released parameter is gone
    for (tool, old) in was["mcp"]["tools"].as_object().expect("tools") {
        assert_eq!(
            old["description"], now["mcp"]["tools"][tool]["description"],
            "{tool}"
        );
        // a tool with no parameter has no `properties` member
        let params = |v: &Value| -> BTreeSet<String> {
            v["input_schema"]["properties"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default()
        };
        assert!(
            params(old).is_subset(&params(&now["mcp"]["tools"][tool])),
            "{tool} lost a parameter"
        );
    }
}

/// The tools of `names` that have no kungfu row (D-WB5JU: each tool names the ruleset its caller
/// must have loaded, or none).
fn without_kungfu_row(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| rdy_engine::kungfu::policy(n).is_none())
        .cloned()
        .collect()
}

/// rpl34: a tool that the contract lists cannot be missing from the kungfu table. The check names
/// a tool with no row (so a new tool added to the fixture without one fails here), and the new
/// tool `rdy_links` is a warn-mode read of `rdy:read`. No schema discloses the argument.
#[test]
fn rpl34_new_tool_cannot_omit_kungfu() {
    let now = read("repo-links-v1.json");
    let listed: Vec<String> = now["mcp"]["tool_names"]
        .as_array()
        .expect("tool names")
        .iter()
        .map(|n| n.as_str().expect("a tool name").to_string())
        .collect();
    assert!(listed.iter().any(|n| n == "rdy_links"));
    assert_eq!(
        without_kungfu_row(&listed),
        Vec::<String>::new(),
        "a listed tool has no kungfu row"
    );
    // the check does catch an omission
    let mut with_new = listed.clone();
    with_new.push("rdy_a_tool_nobody_registered".to_string());
    assert_eq!(
        without_kungfu_row(&with_new),
        ["rdy_a_tool_nobody_registered"]
    );
    // the new tool's row
    let row = rdy_engine::kungfu::policy("rdy_links").expect("a row for rdy_links");
    assert_eq!(row.ruleset, Some("rdy:read"));
    assert_eq!(row.response, rdy_engine::kungfu::Response::Warn);
    // the hub's names have no row and no tool
    for hub in ["rdy_link_export", "rdy_take_delivery", "rdy_revoke_link"] {
        assert!(rdy_engine::kungfu::policy(hub).is_none(), "{hub}");
        assert!(!listed.iter().any(|n| n == hub), "{hub}");
    }
    // the argument is no parameter of any tool
    for (tool, entry) in now["mcp"]["tools"].as_object().expect("tools") {
        assert!(
            entry["input_schema"]["properties"].get("kungfu").is_none(),
            "{tool} discloses the kungfu argument"
        );
    }
}

/// rpl34: the template collector of the contract refuses a template name that two constants
/// share with different text, and names both files. The pairs that already existed when Release 1
/// was captured (`LIMIT_FIX`, `WALK_FIX`) keep the text of the file that sorts last.
#[test]
fn rpl34_a_second_constant_cannot_shadow_a_released_template() {
    use crate::release_contract::{RELEASED_SHARED_TEMPLATES, collect_templates, template_regex};
    let consts = template_regex();
    let file = |path: &str, name: &str, text: &str| {
        (
            path.to_string(),
            format!("const {name}: &str = \"{text}\";\n"),
        )
    };
    // two texts under one name: refused, and both files are named
    let refused = collect_templates(
        &[
            file("crates/a/src/write.rs", "OP_FIX", "each op needs op"),
            file(
                "crates/b/src/links.rs",
                "OP_FIX",
                "use op list, show or refs",
            ),
        ],
        &consts,
    )
    .expect_err("one name, two texts");
    assert!(refused.contains("OP_FIX"), "{refused}");
    assert!(refused.contains("crates/a/src/write.rs"), "{refused}");
    assert!(refused.contains("crates/b/src/links.rs"), "{refused}");
    // the same text twice is one template
    let same = collect_templates(
        &[
            file("crates/a/src/x.rs", "NO_LATER_FIX", "name a later"),
            file("crates/b/src/y.rs", "NO_LATER_FIX", "name a later"),
        ],
        &consts,
    )
    .expect("the same text is no shadow");
    assert_eq!(same.len(), 1);
    // a message constant is a template too
    assert!(
        collect_templates(
            &[
                file("crates/a/src/x.rs", "W_X_MESSAGE", "one"),
                file("crates/b/src/y.rs", "W_X_MESSAGE", "two"),
            ],
            &consts
        )
        .is_err()
    );
    // a name that is no template is none of the collector's business
    assert!(
        collect_templates(
            &[
                file("crates/a/src/x.rs", "LIMIT", "one"),
                file("crates/b/src/y.rs", "LIMIT", "two"),
            ],
            &consts
        )
        .expect("not a template")
        .is_empty()
    );
    // the two released pairs: the last file's text stands, as the baseline records it
    assert_eq!(RELEASED_SHARED_TEMPLATES, ["LIMIT_FIX", "WALK_FIX"]);
    let released = collect_templates(
        &[
            file("crates/a/src/trace.rs", "WALK_FIX", "first"),
            file("crates/b/src/neighbors.rs", "WALK_FIX", "second"),
        ],
        &consts,
    )
    .expect("a released pair");
    assert_eq!(released["WALK_FIX"], "\"second\"");
    // the real baseline holds exactly that text of the pair, and every released template of
    // the pairs is one entry
    let was = read("release-1.json");
    assert!(was["errors_warnings"]["templates"]["WALK_FIX"].is_string());
    assert!(was["errors_warnings"]["templates"]["LIMIT_FIX"].is_string());
}
