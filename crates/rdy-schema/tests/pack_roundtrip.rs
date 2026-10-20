//! hpb1 (D-KDFZ3): the effective pack is a render input, so it serializes in full and reads back
//! equal. The serialized form holds the pack's own fields only: no original YAML, config path,
//! remote, credential or signing setting.
use rdy_schema::*;
use serde_json::Value;
use std::collections::BTreeSet;

/// A child of the standard pack that sets every optional part a pack can hold.
const TEAM: &str = r#"
name: team
version: "7"
extends: rdy-standard
link_types: [blocks]
default_doc_type: page
duplicate_text_min_words: 5
tensions:
  fork: advisory
  value_disagreement: advisory
areas:
  payments:
    description: money in and out
    paths: [wallets, payments/**]
  identity:
    paths: [identity/*]
coverage:
  weights: {governed: 2, connected: 0}
  dismissed_weight: 25
  drift_threshold: 70
block_types:
  risk:
    prefix: rk
    traits: [normative, informative]
    body: forbidden
    class: supporting
    tracking: [owner]
    children: [claim]
    fields:
      owner: { kind: str }
      level: { kind: enum, required: true, values: [low, high] }
      tags: { kind: list }
      extra: { kind: map }
      score: { kind: int }
      open: { kind: bool }
doc_types:
  brief:
    nav: false
    skeleton:
      - { title: Summary, required: true }
      - { title: Notes }
"#;

fn team() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set.add_yaml(TEAM).unwrap();
    set.resolve(&name).unwrap()
}

fn keys(v: &Value) -> BTreeSet<&str> {
    v.as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn set(names: &[&'static str]) -> BTreeSet<&'static str> {
    names.iter().copied().collect()
}

fn serializable<T: serde::Serialize>() {}

#[test]
fn hpb1_pack_roundtrips_without_config_or_paths() {
    // the closed derive list
    serializable::<Trait>();
    serializable::<FieldKind>();
    serializable::<FieldDef>();
    serializable::<BodyRule>();
    serializable::<BlockTypeDef>();
    serializable::<SkeletonSection>();
    serializable::<DocTypeDef>();
    serializable::<AreaDef>();
    serializable::<Pack>();
    serializable::<CoverageSettings>();

    for pack in [
        PackSet::with_builtin().resolve("rdy-standard").unwrap(),
        team(),
    ] {
        let json = serde_json::to_string(&pack).unwrap();
        let back: Pack = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pack, "{} reads back equal", pack.name);
        assert_eq!(serde_json::to_string(&back).unwrap(), json, "stable bytes");

        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            keys(&v),
            set(&[
                "name",
                "version",
                "extends",
                "link_types",
                "block_types",
                "doc_types",
                "default_doc_type",
                "tensions",
                "duplicate_text_min_words",
                "areas",
                "coverage",
            ]),
            "the pack's own fields only"
        );
        for t in v["block_types"].as_object().unwrap().values() {
            assert_eq!(
                keys(t),
                set(&[
                    "prefix", "traits", "fields", "children", "body", "class", "tracking"
                ])
            );
            for f in t["fields"].as_object().unwrap().values() {
                assert_eq!(keys(f), set(&["kind", "required", "values"]));
            }
        }
        for d in v["doc_types"].as_object().unwrap().values() {
            assert_eq!(keys(d), set(&["skeleton", "nav"]));
            for s in d["skeleton"].as_array().unwrap() {
                assert_eq!(keys(s), set(&["title", "required"]));
            }
        }
        for a in v["areas"].as_object().unwrap().values() {
            assert_eq!(keys(a), set(&["description", "paths"]));
        }
        if !v["coverage"].is_null() {
            assert_eq!(
                keys(&v["coverage"]),
                set(&["weights", "dismissed_weight", "drift_threshold"])
            );
        }
        // ordered maps and sets keep their order: block types and link types ascend
        let types: Vec<&String> = pack.block_types.keys().collect();
        let at: Vec<usize> = types
            .iter()
            .map(|t| json.find(&format!("\"{t}\":{{\"prefix\"")).unwrap())
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "block types ascend");
        let links: Vec<&str> = v["link_types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        let mut sorted = links.clone();
        sorted.sort_unstable();
        assert_eq!(links, sorted, "link types ascend");
    }

    // the child pack's values, by wire name
    let v = serde_json::to_value(team()).unwrap();
    assert_eq!(v["tensions"]["fork"], "advisory");
    assert_eq!(v["coverage"]["weights"]["governed"], 2);
    assert_eq!(v["block_types"]["risk"]["class"], "supporting");
    assert_eq!(v["block_types"]["risk"]["body"], "forbidden");
    assert_eq!(v["block_types"]["risk"]["fields"]["level"]["kind"], "enum");
    assert_eq!(v["block_types"]["risk"]["traits"][0], "normative");
    assert_eq!(v["doc_types"]["brief"]["nav"], false);

    // the strict fields stay strict on the way back in
    let mut bad = v.clone();
    bad["remote"] = Value::from("https://hub.example.invalid/team");
    let e = serde_json::from_value::<Pack>(bad).unwrap_err().to_string();
    assert!(e.starts_with("unknown field `remote`"), "{e}");
    let mut bad = v;
    bad["block_types"]["risk"]["signing"] = Value::from("key");
    let e = serde_json::from_value::<Pack>(bad).unwrap_err().to_string();
    assert!(e.starts_with("unknown field `signing`"), "{e}");
}
