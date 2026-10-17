//! The print model (D-WQSAY, D-EVRNM): exactly what a reader sees on the site, in one model that
//! every print format (Markdown, one HTML file, PDF) writes.
//!
//! A print holds a title part (the project's name, its description, the version label and the
//! scope), then every folder and doc page in tree order, then the glossary when the site has one
//! (O2). It holds the words of the site's own pages: the blocks each page builds, read back from
//! the renderer's HTML ([`ir::from_html`]). Nothing on the site that acts is printed: no button,
//! script, navigation, search, review mark, change summary, version pill or "Why?" trail
//! (D-X61DT). A diagram prints as its source (O3). No id is shown (O4).
pub mod fonts;
pub mod image;
pub mod ir;
pub mod layout;
pub mod md;
pub mod pdf;
pub mod single;

use crate::page::{self, Kind};
use crate::{Mode, ScopeRead, env, read_scope, review, tpl, version};
use rdy_engine::{RdyError, ReadState, Scope};
use rdy_model::{BlockId, Hash, tree};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A print of one scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Print {
    pub title: String,
    pub description: Option<String>,
    /// The version label as the colophon writes it.
    pub label: String,
    /// The scope's text: `canon`, `view:<name>`, `tag:<version>`, `landing:<hash>` or
    /// `proposal:<hash>`.
    pub scope: String,
    /// Folders and docs in tree order, then the glossary (O2).
    pub docs: Vec<PrintDoc>,
    /// Every figure blob a doc names.
    pub blobs: BTreeMap<Hash, Blob>,
    /// The site build's warnings for the scope.
    pub warnings: Vec<String>,
    /// The project's `.rdy` directory, as the project path names it. The writers refuse an
    /// `out` that resolves into it (O29). It is not part of any output.
    pub rdy_dir: std::path::PathBuf,
}

/// A figure's file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    pub media_type: String,
    pub bytes: Vec<u8>,
    /// The site's name for the file under `assets/` (`assets/<hash>.<extension>`).
    pub file: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Folder,
    Doc,
    Glossary,
}

/// One page of the print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintDoc {
    pub kind: PageKind,
    /// The folder's or doc's id (the Markdown front matter's `rdy_id`, O4); empty for the
    /// glossary, which is no block.
    pub id: String,
    pub title: String,
    /// The site's page url (site-relative, `/` separators).
    pub page: String,
    /// How deep the page sits in the tree: 0 at the top.
    pub depth: usize,
    pub blocks: Vec<PBlock>,
}

/// A block a reader sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PBlock {
    Heading {
        level: u8,
        anchor: Option<String>,
        runs: Vec<Run>,
    },
    Para {
        anchor: Option<String>,
        runs: Vec<Run>,
    },
    List {
        ordered: bool,
        start: u32,
        items: Vec<Vec<PBlock>>,
    },
    Table {
        anchor: Option<String>,
        head: Vec<Vec<Run>>,
        rows: Vec<Vec<Vec<Run>>>,
    },
    Code {
        anchor: Option<String>,
        lang: Option<String>,
        text: String,
    },
    Figure {
        anchor: Option<String>,
        blob: Option<Hash>,
        alt: String,
        caption: Vec<Run>,
    },
    Callout {
        anchor: Option<String>,
        label: String,
        blocks: Vec<PBlock>,
    },
}

/// A piece of a line of text that looks one way. A line break is `\n` in its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<Href>,
}

/// Where a link leads: a page of the print and its anchor, or a place outside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Href {
    Page {
        page: String,
        anchor: Option<String>,
    },
    External(String),
}

/// What an export reply says about the print it wrote.
#[derive(Clone, Debug, Serialize)]
pub struct PrintSummary {
    pub out: String,
    pub format: &'static str,
    pub docs: usize,
    pub folders: usize,
    pub assets: usize,
    pub pages: Option<usize>,
    pub bytes: u64,
    pub warnings: Vec<String>,
}

