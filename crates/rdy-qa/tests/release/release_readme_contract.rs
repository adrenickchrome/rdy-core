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
//!
//! Task 2 adds the figure's optional `dark_blob` and `dark_media_type` pair and its probe, the
//! incompatible store feature `figure_dark`, the dark dump fixture's digests, and the
//! publication comparison's attachment rows with their `variant` (seen through a probe project
//! that now changes figures). The derived identities follow the pack again.
//!
//! Task 3 adds the single Markdown layout (D-ESHXM, D-6PR3Y, D-H1FRY): the `assets` parameter
//! and the `single` layout of `rdy_export` and `rdy export`, the layout's files and result in
//! the Markdown exports with each layout's file of a D-7M4TR list-part doc, the texts of its
//! refusals and warning, and the export probes that replace the export's rows among the commands
//! and tools that are not probed.
//!
//! Every changed member of the current fixture is in exactly one task's list or in
//! `DERIVED_CHANGES`, which follows the standard pack's current digest; each task's test checks
//! the whole closed list and the content of its own members.
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

/// Task 1: its members of `readme-from-rdy-v1.json` that differ from `m4bc-v1.json`, as
/// `added|removed|changed <path>`. The fixtures' lists of identified entries are compared by
/// identity. The schema field and its probe are Task 1's own change.
const RFR1_CHANGES: &[&str] = &[
    "added /schema_packs/rdy_standard/block_types/section/fields/collapsed",
    "added /schema_packs/validation/section_collapsed",
];

/// The derived identities: the derived-identities scenario hashes the standard pack into every
/// landing id, so these members move with each task that changes the pack (here, Task 2's).
const DERIVED_CHANGES: &[&str] = &[
    "changed /derived_identities/scenario/inboxes/agent:a/a",
    "changed /derived_identities/scenario/logged_tensions",
    "removed /derived_identities/scenario/tensions/tn-31af126dccec",
    "removed /derived_identities/scenario/tensions/tn-c7804141f02d",
    "added /derived_identities/scenario/tensions/tn-0a79891d1de5",
    "added /derived_identities/scenario/tensions/tn-bb161e5eeac7",
];

/// Task 2: its members of `readme-from-rdy-v1.json` that differ from `m4bc-v1.json`.
const RFR2_CHANGES: &[&str] = &[
    "added /schema_packs/rdy_standard/block_types/figure/fields/dark_blob",
    "added /schema_packs/rdy_standard/block_types/figure/fields/dark_media_type",
    "added /schema_packs/validation/figure_dark",
    "added /store/features/name=figure_dark",
    "changed /store/registered_names",
    "added /store/feature_rules/figure_dark",
    "added /dump_load/fixture_digests/v4-f1-e1-figure-dark.expect.json",
    "added /dump_load/fixture_digests/v4-f1-e1-figure-dark.rdydump",
    "changed /cli/runtime/probes/publication check against/stdout/disclosure/added_attachments",
    "changed /cli/runtime/probes/publication check against/stdout/disclosure/changed_attachments",
    "changed /cli/runtime/probes/publication check against/stdout/disclosure/removed_attachments",
    "changed /cli/runtime/probes/publication check against/stdout/disclosure/changed_docs",
    "changed /mcp/result_fields/rdy_publication_check against/ok/disclosure/added_attachments",
    "changed /mcp/result_fields/rdy_publication_check against/ok/disclosure/changed_attachments",
    "changed /mcp/result_fields/rdy_publication_check against/ok/disclosure/removed_attachments",
    "changed /mcp/result_fields/rdy_publication_check against/ok/disclosure/changed_docs",
];

/// Task 3: its members of `readme-from-rdy-v1.json` that differ from `m4bc-v1.json`.
const RFR3_CHANGES: &[&str] = &["TO BE CAPTURED"];

