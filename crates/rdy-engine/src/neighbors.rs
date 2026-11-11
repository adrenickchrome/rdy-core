//! nb2 (D-YXBXH): a block's neighbors. Its section neighbors are the other content blocks with
//! the same direct parent; its entity neighbors are the live content blocks tied to an entity it
//! is tied to, by a reference, `describes` or `affects`. Only content blocks take part: a
//! supporting block (decision, question, rationale, finding, source, or a pack's own supporting
//! type) never does.
//!
//! The query reads local adjacencies only: each source's parent's children, its outgoing edges
//! and each tied entity's incoming edges, each walked once per call and reused; ids stay borrowed
//! until a row is shown. It stores nothing and adds no graph edge. Every bound is a work guard, never a relevance cap: a query
//! past one returns `E_LIMIT` and no partial list.
use crate::lexicon::{is_content, is_tie};
use crate::query::summary_in;
use crate::tensions::index::is_deprecated;
use crate::{E_LIMIT, Materialized, RdyError};
use rdy_graph::Graph;
use rdy_model::{Block, BlockId};
use rdy_schema::{Pack, Trait, TypeClass};
use std::collections::{BTreeMap, BTreeSet};

/// The most section neighbors one source may have.
pub const MAX_SECTION_NEIGHBORS: usize = 100_000;
/// The most eligible tied content blocks one entity may have.
pub const MAX_ENTITY_TIED: usize = 100_000;
/// The most edge and member visits one call may make.
pub const MAX_NEIGHBOR_VISITS: usize = 1_000_000;
/// The most distinct neighbor rows one call may return.
pub const MAX_NEIGHBOR_ROWS: usize = 100_000;

const WALK_FIX: &str = "brief fewer blocks, or split the change into smaller proposals";

/// How a neighbor shares something with its sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeighborVia {
    Section,
    Entity,
}

/// One neighbor row: `block` is a neighbor of each block in `of` (sorted), by `via`; `entity` is
/// the shared entity of an entity row. `doc` is the neighbor's containing doc.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Neighbor {
    pub block: String,
    pub of: Vec<String>,
    pub via: NeighborVia,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    pub doc: String,
    pub summary: String,
}

/// Whether `b` takes part in neighbors: it is content ([`is_content`]) and its type is not of
/// the supporting class.
pub(crate) fn takes_part(pack: &Pack, b: &Block) -> bool {
    is_content(pack, b) && pack.class_of(&b.block_type) != TypeClass::Supporting
}

/// The neighbors of the blocks in `covered`, merged and sorted (module doc). A covered block
/// that does not exist, has no parent or containing doc, or does not take part gives no rows. A
/// covered block can be a neighbor of another covered block; a block is never its own neighbor.
pub fn neighbors_in(
    m: &Materialized,
    g: &Graph,
    pack: &Pack,
    covered: &BTreeSet<BlockId>,
) -> Result<Vec<Neighbor>, RdyError> {
    Walk::new(m, g, pack, Limits::FIXED)
        .run(covered)
        .map(|(rows, _)| rows)
}

/// The work bounds of one call; [`Limits::FIXED`] in production.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub section: usize,
    pub entity: usize,
    pub visits: usize,
    pub rows: usize,
}

impl Limits {
    pub(crate) const FIXED: Limits = Limits {
        section: MAX_SECTION_NEIGHBORS,
        entity: MAX_ENTITY_TIED,
        visits: MAX_NEIGHBOR_VISITS,
        rows: MAX_NEIGHBOR_ROWS,
    };

    fn section_error(&self, source: &BlockId) -> RdyError {
        RdyError::new(
            E_LIMIT,
            format!(
                "section neighbors of {source} exceed {} blocks",
                self.section
            ),
            format!("split the parent of {source} into smaller sections"),
        )
    }

    fn entity_error(&self, entity: &BlockId) -> RdyError {
        RdyError::new(
            E_LIMIT,
            format!(
                "entity {entity} exceeds {} tied content blocks",
                self.entity
            ),
            format!("split {entity} into narrower entities"),
        )
    }

    fn walk_error(&self) -> RdyError {
        RdyError::new(
            E_LIMIT,
            format!("neighbor walk exceeds {} visits", self.visits),
            WALK_FIX,
        )
    }

