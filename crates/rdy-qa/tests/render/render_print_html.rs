//! ec7 (D-6SANB, D-X61DT): the print as one HTML file. It opens offline, holds every doc in tree
//! order, and has no script, form, button or link to a file or a site (FW-RV3a).
use rdy_engine::{E_ALREADY_EXISTS, E_BAD_INPUT, Rdy, Scope};
use rdy_qa::golden::*;
use rdy_render::print::{self, Href, PBlock, PageKind, Print, Run};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn canon(r: &Rdy) -> Print {
    print::build(&r.read(), &Scope::Canon).expect("print canon")
}

fn view(r: &Rdy) -> Print {
    print::build(&r.read(), &Scope::View("editing".into())).expect("print the view")
}

/// A page's id in the file: its path without `.html`.
fn page_id(page: &str) -> &str {
    page.strip_suffix(".html").unwrap_or(page)
}

fn single(p: &Print) -> String {
    String::from_utf8(print::single::single_html(p)).expect("the file is UTF-8")
}

/// The values of every `name="…"` attribute in `html`.
fn attr_values<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let key = format!(" {name}=\"");
    let mut out = vec![];
    let mut rest = html;
    while let Some(i) = rest.find(&key) {
        let from = i + key.len();
        let len = rest[from..].find('"').expect("an attribute is closed");
        out.push(&rest[from..from + len]);
        rest = &rest[from + len..];
    }
    out
}

/// `html` less each `data:` URI, which holds fonts and images.
fn without_data(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(i) = rest.find("data:") {
        out.push_str(&rest[..i]);
        let end = rest[i..]
            .find(['"', ')', '\''])
            .map_or(rest.len(), |e| i + e);
        out.push_str("data:");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = vec![];
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes().take_while(|c| *c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("not base64: {c}"),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// Every run of `blocks`, in order.
fn runs_of<'a>(blocks: &'a [PBlock], out: &mut Vec<&'a Run>) {
    for b in blocks {
        match b {
            PBlock::Heading { runs, .. } | PBlock::Para { runs, .. } => out.extend(runs),
            PBlock::List { items, .. } => items.iter().for_each(|i| runs_of(i, out)),
            PBlock::Table { head, rows, .. } => {
                for cell in head.iter().chain(rows.iter().flatten()) {
                    out.extend(cell);
                }
            }
            PBlock::Figure { caption, .. } => out.extend(caption),
            PBlock::Callout { blocks, .. } => runs_of(blocks, out),
            PBlock::Code { .. } => {}
        }
    }
}

fn figure_count(blocks: &[PBlock]) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            PBlock::Figure { blob: Some(_), .. } => 1,
            PBlock::List { items, .. } => items.iter().map(|i| figure_count(i)).sum(),
            PBlock::Callout { blocks, .. } => figure_count(blocks),
            _ => 0,
        })
        .sum()
}

#[test]
fn ec7_one_file_holds_every_doc_in_tree_order() {
    let (_d, r, _ids) = project();
    let p = canon(&r);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("p.html");
    let summary = print::single::write_single_html(&p, &out).expect("write the file");
    let bytes = std::fs::read(&out).unwrap();
    let html = String::from_utf8(bytes.clone()).unwrap();
    assert!(html.starts_with("<!doctype html>"), "{}", &html[..40]);
    assert!(html.contains("<html lang=\"en\">"));
    assert!(html.contains("<style>"));
    assert_eq!(bytes, print::single::single_html(&p));

    let expected: Vec<&str> = p.docs.iter().map(|d| page_id(&d.page)).collect();
    assert!(expected.len() > 60, "the corpus and its appendix");
    // one section per print doc, in tree order, then the glossary
    let sections: Vec<&str> = html
        .split("<section id=\"")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    assert_eq!(sections, expected);
    assert_eq!(*sections.last().unwrap(), "glossary");
    assert!(sections.iter().any(|s| s.contains('/')), "a nested page");
    // the contents list: one link per doc, in the same order, nested by depth
    let nav = &html[html.find("<nav").unwrap()..html.find("</nav>").unwrap()];
    let listed: Vec<&str> = nav
        .split("<li><a href=\"#")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    assert_eq!(listed, expected);
    let mut depth = 0usize;
    let mut max = 0usize;
    for tag in nav.split('<').skip(1) {
        if tag.starts_with("ul") {
            depth += 1;
            max = max.max(depth);
        } else if tag.starts_with("/ul") {
            depth -= 1;
        }
    }
    assert_eq!(depth, 0, "the lists are balanced");
    assert!(max >= 3, "nested by depth: {max}");
    assert_eq!(nav.matches("<ul").count(), nav.matches("</ul>").count());
    // the title part
    assert!(html.contains(&format!("<h1>{}</h1>", p.title)));
    // the summary
    assert_eq!(summary.format, "html");
    assert_eq!(summary.bytes, bytes.len() as u64);
    assert_eq!(summary.out, out.display().to_string());
    assert_eq!(
        summary.docs,
        p.docs
            .iter()
            .filter(|d| matches!(d.kind, PageKind::Doc))
            .count()
    );
    assert_eq!(
        summary.folders,
        p.docs
            .iter()
            .filter(|d| matches!(d.kind, PageKind::Folder))
            .count()
    );
    assert_eq!(summary.assets, p.blobs.len());
    assert_eq!(summary.pages, None);
    assert_eq!(summary.warnings, p.warnings);
}

