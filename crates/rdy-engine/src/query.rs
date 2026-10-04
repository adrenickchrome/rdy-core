use crate::error::*;
use crate::{Materialized, Rdy, Scope};
use rdy_graph::{DerivedStatus, Edge, Graph, Suspect};
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
}

#[derive(Clone, Debug, Serialize)]
pub struct ImpactItem {
    pub depth: u32,
    pub id: String,
    pub block_type: String,
    pub summary: String,
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
    let keys: &[&str] = if rdy_model::tree::is_tree_type(&b.block_type) {
        &["title", "name"]
    } else {
        &["statement", "prompt", "name", "title"]
    };
    let (text, mode) = match keys
        .iter()
        .find_map(|f| b.field_str(f).filter(|s| !s.is_empty()))
    {
        Some(field) => (field, RefMode::Strict),
        None => (b.body.as_str(), RefMode::for_body(&b.block_type)),
    };
    let mut out = String::new();
    if mode == RefMode::Off {
        out.push_str(text);
    } else {
        let plain = |s: &str| s.replace("\\{{", "{{");
        let mut last = 0;
        for span in ref_spans(text, mode) {
            out.push_str(&plain(&text[last..span.range.start]));
            out.push_str(&resolve(&span.token));
            last = span.range.end;
        }
        out.push_str(&plain(&text[last..]));
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
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

impl Rdy {
    pub fn graph(&mut self, scope: &Scope) -> Result<&Graph, RdyError> {
        self.materialized(scope)?;
        if !self.graphs.contains_key(scope) {
            let g = Graph::build(&self.cache[scope].blocks);
            self.graphs.insert(scope.clone(), g);
        }
        Ok(&self.graphs[scope])
    }

    pub fn suspects(&mut self, scope: &Scope) -> Result<Vec<Suspect>, RdyError> {
        self.graph(scope)?;
        self.materialized(scope)?;
        let (g, m, store) = (&self.graphs[scope], &self.cache[scope], &self.store);
        Ok(g.suspects(&m.blocks, &|h| store.get_block(h).ok()))
    }

    pub fn context(&mut self, scope: &Scope, id: &BlockId) -> Result<Context, RdyError> {
        let block = self.block(scope, id)?;
        self.graph(scope)?;
        let (m, g, store) = (&self.cache[scope], &self.graphs[scope], &self.store);
        let suspects = g.suspects_of(id, &m.blocks, &|h| store.get_block(h).ok());
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
        })
    }

    pub fn impact(
        &mut self,
        scope: &Scope,
        id: &BlockId,
        depth: u32,
    ) -> Result<Vec<ImpactItem>, RdyError> {
        self.block(scope, id)?;
        self.graph(scope)?;
        let (g, m) = (&self.graphs[scope], &self.cache[scope]);
        Ok(g.impact(id, depth)
            .into_iter()
            .filter_map(|(d, bid)| {
                m.get(&bid).map(|b| ImpactItem {
                    depth: d,
                    id: bid.to_string(),
                    block_type: b.block_type.clone(),
                    summary: summary_in(b, m),
                })
            })
            .collect())
    }
}
