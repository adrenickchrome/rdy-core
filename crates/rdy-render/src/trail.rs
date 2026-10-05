//! The "Why ?" trail of a block, the agents' affordance and the copied reference.
use crate::html::{self, Link};
use crate::inline::{self, Form};
use crate::site::{Site, rel};
use rdy_engine::Materialized;
use rdy_graph::{DerivedStatus, EdgeKind, Graph};
use rdy_model::decision::{self, AFFECTS, ANSWERS, DECIDED_BY, IMPLEMENTS, REVOKES, SUPERSEDES};
use rdy_model::reference::{format_ref, head, snippet};
use rdy_model::{Block, BlockId, FieldValue, RefMode};
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

/// What the collapsed "For agents" block of a page shows about a block.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Agents {
    pub id: String,
    pub block_type: String,
    pub lifecycle: Option<String>,
    /// The decisions that govern the block, each with its status word.
    pub governing: Vec<(String, String)>,
    pub suspect: bool,
    pub json_href: String,
    /// The `go/<id>.html` page, relative to the site root.
    pub permalink: String,
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
    link: &'a dyn Fn(&BlockId) -> Option<Link>,
    /// The href of the page of the doc that holds a block, from the page being written (`""` for
    /// that page itself): a statement's relative links are read from there.
    doc_page: &'a dyn Fn(&BlockId) -> String,
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
            },
        )
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

    /// Every decision that governs `b`: reached by `decided_by` or `implements` from it, or by
    /// `affects` to it.
    fn governing(&self, b: &Block) -> BTreeSet<BlockId> {
        let mut out = BTreeSet::new();
        for e in self.g.outgoing(&b.id) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == DECIDED_BY || n == IMPLEMENTS)
                && self.decision(&e.to).is_some()
            {
                out.insert(e.to.clone());
            }
        }
        for e in self.g.incoming(&b.id) {
            if matches!(&e.kind, EdgeKind::Link(n) if n == AFFECTS)
                && self.decision(&e.from).is_some()
            {
                out.insert(e.from.clone());
            }
        }
        out
    }

    /// `start` and every decision reached from it through `supersedes` and `revokes` edges, in
    /// either direction, followed transitively.
    fn chain(&self, start: BTreeSet<BlockId>) -> BTreeSet<BlockId> {
        let mut seen = start.clone();
        let mut todo: Vec<BlockId> = start.into_iter().collect();
        while let Some(id) = todo.pop() {
            if seen.len() > self.m.blocks.len() {
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
                                    html::escape(part),
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
        let url = site.url_of(id)?;
        Some(Link {
            href: url.href_from(from),
            label: site.summary(b),
            title: String::new(),
            key: b.field_str("key").map(String::from),
        })
    }
}

/// The trail of `b`: its governing decisions, what became of each, and the open questions that
/// mention it. A block with nothing to tell has no events.
pub fn for_block(b: &Block, g: &Graph, m: &Materialized, site: &Site, from: &str) -> Trail {
    let link = link_in(m, site, from);
    let doc_page = |id: &BlockId| match m.doc_of(id).and_then(|d| site.page(&d.id)) {
        Some(p) if p != from => rel(from, &p),
        _ => String::new(),
    };
    let cx = Cx {
        g,
        m,
        link: &link,
        doc_page: &doc_page,
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
                    cx.html(q, q.field_str("prompt").unwrap_or(""))
                ),
                open: true,
            },
        );
    }
    Trail {
        events: events.into_values().collect(),
    }
}

fn status_word(s: &DerivedStatus) -> &'static str {
    match s {
        DerivedStatus::Proposed => "proposed",
        DerivedStatus::Rejected => "rejected",
        DerivedStatus::Live => "live",
        DerivedStatus::PartiallySuperseded { .. } => "partially superseded",
        DerivedStatus::Superseded { .. } => "superseded",
        DerivedStatus::Revoked { .. } => "revoked",
    }
}

/// What the "For agents" block of `b` shows. `json_href` is the page's JSON twin.
pub fn agents(
    b: &Block,
    g: &Graph,
    m: &Materialized,
    suspects: &BTreeSet<BlockId>,
    site: &Site,
    _from: &str,
    json_href: &str,
) -> Agents {
    let link = |_: &BlockId| None;
    let doc_page = |_: &BlockId| String::new();
    let cx = Cx {
        g,
        m,
        link: &link,
        doc_page: &doc_page,
    };
    let mut governing: Vec<(String, String, String)> = cx
        .governing(b)
        .into_iter()
        .filter_map(|id| {
            let d = m.get(&id)?;
            let status = g.status(&id)?;
            Some((
                d.field_str("decided_at").unwrap_or("9999").to_string(),
                id.to_string(),
                status_word(&status).to_string(),
            ))
        })
        .collect();
    governing.sort();
    Agents {
        id: b.id.to_string(),
        block_type: b.block_type.clone(),
        lifecycle: b.field_str("lifecycle").map(String::from),
        governing: governing.into_iter().map(|(_, id, s)| (id, s)).collect(),
        suspect: suspects.contains(&b.id),
        json_href: json_href.to_string(),
        permalink: site.go_page(&b.id),
    }
}

/// `text` as plain words in `form`: each reference as its display text or its target's label (or
/// current key, when typed as one), a dangling one as the unresolved marker's words (never its
/// id: the copied ref sits in a `data-ref` attribute), `\{{` as `{{`.
fn resolved(text: &str, mode: RefMode, form: Form, site: &Site) -> String {
    site.plain(text, mode, form, 0, &|_| {
        crate::html::UNRESOLVED.to_string()
    })
}

/// The text a reference to `b` quotes: its statement, prompt, name or title, else its body,
/// else its alt text or caption, each as plain words.
fn primary_text(b: &Block, site: &Site) -> String {
    let field = |names: &[(&str, Form)]| {
        names.iter().find_map(|(f, form)| {
            b.field_str(f)
                .filter(|s| !s.trim().is_empty())
                .map(|s| resolved(s, RefMode::Strict, *form, site))
        })
    };
    field(&[
        ("statement", Form::Spans),
        ("prompt", Form::Spans),
        ("name", Form::Label),
        ("title", Form::Label),
    ])
    .or_else(|| {
        Some(resolved(
            &b.body,
            RefMode::for_body(&b.block_type),
            Form::Body,
            site,
        ))
        .filter(|s| !s.trim().is_empty())
    })
    .or_else(|| field(&[("alt", Form::Label), ("caption", Form::Label)]))
    .unwrap_or_default()
}

/// The doc's title, then the sections down to the block's nearest one (itself, for a section).
fn path_of(b: &Block, m: &Materialized, site: &Site) -> Vec<String> {
    let (mut sections, mut doc) = (vec![], None);
    let mut cur = Some(b);
    let mut steps = 0;
    while let Some(c) = cur {
        steps += 1;
        if steps > m.blocks.len() + 1 {
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
