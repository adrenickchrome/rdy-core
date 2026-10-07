//! rpag1, rfoc1, rpat1: the parts of `rdy_read` that page whole blocks with a cursor, find one
//! section of a doc by its heading slug, and keep only the blocks a pattern matches.
use rdy_engine::{E_BAD_INPUT, E_UNKNOWN_BLOCK, Materialized, RdyError};
use rdy_model::tree::{DOC, slug as slug_of};
use rdy_model::{Block, BlockId};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

/// The most cut reads one MCP session keeps a cursor for; the oldest is dropped first.
pub const READ_REGISTRY_CAP: usize = 32;
/// The longest pattern `rdy_read` accepts, in characters.
pub const MAX_PATTERN: usize = 1024;

/// A read as its first call gave it (`format` defaulted to `compact`). Its JSON is part of the
/// page token, and a page call reads with it again.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReadQuery {
    pub id: String,
    pub scope: Option<String>,
    pub format: String,
    pub pattern: Option<String>,
}

/// The cut reads of one MCP session, by page token, newest last.
#[derive(Default)]
pub struct ReadRegistry {
    reads: VecDeque<(String, ReadQuery)>,
}

impl ReadRegistry {
    /// Records `q` under `token` as the newest read, and drops the oldest past the cap.
    pub fn register(&mut self, token: &str, q: ReadQuery) {
        self.reads.retain(|(t, _)| t != token);
        self.reads.push_back((token.to_string(), q));
        while self.reads.len() > READ_REGISTRY_CAP {
            self.reads.pop_front();
        }
    }

    pub fn get(&self, token: &str) -> Option<ReadQuery> {
        self.reads
            .iter()
            .find(|(t, _)| t == token)
            .map(|(_, q)| q.clone())
    }
}

/// What a read keeps: a substring (any case), or a `/regex/`.
pub enum Pattern {
    /// Lowercased.
    Substring(String),
    Regex(regex::Regex),
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Pattern, RdyError> {
        if text.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "pattern is empty",
                "pass a substring, or /regex/",
            ));
        }
        if text.chars().count() > MAX_PATTERN {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("pattern is longer than {MAX_PATTERN} characters"),
                "use a shorter pattern",
            ));
        }
        if text.len() >= 2 && text.starts_with('/') && text.ends_with('/') {
            let inner = &text[1..text.len() - 1];
            return regex::RegexBuilder::new(inner)
                .size_limit(1 << 20)
                .build()
                .map(Pattern::Regex)
                .map_err(|e| {
                    RdyError::new(
                        E_BAD_INPUT,
                        format!("pattern is not a valid regex: {e}"),
                        "write /regex/ in Rust regex syntax, or a plain substring",
                    )
                });
        }
        Ok(Pattern::Substring(text.to_lowercase()))
    }

    pub fn matches(&self, line: &str) -> bool {
        match self {
            Pattern::Substring(s) => line.to_lowercase().contains(s.as_str()),
            Pattern::Regex(r) => r.is_match(line),
        }
    }
}

/// A page cursor `<64 lowercase hex>:<offset>` as its token and offset; anything else is `None`.
pub fn parse_cursor(text: &str) -> Option<(String, usize)> {
    let (token, offset) = text.split_once(':')?;
    let hex = token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let digits = (1..=9).contains(&offset.len()) && offset.bytes().all(|b| b.is_ascii_digit());
    if !hex || !digits {
        return None;
    }
    Some((token.to_string(), offset.parse().ok()?))
}

/// rfoc1: the section of `doc` that `slug` names. Sections are named by the slug of their title,
/// depth-first in child order; the 2nd, 3rd, … section with the same slug in this doc is named
/// `{slug}-2`, `{slug}-3`, …. The first exact match wins.
pub fn section_by_slug(m: &Materialized, doc: &Block, slug: &str) -> Result<BlockId, RdyError> {
    if doc.block_type != DOC {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!(
                "{} is a {}, not a doc: #<heading> reads a section of a doc",
                doc.id, doc.block_type
            ),
            "pass a doc's id, key or path before #",
        ));
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut walk: Vec<&Block> = m.children(Some(&doc.id)).into_iter().rev().collect();
    while let Some(b) = walk.pop() {
        // a doc inside this doc has its own sections
        if b.block_type == DOC {
            continue;
        }
        if b.block_type == "section" {
            let base = slug_of(b.field_str("title").unwrap_or(""));
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { base } else { format!("{base}-{n}") };
            if name == slug {
                return Ok(b.id.clone());
            }
        }
        walk.extend(m.children(Some(&b.id)).into_iter().rev());
    }
    Err(RdyError::new(
        E_UNKNOWN_BLOCK,
        format!("no section #{slug} in {}", doc.id),
        "rdy_read the doc to see its section titles",
    ))
}

