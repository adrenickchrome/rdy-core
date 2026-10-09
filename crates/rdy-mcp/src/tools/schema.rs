//! rdy_schema: the schema and the write reference.

use super::*;
use crate::view_tools;
use rdy_engine::*;
use rdy_schema::{BodyRule, Trait};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SchemaParams {
    /// Describe one block type; omit to list all types.
    pub block_type: Option<String>,
}

pub fn rdy_schema(st: &mut McpState, p: SchemaParams) -> Result<Value, RdyError> {
    let st_ = st.engine.read();
    let pack = st_.pack();
    let traits = |ts: &std::collections::BTreeSet<Trait>| {
        ts.iter()
            .map(|t| format!("{t:?}").to_lowercase())
            .collect::<Vec<_>>()
    };
    match p.block_type {
        Some(t) => {
            let def = pack.block_types.get(&t).ok_or_else(|| {
                RdyError::new(
                    E_BAD_INPUT,
                    format!("unknown block type {t:?}"),
                    "call rdy_schema without block_type to list types",
                )
            })?;
            let fields: serde_json::Map<String, Value> = def.fields.iter().map(|(k, f)| (k.clone(), json!({"kind": format!("{:?}", f.kind).to_lowercase(), "required": f.required, "values": f.values}))).collect();
            let example_fields: serde_json::Map<String, Value> = def
                .fields
                .iter()
                .filter(|(_, f)| f.required)
                .map(|(k, f)| {
                    (
                        k.clone(),
                        f.values.first().map(|v| json!(v)).unwrap_or(json!("…")),
                    )
                })
                .collect();
            let mut example = json!({"op": "create", "type": t, "parent": "<section id>", "fields": example_fields});
            if def.body == BodyRule::Required {
                example["body"] = json!("…");
            }
            let mut out = json!({"type": t, "prefix": def.prefix, "class": pack.class_of(&t).as_str(), "traits": traits(&def.traits), "body": format!("{:?}", def.body).to_lowercase(), "fields": fields, "children": def.children, "example": example});
            out["links"] = json!({"shape": LINK_SHAPE, "example": link_example(), "update": LINK_UPDATE, "kinds": pack.link_types});
            if rdy_model::tree::is_tree_type(&t) {
                out["example"]["parent"] = json!("<folder id or path; omit for the top level>");
                out["name_rules"] = json!(NAME_RULES);
            }
            // trk1: fields that stay writable on an accepted or rejected decision
            if !def.tracking.is_empty() {
                out["tracking"] = json!(def.tracking);
            }
            Ok(out)
        }
        None => {
            let entity_types: Vec<&String> = pack
                .block_types
                .iter()
                .filter(|(_, d)| d.traits.contains(&Trait::Defines))
                .map(|(n, _)| n)
                .collect();
            Ok(json!({
                "pack": pack.name,
                "types": pack.block_types.iter().map(|(n, d)| {
                    let mut row = json!({"type": n, "prefix": d.prefix, "class": pack.class_of(n).as_str(), "traits": traits(&d.traits)});
                    if !d.tracking.is_empty() {
                        row["tracking"] = json!(d.tracking);
                    }
                    row
                }).collect::<Vec<_>>(),
                "link_types": pack.link_types,
                "areas": pack.areas.iter().map(|(n, a)| json!({"name": n, "description": a.description, "paths": a.paths})).collect::<Vec<_>>(),
                "link_shape": {"shape": LINK_SHAPE, "example": link_example(), "update": LINK_UPDATE},
                "ops": WRITE_OPS.iter().map(|(n, s)| (n.to_string(), json!(s))).collect::<serde_json::Map<_, _>>(),
                "views": view_tools::SCHEMA_DOC,
                "intents": crate::intent_tools::INTENT_DOC,
                "alternatives": crate::alt_tools::ALT_DOC,
                "tensions": rdy_engine::tensions::api::TENSIONS_DOC,
                "decisions": {"home": st_.decisions_home(), "rule": DECISIONS_RULE},
                "entities": {"types": entity_types, "rule": ENTITY_RULE},
                "limits": {"ops_per_write": MAX_OPS_PER_WRITE, "request_bytes": MAX_REQUEST_BYTES, "asset_bytes": MAX_ASSET_BYTES, "asset_bytes_stdio": MAX_ASSET_BYTES_STDIO, "max_tokens": MAX_TOKENS_LIMIT, "socket_line_bytes": MAX_REQUEST_BYTES, "transport": TRANSPORT_NOTE},
                "errors": ERROR_ROWS.iter().map(|(c, w, f)| (c.to_string(), json!({"when": w, "fix": f}))).collect::<serde_json::Map<_, _>>(),
                "doc_types": pack.doc_types.iter().map(|(n, d)| json!({"doc_type": n, "skeleton": d.skeleton.iter().map(|s| json!({"title": s.title, "required": s.required})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                "text": TEXT_RULES,
                "edit_loop": EDIT_LOOP_DOC,
            }))
        }
    }
}
