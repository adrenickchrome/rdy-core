mod html;
pub use html::{escape, text_html};

use rdy_engine::{summary, Rdy, RdyError, Scope, E_IO};
use rdy_graph::{DerivedStatus, EdgeKind};
use rdy_model::{decision, Block, BlockId, FieldValue, Hash};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
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
    RdyError::new(E_IO, format!("template error: {e}"), "this is a bug in rdy-render templates")
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

fn env() -> Result<minijinja::Environment<'static>, RdyError> {
    let mut env = minijinja::Environment::new();
    env.add_template("layout.html", include_str!("../templates/layout.html")).map_err(tpl)?;
    env.add_template("index.html", include_str!("../templates/index.html")).map_err(tpl)?;
    env.add_template("doc.html", include_str!("../templates/doc.html")).map_err(tpl)?;
    env.add_template("decision.html", include_str!("../templates/decision.html")).map_err(tpl)?;
    Ok(env)
}

pub fn render_site(rdy: &mut Rdy, scope: &Scope, out: &Path) -> Result<RenderSummary, RdyError> {
    let m = rdy.materialized(scope)?.clone();
    let suspects: BTreeSet<BlockId> = rdy.suspects(scope)?.into_iter().map(|s| s.block).collect();
    let g = rdy.graph(scope)?;
    let env = env()?;
    let site = "rdy";
    for d in ["doc", "decision", "assets"] {
        std::fs::create_dir_all(out.join(d)).map_err(io)?;
    }
    let mut stats = RenderSummary { pages: 0, assets: 0 };

    // where each block lives, for links
    let href_from = |from_doc: Option<&BlockId>, id: &BlockId, prefix: &str| -> Option<(String, String)> {
        let b = m.get(id)?;
        let label = summary(b);
        if b.block_type == decision::DECISION {
            return Some((format!("{prefix}decision/{id}.html"), label));
        }
        let doc = m.doc_of(id)?;
        if Some(&doc.id) == from_doc {
            Some((format!("#{id}"), label))
        } else {
            Some((format!("{prefix}doc/{}.html#{id}", doc.id), label))
        }
    };

    let mut blobs = BTreeSet::new();
    let mut doc_rows = vec![];
    for doc in m.children(None) {
        let mut items = vec![];
        let mut has_mermaid = false;
        let mut stack: Vec<(&Block, usize)> = m.children(Some(&doc.id)).into_iter().rev().map(|b| (b, 1)).collect();
        while let Some((b, depth)) = stack.pop() {
            let link = |id: &BlockId| href_from(Some(&doc.id), id, "../");
            let html = if b.block_type == "figure" {
                let h = b.field_str("blob").unwrap_or("");
                if let Ok(hash) = h.parse::<Hash>() {
                    blobs.insert(hash);
                }
                let alt = escape(b.field_str("alt").unwrap_or(""));
                if b.field_str("media_type").unwrap_or("").starts_with("image/") {
                    format!("<figure><img src=\"../assets/{}\" alt=\"{alt}\"><figcaption>{alt}</figcaption></figure>", escape(h))
                } else {
                    format!("<p><a href=\"../assets/{}\" download>{alt}</a></p>", escape(h))
                }
            } else {
                if b.block_type == "diagram" {
                    has_mermaid = true;
                }
                html::block_html(b, depth, &link, &format!("../decision/{}.html", b.id), status_word(&g.status(&b.id)))
            };
            items.push(json!({ "id": b.id.to_string(), "block_type": b.block_type, "html": html, "suspect": suspects.contains(&b.id) }));
            for c in m.children(Some(&b.id)).into_iter().rev() {
                stack.push((c, depth + 1));
            }
        }
        let title = doc.field_str("title").unwrap_or("").to_string();
        let docj = json!({ "id": doc.id.to_string(), "title": title, "doc_type": doc.field_str("doc_type"), "lifecycle": doc.field_str("lifecycle") });
        let page = env.get_template("doc.html").map_err(tpl)?.render(json!({ "site": site, "title": title, "root": "../", "mermaid": has_mermaid, "doc": docj, "items": items })).map_err(tpl)?;
        std::fs::write(out.join("doc").join(format!("{}.html", doc.id)), page).map_err(io)?;
        stats.pages += 1;
        doc_rows.push(docj);
    }

    let mut decisions = vec![];
    for b in m.blocks.values().filter(|b| b.block_type == decision::DECISION) {
        let status = status_word(&g.status(&b.id));
        let link_row = |e_kind: &EdgeKind, id: &BlockId, part: &Option<String>| {
            let (href, label) = href_from(None, id, "../").unwrap_or((String::new(), id.to_string()));
            let kind = match e_kind { EdgeKind::Link(n) => n.clone(), EdgeKind::References => "references".into(), EdgeKind::Contains => "contains".into() };
            json!({ "kind": kind, "href": href, "label": label, "part": part })
        };
        let outgoing: Vec<_> = g.outgoing(&b.id).iter().filter(|e| e.kind != EdgeKind::Contains).map(|e| link_row(&e.kind, &e.to, &e.part)).collect();
        let incoming: Vec<_> = g.incoming(&b.id).iter().filter(|e| e.kind != EdgeKind::Contains).map(|e| link_row(&e.kind, &e.from, &e.part)).collect();
        let options: Vec<String> = match b.fields.get("options") { Some(FieldValue::List(o)) => o.iter().filter_map(|x| x.as_str().map(String::from)).collect(), _ => vec![] };
        let statement = b.field_str("statement").unwrap_or("").to_string();
        let dj = json!({
            "id": b.id.to_string(), "statement": statement, "status": status,
            "decided_at": b.field_str("decided_at"), "authority_basis": b.field_str("authority_basis"),
            "body": if b.body.is_empty() { String::new() } else { text_html(&b.body, &|id| href_from(None, id, "../")) },
            "options": options, "outgoing": outgoing, "incoming": incoming,
        });
        let page = env.get_template("decision.html").map_err(tpl)?.render(json!({ "site": site, "title": statement, "root": "../", "mermaid": false, "d": dj })).map_err(tpl)?;
        std::fs::write(out.join("decision").join(format!("{}.html", b.id)), page).map_err(io)?;
        stats.pages += 1;
        decisions.push(json!({ "id": b.id.to_string(), "statement": statement, "status": status }));
    }

    let page = env.get_template("index.html").map_err(tpl)?.render(json!({ "site": site, "title": "Home", "root": "", "mermaid": false, "docs": doc_rows, "decisions": decisions })).map_err(tpl)?;
    std::fs::write(out.join("index.html"), page).map_err(io)?;
    stats.pages += 1;

    for h in blobs {
        std::fs::write(out.join("assets").join(h.to_string()), rdy.store().get_blob(&h)?).map_err(io)?;
        stats.assets += 1;
    }
    Ok(stats)
}
