//! The golden fixture's shape (gold1).
use rdy_engine::*;
use rdy_model::tree;
use rdy_qa::golden;

#[test]
fn gold1_the_fixture_loads_with_every_named_block_and_about_400_blocks() {
    let (_d, r, ids) = golden::project();
    let m = r.read().materialized(&Scope::Canon).unwrap().clone();
    let committed = m
        .iter()
        .filter(|b| {
            m.doc_of(&b.id)
                .is_some_and(|d| !d.field_str("title").unwrap_or("").starts_with("Appendix"))
        })
        .count();
    assert!(
        (350..=500).contains(&committed),
        "{committed} blocks outside the appendix"
    );
    for (name, id) in ids.named() {
        assert!(m.get(id).is_some(), "{name} ({id}) is missing");
    }
    let types: std::collections::BTreeSet<&str> = m.iter().map(|b| b.block_type.as_str()).collect();
    for t in [
        "doc",
        "folder",
        "section",
        "prose",
        "claim",
        "requirement",
        "constraint",
        "term",
        "component",
        "actor",
        "interface",
        "flow",
        "example",
        "note",
        "rationale",
        "table",
        "diagram",
        "figure",
        "code",
        "decision",
        "question",
    ] {
        assert!(types.contains(t), "no {t} block");
    }
}

#[test]
fn gold1_the_tree_has_the_folders_depths_cap_and_long_doc() {
    let (_d, r, ids) = golden::project();
    let m = r.read().materialized(&Scope::Canon).unwrap().clone();
    assert_eq!(m.children(Some(&ids.appendix_folder)).len(), 60);
    let sections = m
        .iter()
        .filter(|b| {
            b.block_type == "section"
                && m.doc_of(&b.id).map(|d| d.id.clone()) == Some(ids.spec_doc.clone())
        })
        .count();
    assert!(sections >= 30, "{sections} sections in the long doc");
    let depth = |mut id: rdy_model::BlockId| {
        let mut n = 0;
        while let Some(b) = m.get(&id) {
            if b.block_type == "section" {
                n += 1
            }
            match &b.parent {
                Some(p) => id = p.clone(),
                None => break,
            }
        }
        n
    };
    assert!(
        m.iter()
            .filter(|b| b.block_type == "section")
            .any(|b| depth(b.id.clone()) == 4),
        "no section at depth 4"
    );
    let bottom = m
        .iter()
        .find(|b| b.field_str("title") == Some("The bottom"))
        .expect("bottom doc");
    let mut folders = 0;
    let mut cur = bottom.parent.clone();
    while let Some(p) = cur {
        let b = m.get(&p).unwrap();
        if b.block_type == tree::FOLDER {
            folders += 1
        }
        cur = b.parent.clone();
    }
    assert_eq!(folders, tree::MAX_FOLDER_DEPTH);
}

#[test]
fn gold1_versions_landings_and_the_view_have_every_case() {
    let (_d, r, _ids) = golden::project();
    assert_eq!(
        r.read().version_position(&Scope::Canon).unwrap().label,
        "v1.0.0 + 2 landings"
    );
    assert_eq!(
        r.read()
            .version_position(&Scope::View("editing".into()))
            .unwrap()
            .label,
        "v1.0.0 + 2 landings · view editing, 2 unlanded changes"
    );
    let v = r.read().versions().unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(
        v[0].message.as_deref(),
        Some("Accepted by the Steering Council")
    );
}

