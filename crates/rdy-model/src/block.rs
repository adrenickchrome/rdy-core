use crate::{BlockId, FieldValue, Hash};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn hash_of<T: Serialize>(t: &T) -> Hash {
    Hash::of(
        &postcard::to_allocvec(t).expect("postcard encoding of an in-memory value cannot fail"),
    )
}

/// The domain prefix of [`Block::content_hash`], so a content hash never equals a version hash.
pub const CONTENT_DOMAIN: &[u8] = b"rdy-content-1\0";

/// Fields rdy derives from other state (a decision's status and date, a section's lifecycle).
/// They are not part of a block's content, so a pin does not see them change.
pub const DERIVED_FIELDS: [&str; 3] = ["status", "decided_at", "lifecycle"];

/// trk1 (FW-TR1, D-DZC1J): the tracking fields a pack marks, per block type. A tracking field
/// stays writable on an accepted or rejected decision and is left out of the content hash.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tracking {
    by_type: BTreeMap<String, BTreeSet<String>>,
}

impl Tracking {
    /// The tracking fields of each block type; a type with an empty set is dropped.
    pub fn new(map: BTreeMap<String, BTreeSet<String>>) -> Tracking {
        Tracking {
            by_type: map.into_iter().filter(|(_, f)| !f.is_empty()).collect(),
        }
    }

    /// The tracking fields of `block_type`, if it has any.
    pub fn fields(&self, block_type: &str) -> Option<&BTreeSet<String>> {
        self.by_type.get(block_type)
    }

    pub fn is_tracking(&self, block_type: &str, field: &str) -> bool {
        self.by_type
            .get(block_type)
            .is_some_and(|f| f.contains(field))
    }

    pub fn is_empty(&self) -> bool {
        self.by_type.is_empty()
    }
}

/// The part of a block a pin records (FW-LP1): everything except its pins and derived fields.
#[derive(Serialize)]
struct Content<'a> {
    id: &'a BlockId,
    block_type: &'a String,
    parent: &'a Option<BlockId>,
    order: &'a String,
    fields: Vec<(&'a String, &'a FieldValue)>,
    body: &'a String,
    links: &'a BTreeMap<String, Vec<Link>>,
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

    /// lpin1 (FW-LP1): the hash a pin records. It covers the block's id, type, parent, order,
    /// fields except [`DERIVED_FIELDS`], body and links, so a change to the block's pins or
    /// derived fields only leaves it unchanged. Domain-separated by [`CONTENT_DOMAIN`].
    /// Equal to [`Block::content_hash_with`] with an empty [`Tracking`].
    pub fn content_hash(&self) -> Hash {
        self.content_hash_with(&Tracking::default())
    }

    /// trk1: [`Block::content_hash`] that also leaves out the tracking fields of the block's
    /// type, so a change to a tracking field makes no pin stale. With an empty [`Tracking`] it
    /// equals [`Block::content_hash`] byte for byte.
    pub fn content_hash_with(&self, t: &Tracking) -> Hash {
        let tracking = t.fields(&self.block_type);
        let content = Content {
            id: &self.id,
            block_type: &self.block_type,
            parent: &self.parent,
            order: &self.order,
            fields: self
                .fields
                .iter()
                .filter(|(k, _)| !DERIVED_FIELDS.contains(&k.as_str()))
                .filter(|(k, _)| !tracking.is_some_and(|f| f.contains(k.as_str())))
                .collect(),
            body: &self.body,
            links: &self.links,
        };
        let mut bytes = CONTENT_DOMAIN.to_vec();
        bytes.extend(
            postcard::to_allocvec(&content)
                .expect("postcard encoding of an in-memory value cannot fail"),
        );
        Hash::of(&bytes)
    }

    pub fn field_str(&self, name: &str) -> Option<&str> {
        self.fields.get(name).and_then(FieldValue::as_str)
    }

    /// Every `{{token}}` this block's body and string fields contain, under `RefMode` rules.
    pub fn ref_tokens(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = ref_spans(&self.body, RefMode::for_body(&self.block_type))
            .into_iter()
            .map(|s| s.token)
            .collect();
        for v in self.fields.values() {
            scan_value(v, &mut out);
        }
        out
    }
}

