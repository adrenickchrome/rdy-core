//! sod3 (D-F44G6, refined by D-WYZ97 and D-SMP5A): a review target of the live site renders on
//! request, and the bytes it sends are the bytes of the target's whole build. The whole build
//! belongs to these tests alone, as the byte oracle: the daemon never makes one for a review
//! target. The tests are guards of the live layer: they add proof and replace no expected byte.
use rdy_daemon::Shared;
use rdy_daemon::site::guard::{ScopeRef, SiteTarget};
use rdy_engine::publication::check as publication_check;
use rdy_engine::{Rdy, ReadState};
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::golden::tiny;
use rdy_qa::http;
use rdy_render::live::{LiveBuild, ProfileHistoryMemo, build_live, build_profile_live, page};
use rdy_render::{Mode, NotFoundKind, build_profile_site, build_site};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The words the generated missing page of a target says.
const MISS: &str = "There is no page at this address.";
/// The live script every live page loads and no generated missing page does.
const LIVE_SCRIPT: &str = "/_rdy/live.js";

// --- the world ----------------------------------------------------------------------------------

/// A project on a daemon: canon with a folder `manual` (a doc with prose, a figure and a
/// diagram), docs `alpha` and `beta`, a doc `secret` that refers to the intro, a term, and three
/// profiles; the views `rv` and `rp`, each with the same edits; the proposal of `rp`, version 1.
struct World {
    host: Hosted,
    url: String,
    /// The first version of the proposal of view `rp`: `P-...@1`.
    version: String,
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    write(r, view, ops)
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect()
}

fn profile_op(key: &str, audience: &str, admin: &str, select: &[&str]) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
        "parent": admin,
        "fields": {"key": key, "title": format!("{key} book"), "description": format!("About {key}."),
            "audience": audience, "select": select}})
}

