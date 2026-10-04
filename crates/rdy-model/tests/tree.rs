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
