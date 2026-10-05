mod html;
pub use html::{escape, text_html};

use rdy_engine::{E_IO, Materialized, Rdy, RdyError, Scope, summary_in};
use rdy_graph::{DerivedStatus, EdgeKind};
use rdy_model::{Block, BlockId, FieldValue, Hash, decision, tree};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct RenderSummary {
    pub pages: usize,
    pub assets: usize,
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

fn status_word(s: &Option<DerivedStatus>) -> &'static str {
    match s {
        Some(DerivedStatus::Proposed) => "proposed",
        Some(DerivedStatus::Rejected) => "rejected",
        Some(DerivedStatus::Live) => "live",
        Some(DerivedStatus::PartiallySuperseded { .. }) => "partially superseded",
        Some(DerivedStatus::Superseded { .. }) => "superseded",
        Some(DerivedStatus::Revoked { .. }) => "revoked",
        None => "",
    }
}

/// A link from page `from` to page `to`, both site-relative.
fn rel(from: &str, to: &str) -> String {
    format!("{}{to}", "../".repeat(from.matches('/').count()))
}

/// The most entries one list of the sidebar shows (the index's Contents shows all).
const SIDEBAR_SIBLINGS: usize = 50;

struct Site<'a> {
    m: &'a Materialized,
    /// Every folder's and doc's tree path.
    paths: BTreeMap<BlockId, String>,
}

