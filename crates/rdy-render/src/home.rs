//! The home page's context: a card for every top-level folder and doc, and the versions, newest
//! first, in a reader's words (a date and the message, never a hash).
use crate::site::{Site, rel};
use rdy_engine::{Materialized, VersionInfo};
use rdy_model::{Block, RefMode, ref_spans, tree};
use serde::Serialize;
use std::collections::BTreeMap;

/// The longest summary a card shows.
const SUMMARY_MAX: usize = 160;

#[derive(Clone, Debug, Serialize)]
pub struct Card {
    pub title: String,
    pub href: String,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Version {
    pub name: String,
    pub message: Option<String>,
    /// The day the version was cut, as `8 February 2021`.
    pub when: String,
    pub current: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Home {
    pub description: Option<String>,
    pub cards: Vec<Card>,
    /// Newest first.
    pub versions: Vec<Version>,
}

pub fn build(
    site: &Site,
    m: &Materialized,
    versions: &[VersionInfo],
    landed_ms: &BTreeMap<String, i64>,
    description: Option<&str>,
    has_terms: bool,
    has_questions: bool,
) -> Home {
    let mut cards: Vec<Card> = m
        .children(None)
        .into_iter()
        .filter(|b| tree::is_tree_type(&b.block_type))
        .filter_map(|b| {
            let page = site.page(&b.id)?;
            let summary = if b.block_type == tree::FOLDER {
                b.field_str("description").unwrap_or_default().to_string()
            } else {
                first_prose(site, m, b, 0)
            };
            Some(Card {
                title: site.label(b),
                href: rel("index.html", &page),
                summary: cut(&summary, SUMMARY_MAX),
            })
        })
        .collect();
    if has_terms {
        cards.push(Card {
            title: "Glossary".into(),
            href: "glossary.html".into(),
            summary: "The terms this project defines.".into(),
        });
    }
    if has_questions {
        cards.push(Card {
            title: "Open questions".into(),
            href: "questions.html".into(),
            summary: "What is still undecided.".into(),
        });
    }
    let current = versions.last().map(|v| &v.name);
    Home {
        description: description.map(String::from),
        cards,
        versions: versions
            .iter()
            .rev()
            .map(|v| Version {
                name: v.name.clone(),
                message: v.message.clone().filter(|s| !s.trim().is_empty()),
                when: landed_ms
                    .get(&v.landing)
                    .map(|ms| date(*ms))
                    .unwrap_or_default(),
                current: Some(&v.name) == current,
            })
            .collect(),
    }
}

/// The first prose block under `b` (depth first, in order), as plain text: a reference reads as
/// its target's label (an unknown one is dropped, never an id) and `\{{` as `{{`.
fn first_prose(site: &Site, m: &Materialized, b: &Block, depth: usize) -> String {
    // a damaged store can nest sections without end
    if depth > tree::MAX_FOLDER_DEPTH + 64 {
        return String::new();
    }
    for c in m.children(Some(&b.id)) {
        if c.block_type == "prose" && !c.body.trim().is_empty() {
            return plain(site, m, c);
        }
        let inner = first_prose(site, m, c, depth + 1);
        if !inner.is_empty() {
            return inner;
        }
    }
    String::new()
}

fn plain(site: &Site, m: &Materialized, b: &Block) -> String {
    let text = &b.body;
    let mut out = String::new();
    let mut last = 0;
    for span in ref_spans(text, RefMode::for_body(&b.block_type)) {
        out.push_str(&text[last..span.range.start].replace("\\{{", "{{"));
        if let Some(t) = rdy_model::BlockId::parse(&span.token)
            .ok()
            .and_then(|id| m.get(&id))
        {
            out.push_str(&site.label(t));
        }
        last = span.range.end;
    }
    out.push_str(&text[last..].replace("\\{{", "{{"));
    out
}

/// `text` with its whitespace collapsed, cut at a word boundary to at most `max` characters, with
/// `…` when something was cut.
fn cut(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let head: String = text.chars().take(max).collect();
    // the cut falls on a word boundary when the next character is a space
    let next_is_space = text.chars().nth(max).is_some_and(char::is_whitespace);
    let kept = if next_is_space {
        head.as_str()
    } else {
        head.rsplit_once(' ').map_or(head.as_str(), |(w, _)| w)
    };
    format!("{}…", kept.trim_end_matches([' ', ',', ';', ':', '.', '-']))
}

/// `8 February 2021` for a time in milliseconds since the epoch (UTC).
fn date(ms: i64) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    // days since 1970-01-01 to a civil date (Hinnant's algorithm)
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day} {} {year}", MONTHS[(month - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ver1_dates_read_day_month_year() {
        assert_eq!(date(0), "1 January 1970");
        // 2021-02-08T10:00:00Z
        assert_eq!(date(1_612_778_400_000), "8 February 2021");
        // a leap day, and the last millisecond of a year
        assert_eq!(date(1_582_934_400_000), "29 February 2020");
        assert_eq!(date(1_609_459_199_999), "31 December 2020");
        assert_eq!(date(-1), "31 December 1969");
    }

    #[test]
    fn ver1_a_summary_is_cut_at_a_word_boundary() {
        assert_eq!(cut("  a  short\n text ", 160), "a short text");
        let long = "word ".repeat(60);
        let s = cut(&long, 160);
        assert!(s.ends_with("word…") && s.chars().count() <= 161, "{s}");
        // a cut inside a word backs up to the word before
        assert_eq!(cut("alpha beta gamma", 8), "alpha…");
        // a cut exactly at a space keeps the whole word
        assert_eq!(cut("alpha beta gamma", 10), "alpha beta…");
        // one long word has no boundary to back up to
        assert_eq!(cut("abcdefghij", 4), "abcd…");
    }
}
