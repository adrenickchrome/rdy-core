//! FW-PR1: deterministic search over block text. A literal or a regex, matched line by line in
//! each block's body and string fields, in reading order, with lines windowed and capped per
//! block, pages cut by a token budget and a stateless cursor that a changed match set stales.
use crate::error::{E_BAD_INPUT, E_LIMIT, E_PAGE_STALE, E_UNKNOWN_BLOCK, PAGE_STALE_FIX, RdyError};
use crate::materialize::Materialized;
use crate::{ReadState, Scope};
use rdy_graph::EdgeKind;
use rdy_model::decision::{AFFECTS, DECIDED_BY, DECISION, DESCRIBES, IMPLEMENTS};
use rdy_model::{Block, BlockId, FieldValue, Hash};
use rdy_schema::Trait;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// The longest query, in bytes, before a literal is escaped.
pub const MAX_PATTERN_BYTES: usize = 1024;
/// The compiled program limit, in bytes.
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// The lazy-DFA cache limit of one search, in bytes. A short pattern with large counted repeats
/// needs many DFA states; with too small a cache the regex crate falls back to a much slower
/// engine on long lines.
pub const REGEX_DFA_LIMIT: usize = 16 << 20;
/// The deepest nesting of groups and repeats a pattern may have.
pub const REGEX_NEST_LIMIT: u32 = 64;
/// Matching lines shown per block; the rest are counted in `more_lines`.
pub const LINES_PER_MATCH: usize = 10;
/// Characters of a long line shown around its first match.
pub const LINE_CHARS: usize = 240;
/// The token budget of a page when the caller names none.
pub const DEFAULT_SEARCH_TOKENS: usize = 4000;
/// Doc paths listed in a result's totals; the rest are counted in `more_docs`.
pub const TOTALS_DOCS: usize = 50;

/// Characters shown before the first match of a windowed line.
const LEAD_CHARS: usize = 80;
const TOKEN_DOMAIN: &[u8] = b"rdy-search-1\0";
/// The bytes one search may rescan after empty matches. After an empty match the regex search
/// starts again one character on, and it can scan to the end of the line, so a pattern that
/// matches empty text costs a line's length squared. The first restart on a line is free (it costs
/// at most one more pass, as the first search does); each later one is charged the rest of the line.
/// The charge is the worst case, so a pattern that matches empty text at each position of a line
/// over about 8 KiB (half its length squared passes the limit) is refused, even a cheap one like
/// `q*`. A cheaper estimate needs the `regex-automata` edge.
const EMPTY_RESCAN_LIMIT: usize = 32 << 20;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub ignore_case: bool,
    /// Every given filter must hold (AND). A block that fails one is not text-matched at all.
    #[serde(default)]
    pub filters: SearchFilters,
}

/// sflt1 (FW-PR1a, FW-SR4): which blocks a search looks at. Within `types` and within `traits`
/// any listed value matches; every other filter given must also hold.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchFilters {
    /// The block's type is one of these (keys of the pack's block types).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    /// The block's type holds at least one of these traits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traits: Vec<String>,
    /// The block sits in this doc or folder (whole path segments, case-sensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `draft`, `stable` or `deprecated`: the block's own or nearest ancestor's lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    /// A version, `tag:<version>` or `landing:<id>`: the block changed since that landing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,
    /// Drop matches inside a superseded, rejected or revoked decision.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclude_history: bool,
    /// A decision (id or key): blocks it governs (decided_by, implements, affects).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governed_by: Option<String>,
    /// A block (id, key or path): blocks that `describes`-link to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub describes: Option<String>,
    /// A block (id, key or path): blocks it links or references, or that link or reference it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_to: Option<String>,
    /// Only blocks with an open suspect.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suspect: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchResult {
    pub query: String,
    pub regex: bool,
    pub ignore_case: bool,
    /// Matching blocks over the whole search, every page.
    pub total: usize,
    /// stot1 (FW-SR3): hits per doc path and per block type over the whole search, every page.
    pub totals: SearchTotals,
    /// One page of entries; identical hits share an entry (`also`).
    pub matches: Vec<SearchMatch>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Entries after this page.
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// The cursor of the next page, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchMatch {
    pub id: String,
    #[serde(rename = "type")]
    pub block_type: String,
    pub path: Option<String>,
    pub lines: Vec<SearchLine>,
    /// Matching lines past `LINES_PER_MATCH`.
    #[serde(skip_serializing_if = "is_zero")]
    pub more_lines: usize,
    /// The block sits in a superseded, rejected or revoked decision.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history: bool,
    /// sord1 (FW-SR1): no line holds the phrase; the block holds every word of it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub all_words: bool,
    /// scol1 (FW-SR3): later hits with the same type, lines, `more_lines`, `all_words` and
    /// `history`, in result order. Never cut.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<SearchAlso>,
}

