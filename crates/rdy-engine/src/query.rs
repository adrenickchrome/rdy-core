use crate::error::*;
use crate::{Rdy, Scope};
use rdy_graph::{DerivedStatus, Edge, Graph, Suspect};
use rdy_model::{Block, BlockId, FieldValue};
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

pub fn summary(b: &Block) -> String {
    let text = ["statement", "name", "title"].iter().find_map(|f| b.field_str(f).filter(|s| !s.is_empty())).unwrap_or(b.body.as_str());
    text.chars().take(80).collect()
}

pub fn field_json(v: &FieldValue) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        FieldValue::Null => J::Null,
        FieldValue::Bool(b) => J::Bool(*b),
        FieldValue::Int(i) => J::from(*i),
        FieldValue::Str(s) => J::String(s.clone()),
        FieldValue::List(l) => J::Array(l.iter().map(field_json).collect()),
        FieldValue::Map(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), field_json(v))).collect()),
    }
}

impl Rdy {
    pub fn graph(&mut self, scope: &Scope) -> Result<&Graph, RdyError> {
        if !self.graphs.contains_key(scope) {
            let g = Graph::build(&self.materialized(scope)?.blocks);
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
        let all_suspects = self.suspects(scope)?;
        let m = &self.cache[scope];
        let g = &self.graphs[scope];
        Ok(Context {
            id: block.id.to_string(),
            block_type: block.block_type.clone(),
            version: block.version_hash().to_string(),
            fields: serde_json::Value::Object(block.fields.iter().map(|(k, v)| (k.clone(), field_json(v))).collect()),
            body: block.body.clone(),
            doc: m.doc_of(id).map(|d| d.id.to_string()),
            parent: block.parent.as_ref().map(|p| p.to_string()),
            children: m.children(Some(id)).iter().map(|c| c.id.to_string()).collect(),
            outgoing: g.outgoing(id).to_vec(),
            incoming: g.incoming(id).to_vec(),
            status: g.status(id),
            governing: g.governing(id).iter().map(|d| d.to_string()).collect(),
            suspects: all_suspects.into_iter().filter(|s| &s.block == id).collect(),
        })
    }

    pub fn impact(&mut self, scope: &Scope, id: &BlockId, depth: u32) -> Result<Vec<ImpactItem>, RdyError> {
        self.block(scope, id)?;
        self.graph(scope)?;
        let (g, m) = (&self.graphs[scope], &self.cache[scope]);
        Ok(g.impact(id, depth)
            .into_iter()
            .filter_map(|(d, bid)| m.get(&bid).map(|b| ImpactItem { depth: d, id: bid.to_string(), block_type: b.block_type.clone(), summary: summary(b) }))
            .collect())
    }
}
