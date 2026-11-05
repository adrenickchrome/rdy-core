//! A table's body shows as a paragraph right after the table, in the site, the print and the
//! Markdown export alike (D-7VCB9, FW-TB1.body).
use rdy_engine::*;
use rdy_model::*;
use rdy_render::print::ir::from_html;
use rdy_render::print::md::{Layout, emit_files};
use rdy_render::print::{PBlock, PageKind, Print, PrintDoc, Run};
use rdy_render::*;
use serde_json::json;
use std::collections::BTreeMap;

const BODY: &str = "Counts are per release.";

/// The site page of a doc holding two tables: one with a body and one with none.
fn site_page() -> String {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let agent = Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    r.write(
        &agent,
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Counts", "doc_type": "components"}},
                {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Tables"}},
                {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": ["A"], "rows": [["1"]]}, "body": BODY},
                {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": ["B"], "rows": [["2"]]}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let out = tempfile::tempdir().unwrap();
    render_site(&r.read(), &Scope::View("work".into()), out.path()).unwrap();
    std::fs::read_to_string(out.path().join("counts.html")).unwrap()
}

#[test]
fn rsig10_site_shows_a_tables_body_after_the_table() {
    let page = site_page();
    let with = page
        .find("<caption>Table 1</caption>")
        .expect("first table");
    let end = with + page[with..].find("</table></div>").unwrap() + "</table></div>".len();
    assert!(
        page[end..].starts_with(&format!("<p>{BODY}</p>")),
        "the body follows the table: {}",
        &page[end..end + 60]
    );
    let none = page
        .find("<caption>Table 2</caption>")
        .expect("second table");
    let end = none + page[none..].find("</table></div>").unwrap() + "</table></div>".len();
    assert!(
        !page[end..].starts_with("<p></p>") && !page[end..].starts_with("<p>"),
        "a table with no body adds no paragraph: {}",
        &page[end..end + 40]
    );
    assert_eq!(page.matches(&format!("<p>{BODY}</p>")).count(), 1);
}

#[test]
fn rsig10_print_layout_keeps_the_body_after_the_table() {
    let html = format!(
        "<div class=\"table-wrap\"><table><caption>With</caption><thead><tr><th scope=\"col\">A</th></tr></thead><tbody><tr><td>1</td></tr></tbody></table></div><p>{BODY}</p>\
         <div class=\"table-wrap\"><table><caption>Without</caption><thead><tr><th scope=\"col\">B</th></tr></thead><tbody><tr><td>2</td></tr></tbody></table></div>"
    );
    let blocks = from_html(&html, "counts.html");
    let kinds: Vec<&str> = blocks
        .iter()
        .map(|b| match b {
            PBlock::Para { .. } => "para",
            PBlock::Table { .. } => "table",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        ["para", "table", "para", "para", "table"],
        "{blocks:?}"
    );
    let PBlock::Para { runs, .. } = &blocks[2] else {
        unreachable!()
    };
    assert_eq!(
        runs.iter()
            .map(|r: &Run| r.text.as_str())
            .collect::<String>(),
        BODY
    );
}

#[test]
fn rsig10_markdown_export_writes_the_body_after_the_rows() {
    let html = format!(
        "<div class=\"table-wrap\"><table><caption>With</caption><thead><tr><th scope=\"col\">A</th></tr></thead><tbody><tr><td>1</td></tr></tbody></table></div><p>{BODY}</p>\
         <div class=\"table-wrap\"><table><caption>Without</caption><thead><tr><th scope=\"col\">B</th></tr></thead><tbody><tr><td>2</td></tr></tbody></table></div>"
    );
    let page = "counts.html";
    let p = Print {
        title: "Project".into(),
        description: None,
        label: String::new(),
        scope: String::new(),
        tag_redacted: false,
        docs: vec![PrintDoc {
            kind: PageKind::Doc,
            id: "d-1".into(),
            title: "Page".into(),
            page: page.into(),
            depth: 0,
            blocks: from_html(&html, page),
        }],
        blobs: BTreeMap::new(),
        warnings: vec![],
        rdy_dir: std::path::PathBuf::new(),
    };
    let mut md = None;
    emit_files(&p, Layout::Plain, false, |path, data| {
        if path == "counts.md" {
            md = Some(String::from_utf8(data.to_vec()).unwrap());
        }
        Ok(())
    })
    .unwrap();
    let md = md.expect("the page's file");
    let row = md.find("| 1 |").expect("first row");
    let body = md.find(BODY).expect("the body");
    let next = md.find("Without").expect("second caption");
    assert!(row < body && body < next, "{md}");
    assert_eq!(md.matches(BODY).count(), 1);
}
