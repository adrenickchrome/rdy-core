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
pub mod single_md;

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

/// The layout of a Markdown export: a tree's layout, or one doc as one file (D-ESHXM).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportLayout {
    /// The Markdown tree, laid out as the layout says.
    Tree(Layout),
    /// One doc as one Markdown file ([`single_md`]).
    Single,
}

impl ExportLayout {
    /// The layout as the CLI and the tools name it.
    pub fn name(self) -> &'static str {
        match self {
            ExportLayout::Tree(l) => l.name(),
            ExportLayout::Single => single_md::LAYOUT,
        }
    }
}

/// What an export asks for besides its scope, path and format: its layout, and the single
/// Markdown file's assets folder (D-6PR3Y), relative to the folder of `out`, as the caller
/// wrote it.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExportOptions<'a> {
    pub layout: Option<ExportLayout>,
    pub assets: Option<&'a str>,
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
    export_with(
        rdy,
        scope,
        out,
        format,
        &ExportOptions {
            layout: layout.map(ExportLayout::Tree),
            assets: None,
        },
    )
}

/// [`export`] with its options: also the single Markdown file and its assets folder (D-ESHXM,
/// D-6PR3Y, D-H1FRY). The layout's refusal comes first, then the assets refusal, then O29's
/// path rules; the single file's paths are checked before the print is built.
pub fn export_with(
    rdy: &ReadState,
    scope: &Scope,
    out: &Path,
    format: Format,
    opts: &ExportOptions,
) -> Result<PrintSummary, RdyError> {
    check_options(format, opts)?;
    // O29 before the print is built: no format writes into the store
    let rdy_dir = rdy.root().join(".rdy");
    check_out(&rdy_dir, out)?;
    match opts.layout {
        Some(ExportLayout::Single) => {
            let paths = single_md::check_paths(&rdy_dir, out, opts.assets)?;
            rdy_engine::refuse_inflight_export(scope)?;
            let (p, facts) = build_single(prepare::source(rdy, scope, Purpose::Print)?)?;
            single_md::write(&p, &facts, &paths)
        }
        layout => {
            let p = build(rdy, scope)?;
            write(&p, out, format, tree_layout(layout))
        }
    }
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
    export_profile_with(
        read,
        scope_text,
        profile,
        out,
        format,
        &ExportOptions {
            layout: layout.map(ExportLayout::Tree),
            assets: None,
        },
    )
}

/// [`export_profile`] with its options, as [`export_with`] takes them.
pub fn export_profile_with(
    read: &ReadState,
    scope_text: &str,
    profile: &str,
    out: &Path,
    format: Format,
    opts: &ExportOptions,
) -> Result<PrintSummary, RdyError> {
    check_options(format, opts)?;
    let rdy_dir = read.root().join(".rdy");
    check_out(&rdy_dir, out)?;
    match opts.layout {
        Some(ExportLayout::Single) => {
            let paths = single_md::check_paths(&rdy_dir, out, opts.assets)?;
            let (p, facts) = build_single(crate::publication::prepare(
                read,
                scope_text,
                profile,
                Purpose::Print,
            )?)?;
            single_md::write(&p, &facts, &paths)
        }
        layout => {
            let p = build_profile(read, scope_text, profile)?;
            write(&p, out, format, tree_layout(layout))
        }
    }
}

/// The tree layout of a layout that is not the single file.
fn tree_layout(layout: Option<ExportLayout>) -> Option<Layout> {
    match layout {
        Some(ExportLayout::Tree(l)) => Some(l),
        _ => None,
    }
}

/// A layout applies to the Markdown format only, and comes first (D-H1FRY); the assets folder
/// applies to the single Markdown file only.
fn check_options(format: Format, opts: &ExportOptions) -> Result<(), RdyError> {
    if let Some(layout) = opts.layout.filter(|_| !matches!(format, Format::Md)) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("--layout {} applies to the md format only", layout.name()),
            "drop --layout, or export with --format md",
        ));
    }
    if opts.assets.is_some() && opts.layout != Some(ExportLayout::Single) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "--assets applies to --format md --layout single only",
            "drop --assets, or export with --format md --layout single",
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
    /// The blobs of the light images the figures show: what the PDF and the single HTML file
    /// hold. A dark image (D-3DAX4) is for the Markdown outputs alone.
    pub(crate) fn light_blobs(&self) -> BTreeSet<Hash> {
        fn walk(blocks: &[PBlock], out: &mut BTreeSet<Hash>) {
            for b in blocks {
                match b {
                    PBlock::Figure { blob: Some(h), .. } => {
                        out.insert(*h);
                    }
                    PBlock::List { items, .. } => items.iter().for_each(|i| walk(i, out)),
                    PBlock::Callout { blocks, .. } | PBlock::Fold { blocks, .. } => {
                        walk(blocks, out)
                    }
                    _ => {}
                }
            }
        }
        let mut out = BTreeSet::new();
        for d in &self.docs {
            walk(&d.blocks, &mut out);
        }
        out
    }

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
    /// A figure: its image's blob, and its dark image's (D-3DAX4) when the page shows one. The
    /// PDF and the single HTML file show the light image only; the Markdown outputs write a
    /// `<picture>` when both files are in the print.
    Figure {
        anchor: Option<String>,
        blob: Option<Hash>,
        dark: Option<Hash>,
        alt: String,
        caption: Vec<Run>,
    },
    Callout {
        anchor: Option<String>,
        label: String,
        blocks: Vec<PBlock>,
    },
    /// A collapsed section (the section's `collapsed` field): its heading and every block under
    /// it, as the section's extent holds them. The Markdown writers write it as `<details>`; the
    /// PDF and the single HTML file show it as a plain open section, so the heading is read as
    /// a [`PBlock::Heading`] of `level` followed by `blocks`.
    Fold {
        anchor: Option<String>,
        level: u8,
        runs: Vec<Run>,
        blocks: Vec<PBlock>,
    },
}

