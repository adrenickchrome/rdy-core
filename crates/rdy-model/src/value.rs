use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value as Yaml;
use std::collections::BTreeMap;

/// The most levels one block field value nests (final review ruling 1): the top-level value is
/// level 1, and each element of a list or a map is one level below its container. A write refuses
/// a deeper field before anything is persisted, so every written block stays exportable: the
/// deepest record that embeds a block, a CHANGE whose op carries the full block, then stays
/// within the stream decoder's 64 nesting levels (`rdy_store::bounded::MAX_DEPTH`). The value is
/// the one the store's decoder test proves for that record (`ha1_field_nesting_bound_fits_the_
/// deepest_embedding_record`), not counted by hand.
pub const MAX_FIELD_NESTING: usize = 29;

/// The refusal text of a field deeper than [`MAX_FIELD_NESTING`] levels.
pub fn field_nesting_message() -> String {
    format!("field nesting exceeds {MAX_FIELD_NESTING} levels")
}

/// The fix of a field deeper than [`MAX_FIELD_NESTING`] levels.
pub fn field_nesting_fix() -> String {
    format!("flatten the field to at most {MAX_FIELD_NESTING} levels")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValue {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<FieldValue>),
    Map(BTreeMap<String, FieldValue>),
}

impl FieldValue {
    /// Whether this value nests more than `max` levels (itself level 1, each list or map element
    /// one level below its container). Walks without recursion and stops at the first value past
    /// `max`.
    pub fn nests_deeper_than(&self, max: usize) -> bool {
        let mut open = vec![(self, 1usize)];
        while let Some((v, level)) = open.pop() {
            if level > max {
                return true;
            }
            match v {
                FieldValue::List(items) => open.extend(items.iter().map(|i| (i, level + 1))),
                FieldValue::Map(m) => open.extend(m.values().map(|i| (i, level + 1))),
                _ => {}
            }
        }
        false
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            FieldValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn from_yaml(v: &Yaml) -> Result<FieldValue, String> {
        Ok(match v {
            Yaml::Null => FieldValue::Null,
            Yaml::Bool(b) => FieldValue::Bool(*b),
            Yaml::Number(n) => match n.as_i64() {
                Some(i) => FieldValue::Int(i),
                None => {
                    return Err(format!(
                        "number {n} is not an integer: floats are not supported, write it as a string"
                    ));
                }
            },
            Yaml::String(s) => FieldValue::Str(s.clone()),
            Yaml::Sequence(items) => FieldValue::List(
                items
                    .iter()
                    .map(FieldValue::from_yaml)
                    .collect::<Result<_, _>>()?,
            ),
            Yaml::Mapping(m) => {
                let mut out = BTreeMap::new();
                for (k, v) in m {
                    let k = k
                        .as_str()
                        .ok_or_else(|| format!("map key {k:?} must be a string"))?;
                    out.insert(k.to_string(), FieldValue::from_yaml(v)?);
                }
                FieldValue::Map(out)
            }
            Yaml::Tagged(t) => return Err(format!("YAML tag {} is not supported", t.tag)),
        })
    }

    pub fn to_yaml(&self) -> Yaml {
        match self {
            FieldValue::Null => Yaml::Null,
            FieldValue::Bool(b) => Yaml::Bool(*b),
            FieldValue::Int(i) => Yaml::Number((*i).into()),
            FieldValue::Str(s) => Yaml::String(s.clone()),
            FieldValue::List(items) => {
                Yaml::Sequence(items.iter().map(FieldValue::to_yaml).collect())
            }
            FieldValue::Map(m) => {
                let mut out = serde_yaml_ng::Mapping::new();
                for (k, v) in m {
                    out.insert(Yaml::String(k.clone()), v.to_yaml());
                }
                Yaml::Mapping(out)
            }
        }
    }
}