/// A hit folded into an earlier entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchAlso {
    pub id: String,
    pub path: Option<String>,
}

/// Hits over the whole search: per doc path (most hits first, at most `TOTALS_DOCS`) and per
/// block type.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SearchTotals {
    pub docs: Vec<DocHits>,
    pub types: BTreeMap<String, usize>,
    /// Doc paths left out of `docs`.
    #[serde(skip_serializing_if = "is_zero")]
    pub more_docs: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocHits {
    pub path: Option<String>,
    pub hits: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct SearchLine {
    /// `body`, or a field name with `[i]` for a list item and `.key` for a map entry.
    pub field: String,
    /// 1-based, per source.
    pub line: usize,
    pub text: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl ReadState {
    /// bsrch1: the blocks in `scope` whose text matches `q`, one page of them.
    pub fn search(
        &self,
        scope: &Scope,
        q: &SearchQuery,
        page: Option<&str>,
        max_tokens: Option<usize>,
    ) -> Result<SearchResult, RdyError> {
        let (re, words) = compile(q)?;
        let m = self.materialized(scope)?;
        let plan = self.filter_plan(scope, &m, &q.filters)?;
        let cursor = page.map(parse_page).transpose()?;
        let graph = self.graph(scope)?;
        let history_of = |b: &Block| -> bool {
            let mut cur = Some(b);
            for _ in 0..m.len() {
                let Some(c) = cur else { break };
                if c.block_type == DECISION
                    && graph
                        .status(&c.id)
                        .is_some_and(|s| matches!(s.tag(), "superseded" | "rejected" | "revoked"))
                {
                    return true;
                }
                cur = c.parent.as_ref().and_then(|p| m.get(p));
            }
            false
        };

        // phrase hits, then all-words hits, each in reading order
        let mut phrase: Vec<Hit> = vec![];
        let mut all_words: Vec<Hit> = vec![];
        // reading order: depth-first from the top level; a parent loop cannot spin. Each entry
        // carries the lifecycle its parent passes down.
        let mut stack: Vec<(&Block, Option<&str>)> = m
            .children(None)
            .into_iter()
            .rev()
            .map(|b| (b, None))
            .collect();
        let mut rescan = 0;
        for _ in 0..m.len() {
            let Some((b, inherited)) = stack.pop() else {
                break;
            };
            let own = match b.fields.get("lifecycle") {
                Some(FieldValue::Str(l)) => Some(l.as_str()),
                _ => inherited,
            };
            if plan.passes(&m, b, own) {
                // history is cheap to know before the text pass, so exclusion skips the match
                let early = q.filters.exclude_history.then(|| history_of(b));
                if early != Some(true)
                    && let Some((lines, more_lines, words)) =
                        match_block(&re, &words, b, &mut rescan)?
                {
                    let hit = Hit {
                        block: b,
                        lines,
                        more_lines,
                        history: early.unwrap_or_else(|| history_of(b)),
                    };
                    if words {
                        all_words.push(hit);
                    } else {
                        phrase.push(hit);
                    }
                }
            }
            stack.extend(m.children(Some(&b.id)).into_iter().rev().map(|c| (c, own)));
        }

        let mut token = TOKEN_DOMAIN.to_vec();
        token.extend_from_slice(q.text.as_bytes());
        token.push(0);
        token.push(u8::from(q.regex));
        token.push(u8::from(q.ignore_case));
        token.extend(serde_json::to_vec(&q.filters).unwrap_or_default());
        token.push(0);
        let words_from = phrase.len();
        let hits: Vec<Hit> = phrase.into_iter().chain(all_words).collect();
        // every collapse-key input the version does not cover goes in too: `history` comes from
        // other decisions, so a supersede can regroup the entries without moving any version
        for (i, h) in hits.iter().enumerate() {
            let version = m
                .version_of(&h.block.id)
                .unwrap_or_else(|| h.block.version_hash());
            token.extend_from_slice(h.block.id.as_str().as_bytes());
            token.push(0);
            token.extend_from_slice(&version.0);
            token.push(u8::from(h.history));
            token.push(u8::from(i >= words_from));
        }
        let token = Hash::of(&token).to_string();

        // scol1: a hit identical to an earlier entry joins it; entries keep their first position
        let total = hits.len();
        let mut entries: Vec<SearchMatch> = vec![];
        let mut seen: HashMap<EntryKey, usize> = HashMap::new();
        let mut types: BTreeMap<String, usize> = BTreeMap::new();
        let mut docs: HashMap<Option<String>, usize> = HashMap::new();
        for (i, h) in hits.into_iter().enumerate() {
            let path = m.path_of(&h.block.id);
            *types.entry(h.block.block_type.clone()).or_default() += 1;
            *docs.entry(path.clone()).or_default() += 1;
            let key = EntryKey {
                block_type: h.block.block_type.clone(),
                lines: h.lines,
                more_lines: h.more_lines,
                all_words: i >= words_from,
                history: h.history,
            };
            if let Some(&at) = seen.get(&key) {
                entries[at].also.push(SearchAlso {
                    id: h.block.id.to_string(),
                    path,
                });
                continue;
            }
            entries.push(SearchMatch {
                id: h.block.id.to_string(),
                block_type: key.block_type.clone(),
                path,
                lines: key.lines.clone(),
                more_lines: key.more_lines,
                history: key.history,
                all_words: key.all_words,
                also: vec![],
            });
            seen.insert(key, entries.len() - 1);
        }
        let totals = totals_of(types, docs);

        let count = entries.len();
        let offset = match cursor {
            None => 0,
            Some((t, _)) if t != token => {
                return Err(RdyError::new(
                    E_PAGE_STALE,
                    "the matches behind this page changed since the first page, or the query differs",
                    PAGE_STALE_FIX,
                ));
            }
            Some((_, offset)) if offset >= count => return Err(bad_page()),
            Some((_, offset)) => offset,
        };

        // the totals come first, so they take their share of the budget
        let budget = max_tokens
            .unwrap_or(DEFAULT_SEARCH_TOKENS)
            .saturating_sub(tokens_of(&totals));
        let mut used = 0;
        let mut matches = vec![];
        for entry in entries.into_iter().skip(offset) {
            let cost = tokens_of(&entry);
            if !matches.is_empty() && used + cost > budget {
                break;
            }
            used += cost;
            matches.push(entry);
        }
        let end = offset + matches.len();
        let omitted = count - end;
        Ok(SearchResult {
            query: q.text.clone(),
            regex: q.regex,
            ignore_case: q.ignore_case,
            total,
            totals,
            matches,
            truncated: omitted > 0,
            omitted,
            next: (omitted > 0).then(|| format!("{token}:{end}")),
        })
    }
}

/// One matching block before collapse.
struct Hit<'a> {
    block: &'a Block,
    lines: Vec<SearchLine>,
    more_lines: usize,
    history: bool,
}

/// What two hits share to share an entry.
#[derive(PartialEq, Eq, Hash)]
struct EntryKey {
    block_type: String,
    lines: Vec<SearchLine>,
    more_lines: usize,
    all_words: bool,
    history: bool,
}

/// A page's measure of a value: its JSON length / 4, plus 1.
fn tokens_of(value: &impl Serialize) -> usize {
    serde_json::to_string(value).map_or(0, |s| s.len()) / 4 + 1
}

/// stot1: docs by hits descending, then path ascending with no path last; at most `TOTALS_DOCS`.
fn totals_of(types: BTreeMap<String, usize>, docs: HashMap<Option<String>, usize>) -> SearchTotals {
    let mut docs: Vec<DocHits> = docs
        .into_iter()
        .map(|(path, hits)| DocHits { path, hits })
        .collect();
    docs.sort_by(|a, b| {
        b.hits.cmp(&a.hits).then_with(|| match (&a.path, &b.path) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        })
    });
    let more_docs = docs.len().saturating_sub(TOTALS_DOCS);
    docs.truncate(TOTALS_DOCS);
    SearchTotals {
        docs,
        types,
        more_docs,
    }
}

