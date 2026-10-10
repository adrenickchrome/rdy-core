use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::path::Path;

fn files(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, d: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out)
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_string_lossy().into(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
    walk(root, root, &mut out);
    // a re-export lands again: its landing times are new, the rest of authors.yaml is not
    if let Some(a) = out.get_mut("authors.yaml") {
        let kept: String = String::from_utf8_lossy(a)
            .lines()
            .filter(|l| !l.trim_start().starts_with("landed_ms:"))
            .map(|l| format!("{l}\n"))
            .collect();
        *a = kept.into_bytes();
    }
    out
}

/// A random project from a recipe: folders, docs, sections, claims with references, a term, a figure.
fn build(r: &mut Rdy, shape: &[(u8, u8)]) {
    let blob = r.store_mut().put_blob(b"\x89PNG qa").unwrap().to_string();
    let mut ops = vec![
        json!({"op": "create", "type": "folder", "id": "$f0", "fields": {"name": "root-folder", "title": "Root"}}),
    ];
    for (i, (docs, claims)) in shape.iter().enumerate() {
        let f = format!("$f{}", i + 1);
        ops.push(json!({"op": "create", "type": "folder", "id": f, "parent": "$f0", "fields": {"name": format!("f{i}")}}));
        for d in 0..(*docs % 3 + 1) {
            let (doc, sec, term) = (
                format!("$d{i}_{d}"),
                format!("$s{i}_{d}"),
                format!("$t{i}_{d}"),
            );
            ops.push(json!({"op": "create", "type": "doc", "id": doc, "parent": f, "fields": {"title": format!("Doc {i} {d} — ünïcødé"), "doc_type": "glossary"}}));
            ops.push(json!({"op": "create", "type": "section", "id": sec, "parent": doc, "fields": {"title": "Terms"}}));
            ops.push(json!({"op": "create", "type": "term", "id": term, "parent": sec, "fields": {"name": "Ledger"}, "body": "Line one.\n\"quoted\": colon"}));
            for c in 0..(*claims % 4) {
                ops.push(json!({"op": "create", "type": "claim", "parent": sec, "body": format!("Claim {c} uses {{{{{term}}}}} and \\{{{{literal}}}}.")}));
            }
            ops.push(json!({"op": "create", "type": "figure", "parent": sec, "fields": {"blob": blob, "media_type": "image/png", "alt": "fig"}}));
        }
    }
    write(r, "work", json!(ops)).unwrap();
    land(r, "work").unwrap();
}

/// Seeds this thread's block ids for one case; unseeds them on drop, even if the case fails.
struct SeededIds;

impl SeededIds {
    fn new(key: &str) -> SeededIds {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        rdy_model::seed_ids_for_tests(h.finish());
        SeededIds
    }
}

