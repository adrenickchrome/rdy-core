//! Graph coverage (FW-EN4, FW-EN1a): the entity measures that Coverage and confidence (FW-CV1) wraps as its Grounded and Connected dimensions.
use crate::lexicon::{is_content, is_fact, is_tie, link_kind_for, linked_from_outside, tied};
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_graph::Graph;
use rdy_model::entity::{ENTITY, KIND};
use rdy_model::{Block, BlockId, tree};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The most unlinked docs a coverage reading lists; the rest are counted.
pub const UNLINKED_DOCS_CAP: usize = 20;

/// The entity measures of one scope, or of the blocks a briefing covers (FW-EN4, FW-EN1a).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GraphCoverage {
    /// `all` for a whole scope, `covered` for the blocks a briefing covers.
    pub scope: String,
    pub grounded: Grounded,
    pub connected: Connected,
    /// Entity blocks: blocks of a type with `defines`.
    pub entities: usize,
    /// The open `kind` values of entity blocks, with their counts, by kind.
    pub kinds: Vec<KindCount>,
    /// Groups of kinds that differ only in case, spacing, `-`, `_` or a final `s`.
    pub similar_kinds: Vec<Vec<String>>,
}

/// The Grounded measures (FW-EN4, FW-EN1a): normative facts and all content tied to an entity, and entity mentions linked. Coverage and confidence (FW-CV1) wraps them as its Grounded dimension.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Grounded {
    pub facts: usize,
    pub facts_tied: usize,
    pub facts_tied_pct: Option<u32>,
    pub content: usize,
    pub content_tied: usize,
    pub content_tied_pct: Option<u32>,
    pub mentions: usize,
    pub mentions_linked: usize,
    pub mentions_linked_pct: Option<u32>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Docs that nothing outside them links to (FW-EN4): part of the Connected dimension.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Connected {
    pub docs: usize,
    pub unlinked_docs: Vec<DocRef>,
    #[serde(skip_serializing_if = "is_zero")]
    pub unlinked_docs_omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocRef {
    pub id: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KindCount {
    pub kind: String,
    pub entities: usize,
}

/// `part * 100 / total`, rounded down; none when there is nothing to measure.
fn pct(part: usize, total: usize) -> Option<u32> {
    (total > 0).then(|| (part * 100 / total) as u32)
}

/// A kind's normal form: lowercase, without spaces, `-` and `_`, and without one final `s` when
/// more than 3 characters remain.
fn normal_kind(kind: &str) -> String {
    let mut n: String = kind
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .collect();
    if n.ends_with('s') && n.chars().count() > 4 {
        n.pop();
    }
    n
}

/// Whether `from` is tied to a block that exists and is an entity: one pass over its outgoing
/// edges, each a reference, a `describes` link or an `affects` link to an entity.
fn tied_to_entity(
    g: &Graph,
    m: &Materialized,
    is_entity: &impl Fn(&str) -> bool,
    from: &BlockId,
) -> bool {
    g.outgoing(from)
        .iter()
        .any(|e| is_tie(&e.kind) && m.get(&e.to).is_some_and(|t| is_entity(&t.block_type)))
}

impl ReadState {
    /// Graph coverage (FW-EN4, FW-EN1a) of every block of `scope`, or of the `covered` ids when
    /// given (what a briefing covers). The whole scope's is a function of its build, so it is
    /// memoised there (a write gives a new build).
    pub fn graph_coverage(
        &self,
        scope: &Scope,
        covered: Option<&BTreeSet<BlockId>>,
    ) -> Result<GraphCoverage, RdyError> {
        if covered.is_some() {
            return self.coverage_of(scope, covered);
        }
        let built = self.built(scope)?;
        if let Some(c) = built.coverage.get() {
            return Ok(GraphCoverage::clone(c));
        }
        let c = Arc::new(self.coverage_of(scope, None)?);
        Ok(GraphCoverage::clone(built.coverage.get_or_init(|| c)))
    }

    fn coverage_of(
        &self,
        scope: &Scope,
        covered: Option<&BTreeSet<BlockId>>,
    ) -> Result<GraphCoverage, RdyError> {
        let m = self.materialized(scope)?;
        let g = self.graph(scope)?;
        let lex = self.lexicon(scope)?;
        let pack = &self.pack;
        let is_entity = |t: &str| lex.is_entity_type(t);
        let blocks: Vec<&Block> = match covered {
            None => m.iter().collect(),
            Some(ids) => ids.iter().filter_map(|id| m.get(id)).collect(),
        };
        let mut grounded = Grounded::default();
        let mut docs: Vec<&Block> = vec![];
        let mut entities = 0;
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for b in blocks {
            if b.block_type == tree::DOC {
                docs.push(b);
            }
            if is_entity(&b.block_type) {
                entities += 1;
            }
            if b.block_type == ENTITY
                && let Some(k) = b.field_str(KIND).map(str::trim).filter(|k| !k.is_empty())
            {
                *kinds.entry(k.to_string()).or_default() += 1;
            }
            let content = is_content(pack, b);
            let fact = is_fact(pack, b);
            if content || fact {
                let linked = tied_to_entity(&g, &m, &is_entity, &b.id);
                if fact {
                    grounded.facts += 1;
                    grounded.facts_tied += usize::from(linked);
                }
                if content {
                    grounded.content += 1;
                    grounded.content_tied += usize::from(linked);
                }
            }
            if link_kind_for(pack, b).is_some() {
                for mention in lex.mentions(b) {
                    let Ok(entity) = BlockId::parse(&mention.entity) else {
                        continue;
                    };
                    grounded.mentions += 1;
                    grounded.mentions_linked += usize::from(tied(&g, &b.id, &entity));
                }
            }
        }
        grounded.facts_tied_pct = pct(grounded.facts_tied, grounded.facts);
        grounded.content_tied_pct = pct(grounded.content_tied, grounded.content);
        grounded.mentions_linked_pct = pct(grounded.mentions_linked, grounded.mentions);

        let mut unlinked: Vec<DocRef> = docs
            .iter()
            .filter(|d| !linked_from_outside(&g, &m, &d.id))
            .map(|d| DocRef {
                id: d.id.to_string(),
                path: m.path_of(&d.id).unwrap_or_default(),
            })
            .collect();
        unlinked.sort_by(|a, b| (&a.path, &a.id).cmp(&(&b.path, &b.id)));
        let omitted = unlinked.len().saturating_sub(UNLINKED_DOCS_CAP);
        unlinked.truncate(UNLINKED_DOCS_CAP);

        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for k in kinds.keys() {
            groups.entry(normal_kind(k)).or_default().push(k.clone());
        }
        // the kinds are visited in byte order, so each group is already sorted
        let mut similar_kinds: Vec<Vec<String>> =
            groups.into_values().filter(|g| g.len() > 1).collect();
        similar_kinds.sort();

        Ok(GraphCoverage {
            scope: if covered.is_some() { "covered" } else { "all" }.to_string(),
            grounded,
            connected: Connected {
                docs: docs.len(),
                unlinked_docs: unlinked,
                unlinked_docs_omitted: omitted,
            },
            entities,
            kinds: kinds
                .into_iter()
                .map(|(kind, entities)| KindCount { kind, entities })
                .collect(),
            similar_kinds,
        })
    }
}
