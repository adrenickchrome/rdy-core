//! spv5 (D-CWC91; D-2F29G, D-X61DT, D-BMX8P): the live menu gains a fourth list, Profiles, with
//! "All docs" first. The list is read in the displayed scope from stored profile metadata alone;
//! a scope row keeps the current profile by its id; the list caps its selection work; the key
//! resolves no selection; the script fetches the pill's own context. Nothing on it acts or waits
//! on a person.
use rdy_engine::site_scope::ScopeRef;
use rdy_engine::site_target::SiteTarget;
use rdy_engine::{ProposalInfo, Rdy, ReadState, Scope, publication};
use rdy_model::{Block, BlockId, Hash};
use rdy_qa::fixtures::{approver, land, project, raw, raw_commit, write, writer};
use rdy_render::menu::{
    Menu, MenuRow, build_versions_for, docs_changed, menu, menu_for, menu_key_for,
    resolutions_for_tests,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;

// --- helpers ------------------------------------------------------------------------------------

fn ctx(scope: ScopeRef, profile: Option<&str>) -> SiteTarget {
    SiteTarget::Menu {
        scope,
        profile: profile.map(str::to_string),
    }
}

/// Writes `ops` to `view`; returns the ids of the blocks it created, by local name.
fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    write(r, view, ops)
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
}

fn doc(name: &str) -> Value {
    json!({"op": "create", "type": "doc", "id": format!("${name}"),
        "fields": {"name": name, "title": name.to_uppercase(), "doc_type": "page"}})
}

fn profile_op(
    local: &str,
    key: &str,
    title: &str,
    audience: &str,
    select: &[&str],
    admin: &str,
) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${local}"),
        "parent": admin,
        "fields": {"key": key, "title": title, "audience": audience, "select": select}})
}

/// A project with docs `a`, `b` and `c`, an `admin` doc and three landed profiles:
/// `handbook` (public, one doc), `agents` (also titled "Handbook"; org, two docs) and `zeta`
/// (titled "Archive"; org, one doc). The tag `v0.9.0` is before the profiles, `v1.0.0` after.
struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    a: String,
    ap: String,
    b: String,
    c: String,
    admin: String,
    handbook: String,
    agents: String,
    zeta: String,
}

fn world() -> World {
    let (dir, mut r) = project();
    let ids = put(
        &mut r,
        "docs",
        json!([
            doc("a"),
            json!({"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Alpha."}),
            doc("b"),
            doc("c"),
            doc("admin")
        ]),
    );
    land(&mut r, "docs").unwrap();
    r.tag(&approver(), "v0.9.0", None, None).unwrap();
    let [a, ap, b, c, admin] = ["$a", "$ap", "$b", "$c", "$admin"].map(|n| ids[n].clone());
    let made = put(
        &mut r,
        "profiles",
        json!([
            profile_op("handbook", "handbook", "Handbook", "public", &[&a], &admin),
            profile_op("agents", "agents", "Handbook", "org", &[&a, &b], &admin),
            profile_op("zeta", "zeta", "Archive", "org", &[&c], &admin),
        ]),
    );
    land(&mut r, "profiles").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    World {
        _dir: dir,
        a,
        ap,
        b,
        c,
        admin,
        handbook: made["$handbook"].clone(),
        agents: made["$agents"].clone(),
        zeta: made["$zeta"].clone(),
        r,
    }
}

fn propose(r: &mut Rdy, view: &str, title: &str) -> ProposalInfo {
    let summary = rdy_qa::intake::summary_for(r, view);
    r.propose(&writer(), view, title, summary).unwrap()
}

fn menu_of(st: &ReadState, c: &SiteTarget) -> Menu {
    menu_for(st, c, &mut |s| docs_changed(st, s)).unwrap()
}

/// (label, meta, href, current) of each row.
fn rows(rows: &[MenuRow]) -> Vec<(String, String, Option<String>, bool)> {
    rows.iter()
        .map(|r| {
            (
                r.label.clone(),
                r.meta.join(" · "),
                r.href.clone(),
                r.current,
            )
        })
        .collect()
}