impl Drop for SeededIds {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

proptest! {
    #![proptest_config(Config { cases: cases(12), rng_seed: RngSeed::Fixed(0x10_7e57), failure_persistence: None, ..Config::default() })]
    #[test]
    fn io_round_trip_is_exact(shape in proptest::collection::vec((any::<u8>(), any::<u8>()), 1..5)) {
        let _ids = SeededIds::new(&format!("{shape:?}"));
        let (_a, mut a) = project();
        build(&mut a, &shape);
        let out = tempfile::tempdir().unwrap();
        let first = out.path().join("one");
        a.read().export(&Scope::Canon, &first).unwrap();
        let (_b, mut b) = project();
        b.import(&approver(), "import", &first).unwrap();
        land(&mut b, "import").unwrap();
        prop_assert_eq!(&b.read().materialized(&Scope::Canon).unwrap().snapshot(), &a.read().materialized(&Scope::Canon).unwrap().snapshot());
        let second = out.path().join("two");
        b.read().export(&Scope::Canon, &second).unwrap();
        prop_assert_eq!(files(&first), files(&second));
    }
}

/// Each hostile export directory is refused and stores nothing. How fast is a perf-tier question
/// (`refuse_import_20mb_scalar`, `refuse_check_20mb_scalar`).
fn refused(dir: &Path) {
    refused_import(dir);
    refused_check(dir);
}

/// Import refuses the tree and stores nothing.
fn refused_import(dir: &Path) {
    let (_d, mut r) = project();
    let before = r.store().view_state("import").unwrap();
    let e = r.import(&approver(), "import", dir).unwrap_err();
    r.invalidate();
    assert_eq!(
        r.store().view_state("import").unwrap(),
        before,
        "{}: {e}",
        dir.display()
    );
}

/// `rdy check` reports a positioned error for the tree.
fn refused_check(dir: &Path) {
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    let diags = check_path(&pack, None, dir).unwrap();
    assert!(
        diags
            .iter()
            .any(|d| d.severity == rdy_schema::Severity::Error && d.line > 0),
        "{}: no positioned error from rdy check: {diags:?}",
        dir.display()
    );
}

fn export_dir(docs: &[(&str, &str)]) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    for (path, text) in docs {
        let p = t.path().join("docs").join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    t
}

const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: T\n    doc_type: glossary\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Terms\n";

#[test]
fn io_hostile_trees_are_refused_and_store_nothing() {
    let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200));
    let wide = format!(
        "[{}]",
        (0..100_000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let cases: Vec<(&str, String)> = vec![
        (
            "syntax.yaml",
            "- id: d-AAAAA\n  type: doc\n    order: a1\n".into(),
        ),
        ("deep.yaml", deep),
        ("wide.yaml", wide),
        (
            "badorder.yaml",
            DOC.replace(
                "order: a1\n  fields:\n    title: Terms",
                "order: a0\n  fields:\n    title: Terms",
            ),
        ),
        (
            "unicodeorder.yaml",
            DOC.replace(
                "order: a1\n  fields:\n    title: Terms",
                "order: é\n  fields:\n    title: Terms",
            ),
        ),
        (
            "longorder.yaml",
            DOC.replace(
                "order: a1\n  fields:\n    title: Terms",
                &format!("order: {}\n  fields:\n    title: Terms", "z".repeat(30)),
            ),
        ),
        (
            "nested.yaml",
            format!(
                "{DOC}- id: d-BBBBB\n  type: doc\n  order: a2\n  fields:\n    title: U\n    doc_type: glossary\n"
            ),
        ),
        ("d-AAAAA.yaml", DOC.into()),
    ];
    for (name, text) in cases {
        let t = export_dir(&[(name, &text)]);
        refused(t.path());
    }
    // the same block id twice in one doc file
    let t = export_dir(&[("dup.yaml", &format!("{DOC}{DOC}"))]);
    refused(t.path());
    // the same id in two different doc files
    let t = export_dir(&[("a.yaml", DOC), ("b.yaml", DOC)]);
    refused(t.path());
    // format mixes: a format-1 manifest over a format-2 tree
    let t = export_dir(&[
        (
            "f/_folder.yaml",
            "- id: f-AAAAA\n  type: folder\n  order: a1\n  fields:\n    title: F\n",
        ),
        ("f/doc.yaml", DOC),
    ]);
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 1\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    refused_import(t.path());
    // a format-2 manifest over a flat format-1 layout
    let t = export_dir(&[("d-AAAAA.yaml", DOC)]);
    refused(t.path());
    // A 20 MB doc file: proves bounded time on a huge scalar, not a size limit (none is specified).
    let big = format!(
        "{DOC}- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: \"{}\"\n  fields:\n    bogus: x\n",
        "x".repeat(20_000_000)
    );
    let t = export_dir(&[("big.yaml", &big)]);
    refused(t.path());
}

/// ORD-3: import and `rdy check` refuse an order key over 24 characters with `E_ORDER_KEY`
/// (and only that), while a key of exactly 24 is accepted.
#[test]
fn ord3_import_and_check_refuse_an_order_key_over_24_characters() {
    let with_key = |key: &str| {
        DOC.replace(
            "order: a1\n  fields:\n    title: Terms",
            &format!("order: {key}\n  fields:\n    title: Terms"),
        )
    };
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    assert_eq!(rdy_model::MAX_ORDER_KEY_LEN, 24);
    // the boundary: 24 characters is valid everywhere
    let ok = export_dir(&[("glossary.yaml", &with_key(&"z".repeat(24)))]);
    let diags = check_path(&pack, None, ok.path()).unwrap();
    assert!(
        diags
            .iter()
            .all(|d| d.severity != rdy_schema::Severity::Error),
        "{diags:?}"
    );
    let (_d, mut r) = project();
    r.import(&approver(), "import", ok.path()).unwrap();
    // 25 characters: refused by both, for the key
    let long = export_dir(&[("glossary.yaml", &with_key(&"z".repeat(25)))]);
    let diags = check_path(&pack, None, long.path()).unwrap();
    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == rdy_schema::Severity::Error)
        .collect();
    assert!(!errors.is_empty(), "{diags:?}");
    assert!(errors.iter().all(|d| d.code == "E_ORDER_KEY"), "{errors:?}");
    assert!(errors.iter().all(|d| d.line > 0), "{errors:?}");
    let (_d, mut r) = project();
    let before = r.store().view_state("import").unwrap();
    let e = r.import(&approver(), "import", long.path()).unwrap_err();
    assert!(
        e.code == "E_ORDER_KEY" || e.issues.iter().any(|i| i.code == "E_ORDER_KEY"),
        "{e:?}"
    );
    r.invalidate();
    assert_eq!(r.store().view_state("import").unwrap(), before);
}

/// A symlink from docs/ to a valid doc file outside the export must be refused (sym1): rdy
/// never follows links in an export, and names the link at line 1.
#[cfg(unix)]
#[test]
fn io_symlink_out_of_the_export_is_refused() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("real.yaml"), DOC).unwrap();
    let t = export_dir(&[]);
    std::os::unix::fs::symlink(
        outside.path().join("real.yaml"),
        t.path().join("docs/linked.yaml"),
    )
    .unwrap();
    refused(t.path());
}

#[test]
fn fmt1_a_format_1_manifest_over_a_format_2_tree_is_refused_not_imported_empty() {
    let t = export_dir(&[
        (
            "f/_folder.yaml",
            "- id: f-AAAAA\n  type: folder\n  order: a1\n  fields:\n    title: F\n",
        ),
        ("f/doc.yaml", DOC),
    ]);
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 1\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    refused_import(t.path());
}