/// The scope as rdy reads it back: a version by its tag.
fn scope_text(rdy: &ReadState, scope: &Scope) -> Result<String, RdyError> {
    Ok(match scope {
        Scope::Canon => "canon".to_string(),
        Scope::View(v) => format!("view:{v}"),
        Scope::At(h) => {
            let hex = h.to_string();
            match rdy.versions()?.into_iter().find(|v| v.landing == hex) {
                Some(v) => format!("tag:{}", v.name),
                None => format!("landing:{hex}"),
            }
        }
        Scope::Proposal(v) => format!("proposal:{v}"),
        Scope::Inflight => rdy_engine::INFLIGHT_SCOPE.to_string(),
    })
}

/// How deep a page sits: a folder's page is `<folder>/index.html`, one level below the folder.
fn depth_of(page_url: &str, kind: PageKind) -> usize {
    page_url
        .matches('/')
        .count()
        .saturating_sub(usize::from(kind == PageKind::Folder))
}

/// The print of `scope`. Refuses `inflight` (`refuse_inflight_export`). Builds every page as the
/// site does, in `Mode::Static`, with no review marks, no change summary and no header. The same
/// state and scope give an equal print.
pub fn build(rdy: &ReadState, scope: &Scope) -> Result<Print, RdyError> {
    rdy_engine::refuse_inflight_export(scope)?;
    read_scope(rdy, scope, |read| build_from(rdy, scope, read))
}

