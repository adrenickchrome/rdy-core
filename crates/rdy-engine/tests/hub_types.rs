//! hsa1: the hub client's bounded contracts, token form and request identity. Pure types and
//! checks: no network, credential file or clock is read here.
use rdy_engine::hub_types::*;
use rdy_engine::{
    E_ACTOR_UNBOUND, E_BAD_INPUT, E_LIMIT, E_UPSTREAM_ACTOR_CLAIM, E_UPSTREAM_AUTH,
    E_UPSTREAM_ORIGIN, E_UPSTREAM_PROTOCOL, E_UPSTREAM_REFUSED, E_UPSTREAM_TOKEN,
    E_UPSTREAM_TRANSPORT, E_UPSTREAM_UNCERTAIN, RdyError, Session, SessionKind,
    upstream_publish_origin,
};
use rdy_model::Role;
use std::marker::PhantomData;

/// The settled token vectors that pass the client check.
const PASSING_TOKENS: [&str; 5] = [
    "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf",
    "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC",
    "rdyh_p_000000000000000000000000000000000000000025UVUe",
    "rdyx_a_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR2kpMVZ",
    // an unused kind letter, and a checksum that shows its 0 padding
    "rdyh_d_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR0JTskl",
];

/// The settled token vectors that fail the client check.
const FAILING_TOKENS: [&str; 7] = [
    // last checksum character changed
    "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYA",
    // first secret character changed
    "rdyh_a_1123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf",
    // prefix changed: the CRC covers it
    "rdyx_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf",
    // 52 bytes; the checksum matches
    "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabc48FkQz",
    // `-` is not in the alphabet; the checksum matches
    "rdyh_a_0123456789ABCDEFGHIJ-LMNOPQRSTUVWXYZabcd3q1jRl",
    // `rdyz_` is not a prefix; the checksum matches
    "rdyz_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd48L11G",
    // an upper-case kind letter; the checksum matches
    "rdyh_A_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd1hUGlF",
];

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
const REFRESH: &str = "rdyh_r_efghijklmnopqrstuvwxyz0123456789ABCDEFGH3qAvLC";
const DEVICE_CODE: &str = "device-code-sentinel-7Hq2";

fn assert_err(r: Result<impl std::fmt::Debug, RdyError>, code: &str, message: &str, fix: &str) {
    let e = r.expect_err("expected a refusal");
    assert_eq!(e.code, code, "{e}");
    assert_eq!(e.message, message, "{e}");
    assert_eq!(e.fix, fix, "{e}");
}

fn bad_url(r: Result<impl std::fmt::Debug, RdyError>) {
    assert_err(
        r,
        E_BAD_INPUT,
        "the hub address is not an HTTPS URL",
        "use an HTTPS address without credentials, a query or a fragment",
    );
}

fn limit(r: Result<impl std::fmt::Debug, RdyError>, limit: &str) {
    assert_err(
        r,
        E_LIMIT,
        &format!("hub input exceeds {limit}"),
        "reduce the input to the stated limit",
    );
}

fn unbound(r: Result<impl std::fmt::Debug, RdyError>) {
    assert_err(
        r,
        E_ACTOR_UNBOUND,
        "the session actor is not a hub actor",
        "set an actor in the hub's human, agent or ci form, then try again",
    );
}

fn protocol(r: Result<impl std::fmt::Debug, RdyError>) {
    assert_err(
        r,
        E_UPSTREAM_PROTOCOL,
        "the hub reply does not match the client contract",
        "check that this rdy version supports the hub's API",
    );
}

fn bad_label(r: Result<(), RdyError>) {
    assert_err(
        r,
        E_BAD_INPUT,
        "label must contain 1 to 64 characters without edge whitespace or control characters",
        "change label, or leave it out",
    );
}

fn bad_machine(r: Result<(), RdyError>) {
    assert_err(
        r,
        E_BAD_INPUT,
        "machine_name must contain 1 to 64 characters without edge whitespace or control characters",
        "change machine_name",
    );
}

fn bad_time(r: Result<impl std::fmt::Debug, RdyError>) {
    assert_err(
        r,
        E_BAD_INPUT,
        "compromised_since is not an RFC3339 time",
        "use an RFC3339 time with a time zone",
    );
}

