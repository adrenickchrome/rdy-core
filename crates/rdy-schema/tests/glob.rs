//! The path glob matcher (glob1).
use proptest::prelude::*;
use rdy_schema::{Glob, GlobError, MAX_GLOB_LEN, MAX_PATH_SEGMENTS};

fn g(p: &str) -> Glob {
    Glob::compile(p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

#[test]
fn glob1_literals_match_whole_paths_only() {
    assert!(g("wallets").matches("wallets"));
    assert!(!g("wallets").matches("wallets/transactions"));
    assert!(!g("wallets/transactions").matches("wallets"));
    assert!(!g("wallets").matches(""));
    assert!(g("a.b_c-d").matches("a.b_c-d"));
    assert!(!g("Wallets").matches("wallets"), "case-sensitive");
}

#[test]
fn glob1_a_prefix_is_not_a_match() {
    assert!(!g("wallet").matches("wallets"));
    assert!(!g("wallet").covers("wallets"));
    assert!(!g("wallets/tx").covers("wallets/txs"));
}

#[test]
fn glob1_a_folder_pattern_covers_its_subtree() {
    let w = g("wallets");
    assert!(w.covers("wallets"));
    assert!(w.covers("wallets/transactions"));
    assert!(w.covers("wallets/a/b/c"));
    assert!(!w.covers("payments/wallets"));
    let wx = g("wallets/*/x");
    assert!(wx.covers("wallets/a/x") && wx.covers("wallets/a/x/y"));
    assert!(!wx.covers("wallets/a"));
}

#[test]
fn glob1_star_is_one_segment_and_doublestar_is_any_number() {
    let s = g("wallets/*");
    assert!(s.matches("wallets/a"));
    assert!(!s.matches("wallets/a/b"), "* stays inside a segment");
    assert!(
        s.covers("wallets/a/b"),
        "but covers the subtree of what it matches"
    );
    assert!(!s.matches("wallets"));
    assert!(g("w*s").matches("wallets") && g("w*s").matches("ws") && !g("w*s").matches("wallet"));
    assert!(g("*.md").matches("x.md") && !g("*.md").matches("a/x.md"));
    let d = g("**/glossary");
    assert!(d.matches("glossary") && d.matches("x/glossary") && d.matches("x/y/z/glossary"));
    assert!(!d.matches("glossary/x"));
    assert!(d.covers("x/glossary/terms"));
    assert!(g("**").matches("anything/at/all") && g("**").matches("a"));
    assert!(
        g("a/**/b").matches("a/b")
            && g("a/**/b").matches("a/x/y/b")
            && !g("a/**/b").matches("a/b/c")
    );
    assert!(g("**/**/x").matches("x") && g("**/**/x").matches("p/q/x"));
}

#[test]
fn glob1_question_mark_is_exactly_one_name_character() {
    assert!(g("v?").matches("v1") && !g("v?").matches("v") && !g("v?").matches("v12"));
    assert!(!g("a?b").matches("a/b"), "? never matches a slash");
}

#[test]
fn glob1_invalid_patterns_are_refused_with_the_reason() {
    assert_eq!(Glob::compile("").unwrap_err(), GlobError::Empty);
    assert!(matches!(
        Glob::compile("/a").unwrap_err(),
        GlobError::BadSlash
    ));
    assert!(matches!(
        Glob::compile("a/").unwrap_err(),
        GlobError::BadSlash
    ));
    assert!(matches!(
        Glob::compile("a//b").unwrap_err(),
        GlobError::BadSlash
    ));
    assert!(matches!(
        Glob::compile("a**").unwrap_err(),
        GlobError::BadDoubleStar
    ));
    assert!(matches!(
        Glob::compile("**b/c").unwrap_err(),
        GlobError::BadDoubleStar
    ));
    assert!(matches!(
        Glob::compile("a/***/b").unwrap_err(),
        GlobError::BadDoubleStar
    ));
    let e = Glob::compile("a b").unwrap_err();
    assert_eq!(e, GlobError::BadChar { ch: ' ', at: 1 });
    assert!(e.to_string().contains("not allowed in a path glob"), "{e}");
    assert!(matches!(
        Glob::compile("a[b]").unwrap_err(),
        GlobError::BadChar { ch: '[', .. }
    ));
    assert!(matches!(
        Glob::compile("é").unwrap_err(),
        GlobError::BadChar { ch: 'é', at: 0 }
    ));
    let long = "a".repeat(MAX_GLOB_LEN + 1);
    assert!(
        matches!(Glob::compile(&long).unwrap_err(), GlobError::TooLong { len } if len == MAX_GLOB_LEN + 1)
    );
    assert!(Glob::compile(&"a".repeat(MAX_GLOB_LEN)).is_ok());
}

#[test]
fn glob1_limits_are_enforced() {
    let deep = vec!["a"; MAX_PATH_SEGMENTS + 1].join("/");
    assert!(
        !g("**").matches(&deep),
        "over the segment limit never matches"
    );
    let max = vec!["a"; MAX_PATH_SEGMENTS].join("/");
    assert!(g("**").matches(&max));
    assert!(!g("a").matches("a/"), "an empty segment never matches");
    assert!(!g("**").matches("/a"));
}

#[test]
fn glob1_many_doublestars_are_linear_time() {
    let pattern = format!("{}/a", vec!["**"; 60].join("/"));
    let p = g(&pattern);
    let path = format!("{}/b", vec!["a"; MAX_PATH_SEGMENTS - 1].join("/"));
    let t0 = std::time::Instant::now();
    for _ in 0..100 {
        assert!(!p.matches(&path));
    }
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(200),
        "{:?}",
        t0.elapsed()
    );
    let path2 = format!("{}/a", vec!["x"; MAX_PATH_SEGMENTS - 1].join("/"));
    assert!(p.matches(&path2));
    // 120 bare stars would be `BadDoubleStar`; interleave literals to keep 60 stars in one segment.
    let stars = g(&format!("{}a", "*b".repeat(60)));
    let t0 = std::time::Instant::now();
    assert!(!stars.matches(&"b".repeat(200)));
    assert!(t0.elapsed() < std::time::Duration::from_millis(50));
}

/// A reference matcher: expand the pattern into a regex over segments is overkill; instead
/// compare against the obvious recursive definition on small inputs.
fn reference(pat: &[&str], path: &[&str]) -> bool {
    match (pat.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            reference(&pat[1..], path) || (!path.is_empty() && reference(pat, &path[1..]))
        }
        (Some(p), Some(s)) => seg(p.as_bytes(), s.as_bytes()) && reference(&pat[1..], &path[1..]),
        _ => false,
    }
}
fn seg(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => seg(&p[1..], s) || (!s.is_empty() && seg(p, &s[1..])),
        (Some(b'?'), Some(_)) => seg(&p[1..], &s[1..]),
        (Some(a), Some(b)) => a == b && seg(&p[1..], &s[1..]),
        _ => false,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn glob1_matches_agrees_with_the_reference(
        pat in prop::collection::vec(prop_oneof![Just("**".to_string()), "[ab*?]{1,3}"], 1..5)
            .prop_filter("'**' only as a whole segment", |segs| segs.iter().all(|s| s == "**" || !s.contains("**"))),
        path in prop::collection::vec("[ab]{1,3}", 1..6),
    ) {
        let pattern = pat.join("/");
        let joined = path.join("/");
        let expected = reference(&pat.iter().map(String::as_str).collect::<Vec<_>>(), &path.iter().map(String::as_str).collect::<Vec<_>>());
        let got = g(&pattern).matches(&joined);
        prop_assert_eq!(got, expected, "{} vs {}", pattern, joined);
        // covers == matches on some prefix at a boundary (or the whole)
        let any_prefix = (1..=path.len()).any(|n| reference(&pat.iter().map(String::as_str).collect::<Vec<_>>(), &path[..n].iter().map(String::as_str).collect::<Vec<_>>()));
        prop_assert_eq!(g(&pattern).covers(&joined), any_prefix);
    }
}

// Guard (fix round 1): the TooLong text follows MAX_GLOB_LEN rather than repeating the number.
#[test]
fn glob1_the_too_long_text_names_the_limit_constant() {
    let e = Glob::compile(&"a".repeat(MAX_GLOB_LEN + 1)).unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "a path glob is at most {MAX_GLOB_LEN} bytes; this one is {}",
            MAX_GLOB_LEN + 1
        )
    );
}

// Guard (fix round 1): `?` is one character, not one byte, so the allocation-free ASCII scan
// must not change what a non-ASCII segment matches.
#[test]
fn glob1_question_mark_is_one_character_in_a_non_ascii_name() {
    assert!(g("caf?").matches("café"));
    assert!(!g("caf??").matches("café"));
    assert!(!g("caf*e").matches("café"));
    assert!(g("c*?").matches("café"));
    assert!(!g("cafe").matches("café"));
}
