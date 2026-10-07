//! akey1: the key a doc or folder gets from its path (FW-PR3), and how a taken one is made unique.
use rdy_model::key::{from_path, problem, unique};

#[test]
fn akey1_a_path_becomes_a_lowercase_dotted_key() {
    assert_eq!(
        from_path("Reference/MCP-tools", "doc"),
        "reference.mcp-tools"
    );
    assert_eq!(from_path("agents", "folder"), "agents");
    assert_eq!(from_path("a/b_c/d.e", "doc"), "a.b_c.d.e");
}

#[test]
fn akey1_separator_runs_collapse_and_trailing_ones_go() {
    assert_eq!(from_path("v1-/-draft", "doc"), "v1-draft");
    assert_eq!(from_path("notes_", "doc"), "notes");
}

#[test]
fn akey1_a_path_that_cannot_start_a_key_is_prefixed_with_its_type() {
    assert_eq!(from_path("2026/plan", "doc"), "doc.2026.plan");
    assert_eq!(from_path("p-values", "doc"), "doc.p-values");
    assert_eq!(from_path("-x", "folder"), "folder.x");
    assert_eq!(from_path("drafts/p-values", "doc"), "drafts.p-values");
}

#[test]
fn akey1_a_long_path_keeps_its_leaf_and_fits_32_characters() {
    assert_eq!(
        from_path(
            "guides/how-to/working-with-views/advanced-including-changes",
            "doc"
        ),
        "advanced-including-changes"
    );
    assert_eq!(
        from_path("a/this-is-a-very-long-document-name-indeed", "doc"),
        "this-is-a-very-long-document-nam"
    );
    assert_eq!(
        from_path("a/abcdefghijklmnopqrstuvwxyzabcde-fgh", "doc"),
        "abcdefghijklmnopqrstuvwxyzabcde"
    );
}

#[test]
fn akey1_every_derived_key_is_valid() {
    for (path, ty) in [
        ("Reference/MCP-tools", "doc"),
        ("2026/plan", "doc"),
        ("p-values", "doc"),
        ("-x", "folder"),
        ("a.b/c-d/e_f", "doc"),
        (
            "guides/how-to/working-with-views/advanced-including-changes",
            "doc",
        ),
        ("x/0123456789012345678901234567890123456789", "doc"),
        ("AB-12345/CD-67890", "folder"),
    ] {
        let k = from_path(path, ty);
        assert!(problem(&k).is_none(), "{path}: {k:?} {:?}", problem(&k));
        assert!(k.chars().count() <= 32, "{path}: {k}");
    }
}

#[test]
fn akey1_a_taken_key_gets_the_first_free_numeric_suffix_within_32_characters() {
    let taken = ["spec", "spec-2"];
    assert_eq!(unique("spec", |k| taken.contains(&k)), "spec-3");
    assert_eq!(unique("other", |k| taken.contains(&k)), "other");
    let full = "abcdefghijklmnopqrstuvwxyz-abcde";
    assert_eq!(
        unique(full, |k| k == full),
        "abcdefghijklmnopqrstuvwxyz-abc-2"
    );
    let sep = "abcdefghijklmnopqrstuvwxyzabc-de";
    assert_eq!(unique(sep, |k| k == sep), "abcdefghijklmnopqrstuvwxyzabc-2");
}
