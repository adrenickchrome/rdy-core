//! ls10: the site quality bar on the pages the live server sends (D-VE4NK; quality-bar spec
//! 7.2): the leak scan over canon, a view and a proposal version, pages that work without
//! JavaScript, the generated pages (glossary, disagreements), a small script, and nothing on a
//! served page that acts (D-N5NSN). These tests are guards of the live layer and the server.
use rdy_daemon::Shared;
use rdy_daemon::site::guard::{Guard, Route, SiteTarget};
use rdy_daemon::site::http::parse_head;
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
        let build = build_live(&st, scope, &source_target(prefix)).expect("the live build");
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

/// The source target the site serves under `prefix` (`""`, `~view/<enc>/`, ...), as the site's
/// guard routes it.
fn source_target(prefix: &str) -> SiteTarget {
    let guard = Guard::full(1, None);
    let head = format!("GET /{prefix}index.html HTTP/1.1\r\nHost: 127.0.0.1:1\r\n\r\n");
    match guard.check(&parse_head(head.as_bytes()).unwrap()) {
        Ok(Route::Page { target, .. }) => target,
        other => panic!("/{prefix} routes to {other:?}"),
    }
}

/// The golden project on a daemon, with an edit of the tutorial proposed once, and a static render
/// of each of its three scopes (what `rdy render --out` writes).
pub(crate) struct Golden {
    pub live: LiveHost,
    pub canon: Scope,
    pub view: Scope,
    pub proposal: Scope,
    /// The site of the tag `v1.0.0` (`/~tag/v1.0.0/`, a read-only scope).
    pub tag: Scope,
    pub view_prefix: String,
    pub proposal_prefix: String,
    pub tag_prefix: String,
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
        .propose(&writer(), "tutorial-edit", "Edit the tutorial", None)
        .expect("propose the edit");
    let pref = r.resolve_proposal(&p.id).unwrap().to_string();
    let canon = Scope::Canon;
    let view = Scope::View("editing".into());
    let proposal = r.read().resolve_scope(&format!("proposal:{pref}")).unwrap();
    let tag = r.read().resolve_scope("tag:v1.0.0").unwrap();
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
        tag,
        view_prefix: "~view/editing/".into(),
        proposal_prefix: format!("~proposal/{pref}/"),
        tag_prefix: "~tag/v1.0.0/".into(),
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
pub(crate) fn grouped_live() -> (LiveHost, tempfile::TempDir) {
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

// -------------------------------------------------------------------------------- sr10
// The site quality bar on the review pages (D-VE4NK; bar 7.2): the served pages of a view, a
// proposal version and a tag, the diff summary pages, the versions page and the new header. These
// tests are guards of Tasks 2 to 8: they may pass at once; a failure is a defect of the task that
// owns the rule.

/// The scopes the server sends, with the path prefix of each: canon, the `editing` view, its
/// proposal version and the tag `v1.0.0`.
fn sr10_scopes(g: &Golden) -> [(&'static str, &Scope, &str); 4] {
    [
        ("canon", &g.canon, ""),
        ("view:editing", &g.view, g.view_prefix.as_str()),
        (
            "the proposal version",
            &g.proposal,
            g.proposal_prefix.as_str(),
        ),
        ("tag:v1.0.0", &g.tag, g.tag_prefix.as_str()),
    ]
}

/// The versions page, as the server sends it.
pub(crate) fn versions_page(g: &Golden) -> String {
    let r = http::get(&format!("{}~versions/", g.live.url));
    assert_eq!(r.status, 200, "GET /~versions/: {}", r.head);
    String::from_utf8(r.body).expect("the versions page is text")
}

/// `html` with the `hidden` attribute removed from every tag, so that the text a script shows
/// (the inline diffs, the toggles) is read too. A quoted value is kept whole, so the word inside a
/// title stays.
fn unhidden(html: &str) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[a-zA-Z][^>]*>").unwrap());
    static HIDDEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)"[^"]*"|'[^']*'|\shidden(?:="[^"]*")?([\s/>]|$)"#).unwrap()
    });
    TAGS.replace_all(html, |t: &regex::Captures| {
        HIDDEN
            .replace_all(&t[0], |a: &regex::Captures| {
                match a.get(1) {
                    // the bare attribute: keep what follows it
                    Some(after) => after.as_str().to_string(),
                    None => a[0].to_string(),
                }
            })
            .into_owned()
    })
    .into_owned()
}

