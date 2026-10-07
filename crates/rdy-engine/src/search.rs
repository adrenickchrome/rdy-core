//! FW-PR1: deterministic search over block text. A literal or a regex, matched line by line in
//! each block's body and string fields, in reading order, with lines windowed and capped per
//! block, pages cut by a token budget and a stateless cursor that a changed match set stales.
use crate::error::{E_BAD_INPUT, E_LIMIT, E_PAGE_STALE, RdyError};
use crate::{ReadState, Scope};
use rdy_model::{Block, FieldValue, Hash};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

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

/// Characters shown before the first match of a windowed line.
const LEAD_CHARS: usize = 80;
const TOKEN_DOMAIN: &[u8] = b"rdy-search-1\0";
/// The bytes one search may rescan after empty matches. After an empty match the regex search
/// starts again one character on, and it can scan to the end of the line, so a pattern that
/// matches empty text costs a line's length squared. The first restart on a line is free (it costs
/// at most one more pass, as the first search does); each later one is charged the rest of the line.
const EMPTY_RESCAN_LIMIT: usize = 32 << 20;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub ignore_case: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchResult {
    pub query: String,
    pub regex: bool,
    pub ignore_case: bool,
    /// Matching blocks over the whole search, every page.
    pub total: usize,
    pub matches: Vec<SearchMatch>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Matches after this page.
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
}

#[derive(Clone, Debug, PartialEq, Serialize)]
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
        let re = compile(q)?;
        let cursor = page.map(parse_page).transpose()?;
        let m = self.materialized(scope)?;

        let mut found: Vec<(&Block, Vec<SearchLine>, usize)> = vec![];
        let mut token = TOKEN_DOMAIN.to_vec();
        token.extend_from_slice(q.text.as_bytes());
        token.push(0);
        token.push(u8::from(q.regex));
        token.push(u8::from(q.ignore_case));
        // reading order: depth-first from the top level; a parent loop cannot spin
        let mut stack: Vec<&Block> = m.children(None).into_iter().rev().collect();
        let mut rescan = 0;
        for _ in 0..m.len() {
            let Some(b) = stack.pop() else { break };
            if let Some((lines, more)) = match_block(&re, b, &mut rescan)? {
                let version = m.version_of(&b.id).unwrap_or_else(|| b.version_hash());
                token.extend_from_slice(b.id.as_str().as_bytes());
                token.push(0);
                token.extend_from_slice(&version.0);
                found.push((b, lines, more));
            }
            stack.extend(m.children(Some(&b.id)).into_iter().rev());
        }
        let token = Hash::of(&token).to_string();

        let total = found.len();
        let offset = match cursor {
            None => 0,
            Some((t, _)) if t != token => {
                return Err(RdyError::new(
                    E_PAGE_STALE,
                    "the matches behind this page changed since the first page, or the query differs",
                    "search again without page",
                ));
            }
            Some((_, offset)) if offset >= total => return Err(bad_page()),
            Some((_, offset)) => offset,
        };

        let budget = max_tokens.unwrap_or(DEFAULT_SEARCH_TOKENS);
        let mut used = 0;
        let mut matches = vec![];
        for (b, lines, more_lines) in found.into_iter().skip(offset) {
            let hit = SearchMatch {
                id: b.id.to_string(),
                block_type: b.block_type.clone(),
                path: m.path_of(&b.id),
                lines,
                more_lines,
            };
            let cost = serde_json::to_string(&hit).map_or(0, |s| s.len()) / 4 + 1;
            if !matches.is_empty() && used + cost > budget {
                break;
            }
            used += cost;
            matches.push(hit);
        }
        let end = offset + matches.len();
        let omitted = total - end;
        Ok(SearchResult {
            query: q.text.clone(),
            regex: q.regex,
            ignore_case: q.ignore_case,
            total,
            matches,
            truncated: omitted > 0,
            omitted,
            next: (omitted > 0).then(|| format!("{token}:{end}")),
        })
    }
}

/// The query as a bounded regex; a literal is escaped first.
fn compile(q: &SearchQuery) -> Result<Regex, RdyError> {
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
    RegexBuilder::new(&pattern)
        .case_insensitive(q.ignore_case)
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

/// The block's matching lines (at most `LINES_PER_MATCH`) and how many more matched, or `None`
/// when no line holds a non-empty match. Sources: the body, then fields by name. `rescan` carries
/// the search's empty-match charge from block to block.
fn match_block(
    re: &Regex,
    b: &Block,
    rescan: &mut usize,
) -> Result<Option<(Vec<SearchLine>, usize)>, RdyError> {
    let mut lines = vec![];
    let mut more = 0;
    let mut failed = None;
    let mut hit = |field: &str, text: &str| {
        for (i, line) in text.lines().enumerate() {
            if failed.is_some() {
                return;
            }
            let at = match first_match(re, line, rescan) {
                Ok(Some(at)) => at,
                Ok(None) => continue,
                Err(e) => {
                    failed = Some(e);
                    return;
                }
            };
            if lines.len() < LINES_PER_MATCH {
                lines.push(SearchLine {
                    field: field.to_string(),
                    line: i + 1,
                    text: window(line, at),
                });
            } else {
                more += 1;
            }
        }
    };
    hit("body", &b.body);
    for (name, value) in &b.fields {
        strings(name.clone(), value, &mut hit);
    }
    if let Some(e) = failed {
        return Err(e);
    }
    Ok((!lines.is_empty()).then_some((lines, more)))
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
                    "the pattern matches empty text too often; searching past those matches costs too much",
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
