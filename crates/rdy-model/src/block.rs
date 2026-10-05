use crate::{BlockId, FieldValue, Hash};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn hash_of<T: Serialize>(t: &T) -> Hash {
    Hash::of(
        &postcard::to_allocvec(t).expect("postcard encoding of an in-memory value cannot fail"),
    )
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
/// display text, and `display` holds it unescaped (`{{id|\@x}}` gives `Some("@x")`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefSpan {
    pub range: std::ops::Range<usize>,
    pub token: String,
    pub display: Option<String>,
    pub key_typed: bool,
}

impl RefSpan {
    /// The stored `{{…}}` form of this span with `token` (for example a resolved id) in place of
    /// its own, keeping the key-typed mark or the display text (re-escaping a leading `@`).
    pub fn encode(&self, token: &str) -> String {
        match (&self.display, self.key_typed) {
            (_, true) => format!("{{{{{token}|@key}}}}"),
            (Some(d), false) if d.starts_with('@') => format!("{{{{{token}|\\{d}}}}}"),
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
            let display = match display {
                _ if key_typed => None,
                Some(d) => Some(
                    d.strip_prefix('\\')
                        .filter(|r| r.starts_with('@'))
                        .unwrap_or(d),
                ),
                None => None,
            };
            out.push(RefSpan {
                range: start..end,
                token: token.to_string(),
                display: display.map(str::to_string),
                key_typed,
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
