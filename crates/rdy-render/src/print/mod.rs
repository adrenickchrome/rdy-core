//! The print model (D-WQSAY, D-EVRNM): exactly what a reader sees on the site, in one model that
//! every print format (Markdown, one HTML file, PDF) writes.
//!
//! A print holds a title part (the project's name, its description, the version label and the
//! scope), then every folder and doc page in tree order, then the glossary when the site has one
//! (O2). A named profile's print ([`build_profile`]) is built the same way from its selected view
//! alone, under the profile's title and description, with no scope and only the explicitly chosen
//! tag's name as its label (D-5CVWB, RULING F3). It holds the words of the site's own pages: the
//! blocks each page builds, read back from the renderer's HTML ([`ir::from_html`]). Nothing on
//! the site that acts is printed: no button, script, navigation, search, review mark, change
//! summary, version pill or "Why?" trail (D-X61DT). A diagram prints as its source (O3). No id
//! is shown (O4).
pub mod fonts;
pub mod image;
pub mod ir;
pub mod layout;
pub mod md;
pub mod pdf;
pub mod single;

pub use md::Layout;

use crate::page::{self, Kind};
use crate::prepare::{self, Prepared, Purpose};
use crate::site::Site;
use crate::{Mode, env, tpl, version};
use rdy_engine::{E_BAD_INPUT, E_IO, RdyError, ReadState, Scope};
use rdy_model::{BlockId, Hash, tree};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The print formats an export writes (O1).
#[derive(Clone, Copy, Debug)]
pub enum Format {
    /// One PDF file.
    Pdf,
    /// One HTML file.
    Html,
    /// A Markdown tree.
    Md,
}

/// Prints `scope` as `format` at `out` (D-EVRNM: `rdy export` and `rdy_export`). A layout
/// applies to the Markdown tree only; `None` lays it out plainly. Refuses `inflight`. O29's path
/// rules (`check_out`) come before the print is built, the same for every format. The writer of
/// the format never overwrites: a file must not exist, a directory may be empty. It writes no
/// format-3 tree (O12). Without a profile the print is the full source (RULING F2).
pub fn export(
    rdy: &ReadState,
    scope: &Scope,
    out: &Path,
    format: Format,
    layout: Option<Layout>,
) -> Result<PrintSummary, RdyError> {
    check_layout(format, layout)?;
    // O29 before the print is built: no format writes into the store
    check_out(&rdy.root().join(".rdy"), out)?;
    let p = build(rdy, scope)?;
    write(&p, out, format, layout)
}

/// Prints the publication profile `profile` (an id or key) at `scope_text` (`canon`,
/// `tag:<name>` or `landing:<hash>`) as `format` at `out` (D-5CVWB): the same selected view and
/// reference check as the profile's site, with [`export`]'s layout, path and no-overwrite rules
/// and its writers. The profile resolves and its check runs once, before anything is written: a
/// public profile whose check is blocked refuses with `E_PUBLICATION_REFERENCE` and leaves no
/// file, directory or staging behind. An org profile's warning comes first in the warnings.
pub fn export_profile(
    read: &ReadState,
    scope_text: &str,
    profile: &str,
    out: &Path,
    format: Format,
    layout: Option<Layout>,
) -> Result<PrintSummary, RdyError> {
    check_layout(format, layout)?;
    check_out(&read.root().join(".rdy"), out)?;
    let p = build_profile(read, scope_text, profile)?;
    write(&p, out, format, layout)
}

/// A layout applies to the Markdown tree only.
fn check_layout(format: Format, layout: Option<Layout>) -> Result<(), RdyError> {
    if layout.is_some() && !matches!(format, Format::Md) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "--layout mkdocs applies to the md format only",
            "drop --layout, or export with --format md",
        ));
    }
    Ok(())
}