fn origin(s: &str) -> Origin {
    Origin::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn agent_session(actor: &str, run: Option<&str>) -> Session {
    Session {
        actor: actor.to_string(),
        run: run.map(str::to_string),
        role: Role::Writer,
        kind: SessionKind::Agent,
    }
}

fn human_session(actor: &str) -> Session {
    Session::human(actor.to_string(), Role::Writer).unwrap()
}

/// A malformed human session, as no constructor makes one: the hub claim refuses it itself.
fn raw_human_session(actor: &str) -> Session {
    Session {
        actor: actor.to_string(),
        run: Some("cli".to_string()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

#[test]
fn hsa1_token_vectors_check_prefix_and_crc_but_not_kind() {
    for t in PASSING_TOKENS {
        assert_eq!(t.len(), TOKEN_BYTES);
        token_form(t).unwrap_or_else(|e| panic!("{t} should pass: {e}"));
    }
    for t in FAILING_TOKENS {
        let e = token_form(t).expect_err(t);
        assert_eq!(e.code, E_UPSTREAM_TOKEN, "{t}");
        assert_eq!(e.message, "the hub token has an invalid form");
        assert_eq!(e.fix, "sign in to this install again");
        assert!(
            !format!("{e:?}").contains(&t[7..47]),
            "{t}: the error echoes it"
        );
    }
    // Neither the kind letter `d` nor any other lower-case letter is tested against the letters in
    // use; the checksum alone decides.
    token_form("rdyh_d_TESTVECTORTESTVECTORTESTVECTORTESTVECTOR0JTskl").unwrap();
    // Other shapes fail without a panic: empty, too long, a multi-byte character at 53 bytes.
    for t in [
        "",
        "rdyh_",
        "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYff",
        "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabé2F6gYf",
        "rdyh_a-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf",
        "RDYH_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf",
    ] {
        assert_eq!(token_form(t).expect_err(t).code, E_UPSTREAM_TOKEN, "{t}");
    }
    // The alphabet is in digit order.
    assert_eq!(
        TOKEN_ALPHABET,
        "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
    );
}

#[test]
fn hsa1_origins_compare_scheme_host_and_port() {
    let same = [
        ("https://hub.test", "https://hub.test"),
        ("https://Hub.TEST", "https://hub.test"),
        ("https://hub.test/", "https://hub.test"),
        ("HTTPS://hub.test:443", "https://hub.test"),
        ("https://hub.test:443/", "https://hub.test"),
        ("https://hub.test:8443", "https://hub.test:8443"),
        ("https://bücher.test", "https://xn--bcher-kva.test"),
        ("https://BÜCHER.test/", "https://xn--bcher-kva.test"),
        ("https://[::1]:443/", "https://[::1]"),
        ("https://127.0.0.1:9000", "https://127.0.0.1:9000"),
    ];
    for (input, canonical) in same {
        assert_eq!(origin(input).as_str(), canonical, "{input}");
        assert_eq!(origin(input), origin(canonical), "{input}");
        assert_eq!(origin(input).to_string(), canonical, "{input}");
    }
    assert_ne!(origin("https://hub.test"), origin("https://hub.test:8443"));
    assert_ne!(origin("https://hub.test"), origin("https://hub2.test"));

    for input in [
        "",
        "hub.test",
        "http://hub.test",
        "ftp://hub.test",
        "https:hub.test",
        "https:/hub.test",
        "https://",
        "https:///hub.test",
        "https://user@hub.test",
        "https://user:pw@hub.test",
        "https://@hub.test",
        "https://hub.test?",
        "https://hub.test?x=1",
        "https://hub.test/?x=1",
        "https://hub.test#",
        "https://hub.test#frag",
        "https://hub.test/path",
        "https://hub.test//",
        "https://hub.test\\",
        "https:\\\\hub.test",
        "https://hub.test\\@evil.test",
        "https://hub.te\tst",
        "https://hub.test\n",
        " https://hub.test",
        "https://hub.test ",
        "https://hub.te\u{0}st",
        "https://hub\u{7f}.test",
        "https://hub%2etest",
        "https://%68ub.test",
        "https://hub.test:99999",
        "https://hub.test:x",
    ] {
        let r = Origin::parse(input);
        if let Err(e) = &r {
            assert!(
                input.trim().is_empty() || !e.message.contains(input.trim()),
                "{input:?}: the error echoes the input"
            );
        }
        bad_url(r);
    }

    // Remote URLs may carry a path under the same rules; no trailing slash is emitted.
    let remote = RemoteUrl::parse("https://Hub.TEST:443/org/project/").unwrap();
    assert_eq!(remote.as_str(), "https://hub.test/org/project");
    assert_eq!(remote.origin(), &origin("https://hub.test"));
    let root = RemoteUrl::parse("https://hub.test/").unwrap();
    assert_eq!(root.as_str(), "https://hub.test");
    for input in [
        "http://hub.test/org",
        "https://user@hub.test/org",
        "https://hub.test/org?x",
        "https://hub.test/org#x",
        "https://hub.test/o\\rg",
        "https://hub.test/o rg",
        "https://hub%2etest/org",
    ] {
        bad_url(RemoteUrl::parse(input));
    }

    // A key route ID is one path segment: separators and dots cannot climb out of it.
    let install = origin("https://hub.test");
    assert_eq!(
        install
            .join(&[
                "api",
                "v1",
                "me",
                "keys",
                "skey_00000000000000000000000003",
                "retire"
            ])
            .unwrap(),
        "https://hub.test/api/v1/me/keys/skey_00000000000000000000000003/retire"
    );
    assert_eq!(
        install
            .join(&["api", "v1", "me", "keys", "a/../b?c#d%e", "retire"])
            .unwrap(),
        "https://hub.test/api/v1/me/keys/a%2F..%2Fb%3Fc%23d%25e/retire"
    );
    for segment in ["", ".", ".."] {
        protocol(install.join(&["api", "v1", "me", "keys", segment, "retire"]));
    }
    // A URL parser drops tab, LF and CR inside a path, which would turn one key id into another
    // or a disguised dot segment into a climb: a control character is refused, never dropped.
    for segment in [
        "skey_0000000000000000000000000\n3",
        "skey_0000000000000000000000000\r3",
        "skey_0000000000000000000000000\t3",
        "skey_0000000000000000000000000\r\n3",
        ".\n.",
        "\r..",
        ".\t.",
        ".\n",
        "\t",
        "\n",
        "a\u{0}b",
        "a\u{7f}b",
        "a\u{85}b",
    ] {
        protocol(install.join(&["api", "v1", "me", "keys", segment, "retire"]));
    }
    // A percent-encoded dot is text: it stays one segment, with its `%` encoded.
    assert_eq!(
        install
            .join(&["api", "v1", "me", "keys", "%2e%2E", "retire"])
            .unwrap(),
        "https://hub.test/api/v1/me/keys/%252e%252E/retire"
    );
    assert_eq!(
        install.join(&["api", "v1", "me", "keys", "..."]).unwrap(),
        "https://hub.test/api/v1/me/keys/..."
    );
    // Other bytes are kept, encoded, and a URL parser reads back exactly the given segments.
    let joined = install.join(&["keys", "é \\;", "%2e"]).unwrap();
    assert_eq!(joined, "https://hub.test/keys/%C3%A9%20%5C%3B/%252e");
    let parsed = url::Url::parse(&joined).unwrap();
    assert_eq!(parsed.as_str(), joined, "the parser keeps the joined text");
    assert_eq!(
        parsed.path_segments().unwrap().collect::<Vec<_>>(),
        ["keys", "%C3%A9%20%5C%3B", "%252e"]
    );
}

#[test]
fn hsa1_an_origin_pin_belongs_to_one_install() {
    let a = origin("https://a.test");
    let b = origin("https://b.test");
    let files = origin("https://files.a.test");
    let fence_a = OriginFence::new(a.clone(), [files.clone()]).unwrap();
    let fence_b = OriginFence::new(b.clone(), []).unwrap();
    assert_eq!(fence_a.install(), &a);
    assert!(fence_a.pinned().contains(&files));

    assert_eq!(fence_a.check("https://a.test/auth/token").unwrap(), a);
    assert_eq!(fence_a.check("https://A.test:443/api/v1/me").unwrap(), a);
    assert_eq!(
        fence_a
            .check("https://a.test/api/v1/me/keys?fingerprint=SHA256%3Ax")
            .unwrap(),
        a
    );
    assert_eq!(fence_a.check("https://files.a.test/x").unwrap(), files);
    assert_eq!(fence_b.check("https://b.test/auth/token").unwrap(), b);

    let refused = |fence: &OriginFence, destination: &str, install: &str| {
        assert_err(
            fence.check(destination),
            E_UPSTREAM_ORIGIN,
            &format!("the request destination is not trusted for {install}"),
            "use the install origin, or add its destination to the pinned origins of this install (hub.installs.<install>.pinned_origins)",
        );
        // The fence's refusal is exactly the pin-aware helper's (TASK-291).
        let e = fence.check(destination).unwrap_err();
        let helper = upstream_publish_origin(install);
        assert_eq!(
            (e.code, &e.message, &e.fix),
            (helper.code, &helper.message, &helper.fix)
        );
    };
    // another install, and another install's pinned origin
    refused(&fence_a, "https://b.test/auth/token", "https://a.test");
    refused(&fence_b, "https://a.test/auth/token", "https://b.test");
    refused(&fence_b, "https://files.a.test/x", "https://b.test");
    // no DNS suffix, port, scheme, userinfo, fragment or look-alike widens the fence
    for destination in [
        "https://x.a.test/",
        "https://a.test.evil/",
        "https://a.test:8443/",
        "http://a.test/",
        "https://evil@a.test/",
        "https://a.test/#frag",
        "https://a.test\\@b.test/",
        "https://a.te\nst/",
        "https://%61.test/",
        "a.test/auth",
        "",
    ] {
        refused(&fence_a, destination, "https://a.test");
    }

    // at most 16 pinned origins
    let pins = |n: usize| -> Vec<Origin> {
        (0..n)
            .map(|i| origin(&format!("https://p{i}.a.test")))
            .collect()
    };
    assert_eq!(
        OriginFence::new(a.clone(), pins(MAX_ORIGINS))
            .unwrap()
            .pinned()
            .len(),
        16
    );
    limit(
        OriginFence::new(a.clone(), pins(MAX_ORIGINS + 1)),
        "16 origins",
    );
}

#[test]
fn hsa1_claims_keep_agent_actor_and_run() {
    // Agents and CI keep actor and run byte for byte, bound or not, with no human handle.
    for (actor, run) in [
        ("agent:claude", Some("r7")),
        ("agent:claude", Some("run.2026-10-18_1")),
        ("agent:claude", Some("Run-7")),
        ("agent:x", Some("r")),
        ("agent:acme/claude-code", Some("r7")),
        ("ci:0123456789abcdef0123456789abcdef/nightly", None),
        ("ci:0123456789abcdef0123456789abcdef/build-42", Some("r7")),
    ] {
        let session = agent_session(actor, run);
        let before = session.clone();
        let claim = RequestClaim::from_session(&session).unwrap();
        assert_eq!(claim.actor.as_bytes(), actor.as_bytes());
        assert_eq!(claim.run.as_deref(), run, "an absent run remains absent");
        // The transport's header type takes the same bytes unchanged.
        let header = ureq::http::HeaderValue::from_str(&claim.actor).unwrap();
        assert_eq!(header.as_bytes(), actor.as_bytes());
        if let Some(run) = run {
            let header = ureq::http::HeaderValue::from_str(run).unwrap();
            assert_eq!(header.as_bytes(), run.as_bytes());
        }
        claim.validate_for_send().unwrap();
        let bound = claim.bind_human("test-person").unwrap();
        assert_eq!(bound.actor.as_bytes(), actor.as_bytes());
        assert_eq!(bound.run.as_deref(), run);
        bound.validate_for_send().unwrap();
        assert_eq!(session, before, "the local session is unchanged");
    }

    // A human claim comes only from a genuine human session, and only once it is bound.
    let human = human_session("human:nick");
    let claim = RequestClaim::from_session(&human).unwrap();
    assert_eq!(claim.actor, "human:nick");
    assert_eq!(claim.run.as_deref(), Some("cli"));
    unbound(claim.validate_for_send());
    let bound = claim.bind_human("test-person").unwrap();
    assert_eq!(bound.actor, "human:test-person");
    assert_eq!(bound.run.as_deref(), Some("cli"));
    bound.validate_for_send().unwrap();
    assert_eq!(human.actor, "human:nick", "the local session is unchanged");

    // A human session whose actor is a CI or agent actor keeps it.
    let ci_box = human_session("ci:00000000000000000000000000000000/nightly");
    let bound = RequestClaim::from_session(&ci_box)
        .unwrap()
        .bind_human("test-person")
        .unwrap();
    assert_eq!(bound.actor, "ci:00000000000000000000000000000000/nightly");
    bound.validate_for_send().unwrap();

    // An agent session never claims human:.
    unbound(RequestClaim::from_session(&agent_session(
        "human:nick",
        Some("r7"),
    )));
    unbound(RequestClaim::from_session(&agent_session(
        "human:test-person",
        None,
    )));

    // The hub grammar: a known prefix with a nonempty suffix, and header-safe bytes.
    for actor in [
        "nick",
        "agent:",
        "ci:",
        "human:",
        "bot:x",
        "Agent:x",
        // Upper case, spaces, Unicode, non-breaking spaces and bare CI names are outside the form.
        "agent:Claude",
        "agent:Claude Code",
        "agent:é",
        "agent:Клод",
        "ci:ビルド",
        "ci:build-42",
        "ci:nightly",
        "agent:x\u{a0}",
        "agent:a\nb",
        "agent:a\rb",
        "agent:a\tb",
        "agent:a\u{0}",
        "agent:a\u{7f}",
        "agent:a\u{85}",
        "agent:a\u{9f}",
        "agent:x ",
        " agent:x",
    ] {
        unbound(RequestClaim::from_session(&agent_session(
            actor,
            Some("r7"),
        )));
    }
    unbound(RequestClaim::from_session(&raw_human_session("nick")));
    for run in [
        "",
        "r\n7",
        "r\t7",
        "r7\u{7f}",
        "r7\u{85}",
        " r7",
        "r7 ",
        "run:2026-10-18.1",
        "ré",
        "прогон-1",
        "\u{a0}r",
        "none",
        "reaffirm-1",
    ] {
        unbound(RequestClaim::from_session(&agent_session(
            "agent:claude",
            Some(run),
        )));
    }
}

#[test]
fn id5_hub_claims_use_the_full_actor_and_run_forms() {
    let repo = "0123456789abcdef0123456789abcdef";
    let name = |n: usize| "a".repeat(n);
    let ok = |actor: &str, run: Option<&str>| {
        let session = agent_session(actor, run);
        let before = session.clone();
        let claim = RequestClaim::from_session(&session)
            .unwrap_or_else(|e| panic!("{actor:?} {run:?} should pass: {e}"));
        assert_eq!(claim.actor.as_bytes(), actor.as_bytes());
        assert_eq!(claim.run.as_deref(), run);
        claim.validate_for_send().unwrap();
        assert_eq!(session, before, "the local session is unchanged");
    };
    let bad = |actor: &str, run: Option<&str>| {
        unbound(RequestClaim::from_session(&agent_session(actor, run)));
    };

    // The valid actor forms; an agent session claims no `human:` actor.
    ok("agent:a", None);
    ok("agent:a/b", Some("r"));
    ok(&format!("ci:{repo}/a"), None);
    ok(&format!("agent:{}", name(1)), None);
    ok(&format!("agent:{}", name(64)), None);
    ok(&format!("agent:{}/{}", name(64), name(64)), None);
    assert_eq!(format!("agent:{}/{}", name(64), name(64)).len(), 135);
    ok(&format!("ci:{repo}/{}", name(64)), None);
    ok("agent:a.b_c-d/e.f_g-h", Some("run.1"));
    bad("human:a", Some("r7"));

    // A genuine human session with a valid handle-form actor passes and binds.
    let human = human_session("human:a");
    let claim = RequestClaim::from_session(&human).unwrap();
    assert_eq!(claim.actor, "human:a");
    let bound = claim.bind_human("b").unwrap();
    assert_eq!(bound.actor, "human:b");
    bound.validate_for_send().unwrap();
    assert_eq!(human.actor, "human:a", "the local session is unchanged");

    // Names bound: 1, 64 and 65 bytes.
    bad(&format!("agent:{}", name(65)), None);
    bad(&format!("agent:{}/{}", name(65), name(1)), None);
    bad(&format!("agent:{}/{}", name(1), name(65)), None);
    bad(&format!("ci:{repo}/{}", name(65)), None);
    bad(&format!("human:{}", name(65)), None);
    // The 136-byte actor.
    let long = format!("agent:{}/{}", name(64), "a".repeat(65));
    assert_eq!(long.len(), 136);
    bad(&long, None);
    assert_eq!(
        RequestClaim::from_session(&agent_session(&"a".repeat(136), None))
            .unwrap_err()
            .code,
        E_ACTOR_UNBOUND
    );

    // Invalid actors.
    for actor in [
        "",
        "nick",
        "agent:",
        "ci:",
        "human:",
        "bot:x",
        "Agent:x",
        "agent:A",
        "agent:a/B",
        "agent:Claude Code",
        "agent:a b",
        "agent:é",
        "agent:Клод",
        "agent:x\u{a0}",
        "agent:a\nb",
        "agent:a\rb",
        "agent:a\tb",
        "agent:a\u{0}",
        "agent:a\u{7f}",
        "agent:a\u{85}",
        "agent:-a",
        "agent:a-",
        "agent:.a",
        "agent:a_",
        "agent:a/-b",
        "agent:a/b-",
        "agent:a!",
        "agent:a:b",
        "agent:x ",
        " agent:x",
        "agent:/b",
        "agent:a/",
        "agent:a/b/c",
        "ci:build-42",
        "ci:nightly",
        "ci:ビルド",
        "ci:0123456789abcdef0123456789abcde/a",
        "ci:0123456789abcdef0123456789abcdef0/a",
        "ci:0123456789ABCDEF0123456789abcdef/a",
        "ci:0123456789abcdef0123456789abcdef",
        "ci:0123456789abcdef0123456789abcdef/",
        "ci:0123456789abcdef0123456789abcdef/a/b",
        "ci:0123456789abcdef0123456789abcdeg/a",
        "ci:0123456789abcdef0123456789abcdef/A",
        "human:a/b",
        "human:Nick",
    ] {
        bad(actor, Some("r7"));
    }
    unbound(RequestClaim::from_session(&raw_human_session("nick")));
    unbound(RequestClaim::from_session(&raw_human_session("human:Nick")));

    // Runs: letters of either case, 1 to 64 bytes.
    for run in [
        "r",
        "R7",
        "Run.1_2-x",
        &"r".repeat(64),
        &"R".repeat(64),
        "reaffirm",
        "workshop",
        "nonesuch",
    ] {
        ok("agent:a", Some(run));
    }
    for run in [
        "",
        &"r".repeat(65),
        "/",
        "a/b",
        "a:b",
        "-a",
        "a-",
        ".a",
        "a.",
        "_a",
        "a_",
        "none",
        "work",
        "canon",
        "inflight",
        "reaffirm-1",
        "reaffirm-",
        "r 7",
        " r7",
        "r7 ",
        "r\n7",
        "r\t7",
        "r7\u{7f}",
        "r7\u{85}",
        "ré",
        "прогон-1",
        "\u{a0}r",
    ] {
        bad("agent:a", Some(run));
        // A bound human claim refuses the same run at send.
        let mut claim = bound.clone();
        claim.run = Some(run.to_string());
        unbound(claim.validate_for_send());
    }

    // The public byte bounds stay as the stored-handle decoders use them.
    assert_eq!(MAX_ACTOR_BYTES, 512);
    assert_eq!(MAX_RUN_BYTES, 256);
}

#[test]
fn hsa1_a_wrong_bound_human_handle_is_refused_before_send() {
    let bound = RequestClaim::from_session(&human_session("human:nick"))
        .unwrap()
        .bind_human("test-person")
        .unwrap();
    bound.validate_for_send().unwrap();

    for tampered in ["human:nick", "human:test-person2", "human:Test-person"] {
        let mut claim = bound.clone();
        claim.actor = tampered.to_string();
        unbound(claim.validate_for_send());
    }
    let mut claim = bound.clone();
    claim.run = Some("cli\n".to_string());
    unbound(claim.validate_for_send());

    // An agent claim cannot become a human one by editing its actor.
    let mut agent = RequestClaim::from_session(&agent_session("agent:claude", Some("r7")))
        .unwrap()
        .bind_human("test-person")
        .unwrap();
    agent.actor = "human:test-person".to_string();
    unbound(agent.validate_for_send());

    // A handle that cannot form a header-safe human actor is a contract mismatch.
    let claim = RequestClaim::from_session(&human_session("human:nick")).unwrap();
    for handle in [
        "",
        "a b",
        "a\nb",
        "é",
        &"h".repeat(MAX_ACTOR_BYTES),
        "Test-Person",
        "a/b",
        "-a",
        "a-",
        "a!",
        &"h".repeat(65),
    ] {
        protocol(claim.bind_human(handle));
    }
}

#[test]
fn hsa1_bounds_fail_before_unbounded_growth() {
    // URLs: 4096 bytes.
    let path = |total: usize| {
        let head = "https://hub.test/";
        format!("{head}{}", "p".repeat(total - head.len()))
    };
    assert_eq!(
        RemoteUrl::parse(&path(MAX_URL_BYTES))
            .unwrap()
            .as_str()
            .len(),
        4096
    );
    limit(RemoteUrl::parse(&path(MAX_URL_BYTES + 1)), "4096 bytes");
    limit(
        Origin::parse(&format!("https://{}", "h".repeat(MAX_URL_BYTES))),
        "4096 bytes",
    );
    let fence = OriginFence::new(origin("https://hub.test"), []).unwrap();
    fence.check(&path(MAX_URL_BYTES)).unwrap();
    assert_eq!(
        fence.check(&path(MAX_URL_BYTES + 1)).unwrap_err().code,
        E_LIMIT
    );
    // A joined URL: 4096 bytes in all, counting the origin, every `/` and the encoding.
    let install = origin("https://hub.test");
    let room = MAX_URL_BYTES - "https://hub.test/".len();
    let joined = install.join(&["k".repeat(room).as_str()]).unwrap();
    assert_eq!(joined.len(), MAX_URL_BYTES);
    limit(install.join(&["k".repeat(room + 1).as_str()]), "4096 bytes");
    // Encoding writes `/` as three bytes: 1359 of them and two letters fill the room exactly.
    let slashes = "/".repeat(1359);
    assert_eq!(1359 * 3 + 2, room);
    let joined = install.join(&[format!("{slashes}kk").as_str()]).unwrap();
    assert_eq!(joined.len(), MAX_URL_BYTES);
    limit(
        install.join(&[format!("{slashes}kkk").as_str()]),
        "4096 bytes",
    );
    limit(
        install.join(&[format!("{slashes}/").as_str()]),
        "4096 bytes",
    );
    // Each separator counts: 2040 one-letter segments fill the room, 2041 do not fit.
    let many = vec!["k"; 2040];
    assert_eq!(install.join(&many).unwrap().len(), MAX_URL_BYTES);
    limit(install.join(&vec!["k"; 2041]), "4096 bytes");
    // An over-long key id in a route, and one id over 4096 bytes.
    let long = "k".repeat(MAX_ID_BYTES);
    limit(
        install.join(&["api", "v1", "me", "keys", &long, "retire"]),
        "4096 bytes",
    );
    limit(
        install.join(&["k".repeat(MAX_ID_BYTES + 1).as_str()]),
        "4096 bytes",
    );
    let port = origin("https://hub.test:8443");
    let room = MAX_URL_BYTES - "https://hub.test:8443/".len();
    assert_eq!(
        port.join(&["k".repeat(room).as_str()]).unwrap().len(),
        MAX_URL_BYTES
    );
    limit(port.join(&["k".repeat(room + 1).as_str()]), "4096 bytes");

    // Token: exactly 53 bytes.
    assert_eq!(TOKEN_BYTES, 53);
    assert_eq!(
        token_form(&format!("{ACCESS}0")).unwrap_err().code,
        E_UPSTREAM_TOKEN
    );

    // Claim bounds are the grammar's: an actor of 135 bytes, a run of 64. The public byte
    // constants stay for stored handles.
    assert_eq!((MAX_ACTOR_BYTES, MAX_RUN_BYTES), (512, 256));
    let actor = format!("agent:{0}/{0}", "a".repeat(64));
    assert_eq!(actor.len(), 135);
    RequestClaim::from_session(&agent_session(&actor, None)).unwrap();
    unbound(RequestClaim::from_session(&agent_session(
        &format!("{actor}a"),
        None,
    )));
    unbound(RequestClaim::from_session(&agent_session(
        &format!("agent:{}", "a".repeat(MAX_ACTOR_BYTES)),
        None,
    )));
    let run = "r".repeat(64);
    RequestClaim::from_session(&agent_session("agent:x", Some(&run))).unwrap();
    unbound(RequestClaim::from_session(&agent_session(
        "agent:x",
        Some(&format!("{run}r")),
    )));
    unbound(RequestClaim::from_session(&agent_session(
        "agent:x",
        Some(&"r".repeat(MAX_RUN_BYTES + 1)),
    )));
    let claim = RequestClaim::from_session(&human_session("human:nick")).unwrap();
    claim
        .bind_human(&"h".repeat(64))
        .unwrap()
        .validate_for_send()
        .unwrap();
    protocol(claim.bind_human(&"h".repeat(65)));

    // Label and machine name: 1-64 code points, at most 256 bytes, no Cc, no edge white space.
    for (check, refused) in [
        (
            check_label as fn(&str) -> Result<(), RdyError>,
            bad_label as fn(Result<(), RdyError>),
        ),
        (check_machine_name, bad_machine),
    ] {
        check("a").unwrap();
        check(&"a".repeat(64)).unwrap();
        let wide = "😀".repeat(64);
        assert_eq!(wide.len(), 256);
        check(&wide).unwrap();
        check("laptop key").unwrap();
        check("Nick's MacBook Pro").unwrap();
        refused(check(""));
        refused(check(&"a".repeat(65)));
        refused(check(&"😀".repeat(65)));
        refused(check(&"é".repeat(129)));
        refused(check(&"a".repeat(100_000)));
        for edge in [" a", "a ", "\u{a0}a", "a\u{2003}", "\u{3000}a"] {
            refused(check(edge));
        }
        for control in [
            "a\u{0}b", "a\nb", "a\tb", "a\u{7f}b", "a\u{85}b", "a\u{9f}b",
        ] {
            refused(check(control));
        }
    }
    assert_eq!(
        (
            MAX_LABEL_CHARS,
            MAX_LABEL_BYTES,
            MAX_MACHINE_NAME_CHARS,
            MAX_MACHINE_NAME_BYTES
        ),
        (64, 256, 64, 256)
    );

    // User code: exactly eight letters from BCDFGHJKLMNPQRSTVWXZ.
    check_user_code("BCDFGHJK").unwrap();
    check_user_code("ZZZZZZZZ").unwrap();
    for code in [
        "BCDFGHJ",
        "BCDFGHJKL",
        "BCDFGHJA",
        "bcdfghjk",
        "BCDF-HJK",
        "",
        "BCDFGHJ😀",
    ] {
        protocol(check_user_code(code));
    }

    // Device life 1-600 seconds; the initial interval is 5; slow-down adds 5 up to 300.
    check_device_expires_in(1).unwrap();
    check_device_expires_in(600).unwrap();
    protocol(check_device_expires_in(0));
    protocol(check_device_expires_in(601));
    check_initial_interval(5).unwrap();
    protocol(check_initial_interval(4));
    protocol(check_initial_interval(6));
    assert_eq!(slow_down(5).unwrap(), 10);
    assert_eq!(slow_down(295).unwrap(), 300);
    protocol(slow_down(296));
    protocol(slow_down(300));
    protocol(slow_down(u64::MAX));

    // Access life 1-3600 seconds.
    check_access_expires_in(1).unwrap();
    check_access_expires_in(3600).unwrap();
    protocol(check_access_expires_in(0));
    protocol(check_access_expires_in(3601));

    // Absolute times use checked arithmetic.
    assert_eq!(deadline_ms(1_000, 600).unwrap(), 601_000);
    protocol(deadline_ms(i64::MAX - 1_000, 600));
    protocol(deadline_ms(0, u64::MAX));
    protocol(deadline_ms(-1, 1));

    // Device code 1-4096 bytes; an issued pair passes the token check.
    let start = |device_code: String, user_code: &str, uri: &str| DeviceStartReply {
        device_code: Secret::new(device_code),
        user_code: user_code.to_string(),
        verification_uri: uri.to_string(),
        expires_in: 600,
        interval: 5,
    };
    let device_uri = "https://hub.test/app/device";
    start("d".repeat(MAX_DEVICE_CODE_BYTES), "BCDFGHJK", device_uri)
        .validate(&install)
        .unwrap();
    protocol(
        start(
            "d".repeat(MAX_DEVICE_CODE_BYTES + 1),
            "BCDFGHJK",
            device_uri,
        )
        .validate(&install),
    );
    protocol(start(String::new(), "BCDFGHJK", device_uri).validate(&install));
    protocol(start("d".into(), "BCDFGHJA", device_uri).validate(&install));
    for uri in [
        "https://hub.test/app/device?user_code=BCDFGHJK",
        "https://hub.test/app/device#BCDFGHJK",
        "https://hub.test/app/device/BCDFGHJK",
        "https://other.test/app/device",
        "http://hub.test/app/device",
    ] {
        protocol(start("d".into(), "BCDFGHJK", uri).validate(&install));
    }
    let pair = |access: &str, expires_in: u64| TokenPair {
        access_token: Secret::new(access),
        refresh_token: Secret::new(REFRESH),
        expires_in,
    };
    pair(ACCESS, 3600).validate().unwrap();
    assert_eq!(
        pair(FAILING_TOKENS[0], 3600).validate().unwrap_err().code,
        E_UPSTREAM_TOKEN
    );
    protocol(pair(ACCESS, 3601).validate());

    // Timestamps: 64 bytes.
    let stamp = format!("2026-10-17T11:00:00{}Z", " ".repeat(44));
    assert_eq!(stamp.len(), 64);
    bad_time(parse_compromised_since(&stamp));
    limit(parse_compromised_since(&format!("{stamp} ")), "64 bytes");

    // The named ceilings and their units.
    for (n, max, unit, text) in [
        (MAX_DEPTH, 32, LimitUnit::Levels, "32 levels"),
        (MAX_MEMBERS, 4096, LimitUnit::Entries, "4096 entries"),
        (MAX_HEADERS, 64, LimitUnit::Headers, "64 headers"),
        (MAX_REMOTES, 64, LimitUnit::Remotes, "64 remotes"),
        (MAX_INSTALLS, 32, LimitUnit::Installs, "32 installs"),
        (MAX_ORIGINS, 16, LimitUnit::Origins, "16 origins"),
        (MAX_CA_CERTS, 64, LimitUnit::Certificates, "64 certificates"),
        (MAX_JSON_BYTES, 1_048_576, LimitUnit::Bytes, "1048576 bytes"),
        (
            MAX_CONFIG_BYTES,
            1_048_576,
            LimitUnit::Bytes,
            "1048576 bytes",
        ),
        (MAX_CA_BYTES, 1_048_576, LimitUnit::Bytes, "1048576 bytes"),
        (MAX_SCALAR_BYTES, 16384, LimitUnit::Bytes, "16384 bytes"),
        (MAX_HEADER_BYTES, 32768, LimitUnit::Bytes, "32768 bytes"),
        (MAX_TIMESTAMP_BYTES, 64, LimitUnit::Bytes, "64 bytes"),
        (
            MAX_SIG_STATEMENT_BYTES,
            16384,
            LimitUnit::Bytes,
            "16384 bytes",
        ),
        (MAX_SIG_KEY_BYTES, 16384, LimitUnit::Bytes, "16384 bytes"),
        (MAX_SIG_PROOF_BYTES, 16384, LimitUnit::Bytes, "16384 bytes"),
        (MAX_SIG_VALUE_BYTES, 4096, LimitUnit::Bytes, "4096 bytes"),
        (MAX_SIG_NAMESPACE_BYTES, 64, LimitUnit::Bytes, "64 bytes"),
    ] {
        assert_eq!(n, max, "{text}");
        check_count(n, n, unit).unwrap();
        limit(check_count(n + 1, n, unit), text);
    }
}

/// `true` when `T` implements Display (autoref specialization: the impl on `Probe` is tried
/// first, the fallback on `&Probe` only when its bound fails).
struct Probe<T>(PhantomData<T>);
trait DisplaysYes {
    fn displays(&self) -> bool {
        true
    }
}
impl<T: std::fmt::Display> DisplaysYes for Probe<T> {}
trait DisplaysNo {
    fn displays(&self) -> bool {
        false
    }
}
impl<T> DisplaysNo for &Probe<T> {}
struct SerProbe<T>(PhantomData<T>);
trait SerializesYes {
    fn serializes(&self) -> bool {
        true
    }
}
impl<T: serde::Serialize> SerializesYes for SerProbe<T> {}
trait SerializesNo {
    fn serializes(&self) -> bool {
        false
    }
}
impl<T> SerializesNo for &SerProbe<T> {}

#[test]
// The probes call through `&Probe` on purpose: that borrow is what selects the fallback.
#[allow(clippy::needless_borrow)]
fn hsa1_secret_debug_and_errors_hide_bytes() {
    let secrets = [ACCESS, REFRESH, DEVICE_CODE];
    let hidden = |what: &str, text: String| {
        for s in secrets {
            assert!(!text.contains(s), "{what} shows a secret: {text}");
        }
        assert!(!text.contains("0123456789ABCDEF"), "{what}: {text}");
        assert!(!text.contains("efghijklmnop"), "{what}: {text}");
        assert!(!text.contains("sentinel"), "{what}: {text}");
    };

    let secret = Secret::new(ACCESS);
    assert_eq!(secret.expose(), ACCESS);
    hidden("Secret", format!("{secret:?}"));
    hidden("Secret pretty", format!("{secret:#?}"));
    assert!(
        !(&Probe::<Secret>(PhantomData)).displays(),
        "Secret implements Display"
    );
    assert!(
        !(&SerProbe::<Secret>(PhantomData)).serializes(),
        "Secret implements Serialize"
    );
    assert!(
        (&Probe::<Origin>(PhantomData)).displays(),
        "the probe works"
    );
    assert!(
        (&SerProbe::<HubKey>(PhantomData)).serializes(),
        "the probe works"
    );
    for ty in [
        (&SerProbe::<HubCall>(PhantomData)).serializes(),
        (&SerProbe::<HubOperation>(PhantomData)).serializes(),
        (&SerProbe::<HubReply>(PhantomData)).serializes(),
        (&SerProbe::<TokenPair>(PhantomData)).serializes(),
        (&SerProbe::<DeviceStartReply>(PhantomData)).serializes(),
    ] {
        assert!(!ty, "a token-bearing aggregate implements Serialize");
    }

    let install = origin("https://hub.test");
    let claim = RequestClaim::from_session(&agent_session("agent:claude", Some("r7"))).unwrap();
    let call = |bearer: Option<&str>, operation: HubOperation| HubCall {
        install: install.clone(),
        destination: "https://hub.test/auth/token".to_string(),
        ca_file: None,
        claim: Some(claim.clone()),
        bearer: bearer.map(Secret::new),
        operation,
    };
    for c in [
        call(Some(ACCESS), HubOperation::Me),
        call(
            None,
            HubOperation::DevicePoll {
                device_code: Secret::new(DEVICE_CODE),
            },
        ),
        call(
            None,
            HubOperation::Refresh {
                refresh_token: Secret::new(REFRESH),
            },
        ),
        call(
            None,
            HubOperation::SignOut {
                token: Secret::new(REFRESH),
            },
        ),
        call(
            Some(ACCESS),
            HubOperation::Revoke {
                key_id: "skey_00000000000000000000000003".into(),
                compromised_from: None,
            },
        ),
    ] {
        hidden("HubCall", format!("{c:?}"));
        hidden("HubCall pretty", format!("{c:#?}"));
        hidden("HubOperation", format!("{:?}", c.operation));
    }

    let tokens = || TokenPair {
        access_token: Secret::new(ACCESS),
        refresh_token: Secret::new(REFRESH),
        expires_in: 3600,
    };
    let replies = [
        HubReply::DeviceStart(DeviceStartReply {
            device_code: Secret::new(DEVICE_CODE),
            user_code: "BCDFGHJK".into(),
            verification_uri: "https://hub.test/app/device".into(),
            expires_in: 600,
            interval: 5,
        }),
        HubReply::DevicePoll(DevicePollReply::Tokens(tokens())),
        HubReply::Refresh(RefreshReply::Tokens(tokens())),
    ];
    for r in &replies {
        hidden("HubReply", format!("{r:?}"));
        hidden("HubReply pretty", format!("{r:#?}"));
    }
    hidden("TokenPair", format!("{:?}", tokens()));

    // Errors never format their input.
    for t in [ACCESS, REFRESH] {
        let bad = format!("{}x", &t[..52]);
        let e = token_form(&bad).unwrap_err();
        hidden("token error", format!("{e:?} {e}"));
        let e = HubFailure::Local(e).into_error(&install);
        hidden("converted error", format!("{e:?} {e}"));
    }
    let e = Origin::parse(&format!("https://{ACCESS}@hub.test")).unwrap_err();
    hidden("origin error", format!("{e:?}"));
    let e = parse_compromised_since(ACCESS).unwrap_err();
    hidden("time error", format!("{e:?}"));
    let e = check_label(&format!("{ACCESS}\n")).unwrap_err();
    hidden("label error", format!("{e:?}"));
    let e = check_machine_name(&format!("{ACCESS}\n")).unwrap_err();
    hidden("machine error", format!("{e:?}"));
    let e =
        RequestClaim::from_session(&agent_session(&format!("agent:{ACCESS}\n"), None)).unwrap_err();
    hidden("claim error", format!("{e:?}"));

    // Failure conversion uses the shared texts.
    let converted = |f: HubFailure| f.into_error(&install);
    let e = converted(HubFailure::ActorClaim);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_ACTOR_CLAIM,
            "the hub refused the actor claim",
            "check the current account handle and session actor, then try again"
        )
    );
    let e = converted(HubFailure::AuthRequired);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_AUTH,
            "sign-in is required for https://hub.test",
            "start a new sign-in for this install"
        )
    );
    let e = converted(HubFailure::Refused);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_REFUSED,
            "the hub refused the request",
            "check the account's access on the hub before trying again"
        )
    );
    let e = converted(HubFailure::Transport {
        may_have_sent: false,
    });
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_TRANSPORT,
            "the hub request failed",
            "check the install address, network and CA setting, then try again"
        )
    );
    let e = converted(HubFailure::Transport {
        may_have_sent: true,
    });
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            "the hub request may have completed",
            "check its state on the hub before trying again"
        )
    );
    protocol(Err::<(), _>(converted(HubFailure::Protocol)));
    // Local preserves a preflight error unchanged.
    let local = check_label("").unwrap_err();
    let e = converted(HubFailure::Local(local.clone()));
    assert_eq!(
        (e.code, e.message, e.fix),
        (local.code, local.message, local.fix)
    );
}

