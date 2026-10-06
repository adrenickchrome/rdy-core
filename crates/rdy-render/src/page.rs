//! The context of every page, and the one place that calls every module that fills a part of it.
use crate::html::{self, BlockCtx, Link};
use crate::inline::{self, Ctx};
use crate::site::{Site, Url, rel};
use crate::trail::Trail;
use crate::{assets, home, nav, review, search, toc, trail, version};
use rdy_engine::{Materialized, VersionInfo, VersionPosition};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::{Block, BlockId, Hash, decision, tree};
use rdy_schema::Pack;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::BTreeMap;

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
    /// The blobs some page links to, by the site-relative path of their file, filled as pages
    /// are built.
    pub used_blobs: RefCell<BTreeMap<String, Hash>>,
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
    /// The word the lede carries in a draft render.
    pub lede_change: Option<review::Mark>,
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
    pub href: String,
    pub doc_title: String,
    pub section_title: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct GlossaryBody {
    pub terms: Vec<TermRow>,
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
    pub redirect: Option<RedirectBody>,
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
    /// `home`, `folder`, `doc`, `glossary`, `questions` or `redirect`.
    pub kind: &'static str,
    /// The prefix that leads from this page to the site's root (`../` per level).
    pub root: String,
    /// The text of the inline script that sets the theme before the page paints.
    pub init_js: &'static str,
    pub version: version::Label,
    pub crumbs: Vec<Crumb>,
    pub nav: nav::Nav,
    pub toc: toc::Toc,
    pub pager: Pager,
    pub search_href: String,
    pub has_mermaid: bool,
    pub has_terms: bool,
    pub has_questions: bool,
    /// A draft render, which offers Show changes.
    pub draft: bool,
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
    /// The permalink page of a block and where it leads.
    Redirect(&'a BlockId, &'a Url),
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
    /// A link from `from` to the block `id`, if it has a place on the site.
    fn link(&self, from: &str, id: &BlockId) -> Option<Link> {
        let b = self.m.get(id)?;
        let url = self.site.url_of(id)?;
        Some(Link {
            href: url.href_from(from),
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
            .m
            .doc_of(&b.id)
            .map(|d| self.site.label(d))
            .unwrap_or_default();
        let mut cur = Some(b);
        while let Some(c) = cur {
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
                has_terms,
                has_questions,
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
        Kind::Redirect(id, url) => {
            let (_, label, href) = redirect_target(sh, id, url);
            body.redirect = Some(RedirectBody { label, href });
        }
    }
    // a permalink page is a bare redirect: it shows no navigation and no crumbs, so it builds
    // none (building the site's navigation once per block made a render O(blocks x pages))
    let redirect = kind_name == "redirect";
    PageContext {
        site_name: sh.site_name.to_string(),
        site_description: sh.description.map(String::from),
        lang: "en",
        title,
        root: "../".repeat(page_url.matches('/').count()),
        init_js: assets::INIT_JS,
        version: version::label(sh.position),
        crumbs: if redirect {
            vec![]
        } else {
            sh.crumbs(&page_url, &chain, tail)
        },
        nav: if redirect {
            nav::Nav::default()
        } else {
            nav::build(site, current, &page_url, has_terms, has_questions)
        },
        toc: toc::build(&items),
        pager,
        search_href: rel(&page_url, "search-index.json"),
        has_mermaid,
        has_terms,
        has_questions,
        draft: sh.review.is_some(),
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
        lede_change: lede_block.and_then(|l| sh.review.and_then(|r| r.mark(&l.id))),
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
            "term" | "component" | "actor" | "interface" | "flow" => b.field_str("name"),
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
    while let Some(p) = cur {
        if p.block_type == "section" {
            return (doc, sh.site.label(p));
        }
        cur = p.parent.as_ref().and_then(|p| sh.m.get(p));
    }
    (doc, String::new())
}

fn glossary_body(sh: &Shared, page: &str) -> GlossaryBody {
    let link = |id: &BlockId| sh.link(page, id);
    let terms = sh
        .site
        .terms
        .iter()
        .filter_map(|id| {
            let b = sh.m.get(id)?;
            let (doc_title, section_title) = whereabouts(sh, b);
            let section = b.parent.as_ref().and_then(|p| sh.site.url_of(p))?;
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
                href: section.href_from(page),
                doc_title,
                section_title,
            })
        })
        .collect();
    GlossaryBody { terms }
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