fn row(
    label: &str,
    meta: &str,
    href: &str,
    current: bool,
) -> (String, String, Option<String>, bool) {
    (label.into(), meta.into(), Some(href.into()), current)
}

fn page(st: &ReadState, c: &SiteTarget) -> String {
    let b = build_versions_for(st, c, &mut |s| docs_changed(st, s)).unwrap();
    String::from_utf8(b.files["index.html"].clone()).unwrap()
}

// --- the list -----------------------------------------------------------------------------------

#[test]
fn spv5_profiles_are_sorted_counted_and_marked_in_the_displayed_scope() {
    // a project with no stored profile has no Profiles list at all
    let (_d0, r0) = project();
    {
        let st = r0.read();
        let m = menu_of(&st, &ctx(ScopeRef::Canon, None));
        assert!(m.profiles.is_empty() && m.profiles_omitted == 0, "{m:?}");
        let html = page(&st, &ctx(ScopeRef::Canon, None));
        assert!(
            !html.contains("Profiles</h2>") && !html.contains("All docs"),
            "{html}"
        );
        assert_eq!(html.matches("<ul class=\"vmenu-list\">").count(), 1);
    }

    let mut w = world();
    // canon moves on after the tag: one profile is renamed
    put(
        &mut w.r,
        "late",
        json!([{"op": "update", "id": w.handbook, "fields": {"title": "Handbook v2"}}]),
    );
    land(&mut w.r, "late").unwrap();
    // a view that selects more docs, and a proposal that adds a profile canon lacks
    put(
        &mut w.r,
        "chg",
        json!([{"op": "update", "id": w.handbook, "fields": {"select": [w.a, w.b, w.c]}}]),
    );
    put(
        &mut w.r,
        "only",
        json!([profile_op(
            "guide",
            "guide",
            "Draft guide",
            "public",
            &[&w.a, &w.b],
            &w.admin
        )]),
    );
    let p = propose(&mut w.r, "only", "A guide");
    let st = w.r.read();
    let at = |s: ScopeRef, key: Option<&str>| menu_of(&st, &ctx(s, key));

    // canon: All docs first, then title, key
    let canon = at(ScopeRef::Canon, None);
    assert_eq!(
        rows(&canon.profiles),
        vec![
            row("All docs", "", "/", true),
            row("Archive", "org · 1 doc", "/~profile/zeta/", false),
            row("Handbook", "org · 2 docs", "/~profile/agents/", false),
            row(
                "Handbook v2",
                "public · 1 doc",
                "/~profile/handbook/",
                false
            ),
        ]
    );
    assert_eq!(canon.profiles_omitted, 0);

    // the tag holds its own revisions: duplicate titles sort by key
    let tag = at(ScopeRef::Tag("v1.0.0".into()), None);
    assert_eq!(
        rows(&tag.profiles),
        vec![
            row("All docs", "", "/~tag/v1.0.0/", true),
            row(
                "Archive",
                "org · 1 doc",
                "/~tag/v1.0.0/~profile/zeta/",
                false
            ),
            row(
                "Handbook",
                "org · 2 docs",
                "/~tag/v1.0.0/~profile/agents/",
                false
            ),
            row(
                "Handbook",
                "public · 1 doc",
                "/~tag/v1.0.0/~profile/handbook/",
                false
            ),
        ]
    );
    // a tag taken before any profile has no list
    assert!(at(ScopeRef::Tag("v0.9.0".into()), None).profiles.is_empty());

    // a view's changed selection counts in that view only
    let view = at(ScopeRef::View("chg".into()), None);
    assert_eq!(
        rows(&view.profiles),
        vec![
            row("All docs", "", "/~view/chg/", true),
            row("Archive", "org · 1 doc", "/~view/chg/~profile/zeta/", false),
            row(
                "Handbook",
                "org · 2 docs",
                "/~view/chg/~profile/agents/",
                false
            ),
            row(
                "Handbook v2",
                "public · 3 docs",
                "/~view/chg/~profile/handbook/",
                false
            ),
        ]
    );

    // a proposal-only profile is listed in the proposal, and nowhere in canon
    let prefix = format!("/~proposal/{}/", p.r#ref);
    let prop = at(ScopeRef::Proposal(p.r#ref.clone()), None);
    assert_eq!(
        rows(&prop.profiles)[..3],
        [
            row("All docs", "", &prefix, true),
            row(
                "Archive",
                "org · 1 doc",
                &format!("{prefix}~profile/zeta/"),
                false
            ),
            row(
                "Draft guide",
                "public · 2 docs",
                &format!("{prefix}~profile/guide/"),
                false
            ),
        ]
    );
    assert!(canon.profiles.iter().all(|r| r.label != "Draft guide"));

    // the current row: a stored profile marks its own row, never All docs
    let marked = at(ScopeRef::Tag("v1.0.0".into()), Some("handbook"));
    let current: Vec<_> = marked.profiles.iter().filter(|r| r.current).collect();
    assert_eq!(current.len(), 1, "{marked:?}");
    assert_eq!(
        current[0].href.as_deref(),
        Some("/~tag/v1.0.0/~profile/handbook/")
    );
    // an explicit implicit profile marks no row, including All docs
    let implicit = at(ScopeRef::Canon, Some("implicit:org"));
    assert!(implicit.profiles.iter().all(|r| !r.current), "{implicit:?}");
    assert_eq!(
        implicit.profiles.len(),
        4,
        "the implicit profile is not listed"
    );
    // a key folds to the stored profile
    assert_eq!(
        at(ScopeRef::Canon, Some("agents"))
            .profiles
            .iter()
            .filter(|r| r.current)
            .count(),
        1
    );

    // the page: the section, the mark, the contextual Home and menu links
    let html = page(&st, &ctx(ScopeRef::Canon, Some("agents")));
    let at_html = |needle: &str| {
        html.find(needle)
            .unwrap_or_else(|| panic!("{needle} in {html}"))
    };
    assert!(at_html("<h2>Versions</h2>") < at_html("<h2>Proposals</h2>"));
    assert!(at_html("<h2>Views</h2>") < at_html("<h2>Profiles</h2>"));
    assert_eq!(html.matches("<ul class=\"vmenu-list\">").count(), 4);
    assert!(
        html.contains("<a href=\"/~profile/agents/\" aria-current=\"page\">Handbook</a> <span class=\"vmenu-meta\">org · 2 docs (current)</span>"),
        "{html}"
    );
    assert_eq!(html.matches("aria-current=\"page\"").count(), 3, "{html}");
    assert!(html.contains(
        "<a href=\"/~profile/zeta/\">Archive</a> <span class=\"vmenu-meta\">org · 1 doc</span>"
    ));
    assert!(html.contains("<a href=\"/\">All docs</a>"), "{html}");
    assert!(
        html.contains("<h1>Versions, proposals, views and profiles</h1>"),
        "{html}"
    );
    assert!(
        html.contains(
            "<nav class=\"vmenu\" aria-label=\"Versions, proposals, views and profiles\">"
        ),
        "{html}"
    );
    assert!(
        html.contains("<nav class=\"crumbs\" aria-label=\"Breadcrumb\"><a href=\"/~profile/agents/\">Home</a><span class=\"crumb-sep\" aria-hidden=\"true\"></span><span aria-current=\"page\">Versions, proposals, views and profiles</span></nav>"),
        "{html}"
    );
    assert!(
        html.contains("href=\"/~versions/?scope=canon&amp;profile=agents\" aria-current=\"page\">Versions, proposals, views and profiles</a>"),
        "{html}"
    );
    for word in [
        "Approve",
        "approve",
        "ready",
        "needs you",
        "waiting",
        "<form",
        "<button",
    ] {
        let main = &html[html.find("<main").unwrap()..html.find("</main>").unwrap()];
        assert!(!main.contains(word), "{word:?} in {main}");
    }
    // the bare source menu is the menu of canon with no profile
    let m = menu(&st, &mut |s| docs_changed(&st, s)).unwrap();
    assert_eq!(m, canon);
}

#[test]
fn spv5_scope_switch_keeps_identity_or_falls_back() {
    let mut w = world();
    // a view renames the key of the profile; another view removes it and reuses its key
    put(
        &mut w.r,
        "ren",
        json!([{"op": "update", "id": w.handbook, "fields": {"key": "manual"}}]),
    );
    put(
        &mut w.r,
        "reuse",
        json!([
            {"op": "delete", "id": w.handbook},
            profile_op("again", "handbook", "Handbook again", "public", &[&w.a], &w.admin),
        ]),
    );
    let st = w.r.read();
    let hrefs = |m: &Menu| -> Vec<(String, String)> {
        m.versions
            .iter()
            .chain(&m.views)
            .map(|r| (r.label.clone(), r.href.clone().unwrap_or_default()))
            .collect()
    };
    let expect: Vec<(&str, &str)> = vec![
        ("Latest", "/~profile/handbook/"),
        ("v1.0.0", "/~tag/v1.0.0/~profile/handbook/"),
        // the tag before the profile holds no such id: full source
        ("v0.9.0", "/~tag/v0.9.0/"),
        // the key is renamed in the view: the link follows the id
        ("ren", "/~view/ren/~profile/manual/"),
        // the id is gone and another profile reuses the key: no selection
        ("reuse", "/~view/reuse/"),
    ];
    let m = menu_of(&st, &ctx(ScopeRef::Canon, Some("handbook")));
    let got = hrefs(&m);
    let want: Vec<(String, String)> = expect
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    assert_eq!(got, want, "{m:?}");

    // from the view that renamed the key, canon's row uses canon's key for the same id
    let m = menu_of(&st, &ctx(ScopeRef::View("ren".into()), Some("manual")));
    assert_eq!(m.versions[0].href.as_deref(), Some("/~profile/handbook/"));
    assert_eq!(
        m.views
            .iter()
            .find(|r| r.label == "ren")
            .unwrap()
            .href
            .as_deref(),
        Some("/~view/ren/~profile/manual/")
    );

    // no selection: every row links to its full source
    let m = menu_of(&st, &ctx(ScopeRef::Canon, None));
    assert_eq!(
        hrefs(&m).into_iter().map(|(_, h)| h).collect::<Vec<_>>(),
        [
            "/",
            "/~tag/v1.0.0/",
            "/~tag/v0.9.0/",
            "/~view/ren/",
            "/~view/reuse/"
        ]
    );

    // the implicit profile persists across every scope, holding or not
    let m = menu_of(
        &st,
        &ctx(ScopeRef::View("ren".into()), Some("implicit:org")),
    );
    assert_eq!(
        hrefs(&m).into_iter().map(|(_, h)| h).collect::<Vec<_>>(),
        [
            "/~profile/implicit%3Aorg/",
            "/~tag/v1.0.0/~profile/implicit%3Aorg/",
            "/~tag/v0.9.0/~profile/implicit%3Aorg/",
            "/~view/ren/~profile/implicit%3Aorg/",
            "/~view/reuse/~profile/implicit%3Aorg/",
        ]
    );

    // a block that is not a profile in a scope is an error, not a fallback: the id of a doc
    // cannot be selected by key, and an unknown key is refused
    let err = menu_for(&st, &ctx(ScopeRef::Canon, Some("nosuchkey")), &mut |s| {
        docs_changed(&st, s)
    })
    .unwrap_err();
    assert_eq!(err.code, "E_NOT_FOUND", "{err}");
    // unrelated profiles on the same scope are untouched: a stored profile of canon
    let _ = (&w.agents, &w.zeta);
}

#[test]
fn spv5_scope_rows_reuse_source_counts() {
    let mut w = world();
    put(
        &mut w.r,
        "chg",
        json!([{"op": "update", "id": w.handbook, "fields": {"select": [w.a, w.b, w.c]}}]),
    );
    put(
        &mut w.r,
        "other",
        json!([{"op": "update", "id": w.ap, "body": "Alpha page."}]),
    );
    let _ = propose(&mut w.r, "other", "Other");
    let st = w.r.read();

    let run = |c: &SiteTarget| {
        let calls: RefCell<Vec<Scope>> = RefCell::new(vec![]);
        let m = menu_for(&st, c, &mut |s| {
            calls.borrow_mut().push(s.clone());
            Ok(match s {
                Scope::View(_) => 7,
                _ => 3,
            })
        })
        .unwrap();
        (m, calls.into_inner())
    };
    let (source, source_calls) = run(&ctx(ScopeRef::Canon, None));
    let (stored, stored_calls) = run(&ctx(ScopeRef::Canon, Some("handbook")));
    let (implicit, implicit_calls) = run(&ctx(ScopeRef::Canon, Some("implicit:org")));
    let (other_scope, other_calls) = run(&ctx(ScopeRef::View("chg".into()), Some("handbook")));

    // the supplied callback gives every proposal and view count, once, in every context
    assert_eq!(source_calls.len(), 3, "{source_calls:?}");
    for calls in [&stored_calls, &implicit_calls, &other_calls] {
        assert_eq!(calls, &source_calls);
    }
    let metas = |m: &Menu| -> Vec<String> {
        m.proposals
            .iter()
            .chain(&m.views)
            .map(|r| r.meta.join(" · "))
            .collect()
    };
    assert_eq!(
        metas(&source),
        [
            "by qa · 3 docs changed",
            "by qa · 7 docs changed",
            "by qa · 7 docs changed"
        ]
    );
    for m in [&stored, &implicit, &other_scope] {
        assert_eq!(metas(m), metas(&source));
        assert_eq!(
            m.proposals
                .iter()
                .chain(&m.views)
                .map(|r| r.label.clone())
                .collect::<Vec<_>>(),
            source
                .proposals
                .iter()
                .chain(&source.views)
                .map(|r| r.label.clone())
                .collect::<Vec<_>>()
        );
    }
    // the profile changes only the links
    let links = |m: &Menu| -> Vec<Option<String>> {
        m.proposals
            .iter()
            .chain(&m.views)
            .map(|r| r.href.clone())
            .collect()
    };
    assert_ne!(links(&stored), links(&source));
    assert!(links(&stored)[1..].iter().any(|h| {
        h.as_deref()
            .is_some_and(|h| h.contains("~profile/handbook/"))
    }));
    // the supplied callback is also the daemon's: a failing count fails the menu
    let err = menu_for(&st, &ctx(ScopeRef::Canon, Some("handbook")), &mut |_| {
        Err(rdy_engine::RdyError::new("E_BAD_INPUT", "count", "fix"))
    })
    .unwrap_err();
    assert_eq!(err.message, "count");
}

#[test]
fn spv5_profiles_list_caps_selection_work() {
    let (_dir, mut r) = project();
    let ids = put(&mut r, "docs", json!([doc("a"), doc("admin")]));
    land(&mut r, "docs").unwrap();
    let (a, admin) = (ids["$a"].clone(), ids["$admin"].clone());
    let make = |r: &mut Rdy, view: &str, from: usize, to: usize| {
        let ops: Vec<Value> = (from..to)
            .map(|i| {
                profile_op(
                    &format!("p{i}"),
                    &format!("k{i:03}"),
                    &format!("T{i:03}"),
                    "org",
                    &[&a],
                    &admin,
                )
            })
            .collect();
        put(r, view, Value::Array(ops))
    };
    // 101 stored profiles: the 100 smallest are listed, one is omitted
    for from in (0..101).step_by(40) {
        make(&mut r, "bulk", from, (from + 40).min(101));
    }
    land(&mut r, "bulk").unwrap();

    let c = ctx(ScopeRef::Canon, None);
    let before = resolutions_for_tests();
    let m = {
        let st = r.read();
        menu_of(&st, &c)
    };
    assert_eq!(
        resolutions_for_tests() - before,
        100,
        "only the retained profiles resolve"
    );
    assert_eq!(m.profiles.len(), 101, "All docs and 100 profiles");
    assert_eq!(m.profiles_omitted, 1);
    assert_eq!(m.profiles[0].label, "All docs");
    let labels: Vec<String> = m.profiles[1..].iter().map(|r| r.label.clone()).collect();
    let want: Vec<String> = (0..100).map(|i| format!("T{i:03}")).collect();
    assert_eq!(labels, want, "the smallest 100 by title, in order");
    assert!(m.profiles.iter().all(|r| r.href.is_some()));
    {
        let st = r.read();
        let html = page(&st, &c);
        assert!(
            html.contains(
                "<p class=\"vmenu-meta\">1 more profile: ask your agent for its link</p>"
            ),
            "{html}"
        );
        assert!(!html.contains("T100"), "an omitted profile has no row");
    }

    // 102: the same 100 rows, the count says so
    make(&mut r, "more", 101, 102);
    land(&mut r, "more").unwrap();
    let st = r.read();
    let before = resolutions_for_tests();
    let m2 = menu_of(&st, &c);
    assert_eq!(resolutions_for_tests() - before, 100);
    assert_eq!(m2.profiles_omitted, 2);
    assert_eq!(rows(&m2.profiles), rows(&m.profiles), "stable ordering");
    let html = page(&st, &c);
    assert!(
        html.contains(
            "<p class=\"vmenu-meta\">2 more profiles: ask your agent for their links</p>"
        ),
        "{html}"
    );
    assert!(
        !html.contains("<a href=\"/~profile/k101/\""),
        "overflow is not a row: {html}"
    );

    // a current profile outside the cap marks no row, All docs included
    let out = menu_of(&st, &ctx(ScopeRef::Canon, Some("k101")));
    assert_eq!(out.profiles.len(), 101);
    assert!(out.profiles.iter().all(|r| !r.current), "{out:?}");
    // and one inside the cap marks its row
    let inside = menu_of(&st, &ctx(ScopeRef::Canon, Some("k005")));
    assert_eq!(inside.profiles.iter().filter(|r| r.current).count(), 1);
    drop(st);

    // invalid metadata fails the build, even for a profile the list would omit
    let bad: Block = raw(
        "p-BAD01",
        "publication_profile",
        Some(admin.as_str()),
        "zz",
        &[
            ("key", "zzzz"),
            ("title", "ZZZ"),
            ("audience", "org"),
            ("select", "not a list"),
        ],
        "",
    );
    raw_commit(&mut r, "bad", vec![bad]);
    let st = r.read();
    let scope = Scope::View("bad".into());
    let want = publication::profile_at(&st, &scope, "p-BAD01").unwrap_err();
    assert_eq!(want.code, "E_VALIDATION");
    let got = menu_for(&st, &ctx(ScopeRef::View("bad".into()), None), &mut |s| {
        docs_changed(&st, s)
    })
    .unwrap_err();
    assert_eq!(
        (got.code, got.message.as_str(), got.fix.as_str()),
        (want.code, want.message.as_str(), want.fix.as_str())
    );
    let key_err = menu_key_for(&st, &ctx(ScopeRef::View("bad".into()), None), 0).unwrap_err();
    assert_eq!(key_err.message, want.message);
    let _ = BlockId::parse("p-BAD01").unwrap();
}

#[test]
fn spv5_menu_key_tracks_profile_and_selection_changes() {
    let mut w = world();
    put(
        &mut w.r,
        "kv",
        json!([{"op": "update", "id": w.ap, "body": "First."}]),
    );
    let kv = ctx(ScopeRef::View("kv".into()), Some("handbook"));
    let key = |r: &Rdy, c: &SiteTarget| {
        let st = r.read();
        menu_key_for(&st, c, 0).unwrap()
    };
    let before = resolutions_for_tests();
    let k0 = key(&w.r, &kv);
    assert_eq!(k0, key(&w.r, &kv), "a self-read is stable");

    // a body edit of a selected doc moves the key through the scope's head
    put(
        &mut w.r,
        "kv",
        json!([{"op": "update", "id": w.ap, "body": "Second."}]),
    );
    let k1 = key(&w.r, &kv);
    assert_ne!(k1, k0, "a selected doc's change moves the key");
    // a change of the profile itself moves it
    put(
        &mut w.r,
        "kv",
        json!([{"op": "update", "id": w.handbook, "fields": {"title": "Renamed"}}]),
    );
    let k2 = key(&w.r, &kv);
    assert_ne!(k2, k1, "the profile's version moves the key");
    let k3 = key(&w.r, &ctx(ScopeRef::View("kv".into()), None));
    assert_ne!(k3, k2, "the selected profile is part of the context");

    // each part of the context moves it
    let st = w.r.read();
    let keys: Vec<Hash> = [
        ctx(ScopeRef::Canon, None),
        ctx(ScopeRef::Canon, Some("handbook")),
        ctx(ScopeRef::Canon, Some("agents")),
        ctx(ScopeRef::Canon, Some("implicit:org")),
        ctx(ScopeRef::Tag("v1.0.0".into()), None),
        ctx(ScopeRef::View("kv".into()), None),
    ]
    .iter()
    .map(|c| menu_key_for(&st, c, 0).unwrap())
    .collect();
    for (i, a) in keys.iter().enumerate() {
        for b in &keys[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // the menu of canon has one spelling
    assert_eq!(
        menu_key_for(&st, &SiteTarget::Source(ScopeRef::Versions), 0).unwrap(),
        menu_key_for(&st, &ctx(ScopeRef::Canon, None), 0).unwrap()
    );
    // the pack generation moves it
    assert_ne!(
        menu_key_for(&st, &ctx(ScopeRef::Canon, None), 1).unwrap(),
        keys[0]
    );
    drop(st);

    // a version of the displayed scope: a new tag moves the base key
    let k4 = key(&w.r, &ctx(ScopeRef::Canon, None));
    w.r.tag(&approver(), "v1.0.1", None, None).unwrap();
    assert_ne!(key(&w.r, &ctx(ScopeRef::Canon, None)), k4);

    // the key reads metadata alone: it resolved no selection in all of this
    assert_eq!(resolutions_for_tests(), before, "no selection resolved");
    let _ = (&w.b, &w.c, &w.agents, &w.zeta);
}

#[test]
fn spv5_menu_key_moves_when_the_list_crosses_its_cap() {
    let (_dir, mut r) = project();
    let ids = put(&mut r, "docs", json!([doc("a"), doc("admin")]));
    land(&mut r, "docs").unwrap();
    let (a, admin) = (ids["$a"].clone(), ids["$admin"].clone());
    let make = |r: &mut Rdy, view: &str, from: usize, to: usize| {
        let ops: Vec<Value> = (from..to)
            .map(|i| {
                profile_op(
                    &format!("p{i}"),
                    &format!("k{i:03}"),
                    &format!("T{i:03}"),
                    "org",
                    &[&a],
                    &admin,
                )
            })
            .collect();
        put(r, view, Value::Array(ops));
    };
    make(&mut r, "bulk", 0, 40);
    make(&mut r, "bulk", 40, 80);
    make(&mut r, "bulk", 80, 100);
    land(&mut r, "bulk").unwrap();
    let c = ctx(ScopeRef::Canon, None);
    let k100 = {
        let st = r.read();
        assert_eq!(menu_of(&st, &c).profiles_omitted, 0);
        menu_key_for(&st, &c, 0).unwrap()
    };
    make(&mut r, "one", 100, 101);
    land(&mut r, "one").unwrap();
    let st = r.read();
    assert_eq!(menu_of(&st, &c).profiles_omitted, 1);
    assert_ne!(menu_key_for(&st, &c, 0).unwrap(), k100);
}

// --- the script ---------------------------------------------------------------------------------

/// The cases, after the harness of `render_menu`: the pill's own `href` is the request URL.
const PROFILE_CASES: &str = r##"
(async () => {
  const out = { errors: [] };
  process.on('unhandledRejection', (e) => out.errors.push(String((e && e.message) || e)));
  const A = '/~versions/?scope=view%3Aw&profile=docs';
  const B = '/~versions/?scope=tag%3A1.0.0';
  // two pages, two contexts: each fetches its own pill's href, and nothing else (the harness
  // keeps one focus, so each page is read before the next is made)
  const a = setup({ menu: true, href: A });
  const before = Object.keys(a.window).sort();
  a.click(a.pill); await tick();
  out.a = a.state();
  // a refresh asks for the same URL again
  a.log.streams[0].listeners.reload.forEach((f) => f({}));
  await tick();
  out.a_refreshed = a.state();
  // the keyboard closes it and gives the focus back to the pill
  a.key('Escape');
  out.a_closed = a.state();
  const b = setup({ menu: true, href: B });
  b.click(b.pill); await tick();
  out.b = b.state();
  // a label event replaces the pill: the refresh keeps the URL the menu was opened with
  const c = setup({ menu: true, href: A });
  c.click(c.pill); await tick();
  c.relabel(true);
  c.log.streams[0].listeners.reload.forEach((f) => f({}));
  await tick();
  out.c = c.state();
  // a failed fetch follows the same href
  const d = setup({ menu: true, href: A, fail: true });
  d.click(d.pill); await tick();
  out.d = d.state();
  // a pill whose href leaves the origin is a plain link: no fetch, no preventDefault
  const e = setup({ menu: true, href: 'https://elsewhere.example/~versions/' });
  out.e_prevented = e.click(e.pill); await tick();
  out.e = e.state();
  const f = setup({ menu: true, href: '//elsewhere.example/~versions/' });
  out.f_prevented = f.click(f.pill); await tick();
  out.f = f.state();
  const g = setup({ menu: true, href: '/\\elsewhere.example/' });
  out.g_prevented = g.click(g.pill); await tick();
  out.g = g.state();
  out.leaked = Object.keys(a.window).sort().filter((k) => !before.includes(k));
  console.log(JSON.stringify(out));
})().catch((e) => { console.error(e); process.exit(1); });
"##;

#[test]
fn spv5_script_fetches_the_pills_context_and_keeps_focus() {
    if !rdy_qa::skip::node_available() {
        rdy_qa::skip::skip(
            "spv5_script_fetches_the_pills_context_and_keeps_focus",
            rdy_qa::skip::Why::Prerequisite,
            "node is not installed",
        );
        return;
    }
    let js = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../rdy-render/theme/rdy-standard/js/menu.js"
    );
    let dom = crate::render_menu::MENU_DOM;
    let o = std::process::Command::new("node")
        .args(["-e", &format!("{dom}{PROFILE_CASES}"), "--", js])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let a_url = "/~versions/?scope=view%3Aw&profile=docs";
    let b_url = "/~versions/?scope=tag%3A1.0.0";
    assert_eq!(v["errors"], json!([]), "no callback throws: {v}");
    // two contexts, two requests: each page fetched its own pill's href
    assert_eq!(v["a"]["fetched"], json!([a_url]), "{v}");
    assert_eq!(v["b"]["fetched"], json!([b_url]), "{v}");
    assert_eq!(v["a"]["open"], true, "{v}");
    assert_eq!(
        v["a"]["label"], "Versions, proposals, views and profiles",
        "{v}"
    );
    assert_eq!(v["a"]["focus"], "/", "the first link has the focus: {v}");
    // a refresh repeats the URL; the old stream closes and a new one opens
    assert_eq!(v["a_refreshed"]["fetched"], json!([a_url, a_url]), "{v}");
    assert_eq!(v["a_refreshed"]["streams"][0]["closed"], true, "{v}");
    assert_eq!(v["a_refreshed"]["streams"][1]["closed"], false, "{v}");
    // a replaced pill: the refresh keeps the menu's URL
    assert_eq!(v["c"]["fetched"], json!([a_url, a_url]), "{v}");
    assert_eq!(v["c"]["replaced"], true, "{v}");
    assert_eq!(v["c"]["open"], true, "{v}");
    // a failed fetch follows the pill's own link
    assert_eq!(v["d"]["followed"], a_url, "{v}");
    assert_eq!(v["d"]["open"], false, "{v}");
    // the keyboard closes it and gives the focus back
    assert_eq!(v["a_closed"]["open"], false, "{v}");
    assert_eq!(v["a_closed"]["focus"], "pill", "{v}");
    assert_eq!(v["a_closed"]["expanded"], "false", "{v}");
    assert!(
        v["a_closed"]["streams"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["closed"] == true),
        "{v}"
    );
    // only a same-origin path is fetched; any other href is left to the browser
    for k in ["e", "f", "g"] {
        assert_eq!(v[format!("{k}_prevented")], false, "{k}: {v}");
        assert_eq!(v[k]["fetched"], json!([]), "{k}: {v}");
        assert_eq!(v[k]["open"], false, "{k}: {v}");
    }
    // the script leaves nothing on the window object
    assert_eq!(v["leaked"], json!([]), "{v}");
}
