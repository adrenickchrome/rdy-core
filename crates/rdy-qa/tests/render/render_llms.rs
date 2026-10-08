//! FW-D5 refined: `llms.txt` carries what a reader sees (llms1); no JSON twin (people1).
use rdy_engine::Scope;
use rdy_qa::golden::*;

fn render(r: &rdy_engine::Rdy) -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    out
}

/// Every character of text in `html` between `from` and `to`, tags dropped, entities decoded and
/// whitespace removed, so a cut line compares by its words.
fn squashed_text(html: &str, from: &str, to: &str) -> String {
    let a = html.find(from).unwrap();
    let b = html.rfind(to).unwrap();
    let tags = regex::Regex::new(r"<[^>]*>").unwrap();
    rdy_qa::site::unescape(&tags.replace_all(&html[a..b], " "))
        .split_whitespace()
        .collect()
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect()
}

#[test]
fn llms1_llms_txt_is_the_sites_contents_in_tree_order() {
    let (_d, r) = tree_site();
    let out = render(&r);
    let t = std::fs::read_to_string(out.path().join("llms.txt")).unwrap();
    assert_eq!(
        t,
        "# Tree\n\nTree, unversioned.\n\n## Pages\n\n\
         - [Intro](intro.html): Where to start.\n\
         - [Alpha](a/index.html)\n\
         \x20 - [One](a/one.html)\n\
         \x20 - [Two](a/two.html)\n\
         - [Beta](b/index.html)\n\
         \x20 - [Gamma](b/c/index.html)\n\
         \x20   - [Three](b/c/three.html)\n\
         \n## Reference\n\n\
         - [Glossary](glossary.html)\n\
         - [Open questions](questions.html)\n"
    );
}

#[test]
fn llms1_every_line_of_llms_txt_is_text_a_reader_sees() {
    let (_d, _r, _ids, out) = site_canon();
    let t = std::fs::read_to_string(out.path().join("llms.txt")).unwrap();
    for never in [
        ".json",
        "go/",
        "permalink",
        "twin",
        "Search index",
        "rendered by rdy",
    ] {
        assert!(!t.contains(never), "{never} in {t}");
    }
    let id = regex::Regex::new(r"\b[A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}\b").unwrap();
    let hash = regex::Regex::new(r"\b[0-9a-fA-F]{12,64}\b").unwrap();
    assert!(!id.is_match(&t) && !hash.is_match(&t), "{t}");
    // an inline tag is not a word break: `<code>go north</code>;` reads `go north;`, and the
    // squashed comparison below cannot see a space a reader never sees
    for line in t.lines() {
        for gap in [" ,", " .", " ;"] {
            assert!(!line.contains(gap), "{gap:?} in {line}");
        }
    }
    assert!(
        t.contains("\n> "),
        "the golden project has a description: {t}"
    );
    let home = squashed_text(&html(out.path(), "index.html"), "<body", "</body>");
    let item = regex::Regex::new(r"^ *- \[([^\]]*)\]\(([^)]*)\)(?:: (.*))?$").unwrap();
    for line in t.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(c) = item.captures(line) {
            let page = html(out.path(), &c[2]);
            let main = squashed_text(&page, "<main", "</main>");
            assert!(
                main.contains(&squash(&c[1])),
                "{line}: the title is not on {}",
                &c[2]
            );
            if let Some(blurb) = c.get(3) {
                assert!(
                    main.contains(&squash(blurb.as_str())),
                    "{line}: the blurb is not on {}",
                    &c[2]
                );
            }
        } else {
            let words = line
                .trim_start_matches("## ")
                .trim_start_matches("# ")
                .trim_start_matches("> ");
            assert!(
                home.contains(&squash(words)),
                "{line}: not on the home page"
            );
        }
    }
}

/// Review Focus 5 (the old json1 test's llms half).
#[test]
fn llms1_a_hostile_title_cannot_become_a_link_scheme() {
    let (_d, r) = hostile();
    let out = render(&r);
    let t = std::fs::read_to_string(out.path().join("llms.txt")).unwrap();
    assert!(
        !t.contains("](javascript:"),
        "a hostile title cannot become a link scheme: {t}"
    );
}

