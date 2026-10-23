use crate::glob::Glob;
use crate::pack::{BodyRule, FieldKind, Pack, TypeClass};
use crate::publication;
use crate::supporting;
use rdy_model::{Block, BlockId, FieldValue, RefMode, ref_spans};
use rdy_model::{decision, engine_fields, tree};
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

/// The engine field naming the code a block reflects: a list of path globs, on content block types.
pub const REFLECTS_FIELD: &str = "reflects";

/// The question field holding the labels of its card's options.
const QUESTION_OPTIONS: &str = "options";

/// The most path globs one `reflects` field may hold.
pub const MAX_REFLECTS_GLOBS: usize = 32;

/// The fix of a publication profile placed anywhere but directly under a doc.
const PROFILE_PARENT_FIX: &str = "set parent to a doc id";

pub(crate) fn err(block: &Block, code: &'static str, message: String, fix: String) -> Issue {
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

/// Checks the `reflects` field: a list of at most `MAX_REFLECTS_GLOBS` strings, each a path glob.
fn check_reflects(block: &Block, v: &FieldValue, out: &mut Vec<Issue>) {
    let globs: Option<Vec<&str>> = match v {
        FieldValue::List(items) => items.iter().map(FieldValue::as_str).collect(),
        _ => None,
    };
    let (Some(globs), FieldValue::List(items)) = (globs, v) else {
        out.push(err(
            block,
            "E_FIELD_KIND",
            format!("field {REFLECTS_FIELD:?} must be a list of code path globs"),
            "write reflects as a list of strings such as [crates/*/src/error.rs]".to_string(),
        ));
        return;
    };
    if items.len() > MAX_REFLECTS_GLOBS {
        out.push(err(
            block,
            "E_LIMIT",
            format!(
                "field {REFLECTS_FIELD:?} holds at most {MAX_REFLECTS_GLOBS} path globs; this one has {}",
                items.len()
            ),
            "split the block, or use a broader glob such as src/**/*.rs".to_string(),
        ));
        return;
    }
    for glob in globs {
        if let Err(e) = Glob::compile(glob) {
            out.push(err(
                block,
                "E_BAD_GLOB",
                format!("reflects glob {glob:?}: {e}"),
                "write a path glob relative to the code repo's top folder, such as crates/*/src/error.rs".to_string(),
            ));
        }
    }
}

/// E_PREFIX for a block read from files (import, `rdy check`): its id's prefix belongs to no
/// type, or to a type it could not have been retyped from (rtyp1), so `D-` stays a decision and
/// `c-` never names a supporting type. A write checks the exact prefix when it creates a block.
pub fn validate_prefix(pack: &Pack, block: &Block) -> Option<Issue> {
    let def = pack.block_types.get(&block.block_type)?;
    let prefix = block.id.prefix();
    if prefix == def.prefix
        || pack
            .block_types
            .iter()
            .any(|(from, d)| d.prefix == prefix && pack.may_retype(from, &block.block_type))
    {
        return None;
    }
    Some(err(
        block,
        "E_PREFIX",
        format!(
            "id {} has prefix {:?} but type {} uses {:?}",
            block.id, prefix, block.block_type, def.prefix
        ),
        format!(
            "omit the id to let rdy generate one, or use the prefix {:?}",
            def.prefix
        ),
    ))
}

/// The write schema: every check, whatever the block's read marker says, so a write can never
/// pass a removed block's stub off as content (D-3M81W).
///
/// First, the one field nesting bound every writer keeps (final review ruling 1): a field
/// deeper than [`rdy_model::MAX_FIELD_NESTING`] levels is the only issue, `E_LIMIT` `field
/// nesting exceeds 29 levels`, so no written block is one that its export could not carry. A
/// stored state's reads ([`validate_read_block`]) and fsck do not apply it.
pub fn validate_block(pack: &Pack, block: &Block, parent: Option<&Block>) -> Vec<Issue> {
    if block
        .fields
        .values()
        .any(|v| v.nests_deeper_than(rdy_model::MAX_FIELD_NESTING))
    {
        return vec![err(
            block,
            "E_LIMIT",
            rdy_model::field_nesting_message(),
            rdy_model::field_nesting_fix(),
        )];
    }
    check_block(pack, block, parent, false)
}

/// The read schema of a stored state (D-3M81W). A normal block gets exactly the checks of
/// [`validate_block`]. A trusted read projection of a removed version
/// ([`Block::is_redacted`]) keeps its type, parent and order checks (and `validate_prefix`
/// applies as before); only the checks of what removal made unavailable are skipped: required
/// fields, and the body rule (its body is the notice). A name equal to its own id (the neutral
/// segment a working copy names a removed doc's file or folder's directory by) is not checked.
pub fn validate_read_block(pack: &Pack, block: &Block, parent: Option<&Block>) -> Vec<Issue> {
    check_block(pack, block, parent, block.is_redacted())
}

/// `projection`: skip the required-field and body checks, which a removed version cannot meet,
/// and the name check of a name equal to the block's id.
fn check_block(pack: &Pack, block: &Block, parent: Option<&Block>, projection: bool) -> Vec<Issue> {
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
    for (name, v) in &block.fields {
        if name == "key" {
            // `key` is an engine field on every type, never listed per type.
            match v.as_str() {
                None => out.push(err(
                    block,
                    "E_FIELD_KIND",
                    "field \"key\" must be Str".to_string(),
                    "write key as a str value".to_string(),
                )),
                Some(k) => {
                    if let Some(reason) = rdy_model::key::problem(k) {
                        out.push(err(
                            block,
                            "E_BAD_KEY",
                            format!("{k:?} is not a valid key: {reason}"),
                            "use letters, digits and - _ . such as FW-S1 or REQ-12; a key cannot look like a block id".to_string(),
                        ));
                    }
                }
            }
            continue;
        }
        if name == rdy_model::key::AUTO_FIELD {
            // `key_auto` is an engine field on docs and folders: their key came from the path
            if !tree::is_tree_type(&block.block_type) {
                out.push(err(
                    block,
                    "E_FIELD_UNKNOWN",
                    format!(
                        "type {} cannot carry {name:?}: only a doc's or folder's key is derived from its path",
                        block.block_type
                    ),
                    format!("remove {name}"),
                ));
            } else if v != &FieldValue::Bool(true) || !block.fields.contains_key("key") {
                out.push(err(
                    block,
                    "E_FIELD_KIND",
                    format!("field {name:?} must be true, next to a key"),
                    format!("remove {name}; rdy sets it when it derives a key from the path"),
                ));
            }
            continue;
        }
        if let Some(shape) = engine_fields::check(name, v) {
            // `reaffirmed` and `reflects_at` are engine metadata on every type (O22): only rdy's
            // own writes set them, so a bad shape is damaged stored metadata.
            if shape.is_err() {
                out.push(err(
                    block,
                    "E_FIELD_KIND",
                    format!("field {name:?} has an invalid engine value"),
                    "restore the block through rdy's write tools".to_string(),
                ));
            }
            continue;
        }
        if name == REFLECTS_FIELD {
            // `reflects` is an engine field on content types, never listed per type.
            if pack.class_of(&block.block_type) == TypeClass::Supporting {
                out.push(err(
                    block,
                    "E_FIELD_UNKNOWN",
                    format!(
                        "type {} cannot carry {REFLECTS_FIELD:?}: reflects justifies content; a decision links to code through Implements: trailers",
                        block.block_type
                    ),
                    "remove reflects; a decision links to code through Implements: trailers on the commit".to_string(),
                ));
                continue;
            }
            check_reflects(block, v, &mut out);
            continue;
        }
        if block.block_type == supporting::FINDING && name == supporting::EVIDENCE_FIELD {
            // rf1: a finding's evidence has its own closed shape (D-Z8P09)
            out.extend(supporting::validate_evidence(block));
            continue;
        }
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
    // A question's options are its card's labels: every entry is a string (FW-I1). Other list
    // fields hold what their type says and are not checked here.
    if block.block_type == decision::QUESTION
        && let Some(FieldValue::List(items)) = block.fields.get(QUESTION_OPTIONS)
        && items.iter().any(|v| v.as_str().is_none())
    {
        out.push(err(
            block,
            "E_FIELD_KIND",
            format!("field {QUESTION_OPTIONS:?} must be a list of strings"),
            format!("write {QUESTION_OPTIONS} as a list of strings"),
        ));
    }
    for (name, fd) in &def.fields {
        if fd.required && !projection && !block.fields.contains_key(name) {
            out.push(err(
                block,
                "E_FIELD_REQUIRED",
                format!("type {} requires field {name:?}", block.block_type),
                format!("add {name} ({})", kind_word(fd.kind)),
            ));
        }
    }
    match def.body {
        _ if projection => {}
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
    let top_ok = tree::is_tree_type(&block.block_type);
    // a publication profile sits directly under a doc, whatever a pack lists
    let profile = block.block_type == publication::PROFILE_TYPE;
    match (parent, &block.parent) {
        (Some(p), Some(_)) => {
            // docs and folders have paths: they sit only in folders, whatever a pack lists
            let allowed = if top_ok {
                p.block_type == tree::FOLDER
            } else if profile {
                p.block_type == tree::DOC
            } else {
                pack.block_types
                    .get(&p.block_type)
                    .map(|d| d.children.contains(&block.block_type))
                    .unwrap_or(false)
            };
            if !allowed {
                out.push(err(
                    block,
                    "E_CHILD_TYPE",
                    format!("a {} cannot contain a {}", p.block_type, block.block_type),
                    if top_ok {
                        "put it at the top level or inside a folder".into()
                    } else if profile {
                        PROFILE_PARENT_FIX.into()
                    } else {
                        "place it under a section (or another allowed parent type)".into()
                    },
                ));
            }
        }
        (None, Some(pid)) => out.push(err(
            block,
            "E_MISSING_PARENT",
            format!("{} has parent {pid}, which does not exist", block.id),
            format!(
                "move {} under an existing block, or recreate {pid}",
                block.id
            ),
        )),
        (_, None) if top_ok => {}
        (_, None) => out.push(err(
            block,
            "E_ORPHAN",
            format!("{} must have a parent", block.block_type),
            if profile {
                PROFILE_PARENT_FIX.into()
            } else {
                "set parent to the section that should contain it".into()
            },
        )),
    }
    if top_ok
        && let Some(name) = block.field_str("name")
        // a projection in the file or directory its id names (the neutral segment of a removed
        // doc or folder) is no name a person wrote
        && !(projection && name == block.id.as_str())
        && let Some(why) = tree::name_problem(name)
    {
        out.push(err(
            block,
            "E_BAD_NAME",
            format!("{name:?} is not a valid name: {why}"),
            format!(
                "use a name like {:?}",
                tree::default_name(Some(name), &block.id)
            ),
        ));
    }
    if block.block_type == tree::DOC
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
    reserved_at(block, &mut out);
    if !rdy_model::valid_order_key(&block.order) {
        out.push(err(
            block,
            "E_ORDER_KEY",
            format!(
                "order key {:?} is malformed or longer than {} characters",
                block.order,
                rdy_model::MAX_ORDER_KEY_LEN
            ),
            if rdy_model::decision::is_frozen(block) {
                "this decision is frozen and can't be re-keyed; move its movable siblings to another parent".into()
            } else {
                "rebalance the siblings: {\"op\":\"rebalance\",\"parent\":<the parent's id or path>}"
                    .into()
            },
        ));
    }
    if profile && !projection {
        publication::check_values(block, &mut out);
    }
    out
}

/// Refuses display text that starts with an unescaped `@` other than the `@key` mark: a leading
/// `@` is reserved for engine marks (FW-F2a), so a literal one is written `\@`. Checked in bodies
/// read in `Strict` mode and in every string field but the engine metadata fields.
fn reserved_at(block: &Block, out: &mut Vec<Issue>) {
    fn scan(block: &Block, place: &str, text: &str, mode: RefMode, out: &mut Vec<Issue>) {
        for span in ref_spans(text, mode) {
            let Some(d) = span.display.as_deref() else {
                continue;
            };
            if d.starts_with('@') && !span.at_escaped {
                out.push(err(
                    block,
                    "E_BAD_REF",
                    format!(
                        "{place} has a reference whose display text {d:?} starts with @, which is reserved"
                    ),
                    format!(
                        "write \\@ for a literal @ ({{{{{}|\\{d}}}}}), or @key alone to mark a key-typed reference",
                        span.token
                    ),
                ));
            }
        }
    }
    fn field(block: &Block, name: &str, v: &FieldValue, out: &mut Vec<Issue>) {
        match v {
            FieldValue::Str(s) => scan(block, &format!("field {name:?}"), s, RefMode::Strict, out),
            FieldValue::List(items) => items.iter().for_each(|i| field(block, name, i, out)),
            FieldValue::Map(m) => m.values().for_each(|i| field(block, name, i, out)),
            _ => {}
        }
    }
    if RefMode::for_body(&block.block_type) == RefMode::Strict {
        scan(block, "the body", &block.body, RefMode::Strict, out);
    }
    // the engine metadata fields hold no references (p-RNK9B)
    for (name, v) in &block.fields {
        if !engine_fields::is_engine_field(name) {
            field(block, name, v, out);
        }
    }
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