fn build_from(rdy: &ReadState, scope: &Scope, read: ScopeRead) -> Result<Print, RdyError> {
    let ScopeRead {
        position,
        versions,
        landed_ms,
        m,
        blobs,
        site_name,
        description,
        pack,
        g,
        site,
    } = read;
    // the label as the colophon writes it, which names the view or proposal version a print of
    // one shows; the pages show no review mark
    let listed = review::listed(rdy, scope)?;
    let reviewed = match listed.as_ref().map(|rv| rv.changed_blocks()) {
        Some(blocks) => crate::reviewed_of(rdy, scope, blocks)?,
        None => None,
    };
    let label = version::text_label(&position, reviewed.as_ref()).text;
    let sh = page::Shared {
        site: &site,
        m,
        g: &g,
        position: &position,
        pack: &pack,
        site_name: &site_name,
        description: description.as_deref(),
        versions: &versions,
        landed_ms: &landed_ms,
        blobs: &blobs,
        review: None,
        listed: None,
        reviewed: None,
        header: version::header(&position, None),
        headers: Default::default(),
        used_blobs: Default::default(),
        mode: Mode::Static,
        inline_diffs: Default::default(),
        changes_page: false,
        since_page: false,
    };

    // the pages in the order a reader meets them in the tree, then the glossary
    let mut kinds: Vec<(PageKind, String, Kind)> = site
        .tree_pages()
        .map(|id| {
            if m.get(id).is_some_and(|b| b.block_type == tree::FOLDER) {
                (PageKind::Folder, id.to_string(), Kind::Folder(id))
            } else {
                (PageKind::Doc, id.to_string(), Kind::Doc(id))
            }
        })
        .collect();
    if !site.terms.is_empty() {
        kinds.push((PageKind::Glossary, String::new(), Kind::Glossary));
    }
    // each page with the files it renders: site path → (blob, media type)
    let pages: Vec<_> = kinds
        .into_iter()
        .map(|(kind, id, k)| {
            let ctx = page::build(&sh, k);
            (kind, id, ctx, sh.used_blobs.take())
        })
        .collect();
    let env = env()?;
    // each blob's file and media type as the first figure of the print that shows it names them:
    // a figure the print leaves out, or a download link, names none
    let mut assets: BTreeMap<Hash, (String, String)> = BTreeMap::new();
    let mut docs = vec![];
    for (kind, id, ctx, used) in &pages {
        let at = ctx.page_url.as_str();
        let mut figures = vec![];
        let from_html = |html: &str, figures: &mut Vec<String>| {
            let (blocks, files) = ir::from_html_with_figures(html, at);
            figures.extend(files);
            blocks
        };
        let mut blocks = vec![];
        match kind {
            PageKind::Folder => {
                if let Some(d) = ctx
                    .body
                    .folder
                    .as_ref()
                    .and_then(|f| f.description_html.as_ref())
                {
                    blocks.extend(from_html(&format!("<p>{d}</p>"), &mut figures));
                }
            }
            PageKind::Doc => {
                if let Some(d) = &ctx.body.doc
                    && let Some(lede) = &d.lede
                {
                    let mut lede = from_html(&format!("<p>{lede}</p>"), &mut figures);
                    if let Some(a) = &d.lede_anchor {
                        ir::set_anchor(&mut lede, a);
                    }
                    blocks.extend(lede);
                }
                for item in &ctx.items {
                    let mut b = from_html(&item.html, &mut figures);
                    if !item.anchor.is_empty() {
                        ir::set_anchor(&mut b, &item.anchor);
                    }
                    blocks.extend(b);
                }
            }
            PageKind::Glossary => {
                let html = env
                    .get_template("partials/glossary.html")
                    .map_err(tpl)?
                    .render(ctx)
                    .map_err(tpl)?;
                blocks.extend(from_html(&html, &mut figures));
            }
        }
        for file in figures {
            if let Some((h, media_type)) = used.get(&file) {
                assets
                    .entry(*h)
                    .or_insert_with(|| (file, media_type.clone()));
            }
        }
        docs.push(PrintDoc {
            kind: *kind,
            id: id.clone(),
            title: ctx.title.clone(),
            page: ctx.page_url.clone(),
            depth: depth_of(&ctx.page_url, *kind),
            blocks,
        });
    }
    // a link leads to a page of the print and an anchor a block of that page has: a link to a
    // page the print leaves out reads as its words; a block's id (the site's id anchor) reads as
    // the anchor the print gives that block; an anchor no printed block has is dropped
    let mut on_page: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for d in &docs {
        ir::anchors(&d.blocks, on_page.entry(d.page.clone()).or_default());
    }
    let to = |h: &Href| -> Option<Href> {
        let Href::Page { page, anchor } = h else {
            return Some(h.clone());
        };
        let on = on_page.get(page)?;
        let anchor = anchor.as_deref().and_then(|a| {
            let a = match a.parse::<BlockId>() {
                Ok(id) if m.get(&id).is_some() => {
                    site.url_of(&id).filter(|u| u.page == *page)?.anchor?
                }
                _ => a.to_string(),
            };
            on.contains(&a).then_some(a)
        });
        Some(Href::Page {
            page: page.clone(),
            anchor,
        })
    };
    for d in &mut docs {
        ir::map_links(&mut d.blocks, &to);
    }

    // the files of the figures the pages show
    let mut figures = BTreeSet::new();
    for d in &docs {
        figure_blobs(&d.blocks, &mut figures);
    }
    let mut print_blobs = BTreeMap::new();
    for h in figures {
        let (Some(bytes), Some((file, media_type))) = (blobs.get(&h), assets.remove(&h)) else {
            continue;
        };
        print_blobs.insert(
            h,
            Blob {
                media_type,
                bytes: bytes.clone(),
                file,
            },
        );
    }
    // the site build's warnings: a summary page a doc keeps, and the glossary's
    let since = crate::since_of(rdy, scope, &position);
    let warnings = crate::scope_warnings(
        m,
        &site,
        reviewed.is_some() && listed.is_some(),
        since.as_ref().map(|(name, ..)| name.as_str()),
    );

    Ok(Print {
        title: site_name.clone(),
        description: description.clone(),
        label,
        scope: scope_text(rdy, scope)?,
        docs,
        blobs: print_blobs,
        warnings,
        rdy_dir: rdy.root().join(".rdy"),
    })
}

/// The blobs the figures of `blocks` show.
fn figure_blobs(blocks: &[PBlock], out: &mut BTreeSet<Hash>) {
    for b in blocks {
        match b {
            PBlock::Figure { blob: Some(h), .. } => {
                out.insert(*h);
            }
            PBlock::List { items, .. } => items.iter().for_each(|i| figure_blobs(i, out)),
            PBlock::Callout { blocks, .. } => figure_blobs(blocks, out),
            _ => {}
        }
    }
}
