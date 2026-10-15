//! The context of every page, and the one place that calls every module that fills a part of it.
use crate::html::{self, BlockCtx, Link};
use crate::inline::{self, Ctx};
use crate::site::{Site, Url, rel};
use crate::trail::Trail;
use crate::{Mode, assets, changes, home, nav, review, search, toc, trail, version};
use rdy_engine::{Materialized, VersionInfo, VersionPosition};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::{Block, BlockId, Hash, decision, tree};
use rdy_schema::Pack;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

/// What a render reads, whichever page it builds.
pub struct Shared<'a> {
    pub site: &'a Site<'a>,
    pub m: &'a Materialized,
    pub g: &'a Graph,
    pub position: &'a VersionPosition,
    pub pack: &'a Pack,
    pub site_name: &'a str,
    pub description: Option<&'a str>,
    pub versions: &'a [VersionInfo],
    /// Landing hash (hex) to the time it was approved, in milliseconds.
    pub landed_ms: &'a BTreeMap<String, i64>,
    /// The figure blobs the store holds, by hash, with their bytes.
    pub blobs: &'a BTreeMap<Hash, Vec<u8>>,
    /// What a draft changes against the canon it is based on; none for canon and tags.
    pub review: Option<&'a review::Review>,
    /// The view or proposal version a review render shows, and the blocks it changes; none for
    /// canon and tags.
    pub reviewed: Option<version::Reviewed>,
    /// The header's words: the pill and its chips.
    pub header: version::Header,
    /// The header's markup, rendered once per relative root in a static build, once in a live
    /// one (see [`Shared::header_html`]).
    pub headers: RefCell<BTreeMap<String, String>>,
    /// The blobs some page links to, by the site-relative path of their file, filled as pages
    /// are built.
    pub used_blobs: RefCell<BTreeMap<String, Hash>>,
    /// A static site, or a live one whose pages carry the label and header slots.
    pub mode: Mode<'a>,
    /// The word diff of each changed block of a review render, by id: an edited block on its
    /// page carries it for Show diff (sr6). Empty for canon and tags.
    pub inline_diffs: BTreeMap<BlockId, String>,
    /// The build writes `changes.html` (a review render whose path no doc owns); the review
    /// chip links it only then.
    pub changes_page: bool,
    /// The build writes `changes-since.html`; the landings chip links it only then.
    pub since_page: bool,
}

/// What the navigation and the home page know about the render beyond the tree: the reference
/// pages it has, and a draft's changes.
#[derive(Clone, Copy)]
pub struct SiteFacts<'a> {
    pub has_terms: bool,
    pub has_questions: bool,
    pub has_disagreements: bool,
    /// What a draft changes against the canon it is based on; none for canon and tags.
    pub review: Option<&'a review::Review>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Crumb {
    pub label: String,
    /// Where the crumb leads; none for the page the reader is on.
    pub href: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PagerLink {
    pub title: String,
    pub href: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Pager {
    pub prev: Option<PagerLink>,
    pub next: Option<PagerLink>,
}

/// One block of a doc, as the doc template and the toc see it.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub id: String,
    pub block_type: String,
    pub depth: usize,
    pub anchor: String,
    /// A reader's word for what the block is.
    pub kind_word: &'static str,
    /// The heading text of a section, the name of what a definition defines.
    pub title: Option<String>,
    pub html: String,
    /// The block's type is presentational: no marker, no "Why ?" trail.
    pub presentational: bool,
    pub trail: Trail,
    pub ref_head: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    /// The word the block carries in a draft render; none on canon and tags.
    pub change: Option<review::Mark>,
    /// For a block marked Edited on a review page: its word diff, which the page holds hidden
    /// until Show diff turns it on (sr6).
    pub diff_html: Option<String>,
    /// The removed lines a draft shows just before this block.
    pub removed_before: Vec<RemovedLine>,
}

