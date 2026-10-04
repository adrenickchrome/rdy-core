use crate::{BlockId, FieldValue, Hash};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn hash_of<T: Serialize>(t: &T) -> Hash {
    Hash::of(&postcard::to_allocvec(t).expect("postcard encoding of an in-memory value cannot fail"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub target: BlockId,
    pub part: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub block_type: String,
    pub parent: Option<BlockId>,
    pub order: String,
    pub fields: BTreeMap<String, FieldValue>,
    pub body: String,
    pub links: BTreeMap<String, Vec<Link>>,
    pub pins: BTreeMap<BlockId, Hash>,
}

impl Block {
    pub fn version_hash(&self) -> Hash {
        hash_of(self)
    }

    pub fn field_str(&self, name: &str) -> Option<&str> {
        self.fields.get(name).and_then(FieldValue::as_str)
    }

    pub fn ref_tokens(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        scan(&self.body, &mut out);
        for v in self.fields.values() {
            scan_value(v, &mut out);
        }
        out
    }
}

fn scan_value(v: &FieldValue, out: &mut BTreeSet<String>) {
    match v {
        FieldValue::Str(s) => scan(s, out),
        FieldValue::List(items) => items.iter().for_each(|i| scan_value(i, out)),
        FieldValue::Map(m) => m.values().for_each(|i| scan_value(i, out)),
        _ => {}
    }
}

fn scan(text: &str, out: &mut BTreeSet<String>) {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                out.insert(after[..end].trim().to_string());
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
}
