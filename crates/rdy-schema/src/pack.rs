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
    /// Decoration: exempt from the briefing and justification rules, never a suspect source,
    /// rendered without a marker or a "Why ?" pill. Excludes `normative` and `defines`.
    Presentational,
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

/// What a block type is for. A doc asset is content a person reads and edits in a working-copy
/// file; a supporting type (decisions, questions, rationale) lives in the store and is reached
/// by links and tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeClass {
    #[default]
    DocAsset,
    Supporting,
}

impl TypeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeClass::DocAsset => "doc_asset",
            TypeClass::Supporting => "supporting",
        }
    }
}

/// Link kinds that live only in the store: a working-copy file never shows them.
pub const STORE_ONLY_LINK_KINDS: &[&str] = &["decided_by", "implements"];

/// A link of any other kind whose target has one of these types is store-only too.
pub const STORE_ONLY_TARGET_TYPES: &[&str] = &["decision", "question"];

/// Fields rdy sets and keeps in the store: a working-copy file never shows them, and a file that
/// sets one is refused. Must equal `rdy_model::key::AUTO_FIELD` once AE2 merges.
pub const STORE_ONLY_FIELDS: &[&str] = &["key_auto"];

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
    /// `None` until the pack resolves: a child keeps its parent's class; the default is a doc asset.
    #[serde(default)]
    pub class: Option<TypeClass>,
    /// trk1 (D-DZC1J): fields that stay writable on an accepted or rejected decision and are
    /// left out of the content hash. A child pack's set adds to its parent's.
    #[serde(default)]
    pub tracking: BTreeSet<String>,
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
    /// Whether docs of this type are listed in the site's navigation (the sidebar, folder index
    /// pages, the home page's contents, the previous/next pager and the search index). A doc of
    /// a type with `nav: false` still has its page, which links reach.
    #[serde(default = "nav_default")]
    pub nav: bool,
}

fn nav_default() -> bool {
    true
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
    /// Normative text duplicates need at least this many words (default 3).
    #[serde(default)]
    pub duplicate_text_min_words: Option<usize>,
    /// Named areas of the tree, by area name.
    #[serde(default)]
    pub areas: BTreeMap<String, AreaDef>,
    /// How the pack weights the coverage dimensions and its two coverage numbers.
    #[serde(default)]
    pub coverage: Option<CoverageSettings>,
}

/// The six dimensions a coverage score measures, in report order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageDimension {
    Grounded,
    Consistent,
    Current,
    Connected,
    Complete,
    Governed,
}

impl CoverageDimension {
    pub const ALL: [CoverageDimension; 6] = [
        CoverageDimension::Grounded,
        CoverageDimension::Consistent,
        CoverageDimension::Current,
        CoverageDimension::Connected,
        CoverageDimension::Complete,
        CoverageDimension::Governed,
    ];

    /// The wire name: the same word the pack file uses.
    pub fn name(&self) -> &'static str {
        match self {
            CoverageDimension::Grounded => "grounded",
            CoverageDimension::Consistent => "consistent",
            CoverageDimension::Current => "current",
            CoverageDimension::Connected => "connected",
            CoverageDimension::Complete => "complete",
            CoverageDimension::Governed => "governed",
        }
    }
}

/// The pack's `coverage:` section. Every part is optional; a missing part takes its default.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageSettings {
    /// A whole-number weight per dimension. A missing dimension weighs 1; 0 leaves it out.
    #[serde(default)]
    pub weights: BTreeMap<CoverageDimension, u32>,
    /// Percent of one a dismissed tension costs, 0 to 100 (default 50).
    #[serde(default)]
    pub dismissed_weight: Option<u32>,
    /// Percent of shared text a restatement must match to count as drift, 1 to 100 (default 60).
    #[serde(default)]
    pub drift_threshold: Option<u32>,
}

/// The percent of one a dismissed tension costs, when the pack does not set `dismissed_weight`.
pub const DEFAULT_DISMISSED_WEIGHT: u32 = 50;
/// The drift match percent, when the pack does not set `drift_threshold`.
pub const DEFAULT_DRIFT_THRESHOLD: u32 = 60;

/// The fewest words a normative text needs before two equal texts are a duplicate definition,
/// when the pack does not set `duplicate_text_min_words`.
pub const DUPLICATE_TEXT_MIN_WORDS: usize = 3;

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
    ValueDisagreement,
}