/// The filters of one search, checked and resolved against its scope.
struct FilterPlan {
    types: Vec<String>,
    traits: Vec<Trait>,
    path: Option<String>,
    lifecycle: Option<String>,
    /// canon as of the `changed_since` landing
    since: Option<Arc<Materialized>>,
    /// the blocks each graph filter keeps, intersected
    allowed: Vec<BTreeSet<BlockId>>,
    pack_traits: Vec<(String, Vec<Trait>)>,
}

impl FilterPlan {
    /// Whether `b` holds every filter. `lifecycle` is the block's own or inherited lifecycle.
    fn passes(&self, m: &Materialized, b: &Block, lifecycle: Option<&str>) -> bool {
        if !self.types.is_empty() && !self.types.contains(&b.block_type) {
            return false;
        }
        if !self.traits.is_empty() {
            let held = self
                .pack_traits
                .iter()
                .find(|(t, _)| *t == b.block_type)
                .is_some_and(|(_, held)| self.traits.iter().any(|t| held.contains(t)));
            if !held {
                return false;
            }
        }
        if let Some(prefix) = &self.path {
            let hit = m.path_of(&b.id).is_some_and(|p| {
                p == *prefix
                    || p.strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            });
            if !hit {
                return false;
            }
        }
        if let Some(want) = &self.lifecycle
            && lifecycle.unwrap_or("draft") != want
        {
            return false;
        }
        if let Some(old) = &self.since
            && m.version_of(&b.id) == old.version_of(&b.id)
        {
            return false;
        }
        self.allowed.iter().all(|set| set.contains(&b.id))
    }
}