/// How `{{…}}` in a text is read. In every mode `\{{` is an escape for a literal `{{`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefMode {
    /// Every `{{…}}` is a reference (prose bodies and all string fields).
    Strict,
    /// Only id-shaped tokens and `$placeholder`s are references; other `{{…}}` is content
    /// (diagram source, e.g. a Mermaid hexagon `A{{Decide?}}`).
    IdShaped,
    /// Nothing is a reference (code bodies).
    Off,
}

impl RefMode {
    /// The mode for the body of a block of `block_type`; fields are always `Strict`.
    pub fn for_body(block_type: &str) -> RefMode {
        match block_type {
            "code" => RefMode::Off,
            "diagram" => RefMode::IdShaped,
            _ => RefMode::Strict,
        }
    }
}

/// One reference in a text: `range` covers the whole `{{…}}`. `token` is the trimmed text before
/// the first `|` (the id, a `$placeholder` or a key), and `display` is the trimmed rest, if any.
///
/// A display text that is exactly `@key` is the stored mark of a reference the author typed as a
/// key: `key_typed` is then true and `display` is `None`. `\@` escapes a literal leading `@` in
/// display text, and `display` holds it unescaped (`{{id|\@x}}` gives `Some("@x")`), with
/// `at_escaped` true. Any other display text starting with an unescaped `@` is reserved (FW-F2a);
/// validation refuses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefSpan {
    pub range: std::ops::Range<usize>,
    pub token: String,
    pub display: Option<String>,
    pub key_typed: bool,
    pub at_escaped: bool,
}

impl RefSpan {
    /// The stored `{{…}}` form of this span with `token` (for example a resolved id) in place of
    /// its own, keeping the key-typed mark or the display text as written (an escaped leading `@`
    /// stays escaped and an unescaped one stays unescaped, so validation sees the author's text).
    pub fn encode(&self, token: &str) -> String {
        match (&self.display, self.key_typed) {
            (_, true) => format!("{{{{{token}|@key}}}}"),
            (Some(d), false) if self.at_escaped => format!("{{{{{token}|\\{d}}}}}"),
            (Some(d), false) => format!("{{{{{token}|{d}}}}}"),
            (None, false) => format!("{{{{{token}}}}}"),
        }
    }
}

/// The references in `text`, in order, under `mode`.
pub fn ref_spans(text: &str, mode: RefMode) -> Vec<RefSpan> {
    let mut out = vec![];
    if mode == RefMode::Off {
        return out;
    }
    let mut pos = 0;
    while let Some(found) = text[pos..].find("{{") {
        let start = pos + found;
        if text[..start].ends_with('\\') {
            pos = start + 2;
            continue;
        }
        let Some(len) = text[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + len + 2;
        let inner = &text[start + 2..start + 2 + len];
        let (token, display) = match inner.split_once('|') {
            Some((t, d)) => (t.trim(), Some(d.trim()).filter(|d| !d.is_empty())),
            None => (inner.trim(), None),
        };
        if mode == RefMode::Strict || token.starts_with('$') || BlockId::parse(token).is_ok() {
            let key_typed = display == Some("@key");
            let escaped = display
                .and_then(|d| d.strip_prefix('\\'))
                .filter(|r| r.starts_with('@'));
            let display = match display {
                _ if key_typed => None,
                Some(d) => Some(escaped.unwrap_or(d)),
                None => None,
            };
            out.push(RefSpan {
                range: start..end,
                token: token.to_string(),
                display: display.map(str::to_string),
                key_typed,
                at_escaped: escaped.is_some() && !key_typed,
            });
        }
        pos = end;
    }
    out
}

fn scan_value(v: &FieldValue, out: &mut BTreeSet<String>) {
    match v {
        FieldValue::Str(s) => {
            out.extend(ref_spans(s, RefMode::Strict).into_iter().map(|s| s.token))
        }
        FieldValue::List(items) => items.iter().for_each(|i| scan_value(i, out)),
        FieldValue::Map(m) => m.values().for_each(|i| scan_value(i, out)),
        _ => {}
    }
}
