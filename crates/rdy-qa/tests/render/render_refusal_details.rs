//! ui3 (D-N507H, D-94BT2, D-E8P8J, D-WQ5GC, D-YXK1V, D-0VCQ5; system/site p-GHG75, p-F63VB,
//! tb-QCTS0): a rendered refusal shows its message in plain words, and an info control beside it
//! opens a popover with the error code and its fix. The control is a native `details` element: it
//! needs no script. The generated 404 uses it and stays outside every document output; every
//! transport refusal keeps its status and its plain-text answer.
use rdy_daemon::site::guard::{Refusal, refusal};
use rdy_daemon::site::server::MAX_CONNECTIONS;
use rdy_engine::publication::{IMPLICIT_ORG, ReadPublicationSource};
use rdy_engine::site_scope::ScopeRef;
use rdy_engine::site_target::SiteTarget;
use rdy_engine::*;
use rdy_qa::daemon::Hosted;
use rdy_qa::fixtures::{approver, land, project, raw, raw_commit, write};
use rdy_qa::golden::tiny;
use rdy_qa::http;
use rdy_render::live::build_live;
use rdy_render::refusal::{self as refusal_html, Detail};
use rdy_render::render_input::capture_inputs;
use rdy_render::{Mode, build_profile_site, build_publication, build_site};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};

const MARKER: &str = "<html lang=\"en\" data-rdy-kind=\"not-found\">";
const PAGE_MESSAGE: &str =
    "There is no page at this address. It may have moved, or the link may be wrong.";
const SCOPE_MESSAGE: &str = "This link names a version, view, proposal or profile that is not on the site now. A view or proposal leaves the site when its work lands or stops.";
const PAGE_FIX: &str = "Search these pages, or go to the home page.";
const SCOPE_FIX: &str = "See the latest docs, or open the list of versions, proposals and views.";
const KEPT: &str = "the top-level doc named 404 has the page 404.html: the site's not-found page is left out. Rename the doc to publish it.";
const PLAIN: &str = "Not found. Ask your agent for the site's link (rdy_site), or run rdy site.";
const BROKEN: &str =
    "This page could not be built. The person who runs this site can find the reason in rdy's log.";

/// The exact refusal block of `message`, `code` and `fix` (already escaped).
fn block(message: &str, code: &str, fix: &str) -> String {
    format!(
        "<div class=\"refusal\"><p class=\"refusal-message\">{message}</p><details class=\"refusal-detail\"><summary aria-label=\"Error details\"><span aria-hidden=\"true\">ⓘ</span></summary><div class=\"refusal-popover\"><p><code>{code}</code></p><p>{fix}</p></div></details></div>"
    )
}

/// A project named `Lost project`, `site.base_path` = `base`, holding `ops`, landed; its store
/// closed.
fn seeded(base: &str, ops: Value) -> (tempfile::TempDir, BTreeMap<String, String>) {
    let (dir, r) = project();
    drop(r);
    std::fs::write(
        dir.path().join(".rdy.yaml"),
        format!("pack: rdy-standard\nname: \"Lost project\"\nsite:\n  base_path: {base}\n"),
    )
    .unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let ids = write(&mut r, "work", ops).unwrap().created;
    land(&mut r, "work").unwrap();
    (dir, ids.into_iter().collect())
}

