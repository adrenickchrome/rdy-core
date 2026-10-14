//! ls1: the site as bytes in memory; rdy render --out writes exactly those bytes.
use rdy_engine::Scope;
use rdy_render::{Mode, SiteFiles, build_site, escape, render_site};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

const STATIC_NOSCRIPT: &str =
    "Search needs JavaScript. Everything else on this page works without it.";
const LIVE_NOSCRIPT: &str =
    "Search and live updates need JavaScript. Everything else on this page works without it.";

/// Every file under `dir`, by its path relative to `dir`, except the site marker.
fn read_tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
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
                if rel != rdy_render::SITE_MARKER {
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
    }
    out
}

#[test]
fn ls1_build_site_is_what_render_writes() {
    let (_dir, r, _ids, out) = rdy_qa::golden::site_canon();
    let built = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    let written = read_tree(out.path());
    assert_eq!(
        built.files.keys().collect::<Vec<_>>(),
        written.keys().collect::<Vec<_>>(),
        "the same files"
    );
    for (path, bytes) in &written {
        assert!(built.files[path] == *bytes, "{path} differs");
    }
    assert_eq!(built.label, "v1.0.0, 2 changes since");
}

#[test]
fn ls1_a_draft_build_is_what_render_writes() {
    let (_dir, r, _ids, out) = rdy_qa::golden::site_view();
    let built = build_site(&r.read(), &Scope::View("editing".into()), Mode::Static).unwrap();
    let written = read_tree(out.path());
    assert_eq!(built.files.len(), written.len());
    for (path, bytes) in &written {
        assert!(built.files[path] == *bytes, "{path} differs");
    }
}

#[test]
fn ls1_a_build_is_the_same_bytes_every_time() {
    // guard: the change test compares builds byte for byte
    let (_dir, r, _ids) = rdy_qa::golden::project();
    let a = build_site(&r.read(), &Scope::Canon, Mode::Live).unwrap();
    let b = build_site(&r.read(), &Scope::Canon, Mode::Live).unwrap();
    assert!(a.files == b.files);
}

/// The site's files with `label` spliced into every range `label_at` records, last range first:
/// escaped in a page, raw in the llms files (what the live site's filler does).
fn fill(site: &SiteFiles, label: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = site.files.clone();
    for (path, ranges) in &site.label_at {
        let text = if path.ends_with(".html") {
            escape(label)
        } else {
            label.to_string()
        };
        let bytes = out.get_mut(path).expect("a range names a file of the site");
        for r in ranges.iter().rev() {
            bytes.splice(r.clone(), text.bytes());
        }
    }
    out
}

/// Every page but a permalink page has two ranges (the version pill, then the colophon), the
/// llms files one each (the head line), every other file none; each holds the label as the build
/// wrote it, at its place in the markup.
fn assert_label_places(site: &SiteFiles) {
    for path in site.label_at.keys() {
        assert!(
            site.files.contains_key(path),
            "{path} is a file of the site"
        );
    }
    for (path, bytes) in &site.files {
        let ranges = site.label_at.get(path).map_or(&[][..], Vec::as_slice);
        let text = String::from_utf8_lossy(bytes);
        let llms = path == "llms.txt" || path == "llms-full.txt";
        let page = path.ends_with(".html") && !text.contains("data-rdy-kind=\"redirect\"");
        let want = if page {
            2
        } else if llms {
            1
        } else {
            0
        };
        assert_eq!(ranges.len(), want, "{path}: {ranges:?}");
        if want == 0 {
            assert!(
                !site.label_at.contains_key(path),
                "{path} has an empty entry"
            );
        }
        let label = if page {
            escape(&site.label)
        } else {
            site.label.clone()
        };
        for r in ranges {
            assert_eq!(
                &bytes[r.clone()],
                label.as_bytes(),
                "{path} {r:?} holds the label"
            );
        }
        if page {
            assert!(
                text[..ranges[0].start].ends_with("data-rdy-version>")
                    && text[ranges[0].end..].starts_with("</span>"),
                "{path}: the first range is the version pill"
            );
            let before = &text[..ranges[1].start];
            let open = before.rfind("<p class=\"colophon\">").expect("a colophon") + 20;
            assert!(
                before[open..].ends_with(", ")
                    && !before[open..].contains(['<', '>'])
                    && text[ranges[1].end..].starts_with(".</p>"),
                "{path}: the second range is the colophon's label"
            );
        }
        if llms {
            let before = &text[..ranges[0].start];
            assert!(
                before.ends_with(", ")
                    && !before[before.rfind('\n').unwrap()..].contains('#')
                    && text[ranges[0].end..].starts_with(".\n\n"),
                "{path}: the range is the head line's label"
            );
        }
    }
}

/// `live` with its noscript sentence put back to the static one.
fn unlive(path: &str, bytes: &[u8]) -> Vec<u8> {
    if path.ends_with(".html") {
        String::from_utf8_lossy(bytes)
            .replace(LIVE_NOSCRIPT, STATIC_NOSCRIPT)
            .into_bytes()
    } else {
        bytes.to_vec()
    }
}

