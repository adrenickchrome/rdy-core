//! FW-PR1: deterministic search over block text. A literal or a regex, matched line by line in
//! each block's body and string fields, in reading order, with lines windowed and capped per
//! block, pages cut by a token budget and a stateless cursor that a changed match set stales.
use crate::coverage::{CoverageIssues, Level, confidence_level_of, coverage_error_text, rollup};
use crate::error::{E_BAD_INPUT, E_LIMIT, E_PAGE_STALE, E_UNKNOWN_BLOCK, PAGE_STALE_FIX, RdyError};
use crate::factsheet::{FactSheet, MAX_RESOLVED};
use crate::lexicon::{NameKind, link_kind_for, tied};
use crate::materialize::Materialized;
use crate::suggest::{Suggestion, suggestion_id};
use crate::tensions::index::is_deprecated;
use crate::vectors::{
    CANDIDATE_RECORD_BYTES, candidates, round3, score_operands, select_seeds, tf,
};
use crate::vectors::{
    Doc, Operand, VectorBudget, VectorWeights, WeightScope, WordCounts, reasons_of_operands,
};
use crate::{ReadState, Scope, now_ms};
use rdy_graph::{EdgeKind, GoverningCache, GoverningStatuses};
use rdy_model::decision::{DECISION, DESCRIBES};
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
/// vxs7 (D-76Y9H, D-5VGAS): the longest `like_text`, in bytes.
pub const MAX_LIKE_TEXT_BYTES: usize = 16_384;
/// vxs7 (D-76Y9H): the lowest exact score a `like` or `like_text` hit has.
pub const LIKE_FLOOR: f64 = 0.10;

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

/// vxs7 (D-76Y9H, D-5VGAS): what one search starts from: a text to match, a block to rank the
/// scope against (`like`), or a text to rank it against (`like_text`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchSource {
    Query(String),
    Like(String),
    LikeText(String),
}

/// vxs7: one search as every transport gives it: exactly one source, the matching flags and the
/// filters. [`SearchInput::new`] checks the source refusals a transport can check alone;
/// [`ReadState::search_input`] checks them again with the ones that read the scope.
#[derive(Clone, Debug)]
pub struct SearchInput {
    pub source: SearchSource,
    pub regex: bool,
    pub ignore_case: bool,
    pub filters: SearchFilters,
}

impl SearchInput {
    /// The input of the transport options `query`, `like` and `like_text` (a blank value counts
    /// as given), with the source refusals 1, 2, 3 and 5 of D-5VGAS, in that order.
    pub fn new(
        query: Option<String>,
        like: Option<String>,
        like_text: Option<String>,
        regex: bool,
        ignore_case: bool,
        filters: SearchFilters,
    ) -> Result<SearchInput, RdyError> {
        let input = SearchInput {
            source: validate_source(query, like, like_text)?,
            regex,
            ignore_case,
            filters,
        };
        input.check_mode()?;
        Ok(input)
    }

    /// D-5VGAS refusals 2, 3 and 5, in order: regex and sort apply to a query only, and a
    /// `like_text` is at most [`MAX_LIKE_TEXT_BYTES`].
    fn check_mode(&self) -> Result<(), RdyError> {
        if matches!(self.source, SearchSource::Query(_)) {
            return Ok(());
        }
        if self.regex {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "regex applies to query only",
                "drop regex",
            ));
        }
        if self.filters.sort.is_some() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "sort applies to query only",
                "drop sort; like and like_text hits come in score order",
            ));
        }
        if let SearchSource::LikeText(text) = &self.source
            && text.len() > MAX_LIKE_TEXT_BYTES
        {
            return Err(RdyError::new(
                E_LIMIT,
                format!("like_text is over {MAX_LIKE_TEXT_BYTES} bytes"),
                "send a shorter like_text",
            ));
        }
        Ok(())
    }
}

impl From<SearchQuery> for SearchInput {
    fn from(q: SearchQuery) -> SearchInput {
        SearchInput {
            source: SearchSource::Query(q.text),
            regex: q.regex,
            ignore_case: q.ignore_case,
            filters: q.filters,
        }
    }
}