/// Writes the print `p` as `format` at `out`: every export, with or without a profile, writes
/// through here.
fn write(
    p: &Print,
    out: &Path,
    format: Format,
    layout: Option<Layout>,
) -> Result<PrintSummary, RdyError> {
    match format {
        Format::Pdf => pdf::write_pdf(p, out),
        Format::Html => single::write_single_html(p, out),
        Format::Md => md::write_markdown(p, out, layout.unwrap_or(Layout::Plain)),
    }
}

/// The directory `out` is written in (`.` for a bare name) and its file name. An `out` with no
/// file name (`/`, `..`) is refused (O29).
pub(crate) fn parent_and_name(out: &Path) -> Result<(&Path, &std::ffi::OsStr), RdyError> {
    let name = out.file_name().ok_or_else(|| {
        RdyError::new(
            E_BAD_INPUT,
            format!("{} does not name a directory", out.display()),
            "pass a directory path such as ./docs-md",
        )
    })?;
    let parent = out
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok((parent, name))
}

/// O29's path rules, the same for every format: `out` has a file name, its parent exists, and
/// it is not in the project's `.rdy` directory `rdy_dir`, nor that directory itself. A file in
/// the store can make it unopenable. The paths are compared resolved, so a symlink (the
/// project's `.rdy` may be one), the store's physical path or `..` does not pass. A path named
/// `.rdy` elsewhere is an ordinary path.
pub(crate) fn check_out(rdy_dir: &Path, out: &Path) -> Result<(), RdyError> {
    let (dir, name) = parent_and_name(out)?;
    if !dir.exists() {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} does not exist", dir.display()),
            "create the directory first, or pick another path",
        ));
    }
    let io = |at: &Path, e: std::io::Error| {
        RdyError::new(E_IO, format!("{}: {e}", at.display()), "check the path")
    };
    // a project with no `.rdy` directory has no store to protect
    let rdy = match std::fs::canonicalize(rdy_dir) {
        Ok(rdy) => rdy,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(rdy_dir, e)),
    };
    let mut target = std::fs::canonicalize(dir)
        .map_err(|e| io(dir, e))?
        .join(name);
    // an `out` that is already there, a symlink to the store among them, is where it leads
    if let Ok(resolved) = std::fs::canonicalize(&target) {
        target = resolved;
    }
    if target.starts_with(&rdy) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{} is inside the project .rdy directory", out.display()),
            "export to a path outside .rdy",
        ));
    }
    Ok(())
}

/// A print of one scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Print {
    pub title: String,
    pub description: Option<String>,
    /// The version label as the colophon writes it. A named profile's is its chosen tag's name,
    /// else empty (RULING F3); a writer leaves out an empty label.
    pub label: String,
    /// The scope's text: `canon`, `view:<name>`, `tag:<version>`, `landing:<hash>` or
    /// `proposal:<hash>`. Empty for a named profile; a writer leaves out an empty scope.
    pub scope: String,
    /// A named profile's chosen tag's history holds content a redaction removed
    /// (`VersionInfo::redacted`): each writer marks the label `redacted`, as the profile's site
    /// does beside the tag's name (D-3M81W). False without a chosen tag and for a source print.
    pub tag_redacted: bool,
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

impl Print {
    /// What the Markdown and PDF version line writes after the label: ` (redacted)` for an
    /// affected chosen tag (D-3M81W), else nothing; nothing without a label (RULING F3).
    pub(crate) fn redacted_suffix(&self) -> &'static str {
        if self.tag_redacted && !self.label.is_empty() {
            " (redacted)"
        } else {
            ""
        }
    }
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
    build_from(prepare::source(rdy, scope, Purpose::Print)?)
}

/// The print of the publication profile `profile` (an id or key) at `scope_text` (`canon`,
/// `tag:<name>` or `landing:<hash>`) (D-5CVWB, D-0VCQ5): built as [`build`] builds, from the
/// profile's selected state, induced graph and selected figures alone. Its title and description
/// are the profile's; its label is the explicitly chosen tag's name, else empty (RULING F3); its
/// scope is empty, so it names no source scope or hash. A public profile whose reference check is
/// blocked refuses with `E_PUBLICATION_REFERENCE`.
pub fn build_profile(read: &ReadState, scope_text: &str, profile: &str) -> Result<Print, RdyError> {
    build_from(crate::publication::prepare(
        read,
        scope_text,
        profile,
        Purpose::Print,
    )?)
}