#[test]
fn hsa1_compromised_time_preserves_the_instant() {
    for (input, canonical) in [
        ("2026-10-17T11:00:00Z", "2026-10-17T11:00:00Z"),
        ("2026-10-17t11:00:00z", "2026-10-17T11:00:00Z"),
        ("2026-10-17T11:00:00+00:00", "2026-10-17T11:00:00Z"),
        ("2026-10-17T11:00:00-00:00", "2026-10-17T11:00:00Z"),
        ("2026-10-17T13:30:00+02:30", "2026-10-17T11:00:00Z"),
        ("2026-10-17T06:00:00-05:00", "2026-10-17T11:00:00Z"),
        ("2026-10-18T00:30:00+01:00", "2026-10-17T23:30:00Z"),
        ("2026-12-31T23:30:00-01:00", "2027-01-01T00:30:00Z"),
        ("2026-10-17T11:00:00.5Z", "2026-10-17T11:00:00.5Z"),
        ("2026-10-17T11:00:00.120Z", "2026-10-17T11:00:00.12Z"),
        ("2026-10-17T11:00:00.000Z", "2026-10-17T11:00:00Z"),
        (
            "2026-10-17T11:00:00.000000001Z",
            "2026-10-17T11:00:00.000000001Z",
        ),
        (
            "2026-10-17T12:00:00.123456789+01:00",
            "2026-10-17T11:00:00.123456789Z",
        ),
        (
            "2024-02-29T23:59:59.999999999-12:00",
            "2024-03-01T11:59:59.999999999Z",
        ),
    ] {
        assert_eq!(
            parse_compromised_since(input).unwrap(),
            canonical,
            "{input}"
        );
        // The canonical form reads back to itself.
        assert_eq!(parse_compromised_since(canonical).unwrap(), canonical);
    }
    for input in [
        "",
        "yesterday",
        "2026-10-17",
        "2026-10-17T11:00:00",
        "2026-10-17T11:00Z",
        "2026-10-17X11:00:00Z",
        "2026-10-17 11:00:00Z",
        "2026-10-17T11:00:00.Z",
        "2026-10-17T11:00:00.1234567891Z",
        "2026-10-17T11:00:00.123456789012Z",
        "2026-02-30T00:00:00Z",
        "2026-10-17T24:00:00Z",
        "2026-12-31T23:59:60Z",
        "2026-10-17T11:00:00+24:00",
        "2026-10-17T11:00:00+0100",
        "0000-01-01T00:30:00+01:00",
        "2026-10-17T11:00:00Z ",
        " 2026-10-17T11:00:00Z",
        "２026-10-17T11:00:00Z",
    ] {
        bad_time(parse_compromised_since(input));
    }
}

