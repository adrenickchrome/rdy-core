//! An entity's fact sheet (FW-EN1, FW-EN1a).
use crate::error::{E_BAD_INPUT, E_UNKNOWN_BLOCK, RdyError};
use crate::lexicon::{NameKind, is_content, is_fact};
use crate::query::body_in;
use crate::{ReadState, Scope, summary_in};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::decision::{AFFECTS, DECIDED_BY, DECISION, DESCRIBES, IMPLEMENTS};
use rdy_model::entity::{KIND, aliases, value_text};
use rdy_model::{Block, BlockId};
use rdy_schema::Trait;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The most entries each list of a sheet holds; `omitted` counts the rest per list.
pub const SHEET_LIST_CAP: usize = 20;
/// The most fact sheets one search returns; `entities_omitted` counts the rest.
pub const MAX_RESOLVED: usize = 8;

/// What the graph says about one entity: its definition and value, the facts and other content
/// that describe it, the decisions that govern it or those, and what else depends on it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FactSheet {
    pub id: String,
    #[serde(rename = "type")]
    pub block_type: String,
    pub matched_by: NameKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The entity's body, each reference read as the summaries read it (a value as its value).
    pub definition: String,
    pub path: Option<String>,
    /// Normative facts tied to the entity (claims, requirements, constraints, flows).
    pub facts: Vec<SheetBlock>,
    /// Other content tied to the entity (prose, notes, examples, ...).
    pub also_described_in: Vec<SheetBlock>,
    pub decisions: Vec<SheetDecision>,
    pub dependents: Vec<SheetBlock>,
    /// Entries each list's cut dropped, by list name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SheetBlock {
    pub id: String,
    #[serde(rename = "type")]
    pub block_type: String,
    pub via: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SheetDecision {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub status: String,
    pub statement: String,
    pub via: String,
    /// The facts or content blocks a `governs` decision governs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<String>,
}

/// The order in which a block met twice keeps its `via`: the first of these wins.
const VIA_ORDER: [&str; 5] = [AFFECTS, DESCRIBES, "references", DECIDED_BY, "governs"];

fn via_rank(via: &str) -> usize {
    VIA_ORDER
        .iter()
        .position(|v| *v == via)
        .unwrap_or(VIA_ORDER.len())
}

/// A tie edge's kind (a reference, a `describes` or an `affects` link), or `None`.
fn tie_kind(kind: &EdgeKind) -> Option<String> {
    match kind {
        EdgeKind::References => Some(kind.name()),
        EdgeKind::Link(n) if n == DESCRIBES || n == AFFECTS => Some(n.clone()),
        _ => None,
    }
}

/// A decision's derived status tag when the sheet lists it (`live`, `partially_superseded` or
/// `proposed`); `None` for any other status and for a block that is not a decision.
fn listed_status(g: &Graph, id: &BlockId) -> Option<&'static str> {
    g.status(id)
        .map(|s| s.tag())
        .filter(|t| matches!(*t, "live" | "partially_superseded" | "proposed"))
}

/// Records `id` with `via` in `list`, keeping the earlier `via` in `VIA_ORDER`.
fn note(list: &mut BTreeMap<BlockId, String>, id: &BlockId, via: String) {
    match list.get(id) {
        Some(old) if via_rank(old) <= via_rank(&via) => {}
        _ => {
            list.insert(id.clone(), via);
        }
    }
}

/// Sorts by id (the map's order), cuts to `SHEET_LIST_CAP` and counts the cut under `name`.
fn cut<T>(name: &str, mut items: Vec<T>, omitted: &mut BTreeMap<String, usize>) -> Vec<T> {
    if items.len() > SHEET_LIST_CAP {
        omitted.insert(name.to_string(), items.len() - SHEET_LIST_CAP);
        items.truncate(SHEET_LIST_CAP);
    }
    items
}

impl ReadState {
    /// The entities `text` names in `scope` (key, name or alias, any case and spacing), each once
    /// with its best match kind, sorted by (kind, id).
    pub fn resolve_entities(
        &self,
        scope: &Scope,
        text: &str,
    ) -> Result<Vec<(BlockId, NameKind)>, RdyError> {
        let m = self.materialized(scope)?;
        let mut out = self.lexicon(scope)?.resolve(text);
        out.retain(|(id, _)| m.get(id).is_some());
        Ok(out)
    }