fn guide() -> Value {
    json!([
        {"op": "create", "type": "doc", "id": "$other", "fields": {"name": "other", "title": "Other page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$os", "parent": "$other", "fields": {"title": "Other section"}},
        {"op": "create", "type": "prose", "parent": "$os", "body": "Other words."},
        {"op": "create", "type": "doc", "id": "$intro", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$intro", "fields": {"title": "Start here"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Intro words."}
    ])
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Asserts `html` holds the 404 of the variant with `message`, `fix` and the linked `sentence`.
#[track_caller]
fn assert_404(html: &str, message: &str, fix: &str, sentence: &str, what: &str) {
    assert!(
        html.contains(&block(message, "404", fix)),
        "{what}: the refusal block: {html}"
    );
    assert!(html.contains("<h1>Page not found</h1>"), "{what}");
    // the linked sentence stays visible below the block
    let at = html.find("</details></div>").expect("the block") + "</details></div>".len();
    assert!(
        html[at..].trim_start().starts_with(sentence),
        "{what}: {html}"
    );
    // the block itself holds no script, button or handler
    let block_html = &html[html.find("<div class=\"refusal\">").unwrap()..at];
    assert!(
        !block_html.contains("<script") && !block_html.contains("<button"),
        "{what}"
    );
}

const PAGE_SENTENCE: &str = "<p><a href=\"#search\">Search these pages</a>, or go to the <a href=\"{root}index.html\">home page</a>.</p>";

fn page_sentence(root: &str) -> String {
    PAGE_SENTENCE.replace("{root}", root)
}

const SCOPE_SENTENCE: &str = "<p>See the <a href=\"/index.html\">latest docs</a>, or open the <a href=\"/~versions/\">list of versions, proposals and views</a>.</p>";

#[test]
fn ui3_error_details_escape_every_text_and_work_without_script() {
    // plain text: the exact structure
    let d = Detail {
        code: "E_NOPE".into(),
        fix: "Try again.".into(),
    };
    assert_eq!(
        refusal_html::html("It failed.", &d).unwrap(),
        block("It failed.", "E_NOPE", "Try again.")
    );

    // hostile text: each of the three is text, never markup
    let d = Detail {
        code: "<script>alert('code')</script>".into(),
        fix: "<img src=x onerror=alert(1)> & \"fix\"".into(),
    };
    let html = refusal_html::html("<b onclick=\"x()\">message</b>", &d).unwrap();
    let mut rest = html.as_str();
    let mut tags = vec![];
    while let Some(i) = rest.find('<') {
        let j = rest[i..].find('>').expect("a closed tag") + i;
        tags.push(&rest[i..=j]);
        rest = &rest[j + 1..];
    }
    // only the partial's own tags: no tag from a text
    let own = [
        "<div class=\"refusal\">",
        "<p class=\"refusal-message\">",
        "</p>",
        "<details class=\"refusal-detail\">",
        "<summary aria-label=\"Error details\">",
        "<span aria-hidden=\"true\">",
        "</span>",
        "</summary>",
        "<div class=\"refusal-popover\">",
        "<p>",
        "<code>",
        "</code>",
        "</details>",
        "</div>",
    ];
    for t in &tags {
        assert!(own.contains(t), "{t} is not the partial's own tag: {html}");
    }
    assert!(!rest.contains('<') && !rest.contains('>'));
    for escaped in [
        "&lt;b onclick=",
        "&lt;script&gt;alert(&#39;code&#39;)",
        "&lt;img src=x onerror=alert(1)&gt; &amp; &quot;fix&quot;",
    ] {
        assert!(html.contains(escaped), "{escaped}: {html}");
    }
    // the code and the fix are inside the details; the message is outside it
    let details = html.find("<details").unwrap();
    assert!(html.find("message").unwrap() < details);
    assert!(html.find("&lt;script&gt;").unwrap() > details);
    assert!(html.find("&lt;img").unwrap() > details);
    // no script, no handler attribute, no request id, no button in the summary
    for gone in [
        "<script", "<button", "<a ", "request", "Request", "req-", " id=",
    ] {
        assert!(!html.contains(gone), "{gone}: {html}");
    }
    let tag_attrs = tags.join(" ");
    assert!(!tag_attrs.contains("onclick") && !tag_attrs.contains("onerror"));
}

#[test]
fn ui3_both_404_variants_have_exact_words_and_details() {
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$hid", "fields": {"name": "hidden", "title": "HIDDENCANARY doc", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$hs", "parent": "$hid", "fields": {"title": "Hidden section"}},
        {"op": "create", "type": "prose", "parent": "$hs", "body": "Hidden words."},
        {"op": "create", "type": "doc", "id": "$shown", "fields": {"name": "shown", "title": "Shown page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ss", "parent": "$shown", "fields": {"title": "Shown section"}},
        {"op": "create", "type": "prose", "parent": "$ss", "body": "Shown words."}
    ]);
    for base in ["/", "/docs/"] {
        let (dir, ids) = seeded(base, ops.clone());
        let mut r = Rdy::open(dir.path()).unwrap();
        write(
            &mut r,
            "work",
            json!([{"op": "create", "type": "publication_profile", "parent": ids["$hid"],
                "fields": {"key": "shownorg", "title": "Shown site", "audience": "org", "select": [ids["$shown"]]}}]),
        )
        .unwrap();
        land(&mut r, "work").unwrap();
        let st = r.read();

        // a static render at the base: the Page variant
        let site = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
        let html = text(&site.files["404.html"]);
        assert!(html.contains(MARKER), "{base}");
        assert_404(
            &html,
            PAGE_MESSAGE,
            PAGE_FIX,
            &page_sentence(base),
            &format!("static {base}"),
        );
        assert!(!html.contains(SCOPE_MESSAGE));

        // the selected profile's brand, the same words
        let site = build_profile_site(&st, "canon", "shownorg").unwrap();
        let html = text(&site.files["404.html"]);
        assert!(html.contains("<title>Page not found · Shown site</title>"));
        assert!(html.contains("<span class=\"brand-name\">Shown site</span>"));
        assert_404(
            &html,
            PAGE_MESSAGE,
            PAGE_FIX,
            &page_sentence(base),
            &format!("profile {base}"),
        );
        assert!(!html.contains("Lost project") && !html.contains("HIDDENCANARY"));
    }

    // live: canon's build holds both variants; another scope has the Page variant alone
    let (dir, ids) = seeded("/", guide());
    let mut r = Rdy::open(dir.path()).unwrap();
    write(
        &mut r,
        "side",
        json!([{"op": "create", "type": "prose", "parent": ids["$os"], "body": "Side words."}]),
    )
    .unwrap();
    let st = r.read();
    let canon = build_live(&st, &Scope::Canon, &SiteTarget::Source(ScopeRef::Canon)).unwrap();
    let page = text(canon.not_found.page.as_deref().expect("the Page variant"));
    let scope = text(canon.not_found.scope.as_deref().expect("the Scope variant"));
    assert_404(
        &page,
        PAGE_MESSAGE,
        PAGE_FIX,
        &page_sentence("/"),
        "open page",
    );
    assert_404(
        &scope,
        SCOPE_MESSAGE,
        SCOPE_FIX,
        SCOPE_SENTENCE,
        "open scope",
    );
    assert!(!page.contains(SCOPE_MESSAGE) && !scope.contains(PAGE_MESSAGE));
    for html in [&page, &scope] {
        assert!(html.contains("<title>Page not found · Lost project</title>"));
        for gone in [
            "/_rdy/live.js",
            "data-events",
            "_rdy/events",
            "class=\"pill\"",
        ] {
            assert!(!html.contains(gone), "{gone}");
        }
    }
    let view = build_live(
        &st,
        &Scope::View("side".into()),
        &SiteTarget::Source(ScopeRef::View("side".into())),
    )
    .unwrap();
    assert!(view.not_found.scope.is_none());
    let page = text(view.not_found.page.as_deref().expect("the Page variant"));
    assert_404(
        &page,
        PAGE_MESSAGE,
        PAGE_FIX,
        &page_sentence("/~view/side/"),
        "unavailable scope",
    );
}

