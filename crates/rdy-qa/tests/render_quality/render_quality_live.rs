//! ls10: the site quality bar on the pages the live server sends (D-VE4NK; quality-bar spec
//! 7.2): the leak scan over canon, a view and a proposal version, pages that work without
//! JavaScript, the generated pages (glossary, disagreements), a small script, and nothing on a
//! served page that acts (D-N5NSN). These tests are guards of the live layer and the server.
use rdy_daemon::Shared;
use rdy_engine::{Rdy, ReadState, Scope};
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::writer;
use rdy_qa::golden;
use rdy_qa::http;
use rdy_qa::site::{leaks, visible_runs};
use rdy_render::live::{LIVE_JS, build_live};
use regex::Regex;
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, LazyLock};

/// The noscript sentence of a static page and of a live page. They are the one text that may
/// differ between the two.
const STATIC_NOSCRIPT: &str = "Search needs JavaScript.";
const LIVE_NOSCRIPT: &str = "Search and live updates need JavaScript.";

/// A page of the golden project that the `editing` view changes.
pub(crate) const CHANGED_DOC: &str = "tutorial/adventure.html";

/// A daemon over a project, and the root URL of its site.
pub(crate) struct LiveHost {
    pub host: Hosted,
    pub url: String,
}

impl LiveHost {
    /// Serves the project in `dir` (its store closed).
    pub fn new(dir: tempfile::TempDir) -> LiveHost {
        let host = Hosted::start_in(dir);
        let url = host
            .run
            .as_ref()
            .unwrap()
            .shared()
            .site_info
            .get()
            .cloned()
            .expect("the site info is set before ready")
            .url
            .expect("the site listens");
        LiveHost { host, url }
    }

    fn shared(&self) -> &Arc<Shared> {
        self.host.run.as_ref().unwrap().shared()
    }

    /// Every file of the live build of `scope`, fetched through the server at `prefix` into a
    /// new directory at the same paths: only the `.html` pages when `html_only`.
    pub fn fetch(&self, scope: &Scope, prefix: &str, html_only: bool) -> tempfile::TempDir {
        let st: Arc<ReadState> = self.shared().state.load_full();
        let build = build_live(&st, scope).expect("the live build");
        let out = tempfile::tempdir().unwrap();
        for path in build.files.keys() {
            if html_only && !path.ends_with(".html") {
                continue;
            }
            let r = http::get(&format!("{}{prefix}{path}", self.url));
            assert_eq!(r.status, 200, "GET {prefix}{path}: {}", r.head);
            if path.ends_with(".html") {
                let ty = r.header("content-type").unwrap_or_default();
                assert!(ty.starts_with("text/html"), "{prefix}{path}: {ty}");
            }
            let to = out.path().join(path);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::write(to, &r.body).unwrap();
        }
        out
    }
}

/// The golden project on a daemon, with an edit of the tutorial proposed once, and a static render
/// of each of its three scopes (what `rdy render --out` writes).
pub(crate) struct Golden {
    pub live: LiveHost,
    pub canon: Scope,
    pub view: Scope,
    pub proposal: Scope,
    pub view_prefix: String,
    pub proposal_prefix: String,
    pub static_canon: tempfile::TempDir,
    pub static_view: tempfile::TempDir,
    pub static_proposal: tempfile::TempDir,
}

pub(crate) fn golden_live() -> Golden {
    let _clock = golden::fixture_clock();
    let (dir, mut r, ids) = golden::project();
    // The `editing` view holds blocks that no gate accepts (a dangling reference), so it can
    // not be proposed. A second view edits the same tutorial prose and is proposed once.
    golden::write(
        &mut r,
        "tutorial-edit",
        json!([{"op": "update", "id": ids.tutorial_prose.to_string(),
            "body": "So far we have used three simple patterns. They compose."}]),
    )
    .expect("the tutorial edit");
    let p = r
        .propose(&writer(), "tutorial-edit", "Edit the tutorial")
        .expect("propose the edit");
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    let canon = Scope::Canon;
    let view = Scope::View("editing".into());
    let proposal = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let render = |scope: &Scope| {
        let out = tempfile::tempdir().unwrap();
        rdy_render::render_site(&r.read(), scope, out.path()).expect("the static render");
        out
    };
    let static_canon = render(&canon);
    let static_view = render(&view);
    let static_proposal = render(&proposal);
    // the store is locked while a handle is open
    drop(r);
    Golden {
        live: LiveHost::new(dir),
        canon,
        view,
        proposal,
        view_prefix: "~view/editing/".into(),
        proposal_prefix: format!("~proposal/{pref}/"),
        static_canon,
        static_view,
        static_proposal,
    }
}