/// D-5VGAS refusal 1: exactly one of `query`, `like` and `like_text`.
pub fn validate_source(
    query: Option<String>,
    like: Option<String>,
    like_text: Option<String>,
) -> Result<SearchSource, RdyError> {
    match (query, like, like_text) {
        (Some(q), None, None) => Ok(SearchSource::Query(q)),
        (None, Some(id), None) => Ok(SearchSource::Like(id)),
        (None, None, Some(text)) => Ok(SearchSource::LikeText(text)),
        _ => Err(RdyError::new(
            E_BAD_INPUT,
            "give exactly one of query, like or like_text",
            "send one of query, like or like_text",
        )),
    }
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
    /// Only blocks involved in an open tension (rdy_tensions lists them).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tension: bool,
    /// linked or unlinked: blocks that name an entity (by name or alias, whole words) and are tied to it (linked), or that name one they are not tied to yet (unlinked). Only blocks that can link to an entity count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentions: Option<String>,
    /// Only blocks whose coverage score is below this (0 to 100). The current state only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_below: Option<u32>,
    /// `coverage`: list the lowest block score first, the default order between equal scores.
    /// `relevance` (D-76Y9H): inside the phrase tier and the all-words tier, the highest lexical
    /// score against the query first, then reading order. `reading`: the default order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchResult {
    pub query: String,
    pub regex: bool,
    pub ignore_case: bool,
    /// vxs7 (D-5VGAS): the `like` block, echoed; `query` is then empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub like: Option<String>,
    /// vxs7 (D-5VGAS): the `like_text`, echoed; `query` is then empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub like_text: Option<String>,
    /// esrch1 (FW-EN1, FW-SR1): on the first page, the fact sheets of the entities a literal
    /// query names (its key, name or alias), at most `MAX_RESOLVED`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<FactSheet>,
    /// Named entities past `MAX_RESOLVED`.
    #[serde(skip_serializing_if = "is_zero")]
    pub entities_omitted: usize,
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
    /// Why the hits' coverage could not be read, as `<code>: <message>` (the hits stand, without
    /// coverage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_error: Option<String>,
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
    /// vxs6 (D-76Y9H): with `sort: relevance`, the block's lexical score against the query,
    /// rounded to three decimals. Absent in every other order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// vxs7 (D-76Y9H): in a `like` mode, up to five words the hit shares with the source, by
    /// weight product. Absent in every other mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<String>>,
    /// vxs7: in a `like` mode, up to five link targets the hit shares with the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    /// scol1 (FW-SR3): later hits with the same type, lines, `more_lines`, `all_words` and
    /// `history`, in result order. Never cut. A ranked entry stands in its best member's place
    /// (D-5VGAS).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<SearchAlso>,
    /// esrch1: a link from this block to each entity the query names, when the block holds the
    /// query as a phrase, names the entity by name or alias as whole words, and is not tied to
    /// the entity yet.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    /// cc14 (FW-SR1): the block's coverage score, 0 to 100. Absent at a past landing, where
    /// coverage is not computed (FW-H7). A collapsed entry shows its first block's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<u32>,
    /// cc14: the block's coverage issues that are not dismissed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issues: Option<usize>,
    /// cc14 (FW-CV2): how far rdy vouches for the hit; a collapsed entry takes the lowest level
    /// of its blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Level>,
}

