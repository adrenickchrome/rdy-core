mod assets;
pub mod disagree;
mod highlight;
mod home;
mod html;
pub mod inline;
pub mod live;
mod llms;
mod nav;
mod page;
pub mod review;
mod search;
mod site;
mod toc;
mod trail;
mod version;

pub use html::{Link, escape, text_html};

use rdy_engine::{E_IO, RdyError, ReadState, Scope};
use rdy_model::{Hash, tree};
use serde::Serialize;
use site::Site;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct RenderSummary {
    /// The HTML pages, not counting the permalink pages.
    pub pages: usize,
    /// The figure files written under `assets/`.
    pub assets: usize,
    /// The permalink pages under `go/`.
    pub redirects: usize,
    /// Things the render left behind that the person should know about (a hidden old copy).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn io(e: std::io::Error) -> RdyError {
    RdyError::new(E_IO, e.to_string(), "check the output directory")
}

fn tpl(e: minijinja::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("template error: {e}"),
        "this is a bug in rdy-render templates",
    )
}

/// The version label's text, as the `label` filter marks it in a template (`{{ version.text |
/// label }}`): the formatter writes it as text and records the bytes it took in the page.
#[derive(Debug)]
struct LabelText(String);

impl minijinja::value::Object for LabelText {}

/// The header's version markup (the pill and its chips), as the `header` filter marks it in a
/// template (`{{ header | header }}`): the formatter writes it as it is (it is built escaped,
/// `version::header_html`) and records the bytes it took in the page.
#[derive(Debug)]
struct HeaderHtml(String);

impl minijinja::value::Object for HeaderHtml {}

/// What a recorded range of a page holds: the version label's text, or the header's markup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SlotKind {
    Label,
    Header,
}

/// A recorded range of a file and what it holds.
pub type Slot = (SlotKind, std::ops::Range<usize>);

/// The page being rendered on this thread, and where the slots went in it. A page renders into
/// it ([`render_page`]), so the formatter knows how many bytes come before a slot.
#[derive(Default)]
struct PageOut {
    bytes: Vec<u8>,
    label_at: Vec<Slot>,
}

thread_local! {
    static PAGE_OUT: std::cell::RefCell<Option<PageOut>> = const { std::cell::RefCell::new(None) };
}

/// The sink [`render_page`] hands minijinja: it appends to this thread's [`PageOut`].
struct PageSink;