/// The settled pending-key fixture (bind reply's `key`).
const PENDING_KEY: &str = r#"{"id":"skey_00000000000000000000000003","account":"acc_00000000000000000000000001","fingerprint":"SHA256:RHFBqxmd4FLrdHDM2DGtlKdMxg/nN9xD3ZpkWHSvlaM","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMxZFwIQHLofF5C/SY2UPr6zgqjzUfcX6+XICjAhsvur","label":"laptop key","kind":"person","state":"pending","expires_at":null,"bound_at":null,"retired_at":null,"revoked_at":null,"compromised_from":null}"#;

/// The twelve members of the key object.
const KEY_MEMBERS: [&str; 12] = [
    "id",
    "account",
    "fingerprint",
    "public_key",
    "label",
    "kind",
    "state",
    "expires_at",
    "bound_at",
    "retired_at",
    "revoked_at",
    "compromised_from",
];

/// The five key members whose value may be null.
const NULLABLE_KEY_MEMBERS: [&str; 5] = [
    "expires_at",
    "bound_at",
    "retired_at",
    "revoked_at",
    "compromised_from",
];

#[test]
fn hsa1_key_replies_hold_every_member() {
    let full: serde_json::Value = serde_json::from_str(PENDING_KEY).unwrap();
    let key: HubKey = serde_json::from_value(full.clone()).unwrap();
    assert_eq!(key.id, "skey_00000000000000000000000003");
    assert_eq!((key.kind, key.state), (KeyKind::Person, KeyState::Pending));
    assert_eq!(
        (
            &key.expires_at,
            &key.bound_at,
            &key.retired_at,
            &key.revoked_at,
            &key.compromised_from
        ),
        (&None, &None, &None, &None, &None),
        "an explicit null is None"
    );
    // The key writes back every member, nulls included.
    let written = serde_json::to_value(&key).unwrap();
    assert_eq!(written, full);

    // A nullable member may hold a time.
    for member in NULLABLE_KEY_MEMBERS {
        let mut v = full.clone();
        v[member] = serde_json::Value::from("2026-10-17T12:05:00Z");
        let key: HubKey = serde_json::from_value(v).unwrap();
        let value = serde_json::to_value(&key).unwrap();
        assert_eq!(value[member], "2026-10-17T12:05:00Z", "{member}");
    }

    // Every reply holds every member: an absent one, nullable or not, is a refusal.
    let without = |member: &str| {
        let mut v = full.clone();
        v.as_object_mut().unwrap().remove(member).unwrap();
        v
    };
    for member in KEY_MEMBERS {
        assert!(
            serde_json::from_value::<HubKey>(without(member)).is_err(),
            "a key without {member} was accepted"
        );
    }
    // A null where a value is required, and an unknown member, are refusals too.
    for member in KEY_MEMBERS {
        if NULLABLE_KEY_MEMBERS.contains(&member) {
            continue;
        }
        let mut v = full.clone();
        v[member] = serde_json::Value::Null;
        assert!(
            serde_json::from_value::<HubKey>(v).is_err(),
            "{member}: null"
        );
    }
    let mut v = full.clone();
    v["extra"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<HubKey>(v).is_err());

    // The same holds for the key nested in a bind reply.
    let bind = |key: serde_json::Value| {
        serde_json::json!({
            "key": key,
            "confirm_url": "https://hub.test/app/settings/keys/skey_00000000000000000000000003/confirm",
        })
    };
    let result: BindResult = serde_json::from_value(bind(full.clone())).unwrap();
    assert_eq!(result.key, key_from(PENDING_KEY));
    for member in KEY_MEMBERS {
        assert!(
            serde_json::from_value::<BindResult>(bind(without(member))).is_err(),
            "a bind reply whose key lacks {member} was accepted"
        );
    }
}