/// A hit folded into an earlier entry. It keeps its own suggestions.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchAlso {
    pub id: String,
    pub path: Option<String>,
    /// vxs6 (D-5VGAS): the member's own score, in a ranked order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// vxs7 (D-5VGAS): in a `like` mode, the member's own shared words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<String>>,
    /// vxs7: in a `like` mode, the member's own shared link targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
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
        // vxs6 (D-76Y9H, D-5VGAS): a pattern has no words, and ranking reads the current state
        // only; both refusals come before anything is read
        let relevance = q.filters.sort.as_deref() == Some(SORT_RELEVANCE);
        let weigh = if relevance {
            if q.regex {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    "sort relevance needs a literal query",
                    "drop regex, or drop sort",
                ));
            }
            Some(self.ranked_scope(scope, "sort relevance")?)
        } else {
            None
        };
        let (re, words) = compile(q)?;
        // 3b (esrch1): a literal that names entities starts from them, and reads text the way
        // resolution does (any case, any spacing), so every unlinked mention is a match
        let resolved = if q.regex {
            vec![]
        } else {
            self.resolve_entities(scope, &q.text)?
        };
        let ignore_case = q.ignore_case || !resolved.is_empty();
        let (re, words) = if resolved.is_empty() {
            (re, words)
        } else {
            compile_as(q, true, true)?
        };
        let m = self.materialized(scope)?;
        let plan = self.filter_plan(scope, &m, &q.filters)?;
        let cursor = page.map(parse_page).transpose()?;
        let graph = self.graph(scope)?;
        let history_of = |b: &Block| -> bool { in_history(&m, &graph, b) };

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
                        all_words: words,
                        cov: None,
                        score: None,
                        words: None,
                        links: None,
                        suggestions: vec![],
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

        // 3b (esrch1): the first page shows the sheets of the first `MAX_RESOLVED` resolved
        // entities; every page builds them, so every page drops the same hits.
        let sheets: Vec<FactSheet> = resolved
            .iter()
            .take(MAX_RESOLVED)
            .map(|(e, kind)| self.fact_sheet(scope, e, *kind))
            .collect::<Result<_, _>>()?;
        if !resolved.is_empty() {
            // A hit drops out only when a sheet holds it: the sheet's entity, or a block tied to
            // the entity that one of its lists shows (a history decision is not listed, and a
            // list's cut is not shown). A filtered search keeps every hit: a sheet shows no
            // suspect, change or filter target.
            // (a sort changes the order only, so it does not count as a filter)
            let unfiltered = SearchFilters {
                sort: None,
                ..q.filters.clone()
            };
            if unfiltered == SearchFilters::default() {
                let mut held: BTreeSet<BlockId> = BTreeSet::new();
                for ((e, _), sheet) in resolved.iter().zip(&sheets) {
                    held.insert(e.clone());
                    let listed = sheet
                        .facts
                        .iter()
                        .chain(&sheet.also_described_in)
                        .chain(&sheet.dependents)
                        .map(|b| b.id.as_str())
                        .chain(sheet.decisions.iter().map(|d| d.id.as_str()));
                    for id in listed {
                        if let Ok(id) = BlockId::parse(id)
                            && tied(&graph, &id, e)
                        {
                            held.insert(id);
                        }
                    }
                }
                phrase.retain(|h| !held.contains(&h.block.id));
                all_words.retain(|h| !held.contains(&h.block.id));
            }
            // a link suggestion per entity the hit's text names as whole words (a name or an
            // alias, as a write's suggestions read it) and is not tied to yet
            let pack = self.pack();
            let lexicon = self.lexicon(scope)?;
            for h in &mut phrase {
                let Some(kind) = link_kind_for(pack, h.block) else {
                    continue;
                };
                let mentions = lexicon.mentions(h.block);
                for (e, _) in &resolved {
                    let Some(mention) = mentions.iter().find(|mn| mn.entity == e.as_str()) else {
                        continue;
                    };
                    // a dismissed pair is settled in every scope until a restore (D-E6JD1),
                    // and a deprecated entity is reworded, never linked (D-S46DE)
                    if h.block.id == *e
                        || tied(&graph, &h.block.id, e)
                        || self.mention_dismissed(&h.block.id, e).is_some()
                        || is_deprecated(&m, &graph, e)
                    {
                        continue;
                    }
                    let (block, target) = (h.block.id.to_string(), e.to_string());
                    h.suggestions.push(Suggestion::Link {
                        id: suggestion_id(kind, &block, &target),
                        block,
                        link: kind.to_string(),
                        target,
                        mention: q.text.clone(),
                        field: mention.field.clone(),
                    });
                }
            }
        }

        let mut token = TOKEN_DOMAIN.to_vec();
        token.extend_from_slice(q.text.as_bytes());
        token.push(0);
        token.push(u8::from(q.regex));
        token.push(u8::from(q.ignore_case));
        // `sort: reading` is the default order, so it pages as an absent sort does
        if q.filters.sort.as_deref() == Some(SORT_READING) {
            let reading = SearchFilters {
                sort: None,
                ..q.filters.clone()
            };
            token.extend(serde_json::to_vec(&reading).unwrap_or_default());
        } else {
            token.extend(serde_json::to_vec(&q.filters).unwrap_or_default());
        }
        token.push(0);
        let hits: Vec<Hit> = phrase.into_iter().chain(all_words).collect();
        let rank = match &weigh {
            Some(ws) => Rank::Relevance(self.vector_weights(WeightScope::Read(ws))?, &q.text),
            None => Rank::Matched,
        };
        let head = Head {
            query: q.text.clone(),
            regex: q.regex,
            ignore_case,
            like: None,
            like_text: None,
        };
        self.page_of(
            scope, &m, &q.filters, hits, token, rank, &resolved, sheets, cursor, max_tokens, head,
        )
    }

    /// The page of `hits` (in match order, or scored and in score order for `Rank::Like`) that
    /// `cursor` asks for: coverage, the coverage filter and sort, relevance, the page token
    /// (`token` holds the request's part), the collapse into entries, the totals and the token
    /// budget. `resolved` and `sheets` are a literal's named entities and their fact sheets.
    #[allow(clippy::too_many_arguments)]
    fn page_of<'m>(
        &self,
        scope: &Scope,
        m: &'m Materialized,
        filters: &SearchFilters,
        mut hits: Vec<Hit<'m>>,
        mut token: Vec<u8>,
        rank: Rank<'_>,
        resolved: &[(BlockId, NameKind)],
        sheets: Vec<FactSheet>,
        cursor: Option<(String, usize)>,
        max_tokens: Option<usize>,
        head: Head,
    ) -> Result<SearchResult, RdyError> {
        let ranked = !matches!(rank, Rank::Matched);
        // cc14 (FW-SR1, FW-SR4): `coverage_below` drops the hits at or over the bound and
        // `sort: coverage` lists the lowest first, so only those two need every hit's coverage
        // before the page cut, and only they put it in the page token (membership and order
        // depend on it). Any other search gives coverage to the entries of its page only. A
        // past landing or a proposal version has no coverage, so a search there carries none.
        let by_coverage = filters.sort.as_deref() == Some(SORT_COVERAGE);
        let wants_coverage = filters.coverage_below.is_some() || by_coverage;
        // A filter or sort on coverage at a past landing or in a proposal version is refused. A
        // coverage read error never fails a search: the hits carry no coverage (so
        // `coverage_below` keeps none) and `coverage_error` says why.
        let past = self.coverage_left_out(scope);
        if past && wants_coverage {
            self.coverage_scope(scope)?;
        }
        let (issues, coverage_error) = if past {
            (None, None)
        } else {
            // ck3: the shown issues and score carry the source facts; the level reads the
            // intrinsic issues, for a source fact changes no confidence input
            match self
                .coverage_issues(scope)
                .and_then(|base| Ok((self.display_coverage_issues(scope)?, base)))
            {
                Ok(i) => (Some(i), None),
                Err(e) => (None, Some(coverage_error_text(&e))),
            }
        };
        if wants_coverage && let Some((shown, base)) = &issues {
            for h in &mut hits {
                h.cov = Some(self.hit_coverage(scope, shown, base, &h.block.id)?);
            }
        }
        if let Some(n) = filters.coverage_below {
            hits.retain(|h| h.cov.as_ref().is_some_and(|c| c.score < n));
        }
        if by_coverage {
            // stable: equal scores keep the default order
            hits.sort_by_key(|h| h.cov.as_ref().map_or(100, |c| c.score));
        }
        if let Rank::Relevance(w, text) = &rank {
            // D-J5FWG: every kept hit against the query's whole lexical vector, no seed or cap.
            // Stable: the phrase tier stays before the all-words tier, and equal scores keep
            // reading order.
            relevance_scores(w, text, &mut hits)?;
            hits.sort_by(|a, b| {
                a.all_words
                    .cmp(&b.all_words)
                    .then_with(|| b.score.unwrap_or(0.0).total_cmp(&a.score.unwrap_or(0.0)))
            });
        }
        // every collapse-key input the version does not cover goes in too: `history` comes from
        // other decisions, so a supersede can regroup the entries without moving any version
        for h in &hits {
            let version = m
                .version_of(&h.block.id)
                .unwrap_or_else(|| h.block.version_hash());
            token.extend_from_slice(h.block.id.as_str().as_bytes());
            token.push(0);
            token.extend_from_slice(&version.0);
            token.push(u8::from(h.history));
            token.push(u8::from(h.all_words));
            // vxs7: a like hit's lines follow the words it shares with the source, which no hit
            // version covers, and they are the rest of the collapse key: an edit of the source
            // that regroups the entries moves the cursor; a drift of scores alone does not
            if matches!(rank, Rank::Like) {
                for l in &h.lines {
                    token.extend_from_slice(l.field.as_bytes());
                    token.push(0);
                    token.extend_from_slice(&(l.line as u64).to_le_bytes());
                    token.extend_from_slice(l.text.as_bytes());
                    token.push(0);
                }
                token.extend_from_slice(&(h.more_lines as u64).to_le_bytes());
                token.push(0xff);
            }
            // coverage moves with other blocks' tensions and suspects, not with this version
            if let Some(c) = &h.cov {
                token.extend_from_slice(&c.score.to_le_bytes());
                token.extend_from_slice(&(c.issues as u64).to_le_bytes());
                token.push(c.level as u8);
            }
        }
        if !resolved.is_empty() {
            token.extend_from_slice(b"entities\0");
            for (e, kind) in resolved {
                // resolved ids are in the scope, so each has a version
                let version = m
                    .version_of(e)
                    .or_else(|| m.get(e).map(Block::version_hash));
                token.extend_from_slice(e.as_str().as_bytes());
                token.push(0);
                token.push(*kind as u8);
                if let Some(v) = version {
                    token.extend_from_slice(&v.0);
                }
            }
        }
        let token = Hash::of(&token).to_string();

        // scol1: a hit identical to an earlier entry joins it; entries keep their first position
        let total = hits.len();
        let mut entries: Vec<SearchMatch> = vec![];
        let mut seen: HashMap<EntryKey, usize> = HashMap::new();
        let mut types: BTreeMap<String, usize> = BTreeMap::new();
        let mut docs: HashMap<Option<String>, usize> = HashMap::new();
        for h in hits {
            let path = m.path_of(&h.block.id);
            *types.entry(h.block.block_type.clone()).or_default() += 1;
            *docs.entry(path.clone()).or_default() += 1;
            let key = EntryKey {
                block_type: h.block.block_type.clone(),
                lines: h.lines,
                more_lines: h.more_lines,
                all_words: h.all_words,
                history: h.history,
            };
            // D-5VGAS: in a ranked order, hits with no matching line never collapse
            let joins = !(ranked && key.lines.is_empty());
            if joins && let Some(&at) = seen.get(&key) {
                if let (Some(have), Some(c)) = (&mut entries[at].confidence, &h.cov) {
                    // the entry takes the lowest level of its blocks
                    if level_rank(c.level) > level_rank(*have) {
                        *have = c.level;
                    }
                }
                entries[at].also.push(SearchAlso {
                    id: h.block.id.to_string(),
                    path,
                    score: h.score.map(round3),
                    words: h.words,
                    links: h.links,
                    suggestions: h.suggestions,
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
                score: h.score.map(round3),
                words: h.words,
                links: h.links,
                also: vec![],
                suggestions: h.suggestions,
                coverage: h.cov.as_ref().map(|c| c.score),
                issues: h.cov.as_ref().map(|c| c.issues),
                confidence: h.cov.as_ref().map(|c| c.level),
            });
            if joins {
                seen.insert(key, entries.len() - 1);
            }
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

        // the first page's fact sheets (FW-SR1: entity matches first)
        let (entities, entities_omitted) = if cursor.is_none() {
            let omitted = resolved.len() - sheets.len();
            (sheets, omitted)
        } else {
            (vec![], 0)
        };

        // the totals come first, then the sheets, so they take their share of the budget
        let mut budget = max_tokens
            .unwrap_or(DEFAULT_SEARCH_TOKENS)
            .saturating_sub(tokens_of(&totals));
        if !entities.is_empty() {
            budget = budget.saturating_sub(tokens_of(&entities));
        }
        let mut used = 0;
        let mut matches = vec![];
        for mut entry in entries.into_iter().skip(offset) {
            if !wants_coverage && let Some((shown, base)) = &issues {
                // the page's entries only: one level pass per block, the entry takes the lowest
                self.attach_coverage(scope, shown, base, &mut entry)?;
            }
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
            query: head.query,
            regex: head.regex,
            ignore_case: head.ignore_case,
            like: head.like,
            like_text: head.like_text,
            entities,
            entities_omitted,
            total,
            totals,
            matches,
            truncated: omitted > 0,
            omitted,
            next: (omitted > 0).then(|| format!("{token}:{end}")),
            coverage_error,
        })
    }
}

