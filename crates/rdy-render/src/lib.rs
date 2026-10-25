mod assets;
mod changes;
mod diagram_svg;
pub mod disagree;
mod highlight;
mod home;
mod html;
pub mod inline;
pub mod live;
mod llms;
pub mod menu;
mod nav;
mod one_page;
mod page;
mod prepare;
pub mod print;
#[cfg(feature = "probe")]
pub mod probe;
/// The feature-off probe: private, and `hit()` compiles to nothing.
#[cfg(not(feature = "probe"))]
mod probe {
    #[inline(always)]
    pub(crate) fn hit() {}
}
pub mod process;
mod profile_live;
mod publication;
pub mod publish;
pub mod render_input;
pub mod review;
mod search;
mod site;
mod toc;
mod trail;
mod version;

pub use html::{Link, escape, text_html};
pub use one_page::{RenderedPage, render_publication_page};
/// Diagrams drawn as SVG, behind the one pinned crate (D-FY4VF).
pub use rdy_diagram as diagram;

use prepare::{Presentation, ProfilePresentation};
use rdy_engine::publication::PublicationSource;
use rdy_engine::publication_manifest::{
    MANIFEST_RECIPE, MAX_ARTIFACT_BYTES, MAX_FILE_BYTES, MAX_FILES, Manifest, ManifestEntry,
    NOTICE_SLOT, manifest_hash,
};
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::{E_BAD_INPUT, E_IO, E_LIMIT, Materialized, RdyError, ReadState, Scope};
use rdy_model::{Hash, tree};
use serde::Serialize;
use site::Site;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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

/// The empty notice slot of a publication page (hub D-YJ9TA), as a template writes it right after
/// its opening main element (`{{ notice_slot }}`, which only a publication page's context holds):
/// the formatter writes the 18 bytes of [`NOTICE_SLOT`] and records where they went in the page.
/// The hub fills the slot; core writes neither a notice nor its text.
#[derive(Debug)]
struct NoticeHtml;

impl minijinja::value::Object for NoticeHtml {}

/// `E_BAD_INPUT`: a publication page whose template wrote no notice slot, or more than one, or
/// whose recorded slot does not hold the empty slot's bytes.
fn bad_notice_slot() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "publication page has an invalid notice slot",
        "use the templates shipped with this rdy version",
    )
}

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
    /// Where the page wrote its notice slot ([`NoticeHtml`]), when it wrote one.
    notice: Option<std::ops::Range<usize>>,
    /// The page tried to write a second notice slot: the render fails.
    second_notice: bool,
    /// A write the build's limits refused: the render fails with it.
    refused: Option<RdyError>,
}

/// A page [`render_page_out`] rendered: its bytes, its label and header ranges, and its notice
/// slot's range.
#[derive(Debug)]
struct Rendered {
    bytes: Vec<u8>,
    slots: Vec<Slot>,
    notice: Option<std::ops::Range<usize>>,
}

thread_local! {
    static PAGE_OUT: std::cell::RefCell<Option<PageOut>> = const { std::cell::RefCell::new(None) };
}

/// The sink [`render_page`] hands minijinja: it appends to this thread's [`PageOut`], each
/// write admitted first by a publication build's limits when there are some.
struct PageSink<'b>(Option<&'b Budget>);

