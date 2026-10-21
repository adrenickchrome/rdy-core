//! spv3: the live site's targets (D-CWC91, D-DK03K): the closed grammar of what a listener serves,
//! the root each target has, and the menu and event contexts it writes.
use rdy_engine::site_scope::ScopeRef;
use rdy_engine::site_target::SiteTarget;

fn profile(scope: ScopeRef, key: &str) -> SiteTarget {
    SiteTarget::Profile {
        scope,
        key: key.to_string(),
    }
}

fn menu(scope: ScopeRef, profile: Option<&str>) -> SiteTarget {
    SiteTarget::Menu {
        scope,
        profile: profile.map(str::to_string),
    }
}

/// One percent-decoding pass, as the HTTP layer does it.
fn decode_once(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[test]
fn spv3_profile_roots_encode_exactly_one_segment() {
    let p = |s: &str| ScopeRef::Proposal(s.to_string());
    assert_eq!(profile(ScopeRef::Canon, "docs").root(), "/~profile/docs/");
    assert_eq!(
        profile(ScopeRef::Tag("1.2.0".into()), "docs").root(),
        "/~tag/1.2.0/~profile/docs/"
    );
    assert_eq!(
        profile(ScopeRef::View("a b/c".into()), "docs").root(),
        "/~view/a%20b%2Fc/~profile/docs/"
    );
    assert_eq!(
        profile(p("P-abcd@2"), "docs").root(),
        "/~proposal/P-abcd@2/~profile/docs/",
        "a pinned proposal version"
    );
    assert_eq!(
        profile(p("P-abcd"), "docs").root(),
        "/~proposal/P-abcd/~profile/docs/",
        "the latest version of a proposal"
    );
    // a dot is encoded, so a dotted key stays one segment and is never a step
    assert_eq!(
        profile(ScopeRef::Canon, "a.b-c_d").root(),
        "/~profile/a%2Eb-c_d/"
    );
    // the implicit token: its colon is encoded too
    assert_eq!(
        profile(p("P-abcd@1"), "implicit:org").root(),
        "/~proposal/P-abcd@1/~profile/implicit%3Aorg/"
    );
    // no double decoding: a `%` of a view name is encoded, so one pass gives the name back
    let v = profile(ScopeRef::View("100%2F".into()), "docs");
    assert_eq!(v.root(), "/~view/100%252F/~profile/docs/");
    let segment = v.root().split('/').nth(2).unwrap().to_string();
    assert_eq!(decode_once(&segment), "100%2F");
    // the key segment decodes once to the key
    let segment = profile(ScopeRef::Canon, "a.b").root();
    let segment = segment.split('/').nth(2).unwrap().to_string();
    assert_eq!(decode_once(&segment), "a.b");
    // only-mode and source canon are the site's root; other source targets keep the scope prefix
    assert_eq!(SiteTarget::Source(ScopeRef::Canon).root(), "/");
    assert_eq!(SiteTarget::ProfileOnly { key: "docs".into() }.root(), "/");
    assert_eq!(
        SiteTarget::Source(ScopeRef::View("w".into())).root(),
        "/~view/w/"
    );
    assert_eq!(
        SiteTarget::Source(ScopeRef::Tag("1.0.0".into())).root(),
        "/~tag/1.0.0/"
    );
    // the scope each target reads
    assert_eq!(
        SiteTarget::ProfileOnly { key: "docs".into() }.scope(),
        ScopeRef::Canon
    );
    assert_eq!(profile(p("P-abcd@2"), "docs").scope(), p("P-abcd@2"));
    assert_eq!(
        menu(ScopeRef::Tag("1.0.0".into()), None).scope().text(),
        "tag:1.0.0"
    );
}

#[test]
fn spv3_target_validation_is_closed() {
    let ok = |t: SiteTarget| assert!(t.validate().is_ok(), "{t:?}");
    let bad = |t: SiteTarget| {
        let e = t.validate().expect_err(&format!("{t:?}"));
        assert_eq!(e.code, "E_BAD_INPUT", "{t:?}");
        assert_eq!(e.message, "invalid live site target");
        assert_eq!(e.fix, "use a source scope and a publication profile key");
    };
    let key32 = "a".repeat(32);
    let key33 = "a".repeat(33);
    ok(profile(ScopeRef::Canon, &key32));
    bad(profile(ScopeRef::Canon, &key33));
    ok(profile(ScopeRef::Canon, "implicit:org"));
    ok(profile(ScopeRef::Canon, "a.b-c_d"));
    ok(SiteTarget::ProfileOnly { key: "docs".into() });
    ok(menu(ScopeRef::Canon, None));
    ok(menu(ScopeRef::View("w".into()), Some("docs")));
    ok(SiteTarget::Source(ScopeRef::Versions));
    ok(SiteTarget::Source(ScopeRef::Proposal("P-abcd@3".into())));
    for key in [
        "",
        // block ids
        "D-CWC91",
        "p-DTQH3",
        "tb-QCTS0",
        "n-DD240",
        // the proposal-id forms
        "P-abcd",
        "p-abcd",
        // separators and steps
        ".",
        "..",
        "a/b",
        "a\\b",
        "a\0b",
        "/",
        "a/",
        "a..b",
        "a-",
        "-a",
        "a%2Eb",
        "a b",
        // Unicode
        "caf\u{e9}",
        "\u{43a}\u{43b}\u{44e}\u{447}",
        "a\u{202e}b",
        // not folded: one spelling of a key
        "Docs",
        // the implicit token is exact
        "implicit:other",
        "Implicit:org",
        "implicit:org ",
        "implicit:",
    ] {
        bad(profile(ScopeRef::Canon, key));
        bad(SiteTarget::ProfileOnly { key: key.into() });
        bad(menu(ScopeRef::Canon, Some(key)));
    }
    // nested Versions, and scopes the site's grammar does not spell
    bad(profile(ScopeRef::Versions, "docs"));
    bad(menu(ScopeRef::Versions, None));
    bad(menu(ScopeRef::Versions, Some("docs")));
    for scope in [
        ScopeRef::Proposal("inflight".into()),
        ScopeRef::Proposal("p-abcd".into()),
        ScopeRef::Proposal("P-ABCD".into()),
        ScopeRef::Proposal("P-abcd@0".into()),
        ScopeRef::Proposal("P-abcd@01".into()),
        ScopeRef::Proposal("P-abc".into()),
        ScopeRef::Proposal(String::new()),
        ScopeRef::View(String::new()),
        ScopeRef::View("a:b".into()),
        ScopeRef::Tag("not a version".into()),
        ScopeRef::Tag(String::new()),
    ] {
        bad(SiteTarget::Source(scope.clone()));
        bad(profile(scope.clone(), "docs"));
        bad(menu(scope, None));
    }
}

#[test]
fn spv3_menu_and_event_contexts_are_deterministic() {
    let p = |s: &str| ScopeRef::Proposal(s.to_string());
    // the unfiltered canon menu keeps the base spellings
    for t in [
        SiteTarget::Source(ScopeRef::Versions),
        menu(ScopeRef::Canon, None),
    ] {
        assert_eq!(t.menu_url().as_deref(), Some("/~versions/"), "{t:?}");
        assert_eq!(t.events_query(), "scope=versions", "{t:?}");
    }
    assert_eq!(
        SiteTarget::Source(ScopeRef::Canon).menu_url().as_deref(),
        Some("/~versions/")
    );
    assert_eq!(
        SiteTarget::Source(ScopeRef::Canon).events_query(),
        "scope=canon"
    );
    // only-mode: no menu, and the scope is fixed
    let only = SiteTarget::ProfileOnly { key: "docs".into() };
    assert_eq!(only.menu_url(), None);
    assert_eq!(only.events_query(), "scope=canon");
    // scope first, then the profile; the menu mark last
    let cases: [(SiteTarget, &str, &str); 9] = [
        (
            profile(ScopeRef::Canon, "docs"),
            "/~versions/?scope=canon&profile=docs",
            "scope=canon&profile=docs",
        ),
        (
            profile(p("P-abcd@2"), "implicit:org"),
            "/~versions/?scope=proposal%3AP-abcd%402&profile=implicit%3Aorg",
            "scope=proposal:P-abcd@2&profile=implicit:org",
        ),
        (
            menu(ScopeRef::Canon, Some("a.b")),
            "/~versions/?scope=canon&profile=a.b",
            "scope=canon&profile=a.b&menu=1",
        ),
        (
            menu(ScopeRef::View("a b".into()), None),
            "/~versions/?scope=view%3Aa%20b",
            "scope=view:a%20b&menu=1",
        ),
        (
            menu(ScopeRef::Tag("1.0.0".into()), None),
            "/~versions/?scope=tag%3A1.0.0",
            "scope=tag:1.0.0&menu=1",
        ),
        (
            menu(p("P-abcd"), Some("docs")),
            "/~versions/?scope=proposal%3AP-abcd&profile=docs",
            "scope=proposal:P-abcd&profile=docs&menu=1",
        ),
        (
            SiteTarget::Source(ScopeRef::View("a/b~c_d-e.f".into())),
            "/~versions/?scope=view%3Aa%2Fb~c_d-e.f",
            "scope=view:a%2Fb~c_d-e.f",
        ),
        (
            SiteTarget::Source(ScopeRef::View("caf\u{e9}%".into())),
            "/~versions/?scope=view%3Acaf%C3%A9%25",
            "scope=view:caf%C3%A9%25",
        ),
        (
            SiteTarget::Source(ScopeRef::Tag("1.2.3-rc.1".into())),
            "/~versions/?scope=tag%3A1.2.3-rc.1",
            "scope=tag:1.2.3-rc.1",
        ),
    ];
    for (t, url, events) in cases {
        assert!(t.validate().is_ok(), "{t:?}");
        assert_eq!(t.menu_url().as_deref(), Some(url), "{t:?}");
        assert_eq!(t.events_query(), events, "{t:?}");
        // deterministic: the same target spells the same bytes
        assert_eq!(t.clone().events_query(), events);
    }
}