impl ReadState {
    /// vxs6 (D-76Y9H, D-5VGAS): the scope a ranked mode `p` (`sort relevance`, `like` or
    /// `like_text`) weighs its vectors in. Canon's head is canon. `E_BAD_INPUT` for a past
    /// landing (ranking over time is not core, FW-H7) and for a proposal version.
    pub(crate) fn ranked_scope(&self, scope: &Scope, p: &str) -> Result<Scope, RdyError> {
        match scope {
            Scope::Proposal(v) => Err(RdyError::new(
                E_BAD_INPUT,
                format!("{p} ranks canon and views only, not proposal:{}", v.short()),
                "ask for the proposal's view, or canon",
            )),
            Scope::At(h) if self.is_past_landing(scope) => Err(RdyError::new(
                E_BAD_INPUT,
                format!("{p} ranks the current state only, not landing:{h}"),
                "ask for canon or a view; ranking over time is not part of rdy core",
            )),
            Scope::At(_) => Ok(Scope::Canon),
            other => Ok(other.clone()),
        }
    }
}

/// vxs7: the source of a `like` mode: a block id, or a text.
#[derive(Clone, Copy)]
enum LikeSource<'a> {
    Block(&'a str),
    Text(&'a str),
}

impl LikeSource<'_> {
    /// The parameter, as a refusal names it.
    fn param(&self) -> &'static str {
        match self {
            LikeSource::Block(_) => "like",
            LikeSource::Text(_) => "like_text",
        }
    }
}