#[test]
fn ui3_404_stays_out_of_all_document_outputs() {
    let (dir, _ids) = seeded("/docs/", guide());
    let mut r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let site = build_site(&st, &Scope::Canon, Mode::Static).unwrap();
    let html = text(&site.files["404.html"]);
    assert!(html.contains(MARKER) && html.contains("class=\"refusal-detail\""));
    let live = build_site(&st, &Scope::Canon, Mode::Live { root: "/" }).unwrap();
    assert!(!live.files.contains_key("404.html"));
    // the counts are the documentation pages' own
    let pages = site
        .files
        .keys()
        .filter(|p| p.ends_with(".html") && !p.starts_with("go/"))
        .count();
    assert_eq!(site.summary.pages, pages - 1);
    assert_eq!(site.summary.pages, live.summary.pages);
    assert_eq!(site.summary.redirects, live.summary.redirects);
    // no other output holds the generated page, its words, its code or its control
    for (path, bytes) in &site.files {
        if path == "404.html" {
            continue;
        }
        let t = String::from_utf8_lossy(bytes);
        if !(path.ends_with(".html") || path.ends_with(".txt") || path.ends_with(".json")) {
            continue;
        }
        for word in [
            "404.html",
            "Page not found",
            "not-found",
            PAGE_MESSAGE,
            PAGE_FIX,
            "refusal-detail",
            "Error details",
        ] {
            assert!(!t.contains(word), "{path} holds {word}");
        }
    }
    for llms in ["llms.txt", "llms-full.txt"] {
        assert!(site.files.contains_key(llms), "{llms}");
    }
    let index: Value = serde_json::from_slice(&site.files["search-index.json"]).unwrap();
    let entries = index["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(entries.iter().all(|e| !e.to_string().contains("404.html")));

    // a publication keeps no generated 404
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let tag = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == "v1.0.0")
        .unwrap()
        .1;
    let landing = r.store().read_tag(&tag).unwrap().landing;
    let read = r.read();
    let inputs = capture_inputs(&read, &Scope::At(landing)).unwrap();
    let source = ReadPublicationSource::with_settings(
        &read,
        &inputs.pack,
        &inputs.title,
        inputs.description.as_deref(),
    );
    let m = read.materialized(&Scope::At(landing)).unwrap();
    let a = build_publication(&source, &m, "v1.0.0", false, IMPLICIT_ORG, &inputs).unwrap();
    assert!(!a.files.contains_key("404.html"));
    assert!(!a.manifest.files.iter().any(|e| e.path == "404.html"));
    for (path, bytes) in &a.files {
        let t = String::from_utf8_lossy(bytes);
        assert!(!t.contains("not-found"), "{path}");
        // the stylesheet holds the control's rules; no page holds the control
        if path.ends_with(".html") {
            assert!(!t.contains("refusal-detail"), "{path}");
        }
    }
    drop(r);

    // an authored 404 doc keeps 404.html, gives the one warning, and has no refusal block
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$lost", "fields": {"name": "404", "title": "Authored lost page", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$ls", "parent": "$lost", "fields": {"title": "Where to go"}},
        {"op": "create", "type": "prose", "parent": "$ls", "body": "AUTHOREDCANARY words."}
    ]);
    let (dir, _ids) = seeded("/", ops);
    let r = Rdy::open(dir.path()).unwrap();
    let site = build_site(&r.read(), &Scope::Canon, Mode::Static).unwrap();
    let html = text(&site.files["404.html"]);
    assert!(html.contains("AUTHOREDCANARY words."));
    assert!(!html.contains("data-rdy-kind") && !html.contains("refusal-detail"));
    assert_eq!(
        site.summary.warnings.iter().filter(|w| *w == KEPT).count(),
        1,
        "{:?}",
        site.summary.warnings
    );
}