    fn rows_error(&self) -> RdyError {
        RdyError::new(
            E_LIMIT,
            format!("neighbor result exceeds {} rows", self.rows),
            WALK_FIX,
        )
    }
}

/// One row while a walk collects it: `of` holds its sources in source order, each once.
struct Row<'a> {
    block: &'a BlockId,
    via: NeighborVia,
    entity: Option<&'a BlockId>,
    of: Vec<&'a BlockId>,
}

/// A parent's participating children, in child order, each with the index of its section row once
/// it has one. A block has one parent, so its section row is found here, with no key search.
#[derive(Default)]
struct ParentList<'a> {
    children: Vec<(&'a BlockId, Option<usize>)>,
}

/// One call's walk: the bounds, the visit count and the adjacency lists read so far.
pub(crate) struct Walk<'a> {
    m: &'a Materialized,
    g: &'a Graph,
    pack: &'a Pack,
    limits: Limits,
    visits: usize,
    /// Each parent's participating children.
    parents: BTreeMap<&'a BlockId, ParentList<'a>>,
    /// Each entity's eligible tied content blocks, in id order.
    entities: BTreeMap<&'a BlockId, Vec<&'a BlockId>>,
    rows: Vec<Row<'a>>,
    /// How many sources the walk reads, the most a row can list.
    /// The index in `rows` of each entity row, by (block, entity).
    entity_rows: BTreeMap<(&'a BlockId, &'a BlockId), usize>,
}

/// A new row in `rows`, refused past the row bound; its index.
fn new_row<'a>(
    rows: &mut Vec<Row<'a>>,
    limits: &Limits,
    block: &'a BlockId,
    via: NeighborVia,
    entity: Option<&'a BlockId>,
) -> Result<usize, RdyError> {
    if rows.len() >= limits.rows {
        return Err(limits.rows_error());
    }
    rows.push(Row {
        block,
        via,
        entity,
        of: Vec::new(),
    });
    Ok(rows.len() - 1)
}

/// A row of [`neighbor_links`]: the neighbor, how it is one, and its sources.
pub(crate) struct NeighborLink<'a> {
    pub block: &'a BlockId,
    pub via: NeighborVia,
    pub of: Vec<&'a BlockId>,
}

/// The rows [`neighbors_in`] gives for `covered`, for a caller that reads only who neighbors whom:
/// in no set order, with no summary, doc or path worked out. The same bounds refuse.
pub(crate) fn neighbor_links<'a>(
    m: &'a Materialized,
    g: &'a Graph,
    pack: &'a Pack,
    covered: &BTreeSet<BlockId>,
) -> Result<Vec<NeighborLink<'a>>, RdyError> {
    let mut walk = Walk::new(m, g, pack, Limits::FIXED);
    walk.collect(covered)?;
    Ok(walk
        .rows
        .into_iter()
        .map(|r| NeighborLink {
            block: r.block,
            via: r.via,
            of: r.of,
        })
        .collect())
}

impl<'a> Walk<'a> {
    pub(crate) fn new(m: &'a Materialized, g: &'a Graph, pack: &'a Pack, limits: Limits) -> Self {
        Walk {
            m,
            g,
            pack,
            limits,
            visits: 0,
            parents: BTreeMap::new(),
            entities: BTreeMap::new(),
            rows: vec![],
            entity_rows: BTreeMap::new(),
        }
    }

    /// The rows and the number of visits the walk made.
    pub(crate) fn run(
        mut self,
        covered: &BTreeSet<BlockId>,
    ) -> Result<(Vec<Neighbor>, usize), RdyError> {
        self.collect(covered)?;
        let visits = self.visits;
        Ok((self.finish(), visits))
    }

    /// Walks each source of `covered`, in id order, so each row's `of` is sorted.
    fn collect(&mut self, covered: &BTreeSet<BlockId>) -> Result<(), RdyError> {
        for id in covered {
            self.source(id)?;
        }
        Ok(())
    }

    /// Counts `n` visits; refuses before the count passes the bound.
    fn visit(&mut self, n: usize) -> Result<(), RdyError> {
        match self.visits.checked_add(n) {
            Some(v) if v <= self.limits.visits => {
                self.visits = v;
                Ok(())
            }
            _ => Err(self.limits.walk_error()),
        }
    }