impl std::io::Write for PageSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        PAGE_OUT.with_borrow_mut(|o| match o {
            Some(o) => {
                o.bytes.extend_from_slice(b);
                Ok(b.len())
            }
            None => Err(std::io::Error::other("no page is being rendered")),
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Renders a page template: its bytes, and the byte ranges where it wrote the version label and
/// the header, in order.
fn render_page<S: Serialize>(
    env: &minijinja::Environment<'static>,
    name: &str,
    ctx: S,
) -> Result<(Vec<u8>, Vec<Slot>), RdyError> {
    let t = env.get_template(name).map_err(tpl)?;
    PAGE_OUT.set(Some(PageOut::default()));
    let rendered = t.render_captured_to(ctx, PageSink).map(drop);
    let out = PAGE_OUT.take().unwrap_or_default();
    rendered.map_err(tpl)?;
    Ok((out.bytes, out.label_at))
}

fn write_failure() -> minijinja::Error {
    minijinja::Error::new(minijinja::ErrorKind::WriteFailure, "cannot write the page")
}

/// Writes a slot's `text` (already in its final form) into the page [`render_page`] is
/// rendering, and records the bytes it took. A slot must go straight into the page: written into
/// a captured block (a macro call, `{% set %}`, `{% filter %}`), its place would be unknown, so
/// that is an error. An empty slot is an error too.
fn write_slot(
    out: &mut minijinja::Output,
    kind: SlotKind,
    text: &str,
) -> Result<(), minijinja::Error> {
    let len = || PAGE_OUT.with_borrow(|o| o.as_ref().map(|o| o.bytes.len()));
    let start = len();
    out.write_str(text).map_err(|_| write_failure())?;
    // an empty slot takes no bytes, so the check below could not tell a captured block from
    // the page: it is refused (`version::text_label` and `version::header_html` never give one)
    match (start, len()) {
        (Some(start), Some(end)) if !text.is_empty() && end - start == text.len() => {
            PAGE_OUT.with_borrow_mut(|o| {
                if let Some(o) = o {
                    o.label_at.push((kind, start..end));
                }
            });
            Ok(())
        }
        _ => Err(minijinja::Error::new(
            minijinja::ErrorKind::InvalidOperation,
            match kind {
                SlotKind::Label => {
                    "the version label is written only straight into a page that render_page renders"
                }
                SlotKind::Header => {
                    "the version header is written only straight into a page that render_page renders"
                }
            },
        )),
    }
}

/// Writes the version label (a [`LabelText`]): escaped in an HTML template.
fn write_label(
    out: &mut minijinja::Output,
    state: &minijinja::State,
    label: &LabelText,
) -> Result<(), minijinja::Error> {
    let text = match state.auto_escape() {
        minijinja::AutoEscape::Html => escape(&label.0),
        _ => label.0.clone(),
    };
    write_slot(out, SlotKind::Label, &text)
}

/// Writes a value into a template: text is HTML-escaped as `html::escape` does, so that a slash
/// in a path stays a slash (minijinja's own escape turns it into `&#x2f;`); the version label
/// goes through [`write_label`], the header's markup through [`write_slot`] as it is.
fn format_value(
    out: &mut minijinja::Output,
    state: &minijinja::State,
    value: &minijinja::Value,
) -> Result<(), minijinja::Error> {
    if let Some(label) = value.downcast_object_ref::<LabelText>() {
        return write_label(out, state, label);
    }
    if let Some(header) = value.downcast_object_ref::<HeaderHtml>() {
        return write_slot(out, SlotKind::Header, &header.0);
    }
    if matches!(state.auto_escape(), minijinja::AutoEscape::Html)
        && !value.is_safe()
        && let Some(text) = value.as_str()
    {
        return out.write_str(&escape(text)).map_err(|_| write_failure());
    }
    minijinja::escape_formatter(out, state, value)
}

fn env() -> Result<minijinja::Environment<'static>, RdyError> {
    let mut env = minijinja::Environment::new();
    env.set_formatter(format_value);
    env.add_filter("label", |text: String| {
        minijinja::Value::from_object(LabelText(text))
    });
    env.add_filter("header", |html: String| {
        minijinja::Value::from_object(HeaderHtml(html))
    });
    macro_rules! templates {
        ($($name:literal),* $(,)?) => {
            $(env
                .add_template($name, include_str!(concat!("../templates/", $name)))
                .map_err(tpl)?;)*
        };
    }
    templates!(
        "layout.html",
        "index.html",
        "folder.html",
        "doc.html",
        "glossary.html",
        "questions.html",
        "disagreements.html",
        "redirect.html",
        "partials/nav.html",
        "partials/toc.html",
        "partials/pager.html",
        "partials/search.html",
        "partials/block.html",
        "partials/tools.html",
        "partials/why.html",
        "partials/version.html",
        "partials/colophon.html",
        "partials/change.html",
        "partials/removed.html",
    );
    Ok(env)
}

/// The template of a kind of page.
fn template_of(kind: &str) -> &'static str {
    match kind {
        "home" => "index.html",
        "folder" => "folder.html",
        "doc" => "doc.html",
        "glossary" => "glossary.html",
        "questions" => "questions.html",
        "disagreements" => "disagreements.html",
        _ => "redirect.html",
    }
}

/// ren1: the file that marks a directory as a site rdy rendered, which a later render may replace.
pub const SITE_MARKER: &str = ".rdy-site";

/// An io error that names the path it happened at.
fn io_at(what: &str, path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{what} {}: {e}", path.display()),
        "check that the output directory and its parent are writable",
    )
}