#[test]
fn people1_the_site_writes_no_json_but_the_search_index_and_links_to_none() {
    let (_d, r, _ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    let s = rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let v = serde_json::to_value(&s).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["assets", "pages", "redirects"]);
    let mut json = vec![];
    let mut stack = vec![out.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "json") {
                json.push(
                    p.strip_prefix(out.path())
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    assert_eq!(json, ["search-index.json"]);
    let href_json = regex::Regex::new(r#"href="[^"]*\.json""#).unwrap();
    for p in pages(out.path()) {
        let h = std::fs::read_to_string(&p).unwrap();
        assert!(
            !h.contains("application/json") && !href_json.is_match(&h),
            "{} links to JSON",
            p.display()
        );
    }
}

/// Review Focus 5: a twin left by an older render does not survive a new one.
#[test]
fn people1_a_rerender_over_an_old_site_leaves_no_twin() {
    let (_d, r, _ids) = decided_site();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    std::fs::write(out.path().join("ledgers.json"), "{}").unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    assert!(!out.path().join("ledgers.json").exists());
}

/// The words of a title, letters and digits only, so a title compares across both files (the
/// index drops brackets and parentheses from link text).
fn letters(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The founder's ruling (shape c): `llms-full.txt` is every page's reader text in reading order,
/// each page's title a `#` heading.
#[test]
fn llms1_llms_full_txt_is_every_pages_reader_text_in_reading_order() {
    let (_d, _r, _ids, out) = site_canon();
    let index = std::fs::read_to_string(out.path().join("llms.txt")).unwrap();
    let full = std::fs::read_to_string(out.path().join("llms-full.txt")).unwrap();
    assert!(
        full.starts_with("# Structural pattern matching\n\n> "),
        "{full}"
    );
    // the pages in the order the index lists them, each a `#` heading
    let item = regex::Regex::new(r"(?m)^ *- \[([^\]]*)\]\(([^)]*)\)").unwrap();
    let listed: Vec<String> = item
        .captures_iter(&index)
        .filter(|c| !matches!(&c[2], "glossary.html" | "questions.html"))
        .map(|c| letters(&c[1]))
        .collect();
    // a `# ` line inside a fenced code block is code, not a heading
    let mut fenced = false;
    let headings: Vec<String> = full
        .lines()
        .filter(|l| {
            let fence = l.starts_with("```");
            fenced ^= fence;
            !fenced && !fence
        })
        .filter_map(|l| l.strip_prefix("# "))
        .skip(1)
        .map(letters)
        .collect();
    assert_eq!(headings, listed);
    // a paragraph of the long doc, its references read as their labels
    for words in [
        "Every term used here is defined once, in the section called Terms.",
        "This is the normative description of the match statement.",
    ] {
        assert!(full.contains(words), "{words} not in llms-full.txt");
    }
    let spec = full
        .find("\n# The match statement and its patterns\n")
        .expect("the long doc");
    assert!(
        full[spec..].contains("\n## Summary\n"),
        "its sections are headings"
    );
    // a rationale is off the page, so off the file (FW-D10)
    assert!(!full.contains("say a shape once"), "{full}");
}

/// Neither file carries a block id, a type's name as a label or the landing hash.
#[test]
fn llms1_neither_file_carries_an_id_a_type_name_or_the_landing_hash() {
    let (_d, r, ids, out) = site_canon();
    let landings: Vec<String> = r
        .read()
        .versions()
        .unwrap()
        .into_iter()
        .map(|v| v.landing)
        .collect();
    assert!(!landings.is_empty());
    let types: std::collections::BTreeSet<String> = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .map(|b| b.block_type.clone())
        .collect();
    let id = regex::Regex::new(r"\b[A-Za-z]{1,3}-[0-9A-HJKMNP-TV-Z]{5}\b").unwrap();
    for file in ["llms.txt", "llms-full.txt"] {
        let t = std::fs::read_to_string(out.path().join(file)).unwrap();
        assert!(!id.is_match(&t), "{file}: {:?}", id.find(&t));
        for (name, b) in ids.named() {
            assert!(!t.contains(&b.to_string()), "{file}: the id of {name}");
        }
        for l in &landings {
            assert!(!t.contains(&l[..12]), "{file}: the landing hash {l}");
        }
        for ty in &types {
            for label in [
                format!("[{ty}]"),
                format!("({ty})"),
                format!("type: {ty}"),
                format!("\"{ty}\""),
            ] {
                assert!(!t.contains(&label), "{file}: the type label {label}");
            }
        }
    }
}