    fn source(&mut self, id: &BlockId) -> Result<(), RdyError> {
        let m = self.m;
        let Some(src) = m.get(id) else {
            return Ok(());
        };
        let Some(parent) = src.parent.as_ref() else {
            return Ok(());
        };
        if m.doc_of(id).is_none() || !takes_part(self.pack, src) {
            return Ok(());
        }
        let id = &src.id;

        // section: the direct siblings that take part, self excluded
        self.read_parent(parent, id)?;
        let used = self.parents[parent].children.len();
        self.visit(used)?;

        // entity: each tied entity in id order, so the least entity wins a pair
        let g = self.g;
        let out = g.outgoing(id);
        self.visit(out.len())?;
        let targets: BTreeSet<&'a BlockId> = out
            .iter()
            .filter(|e| is_tie(&e.kind))
            .filter(|e| m.get(&e.to).is_some_and(|t| self.defines(t)))
            .map(|e| &e.to)
            .collect();
        let mut by_entity: BTreeMap<&'a BlockId, &'a BlockId> = BTreeMap::new();
        for entity in targets {
            self.read_entity(entity)?;
            let used = self.entities[entity].len();
            self.visit(used)?;
            for &n in &self.entities[entity] {
                // every entity neighbor takes part, so a section neighbor is one with this parent
                let sibling = m.get(n).is_some_and(|b| b.parent.as_ref() == Some(parent));
                if n != id && !sibling {
                    by_entity.entry(n).or_insert(entity);
                }
            }
        }

        // a section row is found through the parent's list
        let rows = &mut self.rows;
        let list = self.parents.get_mut(parent).expect("read above");
        for (n, row) in &mut list.children {
            if *n == id {
                continue;
            }
            let index = match *row {
                Some(index) => index,
                None => {
                    let index = new_row(rows, &self.limits, n, NeighborVia::Section, None)?;
                    *row = Some(index);
                    index
                }
            };
            rows[index].of.push(id);
        }
        for (n, entity) in by_entity {
            let index = match self.entity_rows.get(&(n, entity)) {
                Some(index) => *index,
                None => {
                    let via = NeighborVia::Entity;
                    let index = new_row(rows, &self.limits, n, via, Some(entity))?;
                    self.entity_rows.insert((n, entity), index);
                    index
                }
            };
            rows[index].of.push(id);
        }
        Ok(())
    }

    fn defines(&self, b: &Block) -> bool {
        self.pack
            .block_types
            .get(&b.block_type)
            .is_some_and(|t| t.traits.contains(&Trait::Defines))
    }

    /// Reads `parent`'s participating children once per call. `source` is one of them, so a
    /// list longer than the section bound plus one refuses for `source`.
    fn read_parent(&mut self, parent: &'a BlockId, source: &BlockId) -> Result<(), RdyError> {
        if self.parents.contains_key(parent) {
            return Ok(());
        }
        let m = self.m;
        let mut list = ParentList::default();
        for c in m.children(Some(parent)) {
            self.visit(1)?;
            if !takes_part(self.pack, c) {
                continue;
            }
            if list.children.len() > self.limits.section {
                return Err(self.limits.section_error(source));
            }
            list.children.push((&c.id, None));
        }
        self.parents.insert(parent, list);
        Ok(())
    }

    /// Reads the eligible content blocks tied to `entity` once per call: existing, taking part,
    /// not deprecated, with a containing doc; each once however many ties it has.
    fn read_entity(&mut self, entity: &'a BlockId) -> Result<(), RdyError> {
        if self.entities.contains_key(entity) {
            return Ok(());
        }
        let (m, g) = (self.m, self.g);
        let mut set: BTreeSet<&'a BlockId> = BTreeSet::new();
        for e in g.incoming(entity) {
            self.visit(1)?;
            if !is_tie(&e.kind) || set.contains(&e.from) {
                continue;
            }
            let Some(b) = m.get(&e.from) else {
                continue;
            };
            if !takes_part(self.pack, b) || is_deprecated(m, g, &b.id) || m.doc_of(&b.id).is_none()
            {
                continue;
            }
            if set.len() >= self.limits.entity {
                return Err(self.limits.entity_error(entity));
            }
            set.insert(&b.id);
        }
        self.entities.insert(entity, set.into_iter().collect());
        Ok(())
    }

