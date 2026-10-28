use crate::glob::Glob;
use crate::pack::*;
use crate::publication;
use crate::supporting;
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

/// The message of `e`; for a duplicate-key error, its location moved from where the repeated
/// mapping starts (all serde_yaml_ng gives) to the line and column of the second use of the key.
fn with_duplicate_line(e: &serde_yaml_ng::Error, yaml: &str) -> String {
    let text = e.to_string();
    let (Some(at), Some(loc)) = (text.find("duplicate entry with key \""), e.location()) else {
        return text;
    };
    let rest = &text[at + "duplicate entry with key \"".len()..];
    let Some(key) = rest.split('"').next() else {
        return text;
    };
    let Some((line, col)) = second_use_of(yaml, key, loc.line(), loc.column()) else {
        return text;
    };
    text.replace(
        &format!("at line {} column {}", loc.line(), loc.column()),
        &format!("at line {line} column {col}"),
    )
}

/// The 1-based line and column of the second `key:` at the indentation of the block mapping that
/// starts at `line`, `col` (both 1-based); None for a flow mapping or when it is not found.
fn second_use_of(yaml: &str, key: &str, line: usize, col: usize) -> Option<(usize, usize)> {
    let indent = col.checked_sub(1)?;
    let mut seen = 0;
    for (i, text) in yaml.lines().enumerate().skip(line.checked_sub(1)?) {
        let body = if i + 1 == line {
            text.get(indent..)?
        } else {
            let trimmed = text.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let ind = text.len() - trimmed.len();
            if ind < indent {
                return None;
            }
            if ind > indent {
                continue;
            }
            trimmed
        };
        let is_key = [key.to_string(), format!("\"{key}\""), format!("'{key}'")]
            .iter()
            .any(|k| {
                body.strip_prefix(k.as_str())
                    .is_some_and(|r| r.trim_start().starts_with(':'))
            });
        if is_key {
            seen += 1;
            if seen == 2 {
                return Some((i + 1, indent + 1));
            }
        }
    }
    None
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
        // pkdup1: a repeated map key anywhere in the file is refused (the typed parse would keep
        // the last value silently). serde_yaml_ng's `Value` mapping refuses it and names the key.
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml)
            .map_err(|e| SchemaError::Yaml(with_duplicate_line(&e, yaml)))?;
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
        // D-2K8Q5, D-WMCFW: a semantic tension blocks only when a person accepts the advice
        if pack.tensions.contains_key(&TensionKind::Semantic) {
            return Err(SchemaError::Invalid(format!(
                "pack {} cannot set the severity of semantic tensions; a person accepts a reviewer's advice to block",
                pack.name
            )));
        }
        for ext in extensions() {
            check_extension(&pack, &ext)?;
        }
        if pack.block_types.contains_key(publication::PROFILE_TYPE) {
            return Err(SchemaError::Invalid(format!(
                "{} is an engine type and cannot be declared by a pack",
                publication::PROFILE_TYPE
            )));
        }
        // D-4WC6R: the engine adds the reflects link kind to every pack
        if pack.link_types.contains(supporting::REFLECTS_LINK) {
            return Err(SchemaError::Invalid(format!(
                "{} is an engine link kind and cannot be declared by a pack",
                supporting::REFLECTS_LINK
            )));
        }
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
                if tdef.class.is_none() {
                    tdef.class = parent_def.class;
                }
                let mut tracking = parent_def.tracking.clone();
                tracking.extend(tdef.tracking);
                tdef.tracking = tracking;
            }
            out.block_types.insert(tname, tdef);
        }
        if child.default_doc_type.is_some() {
            out.default_doc_type = child.default_doc_type;
        }
        out.doc_types.extend(child.doc_types);
        out.tensions.extend(child.tensions);
        if child.duplicate_text_min_words.is_some() {
            out.duplicate_text_min_words = child.duplicate_text_min_words;
        }
        out.areas.extend(child.areas);
        if let Some(c) = child.coverage {
            let o = out.coverage.get_or_insert_with(CoverageSettings::default);
            o.weights.extend(c.weights);
            if c.dismissed_weight.is_some() {
                o.dismissed_weight = c.dismissed_weight;
            }
            if c.drift_threshold.is_some() {
                o.drift_threshold = c.drift_threshold;
            }
        }
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
    // rf1: every children list that holds decision may hold a finding and a source too; done
    // before the doc's publication profile is placed, which closes the doc's list
    for def in pack.block_types.values_mut() {
        if def.children.iter().any(|c| c == d::DECISION) {
            for ty in [supporting::FINDING, supporting::SOURCE] {
                if !def.children.iter().any(|c| c == ty) {
                    def.children.push(ty.to_string());
                }
            }
        }
    }
    force(pack, tree::DOC, "d", Trait::Structural, doc_core, None);
    force(
        pack,
        tree::FOLDER,
        "fd",
        Trait::Structural,
        folder_core(),
        None,
    );
    for t in [tree::DOC, tree::FOLDER] {
        let def = pack.block_types.get_mut(t).expect("forced above");
        def.traits = BTreeSet::from([Trait::Structural]);
        def.body = BodyRule::Forbidden;
    }
    // a folder holds folders and docs, and the four types that may sit outside a doc
    // (D-P3YD5, D-X3136), whatever a pack lists
    pack.block_types
        .get_mut(tree::FOLDER)
        .expect("forced above")
        .children = [tree::FOLDER, tree::DOC]
        .into_iter()
        .chain(tree::OUTSIDE_DOC_TYPES)
        .map(str::to_string)
        .collect();
    // A publication profile (D-KDFZ3): one closed definition, a direct child of a doc only.
    pack.block_types.insert(
        publication::PROFILE_TYPE.to_string(),
        publication::type_def(),
    );
    let doc_children = &mut pack
        .block_types
        .get_mut(tree::DOC)
        .expect("forced above")
        .children;
    doc_children.retain(|c| c != publication::PROFILE_TYPE);
    doc_children.push(publication::PROFILE_TYPE.to_string());
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
                    "delegated",
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
        ("needs_content", f(FieldKind::Bool, false, &[])),
        // The authored words of the decision's card (FW-I1).
        ("title", f(FieldKind::Str, false, &[])),
        ("why", f(FieldKind::Str, false, &[])),
        (
            "recommend",
            f(FieldKind::Enum, false, &["confirm", "reject"]),
        ),
        ("recommend_reason", f(FieldKind::Str, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    // The authored words of the question's card (FW-I1): `options` holds two to four labels.
    let question_core: BTreeMap<String, FieldDef> = [
        ("prompt", f(FieldKind::Str, true, &[])),
        ("title", f(FieldKind::Str, false, &[])),
        ("why", f(FieldKind::Str, false, &[])),
        ("options", f(FieldKind::List, false, &[])),
        ("recommended", f(FieldKind::Str, false, &[])),
        ("recommend_reason", f(FieldKind::Str, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();

    force(
        pack,
        d::DECISION,
        "D",
        Trait::Normative,
        decision_core,
        Some(TypeClass::Supporting),
    );
    force(
        pack,
        d::QUESTION,
        "Q",
        Trait::Informative,
        question_core,
        Some(TypeClass::Supporting),
    );
    // rf1 (D-Z8P09): a finding and a source, informative supporting types with an optional body
    // and no children; `check_extension` has already refused any other pack definition.
    for (name, prefix, core) in [
        (
            supporting::FINDING,
            supporting::FINDING_PREFIX,
            supporting::finding_core(),
        ),
        (
            supporting::SOURCE,
            supporting::SOURCE_PREFIX,
            supporting::source_core(),
        ),
    ] {
        force(
            pack,
            name,
            prefix,
            Trait::Informative,
            core,
            Some(TypeClass::Supporting),
        );
        pack.block_types.get_mut(name).expect("forced above").body = BodyRule::Optional;
    }
    // A rationale explains a decision, so it may sit under one.
    if pack.block_types.contains_key("rationale") {
        let decision = pack.block_types.get_mut(d::DECISION).expect("forced above");
        if !decision.children.iter().any(|c| c == "rationale") {
            decision.children.push("rationale".to_string());
        }
    }
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
        supporting::REFLECTS_LINK,
    ] {
        pack.link_types.insert(l.to_string());
    }
}

fn folder_core() -> BTreeMap<String, FieldDef> {
    [
        ("name", f(FieldKind::Str, true, &[])),
        ("title", f(FieldKind::Str, false, &[])),
        ("description", f(FieldKind::Str, false, &[])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// An engine type a pack may declare only to add optional fields.
struct Extension {
    ty: &'static str,
    prefix: &'static str,
    trait_ok: fn(Trait) -> bool,
    core: BTreeMap<String, FieldDef>,
    /// Whether a pack may repeat a core field exactly; a folder's may not be named at all.
    repeat_core: bool,
}

fn extensions() -> [Extension; 3] {
    [
        Extension {
            ty: tree::FOLDER,
            prefix: "fd",
            trait_ok: |t| t == Trait::Structural,
            core: folder_core(),
            repeat_core: false,
        },
        Extension {
            ty: supporting::FINDING,
            prefix: supporting::FINDING_PREFIX,
            trait_ok: supporting::trait_allowed,
            core: supporting::finding_core(),
            repeat_core: true,
        },
        Extension {
            ty: supporting::SOURCE,
            prefix: supporting::SOURCE_PREFIX,
            trait_ok: supporting::trait_allowed,
            core: supporting::source_core(),
            repeat_core: true,
        },
    ]
}

/// A pack may add optional fields to `folder`, `finding` and `source`, nothing else: checked in
/// the order prefix, children, body, traits, then fields in key order. Its class is forced, so it
/// is not checked.
fn check_extension(pack: &Pack, ext: &Extension) -> Result<(), SchemaError> {
    let Some(def) = pack.block_types.get(ext.ty) else {
        return Ok(());
    };
    let refuse = |what: &str| {
        Err(SchemaError::Invalid(format!(
            "pack {} cannot redefine {} ({what}); it may only add optional fields",
            pack.name, ext.ty
        )))
    };
    if def.prefix != ext.prefix {
        return refuse("prefix");
    }
    if !def.children.is_empty() {
        return refuse("children");
    }
    if def.body == BodyRule::Required {
        return refuse("body");
    }
    if !def.traits.iter().all(|t| (ext.trait_ok)(*t)) {
        return refuse("traits");
    }
    for (name, fd) in &def.fields {
        let bad = match ext.core.get(name) {
            Some(core) => !ext.repeat_core || core != fd,
            None => fd.required,
        };
        if bad {
            return refuse(&format!("field {name}"));
        }
    }
    Ok(())
}

fn force(
    pack: &mut Pack,
    name: &str,
    prefix: &str,
    tr: Trait,
    core: BTreeMap<String, FieldDef>,
    class: Option<TypeClass>,
) {
    let def = pack
        .block_types
        .entry(name.to_string())
        .or_insert_with(|| BlockTypeDef {
            prefix: prefix.to_string(),
            traits: BTreeSet::new(),
            fields: BTreeMap::new(),
            children: vec![],
            body: BodyRule::Optional,
            class: None,
            tracking: BTreeSet::new(),
        });
    def.prefix = prefix.to_string();
    def.class = class;
    def.traits.insert(tr);
    def.fields.extend(core);
}

/// The block-type prefix that proposal refs (`P-<id>@<n>`) use; no pack type may take it.
pub const PROPOSAL_PREFIX: &str = "P";

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
        if t.prefix == PROPOSAL_PREFIX {
            return Err(SchemaError::Invalid(format!(
                "type {name:?} has prefix \"P\", which is reserved for proposal refs (P-<id>@<n>)"
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
        if let Some(f) = rdy_model::engine_fields::ENGINE_FIELDS
            .iter()
            .find(|f| t.fields.contains_key(**f))
        {
            return Err(SchemaError::Invalid(format!(
                "type {name}: field {f} is reserved for the engine"
            )));
        }
        check_tracking(name, t)?;
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
    check_coverage(pack)?;
    Ok(())
}

/// The ranges of the pack's `coverage:` numbers, and at least one dimension with a weight.
fn check_coverage(pack: &Pack) -> Result<(), SchemaError> {
    let Some(c) = &pack.coverage else {
        return Ok(());
    };
    if let Some(n) = c.dismissed_weight
        && n > 100
    {
        return Err(SchemaError::Invalid(format!(
            "coverage.dismissed_weight is {n}; it must be 0 to 100"
        )));
    }
    if let Some(n) = c.drift_threshold
        && !(1..=100).contains(&n)
    {
        return Err(SchemaError::Invalid(format!(
            "coverage.drift_threshold is {n}; it must be 1 to 100"
        )));
    }
    if CoverageDimension::ALL
        .iter()
        .all(|d| pack.coverage_weight(*d) == 0)
    {
        return Err(SchemaError::Invalid(
            "coverage.weights are all 0; at least one dimension needs a weight".to_string(),
        ));
    }
    Ok(())
}

/// trk1: each tracking field of `t` is a field of the type, and neither part of a decision's
/// ruling nor derived or reserved by the engine. An engine metadata field (`reaffirmed`,
/// `reflects_at`) gets the same diagnostic as a pack field of that name.
fn check_tracking(name: &str, t: &BlockTypeDef) -> Result<(), SchemaError> {
    for f in &t.tracking {
        if rdy_model::engine_fields::is_engine_field(f) {
            return Err(SchemaError::Invalid(format!(
                "type {name}: field {f} is reserved for the engine"
            )));
        }
        let why = if name == d::DECISION && d::RULING_FIELDS.contains(&f.as_str()) {
            "is part of the ruling"
        } else if rdy_model::block::DERIVED_FIELDS.contains(&f.as_str())
            || f == "key"
            || f == crate::validate::REFLECTS_FIELD
        {
            "is derived or reserved by the engine"
        } else if !t.fields.contains_key(f) {
            "is not a field of the type"
        } else {
            continue;
        };
        return Err(SchemaError::Invalid(format!(
            "type {name}: tracking field {f} {why}"
        )));
    }
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