/// Renders `scope` as a static site at `out`, as export does: into a staging directory beside
/// `out`, then swapped into place, so renamed and deleted pages never linger. `out` may be new,
/// an empty directory, or a site rdy rendered before; anything else is refused untouched, and a
/// symbolic link is never followed. When `out` cannot be swapped by rename (its parent is not
/// writable, or it is a mount point), its contents are replaced in place instead.
pub fn render_site(rdy: &ReadState, scope: &Scope, out: &Path) -> Result<RenderSummary, RdyError> {
    rdy_engine::refuse_inflight_export(scope)?;
    let refuse = |what: String| {
        RdyError::new(
            rdy_engine::E_BAD_INPUT,
            what,
            "render into a new or empty directory, or one rdy rendered before (it holds .rdy-site)",
        )
    };
    // `link/` and `link/.` resolve a link when examined, so look at the path without them.
    let out: PathBuf = out.components().collect();
    if std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(refuse(format!("{} is a symbolic link", out.display())));
    }
    // `.`, `..` and `x/..` name no directory until resolved (after the link guard above, which
    // looks at the path as given).
    let out = if out.file_name().is_none() {
        std::fs::canonicalize(&out)
            .map_err(|_| refuse(format!("{} does not name a directory", out.display())))?
    } else {
        out
    };
    let out = out.as_path();
    let exists = out.exists();
    if exists {
        if !out.is_dir() {
            return Err(refuse(format!("{} is a file", out.display())));
        }
        let empty = std::fs::read_dir(out)
            .map_err(|e| io_at("cannot read", out, e))?
            .next()
            .is_none();
        let marked = std::fs::symlink_metadata(out.join(SITE_MARKER)).is_ok_and(|m| m.is_file());
        if !empty && !marked {
            return Err(refuse(format!(
                "{} is not empty and is not a site rdy rendered",
                out.display()
            )));
        }
    }
    let name = out
        .file_name()
        .ok_or_else(|| refuse(format!("{} does not name a directory", out.display())))?
        .to_string_lossy()
        .to_string();
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !exists {
        std::fs::create_dir_all(parent).map_err(|e| io_at("cannot create", parent, e))?;
    }
    // The directory the person is standing in is never swapped by rename: that would leave their
    // shell in a deleted directory. Its contents are replaced in place, staged inside it.
    let is_cwd = exists
        && match (std::fs::canonicalize(out), std::env::current_dir()) {
            (Ok(o), Ok(c)) => std::fs::canonicalize(c).is_ok_and(|c| c == o),
            _ => false,
        };
    // Staging goes beside `out`; when that is not allowed and `out` exists, inside it.
    let beside = parent.join(format!(".{name}.rdy-render-partial"));
    if !is_cwd && beside.exists() {
        std::fs::remove_dir_all(&beside).map_err(|e| io_at("cannot clear", &beside, e))?;
    }
    let beside_made = if is_cwd {
        Err(std::io::Error::other("it is the current directory"))
    } else {
        std::fs::create_dir(&beside)
    };
    let (staging, inside) = match beside_made {
        Ok(()) => (beside, false),
        Err(e) if exists => {
            let inside = out.join(STAGING_INSIDE);
            if inside.exists() {
                std::fs::remove_dir_all(&inside).map_err(|e| io_at("cannot clear", &inside, e))?;
            }
            std::fs::create_dir(&inside).map_err(|e2| {
                let mut err = io_at("cannot stage in", &inside, e2);
                err.message += &format!(" (and beside {}: {e})", out.display());
                err
            })?;
            (inside, true)
        }
        Err(e) => return Err(io_at("cannot stage in", &beside, e)),
    };
    let built = build_site(rdy, scope, Mode::Static).and_then(|site| {
        write_files(&staging, &site.files)?;
        Ok(site.summary)
    });
    let mut stats = match built.and_then(|s| {
        std::fs::write(
            staging.join(SITE_MARKER),
            "A site rendered by rdy. rdy render replaces this directory as a whole.\n",
        )
        .map_err(|e| io_at("cannot write the marker in", &staging, e))?;
        Ok(s)
    }) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    let swapped = if !exists {
        std::fs::rename(&staging, out).map_err(|e| io_at("cannot move the site to", out, e))
    } else if inside {
        replace_in_place(&staging, out)
    } else {
        let old = parent.join(format!(".{name}.rdy-render-old"));
        match swap_by_rename(&staging, out, &old) {
            Swap::Swapped(warning) => {
                stats.warnings.extend(warning);
                Ok(())
            }
            Swap::Refused => replace_in_place(&staging, out),
            Swap::OutMissing(e) => Err(RdyError::new(
                E_IO,
                format!(
                    "cannot move the site to {}: {e}; the previous site is now at {}",
                    out.display(),
                    old.display()
                ),
                format!("move {} back to {}", old.display(), out.display()),
            )),
        }
    };
    if let Err(e) = swapped {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    Ok(stats)
}

/// Where staging goes inside `out` when its parent can't take it.
const STAGING_INSIDE: &str = ".rdy-render-partial";

/// How [`swap_by_rename`] ended.
#[derive(Debug, PartialEq, Eq)]
enum Swap {
    /// `staging` is `out` now; the string is a warning when the old copy could not be removed.
    Swapped(Option<String>),
    /// A rename was refused (a mount point, a parent that is not writable) and `out` is as it was.
    Refused,
    /// `out` was moved aside and neither the new site nor the old one could be put in its place:
    /// the previous site is at the aside path. The error is the failed rename's.
    OutMissing(String),
}

/// Moves `out` aside, `staging` into its place, and drops the old copy.
fn swap_by_rename(staging: &Path, out: &Path, old: &Path) -> Swap {
    swap_with(staging, out, old, &|from, to| std::fs::rename(from, to))
}

