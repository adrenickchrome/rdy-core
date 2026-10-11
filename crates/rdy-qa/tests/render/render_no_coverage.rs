//! cc15: the static site shows no coverage and no confidence (FW-CH3a, D-ABCEB).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::path::{Path, PathBuf};

/// A landed world with coverage issues of several kinds: a doc missing its required sections,
/// an empty section, an unlinked mention, an open question and decisions nothing applies.
fn world(r: &mut Rdy) {
    write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$empty", "parent": "$o", "fields": {"title": "Empty"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "The Ledger is append-only, and the Ledger never shrinks."},
        {"op": "create", "type": "decision", "id": "$d1", "parent": "$s", "fields": {"statement": "Balances live in one store", "status": "proposed"}},
        {"op": "create", "type": "decision", "id": "$d2", "parent": "$s", "fields": {"statement": "Refunds need two approvers", "status": "proposed"}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Who approves refunds?"}},
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$l", "body": "Nothing here."}
    ]))
    .unwrap();
    land(r, "work").unwrap();
}

/// Every `*.html` file under `dir`, recursively, in a stable order.
fn html_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "html") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn cc15_the_site_shows_no_coverage_or_confidence() {
    let (_d, mut r) = project();
    world(&mut r);
    let out = tempfile::tempdir().unwrap();
    let site = out.path().join("site");
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
    let files = html_files(&site);
    assert!(!files.is_empty(), "the site has pages");
    for f in files {
        let t = std::fs::read_to_string(&f).unwrap().to_lowercase();
        assert!(
            !t.contains("coverage") && !t.contains("confidence"),
            "{}",
            f.display()
        );
    }
}
