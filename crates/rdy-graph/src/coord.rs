//! cgr1: the coordination graph. Its nodes are blocks, changes, proposals, alternative sets,
//! intents, actors, runs and areas; its edges say who made what, what a change writes and
//! depends on, and what an intent or a decision covers. Blocks link to each other in `Graph`.
use imbl::OrdMap;
use rdy_model::{BlockId, Hash};
use serde::Serialize;

/// A hash as its 64 hex digits, as every surface shows one.
fn hex<S: serde::Serializer>(h: &Hash, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(h)
}

/// A node of the coordination graph. Hashes serialize as hex.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "node", content = "id", rename_all = "snake_case")]
pub enum Node {
    Block(BlockId),
    Change(#[serde(serialize_with = "hex")] Hash),
    /// A proposal version.
    Proposal(#[serde(serialize_with = "hex")] Hash),
    /// An alternative set, by its stable id.
    AltSet(#[serde(serialize_with = "hex")] Hash),
    /// An intent, by its stable id.
    Intent(#[serde(serialize_with = "hex")] Hash),
    Actor(String),
    /// An actor's run: (actor, run).
    Run(String, String),
    Area(String),
}

/// The kind of a coordination edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordKind {
    /// A block, to a change that writes it.
    AuthoredIn,
    /// A change, to a change it depends on.
    DependsOn,
    /// A change or a proposal, to the alternative set it is a member's work in.
    AlternativeOf,
    /// A decision, to an area it is in.
    Affects,
    /// A change, to a proposal version that holds it.
    ProposedIn,
    /// A change, proposal or intent, to the run (or the actor, without a run) that made it.
    MadeBy,
    /// A run, to its actor.
    RunOf,
    /// An intent, to a block or an area it covers.
    Covers,
    /// A change, to the intent it was written under.
    UnderIntent,
}

/// One coordination edge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CoordEdge {
    pub from: Node,
    pub kind: CoordKind,
    pub to: Node,
}

/// The coordination graph: each node's outgoing and incoming edges, each list ordered by
/// `(kind, other end)`. A repeated edge is kept once. Persistent maps, so a clone is cheap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coord {
    out: OrdMap<Node, Vec<CoordEdge>>,
    inc: OrdMap<Node, Vec<CoordEdge>>,
    count: usize,
}

impl Coord {
    /// Adds the edge `from -kind-> to`, unless the graph has it already.
    pub fn add(&mut self, from: Node, kind: CoordKind, to: Node) {
        let out = self.out.entry(from.clone()).or_default();
        let at = match out.binary_search_by(|e| (e.kind, &e.to).cmp(&(kind, &to))) {
            Ok(_) => return,
            Err(at) => at,
        };
        let edge = CoordEdge { from, kind, to };
        out.insert(at, edge.clone());
        let inc = self.inc.entry(edge.to.clone()).or_default();
        let at = inc
            .binary_search_by(|e| (e.kind, &e.from).cmp(&(kind, &edge.from)))
            .unwrap_or_else(|at| at);
        inc.insert(at, edge);
        self.count += 1;
    }

    /// The edges from `n`, by `(kind, to)`.
    pub fn outgoing(&self, n: &Node) -> &[CoordEdge] {
        self.out.get(n).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The edges to `n`, by `(kind, from)`.
    pub fn incoming(&self, n: &Node) -> &[CoordEdge] {
        self.inc.get(n).map(Vec::as_slice).unwrap_or(&[])
    }

    /// How many distinct edges the graph has.
    pub fn edge_count(&self) -> usize {
        self.count
    }
}