/// One removed line of a draft: the old text of a block the draft took out, as HTML.
#[derive(Clone, Debug, Serialize)]
pub struct RemovedLine {
    pub html: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DocBody {
    pub title: String,
    /// The parent folder's title; none at the top level.
    pub eyebrow: Option<String>,
    /// The first block of the first section as HTML (spans), when it is a prose of one paragraph
    /// and the doc has more sections; it is left out of `items`.
    pub lede: Option<String>,
    /// The anchor the lede has on the page (it keeps the one its block would have had).
    pub lede_anchor: Option<String>,
    /// The word the doc's own block carries in a draft render.
    pub change: Option<review::Mark>,
    /// The word diff of the doc's own block (its title), when it is marked Edited (sr6).
    pub diff: Option<String>,
    /// The word the lede carries in a draft render.
    pub lede_change: Option<review::Mark>,
    /// The lede's word diff, when it is marked Edited (sr6).
    pub lede_diff: Option<String>,
    /// The removed lines a draft shows after the last block.
    pub removed_end: Vec<RemovedLine>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChildRow {
    pub href: String,
    pub label: String,
    pub kind: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct FolderBody {
    pub title: String,
    /// The description as written.
    pub description: Option<String>,
    /// The description as HTML (spans).
    pub description_html: Option<String>,
    pub children: Vec<ChildRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TermRow {
    pub anchor: String,
    pub name: String,
    pub body: String,
    /// The term's section in the doc that defines it; none when that doc has no page (a
    /// top-level doc named `glossary`, D-Y4884): the entry then has no "defined in" link.
    pub href: Option<String>,
    pub doc_title: String,
    pub section_title: String,
}

/// The terms of one top-level folder, or of the top-level docs, under a heading (D-Y4884).
#[derive(Clone, Debug, Serialize)]
pub struct TermGroup {
    pub label: String,
    pub anchor: String,
    pub terms: Vec<TermRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GlossaryBody {
    pub groups: Vec<TermGroup>,
    /// Some term has no link: its doc has no page (a top-level doc named `glossary`).
    pub some_unlinked: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestionRow {
    pub anchor: String,
    pub prompt: String,
    pub href: String,
    pub section_title: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestionGroup {
    pub doc_title: String,
    pub questions: Vec<QuestionRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestionsBody {
    pub groups: Vec<QuestionGroup>,
}

/// One link of an entry on the disagreements page.
#[derive(Clone, Debug, Serialize)]
pub struct DisagreementLink {
    pub href: String,
    /// The block's doc title and the first words of the block.
    pub text: String,
    pub title: String,
}

/// One entry: the reader sentence and the blocks it is about. Never an id, a kind name or a
/// severity.
#[derive(Clone, Debug, Serialize)]
pub struct DisagreementRow {
    pub sentence: &'static str,
    /// The first [`SHOWN_LINKS`] links.
    pub links: Vec<DisagreementLink>,
    /// The rest, behind "Show all"; empty when there are `SHOWN_LINKS` or fewer.
    pub more: Vec<DisagreementLink>,
}

/// How many links of a disagreement show before the fold (ls11).
pub const SHOWN_LINKS: usize = 5;

#[derive(Clone, Debug, Serialize)]
pub struct DisagreementsBody {
    pub entries: Vec<DisagreementRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RedirectBody {
    pub label: String,
    pub href: String,
}

/// The part of the context that belongs to one kind of page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Body {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<DocBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<home::Home>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glossary: Option<GlossaryBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions: Option<QuestionsBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disagreements: Option<DisagreementsBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect: Option<RedirectBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<changes::Summary>,
}

/// Everything a template reads.
#[derive(Clone, Debug, Serialize)]
pub struct PageContext {
    pub site_name: String,
    pub site_description: Option<String>,
    pub lang: &'static str,
    pub title: String,
    /// Site-relative, with `/` separators.
    pub page_url: String,
    /// `home`, `folder`, `doc`, `glossary`, `questions`, `disagreements`, `changes` or
    /// `redirect`.
    pub kind: &'static str,
    /// The prefix that leads from this page to the site's root (`../` per level).
    pub root: String,
    /// The text of the inline script that sets the theme before the page paints.
    pub init_js: &'static str,
    /// The text label of the colophon (and the `frozen` flag of the header).
    pub version: version::Label,
    /// The header's version markup: the pill and its chips (`version::header_html`).
    pub header: String,
    pub crumbs: Vec<Crumb>,
    pub nav: nav::Nav,
    pub toc: toc::Toc,
    pub pager: Pager,
    pub search_href: String,
    pub has_mermaid: bool,
    pub has_terms: bool,
    pub has_questions: bool,
    /// A review render with marks, which offers Show changes and Show diff (sr7).
    pub reviewing: bool,
    /// The review bar's words (`version::review_bar`); none on canon and tags.
    pub review_bar: Option<String>,
    /// The heading of the home page's list of changes (`version::changes_heading`); none on canon
    /// and tags.
    pub review_heading: Option<&'static str>,
    /// A page the live site serves: its version label is the slot, and it updates itself.
    pub live: bool,
    pub items: Vec<Item>,
    pub body: Body,
}

/// Which page to build.
pub enum Kind<'a> {
    Home,
    Folder(&'a BlockId),
    Doc(&'a BlockId),
    Glossary,
    Questions,
    Disagreements,
    /// The permalink page of a block and where it leads.
    Redirect(&'a BlockId, &'a Url),
    /// The change summary of a review render (`changes.html`).
    Changes(&'a changes::Summary),
    /// The summary of the landings since the tag (`changes-since.html`).
    ChangesSince(&'a changes::Summary),
}

/// A reader's word for a block's type: the one the eyebrow, the toc and the search use.
fn kind_word(block_type: &str) -> &'static str {
    match block_type {
        "section" => "section",
        "term" => "term",
        "figure" => "figure",
        "table" => "table",
        "code" => "code",
        "diagram" => "diagram",
        _ => "block",
    }
}

impl Shared<'_> {
    /// The header's markup for a page at relative root `root` (RULING F4). A static page links
    /// to the home page's Versions list and to the change pages by its root; a live page links
    /// to `/~versions/`, whose menu the pill opens, and to the change pages under the scope's
    /// path, the same on every page. Each distinct markup is rendered once.
    pub fn header_html(&self, root: &str) -> String {
        let key = match self.mode {
            Mode::Static => root,
            Mode::Live { .. } => "",
        };
        if let Some(h) = self.headers.borrow().get(key) {
            return h.clone();
        }
        let (base, versions, menu) = match self.mode {
            Mode::Static => (
                root.to_string(),
                format!("{root}index.html#versions"),
                false,
            ),
            Mode::Live { root } => (
                root.to_string(),
                format!(
                    "/{}",
                    rdy_engine::site_scope::ScopeRef::Versions.path_prefix()
                ),
                true,
            ),
        };
        let since = format!("{base}{}", rdy_engine::site_scope::SINCE_PAGE);
        let changes = format!("{base}{}", rdy_engine::site_scope::CHANGES_PAGE);
        let html = version::header_html(
            &self.header,
            &version::HeaderLinks {
                versions: &versions,
                since: self.since_page.then_some(since.as_str()),
                changes: self.changes_page.then_some(changes.as_str()),
                menu,
            },
        );
        self.headers
            .borrow_mut()
            .insert(key.to_string(), html.clone());
        html
    }

    /// A link from `from` to the block `id`, if the store resolves it; with no href when the
    /// block has no place on the site (it then reads as its label, unlinked).
    fn link(&self, from: &str, id: &BlockId) -> Option<Link> {
        let b = self.m.get(id)?;
        let Some(url) = self.site.url_of(id) else {
            return Some(Link {
                href: None,
                label: self.site.label(b),
                title: String::new(),
                key: crate::site::typed_key(b).map(String::from),
            });
        };
        Some(Link {
            href: Some(url.href_from(from)),
            // the site's label never shows an id, even of a reference the target's text holds
            label: self.site.label(b),
            // the hover describes where the link leads: for a decision, the block it governs
            title: self.hover(self.site.destination(id).unwrap_or(b), &url),
            key: crate::site::typed_key(b).map(String::from),
        })
    }

    /// The href of the page of the doc that holds `id`, from page `from`, with no anchor; `""`
    /// when that is `from` itself (or the block has no doc with a page).
    fn doc_page(&self, from: &str, id: &BlockId) -> String {
        match self.m.doc_of(id).and_then(|d| self.site.page(&d.id)) {
            Some(p) if p != from => rel(from, &p),
            _ => String::new(),
        }
    }

    /// Where a link leads, as its hover text says it: `Doc › Section` for a block (its nearest
    /// section, itself for a section), the title for a doc or folder, the collected page's name
    /// for a block that lives there.
    fn hover(&self, b: &Block, url: &Url) -> String {
        if tree::is_tree_type(&b.block_type) {
            return self.site.label(b);
        }
        match url.page.as_str() {
            "questions.html" if self.site.open_questions.contains(&b.id) => {
                return "Open questions".into();
            }
            "glossary.html" if self.site.terms.contains(&b.id) => return "Glossary".into(),
            _ => {}
        }
        let doc = self
            .site
            .doc_of(&b.id)
            .map(|d| self.site.label(d))
            .unwrap_or_default();
        let mut cur = Some(b);
        // a damaged store may loop: never walk more steps than there are blocks
        for _ in 0..=self.m.len() {
            let Some(c) = cur else { break };
            if c.block_type == "section" {
                return format!("{doc} › {}", self.site.label(c));
            }
            cur = c.parent.as_ref().and_then(|p| self.m.get(p));
        }
        doc
    }

    /// The version name when the page is rendered from a tag, so that a copied reference can
    /// name it.
    fn pin(&self) -> Option<&str> {
        let p = self.position;
        match (&p.version, p.frozen, p.landings_since) {
            (Some(v), true, 0) => Some(&v.name),
            _ => None,
        }
    }

    fn crumbs(&self, from: &str, chain: &[BlockId], tail: Option<&str>) -> Vec<Crumb> {
        let mut out = vec![Crumb {
            label: self.site_name.to_string(),
            href: (!chain.is_empty() || tail.is_some()).then(|| rel(from, "index.html")),
        }];
        for (i, id) in chain.iter().enumerate() {
            let Some(b) = self.m.get(id) else { continue };
            let here = tail.is_none() && i + 1 == chain.len();
            let href = if here {
                None
            } else {
                self.site.page(id).map(|p| rel(from, &p))
            };
            out.push(Crumb {
                label: self.site.label(b),
                href,
            });
        }
        if let Some(t) = tail {
            out.push(Crumb {
                label: t.to_string(),
                href: None,
            });
        }
        out
    }
}

/// The page context for `kind`, with every module that fills a part of it called.
pub fn build(sh: &Shared, kind: Kind) -> PageContext {
    let site = sh.site;
    let has_terms = !site.terms.is_empty();
    let has_questions = !site.open_questions.is_empty();
    let facts = SiteFacts {
        has_terms,
        has_questions,
        has_disagreements: !site.disagreements.is_empty(),
        review: sh.review,
    };
    let (page_url, kind_name, title, current, chain, tail): (
        String,
        &'static str,
        String,
        Option<&BlockId>,
        Vec<BlockId>,
        Option<&str>,
    ) = match &kind {
        Kind::Home => (
            "index.html".into(),
            "home",
            sh.site_name.to_string(),
            None,
            vec![],
            None,
        ),
        Kind::Folder(id) | Kind::Doc(id) => {
            let b = sh.m.get(id).expect("a page's block exists");
            (
                site.page(id).expect("a page's block has a page"),
                if matches!(kind, Kind::Doc(_)) {
                    "doc"
                } else {
                    "folder"
                },
                site.label(b),
                Some(*id),
                site.chain(id),
                None,
            )
        }
        Kind::Glossary => (
            "glossary.html".into(),
            "glossary",
            "Glossary".into(),
            None,
            vec![],
            Some("Glossary"),
        ),
        Kind::Questions => (
            "questions.html".into(),
            "questions",
            "Open questions".into(),
            None,
            vec![],
            Some("Open questions"),
        ),
        Kind::Disagreements => (
            "disagreements.html".into(),
            "disagreements",
            "Where the docs disagree".into(),
            None,
            vec![],
            Some("Where the docs disagree"),
        ),
        Kind::Redirect(id, _) => {
            let b = sh.m.get(id).expect("a permalink's block exists");
            (
                site.go_page(id),
                "redirect",
                site.label(b),
                None,
                vec![],
                None,
            )
        }
        Kind::Changes(c) => (
            rdy_engine::site_scope::CHANGES_PAGE.into(),
            "changes",
            c.heading.clone(),
            None,
            vec![],
            Some(c.heading.as_str()),
        ),
        Kind::ChangesSince(c) => (
            rdy_engine::site_scope::SINCE_PAGE.into(),
            "changes",
            c.heading.clone(),
            None,
            vec![],
            Some(c.heading.as_str()),
        ),
    };
    let mut body = Body::default();
    let mut items = vec![];
    let mut has_mermaid = false;
    let mut pager = Pager::default();
    match &kind {
        Kind::Home => {
            body.home = Some(home::build(
                site,
                sh.m,
                sh.versions,
                sh.landed_ms,
                sh.description,
                facts,
            ));
        }
        Kind::Folder(id) => {
            let f = sh.m.get(id).expect("a folder exists");
            let children =
                sh.m.children(Some(id))
                    .into_iter()
                    .filter(|c| tree::is_tree_type(&c.block_type) && site.in_nav(&c.id))
                    .filter_map(|c| {
                        Some(ChildRow {
                            href: rel(&page_url, &site.page(&c.id)?),
                            label: site.label(c),
                            kind: if c.block_type == tree::FOLDER {
                                "folder"
                            } else {
                                "doc"
                            },
                        })
                    })
                    .collect();
            let link = |id: &BlockId| sh.link(&page_url, id);
            let description = f.field_str("description");
            body.folder = Some(FolderBody {
                title: title.clone(),
                description: description.map(String::from),
                description_html: description.map(|d| {
                    inline::spans_html(
                        d,
                        &Ctx {
                            link: &link,
                            doc_page: "",
                        },
                    )
                }),
                children,
            });
        }
        Kind::Doc(id) => {
            let doc = sh.m.get(id).expect("a doc exists");
            let (doc_body, built, mermaid) = doc_parts(sh, doc, &page_url, &title);
            body.doc = Some(doc_body);
            items = built;
            has_mermaid = mermaid;
            let (prev, next) = site.prev_next(id);
            let link = |id: Option<BlockId>| {
                let b = sh.m.get(&id?)?;
                Some(PagerLink {
                    title: site.label(b),
                    href: nav::relative(&page_url, &site.page(&b.id)?),
                })
            };
            pager = Pager {
                prev: link(prev),
                next: link(next),
            };
        }
        Kind::Glossary => body.glossary = Some(glossary_body(sh, &page_url)),
        Kind::Questions => body.questions = Some(questions_body(sh, &page_url)),
        Kind::Disagreements => body.disagreements = Some(disagreements_body(sh, &page_url)),
        Kind::Redirect(id, url) => {
            let (_, label, href) = redirect_target(sh, id, url);
            body.redirect = Some(RedirectBody { label, href });
        }
        Kind::Changes(c) | Kind::ChangesSince(c) => body.changes = Some((*c).clone()),
    }
    // a permalink page is a bare redirect: it shows no navigation and no crumbs, so it builds
    // none (building the site's navigation once per block made a render O(blocks x pages))
    let redirect = kind_name == "redirect";
    let root = "../".repeat(page_url.matches('/').count());
    let header = sh.header_html(&root);
    PageContext {
        site_name: sh.site_name.to_string(),
        site_description: sh.description.map(String::from),
        lang: "en",
        title,
        root,
        init_js: assets::INIT_JS,
        version: version::text_label(sh.position, sh.reviewed.as_ref()),
        header,
        crumbs: if redirect {
            vec![]
        } else {
            sh.crumbs(&page_url, &chain, tail)
        },
        nav: if redirect {
            nav::Nav::default()
        } else {
            nav::build(site, current, &page_url, facts)
        },
        // the glossary's "On this page" names its groups, however few; a doc's lists its sections
        toc: match &body.glossary {
            Some(g) => toc::Toc {
                entries: g
                    .groups
                    .iter()
                    .map(|g| toc::TocEntry {
                        label: g.label.clone(),
                        anchor: g.anchor.clone(),
                        depth: 1,
                        children: vec![],
                    })
                    .collect(),
            },
            None => toc::build(&items),
        },
        pager,
        search_href: rel(&page_url, "search-index.json"),
        has_mermaid,
        has_terms,
        has_questions,
        reviewing: sh.review.is_some(),
        review_bar: sh.reviewed.as_ref().map(version::review_bar),
        review_heading: sh
            .reviewed
            .as_ref()
            .map(|r| version::changes_heading(r.kind)),
        live: matches!(sh.mode, Mode::Live { .. }),
        items,
        body,
        kind: kind_name,
        page_url,
    }
}

/// A permalink page's path, and the label and href of the block it leads to.
pub fn redirect_target(sh: &Shared, id: &BlockId, url: &Url) -> (String, String, String) {
    let b = sh.m.get(id).expect("a permalink's block exists");
    let page = sh.site.go_page(id);
    let href = url.href_from(&page);
    (page, sh.site.label(b), href)
}

/// Collects a finished page's search entries. A permalink page has none.
pub fn finish(sh: &Shared, ctx: &PageContext, index: &mut search::Index) {
    if ctx.kind != "redirect" {
        search::collect(index, ctx, sh.m, sh.site);
    }
}

/// The doc's own part of the context, its blocks as items and whether one is a Mermaid diagram.
fn doc_parts(sh: &Shared, doc: &Block, page: &str, title: &str) -> (DocBody, Vec<Item>, bool) {
    let (m, site) = (sh.m, sh.site);
    let link = |id: &BlockId| sh.link(page, id);
    // the lede: the doc's first child when it is a prose of one paragraph (content directly in
    // the doc), else the first block of the first section when it is such a prose and the doc
    // has more sections (a one-section doc keeps all its text under its heading); a prose of
    // several paragraphs or with a list renders in place
    let children = m.children(Some(&doc.id));
    let one_paragraph = |p: &Block| p.block_type == "prose" && inline::is_one_paragraph(&p.body);
    let lede_block = children
        .first()
        .copied()
        .filter(|p| one_paragraph(p))
        .or_else(|| {
            children
                .first()
                .filter(|s| {
                    s.block_type == "section"
                        && children
                            .iter()
                            .filter(|c| c.block_type == "section")
                            .count()
                            > 1
                })
                .and_then(|s| m.children(Some(&s.id)).into_iter().next())
                .filter(|p| one_paragraph(p))
        });
    let own = Ctx {
        link: &link,
        doc_page: "",
    };
    let eyebrow = doc
        .parent
        .as_ref()
        .and_then(|p| m.get(p))
        .map(|f| site.label(f));
    let mut doc_body = DocBody {
        title: title.to_string(),
        eyebrow,
        lede: lede_block.map(|p| inline::spans_html(&p.body, &own)),
        lede_anchor: lede_block.and_then(|p| site.anchors.get(&p.id).cloned()),
        change: sh.review.and_then(|r| r.mark(&doc.id)),
        diff: edited_diff(sh, &doc.id),
        lede_change: lede_block.and_then(|l| sh.review.and_then(|r| r.mark(&l.id))),
        lede_diff: lede_block.and_then(|l| edited_diff(sh, &l.id)),
        removed_end: vec![],
    };
    let (mut items, mut has_mermaid) = (vec![], false);
    let mut stack: Vec<(&Block, usize)> = m
        .children(Some(&doc.id))
        .into_iter()
        .rev()
        .map(|b| (b, 1))
        .collect();
    let used = &sh.used_blobs;
    while let Some((b, depth)) = stack.pop() {
        for c in m.children(Some(&b.id)).into_iter().rev() {
            stack.push((c, depth + 1));
        }
        // a decision has no place in the text, and the lede is shown above it
        if b.block_type == decision::DECISION || lede_block.is_some_and(|l| l.id == b.id) {
            continue;
        }
        // a rationale is supporting text: off the site (FW-D10)
        if b.block_type == crate::site::RATIONALE {
            continue;
        }
        if b.block_type == "diagram" && b.field_str("lang") == Some("mermaid") {
            has_mermaid = true;
        }
        let anchor = site.anchors.get(&b.id).cloned().unwrap_or_default();
        let described_by: Vec<Link> =
            sh.g.incoming(&b.id)
                .iter()
                .filter(|e| matches!(&e.kind, EdgeKind::Link(n) if n == decision::DESCRIBES))
                .filter_map(|e| sh.link(page, &e.from))
                .collect();
        let present = |h: &Hash| sh.blobs.contains_key(h);
        let asset_href = |h: &Hash, media: &str| {
            let path = assets::blob_path(&h.to_string(), media);
            used.borrow_mut().insert(path.clone(), *h);
            rel(page, &path)
        };
        let blob_len = |h: &Hash| sh.blobs.get(h).map(|b| b.len() as u64);
        let ctx = BlockCtx {
            depth,
            anchor: &anchor,
            link: &link,
            // every block of the doc is on the doc's own page
            doc_page: "",
            pack: sh.pack,
            present: &present,
            asset_href: &asset_href,
            blob_len: &blob_len,
            table_number: (b.block_type == "table").then(|| site.number(b)),
            described_by: &described_by,
        };
        let (ref_head, ref_) = trail::ref_attrs(b, site, sh.pin(), m);
        let presentational = sh.pack.is_presentational(&b.block_type);
        // the heading or name as a reader reads it: its references as their labels
        let title = match b.block_type.as_str() {
            "section" => b.field_str("title"),
            "term" | "component" | "actor" | "interface" | "flow" | "value" | "entity" => {
                b.field_str("name")
            }
            _ => None,
        }
        .map(|t| site.words(t));
        items.push(Item {
            id: b.id.to_string(),
            block_type: b.block_type.clone(),
            depth,
            anchor: anchor.clone(),
            kind_word: kind_word(&b.block_type),
            title,
            html: html::block_html(b, &ctx),
            presentational,
            trail: if presentational {
                Trail::default()
            } else {
                trail::for_block(b, sh.g, m, site, page)
            },
            ref_head,
            ref_,
            change: sh.review.and_then(|r| r.mark(&b.id)),
            diff_html: edited_diff(sh, &b.id),
            removed_before: vec![],
        });
    }
    // the removed lines: before the block they stood before, else at the doc's end
    for line in sh.review.map(|r| r.removed_in(&doc.id)).unwrap_or_default() {
        let html = RemovedLine {
            html: removed_html(sh, &line.blocks, page),
        };
        let target = match &line.before {
            Some(b) if lede_block.is_some_and(|l| l.id == *b) => items.first_mut(),
            Some(b) => {
                let id = b.to_string();
                items.iter_mut().find(|i| i.id == id)
            }
            None => None,
        };
        match target {
            Some(item) => item.removed_before.push(html),
            None => doc_body.removed_end.push(html),
        }
    }
    (doc_body, items, has_mermaid)
}

/// The word diff a block marked Edited carries on a review page (sr6); none for any other block.
fn edited_diff(sh: &Shared, id: &BlockId) -> Option<String> {
    let edited = sh.review.and_then(|r| r.mark(id)) == Some(review::Mark::Edited);
    edited.then(|| sh.inline_diffs.get(id).cloned()).flatten()
}

/// The old text of removed blocks as HTML, written outside the block pipeline: each heading text
/// is a plain paragraph (the page's outline stays the draft's), and no element has an id.
fn removed_html(sh: &Shared, blocks: &[Block], page: &str) -> String {
    let link = |id: &BlockId| sh.link(page, id);
    let ictx = Ctx {
        link: &link,
        doc_page: "",
    };
    let used = &sh.used_blobs;
    let present = |h: &Hash| sh.blobs.contains_key(h);
    let asset_href = |h: &Hash, media: &str| {
        let path = assets::blob_path(&h.to_string(), media);
        used.borrow_mut().insert(path.clone(), *h);
        rel(page, &path)
    };
    let blob_len = |h: &Hash| sh.blobs.get(h).map(|b| b.len() as u64);
    let ctx = BlockCtx {
        depth: 1,
        anchor: "",
        link: &link,
        doc_page: "",
        pack: sh.pack,
        present: &present,
        asset_href: &asset_href,
        blob_len: &blob_len,
        table_number: None,
        described_by: &[],
    };
    let mut out = String::new();
    for b in blocks {
        let heading = ["title", "name", "statement", "prompt"]
            .into_iter()
            .find_map(|f| b.field_str(f));
        if let Some(t) = heading {
            out.push_str(&format!(
                "<p class=\"chg-old-title\">{}</p>",
                inline::label_html(t, &ictx)
            ));
        }
        match b.block_type.as_str() {
            "code" | "diagram" => out.push_str(&format!(
                "<pre><code>{}</code></pre>",
                html::escape(&b.body)
            )),
            "table" | "figure" => out.push_str(&html::block_html(b, &ctx)),
            _ => out.push_str(&inline::body_html(&b.body, &ictx)),
        }
    }
    out
}

/// The title of a block's doc and of the nearest section above it, as a reader says where it is;
/// the section's title is empty when the block sits directly in the doc.
fn whereabouts(sh: &Shared, b: &Block) -> (String, String) {
    let doc =
        sh.m.doc_of(&b.id)
            .map(|d| sh.site.label(d))
            .unwrap_or_default();
    let mut cur = b.parent.as_ref().and_then(|p| sh.m.get(p));
    // a damaged store may loop: never walk more steps than there are blocks
    for _ in 0..=sh.m.len() {
        let Some(p) = cur else { break };
        if p.block_type == "section" {
            return (doc, sh.site.label(p));
        }
        cur = p.parent.as_ref().and_then(|p| sh.m.get(p));
    }
    (doc, String::new())
}

fn glossary_body(sh: &Shared, page: &str) -> GlossaryBody {
    let groups: Vec<TermGroup> = sh
        .site
        .term_groups
        .iter()
        .map(|g| TermGroup {
            label: g.label.clone(),
            anchor: g.anchor.clone(),
            terms: term_rows(sh, page, &g.terms),
        })
        .collect();
    let some_unlinked = groups
        .iter()
        .any(|g| g.terms.iter().any(|t| t.href.is_none()));
    GlossaryBody {
        groups,
        some_unlinked,
    }
}

/// The glossary entries of `terms`: each term, its body and where it is defined.
fn term_rows(sh: &Shared, page: &str, terms: &[BlockId]) -> Vec<TermRow> {
    let link = |id: &BlockId| sh.link(page, id);
    terms
        .iter()
        .filter_map(|id| {
            let b = sh.m.get(id)?;
            let (doc_title, section_title) = whereabouts(sh, b);
            let section = b.parent.as_ref().and_then(|p| sh.site.url_of(p));
            Some(TermRow {
                anchor: sh.site.term_anchors.get(id)?.clone(),
                name: sh.site.words(b.field_str("name").unwrap_or("")),
                body: inline::body_inline_html(
                    &b.body,
                    &Ctx {
                        link: &link,
                        doc_page: &sh.doc_page(page, id),
                    },
                ),
                href: section.map(|s| s.href_from(page)),
                doc_title,
                section_title,
            })
        })
        .collect()
}

/// The disagreements page's entries: each block of a group of one entry as a link whose text is
/// its doc title and the first words of the block (the label rule of a reference, p-2M4KD); each
/// page of a group of more as a link whose text is the doc title alone (the block stands for many
/// points). The first [`SHOWN_LINKS`] links show; the rest fold.
fn disagreements_body(sh: &Shared, page: &str) -> DisagreementsBody {
    let entries = sh
        .site
        .disagreements
        .iter()
        .map(|d| {
            // each link with the folders above its doc, top first, as the breadcrumb names them
            let built: Vec<(DisagreementLink, String)> = d
                .blocks
                .iter()
                .filter_map(|id| {
                    let link = sh.link(page, id)?;
                    // the entries hold only blocks that have a place (`set_disagreements`)
                    let href = link.href?;
                    let doc = sh.site.doc_of(id);
                    let folders = doc.map(|doc| folder_path(sh, &doc.id)).unwrap_or_default();
                    let doc = doc.map(|d| sh.site.label(d));
                    let text = match doc {
                        Some(doc) if d.repeats > 1 => doc,
                        Some(doc) if doc != link.label => format!("{doc} › {}", link.label),
                        _ => link.label,
                    };
                    Some((
                        DisagreementLink {
                            href,
                            text,
                            title: link.title,
                        },
                        folders,
                    ))
                })
                .collect();
            let mut links = tell_apart(built);
            let more = links.split_off(links.len().min(SHOWN_LINKS));
            DisagreementRow {
                sentence: d.sentence,
                links,
                more,
            }
        })
        .collect();
    DisagreementsBody { entries }
}

/// The folders above `doc`, top first, joined as the breadcrumb and the hover join them
/// (`Area 3 › Sub`); empty for a doc at the top level.
fn folder_path(sh: &Shared, doc: &BlockId) -> String {
    let chain = sh.site.chain(doc);
    let folders = chain.split_last().map(|(_, f)| f).unwrap_or_default();
    folders
        .iter()
        .filter_map(|id| sh.m.get(id))
        .map(|b| sh.site.label(b))
        .collect::<Vec<_>>()
        .join(" › ")
}

/// The links of one entry, told apart (visual review F1, rule 2.4): when two or more of them
/// would show the same text, each of those leads with its folder path; the same for the hover
/// title. A link whose text is unique stays as it is.
fn tell_apart(built: Vec<(DisagreementLink, String)>) -> Vec<DisagreementLink> {
    let mut texts: HashMap<String, usize> = HashMap::new();
    let mut titles: HashMap<String, usize> = HashMap::new();
    for (l, _) in &built {
        *texts.entry(l.text.clone()).or_default() += 1;
        *titles.entry(l.title.clone()).or_default() += 1;
    }
    built
        .into_iter()
        .map(|(mut l, folders)| {
            if !folders.is_empty() {
                if texts[&l.text] > 1 {
                    l.text = format!("{folders} › {}", l.text);
                }
                if !l.title.is_empty() && titles[&l.title] > 1 {
                    l.title = format!("{folders} › {}", l.title);
                }
            }
            l
        })
        .collect()
}

fn questions_body(sh: &Shared, page: &str) -> QuestionsBody {
    let link = |id: &BlockId| sh.link(page, id);
    let mut groups: Vec<QuestionGroup> = vec![];
    // `open_questions` is in reading order, so one doc's questions are together
    let mut last: Option<BlockId> = None;
    for id in &sh.site.open_questions {
        let Some(b) = sh.m.get(id) else { continue };
        let Some(doc) = sh.m.doc_of(id) else { continue };
        let (doc_title, section_title) = whereabouts(sh, b);
        let Some(from) = sh.site.url_of(&doc.id) else {
            continue;
        };
        let target = Url {
            page: from.page,
            anchor: sh.site.anchors.get(id).cloned(),
        };
        let row = QuestionRow {
            anchor: sh
                .site
                .question_anchors
                .get(id)
                .cloned()
                .unwrap_or_default(),
            prompt: inline::spans_html(
                b.field_str("prompt").unwrap_or(""),
                &Ctx {
                    link: &link,
                    doc_page: &sh.doc_page(page, id),
                },
            ),
            href: target.href_from(page),
            section_title,
        };
        if last.as_ref() != Some(&doc.id) {
            groups.push(QuestionGroup {
                doc_title,
                questions: vec![],
            });
            last = Some(doc.id.clone());
        }
        if let Some(g) = groups.last_mut() {
            g.questions.push(row);
        }
    }
    QuestionsBody { groups }
}
