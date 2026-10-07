//! Keys: short human ids (`fields.key`, e.g. `REQ-12`), unique per project ignoring case. The
//! engine resolves them on write and import, so the store, pins and the graph stay id-only.
use crate::Materialized;
use crate::error::*;
use rdy_model::{Block, BlockId, FieldValue, RefMode, key, ref_spans};
use std::collections::BTreeMap;

/// The key `b` holds, as written.
pub(crate) fn key_of(b: &Block) -> Option<&str> {
    b.field_str("key")
}

/// Folded key → its holders (key as written, id), in the order the blocks were given.
#[derive(Default)]
pub(crate) struct Keys {
    map: BTreeMap<String, Vec<(String, BlockId)>>,
}

impl Keys {
    pub(crate) fn of<'a>(blocks: impl IntoIterator<Item = &'a Block>) -> Keys {
        let mut k = Keys::default();
        for b in blocks {
            k.add(b);
        }
        k
    }

    pub(crate) fn add(&mut self, b: &Block) {
        if let Some(written) = key_of(b) {
            self.map
                .entry(key::fold(written))
                .or_default()
                .push((written.to_string(), b.id.clone()));
        }
    }

    /// The first holder of `text` (compared ignoring case).
    pub(crate) fn get(&self, text: &str) -> Option<&(String, BlockId)> {
        self.map.get(&key::fold(text))?.first()
    }

    /// Every holder of `text` (compared ignoring case).
    pub(crate) fn holders(&self, text: &str) -> &[(String, BlockId)] {
        self.map
            .get(&key::fold(text))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// The `E_KEY_CLASH` error for key `k` held by both `a` and `b`, the lower id named first
/// (import and check word it the same way).
pub(crate) fn clash_error(k: &str, a: &BlockId, b: &BlockId) -> RdyError {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    RdyError::new(
        E_KEY_CLASH,
        format!("key {k:?} is on both {lo} and {hi}"),
        "give one of them another key",
    )
}

/// The section titles under each parent among `blocks`, in the order given (skel1: what a doc
/// holds for `skeleton_gaps`, read by import and check).
pub(crate) fn section_titles<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
) -> BTreeMap<&'a BlockId, Vec<&'a str>> {
    let mut titles: BTreeMap<&BlockId, Vec<&str>> = BTreeMap::new();
    for b in blocks {
        if b.block_type == "section"
            && let (Some(p), Some(t)) = (&b.parent, b.field_str("title"))
        {
            titles.entry(p).or_default().push(t);
        }
    }
    titles
}

/// Every pair of distinct ids among `blocks` sharing a key (ignoring case), each pair once and
/// ordered by id: (the first holder's key as written, the lower id, the higher id).
pub(crate) fn clashes<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
) -> Vec<(String, BlockId, BlockId)> {
    let mut by_key: BTreeMap<String, Vec<(&str, &BlockId)>> = BTreeMap::new();
    for b in blocks {
        if let Some(k) = key_of(b) {
            by_key.entry(key::fold(k)).or_default().push((k, &b.id));
        }
    }
    let mut out = vec![];
    for mut holders in by_key.into_values() {
        holders.sort_by(|a, b| a.1.cmp(b.1));
        holders.dedup_by(|a, b| a.1 == b.1);
        for (i, (k, a)) in holders.iter().enumerate() {
            for (_, b) in &holders[i + 1..] {
                out.push((k.to_string(), (*a).clone(), (*b).clone()));
            }
        }
    }
    out.sort_by(|x, y| (&x.1, &x.2).cmp(&(&y.1, &y.2)));
    out
}

/// `text` with every `{{KEY}}` and `{{KEY|words}}` that `lookup` resolves stored as the id. In
/// `Strict` text only: a key typed bare keeps the key-typed mark (`{{id|@key}}`, FW-F2a), and
/// display text is kept. Ids, `$placeholder`s and unresolved tokens are copied unchanged.
pub(crate) fn rewrite(
    text: &str,
    mode: RefMode,
    lookup: &dyn Fn(&str) -> Option<BlockId>,
) -> String {
    if mode != RefMode::Strict || !text.contains("{{") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for span in ref_spans(text, mode) {
        if span.token.starts_with('$')
            || BlockId::parse(&span.token).is_ok()
            || key::problem(&span.token).is_some()
        {
            continue;
        }
        let Some(id) = lookup(&span.token) else {
            continue;
        };
        let mut stored = span.clone();
        stored.key_typed = span.key_typed || span.display.is_none();
        out.push_str(&text[last..span.range.start]);
        out.push_str(&stored.encode(&id.to_string()));
        last = span.range.end;
    }
    out.push_str(&text[last..]);
    out
}

/// A block id, else a key, else a path. A text that is both the key of one block and the path of
/// another is refused rather than letting one silently win.
pub(crate) fn resolve_text(m: &Materialized, text: &str) -> Result<BlockId, RdyError> {
    resolve_with(m, text, &|t| {
        let folded = key::fold(t);
        m.iter()
            .find(|b| key_of(b).is_some_and(|k| key::fold(k) == folded))
            .map(|b| b.id.clone())
    })
}

/// `resolve_text` with the key lookup given (a write counts keys set by its earlier ops).
pub(crate) fn resolve_with(
    m: &Materialized,
    text: &str,
    key_lookup: &dyn Fn(&str) -> Option<BlockId>,
) -> Result<BlockId, RdyError> {
    if let Ok(id) = BlockId::parse(text) {
        return Ok(id);
    }
    let by_key = if key::problem(text).is_none() {
        key_lookup(text)
    } else {
        None
    };
    let by_path = crate::paths::resolve_in(m, text);
    match (by_key, by_path) {
        (Some(k), Ok(p)) if k != p => Err(RdyError::new(
            E_BAD_INPUT,
            format!("{text:?} is both the key of {k} and the path of {p}"),
            "use the id",
        )),
        (Some(k), _) => Ok(k),
        (None, path) => path,
    }
}

/// `b` with its body (as its type reads references) and every string field (strictly, through
/// lists and maps) passed through `rewrite`.
pub(crate) fn rewrite_block(b: &mut Block, lookup: &dyn Fn(&str) -> Option<BlockId>) {
    fn value(v: &mut FieldValue, lookup: &dyn Fn(&str) -> Option<BlockId>) {
        match v {
            FieldValue::Str(s) => *s = rewrite(s, RefMode::Strict, lookup),
            FieldValue::List(items) => items.iter_mut().for_each(|i| value(i, lookup)),
            FieldValue::Map(m) => m.values_mut().for_each(|i| value(i, lookup)),
            _ => {}
        }
    }
    b.body = rewrite(&b.body, RefMode::for_body(&b.block_type), lookup);
    b.fields.values_mut().for_each(|v| value(v, lookup));
}

/// lbr1: the fix text that says how to write a `{{token}}` that names nothing as literal text.
pub(crate) fn literal_hint(token: &str) -> String {
    format!("or, if \"{{{{{token}}}}}\" is literal text, write it as \\{{{{{token}}}}}")
}