/// A piece of a line of text that looks one way. A line break is `\n` in its text. A remote
/// image (D-3DAX4) is a run of its own whose `text` is the alt text and whose `image` is its
/// `https:` URL: the PDF and the single HTML file show the alt text, inside its link when it
/// has one, and the Markdown outputs write the image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<Href>,
    pub image: Option<String>,
    /// The reference these words are, in the single Markdown file's print alone (D-ESHXM rule 8,
    /// D-6PR3Y rule 6): its block and the id it names. None in every other print.
    pub ref_of: Option<RefOf>,
}

/// Where a reference of the single Markdown file's print stands: the block whose text holds it,
/// and the token it names (an id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefOf {
    pub block: String,
    pub target: String,
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
    /// The single Markdown file's layout (`single`); no other reply has the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<&'static str>,
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
/// read, and a publication's print from what `publication::prepare_resolved` read: it reads no
/// source. A publication's `Prepared` has an empty `rdy_dir`, which no writer places by.
pub(crate) fn build_from(prepared: Prepared) -> Result<Print, RdyError> {
    build_from_as(prepared, false).map(|(p, _)| p)
}

/// The print of the single Markdown file (D-ESHXM): [`build_from`] read without captions,
/// labels or pills, with each doc's links as its file writes them and the facts of its
/// figures and warnings.
fn build_single(prepared: Prepared) -> Result<(Print, single_md::Facts), RdyError> {
    crate::inline::with_ref_markup(|| build_from_as(prepared, true))
}