/// Serves the project in `dir` (closed) after the handle `r` is dropped.
fn serve(dir: tempfile::TempDir, r: Rdy) -> LiveHost {
    drop(r);
    LiveHost::new(dir)
}

/// The canon site of `retired_reference()` (a canon with a disagreement) on a daemon, with its
/// static render.
pub(crate) fn disagree_live() -> (LiveHost, tempfile::TempDir) {
    let (dir, r, _s) = golden::retired_reference();
    let stat = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, stat.path()).unwrap();
    (serve(dir, r), stat)
}

/// A canon whose one disagreement repeats over seven pages: its page folds a list behind
/// "Show all" (as ls11), on a daemon, with its static render.
fn grouped_live() -> (LiveHost, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::testkit::generate_tree(dir.path(), 7, 1, 3);
    let mut r = Rdy::open(dir.path()).unwrap();
    rdy_qa::fixtures::land(&mut r, "work").unwrap();
    let stat = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, stat.path()).unwrap();
    (serve(dir, r), stat)
}

static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script>").unwrap());
static NOSCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<noscript>(.*?)</noscript>").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());

/// The visible text runs of `html` with every `<script>` element removed.
fn texts(html: &str) -> Vec<String> {
    visible_runs(&SCRIPT.replace_all(html, ""))
        .into_iter()
        .map(|r| r.text)
        .collect()
}

/// The text of the page's `<noscript>` elements, whitespace collapsed.
fn noscript(html: &str) -> String {
    let all: String = NOSCRIPT
        .captures_iter(html)
        .map(|c| TAG.replace_all(&c[1], " ").into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    all.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read(dir: &Path, page: &str) -> String {
    golden::html(dir, page)
}

/// The served page, without its scripts, reads as the static page, except for the noscript
/// sentence.
fn assert_works_without_js(served: &Path, stat: &Path, page: &str, what: &str) {
    let (s, t) = (read(served, page), read(stat, page));
    let (a, b) = (texts(&s), texts(&t));
    if a != b {
        let at = a.iter().zip(&b).position(|(x, y)| x != y);
        panic!(
            "{what} {page}: the served text differs from the static text (served {} runs, static {}), first difference at {at:?}: {:?} against {:?}",
            a.len(),
            b.len(),
            at.map(|i| &a[i]),
            at.map(|i| &b[i]),
        );
    }
    let (ns, nt) = (noscript(&s), noscript(&t));
    assert!(
        ns.contains(LIVE_NOSCRIPT),
        "{what} {page}: the served noscript note: {ns}"
    );
    assert!(
        nt.contains(STATIC_NOSCRIPT) && !nt.contains("live updates"),
        "{what} {page}: the static noscript note: {nt}"
    );
    assert_eq!(
        ns.replace(LIVE_NOSCRIPT, STATIC_NOSCRIPT),
        nt,
        "{what} {page}: the noscript notes differ in more than the sentence"
    );
}

/// The doc page of `site` with the most visible text: not a folder page, a generated page or a
/// permalink (whose text is a sentence).
pub(crate) fn longest_doc(site: &Path) -> String {
    golden::pages(site)
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(site)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let len: usize = texts(&golden::html(site, &rel))
                .iter()
                .map(String::len)
                .sum();
            (rel, len)
        })
        .filter(|(rel, _)| {
            !rel.ends_with("index.html") && rel != "glossary.html" && rel != "disagreements.html"
        })
        .max_by_key(|(rel, len)| (*len, std::cmp::Reverse(rel.clone())))
        .map(|(rel, _)| rel)
        .expect("a doc page")
}

