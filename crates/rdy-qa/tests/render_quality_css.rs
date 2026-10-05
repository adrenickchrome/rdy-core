//! The stylesheet's token structure and contrast, and the static no-horizontal-scroll checks
//! (css1, scroll1).
use rdy_qa::{golden, site::*};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;

fn stylesheet(site: &Path) -> Css {
    let p = site.join("assets/site.css");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    parse_css(&text)
}

fn canon_css() -> Css {
    let (_d, _r, _ids, out) = golden::site_canon();
    stylesheet(out.path())
}

/// Whether the block is one of the three token blocks `tokens()` reads.
fn is_token_block(b: &CssBlock) -> bool {
    let dark = b
        .media
        .as_deref()
        .is_some_and(|m| m.replace(' ', "").contains("prefers-color-scheme:dark"));
    match (b.selector.as_str(), b.media.is_some()) {
        (":root", false) => true,
        (r#":root:not([data-theme="light"])"#, true) => dark,
        (r#":root[data-theme="dark"]"#, false) => true,
        _ => false,
    }
}

#[test]
fn css1_three_token_blocks_identical_and_no_literal_elsewhere() {
    let css = canon_css();
    let t = tokens(&css).expect("the token blocks parse");
    assert!(!t.light.is_empty(), "no light tokens");
    for (map, name) in [
        (&t.dark_media, "dark media"),
        (&t.dark_attr, "dark attribute"),
    ] {
        for k in map.keys() {
            assert!(
                t.light.contains_key(k),
                "{name} block names {k}, which the light block lacks"
            );
        }
    }
    for (k, v) in &t.light {
        if !colour_literals(v).is_empty() {
            assert!(
                t.dark_media.contains_key(k),
                "{k} holds a colour in light and is missing from the dark media block"
            );
            assert!(
                t.dark_attr.contains_key(k),
                "{k} holds a colour in light and is missing from the dark attribute block"
            );
        }
    }
    assert_eq!(t.dark_media, t.dark_attr, "the two dark blocks differ");
    let mut stray = vec![];
    for b in css.blocks.iter().filter(|b| !is_token_block(b)) {
        for (prop, value) in &b.decls {
            for lit in colour_literals(value) {
                stray.push(format!("{} {{ {prop}: {value} }} has {lit}", b.selector));
            }
        }
    }
    assert!(
        stray.is_empty(),
        "colour literals outside the token blocks:\n  {}",
        stray.join("\n  ")
    );
}

/// The theme a map describes: the light tokens with the map's own values over them.
fn theme(
    light: &BTreeMap<String, String>,
    over: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut t = light.clone();
    t.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
    t
}

#[test]
fn css1_every_declared_pair_meets_its_floor_in_both_themes() {
    let t = tokens(&canon_css()).expect("the token blocks parse");
    assert!(!t.pairs.is_empty(), "no --pair-* is declared");
    let empty = BTreeMap::new();
    let themes = [
        ("light", theme(&t.light, &empty)),
        ("dark (media)", theme(&t.light, &t.dark_media)),
        ("dark (attribute)", theme(&t.light, &t.dark_attr)),
    ];
    let mut failures = vec![];
    for (name, map) in &themes {
        for p in &t.pairs {
            let floor = match p.kind {
                PairKind::Text => 4.5,
                PairKind::Ui => 3.0,
            };
            let colour = |token: &str| {
                resolve(map, token)
                    .and_then(|v| srgb(&v))
                    .ok_or_else(|| format!("{name}: {} cannot resolve {token}", p.name))
            };
            match (colour(&p.fg), colour(&p.bg)) {
                (Ok(fg), Ok(bg)) => {
                    let ratio = contrast(fg, bg);
                    if ratio < floor {
                        failures.push(format!(
                            "{name}: {} {} on {} is {ratio:.2}:1, floor {floor}",
                            p.name, p.fg, p.bg
                        ));
                    }
                }
                (a, b) => failures.extend(a.err().into_iter().chain(b.err())),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn css1_the_required_pairs_are_declared() {
    let t = tokens(&canon_css()).expect("the token blocks parse");
    let declared: Vec<&str> = t.pairs.iter().map(|p| p.name.as_str()).collect();
    for want in [
        "body",
        "soft",
        "faint",
        "faint-surface",
        "link",
        "link-note",
        "ink-note",
        "soft-note",
        "accent-on-soft",
        "soft-surface",
        "ink-mark",
        "tk-k",
        "tk-s",
        "tk-c",
        "tk-n",
        "tk-f",
        "tk-t",
        "tk-d",
        "focus",
        "line-ui",
        "brand-glyph",
    ] {
        assert!(
            declared.contains(&format!("--pair-{want}").as_str()),
            "--pair-{want} is not declared (have {declared:?})"
        );
    }
}

// --------------------------------------------------------------------------- scroll

/// Every block whose selector list has `part` as one of its comma-separated selectors.
fn rules_for<'a>(css: &'a Css, part: &'a str) -> impl Iterator<Item = &'a CssBlock> {
    css.blocks.iter().filter(move |b| {
        b.selector
            .split(',')
            .any(|s| s.trim().replace('\'', "\"") == part)
    })
}

fn declares(css: &Css, part: &str, prop: &str, values: &[&str]) -> bool {
    rules_for(css, part).any(|b| {
        b.decls
            .iter()
            .any(|(p, v)| p == prop && values.contains(&v.replace(" !important", "").trim()))
    })
}

/// Task 5 handoff: the search input's border is a UI boundary, so it uses `--line-ui` (3:1 on
/// the page), not the decorative `--line` (about 1.2:1). `var(--line)` is not a literal, so css1
/// cannot catch it. Task 7 writes the `#search` rule inside the `search` section; this test is
/// owned by Task 7 and its ignore is removed there.
#[test]
fn css1_the_search_input_border_uses_the_ui_line_and_accent_on_focus() {
    let css = canon_css();
    assert!(
        declares(&css, "#search", "border", &["1px solid var(--line-ui)"]),
        "#search must declare `border: 1px solid var(--line-ui)`"
    );
    assert!(
        declares(&css, "#search:focus", "border-color", &["var(--accent)"]),
        "#search:focus must declare `border-color: var(--accent)`"
    );
    assert!(
        !rules_for(&css, "#search").any(|b| b
            .decls
            .iter()
            .any(|(_, v)| v.contains("var(--line)") || v.contains("var(--line-strong)"))),
        "#search must not take its border from --line"
    );
}

#[test]
fn scroll1_no_fixed_width_wider_than_the_phone() {
    let wide: Vec<_> = fixed_widths(&canon_css())
        .into_iter()
        .filter(|(_, _, px)| *px > 344)
        .collect();
    assert!(wide.is_empty(), "width or min-width over 344px: {wide:?}");
}

#[test]
fn scroll1_content_never_refuses_to_wrap() {
    let css = canon_css();
    let content = Regex::new(r"content|page|prose|(^|[\s>+~,])p($|[\s.:#\[>+~,])").unwrap();
    for b in &css.blocks {
        if content.is_match(&b.selector) {
            assert!(
                !b.decls
                    .iter()
                    .any(|(p, v)| p == "white-space" && v.trim() == "nowrap"),
                "{}: white-space: nowrap",
                b.selector
            );
        }
    }
}

#[test]
fn scroll1_the_stylesheet_lets_wide_things_shrink_or_scroll() {
    let css = canon_css();
    for tag in ["img", "svg", "video"] {
        assert!(
            declares(&css, tag, "max-width", &["100%"]),
            "{tag} lacks max-width: 100%"
        );
        assert!(
            declares(&css, tag, "height", &["auto"]),
            "{tag} lacks height: auto"
        );
    }
    let pre = css.blocks.iter().any(|b| {
        b.selector.split(',').any(|s| {
            let s = s.trim();
            s == "pre" || s.starts_with("pre.") || s.starts_with("pre:")
        }) && b
            .decls
            .iter()
            .any(|(p, v)| (p == "overflow-x" || p == "overflow") && v == "auto")
    });
    assert!(pre, "no rule gives pre overflow-x: auto");
    assert!(
        declares(
            &css,
            r#"[data-rdy="content"]"#,
            "overflow-wrap",
            &["anywhere"]
        ),
        "[data-rdy=\"content\"] lacks overflow-wrap: anywhere"
    );
}

#[test]
fn scroll1_every_page_has_the_exact_viewport_meta() {
    let (_d, _r, _ids, out) = golden::site_canon();
    for p in golden::pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        assert!(
            h.contains("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"),
            "{}",
            p.display()
        );
    }
}

#[test]
fn scroll1_tables_are_wrapped_and_pre_has_a_class() {
    let wrapped = Regex::new(r#"<[a-z]+ [^>]*class="[^"]*\btable-wrap\b[^"]*"[^>]*>\s*$"#).unwrap();
    let (_d, _r, _ids, out) = golden::site_canon();
    let mut tables = 0;
    for p in golden::pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        for (i, _) in h.match_indices("<table") {
            tables += 1;
            assert!(
                wrapped.is_match(&h[..i]),
                "{}: a <table> outside .table-wrap",
                p.display()
            );
        }
        for (i, _) in h.match_indices("<pre") {
            let tag = &h[i..i + h[i..].find('>').unwrap()];
            let classes: Vec<&str> = attrs(tag, "class")
                .first()
                .map_or(vec![], |c| c.split_ascii_whitespace().collect());
            assert!(
                classes.contains(&"code") || classes.contains(&"mermaid"),
                "{}: {tag}",
                p.display()
            );
        }
    }
    assert!(tables > 0, "the golden site has no table");
}

#[test]
fn scroll1_no_unbreakable_token_longer_than_60_characters_outside_code() {
    let (_d, _r, _ids, out) = golden::site_canon();
    // the long doc is also the wide-table page; it holds a 40-odd character identifier
    let h = golden::html(out.path(), "specification/patterns.html");
    let mut longest = 0;
    for run in visible_runs(&h) {
        for token in run.text.split_ascii_whitespace() {
            let n = token.chars().count();
            longest = longest.max(n);
            assert!(n <= 60, "a {n}-character token without a space: {token}");
        }
    }
    assert!(
        longest >= 40,
        "the long identifier is missing (longest token {longest})"
    );
}