/// How the hits of a search come to [`ReadState::page_of`].
enum Rank<'a> {
    /// In match order (reading order, or coverage with `sort: coverage`).
    Matched,
    /// D-76Y9H: scored against the query's text with these weights, then sorted in each tier.
    Relevance(Arc<VectorWeights>, &'a str),
    /// D-76Y9H: a `like` mode; the hits come scored and in score order.
    Like,
}

/// What a result echoes of its request.
struct Head {
    query: String,
    regex: bool,
    ignore_case: bool,
    like: Option<String>,
    like_text: Option<String>,
}

impl ReadState {
    /// vxs7 (D-76Y9H, D-5VGAS): one page of a search from any source. A query searches as
    /// [`ReadState::search`] does; `like` and `like_text` rank the scope's blocks by score. The
    /// D-5VGAS refusals come in their order: the source's own (2, 3, 5) first.
    pub fn search_input(
        &self,
        scope: &Scope,
        input: &SearchInput,
        page: Option<&str>,
        max_tokens: Option<usize>,
    ) -> Result<SearchResult, RdyError> {
        input.check_mode()?;
        let source = match &input.source {
            SearchSource::Query(text) => {
                let q = SearchQuery {
                    text: text.clone(),
                    regex: input.regex,
                    ignore_case: input.ignore_case,
                    filters: input.filters.clone(),
                };
                return self.search(scope, &q, page, max_tokens);
            }
            SearchSource::Like(id) => LikeSource::Block(id),
            SearchSource::LikeText(text) => LikeSource::Text(text),
        };
        self.like_hits(scope, source, input, page, max_tokens)
    }