/// Every changed member of the current fixture against `m4bc-v1.json` is in one task's list or
/// in `DERIVED_CHANGES`, and none else moved: a member that is not listed is byte-identical.
fn assert_closed(old: &Value, new: &Value) {
    let mut found = vec![];
    leaves(&keyed(old), &keyed(new), "", &mut found);
    let mut want: Vec<String> = [RFR1_CHANGES, RFR2_CHANGES, RFR3_CHANGES, DERIVED_CHANGES]
        .concat()
        .iter()
        .map(|p| (*p).to_string())
        .collect();
    found.sort();
    want.sort();
    assert_eq!(
        found, want,
        "every changed path of readme-from-rdy-v1.json is listed, and none else"
    );
}

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
    assert_closed(&old, &new);

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
    // the figure's own change is Task 2's (`rfr2_contract_adds_only_image_support_and_feature`)
    for ty in ["prose", "claim", "note", "doc", "table", "figure"] {
        let p = format!("/schema_packs/rdy_standard/block_types/{ty}");
        if ty != "figure" {
            assert_eq!(old.pointer(&p), new.pointer(&p), "{ty} is unchanged");
        }
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

/// The shape of one attachment row of the publication comparison: Task 2 adds the optional
/// `variant` (D-TBDA1), which a dark image's row sets and a light image's row leaves out.
fn attachment_row() -> Value {
    serde_json::json!({
        "available": "bool", "blob": "string", "block": "string", "home_doc": "string",
        "variant?": "string"
    })
}

#[test]
fn rfr2_contract_adds_only_image_support_and_feature() {
    assert_eq!(sha("m4bc-v1.json"), M4BC_V1_SHA, "m4bc-v1.json is frozen");
    let (old, new) = (read("m4bc-v1.json"), read("readme-from-rdy-v1.json"));
    assert_closed(&old, &new);

    // the schema: two optional text fields on the figure, and nothing else of any type moved
    let types = "/schema_packs/rdy_standard/block_types";
    let optional = serde_json::json!({"kind": "str", "required": false, "values": []});
    for f in ["dark_blob", "dark_media_type"] {
        assert_eq!(
            new.pointer(&format!("{types}/figure/fields/{f}")),
            Some(&optional),
            "{f}"
        );
    }
    let figure = |v: &Value| -> Value {
        let mut t = v
            .pointer(&format!("{types}/figure"))
            .expect("the figure")
            .clone();
        let fields = t["fields"].as_object_mut().expect("its fields");
        fields.remove("dark_blob");
        fields.remove("dark_media_type");
        t
    };
    assert_eq!(figure(&old), figure(&new), "the figure's other members");
    let names = |v: &Value| -> BTreeSet<String> {
        v.pointer(types)
            .and_then(Value::as_object)
            .expect("the block types")
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(names(&old), names(&new));
    for ty in names(&new) {
        if ty == "figure" || ty == "section" {
            continue;
        }
        let p = format!("{types}/{ty}");
        assert_eq!(old.pointer(&p), new.pointer(&p), "{ty} is unchanged");
    }

    // the D-6PR3Y probe: no issue for a light figure or the pair; one field alone is refused
    let pair_refusal = serde_json::json!([[
        "E_VALIDATION",
        "fg-F1G01's dark_blob and dark_media_type are set together",
        "set both fields, or neither"
    ]]);
    assert_eq!(
        new.pointer("/schema_packs/validation/figure_dark"),
        Some(&serde_json::json!({
            "light_only": [],
            "pair": [],
            "dark_blob_only": pair_refusal,
            "dark_media_type_only": pair_refusal,
            "bad_kind": [["E_FIELD_KIND", "field \"dark_blob\" must be Str", "write dark_blob as a str value"]],
        }))
    );

    // the store feature: incompatible, first shipped by 0.51.0, with its rule; the other
    // features, the classes, the activation and every format and encoding member are as before
    let features = |v: &Value| -> Vec<Value> {
        v.pointer("/store/features")
            .and_then(Value::as_array)
            .expect("the features")
            .clone()
    };
    let mut want = features(&old);
    want.push(serde_json::json!({"class": "incompatible", "name": "figure_dark", "required_rdy": "0.51.0"}));
    let key = |f: &Value| f["name"].as_str().unwrap_or_default().to_string();
    want.sort_by_key(key);
    let mut got = features(&new);
    got.sort_by_key(key);
    assert_eq!(got, want);
    let mut names = old["store"]["registered_names"]
        .as_array()
        .expect("the registered names")
        .clone();
    names.push(Value::from("figure_dark"));
    names.sort_by_key(|n| n.as_str().unwrap_or_default().to_string());
    assert_eq!(new["store"]["registered_names"], Value::Array(names));
    assert_eq!(
        new.pointer("/store/feature_rules/figure_dark"),
        Some(&Value::from(
            "a figure BLOCK whose dark_blob is set, alone or in a CHANGE's create or update"
        ))
    );
    for p in [
        "/store/activation",
        "/store/classes",
        "/store/encoding_versions",
    ] {
        assert_eq!(old.pointer(p), new.pointer(p), "{p}");
    }

    // the dark dump fixture's two files, as the files on disk are
    let dump = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/dump");
    for f in [
        "v4-f1-e1-figure-dark.rdydump",
        "v4-f1-e1-figure-dark.expect.json",
    ] {
        let b = std::fs::read(dump.join(f)).expect("the dark dump fixture");
        assert_eq!(
            new["dump_load"]["fixture_digests"][f],
            Value::from(format!("{} {}", b.len(), rdy_model::Hash::of(&b))),
            "{f}"
        );
    }

    // the comparison's attachment rows (D-TBDA1), alike over the CLI and the chat tool; the probe
    // project now changes figures, so its changed docs are seen too
    let mut changed = attachment_row();
    let m = changed.as_object_mut().expect("an object");
    for (k, v) in [
        ("available", "bool"),
        ("blob", "string"),
        ("home_doc", "string"),
    ] {
        m.remove(k);
        m.insert(format!("before_{k}"), Value::from(v));
        m.insert(format!("after_{k}"), Value::from(v));
    }
    for at in [
        "/cli/runtime/probes/publication check against/stdout/disclosure",
        "/mcp/result_fields/rdy_publication_check against/ok/disclosure",
    ] {
        let d = new.pointer(at).expect("the disclosure");
        assert_eq!(
            d["added_attachments"],
            serde_json::json!([attachment_row()]),
            "{at}"
        );
        assert_eq!(
            d["removed_attachments"],
            serde_json::json!([attachment_row()]),
            "{at}"
        );
        assert_eq!(
            d["changed_attachments"],
            serde_json::json!([changed]),
            "{at}"
        );
        assert_eq!(
            d["changed_docs"],
            serde_json::json!([{
                "after_digest": "string", "after_path": "string", "before_digest": "string",
                "before_path": "string", "id": "string", "redacted": "bool"
            }]),
            "{at}"
        );
        let before = old.pointer(at).expect("the disclosure before");
        for k in [
            "added_attachments",
            "removed_attachments",
            "changed_attachments",
            "changed_docs",
        ] {
            assert_eq!(before[k], serde_json::json!([]), "{at}/{k} was empty");
        }
    }

    // the derived identities move as id text only
    assert_eq!(
        without_ids(&old["derived_identities"]),
        without_ids(&new["derived_identities"]),
        "the derived identities differ in their ids alone"
    );
}

#[test]
fn rfr3_contract_adds_only_single_export() {
    assert_eq!(sha("m4bc-v1.json"), M4BC_V1_SHA, "m4bc-v1.json is frozen");
    let (old, new) = (read("m4bc-v1.json"), read("readme-from-rdy-v1.json"));
    assert_closed(&old, &new);

    // the tool's parameters: the layout `single` and the optional `assets` folder
    let schema = new
        .pointer("/mcp/tools/rdy_export/input_schema")
        .expect("the export tool's schema");
    assert_eq!(
        schema["$defs"]["ExportLayout"]["enum"],
        serde_json::json!(["mkdocs", "techdocs", "single"])
    );
    assert_eq!(
        schema["properties"]["assets"]["type"],
        serde_json::json!(["string", "null"])
    );
    let required = schema["required"].as_array().expect("required");
    assert!(
        !required.contains(&Value::from("assets")),
        "assets is optional"
    );
    let keys = |v: &Value| -> BTreeSet<String> {
        v["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .cloned()
            .collect()
    };
    let mut was = keys(&old["mcp"]["tools"]["rdy_export"]["input_schema"]);
    was.insert("assets".into());
    assert_eq!(keys(schema), was, "only assets is added");

    // the command line: `--assets` and the layout `single` on `rdy export`
    let export = new["cli"]["tree"]["subcommands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|c| c["name"] == "export")
        .expect("rdy export");
    let text = export.to_string();
    assert!(text.contains("assets"), "{text}");
    assert!(text.contains("single"), "{text}");

    // the single file: no front matter or anchor; its image named by the SHA-256 of its bytes
    let single = new
        .pointer("/markdown_exports/layouts/single")
        .expect("the single layout");
    let readme = &single["files"]["README.md"];
    assert_eq!(readme["front_matter_keys"], Value::Null);
    assert_eq!(readme["anchors"], serde_json::json!([]));
    assert!(
        readme["headings"][0]
            .as_str()
            .is_some_and(|h| h.starts_with("# ")),
        "{readme}"
    );
    for (name, entry) in single["files"].as_object().expect("files") {
        if let Some(file) = name.strip_prefix("assets/") {
            let (stem, _) = file.split_once('.').expect("an extension");
            assert_eq!(stem.len(), 64, "{name}");
            assert!(entry["len"].is_u64(), "{name}");
        }
    }
    assert_eq!(single["result"]["layout"], "string", "{single}");
    assert_eq!(single["result"]["pages"], "null", "{single}");
    // each layout writes a list item's parts: the paragraph and the code stay in their item
    for layout in ["plain", "mkdocs", "techdocs", "single"] {
        let parts = new["markdown_exports"]["layouts"][layout]["list_parts"]
            .as_str()
            .unwrap_or_else(|| panic!("{layout} has a list-part file"));
        assert!(parts.contains("```sh\n"), "{layout}: {parts}");
        assert!(parts.contains("rdy init ~/p\n"), "{layout}: {parts}");
        assert!(
            parts.contains("A paragraph of its own.\n"),
            "{layout}: {parts}"
        );
        assert!(
            parts.contains("<b> *not em* `x`\n"),
            "{layout}: literal code: {parts}"
        );
    }
    assert_eq!(
        new["markdown_exports"]["layouts"]["single"]["list_parts"],
        Value::from(
            "# Parts\n\n1. **Make a project.**\n\n   ```sh\n   rdy init ~/p\n   ```\n\n2. Second\n   line.\n\n   A paragraph of its own.\n\n   ```text\n   <b> *not em* `x`\n   ```\n\n   - nested\n"
        )
    );

    // the texts of the refusals and the warning
    let templates = &new["errors_warnings"]["templates"];
    for (name, text) in [
        (
            "SINGLE_MD_DOCS_MESSAGE",
            "a single-file Markdown export takes one doc; the selection holds <n>",
        ),
        (
            "SINGLE_MD_DOCS_FIX",
            "choose a profile that selects one doc",
        ),
        (
            "SINGLE_MD_FIGURE_MESSAGE",
            "figure <id> needs --assets <dir>",
        ),
        (
            "SINGLE_MD_FIGURE_FIX",
            "pass --assets with a folder beside the file",
        ),
        (
            "SINGLE_MD_ASSETS_PATH_MESSAGE",
            "--assets <path> must be a folder below the folder of --out",
        ),
        (
            "SINGLE_MD_ASSETS_PATH_FIX",
            "pass a relative folder path with no .. part, such as assets",
        ),
        ("SINGLE_MD_ASSETS_FULL_MESSAGE", "<path> is not empty"),
        (
            "SINGLE_MD_ASSETS_FULL_FIX",
            "choose an empty directory (or one that does not exist yet) so no stale files mix into the export",
        ),
        (
            "SINGLE_MD_ASSETS_FILE_MESSAGE",
            "<path> is a file: --assets names a folder",
        ),
        (
            "SINGLE_MD_ASSETS_FILE_FIX",
            "pass a folder path such as assets",
        ),
        (
            "SINGLE_MD_REF_MESSAGE",
            "warning: <doc path>: <block id>: the ref to <target id> is not in this file; it is written as its label",
        ),
        (
            "EXPORT_ASSETS_MESSAGE",
            "--assets applies to --format md --layout single only",
        ),
        (
            "EXPORT_ASSETS_FIX",
            "drop --assets, or export with --format md --layout single",
        ),
    ] {
        assert_eq!(templates[name], Value::from(text), "{name}");
    }

    // the probes: the single file's result and its refusal, over the command line and the tool
    for (at, ok, refused) in [
        ("/cli/runtime/probes/export single", "stdout", "stderr"),
        ("/mcp/result_fields/rdy_export single", "ok", "refused"),
    ] {
        let probe = new.pointer(at).unwrap_or_else(|| panic!("{at}"));
        let result = &probe[ok];
        for k in [
            "out", "format", "layout", "docs", "folders", "assets", "pages", "bytes", "warnings",
        ] {
            assert!(result.get(k).is_some(), "{at}: {k} in {result}");
        }
        assert_eq!(
            result["warnings"],
            serde_json::json!(["string"]),
            "{at}: populated"
        );
        assert!(probe[refused].is_object(), "{at}: a refusal is probed");
    }
    // a tree export's result has no layout
    for at in [
        "/cli/runtime/probes/export/stdout",
        "/mcp/result_fields/rdy_export/ok",
    ] {
        let r = new.pointer(at).unwrap_or_else(|| panic!("{at}"));
        assert!(r.get("layout").is_none(), "{at}: {r}");
    }
    // the export is probed now, so it leaves the lists of what is not probed
    assert!(
        new.pointer("/cli/runtime/not_probed_commands/export")
            .is_none()
    );
    assert!(
        new.pointer("/mcp/result_fields_not_captured/rdy_export")
            .is_none()
    );
    assert!(
        old.pointer("/cli/runtime/not_probed_commands/export")
            .is_some()
    );
    assert!(
        old.pointer("/mcp/result_fields_not_captured/rdy_export")
            .is_some()
    );
}
