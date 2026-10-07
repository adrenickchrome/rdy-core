//! Suggestions (FW-EN3): links to the entities written text names, and new entities for literals that recur.
use crate::error::RdyError;
use crate::lexicon::{Mention, link_kind_for, linked_from_outside, tied};
use crate::{ReadState, Scope};
use rdy_model::entity::{MIN_MENTION_CHARS, code_literals, find_phrases, phrase, text_sources};
use rdy_model::tree::DOC;
use rdy_model::{Block, BlockId, Hash, Op};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One suggestion: a link from a written block to an entity its text names, or a new entity for
/// a code literal that recurs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Suggestion {
    Link {
        id: String,
        block: String,
        link: String,
        target: String,
        mention: String,
        field: String,
    },
    Entity {
        id: String,
        literal: String,
        blocks: Vec<String>,
        count: usize,
    },
}

/// A suggestion's id: `sg-` and the first 16 hex characters of the hash of
/// `rdy-suggestion\0{kind}\0{subject}\0{target}`.
pub fn suggestion_id(kind: &str, subject: &str, target: &str) -> String {
    let mut bytes = b"rdy-suggestion\0".to_vec();
    for (i, part) in [kind, subject, target].into_iter().enumerate() {
        if i > 0 {
            bytes.push(0);
        }
        bytes.extend_from_slice(part.as_bytes());
    }
    format!("sg-{}", &Hash::of(&bytes).to_string()[..16])
}

/// The most suggestions one write returns; the rest are counted.
pub const MAX_SUGGESTIONS: usize = 50;
/// A code literal suggests a new entity once at least this many blocks hold it.
pub const RECUR_MIN: usize = 3;
/// The most holders an entity suggestion lists.
pub const ENTITY_BLOCKS_SHOWN: usize = 10;
/// The most mentions of new docs one call lists; the rest are counted.
pub const MENTIONED_BY_CAP: usize = 20;

/// A block outside a new doc whose text names the doc: the doc's id, the block's id, the field
/// the text is in, and the text as written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MentionedBy {
    pub doc: String,
    pub block: String,
    pub field: String,
    pub mention: String,
}