    /// vxs7 (D-76Y9H, D-5VGAS, D-RY7B5): a `like` or `like_text` search. Refusals 6 (a
    /// `like_text` with no word a block of the scope holds, read from the lexicon's membership),
    /// 7 (an unknown `like` id) and 8 (a past landing or a proposal version) come in that order,
    /// before any weight is read. The source's seeds find the candidates; a source with none has
    /// no hits and no refusal. Every candidate the filters keep, of any type and other than the
    /// `like` block, scores with all its features, and a hit's exact score reaches
    /// [`LIKE_FLOOR`]. Hits come by exact score, then in reading order; a guard refuses the
    /// search.
    fn like_hits(
        &self,
        scope: &Scope,
        source: LikeSource<'_>,
        input: &SearchInput,
        page: Option<&str>,
        max_tokens: Option<usize>,
    ) -> Result<SearchResult, RdyError> {
        let ranked = self.ranked_scope(scope, source.param());
        // refusals 6 and 7 read the scope the search ranks in, or the one asked for when that
        // one has no ranking
        let at = ranked.as_ref().unwrap_or(scope);
        let like_id = match source {
            LikeSource::Text(text) => {
                let lexicon = self.lexicon(at)?;
                let known = rdy_model::entity::words(text)
                    .iter()
                    .any(|w| lexicon.word_df(&w.folded) > 0);
                if !known {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        "like_text has no word that a block in this scope holds",
                        "use words that the project uses, or search with query",
                    ));
                }
                None
            }
            LikeSource::Block(id) => {
                let m = self.materialized(at)?;
                match BlockId::parse(id) {
                    Ok(b) if m.get(&b).is_some() => Some(b),
                    _ => {
                        return Err(RdyError::new(
                            E_UNKNOWN_BLOCK,
                            format!("no block {id}"),
                            "check the id with rdy_read",
                        ));
                    }
                }
            }
        };
        let weigh = ranked?;
        let m = self.materialized(&weigh)?;
        let plan = self.filter_plan(&weigh, &m, &input.filters)?;
        let cursor = page.map(parse_page).transpose()?;
        let graph = self.graph(&weigh)?;
        let w = self.vector_weights(WeightScope::Read(&weigh))?;

        // the source's two vectors: a block's own, or a text's words and the entities it names
        let counts: WordCounts;
        let named: Vec<BlockId>;
        let src = match (&like_id, source) {
            (Some(id), _) => Operand::of(&w, Doc::Block(id)).map_err(|_| vector_work_refused())?,
            (None, LikeSource::Text(text)) => {
                counts = text_counts(text);
                named = named_entities(&*self.lexicon(&weigh)?, text);
                let mut link: Vec<(&BlockId, f64)> = Vec::with_capacity(named.len());
                for e in &named {
                    if let Some(x) = w.target_weight(e).map_err(|_| vector_work_refused())? {
                        link.push((e, x));
                    }
                }
                Operand {
                    lexical: text_lexical(&w, &counts)?,
                    link: unit_of(link),
                }
            }
            (None, LikeSource::Block(_)) => Operand::default(),
        };
        let mut budget = VectorBudget::search();
        budget.begin_source();
        let seeds = select_seeds(&w, &src);
        let found: Vec<&BlockId> = if seeds.is_empty() {
            vec![]
        } else {
            candidates(&w, &seeds, &mut budget).map_err(|_| vector_work_refused())?
        };

        // the candidates in reading order, each with the lifecycle its parent passes down
        let mut hits: Vec<Hit> = vec![];
        if !found.is_empty() {
            let mut stack: Vec<(&Block, Option<&str>)> = m
                .children(None)
                .into_iter()
                .rev()
                .map(|b| (b, None))
                .collect();
            for _ in 0..m.len() {
                let Some((b, inherited)) = stack.pop() else {
                    break;
                };
                let own = match b.fields.get("lifecycle") {
                    Some(FieldValue::Str(l)) => Some(l.as_str()),
                    _ => inherited,
                };
                if found.binary_search(&&b.id).is_ok()
                    && like_id.as_ref() != Some(&b.id)
                    && plan.passes(&m, b, own)
                {
                    let history = in_history(&m, &graph, b);
                    if !(input.filters.exclude_history && history) {
                        let candidate = Operand::of(&w, Doc::Block(&b.id))
                            .map_err(|_| vector_work_refused())?;
                        let score = score_operands(&src, &candidate, &mut budget)
                            .map_err(|_| vector_work_refused())?;
                        if score >= LIKE_FLOOR {
                            let why = reasons_of_operands(&src, &candidate);
                            let (lines, more_lines) = like_lines(b, &why.words);
                            hits.push(Hit {
                                block: b,
                                lines,
                                more_lines,
                                history,
                                all_words: false,
                                cov: None,
                                score: Some(score),
                                words: Some(why.words),
                                links: Some(why.links.iter().map(BlockId::to_string).collect()),
                                suggestions: vec![],
                            });
                        }
                    }
                }
                stack.extend(m.children(Some(&b.id)).into_iter().rev().map(|c| (c, own)));
            }
        }
        // by exact score; stable, so equal scores keep reading order
        hits.sort_by(|a, b| b.score.unwrap_or(0.0).total_cmp(&a.score.unwrap_or(0.0)));

        // the cursor covers the mode, its source and the filters (D-5VGAS); `ignore_case` has
        // no effect, so it is not part of it
        let mut token = TOKEN_DOMAIN.to_vec();
        token.push(0);
        token.push(u8::from(input.regex));
        token.push(0);
        token.extend(serde_json::to_vec(&input.filters).unwrap_or_default());
        token.push(0);
        let (like, like_text) = match source {
            LikeSource::Block(id) => {
                token.extend_from_slice(b"like\0");
                token.extend_from_slice(id.as_bytes());
                (Some(id.to_string()), None)
            }
            LikeSource::Text(text) => {
                token.extend_from_slice(b"like_text\0");
                token.extend_from_slice(text.as_bytes());
                (None, Some(text.to_string()))
            }
        };
        token.push(0);
        let head = Head {
            query: String::new(),
            regex: input.regex,
            ignore_case: input.ignore_case,
            like,
            like_text,
        };
        self.page_of(
            &weigh,
            &m,
            &input.filters,
            hits,
            token,
            Rank::Like,
            &[],
            vec![],
            cursor,
            max_tokens,
            head,
        )
    }
}

