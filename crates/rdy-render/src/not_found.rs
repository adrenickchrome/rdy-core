//! The site's own not-found page (D-94BT2, D-E8P8J; system/site n-REYZ9): built in the site's
//! layout and theme from the selected site alone, with no author input and no template override.
//! It is an extra file, not a documentation page: no navigation, pager, search entry, llms file,
//! permalink or page count lists it. Every link it writes starts at the site's base (a page that
//! answers any missing path cannot lead anywhere relative to that path), and its html element
//! carries the `data-rdy-kind="not-found"` marker that tells it from an authored `404.html`.
use crate::nav::{Nav, NavEntry};
use crate::page::{self, Crumb, PageContext, SiteFacts};
use crate::{RdyError, assets, render_page};
use serde::Serialize;
use std::collections::BTreeMap;

/// The path of the generated page in a local static site.
pub(crate) const PAGE: &str = "404.html";

/// The render's warning when an authored top-level doc named 404 keeps `404.html` (D-E8P8J).
pub(crate) const KEPT: &str = "the top-level doc named 404 has the page 404.html: the site's not-found page is left out. Rename the doc to publish it.";

/// The page's heading, and the browser title's first part.
const HEADING: &str = "Page not found";

/// What the page says for a missing page.
const PAGE_MESSAGE: &str =
    "There is no page at this address. It may have moved, or the link may be wrong.";

/// What the page says for a link to a version, view, proposal or profile the site no longer has.
const SCOPE_MESSAGE: &str = "This link names a version, view, proposal or profile that is not on the site now. A view or proposal leaves the site when its work lands or stops.";

/// Which not-found page to build: a missing page of a site, or a missing scope of the live site
/// (a tag, view, proposal or profile that is not on it now).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotFoundKind {
    Page,
    Scope,
}

/// A built not-found page: its bytes, and the theme files it loads, by their site-relative paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotFoundPage {
    pub html: Vec<u8>,
    pub assets: BTreeMap<String, Vec<u8>>,
}

/// The part of the context only the not-found template reads.
#[derive(Serialize)]
struct Words {
    message: &'static str,
    scope: bool,
    home_href: String,
    latest_href: String,
    versions_href: String,
}

/// `href` as a page served at any path under `base` writes it: a fragment-only link stays on
/// the page; any other link, relative to the site's root, gets the base in front.
fn rooted(base: &str, href: &str) -> String {
    match href.starts_with('#') {
        true => href.to_string(),
        false => format!("{base}{href}"),
    }
}

/// The entries of `list` with every link rooted at `base`, and none of them the current page.
fn rooted_entries(base: &str, list: &mut [NavEntry]) {
    for e in list {
        e.href = rooted(base, &e.href);
        e.current = false;
        e.on_path = false;
        if let Some((_, href)) = &mut e.more {
            *href = rooted(base, href);
        }
        if let Some(href) = &mut e.change_href {
            *href = rooted(base, href);
        }
        rooted_entries(base, &mut e.children);
    }
}

/// The navigation of the site's home page, every link rooted at `base`, with no current page.
fn navigation(sh: &page::Shared, base: &str) -> Nav {
    let site = sh.site;
    let facts = SiteFacts {
        has_terms: !site.terms.is_empty(),
        has_questions: !site.open_questions.is_empty(),
        has_disagreements: !site.disagreements.is_empty(),
        review: sh.review,
        listed: sh.listed,
        old: sh.old_site,
    };
    let mut nav = crate::nav::build(site, None, "index.html", facts, &sh.review_anchors);
    for g in &mut nav.groups {
        g.current = false;
        if let Some(href) = &mut g.href {
            *href = rooted(base, href);
        }
        rooted_entries(base, &mut g.entries);
    }
    nav
}

