//! The "Why ?" trail of a block and the copied reference.
use crate::html::{self, Link};
use crate::inline;
use crate::site::{Site, rel};
use rdy_engine::Materialized;
use rdy_engine::reader::text::decision_statement_source;
use rdy_graph::{DerivedStatus, EdgeKind, GoverningCache, GoverningStatuses, Graph};
use rdy_model::decision::{self, ANSWERS, REVOKES, SUPERSEDES};
use rdy_model::redaction::CONTENT_REMOVED;
use rdy_model::reference::{format_ref, head, snippet};
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::{Pack, Trait};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

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
    /// HTML, empty for none: the closing line that names the entities `b` describes through
    /// which decisions reach it that are no row (D-CMXNP), each linked to its home.
    pub through: String,
}

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
        decision_statement_source(d, self.links_out(&d.id, REVOKES))
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

    /// Whether the selection holds `id` and the pack says its type defines an entity.
    fn is_entity(&self, id: &BlockId) -> bool {
        self.m.get(id).is_some_and(|e| {
            self.pack
                .block_types
                .get(&e.block_type)
                .is_some_and(|t| t.traits.contains(&Trait::Defines))
        })
    }

    /// Every decision that governs `b`, by the graph's one route query, split in two (D-CMXNP):
    /// those that reach `b` through `decided_by`, `implements` or a direct `affects`, and those
    /// that reach it only through an entity it describes. Every status is kept (the trail tells
    /// what became of a ruling). A decision or an entity that the selection holds back is not in
    /// `m`, so it adds no route. `cache` is the one every trail read from `g` shares.
    fn governing(
        &self,
        b: &Block,
        cache: &mut GoverningCache,
    ) -> (BTreeSet<BlockId>, BTreeSet<BlockId>) {
        let (mut direct, mut through) = (BTreeSet::new(), BTreeSet::new());
        for r in self.g.governing_routes(
            &b.id,
            GoverningStatuses::All,
            |id| self.is_entity(id),
            cache,
        ) {
            if self.decision(&r.decision).is_none() {
                continue;
            }
            // a direct route outranks an entity route, so `entity` is set only for the latter
            match r.entity {
                Some(_) => through.insert(r.decision),
                None => direct.insert(r.decision),
            };
        }
        (direct, through)
    }

    /// The closing line of `b`'s trail (D-CMXNP): the entities `b` describes that a decision in
    /// `through` affects, each linked to its home, by label, at most five, then how many more.
    /// Empty when there are none.
    fn through_line(&self, b: &Block, through: &BTreeSet<BlockId>) -> String {
        let affected: BTreeSet<&BlockId> = through
            .iter()
            .flat_map(|d| self.g.outgoing(d))
            .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == decision::AFFECTS))
            .map(|e| &e.to)
            .collect();
        let described: BTreeSet<&BlockId> = self
            .g
            .outgoing(&b.id)
            .iter()
            .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == decision::DESCRIBES))
            .map(|e| &e.to)
            .filter(|e| affected.contains(e) && self.is_entity(e))
            .collect();
        // by label, then by id; the anchor last
        let mut named: Vec<(String, &BlockId, String)> = described
            .into_iter()
            .filter_map(|e| (self.link)(e).filter(|l| !l.dropped).map(|l| (l, e)))
            .map(|(l, e)| (l.label.clone(), e, html::anchor(&l)))
            .collect();
        if named.is_empty() {
            return String::new();
        }
        named.sort();
        let names: Vec<String> = named.into_iter().map(|(_, _, a)| a).collect();
        let list = match names.len() {
            n if n > 5 => format!("{} and {} more", names[..5].join(", "), n - 5),
            _ => list_and(&names),
        };
        format!("Also governed through: {list}.")
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
        // a decision outside a named profile's selection (D-7M0YF)
        if site.drops(id) {
            return Some(Link::dropped());
        }
        let b = m.get(id)?;
        Some(Link {
            // a block with no place on the site reads as its label, unlinked
            href: site.url_of(id).map(|url| url.href_from(from)),
            label: site.summary(b),
            title: String::new(),
            key: crate::site::typed_key(b).map(String::from),
            removed: b.is_redacted(),
            dropped: false,
        })
    }
}

