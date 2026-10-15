//! ls1: the site as bytes in memory; rdy render --out writes exactly those bytes.
use rdy_engine::Scope;
use rdy_render::{Mode, SiteFiles, SlotKind, build_site, escape, render_site};
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
    assert_eq!(
        built.header, "",
        "a static build writes its header per page root"
    );
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
    let a = build_site(&r.read(), &Scope::Canon, LIVE).unwrap();
    let b = build_site(&r.read(), &Scope::Canon, LIVE).unwrap();
    assert!(a.files == b.files);
}

/// A live build of canon, served at the site's root.
const LIVE: Mode<'static> = Mode::Live { root: "/" };

/// A header to fill in: any markup the filler is handed.
const HEADER: &str = "<a class=\"pill\" href=\"/~versions/\">v9.9.9 &lt;&amp;&gt;</a>";

/// The site's files with `label` and `header` spliced into every range `label_at` records, last
/// range first: the label escaped in a page and raw in the llms files, the header as it is (what
/// the live site's filler does).
fn fill(site: &SiteFiles, label: &str, header: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = site.files.clone();
    for (path, ranges) in &site.label_at {
        let text = if path.ends_with(".html") {
            escape(label)
        } else {
            label.to_string()
        };
        let bytes = out.get_mut(path).expect("a range names a file of the site");
        for (kind, r) in ranges.iter().rev() {
            let with = match kind {
                SlotKind::Label => text.as_str(),
                SlotKind::Header => header,
            };
            bytes.splice(r.clone(), with.bytes());
        }
    }
    out
}

/// Every page but a permalink page has two ranges (the header, then the colophon's label), the
/// llms files one each (the head line's label), every other file none; each holds what the build
/// wrote, at its place in the markup: the label as the build wrote it, the header as `header`
/// when one is given (a live build: the same on every page).
fn assert_label_places(site: &SiteFiles, header: Option<&str>) {
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
        let want: &[SlotKind] = if page {
            &[SlotKind::Header, SlotKind::Label]
        } else if llms {
            &[SlotKind::Label]
        } else {
            &[]
        };
        let kinds: Vec<SlotKind> = ranges.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, want, "{path}: {ranges:?}");
        if want.is_empty() {
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
        for (kind, r) in ranges {
            match (kind, header) {
                (SlotKind::Label, _) => {
                    assert_eq!(
                        &bytes[r.clone()],
                        label.as_bytes(),
                        "{path} {r:?} holds the label"
                    )
                }
                (SlotKind::Header, Some(h)) => {
                    assert_eq!(
                        &bytes[r.clone()],
                        h.as_bytes(),
                        "{path} {r:?} holds the header"
                    )
                }
                (SlotKind::Header, None) => assert!(
                    text[r.clone()].starts_with("<a class=\"pill\""),
                    "{path} {r:?} holds a header"
                ),
            }
        }
        if page {
            let h = &ranges[0].1;
            assert!(
                text[..h.start].ends_with("data-rdy-version>")
                    && text[h.end..].starts_with("</span>\n"),
                "{path}: the first range is the header"
            );
            let l = &ranges[1].1;
            let before = &text[..l.start];
            let open = before.rfind("<p class=\"colophon\">").expect("a colophon") + 20;
            assert!(
                before[open..].ends_with(", <span data-rdy-label>")
                    && !before[open..before.len() - 21].contains(['<', '>'])
                    && text[l.end..].starts_with("</span>.</p>"),
                "{path}: the second range is the colophon's label"
            );
        }
        if llms {
            let before = &text[..ranges[0].1.start];
            assert!(
                before.ends_with(", ")
                    && !before[before.rfind('\n').unwrap()..].contains('#')
                    && text[ranges[0].1.end..].starts_with(".\n\n"),
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
    let live = build_site(&st, &Scope::Canon, LIVE).unwrap();
    assert_eq!(
        stat.files.keys().collect::<Vec<_>>(),
        live.files.keys().collect::<Vec<_>>()
    );
    assert_eq!(live.label, stat.label);
    let kinds = |s: &SiteFiles| -> BTreeMap<String, Vec<SlotKind>> {
        s.label_at
            .iter()
            .map(|(p, r)| (p.clone(), r.iter().map(|(k, _)| *k).collect()))
            .collect()
    };
    assert_eq!(
        kinds(&live),
        kinds(&stat),
        "the same slots in the same files"
    );
    assert_label_places(&stat, None);
    assert_label_places(&live, Some(&live.header));
    // the live build is the static build but for its noscript sentence and its header's links:
    // filled with the same header, the two builds agree
    let (a, b) = (
        fill(&live, &live.label, HEADER),
        fill(&stat, &stat.label, HEADER),
    );
    for (path, bytes) in &a {
        assert!(unlive(path, bytes) == b[path], "{path}");
    }
    // filled with another label, the two builds still agree
    let other = "v9.9.9 <&> \u{E002}, 1 change since";
    let (a, b) = (fill(&live, other, HEADER), fill(&stat, other, HEADER));
    for (path, bytes) in &a {
        assert!(unlive(path, bytes) == b[path], "{path}");
    }
    let home = String::from_utf8_lossy(&a["index.html"]);
    assert!(home.contains(&escape(other)) && home.contains(HEADER) && home.contains(LIVE_NOSCRIPT));
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
    let live = build_site(&st, &Scope::Canon, LIVE).unwrap();
    assert_eq!(
        stat.label, "unversioned",
        "the lookalike is the label's own text"
    );
    assert_label_places(&stat, None);
    assert_label_places(&live, Some(&live.header));
    let (a, b) = (
        fill(&live, &live.label, HEADER),
        fill(&stat, &stat.label, HEADER),
    );
    for (path, bytes) in &a {
        assert!(unlive(path, bytes) == b[path], "{path}");
    }
    let other = "v2.0.0, 1 change since";
    let (a, b) = (fill(&live, other, HEADER), fill(&stat, other, HEADER));
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
    // the colophon takes the label, the header its markup
    assert_eq!(page.matches(other).count(), 1, "{page}");
    assert_eq!(page.matches(HEADER).count(), 1, "{page}");
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