impl TensionKind {
    pub const ALL: [TensionKind; 8] = [
        TensionKind::EditCollision,
        TensionKind::Fork,
        TensionKind::PremiseShift,
        TensionKind::AreaOverlap,
        TensionKind::DuplicateDefinition,
        TensionKind::UnjustifiedChange,
        TensionKind::DeprecatedReference,
        TensionKind::ValueDisagreement,
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
            TensionKind::ValueDisagreement => "value_disagreement",
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

    /// Whether a tension of this kind is advisory in draft content when the pack does not set
    /// its severity: blocking in stable content, advisory in draft. True for
    /// `duplicate_definition` and `value_disagreement`.
    pub fn softens_in_draft(&self) -> bool {
        matches!(
            self,
            TensionKind::DuplicateDefinition | TensionKind::ValueDisagreement
        )
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
    /// The class of a block type; a doc asset when the type is unknown or sets none.
    pub fn class_of(&self, block_type: &str) -> TypeClass {
        self.block_types
            .get(block_type)
            .and_then(|t| t.class)
            .unwrap_or_default()
    }

    /// rtyp1: whether a block of type `from` may become a `to` in place, keeping its id. Both
    /// are known content types of the doc-asset class with the same presentational trait; docs,
    /// folders and supporting types keep their type.
    pub fn may_retype(&self, from: &str, to: &str) -> bool {
        let content = |t: &str| {
            self.block_types.contains_key(t)
                && !rdy_model::tree::is_tree_type(t)
                && self.class_of(t) == TypeClass::DocAsset
        };
        content(from) && content(to) && self.is_presentational(from) == self.is_presentational(to)
    }

    /// The class of the type whose `prefix` is `prefix`; `None` when no type has it.
    pub fn class_of_prefix(&self, prefix: &str) -> Option<TypeClass> {
        self.block_types
            .values()
            .find(|t| t.prefix == prefix)
            .map(|t| t.class.unwrap_or_default())
    }

    /// trk1: the tracking fields of every block type that has any.
    pub fn tracking(&self) -> rdy_model::Tracking {
        rdy_model::Tracking::new(
            self.block_types
                .iter()
                .map(|(name, t)| (name.clone(), t.tracking.clone()))
                .collect(),
        )
    }

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

    /// arn1: the areas' patterns compiled once, for callers that ask about many paths. Answers
    /// as [`Pack::areas_for_path`] does.
    pub fn area_matcher(&self) -> AreaMatcher {
        AreaMatcher {
            areas: self
                .areas
                .keys()
                .map(|name| (name.clone(), self.area_globs(name)))
                .collect(),
        }
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

    /// The fewest words a normative text needs before two equal texts are a duplicate
    /// definition: the pack's `duplicate_text_min_words`, else [`DUPLICATE_TEXT_MIN_WORDS`].
    pub fn duplicate_text_min_words(&self) -> usize {
        self.duplicate_text_min_words
            .unwrap_or(DUPLICATE_TEXT_MIN_WORDS)
    }

    /// The weight of coverage dimension `d`: the pack's, else 1. A weight of 0 leaves the
    /// dimension out of every score.
    pub fn coverage_weight(&self, d: CoverageDimension) -> u32 {
        self.coverage
            .as_ref()
            .and_then(|c| c.weights.get(&d).copied())
            .unwrap_or(1)
    }

    /// The percent of one a dismissed tension costs: the pack's, else
    /// [`DEFAULT_DISMISSED_WEIGHT`].
    pub fn dismissed_weight(&self) -> u32 {
        self.coverage
            .as_ref()
            .and_then(|c| c.dismissed_weight)
            .unwrap_or(DEFAULT_DISMISSED_WEIGHT)
    }

    /// The drift match percent: the pack's, else [`DEFAULT_DRIFT_THRESHOLD`].
    pub fn drift_threshold(&self) -> u32 {
        self.coverage
            .as_ref()
            .and_then(|c| c.drift_threshold)
            .unwrap_or(DEFAULT_DRIFT_THRESHOLD)
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

    /// Whether `block_type` is declared with the `asset` trait (an unknown type is not).
    pub fn is_asset(&self, block_type: &str) -> bool {
        self.block_types
            .get(block_type)
            .is_some_and(|t| t.traits.contains(&Trait::Asset))
    }

    /// Whether `block_type` is declared with the `presentational` trait (an unknown type is not).
    pub fn is_presentational(&self, block_type: &str) -> bool {
        self.block_types
            .get(block_type)
            .is_some_and(|t| t.traits.contains(&Trait::Presentational))
    }
}

/// arn1: a pack's areas with their path patterns compiled once, in area-name order. A pattern that
/// does not compile (impossible after `resolve`) covers nothing, as in [`Pack::areas_for_path`].
#[derive(Clone, Debug, Default)]
pub struct AreaMatcher {
    areas: Vec<(String, Vec<Glob>)>,
}

impl AreaMatcher {
    /// The names, ascending, of every area with a path pattern that covers `path`.
    pub fn areas_for_path(&self, path: &str) -> Vec<&str> {
        self.areas
            .iter()
            .filter(|(_, globs)| globs.iter().any(|g| g.covers(path)))
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// Whether the pack declares no areas.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }
}