impl ReadState {
    /// The suggestions for `change` in this state (the one the write published): for each
    /// written block that can link, a link to each entity its text names that it is not tied
    /// to; then, for each code literal of the written blocks that at least [`RECUR_MIN`] blocks
    /// hold and that names no entity, a new entity. Links by (block, target), then entities by
    /// literal, cut to [`MAX_SUGGESTIONS`]; the count the cut dropped is returned.
    pub fn suggestions(
        &self,
        scope: &Scope,
        change: &Hash,
    ) -> Result<(Vec<Suggestion>, usize), RdyError> {
        let ch = self.reader().change(change)?;
        let m = self.materialized(scope)?;
        let ids: BTreeSet<&BlockId> = ch
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Create { .. } | Op::Update { .. }))
            .map(Op::block_id)
            .collect();
        let written: Vec<&Block> = ids.into_iter().filter_map(|id| m.get(id)).collect();
        let linking: Vec<(&Block, &'static str)> = written
            .iter()
            .filter_map(|b| link_kind_for(self.pack(), b).map(|k| (*b, k)))
            .collect();
        let literals: BTreeSet<String> = written
            .iter()
            .flat_map(|b| text_sources(b))
            .flat_map(|(_, text)| code_literals(&text))
            .collect();
        // nothing to name and nothing to count: no lexicon to read
        if linking.is_empty() && literals.is_empty() {
            return Ok((vec![], 0));
        }
        let lex = self.lexicon(scope)?;
        let named: Vec<(&Block, &'static str, Vec<Mention>)> = linking
            .into_iter()
            .map(|(b, kind)| (b, kind, lex.mentions(b)))
            .filter(|(_, _, ms)| !ms.is_empty())
            .collect();
        // a graph only when some written block names an entity
        let g = if named.is_empty() {
            None
        } else {
            Some(self.graph(scope)?)
        };
        let mut out: Vec<Suggestion> = vec![];
        for (b, kind, mentions) in named {
            let mut found: Vec<(BlockId, Mention)> = mentions
                .into_iter()
                .filter_map(|ms| Some((BlockId::parse(&ms.entity).ok()?, ms)))
                .filter(|(target, _)| g.as_ref().is_some_and(|g| !tied(g, &b.id, target)))
                .collect();
            found.sort_by(|x, y| x.0.cmp(&y.0));
            for (target, ms) in found {
                let (block, target) = (b.id.to_string(), target.to_string());
                out.push(Suggestion::Link {
                    id: suggestion_id(kind, &block, &target),
                    block,
                    link: kind.to_string(),
                    target,
                    mention: ms.text,
                    field: ms.field,
                });
            }
        }
        for literal in literals {
            let holders = lex.literal_blocks(&literal);
            if holders.len() < RECUR_MIN || !lex.resolve(&literal).is_empty() {
                continue;
            }
            out.push(Suggestion::Entity {
                id: suggestion_id("entity", &literal, ""),
                blocks: holders
                    .iter()
                    .take(ENTITY_BLOCKS_SHOWN)
                    .map(BlockId::to_string)
                    .collect(),
                count: holders.len(),
                literal,
            });
        }
        let omitted = out.len().saturating_sub(MAX_SUGGESTIONS);
        out.truncate(MAX_SUGGESTIONS);
        Ok((out, omitted))
    }

    /// The blocks that mention each new doc of `docs` (ids that are not docs are ignored): a doc
    /// is new while nothing outside it links into it. Its phrases are those of its `title` and
    /// its `key` with at least [`MIN_MENTION_CHARS`] characters. Every block that can link and
    /// sits in another doc is scanned; each (doc, block) pair is listed once, with the first
    /// field and the text as written, unless the block is tied to the doc. Sorted by (doc,
    /// block), cut to [`MENTIONED_BY_CAP`]; the count the cut dropped is returned.
    pub fn mentioned_by(
        &self,
        scope: &Scope,
        docs: &BTreeSet<BlockId>,
    ) -> Result<(Vec<MentionedBy>, usize), RdyError> {
        let m = self.materialized(scope)?;
        let candidates: Vec<&Block> = docs
            .iter()
            .filter_map(|id| m.get(id))
            .filter(|b| b.block_type == DOC)
            .collect();
        if candidates.is_empty() {
            return Ok((vec![], 0));
        }
        let g = self.graph(scope)?;
        // each phrase and the new docs it names
        let mut phrases: BTreeMap<String, BTreeSet<BlockId>> = BTreeMap::new();
        for d in candidates {
            if linked_from_outside(&g, &m, &d.id) {
                continue;
            }
            for text in [d.field_str("title"), d.field_str("key")]
                .into_iter()
                .flatten()
            {
                let p = phrase(text);
                if p.chars().count() >= MIN_MENTION_CHARS {
                    phrases.entry(p).or_default().insert(d.id.clone());
                }
            }
        }
        if phrases.is_empty() {
            return Ok((vec![], 0));
        }
        let known = |p: &str| phrases.contains_key(p);
        let mut found: BTreeMap<(BlockId, BlockId), (String, String)> = BTreeMap::new();
        for b in m.iter() {
            if link_kind_for(self.pack(), b).is_none() {
                continue;
            }
            let Some(own) = m.doc_of(&b.id).map(|d| d.id.clone()) else {
                continue;
            };
            for (field, text) in text_sources(b) {
                for (p, range) in find_phrases(&text, &known) {
                    for doc in phrases.get(&p).into_iter().flatten() {
                        if *doc == own || found.contains_key(&(doc.clone(), b.id.clone())) {
                            continue;
                        }
                        if tied(&g, &b.id, doc) {
                            continue;
                        }
                        found.insert(
                            (doc.clone(), b.id.clone()),
                            (field.clone(), text[range.clone()].to_string()),
                        );
                    }
                }
            }
        }
        let omitted = found.len().saturating_sub(MENTIONED_BY_CAP);
        let out = found
            .into_iter()
            .take(MENTIONED_BY_CAP)
            .map(|((doc, block), (field, mention))| MentionedBy {
                doc: doc.to_string(),
                block: block.to_string(),
                field,
                mention,
            })
            .collect();
        Ok((out, omitted))
    }
}