/// D-76Y9H: the entities `text` names through the name matcher (a name or an alias, as a
/// block's mentions read it), by id.
fn named_entities(lexicon: &crate::lexicon::Lexicon, text: &str) -> Vec<BlockId> {
    let probe = Block {
        id: BlockId::MIN,
        block_type: String::new(),
        parent: None,
        order: String::new(),
        fields: BTreeMap::new(),
        body: text.to_string(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    };
    let mut out: Vec<BlockId> = lexicon
        .mentions(&probe)
        .into_iter()
        .filter_map(|mn| BlockId::parse(&mn.entity).ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A `like` hit's lines: each line of the block that holds one of its `shown` words (whole and
/// lower-cased, as vector words are), windowed at the first, at most `LINES_PER_MATCH`, and how
/// many more there are.
fn like_lines(b: &Block, shown: &[String]) -> (Vec<SearchLine>, usize) {
    let mut lines = vec![];
    let mut more = 0;
    if shown.is_empty() {
        return (lines, more);
    }
    let _ = each_line(b, &mut |field, i, line| {
        if let Some(w) = rdy_model::entity::words(line)
            .into_iter()
            .find(|w| shown.contains(&w.folded))
        {
            keep(&mut lines, &mut more, field, i, line, w.start);
        }
        Ok(())
    });
    (lines, more)
}

/// The `sort` values: the default reading order named, coverage and relevance.
const SORT_READING: &str = "reading";
const SORT_COVERAGE: &str = "coverage";
const SORT_RELEVANCE: &str = "relevance";

/// The refusal of a ranked search that passes its vector work or weight limit.
fn vector_work_refused() -> RdyError {
    RdyError::new(
        E_LIMIT,
        "vector search exceeds its work limit",
        "use query without sort relevance",
    )
}

/// D-76Y9H, D-J5FWG: gives each hit its exact lexical score against `text`. The query's lexical
/// vector holds every word of `text` that a block of the scope holds, `tf × idf` under `w`, by
/// word, divided by its length; a query has no link vector. A hit's score is the dot product
/// with its own lexical vector: 0 for a hit that shares no whole word. The hits are the query's
/// candidates: their ids and scratch, then the products and lazy weights, are charged to one
/// search budget first; a guard refuses the search.
fn relevance_scores(w: &VectorWeights, text: &str, hits: &mut [Hit<'_>]) -> Result<(), RdyError> {
    let counts = text_counts(text);
    let query = Operand {
        lexical: text_lexical(w, &counts)?,
        link: vec![],
    };
    let mut budget = VectorBudget::search();
    budget.begin_source();
    // every kept hit is a candidate of the query: its id and its scratch are charged before any
    // hit is scored, so a hit set past the candidate or scratch limit is refused, never cut
    let scratch: u64 = hits
        .iter()
        .map(|h| CANDIDATE_RECORD_BYTES + h.block.id.as_str().len() as u64)
        .sum();
    budget
        .charge_candidates(hits.len() as u64, scratch)
        .map_err(|_| vector_work_refused())?;
    for h in hits {
        let block = Operand {
            lexical: w
                .lexical(Doc::Block(&h.block.id))
                .map_err(|_| vector_work_refused())?,
            link: vec![],
        };
        let score =
            score_operands(&query, &block, &mut budget).map_err(|_| vector_work_refused())?;
        h.score = Some(score);
    }
    Ok(())
}

/// How often each word of `text` occurs, as the lexicon reads a block's words.
fn text_counts(text: &str) -> WordCounts {
    WordCounts::of(rdy_model::entity::words(text).into_iter().map(|w| w.folded))
}

/// The lexical vector of a text whose words occur `counts` times: each word that a block of the
/// scope holds, `tf × idf` under `w`, by word, divided by its length. Empty when no word is known.
fn text_lexical<'c>(
    w: &VectorWeights,
    counts: &'c WordCounts,
) -> Result<Vec<(&'c str, f64)>, RdyError> {
    let mut lexical: Vec<(&str, f64)> = Vec::with_capacity(counts.len());
    for (word, count) in counts.iter() {
        if let Some(idf) = w.idf(word).map_err(|_| vector_work_refused())? {
            lexical.push((word, tf(count) * idf));
        }
    }
    Ok(unit_of(lexical))
}

/// `raw` divided by its length, summed in key order as a block's norm is; empty when the
/// length is not above 0.
fn unit_of<K>(mut raw: Vec<(K, f64)>) -> Vec<(K, f64)> {
    let mut sum = 0.0;
    for (_, x) in &raw {
        sum += x * x;
    }
    let norm = sum.sqrt();
    if norm > 0.0 {
        for (_, x) in &mut raw {
            *x /= norm;
        }
    } else {
        raw.clear();
    }
    raw
}

/// One matching block before collapse.
struct Hit<'a> {
    block: &'a Block,
    lines: Vec<SearchLine>,
    more_lines: usize,
    history: bool,
    /// sord1: no line holds the phrase; the block holds every word of it.
    all_words: bool,
    /// cc14: the block's coverage; none at a past landing.
    cov: Option<HitCoverage>,
    /// vxs6: the exact lexical score against the query, with `sort: relevance`, or the exact
    /// score against the source in a `like` mode.
    score: Option<f64>,
    /// vxs7: in a `like` mode, the shared words and link targets ([`crate::vectors::reasons`]).
    words: Option<Vec<String>>,
    links: Option<Vec<String>>,
    /// esrch1: link suggestions; not part of the collapse key.
    suggestions: Vec<Suggestion>,
}

/// Whether `b` sits in a superseded, rejected or revoked decision (itself or an ancestor).
fn in_history(m: &Materialized, graph: &rdy_graph::Graph, b: &Block) -> bool {
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
}

/// The order of confidence levels, the lowest level last.
fn level_rank(l: Level) -> u8 {
    match l {
        Level::High => 0,
        Level::Medium => 1,
        Level::Low => 2,
    }
}

/// The coverage a hit carries.
struct HitCoverage {
    score: u32,
    issues: usize,
    level: Level,
}

impl ReadState {
    /// The coverage of the hit block `id`: its score alone, its issues that are not dismissed,
    /// and its confidence level over the hit's neighbourhood. The score and the issues read the
    /// `shown` issues (source facts included, ck3); the level reads the `intrinsic` ones.
    fn hit_coverage(
        &self,
        scope: &Scope,
        shown: &CoverageIssues,
        intrinsic: &CoverageIssues,
        id: &BlockId,
    ) -> Result<HitCoverage, RdyError> {
        let entry = shown.blocks.get(id);
        let score = rollup(self.pack(), entry.into_iter()).score;
        let open = entry.map_or(0, |e| {
            e.issues.iter().filter(|i| i.dismissed.is_none()).count()
        });
        let hood = self.hit_neighbourhood(scope, id)?;
        let level = confidence_level_of(intrinsic, &hood, Some(id));
        Ok(HitCoverage {
            score,
            issues: open,
            level,
        })
    }

    /// Give `entry` the coverage of its first block and the lowest level of all its blocks.
    fn attach_coverage(
        &self,
        scope: &Scope,
        shown: &CoverageIssues,
        intrinsic: &CoverageIssues,
        entry: &mut SearchMatch,
    ) -> Result<(), RdyError> {
        let first = BlockId::parse(&entry.id).map_err(|_| bad_page())?;
        let c = self.hit_coverage(scope, shown, intrinsic, &first)?;
        let mut level = c.level;
        for a in &entry.also {
            let id = BlockId::parse(&a.id).map_err(|_| bad_page())?;
            let l = confidence_level_of(intrinsic, &self.hit_neighbourhood(scope, &id)?, Some(&id));
            if level_rank(l) > level_rank(level) {
                level = l;
            }
        }
        entry.coverage = Some(c.score);
        entry.issues = Some(c.issues);
        entry.confidence = Some(level);
        Ok(())
    }
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
            // rf6 (D-VPATZ): the blocks it governs by the four routes, whatever its status
            let pack = self.pack();
            let set: BTreeSet<BlockId> = g
                .governed_through(
                    &id,
                    GoverningStatuses::All,
                    |e| crate::query::is_entity_in(pack, m, e),
                    &mut GoverningCache::default(),
                )
                .into_iter()
                .collect();
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
        if f.tension {
            allowed.push(self.open_tension_blocks(now_ms())?);
        }
        if let Some(want) = &f.mentions {
            if !matches!(want.as_str(), "linked" | "unlinked") {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("mentions: {want:?} is not linked or unlinked"),
                    "pass linked or unlinked",
                ));
            }
            let linked = want == "linked";
            let lex = self.lexicon(scope)?;
            let mut set: BTreeSet<BlockId> = BTreeSet::new();
            for b in m.iter() {
                if link_kind_for(pack, b).is_none() {
                    continue;
                }
                // unlinked: neither tied nor dismissed for this block (D-E6JD1, FW-CV4)
                let hit = lex.mentions(b).iter().any(|ms| {
                    BlockId::parse(&ms.entity).is_ok_and(|e| {
                        let tied = tied(&g, &b.id, &e);
                        if linked {
                            tied
                        } else {
                            !tied && self.mention_dismissed(&b.id, &e).is_none()
                        }
                    })
                });
                if hit {
                    set.insert(b.id.clone());
                }
            }
            allowed.push(set);
        }
        if let Some(s) = &f.sort
            && ![SORT_COVERAGE, SORT_RELEVANCE, SORT_READING].contains(&s.as_str())
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("sort takes coverage, relevance or reading, not {s}"),
                "pass coverage, relevance or reading, or omit sort",
            ));
        }
        if let Some(n) = f.coverage_below
            && n > 100
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("coverage_below is {n}; it must be 0 to 100"),
                "pass a score from 0 to 100",
            ));
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
    compile_as(q, q.ignore_case, false)
}

/// [`compile`] with `ignore_case` given; with `names` (a literal that names an entity), the
/// literal's words match with any spacing between them.
fn compile_as(
    q: &SearchQuery,
    ignore_case: bool,
    names: bool,
) -> Result<(Regex, Vec<Regex>), RdyError> {
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
    } else if names {
        q.text
            .split_whitespace()
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(r"\s+")
    } else {
        regex::escape(&q.text)
    };
    let phrase = bounded_regex(&pattern, ignore_case)?;
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
            .map(|w| bounded_regex(&regex::escape(w), ignore_case))
            .collect::<Result<_, _>>()?
    } else {
        vec![]
    };
    Ok((phrase, words))
}

/// One regex with the search's limits (program size, lazy-DFA cache, nesting); its errors as rdy
/// errors. `rdy_read`'s `/regex/` patterns use it too.
pub fn bounded_regex(pattern: &str, ignore_case: bool) -> Result<Regex, RdyError> {
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
pub(crate) fn window(line: &str, at: usize) -> String {
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
