use crate::pack::*;
use rdy_model::decision as d;
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
}

#[derive(Clone, Debug, Default)]
pub struct PackSet {
    raw: BTreeMap<String, Pack>,
}

impl PackSet {
    pub fn with_builtin() -> PackSet {
        let mut s = PackSet::default();
        s.add_yaml(STANDARD_YAML).expect("built-in pack parses");
        s
    }

    pub fn add_yaml(&mut self, yaml: &str) -> Result<String, SchemaError> {
        let pack: Pack = serde_yaml_ng::from_str(yaml).map_err(|e| SchemaError::Yaml(e.to_string()))?;
        let name = pack.name.clone();
        self.raw.insert(name.clone(), pack);
        Ok(name)
    }

    pub fn resolve(&self, name: &str) -> Result<Pack, SchemaError> {
        let mut pack = self.merged(name, &mut Vec::new())?;
        inject_engine_types(&mut pack);
        check(&pack)?;
        Ok(pack)
    }

    fn merged(&self, name: &str, seen: &mut Vec<String>) -> Result<Pack, SchemaError> {
        if seen.iter().any(|s| s == name) {
            return Err(SchemaError::Cycle(name.to_string()));
        }
        seen.push(name.to_string());
        let child = self.raw.get(name).ok_or_else(|| SchemaError::UnknownPack(name.to_string()))?.clone();
        let Some(parent_name) = child.extends.clone() else { return Ok(child) };
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
        out.doc_types.extend(child.doc_types);
        Ok(out)
    }
}

fn f(kind: FieldKind, required: bool, values: &[&str]) -> FieldDef {
    FieldDef { kind, required, values: values.iter().map(|s| s.to_string()).collect() }
}

fn inject_engine_types(pack: &mut Pack) {
    let decision_core: BTreeMap<String, FieldDef> = [
        ("statement", f(FieldKind::Str, true, &[])),
        ("status", f(FieldKind::Enum, true, &["proposed", "accepted", "rejected"])),
        ("options", f(FieldKind::List, false, &[])),
        ("authority_basis", f(FieldKind::Enum, false, &["direct_ruling", "recorded_ruling", "precedent", "elimination", "none"])),
        ("attested_by", f(FieldKind::Str, false, &[])),
        ("reversibility", f(FieldKind::Enum, false, &["one_way", "two_way"])),
        ("decided_at", f(FieldKind::Str, false, &[])),
        ("recorded_at", f(FieldKind::Str, false, &[])),
        ("review_by", f(FieldKind::Str, false, &[])),
        ("kind", f(FieldKind::Str, false, &[])),
        ("placeholders", f(FieldKind::List, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let question_core: BTreeMap<String, FieldDef> =
        [("prompt".to_string(), f(FieldKind::Str, true, &[]))].into_iter().collect();

    force(pack, d::DECISION, "D", Trait::Normative, decision_core);
    force(pack, d::QUESTION, "Q", Trait::Informative, question_core);
    for l in [d::SUPERSEDES, d::REFINES, d::DEPENDS_ON, d::CONDITIONAL_ON, d::REVOKES, d::ANSWERS, d::DECIDED_BY, d::IMPLEMENTS, d::DESCRIBES, d::AFFECTS] {
        pack.link_types.insert(l.to_string());
    }
}

fn force(pack: &mut Pack, name: &str, prefix: &str, tr: Trait, core: BTreeMap<String, FieldDef>) {
    let def = pack.block_types.entry(name.to_string()).or_insert_with(|| BlockTypeDef {
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
        if !(1..=3).contains(&t.prefix.len()) || !t.prefix.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(SchemaError::Invalid(format!("type {name:?} has invalid prefix {:?}", t.prefix)));
        }
        if let Some(other) = prefixes.insert(t.prefix.clone(), name.clone()) {
            return Err(SchemaError::Invalid(format!("prefix {:?} used by both {other:?} and {name:?}", t.prefix)));
        }
        for c in &t.children {
            if !pack.block_types.contains_key(c) {
                return Err(SchemaError::Invalid(format!("type {name:?} allows unknown child type {c:?}")));
            }
        }
        for (fname, fd) in &t.fields {
            if fd.kind == FieldKind::Enum && fd.values.is_empty() {
                return Err(SchemaError::Invalid(format!("enum field {name}.{fname} has no values")));
            }
        }
    }
    Ok(())
}