fn swap_with(
    staging: &Path,
    out: &Path,
    old: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Swap {
    if old.exists() && std::fs::remove_dir_all(old).is_err() {
        return Swap::Refused;
    }
    if rename(out, old).is_err() {
        return Swap::Refused;
    }
    if let Err(e) = rename(staging, out) {
        return match rename(old, out) {
            Ok(()) => Swap::Refused,
            Err(_) => Swap::OutMissing(e.to_string()),
        };
    }
    Swap::Swapped(std::fs::remove_dir_all(old).err().map(|e| {
        format!(
            "the previous site could not be removed and is left at {}: {e}",
            old.display()
        )
    }))
}

/// Replaces what `out` holds with what `staging` holds, without renaming `out` itself. `out`
/// is empty or an earlier site, so its children go; the marker is moved in first so that a
/// failure partway leaves a directory a later render accepts.
fn replace_in_place(staging: &Path, out: &Path) -> Result<(), RdyError> {
    let staging_name = staging.file_name();
    let remove = |path: &Path| -> Result<(), RdyError> {
        let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
        let r = if is_dir {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        r.map_err(|e| io_at("cannot remove", path, e))
    };
    for entry in std::fs::read_dir(out).map_err(|e| io_at("cannot read", out, e))? {
        let path = entry.map_err(|e| io_at("cannot read", out, e))?.path();
        if path.file_name() == staging_name || path.file_name() == Some(SITE_MARKER.as_ref()) {
            continue;
        }
        remove(&path)?;
    }
    let marker = out.join(SITE_MARKER);
    if std::fs::symlink_metadata(&marker).is_ok() {
        remove(&marker)?;
    }
    move_entry(&staging.join(SITE_MARKER), &marker)?;
    for entry in std::fs::read_dir(staging).map_err(|e| io_at("cannot read", staging, e))? {
        let from = entry.map_err(|e| io_at("cannot read", staging, e))?.path();
        let to = out.join(from.file_name().unwrap_or_default());
        move_entry(&from, &to)?;
    }
    std::fs::remove_dir(staging).map_err(|e| io_at("cannot remove", staging, e))
}

/// Renames `from` to `to`, or copies it when a rename can't cross filesystems.
fn move_entry(from: &Path, to: &Path) -> Result<(), RdyError> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_tree(from, to)?;
    let is_dir = std::fs::symlink_metadata(from).is_ok_and(|m| m.is_dir());
    let r = if is_dir {
        std::fs::remove_dir_all(from)
    } else {
        std::fs::remove_file(from)
    };
    r.map_err(|e| io_at("cannot remove", from, e))
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), RdyError> {
    if std::fs::symlink_metadata(from).is_ok_and(|m| m.is_dir()) {
        std::fs::create_dir_all(to).map_err(|e| io_at("cannot create", to, e))?;
        for entry in std::fs::read_dir(from).map_err(|e| io_at("cannot read", from, e))? {
            let child = entry.map_err(|e| io_at("cannot read", from, e))?.path();
            copy_tree(&child, &to.join(child.file_name().unwrap_or_default()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| io_at("cannot copy to", to, e))
    }
}

/// Which site a build is for: `rdy render --out` (static), or the live site `rdy serve` serves,
/// whose noscript sentence also names live updates (ledger O9). Both write the same label. The
/// header's links differ (RULING F4): a static page links to the home page's Versions list and to
/// the change pages by its relative root; a live page links to `/~versions/` (the pill opens the
/// menu) and to the change pages under `root`, the scope's absolute path (`/`, `/~view/<enc>/`,
/// `/~proposal/<ref>/` or `/~tag/<name>/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode<'a> {
    Static,
    Live { root: &'a str },
}

/// A built site: every file by its site-relative path (`index.html`, `go/p-AAAAA.html`,
/// `assets/site.css`, `search-index.json`, `llms.txt`), with its bytes.
pub struct SiteFiles {
    pub files: BTreeMap<String, Vec<u8>>,
    /// The version label's text (the colophon and the llms files), as the build wrote it.
    pub label: String,
    /// The header's version markup (the pill and its chips) as a live build writes it on every
    /// page; empty in a static build, whose pages write it by their relative root.
    pub header: String,
    /// Where the build wrote the label and the header: for each file, the byte ranges with what
    /// each holds, in order. Only the HTML pages (the header, then the colophon's label; never a
    /// permalink page under `go/`), `llms.txt` and `llms-full.txt` (the head line's label) have
    /// ranges; a file with none has no entry. The build records each range as it writes it and
    /// never searches its output for it. The live site's filler puts a new label and header into
    /// exactly these ranges (the label escaped in a page and on one line in the llms files, as
    /// the build writes it there), last range first, and touches nothing else: a doc can hold the
    /// label's text or any other character.
    pub label_at: BTreeMap<String, Vec<Slot>>,
    pub summary: RenderSummary,
}

/// The render's warning when a top-level doc named `glossary` has no page (D-Y4884).
const GLOSSARY_DOC_DROPPED: &str = "the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it.";

/// The same warning when no term is defined anywhere, so the site has no glossary page either.
const GLOSSARY_DOC_DROPPED_NO_TERMS: &str = "the top-level doc named glossary has no page: the site keeps the name glossary.html for its generated glossary. Rename the doc to publish it.";

/// Builds the site of `scope` in memory. The same state, scope and mode give the same bytes.
pub fn build_site(rdy: &ReadState, scope: &Scope, mode: Mode) -> Result<SiteFiles, RdyError> {
    let position = rdy.version_position(scope)?;
    let versions = rdy.versions()?;
    // when each version's landing was approved (only the versions' landings are read)
    let landed_ms: BTreeMap<String, i64> = versions
        .iter()
        .filter_map(|v| {
            let h: Hash = v.landing.parse().ok()?;
            let rec = rdy.reader().landing(&h).ok()?;
            Some((v.landing.clone(), rec.approved_by.at_ms))
        })
        .collect();
    let m = rdy.materialized(scope)?;
    // the figure blobs the store holds (read before `g` borrows `rdy`)
    let mut blobs: BTreeMap<Hash, Vec<u8>> = BTreeMap::new();
    for b in m.iter().filter(|b| b.block_type == "figure") {
        if let Some(h) = b.field_str("blob").and_then(|h| h.parse::<Hash>().ok())
            && !blobs.contains_key(&h)
            && rdy.reader().has_blob(&h).unwrap_or(false)
        {
            blobs.insert(h, rdy.reader().get_blob(&h)?);
        }
    }
    let site_name = rdy.project_name();
    let description = rdy.project_description();
    let pack = rdy.pack().clone();
    let g = rdy.graph(scope)?;
    let env = env()?;
    let mut site = Site::new(&m, &pack, &site_name);
    site.set_disagreements(disagree::list(rdy, scope)?);
    let review = review::build(rdy, scope)?;
    let reviewed = match &review {
        Some(rv) => reviewed_of(rdy, scope, rv)?,
        None => None,
    };
    let header = version::header(&position, reviewed.as_ref());
    let sh = page::Shared {
        site: &site,
        m: &m,
        g: &g,
        position: &position,
        pack: &pack,
        site_name: &site_name,
        description: description.as_deref(),
        versions: &versions,
        landed_ms: &landed_ms,
        blobs: &blobs,
        review: review.as_ref(),
        reviewed: reviewed.clone(),
        header,
        headers: Default::default(),
        used_blobs: Default::default(),
        mode,
    };
    let mut summary = RenderSummary {
        pages: 0,
        assets: 0,
        redirects: 0,
        warnings: vec![],
    };
    if site.glossary_doc_dropped {
        summary.warnings.push(if site.terms.is_empty() {
            GLOSSARY_DOC_DROPPED_NO_TERMS.into()
        } else {
            GLOSSARY_DOC_DROPPED.into()
        });
    }

    // the pages, in the order a reader meets them: the home page, the tree, the collected pages
    let mut kinds = vec![page::Kind::Home];
    kinds.extend(site.tree_pages().map(|id| {
        if m.get(id).is_some_and(|b| b.block_type == tree::FOLDER) {
            page::Kind::Folder(id)
        } else {
            page::Kind::Doc(id)
        }
    }));
    if !site.terms.is_empty() {
        kinds.push(page::Kind::Glossary);
    }
    if !site.open_questions.is_empty() {
        kinds.push(page::Kind::Questions);
    }
    if !site.disagreements.is_empty() {
        kinds.push(page::Kind::Disagreements);
    }
    let mut index = search::Index::default();
    let mut contexts = vec![];
    let mut has_mermaid = false;
    for kind in kinds {
        let ctx = page::build(&sh, kind);
        has_mermaid |= ctx.has_mermaid;
        page::finish(&sh, &ctx, &mut index);
        contexts.push(ctx);
    }
    // the pages are rendered on a few threads
    let pages = par_map(&contexts, |ctx| {
        let (html, at) = render_page(&env, template_of(ctx.kind), ctx)?;
        Ok((ctx.page_url.clone(), html, at))
    })?;
    summary.pages = pages.len();
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut label_at: BTreeMap<String, Vec<Slot>> = BTreeMap::new();
    for (path, html, at) in pages {
        if !at.is_empty() {
            label_at.insert(path.clone(), at);
        }
        files.insert(path, html);
    }

    // a permalink page for every folder, doc and block that has a place on the site: the
    // template is rendered once, and every page fills in its target's href and label; a doc page
    // at a permalink's path (a doc named like an id in a folder named `go`) is replaced by the
    // permalink, as it always was
    let mut redirect: Option<RedirectTemplate> = None;
    for id in m.ids() {
        let Some(url) = site.url_of(id) else { continue };
        let t = match &mut redirect {
            Some(t) => t,
            None => {
                let ctx = page::build(&sh, page::Kind::Redirect(id, &url));
                redirect.insert(RedirectTemplate::new(&env, ctx)?)
            }
        };
        let (page_url, label, href) = page::redirect_target(&sh, id, &url);
        let html = t.fill(&label, &href);
        #[cfg(debug_assertions)]
        if summary.redirects.is_multiple_of(997) {
            let ctx = page::build(&sh, page::Kind::Redirect(id, &url));
            let full = env
                .get_template("redirect.html")
                .map_err(tpl)?
                .render(&ctx)
                .map_err(tpl)?;
            debug_assert_eq!(
                html, full,
                "the prebuilt permalink page differs from the template"
            );
        }
        label_at.remove(&page_url);
        files.insert(page_url, html.into_bytes());
        summary.redirects += 1;
    }

    let label = version::text_label(&position, reviewed.as_ref());
    let header = match mode {
        Mode::Live { .. } => sh.header_html(""),
        Mode::Static => String::new(),
    };
    files.insert(
        "search-index.json".into(),
        search::bytes(&index, &site_name),
    );
    let (llms, at) =
        llms::index_bytes(&site, &contexts, &site_name, description.as_deref(), &label);
    files.insert("llms.txt".into(), llms);
    label_at.insert("llms.txt".into(), vec![(SlotKind::Label, at)]);
    let (llms, at) = llms::full_bytes(&contexts, &site_name, description.as_deref(), &label);
    files.insert("llms-full.txt".into(), llms);
    label_at.insert("llms-full.txt".into(), vec![(SlotKind::Label, at)]);
    for (path, bytes) in assets::files(has_mermaid) {
        files.insert(path.into(), bytes);
    }
    // each blob moves into the file map at its last path, so the build never holds two copies
    // of it (one hash under two paths is copied once)
    let used = sh.used_blobs.take();
    drop(sh);
    let mut uses: BTreeMap<&Hash, usize> = BTreeMap::new();
    for h in used.values() {
        *uses.entry(h).or_default() += 1;
    }
    for (path, h) in &used {
        let left = uses.get_mut(h).expect("counted above");
        *left -= 1;
        let bytes = if *left == 0 {
            blobs.remove(h)
        } else {
            blobs.get(h).cloned()
        };
        files.insert(path.clone(), bytes.expect("a used blob is held"));
        summary.assets += 1;
    }
    Ok(SiteFiles {
        files,
        label: label.text,
        header,
        label_at,
        summary,
    })
}

/// What a review render shows (D-BMX8P): a view's name and owner, or a proposal version's title
/// and author, with the blocks it changes (`Review::changed_blocks`). None for any other scope.
fn reviewed_of(
    rdy: &ReadState,
    scope: &Scope,
    review: &review::Review,
) -> Result<Option<version::Reviewed>, RdyError> {
    let blocks = review.changed_blocks();
    Ok(match scope {
        Scope::View(name) => Some(version::Reviewed {
            kind: version::ReviewedKind::View,
            name: name.clone(),
            author: rdy
                .view_head(name)
                .and_then(|h| h.owner.as_ref())
                .map(|o| o.actor.clone()),
            blocks,
        }),
        Scope::Proposal(v) => {
            let p = rdy.proposal_version(v)?;
            Some(version::Reviewed {
                kind: version::ReviewedKind::Proposal,
                name: p.title,
                author: Some(p.provenance.actor),
                blocks,
            })
        }
        Scope::Canon | Scope::At(_) | Scope::Inflight => None,
    })
}

/// Writes every file of a built site under `out`.
fn write_files(out: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), RdyError> {
    let files: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    make_dirs(out, &files)?;
    write_all(out, &files)
}

/// The permalink template rendered once, with a marker where a page's href and label go: the
/// template reads nothing else that differs from one permalink page to the next.
struct RedirectTemplate {
    /// The text between the markers, `slots.len() + 1` pieces.
    pieces: Vec<String>,
    /// Which value each marker stands for: true for the href, false for the label.
    slots: Vec<bool>,
}

impl RedirectTemplate {
    const HREF: char = '\u{E000}';
    const LABEL: char = '\u{E001}';

    fn new(
        env: &minijinja::Environment<'static>,
        mut ctx: page::PageContext,
    ) -> Result<RedirectTemplate, RdyError> {
        ctx.body.redirect = Some(page::RedirectBody {
            label: Self::LABEL.to_string(),
            href: Self::HREF.to_string(),
        });
        let html = env
            .get_template("redirect.html")
            .map_err(tpl)?
            .render(&ctx)
            .map_err(tpl)?;
        let (mut pieces, mut slots, mut cur) = (vec![], vec![], String::new());
        for c in html.chars() {
            if c == Self::HREF || c == Self::LABEL {
                pieces.push(std::mem::take(&mut cur));
                slots.push(c == Self::HREF);
            } else {
                cur.push(c);
            }
        }
        pieces.push(cur);
        Ok(RedirectTemplate { pieces, slots })
    }

    /// The page for a target, its values escaped as the template escapes them.
    fn fill(&self, label: &str, href: &str) -> String {
        let (label, href) = (escape(label), escape(href));
        let mut out = String::with_capacity(
            self.pieces.iter().map(String::len).sum::<usize>() + 2 * href.len() + label.len(),
        );
        for (piece, slot) in self.pieces.iter().zip(&self.slots) {
            out.push_str(piece);
            out.push_str(if *slot { &href } else { &label });
        }
        out.push_str(self.pieces.last().map_or("", String::as_str));
        out
    }
}

/// `f` of every item, in order, worked out on a few threads.
fn par_map<T: Sync, R: Send>(
    items: &[T],
    f: impl Fn(&T) -> Result<R, RdyError> + Sync,
) -> Result<Vec<R>, RdyError> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8);
    let chunk = items.len().div_ceil(threads).max(1);
    let f = &f;
    std::thread::scope(|s| {
        let workers: Vec<_> = items
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().map(f).collect::<Result<Vec<R>, _>>()))
            .collect();
        let mut out = Vec::with_capacity(items.len());
        for w in workers {
            out.extend(w.join().expect("a render thread does not panic")?);
        }
        Ok(out)
    })
}

