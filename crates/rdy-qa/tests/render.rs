use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::path::{Path, PathBuf};

const EVIL: &str = "<script>alert(1)</script>\"'><img src=x onerror=alert(2)>&amp;";

fn hostile_site() -> (tempfile::TempDir, tempfile::TempDir) {
    let (d, mut r) = project();
    let blob = r.store_mut().put_blob(b"png").unwrap().to_string();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "evil", "title": EVIL, "description": EVIL}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "page", "title": EVIL, "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": EVIL}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": EVIL}, "body": EVIL},
        {"op": "create", "type": "claim", "parent": "$s", "body": format!("{EVIL} {{{{$t}}}}")},
        {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": [EVIL], "rows": [[EVIL]]}},
        {"op": "create", "type": "flow", "parent": "$s", "fields": {"name": EVIL, "steps": [EVIL]}},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": EVIL}, "body": format!("graph TD\n A[{EVIL}]")},
        {"op": "create", "type": "code", "parent": "$s", "fields": {"lang": EVIL}, "body": EVIL},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": EVIL}}
    ])).unwrap();
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: res.created["$s"].clone(),
            statement: EVIL.into(),
            options: vec![EVIL.into()],
            body: EVIL.into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v0.1.0", None, Some(EVIL.into()))
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    (d, out)
}

fn pages(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p)
            } else if p.extension().is_some_and(|x| x == "html") {
                out.push(p)
            }
        }
    }
    out
}

#[test]
fn render_hostile_text_is_escaped_on_every_page() {
    let (_d, site) = hostile_site();
    for p in pages(site.path()) {
        let html = std::fs::read_to_string(&p).unwrap();
        for raw in ["<script>alert(1)", "onerror=alert(2)>", "\"'><img"] {
            assert!(!html.contains(raw), "{}: raw {raw:?}", p.display());
        }
    }
}

/// Every attribute value of `attr` in `html`.
fn attrs<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let key = format!("{attr}=\"");
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find(&key) {
        rest = &rest[i + key.len()..];
        if let Some(j) = rest.find('"') {
            out.push(&rest[..j]);
            rest = &rest[j..];
        }
    }
    out
}

#[test]
fn render_every_link_resolves() {
    let (_d, site) = hostile_site();
    let mut broken = vec![];
    for p in pages(site.path()) {
        let html = std::fs::read_to_string(&p).unwrap();
        for link in attrs(&html, "href").into_iter().chain(attrs(&html, "src")) {
            if link.starts_with("http://") || link.starts_with("https://") || link.is_empty() {
                continue;
            }
            let link = link.replace("&amp;", "&");
            let (file, anchor) = match link.split_once('#') {
                Some((f, a)) => (f.to_string(), Some(a.to_string())),
                None => (link.clone(), None),
            };
            let target = if file.is_empty() {
                p.clone()
            } else {
                p.parent().unwrap().join(&file)
            };
            if !target.exists() {
                broken.push(format!("{} → {link}", p.display()));
                continue;
            }
            if let Some(a) = anchor {
                let t = std::fs::read_to_string(&target).unwrap_or_default();
                if !t.contains(&format!("id=\"{a}\"")) {
                    broken.push(format!("{} → {link} (no anchor)", p.display()));
                }
            }
        }
    }
    assert!(broken.is_empty(), "{broken:#?}");
}
