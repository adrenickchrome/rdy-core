//! The entity lexicon (FW-EN1, FW-EN3): entity keys, names and aliases by phrase, and the blocks that hold each code literal. Built on first use per scope and carried across writes like the graph.
use crate::Materialized;
use rdy_graph::{EdgeKind, Graph};
use rdy_model::entity::{aliases, code_literals, find_phrases, phrase, source_field, text_sources};
use rdy_model::{Block, BlockId, decision};
use rdy_schema::{FieldKind, Pack, Trait};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// How a phrase names an entity. Declaration order is the match order: a key beats a name, a
/// name beats an alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NameKind {
    Key,
    Name,
    Alias,
}

/// One entity a block's text names: the entity's id, the field the text is in, and the text as
/// written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Mention {
    pub entity: String,
    pub field: String,
    pub text: String,
}

/// The entity lexicon of one scope. Every part is a function of single blocks, so carrying it
/// across a write (`with_changes`) equals a rebuild.
#[derive(Clone, Debug, PartialEq)]
pub struct Lexicon {
    /// The pack's entity types: every type with `defines`.
    entity_types: Arc<BTreeSet<String>>,
    /// The pack's `enum` fields by type: never read as text.
    enum_fields: Arc<BTreeMap<String, BTreeSet<String>>>,
    /// What each entity contributed: its key, name and alias phrases.
    names: imbl::OrdMap<BlockId, Vec<(NameKind, String)>>,
    /// Each phrase and the entities it names, with how.
    phrases: imbl::OrdMap<String, imbl::OrdSet<(BlockId, NameKind)>>,
    /// Each code literal (exact text) and the blocks whose text holds it.
    literals: imbl::OrdMap<String, imbl::OrdSet<BlockId>>,
}

