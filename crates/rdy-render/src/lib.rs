mod assets;
pub mod disagree;
mod highlight;
mod home;
mod html;
pub mod inline;
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

/// Writes a value into a template: text is HTML-escaped as `html::escape` does, so that a slash
/// in a path stays a slash (minijinja's own escape turns it into `&#x2f;`).
fn format_value(
    out: &mut minijinja::Output,
    state: &minijinja::State,
    value: &minijinja::Value,
) -> Result<(), minijinja::Error> {
    if matches!(state.auto_escape(), minijinja::AutoEscape::Html)
        && !value.is_safe()
        && let Some(text) = value.as_str()
    {
        return out.write_str(&escape(text)).map_err(|_| {
            minijinja::Error::new(minijinja::ErrorKind::WriteFailure, "cannot write the page")
        });
    }
    minijinja::escape_formatter(out, state, value)
}

fn env() -> Result<minijinja::Environment<'static>, RdyError> {
    let mut env = minijinja::Environment::new();
    env.set_formatter(format_value);
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
    let mut stats = match render_into(rdy, scope, &staging).and_then(|s| {
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

fn render_into(rdy: &ReadState, scope: &Scope, out: &Path) -> Result<RenderSummary, RdyError> {
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
    let mut site = Site::new(&m, &pack);
    site.set_disagreements(disagree::list(rdy, scope)?);
    let review = review::build(rdy, scope)?;
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
        used_blobs: Default::default(),
    };
    let mut stats = RenderSummary {
        pages: 0,
        assets: 0,
        redirects: 0,
        warnings: vec![],
    };
    let ensure_dir = |page: &str| -> Result<(), RdyError> {
        if let Some(parent) = out.join(page).parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        Ok(())
    };

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
    // a permalink page for every folder, doc and block that has a place on the site: the
    // template is rendered once, and every page fills in its target's href and label
    let mut redirect: Option<RedirectTemplate> = None;
    let mut permalinks: Vec<(String, String)> = vec![];
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
        if permalinks.len().is_multiple_of(997) {
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
        permalinks.push((page_url, html));
    }
    // they are written while the pages are built; a doc page at a permalink's path (a doc
    // named like an id in a folder named `go`) is written over by the permalink, as it always was
    let taken: BTreeSet<String> = permalinks.iter().map(|(p, _)| p.clone()).collect();
    stats.redirects = permalinks.len();
    make_dirs(out, &permalinks)?;
    let writer = {
        let out = out.to_path_buf();
        std::thread::spawn(move || write_all(&out, &permalinks))
    };
    let pages = (|| -> Result<bool, RdyError> {
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
        let mut files = par_map(&contexts, |ctx| {
            let html = env
                .get_template(template_of(ctx.kind))
                .map_err(tpl)?
                .render(ctx)
                .map_err(tpl)?;
            Ok((ctx.page_url.clone(), html))
        })?;
        stats.pages = files.len();
        files.retain(|(p, _)| !taken.contains(p));
        make_dirs(out, &files)?;
        write_all(out, &files)?;
        search::write(&index, &site_name, out)?;
        let label = version::label(&position);
        llms::write_index(
            &site,
            out,
            &contexts,
            &site_name,
            description.as_deref(),
            &label,
        )?;
        llms::write_full(out, &contexts, &site_name, description.as_deref(), &label)?;
        Ok(has_mermaid)
    })();
    let written = writer.join().expect("the permalink writer does not panic");
    let has_mermaid = pages?;
    written?;
    assets::write(out, has_mermaid)?;
    for (path, h) in sh.used_blobs.borrow().iter() {
        ensure_dir(path)?;
        std::fs::write(out.join(path), &blobs[h]).map_err(io)?;
        stats.assets += 1;
    }
    Ok(stats)
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

/// Makes the directories of every (site-relative path, text) under `out`.
fn make_dirs(out: &Path, files: &[(String, String)]) -> Result<(), RdyError> {
    let dirs: BTreeSet<&Path> = files
        .iter()
        .filter_map(|(p, _)| Path::new(p).parent())
        .collect();
    for dir in dirs {
        std::fs::create_dir_all(out.join(dir)).map_err(io)?;
    }
    Ok(())
}

/// Writes every (site-relative path, text) under `out`, on a few threads; their directories
/// exist.
fn write_all(out: &Path, files: &[(String, String)]) -> Result<(), RdyError> {
    par_map(files, |(path, text)| {
        std::fs::write(out.join(path), text).map_err(io)
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