/// The inner HTML of the first element whose opening tag holds `marker`, up to its matching close.
fn inner_of(html: &str, marker: &str) -> Option<String> {
    let at = html.find(marker)?;
    let open = html[..at].rfind('<')?;
    let name: String = html[open + 1..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    let start = open + html[open..].find('>')? + 1;
    let (open_tag, close_tag) = (format!("<{name}"), format!("</{name}>"));
    let (mut depth, mut i) = (1usize, start);
    while i < html.len() {
        let rest = &html[i..];
        let next_close = rest.find(&close_tag)?;
        let next_open = rest.find(&open_tag).filter(|&o| {
            let after = rest[o + open_tag.len()..].chars().next();
            o < next_close && after.is_some_and(|c| c.is_whitespace() || c == '>' || c == '/')
        });
        match next_open {
            Some(o) => {
                depth += 1;
                i += o + open_tag.len();
            }
            None => {
                depth -= 1;
                if depth == 0 {
                    return Some(html[start..i + next_close].to_string());
                }
                i += next_close + close_tag.len();
            }
        }
    }
    None
}

/// Whether `html` is a permalink page, a one-line "this page has moved" with no header.
fn is_redirect(html: &str) -> bool {
    html.contains("data-rdy-kind=\"redirect\"")
}

/// The header's version fragment: the pill and its chips.
fn version_fragment(page: &str, html: &str) -> String {
    inner_of(html, "data-rdy-version")
        .unwrap_or_else(|| panic!("{page}: the header has no [data-rdy-version]"))
}

/// The visible text of `fragment`, one string per run.
fn runs_of(fragment: &str) -> Vec<String> {
    visible_runs(fragment).into_iter().map(|r| r.text).collect()
}

/// The words of a fragment: its visible runs joined by a space.
fn words_of(fragment: &str) -> String {
    runs_of(fragment).join(" ")
}

/// The `title` and `aria-label` values in `html`.
fn hovers(html: &str) -> Vec<String> {
    let mut out = vec![];
    for key in ["title", "aria-label"] {
        out.extend(
            rdy_qa::site::attrs(html, key)
                .into_iter()
                .map(rdy_qa::site::unescape),
        );
    }
    out
}

#[test]
fn sr10_served_review_pages_pass_the_leak_scan() {
    let g = golden_live();
    for (what, scope, prefix) in sr10_scopes(&g) {
        let served = g.live.fetch(scope, prefix, true);
        let pages = page_set(served.path());
        assert!(pages.len() > 10, "{what}: guard: pages were served");
        match what {
            "canon" => assert!(
                pages.iter().any(|p| p == "changes-since.html"),
                "{what}: the landings since the tag have a page: {pages:?}"
            ),
            "view:editing" | "the proposal version" => assert!(
                pages.iter().any(|p| p == "changes.html"),
                "{what}: a review render has its change summary: {pages:?}"
            ),
            _ => {}
        }
        // every hidden attribute removed: the text a script shows is read too
        let mut unhid = 0;
        for page in &pages {
            let (html, path) = (read(served.path(), page), served.path().join(page));
            let open = unhidden(&html);
            if open != html {
                unhid += 1;
            }
            std::fs::write(path, open).unwrap();
        }
        if what != "canon" && what != "tag:v1.0.0" {
            assert!(unhid > 0, "{what}: guard: some page held a hidden element");
        }
        let found = leaks(served.path());
        assert!(found.is_empty(), "{what}: {found:#?}");
    }
}

#[test]
fn sr10_the_versions_page_passes_the_leak_scan() {
    let g = golden_live();
    let page = versions_page(&g);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), unhidden(&page)).unwrap();
    // guard: the page lists the three kinds, so the scan reads names, authors and counts
    let words = words_of(&page);
    for h in ["Versions", "Proposals", "Views"] {
        assert!(words.contains(h), "the versions page lists {h}: {words}");
    }
    assert!(
        words.contains("Edit the tutorial"),
        "the proposal is listed by its title: {words}"
    );
    let found = leaks(dir.path());
    assert!(found.is_empty(), "the versions page: {found:#?}");
}

/// The `editing` view of a project whose tutorial holds a changed block of each kind: the golden
/// project (the view edits the tutorial prose) plus a block added to the tutorial in the same
/// view. The golden project itself is kept as it is: its pins count its changes.
fn mixed_view_live() -> (LiveHost, Scope, &'static str) {
    let _clock = golden::fixture_clock();
    let (dir, mut r, ids) = golden::project();
    golden::write(
        &mut r,
        "editing",
        json!([{"op": "create", "type": "prose", "parent": ids.tutorial_doc.to_string(),
            "body": "A paragraph added to the tutorial."}]),
    )
    .expect("the block added to the tutorial");
    drop(r);
    (
        LiveHost::new(dir),
        Scope::View("editing".into()),
        "~view/editing/",
    )
}