/// The edits of a review view: a changed paragraph, a removed doc and a removed block, and the
/// docs that take the names of generated files (`changes`, `glossary`, `404`).
fn edits(r: &mut Rdy, view: &str, ids: &BTreeMap<String, String>) {
    put(
        r,
        view,
        json!([
            {"op": "update", "id": ids["$ip"], "body": format!("INTRO-{view}")},
            {"op": "delete", "id": ids["$b1"]},
            {"op": "delete", "id": ids["$beta"]},
            {"op": "delete", "id": ids["$a2"]},
            {"op": "create", "type": "doc", "id": "$c", "fields": {"name": "changes", "title": "Authored changes", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$c", "body": "AUTHORED-CHANGES"},
            {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Our words", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$g", "body": "AUTHORED-GLOSSARY"},
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "404", "title": "Authored missing page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$n", "body": "AUTHORED-404-BODY"}
        ]),
    );
}

fn world() -> World {
    let (dir, mut r) = tiny();
    let png = b"\x89PNG\r\n\x1a\nFIGBYTES-sod3".to_vec();
    let blob = r.store_mut().put_blob(&png).unwrap().to_string();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-CANON"},
            {"op": "create", "type": "doc", "id": "$figs", "parent": "$manual", "fields": {"name": "figs", "title": "Figures", "doc_type": "page"}},
            {"op": "create", "type": "figure", "parent": "$figs", "fields": {"blob": blob, "media_type": "image/png", "alt": "A figure"}},
            {"op": "create", "type": "diagram", "parent": "$figs", "fields": {"lang": "mermaid", "alt": "flow"}, "body": "graph TD\n  A --> B"},
            {"op": "create", "type": "doc", "id": "$alpha", "fields": {"name": "alpha", "title": "Alpha", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$a1", "parent": "$alpha", "body": "Alpha first."},
            {"op": "create", "type": "prose", "id": "$a2", "parent": "$alpha", "body": "Alpha second."},
            {"op": "create", "type": "doc", "id": "$beta", "fields": {"name": "beta", "title": "Beta", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$b1", "parent": "$beta", "body": "Beta first."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    let ip = ids["$ip"].clone();
    ids.extend(put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$secret", "body": format!("SECRET sees {{{{{ip}}}}}.")},
            {"op": "create", "type": "doc", "id": "$words", "fields": {"name": "words", "title": "Words", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ws", "parent": "$words", "fields": {"title": "Terms"}},
            {"op": "create", "type": "term", "parent": "$ws", "fields": {"name": "Sodterm"}, "body": "A term the glossary lists."}
        ]),
    ));
    land(&mut r, "w1").unwrap();
    let admin = ids["$admin"].clone();
    ids.extend(put(
        &mut r,
        "admin",
        json!([
            profile_op("guide", "org", &admin, &[&ids["$manual"]]),
            profile_op("other", "public", &admin, &[&ids["$secret"]])
        ]),
    ));
    land(&mut r, "admin").unwrap();
    edits(&mut r, "rv", &ids);
    edits(&mut r, "rp", &ids);
    let version = r
        .propose(&writer(), "rp", "Edit it", Some("Changes the intro."))
        .unwrap()
        .r#ref;
    assert!(version.ends_with("@1"), "{version}");
    drop(r);
    let host = Hosted::start_in(dir);
    let url = host
        .run
        .as_ref()
        .unwrap()
        .shared()
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    World { host, url, version }
}

impl World {
    fn shared(&self) -> Arc<Shared> {
        Arc::clone(self.host.run.as_ref().unwrap().shared())
    }

    fn get(&self, prefix: &str, path: &str) -> http::Response {
        http::get(&format!("{}{prefix}{path}", self.url))
    }

    /// The review targets: a view and a proposal version, each without a profile and with an org
    /// and a public profile (whose reference check is blocked: it still serves).
    fn targets(&self) -> Vec<SiteTarget> {
        let scopes = [
            ScopeRef::View("rv".into()),
            ScopeRef::Proposal(self.version.clone()),
        ];
        let mut out = vec![];
        for scope in scopes {
            out.push(SiteTarget::Source(scope.clone()));
            for key in ["guide", "other"] {
                out.push(SiteTarget::Profile {
                    scope: scope.clone(),
                    key: key.into(),
                });
            }
        }
        out
    }
}

/// The whole build of `target` from `st`, as the test's byte oracle.
fn whole(st: &ReadState, target: &SiteTarget) -> LiveBuild {
    match target {
        SiteTarget::Source(ScopeRef::View(name)) => {
            let scope = st
                .resolve_read_scope(&format!("view:{name}"), None)
                .unwrap();
            build_live(st, &scope, target).unwrap()
        }
        SiteTarget::Source(ScopeRef::Proposal(r)) => {
            let scope = st.resolve_scope(&format!("proposal:{r}")).unwrap();
            build_live(st, &scope, target).unwrap()
        }
        SiteTarget::Profile { .. } => {
            build_profile_live(st, target, &mut ProfileHistoryMemo::default())
                .unwrap()
                .live
        }
        other => panic!("not a review target: {other:?}"),
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

// --- 1. the bytes of a request are the bytes of the whole build ---------------------------------

/// Paths the whole build of every target has, so the matrix covers each kind of file.
fn assert_kinds(files: &BTreeMap<String, Vec<u8>>, what: &str) {
    for need in [
        "index.html",
        "search-index.json",
        "llms.txt",
        "llms-full.txt",
    ] {
        assert!(files.contains_key(need), "{what}: has {need}");
    }
    assert!(
        files.keys().any(|p| p.starts_with("assets/")),
        "{what}: has assets"
    );
}

#[test]
fn sod3_on_request_page_matches_whole_build_byte_for_byte() {
    let w = world();
    let sh = w.shared();
    // the same state for the oracle and the daemon: nothing writes while the test runs
    let st: Arc<ReadState> = sh.state.load_full();
    let mut compared = 0usize;
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for target in w.targets() {
        let build = whole(&st, &target);
        let prefix = target.root().trim_start_matches('/').to_string();
        let what = format!("{target:?}");
        assert_kinds(&build.files, &what);
        assert_collisions(&w, &build, &target);
        // every file of the whole build, existing files first through `live::page`
        let mut paths: Vec<&String> = build.files.keys().collect();
        // a direct asset first: its referring page was never requested
        paths.sort_by_key(|p| !p.starts_with("assets/"));
        for path in paths {
            let want = page(&build, path, &target).unwrap();
            let got = w.get(&prefix, path);
            assert_eq!(got.status, 200, "{what} {path}: {} (transport: {})", got.head, got.transport);
            assert!(
                got.body == want,
                "{what} {path}: the whole build's bytes ({} sent, {} built)",
                got.body.len(),
                want.len()
            );
            compared += 1;
            let kind = match path.as_str() {
                "index.html" => "home",
                "search-index.json" => "search",
                "llms.txt" | "llms-full.txt" => "llms",
                "changes.html" | "404.html" | "glossary.html" => "authored",
                p if p.starts_with("assets/") => "asset",
                p if p.starts_with("go/") => "permalink",
                p if p.ends_with("/index.html") => "folder",
                p if p.ends_with(".html") => "doc",
                _ => "other",
            };
            *kinds.entry(kind).or_default() += 1;
        }
        // misses: the generated page of the target, status 404, nothing live in it
        let miss = build
            .not_found
            .get(NotFoundKind::Page)
            .expect("the whole build holds the target's missing page")
            .to_vec();
        for path in [
            "nosuch.html",
            "deep/nosuch.html",
            "assets/nosuch.png",
            "go/nosuch.html",
            "llms-nosuch.txt",
        ] {
            let got = w.get(&prefix, path);
            assert_eq!(got.status, 404, "{what} {path}: {} (transport: {})", got.head, got.transport);
            assert!(got.body == miss, "{what} {path}: the generated miss");
            let t = text(&got.body);
            assert!(t.contains(MISS), "{what} {path}: the themed sentence");
            assert!(!t.contains(LIVE_SCRIPT), "{what} {path}: no live script");
            assert!(
                !t.contains("data-rev") && !t.contains("data-page"),
                "{what} {path}: no page revision"
            );
        }
        // an authored 404 is an existing file: `live::page` is its oracle, and it is not the
        // generated response a miss gets
        if let Some(authored) = build.files.get("404.html") {
            assert!(text(authored).contains("AUTHORED-404-BODY"), "{what}");
            let want = page(&build, "404.html", &target).unwrap();
            let got = w.get(&prefix, "404.html");
            assert_eq!(got.status, 200, "{what} 404.html: {} (transport: {})", got.head, got.transport);
            assert!(got.body == want, "{what} 404.html: the authored page");
            assert!(got.body != miss, "{what}: authored 404 is not the miss");
        }
    }
    for kind in [
        "home",
        "doc",
        "folder",
        "permalink",
        "search",
        "llms",
        "asset",
        "authored",
    ] {
        assert!(
            kinds.get(kind).copied().unwrap_or(0) > 0,
            "the matrix has {kind}: {kinds:?}"
        );
    }
    assert!(compared > 100, "{compared} files compared");
    // the misses opened no stream and entered no index; no whole build was made
    assert_eq!(sh.site.streams(), 0, "no request opened a stream");
    assert!(
        !sh.site
            .ledger()
            .kept
            .iter()
            .any(|(_, p, _)| p.contains("nosuch")),
        "no generated miss is a kept page"
    );
    assert_eq!(sh.site.review_builds(), 0, "no whole review build");
}

/// Authored-path collisions, removed content and excluded content, as the oracle holds them and
/// as a request gets them.
fn assert_collisions(w: &World, build: &LiveBuild, target: &SiteTarget) {
    {
        let what = format!("{target:?}");
        let source = matches!(target, SiteTarget::Source(_));
        let guide = matches!(target, SiteTarget::Profile { key, .. } if key == "guide");
        // collisions: the authored changes page wins, and the glossary doc keeps no page
        if source {
            let changes = text(&build.files["changes.html"]);
            assert!(changes.contains("AUTHORED-CHANGES"), "{what}");
            assert!(
                build
                    .warnings
                    .iter()
                    .any(|w| w.contains("top-level doc named glossary")),
                "{what}: {:?}",
                build.warnings
            );
            assert!(!text(&build.files["glossary.html"]).contains("AUTHORED-GLOSSARY"));
            // removed content: the removed doc has no page
            assert!(!build.files.contains_key("beta.html"), "{what}: beta");
        }
        // excluded content: a profile page holds only its selection
        if guide {
            assert!(!build.files.contains_key("alpha.html"), "{what}: alpha");
            assert!(build.files.contains_key("manual/intro.html"), "{what}");
        }
        // each of these is the same bytes on request
        let prefix = target.root().trim_start_matches('/').to_string();
        for path in ["changes.html", "glossary.html", "alpha.html", "beta.html"] {
            let got = w.get(&prefix, path);
            match page(build, path, target) {
                Some(want) => {
                    assert_eq!(got.status, 200, "{what} {path} (transport: {})", got.transport);
                    assert!(got.body == want, "{what} {path}");
                }
                None => {
                    assert_eq!(got.status, 404, "{what} {path}: {} (transport: {})", got.head, got.transport);
                    let miss = build.not_found.get(NotFoundKind::Page).unwrap();
                    assert!(got.body == miss, "{what} {path}: the generated miss");
                }
            }
        }
    }
}

// --- 2. static and publication outputs ----------------------------------------------------------

type Site = (String, BTreeMap<String, Vec<u8>>);

/// The static output of the project and its publication-check facts, as they stand now.
fn outputs(st: &ReadState) -> (Vec<Site>, Vec<(String, Value)>) {
    let mut sites = vec![];
    for (name, scope) in [
        ("canon", "canon"),
        ("view:rv", "view:rv"),
        ("view:rp", "view:rp"),
    ] {
        let resolved = st.resolve_read_scope(scope, None).unwrap();
        let site = build_site(st, &resolved, Mode::Static).unwrap();
        sites.push((name.to_string(), site.files));
    }
    let site = build_profile_site(st, "canon", "guide").unwrap();
    sites.push(("canon~guide".to_string(), site.files));
    let mut facts = vec![];
    for (profile, scope) in [
        ("guide", None),
        ("other", None),
        ("guide", Some("view:rv")),
        ("other", Some("view:rv")),
    ] {
        let report = publication_check(st, profile, scope, None, None).unwrap();
        facts.push((format!("{profile}@{scope:?}"), report));
    }
    (sites, facts)
}

#[test]
fn sod3_live_review_keeps_static_and_publication_outputs() {
    let w = world();
    let sh = w.shared();
    let st: Arc<ReadState> = sh.state.load_full();
    let (static_before, facts_before) = outputs(&st);
    assert!(
        static_before
            .iter()
            .all(|(_, files)| !text(&files["index.html"]).contains(LIVE_SCRIPT)),
        "a static page loads no live script: {:?}",
        static_before.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    assert!(
        facts_before.iter().any(|(_, f)| f.is_object()),
        "the publication check reported"
    );
    // live review requests: every target, every file, a miss, and the link's complete check
    for target in w.targets() {
        let build = whole(&st, &target);
        let prefix = target.root().trim_start_matches('/').to_string();
        for path in build.files.keys() {
            assert_eq!(w.get(&prefix, path).status, 200, "{target:?} {path} (transport: {})", rdy_qa::http::last_transport());
        }
        assert_eq!(w.get(&prefix, "nosuch.html").status, 404, "transport: {}", rdy_qa::http::last_transport());
        sh.site
            .review_facts(&sh, &target)
            .unwrap_or_else(|e| panic!("{target:?}: {e}"));
    }
    // the daemon wrote nothing: the same state, so the same static output and check facts
    let (static_after, facts_after) = outputs(&st);
    assert_eq!(static_before.len(), static_after.len());
    for ((n, before), (_, after)) in static_before.iter().zip(&static_after) {
        assert!(before == after, "static output of {n} is as it was");
    }
    assert_eq!(facts_before, facts_after, "the publication check facts");
    // and a fresh read of the same store gives the same bytes again
    let fresh = sh.state.load_full();
    let (static_fresh, facts_fresh) = outputs(&fresh);
    assert!(static_before == static_fresh, "static output, fresh read");
    assert_eq!(facts_before, facts_fresh);
    assert_eq!(sh.site.review_builds(), 0, "no whole review build");
}