fn page_set(dir: &Path) -> Vec<String> {
    golden::pages(dir)
        .into_iter()
        .map(|p| {
            p.strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn ls10_served_pages_pass_the_leak_scan() {
    let g = golden_live();
    for (what, scope, prefix, stat) in [
        ("canon", &g.canon, "", &g.static_canon),
        (
            "view:editing",
            &g.view,
            g.view_prefix.as_str(),
            &g.static_view,
        ),
        (
            "the proposal version",
            &g.proposal,
            g.proposal_prefix.as_str(),
            &g.static_proposal,
        ),
    ] {
        let served = g.live.fetch(scope, prefix, true);
        let pages = page_set(served.path());
        assert!(pages.len() > 10, "{what}: guard: pages were served");
        assert_eq!(
            pages,
            page_set(stat.path()),
            "{what}: the server sends the pages the render writes"
        );
        let found = leaks(served.path());
        assert!(found.is_empty(), "{what}: {found:#?}");
    }
}

#[test]
fn ls10_served_pages_work_without_javascript() {
    let g = golden_live();
    let canon = g.live.fetch(&g.canon, "", true);
    let long = longest_doc(g.static_canon.path());
    for page in ["index.html", "specification/index.html", long.as_str()] {
        assert_works_without_js(canon.path(), g.static_canon.path(), page, "canon");
    }
    let view = g.live.fetch(&g.view, &g.view_prefix, true);
    assert_works_without_js(view.path(), g.static_view.path(), CHANGED_DOC, "view");
    // the review marks are words, so they stay without the script
    let words = texts(&read(view.path(), CHANGED_DOC));
    assert!(
        words.iter().any(|t| t == "Edited"),
        "the changed doc keeps its Edited mark: {words:?}"
    );
    let new = page_set(view.path())
        .iter()
        .any(|p| texts(&read(view.path(), p)).iter().any(|t| t == "New"));
    assert!(new, "some page of the view keeps its New mark");
    let proposal = g.live.fetch(&g.proposal, &g.proposal_prefix, true);
    assert_works_without_js(
        proposal.path(),
        g.static_proposal.path(),
        CHANGED_DOC,
        "the proposal version",
    );
}

#[test]
fn ls10_the_generated_pages_pass_on_the_live_site() {
    let g = golden_live();
    let canon = g.live.fetch(&g.canon, "", true);
    assert_works_without_js(
        canon.path(),
        g.static_canon.path(),
        "glossary.html",
        "canon",
    );
    let found: Vec<_> = leaks(canon.path())
        .into_iter()
        .filter(|l| l.page == "glossary.html")
        .collect();
    assert!(found.is_empty(), "the glossary: {found:#?}");

    let (host, stat) = disagree_live();
    let served = host.fetch(&Scope::Canon, "", true);
    assert_works_without_js(served.path(), stat.path(), "disagreements.html", "canon");
    let found = leaks(served.path());
    assert!(found.is_empty(), "the disagreements page: {found:#?}");

    // a repeated disagreement folds its pages behind "Show all": a native <details>, so it
    // opens without the script
    let (host, stat) = grouped_live();
    let served = host.fetch(&Scope::Canon, "", true);
    assert_works_without_js(served.path(), stat.path(), "disagreements.html", "grouped");
    let page = read(served.path(), "disagreements.html");
    assert!(
        page.contains("<details class=\"more-links\"><summary>Show all</summary>"),
        "the fold is a native disclosure: {page}"
    );
    assert!(
        texts(&page).iter().any(|t| t == "Show all"),
        "the fold's summary is visible text"
    );
    let found = leaks(served.path());
    assert!(found.is_empty(), "the grouped page: {found:#?}");
}

#[test]
fn ls10_the_live_script_is_small() {
    assert!(
        LIVE_JS.len() < 1500,
        "the live script is {} bytes",
        LIVE_JS.len()
    );
    assert!(!LIVE_JS.contains("http"), "the script names no other host");
}

#[test]
fn ls10_no_page_shows_a_call_to_action() {
    // guard (k-N0EJF, D-Q13XN, D-N5NSN): nothing on the site acts
    let g = golden_live();
    let (dh, _stat) = disagree_live();
    let sites = [
        g.live.fetch(&g.canon, "", true),
        g.live.fetch(&g.view, &g.view_prefix, true),
        g.live.fetch(&g.proposal, &g.proposal_prefix, true),
        dh.fetch(&Scope::Canon, "", true),
    ];
    let mut seen = 0;
    for site in &sites {
        for page in page_set(site.path()) {
            let html = read(site.path(), &page);
            seen += 1;
            for bad in ["Approve", "needs you", "<form"] {
                assert!(!html.contains(bad), "{page} holds {bad:?}");
            }
        }
    }
    assert!(seen > 30, "guard: {seen} pages were scanned");
}
