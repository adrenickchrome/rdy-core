use crate::error::*;
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{DerivedStatus, Edge, Suspect};
use rdy_model::{Block, BlockId, FieldValue, RefMode, ref_spans};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Context {
    pub id: String,
    pub block_type: String,
    pub version: String,
    pub fields: serde_json::Value,
    pub body: String,
    pub doc: Option<String>,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub outgoing: Vec<Edge>,
    pub incoming: Vec<Edge>,
    pub status: Option<DerivedStatus>,
    pub governing: Vec<String>,
    pub suspects: Vec<Suspect>,
    /// When a decision takes effect: its `effective_from`, else when it was recorded. Absent for
    /// every other block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImpactItem {
    pub depth: u32,
    pub id: String,
    pub block_type: String,
    pub summary: String,
    pub via: ImpactVia,
}

/// The edge that first reached an impact item: the item depends on `from` through `kind`.
#[derive(Clone, Debug, Serialize)]
pub struct ImpactVia {
    pub from: String,
    pub kind: String,
}

/// A one-line label for `b` (at most 80 chars): its statement, prompt, name or title field, else
/// its body, with whitespace collapsed and each `{{id}}` reference shown as the bare id.
pub fn summary(b: &Block) -> String {
    summarize(b, &|tok| tok.to_string())
}

/// Like [`summary`], but each `{{id}}` reference to a block in `m` is shown as that block's own
/// [`summary`] (never expanded further); unknown targets show the bare id.
pub fn summary_in(b: &Block, m: &Materialized) -> String {
    summarize(b, &|tok| {
        BlockId::parse(tok)
            .ok()
            .and_then(|id| m.get(&id))
            .map(summary)
            .unwrap_or_else(|| tok.to_string())
    })
}

fn summarize(b: &Block, resolve: &dyn Fn(&str) -> String) -> String {
    primary_text(b, resolve).0.chars().take(80).collect()
}

/// `b`'s primary text (its statement, prompt, name or title field, else its body), with
/// whitespace collapsed and each reference rendered by `resolve`; and whether it is the body.
fn primary_text(b: &Block, resolve: &dyn Fn(&str) -> String) -> (String, bool) {
    let (text, from_body) = resolved_text(b, resolve);
    (collapse(&text), from_body)
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Like [`primary_text`], but with the text's own whitespace and line breaks kept.
fn resolved_text(b: &Block, resolve: &dyn Fn(&str) -> String) -> (String, bool) {
    let keys: &[&str] = if rdy_model::tree::is_tree_type(&b.block_type) {
        &["title", "name"]
    } else {
        &["statement", "prompt", "name", "title"]
    };
    match keys
        .iter()
        .find_map(|f| b.field_str(f).filter(|s| !s.is_empty()))
    {
        Some(field) => (render_refs(field, RefMode::Strict, resolve), false),
        None => (
            render_refs(&b.body, RefMode::for_body(&b.block_type), resolve),
            true,
        ),
    }
}

fn render_refs(text: &str, mode: RefMode, resolve: &dyn Fn(&str) -> String) -> String {
    if mode == RefMode::Off {
        return text.to_string();
    }
    let plain = |s: &str| s.replace("\\{{", "{{");
    let mut out = String::new();
    let mut last = 0;
    for span in ref_spans(text, mode) {
        out.push_str(&plain(&text[last..span.range.start]));
        out.push_str(&resolve(&span.token));
        last = span.range.end;
    }
    out.push_str(&plain(&text[last..]));
    out
}

/// The longest a compact line's fallback text gets, ellipsis included.
const COMPACT_LINE_MAX: usize = 200;

/// read1: the compact read's text for `b`: its whole statement, prompt, name or title field, else
/// the first sentence of its body. A body with no sentence end (and any diagram, code or table
/// body, which hold ". " as content) shows as its `alt` or `caption` field, else its first line,
/// capped at 200 characters with an ellipsis. References stay as bare ids; a field or sentence
/// is not cut at a length (the read's max_tokens does the cutting).
pub fn compact_summary(b: &Block) -> String {
    let (text, from_body) = resolved_text(b, &|tok| tok.to_string());
    if !from_body {
        return collapse(&text);
    }
    let prose = !matches!(b.block_type.as_str(), "diagram" | "code" | "table");
    if prose && let Some(s) = first_sentence(&collapse(&text)) {
        return s.to_string();
    }
    for f in ["alt", "caption"] {
        if let Some(v) = b.field_str(f).filter(|s| !s.trim().is_empty()) {
            return collapse(&render_refs(v, RefMode::Strict, &|tok| tok.to_string()));
        }
    }
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    let line = line.unwrap_or_default();
    if line.chars().count() > COMPACT_LINE_MAX {
        let mut cut: String = line.chars().take(COMPACT_LINE_MAX - 1).collect();
        cut.push('…');
        cut
    } else {
        line.to_string()
    }
}

/// Up to and including the first `.`, `?` or `!` followed by a space or the end of `text`.
fn first_sentence(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    text.char_indices()
        .find(|(i, c)| matches!(c, '.' | '?' | '!') && bytes.get(i + 1).is_none_or(|n| *n == b' '))
        .map(|(i, _)| &text[..=i])
}

pub fn field_json(v: &FieldValue) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        FieldValue::Null => J::Null,
        FieldValue::Bool(b) => J::Bool(*b),
        FieldValue::Int(i) => J::from(*i),
        FieldValue::Str(s) => J::String(s.clone()),
        FieldValue::List(l) => J::Array(l.iter().map(field_json).collect()),
        FieldValue::Map(m) => {
            J::Object(m.iter().map(|(k, v)| (k.clone(), field_json(v))).collect())
        }
    }
}

