//! What a write leaves for its author to do: the blocks that depend on what the write changed,
//! read from the state the write published. Each entry has a stable id (`cq-<16 hex>`).
use crate::brief::{BRIEF_DEFAULT_TOKENS, FollowUp, expand_error, follow_up};
use crate::engine::{Engine, write_typed};
use crate::error::RdyError;
use crate::{Rdy, ReadState, Scope};
use rdy_graph::{Edge, EdgeKind, IMPACT_LINKS, SUSPECT_LINKS, Suspect};
use rdy_model::decision::{DECISION, REVOKES, SUPERSEDES};
use rdy_model::{BlockId, Hash, Op};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Why a block is listed. M2c3 adds `Conflict`, M2d2 `Tension`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsequenceKind {
    /// The block is suspect on a changed block in the state the write published.
    Suspect,
    /// The block depends on a changed block but cannot be suspect (yet).
    Dependent,
}

impl ConsequenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConsequenceKind::Suspect => "suspect",
            ConsequenceKind::Dependent => "dependent",
        }
    }
}

/// The edge and the changed block behind a consequence.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Because {
    pub edge: String,
    pub changed: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Consequence {
    pub id: String,
    pub block: String,
    pub kind: ConsequenceKind,
    pub because: Because,
    pub follow_up: FollowUp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Consequences {
    pub items: Vec<Consequence>,
    /// How many entries a cut dropped, by kind.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub omitted: BTreeMap<String, usize>,
}

/// The hint on a `reaffirm` entry.
pub const REAFFIRM_FIX: &str =
    "supersede it, or leave it suspect; re-affirmation arrives with FW-R6";

/// `cq-` and the first 16 hex characters of the hash of the batch, kind and target.
/// For a write, `batch` is the change hash.
pub fn consequence_id(batch: &Hash, kind: &str, target: &BlockId) -> String {
    let mut bytes = b"rdy-consequence\0".to_vec();
    bytes.extend_from_slice(batch.to_string().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(target.to_string().as_bytes());
    format!("cq-{}", &Hash::of(&bytes).to_string()[..16])
}

/// The first cause found for a dependent block.
struct Cause {
    suspect: bool,
    edge: String,
    changed: BlockId,
}

/// A premise edge: one that can make its source suspect.
fn premise(e: &Edge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => SUSPECT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

/// An edge impact follows.
fn impact(e: &Edge) -> bool {
    match &e.kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => IMPACT_LINKS.contains(&n.as_str()),
        EdgeKind::Contains => false,
    }
}

impl ReadState {
    /// The consequences of `change` in this state (the one the write published): for each block
    /// the change touched, each block outside the change that points at it. Suspect entries come
    /// first, then dependent ones, each by block id; the list is cut from the end to
    /// `max_tokens` (default `BRIEF_DEFAULT_TOKENS`), counting what it drops in `omitted`.
    pub fn consequences(
        &self,
        scope: &Scope,
        change: &Hash,
        max_tokens: Option<usize>,
    ) -> Result<Consequences, RdyError> {
        let ch = self.reader().change(change)?;
        let touched: BTreeSet<BlockId> = ch.ops.iter().map(|o| o.block_id().clone()).collect();
        // a created decision's supersedes / revokes targets changed status
        let mut changed = touched.clone();
        let mut only_creates = true;
        for op in &ch.ops {
            match op {
                Op::Create { block } => {
                    if block.block_type == DECISION {
                        for name in [SUPERSEDES, REVOKES] {
                            for l in block.links.get(name).into_iter().flatten() {
                                changed.insert(l.target.clone());
                            }
                        }
                    }
                }
                _ => only_creates = false,
            }
        }
        // nothing can point at a block that did not exist: no graph to read
        if only_creates && changed == touched {
            return Ok(Consequences::default());
        }

        let g = self.graph(scope)?;
        let m = self.materialized(scope)?;
        let old = |h: &Hash| self.reader().get_block(h).ok();
        let mut suspects_of: BTreeMap<BlockId, Vec<Suspect>> = BTreeMap::new();
        let mut causes: BTreeMap<BlockId, Cause> = BTreeMap::new();
        for x in &changed {
            let mut by_from: BTreeMap<&BlockId, Vec<&Edge>> = BTreeMap::new();
            for e in g.incoming(x) {
                if e.kind != EdgeKind::Contains && !touched.contains(&e.from) {
                    by_from.entry(&e.from).or_default().push(e);
                }
            }
            for (from, mut edges) in by_from {
                if m.get(from).is_none() {
                    continue;
                }
                edges.sort_by_key(|e| e.kind.name());
                let found = suspects_of.entry(from.clone()).or_insert_with(|| {
                    let mut s = g.suspects_of(from, &*m, &old);
                    s.retain(|s| !self.is_presentational_source(&m, s));
                    s
                });
                let suspect = found.iter().any(|s| s.target == *x);
                let pick = if suspect {
                    edges.iter().find(|e| premise(e)).or_else(|| edges.first())
                } else {
                    edges.iter().find(|e| impact(e))
                };
                let Some(e) = pick else { continue };
                let better = causes.get(from).is_none_or(|c| suspect && !c.suspect);
                if better {
                    causes.insert(
                        from.clone(),
                        Cause {
                            suspect,
                            edge: e.kind.name(),
                            changed: x.clone(),
                        },
                    );
                }
            }
        }

        let mut suspect_items = vec![];
        let mut dependent_items = vec![];
        for (block, cause) in causes {
            let Some(src) = m.get(&block) else { continue };
            let deleted = m.get(&cause.changed).is_none();
            // no entry for a decision that asserts nothing live
            let Some(follow) = follow_up(self.pack(), src, g.status(&block).as_ref(), deleted)
            else {
                continue;
            };
            let kind = if cause.suspect {
                ConsequenceKind::Suspect
            } else {
                ConsequenceKind::Dependent
            };
            let item = Consequence {
                id: consequence_id(change, kind.as_str(), &block),
                block: block.to_string(),
                kind,
                because: Because {
                    edge: cause.edge,
                    changed: cause.changed.to_string(),
                },
                follow_up: follow,
                fix: (follow == FollowUp::Reaffirm).then(|| REAFFIRM_FIX.to_string()),
            };
            match kind {
                ConsequenceKind::Suspect => suspect_items.push(item),
                ConsequenceKind::Dependent => dependent_items.push(item),
            }
        }
        suspect_items.extend(dependent_items);
        let mut out = Consequences {
            items: suspect_items,
            omitted: BTreeMap::new(),
        };

        let budget = max_tokens.unwrap_or(BRIEF_DEFAULT_TOKENS).saturating_mul(4);
        while !out.items.is_empty() && serde_json::to_string(&out).map_or(0, |s| s.len()) > budget {
            let drop = out.items.len().div_ceil(2);
            for dropped in out.items.split_off(out.items.len() - drop) {
                *out.omitted
                    .entry(dropped.kind.as_str().to_string())
                    .or_default() += 1;
            }
        }
        Ok(out)
    }
}

/// `write_typed`, then the write's consequences read from the state it published: on success
/// `consequences` is always present (with `consequences_omitted` when a cut dropped some, or
/// `consequences_error` when they could not be read; the write stands); on a briefing refusal
/// the error carries a fresh briefing for exactly the blocks the write touches.
pub fn write_and_report<T: Serialize + 'static>(
    engine: &dyn Engine,
    view: &str,
    max_tokens: Option<usize>,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<serde_json::Value, RdyError> {
    let mut v = match write_typed(engine, f) {
        Ok(v) => v,
        Err(e) => {
            return Err(expand_error(
                e,
                &engine.read(),
                view,
                &engine.briefs(),
                max_tokens,
            ));
        }
    };
    let change = v
        .get("change")
        .and_then(|c| c.as_str())
        .and_then(|c| c.parse::<Hash>().ok());
    match change {
        Some(h) => match engine
            .read()
            .consequences(&Scope::View(view.into()), &h, max_tokens)
        {
            Ok(c) => {
                v["consequences"] = serde_json::to_value(&c.items).unwrap_or_default();
                if !c.omitted.is_empty() {
                    v["consequences_omitted"] =
                        serde_json::to_value(&c.omitted).unwrap_or_default();
                }
            }
            Err(e) => {
                v["consequences"] = serde_json::json!([]);
                v["consequences_error"] = format!("{}: {}", e.code, e.message).into();
            }
        },
        None => v["consequences"] = serde_json::json!([]),
    }
    Ok(v)
}
