//! ls1: the site as bytes in memory; rdy render --out writes exactly those bytes.
use rdy_engine::Scope;
use rdy_render::{LABEL_SLOT, Mode, build_site, escape, render_site};
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
    let slot = LABEL_SLOT.to_string();
    for (path, bytes) in &live.files {
        let text = String::from_utf8_lossy(bytes);
        let filled = if path.ends_with(".html") {
            text.replace(&slot, &escape(&live.label))
                .replace(LIVE_NOSCRIPT, STATIC_NOSCRIPT)
        } else if path == "llms.txt" || path == "llms-full.txt" {
            assert!(text.contains(&slot), "{path} carries the slot");
            text.replace(&slot, &live.label)
        } else {
            assert!(
                !bytes.windows(3).any(|w| w == slot.as_bytes()),
                "{path} holds the slot"
            );
            text.into_owned()
        };
        if path.ends_with(".html") || path.ends_with(".txt") {
            assert_eq!(filled.as_bytes(), &stat.files[path][..], "{path}");
        }
    }
    let home = String::from_utf8_lossy(&live.files["index.html"]);
    assert!(
        home.contains(&slot) && home.contains(LIVE_NOSCRIPT),
        "the home page is live"
    );
}

#[test]
fn ls1_an_in_flight_scope_is_still_refused() {
    // guard: render_site keeps its refusal
    let (_dir, r, _ids) = rdy_qa::golden::project();
    let out = tempfile::tempdir().unwrap();
    let e = render_site(&r.read(), &Scope::Inflight, out.path()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
}
