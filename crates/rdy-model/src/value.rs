use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value as Yaml;
use std::collections::BTreeMap;

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