fn key_from(json: &str) -> HubKey {
    serde_json::from_str(json).unwrap()
}

/// The device page is one exact text, `<install>/app/device`: a same-origin link that only
/// normalizes to it (plain or encoded dot segments, which can carry the device code), or any
/// other path, is refused by the typed check and by the shared comparison alike.
#[test]
fn hsa1_the_device_page_is_compared_byte_for_byte() {
    let install = Origin::parse("https://hub.test").unwrap();
    assert_eq!(
        device_page(&install).unwrap(),
        "https://hub.test/app/device"
    );
    let start = |uri: &str| DeviceStartReply {
        device_code: Secret::new("dc-secret"),
        user_code: "BCDFGHJK".to_string(),
        verification_uri: uri.to_string(),
        expires_in: 600,
        interval: 5,
    };
    start("https://hub.test/app/device")
        .validate(&install)
        .unwrap();
    assert!(is_device_page(&install, "https://hub.test/app/device"));
    for uri in [
        "https://hub.test/app/dc-secret/../device",
        "https://hub.test/app/dc-secret/%2e%2e/device",
        "https://hub.test/app/dc-secret/%2E%2E/device",
        "https://hub.test/app/dc-secret/.%2e/device",
        "https://hub.test/app/./device",
        "https://hub.test/app/%2e/device",
        "https://hub.test/app/device/",
        "https://hub.test/app//device",
        "https://hub.test//app/device",
        "https://HUB.test/app/device",
        "https://hub.test:443/app/device",
        "https://hub.test/app/devic%65",
        "https://hub.test/App/device",
        "https://hub.test/app/other",
        "https://hub.test/app",
        "https://hub.test/",
        "https://hub.test",
    ] {
        protocol(start(uri).validate(&install));
        assert!(!is_device_page(&install, uri), "{uri}");
    }
}
