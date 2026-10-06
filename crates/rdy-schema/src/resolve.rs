use crate::glob::Glob;
use crate::pack::*;
use rdy_model::decision as d;
use rdy_model::tree;
use std::collections::{BTreeMap, BTreeSet};

pub const STANDARD_YAML: &str = include_str!("../packs/rdy-standard.yaml");

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SchemaError {
    #[error("pack YAML is invalid: {0}")]
    Yaml(String),
    #[error("unknown pack {0:?}")]
    UnknownPack(String),
    #[error("pack inheritance cycle through {0:?}")]
    Cycle(String),
    #[error("invalid pack: {0}")]
    Invalid(String),
    #[error("pack {name:?} is defined twice: in {first} and in {second}")]
    Duplicate {
        name: String,
        first: String,
        second: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct PackSet {
    raw: BTreeMap<String, Pack>,
    /// Where each pack came from (a file path, or the built-in), for error messages.
    sources: BTreeMap<String, String>,
}

impl PackSet {
    pub fn with_builtin() -> PackSet {
        let mut s = PackSet::default();
        s.add_yaml_from(STANDARD_YAML, "the built-in pack")
            .expect("built-in pack parses");
        s
    }

    pub fn add_yaml(&mut self, yaml: &str) -> Result<String, SchemaError> {
        self.add_yaml_from(yaml, "an inline pack")
    }

    /// Adds a pack read from `source`. A name already taken is refused (pack1): a pack is
    /// replaced only by a differently named pack, which may extend it (spec §4.5).
    pub fn add_yaml_from(&mut self, yaml: &str, source: &str) -> Result<String, SchemaError> {
        let pack: Pack =
            serde_yaml_ng::from_str(yaml).map_err(|e| SchemaError::Yaml(e.to_string()))?;
        let name = pack.name.clone();
        if let Some(first) = self.sources.get(&name) {
            return Err(SchemaError::Duplicate {
                name,
                first: first.clone(),
                second: source.to_string(),
            });
        }
        self.sources.insert(name.clone(), source.to_string());
        self.raw.insert(name.clone(), pack);
        Ok(name)
    }

    pub fn resolve(&self, name: &str) -> Result<Pack, SchemaError> {
        let mut pack = self.merged(name, &mut Vec::new())?;
        check_folder_extension(&pack)?;
        inject_engine_types(&mut pack);
        check(&pack)?;
        Ok(pack)
    }

    /// `name` followed by every pack it extends, nearest first.
    pub fn lineage(&self, name: &str) -> Result<Vec<String>, SchemaError> {
        let mut out: Vec<String> = vec![];
        let mut cur = Some(name.to_string());
        while let Some(n) = cur {
            if out.contains(&n) {
                return Err(SchemaError::Cycle(n));
            }
            cur = self
                .raw
                .get(&n)
                .ok_or_else(|| SchemaError::UnknownPack(n.clone()))?
                .extends
                .clone();
            out.push(n);
        }
        Ok(out)
    }

    fn merged(&self, name: &str, seen: &mut Vec<String>) -> Result<Pack, SchemaError> {
        if seen.iter().any(|s| s == name) {
            return Err(SchemaError::Cycle(name.to_string()));
        }
        seen.push(name.to_string());
        let child = self
            .raw
            .get(name)
            .ok_or_else(|| SchemaError::UnknownPack(name.to_string()))?
            .clone();
        let Some(parent_name) = child.extends.clone() else {
            return Ok(child);
        };
        let mut out = self.merged(&parent_name, seen)?;
        out.name = child.name;
        out.version = child.version;
        out.extends = Some(parent_name);
        out.link_types.extend(child.link_types);
        for (tname, mut tdef) in child.block_types {
            if let Some(parent_def) = out.block_types.get(&tname) {
                let mut fields = parent_def.fields.clone();
                fields.extend(tdef.fields);
                tdef.fields = fields;
            }
            out.block_types.insert(tname, tdef);
        }
        if child.default_doc_type.is_some() {
            out.default_doc_type = child.default_doc_type;
        }
        out.doc_types.extend(child.doc_types);
        out.tensions.extend(child.tensions);
        out.areas.extend(child.areas);
        Ok(out)
    }
}

fn f(kind: FieldKind, required: bool, values: &[&str]) -> FieldDef {
    FieldDef {
        kind,
        required,
        values: values.iter().map(|s| s.to_string()).collect(),
    }
}

fn inject_engine_types(pack: &mut Pack) {
    let doc_core: BTreeMap<String, FieldDef> = [
        ("name", f(FieldKind::Str, true, &[])),
        ("title", f(FieldKind::Str, true, &[])),
        ("doc_type", f(FieldKind::Str, true, &[])),
        (
            "lifecycle",
            f(FieldKind::Enum, false, &["draft", "stable", "deprecated"]),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let folder_core: BTreeMap<String, FieldDef> = [
        ("name", f(FieldKind::Str, true, &[])),
        ("title", f(FieldKind::Str, false, &[])),
        ("description", f(FieldKind::Str, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    force(pack, tree::DOC, "d", Trait::Structural, doc_core);
    force(pack, tree::FOLDER, "fd", Trait::Structural, folder_core);
    for t in [tree::DOC, tree::FOLDER] {
        let def = pack.block_types.get_mut(t).expect("forced above");
        def.traits = BTreeSet::from([Trait::Structural]);
        def.body = BodyRule::Forbidden;
    }
    pack.block_types
        .get_mut(tree::FOLDER)
        .expect("forced above")
        .children = vec![tree::FOLDER.to_string(), tree::DOC.to_string()];
    let decision_core: BTreeMap<String, FieldDef> = [
        ("statement", f(FieldKind::Str, true, &[])),
        (
            "status",
            f(FieldKind::Enum, true, &["proposed", "accepted", "rejected"]),
        ),
        ("options", f(FieldKind::List, false, &[])),
        (
            "authority_basis",
            f(
                FieldKind::Enum,
                false,
                &[
                    "direct_ruling",
                    "recorded_ruling",
                    "precedent",
                    "elimination",
                    "none",
                ],
            ),
        ),
        ("attested_by", f(FieldKind::Str, false, &[])),
        (
            "reversibility",
            f(FieldKind::Enum, false, &["one_way", "two_way"]),
        ),
        ("decided_at", f(FieldKind::Str, false, &[])),
        ("recorded_at", f(FieldKind::Str, false, &[])),
        ("effective_from", f(FieldKind::Str, false, &[])),
        ("review_by", f(FieldKind::Str, false, &[])),
        ("kind", f(FieldKind::Str, false, &[])),
        ("placeholders", f(FieldKind::List, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let question_core: BTreeMap<String, FieldDef> =
        [("prompt".to_string(), f(FieldKind::Str, true, &[]))]
            .into_iter()
            .collect();

    force(pack, d::DECISION, "D", Trait::Normative, decision_core);
    force(pack, d::QUESTION, "Q", Trait::Informative, question_core);
    for l in [
        d::SUPERSEDES,
        d::REFINES,
        d::DEPENDS_ON,
        d::CONDITIONAL_ON,
        d::REVOKES,
        d::ANSWERS,
        d::DECIDED_BY,
        d::IMPLEMENTS,
        d::DESCRIBES,
        d::AFFECTS,
    ] {
        pack.link_types.insert(l.to_string());
    }
}

/// A pack may add optional fields to `folder`, nothing else.
fn check_folder_extension(pack: &Pack) -> Result<(), SchemaError> {
    let Some(def) = pack.block_types.get(tree::FOLDER) else {
        return Ok(());
    };
    let refuse = |what: &str| {
        Err(SchemaError::Invalid(format!(
            "pack {} cannot redefine folder ({what}); it may only add optional fields",
            pack.name
        )))
    };
    if def.prefix != "fd" {
        return refuse("prefix");
    }
    if !def.children.is_empty() {
        return refuse("children");
    }
    if def.body == BodyRule::Required {
        return refuse("body");
    }
    if def.traits.iter().any(|t| *t != Trait::Structural) {
        return refuse("traits");
    }
    for (name, fd) in &def.fields {
        if fd.required || ["name", "title", "description"].contains(&name.as_str()) {
            return refuse(&format!("field {name}"));
        }
    }
    Ok(())
}

fn force(pack: &mut Pack, name: &str, prefix: &str, tr: Trait, core: BTreeMap<String, FieldDef>) {
    let def = pack
        .block_types
        .entry(name.to_string())
        .or_insert_with(|| BlockTypeDef {
            prefix: prefix.to_string(),
            traits: BTreeSet::new(),
            fields: BTreeMap::new(),
            children: vec![],
            body: BodyRule::Optional,
        });
    def.prefix = prefix.to_string();
    def.traits.insert(tr);
    def.fields.extend(core);
}

fn check(pack: &Pack) -> Result<(), SchemaError> {
    let mut prefixes = BTreeMap::new();
    for (name, t) in &pack.block_types {
        if !(1..=3).contains(&t.prefix.len()) || !t.prefix.bytes().all(|b| b.is_ascii_alphabetic())
        {
            return Err(SchemaError::Invalid(format!(
                "type {name:?} has invalid prefix {:?}",
                t.prefix
            )));
        }
        if let Some(other) = prefixes.insert(t.prefix.clone(), name.clone()) {
            return Err(SchemaError::Invalid(format!(
                "prefix {:?} used by both {other:?} and {name:?}",
                t.prefix
            )));
        }
        for c in &t.children {
            if !pack.block_types.contains_key(c) {
                return Err(SchemaError::Invalid(format!(
                    "type {name:?} allows unknown child type {c:?}"
                )));
            }
        }
        if t.fields.contains_key("key") {
            return Err(SchemaError::Invalid(format!(
                "type {name}: field key is reserved for the engine on every type"
            )));
        }
        if t.fields.contains_key(crate::validate::REFLECTS_FIELD) {
            return Err(SchemaError::Invalid(format!(
                "type {name}: field reflects is reserved for the engine on content types"
            )));
        }
        for (fname, fd) in &t.fields {
            if fd.kind == FieldKind::Enum && fd.values.is_empty() {
                return Err(SchemaError::Invalid(format!(
                    "enum field {name}.{fname} has no values"
                )));
            }
        }
        if t.traits.contains(&Trait::Presentational)
            && (t.traits.contains(&Trait::Normative) || t.traits.contains(&Trait::Defines))
        {
            return Err(SchemaError::Invalid(format!(
                "type {name}: presentational excludes normative and defines"
            )));
        }
    }
    if let Some(d) = &pack.default_doc_type
        && !pack.doc_types.contains_key(d)
    {
        return Err(SchemaError::Invalid(format!(
            "default_doc_type {d:?} is not one of the pack's doc_types"
        )));
    }
    check_areas(pack)?;
    Ok(())
}

/// Area names are `^[a-z0-9][a-z0-9_-]{0,63}$` and every path pattern compiles.
fn check_areas(pack: &Pack) -> Result<(), SchemaError> {
    for (name, area) in &pack.areas {
        let b = name.as_bytes();
        let valid = (1..=64).contains(&b.len())
            && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
            && b.iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-');
        if !valid {
            return Err(SchemaError::Invalid(format!(
                "area {name:?} has an invalid name: use a-z, 0-9, '_' and '-', 1 to 64 characters, starting with a letter or digit"
            )));
        }
        for pattern in &area.paths {
            if let Err(e) = Glob::compile(pattern) {
                return Err(SchemaError::Invalid(format!(
                    "area {name:?} path {pattern:?}: {e}"
                )));
            }
        }
    }
    Ok(())
}