/// [`build`] and [`build_profile`] from what [`prepare::source`] or `publication::prepare`
/// read: it reads no source.
fn build_from(prepared: Prepared) -> Result<Print, RdyError> {
    let Prepared {
        m,
        g,
        pack,
        blobs,
        site_name,
        description,
        position,
        versions,
        landed_ms,
        disagreements,
        scope_text,
        rdy_dir,
        history,
        profile,
    } = prepared;
    let m: &rdy_engine::Materialized = &m;
    let mut site = Site::with_unavailable(
        m,
        &pack,
        &site_name,
        profile.as_ref().map(|_| crate::inline::UNAVAILABLE),
    );
    site.set_disagreements(disagreements);
    // the Markdown tree writes a file per page: two siblings that would be one file refuse every
    // format alike, before anything is built or written
    if let Some(e) = site.path_clash("export") {
        return Err(e);
    }
    // the label as the colophon writes it, which names the view or proposal version a print of
    // one shows; the pages show no review mark
    let local = profile
        .as_ref()
        .map(|_| crate::publication::LocalTargets::of(&site));
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
        blobs: &blobs.bytes,
        removed_blobs: &blobs.removed,
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
        profile: profile.as_ref(),
        local: local.as_ref(),
        unavailable: match profile {
            Some(_) => crate::inline::UNAVAILABLE,
            None => crate::inline::UNRESOLVED,
        },
    };
    // a named profile's label is its chosen tag's name (RULING F3)
    let label = match &profile {
        Some(_) => sh.label().text,
        None => version::text_label(&position, history.reviewed.as_ref()).text,
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
    // the anchor the print gives that block, and a page's own id leads to the page; an anchor no
    // printed block has is dropped from a source print's link, and a named profile's link to it
    // reads as its words, as a link to a site-only page does (D-5CVWB)
    let mut on_page: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for d in &docs {
        ir::anchors(&d.blocks, on_page.entry(d.page.clone()).or_default());
    }
    let profiled = profile.is_some();
    let to = |h: &Href| -> Option<Href> {
        let Href::Page { page, anchor } = h else {
            return Some(h.clone());
        };
        let on = on_page.get(page)?;
        // the printed anchor `anchor` asks for: `Some(None)` for the page itself, `None` when no
        // printed block has it
        let printed: Option<Option<String>> = match anchor.as_deref() {
            None => Some(None),
            Some(a) => match a.parse::<BlockId>() {
                Ok(id) if m.get(&id).is_some() => {
                    match site.url_of(&id).filter(|u| u.page == *page) {
                        // a page's own id leads to the page
                        Some(u) => match u.anchor {
                            None => Some(None),
                            Some(a) => on.contains(&a).then_some(Some(a)),
                        },
                        None => None,
                    }
                }
                _ => on.contains(a).then(|| Some(a.to_string())),
            },
        };
        match printed {
            Some(anchor) => Some(Href::Page {
                page: page.clone(),
                anchor,
            }),
            None if profiled => None,
            None => Some(Href::Page {
                page: page.clone(),
                anchor: None,
            }),
        }
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
        let (Some(bytes), Some((file, media_type))) = (blobs.bytes.get(&h), assets.remove(&h))
        else {
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
    Ok(Print {
        title: site_name.clone(),
        description: description.clone(),
        label,
        scope: scope_text,
        tag_redacted: profile.as_ref().is_some_and(|p| p.tag_redacted),
        docs,
        blobs: print_blobs,
        // the site build's warnings: a summary page a doc keeps, and the glossary's
        warnings: history.warnings,
        rdy_dir,
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
