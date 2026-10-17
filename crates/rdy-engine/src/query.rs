use crate::error::*;
use crate::{Materialized, ReadState, Scope};
use rdy_graph::{DerivedStatus, Edge, Suspect};
use rdy_model::entity::render_refs;
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::{Block, BlockId, FieldValue, RefMode, RefSpan};
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
    /// The pack areas the block is in (arn1); absent when it is in none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<String>,
    /// cgr1: in the `inflight` scope, the in-flight changes that write the block, by change;
    /// absent in every other scope and when none does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inflight: Vec<crate::coord_graph::InflightChange>,
    /// malt1: in a view that is a member of an open alternative set, the other members that also
    /// write the block; absent in every other scope and when none does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<crate::alt::AltPeer>,
    /// trk1: the block type's tracking fields, ascending; absent when it has none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tracking: Vec<String>,
    /// rdx6: for a stub of removed content, the request that removed it (its full hash); absent
    /// for every block whose content stays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redaction: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImpactItem {
    pub depth: u32,
    pub id: String,
    pub block_type: String,
    pub summary: String,
    pub via: ImpactVia,
    /// The pack areas the item is in (arn1); absent when it is in none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<String>,
}

/// The edge that first reached an impact item: the item depends on `from` through `kind`.
#[derive(Clone, Debug, Serialize)]
pub struct ImpactVia {
    pub from: String,
    pub kind: String,
}

/// A one-line label for `b` (at most 80 chars, cut at a word with `…`; see [`cut_line`]): its
/// statement, prompt, name or title field, else its body, with whitespace collapsed. A reference
/// with link words shows them; any other shows its bare id.
pub fn summary(b: &Block) -> String {
    summarize(b, &|span| {
        link_words(span).unwrap_or_else(|| span.token.clone())
    })
}

/// Like [`summary`], but a reference without link words shows what [`label_in`] gives for its
/// target in `m`: a value's text, else the target's name or title, else the id. The target's
/// body is never shown and a reference inside a target is never expanded.
pub fn summary_in(b: &Block, m: &Materialized) -> String {
    summarize(b, &|span| label_in(span, m))
}

/// `b`'s body with each reference rendered as [`summary_in`] renders it, whitespace and line
/// breaks kept (a fact sheet's definition).
pub(crate) fn body_in(b: &Block, m: &Materialized) -> String {
    render_refs(&b.body, RefMode::for_body(&b.block_type), &|span| {
        label_in(span, m)
    })
}

/// A reference's link words: its display text (`None` for a bare or `@key` reference).
fn link_words(span: &RefSpan) -> Option<String> {
    span.display.clone().filter(|d| !d.is_empty())
}

/// How a reference reads in [`summary_in`]. Closed rule: (1) link words show; (2) a reference
/// to a value shows the value's text; (3) any other reference to a known block shows the
/// target's `name` field, else its `title` field, else its id; (4) an unknown target shows its
/// id.
fn label_in(span: &RefSpan, m: &Materialized) -> String {
    if let Some(words) = link_words(span) {
        return words;
    }
    let Some(target) = BlockId::parse(&span.token).ok().and_then(|id| m.get(&id)) else {
        return span.token.clone();
    };
    if let Some(v) = rdy_model::entity::value_text(target) {
        return v;
    }
    ["name", "title"]
        .iter()
        .find_map(|f| target.field_str(f).filter(|s| !s.is_empty()))
        .map(|text| {
            // A reference inside a target is never expanded: it shows as link words or its id.
            render_refs(text, RefMode::Strict, &|inner| {
                link_words(inner).unwrap_or_else(|| inner.token.clone())
            })
        })
        .unwrap_or_else(|| span.token.clone())
}