#[test]
fn ec7_every_internal_href_names_an_id_in_the_file() {
    let (_d, r, _ids) = project();
    for p in [canon(&r), view(&r)] {
        let html = single(&p);
        let ids = attr_values(&html, "id");
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "no id twice");
        let hrefs: Vec<&str> = attr_values(&html, "href")
            .into_iter()
            .filter(|h| h.starts_with('#'))
            .collect();
        let with_anchor = hrefs.iter().filter(|h| h.contains(':')).count();
        assert!(
            hrefs.len() > 60,
            "the contents and the links: {}",
            hrefs.len()
        );
        assert!(with_anchor > 10, "links to anchors: {with_anchor}");
        for h in &hrefs {
            assert!(unique.contains(&h[1..]), "{h} names no id");
        }
        // an id only in the characters of a name
        for id in &ids {
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-:/".contains(c)),
                "{id}"
            );
        }
        // every authored page link of the print is one of the hrefs, with the id its page and
        // anchor give
        let mut n = 0;
        for d in &p.docs {
            let mut runs = vec![];
            runs_of(&d.blocks, &mut runs);
            for run in runs {
                if let Some(Href::Page { page, anchor }) = &run.link {
                    let want = match anchor {
                        Some(a) => format!("#{}:{a}", page_id(page)),
                        None => format!("#{}", page_id(page)),
                    };
                    assert!(hrefs.contains(&want.as_str()), "{want}");
                    n += 1;
                }
            }
        }
        assert!(n > 10, "the corpus links to pages: {n}");
    }
}