const TRAIT_NAMES: [(&str, Trait); 6] = [
    ("normative", Trait::Normative),
    ("informative", Trait::Informative),
    ("defines", Trait::Defines),
    ("describes", Trait::Describes),
    ("asset", Trait::Asset),
    ("structural", Trait::Structural),
];

impl ReadState {
    /// Checks the filters in their declared order and resolves what a block test needs.
    fn filter_plan(
        &self,
        scope: &Scope,
        m: &Materialized,
        f: &SearchFilters,
    ) -> Result<FilterPlan, RdyError> {
        let pack = self.pack();
        for t in &f.types {
            if !pack.block_types.contains_key(t) {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("types: no block type {t:?} in pack {}", pack.name),
                    "rdy_schema lists the block types",
                ));
            }
        }
        let mut traits = vec![];
        for t in &f.traits {
            let Some((_, held)) = TRAIT_NAMES.iter().find(|(n, _)| n == t) else {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("traits: {t:?} is not a trait"),
                    "use normative, informative, defines, describes, asset or structural",
                ));
            };
            traits.push(*held);
        }
        let path = match &f.path {
            None => None,
            Some(p) => {
                // a leading slash is the project root, a trailing one the folder itself
                let p = p.trim_matches('/');
                if p.is_empty() {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        "path is empty",
                        "pass a doc or folder path, e.g. guides",
                    ));
                }
                Some(p.to_string())
            }
        };
        if let Some(l) = &f.lifecycle
            && !matches!(l.as_str(), "draft" | "stable" | "deprecated")
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("lifecycle: {l:?} is not draft, stable or deprecated"),
                "pass draft, stable or deprecated",
            ));
        }
        let since = match &f.changed_since {
            None => None,
            Some(t) => Some(self.materialized(&self.since_scope(t)?)?),
        };
        let g = self.graph(scope)?;
        let mut allowed = vec![];
        if let Some(text) = &f.governed_by {
            let id = self.resolve_id(scope, text)?;
            let d = block_of(m, &id)?;
            if d.block_type != DECISION {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("governed_by {text:?} is a {}, not a decision", d.block_type),
                    "pass a decision's id or key",
                ));
            }
            let mut set: BTreeSet<BlockId> = g
                .incoming(&id)
                .iter()
                .filter(
                    |e| matches!(&e.kind, EdgeKind::Link(n) if n == DECIDED_BY || n == IMPLEMENTS),
                )
                .map(|e| e.from.clone())
                .collect();
            set.extend(
                g.outgoing(&id)
                    .iter()
                    .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == AFFECTS))
                    .map(|e| e.to.clone()),
            );
            allowed.push(set);
        }
        if let Some(text) = &f.describes {
            let id = self.resolve_id(scope, text)?;
            block_of(m, &id)?;
            allowed.push(
                g.incoming(&id)
                    .iter()
                    .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == DESCRIBES))
                    .map(|e| e.from.clone())
                    .collect(),
            );
        }
        if let Some(text) = &f.linked_to {
            let id = self.resolve_id(scope, text)?;
            block_of(m, &id)?;
            let mut set: BTreeSet<BlockId> = g
                .incoming(&id)
                .iter()
                .filter(|e| e.kind != EdgeKind::Contains)
                .map(|e| e.from.clone())
                .collect();
            set.extend(
                g.outgoing(&id)
                    .iter()
                    .filter(|e| e.kind != EdgeKind::Contains)
                    .map(|e| e.to.clone()),
            );
            set.remove(&id);
            allowed.push(set);
        }
        if f.suspect {
            allowed.push(self.suspects(scope)?.into_iter().map(|s| s.block).collect());
        }
        let pack_traits = pack
            .block_types
            .iter()
            .map(|(n, d)| (n.clone(), d.traits.iter().copied().collect()))
            .collect();
        Ok(FilterPlan {
            types: f.types.clone(),
            traits,
            path,
            lifecycle: f.lifecycle.clone(),
            since,
            allowed,
            pack_traits,
        })
    }

    /// The canon scope a `changed_since` text names: `tag:` or `landing:` as given, else a bare
    /// version (a tag), else a bare landing id.
    fn since_scope(&self, t: &str) -> Result<Scope, RdyError> {
        let prefixed = t.starts_with("tag:") || t.starts_with("landing:");
        let tries = if prefixed {
            vec![t.to_string()]
        } else {
            vec![format!("tag:{t}"), format!("landing:{t}")]
        };
        for text in tries {
            if let Ok(at @ Scope::At(_)) = self.resolve_scope(&text) {
                return Ok(at);
            }
        }
        Err(RdyError::new(
            E_BAD_INPUT,
            format!("changed_since {t:?} names no version, tag or landing"),
            "pass a version (rdy tags lists them), tag:<version> or landing:<id>",
        ))
    }
}