#[test]
fn gold1_edge_content_is_present() {
    let (_d, r, ids) = golden::project();
    let m = r.read().materialized(&Scope::Canon).unwrap().clone();
    assert_eq!(
        m.get(&ids.greek_doc).unwrap().field_str("title"),
        Some("Σημασιολογία")
    );
    assert_eq!(
        m.get(&ids.long_title_doc)
            .unwrap()
            .field_str("title")
            .unwrap()
            .chars()
            .count(),
        200
    );
    assert!(m.get(&ids.big_code).unwrap().body.lines().count() >= 2000);
    assert!(m.get(&ids.brace_code).unwrap().body.contains("{{"));
    assert!(
        m.get(&ids.long_token_prose)
            .unwrap()
            .body
            .contains("CaseBlockEvaluationOrderIsStrictlyTopToBottom")
    );
    let v = r
        .read()
        .materialized(&Scope::View("editing".into()))
        .unwrap()
        .clone();
    for (name, id) in ids.view_named() {
        assert!(
            v.get(id).is_some() && m.get(id).is_none(),
            "{name} lives only in the editing view"
        );
    }
    assert!(
        v.get(&ids.dangling_ref_prose)
            .unwrap()
            .body
            .contains("{{p-ZZZZZ}}")
    );
    assert_eq!(
        v.get(&ids.proposed_decision).unwrap().field_str("status"),
        Some("proposed")
    );
    if let Some(rdy_model::FieldValue::List(cols)) =
        m.get(&ids.wide_table).unwrap().fields.get("columns")
    {
        assert_eq!(cols.len(), 20)
    } else {
        panic!("columns")
    }
    assert!(
        r.store()
            .has_blob(
                &m.get(&ids.image_figure)
                    .unwrap()
                    .field_str("blob")
                    .unwrap()
                    .parse()
                    .unwrap()
            )
            .unwrap()
    );
}

/// Prints the store's hash of every file under the fixture's `blobs/` directory, which is
/// what each file must be named (`rdy import` checks the name). Run it after changing a blob:
/// `cargo test -p rdy-qa --release --test render_quality_fixture -- --ignored --nocapture`
#[test]
#[ignore = "helper: regenerates the blob file names"]
fn gold1_print_blob_hashes() {
    let dir = std::path::Path::new(golden::FIXTURE_DIR).join("blobs");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for f in files {
        let bytes = std::fs::read(&f).unwrap();
        println!(
            "{} -> {} ({} bytes)",
            f.file_name().unwrap().to_string_lossy(),
            rdy_store::Store::blob_hash(&bytes),
            bytes.len()
        );
    }
}

#[test]
fn gold1_the_site_helpers_render_canon_the_frozen_version_and_the_view() {
    for (what, (_d, _r, _ids, out)) in [
        ("canon", golden::site_canon()),
        ("frozen", golden::site_frozen()),
        ("view", golden::site_view()),
    ] {
        let pages = golden::pages(out.path());
        assert!(pages.len() > 80, "{what}: {} pages", pages.len());
        assert!(
            golden::html(out.path(), "index.html").contains("<html"),
            "{what}: no home page"
        );
    }
}

#[test]
fn gold1_the_fixture_has_direct_content_keys_and_the_dx2_shapes() {
    let (_d, r, ids) = golden::project();
    let m = r.read().materialized(&Scope::Canon).unwrap().clone();
    let first: Vec<rdy_model::BlockId> = m
        .children(Some(&ids.spec_doc))
        .iter()
        .take(4)
        .map(|b| b.id.clone())
        .collect();
    assert_eq!(
        first,
        [
            ids.opening_prose.clone(),
            ids.opening_list.clone(),
            ids.opening_steps.clone(),
            ids.opening_note.clone()
        ]
    );
    let keyed: std::collections::BTreeSet<&str> =
        m.iter().filter_map(|b| b.field_str("key")).collect();
    assert_eq!(keyed, golden::KEYS.into_iter().collect());
    for b in m.iter() {
        for k in golden::KEYS {
            assert!(
                !b.body.contains(&format!("{{{{{k}")),
                "{} still holds {{{{{k}…",
                b.id
            );
        }
    }
    assert!(
        m.get(&ids.opening_list)
            .unwrap()
            .body
            .contains(&format!("{{{{{}|guards}}}}", ids.guards_section))
    );
    assert!(
        m.get(&ids.langless_code)
            .unwrap()
            .field_str("lang")
            .is_none()
    );
    assert!(
        m.get(&ids.altless_diagram)
            .unwrap()
            .field_str("alt")
            .is_none()
    );
}