    /// fsh1: the fact sheet of entity `id` in `scope`.
    pub fn fact_sheet(
        &self,
        scope: &Scope,
        id: &BlockId,
        matched_by: NameKind,
    ) -> Result<FactSheet, RdyError> {
        let m = self.materialized(scope)?;
        let e = m.get(id).ok_or_else(|| {
            RdyError::new(
                E_UNKNOWN_BLOCK,
                format!("no block {id}"),
                "check the id with rdy_read",
            )
        })?;
        let pack = self.pack();
        let defines = pack
            .block_types
            .get(&e.block_type)
            .is_some_and(|t| t.traits.contains(&Trait::Defines));
        if !defines {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{id} is a {}, not an entity", e.block_type),
                "pass an entity: a block of a type that defines something",
            ));
        }
        let g = self.graph(scope)?;

        let mut facts: BTreeMap<BlockId, String> = BTreeMap::new();
        let mut content: BTreeMap<BlockId, String> = BTreeMap::new();
        let mut dependents: BTreeMap<BlockId, String> = BTreeMap::new();
        // id -> (via, status, on)
        let mut decisions: BTreeMap<BlockId, (String, &'static str, BTreeSet<BlockId>)> =
            BTreeMap::new();
        let mut decide = |d: &BlockId, via: String, status: &'static str, on: Option<&BlockId>| {
            let entry = decisions
                .entry(d.clone())
                .or_insert_with(|| (via.clone(), status, BTreeSet::new()));
            if via_rank(&via) < via_rank(&entry.0) {
                entry.0 = via;
            }
            if let Some(b) = on {
                entry.2.insert(b.clone());
            }
        };
        for edge in g.incoming(id) {
            let Some(via) = tie_kind(&edge.kind) else {
                continue;
            };
            // an entity that names itself is not its own dependent
            let Some(from) = m.get(&edge.from).filter(|f| f.id != *id) else {
                continue;
            };
            if from.block_type == DECISION {
                // a rejected, superseded or revoked decision is not listed
                if let Some(status) = listed_status(&g, &from.id) {
                    decide(&from.id, via, status, None);
                }
            } else if is_fact(pack, from) {
                note(&mut facts, &from.id, via);
            } else if is_content(pack, from) {
                note(&mut content, &from.id, via);
            } else {
                note(&mut dependents, &from.id, via);
            }
        }

        // the decisions that govern each fact and content block: the edges `Graph::governing`
        // reads, with proposed decisions kept too
        for b in facts.keys().chain(content.keys()) {
            let governing = g
                .outgoing(b)
                .iter()
                .filter(
                    |e| matches!(&e.kind, EdgeKind::Link(n) if n == DECIDED_BY || n == IMPLEMENTS),
                )
                .map(|e| &e.to)
                .chain(
                    g.incoming(b)
                        .iter()
                        .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == AFFECTS))
                        .map(|e| &e.from),
                );
            for d in governing {
                if let Some(status) = listed_status(&g, d) {
                    decide(d, "governs".into(), status, Some(b));
                }
            }
        }
        for edge in g.outgoing(id) {
            if matches!(&edge.kind, EdgeKind::Link(n) if n == DECIDED_BY)
                && let Some(status) = listed_status(&g, &edge.to)
            {
                decide(&edge.to, DECIDED_BY.into(), status, None);
            }
        }

        // what else depends on it, unless the sheet lists it already
        for hit in g.impact_via(id, 1) {
            let listed = facts.contains_key(&hit.id)
                || content.contains_key(&hit.id)
                || decisions.contains_key(&hit.id)
                || dependents.contains_key(&hit.id);
            // a decision the sheet does not list stays off it
            let shown = m
                .get(&hit.id)
                .is_some_and(|b| b.block_type != DECISION || listed_status(&g, &hit.id).is_some());
            if listed || !shown {
                continue;
            }
            dependents.insert(hit.id, hit.via.kind);
        }

        let blocks = |list: BTreeMap<BlockId, String>| -> Vec<SheetBlock> {
            list.into_iter()
                .filter_map(|(bid, via)| {
                    let b = m.get(&bid)?;
                    Some(SheetBlock {
                        id: bid.to_string(),
                        block_type: b.block_type.clone(),
                        via,
                        summary: summary_in(b, &m),
                    })
                })
                .collect()
        };
        let decisions: Vec<SheetDecision> = decisions
            .into_iter()
            .filter_map(|(did, (via, status, on))| {
                let d = m.get(&did)?;
                Some(SheetDecision {
                    id: did.to_string(),
                    key: d.field_str("key").map(str::to_string),
                    status: status.to_string(),
                    statement: d.field_str("statement").unwrap_or_default().to_string(),
                    via,
                    on: on.iter().map(BlockId::to_string).collect(),
                })
            })
            .collect();

        let mut omitted = BTreeMap::new();
        let facts = cut("facts", blocks(facts), &mut omitted);
        let also_described_in = cut("also_described_in", blocks(content), &mut omitted);
        let decisions = cut("decisions", decisions, &mut omitted);
        let dependents = cut("dependents", blocks(dependents), &mut omitted);
        let path = match m.doc_of(id) {
            Some(doc) => m.path_of(&doc.id),
            None => m.path_of(id),
        };
        Ok(FactSheet {
            id: id.to_string(),
            block_type: e.block_type.clone(),
            matched_by,
            name: name_of(e),
            key: e.field_str("key").map(str::to_string),
            aliases: aliases(e).into_iter().map(str::to_string).collect(),
            kind: e
                .field_str(KIND)
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(str::to_string),
            value: value_text(e),
            definition: body_in(e, &m).trim().to_string(),
            path,
            facts,
            also_described_in,
            decisions,
            dependents,
            omitted,
        })
    }
}

/// An entity's name: its `name`, else its `title`, else empty.
fn name_of(b: &Block) -> String {
    b.field_str("name")
        .or_else(|| b.field_str("title"))
        .unwrap_or_default()
        .to_string()
}
