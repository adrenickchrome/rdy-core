use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

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
    let mut out = Default::default();
    walk(root, root, &mut out);
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
        a.export(&Scope::Canon, &first).unwrap();
        let (_b, mut b) = project();
        b.import(&approver(), "import", &first).unwrap();
        land(&mut b, "import").unwrap();
        prop_assert_eq!(&b.materialized(&Scope::Canon).unwrap().snapshot, &a.materialized(&Scope::Canon).unwrap().snapshot);
        let second = out.path().join("two");
        b.export(&Scope::Canon, &second).unwrap();
        prop_assert_eq!(files(&first), files(&second));
    }
}

/// Each hostile export directory is refused, stores nothing and is fast.
fn refused(dir: &Path) {
    let (_d, mut r) = project();
    let before = r.store().view_state("import").unwrap();
    let t = Instant::now();
    let e = r.import(&approver(), "import", dir).unwrap_err();
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "{}: took {:?}",
        dir.display(),
        t.elapsed()
    );
    r.invalidate();
    assert_eq!(
        r.store().view_state("import").unwrap(),
        before,
        "{}: {e}",
        dir.display()
    );
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    let t = Instant::now();
    let diags = check_path(&pack, None, dir).unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
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
fn io_hostile_trees_are_refused_quickly_and_store_nothing() {
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
    // a symlink to a file outside the export
    let t = export_dir(&[]);
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/hosts", t.path().join("docs/hosts.yaml")).unwrap();
    refused(t.path());
    // a 20 MB doc file
    let big = format!(
        "{DOC}- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: \"{}\"\n  fields:\n    bogus: x\n",
        "x".repeat(20_000_000)
    );
    let t = export_dir(&[("big.yaml", &big)]);
    refused(t.path());
}