/// The `<article>` of a page: the document's own text, without the navigation around it.
fn article_of(page: &str, html: &str) -> String {
    inner_of(html, "data-rdy=\"content\"").unwrap_or_else(|| panic!("{page}: no content article"))
}

/// One chip of the header: its tag, its class and its href.
#[derive(Debug)]
struct ChipTag {
    tag: String,
    class: String,
    href: Option<String>,
    text: String,
}

/// The chips of a header fragment, in order.
fn chip_tags(head: &str) -> Vec<ChipTag> {
    static CHIP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<(a|span)\s([^>]*\bclass="(chip[^"]*)"[^>]*)>([^<]*)</(?:a|span)>"#).unwrap()
    });
    static HREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\bhref="([^"]*)""#).unwrap());
    CHIP.captures_iter(head)
        .map(|c| ChipTag {
            tag: c[1].to_string(),
            class: c[3].to_string(),
            href: HREF.captures(&c[2]).map(|h| h[1].to_string()),
            text: c[4].to_string(),
        })
        .collect()
}

/// What is wrong with the chips of `head`, judged one by one. A chip is a link with its own
/// non-empty href; a `<span>` is allowed only for a class in `unlinked_ok` (O1: the page it would
/// open is a top-level doc's page, so it is not written). A chip shows only when its count is
/// not zero, so a chip with no link is never a zero count.
fn chip_problems(head: &str, unlinked_ok: &[&str]) -> Vec<String> {
    let chips = chip_tags(head);
    let mut out = vec![];
    for want in ["chip chip-since", "chip chip-review"] {
        if !chips.iter().any(|c| c.class == want) {
            out.push(format!("no `{want}` chip"));
        }
    }
    for c in &chips {
        if !(c.text.starts_with('+') && c.text[1..].parse::<u32>().is_ok_and(|n| n > 0)) {
            out.push(format!(
                "the chip text {:?} is not a count above zero",
                c.text
            ));
        }
        match (c.tag.as_str(), c.href.as_deref()) {
            ("a", Some(h)) if !h.trim().is_empty() => {}
            ("a", _) => out.push(format!("`{}` is a link with no href", c.class)),
            _ if unlinked_ok.contains(&c.class.as_str()) => {}
            _ => out.push(format!("`{}` is a <{}> with no link", c.class, c.tag)),
        }
    }
    out
}

