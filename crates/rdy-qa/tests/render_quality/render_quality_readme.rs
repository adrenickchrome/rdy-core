//! rfr1 (D-3VVNC): closed content is reader text. A collapsed section is a `<details>` that
//! starts closed, and the leak detector reads it as it reads any other part of the page: a closed
//! fold is no place to hide an id, a status word or a file name, and the heading in its summary
//! and the prose in its body stay visible text.
//!
//! rfr2 (D-3DAX4, rule L): a figure's dark image and a remote image in block text add no way out
//! of a profile. Nothing outside the selection reaches its output through an alt text, a `src`,
//! a `srcset` or a Copy payload, and no page asks another origin for a resource.
use rdy_engine::Scope;
use rdy_qa::fixtures::{land, write};
use rdy_qa::golden::{html, pages, tiny};
use rdy_qa::site::{Zone, leaks, visible_runs};
use serde_json::json;

#[test]
fn rfr1_closed_details_are_reader_text() {
    let (_dir, mut r) = tiny();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide page", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$i", "parent": "$g", "fields": {"title": "Intro"}},
            {"op": "create", "type": "prose", "parent": "$i", "body": "Lede words."},
            {"op": "create", "type": "section", "id": "$f", "parent": "$g", "fields": {"title": "Closed fold", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$f", "body": "Plain words in the fold."},
            {"op": "create", "type": "prose", "parent": "$f", "body": "A canary id r-7KQ2M in the fold."},
            {"op": "create", "type": "prose", "parent": "$f", "body": "A canary digest 0123456789abcdef0123 in the fold."},
            {"op": "create", "type": "prose", "parent": "$f", "body": "A canary file leak-canary.yaml in the fold."},
            {"op": "create", "type": "section", "id": "$d", "parent": "$f", "fields": {"title": "draft", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$d", "body": "Words under a status word."},
            {"op": "create", "type": "section", "id": "$n", "parent": "$g", "fields": {"title": "Clean fold", "collapsed": true}},
            {"op": "create", "type": "prose", "parent": "$n", "body": "Nothing to find here."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, out.path()).unwrap();
    let page = html(out.path(), "guide.html");
    assert_eq!(
        page.matches("<details class=\"section-fold\">").count(),
        3,
        "three closed folds"
    );

    // the detector reads a closed fold as content
    let found: Vec<(String, &str)> = leaks(out.path())
        .into_iter()
        .filter(|l| l.page == "guide.html")
        .map(|l| (l.text, l.pattern))
        .collect();
    for (text, pattern) in [
        ("r-7KQ2M", "F1"),
        ("0123456789abcdef0123", "F2"),
        ("leak-canary.yaml", "F6"),
    ] {
        assert!(
            found.iter().any(|(t, p)| t.contains(text) && *p == pattern),
            "{pattern} {text} in a closed fold: {found:?}"
        );
    }
    // a status word as a fold's heading is a label the detector reads
    assert!(
        found.iter().any(|(t, p)| t == "draft" && *p == "F8"),
        "{found:?}"
    );
    // and the clean fold adds none of them
    assert!(
        !found
            .iter()
            .any(|(t, _)| t.contains("Nothing") || t.contains("Clean")),
        "{found:?}"
    );

    // the legitimate words stay visible text: the summary's heading and the body, in the content
    let runs = visible_runs(&page);
    for text in [
        "Closed fold",
        "Plain words in the fold.",
        "Clean fold",
        "Nothing to find here.",
    ] {
        let run = runs
            .iter()
            .find(|r| r.text == text)
            .unwrap_or_else(|| panic!("{text}"));
        assert_eq!(run.zone, Zone::Content, "{text}");
    }
    // the heading in a summary is a label; the body words are running text
    let by = |t: &str| runs.iter().find(|r| r.text == t).unwrap();
    assert!(by("Closed fold").label);
    assert!(by("Closed fold").path.contains(&"summary".to_string()));
    assert!(!by("Plain words in the fold.").label);
    assert!(!pages(out.path()).is_empty());
}

/// A generic picture's bytes: a PNG signature and a distinct tail.
fn picture(tail: &str) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(tail.as_bytes());
    b
}

/// Every file under `dir`.
fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// The paths of `files` whose bytes hold `needle`.
fn carries(files: &std::collections::BTreeMap<String, Vec<u8>>, needle: &[u8]) -> Vec<String> {
    files
        .iter()
        .filter(|(_, b)| b.windows(needle.len()).any(|w| w == needle))
        .map(|(p, _)| p.clone())
        .collect()
}

#[test]
fn rfr2_image_attributes_and_copy_payloads_do_not_leak() {
    use rdy_qa::site::{attrs, unescape};
    let (_dir, mut r) = tiny();
    let shown = [
        r.store_mut().put_blob(&picture("shown light")).unwrap(),
        r.store_mut().put_blob(&picture("shown dark")).unwrap(),
    ];
    let hidden_bytes = [
        picture("hidden light canary"),
        picture("hidden dark canary"),
    ];
    let hidden = [
        r.store_mut().put_blob(&hidden_bytes[0]).unwrap(),
        r.store_mut().put_blob(&hidden_bytes[1]).unwrap(),
    ];
    let fig = |parent: &str, h: &[rdy_model::Hash; 2], alt: &str| {
        json!({"op": "create", "type": "figure", "parent": parent, "fields": {
            "blob": h[0].to_string(), "media_type": "image/png",
            "dark_blob": h[1].to_string(), "dark_media_type": "image/png", "alt": alt}})
    };
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$g", "body": "A badge ![Selected badge](https://img.example.com/selected.svg) and [![Docs badge](https://img.example.com/docs.svg)](https://docs.example.com/)."},
            fig("$g", &shown, "Selected figure"),
            {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$o", "body": "Hidden ![Canary alt q7](https://canary.example.com/hidden.svg) words."},
            fig("$o", &hidden, "Hidden canary alt"),
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": w.created["$admin"],
            "fields": {"key": "readers", "title": "Readers", "audience": "org",
                "select": [w.created["$g"]]}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let st = r.read();
    let site = rdy_render::build_profile_site(&st, "canon", "readers").unwrap();
    let mut md = std::collections::BTreeMap::new();
    let p = rdy_render::print::build_profile(&st, "canon", "readers").unwrap();
    let out = tempfile::tempdir().unwrap();
    rdy_render::print::md::write_markdown(
        &p,
        &out.path().join("md"),
        rdy_render::print::md::Layout::Plain,
    )
    .unwrap();
    for path in files_under(&out.path().join("md")) {
        let rel = path
            .strip_prefix(out.path())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        md.insert(rel, std::fs::read(&path).unwrap());
    }
    for (what, files) in [("site", &site.files), ("Markdown", &md)] {
        // the profile canaries: the hidden page's alt texts, remote URL, bytes and blob names
        for canary in [
            "Canary alt q7".as_bytes(),
            b"canary.example.com",
            b"Hidden canary alt",
            &hidden_bytes[0],
            &hidden_bytes[1],
            hidden[0].to_string().as_bytes(),
            hidden[1].to_string().as_bytes(),
        ] {
            assert_eq!(
                carries(files, canary),
                Vec::<String>::new(),
                "{what}: {}",
                String::from_utf8_lossy(canary)
            );
        }
    }
    // the srcset scan reads each candidate: a remote canary is another origin, a local variant
    // is not
    use crate::render_quality::{external, srcset_urls};
    let urls = srcset_urls("https://canary.example.com/dark.png 2x, assets/dark.png 1x");
    assert_eq!(
        urls,
        ["https://canary.example.com/dark.png", "assets/dark.png"]
    );
    assert!(external(&urls[0]));
    assert!(!external(&urls[1]));
    assert!(external(
        &srcset_urls("assets/a.png, //canary.example.com/b.png 2x")[1]
    ));
    // no page asks another origin for a resource: every src and srcset is the site's own
    for (rel, bytes) in &site.files {
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        for v in attrs(text, "src") {
            let v = unescape(v);
            assert!(!external(&v), "{rel}: src={v}");
        }
        for v in attrs(text, "srcset") {
            for u in srcset_urls(&unescape(v)) {
                assert!(!external(&u), "{rel}: srcset={v}");
            }
        }
    }
    let page = String::from_utf8(site.files["guide.html"].clone()).unwrap();
    let (body, payload) = match page.split_once("<template data-copy-page hidden>") {
        Some((a, b)) => (
            a.to_string(),
            b.split("</template>").next().unwrap().to_string(),
        ),
        None => panic!("no Copy payload: {page}"),
    };
    // the Copy payload: each block's parts, joined
    let parts: serde_json::Value = serde_json::from_str(&unescape(&payload)).unwrap();
    let copy: String = parts["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| b.as_array().unwrap().iter())
        .map(|p| p["value"].as_str().unwrap().to_string())
        .collect();
    assert!(copy.contains("srcset=\"assets/"), "{copy}");
    assert!(!copy.contains("canary"), "{copy}");
    // the page shows the alt text, inside the link; the Copy payload keeps the image
    assert!(body.contains("Selected badge"), "{body}");
    assert!(!body.contains("img.example.com"), "{body}");
    assert!(
        copy.contains("![Selected badge](https://img.example.com/selected.svg)"),
        "{copy}"
    );
    assert!(
        copy.contains(
            "[![Docs badge](https://img.example.com/docs.svg)](https://docs.example.com/)"
        ),
        "{copy}"
    );
    // both images of the selected figure, each named by the site's own file and the alt text
    let imgs: Vec<&str> = body
        .match_indices("<img class=\"figure-")
        .map(|(i, _)| &body[i..i + body[i..].find('>').unwrap()])
        .collect();
    assert_eq!(imgs.len(), 2, "{body}");
    for tag in imgs {
        assert_eq!(attrs(tag, "alt"), vec!["Selected figure"], "{tag}");
        assert!(attrs(tag, "src")[0].starts_with("assets/"), "{tag}");
    }
    // the Markdown picture names the selected images' files, never another origin
    let guide = String::from_utf8(md["md/guide.md"].clone()).unwrap();
    let srcset = attrs(&guide, "srcset");
    assert_eq!(srcset.len(), 1, "{guide}");
    assert!(
        srcset_urls(srcset[0]).iter().all(|u| !external(u)),
        "{guide}"
    );
}