    /// The rows in order: a neighbor that shares a doc with one of its sources first, then by
    /// the neighbor's doc path (none sorts as empty), block id, via and entity id.
    fn finish(self) -> Vec<Neighbor> {
        let m = self.m;
        let mut docs: BTreeMap<&BlockId, Option<&BlockId>> = BTreeMap::new();
        let mut doc_of = |id: &'a BlockId| -> Option<&'a BlockId> {
            *docs
                .entry(id)
                .or_insert_with(|| m.doc_of(id).map(|d| &d.id))
        };
        let mut paths: BTreeMap<&BlockId, std::rc::Rc<str>> = BTreeMap::new();
        type Key<'k> = (
            bool,
            std::rc::Rc<str>,
            &'k BlockId,
            NeighborVia,
            Option<&'k BlockId>,
        );
        let mut keyed: Vec<(Key<'a>, Neighbor)> = Vec::with_capacity(self.rows.len());
        for row in self.rows {
            let Some(b) = m.get(row.block) else { continue };
            let doc = doc_of(row.block);
            let same = doc.is_some() && row.of.iter().any(|s| doc_of(s) == doc);
            let path = doc
                .map(|d| {
                    std::rc::Rc::clone(
                        paths
                            .entry(d)
                            .or_insert_with(|| m.path_of(d).unwrap_or_default().into()),
                    )
                })
                .unwrap_or_else(|| "".into());
            let shown = Neighbor {
                block: row.block.as_str().to_owned(),
                of: row.of.iter().map(|s| s.as_str().to_owned()).collect(),
                via: row.via,
                entity: row.entity.map(|e| e.as_str().to_owned()),
                doc: doc.map(|d| d.as_str().to_owned()).unwrap_or_default(),
                summary: summary_in(b, m),
            };
            keyed.push(((!same, path, row.block, row.via, row.entity), shown));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        keyed.into_iter().map(|(_, row)| row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::{CROCKFORD, StateSnapshot};
    use rdy_schema::PackSet;

    fn pack() -> Pack {
        PackSet::with_builtin().resolve("rdy-standard").unwrap()
    }

    /// The `n`th id with `prefix`, in id order for a fixed prefix.
    fn nid(prefix: &str, n: usize) -> BlockId {
        let mut body = [b'0'; 5];
        let mut v = n;
        for d in body.iter_mut().rev() {
            *d = CROCKFORD[v % 32];
            v /= 32;
        }
        BlockId::parse(&format!("{prefix}-{}", std::str::from_utf8(&body).unwrap())).unwrap()
    }

    fn blk(id: &BlockId, ty: &str, parent: Option<&BlockId>, body: &str) -> Block {
        let mut fields = BTreeMap::new();
        if ty == "doc" {
            fields.insert(
                "name".to_string(),
                rdy_model::FieldValue::Str(id.as_str().to_lowercase()),
            );
        }
        Block {
            id: id.clone(),
            block_type: ty.to_string(),
            parent: parent.cloned(),
            order: "a1".to_string(),
            fields,
            body: body.to_string(),
            links: Default::default(),
            pins: Default::default(),
            foreign: Default::default(),
            read_identity: None,
        }
    }

    fn scope(blocks: Vec<Block>) -> (Materialized, Graph) {
        let g = Graph::build_from(blocks.iter());
        let snap = StateSnapshot {
            blocks: blocks
                .iter()
                .map(|b| (b.id.clone(), b.version_hash()))
                .collect(),
        };
        let m = Materialized::build(
            snap,
            blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
        );
        (m, g)
    }

    /// A doc with section `s-00000` holding `claims` claims (`c-00000` first) and one entity
    /// `en-00000`; claims `c-00000..tied` reference the entity.
    fn local(claims: usize, tied: usize) -> Vec<Block> {
        let (d, s, en) = (nid("d", 0), nid("s", 0), nid("en", 0));
        let mut v = vec![
            blk(&d, "doc", None, ""),
            blk(&s, "section", Some(&d), ""),
            blk(&en, "entity", Some(&s), ""),
        ];
        v[2].fields.insert(
            "name".to_string(),
            rdy_model::FieldValue::Str("Ledger".into()),
        );
        for i in 0..claims {
            let body = if i < tied {
                format!("Claim {i} uses {{{{{en}}}}}.")
            } else {
                format!("Claim {i}.")
            };
            v.push(blk(&nid("c", i), "claim", Some(&s), &body));
        }
        v
    }

    fn wide(limit: usize) -> Limits {
        Limits {
            section: limit,
            entity: limit,
            visits: limit * 1_000,
            rows: limit,
        }
    }

    fn covered(ids: &[BlockId]) -> BTreeSet<BlockId> {
        ids.iter().cloned().collect()
    }

    #[test]
    fn nb2_each_neighbor_bound_passes_at_limit_and_refuses_one_past() {
        let p = pack();
        let src = nid("c", 0);

        // section: 3 siblings pass at a section bound of 3; a 4th refuses for the source, and
        // the parent's list is never kept
        let (m, g) = scope(local(4, 0));
        let lim = Limits {
            section: 3,
            ..wide(100)
        };
        let (rows, _) = Walk::new(&m, &g, &p, lim)
            .run(&covered(std::slice::from_ref(&src)))
            .unwrap();
        assert_eq!(rows.len(), 3);
        let (m, g) = scope(local(5, 0));
        let mut w = Walk::new(&m, &g, &p, lim);
        let e = w.source(&src).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "section neighbors of c-00000 exceed 3 blocks",
                "split the parent of c-00000 into smaller sections"
            )
        );
        assert!(w.parents.is_empty() && w.rows.is_empty(), "no partial list");

        // entity: 3 tied content blocks pass at an entity bound of 3; a 4th refuses. Each tied
        // claim sits in its own section so section neighbors stay out of the count.
        let tied_apart = |n: usize| {
            let (d, en) = (nid("d", 0), nid("en", 0));
            let mut v = vec![blk(&d, "doc", None, ""), blk(&en, "entity", Some(&d), "")];
            for i in 0..n {
                let s = nid("s", i);
                v.push(blk(&s, "section", Some(&d), ""));
                v.push(blk(
                    &nid("c", i),
                    "claim",
                    Some(&s),
                    &format!("Uses {{{{{en}}}}}."),
                ));
            }
            v
        };
        let lim = Limits {
            entity: 3,
            ..wide(100)
        };
        let (m, g) = scope(tied_apart(3));
        let (rows, _) = Walk::new(&m, &g, &p, lim)
            .run(&covered(std::slice::from_ref(&src)))
            .unwrap();
        assert_eq!(rows.len(), 2, "the source and two entity neighbors");
        let (m, g) = scope(tied_apart(4));
        let mut w = Walk::new(&m, &g, &p, lim);
        let e = w.source(&src).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "entity en-00000 exceeds 3 tied content blocks",
                "split en-00000 into narrower entities"
            )
        );
        assert!(
            w.entities.is_empty() && w.rows.is_empty(),
            "no partial list"
        );

