//! The entity lexicon (FW-EN1, FW-EN3): entity keys, names and aliases by phrase, and the blocks that hold each code literal and each word. Built on first use per scope and carried across writes like the graph.
//!
//! One name matcher (D-YXBXH, D-JDW8A): a name or an alias is filed under every key
//! [`phrase_keys`] gives it, and a text phrase names it when one of the phrase's keys is among
//! them. An entity's `key` is an exact identifier, filed under its phrase only. An everyday
//! entity (`everyday: true`) is named only in the text of a block tied to it (O5).
use crate::Materialized;
use rdy_graph::{EdgeKind, Graph, edges_of};
use rdy_model::entity::{
    aliases, code_literals, lines_at, phrase, phrase_keys, shown_sources, source_field,
    text_sources, visit_phrases, visit_text_sources, words,
};
use rdy_model::{Block, BlockId, FieldValue, decision};
use rdy_schema::{FieldKind, Pack, Trait};
use serde::Serialize;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::ControlFlow;
use std::sync::Arc;

thread_local! {
    static TEXT_SCANS: Cell<usize> = const { Cell::new(0) };
}

/// Test hook: how many block texts this thread has read for a mention or literal scan
/// ([`text_in`], the lexicon's own reads and the changed-literal scans of
/// [`crate::consequence_literals`]), so a test can bound the blocks a read touches.
#[doc(hidden)]
pub fn text_scans() -> usize {
    TEXT_SCANS.with(Cell::get)
}

/// Counts one block text read for [`text_scans`].
pub(crate) fn count_scan() {
    TEXT_SCANS.with(|c| c.set(c.get() + 1));
}

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

/// nb4 (D-YXBXH, O5): where a mention's words are, in the stored text of the source leaf that
/// holds them: the leaf (`body`, a field, `name[i]` or `name.key`), the one-based line of the
/// match's first byte, the zero-based half-open UTF-8 byte range of the match on that line
/// (cut at the line's end for a match that goes on to the next line), and the matched words
/// exactly as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MentionText {
    pub field: String,
    pub line: usize,
    pub span: [usize; 2],
    pub words: String,
}

/// A [`Mention`] with its [`MentionText`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedMention {
    pub mention: Mention,
    pub text: MentionText,
}

/// The field that makes an entity everyday: only the boolean `true` does.
pub const EVERYDAY: &str = "everyday";

