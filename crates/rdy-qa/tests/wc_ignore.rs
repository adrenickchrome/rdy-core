//! wcig1: .rdyignore in gitignore syntax (FW-X5): anchoring, directories, negation, globs, limits,
//! and import honouring the import directory's file.
use rdy_engine::E_CONFIG;
use rdy_engine::wc::ignore::*;

fn ig(text: &str) -> Ignore {
    Ignore::parse(text).unwrap()
}

#[test]
fn wcig1_names_match_at_any_depth_and_paths_at_the_root() {
    let i = ig("draft.yaml\n/top.yaml\nsub/inner.yaml\n");
    assert!(i.is_ignored("draft.yaml", false));
    assert!(i.is_ignored("a/b/draft.yaml", false));
    assert!(i.is_ignored("top.yaml", false));
    assert!(!i.is_ignored("a/top.yaml", false));
    assert!(i.is_ignored("sub/inner.yaml", false));
    assert!(!i.is_ignored("x/sub/inner.yaml", false));
}

#[test]
fn wcig1_globs_classes_and_double_stars() {
    let i = ig("*.draft.yaml\nnote?.yaml\n[abc]x.yaml\n[!q]y.yaml\nlogs/**/old.yaml\n**/tmp/\n");
    assert!(i.is_ignored("a/b.draft.yaml", false));
    assert!(!i.is_ignored("a/b.yaml", false));
    assert!(i.is_ignored("note1.yaml", false));
    assert!(!i.is_ignored("note12.yaml", false));
    assert!(i.is_ignored("bx.yaml", false));
    assert!(!i.is_ignored("dx.yaml", false));
    assert!(i.is_ignored("dy.yaml", false));
    assert!(!i.is_ignored("qy.yaml", false));
    assert!(i.is_ignored("logs/old.yaml", false));
    assert!(i.is_ignored("logs/a/b/old.yaml", false));
    assert!(i.is_ignored("x/tmp", true));
    assert!(
        !i.is_ignored("x/tmp", false),
        "a trailing slash matches directories only"
    );
    assert!(
        i.is_ignored("x/tmp/f.yaml", false),
        "a path under an ignored directory"
    );
}

#[test]
fn wcig1_negation_escapes_and_the_last_match_wins() {
    let i = ig("# a comment\n\n*.yaml\n!keep.yaml\n");
    assert!(i.is_ignored("a.yaml", false));
    assert!(!i.is_ignored("keep.yaml", false));
    let i = ig("\\#hash.yaml\nspaced.yaml   \n");
    assert!(i.is_ignored("#hash.yaml", false));
    assert!(i.is_ignored("spaced.yaml", false));
    assert!(!i.is_ignored("x.yaml", false));
    let i = ig("gen/\n!gen/keep.yaml\n");
    assert!(
        i.is_ignored("gen/keep.yaml", false),
        "no re-include under an ignored directory"
    );
}

#[test]
fn wcig1_a_huge_ignore_file_is_e_config_and_patterns_stay_linear() {
    let many: String = (0..2000).map(|i| format!("p{i}.yaml\n")).collect();
    assert_eq!(Ignore::parse(&many).unwrap_err().code, E_CONFIG);
    let big = "a".repeat(MAX_IGNORE_BYTES + 1);
    assert_eq!(Ignore::parse(&big).unwrap_err().code, E_CONFIG);
    // a pattern built to backtrack finishes against a long name (the matcher is linear)
    let i = ig(&format!("{}b\n", "*a".repeat(60)));
    let name = format!("{}.yaml", "a".repeat(4000));
    assert!(!i.is_ignored(&name, false));
}

#[test]
fn wcig1_load_reads_the_root_file_or_nothing() {
    let t = tempfile::tempdir().unwrap();
    assert!(!Ignore::load(t.path()).unwrap().is_ignored("a.yaml", false));
    std::fs::write(t.path().join(IGNORE_FILE), "a.yaml\n").unwrap();
    assert!(Ignore::load(t.path()).unwrap().is_ignored("a.yaml", false));
}

#[test]
fn wcig1_import_skips_what_the_import_dirs_rdyignore_lists() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 3\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(
        t.path().join("docs/keep.yaml"),
        "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Keep\n    doc_type: page\n",
    )
    .unwrap();
    std::fs::write(t.path().join("docs/skip.yaml"), "not: [valid\n").unwrap();
    let (_d, mut r) = rdy_qa::fixtures::project();
    assert!(
        r.import(&rdy_qa::fixtures::approver(), "work", t.path())
            .is_err(),
        "without the ignore file"
    );
    std::fs::write(t.path().join(IGNORE_FILE), "skip.yaml\n").unwrap();
    let w = r
        .import(&rdy_qa::fixtures::approver(), "work", t.path())
        .unwrap();
    assert_eq!(w.created.len(), 1, "{:?}", w.created);
}