fn block_of<'a>(m: &'a Materialized, id: &BlockId) -> Result<&'a Block, RdyError> {
    m.get(id).ok_or_else(|| {
        RdyError::new(
            E_UNKNOWN_BLOCK,
            format!("no block {id}"),
            "check the id with rdy_read",
        )
    })
}

/// The query as a bounded regex, and, for a literal of two or more distinct words, each word as
/// one (sord1). A literal is escaped first.
fn compile(q: &SearchQuery) -> Result<(Regex, Vec<Regex>), RdyError> {
    if q.text.is_empty() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "query is empty",
            "pass the text to find",
        ));
    }
    if q.text.len() > MAX_PATTERN_BYTES {
        return Err(RdyError::new(
            E_LIMIT,
            format!(
                "query is {} bytes; the limit is {MAX_PATTERN_BYTES}",
                q.text.len()
            ),
            "search for a shorter text or pattern",
        ));
    }
    let pattern = if q.regex {
        q.text.clone()
    } else {
        regex::escape(&q.text)
    };
    let phrase = build(&pattern, q.ignore_case)?;
    let mut distinct: Vec<&str> = vec![];
    if !q.regex {
        for w in q.text.split_whitespace() {
            if !distinct.contains(&w) {
                distinct.push(w);
            }
        }
    }
    let words = if distinct.len() >= 2 {
        distinct
            .into_iter()
            .map(|w| build(&regex::escape(w), q.ignore_case))
            .collect::<Result<_, _>>()?
    } else {
        vec![]
    };
    Ok((phrase, words))
}