/// The block a text is in, as the mention scan reads it: its id, and the everyday entities it
/// is tied to by a reference, a `describes` link or an `affects` link (the graph's edges of the
/// block itself), whose mentions it counts.
pub(crate) struct MentionSource {
    id: BlockId,
    ties: BTreeSet<BlockId>,
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
    /// Each key and the entities filed under it, with how: a key under its own phrase, a name or
    /// an alias under each of its [`phrase_keys`].
    phrases: imbl::OrdMap<String, imbl::OrdSet<(BlockId, NameKind)>>,
    /// The everyday entities: those whose `everyday` field is the boolean `true`.
    everyday: imbl::OrdSet<BlockId>,
    /// Each code literal (exact text) and the blocks whose text holds it.
    literals: imbl::OrdMap<String, imbl::OrdSet<BlockId>>,
    /// Each word (case-folded, as [`words`] splits text) and the blocks whose text holds it: a
    /// phrase can only be found in a block that holds each of its words.
    words: imbl::OrdMap<String, imbl::OrdSet<BlockId>>,
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
            everyday: imbl::OrdSet::new(),
            literals: imbl::OrdMap::new(),
            words: imbl::OrdMap::new(),
        };
        // the term maps are gathered in plain maps first and made persistent once: one insert
        // per (term, block) into a persistent set costs several times more
        let mut literals: HashMap<String, Vec<BlockId>> = HashMap::new();
        let mut folded: HashMap<String, Vec<BlockId>> = HashMap::new();
        for b in blocks {
            l.add_names(b);
            let (lits, ws) = l.terms_of(b);
            for t in lits {
                literals.entry(t).or_default().push(b.id.clone());
            }
            for t in ws {
                folded.entry(t).or_default().push(b.id.clone());
            }
        }
        let persist = |m: HashMap<String, Vec<BlockId>>| {
            m.into_iter()
                .map(|(t, ids)| (t, ids.into_iter().collect::<imbl::OrdSet<BlockId>>()))
                .collect::<imbl::OrdMap<_, _>>()
        };
        l.literals = persist(literals);
        l.words = persist(folded);
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

    /// The entities whose key is the phrase of `text`, or whose name or an alias has a key that
    /// `text`'s phrase has, each once with its best kind, sorted by (kind, id). Empty for a text
    /// with no words. Everyday entities resolve too: this is an explicit lookup.
    pub fn resolve(&self, text: &str) -> Vec<(BlockId, NameKind)> {
        let p = phrase(text);
        let mut best: BTreeMap<&BlockId, NameKind> = BTreeMap::new();
        for k in phrase_keys(&p) {
            for (id, kind) in self.phrases.get(k).into_iter().flatten() {
                // a key is an exact identifier
                if *kind == NameKind::Key && k != p {
                    continue;
                }
                best.entry(id)
                    .and_modify(|b| *b = (*b).min(*kind))
                    .or_insert(*kind);
            }
        }
        let mut out: Vec<(BlockId, NameKind)> =
            best.into_iter().map(|(id, k)| (id.clone(), k)).collect();
        out.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        out
    }

    /// The mention source of block `b`: its id and the everyday entities it is tied to.
    pub(crate) fn source(&self, b: &Block) -> MentionSource {
        let ties = if self.everyday.is_empty() {
            BTreeSet::new()
        } else {
            edges_of(b)
                .into_iter()
                .filter(|e| is_tie(&e.kind) && self.everyday.contains(&e.to))
                .map(|e| e.to)
                .collect()
        };
        MentionSource {
            id: b.id.clone(),
            ties,
        }
    }

    /// Whether a text in `source` can name entity `id`: it is not everyday, or `source` is tied
    /// to it.
    fn admits(&self, source: &MentionSource, id: &BlockId) -> bool {
        !self.everyday.contains(id) || source.ties.contains(id)
    }

    /// The entities `b`'s text names by name or alias (never by key), one mention per entity,
    /// the first found, reading `text_in` in order. An entity never mentions itself.
    /// [`Lexicon::mentions_located`] without where the words are.
    pub fn mentions(&self, b: &Block) -> Vec<Mention> {
        self.first_mentions(b).into_iter().map(|(m, _)| m).collect()
    }

    /// [`Lexicon::mentions`], each with where its words are in `b`'s stored text (nb4,
    /// D-YXBXH): the source leaf, the one-based line of the match's first byte, the match's
    /// byte range on that line (cut at the line's end when the match goes on), and the words as
    /// stored. The scan reads the text with each reference blanked to the same bytes, so the
    /// positions are those of the stored text.
    pub fn mentions_located(&self, b: &Block) -> Vec<LocatedMention> {
        let found = self.first_mentions(b);
        if found.is_empty() {
            return vec![];
        }
        // the stored leaves that hold a first match, read once more without a scan
        let mut out: Vec<Option<LocatedMention>> = vec![None; found.len()];
        visit_text_sources(b, &mut |name, stored, _| {
            for (slot, (m, range)) in out.iter_mut().zip(&found) {
                if slot.is_none() && m.field == name {
                    *slot = Some(LocatedMention {
                        mention: m.clone(),
                        text: located(name, stored, range.clone()),
                    });
                }
            }
            ControlFlow::Continue(())
        });
        out.into_iter().flatten().collect()
    }

    /// The scan of [`Lexicon::mentions`]: each entity's first mention, with the byte range of
    /// its words in its source's text.
    fn first_mentions(&self, b: &Block) -> Vec<(Mention, std::ops::Range<usize>)> {
        // no name to find: the text is not read
        if self.phrases.is_empty() {
            return vec![];
        }
        let source = self.source(b);
        let mut seen: BTreeSet<BlockId> = BTreeSet::new();
        let mut out = vec![];
        for (field, text) in self.sources(b) {
            self.visit_mention_spans(&text, &source, &mut |id, range| {
                if !seen.contains(id) {
                    seen.insert(id.clone());
                    out.push((
                        Mention {
                            entity: id.to_string(),
                            field: field.clone(),
                            text: text[range.clone()].to_string(),
                        },
                        range,
                    ));
                }
            });
        }
        out
    }

    /// Each entity `text` names by name or alias (never by key), with the byte range of the
    /// words that name it, in text order; the phrases are those of
    /// [`find_phrases`](rdy_model::entity::find_phrases), longest first.
    /// A phrase that names several entities (through any of its keys) gives each once, in id
    /// order. `source`, the block the text is in, is never among them, and an everyday entity is
    /// among them only when `source` is tied to it. Each occurrence of a name is given.
    /// [`Lexicon::visit_mention_spans`], collected.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mention_spans(
        &self,
        text: &str,
        source: &Block,
    ) -> Vec<(BlockId, std::ops::Range<usize>)> {
        let mut out = vec![];
        let source = self.source(source);
        self.visit_mention_spans(text, &source, &mut |id, range| {
            out.push((id.clone(), range))
        });
        out
    }

    /// Calls `visit` with each (entity, range) [`Lexicon::mention_spans`] gives, in the same
    /// order, as the phrases are found: no occurrence is collected, so a name that many
    /// entities share costs no memory per occurrence.
    ///
    /// An everyday entity that `source` is not tied to is not known there: the scan goes on to
    /// shorter and later phrases, so it never hides another entity's name.
    pub(crate) fn visit_mention_spans(
        &self,
        text: &str,
        source: &MentionSource,
        visit: &mut dyn FnMut(&BlockId, std::ops::Range<usize>),
    ) {
        if self.phrases.is_empty() {
            return;
        }
        visit_phrases(text, &|p| self.known_in(p, source), &mut |p, range| {
            // the entities of every key of the phrase, each once, in id order
            let mut ids: BTreeSet<&BlockId> = BTreeSet::new();
            for k in phrase_keys(p) {
                for (id, kind) in self.phrases.get(k).into_iter().flatten() {
                    if *kind != NameKind::Key && *id != source.id && self.admits(source, id) {
                        ids.insert(id);
                    }
                }
            }
            for id in ids {
                visit(id, range.clone());
            }
        });
    }

    /// Calls `visit` with the range of each phrase the mention scan takes in `text`, in order,
    /// including a phrase that names only `text`'s own block: the words the scan consumes.
    pub(crate) fn visit_phrase_spans(
        &self,
        text: &str,
        source: &MentionSource,
        visit: &mut dyn FnMut(std::ops::Range<usize>),
    ) {
        if self.phrases.is_empty() {
            return;
        }
        visit_phrases(text, &|p| self.known_in(p, source), &mut |_, range| {
            visit(range)
        });
    }

    /// Whether a phrase the mention scan can accept in some block starts with `prefix` (folded
    /// words joined by one space, perhaps ending in part of a word) and is longer than it. A
    /// phrase is accepted through any of its keys, so it extends `prefix` when a key filed under
    /// a name or an alias starts with `prefix` and is longer, or when that key is `prefix`
    /// itself, or `prefix` without a final `e`, and the phrase adds `s` or `es` to it. May
    /// answer yes for a phrase no block accepts.
    pub(crate) fn extends_known(&self, prefix: &str) -> bool {
        use std::ops::Bound;
        let longer = self
            .phrases
            .range::<_, str>((Bound::Excluded(prefix), Bound::Unbounded))
            .take_while(|(p, _)| p.starts_with(prefix))
            .any(|(_, set)| named_by_name(set));
        let stem = |k: &str| self.phrases.get(k).is_some_and(named_by_name);
        longer || stem(prefix) || prefix.strip_suffix('e').is_some_and(stem)
    }

    /// Whether the mention scan accepts phrase `p` in `source`: one of its keys names, by name
    /// or alias, `source` itself or an entity `source` can name.
    fn known_in(&self, p: &str, source: &MentionSource) -> bool {
        phrase_keys(p).any(|k| {
            self.phrases.get(k).is_some_and(|set| {
                set.iter().any(|(id, kind)| {
                    *kind != NameKind::Key && (*id == source.id || self.admits(source, id))
                })
            })
        })
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
    pub fn names_of(b: &Block) -> Vec<(NameKind, String)> {
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
        count_scan();
        let enums = self.enum_fields.get(&b.block_type);
        without(text_sources(b), |f| enums.is_some_and(|e| e.contains(f)))
    }

    /// The blocks whose text holds every earlier word of `phrase` (a [`phrase`]: folded words
    /// joined by one space) and a word its last word can match, sorted by id: for each key of
    /// `phrase`, that key's last word alone or with `s` or `es` added. Every block in which
    /// [`find_phrases`](rdy_model::entity::find_phrases) can find a phrase whose keys meet
    /// `phrase`'s is among them; some others may be. Empty for an empty phrase.
    pub fn blocks_with_words(&self, phrase: &str) -> Vec<BlockId> {
        let ws: Vec<&str> = phrase.split(' ').filter(|w| !w.is_empty()).collect();
        let Some((_, earlier)) = ws.split_last() else {
            return vec![];
        };
        // each word's sets: a block holds the word when one of them holds it
        let mut groups: Vec<Vec<&imbl::OrdSet<BlockId>>> = vec![];
        for w in earlier {
            match self.words.get(*w) {
                Some(set) => groups.push(vec![set]),
                None => return vec![],
            }
        }
        let start = phrase.len() - ws[ws.len() - 1].len();
        let mut forms: BTreeSet<String> = BTreeSet::new();
        for k in phrase_keys(phrase) {
            // a key keeps the earlier words: its last word starts where the phrase's does
            let last = &k[start.min(k.len())..];
            for end in ["", "s", "es"] {
                forms.insert(format!("{last}{end}"));
            }
        }
        let lasts: Vec<&imbl::OrdSet<BlockId>> = forms
            .iter()
            .filter_map(|f| self.words.get(f.as_str()))
            .collect();
        if lasts.is_empty() {
            return vec![];
        }
        groups.push(lasts);
        groups.sort_by_key(|g| g.iter().map(|s| s.len()).sum::<usize>());
        let Some((first, rest)) = groups.split_first() else {
            return vec![];
        };
        let found: BTreeSet<&BlockId> = first.iter().flat_map(|s| s.iter()).collect();
        found
            .into_iter()
            .filter(|id| rest.iter().all(|g| g.iter().any(|s| s.contains(*id))))
            .cloned()
            .collect()
    }

    /// The blocks whose text holds every word of one of `entity`'s name or alias phrases, sorted
    /// by id, without `entity`. A superset of the blocks that name it: the caller checks each with
    /// [`Lexicon::mentions`]. Empty for a block that is no entity.
    pub fn naming(&self, entity: &BlockId) -> Vec<BlockId> {
        let Some(names) = self.names.get(entity) else {
            return vec![];
        };
        let mut out: BTreeSet<BlockId> = BTreeSet::new();
        for (kind, p) in names {
            if *kind != NameKind::Key {
                out.extend(self.blocks_with_words(p));
            }
        }
        out.remove(entity);
        out.into_iter().collect()
    }

    /// The distinct code literals and the distinct folded words of `b`'s text sources.
    fn terms_of(&self, b: &Block) -> (BTreeSet<String>, BTreeSet<String>) {
        let sources = self.sources(b);
        let literals = sources
            .iter()
            .flat_map(|(_, text)| code_literals(text))
            .collect();
        let folded = sources
            .iter()
            .flat_map(|(_, text)| words(text))
            .map(|w| w.folded)
            .collect();
        (literals, folded)
    }

    fn add(&mut self, b: &Block) {
        self.add_names(b);
        let (literals, words) = self.terms_of(b);
        for lit in literals {
            self.literals.entry(lit).or_default().insert(b.id.clone());
        }
        for w in words {
            self.words.entry(w).or_default().insert(b.id.clone());
        }
    }

    /// The key, name and alias phrases of `b` when it is an entity, filed under their keys, and
    /// whether it is everyday.
    fn add_names(&mut self, b: &Block) {
        if self.is_entity_type(&b.block_type) {
            let names = Lexicon::names_of(b);
            for (kind, p) in &names {
                for k in keys_of(*kind, p) {
                    self.phrases
                        .entry(k.to_string())
                        .or_default()
                        .insert((b.id.clone(), *kind));
                }
            }
            self.names.insert(b.id.clone(), names);
            if b.fields.get(EVERYDAY) == Some(&FieldValue::Bool(true)) {
                self.everyday.insert(b.id.clone());
            }
        }
    }

    fn remove(&mut self, b: &Block) {
        if let Some(names) = self.names.remove(&b.id) {
            for (kind, p) in &names {
                for k in keys_of(*kind, p) {
                    if let Some(set) = self.phrases.get_mut(k) {
                        set.remove(&(b.id.clone(), *kind));
                        if set.is_empty() {
                            self.phrases.remove(k);
                        }
                    }
                }
            }
        }
        self.everyday.remove(&b.id);
        let (literals, words) = self.terms_of(b);
        for (map, terms) in [(&mut self.literals, literals), (&mut self.words, words)] {
            for t in terms {
                if let Some(set) = map.get_mut(&t) {
                    set.remove(&b.id);
                    if set.is_empty() {
                        map.remove(&t);
                    }
                }
            }
        }
    }
}

