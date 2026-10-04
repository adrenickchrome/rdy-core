mod html;
pub use html::{escape, text_html};

use rdy_engine::{E_IO, Materialized, Rdy, RdyError, Scope, summary_in};
use rdy_graph::{DerivedStatus, EdgeKind};
use rdy_model::{Block, BlockId, FieldValue, Hash, decision, tree};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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

pub fn render_site(rdy: &mut Rdy, scope: &Scope, out: &Path) -> Result<RenderSummary, RdyError> {
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