#[test]
fn sr10_each_chip_must_link_on_its_own() {
    // guard of the guard: a nonzero chip that is a span, or a link with an empty href, fails even
    // when the other chip is a good link
    let good = r##"<a class="pill" href="x/"><span class="pill-text">v1</span></a> <a class="chip chip-since" href="changes-since.html" title="t">+2</a> <a class="chip chip-review" href="changes.html" title="t">+3</a>"##;
    assert_eq!(chip_problems(good, &[]), Vec::<String>::new());
    let since_span = good.replace(
        r#"<a class="chip chip-since" href="changes-since.html" title="t">+2</a>"#,
        r#"<span class="chip chip-since" title="t">+2</span>"#,
    );
    let found = chip_problems(&since_span, &[]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("chip chip-since"), "{found:?}");
    let review_span = good.replace(
        r#"<a class="chip chip-review" href="changes.html" title="t">+3</a>"#,
        r#"<span class="chip chip-review" title="t">+3</span>"#,
    );
    assert_eq!(chip_problems(&review_span, &[]).len(), 1);
    // O1: the span is allowed for the class whose page a doc owns, and for that class only
    assert!(chip_problems(&review_span, &["chip chip-review"]).is_empty());
    assert_eq!(chip_problems(&since_span, &["chip chip-review"]).len(), 1);
    let empty = good.replace(r#"href="changes.html""#, r#"href="""#);
    assert_eq!(chip_problems(&empty, &["chip chip-review"]).len(), 1);
    let missing = good.replace(r#" href="changes.html""#, "");
    assert_eq!(chip_problems(&missing, &[]).len(), 1);
    // a chip that is lost shows too, and a zero count is not a chip
    let lost = good.replace(
        r#" <a class="chip chip-review" href="changes.html" title="t">+3</a>"#,
        "",
    );
    assert_eq!(chip_problems(&lost, &[]).len(), 1);
    let zero = good.replace(">+3<", ">+0<");
    assert_eq!(chip_problems(&zero, &[]).len(), 1);
}

#[test]
fn sr10_review_pages_work_without_javascript() {
    // the tutorial of this view holds an edited block and a new block
    let (host, scope, prefix) = mixed_view_live();
    let view = host.fetch(&scope, prefix, true);
    for page in [CHANGED_DOC, "changes.html"] {
        let html = read(view.path(), page);
        let words = texts(&html);
        // the changed document: the marks of its own blocks, in its article
        let own = if page == CHANGED_DOC {
            texts(&article_of(page, &html))
        } else {
            words.clone()
        };
        for want in ["Edited", "New"] {
            assert!(
                own.iter().any(|t| t == want),
                "{page}: the {want} word shows without the script: {own:?}"
            );
        }
        assert!(
            words
                .iter()
                .any(|t| t.starts_with("Reviewing view · editing")),
            "{page}: the review bar's sentence: {words:?}"
        );
        let head = version_fragment(page, &html);
        let head_words = runs_of(&head);
        assert!(
            head_words.iter().any(|t| t == "v1.0.0"),
            "{page}: the pill text: {head_words:?}"
        );
        // each chip is a word and a link of its own: no chip stands in for the other
        let found = chip_problems(&head, &[]);
        assert!(found.is_empty(), "{page}: {found:?} in {head}");
        for chip in chip_tags(&head) {
            assert!(
                head_words.contains(&chip.text),
                "{page}: the chip text {:?} is visible text: {head_words:?}",
                chip.text
            );
            let want = match chip.class.as_str() {
                "chip chip-since" => "changes-since.html",
                // the review chip leads to the summary, which holds the diff without the script
                _ => "changes.html",
            };
            assert!(
                chip.href.as_deref().is_some_and(|h| h.ends_with(want)),
                "{page}: `{}` opens {want}: {head}",
                chip.class
            );
        }
        assert!(
            head.contains("<a class=\"pill\" href=\""),
            "{page}: the pill is a link: {head}"
        );
    }
}

#[test]
fn sr10_no_review_page_shows_a_call_to_action() {
    // guard (k-N0EJF, D-X61DT): nothing on the site acts, waits on a person or counts reviews
    let g = golden_live();
    let (dh, _stat) = disagree_live();
    let mut sites: Vec<_> = sr10_scopes(&g)
        .into_iter()
        .map(|(what, scope, prefix)| (what, g.live.fetch(scope, prefix, true)))
        .collect();
    sites.push(("disagreements", dh.fetch(&Scope::Canon, "", true)));
    let mut seen = 0;
    for (what, site) in &sites {
        for page in page_set(site.path()) {
            let html = unhidden(&read(site.path(), &page));
            seen += 1;
            for bad in ["Approve", "needs you", "ready to", "waiting on", "<form"] {
                assert!(!html.contains(bad), "{what} {page} holds {bad:?}");
            }
        }
    }
    assert!(seen > 40, "guard: {seen} pages were scanned");
    let versions = versions_page(&g);
    for bad in ["Approve", "needs you", "ready to", "waiting on", "<form"] {
        assert!(!versions.contains(bad), "the versions page holds {bad:?}");
    }
}

#[test]
fn sr10_the_header_has_no_storage_words() {
    // guard (F11, C1): the visible words of the version fragment hold no storage word; the word
    // `landings` of the landings chip's hover is in `title` and `aria-label` only
    let f11 = rdy_qa::site::PATTERNS
        .iter()
        .find_map(|(id, rule)| match rule {
            rdy_qa::site::Rule::ChromeContext(rx) if *id == "F11" => Some(rx.clone()),
            _ => None,
        })
        .expect("the F11 rule");
    let g = golden_live();
    let hover_attrs = Regex::new(r#"\s(?:title|aria-label)="[^"]*""#).unwrap();
    let mut hovered = 0;
    let mut seen = 0;
    for (what, scope, prefix) in sr10_scopes(&g) {
        let served = g.live.fetch(scope, prefix, true);
        for page in page_set(served.path()) {
            let html = read(served.path(), &page);
            if is_redirect(&html) {
                continue;
            }
            let head = version_fragment(&format!("{what} {page}"), &html);
            seen += 1;
            for run in runs_of(&head) {
                assert!(
                    !f11.is_match(&run),
                    "{what} {page}: a storage word in the header: {run:?}"
                );
            }
            // take the hover attributes out: the word may stay nowhere else
            let bare = hover_attrs.replace_all(&head, "").into_owned();
            assert!(
                !bare.to_lowercase().contains("landing"),
                "{what} {page}: `landings` outside title and aria-label: {bare}"
            );
            if head.contains("landings since") {
                hovered += 1;
                assert!(
                    hovers(&head)
                        .iter()
                        .any(|h| h.contains("landings since v1.0.0")),
                    "{what} {page}: the hover reads N landings since the tag: {head}"
                );
            }
        }
    }
    assert!(seen > 40, "guard: {seen} headers were read");
    assert!(hovered > 0, "guard: some header carries the landings hover");
}

#[test]
fn sr10_no_review_chrome_says_draft() {
    // guard (D-BMX8P, O9): "draft" is the lifecycle word only. The header, the review bar, the
    // list heading, the change pages, the colophon and the versions page never use it for a view
    // or a proposal. Doc content is not read: an author may write the word.
    let g = golden_live();
    let says_draft = |what: &str, text: &str| {
        assert!(
            !text.to_lowercase().contains("draft"),
            "{what} says draft: {text:?}"
        );
    };
    let mut seen = 0;
    for (what, scope, prefix) in [
        ("view:editing", &g.view, g.view_prefix.as_str()),
        (
            "the proposal version",
            &g.proposal,
            g.proposal_prefix.as_str(),
        ),
    ] {
        let served = g.live.fetch(scope, prefix, true);
        let pages = page_set(served.path());
        for page in &pages {
            let html = read(served.path(), page);
            if is_redirect(&html) {
                continue;
            }
            let at = format!("{what} {page}");
            // the header: the pill, the chips, every aria-label and title
            let head = inner_of(&html, "class=\"head\"").expect("the header");
            says_draft(&format!("{at} header"), &words_of(&head));
            for h in hovers(&head) {
                says_draft(&format!("{at} header hover"), &h);
            }
            says_draft(
                &format!("{at} version"),
                &words_of(&version_fragment(&at, &html)),
            );
            let bar = inner_of(&html, "class=\"review-bar\"").expect("the review bar");
            says_draft(&format!("{at} review bar"), &words_of(&bar));
            for h in hovers(&bar) {
                says_draft(&format!("{at} review bar hover"), &h);
            }
            let colophon = inner_of(&html, "class=\"colophon\"").expect("the colophon");
            says_draft(&format!("{at} colophon"), &words_of(&colophon));
            seen += 1;
        }
        // the home page's list heading
        let home = read(served.path(), "index.html");
        let heading = inner_of(&home, "id=\"review-changes\"")
            .unwrap_or_else(|| panic!("{what}: the home page lists the changes"));
        says_draft(&format!("{what} home list heading"), &heading);
        assert!(
            heading.starts_with("Changes in this "),
            "{what}: the list heading: {heading}"
        );
        // the change pages: the h1 and the sentence under it
        for page in ["changes.html", "changes-since.html"] {
            if !pages.iter().any(|p| p == page) {
                continue;
            }
            let html = read(served.path(), page);
            let h1 = inner_of(&html, "<h1").expect("an h1");
            says_draft(&format!("{what} {page} h1"), &h1);
            let lede = inner_of(&html, "class=\"lede\"").expect("the sentence");
            says_draft(&format!("{what} {page} sentence"), &lede);
        }
    }
    assert!(seen > 20, "guard: {seen} pages were read");
    // the versions page: the h1, the h2s and the menu's labels
    let page = versions_page(&g);
    let titles = rdy_qa::site::page_titles(&page);
    assert!(
        titles.len() >= 4,
        "guard: an h1 and three lists: {titles:?}"
    );
    for (_, t) in &titles {
        says_draft("the versions page heading", t);
    }
    for h in hovers(&page) {
        says_draft("the versions page label", &h);
    }
    assert_eq!(titles[0].1, "Versions, proposals, views and profiles");
}

#[test]
fn sr10_the_helpers_read_hidden_attributes_and_nested_elements() {
    // a bare `hidden` goes; a word inside a quoted value stays
    assert_eq!(
        unhidden(
            r#"<div class="a" hidden><p title="a hidden word" hidden="">x hidden y</p><button hidden>b</button></div>"#
        ),
        r#"<div class="a"><p title="a hidden word">x hidden y</p><button>b</button></div>"#
    );
    // the element is read to its own close, through a nested element of the same name
    let html = r#"<p><span class="v" data-rdy-version><span>a</span> <span>b</span></span><span>c</span></p>"#;
    assert_eq!(
        inner_of(html, "data-rdy-version").as_deref(),
        Some("<span>a</span> <span>b</span>")
    );
    assert_eq!(inner_of(html, "no-such-marker"), None);
}