/// The length of `s` written as a JSON string, without its quotes.
fn escaped_len(s: &str) -> usize {
    serde_json::to_string(s)
        .map(|j| j.len() - 2)
        .unwrap_or(s.len())
}

/// rpag1: the end (exclusive) of the page that starts at `from`: the most entries whose join with
/// `sep`, JSON-escaped, is at most `budget_bytes`; never less than one entry while any is left.
pub fn page_entries(entries: &[String], from: usize, sep: &str, budget_bytes: usize) -> usize {
    if from >= entries.len() {
        return from;
    }
    let sep_len = escaped_len(sep);
    let mut total = escaped_len(&entries[from]);
    let mut end = from + 1;
    while end < entries.len() {
        let next = total + sep_len + escaped_len(&entries[end]);
        if next > budget_bytes {
            break;
        }
        total = next;
        end += 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str) -> ReadQuery {
        ReadQuery {
            id: id.into(),
            scope: None,
            format: "compact".into(),
            pattern: None,
        }
    }

    #[test]
    fn rpag1_the_registry_keeps_the_newest_reads() {
        let mut r = ReadRegistry::default();
        for i in 0..=READ_REGISTRY_CAP {
            r.register(&format!("t{i}"), q(&format!("d{i}")));
        }
        assert_eq!(r.get("t0"), None, "the oldest is dropped past the cap");
        assert_eq!(r.get("t1"), Some(q("d1")));
        r.register("t1", q("d1"));
        r.register("t-new", q("x"));
        assert_eq!(
            r.get("t1"),
            Some(q("d1")),
            "registering again makes a read the newest"
        );
        assert_eq!(r.get("t2"), None);
    }

    #[test]
    fn rpag1_a_page_holds_whole_entries_and_at_least_one() {
        let e: Vec<String> = ["aaaa", "bb", "c\"c", "dddddddd"].map(String::from).into();
        // "aaaa\nbb" escapes to 4 + 2 + 2 bytes
        assert_eq!(page_entries(&e, 0, "\n", 8), 2);
        assert_eq!(page_entries(&e, 0, "\n", 7), 1);
        // c"c escapes to 4 bytes
        assert_eq!(page_entries(&e, 2, "\n", 5), 3);
        assert_eq!(
            page_entries(&e, 3, "\n", 1),
            4,
            "one entry over the budget is a page"
        );
        assert_eq!(page_entries(&e, 4, "\n", 100), 4);
        assert_eq!(page_entries(&[], 0, "\n", 100), 0);
    }

    #[test]
    fn rpag1_a_cursor_is_64_hex_and_one_offset() {
        let t = "ab".repeat(32);
        assert_eq!(parse_cursor(&format!("{t}:12")), Some((t.clone(), 12)));
        assert_eq!(parse_cursor(&format!("{t}:1234567890")), None);
        assert_eq!(parse_cursor(&format!("{t}:")), None);
        assert_eq!(parse_cursor(&format!("{t}:+1")), None);
        assert_eq!(parse_cursor(&format!("{t}g:1")), None);
    }

    #[test]
    fn rpat1_a_lone_slash_is_a_substring() {
        assert!(matches!(Pattern::parse("/"), Ok(Pattern::Substring(_))));
        assert!(Pattern::parse("/a/").unwrap().matches("xay"));
        assert!(!Pattern::parse("/A/").unwrap().matches("a"));
        assert!(Pattern::parse("ÄB").unwrap().matches("xäb"));
        assert!(
            Pattern::parse(&"é".repeat(MAX_PATTERN)).is_ok(),
            "characters, not bytes"
        );
    }
}