/// Makes the directories of every (site-relative path, bytes) under `out`.
fn make_dirs(out: &Path, files: &[(&str, &[u8])]) -> Result<(), RdyError> {
    let dirs: BTreeSet<&Path> = files
        .iter()
        .filter_map(|(p, _)| Path::new(p).parent())
        .collect();
    for dir in dirs {
        std::fs::create_dir_all(out.join(dir)).map_err(io)?;
    }
    Ok(())
}

/// Writes every (site-relative path, bytes) under `out`, on a few threads; their directories
/// exist.
fn write_all(out: &Path, files: &[(&str, &[u8])]) -> Result<(), RdyError> {
    par_map(files, |(path, bytes)| {
        std::fs::write(out.join(path), bytes).map_err(io)
    })
    .map(drop)
}

#[cfg(test)]
mod swap_tests {
    use super::*;

    #[test]
    fn par_map_keeps_the_order_and_reports_a_failure() {
        let items: Vec<usize> = (0..1000).collect();
        let doubled = par_map(&items, |i| Ok(i * 2)).unwrap();
        assert_eq!(doubled, items.iter().map(|i| i * 2).collect::<Vec<_>>());
        let failed = par_map(&items, |i| match i {
            700 => Err(RdyError::new(E_IO, "boom", "")),
            _ => Ok(*i),
        });
        assert!(failed.is_err());
        assert!(
            par_map(&Vec::<usize>::new(), |i| Ok(*i))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_prebuilt_permalink_page_escapes_what_it_fills_in() {
        let t = RedirectTemplate {
            pieces: vec!["<a href=\"".into(), "\">".into(), "</a>".into()],
            slots: vec![true, false],
        };
        assert_eq!(
            t.fill("A & <B>", "../x.html#a\"b"),
            format!(
                "<a href=\"{}\">{}</a>",
                escape("../x.html#a\"b"),
                escape("A & <B>")
            )
        );
    }

    #[test]
    fn copy_tree_copies_nested_files() {
        let d = tempfile::tempdir().unwrap();
        let from = d.path().join("from");
        std::fs::create_dir_all(from.join("a/b")).unwrap();
        std::fs::write(from.join("a/b/f.txt"), "x").unwrap();
        std::fs::write(from.join("top.txt"), "y").unwrap();
        let to = d.path().join("to");
        copy_tree(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("a/b/f.txt")).unwrap(), "x");
        assert_eq!(std::fs::read_to_string(to.join("top.txt")).unwrap(), "y");
    }

    #[test]
    fn replace_in_place_clears_the_old_site_and_keeps_a_marker_throughout() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir_all(out.join("old-dir")).unwrap();
        std::fs::write(out.join("old.html"), "old").unwrap();
        std::fs::write(out.join(SITE_MARKER), "old marker").unwrap();
        let staging = d.path().join("stage");
        std::fs::create_dir_all(staging.join("new-dir")).unwrap();
        std::fs::write(staging.join("new.html"), "new").unwrap();
        std::fs::write(staging.join("new-dir/p.html"), "p").unwrap();
        std::fs::write(staging.join(SITE_MARKER), "new marker").unwrap();
        replace_in_place(&staging, &out).unwrap();
        assert!(!staging.exists());
        assert!(!out.join("old.html").exists() && !out.join("old-dir").exists());
        assert_eq!(
            std::fs::read_to_string(out.join("new.html")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("new-dir/p.html")).unwrap(),
            "p"
        );
        assert_eq!(
            std::fs::read_to_string(out.join(SITE_MARKER)).unwrap(),
            "new marker"
        );
    }