/// `text` as one line of at most `max` characters: whitespace collapsed; a text that fits is
/// returned whole; a longer text is cut before character `max` at the last space (at `max - 1`
/// characters when the first word is longer), trailing whitespace dropped, and `…` added.
pub fn cut_line(text: &str, max: usize) -> String {
    let line = collapse(text);
    if line.chars().count() <= max {
        return line;
    }
    let head: Vec<char> = line.chars().take(max).collect();
    let keep = match head.iter().rposition(|c| *c == ' ') {
        Some(i) if i > 0 => i,
        _ => max.saturating_sub(1),
    };
    let mut out: String = head[..keep].iter().collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

fn summarize(b: &Block, resolve: &dyn Fn(&RefSpan) -> String) -> String {
    cut_line(&primary_text(b, resolve).0, 80)
}

/// `b`'s primary text (its statement, prompt, name or title field, else its body), with
/// whitespace collapsed and each reference rendered by `resolve`; and whether it is the body.
fn primary_text(b: &Block, resolve: &dyn Fn(&RefSpan) -> String) -> (String, bool) {
    let (text, from_body) = resolved_text(b, resolve);
    (collapse(&text), from_body)
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Like [`primary_text`], but with the text's own whitespace and line breaks kept.
fn resolved_text(b: &Block, resolve: &dyn Fn(&RefSpan) -> String) -> (String, bool) {
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

/// The source-only line that names the redaction a stub reads under: `Redaction: {hash}.`, the
/// request's full hash. None for a block whose content stays. Source reads show it after
/// `Content removed.`; renderer outputs never do.
fn redaction_line(b: &Block) -> Option<String> {
    b.redaction().map(|h| format!("Redaction: {h}."))
}

/// The full source text of `b`, as a source read shows its body: the body itself, or, for a stub
/// of removed content, `Content removed.` and then, after a blank line, the line that names its
/// redaction (`Redaction: {hash}.`).
pub fn source_body(b: &Block) -> String {
    match redaction_line(b) {
        Some(line) => format!("{CONTENT_REMOVED}\n\n{line}"),
        None => b.body.clone(),
    }
}

/// The longest a compact line's fallback text gets, ellipsis included.
const COMPACT_LINE_MAX: usize = 200;

/// read1: the compact read's text for `b`: its whole statement, prompt, name or title field, else
/// the first sentence of its body. A stub of removed content reads `Content removed.
/// Redaction: {hash}.` (rdx6). A body with no sentence end (and any diagram, code or table
/// body, which hold ". " as content) shows as its `alt` or `caption` field, else its first line,
/// capped at 200 characters with an ellipsis. References stay as bare ids; a field or sentence
/// is not cut at a length (the read's max_tokens does the cutting).
pub fn compact_summary(b: &Block) -> String {
    // a stub of removed content: the notice and the request that removed it, on one line
    if let Some(line) = redaction_line(b) {
        return format!("{CONTENT_REMOVED} {line}");
    }
    let (text, from_body) = resolved_text(b, &|span| span.token.clone());
    if !from_body {
        return collapse(&text);
    }
    let prose = !matches!(b.block_type.as_str(), "diagram" | "code" | "table");
    if prose && let Some(s) = first_sentence(&collapse(&text)) {
        return s.to_string();
    }
    for f in ["alt", "caption"] {
        if let Some(v) = b.field_str(f).filter(|s| !s.trim().is_empty()) {
            return collapse(&render_refs(v, RefMode::Strict, &|span| span.token.clone()));
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

/// cbody1: a `code` block's whole body, one entry per line (each `trim_end`ed), references shown
/// as bare ids. Any other block type gives no lines. This reads the body itself, never a
/// `statement`, `name` or `title` field a pack may set on `code`.
pub fn code_lines(b: &Block) -> Vec<String> {
    if b.block_type != "code" {
        return Vec::new();
    }
    render_refs(&b.body, RefMode::for_body(&b.block_type), &|span| {
        span.token.clone()
    })
    .lines()
    .map(|l| l.trim_end().to_string())
    .collect()
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
        let mut out = g.suspects();
        out.retain(|s| !self.is_presentational_source(&m, s));
        Ok(out)
    }

    /// A change to a presentational block (the suspect's `target`, the block whose content
    /// moved) makes nothing suspect. A target that is gone is typed by its id's prefix, since a
    /// pin records content, not a stored version (lpin1).
    pub(crate) fn is_presentational_source(&self, m: &Materialized, s: &Suspect) -> bool {
        let pack = self.pack();
        match m.get(&s.target) {
            Some(b) => pack.is_presentational(&b.block_type),
            None => pack
                .block_types
                .iter()
                .any(|(name, d)| d.prefix == s.target.prefix() && pack.is_presentational(name)),
        }
    }

    pub fn context(&self, scope: &Scope, id: &BlockId) -> Result<Context, RdyError> {
        let block = self.block(scope, id)?;
        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        let mut suspects = g.suspects_of(id);
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
            areas: self.areas_of(scope, id)?,
            inflight: match scope {
                Scope::Inflight => self.inflight_changes(id)?,
                _ => Vec::new(),
            },
            alternatives: match scope {
                Scope::View(v) => self.alt_peers(v, id)?,
                _ => Vec::new(),
            },
            tracking: self
                .pack()
                .block_types
                .get(&block.block_type)
                .map(|t| t.tracking.iter().cloned().collect())
                .unwrap_or_default(),
            redaction: block.redaction().map(|h| h.to_string()),
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
                    areas: self.areas_in(&m, b),
                })
            })
            .collect())
    }
}
