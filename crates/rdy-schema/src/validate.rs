use crate::pack::{BodyRule, FieldKind, Pack};
use rdy_model::{Block, BlockId, FieldValue};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub severity: Severity,
    pub code: &'static str,
    pub block: Option<BlockId>,
    pub message: String,
    pub fix: String,
}

fn err(block: &Block, code: &'static str, message: String, fix: String) -> Issue {
    Issue {
        severity: Severity::Error,
        code,
        block: Some(block.id.clone()),
        message,
        fix,
    }
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.severity == Severity::Error)
}

fn kind_matches(kind: FieldKind, v: &FieldValue) -> bool {
    matches!(
        (kind, v),
        (FieldKind::Str | FieldKind::Enum, FieldValue::Str(_))
            | (FieldKind::Int, FieldValue::Int(_))
            | (FieldKind::Bool, FieldValue::Bool(_))
            | (FieldKind::List, FieldValue::List(_))
            | (FieldKind::Map, FieldValue::Map(_))
    )
}

/// The pack-YAML spelling of a field kind (`str`, `int`, ...).
fn kind_word(kind: FieldKind) -> String {
    format!("{kind:?}").to_lowercase()
}

pub fn validate_block(pack: &Pack, block: &Block, parent: Option<&Block>) -> Vec<Issue> {
    let mut out = vec![];
    let Some(def) = pack.block_types.get(&block.block_type) else {
        let known: Vec<&str> = pack.block_types.keys().map(String::as_str).collect();
        out.push(err(
            block,
            "E_UNKNOWN_TYPE",
            format!("unknown block type {:?}", block.block_type),
            format!("use one of: {}", known.join(", ")),
        ));
        return out;
    };
    if block.id.prefix() != def.prefix {
        out.push(err(
            block,
            "E_PREFIX",
            format!(
                "id {} has prefix {:?} but type {} uses {:?}",
                block.id,
                block.id.prefix(),
                block.block_type,
                def.prefix
            ),
            format!(
                "omit the id to let rdy generate one, or use the prefix {:?}",
                def.prefix
            ),
        ));
    }
    for (name, v) in &block.fields {
        match def.fields.get(name) {
            None => out.push(err(
                block,
                "E_FIELD_UNKNOWN",
                format!("type {} has no field {name:?}", block.block_type),
                format!(
                    "remove it, or use one of: {}",
                    def.fields.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            )),
            Some(fd) if !kind_matches(fd.kind, v) => out.push(err(
                block,
                "E_FIELD_KIND",
                format!("field {name:?} must be {:?}", fd.kind),
                format!("write {name} as a {} value", kind_word(fd.kind)),
            )),
            Some(fd)
                if fd.kind == FieldKind::Enum
                    && !fd.values.iter().any(|a| Some(a.as_str()) == v.as_str()) =>
            {
                out.push(err(
                    block,
                    "E_FIELD_ENUM",
                    format!("field {name:?} has value {:?}", v.as_str().unwrap_or("")),
                    format!("use one of: {}", fd.values.join(", ")),
                ))
            }
            Some(_) => {}
        }
    }
    for (name, fd) in &def.fields {
        if fd.required && !block.fields.contains_key(name) {
            out.push(err(
                block,
                "E_FIELD_REQUIRED",
                format!("type {} requires field {name:?}", block.block_type),
                format!("add {name} ({})", kind_word(fd.kind)),
            ));
        }
    }
    match def.body {
        BodyRule::Required if block.body.trim().is_empty() => out.push(err(
            block,
            "E_BODY_REQUIRED",
            format!("type {} requires a body", block.block_type),
            "write the block's text in body".into(),
        )),
        BodyRule::Forbidden if !block.body.is_empty() => out.push(err(
            block,
            "E_BODY_FORBIDDEN",
            format!("type {} must not have a body", block.block_type),
            "move the text into a child prose block".into(),
        )),
        _ => {}
    }
    for name in block.links.keys() {
        if !pack.link_types.contains(name) {
            out.push(err(
                block,
                "E_LINK_TYPE",
                format!("unknown link type {name:?}"),
                format!(
                    "use one of: {}",
                    pack.link_types
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    let is_doc = block.block_type == "doc";
    match (is_doc, parent, &block.parent) {
        (true, _, Some(_)) => out.push(err(
            block,
            "E_ROOT_PARENT",
            "a doc cannot have a parent".into(),
            "remove parent".into(),
        )),
        (false, _, None) => out.push(err(
            block,
            "E_ORPHAN",
            format!("{} must have a parent", block.block_type),
            "set parent to the section that should contain it".into(),
        )),
        (false, Some(p), Some(_)) => {
            let allowed = pack
                .block_types
                .get(&p.block_type)
                .map(|d| d.children.contains(&block.block_type))
                .unwrap_or(false);
            if !allowed {
                out.push(err(
                    block,
                    "E_CHILD_TYPE",
                    format!("a {} cannot contain a {}", p.block_type, block.block_type),
                    "place it under a section (or another allowed parent type)".into(),
                ));
            }
        }
        (false, None, Some(pid)) => out.push(err(
            block,
            "E_MISSING_PARENT",
            format!("{} has parent {pid}, which does not exist", block.id),
            format!(
                "move {} under an existing block, or recreate {pid}",
                block.id
            ),
        )),
        (true, _, None) => {}
    }
    if is_doc
        && let Some(dt) = block.field_str("doc_type")
        && !pack.doc_types.contains_key(dt)
    {
        out.push(err(
            block,
            "E_DOC_TYPE",
            format!("unknown doc_type {dt:?}"),
            format!(
                "use one of: {}",
                pack.doc_types
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    out
}

pub fn skeleton_gaps(pack: &Pack, doc: &Block, section_titles: &[&str]) -> Vec<Issue> {
    let Some(dt) = doc
        .field_str("doc_type")
        .and_then(|t| pack.doc_types.get(t))
    else {
        return vec![];
    };
    dt.skeleton
        .iter()
        .filter(|s| s.required && !section_titles.contains(&s.title.as_str()))
        .map(|s| Issue {
            severity: Severity::Warning,
            code: "W_SKELETON_MISSING",
            block: Some(doc.id.clone()),
            message: format!("required section {:?} is missing", s.title),
            fix: format!("add a section titled {:?} under this doc", s.title),
        })
        .collect()
}