/// One bounded regex; its errors as rdy errors.
fn build(pattern: &str, ignore_case: bool) -> Result<Regex, RdyError> {
    RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_LIMIT)
        .nest_limit(REGEX_NEST_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => RdyError::new(
                E_LIMIT,
                "the pattern compiles too large (over 1 MiB)",
                "simplify the pattern, e.g. fewer or smaller counted repeats",
            ),
            other => {
                let text = other.to_string();
                let last = text
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim();
                let detail = last.strip_prefix("error: ").unwrap_or(last);
                RdyError::new(
                    E_BAD_INPUT,
                    format!("query is not a valid pattern: {detail}"),
                    "fix the pattern, or search for the text literally (regex: false)",
                )
            }
        })
}

/// AE1's cursor grammar: 64 lowercase hex characters, `:`, 1 to 9 ASCII digits.
fn parse_page(text: &str) -> Result<(String, usize), RdyError> {
    let (token, offset) = text.split_once(':').ok_or_else(bad_page)?;
    token.parse::<Hash>().map_err(|_| bad_page())?;
    if !(1..=9).contains(&offset.len()) || !offset.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad_page());
    }
    let offset = offset.parse().map_err(|_| bad_page())?;
    Ok((token.to_string(), offset))
}

fn bad_page() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "page is not a cursor from rdy_search's next",
        "pass next from the previous page, unchanged",
    )
}

/// The block's matching lines (at most `LINES_PER_MATCH`), how many more matched, and whether it
/// is an all-words hit; `None` when it is no hit. A phrase hit has a line with a non-empty match of
/// `re`. Else, with `words`, an all-words hit has a match of every word somewhere in its text and
/// shows the lines that hold any word. Sources: the body, then fields by name. `rescan` carries
/// the search's empty-match charge from block to block.
fn match_block(
    re: &Regex,
    words: &[Regex],
    b: &Block,
    rescan: &mut usize,
) -> Result<Option<(Vec<SearchLine>, usize, bool)>, RdyError> {
    let mut lines = vec![];
    let mut more = 0;
    each_line(b, &mut |field, i, line| {
        if let Some(at) = first_match(re, line, rescan)? {
            keep(&mut lines, &mut more, field, i, line, at);
        }
        Ok(())
    })?;
    if !lines.is_empty() {
        return Ok(Some((lines, more, false)));
    }
    // a word holds no line break, so a source matches it exactly when one of its lines does; most
    // blocks lack a word, and this check leaves them after one pass per word at most
    if words.is_empty() || !words.iter().all(|w| any_source(b, |text| w.is_match(text))) {
        return Ok(None);
    }
    let mut held = vec![false; words.len()];
    each_line(b, &mut |field, i, line| {
        let mut first: Option<usize> = None;
        for (w, re) in words.iter().enumerate() {
            // a word is a non-empty literal, so every match is non-empty
            if let Some(m) = re.find(line) {
                held[w] = true;
                first = Some(first.map_or(m.start(), |f| f.min(m.start())));
            }
        }
        if let Some(at) = first {
            keep(&mut lines, &mut more, field, i, line, at);
        }
        Ok(())
    })?;
    Ok(held.iter().all(|h| *h).then_some((lines, more, true)))
}

