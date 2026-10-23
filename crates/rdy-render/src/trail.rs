//! The "Why ?" trail of a block and the copied reference.
use crate::html::{self, Link};
use crate::inline;
use crate::site::{Site, rel};
use rdy_engine::Materialized;
use rdy_graph::{DerivedStatus, EdgeKind, GoverningCache, GoverningStatuses, Graph};
use rdy_model::decision::{self, ANSWERS, REVOKES, SUPERSEDES};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::reference::{format_ref, head, snippet};
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::{Pack, Trait};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// One line of a block's trail: when it happened and what, as plain words.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// `8 February 2021`, `Undated` or `Open`.
    pub when: String,
    /// The day the event happened (`2021-02-08`); none for an open event or an undated one.
    pub sort_key: Option<String>,
    /// HTML: statements are escaped and their references rendered.
    pub text: String,
    /// The matter is still undecided (a proposal or a question).
    pub open: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Trail {
    pub events: Vec<Event>,
}

static REVOKE_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Revoke [A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}: ").expect("regex"));

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

/// `(year, month, day)` of an RFC 3339 timestamp's date part.
fn date_parts(at: &str) -> Option<(u32, usize, u32)> {
    let b = at.as_bytes();
    let digits = |r: std::ops::Range<usize>| {
        b.get(r)
            .filter(|s| s.iter().all(u8::is_ascii_digit))
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| s.parse::<u32>().ok())
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m as usize, d))
}

/// `8 February 2021`, or `Undated`.
fn display_date(at: Option<&str>) -> String {
    match at.and_then(date_parts) {
        Some((y, m, d)) => format!("{d} {} {y}", MONTHS[m - 1]),
        None => "Undated".to_string(),
    }
}

/// `2021-02-08` for a dated event.
fn day_key(at: Option<&str>) -> Option<String> {
    at.and_then(date_parts)
        .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// `{a}`, `{a} and {b}`, `{a}, {b} and {c}`.
fn list_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Closes a sentence unless the text already ends in `.`, `!` or `?`.
fn stop(raw: &str) -> &'static str {
    if raw.trim_end().ends_with(['.', '!', '?']) {
        ""
    } else {
        "."
    }
}

/// Sorts a trail: dated events by day, then the old giving way before the new ruling, then by
/// decision; open events last, proposals before questions.
type Key = (u8, String, u8, String, String);

struct Cx<'a> {
    g: &'a Graph,
    m: &'a Materialized,
    /// The pack that says which block types define entities.
    pack: &'a Pack,
    link: &'a dyn Fn(&BlockId) -> Option<Link>,
    /// The href of the page of the doc that holds a block, from the page being written (`""` for
    /// that page itself): a statement's relative links are read from there.
    doc_page: &'a dyn Fn(&BlockId) -> String,
    /// What a reference that leads nowhere reads as ([`inline::Ctx::unavailable`]).
    unavailable: &'a str,
    /// A named profile's filter of authored local links ([`inline::Ctx::local_link`]).
    local_link: Option<inline::LocalLink<'a>>,
}