        // visits: the exact count passes; one fewer allowed visit refuses
        let (m, g) = scope(local(6, 3));
        let all = covered(&[nid("c", 0), nid("c", 4)]);
        let (_, visits) = Walk::new(&m, &g, &p, wide(100)).run(&all).unwrap();
        let lim = Limits {
            visits,
            ..wide(100)
        };
        assert!(Walk::new(&m, &g, &p, lim).run(&all).is_ok());
        let lim = Limits {
            visits: visits - 1,
            ..wide(100)
        };
        let e = Walk::new(&m, &g, &p, lim).run(&all).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                format!("neighbor walk exceeds {} visits", visits - 1).as_str(),
                "brief fewer blocks, or split the change into smaller proposals"
            )
        );

        // rows: 3 distinct rows pass at a row bound of 3; a 4th refuses with exactly 3 kept
        let lim = Limits {
            rows: 3,
            ..wide(100)
        };
        let (m, g) = scope(local(4, 0));
        let (rows, _) = Walk::new(&m, &g, &p, lim)
            .run(&covered(std::slice::from_ref(&src)))
            .unwrap();
        assert_eq!(rows.len(), 3);
        let (m, g) = scope(local(5, 0));
        let mut w = Walk::new(&m, &g, &p, lim);
        let e = w.source(&src).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "neighbor result exceeds 3 rows",
                "brief fewer blocks, or split the change into smaller proposals"
            )
        );
        assert_eq!(
            w.rows.len(),
            3,
            "the row that would pass the bound is never added"
        );

        // the fixed bounds and their exact texts
        let f = Limits::FIXED;
        assert_eq!(
            (f.section, f.entity, f.visits, f.rows),
            (100_000, 100_000, 1_000_000, 100_000)
        );
        let en = nid("en", 0);
        let e = f.entity_error(&en);
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "entity en-00000 exceeds 100000 tied content blocks",
                "split en-00000 into narrower entities"
            )
        );
        let e = f.walk_error();
        assert_eq!(
            (e.message.as_str(), e.fix.as_str()),
            (
                "neighbor walk exceeds 1000000 visits",
                "brief fewer blocks, or split the change into smaller proposals"
            )
        );
        let e = f.rows_error();
        assert_eq!(
            (e.message.as_str(), e.fix.as_str()),
            (
                "neighbor result exceeds 100000 rows",
                "brief fewer blocks, or split the change into smaller proposals"
            )
        );

        // a 100,000-row construction at the fixed bounds: one source with 100,000 siblings
        // passes with every row; one more sibling refuses with the exact section text
        let (m, g) = scope(local(100_001, 0));
        let rows = neighbors_in(&m, &g, &p, &covered(std::slice::from_ref(&src))).unwrap();
        assert_eq!(rows.len(), 100_000);
        assert_eq!(rows[0].block, "c-00001");
        assert_eq!(rows[99_999].block, nid("c", 100_000).to_string());
        let (m, g) = scope(local(100_002, 0));
        let e = neighbors_in(&m, &g, &p, &covered(std::slice::from_ref(&src))).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "section neighbors of c-00000 exceed 100000 blocks",
                "split the parent of c-00000 into smaller sections"
            )
        );
    }

    #[test]
    fn nb2_a_hundred_thousand_block_scope_reads_local_adjacencies() {
        let p = pack();
        // local doc: section s-00000 with the entity and claims c-00000..c-00004; c-00000 and
        // c-00001 reference the entity
        let mut blocks = local(5, 2);
        // a far doc: 1,000 sections; section 7 holds two claims that reference the entity
        let (far, en) = (nid("d", 1), nid("en", 0));
        blocks.push(blk(&far, "doc", None, ""));
        let mut n = 0;
        for s in 1..=1_000 {
            let sid = nid("s", s);
            blocks.push(blk(&sid, "section", Some(&far), ""));
            for k in 0..98 {
                n += 1;
                let body = if s == 7 && k < 2 {
                    format!("Far claim uses {{{{{en}}}}}.")
                } else {
                    format!("Far claim {n}.")
                };
                blocks.push(blk(&nid("cf", n), "claim", Some(&sid), &body));
            }
        }
        // padding to exactly 100,000 blocks
        let mut pad = 0;
        while blocks.len() < 100_000 {
            pad += 1;
            blocks.push(blk(&nid("p", pad), "prose", Some(&far), "Padding."));
        }
        let (m, g) = scope(blocks);
        assert_eq!(m.len(), 100_000);

        let src = nid("c", 0);
        let (rows, visits) = Walk::new(&m, &g, &p, Limits::FIXED)
            .run(&covered(&[src]))
            .unwrap();
        let got: Vec<(String, NeighborVia, Option<String>)> = rows
            .iter()
            .map(|r| (r.block.clone(), r.via, r.entity.clone()))
            .collect();
        let s = |b: &str| (b.to_string(), NeighborVia::Section, None);
        let e = |b: String| (b, NeighborVia::Entity, Some("en-00000".to_string()));
        assert_eq!(
            got,
            vec![
                s("c-00001"),
                s("c-00002"),
                s("c-00003"),
                s("c-00004"),
                e(nid("cf", 589).to_string()),
                e(nid("cf", 590).to_string()),
            ]
        );
        // the parent's 6 children and their 5 participants; the source's 1 outgoing edge; the
        // entity's 5 incoming edges (its section's `contains` and 4 references) and their 4
        // participants (the source among them). No visit reaches the rest of the 100,000 blocks.
        assert_eq!(visits, 6 + 5 + 1 + 5 + 4);
    }
}
