use crate::glob::Glob;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trait {
    Normative,
    Informative,
    Defines,
    Describes,
    Asset,
    Structural,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Str,
    Int,
    Bool,
    List,
    Map,
    Enum,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyRule {
    #[default]
    Optional,
    Required,
    Forbidden,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockTypeDef {
    pub prefix: String,
    #[serde(default)]
    pub traits: BTreeSet<Trait>,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldDef>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub body: BodyRule,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkeletonSection {
    pub title: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocTypeDef {
    #[serde(default)]
    pub skeleton: Vec<SkeletonSection>,
}

/// A named part of the documentation tree, declared by path globs.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaDef {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pack {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub link_types: BTreeSet<String>,
    #[serde(default)]
    pub block_types: BTreeMap<String, BlockTypeDef>,
    #[serde(default)]
    pub doc_types: BTreeMap<String, DocTypeDef>,
    /// The doc_type `rdy new doc` uses when none is given.
    #[serde(default)]
    pub default_doc_type: Option<String>,
    /// Severity overrides per tension kind: only what the pack wrote.
    #[serde(default)]
    pub tensions: BTreeMap<TensionKind, TensionSeverity>,
    /// Named areas of the tree, by area name.
    #[serde(default)]
    pub areas: BTreeMap<String, AreaDef>,
}

/// The kinds of tension the engine can raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TensionKind {
    EditCollision,
    Fork,
    PremiseShift,
    AreaOverlap,
    DuplicateDefinition,
    UnjustifiedChange,
    DeprecatedReference,
}

impl TensionKind {
    pub const ALL: [TensionKind; 7] = [
        TensionKind::EditCollision,
        TensionKind::Fork,
        TensionKind::PremiseShift,
        TensionKind::AreaOverlap,
        TensionKind::DuplicateDefinition,
        TensionKind::UnjustifiedChange,
        TensionKind::DeprecatedReference,
    ];

    /// The wire name.
    pub fn name(&self) -> &'static str {
        match self {
            TensionKind::EditCollision => "edit_collision",
            TensionKind::Fork => "fork",
            TensionKind::PremiseShift => "premise_shift",
            TensionKind::AreaOverlap => "area_overlap",
            TensionKind::DuplicateDefinition => "duplicate_definition",
            TensionKind::UnjustifiedChange => "unjustified_change",
            TensionKind::DeprecatedReference => "deprecated_reference",
        }
    }

    pub fn default_severity(&self) -> TensionSeverity {
        match self {
            TensionKind::AreaOverlap
            | TensionKind::UnjustifiedChange
            | TensionKind::DeprecatedReference => TensionSeverity::Advisory,
            _ => TensionSeverity::Blocking,
        }
    }
}

impl fmt::Display for TensionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A string that is not the wire name of a [`TensionKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownTensionKind(pub String);

impl fmt::Display for UnknownTensionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = TensionKind::ALL.iter().map(TensionKind::name).collect();
        write!(
            f,
            "unknown tension kind {:?}; one of: {}",
            self.0,
            names.join(", ")
        )
    }
}

impl std::error::Error for UnknownTensionKind {}

impl FromStr for TensionKind {
    type Err = UnknownTensionKind;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TensionKind::ALL
            .into_iter()
            .find(|k| k.name() == s)
            .ok_or_else(|| UnknownTensionKind(s.to_string()))
    }
}

/// Whether a tension of some kind blocks a release or only advises.
/// Declaration order: blocking sorts first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TensionSeverity {
    Blocking,
    Advisory,
}

impl fmt::Display for TensionSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TensionSeverity::Blocking => "blocking",
            TensionSeverity::Advisory => "advisory",
        })
    }
}

impl Pack {
    pub fn type_for_prefix(&self, prefix: &str) -> Option<&str> {
        self.block_types
            .iter()
            .find(|(_, t)| t.prefix == prefix)
            .map(|(n, _)| n.as_str())
    }

    /// The names, ascending, of every area with a path pattern that covers `path`.
    /// Patterns are compiled on each call; one that does not compile (impossible
    /// after `resolve`) covers nothing.
    pub fn areas_for_path(&self, path: &str) -> Vec<&str> {
        self.areas
            .iter()
            .filter(|(_, a)| {
                a.paths
                    .iter()
                    .filter_map(|p| Glob::compile(p).ok())
                    .any(|g| g.covers(path))
            })
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// The compiled patterns of one area; empty for an unknown area.
    pub fn area_globs(&self, name: &str) -> Vec<Glob> {
        self.areas
            .get(name)
            .map(|a| {
                a.paths
                    .iter()
                    .filter_map(|p| Glob::compile(p).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The severity of `kind` under this pack and whether the pack set it
    /// (`true`) or it is the default (`false`). `duplicate_definition`
    /// defaults to blocking; M2d2 downgrades it to advisory in draft content
    /// only when the second value is `false`.
    pub fn tension_severity(&self, kind: TensionKind) -> (TensionSeverity, bool) {
        match self.tensions.get(&kind) {
            Some(s) => (*s, true),
            None => (kind.default_severity(), false),
        }
    }

    /// Every kind in `TensionKind::ALL` order with its severity and whether
    /// the pack overrides it.
    pub fn tension_table(&self) -> Vec<(TensionKind, TensionSeverity, bool)> {
        TensionKind::ALL
            .into_iter()
            .map(|k| {
                let (s, o) = self.tension_severity(k);
                (k, s, o)
            })
            .collect()
    }
}