impl Lexicon {
    /// The lexicon of `blocks` under `pack`.
    pub fn build<'a>(pack: &Pack, blocks: impl IntoIterator<Item = &'a Block>) -> Lexicon {
        let entity_types = pack
            .block_types
            .iter()
            .filter(|(_, t)| t.traits.contains(&Trait::Defines))
            .map(|(n, _)| n.clone())
            .collect();
        let mut l = Lexicon {
            entity_types: Arc::new(entity_types),
            enum_fields: Arc::new(enum_fields(pack)),
            names: imbl::OrdMap::new(),
            phrases: imbl::OrdMap::new(),
            literals: imbl::OrdMap::new(),
        };
        for b in blocks {
            l.add(b);
        }
        l
    }

    /// This lexicon with each `old` block's contribution removed and each `new` block's added.
    pub fn with_changes<'a>(
        &self,
        old: impl IntoIterator<Item = &'a Block>,
        new: impl IntoIterator<Item = &'a Block>,
    ) -> Lexicon {
        let mut l = self.clone();
        for b in old {
            l.remove(b);
        }
        for b in new {
            l.add(b);
        }
        l
    }

    /// Whether `block_type` is an entity type (it has `defines`).
    pub fn is_entity_type(&self, block_type: &str) -> bool {
        self.entity_types.contains(block_type)
    }

    /// How many entities the lexicon holds.
    pub fn entity_count(&self) -> usize {
        self.names.len()
    }

    /// The entities whose key, name or an alias is the phrase of `text`, each once with its best
    /// kind, sorted by (kind, id). Empty for a text with no words.
    pub fn resolve(&self, text: &str) -> Vec<(BlockId, NameKind)> {
        let p = phrase(text);
        if p.is_empty() {
            return vec![];
        }
        let Some(set) = self.phrases.get(&p) else {
            return vec![];
        };
        // the set is ordered by (id, kind): the first entry per id is its best kind
        let mut out: Vec<(BlockId, NameKind)> = vec![];
        for (id, kind) in set {
            if out.last().is_none_or(|(last, _)| last != id) {
                out.push((id.clone(), *kind));
            }
        }
        out.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        out
    }

    /// The entities `b`'s text names by name or alias (never by key), one mention per entity,
    /// the first found, reading `text_in` in order. An entity never mentions itself.
    pub fn mentions(&self, b: &Block) -> Vec<Mention> {
        let known = |p: &str| {
            self.phrases
                .get(p)
                .is_some_and(|set| set.iter().any(|(_, k)| *k != NameKind::Key))
        };
        let mut seen: BTreeSet<BlockId> = BTreeSet::new();
        let mut out = vec![];
        for (field, text) in self.sources(b) {
            for (p, range) in find_phrases(&text, &known) {
                let Some(set) = self.phrases.get(&p) else {
                    continue;
                };
                for (id, kind) in set {
                    if *kind == NameKind::Key || *id == b.id || seen.contains(id) {
                        continue;
                    }
                    seen.insert(id.clone());
                    out.push(Mention {
                        entity: id.to_string(),
                        field: field.clone(),
                        text: text[range.clone()].to_string(),
                    });
                }
            }
        }
        out
    }

    /// The blocks whose text holds `literal` (exact text), sorted by id.
    pub fn literal_blocks(&self, literal: &str) -> Vec<BlockId> {
        self.literals
            .get(literal)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The (kind, phrase) pairs an entity block contributes, without repeats; empty phrases
    /// are skipped.
    fn names_of(b: &Block) -> Vec<(NameKind, String)> {
        let mut out: Vec<(NameKind, String)> = vec![];
        let mut push = |kind: NameKind, text: &str| {
            let p = phrase(text);
            if !p.is_empty() && !out.iter().any(|(k, q)| *k == kind && *q == p) {
                out.push((kind, p));
            }
        };
        if let Some(k) = b.field_str("key") {
            push(NameKind::Key, k);
        }
        if let Some(n) = b.field_str("name") {
            push(NameKind::Name, n);
        }
        for a in aliases(b) {
            push(NameKind::Alias, a);
        }
        out
    }

    /// The text `b` holds under this lexicon's pack: [`text_in`].
    fn sources(&self, b: &Block) -> Vec<(String, String)> {
        let enums = self.enum_fields.get(&b.block_type);
        without(text_sources(b), |f| enums.is_some_and(|e| e.contains(f)))
    }

    /// The distinct code literals of `b`'s text sources.
    fn literals_of(&self, b: &Block) -> BTreeSet<String> {
        self.sources(b)
            .iter()
            .flat_map(|(_, text)| code_literals(text))
            .collect()
    }

    fn add(&mut self, b: &Block) {
        if self.is_entity_type(&b.block_type) {
            let names = Lexicon::names_of(b);
            for (kind, p) in &names {
                self.phrases
                    .entry(p.clone())
                    .or_default()
                    .insert((b.id.clone(), *kind));
            }
            self.names.insert(b.id.clone(), names);
        }
        for lit in self.literals_of(b) {
            self.literals.entry(lit).or_default().insert(b.id.clone());
        }
    }

    fn remove(&mut self, b: &Block) {
        if let Some(names) = self.names.remove(&b.id) {
            for (kind, p) in names {
                if let Some(set) = self.phrases.get_mut(&p) {
                    set.remove(&(b.id.clone(), kind));
                    if set.is_empty() {
                        self.phrases.remove(&p);
                    }
                }
            }
        }
        for lit in self.literals_of(b) {
            if let Some(set) = self.literals.get_mut(&lit) {
                set.remove(&b.id);
                if set.is_empty() {
                    self.literals.remove(&lit);
                }
            }
        }
    }
}

/// Each type's fields of kind `enum` in `pack`.
fn enum_fields(pack: &Pack) -> BTreeMap<String, BTreeSet<String>> {
    pack.block_types
        .iter()
        .map(|(t, def)| {
            let enums = def
                .fields
                .iter()
                .filter(|(_, fd)| fd.kind == FieldKind::Enum)
                .map(|(f, _)| f.clone())
                .collect::<BTreeSet<_>>();
            (t.clone(), enums)
        })
        .filter(|(_, enums)| !enums.is_empty())
        .collect()
}