/// Where the words at `range` of `stored`, the stored text of leaf `field`, are: the one-based
/// line of `range.start`, the range on that line (its end cut at the line's end, before any
/// `\r\n` or `\n`), and the words as stored.
fn located(field: &str, stored: &str, range: std::ops::Range<usize>) -> MentionText {
    let mut line = 1;
    let mut start = 0;
    let mut content = stored.len();
    for (n, (at, text)) in lines_at(stored).enumerate() {
        if at > range.start {
            break;
        }
        line = n + 1;
        start = at;
        content = at + text.len();
    }
    let words = stored.get(range.clone()).unwrap_or_default().to_string();
    MentionText {
        field: field.to_string(),
        line,
        span: [range.start - start, range.end.min(content) - start],
        words,
    }
}

/// The keys a phrase of `kind` is filed under: a key's phrase only, a name's or an alias's
/// [`phrase_keys`].
fn keys_of(kind: NameKind, p: &str) -> impl Iterator<Item = &str> {
    let exact = (kind == NameKind::Key).then_some(p);
    let folded = (kind != NameKind::Key).then(|| phrase_keys(p));
    exact.into_iter().chain(folded.into_iter().flatten())
}

/// Whether a key's entries name some entity by name or alias.
fn named_by_name(set: &imbl::OrdSet<(BlockId, NameKind)>) -> bool {
    set.iter().any(|(_, k)| *k != NameKind::Key)
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
    count_scan();
    without(text_sources(b), |f| is_enum_field(pack, &b.block_type, f))
}

/// Whether `pack` gives `field` of `block_type` kind `enum`: a fixed list, never read as text.
pub(crate) fn is_enum_field(pack: &Pack, block_type: &str, field: &str) -> bool {
    pack.block_types
        .get(block_type)
        .and_then(|d| d.fields.get(field))
        .is_some_and(|fd| fd.kind == FieldKind::Enum)
}

/// The text of `b` as a reader sees it, under `pack`: the fields of [`text_in`], with each
/// reference shown the way `rdy_read` shows it (its bare token), never blanked. Not for a
/// mention scan.
pub fn shown_text_in(pack: &Pack, b: &Block) -> Vec<(String, String)> {
    without(shown_sources(b), |f| is_enum_field(pack, &b.block_type, f))
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
