//! Suggestions (FW-EN3): links to the entities written text names, and new entities for literals that recur.
use crate::error::RdyError;
use crate::lexicon::{Mention, link_kind_for, linked_from_outside, text_in, tied};
use crate::{Materialized, ReadState, Scope};
use rdy_model::entity::{MIN_MENTION_CHARS, code_literals, find_phrases, phrase};
use rdy_model::tree::DOC;
use rdy_model::{Block, BlockId, Hash, Op};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

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

/// Each phrase of the new docs one `mentioned_by` call reads, and the docs it names.
pub(crate) type Phrases = BTreeMap<String, BTreeSet<BlockId>>;
/// Every (doc, block) pair one `mentioned_by` call finds, uncut, with the field and the text.
type Found = imbl::OrdMap<(BlockId, BlockId), (String, String)>;

/// The most mention scans one scope's build keeps; the least recently used goes first.
const MENTION_SCANS: usize = 8;

/// One `mentioned_by` result, uncut, for one set of phrases, kept in the scope's build. Each
/// (doc, block) pair is a function of the block, of the doc it sits in and of the phrases, so
/// a write that moves no other block into another doc changes only the pairs of the blocks it
/// writes: carried across it, those are `stale` and read again on next use.
#[derive(Clone)]
pub(crate) struct MentionScan {
    phrases: Arc<Phrases>,
    found: Found,
    stale: BTreeSet<BlockId>,
}

