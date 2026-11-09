//! Suspects across projects (repo links, D-Y49PX): the derived graph's foreign nodes, keyed by
//! repo and block id, and the suspects that a block's edges to them make, by today's rules.
//!
//! - A node is the current delivered version of a cited block: its content identity and
//!   encoding, and the derived facts the cited side stated (status, superseded by).
//! - A block's foreign edges are its stored-form references `{{<repo>:<id>}}` and its foreign
//!   links, each with the foreign pin the block holds for its target.
//! - A reference, or a link in [`FOREIGN_SUSPECT_LINKS`], whose pin differs from the node's
//!   content (or encoding) is `changed`; a pinned target that no node holds is `deleted`.
//! - A superseded or revoked node makes its citer suspect through any edge but the citer's own
//!   `supersedes` or `revokes` link. A superseded node names a successor only when the selection
//!   holds one.
//! - One suspect per (block, target), the strongest reason kept, as for local suspects.
//!
//! The local graph never reads a foreign edge, so local suspects and their identities stay as
//! they are.
use crate::{EdgeKind, SUSPECT_LINKS, rank};
use rdy_model::decision;
use rdy_model::foreign::{ForeignId, ForeignPin, ForeignVersion};
use rdy_model::reference::{ForeignRef, foreign_ref};
use rdy_model::{Block, BlockId, Hash, RepoId};
use serde::Serialize;
use std::collections::BTreeMap;

/// The engine link kind from a decision to the cited block it departs from (D-8VJAZ). It is a
/// foreign link only.
pub const DEVIATES_FROM: &str = "deviates_from";

/// The link kinds through which a changed or deleted cited target makes a block suspect: the
/// local suspect links, in their order, then `deviates_from`.
pub const FOREIGN_SUSPECT_LINKS: [&str; 6] = [
    SUSPECT_LINKS[0],
    SUSPECT_LINKS[1],
    SUSPECT_LINKS[2],
    SUSPECT_LINKS[3],
    SUSPECT_LINKS[4],
    DEVIATES_FROM,
];

/// Why a cited block is retired, from the derived facts of its current version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Retired {
    Superseded,
    Revoked,
}

impl Retired {
    /// The suspect reason it gives.
    pub fn reason(self) -> &'static str {
        match self {
            Retired::Superseded => "superseded",
            Retired::Revoked => "revoked",
        }
    }
}

/// One foreign node: the current delivered version of a cited block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignNode {
    /// The content identity a pin is checked against, and its encoding.
    pub content: Hash,
    pub encoding: u16,
    /// Superseded or revoked, by the cited side's derived facts.
    pub retired: Option<Retired>,
    /// The first block (by id) that supersedes it and that the selection holds.
    pub successor: Option<BlockId>,
}

/// The foreign nodes of one state, by repo and block id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignNodes {
    nodes: BTreeMap<ForeignId, ForeignNode>,
}

impl ForeignNodes {
    /// Adds a node for each version of `selection`, the current delivered view of `repo`. A
    /// successor is offered only when `selection` holds it.
    pub fn add_selection<'a>(
        &mut self,
        repo: RepoId,
        selection: impl IntoIterator<Item = &'a ForeignVersion>,
    ) {
        let selection: Vec<&ForeignVersion> = selection.into_iter().collect();
        let held: std::collections::BTreeSet<&BlockId> = selection.iter().map(|v| &v.id).collect();
        for v in selection {
            let status = v.derived.status.as_deref();
            let retired = if status == Some("revoked") {
                Some(Retired::Revoked)
            } else if status == Some("superseded") || !v.derived.superseded_by.is_empty() {
                Some(Retired::Superseded)
            } else {
                None
            };
            let successor = v
                .derived
                .superseded_by
                .iter()
                .filter(|s| **s != v.id && held.contains(s))
                .min()
                .cloned();
            self.nodes.insert(
                ForeignId::new(repo, v.id.clone()),
                ForeignNode {
                    content: v.content,
                    encoding: v.encoding,
                    retired,
                    successor,
                },
            );
        }
    }

    /// The node of `id`, if a delivery selects it.
    pub fn get(&self, id: &ForeignId) -> Option<&ForeignNode> {
        self.nodes.get(id)
    }

    /// Whether no node exists.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// An edge from a local block to a cited block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignEdge {
    pub from: BlockId,
    pub to: ForeignId,
    /// `References` or `Link(<kind>)`; never `Contains`.
    pub kind: EdgeKind,
    pub part: Option<String>,
    pub pin: Option<ForeignPin>,
}