// --- transport refusals -------------------------------------------------------------------------

fn sock(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Asserts `r` is exactly the plain answer of `f`: status, type, text, and no HTML.
#[track_caller]
fn assert_plain(r: &http::Response, f: Refusal, body: &str, what: &str) {
    let want = refusal(f);
    assert_eq!(r.status, want.status, "{what}: {}", r.head);
    assert_eq!(text(&r.body), body, "{what}");
    assert_eq!(text(&want.body), body, "{what}");
    assert_eq!(
        r.header("content-type"),
        Some("text/plain; charset=utf-8"),
        "{what}: {}",
        r.head
    );
    assert!(!text(&r.body).contains('<'), "{what}");
}

#[test]
fn ui3_transport_refusals_keep_plain_bodies() {
    // the texts and statuses of the closed set
    for (f, status, body) in [
        (Refusal::TooLarge, 431, "The request head is too large."),
        (Refusal::Malformed, 400, "Bad request."),
        (Refusal::Body, 400, "This site takes no request body."),
        (
            Refusal::Method,
            405,
            "This site is read-only: it answers GET and HEAD only.",
        ),
        (
            Refusal::Host,
            403,
            "This site answers only to its own addresses.",
        ),
        (
            Refusal::Origin,
            403,
            "This site answers only requests from its own pages.",
        ),
        (Refusal::NotFound, 404, PLAIN),
        (
            Refusal::Busy,
            503,
            "Too many live pages are open. Close some tabs, then reload this one.",
        ),
        (Refusal::Broken, 500, BROKEN),
    ] {
        let r = refusal(f);
        assert_eq!((r.status, text(&r.body).as_str()), (status, body));
    }

    // a live site with a page that cannot be built
    let (dir, mut r) = tiny();
    let ids = write(
        &mut r,
        "w1",
        json!([{"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}]),
    )
    .unwrap()
    .created;
    land(&mut r, "w1").unwrap();
    let admin = ids
        .iter()
        .find(|(k, _)| *k == "$admin")
        .unwrap()
        .1
        .to_string();
    raw_commit(
        &mut r,
        "bad",
        vec![raw(
            "p-BAD01",
            "publication_profile",
            Some(&admin),
            "zz",
            &[
                ("key", "zzzz"),
                ("title", "ZZZ"),
                ("audience", "org"),
                ("select", "not a list"),
            ],
            "",
        )],
    );
    drop(r);
    let h = Hosted::start_in(dir);
    let url = h
        .run
        .as_ref()
        .unwrap()
        .shared()
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    let port: u16 = url
        .strip_prefix("http://127.0.0.1:")
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap();
    let own = format!("Host: 127.0.0.1:{port}");
    let raw_req = |req: String| http::raw(sock(port), &req);
    // each refusal comes before any themed lookup: the path is one that gets a themed 404
    for (req, f, body) in [
        (
            format!("POST /nosuch.html HTTP/1.1\r\n{own}\r\nContent-Length: 0\r\n\r\n"),
            Refusal::Method,
            "This site is read-only: it answers GET and HEAD only.",
        ),
        (
            format!("GET /nosuch.html HTTP/1.1\r\n{own}\r\nContent-Length: 4\r\n\r\nbody"),
            Refusal::Body,
            "This site takes no request body.",
        ),
        (
            format!("GET /nosuch.html HTTP/1.1\r\nHost: rebound.invalid:{port}\r\n\r\n"),
            Refusal::Host,
            "This site answers only to its own addresses.",
        ),
        (
            format!("GET /nosuch.html HTTP/1.1\r\n{own}\r\nOrigin: http://rebound.invalid\r\n\r\n"),
            Refusal::Origin,
            "This site answers only requests from its own pages.",
        ),
        (
            format!(
                "GET /nosuch.html HTTP/1.1\r\n{own}\r\nX-Pad: {}\r\n\r\n",
                "a".repeat(9000)
            ),
            Refusal::TooLarge,
            "The request head is too large.",
        ),
        (
            "GET /~view/bad/~profile/zzzz/nosuch.html HTTP/1.1\r\n".to_string() + &own + "\r\n\r\n",
            Refusal::Broken,
            BROKEN,
        ),
    ] {
        let first = req[..req.find('\r').unwrap()].to_string();
        assert_plain(&raw_req(req), f, body, &first);
    }
    // a path the site does not hold is the themed page, so the plain answers above are the guard's
    let r = raw_req(format!("GET /nosuch.html HTTP/1.1\r\n{own}\r\n\r\n"));
    assert_eq!(r.status, 404, "{}", r.head);
    assert_eq!(
        r.header("content-type"),
        Some("text/html; charset=utf-8"),
        "{}",
        r.head
    );
    assert!(text(&r.body).contains("class=\"refusal-detail\""));
    // a connection past the limit gets the busy answer, whole
    let held: Vec<TcpStream> = (0..MAX_CONNECTIONS)
        .map(|_| TcpStream::connect(sock(port)).unwrap())
        .collect();
    let r = raw_req(format!("GET /nosuch.html HTTP/1.1\r\n{own}\r\n\r\n"));
    assert_plain(
        &r,
        Refusal::Busy,
        "Too many live pages are open. Close some tabs, then reload this one.",
        "busy",
    );
    drop(held);
}