#[test]
fn ls1_a_live_build_filled_is_the_static_build() {
    let (_dir, r, _ids) = rdy_qa::golden::project();
    let st = r.read();
    let stat = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    let live = build_site(&st, &Scope::Canon, Mode::Live).unwrap();
    assert_eq!(
        stat.files.keys().collect::<Vec<_>>(),
        live.files.keys().collect::<Vec<_>>()
    );
    assert_eq!(live.label, stat.label);
    assert_eq!(
        live.label_at, stat.label_at,
        "the label sits at the same bytes"
    );
    assert_label_places(&stat);
    assert_label_places(&live);
    // the live build is the static build but for its noscript sentence
    for (path, bytes) in &live.files {
        assert!(unlive(path, bytes) == stat.files[path], "{path}");
    }
    // filled with another label, the two builds still agree
    let other = "v9.9.9 <&> \u{E002}, 1 change since";
    let (a, b) = (fill(&live, other), fill(&stat, other));
    for (path, bytes) in &a {
        assert!(unlive(path, bytes) == b[path], "{path}");
    }
    let home = String::from_utf8_lossy(&a["index.html"]);
    assert!(home.contains(&escape(other)) && home.contains(LIVE_NOSCRIPT));
    assert!(
        !home.contains(&format!(">{}<", escape(&live.label))),
        "the old label is gone"
    );
    let llms = String::from_utf8_lossy(&a["llms.txt"]);
    assert!(llms.contains(&format!(", {other}.\n")), "{llms}");
}

#[test]
fn ls1_a_label_lookalike_in_content_is_never_filled() {
    // U+E002 is a Nerd Font pomicon that pasted terminal text carries; the label's own text can
    // be in a doc too. Neither is a place the label goes.
    let (_dir, mut r) = rdy_qa::golden::tiny();
    let png = b"\x89PNG \xEE\x80\x82 unversioned".to_vec();
    let blob = r.store_mut().put_blob(&png).unwrap().to_string();
    rdy_qa::fixtures::write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "prompt-icons", "title": "Prompt \u{E002} unversioned icons", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Marks"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "The pomicon \u{E002} marks a task, unversioned."},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "image/png", "alt": "A \u{E002} mark"}}
    ]))
    .unwrap();
    rdy_qa::fixtures::land(&mut r, "work").unwrap();
    let st = r.read();
    let stat = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    let live = build_site(&st, &Scope::Canon, Mode::Live).unwrap();
    assert_eq!(
        stat.label, "unversioned",
        "the lookalike is the label's own text"
    );
    assert_label_places(&stat);
    assert_label_places(&live);
    for (path, bytes) in &live.files {
        assert!(unlive(path, bytes) == stat.files[path], "{path}");
    }
    let other = "v2.0.0, 1 change since";
    let (a, b) = (fill(&live, other), fill(&stat, other));
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (path, bytes) in &a {
        assert!(unlive(path, bytes) == b[path], "{path}");
    }
    // the lookalikes are untouched, the label is replaced
    let page = String::from_utf8_lossy(&a["prompt-icons.html"]);
    assert!(page.contains("Prompt \u{E002} unversioned icons"), "{page}");
    assert!(
        page.contains("The pomicon \u{E002} marks a task, unversioned."),
        "{page}"
    );
    assert_eq!(page.matches(other).count(), 2, "{page}");
    for f in ["llms.txt", "llms-full.txt"] {
        let text = String::from_utf8_lossy(&a[f]);
        assert!(
            text.contains("Prompt \u{E002} unversioned icons"),
            "{f}: {text}"
        );
        assert!(text.contains(&format!("Tiny, {other}.\n")), "{f}: {text}");
        assert!(!text.contains("Tiny, unversioned."), "{f}: {text}");
    }
    let blobs: Vec<_> = a.keys().filter(|p| p.ends_with(".png")).collect();
    assert_eq!(blobs.len(), 1, "{blobs:?}");
    assert_eq!(a[blobs[0]], png, "the blob is its bytes");
    assert!(a["search-index.json"] == live.files["search-index.json"]);
    let index = String::from_utf8_lossy(&a["search-index.json"]);
    assert!(
        index.contains("\u{E002}") && index.contains("unversioned"),
        "{index}"
    );
    let go: Vec<_> = a.keys().filter(|p| p.starts_with("go/")).collect();
    assert!(!go.is_empty());
    for p in go {
        assert!(
            a[p] == live.files[p],
            "{p}: a permalink page is never filled"
        );
    }
}

#[test]
fn ls1_an_in_flight_scope_is_still_refused() {
    // guard: render_site keeps its refusal
    let (_dir, r, _ids) = rdy_qa::golden::project();
    let out = tempfile::tempdir().unwrap();
    let e = render_site(&r.read(), &Scope::Inflight, out.path()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
}
