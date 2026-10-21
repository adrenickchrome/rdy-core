use rdy_model::BlockId;
use rdy_model::tree::*;
use std::collections::BTreeSet;

#[test]
fn valid_names() {
    let long = "a".repeat(64);
    for n in [
        "wallets",
        "Wallets",
        "a",
        "transactions.v2",
        "theirs",
        "theirs.v2",
        "x_y",
        "a-b",
        "com0",
        "com10",
        "console",
        "lpt",
        long.as_str(),
    ] {
        assert_eq!(name_problem(n), None, "{n:?}");
    }
}

#[test]
fn invalid_names() {
    let long = "a".repeat(65);
    for n in [
        "",
        long.as_str(),
        "has space",
        "ünï",
        "a/b",
        ".hidden",
        "_folder",
        "doc.yaml",
        "doc.YML",
        "plan.theirs",
        "plan.Theirs",
        "index",
        "INDEX",
        "c-ABCDE",
        "fd-7KQ2M",
        "con",
        "CON",
        "con.txt",
        "Nul.md",
        "com1",
        "LPT9.md",
        "notes.",
    ] {
        assert!(name_problem(n).is_some(), "{n:?} should be invalid");
    }
}

#[test]
fn slugs() {
    assert_eq!(slug("Wallets & Cards"), "wallets-cards");
    assert_eq!(slug("  Hello  "), "hello");
    assert_eq!(slug("Ünïcødé — design"), "n-c-d-design");
    assert_eq!(
        slug("a - b"),
        "a---b",
        "only runs outside the allowed set collapse"
    );
    assert_eq!(slug("日本語"), "");
    assert_eq!(slug("Intro."), "intro");
    assert_eq!(slug("...Notes"), "notes");
    let long = slug(&format!("{} tail", "x".repeat(63)));
    assert_eq!(
        long,
        "x".repeat(63),
        "cut to 64, then a trailing '-' is removed"
    );
}

#[test]
fn default_names() {
    let id = BlockId::parse("d-7KQ2M").unwrap();
    assert_eq!(default_name(Some("Wallets"), &id), "wallets");
    assert_eq!(default_name(Some("日本語"), &id), "untitled-7kq2m");
    assert_eq!(
        default_name(Some("Index"), &id),
        "untitled-7kq2m",
        "slug 'index' is reserved"
    );
    assert_eq!(default_name(Some("CON"), &id), "untitled-7kq2m");
    assert_eq!(default_name(None, &id), "untitled-7kq2m");
    assert_eq!(default_name(Some("Version 1.2..."), &id), "version-1.2");
}

#[test]
fn dedupe_appends_a_number() {
    let taken: BTreeSet<String> = ["wallets", "a", "a-2"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(dedupe("Wallets", &taken), "Wallets-2", "case-insensitive");
    assert_eq!(dedupe("a", &taken), "a-3");
    assert_eq!(dedupe("fresh", &taken), "fresh");
    let long = "b".repeat(64);
    let taken: BTreeSet<String> = [long.clone()].into_iter().collect();
    let d = dedupe(&long, &taken);
    assert_eq!(d.len(), 64);
    assert!(d.ends_with("-2"));
    assert!(is_tree_type("doc") && is_tree_type("folder") && !is_tree_type("section"));
}

const LOOKALIKE: &str = "a name cannot look like a block id (it would be read as one)";

#[test]
fn plc4_only_file_id_prefixes_fold_name_case() {
    // the id grammar is unchanged: the name rule alone folds case, and only for d and fd
    for n in [
        "d-abcde",
        "D-AbCdE",
        "fd-7kq2m",
        "Fd-7Kq2m",
        "d-ABCDE",
        "fd-7KQ2M",
        "ABC-ZZZZZ",
        "c-ABCDE",
    ] {
        assert_eq!(name_problem(n).as_deref(), Some(LOOKALIKE), "{n:?}");
    }
    for n in [
        "ABC-zzzzz",
        "c-abcde",
        "has-space",
        "use-cases",
        "api-specs",
        "the-graph",
        "new-terms",
        "ci-check",
        "abcd-abcde",
        "d-abcdi",
        "d-abcdu",
        "d-abcdef",
        "d-abcd",
    ] {
        assert_eq!(name_problem(n), None, "{n:?}");
    }
    assert!(name_problem("").is_some());
    let long = format!("d-{}", "a".repeat(63));
    assert_eq!(long.len(), 65);
    assert_eq!(
        name_problem(&long).as_deref(),
        Some("a name has 1 to 64 characters")
    );
    assert!(
        BlockId::parse("d-abcde").is_err(),
        "ids stay case-sensitive"
    );
    assert!(!BlockId::is_valid("d-abcde"));
}

#[test]
fn plc4_default_name_preserves_ordinary_slugs() {
    let id = BlockId::parse("d-7KQ2M").unwrap();
    assert_eq!(default_name(Some("D-AbCdE"), &id), "untitled-7kq2m");
    assert_eq!(default_name(Some("fd-7kq2m"), &id), "untitled-7kq2m");
    for (title, want) in [
        ("has space", "has-space"),
        ("Use cases", "use-cases"),
        ("API specs", "api-specs"),
        ("The graph", "the-graph"),
        ("New terms", "new-terms"),
        ("CI check", "ci-check"),
    ] {
        assert_eq!(default_name(Some(title), &id), want, "{title:?}");
    }
}
