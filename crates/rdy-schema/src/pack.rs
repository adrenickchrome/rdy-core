use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

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
pub struct SkeletonSection {
    pub title: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct DocTypeDef {
    #[serde(default)]
    pub skeleton: Vec<SkeletonSection>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
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
}

impl Pack {
    pub fn type_for_prefix(&self, prefix: &str) -> Option<&str> {
        self.block_types.iter().find(|(_, t)| t.prefix == prefix).map(|(n, _)| n.as_str())
    }
}