impl ReadState {
    pub fn suspects(&self, scope: &Scope) -> Result<Vec<Suspect>, RdyError> {
        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        let mut out = g.suspects(&*m, &|h| self.reader().get_block(h).ok());
        out.retain(|s| !self.is_presentational_source(&m, s));
        Ok(out)
    }

    /// A change to a presentational block (the suspect's `target`, the block whose version
    /// moved) makes nothing suspect. A target that is gone is typed by the version the suspect
    /// pinned, read back through the store.
    fn is_presentational_source(&self, m: &Materialized, s: &Suspect) -> bool {
        let presentational = |b: &Block| self.pack().is_presentational(&b.block_type);
        match m.get(&s.target) {
            Some(b) => presentational(b),
            None => m
                .get(&s.block)
                .and_then(|b| b.pins.get(&s.target))
                .and_then(|h| self.reader().get_block(h).ok())
                .is_some_and(|old| presentational(&old)),
        }
    }

    pub fn context(&self, scope: &Scope, id: &BlockId) -> Result<Context, RdyError> {
        let block = self.block(scope, id)?;
        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        let mut suspects = g.suspects_of(id, &*m, &|h| self.reader().get_block(h).ok());
        suspects.retain(|s| !self.is_presentational_source(&m, s));
        Ok(Context {
            id: block.id.to_string(),
            block_type: block.block_type.clone(),
            version: block.version_hash().to_string(),
            fields: serde_json::Value::Object(
                block
                    .fields
                    .iter()
                    .map(|(k, v)| (k.clone(), field_json(v)))
                    .collect(),
            ),
            body: block.body.clone(),
            doc: m.doc_of(id).map(|d| d.id.to_string()),
            parent: block.parent.as_ref().map(|p| p.to_string()),
            children: m
                .children(Some(id))
                .iter()
                .map(|c| c.id.to_string())
                .collect(),
            outgoing: g.outgoing(id).to_vec(),
            incoming: g.incoming(id).to_vec(),
            status: g.status(id),
            governing: g.governing(id).iter().map(|d| d.to_string()).collect(),
            suspects,
            effective_from: rdy_model::decision::effective_from(&block).map(str::to_string),
        })
    }

    pub fn impact(
        &self,
        scope: &Scope,
        id: &BlockId,
        depth: u32,
    ) -> Result<Vec<ImpactItem>, RdyError> {
        self.block(scope, id)?;
        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        Ok(g.impact_via(id, depth)
            .into_iter()
            .filter_map(|h| {
                m.get(&h.id).map(|b| ImpactItem {
                    depth: h.depth,
                    id: h.id.to_string(),
                    block_type: b.block_type.clone(),
                    summary: summary_in(b, &m),
                    via: ImpactVia {
                        from: h.via.from.to_string(),
                        kind: h.via.kind,
                    },
                })
            })
            .collect())
    }
}