    /// Refuses the renames whose target is in `refuse`, otherwise renames.
    fn renamer(refuse: Vec<PathBuf>) -> impl Fn(&Path, &Path) -> std::io::Result<()> {
        move |from, to| {
            if refuse.iter().any(|r| r == to) {
                Err(std::io::Error::other("refused"))
            } else {
                std::fs::rename(from, to)
            }
        }
    }

    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let (staging, out, old) = (
            t.path().join("s"),
            t.path().join("out"),
            t.path().join("old"),
        );
        for (d, f) in [(&staging, "new"), (&out, "prev")] {
            std::fs::create_dir(d).unwrap();
            std::fs::write(d.join(f), f).unwrap();
        }
        (t, staging, out, old)
    }

    #[test]
    fn a_swap_whose_restore_also_fails_reports_out_missing() {
        let (_t, staging, out, old) = dirs();
        let r = renamer(vec![out.clone()]);
        let swap = swap_with(&staging, &out, &old, &r);
        assert!(matches!(swap, Swap::OutMissing(_)), "{swap:?}");
        assert!(!out.exists());
        assert!(old.join("prev").exists(), "the previous site is at old");
    }

    #[test]
    fn a_swap_whose_new_site_fails_but_restore_works_is_refused_with_out_intact() {
        let (_t, staging, out, old) = dirs();
        let calls = std::cell::Cell::new(0);
        let r = |from: &Path, to: &Path| {
            calls.set(calls.get() + 1);
            // the second rename (staging into out) is the one refused
            if calls.get() == 2 {
                Err(std::io::Error::other("refused"))
            } else {
                std::fs::rename(from, to)
            }
        };
        assert_eq!(swap_with(&staging, &out, &old, &r), Swap::Refused);
        assert!(out.join("prev").exists());
        assert!(!old.exists());
    }

    #[test]
    fn a_clean_swap_has_no_warning() {
        let (_t, staging, out, old) = dirs();
        assert_eq!(swap_by_rename(&staging, &out, &old), Swap::Swapped(None));
        assert!(out.join("new").exists());
        assert!(!old.exists());
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    fn env_with(src: &'static str) -> minijinja::Environment<'static> {
        let mut env = env().unwrap();
        env.add_template("t.html", src).unwrap();
        env
    }

    #[test]
    fn a_page_records_where_it_wrote_the_label() {
        let env = env_with("<p>{{ a | label }}</p><i>{{ a | label }}</i>\u{E002}{{ b }}");
        let ctx = serde_json::json!({"a": "x<y", "b": "x<y"});
        let (bytes, at) = render_page(&env, "t.html", ctx).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "<p>x&lt;y</p><i>x&lt;y</i>\u{E002}x&lt;y"
        );
        assert_eq!(at, vec![(SlotKind::Label, 3..9), (SlotKind::Label, 16..22)]);
    }

    #[test]
    fn sr5_a_page_records_where_it_wrote_the_header_unescaped() {
        let env = env_with("<p>{{ h | header }}</p>{{ a | label }}");
        let ctx = serde_json::json!({"h": "<a href=\"x\">v1</a>", "a": "x<y"});
        let (bytes, at) = render_page(&env, "t.html", ctx).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "<p><a href=\"x\">v1</a></p>x&lt;y"
        );
        assert_eq!(
            at,
            vec![(SlotKind::Header, 3..21), (SlotKind::Label, 25..31)]
        );
        let env = env_with("{% set x %}{{ h | header }}{% endset %}{{ x }}");
        let e = render_page(&env, "t.html", serde_json::json!({"h": "<a></a>"})).unwrap_err();
        assert!(e.message.contains("version header"), "{e}");
        let env = env_with("<p>{% set x %}{{ h | header }}{% endset %}</p>{{ x }}");
        let e = render_page(&env, "t.html", serde_json::json!({"h": ""})).unwrap_err();
        assert!(e.message.contains("version header"), "{e}");
    }

    #[test]
    fn the_label_in_a_captured_block_is_refused() {
        let env = env_with("{% set x %}{{ a | label }}{% endset %}{{ x }}");
        let e = render_page(&env, "t.html", serde_json::json!({"a": "v1"})).unwrap_err();
        assert!(e.message.contains("version label"), "{e}");
    }

    /// Final review M13: an empty label takes no bytes, so its place in a captured block would
    /// look right; it is refused.
    #[test]
    fn an_empty_label_in_a_captured_block_is_refused() {
        let env = env_with("<p>{% set x %}{{ a | label }}{% endset %}</p>{{ x }}");
        let e = render_page(&env, "t.html", serde_json::json!({"a": ""})).unwrap_err();
        assert!(e.message.contains("version label"), "{e}");
    }

    #[test]
    fn the_label_outside_a_page_render_is_refused() {
        let env = env_with("{{ a | label }}");
        let t = env.get_template("t.html").unwrap();
        let e = t.render(serde_json::json!({"a": "v1"})).unwrap_err();
        assert!(e.to_string().contains("version label"), "{e}");
    }
}