fn build_from_as(prepared: Prepared, single: bool) -> Result<(Print, single_md::Facts), RdyError> {
    let read_html = if single {
        ir::from_html_single
    } else {
        ir::from_html_with_figures
    };
    let mut facts = single_md::Facts::default();
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
        menu_url: _,
        diagram_cache,
        status,
    } = prepared;
    let m: &rdy_engine::Materialized = &m;
    let mut site = Site::with_words(
        m,
        &pack,
        &site_name,
        profile.as_ref().map(|_| crate::inline::UNAVAILABLE),
        profile
            .as_ref()
            .map(|p| p.dropped_decisions.clone())
            .unwrap_or_default(),
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
        // a print draws no diagram, writes no file and warns of none; each diagram has the site's
        // shape less its drawing (ruling task-6)
        draw_diagrams: false,
        diagrams: std::cell::RefCell::new(page::DiagramAssets::with_cache(diagram_cache)),
        governing: Default::default(),
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
        menu_url: None,
        old_site: None,
        status: &status,
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
            let (blocks, files) = read_html(html, at);
            figures.extend(files);
            blocks
        };
        let mut doc_facts = single_md::DocFacts::default();
        let mut blocks = vec![];
        match kind {
            PageKind::Folder => {
                // the print model reads the text's remote images (D-3DAX4), which the page shows
                // as their alt text
                if let Some(d) = ctx
                    .body
                    .folder
                    .as_ref()
                    .and_then(|f| f.description_md.as_ref().or(f.description_html.as_ref()))
                {
                    blocks.extend(from_html(&format!("<p>{d}</p>"), &mut figures));
                }
            }
            PageKind::Doc => {
                if let Some(d) = &ctx.body.doc
                    && let Some(lede) = d.lede_md.as_ref().or(d.lede.as_ref())
                {
                    let mut lede = from_html(&format!("<p>{lede}</p>"), &mut figures);
                    if let Some(a) = &d.lede_anchor {
                        ir::set_anchor(&mut lede, a);
                    }
                    if single
                        && let Some(doc) = id.parse::<BlockId>().ok().and_then(|i| m.get(&i))
                        && let Some(l) = page::lede_of(m, doc)
                    {
                        single_md::name_refs(&mut lede, &l.id.to_string());
                    }
                    blocks.extend(lede);
                }
                // a collapsed section keeps its extent: the items under it, up to the next item
                // no deeper than it (a filtered child leaves no item, so the heading text alone
                // cannot bound it)
                let mut open: Vec<(usize, PBlock)> = vec![];
                for item in &ctx.items {
                    while open.last().is_some_and(|(d, _)| item.depth <= *d) {
                        let (_, fold) = open.pop().expect("a fold is open");
                        fold_into(&mut open, &mut blocks, fold);
                    }
                    let mut b =
                        from_html(item.md_html.as_ref().unwrap_or(&item.html), &mut figures);
                    if !item.anchor.is_empty() {
                        ir::set_anchor(&mut b, &item.anchor);
                    }
                    if single {
                        single_md::name_refs(&mut b, &item.id);
                        let mut shown = BTreeSet::new();
                        figure_blobs(&b, &mut shown);
                        if !shown.is_empty() {
                            doc_facts
                                .figures
                                .push((item.id.clone(), shown.into_iter().collect()));
                        }
                    }
                    if item.collapsed && matches!(b.first(), Some(PBlock::Heading { .. })) {
                        let PBlock::Heading {
                            level,
                            anchor,
                            runs,
                        } = b.remove(0)
                        else {
                            unreachable!("the first block is a heading")
                        };
                        open.push((
                            item.depth,
                            PBlock::Fold {
                                anchor,
                                level,
                                runs,
                                blocks: b,
                            },
                        ));
                        continue;
                    }
                    match open.last_mut() {
                        Some((_, PBlock::Fold { blocks: inner, .. })) => inner.extend(b),
                        _ => blocks.extend(b),
                    }
                }
                while let Some((_, fold)) = open.pop() {
                    fold_into(&mut open, &mut blocks, fold);
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
        if single && *kind == PageKind::Doc {
            facts.docs.insert(ctx.page_url.clone(), doc_facts);
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
    // the single file's links (D-ESHXM rule 8): before the print's own normalization, which
    // would drop a reference's target, each doc's references and own-page fragments lead to
    // the slug of the heading at or above their target, or read as their words
    if single {
        for d in docs.iter_mut().filter(|d| d.kind == PageKind::Doc) {
            let Some(doc_id) = d.id.parse::<BlockId>().ok() else {
                continue;
            };
            let doc_path = m.path_of(&doc_id).unwrap_or_default();
            let page = d.page.clone();
            let place_of = |token: &str| -> Option<(String, Option<String>)> {
                let id = token.parse::<BlockId>().ok()?;
                m.get(&id)?;
                if id == doc_id {
                    return Some((page.clone(), None));
                }
                site.url_of(&id).map(|u| (u.page, u.anchor))
            };
            let anchor_of = |a: &str| -> String {
                match a.parse::<BlockId>() {
                    Ok(id) if m.get(&id).is_some() => site
                        .url_of(&id)
                        .filter(|u| u.page == page)
                        .and_then(|u| u.anchor)
                        .unwrap_or_else(|| a.to_string()),
                    _ => a.to_string(),
                }
            };
            let places = single_md::Places::of(&d.title, &d.blocks);
            let targets = single_md::Targets {
                doc_path: &doc_path,
                page: &page,
                place_of: &place_of,
                anchor_of: &anchor_of,
            };
            let mut warnings = vec![];
            single_md::resolve(&mut d.blocks, &places, &targets, &mut warnings);
            if let Some(f) = facts.docs.get_mut(&page) {
                f.warnings = warnings;
            }
        }
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
    let print = Print {
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
    };
    Ok((print, facts))
}

/// Puts a finished `fold` into the fold still open above it, else into the page's `blocks`.
fn fold_into(open: &mut [(usize, PBlock)], blocks: &mut Vec<PBlock>, fold: PBlock) {
    match open.last_mut() {
        Some((_, PBlock::Fold { blocks: inner, .. })) => inner.push(fold),
        _ => blocks.push(fold),
    }
}

/// The blobs the figures of `blocks` show, the dark images too.
fn figure_blobs(blocks: &[PBlock], out: &mut BTreeSet<Hash>) {
    for b in blocks {
        match b {
            PBlock::Figure { blob, dark, .. } => {
                out.extend(blob.iter().chain(dark));
            }
            PBlock::List { items, .. } => items.iter().for_each(|i| figure_blobs(i, out)),
            PBlock::Callout { blocks, .. } | PBlock::Fold { blocks, .. } => {
                figure_blobs(blocks, out)
            }
            _ => {}
        }
    }
}