impl Cx<'_> {
    fn decision(&self, id: &BlockId) -> Option<&Block> {
        self.m
            .get(id)
            .filter(|b| b.block_type == decision::DECISION)
    }

    fn links_out(&self, id: &BlockId, kind: &str) -> bool {
        self.g
            .outgoing(id)
            .iter()
            .any(|e| matches!(&e.kind, EdgeKind::Link(n) if n == kind))
    }

    /// The statement as a reader sees it: a revocation shows its reason, not the id it names.
    fn statement(&self, d: &Block) -> String {
        // a removed decision's statement is the notice (D-3M81W)
        if d.is_redacted() {
            return CONTENT_REMOVED.to_string();
        }
        let raw = d.field_str("statement").unwrap_or("");
        if self.links_out(&d.id, REVOKES) {
            capitalised(&REVOKE_PREFIX.replace(raw, ""))
        } else {
            raw.to_string()
        }
    }

    /// A sentence `of` holds (a statement, an option, a prompt), as spans.
    fn html(&self, of: &Block, raw: &str) -> String {
        inline::spans_html(
            raw,
            &inline::Ctx {
                link: self.link,
                doc_page: &(self.doc_page)(&of.id),
                unavailable: self.unavailable,
                local_link: self.local_link,
            },
        )
    }

    /// The words of a superseded part: as written, escaped, for a source output; for a named
    /// profile its references read as plain words through the selection (display words, else
    /// the target's label, else `unavailable`), never a token.
    fn part(&self, part: &str) -> String {
        match self.local_link {
            None => html::escape(part),
            Some(_) => html::words_html(part, self.link, self.unavailable),
        }
    }

    fn at<'b>(&self, d: &'b Block) -> Option<&'b str> {
        d.field_str("decided_at")
    }

    fn options(&self, d: &Block) -> Vec<String> {
        match d.fields.get("options") {
            Some(FieldValue::List(l)) => l
                .iter()
                .filter_map(|v| match v {
                    FieldValue::Str(s) if !s.is_empty() => Some(self.html(d, s)),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }

    /// Every decision that governs `b`, by the graph's one route query: `decided_by`,
    /// `implements`, `affects`, and `affects` an entity `b` describes. Every status is kept
    /// (the trail tells what became of a ruling). A decision or an entity that the selection
    /// holds back is not in `m`, so it adds no route.
    fn governing(&self, b: &Block) -> BTreeSet<BlockId> {
        let is_entity = |id: &BlockId| {
            self.m.get(id).is_some_and(|e| {
                self.pack
                    .block_types
                    .get(&e.block_type)
                    .is_some_and(|t| t.traits.contains(&Trait::Defines))
            })
        };
        let mut cache = GoverningCache::default();
        self.g
            .governing_routes(&b.id, GoverningStatuses::All, is_entity, &mut cache)
            .into_iter()
            .map(|r| r.decision)
            .filter(|d| self.decision(d).is_some())
            .collect()
    }

    /// `start` and every decision reached from it through `supersedes` and `revokes` edges, in
    /// either direction, followed transitively.
    fn chain(&self, start: BTreeSet<BlockId>) -> BTreeSet<BlockId> {
        let mut seen = start.clone();
        let mut todo: Vec<BlockId> = start.into_iter().collect();
        while let Some(id) = todo.pop() {
            if seen.len() > self.m.len() {
                break;
            }
            let near = self
                .g
                .outgoing(&id)
                .iter()
                .map(|e| (&e.kind, &e.to))
                .chain(self.g.incoming(&id).iter().map(|e| (&e.kind, &e.from)));
            for (kind, other) in near {
                if matches!(kind, EdgeKind::Link(n) if n == SUPERSEDES || n == REVOKES)
                    && self.decision(other).is_some()
                    && seen.insert(other.clone())
                {
                    todo.push(other.clone());
                }
            }
        }
        seen
    }

    fn dated(
        &self,
        at: Option<&str>,
        rank: u8,
        id: &BlockId,
        detail: &str,
        text: String,
    ) -> (Key, Event) {
        let day = day_key(at);
        (
            (
                0,
                day.clone().unwrap_or_else(|| "9999-99-99".into()),
                rank,
                id.to_string(),
                detail.to_string(),
            ),
            Event {
                when: display_date(at),
                sort_key: day,
                text,
                open: false,
            },
        )
    }

    fn events_of(&self, id: &BlockId, out: &mut BTreeMap<Key, Event>) {
        let (Some(d), Some(status)) = (self.decision(id), self.g.status(id)) else {
            return;
        };
        let raw = self.statement(d);
        let said = self.html(d, &raw);
        let mut put = |(k, e): (Key, Event)| {
            out.insert(k, e);
        };
        match &status {
            DerivedStatus::Proposed => put((
                (1, String::new(), 0, id.to_string(), String::new()),
                Event {
                    when: "Open".into(),
                    sort_key: None,
                    text: format!("Proposed: {said}{}", stop(&raw)),
                    open: true,
                },
            )),
            DerivedStatus::Rejected => {
                let options = self.options(d);
                let tail = if options.is_empty() {
                    String::new()
                } else {
                    format!(" Options considered: {}.", list_and(&options))
                };
                put(self.dated(
                    self.at(d),
                    1,
                    id,
                    "",
                    format!("Set aside: {said}{}{tail}", stop(&raw)),
                ));
            }
            _ => {
                // a revoking decision speaks through the ruling it withdraws
                if !self.links_out(id, REVOKES) {
                    put(self.dated(
                        self.at(d),
                        1,
                        id,
                        "",
                        format!("Ruled: {said}{}", stop(&raw)),
                    ));
                }
                match &status {
                    DerivedStatus::Superseded { by } => {
                        for y in by.iter().filter_map(|y| self.decision(y)) {
                            let new = self.statement(y);
                            put(self.dated(
                                self.at(y),
                                0,
                                id,
                                &y.id.to_string(),
                                format!(
                                    "Superseded: {said} gave way to {}{}",
                                    self.html(y, &new),
                                    stop(&new)
                                ),
                            ));
                        }
                    }
                    DerivedStatus::PartiallySuperseded { by, parts } => {
                        for (y, part) in by.iter().zip(parts) {
                            let Some(y) = self.decision(y) else { continue };
                            let new = self.statement(y);
                            put(self.dated(
                                self.at(y),
                                0,
                                id,
                                &format!("{}/{part}", y.id),
                                format!(
                                    "Partly superseded ({}): {}{}",
                                    self.part(part),
                                    self.html(y, &new),
                                    stop(&new)
                                ),
                            ));
                        }
                    }
                    DerivedStatus::Revoked { by } => {
                        for y in by.iter().filter_map(|y| self.decision(y)) {
                            let reason = self.statement(y);
                            put(self.dated(
                                self.at(y),
                                0,
                                id,
                                &y.id.to_string(),
                                format!("Withdrawn: {}{}", self.html(y, &reason), stop(&reason)),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn link_in<'a>(
    m: &'a Materialized,
    site: &'a Site,
    from: &'a str,
) -> impl Fn(&BlockId) -> Option<Link> + 'a {
    move |id| {
        let b = m.get(id)?;
        Some(Link {
            // a block with no place on the site reads as its label, unlinked
            href: site.url_of(id).map(|url| url.href_from(from)),
            label: site.summary(b),
            title: String::new(),
            key: crate::site::typed_key(b).map(String::from),
            removed: b.is_redacted(),
        })
    }
}

/// The trail of `b`: its governing decisions, what became of each, and the open questions that
/// mention it, read from `g`, the site's state and `pack` alone (a named profile's selected graph
/// and state). A block with nothing to tell has no events. `unavailable` and `local_link` read references and
/// authored links as [`inline::Ctx`] does.
pub fn for_block(
    b: &Block,
    g: &Graph,
    pack: &Pack,
    site: &Site,
    from: &str,
    unavailable: &str,
    local_link: Option<inline::LocalLink>,
) -> Trail {
    // the site holds the state it was built from
    let m = site.m;
    let link = link_in(m, site, from);
    let doc_page = |id: &BlockId| match m.doc_of(id).and_then(|d| site.page(&d.id)) {
        Some(p) if p != from => rel(from, &p),
        _ => String::new(),
    };
    let cx = Cx {
        g,
        m,
        pack,
        link: &link,
        doc_page: &doc_page,
        unavailable,
        local_link,
    };
    let mut events: BTreeMap<Key, Event> = BTreeMap::new();
    for id in cx.chain(cx.governing(b)) {
        cx.events_of(&id, &mut events);
    }
    for e in g.incoming(&b.id) {
        if e.kind != EdgeKind::References {
            continue;
        }
        let Some(q) = m
            .get(&e.from)
            .filter(|q| q.block_type == decision::QUESTION)
        else {
            continue;
        };
        let answered = g
            .incoming(&q.id)
            .iter()
            .any(|a| matches!(&a.kind, EdgeKind::Link(n) if n == ANSWERS));
        if answered {
            continue;
        }
        events.insert(
            (2, String::new(), 0, q.id.to_string(), String::new()),
            Event {
                when: "Open".into(),
                sort_key: None,
                text: format!(
                    "Question: {}",
                    match q.is_redacted() {
                        true => CONTENT_REMOVED.to_string(),
                        false => cx.html(q, q.field_str("prompt").unwrap_or("")),
                    }
                ),
                open: true,
            },
        );
    }
    Trail {
        events: events.into_values().collect(),
    }
}

/// The text a reference to `b` quotes: its summary's words, uncut (a code, diagram or figure
/// body stays as written but for its references, and an unresolved reference reads as the
/// marker's words, so no id is quoted).
fn primary_text(b: &Block, site: &Site) -> String {
    site.summary_words(b, 0, false)
}

/// The doc's title, then the sections down to the block's nearest one (itself, for a section).
fn path_of(b: &Block, m: &Materialized, site: &Site) -> Vec<String> {
    let (mut sections, mut doc) = (vec![], None);
    let mut cur = Some(b);
    let mut steps = 0;
    while let Some(c) = cur {
        steps += 1;
        if steps > m.len() + 1 {
            break;
        }
        if c.block_type == "section" {
            sections.push(site.label(c));
        } else if rdy_model::tree::is_tree_type(&c.block_type) {
            doc = Some(site.label(c));
            break;
        }
        cur = c.parent.as_ref().and_then(|p| m.get(p));
    }
    doc.into_iter().chain(sections.into_iter().rev()).collect()
}

/// The two `data-ref` attributes of a block's wrapper: the head (`[id]` or `[id@version]`, then
/// the path) and the whole line. `pin` is the version name when the page is rendered from a tag.
pub fn ref_attrs(b: &Block, site: &Site, pin: Option<&str>, m: &Materialized) -> (String, String) {
    let path = path_of(b, m, site);
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    let head = head(&b.id.to_string(), pin, &path);
    let line = format_ref(&head, &snippet(&primary_text(b, site), 80));
    (head, line)
}
