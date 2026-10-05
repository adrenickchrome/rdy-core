//! The home page's context (Task 12 reworks it).
use crate::html::escape;
use crate::site::{Site, rel};
use rdy_engine::{Materialized, VersionInfo};
use rdy_model::{BlockId, tree};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct VersionRow {
    pub name: String,
    pub landing_short: String,
    pub message: Option<String>,
    pub current: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Home {
    pub description: Option<String>,
    /// The whole tree as nested lists, escaped here in Rust and emitted with `|safe`.
    pub tree: String,
    pub versions: Vec<VersionRow>,
}

pub fn build(
    site: &Site,
    m: &Materialized,
    versions: &[VersionInfo],
    _landed_ms: &BTreeMap<String, i64>,
    description: Option<&str>,
    _has_terms: bool,
    _has_questions: bool,
) -> Home {
    let current = versions.last().map(|v| &v.name);
    Home {
        description: description.map(String::from),
        tree: list(site, m, "index.html", None),
        versions: versions
            .iter()
            .map(|v| VersionRow {
                name: v.name.clone(),
                landing_short: v.landing.chars().take(12).collect(),
                message: v.message.clone(),
                current: Some(&v.name) == current,
            })
            .collect(),
    }
}

/// Nested list of the tree under `parent`, every folder open.
fn list(site: &Site, m: &Materialized, from: &str, parent: Option<&BlockId>) -> String {
    let mut s = String::from("<ul>");
    for b in m
        .children(parent)
        .into_iter()
        // only blocks with a page: past MAX_FOLDER_DEPTH (a damaged store) there are none,
        // which also bounds this recursion
        .filter(|b| tree::is_tree_type(&b.block_type))
    {
        let Some(page) = site.page(&b.id) else {
            continue;
        };
        s.push_str(&format!(
            "<li><a href=\"{}\">{}</a>",
            escape(&rel(from, &page)),
            escape(&site.label(b))
        ));
        if b.block_type == tree::FOLDER {
            s.push_str(&list(site, m, from, Some(&b.id)));
        }
        s.push_str("</li>");
    }
    s.push_str("</ul>");
    s
}