/// The trail of `b`: its governing decisions, what became of each, and the open questions that
/// mention it, read from `g`, the site's state and `pack` alone (a named profile's selected graph
/// and state). A block with nothing to tell has no events. `unavailable` and `local_link` read references and
/// authored links as [`inline::Ctx`] does. `cache` holds the governing route query's entity
/// scans: share one across every trail read from `g`, and start a new one for another graph.
#[allow(clippy::too_many_arguments)]
pub fn for_block(
    b: &Block,
    g: &Graph,
    pack: &Pack,
    site: &Site,
    from: &str,
    unavailable: &str,
    local_link: Option<inline::LocalLink>,
    cache: &mut GoverningCache,
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
    let (direct, through) = cx.governing(b, cache);
    let rows = cx.chain(direct);
    for id in &rows {
        cx.events_of(id, &mut events);
    }
    // a decision the chain made a row is named by no entity
    let through: BTreeSet<BlockId> = through.difference(&rows).cloned().collect();
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
        through: cx.through_line(b, &through),
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

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::{Link as ModelLink, Op};

    fn id(s: &str) -> BlockId {
        BlockId::parse(s).unwrap()
    }

    fn blk(i: &str, ty: &str, parent: Option<&str>, order: &str) -> Block {
        Block {
            id: id(i),
            block_type: ty.to_string(),
            parent: parent.map(id),
            order: order.to_string(),
            fields: BTreeMap::new(),
            body: String::new(),
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            foreign: Default::default(),
            read_identity: None,
        }
    }

    fn field(b: &mut Block, k: &str, v: &str) {
        b.fields.insert(k.into(), FieldValue::Str(v.into()));
    }

    fn link(b: &mut Block, name: &str, target: &str) {
        b.links.entry(name.into()).or_default().push(ModelLink {
            target: id(target),
            part: None,
        });
    }

    /// F2 (DR1b final review): the trails of a site's blocks share one governing cache, so an
    /// entity that many rendered blocks describe is scanned once across them, not once a block.
    #[test]
    fn rf7_trails_share_one_governing_cache() {
        let mut doc = blk("d-AAAAA", rdy_model::tree::DOC, None, "a");
        field(&mut doc, "name", "doc");
        field(&mut doc, "title", "Doc");
        field(&mut doc, "doc_type", "page");
        let mut entity = blk("m-AAAAA", "component", Some("d-AAAAA"), "a");
        field(&mut entity, "name", "Widget");
        let mut d = blk("D-AAAAA", decision::DECISION, Some("d-AAAAA"), "b");
        field(&mut d, "status", "accepted");
        field(&mut d, "statement", "Widgets are blue");
        field(&mut d, "decided_at", "2021-02-08T10:00:00Z");
        link(&mut d, "affects", "m-AAAAA");
        let n = 5;
        let mut blocks = vec![doc, entity, d];
        for i in 0..n {
            let mut p = blk(
                &format!("p-A{i:04}"),
                "prose",
                Some("d-AAAAA"),
                &format!("c{i}"),
            );
            p.body = format!("Paragraph {i}.");
            link(&mut p, "describes", "m-AAAAA");
            blocks.push(p);
        }
        let ops: Vec<Op> = blocks
            .iter()
            .map(|b| Op::Create { block: b.clone() })
            .collect();
        let m = Materialized::default().with(&ops);
        let pack = rdy_schema::PackSet::with_builtin()
            .resolve("rdy-standard")
            .expect("the pack");
        let g = Graph::build_from(m.iter());
        let site = Site::new(&m, &pack, "Project");
        let mut cache = GoverningCache::default();
        for i in 0..n {
            let b = m.get(&id(&format!("p-A{i:04}"))).expect("the block");
            let t = for_block(b, &g, &pack, &site, "doc.html", "", None, &mut cache);
            // D-CMXNP: the decision reaches p-A{i:04} only through the entity, so it is no
            // row: the trail's closing line names the entity instead
            assert!(t.events.is_empty(), "{:?}", t.events);
            assert!(t.through.contains(">Widget</a>"), "{}", t.through);
        }
        assert_eq!(cache.entity_scans(), 1, "one scan for the shared entity");
    }

    /// A doc `d-AAAAA` holding `blocks`, as a state, its graph and the standard pack. `held`
    /// names blocks the graph holds but the selection's state does not (a named profile's
    /// held-back block).
    fn world(blocks: Vec<Block>, held: &[&str]) -> (Materialized, Graph, Pack) {
        let mut doc = blk("d-AAAAA", rdy_model::tree::DOC, None, "a");
        field(&mut doc, "name", "doc");
        field(&mut doc, "title", "Doc");
        field(&mut doc, "doc_type", "page");
        let all: Vec<Block> = std::iter::once(doc).chain(blocks).collect();
        let ops = |keep: &dyn Fn(&Block) -> bool| -> Vec<Op> {
            all.iter()
                .filter(|b| keep(b))
                .map(|b| Op::Create { block: b.clone() })
                .collect()
        };
        let full = Materialized::default().with(&ops(&|_| true));
        let g = Graph::build_from(full.iter());
        let m = Materialized::default().with(&ops(&|b| !held.contains(&b.id.to_string().as_str())));
        let pack = rdy_schema::PackSet::with_builtin()
            .resolve("rdy-standard")
            .expect("the pack");
        (m, g, pack)
    }

    fn entity(i: &str, name: &str, order: &str) -> Block {
        let mut e = blk(i, "component", Some("d-AAAAA"), order);
        field(&mut e, "name", name);
        e
    }

    fn ruling(i: &str, statement: &str, at: &str, order: &str, links: &[(&str, &str)]) -> Block {
        let mut d = blk(i, decision::DECISION, Some("d-AAAAA"), order);
        field(&mut d, "status", "accepted");
        field(&mut d, "statement", statement);
        field(&mut d, "decided_at", at);
        for (n, t) in links {
            link(&mut d, n, t);
        }
        d
    }

    fn prose(i: &str, order: &str, describes: &[&str]) -> Block {
        let mut p = blk(i, "prose", Some("d-AAAAA"), order);
        p.body = "A paragraph.".into();
        for t in describes {
            link(&mut p, "describes", t);
        }
        p
    }

    fn trail_of(i: &str, m: &Materialized, g: &Graph, pack: &Pack) -> Trail {
        let site = Site::new(m, pack, "Project");
        let b = m.get(&id(i)).expect("the block");
        for_block(
            b,
            g,
            pack,
            &site,
            "doc.html",
            "",
            None,
            &mut GoverningCache::default(),
        )
    }

    fn anchors(html: &str) -> usize {
        html.matches("<a href=").count()
    }

    /// D-CMXNP: a decision that reaches a block directly is a row; one that reaches it only
    /// through an entity it describes is not, and the trail ends with one line naming that
    /// entity, linked to its home.
    #[test]
    fn cmxnp_entity_only_decision_is_a_line_not_a_row() {
        let (m, g, pack) = world(
            vec![
                entity("m-AAAAA", "Widget", "a"),
                ruling(
                    "D-AAAAA",
                    "Paragraphs are short",
                    "2021-02-08T10:00:00Z",
                    "b",
                    &[("affects", "p-AAAAA")],
                ),
                ruling(
                    "D-BBBBB",
                    "Widgets are blue",
                    "2021-02-09T10:00:00Z",
                    "c",
                    &[("affects", "m-AAAAA")],
                ),
                prose("p-AAAAA", "d", &["m-AAAAA"]),
            ],
            &[],
        );
        let t = trail_of("p-AAAAA", &m, &g, &pack);
        assert_eq!(t.events.len(), 1, "{:?}", t.events);
        assert!(
            t.events[0].text.contains("Paragraphs are short"),
            "{:?}",
            t.events
        );
        assert_eq!(anchors(&t.through), 1, "{}", t.through);
        assert!(
            t.through.starts_with("Also governed through: <a href=\""),
            "{}",
            t.through
        );
        assert!(t.through.ends_with(">Widget</a>."), "{}", t.through);
        // the entity's home: its own place on the site, here on the same page
        assert!(t.through.contains("<a href=\"#widget\">"), "{}", t.through);
    }

    /// D-CMXNP: a decision that reaches the block both directly and through an entity is a row,
    /// and the entity is not named for it.
    #[test]
    fn cmxnp_direct_and_entity_decision_is_a_row_and_names_no_entity() {
        let (m, g, pack) = world(
            vec![
                entity("m-AAAAA", "Widget", "a"),
                ruling(
                    "D-AAAAA",
                    "Widgets are blue",
                    "2021-02-08T10:00:00Z",
                    "b",
                    &[("affects", "m-AAAAA"), ("affects", "p-AAAAA")],
                ),
                entity("m-BBBBB", "Gadget", "e"),
                ruling(
                    "D-BBBBB",
                    "Gadgets are red",
                    "2021-02-09T10:00:00Z",
                    "f",
                    &[("affects", "m-BBBBB")],
                ),
                prose("p-AAAAA", "d", &["m-AAAAA", "m-BBBBB"]),
            ],
            &[],
        );
        let t = trail_of("p-AAAAA", &m, &g, &pack);
        assert_eq!(t.events.len(), 1, "{:?}", t.events);
        assert!(
            t.events[0].text.contains("Widgets are blue"),
            "{:?}",
            t.events
        );
        assert_eq!(anchors(&t.through), 1, "{}", t.through);
        assert!(t.through.contains(">Gadget</a>"), "{}", t.through);
        assert!(
            !t.through.contains("Widget"),
            "no entity-only decision through Widget: {}",
            t.through
        );
    }

    /// D-CMXNP (TASK detail): the line names at most five entities, then the count of the rest.
    #[test]
    fn cmxnp_line_names_five_then_and_n_more() {
        let mut blocks = vec![];
        let names: Vec<String> = (0..7).map(|i| format!("m-AAAA{i}")).collect();
        for (i, e) in names.iter().enumerate() {
            blocks.push(entity(e, &format!("Thing {i}"), &format!("a{i}")));
            blocks.push(ruling(
                &format!("D-AAAA{i}"),
                &format!("Thing {i} is kept"),
                "2021-02-08T10:00:00Z",
                &format!("b{i}"),
                &[("affects", e)],
            ));
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        blocks.push(prose("p-AAAAA", "c", &refs));
        let (m, g, pack) = world(blocks, &[]);
        let t = trail_of("p-AAAAA", &m, &g, &pack);
        assert!(t.events.is_empty(), "{:?}", t.events);
        assert_eq!(anchors(&t.through), 5, "{}", t.through);
        assert!(t.through.ends_with(" and 2 more."), "{}", t.through);
    }

    /// D-CMXNP: an entity the selection holds back is neither named nor counted.
    #[test]
    fn cmxnp_held_back_entity_is_not_named_or_counted() {
        let mut blocks = vec![];
        let names: Vec<String> = (0..6).map(|i| format!("m-AAAA{i}")).collect();
        for (i, e) in names.iter().enumerate() {
            blocks.push(entity(e, &format!("Thing {i}"), &format!("a{i}")));
            blocks.push(ruling(
                &format!("D-AAAA{i}"),
                &format!("Thing {i} is kept"),
                "2021-02-08T10:00:00Z",
                &format!("b{i}"),
                &[("affects", e)],
            ));
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        blocks.push(prose("p-AAAAA", "c", &refs));
        let (m, g, pack) = world(blocks, &["m-AAAA3"]);
        let t = trail_of("p-AAAAA", &m, &g, &pack);
        assert!(t.events.is_empty(), "{:?}", t.events);
        assert_eq!(anchors(&t.through), 5, "{}", t.through);
        assert!(!t.through.contains("more"), "{}", t.through);
        assert!(!t.through.contains("Thing 3"), "{}", t.through);
    }

    /// D-CMXNP: the supersedes/revokes chain still follows from the direct rows, and not from
    /// an entity-only decision.
    #[test]
    fn cmxnp_chain_follows_direct_rows_only() {
        let (m, g, pack) = world(
            vec![
                entity("m-AAAAA", "Widget", "a"),
                ruling(
                    "D-AAAAA",
                    "Paragraphs are short",
                    "2021-02-08T10:00:00Z",
                    "b",
                    &[("affects", "p-AAAAA")],
                ),
                ruling(
                    "D-AAAAB",
                    "Paragraphs are long",
                    "2021-03-08T10:00:00Z",
                    "c",
                    &[("supersedes", "D-AAAAA")],
                ),
                ruling(
                    "D-BBBBA",
                    "Widgets are blue",
                    "2021-02-09T10:00:00Z",
                    "d",
                    &[("affects", "m-AAAAA")],
                ),
                ruling(
                    "D-BBBBB",
                    "Widgets are red",
                    "2021-03-09T10:00:00Z",
                    "e",
                    &[("supersedes", "D-BBBBA")],
                ),
                prose("p-AAAAA", "f", &["m-AAAAA"]),
            ],
            &[],
        );
        let t = trail_of("p-AAAAA", &m, &g, &pack);
        let text: Vec<&str> = t.events.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(t.events.len(), 3, "{text:?}");
        assert!(
            text.iter().any(|e| e
                .starts_with("Superseded: Paragraphs are short gave way to Paragraphs are long")),
            "{text:?}"
        );
        assert!(text.iter().all(|e| !e.contains("Widgets")), "{text:?}");
        assert_eq!(anchors(&t.through), 1, "{}", t.through);
    }

    /// D-CMXNP: an entity's own trail still lists its decisions as rows (they reach it directly).
    #[test]
    fn cmxnp_entity_trail_keeps_its_rows() {
        let (m, g, pack) = world(
            vec![
                entity("m-AAAAA", "Widget", "a"),
                ruling(
                    "D-BBBBB",
                    "Widgets are blue",
                    "2021-02-09T10:00:00Z",
                    "c",
                    &[("affects", "m-AAAAA")],
                ),
                prose("p-AAAAA", "d", &["m-AAAAA"]),
            ],
            &[],
        );
        let t = trail_of("m-AAAAA", &m, &g, &pack);
        assert_eq!(t.events.len(), 1, "{:?}", t.events);
        assert!(
            t.events[0].text.contains("Widgets are blue"),
            "{:?}",
            t.events
        );
        assert_eq!(t.through, "");
    }
}