/// The foreign edges `b`'s own content makes, in the order [`crate::edges_of`] lists local
/// ones: one `References` edge per stored-form foreign reference in token order, then its
/// foreign links in map order; each with the pin `b` holds for its target.
pub fn foreign_edges_of(b: &Block) -> Vec<ForeignEdge> {
    let mut edges = vec![];
    for tok in b.ref_tokens() {
        if let Some(ForeignRef::Stored { repo, id }) = foreign_ref(&tok) {
            let to = ForeignId::new(repo, id);
            edges.push(ForeignEdge {
                from: b.id.clone(),
                pin: b.foreign.pins.get(&to).cloned(),
                to,
                kind: EdgeKind::References,
                part: None,
            });
        }
    }
    for (name, links) in &b.foreign.links {
        for l in links {
            edges.push(ForeignEdge {
                from: b.id.clone(),
                to: l.target.clone(),
                kind: EdgeKind::Link(name.clone()),
                part: l.part.clone(),
                pin: b.foreign.pins.get(&l.target).cloned(),
            });
        }
    }
    edges
}

/// One suspect on a cited target. `successor` is set only for a superseded target whose
/// superseder the selection holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForeignSuspect {
    pub block: BlockId,
    pub target: ForeignId,
    pub reason: String,
    pub successor: Option<BlockId>,
}

/// Whether `k` is a local decision's own `supersedes` or `revokes` link: the cause, never a
/// suspect.
fn retiring(k: &EdgeKind) -> bool {
    matches!(k, EdgeKind::Link(n) if n == decision::SUPERSEDES || n == decision::REVOKES)
}

/// The suspect edge `e` (of `edges`, the citer's foreign edges) makes, by the rules of
/// [`crate::Graph::edge_suspect`] read against `nodes`.
fn edge_suspect(
    e: &ForeignEdge,
    edges: &[ForeignEdge],
    nodes: &ForeignNodes,
) -> Option<ForeignSuspect> {
    if retiring(&e.kind) {
        return None;
    }
    // a decision naming, in its text, a block it supersedes or revokes is expected
    if e.kind == EdgeKind::References && edges.iter().any(|o| o.to == e.to && retiring(&o.kind)) {
        return None;
    }
    let node = nodes.get(&e.to);
    let (reason, successor) = match node.and_then(|n| n.retired) {
        Some(r) => (
            r.reason(),
            node.filter(|_| r == Retired::Superseded)
                .and_then(|n| n.successor.clone()),
        ),
        None => {
            let premise = match &e.kind {
                EdgeKind::References => true,
                EdgeKind::Link(n) => FOREIGN_SUSPECT_LINKS.contains(&n.as_str()),
                EdgeKind::Contains => false,
            };
            if !premise {
                return None;
            }
            match (&e.pin, node) {
                (Some(_), None) => ("deleted", None),
                (Some(p), Some(n)) if (p.content, p.encoding) != (n.content, n.encoding) => {
                    ("changed", None)
                }
                _ => return None,
            }
        }
    };
    Some(ForeignSuspect {
        block: e.from.clone(),
        target: e.to.clone(),
        reason: reason.into(),
        successor,
    })
}

/// The foreign suspects whose block is `b`, against `nodes`: one per cited target, with the
/// strongest reason (revoked, superseded, deleted, changed), sorted by target.
pub fn suspects_of(b: &Block, nodes: &ForeignNodes) -> Vec<ForeignSuspect> {
    if b.foreign.is_empty() || b.is_redacted() {
        return vec![];
    }
    let edges = foreign_edges_of(b);
    let mut best: BTreeMap<ForeignId, ForeignSuspect> = BTreeMap::new();
    for s in edges.iter().filter_map(|e| edge_suspect(e, &edges, nodes)) {
        match best.get(&s.target) {
            Some(have) if rank(&have.reason) >= rank(&s.reason) => {}
            _ => {
                best.insert(s.target.clone(), s);
            }
        }
    }
    best.into_values().collect()
}