impl MentionScan {
    /// `scans` carried across `ops` (applied to `before`): the written blocks become stale. None
    /// when an op can move a block it does not write into another doc: a delete of a block with
    /// children, or an update that changes the parent or the type of a block with children.
    pub(crate) fn carried(
        scans: &[MentionScan],
        before: &Materialized,
        ops: &[Op],
    ) -> Vec<MentionScan> {
        let moves_others = ops.iter().any(|op| {
            let Some(old) = before.get(op.block_id()) else {
                return false;
            };
            let moved = match op {
                Op::Delete { .. } => true,
                Op::Create { block } | Op::Update { block, .. } => {
                    block.parent != old.parent || block.block_type != old.block_type
                }
            };
            moved && !before.children(Some(&old.id)).is_empty()
        });
        if moves_others {
            return vec![];
        }
        scans
            .iter()
            .map(|s| {
                let mut s = s.clone();
                s.stale.extend(ops.iter().map(|op| op.block_id().clone()));
                s
            })
            .collect()
    }
}

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
    /// to; then, for each code literal of the written blocks that has words, that at least
    /// [`RECUR_MIN`] blocks hold and that names no entity, a new entity. Links by (block, target), then entities by
    /// literal, cut to [`MAX_SUGGESTIONS`]; the count the cut dropped is returned.
    pub fn suggestions(
        &self,
        scope: &Scope,
        change: &Hash,
    ) -> Result<(Vec<Suggestion>, usize), RdyError> {
        self.suggestions_of(scope, std::slice::from_ref(change))
    }

    /// [`ReadState::suggestions`] for `changes` written together (an import writes one change
    /// per author): the written blocks are those any of them wrote, and the cut applies to the
    /// whole list.
    pub fn suggestions_of(
        &self,
        scope: &Scope,
        changes: &[Hash],
    ) -> Result<(Vec<Suggestion>, usize), RdyError> {
        let chs = changes
            .iter()
            .map(|h| self.reader().change(h))
            .collect::<Result<Vec<_>, _>>()?;
        let m = self.materialized(scope)?;
        let ids: BTreeSet<&BlockId> = chs
            .iter()
            .flat_map(|ch| &ch.ops)
            .filter(|o| matches!(o, Op::Create { .. } | Op::Update { .. }))
            .map(Op::block_id)
            .collect();
        let written: Vec<&Block> = ids.into_iter().filter_map(|id| m.get(id)).collect();
        let linking: Vec<(&Block, &'static str)> = written
            .iter()
            .filter_map(|b| link_kind_for(self.pack(), b).map(|k| (*b, k)))
            .collect();
        // a literal with no words (`->`) has an empty phrase: no entity can carry it as a name,
        // so a suggestion for it could never clear
        let literals: BTreeSet<String> = written
            .iter()
            .flat_map(|b| text_in(self.pack(), b))
            .flat_map(|(_, text)| code_literals(&text))
            .filter(|l| !phrase(l).is_empty())
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
                // a dismissed pair holds in every scope, until a restore (D-E6JD1)
                .filter(|(target, _)| self.mention_dismissed(&b.id, target).is_none())
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
    /// sits in another doc is read; each (doc, block) pair is listed once, with the first field
    /// and the text as written, unless the block is tied to the doc. Sorted by (doc, block), cut
    /// to [`MENTIONED_BY_CAP`]; the count the cut dropped is returned.
    ///
    /// Only a block that holds every word of a phrase can name it, so the first call for a set
    /// of phrases reads only the blocks the lexicon's word index gives. Its result is kept in the
    /// scope's build and carried across writes ([`MentionScan`]): a later call reads only the
    /// blocks written since.
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
        let mut phrases: Phrases = BTreeMap::new();
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
        let built = self.built(scope)?;
        let kept = {
            let scans = built.mentions.lock().unwrap_or_else(|e| e.into_inner());
            scans.iter().find(|s| *s.phrases == phrases).cloned()
        };
        let scan = match kept {
            Some(kept) if kept.stale.is_empty() => kept,
            Some(kept) => {
                // the blocks written since: their pairs out, then each read again
                let mut found = kept.found;
                for id in &kept.stale {
                    for doc in kept.phrases.values().flatten() {
                        found.remove(&(doc.clone(), id.clone()));
                    }
                    if let Some(b) = m.get(id) {
                        let own = m.doc_of(id).map(|d| d.id.clone());
                        self.mentions_in(&g, &phrases, b, own, &mut found);
                    }
                }
                MentionScan {
                    phrases: kept.phrases,
                    found,
                    stale: BTreeSet::new(),
                }
            }
            None => {
                let lex = self.lexicon(scope)?;
                let read: BTreeSet<BlockId> = phrases
                    .keys()
                    .flat_map(|p| lex.blocks_with_words(p))
                    .collect();
                let mut found = Found::new();
                // the doc each parent sits in: the blocks read share few parents
                let mut doc_under: HashMap<&BlockId, Option<BlockId>> = HashMap::new();
                for b in read.iter().filter_map(|id| m.get(id)) {
                    let own = if b.block_type == DOC {
                        Some(b.id.clone())
                    } else {
                        b.parent.as_ref().and_then(|p| {
                            doc_under
                                .entry(p)
                                .or_insert_with(|| m.doc_of(p).map(|d| d.id.clone()))
                                .clone()
                        })
                    };
                    self.mentions_in(&g, &phrases, b, own, &mut found);
                }
                MentionScan {
                    phrases: Arc::new(phrases),
                    found,
                    stale: BTreeSet::new(),
                }
            }
        };
        {
            let mut scans = built.mentions.lock().unwrap_or_else(|e| e.into_inner());
            scans.retain(|s| s.phrases != scan.phrases);
            scans.insert(0, scan.clone());
            scans.truncate(MENTION_SCANS);
        }
        let found = scan.found;
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

    /// The pairs block `b` (in doc `own`) adds to a mention scan for `phrases`: for each doc a
    /// phrase in its text names, the first field and the text as written, unless `b` cannot
    /// link, sits in no doc or in that doc, or is tied to it.
    fn mentions_in(
        &self,
        g: &rdy_graph::Graph,
        phrases: &Phrases,
        b: &Block,
        own: Option<BlockId>,
        found: &mut Found,
    ) {
        if link_kind_for(self.pack(), b).is_none() {
            return;
        }
        let Some(own) = own else {
            return;
        };
        let known = |p: &str| phrases.contains_key(p);
        // a (doc, block) pair is listed once, with the first field and text that name the doc
        let mut named: BTreeSet<&BlockId> = BTreeSet::new();
        for (field, text) in text_in(self.pack(), b) {
            for (p, range) in find_phrases(&text, &known) {
                for doc in phrases.get(&p).into_iter().flatten() {
                    if *doc == own || named.contains(doc) || tied(g, &b.id, doc) {
                        continue;
                    }
                    named.insert(doc);
                    found.insert(
                        (doc.clone(), b.id.clone()),
                        (field.clone(), text[range.clone()].to_string()),
                    );
                }
            }
        }
    }
}