/// Shows `line` (number `i`, 0-based) windowed at byte `at`, or counts it past the cap.
fn keep(
    lines: &mut Vec<SearchLine>,
    more: &mut usize,
    field: &str,
    i: usize,
    line: &str,
    at: usize,
) {
    if lines.len() < LINES_PER_MATCH {
        lines.push(SearchLine {
            field: field.to_string(),
            line: i + 1,
            text: window(line, at),
        });
    } else {
        *more += 1;
    }
}

/// Whether `f` holds for one source of the block's text (the body, then fields by name).
fn any_source(b: &Block, f: impl Fn(&str) -> bool) -> bool {
    if f(&b.body) {
        return true;
    }
    let mut found = false;
    for (name, value) in &b.fields {
        strings(name.clone(), value, &mut |_, text| found = found || f(text));
        if found {
            return true;
        }
    }
    false
}

/// Calls `f` with each line of the block's text (source label, 0-based line number, line): the
/// body, then fields by name. Stops at the first error.
fn each_line(
    b: &Block,
    f: &mut impl FnMut(&str, usize, &str) -> Result<(), RdyError>,
) -> Result<(), RdyError> {
    let mut failed = None;
    let mut source = |field: &str, text: &str| {
        if failed.is_some() {
            return;
        }
        for (i, line) in text.lines().enumerate() {
            if let Err(e) = f(field, i, line) {
                failed = Some(e);
                return;
            }
        }
    };
    source("body", &b.body);
    for (name, value) in &b.fields {
        strings(name.clone(), value, &mut source);
    }
    failed.map_or(Ok(()), Err)
}

/// The byte offset of the first non-empty match in `line`. Each restart after an empty match,
/// except the line's first, adds the rest of the line to `rescan`; past `EMPTY_RESCAN_LIMIT` the
/// search is `E_LIMIT`.
fn first_match(re: &Regex, line: &str, rescan: &mut usize) -> Result<Option<usize>, RdyError> {
    let mut restarted = false;
    for m in re.find_iter(line) {
        if !m.is_empty() {
            return Ok(Some(m.start()));
        }
        if std::mem::replace(&mut restarted, true) {
            *rescan += line.len() - m.end();
            if *rescan > EMPTY_RESCAN_LIMIT {
                return Err(RdyError::new(
                    E_LIMIT,
                    "the pattern can match empty text, and searching past empty matches on a line over about 8 KiB can cost too much: rdy refuses it in this scope",
                    "make the pattern match at least one character, e.g. x+ rather than x*",
                ));
            }
        }
    }
    Ok(None)
}

/// Every `Str` leaf of `value`, labelled `name`, `name[i]` or `name.key`.
fn strings(label: String, value: &FieldValue, out: &mut impl FnMut(&str, &str)) {
    match value {
        FieldValue::Str(s) => out(&label, s),
        FieldValue::List(items) => {
            for (i, item) in items.iter().enumerate() {
                strings(format!("{label}[{i}]"), item, out);
            }
        }
        FieldValue::Map(entries) => {
            for (k, item) in entries {
                strings(format!("{label}.{k}"), item, out);
            }
        }
        FieldValue::Null | FieldValue::Bool(_) | FieldValue::Int(_) => {}
    }
}

/// `line` whole when it has at most `LINE_CHARS` characters; else `LINE_CHARS` characters from
/// `LEAD_CHARS` before the match at byte `at`, with `…` where text is cut.
fn window(line: &str, at: usize) -> String {
    let len = line.chars().count();
    if len <= LINE_CHARS {
        return line.to_string();
    }
    let start = line[..at].chars().count().saturating_sub(LEAD_CHARS);
    let end = (start + LINE_CHARS).min(len);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(line.chars().skip(start).take(end - start));
    if end < len {
        out.push('…');
    }
    out
}
