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

#[test]
fn akey1_a_taken_p_gets_a_suffix_that_is_still_a_valid_key() {
    // "p-2" would read as a proposal id, so every "p-N" is refused: the suffix differs
    for base in ["p", "P"] {
        let k = unique(base, |k| k == base);
        assert!(problem(&k).is_none(), "{base}: {k:?} {:?}", problem(&k));
        assert_ne!(k, base);
    }
    let taken = ["p", "p_2"];
    let k = unique("p", |k| taken.contains(&k));
    assert!(
        problem(&k).is_none() && !taken.contains(&k.as_str()),
        "{k:?}"
    );
}

#[test]
fn akey1_a_suffix_never_makes_a_key_that_reads_as_a_block_id() {
    // a base of 1 to 3 letters with a 5-digit suffix is a block id ("ab-10000"): skipped
    let taken = |k: &str| {
        k == "ab"
            || k.strip_prefix("ab-")
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n < 10_000)
    };
    let k = unique("ab", taken);
    assert!(problem(&k).is_none(), "{k:?} {:?}", problem(&k));
    assert_eq!(k, "ab-100000");
}
