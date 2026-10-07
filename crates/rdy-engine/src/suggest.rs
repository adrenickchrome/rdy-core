//! Suggestions (FW-EN3): links to the entities written text names, and new entities for literals that recur.
use rdy_model::Hash;
use serde::Serialize;

/// One suggestion: a link from a written block to an entity its text names, or a new entity for
/// a code literal that recurs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Suggestion {
    Link {
        id: String,
        block: String,
        link: String,
        target: String,
        mention: String,
        field: String,
    },
    Entity {
        id: String,
        literal: String,
        blocks: Vec<String>,
        count: usize,
    },
}

/// A suggestion's id: `sg-` and the first 16 hex characters of the hash of
/// `rdy-suggestion\0{kind}\0{subject}\0{target}`.
pub fn suggestion_id(kind: &str, subject: &str, target: &str) -> String {
    let mut bytes = b"rdy-suggestion\0".to_vec();
    for (i, part) in [kind, subject, target].into_iter().enumerate() {
        if i > 0 {
            bytes.push(0);
        }
        bytes.extend_from_slice(part.as_bytes());
    }
    format!("sg-{}", &Hash::of(&bytes).to_string()[..16])
}