/// The not-found page of the selected site `sh`, in the layout of `env` (the build's own
/// environment), with every link rooted at `base` (`/`, a project's `site.base_path`, or a
/// live scope's prefix). It reads nothing but `sh`: the brand and browser title read the
/// selected site's name (a profile's title on a profiled render), and the navigation is the home
/// page's. It shows no version label, chip, review bar, toolbar, trail, contents, pager or
/// colophon, and no live behaviour.
pub(crate) fn build(
    sh: &page::Shared,
    env: &minijinja::Environment<'static>,
    base: &str,
    kind: NotFoundKind,
) -> Result<NotFoundPage, RdyError> {
    let site = sh.site;
    let ctx = PageContext {
        site_name: sh.site_name.to_string(),
        site_description: None,
        lang: "en",
        title: HEADING.into(),
        page_url: PAGE.into(),
        kind: "not-found",
        root: base.to_string(),
        init_js: assets::INIT_JS,
        version: sh.label(),
        header: String::new(),
        crumbs: vec![Crumb {
            label: HEADING.into(),
            href: None,
        }],
        nav: navigation(sh, base),
        toc: Default::default(),
        pager: Default::default(),
        search_href: rooted(base, "search-index.json"),
        has_terms: !site.terms.is_empty(),
        has_questions: !site.open_questions.is_empty(),
        reviewing: false,
        review_bar: None,
        review_heading: None,
        live: false,
        items: vec![],
        // the not-found page has no title toolbar and nothing to copy (Task 9's Copy)
        title_tools: None,
        copy_page: None,
        body: Default::default(),
    };
    let words = Words {
        message: match kind {
            NotFoundKind::Page => PAGE_MESSAGE,
            NotFoundKind::Scope => SCOPE_MESSAGE,
        },
        scope: kind == NotFoundKind::Scope,
        home_href: rooted(base, "index.html"),
        latest_href: rooted(base, "index.html"),
        versions_href: rooted(base, "~versions/"),
    };
    let (html, slots) = render_page(
        env,
        "not_found.html",
        minijinja::context! {
            not_found => minijinja::Value::from_serialize(&words),
            ..minijinja::Value::from_serialize(&ctx)
        },
    )?;
    // the page writes no version label and no header: nothing on it is filled in later. Only
    // the template writes a slot, never the site's text, so a slot here is a template bug, and
    // an error in every build: the page would show an unfilled or stale label
    if !slots.is_empty() {
        return Err(RdyError::new(
            rdy_engine::E_IO,
            "template error: the not-found page wrote a label or header slot",
            "this is a bug in rdy-render templates",
        ));
    }
    Ok(NotFoundPage {
        html,
        assets: assets::files()
            .into_iter()
            .map(|(path, bytes)| (path.to_string(), bytes))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The not-found page of `kind` at `base` for canon of a small generated project.
    fn built(base: &str, kind: NotFoundKind) -> NotFoundPage {
        let dir = tempfile::tempdir().expect("tempdir");
        rdy_engine::testkit::generate_tree(dir.path(), 1, 2, 1);
        let r = rdy_engine::Rdy::open(dir.path()).expect("open");
        let read = r.read();
        let prepared = crate::prepare::source(
            &read,
            &rdy_engine::Scope::Canon,
            crate::prepare::Purpose::Site,
        )
        .expect("prepare");
        crate::with_shared(prepared, crate::Mode::Static, |sh, env, _| {
            build(sh, env, base, kind)
        })
        .expect("build")
        .0
    }

    #[test]
    fn dgm10_scope_variant_names_no_scope_and_links_canon() {
        let page = built("/", NotFoundKind::Scope);
        let html = String::from_utf8(page.html).expect("utf-8");
        assert!(html.contains(&format!("<p>{SCOPE_MESSAGE}</p>")), "{html}");
        assert!(
            html.contains("<p>See the <a href=\"/index.html\">latest docs</a>, or open the <a href=\"/~versions/\">list of versions, proposals and views</a>.</p>"),
            "{html}"
        );
        assert!(!html.contains(PAGE_MESSAGE) && !html.contains("href=\"#search\""));
        assert!(
            html.contains("<title>Page not found · ")
                && html.contains("data-rdy-kind=\"not-found\"")
        );
        // the theme files it loads come with it
        let want: BTreeMap<String, Vec<u8>> = assets::files()
            .into_iter()
            .map(|(p, b)| (p.to_string(), b))
            .collect();
        assert_eq!(page.assets, want);
    }

    /// Final review M1: a label or header slot on the not-found page is an error in every
    /// build, not a debug check. A live build then keeps no page, and the server gives the
    /// plain answer.
    #[test]
    fn dgm10_a_slot_on_the_not_found_page_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        rdy_engine::testkit::generate_tree(dir.path(), 1, 2, 1);
        let r = rdy_engine::Rdy::open(dir.path()).expect("open");
        let read = r.read();
        let prepared = crate::prepare::source(
            &read,
            &rdy_engine::Scope::Canon,
            crate::prepare::Purpose::Site,
        )
        .expect("prepare");
        crate::with_shared(prepared, crate::Mode::Static, |sh, env, _| {
            // the shipped template writes no slot
            assert!(build(sh, env, "/", NotFoundKind::Page).is_ok());
            let mut env = env.clone();
            env.add_template("not_found.html", "<p>{{ version.text | label }}</p>")
                .expect("template");
            let e = build(sh, &env, "/", NotFoundKind::Page).expect_err("a slot");
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                (
                    rdy_engine::E_IO,
                    "template error: the not-found page wrote a label or header slot",
                    "this is a bug in rdy-render templates"
                )
            );
            for kind in [NotFoundKind::Page, NotFoundKind::Scope] {
                assert_eq!(crate::live_not_found(sh, &env, "/", kind), None);
            }
            Ok(())
        })
        .expect("built");
    }

    #[test]
    fn dgm10_page_variant_roots_every_link_at_its_base() {
        let base = "/~view/work/";
        let html = String::from_utf8(built(base, NotFoundKind::Page).html).expect("utf-8");
        assert!(html.contains(&format!("<p>{PAGE_MESSAGE}</p>")));
        assert!(!html.contains(SCOPE_MESSAGE));
        for key in [" href=\"", " src=\"", " data-index=\""] {
            for v in html.split(key).skip(1) {
                let v = &v[..v.find('"').expect("closed")];
                assert!(v.starts_with(base) || v.starts_with('#'), "{key}{v}");
            }
        }
        assert!(html.contains(&format!("<a href=\"{base}index.html\">home page</a>")));
    }
}