impl std::io::Write for PageSink<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        PAGE_OUT.with_borrow_mut(|o| match o {
            Some(o) => {
                if let Some(budget) = self.0 {
                    let len = u64::try_from(o.bytes.len()).unwrap_or(u64::MAX);
                    let n = u64::try_from(b.len()).unwrap_or(u64::MAX);
                    if let Err(e) = budget.admit(len, n) {
                        o.refused = Some(e);
                        return Err(std::io::Error::other("over the publication's limits"));
                    }
                }
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
    render_page_out(env, name, ctx).map(|r| (r.bytes, r.slots))
}

/// [`render_page`], with the range of the notice slot the page wrote. A second notice slot is a
/// render error, refused as an invalid notice slot.
fn render_page_out<S: Serialize>(
    env: &minijinja::Environment<'static>,
    name: &str,
    ctx: S,
) -> Result<Rendered, RdyError> {
    render_page_within(env, name, ctx, None)
}

/// [`render_page_out`], each write admitted by `budget` first when there is one: a write over
/// its limits stops the render with the limit's error, before the page holds those bytes.
fn render_page_within<S: Serialize>(
    env: &minijinja::Environment<'static>,
    name: &str,
    ctx: S,
    budget: Option<&Budget>,
) -> Result<Rendered, RdyError> {
    let t = env.get_template(name).map_err(tpl)?;
    PAGE_OUT.set(Some(PageOut::default()));
    let rendered = t.render_captured_to(ctx, PageSink(budget)).map(drop);
    let out = PAGE_OUT.take().unwrap_or_default();
    if let Some(e) = out.refused {
        return Err(e);
    }
    if out.second_notice {
        return Err(bad_notice_slot());
    }
    rendered.map_err(tpl)?;
    Ok(Rendered {
        bytes: out.bytes,
        slots: out.label_at,
        notice: out.notice,
    })
}

/// The context of a publication page: `ctx`, with the notice slot a template writes right after
/// its opening main element.
fn with_notice_slot<S: Serialize>(ctx: &S) -> minijinja::Value {
    minijinja::context! {
        notice_slot => minijinja::Value::from_object(NoticeHtml),
        ..minijinja::Value::from_serialize(ctx)
    }
}

/// The page's notice slot: its range checked to hold the empty slot's bytes, its start as the
/// byte offset a manifest records. None when the page wrote no slot.
fn notice_offset(
    bytes: &[u8],
    notice: Option<&std::ops::Range<usize>>,
) -> Result<Option<u64>, RdyError> {
    let Some(r) = notice else { return Ok(None) };
    if bytes.get(r.clone()) != Some(NOTICE_SLOT) {
        return Err(bad_notice_slot());
    }
    u64::try_from(r.start)
        .map(Some)
        .map_err(|_| bad_notice_slot())
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

/// Writes the empty notice slot ([`NoticeHtml`]) straight into the page [`render_page`] is
/// rendering, and records the bytes it took. A slot in a captured block, outside a page render or
/// written a second time is an error.
fn write_notice(out: &mut minijinja::Output) -> Result<(), minijinja::Error> {
    let slot = std::str::from_utf8(NOTICE_SLOT).expect("the slot is ASCII");
    let len = || PAGE_OUT.with_borrow(|o| o.as_ref().map(|o| o.bytes.len()));
    let start = len();
    out.write_str(slot).map_err(|_| write_failure())?;
    let refused =
        |what: &'static str| minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, what);
    match (start, len()) {
        (Some(start), Some(end)) if end - start == slot.len() => PAGE_OUT.with_borrow_mut(|o| {
            let Some(o) = o else {
                return Err(refused("no page is being rendered"));
            };
            if o.notice.is_some() {
                o.second_notice = true;
                return Err(refused("a page writes one notice slot"));
            }
            o.notice = Some(start..end);
            Ok(())
        }),
        _ => Err(refused(
            "the notice slot is written only straight into a page that render_page renders",
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
    if value.downcast_object_ref::<NoticeHtml>().is_some() {
        return write_notice(out);
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
        "changes.html",
        "redirect.html",
        "versions.html",
        "partials/nav.html",
        "partials/toc.html",
        "partials/pager.html",
        "partials/search.html",
        "partials/block.html",
        "partials/tools.html",
        "partials/why.html",
        "partials/version.html",
        "partials/review_bar.html",
        "partials/colophon.html",
        "partials/change.html",
        "partials/removed.html",
        "partials/glossary.html",
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
        "changes" => "changes.html",
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
    place_site(out, || build_site(rdy, scope, Mode::Static))
}

/// Places the site `build` builds at `out`, as [`render_site`] describes: the path guards, then
/// staging, then `build` (only once the guards pass), the marker, and the swap into place.
/// Staging is cleaned up when `build` or a later step fails.
pub(crate) fn place_site(
    out: &Path,
    build: impl FnOnce() -> Result<SiteFiles, RdyError>,
) -> Result<RenderSummary, RdyError> {
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
    let built = build().and_then(|site| {
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

/// The render's warning when a top-level doc owns `changes.html` (O1, O9).
const CHANGES_DOC_KEPT: &str = "the top-level doc named changes has the page changes.html: the change summary of this view or proposal is left out. Rename the doc to publish it.";

/// The render's warning when a top-level doc owns `changes-since.html` (O1).
fn since_doc_kept(tag: &str) -> String {
    format!(
        "the top-level doc named changes-since has the page changes-since.html: the summary of changes since {tag} is left out. Rename the doc to publish it."
    )
}

/// The render's warning about a top-level doc named `glossary`, when the site has one (D-Y4884).
fn glossary_warning(site: &Site) -> Option<&'static str> {
    site.glossary_doc_dropped
        .then_some(if site.terms.is_empty() {
            GLOSSARY_DOC_DROPPED_NO_TERMS
        } else {
            GLOSSARY_DOC_DROPPED
        })
}

/// The site build's warnings for a scope, in order: the change summary a top-level doc named
/// `changes` keeps out (`reviewed`: the scope has one), the summary since `since` (the tag's
/// name) a doc named `changes-since` keeps out, then the glossary's. The site and the print give
/// the same warnings.
fn scope_warnings(
    m: &rdy_engine::Materialized,
    site: &Site,
    reviewed: bool,
    since: Option<&str>,
) -> Vec<String> {
    use rdy_engine::site_scope::{CHANGES_PAGE, SINCE_PAGE, summary_page_taken};
    let mut warnings = vec![];
    if reviewed && summary_page_taken(m, CHANGES_PAGE) {
        warnings.push(CHANGES_DOC_KEPT.into());
    }
    if let Some(tag) = since
        && summary_page_taken(m, SINCE_PAGE)
    {
        warnings.push(since_doc_kept(tag));
    }
    warnings.extend(glossary_warning(site).map(String::from));
    warnings
}

/// Builds the site of `scope` in memory. The same state, scope and mode give the same bytes.
/// Without a profile it is the full source (RULING F2).
pub fn build_site(rdy: &ReadState, scope: &Scope, mode: Mode) -> Result<SiteFiles, RdyError> {
    build_site_from(
        prepare::source(rdy, scope, prepare::Purpose::Site)?,
        mode,
        Slots::Source,
        None,
    )
    .map(|b| b.site)
}

/// Builds the static site of the publication profile `profile` (an id or key) at `scope_text`
/// (`canon`, `tag:<name>` or `landing:<hash>`) in memory (D-0VCQ5): every page, link, permalink,
/// trail, glossary, question, search entry, figure and agent file reads the profile's selected
/// state alone. A public profile whose reference check is blocked refuses with
/// `E_PUBLICATION_REFERENCE`; an org profile's warning comes first in the summary's warnings.
pub fn build_profile_site(
    read: &ReadState,
    scope_text: &str,
    profile: &str,
) -> Result<SiteFiles, RdyError> {
    let prepared = publication::prepare(read, scope_text, profile, prepare::Purpose::Site)?;
    build_site_from(prepared, Mode::Static, Slots::Source, None).map(|b| b.site)
}

/// A publication artifact (D-KDFZ3, hub r-50C97): every served file by its site-relative path,
/// with its bytes, and the manifest that lists them. The manifest is no file of the artifact: it
/// is kept beside the files and never hashes itself. `warnings` are the build's: an org
/// profile's reference warning first, then the scope's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationArtifact {
    pub files: BTreeMap<String, Vec<u8>>,
    pub manifest: Manifest,
    pub manifest_hash: Hash,
    pub warnings: Vec<String>,
}

/// Builds the publication of `selector` (a profile id or key, or `implicit:org`) in the tagged
/// snapshot `m` in memory (D-KDFZ3, D-0VCQ5, D-QYCRB): the static site of the profile's
/// selection, read through `source` and the captured `inputs` alone, so equal inputs give equal
/// bytes whatever the store's path, canon or tension state. `inputs` must be this renderer's and
/// `source` must read with their effective pack. The site shows `tag_name` (and its `redacted`
/// mark when `tag_redacted`) as its only history. Every HTML page holds one empty notice slot
/// right after its opening main element, at the offset its manifest entry records; no other file
/// has one. A public profile whose reference check is blocked refuses with
/// `E_PUBLICATION_REFERENCE`. It writes no output directory.
pub fn build_publication(
    source: &dyn PublicationSource,
    m: &Materialized,
    tag_name: &str,
    tag_redacted: bool,
    selector: &str,
    inputs: &RenderInputs,
) -> Result<PublicationArtifact, RdyError> {
    probe::hit();
    publication::check_inputs(source, inputs)?;
    let resolved = rdy_engine::publication::resolve_source(source, m, selector)?;
    let prepared = publication::prepare_resolved(source, resolved, tag_name, tag_redacted, inputs)?;
    // the manifest's limits hold while the site is built, not only once it is
    let budget = Budget::publication();
    artifact(build_site_from(
        prepared,
        Mode::Static,
        Slots::Notices,
        Some(&budget),
    )?)
}

/// `E_BAD_INPUT`: a generated file whose media type rdy does not know (the manifest's own
/// refusal).
fn bad_manifest() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "publication manifest is invalid",
        "use unique relative paths and valid file metadata",
    )
}

/// The media type of the generated file at `path`, by its extension. An SVG is generated only
/// as a drawn diagram's file: `assets/diagrams/` and the 64 lowercase hex digits of its hash.
fn generated_media_type(path: &str) -> Result<&'static str, RdyError> {
    let ext = path.rsplit_once('.').map(|(_, e)| e);
    Ok(match ext {
        Some("html") => "text/html",
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("woff2") => "font/woff2",
        Some("svg") if is_diagram_file(path) => "image/svg+xml",
        _ => return Err(bad_manifest()),
    })
}

/// Whether `path` is a drawn diagram's file: `assets/diagrams/<64 lowercase hex>.svg`.
fn is_diagram_file(path: &str) -> bool {
    path.strip_prefix("assets/diagrams/")
        .and_then(|name| name.strip_suffix(".svg"))
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// A media type without its parameters (`image/svg+xml; charset=utf-8` reads `image/svg+xml`).
fn without_parameters(media_type: &str) -> &str {
    media_type
        .split_once(';')
        .map_or(media_type, |(t, _)| t)
        .trim()
}

/// The manifest of `files`: each file in path order with its hash, size and media type, and each
/// HTML page's recorded notice offset. A figure's file keeps the media type its source named
/// (`media`); every other file is generated.
fn manifest_of(
    files: &BTreeMap<String, Vec<u8>>,
    notices: &BTreeMap<String, u64>,
    media: &BTreeMap<String, String>,
) -> Result<Manifest, RdyError> {
    let mut entries = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        let media_type = match media.get(path) {
            Some(m) => without_parameters(m).to_string(),
            None => generated_media_type(path)?.to_string(),
        };
        let notice_offset = notices.get(path).copied();
        let generated_html = !media.contains_key(path) && media_type == "text/html";
        if generated_html != notice_offset.is_some() {
            return Err(bad_notice_slot());
        }
        entries.push(ManifestEntry {
            path: path.clone(),
            hash: Hash::of(bytes),
            media_type,
            size: u64::try_from(bytes.len()).map_err(|_| bad_manifest())?,
            notice_offset,
        });
    }
    Ok(Manifest { files: entries })
}

/// The artifact of a publication build: its manifest, then the recipe-1 manifest hash.
fn artifact(built: BuiltSite) -> Result<PublicationArtifact, RdyError> {
    let BuiltSite {
        site,
        notices,
        media,
    } = built;
    let manifest = manifest_of(&site.files, &notices, &media)?;
    let manifest_hash = manifest_hash(&manifest, MANIFEST_RECIPE)?;
    Ok(PublicationArtifact {
        files: site.files,
        manifest,
        manifest_hash,
        warnings: site.summary.warnings,
    })
}

/// Renders [`build_profile_site`] at `out`, as [`render_site`] places a site. The whole site is
/// built and checked first: a refusal leaves `out` as it was and stages nothing. A placed site
/// replaces `out` as a whole, so no file of an earlier, wider selection stays.
pub fn render_profile_site(
    read: &ReadState,
    scope_text: &str,
    profile: &str,
    out: &Path,
) -> Result<RenderSummary, RdyError> {
    let files = build_profile_site(read, scope_text, profile)?;
    place_site(out, || Ok(files))
}

/// Whether a build's HTML pages carry the notice slot: a publication's do (hub D-YJ9TA), every
/// other site's keep their bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slots {
    Source,
    Notices,
}

/// What [`build_site_from`] built: the site, and, for a publication build, the byte offset of
/// each HTML page's notice slot by path, recorded as the page was written. `media` holds each
/// figure file's media type as the pages that use it named it, by path.
struct BuiltSite {
    site: SiteFiles,
    notices: BTreeMap<String, u64>,
    media: BTreeMap<String, String>,
}

/// The manifest's limits (`publication_manifest`), as a publication build keeps them while it
/// builds: the number of files, the bytes of one file and the bytes of all files. Each page
/// write, permalink page and file is admitted before its bytes are kept, so the build refuses
/// with `E_LIMIT` before it holds more than the limits allow. It also counts the page contexts
/// the build made, and how many waited to be rendered at once.
struct Budget {
    max_files: usize,
    max_file: u64,
    max_total: u64,
    /// The bytes admitted to the artifact's files so far.
    bytes: AtomicU64,
    /// A limit was reached: the build makes no further page context.
    stopped: AtomicBool,
    /// The page contexts made.
    built: AtomicUsize,
    /// The page contexts made and not yet rendered, and the most there were at once.
    pending: AtomicUsize,
    peak: AtomicUsize,
    /// The most bytes the search index's entries held at once.
    index_held: AtomicUsize,
    /// The bytes each agent or search file's buffer held, by path: what it held when it was
    /// refused, else what it held when it was kept.
    aux: std::sync::Mutex<BTreeMap<String, usize>>,
}

impl Budget {
    fn new(max_files: usize, max_file: u64, max_total: u64) -> Budget {
        Budget {
            max_files,
            max_file,
            max_total,
            bytes: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            built: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            index_held: AtomicUsize::new(0),
            aux: Default::default(),
        }
    }

    /// The limits of a publication manifest.
    fn publication() -> Budget {
        Budget::new(MAX_FILES, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES)
    }

    /// `E_LIMIT`: the artifact would exceed `limit`, in the manifest's words. The build stops.
    fn over(&self, limit: String) -> RdyError {
        self.stopped.store(true, Ordering::SeqCst);
        RdyError::new(
            E_LIMIT,
            format!("publication manifest exceeds {limit}"),
            "reduce the artifact to the stated limit",
        )
    }

    /// Admits `n` more bytes to a file that holds `len` bytes so far.
    fn admit(&self, len: u64, n: u64) -> Result<(), RdyError> {
        if len.checked_add(n).is_none_or(|end| end > self.max_file) {
            return Err(self.over(format!("{} file bytes", self.max_file)));
        }
        self.bytes
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |t| {
                t.checked_add(n).filter(|t| *t <= self.max_total)
            })
            .map(drop)
            .map_err(|_| self.over(format!("{} artifact bytes", self.max_total)))
    }

    /// Gives back the bytes of a file the build replaced.
    fn release(&self, n: usize) {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        let _ = self
            .bytes
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |t| {
                Some(t.saturating_sub(n))
            });
    }

    /// Checks that blobs of `lens` bytes fit, before any of their bytes is read (a live profile's
    /// old and new figures together, D-CWC91): each within the per-file limit, all of them within
    /// the aggregate one. Nothing is admitted: each file is admitted again as it is put.
    fn holds_blobs(&self, lens: impl IntoIterator<Item = u64>) -> Result<(), RdyError> {
        let mut total = 0u64;
        for len in lens {
            if len > self.max_file {
                return Err(self.over(format!("{} file bytes", self.max_file)));
            }
            total = match total.checked_add(len).filter(|t| *t <= self.max_total) {
                Some(t) => t,
                None => return Err(self.over(format!("{} artifact bytes", self.max_total))),
            };
        }
        Ok(())
    }

    /// Checks that the artifact may hold `files` files.
    fn count(&self, files: usize) -> Result<(), RdyError> {
        if files > self.max_files {
            return Err(self.over(format!("{} files", self.max_files)));
        }
        Ok(())
    }

    /// Admits a file of `len` bytes at `path` to `files`: one more file, unless it replaces the
    /// one there.
    fn reserve(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        path: &str,
        len: usize,
    ) -> Result<(), RdyError> {
        if !files.contains_key(path) {
            self.count(files.len().saturating_add(1))?;
        }
        self.admit(0, u64::try_from(len).unwrap_or(u64::MAX))
    }

    /// The most bytes one file may take, and the limit's words: the per-file limit, or the
    /// aggregate one when it is lower.
    fn file_cap(&self) -> (u64, String) {
        self.capped(self.max_total)
    }

    /// The room for a file at `path` in `files`: one more file must fit (unless it replaces the
    /// one there), and its bytes may take no more than the per-file limit nor what the aggregate
    /// limit leaves. The room's bytes, and the words of the limit that sets them.
    fn room(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        path: &str,
    ) -> Result<(u64, String), RdyError> {
        if !files.contains_key(path) {
            self.count(files.len().saturating_add(1))?;
        }
        let left = self
            .max_total
            .saturating_sub(self.bytes.load(Ordering::SeqCst));
        Ok(self.capped(left))
    }

    /// The per-file limit, or `total` with the aggregate limit's words when it is lower.
    fn capped(&self, total: u64) -> (u64, String) {
        if self.max_file <= total {
            (self.max_file, format!("{} file bytes", self.max_file))
        } else {
            (total, format!("{} artifact bytes", self.max_total))
        }
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// A page context was made.
    fn made(&self) {
        self.built.fetch_add(1, Ordering::SeqCst);
        let now = self.pending.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
    }

    /// A page context was rendered.
    fn rendered(&self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }

    /// The search index's entries hold `n` bytes.
    fn index_holds(&self, n: usize) {
        self.index_held.fetch_max(n, Ordering::SeqCst);
    }

    /// The buffer of the file at `path` held `n` bytes.
    fn aux_holds(&self, path: &str, n: usize) {
        if let Ok(mut aux) = self.aux.lock() {
            let held = aux.entry(path.to_string()).or_default();
            *held = (*held).max(n);
        }
    }

    #[cfg(test)]
    fn index_held(&self) -> usize {
        self.index_held.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn aux(&self, path: &str) -> usize {
        self.aux
            .lock()
            .map(|a| a.get(path).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn admitted(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn built(&self) -> usize {
        self.built.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

/// Puts `bytes` at `path` in `files`, admitted by `budget` first when there is one; a file it
/// replaces gives its bytes back.
fn put_file(
    files: &mut BTreeMap<String, Vec<u8>>,
    budget: Option<&Budget>,
    path: String,
    bytes: Vec<u8>,
) -> Result<(), RdyError> {
    if let Some(b) = budget {
        b.reserve(files, &path, bytes.len())?;
    }
    if let (Some(old), Some(b)) = (files.insert(path, bytes), budget) {
        b.release(old.len());
    }
    Ok(())
}

/// An agent file at `path`, made by `make` within the room `budget` leaves for it: a file with
/// no room is refused before it is made, and one that outgrows its room is refused before its
/// buffer holds more.
fn aux_file<T>(
    budget: Option<&Budget>,
    files: &BTreeMap<String, Vec<u8>>,
    path: &str,
    make: impl FnOnce(usize) -> Result<(Vec<u8>, T), llms::Over>,
) -> Result<(Vec<u8>, T), RdyError> {
    let Some(b) = budget else {
        return make(usize::MAX).map_err(|_| {
            RdyError::new(
                E_IO,
                format!("template error: {path} is too large to hold"),
                "this is a bug in rdy-render",
            )
        });
    };
    let (room, limit) = b.room(files, path)?;
    match make(usize::try_from(room).unwrap_or(usize::MAX)) {
        Ok((bytes, t)) => {
            b.aux_holds(path, bytes.capacity());
            Ok((bytes, t))
        }
        Err(o) => {
            b.aux_holds(path, o.held);
            Err(b.over(limit))
        }
    }
}

/// What a page context keeps once its page is rendered: what `llms.txt` and `llms-full.txt`
/// read (its kind, title, path, body and items). The parts every page repeats (the navigation,
/// the crumbs, the pager, the header and the site's name and description) are dropped, so the
/// contexts the build holds grow with the source, not with the pages times the navigation.
fn keep_for_llms(ctx: &mut page::PageContext) {
    ctx.site_name = String::new();
    ctx.site_description = None;
    ctx.root = String::new();
    ctx.header = String::new();
    ctx.crumbs = vec![];
    ctx.nav = Default::default();
    ctx.toc = Default::default();
    ctx.pager = Default::default();
    ctx.search_href = String::new();
    ctx.review_bar = None;
}

/// A page made by `build` for each of `kinds` and rendered by `render`, in order: the contexts
/// (as [`keep_for_llms`] keeps them) and the pages. `build` makes the contexts one by one on
/// this thread while a few threads render them; it waits while a made context waits for a
/// thread, so few contexts with their navigation are held at once. After a
/// failure (a limit of `budget` included) no further context is made, and the error of the
/// context that could not be made, else of the first failed page, is returned.
fn build_and_render<'k>(
    kinds: Vec<page::Kind<'k>>,
    mut build: impl FnMut(page::Kind<'k>) -> Result<page::PageContext, RdyError>,
    render: impl Fn(&page::PageContext) -> Result<Page, RdyError> + Sync,
    budget: Option<&Budget>,
) -> Result<(Vec<page::PageContext>, Vec<Page>), RdyError> {
    use std::sync::mpsc;
    type Done = (usize, page::PageContext, Result<Page, RdyError>);
    let n = kinds.len();
    let threads = render_threads();
    let failed = AtomicBool::new(false);
    // one context waits in the queue: with one being rendered on each thread and one being
    // made, at most `threads + 2` are held unrendered
    let (jobs, queue) = mpsc::sync_channel::<(usize, page::PageContext)>(1);
    // the workers own the queue: should they all stop, a send fails and nothing waits
    let queue = std::sync::Arc::new(std::sync::Mutex::new(queue));
    let (done, results) = mpsc::channel::<Done>();
    let mut unmade: Option<RdyError> = None;
    std::thread::scope(|s| {
        for _ in 0..threads {
            let (queue, done, render, failed) = (queue.clone(), done.clone(), &render, &failed);
            s.spawn(move || {
                loop {
                    let job = queue.lock().map(|q| q.recv());
                    let Ok(Ok((i, mut ctx))) = job else { break };
                    let page = render(&ctx);
                    if page.is_err() {
                        failed.store(true, Ordering::SeqCst);
                    }
                    if let Some(b) = budget {
                        b.rendered();
                    }
                    keep_for_llms(&mut ctx);
                    if done.send((i, ctx, page)).is_err() {
                        break;
                    }
                }
            });
        }
        drop((done, queue));
        for (i, kind) in kinds.into_iter().enumerate() {
            if failed.load(Ordering::SeqCst) || budget.is_some_and(Budget::stopped) {
                break;
            }
            let ctx = match build(kind) {
                Ok(ctx) => ctx,
                Err(e) => {
                    unmade = Some(e);
                    break;
                }
            };
            if let Some(b) = budget {
                b.made();
            }
            if jobs.send((i, ctx)).is_err() {
                break;
            }
        }
        drop(jobs);
    });
    let mut out: Vec<Option<(page::PageContext, Result<Page, RdyError>)>> =
        (0..n).map(|_| None).collect();
    for (i, ctx, page) in results {
        out[i] = Some((ctx, page));
    }
    if let Some(e) = unmade {
        return Err(e);
    }
    if let Some(e) = out
        .iter()
        .find_map(|o| o.as_ref().and_then(|(_, p)| p.as_ref().err()))
    {
        return Err(e.clone());
    }
    let mut contexts = Vec::with_capacity(n);
    let mut pages = Vec::with_capacity(n);
    for o in out {
        // every context was made and rendered: no page failed, so none was skipped
        let (ctx, page) = o.ok_or_else(|| {
            RdyError::new(
                E_IO,
                "template error: a page was not rendered",
                "this is a bug in rdy-render",
            )
        })?;
        contexts.push(ctx);
        pages.push(page?);
    }
    Ok((contexts, pages))
}

/// How many threads render pages.
fn render_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8)
}

/// What the shared setup keeps of a prepared source's history for the build that follows: the
/// change summaries (pages of the site) and the build's warnings.
struct Extras {
    changes: Option<changes::Summary>,
    since: Option<changes::Summary>,
    warnings: Vec<String>,
}

// The page contexts built on this thread (see `page_builds`); test builds only.
#[cfg(test)]
thread_local! {
    static PAGE_BUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many page contexts [`build_page`] made on this thread so far. Tests count the bodies a
/// render builds with it.
#[cfg(test)]
fn page_builds() -> u64 {
    PAGE_BUILDS.get()
}

/// The page context for `kind`: every build calls `page::build` through here, so a test can
/// count the bodies a render makes.
fn build_page(sh: &page::Shared, kind: page::Kind) -> page::PageContext {
    #[cfg(test)]
    PAGE_BUILDS.set(PAGE_BUILDS.get().wrapping_add(1));
    page::build(sh, kind)
}

/// The setup every build shares, from what [`prepare::source`] or `publication::prepare_resolved`
/// read: the site of the selected state, the template environment, the header, the authored
/// local targets and the [`page::Shared`] the pages are built from. `f` builds with them; the
/// figure blobs the pages did not borrow for good come back with its result, for the build to
/// copy once the shared state is dropped. A static build refuses two siblings that would be one
/// file before `f` runs.
fn with_shared<R>(
    prepared: prepare::Prepared,
    mode: Mode,
    f: impl FnOnce(&page::Shared<'_>, &minijinja::Environment<'static>, Extras) -> Result<R, RdyError>,
) -> Result<(R, prepare::FigureBlobs), RdyError> {
    let prepare::Prepared {
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
        history,
        profile,
        menu_url,
        ..
    } = prepared;
    let m: &Materialized = &m;
    // a named profile reads every reference outside its selection in neutral words
    let unavailable = match profile {
        Some(_) => inline::UNAVAILABLE,
        None => html::UNRESOLVED,
    };
    let mut site = Site::with_unavailable(
        m,
        &pack,
        &site_name,
        profile.as_ref().map(|_| inline::UNAVAILABLE),
    );
    site.set_disagreements(disagreements);
    // a static site is written as files: two siblings that would be one file refuse before
    // anything is built, so the last rendered site stays as it was (a live site's pages are not
    // files)
    if let (Mode::Static, Some(e)) = (&mode, site.path_clash("render")) {
        return Err(e);
    }
    let env = env()?;
    let prepare::History {
        listed,
        review_marks,
        reviewed,
        inline_diffs,
        changes: changes_summary,
        since: since_summary,
        warnings,
        old,
    } = history;
    // a named profile's review base: the words only it holds read through its selected state
    let old_site = old
        .as_deref()
        .map(|o| Site::with_unavailable(o, &pack, &site_name, Some(inline::UNAVAILABLE)));
    // the review marks on the pages read only a base a scope names (the task-8 ruling)
    let review = listed.as_ref().filter(|_| review_marks);
    let header = match &profile {
        Some(ProfilePresentation {
            presentation: Presentation::LiveRoute { header, .. },
            ..
        }) => header.clone(),
        Some(p) => version::chosen_header(p.tag_name.as_deref(), position.frozen),
        None => version::header(&position, reviewed.as_ref()),
    };
    // the pages and anchors a named profile's authored local links may lead to, known before
    // any page body is written
    let local = profile
        .as_ref()
        .map(|_| publication::LocalTargets::of(&site));
    // each doc's group on the finished change summary, for the navigation's Edited links
    let review_anchors: BTreeMap<String, String> = changes_summary
        .iter()
        .flat_map(|c| &c.groups)
        .filter_map(|g| Some((g.href.clone()?, g.anchor.clone())))
        .collect();
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
        review,
        listed: listed.as_ref(),
        reviewed: reviewed.clone(),
        header,
        headers: Default::default(),
        used_blobs: Default::default(),
        draw_diagrams: true,
        diagrams: Default::default(),
        governing: Default::default(),
        mode,
        inline_diffs,
        changes_page: changes_summary.is_some(),
        review_anchors,
        since_page: since_summary.is_some(),
        profile: profile.as_ref(),
        local: local.as_ref(),
        unavailable,
        menu_url: menu_url.as_deref(),
        old_site: old_site.as_ref(),
    };
    let extras = Extras {
        changes: changes_summary,
        since: since_summary,
        warnings,
    };
    let out = f(&sh, &env, extras)?;
    drop(sh);
    Ok((out, blobs))
}

/// The pages of a build, in the order a reader meets them: the home page, the tree, the
/// collected pages. The page at a path is the last of them that has it.
fn page_kinds<'a>(site: &'a Site<'_>, m: &Materialized) -> Vec<page::Kind<'a>> {
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
    kinds
}

/// The site-relative path of the page of a kind [`page_kinds`] lists.
fn kind_page(site: &Site<'_>, kind: &page::Kind) -> Option<String> {
    match kind {
        page::Kind::Home => Some("index.html".into()),
        page::Kind::Folder(id) | page::Kind::Doc(id) => site.page(id),
        page::Kind::Glossary => Some("glossary.html".into()),
        page::Kind::Questions => Some("questions.html".into()),
        page::Kind::Disagreements => Some("disagreements.html".into()),
        _ => None,
    }
}

/// A context rendered as a page of `slots`' kind, each write admitted by `budget` when there is
/// one: its path, bytes, label and header ranges, and notice slot offset.
fn render_ctx(
    env: &minijinja::Environment<'static>,
    slots: Slots,
    budget: Option<&Budget>,
    ctx: &page::PageContext,
) -> Result<Page, RdyError> {
    let r = match slots {
        Slots::Source => render_page_within(env, template_of(ctx.kind), ctx, budget)?,
        Slots::Notices => {
            render_page_within(env, template_of(ctx.kind), with_notice_slot(ctx), budget)?
        }
    };
    let notice = notice_offset(&r.bytes, r.notice.as_ref())?;
    Ok((ctx.page_url.clone(), r.bytes, r.slots, notice))
}

/// What the pages of a build leave for its last step: every file but the figures' copies, and
/// the figure files the pages used, by path.
struct Staged {
    files: BTreeMap<String, Vec<u8>>,
    label_at: BTreeMap<String, Vec<Slot>>,
    notices: BTreeMap<String, u64>,
    summary: RenderSummary,
    label: String,
    header: String,
    used: BTreeMap<String, (Hash, String)>,
}

/// [`build_site`] from what [`prepare::source`] read: it reads no source. A publication build
/// passes the manifest's limits as `budget`, kept while it builds.
fn build_site_from(
    prepared: prepare::Prepared,
    mode: Mode,
    slots: Slots,
    budget: Option<&Budget>,
) -> Result<BuiltSite, RdyError> {
    let (staged, mut blobs) = with_shared(prepared, mode, |sh, env, extras| {
        stage_site(sh, env, extras, slots, budget)
    })?;
    let Staged {
        mut files,
        label_at,
        notices,
        mut summary,
        label,
        header,
        used,
    } = staged;
    let media = used_media(&used);
    summary.assets = copy_blobs(&mut files, budget, &used, &mut blobs)?;
    Ok(BuiltSite {
        site: SiteFiles {
            files,
            label,
            header,
            label_at,
            summary,
        },
        notices,
        media,
    })
}

/// The media type each used figure file's first block named, by path.
fn used_media(used: &BTreeMap<String, (Hash, String)>) -> BTreeMap<String, String> {
    used.iter()
        .map(|(path, (_, media_type))| (path.clone(), media_type.clone()))
        .collect()
}

/// Moves each figure file of `used` into `files` at its path, from `blobs`, and returns how many
/// it put there. Each blob moves at its last path, so the build never holds two copies of it
/// (one hash under two paths is copied once). A copy is admitted by `budget` before it is made.
fn copy_blobs(
    files: &mut BTreeMap<String, Vec<u8>>,
    budget: Option<&Budget>,
    used: &BTreeMap<String, (Hash, String)>,
    blobs: &mut prepare::FigureBlobs,
) -> Result<usize, RdyError> {
    let mut uses: BTreeMap<&Hash, usize> = BTreeMap::new();
    for (h, _) in used.values() {
        *uses.entry(h).or_default() += 1;
    }
    let mut copied = 0;
    for (path, (h, _)) in used {
        let left = uses.get_mut(h).expect("counted above");
        *left -= 1;
        if let Some(b) = budget {
            let len = blobs.bytes.get(h).map_or(0, Vec::len);
            b.reserve(files, path, len)?;
        }
        let bytes = if *left == 0 {
            blobs.bytes.remove(h)
        } else {
            blobs.bytes.get(h).cloned()
        };
        let bytes = bytes.expect("a used blob is held");
        if let (Some(old), Some(b)) = (files.insert(path.clone(), bytes), budget) {
            b.release(old.len());
        }
        copied += 1;
    }
    Ok(copied)
}

/// The pages, permalink pages, agent and search files and theme files of a build.
fn stage_site(
    sh: &page::Shared<'_>,
    env: &minijinja::Environment<'static>,
    extras: Extras,
    slots: Slots,
    budget: Option<&Budget>,
) -> Result<Staged, RdyError> {
    let (site, m) = (sh.site, sh.m);
    let (site_name, description) = (sh.site_name, sh.description);
    let Extras {
        changes: changes_summary,
        since: since_summary,
        warnings,
    } = extras;
    let mode = sh.mode;
    let mut summary = RenderSummary {
        pages: 0,
        assets: 0,
        redirects: 0,
        warnings,
    };
    let kinds = page_kinds(site, m);
    // the search index is held to the bytes one file may take while its entries are collected
    let (index_cap, index_limit) = budget.map_or((u64::MAX, String::new()), Budget::file_cap);
    let mut index = search::Index::new(site_name, index_cap);
    // a publication has at most as many files as its limit: count its pages (the change
    // summaries included) before making any
    if let Some(b) = budget {
        let summaries =
            usize::from(changes_summary.is_some()) + usize::from(since_summary.is_some());
        b.count(kinds.len().saturating_add(summaries))?;
    }
    // a page with its notice slot's offset, recorded as it was written (a publication's pages),
    // each write admitted by the budget
    let page_of = |ctx: &page::PageContext| render_ctx(env, slots, budget, ctx);
    // the pages are made one by one and rendered on a few threads
    let (contexts, pages) = build_and_render(
        kinds,
        |kind| {
            let ctx = build_page(sh, kind);
            page::finish(sh, &ctx, &mut index);
            if let Some(b) = budget {
                b.index_holds(index.held());
                if index.over() {
                    return Err(b.over(index_limit.clone()));
                }
            }
            Ok(ctx)
        },
        page_of,
        budget,
    )?;
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut label_at: BTreeMap<String, Vec<Slot>> = BTreeMap::new();
    let mut notices: BTreeMap<String, u64> = BTreeMap::new();
    // the change summaries: pages of the site, kept out of the navigation, the search index,
    // the llms files and the permalinks (sr6)
    let mut extra = vec![];
    if let Some(c) = &changes_summary {
        extra.push(build_page(sh, page::Kind::Changes(c)));
    }
    if let Some(c) = &since_summary {
        extra.push(build_page(sh, page::Kind::ChangesSince(c)));
    }
    let mut pages = pages;
    for ctx in &extra {
        if let Some(b) = budget {
            b.made();
        }
        pages.push(page_of(ctx)?);
        if let Some(b) = budget {
            b.rendered();
        }
    }
    summary.pages = pages.len();
    for (path, html, at, notice) in pages {
        if !at.is_empty() {
            label_at.insert(path.clone(), at);
        }
        if let Some(n) = notice {
            notices.insert(path.clone(), n);
        }
        // the page's bytes were admitted as it was written: it counts as a file here
        if let Some(b) = budget
            && !files.contains_key(&path)
        {
            b.count(files.len() + 1)?;
        }
        if let (Some(old), Some(b)) = (files.insert(path, html), budget) {
            b.release(old.len());
        }
    }

    // a permalink page for every folder, doc and block that has a place on the site: the
    // template is rendered once, and every page fills in its target's href and label; a doc page
    // at a permalink's path (a doc named like an id in a folder named `go`) is replaced by the
    // permalink, as it always was
    let mut redirect: Option<RedirectTemplate> = None;
    for id in m.ids() {
        // a publication profile is off the site: no permalink leads to it
        if m.get(id).is_some_and(site::is_profile) {
            continue;
        }
        let Some(url) = site.url_of(id) else { continue };
        let t = match &mut redirect {
            Some(t) => t,
            None => {
                let ctx = build_page(sh, page::Kind::Redirect(id, &url));
                redirect.insert(RedirectTemplate::new(env, ctx, slots)?)
            }
        };
        let (page_url, label, href) = page::redirect_target(sh, id, &url);
        // the filled page's length is known before it is made
        if let Some(b) = budget {
            b.reserve(&files, &page_url, t.filled_len(&label, &href))?;
        }
        let (html, notice) = t.fill_at(&label, &href);
        #[cfg(debug_assertions)]
        if summary.redirects.is_multiple_of(997) {
            let ctx = build_page(sh, page::Kind::Redirect(id, &url));
            let full = RedirectTemplate::render(env, &ctx, slots)?;
            debug_assert_eq!(
                (html.as_bytes(), notice),
                (full.bytes.as_slice(), full.notice.map(|r| r.start)),
                "the prebuilt permalink page differs from the template"
            );
        }
        label_at.remove(&page_url);
        notices.remove(&page_url);
        let html = html.into_bytes();
        let notice = notice.map(|n| n..n + NOTICE_SLOT.len());
        if let Some(n) = notice_offset(&html, notice.as_ref())? {
            notices.insert(page_url.clone(), n);
        }
        if let (Some(old), Some(b)) = (files.insert(page_url, html), budget) {
            b.release(old.len());
        }
        summary.redirects += 1;
    }

    let label = sh.label();
    let header = match mode {
        Mode::Live { .. } => sh.header_html(""),
        Mode::Static => String::new(),
    };
    // the agent and search files: each is admitted before its buffer is made, and held to its
    // room while it is written
    let (bytes, ()) = aux_file(budget, &files, "search-index.json", |room| {
        match index.len() <= u64::try_from(room).unwrap_or(u64::MAX) {
            true => Ok((search::bytes(&index), ())),
            false => Err(llms::Over { held: 0 }),
        }
    })?;
    drop(index);
    put_file(&mut files, budget, "search-index.json".into(), bytes)?;
    // a label slot takes bytes: a named profile with no chosen tag writes no label
    let (llms, at) = aux_file(budget, &files, "llms.txt", |room| {
        llms::index_bytes(site, &contexts, site_name, description, &label, room)
    })?;
    put_file(&mut files, budget, "llms.txt".into(), llms)?;
    if !at.is_empty() {
        label_at.insert("llms.txt".into(), vec![(SlotKind::Label, at)]);
    }
    let (llms, at) = aux_file(budget, &files, "llms-full.txt", |room| {
        llms::full_bytes(&contexts, site_name, description, &label, room)
    })?;
    put_file(&mut files, budget, "llms-full.txt".into(), llms)?;
    if !at.is_empty() {
        label_at.insert("llms-full.txt".into(), vec![(SlotKind::Label, at)]);
    }
    for (path, bytes) in assets::files() {
        put_file(&mut files, budget, path.into(), bytes)?;
    }
    // the diagrams the pages drew, one file each, and the warnings of those they could not
    // draw, after the build's own (in page and block order)
    let diagrams = sh.diagrams.take();
    for (path, bytes) in diagrams.files() {
        put_file(&mut files, budget, path, bytes)?;
    }
    summary.warnings.extend(diagrams.warnings);
    let used = sh.used_blobs.take();
    Ok(Staged {
        files,
        label_at,
        notices,
        summary,
        label: label.text,
        header,
        used,
    })
}

/// One rendered page: its path, its bytes, its label and header ranges, and its notice slot's
/// offset.
type Page = (String, Vec<u8>, Vec<Slot>, Option<u64>);

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
/// template reads nothing else that differs from one permalink page to the next. A publication's
/// prototype records where its notice slot went; each filled page moves it by the bytes the
/// substitutions before it add.
struct RedirectTemplate {
    /// The text between the markers, `slots.len() + 1` pieces.
    pieces: Vec<String>,
    /// Which value each marker stands for: true for the href, false for the label.
    slots: Vec<bool>,
    /// Where the notice slot starts: the piece that holds it, and its byte offset in that piece.
    notice: Option<(usize, usize)>,
}

impl RedirectTemplate {
    const HREF: char = '\u{E000}';
    const LABEL: char = '\u{E001}';

    /// The permalink page of `ctx` rendered whole, with the notice slot when `slots` asks.
    fn render(
        env: &minijinja::Environment<'static>,
        ctx: &page::PageContext,
        slots: Slots,
    ) -> Result<Rendered, RdyError> {
        match slots {
            Slots::Source => render_page_out(env, "redirect.html", ctx),
            Slots::Notices => render_page_out(env, "redirect.html", with_notice_slot(ctx)),
        }
    }

    fn new(
        env: &minijinja::Environment<'static>,
        mut ctx: page::PageContext,
        slots: Slots,
    ) -> Result<RedirectTemplate, RdyError> {
        ctx.body.redirect = Some(page::RedirectBody {
            label: Self::LABEL.to_string(),
            href: Self::HREF.to_string(),
        });
        let r = Self::render(env, &ctx, slots)?;
        let html = String::from_utf8(r.bytes).map_err(|_| {
            RdyError::new(
                E_IO,
                "template error: the permalink page is not UTF-8",
                "this is a bug in rdy-render templates",
            )
        })?;
        Self::split(&html, r.notice.map(|n| n.start))
    }

    /// The prototype `html` cut at its markers, with its notice slot at byte `at` kept as a
    /// piece and an offset in it. A slot that starts at no character is refused.
    fn split(html: &str, at: Option<usize>) -> Result<RedirectTemplate, RdyError> {
        let (mut pieces, mut slots, mut cur) = (vec![], vec![], String::new());
        let mut notice = None;
        for (i, c) in html.char_indices() {
            if Some(i) == at {
                notice = Some((pieces.len(), cur.len()));
            }
            if c == Self::HREF || c == Self::LABEL {
                pieces.push(std::mem::take(&mut cur));
                slots.push(c == Self::HREF);
            } else {
                cur.push(c);
            }
        }
        pieces.push(cur);
        if at.is_some() && notice.is_none() {
            return Err(bad_notice_slot());
        }
        Ok(RedirectTemplate {
            pieces,
            slots,
            notice,
        })
    }

    /// The page for a target, its values escaped as the template escapes them.
    #[cfg(test)]
    fn fill(&self, label: &str, href: &str) -> String {
        self.fill_at(label, href).0
    }

    /// The bytes of the page [`RedirectTemplate::fill_at`] gives for a target, known before it
    /// is made.
    fn filled_len(&self, label: &str, href: &str) -> usize {
        let (label, href) = (escape(label).len(), escape(href).len());
        self.slots
            .iter()
            .map(|h| if *h { href } else { label })
            .chain(self.pieces.iter().map(String::len))
            .fold(0usize, usize::saturating_add)
    }

    /// The page for a target, and where its notice slot starts: the prototype's offset moved by
    /// the UTF-8 bytes of each value written before it, in place of its marker.
    fn fill_at(&self, label: &str, href: &str) -> (String, Option<usize>) {
        let (label, href) = (escape(label), escape(href));
        let mut out = String::with_capacity(
            self.pieces.iter().map(String::len).sum::<usize>() + 2 * href.len() + label.len(),
        );
        let mut notice = None;
        for (i, piece) in self.pieces.iter().enumerate() {
            if let Some((p, at)) = self.notice
                && p == i
            {
                notice = Some(out.len() + at);
            }
            out.push_str(piece);
            if let Some(slot) = self.slots.get(i) {
                out.push_str(if *slot { &href } else { &label });
            }
        }
        (out, notice)
    }
}

/// `f` of every item, in order, worked out on a few threads.
fn par_map<T: Sync, R: Send>(
    items: &[T],
    f: impl Fn(&T) -> Result<R, RdyError> + Sync,
) -> Result<Vec<R>, RdyError> {
    let threads = render_threads();
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
            notice: None,
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
    fn hpb6_a_page_records_its_notice_slot_in_bytes() {
        let env = env_with("<p>ä☃{{ a | label }}</p><main>{{ notice_slot }}{{ b }}</main>");
        let ctx = serde_json::json!({"a": "é", "b": "<!--rdy-notices-->"});
        let r = render_page_out(&env, "t.html", with_notice_slot(&ctx)).unwrap();
        let html = String::from_utf8(r.bytes.clone()).unwrap();
        let at = "<p>ä☃é</p><main>".len();
        assert_eq!(r.notice, Some(at..at + NOTICE_SLOT.len()));
        assert_eq!(
            notice_offset(&r.bytes, r.notice.as_ref()).unwrap(),
            Some(at as u64)
        );
        // the authored marker text is escaped, after the slot
        assert!(html[at + NOTICE_SLOT.len()..].starts_with("&lt;!--rdy-notices--&gt;"));
        // a source page's context has no slot, and the template writes nothing for it
        let env =
            env_with("<main>{% if notice_slot is defined %}{{ notice_slot }}{% endif %}</main>");
        let r = render_page_out(&env, "t.html", &ctx).unwrap();
        assert_eq!((r.bytes, r.notice), (b"<main></main>".to_vec(), None));
    }

    /// Fix round 1 (finding 4): literal marker bytes in the template, before and after the
    /// formatter's slot, never move the recorded offset. A first-match or a last-match search for
    /// the marker would find a decoy, so this test fails for either.
    #[test]
    fn hpb6_the_recorded_offset_is_the_formatters_slot_among_literal_markers() {
        let marker = std::str::from_utf8(NOTICE_SLOT).unwrap();
        let env = env_with(
            "<!--rdy-notices-->ä<p>{{ a | label }}</p><main>{{ notice_slot }}</main>☃<!--rdy-notices-->",
        );
        let r = render_page_out(
            &env,
            "t.html",
            with_notice_slot(&serde_json::json!({"a": "é"})),
        )
        .unwrap();
        let html = String::from_utf8(r.bytes.clone()).unwrap();
        let at = "<!--rdy-notices-->ä<p>é</p><main>".len();
        // guards: three literal markers, and neither the first nor the last is the slot
        assert_eq!(html.matches(marker).count(), 3, "{html}");
        assert_ne!(html.find(marker), Some(at));
        assert_ne!(html.rfind(marker), Some(at));
        assert_eq!(r.notice, Some(at..at + NOTICE_SLOT.len()));
        assert_eq!(
            notice_offset(&r.bytes, r.notice.as_ref()).unwrap(),
            Some(at as u64)
        );
        // the cached permalink path keeps the formatter's slot among the same decoys
        let mut env = super::env().unwrap();
        env.add_template(
            "t.html",
            "<!--rdy-notices--><a href=\"{{ h }}\">{{ l }}</a><main>{{ notice_slot }}</main>{{ l }}<!--rdy-notices-->",
        )
        .unwrap();
        let render = |h: &str, l: &str| {
            render_page_out(
                &env,
                "t.html",
                with_notice_slot(&serde_json::json!({"h": h, "l": l})),
            )
            .unwrap()
        };
        let proto = render(
            &RedirectTemplate::HREF.to_string(),
            &RedirectTemplate::LABEL.to_string(),
        );
        let html = String::from_utf8(proto.bytes).unwrap();
        let t = RedirectTemplate::split(&html, proto.notice.map(|n| n.start)).unwrap();
        let (page, notice) = t.fill_at("Lâbel ☃", "../dïr/x.html");
        let at = "<!--rdy-notices--><a href=\"../dïr/x.html\">Lâbel ☃</a><main>".len();
        assert_ne!(page.find(marker), Some(at));
        assert_ne!(page.rfind(marker), Some(at));
        assert_eq!(notice, Some(at));
        let notice = notice.map(|n| n..n + NOTICE_SLOT.len());
        assert_eq!(
            notice_offset(page.as_bytes(), notice.as_ref()).unwrap(),
            Some(at as u64)
        );
    }

    #[test]
    fn hpb6_a_second_or_misplaced_notice_slot_is_refused() {
        let ctx = with_notice_slot(&serde_json::json!({}));
        let env = env_with("<main>{{ notice_slot }}{{ notice_slot }}</main>");
        let e = render_page_out(&env, "t.html", &ctx).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_BAD_INPUT,
                "publication page has an invalid notice slot",
                "use the templates shipped with this rdy version"
            )
        );
        let env = env_with("{% set x %}{{ notice_slot }}{% endset %}<main>{{ x }}</main>");
        let e = render_page_out(&env, "t.html", &ctx).unwrap_err();
        assert!(e.message.contains("notice slot"), "{e}");
        let env = env_with("<main>{{ notice_slot }}</main>");
        let e = env
            .get_template("t.html")
            .unwrap()
            .render(&ctx)
            .unwrap_err();
        assert!(e.to_string().contains("notice slot"), "{e}");
        // a recorded range that does not hold the empty slot's bytes
        assert_eq!(
            notice_offset(b"<main><!--rdy-notices--x", Some(&(6..24)))
                .unwrap_err()
                .message,
            "publication page has an invalid notice slot"
        );
        assert!(notice_offset(b"<main>", Some(&(6..24))).is_err());
    }

    #[test]
    fn hpb6_the_cached_permalink_page_moves_its_slot_by_each_substitution() {
        let mut env = env().unwrap();
        env.add_template(
            "t.html",
            "<a href=\"{{ h }}\">{{ l }}</a><main>{{ notice_slot }}</main><b>{{ l }}{{ h }}</b>",
        )
        .unwrap();
        let render = |h: &str, l: &str| {
            render_page_out(
                &env,
                "t.html",
                with_notice_slot(&serde_json::json!({"h": h, "l": l})),
            )
            .unwrap()
        };
        let proto = render(
            &RedirectTemplate::HREF.to_string(),
            &RedirectTemplate::LABEL.to_string(),
        );
        let html = String::from_utf8(proto.bytes).unwrap();
        let t = RedirectTemplate::split(&html, proto.notice.map(|n| n.start)).unwrap();
        assert!(RedirectTemplate::split(&html, Some(html.len() + 1)).is_err());
        for (h, l) in [
            ("", ""),
            ("a.html", "A"),
            ("../ff/日本語.html#x", "Bé & <c> ☃☃"),
            ("../../a/b/c/d.html", "x"),
        ] {
            let full = render(h, l);
            let (page, at) = t.fill_at(l, h);
            assert_eq!(page.as_bytes(), full.bytes.as_slice(), "{h} {l}");
            assert_eq!(at, full.notice.map(|n| n.start), "{h} {l}");
            assert_eq!(
                &page.as_bytes()[at.unwrap()..][..NOTICE_SLOT.len()],
                NOTICE_SLOT
            );
        }
    }

    #[test]
    fn hpb6_generated_media_types_are_closed() {
        for (path, want) in [
            ("index.html", "text/html"),
            ("llms.txt", "text/plain"),
            ("search-index.json", "application/json"),
            ("assets/site.css", "text/css"),
            ("assets/app.js", "application/javascript"),
            ("assets/fonts/onest.woff2", "font/woff2"),
            (
                "assets/diagrams/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.svg",
                "image/svg+xml",
            ),
        ] {
            assert_eq!(generated_media_type(path).unwrap(), want);
        }
        for path in [
            "assets/x.bin",
            "README",
            "a.htm",
            "assets/x.svg",
            "assets/diagrams/0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef.svg",
            "assets/diagrams/a/b.svg",
        ] {
            assert_eq!(
                generated_media_type(path).unwrap_err().message,
                "publication manifest is invalid"
            );
        }
        assert_eq!(
            without_parameters("image/svg+xml; charset=utf-8"),
            "image/svg+xml"
        );
        assert_eq!(without_parameters("image/png"), "image/png");
    }

    #[test]
    fn the_label_outside_a_page_render_is_refused() {
        let env = env_with("{{ a | label }}");
        let t = env.get_template("t.html").unwrap();
        let e = t.render(serde_json::json!({"a": "v1"})).unwrap_err();
        assert!(e.to_string().contains("version label"), "{e}");
    }
}

/// Fix round 1 (finding 1): a publication build keeps the manifest's limits while it builds,
/// never only after every page context and file has been made.
#[cfg(test)]
mod limit_tests {
    use super::*;
    use rdy_engine::E_LIMIT;
    use rdy_engine::publication_manifest::{MAX_ARTIFACT_BYTES, MAX_FILE_BYTES, MAX_FILES};

    /// A generated project of 4 folders of 30 docs with a claim each: every page's navigation
    /// repeats the folders and the current folder's docs, so the pages' bytes grow faster than
    /// the source's.
    fn project() -> (tempfile::TempDir, rdy_engine::Rdy) {
        let dir = tempfile::tempdir().expect("tempdir");
        rdy_engine::testkit::generate_tree(dir.path(), 4, 30, 1);
        let r = rdy_engine::Rdy::open(dir.path()).expect("open");
        (dir, r)
    }

    fn build(r: &rdy_engine::Rdy, budget: Option<&Budget>) -> Result<BuiltSite, RdyError> {
        let read = r.read();
        let prepared = prepare::source(&read, &Scope::View("work".into()), prepare::Purpose::Site)?;
        build_site_from(prepared, Mode::Static, Slots::Notices, budget)
    }

    /// The number of HTML pages of `b` that are not permalinks, and their bytes.
    fn pages(b: &BuiltSite) -> (usize, u64) {
        b.site
            .files
            .iter()
            .filter(|(p, _)| p.ends_with(".html") && !p.starts_with("go/"))
            .fold((0, 0), |(n, s), (_, f)| (n + 1, s + f.len() as u64))
    }

    fn total(b: &BuiltSite) -> u64 {
        b.site.files.values().map(|f| f.len() as u64).sum()
    }

    #[track_caller]
    fn over(e: &RdyError, limit: String) {
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                format!("publication manifest exceeds {limit}").as_str(),
                "reduce the artifact to the stated limit"
            ),
            "{e}"
        );
    }

    #[test]
    fn hpb6_a_publication_build_within_the_limits_is_unchanged() {
        let (_d, r) = project();
        let free = build(&r, None).unwrap();
        let b = Budget::publication();
        let kept = build(&r, Some(&b)).unwrap();
        assert_eq!(kept.site.files, free.site.files);
        assert_eq!(kept.notices, free.notices);
        // every byte of the artifact was counted once, as it was written
        assert_eq!(b.admitted(), total(&free));
        assert_eq!(b.built(), pages(&free).0);
    }

    #[test]
    fn hpb6_a_publication_build_stops_at_its_artifact_limit_while_it_writes() {
        let (_d, r) = project();
        let free = build(&r, None).unwrap();
        let (n, bytes) = pages(&free);
        assert!(n > 120, "guard: {n} pages");
        let limit = bytes / 20;
        let b = Budget::new(MAX_FILES, MAX_FILE_BYTES, limit);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{limit} artifact bytes"));
        // no write went past the limit
        assert!(b.admitted() <= limit, "{} > {limit}", b.admitted());
        // the build stopped making page contexts: the pages that fit and those in flight
        let in_flight = render_threads() + 2;
        assert!(b.peak() <= in_flight, "{} contexts in flight", b.peak());
        assert!(
            b.built() < n / 2,
            "{} of {n} page contexts built",
            b.built()
        );
    }

    #[test]
    fn hpb6_a_publication_build_stops_at_its_file_limits() {
        let (_d, r) = project();
        let free = build(&r, None).unwrap();
        let (n, _) = pages(&free);
        let largest = free.site.files.values().map(Vec::len).max().unwrap() as u64;
        // one file over the per-file limit
        let b = Budget::new(MAX_FILES, largest - 1, MAX_ARTIFACT_BYTES);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{} file bytes", largest - 1));
        assert!(b.admitted() < total(&free));
        // more pages than files: refused before any page context is made
        let b = Budget::new(n - 1, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{} files", n - 1));
        assert_eq!((b.built(), b.admitted()), (0, 0));
        // the pages fit, the permalinks and assets do not
        let files = free.site.files.len();
        let b = Budget::new(files - 1, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{} files", files - 1));
        assert_eq!(b.built(), n);
        assert!(b.admitted() < total(&free));
    }

    /// A project of one doc with a title of `title` bytes and `sections` sections: every search
    /// entry of the doc repeats its title, so the index grows as the title times the sections.
    fn titled(title: usize, sections: usize) -> (tempfile::TempDir, rdy_engine::Rdy) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut r = rdy_engine::Rdy::init(dir.path(), "rdy-standard").expect("init");
        let s = rdy_engine::Session {
            actor: "agent:bench".into(),
            run: None,
            role: rdy_model::Role::Writer,
            kind: rdy_engine::SessionKind::Human,
        };
        let mut ops = vec![
            serde_json::json!({"op": "create", "type": "doc", "id": "$d",
            "fields": {"title": "t".repeat(title), "doc_type": "constraints"}}),
        ];
        ops.extend((0..sections).map(|i| {
            serde_json::json!({"op": "create", "type": "section", "parent": "$d",
                "fields": {"title": format!("S{i}")}})
        }));
        r.write(
            &s,
            rdy_engine::WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(serde_json::json!(ops)).unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .expect("write");
        (dir, r)
    }

    /// Fix round 2: the search index is held to the limits while its entries are collected. An
    /// entry that would take the file over them is never kept, and the file's buffer is never
    /// made.
    #[test]
    fn hpb6_a_search_index_over_the_limits_is_refused_before_its_entries_are_held() {
        let (_d, r) = titled(16 << 10, 300);
        let free = build(&r, None).unwrap();
        let limit: u64 = 1 << 20;
        let index = free.site.files["search-index.json"].len() as u64;
        // guard: the repeated title amplifies the index past the limit (the doc's page repeats
        // it too, and its writes are refused at the limit; its entries are collected first)
        assert!(index > 4 * limit, "guard: {index} bytes");
        let b = Budget::new(MAX_FILES, limit, MAX_ARTIFACT_BYTES);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{limit} file bytes"));
        // the entries kept never took more bytes than the file may have
        assert!(
            b.index_held() as u64 <= limit,
            "{} entry bytes held",
            b.index_held()
        );
        // the file's buffer was never made
        assert_eq!(b.aux("search-index.json"), 0);
        // the aggregate limit holds the entries the same way
        let b = Budget::new(MAX_FILES, MAX_FILE_BYTES, limit);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{limit} artifact bytes"));
        assert!(b.index_held() as u64 <= limit, "{}", b.index_held());
        assert_eq!(b.aux("search-index.json"), 0);
    }

    /// Fix round 2: `search-index.json`, `llms.txt` and `llms-full.txt` are admitted before their
    /// buffers are made or grow: a file the artifact has no room for is never made, and a
    /// buffer never holds more than its room.
    #[test]
    fn hpb6_agent_files_over_the_limits_are_refused_before_their_buffers_grow() {
        let (_d, r) = project();
        let free = build(&r, None).unwrap();
        let f = &free.site.files;
        let assets = f.keys().filter(|p| p.starts_with("assets/")).count();
        // the pages and permalinks are put first, then the index and the two llms files
        let before_index = f.len() - assets - 3;
        let b = Budget::new(before_index, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{before_index} files"));
        assert_eq!(b.aux("search-index.json"), 0);
        let b = Budget::new(before_index + 1, MAX_FILE_BYTES, MAX_ARTIFACT_BYTES);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{} files", before_index + 1));
        assert!(b.aux("search-index.json") > 0);
        assert_eq!(b.aux("llms.txt"), 0);
        // room for every byte before `llms-full.txt` and for all but one of its own
        let full = f["llms-full.txt"].len() as u64;
        let before: u64 = f
            .iter()
            .filter(|(p, _)| *p != "llms-full.txt" && !p.starts_with("assets/"))
            .map(|(_, f)| f.len() as u64)
            .sum();
        let limit = before + full - 1;
        let b = Budget::new(MAX_FILES, MAX_FILE_BYTES, limit);
        let e = build(&r, Some(&b)).err().expect("refused");
        over(&e, format!("{limit} artifact bytes"));
        assert!(b.aux("llms.txt") > 0);
        assert!(
            (b.aux("llms-full.txt") as u64) < full,
            "{} of {full} bytes held",
            b.aux("llms-full.txt")
        );
        assert!(b.admitted() <= limit);
        // with room for exactly its bytes the file is kept, as a free build makes it
        let b = Budget::new(MAX_FILES, MAX_FILE_BYTES, total(&free));
        let kept = build(&r, Some(&b)).unwrap();
        assert_eq!(kept.site.files, free.site.files);
    }
}