impl<'a> Site<'a> {
    fn new(m: &'a Materialized) -> Site<'a> {
        // `level` folders are above `parent`'s children; only a damaged store nests deeper than
        // MAX_FOLDER_DEPTH, and what lies below it gets no page
        fn walk(
            m: &Materialized,
            parent: Option<&BlockId>,
            prefix: &str,
            level: usize,
            out: &mut BTreeMap<BlockId, String>,
        ) {
            if level > tree::MAX_FOLDER_DEPTH {
                return;
            }
            for (id, name) in m.names_under(parent) {
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                if m.get(&id).is_some_and(|b| b.block_type == tree::FOLDER) {
                    walk(m, Some(&id), &path, level + 1, out);
                }
                out.insert(id, path);
            }
        }
        let mut paths = BTreeMap::new();
        walk(m, None, "", 0, &mut paths);
        Site { m, paths }
    }

    /// The page of a folder or doc.
    fn page(&self, id: &BlockId) -> Option<String> {
        let p = self.paths.get(id)?;
        Some(match self.m.get(id)?.block_type == tree::FOLDER {
            true => format!("{p}/index.html"),
            false => format!("{p}.html"),
        })
    }

    fn label(&self, b: &Block) -> String {
        b.field_str("title")
            .filter(|t| !t.is_empty())
            .map(String::from)
            .or_else(|| self.m.name_of(&b.id))
            .unwrap_or_else(|| b.id.to_string())
    }

    /// The folders and doc from the top level down to `id`, top first.
    fn chain(&self, id: &BlockId) -> Vec<BlockId> {
        let mut out = vec![];
        let mut cur = Some(id.clone());
        while let Some(c) = cur {
            if out.len() > self.m.blocks.len() {
                break;
            }
            let Some(b) = self.m.get(&c) else { break };
            if tree::is_tree_type(&b.block_type) {
                out.push(c.clone());
            }
            cur = b.parent.clone();
        }
        out.reverse();
        out
    }

    /// Hrefs are escaped here in Rust and emitted with `|safe`; labels go through autoescape.
    fn crumbs(
        &self,
        from: &str,
        chain: &[BlockId],
        tail: Option<String>,
    ) -> Vec<serde_json::Value> {
        let mut out = vec![json!({"label": "rdy", "href": rel(from, "index.html")})];
        for (i, id) in chain.iter().enumerate() {
            let b = self.m.get(id).expect("chain blocks exist");
            let here = tail.is_none() && i + 1 == chain.len();
            let href = if here {
                None
            } else {
                self.page(id).map(|p| escape(&rel(from, &p)))
            };
            out.push(json!({"label": self.label(b), "href": href}));
        }
        if let Some(t) = tail {
            out.push(json!({"label": t, "href": null}));
        }
        out
    }

    /// Nested list of the tree under `parent`; `expand` decides which folders open. With `cap`,
    /// each list shows at most SIDEBAR_SIBLINGS entries around the one on the current path and
    /// ends with a "+N more" link to the folder's page.
    /// Hrefs and labels are escaped here in Rust; templates emit the result with `|safe`.
    fn list(
        &self,
        from: &str,
        parent: Option<&BlockId>,
        current: Option<&BlockId>,
        expand: &dyn Fn(&BlockId) -> bool,
        cap: bool,
    ) -> String {
        let all: Vec<&Block> = self
            .m
            .children(parent)
            .into_iter()
            // only blocks with a page: past MAX_FOLDER_DEPTH (a damaged store) there are none,
            // which also bounds this recursion
            .filter(|b| tree::is_tree_type(&b.block_type) && self.paths.contains_key(&b.id))
            .collect();
        let (shown, more) = if cap && all.len() > SIDEBAR_SIBLINGS {
            let here = all
                .iter()
                .position(|b| Some(&b.id) == current || expand(&b.id))
                .unwrap_or(0);
            let first = here
                .saturating_sub(SIDEBAR_SIBLINGS / 2)
                .min(all.len() - SIDEBAR_SIBLINGS);
            (
                &all[first..first + SIDEBAR_SIBLINGS],
                all.len() - SIDEBAR_SIBLINGS,
            )
        } else {
            (&all[..], 0)
        };
        let mut s = String::from("<ul>");
        for b in shown {
            let cls = if Some(&b.id) == current {
                " class=\"current\""
            } else {
                ""
            };
            let href = self.page(&b.id).map(|p| rel(from, &p)).unwrap_or_default();
            s.push_str(&format!(
                "<li{cls}><a href=\"{}\">{}</a>",
                escape(&href),
                escape(&self.label(b))
            ));
            if b.block_type == tree::FOLDER && expand(&b.id) {
                s.push_str(&self.list(from, Some(&b.id), current, expand, cap));
            }
            s.push_str("</li>");
        }
        if more > 0 {
            let target = parent
                .and_then(|p| self.page(p))
                .unwrap_or_else(|| "index.html".into());
            s.push_str(&format!(
                "<li class=\"more\"><a href=\"{}\">+{more} more</a></li>",
                escape(&rel(from, &target))
            ));
        }
        s.push_str("</ul>");
        s
    }

    /// The neighbourhood of `chain`'s last node: ancestors, their siblings, the current folder's children.
    fn sidebar(&self, from: &str, chain: &[BlockId], current: Option<&BlockId>) -> String {
        self.list(from, None, current, &|id| chain.contains(id), true)
    }
}

fn env() -> Result<minijinja::Environment<'static>, RdyError> {
    let mut env = minijinja::Environment::new();
    env.add_template("layout.html", include_str!("../templates/layout.html"))
        .map_err(tpl)?;
    env.add_template("index.html", include_str!("../templates/index.html"))
        .map_err(tpl)?;
    env.add_template("folder.html", include_str!("../templates/folder.html"))
        .map_err(tpl)?;
    env.add_template("doc.html", include_str!("../templates/doc.html"))
        .map_err(tpl)?;
    env.add_template("decision.html", include_str!("../templates/decision.html"))
        .map_err(tpl)?;
    Ok(env)
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
pub fn render_site(rdy: &mut Rdy, scope: &Scope, out: &Path) -> Result<RenderSummary, RdyError> {
    let refuse = |what: String| {
        RdyError::new(
            rdy_engine::E_BAD_INPUT,
            what,
            "render into a new or empty directory, or one rdy rendered before (it holds .rdy-site)",
        )
    };
    // `link/` and `link/.` resolve a link when examined, so look at the path without them.
    let out: PathBuf = out.components().collect();
    let out = out.as_path();
    if std::fs::symlink_metadata(out).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(refuse(format!("{} is a symbolic link", out.display())));
    }
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
    // Staging goes beside `out`; when that is not allowed and `out` exists, inside it.
    let beside = parent.join(format!(".{name}.rdy-render-partial"));
    if beside.exists() {
        std::fs::remove_dir_all(&beside).map_err(|e| io_at("cannot clear", &beside, e))?;
    }
    let (staging, inside) = match std::fs::create_dir(&beside) {
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
    let stats = match render_into(rdy, scope, &staging).and_then(|s| {
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
    } else if !inside
        && swap_by_rename(
            &staging,
            out,
            &parent.join(format!(".{name}.rdy-render-old")),
        )
    {
        Ok(())
    } else {
        replace_in_place(&staging, out)
    };
    if let Err(e) = swapped {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    Ok(stats)
}

/// Where staging goes inside `out` when its parent can't take it.
const STAGING_INSIDE: &str = ".rdy-render-partial";

/// Moves `out` aside, `staging` into its place, and drops the old copy. False, with `out` as it
/// was, when a rename is refused (a mount point, a parent that is not writable).
fn swap_by_rename(staging: &Path, out: &Path, old: &Path) -> bool {
    if old.exists() && std::fs::remove_dir_all(old).is_err() {
        return false;
    }
    if std::fs::rename(out, old).is_err() {
        return false;
    }
    if std::fs::rename(staging, out).is_err() {
        let _ = std::fs::rename(old, out);
        return false;
    }
    let _ = std::fs::remove_dir_all(old);
    true
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

fn render_into(rdy: &mut Rdy, scope: &Scope, out: &Path) -> Result<RenderSummary, RdyError> {
    let position = rdy.version_position(scope)?;
    let versions = rdy.versions()?;
    let current = versions.last().map(|v| v.name.clone());
    let version_rows: Vec<_> = versions
        .iter()
        .map(|v| json!({ "name": v.name, "landing_short": &v.landing[..12], "message": v.message, "current": Some(&v.name) == current.as_ref() }))
        .collect();
    let m = rdy.materialized(scope)?.clone();
    let suspects: BTreeSet<BlockId> = rdy.suspects(scope)?.into_iter().map(|s| s.block).collect();
    // figure blobs that exist in the store (checked before `g` borrows `rdy`)
    let present: BTreeSet<Hash> = m
        .blocks
        .values()
        .filter(|b| b.block_type == "figure")
        .filter_map(|b| b.field_str("blob")?.parse::<Hash>().ok())
        .filter(|h| rdy.store().has_blob(h).unwrap_or(false))
        .collect();
    let g = rdy.graph(scope)?;
    let env = env()?;
    let site_name = "rdy";
    let site = Site::new(&m);
    for d in ["_decision", "_assets"] {
        std::fs::create_dir_all(out.join(d)).map_err(io)?;
    }
    let mut stats = RenderSummary {
        pages: 0,
        assets: 0,
    };

    // where each block lives, for links from page `from`
    let href_from =
        |from: &str, from_doc: Option<&BlockId>, id: &BlockId| -> Option<(String, String)> {
            let b = m.get(id)?;
            let label = summary_in(b, &m);
            if b.block_type == decision::DECISION {
                return Some((rel(from, &format!("_decision/{id}.html")), label));
            }
            if tree::is_tree_type(&b.block_type) {
                return Some((rel(from, &site.page(id)?), label));
            }
            let doc = m.doc_of(id)?;
            if Some(&doc.id) == from_doc {
                Some((format!("#{id}"), label))
            } else {
                Some((format!("{}#{id}", rel(from, &site.page(&doc.id)?)), label))
            }
        };

    let mut blobs = BTreeSet::new();
    let ensure_dir = |page: &str| -> Result<(), RdyError> {
        if let Some(parent) = out.join(page).parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        Ok(())
    };
    for doc_id in site.paths.keys() {
        let Some(doc) = m.get(doc_id).filter(|b| b.block_type == tree::DOC) else {
            continue;
        };
        let page_path = site.page(&doc.id).expect("doc has a page");
        let mut items = vec![];
        let mut has_mermaid = false;
        let mut stack: Vec<(&Block, usize)> = m
            .children(Some(&doc.id))
            .into_iter()
            .rev()
            .map(|b| (b, 1))
            .collect();
        while let Some((b, depth)) = stack.pop() {
            let link = |id: &BlockId| href_from(&page_path, Some(&doc.id), id);
            let html = if b.block_type == "figure" {
                let h = b.field_str("blob").unwrap_or("");
                let alt = escape(b.field_str("alt").unwrap_or(""));
                match h.parse::<Hash>() {
                    Ok(hash) if present.contains(&hash) => {
                        blobs.insert(hash);
                        let src = rel(&page_path, &format!("_assets/{h}"));
                        if b.field_str("media_type")
                            .unwrap_or("")
                            .starts_with("image/")
                        {
                            format!(
                                "<figure><img src=\"{src}\" alt=\"{alt}\"><figcaption>{alt}</figcaption></figure>"
                            )
                        } else {
                            format!("<p><a href=\"{src}\" download>{alt}</a></p>")
                        }
                    }
                    _ => format!("<p class=\"asset missing\">missing asset: {alt}</p>"),
                }
            } else {
                if b.block_type == "diagram" {
                    has_mermaid = true;
                }
                html::block_html(
                    b,
                    depth,
                    &link,
                    &rel(&page_path, &format!("_decision/{}.html", b.id)),
                    status_word(&g.status(&b.id)),
                )
            };
            items.push(json!({ "id": b.id.to_string(), "block_type": b.block_type, "html": html, "suspect": suspects.contains(&b.id) }));
            for c in m.children(Some(&b.id)).into_iter().rev() {
                stack.push((c, depth + 1));
            }
        }
        let title = doc.field_str("title").unwrap_or("").to_string();
        let docj = json!({ "id": doc.id.to_string(), "title": title, "doc_type": doc.field_str("doc_type"), "lifecycle": doc.field_str("lifecycle") });
        let chain = site.chain(&doc.id);
        let page = env.get_template("doc.html").map_err(tpl)?.render(json!({
            "site": site_name, "title": title, "version": position.label, "frozen": position.frozen, "mermaid": has_mermaid,
            "crumbs": site.crumbs(&page_path, &chain, None), "sidebar": site.sidebar(&page_path, &chain, Some(&doc.id)),
            "doc": docj, "items": items,
        })).map_err(tpl)?;
        ensure_dir(&page_path)?;
        std::fs::write(out.join(&page_path), page).map_err(io)?;
        stats.pages += 1;
    }

    for folder_id in site.paths.keys() {
        let Some(f) = m.get(folder_id).filter(|b| b.block_type == tree::FOLDER) else {
            continue;
        };
        let page_path = site.page(&f.id).expect("folder has a page");
        let children: Vec<_> = m
            .children(Some(&f.id))
            .into_iter()
            .filter(|c| tree::is_tree_type(&c.block_type))
            .filter_map(|c| {
                let href = escape(&rel(&page_path, &site.page(&c.id)?));
                let kind = if c.block_type == tree::FOLDER {
                    "folder"
                } else {
                    "doc"
                };
                Some(json!({ "href": href, "label": site.label(c), "kind": kind }))
            })
            .collect();
        let label = site.label(f);
        let fj = json!({ "title": label, "description": f.field_str("description"), "id": f.id.to_string() });
        let chain = site.chain(&f.id);
        let page = env.get_template("folder.html").map_err(tpl)?.render(json!({
            "site": site_name, "title": label, "version": position.label, "frozen": position.frozen, "mermaid": false,
            "crumbs": site.crumbs(&page_path, &chain, None), "sidebar": site.sidebar(&page_path, &chain, Some(&f.id)),
            "folder": fj, "children": children,
        })).map_err(tpl)?;
        ensure_dir(&page_path)?;
        std::fs::write(out.join(&page_path), page).map_err(io)?;
        stats.pages += 1;
    }

    let mut decisions = vec![];
    for b in m
        .blocks
        .values()
        .filter(|b| b.block_type == decision::DECISION)
    {
        let status = status_word(&g.status(&b.id));
        let dpage = format!("_decision/{}.html", b.id);
        let link_row = |e_kind: &EdgeKind, id: &BlockId, part: &Option<String>| {
            let (href, label) =
                href_from(&dpage, None, id).unwrap_or((String::new(), id.to_string()));
            let kind = match e_kind {
                EdgeKind::Link(n) => n.clone(),
                EdgeKind::References => "references".into(),
                EdgeKind::Contains => "contains".into(),
            };
            json!({ "kind": kind, "href": href, "label": label, "part": part, "missing": href.is_empty() })
        };
        let outgoing: Vec<_> = g
            .outgoing(&b.id)
            .iter()
            .filter(|e| e.kind != EdgeKind::Contains)
            .map(|e| link_row(&e.kind, &e.to, &e.part))
            .collect();
        let incoming: Vec<_> = g
            .incoming(&b.id)
            .iter()
            .filter(|e| e.kind != EdgeKind::Contains)
            .map(|e| link_row(&e.kind, &e.from, &e.part))
            .collect();
        let options: Vec<String> = match b.fields.get("options") {
            Some(FieldValue::List(o)) => o
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
            _ => vec![],
        };
        let statement = b.field_str("statement").unwrap_or("").to_string();
        let dj = json!({
            "id": b.id.to_string(), "statement": statement, "status": status, "status_class": status.replace(' ', "-"),
            "decided_at": b.field_str("decided_at"), "authority_basis": b.field_str("authority_basis"),
            "body": if b.body.is_empty() { String::new() } else { text_html(&b.body, &|id| href_from(&dpage, None, id)) },
            "options": options, "outgoing": outgoing, "incoming": incoming,
        });
        let chain = m
            .doc_of(&b.id)
            .map(|d| site.chain(&d.id))
            .unwrap_or_default();
        let page = env.get_template("decision.html").map_err(tpl)?.render(json!({
            "site": site_name, "title": statement, "version": position.label, "frozen": position.frozen, "mermaid": false,
            "crumbs": site.crumbs(&dpage, &chain, Some(format!("decision {}", b.id))), "sidebar": site.sidebar(&dpage, &chain, None),
            "d": dj,
        })).map_err(tpl)?;
        std::fs::write(out.join(&dpage), page).map_err(io)?;
        stats.pages += 1;
        decisions.push(json!({ "id": b.id.to_string(), "statement": statement, "status": status, "status_class": status.replace(' ', "-") }));
    }

    let page = env.get_template("index.html").map_err(tpl)?.render(json!({
        "site": site_name, "title": "Home", "version": position.label, "frozen": position.frozen, "versions": version_rows, "mermaid": false,
        "crumbs": [{"label": "rdy", "href": null}], "tree": site.list("index.html", None, None, &|_| true, false), "decisions": decisions,
    })).map_err(tpl)?;
    std::fs::write(out.join("index.html"), page).map_err(io)?;
    stats.pages += 1;

    for h in blobs {
        std::fs::write(
            out.join("_assets").join(h.to_string()),
            rdy.store().get_blob(&h)?,
        )
        .map_err(io)?;
        stats.assets += 1;
    }
    Ok(stats)
}

#[cfg(test)]
mod swap_tests {
    use super::*;

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
}
