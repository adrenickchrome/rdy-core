//! wx30a: a site on a native output root. Its links, page names and search entries are slash
//! URLs, and the root the files were written under never reaches a page, a link, an agent file or
//! the summary. On Unix these run with `/` as the separator and prove the same contract.
use rdy_engine::*;
use rdy_qa::golden::*;
use rdy_render::{Mode, build_site, render_site};
use std::collections::BTreeMap;
use std::path::Path;

/// The values of every `href`, `src` and `url=` in a page.
fn targets(page: &str) -> Vec<String> {
    let mut out = vec![];
    for key in ["href=\"", "src=\"", "url="] {
        for part in page.split(key).skip(1) {
            let end = part.find(['"', '>', ' ']).unwrap_or(part.len());
            out.push(part[..end].to_string());
        }
    }
    out
}

/// A target that names a place on a host disk: a backslash, or a drive letter.
fn host_shaped(t: &str) -> bool {
    t.contains('\\')
        || t.contains("%5C")
        || t.contains("%5c")
        || t.as_bytes().get(1..3) == Some(b":/")
        || t.starts_with("file:")
}

fn check_slash_urls(files: &BTreeMap<String, Vec<u8>>, what: &str) {
    assert!(files.contains_key("wallets/transactions.html"), "{what}");
    for (name, bytes) in files {
        assert!(
            !name.contains('\\') && !name.starts_with('/'),
            "{what}: the file name {name:?} is not a site-relative slash path"
        );
        if name.ends_with(".html") {
            let page = String::from_utf8_lossy(bytes);
            for t in targets(&page) {
                assert!(!host_shaped(&t), "{what}: {name} links to {t:?}");
            }
        }
    }
}

#[test]
fn wx30a_static_and_live_links_are_slash_urls() {
    let (_d, r, _ids) = site_with_decision();
    let st = r.read();
    let built = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    check_slash_urls(&built.files, "static");
    // the live build writes every link from its root
    let live = build_site(&st, &Scope::Canon, Mode::Live { root: "/" }).unwrap();
    check_slash_urls(&live.files, "live");
    let live_page = String::from_utf8_lossy(&live.files["wallets/transactions.html"]).into_owned();
    assert!(
        targets(&live_page).iter().any(|t| t.starts_with('/')),
        "a live link is rooted: {live_page}"
    );
    // the search index names pages by slash URL too
    let index = String::from_utf8_lossy(&built.files["search-index.json"]).into_owned();
    assert!(
        index.contains("wallets/transactions.html"),
        "the search index names the page: {index}"
    );
    assert!(!index.contains("wallets\\\\"), "{index}");
    // the same bytes reach a disk whose separator is not `/`
    let out = tempfile::tempdir().unwrap();
    render_site(&st, &Scope::Canon, out.path()).unwrap();
    let page =
        std::fs::read_to_string(out.path().join("wallets").join("transactions.html")).unwrap();
    assert_eq!(
        page.as_bytes(),
        &built.files["wallets/transactions.html"][..],
        "the page on disk is the page in memory"
    );
}

/// Every file under `dir`, by its slash path.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn wx30a_native_output_roots_do_not_leak_into_site_paths() {
    let (_d, r, _ids) = site_with_decision();
    let st = r.read();
    let base = tempfile::tempdir().unwrap();
    // a root with a space and a nested folder, the way a person types one
    let first = base.path().join("native root").join("site out");
    let second = base.path().join("other").join("site");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();
    let a = render_site(&st, &Scope::Canon, &first).unwrap();
    let b = render_site(&st, &Scope::Canon, &second).unwrap();
    let (ta, tb) = (tree(&first), tree(&second));
    assert_eq!(
        ta.keys().collect::<Vec<_>>(),
        tb.keys().collect::<Vec<_>>(),
        "the same files whatever the root"
    );
    assert_eq!(ta, tb, "the same bytes whatever the root");
    check_slash_urls(&ta, "rendered");
    // no root text, in either spelling, in any file, name or the summary
    let spellings = [
        first.to_string_lossy().into_owned(),
        first.to_string_lossy().replace('\\', "/"),
        "native root".into(),
    ];
    for (name, bytes) in &ta {
        let text = String::from_utf8_lossy(bytes);
        for root in &spellings {
            assert!(
                !text.contains(root.as_str()),
                "{name} holds the output root {root}"
            );
            assert!(!name.contains(root.as_str()), "{name}");
        }
    }
    for s in [&a, &b] {
        let json = serde_json::to_string(s).unwrap();
        for root in &spellings {
            assert!(
                !json.contains(root.as_str()),
                "the summary names {root}: {json}"
            );
        }
    }
    // nothing but the site is left beside it
    let left: Vec<_> = std::fs::read_dir(first.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["site out".to_string()], "{left:?}");
}
