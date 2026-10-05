use rdy_engine::*;
use rdy_qa::golden::{self, pages};
use rdy_qa::site::{attrs, unescape};
use std::path::Path;

fn hostile_site() -> (tempfile::TempDir, tempfile::TempDir) {
    let (d, mut r) = golden::hostile();
    let out = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, out.path()).unwrap();
    (d, out)
}

#[test]
fn render_hostile_text_is_escaped_on_every_page() {
    let (_d, site) = hostile_site();
    for p in pages(site.path()) {
        let html = std::fs::read_to_string(&p).unwrap();
        for raw in [
            "<script>alert(1)",
            "onerror=alert(2)>",
            "\"'><img",
            "<img src=x",
        ] {
            assert!(!html.contains(raw), "{}: raw {raw:?}", p.display());
        }
        for (i, _) in html.match_indices("<script") {
            assert!(
                html[i..].starts_with("<script src=\""),
                "{}: unexpected <script",
                p.display()
            );
        }
        for h in attrs(&html, "href") {
            assert!(
                !h.to_ascii_lowercase().starts_with("javascript:"),
                "{}: {h}",
                p.display()
            );
        }
    }
}

#[derive(Default, Debug)]
struct LinkStats {
    pages: usize,
    max_depth: usize,
    anchors: usize,
    assets: usize,
    /// The `go/` pages whose refresh target resolved.
    go_targets: usize,
}

/// Checks every link in the site; returns the broken ones and what was covered.
fn check_links(site: &Path) -> (Vec<String>, LinkStats) {
    let mut broken = vec![];
    let mut st = LinkStats::default();
    for p in pages(site) {
        let html = std::fs::read_to_string(&p).unwrap();
        st.pages += 1;
        st.max_depth = st
            .max_depth
            .max(p.strip_prefix(site).unwrap().components().count() - 1);
        for link in attrs(&html, "href").into_iter().chain(attrs(&html, "src")) {
            if link.starts_with("http://") || link.starts_with("https://") || link.is_empty() {
                continue;
            }
            let link = unescape(link);
            let (file, anchor) = match link.split_once('#') {
                Some((f, a)) => (f.to_string(), Some(a.to_string())),
                None => (link.clone(), None),
            };
            if anchor.is_some() {
                st.anchors += 1;
            }
            // a figure's file, not the theme's
            if file.contains("assets/") && !file.ends_with(".css") && !file.ends_with(".js") {
                st.assets += 1;
            }
            let mut target = if file.is_empty() {
                p.clone()
            } else {
                p.parent().unwrap().join(&file)
            };
            if !target.exists() {
                broken.push(format!("{} → {link}", p.display()));
                continue;
            }
            if target.is_dir() {
                target = target.join("index.html");
                if !target.exists() {
                    broken.push(format!(
                        "{} → {link} (directory without index.html)",
                        p.display()
                    ));
                    continue;
                }
            }
            if let Some(a) = anchor {
                let t = std::fs::read_to_string(&target).unwrap_or_default();
                if !t.contains(&format!("id=\"{a}\"")) {
                    broken.push(format!("{} → {link} (no anchor)", p.display()));
                }
            }
        }
        // a permalink page's refresh target resolves, with its anchor
        if p.strip_prefix(site).is_ok_and(|r| r.starts_with("go")) {
            let refresh = attrs(&html, "content")
                .into_iter()
                .find_map(|c| c.strip_prefix("0; url="))
                .map(unescape);
            let Some(refresh) = refresh else {
                broken.push(format!("{}: no refresh", p.display()));
                continue;
            };
            let (file, anchor) = match refresh.split_once('#') {
                Some((f, a)) => (f.to_string(), Some(a.to_string())),
                None => (refresh.clone(), None),
            };
            let mut target = p.parent().unwrap().join(&file);
            if target.is_dir() {
                target = target.join("index.html");
            }
            let t = std::fs::read_to_string(&target).unwrap_or_default();
            if t.is_empty() || anchor.is_some_and(|a| !t.contains(&format!("id=\"{a}\""))) {
                broken.push(format!("{} → {refresh} (refresh target)", p.display()));
            } else {
                st.go_targets += 1;
            }
        }
    }
    (broken, st)
}

#[test]
fn render_every_link_resolves() {
    let (_d, site) = hostile_site();
    let (broken, _) = check_links(site.path());
    assert!(broken.is_empty(), "{broken:#?}");
}

#[test]
fn render_every_link_resolves_on_a_rich_site() {
    let (_d, mut r) = golden::generated_tree();
    let landing: rdy_model::Hash = r.versions().unwrap()[0].landing.parse().unwrap();
    let scopes = [
        ("canon", Scope::Canon),
        ("view", Scope::View("draft".into())),
        ("tag", Scope::At(landing)),
    ];
    for (name, scope) in scopes {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&mut r, &scope, out.path()).unwrap();
        let (broken, st) = check_links(out.path());
        assert!(broken.is_empty(), "{name}: {broken:#?}");
        assert!(st.pages >= 10, "{name}: {st:?}");
        assert!(st.max_depth >= 3, "{name}: {st:?}");
        assert!(st.anchors >= 5, "{name}: {st:?}");
        assert!(st.assets >= 1, "{name}: {st:?}");
        assert!(st.go_targets >= 1, "{name}: {st:?}");
    }
}