#[test]
fn ec7_figures_are_data_uris_and_no_url_leaves_the_file() {
    let (_d, mut r, ids) = project();
    // an author's link out, with characters an attribute must escape
    write(
        &mut r,
        "out",
        json!([{"op": "create", "type": "prose", "parent": ids.guards_section.to_string(),
            "body": "See [the paper](https://example.org/p?a=1&b=2)."}]),
    )
    .expect("a link out");
    land(&mut r, "out").expect("land it");
    let p = canon(&r);
    let html = single(&p);
    let figures: usize = p.docs.iter().map(|d| figure_count(&d.blocks)).sum();
    assert!(figures > 0, "the corpus has a figure");
    assert_eq!(html.matches("<img ").count(), figures);
    let blobs: BTreeSet<Vec<u8>> = p.blobs.values().map(|b| b.bytes.clone()).collect();
    for src in attr_values(&html, "src") {
        let b64 = src
            .strip_prefix("data:image/png;base64,")
            .unwrap_or_else(|| panic!("a figure is a data URI: {}", &src[..src.len().min(40)]));
        assert!(blobs.contains(&base64_decode(b64)), "the blob's bytes");
    }
    // the two fonts, as data URIs
    assert_eq!(html.matches("data:font/woff2;base64,").count(), 2);
    let bare = without_data(&html);
    // no `url(` but a data URI, no import, no other source of a file
    assert_eq!(
        bare.matches("url(").count(),
        bare.matches("url(data:").count()
    );
    assert!(!bare.contains("@import"));
    assert!(!bare.contains("srcset"));
    // a URL in the file is an author's external link, and only in an href
    let external: BTreeSet<String> = p
        .docs
        .iter()
        .flat_map(|d| {
            let mut runs = vec![];
            runs_of(&d.blocks, &mut runs);
            runs.into_iter()
                .filter_map(|r| match &r.link {
                    Some(Href::External(u)) => Some(u.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        external.contains("https://example.org/p?a=1&b=2"),
        "{external:?}"
    );
    assert!(html.contains("href=\"https://example.org/p?a=1&amp;b=2\""));
    let mut shown = 0;
    for scheme in ["http://", "https://", "//"] {
        for (i, _) in bare.match_indices(scheme) {
            if scheme == "//" && (bare[..i].ends_with(':') || bare[..i].ends_with("//")) {
                continue;
            }
            let from = bare[..i].rfind(" href=\"");
            let in_href = from.is_some_and(|f| !bare[f + 7..i].contains('"'));
            assert!(
                in_href,
                "a URL outside an href: {}",
                &bare[i.saturating_sub(30)..i + 30]
            );
            shown += 1;
        }
    }
    assert!(shown > 0);
    for u in attr_values(&bare, "href")
        .into_iter()
        .filter(|h| !h.starts_with('#'))
    {
        assert!(external.contains(&unescape_attr(u)), "{u}");
    }
}

fn unescape_attr(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[test]
fn ec7_the_file_has_no_script_form_or_button() {
    let event = regex::Regex::new(r#"<[^>]* on[a-z]+\s*="#).unwrap();
    let (_d, r, _ids) = project();
    for p in [canon(&r), view(&r)] {
        let html = single(&p).to_ascii_lowercase();
        let bare = without_data(&html);
        for tag in [
            "<script",
            "<link",
            "<form",
            "<button",
            "<input",
            "<select",
            "<textarea",
            "<iframe",
            "<object",
            "<embed",
            "<video",
            "<audio",
            "<base",
            "<noscript",
        ] {
            assert!(!bare.contains(tag), "{tag}");
        }
        assert!(!bare.contains("javascript:"));
        assert!(!event.is_match(&bare), "no event attribute");
        // the page asks nothing of a reader: no review mark, no navigation, no search
        for word in [
            "data-review",
            "class=\"review",
            "type=\"search\"",
            "class=\"pill",
        ] {
            assert!(!bare.contains(word), "{word}");
        }
    }
}

#[test]
fn ec7_ids_never_show_in_text() {
    let (_d, r, _ids) = project();
    for scope in [Scope::Canon, Scope::View("editing".into())] {
        let p = print::build(&r.read(), &scope).unwrap();
        let html = without_data(&single(&p));
        let m = r.read().materialized(&scope).unwrap();
        let ids: Vec<String> = m.ids().map(|id| id.to_string()).collect();
        assert!(ids.len() > 100);
        for id in &ids {
            assert!(
                !html.contains(id.as_str()),
                "{scope:?}: {id} shows in the file"
            );
        }
        // the ids of the file are names: a page's path and a block's anchor
        let pages: BTreeMap<&str, &str> = p
            .docs
            .iter()
            .map(|d| (page_id(&d.page), d.page.as_str()))
            .collect();
        for id in attr_values(&html, "id") {
            let page = id.split(':').next().unwrap();
            assert!(pages.contains_key(page), "{id}");
        }
    }
}

#[test]
fn ec7_an_existing_out_is_refused() {
    let (_d, r, _ids) = project();
    let p = canon(&r);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("p.html");
    std::fs::write(&out, b"mine").unwrap();
    let e = print::single::write_single_html(&p, &out).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS);
    assert_eq!(
        e.message,
        format!(
            "{} exists; rdy export never overwrites a file",
            out.display()
        )
    );
    assert_eq!(e.fix, "pick another path, or remove the file");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        b"mine",
        "the file is unchanged"
    );
    // nothing else is left in the directory
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    // a directory is no file to place either
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let e = print::single::write_single_html(&p, &sub).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS);
    // a directory that does not exist
    let e = print::single::write_single_html(&p, &dir.path().join("none/p.html")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    // then the file is placed whole where nothing was
    let free = dir.path().join("free.html");
    print::single::write_single_html(&p, &free).unwrap();
    assert_eq!(
        std::fs::read(&free).unwrap(),
        print::single::single_html(&p)
    );
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.contains(".partial-")), "{names:?}");
}

#[test]
fn ec7_two_exports_are_the_same_bytes() {
    let (_d, r, _ids) = project();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.html"), dir.path().join("b.html"));
    print::single::write_single_html(&canon(&r), &a).unwrap();
    print::single::write_single_html(&canon(&r), &b).unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    let (c, d2) = (dir.path().join("c.html"), dir.path().join("d.html"));
    print::single::write_single_html(&view(&r), &c).unwrap();
    print::single::write_single_html(&view(&r), &d2).unwrap();
    assert_eq!(std::fs::read(&c).unwrap(), std::fs::read(&d2).unwrap());
}

/// What `dir` holds, sorted: a refused export leaves no destination and no partial file.
fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// O29: a file in the project's `.rdy` directory can break the store, so `out` there is refused.
fn assert_inside_rdy(p: &Print, out: &std::path::Path, watched: &[&std::path::Path]) {
    let before: Vec<Vec<String>> = watched.iter().map(|d| names_in(d)).collect();
    let e = print::single::write_single_html(p, out).expect_err("inside .rdy");
    assert_eq!(e.code, E_BAD_INPUT, "{}", e.message);
    assert_eq!(
        e.message,
        format!("{} is inside the project .rdy directory", out.display())
    );
    assert_eq!(e.fix, "export to a path outside .rdy");
    assert!(
        std::fs::symlink_metadata(out).is_err(),
        "no destination file"
    );
    let after: Vec<Vec<String>> = watched.iter().map(|d| names_in(d)).collect();
    assert_eq!(before, after, "no partial file is left");
}

#[test]
fn ec7_an_out_inside_the_project_rdy_directory_is_refused() {
    let (d, r, _ids) = project();
    let p = canon(&r);
    let rdy = d.path().join(".rdy");
    let sub = rdy.join("packs");
    std::fs::create_dir_all(&sub).unwrap();
    // a direct descendant, and one deeper
    assert_inside_rdy(&p, &rdy.join("export.html"), &[&rdy, &sub]);
    assert_inside_rdy(&p, &sub.join("export.yaml"), &[&rdy, &sub]);
    // a path that reaches `.rdy` through `..`
    let other = d.path().join("other");
    std::fs::create_dir(&other).unwrap();
    assert_inside_rdy(
        &p,
        &other.join("..").join(".rdy").join("alias.html"),
        &[&rdy, &sub],
    );
}

#[test]
fn ec7_a_symlink_into_the_project_rdy_directory_is_refused() {
    let (d, r, _ids) = project();
    let p = canon(&r);
    let rdy = d.path().join(".rdy");
    let outside = tempfile::tempdir().unwrap();
    let link = outside.path().join("store");
    rdy_qa::platform::symlink(&rdy, &link).unwrap();
    assert_inside_rdy(&p, &link.join("export.html"), &[&rdy]);
}

/// O29 is containment in the project's `.rdy` directory, not a reserved name: a file or a
/// directory called `.rdy` elsewhere is an ordinary path.
#[test]
fn ec7_a_path_named_rdy_outside_the_project_rdy_directory_is_exported() {
    let (_d, r, _ids) = project();
    let p = canon(&r);
    let outside = tempfile::tempdir().unwrap();
    // an `out` named `.rdy`
    let named = outside.path().join(".rdy");
    let sum = print::single::write_single_html(&p, &named).expect("a file named .rdy elsewhere");
    assert_eq!(sum.bytes, std::fs::read(&named).unwrap().len() as u64);
    // an `out` under a directory named `.rdy`
    let dir = outside.path().join("a").join(".rdy");
    std::fs::create_dir_all(&dir).unwrap();
    let under = dir.join("export.html");
    print::single::write_single_html(&p, &under).expect("a directory named .rdy elsewhere");
    assert_eq!(
        std::fs::read(&under).unwrap(),
        print::single::single_html(&p)
    );
}

/// The project's `.rdy` may be a symlink to a directory whose own path has no `.rdy` in it. The
/// refusal compares resolved paths: the symlink and the physical path both name the store.
#[test]
fn ec7_a_symlinked_project_rdy_directory_is_refused_by_either_path() {
    let (d, r, _ids) = project();
    let p = canon(&r);
    drop(r);
    let storage = tempfile::tempdir().unwrap();
    let state = storage.path().join("state");
    let rdy = d.path().join(".rdy");
    std::fs::rename(&rdy, &state).unwrap();
    rdy_qa::platform::symlink(&state, &rdy).unwrap();
    let packs = state.join("packs");
    std::fs::create_dir_all(&packs).unwrap();
    let alias = storage.path().join("alias");
    rdy_qa::platform::symlink(&state, &alias).unwrap();
    // through the project's symlink, through the physical path, through another alias of it
    assert_inside_rdy(&p, &rdy.join("export.html"), &[&state]);
    assert_inside_rdy(
        &p,
        &rdy.join("packs").join("export.yaml"),
        &[&state, &packs],
    );
    assert_inside_rdy(&p, &state.join("export.html"), &[&state]);
    assert_inside_rdy(&p, &packs.join("export.yaml"), &[&state, &packs]);
    assert_inside_rdy(&p, &alias.join("export.html"), &[&state]);
    // a sibling of the store, in the same storage directory, is outside it
    let ok = storage.path().join("export.html");
    print::single::write_single_html(&p, &ok).expect("a sibling of the store");
}