/// `sources` without those whose field `skip` names.
fn without(sources: Vec<(String, String)>, skip: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    sources
        .into_iter()
        .filter(|(name, _)| !skip(source_field(name)))
        .collect()
}

/// The text a mention or literal scan reads in `b` under `pack`: [`text_sources`] (which skips
/// the metadata fields) without the fields the pack gives kind `enum`, whose values are a fixed
/// list, never prose.
pub fn text_in(pack: &Pack, b: &Block) -> Vec<(String, String)> {
    let def = pack.block_types.get(&b.block_type);
    without(text_sources(b), |f| {
        def.and_then(|d| d.fields.get(f))
            .is_some_and(|fd| fd.kind == FieldKind::Enum)
    })
}

/// The link a block of `b`'s type makes to an entity: none for an unknown type and for a type
/// with `structural`, `presentational`, `asset` or `defines`; `affects` for a decision that is
/// not frozen (none once accepted or rejected); `describes` for every other type.
pub fn link_kind_for(pack: &Pack, b: &Block) -> Option<&'static str> {
    let def = pack.block_types.get(&b.block_type)?;
    let barred = [
        Trait::Structural,
        Trait::Presentational,
        Trait::Asset,
        Trait::Defines,
    ];
    if barred.iter().any(|t| def.traits.contains(t)) {
        return None;
    }
    if b.block_type == decision::DECISION {
        return (!decision::is_frozen(b)).then_some(decision::AFFECTS);
    }
    Some(decision::DESCRIBES)
}

/// A fact: a block whose type has `normative` or `describes`, not `defines`, and is not a
/// decision (claim, requirement, constraint, flow in rdy-standard).
pub fn is_fact(pack: &Pack, b: &Block) -> bool {
    if b.block_type == decision::DECISION {
        return false;
    }
    pack.block_types.get(&b.block_type).is_some_and(|t| {
        (t.traits.contains(&Trait::Normative) || t.traits.contains(&Trait::Describes))
            && !t.traits.contains(&Trait::Defines)
    })
}

/// Content (FW-EN1a): a block that can describe an entity; facts are content.
pub fn is_content(pack: &Pack, b: &Block) -> bool {
    link_kind_for(pack, b) == Some("describes")
}

/// Whether `from` has an outgoing reference, `describes` link or `affects` link to `to`.
pub fn tied(g: &Graph, from: &BlockId, to: &BlockId) -> bool {
    g.outgoing(from)
        .iter()
        .any(|e| e.to == *to && is_tie(&e.kind))
}

/// Whether an edge of `kind` ties its source to its target: a reference, a `describes` link or
/// an `affects` link.
pub(crate) fn is_tie(kind: &EdgeKind) -> bool {
    match kind {
        EdgeKind::References => true,
        EdgeKind::Link(n) => n == decision::DESCRIBES || n == decision::AFFECTS,
        EdgeKind::Contains => false,
    }
}

/// Whether a block outside `doc`'s subtree points into it: any incoming edge other than
/// `contains` to `doc` or a block under it, from a block that is not in the subtree.
pub fn linked_from_outside(g: &Graph, m: &Materialized, doc: &BlockId) -> bool {
    let mut subtree: BTreeSet<BlockId> = BTreeSet::new();
    let mut stack = vec![doc.clone()];
    let mut visits = 0;
    while let Some(id) = stack.pop() {
        if visits >= m.len() || !subtree.insert(id.clone()) {
            continue;
        }
        visits += 1;
        for c in m.children(Some(&id)) {
            if !subtree.contains(&c.id) {
                stack.push(c.id.clone());
            }
        }
    }
    subtree.iter().any(|id| {
        g.incoming(id)
            .iter()
            .any(|e| e.kind != EdgeKind::Contains && !subtree.contains(&e.from))
    })
}
