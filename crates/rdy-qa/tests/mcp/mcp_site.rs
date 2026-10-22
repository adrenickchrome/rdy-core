//! ls7: `rdy_site` gives the person the live site's link, for canon, a view or a proposal version
//! (D-N5NSN, D-FV6G9, D-KQ4CA); with no daemon there is no site.
use rdy_daemon::site::LanOverride;
use rdy_engine::{ProposalInfo, Rdy};
use rdy_qa::daemon::{Hosted, Served, human_command, short_tempdir};
use rdy_qa::fixtures::human_env;
use rdy_qa::golden::{FIXTURE_DIR, Ids, approver, land, write, writer};
use rdy_qa::http;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::net::Ipv4Addr;
use std::path::Path;

/// [`golden_served`], with two more landings after the tag `v1.0.0` (canon reads
/// `v1.0.0 + 2 landings`, as the golden project does).
fn golden_served_ahead() -> Served {
    let (s, _, _) = golden_served();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let ids = Ids::new();
    for (view, id, body) in [
        ("clarify", &ids.motivation_prose, "Three idioms recur."),
        (
            "tighten",
            &ids.guards_prose,
            "Patterns say what the subject looks like.",
        ),
    ] {
        write(
            &mut r,
            view,
            json!([{"op": "update", "id": id.to_string(), "body": body}]),
        )
        .unwrap();
        land(&mut r, view).unwrap();
    }
    r.close().unwrap();
    s
}

/// A served project holding the golden corpus, landed and tagged `v1.0.0`, and a view `editing`
/// with one unlanded change, proposed once. Returns the project, the proposal and the landing.
fn golden_served() -> (Served, ProposalInfo, String) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    let landing = land(&mut r, "import").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let ids = Ids::new();
    write(
        &mut r,
        "editing",
        json!([{"op": "update", "id": ids.tutorial_prose.to_string(),
            "body": "So far we have used three simple patterns. They compose."}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "editing", "tutorial", None).unwrap();
    r.close().unwrap();
    (s, p, landing)
}

fn agent(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    c
}

fn page(url: &str) -> http::Response {
    http::get(url)
}

#[test]
fn ls7_rdy_site_gives_the_link() {
    let (s, _, _) = golden_served();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({})).ok();
    assert_eq!(v["scope"], "canon", "{v}");
    assert!(v.get("warning").is_none(), "{v}");
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{v}");
    assert_eq!(page(url).status, 200, "{v}");
}

#[test]
fn ls7_rdy_site_for_a_view_and_a_proposal() {
    let (s, proposal, landing) = golden_served();
    let proposal_ref = proposal.r#ref.clone();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({"scope": "editing"})).ok();
    assert_eq!(v["scope"], "view:editing", "{v}");
    let url = v["url"].as_str().unwrap();
    assert!(url.ends_with("~view/editing/"), "{v}");
    let got = page(url);
    assert_eq!(got.status, 200, "{v}");
    assert!(
        String::from_utf8_lossy(&got.body).contains("Show changes"),
        "a view serves in review mode"
    );
    let same = c.call("rdy_site", json!({"scope": "view:editing"})).ok();
    assert_eq!(same["url"], v["url"], "{same}");

    let scope = format!("proposal:{proposal_ref}");
    let p = c.call("rdy_site", json!({ "scope": scope })).ok();
    assert_eq!(p["scope"], scope.as_str(), "{p}");
    let url = p["url"].as_str().unwrap();
    assert!(url.ends_with(&format!("~proposal/{proposal_ref}/")), "{p}");
    assert_eq!(page(url).status, 200, "{p}");

    // final review M5 and M2: each form of a proposal gets the one spelling the site reads, and
    // its link serves
    let (stable, _) = proposal_ref.split_once('@').unwrap();
    let upper = format!("P-{}", stable[2..].to_ascii_uppercase());
    for (given, want) in [
        (format!("proposal:{stable}"), format!("proposal:{stable}")),
        (format!("proposal:{}", proposal.id), scope.clone()),
        (format!("proposal:{upper}@+01"), scope.clone()),
    ] {
        let p = c.call("rdy_site", json!({ "scope": given })).ok();
        assert_eq!(p["scope"], want.as_str(), "{given}: {p}");
        let url = p["url"].as_str().unwrap();
        let want_path = format!("/~{}/", want.replacen(':', "/", 1));
        assert!(url.ends_with(&want_path), "{given}: {p}");
        assert_eq!(page(url).status, 200, "{given}: {p}");
    }

    let bad = c.call("rdy_site", json!({"scope": "nosuch"}));
    assert_eq!(bad.code(), Some("E_NOT_FOUND"), "{bad:?}");
    let bad = c.call("rdy_site", json!({"scope": "proposal:P-ffff@9"}));
    assert!(!bad.is_ok(), "{bad:?}");
    // final review M5: the site shows no landing or analysis scope (a tag has its route, sr2)
    for given in [format!("landing:{landing}"), "inflight".to_string()] {
        let bad = c.call("rdy_site", json!({ "scope": given }));
        assert_eq!(bad.code(), Some("E_BAD_INPUT"), "{given}: {bad:?}");
    }
}

#[test]
fn sr2_rdy_site_gives_a_tag_link() {
    let (s, _, _) = golden_served();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({"scope": "tag:v1.0.0"})).ok();
    assert_eq!(v["scope"], "tag:v1.0.0", "{v}");
    let url = v["url"].as_str().unwrap();
    assert!(url.ends_with("/~tag/v1.0.0/"), "{v}");
    assert!(v.get("changes").is_none(), "{v}");
    assert_eq!(page(url).status, 200, "{v}");
    // the stored name, whichever way the tag is spelled
    let bare = c.call("rdy_site", json!({"scope": "tag:1.0.0"})).ok();
    assert_eq!(bare["url"], v["url"], "{bare}");
    assert_eq!(bare["scope"], "tag:v1.0.0", "{bare}");
    let none = c.call("rdy_site", json!({"scope": "tag:v9.9.9"}));
    assert_eq!(none.code(), Some("E_NOT_FOUND"), "{none:?}");
}

/// Ruling task-2 (1): the tag route accepts what `parse_version` accepts, so a stored version
/// longer than 64 bytes still has a site.
#[test]
fn sr2_a_long_valid_version_has_a_site() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    let name = format!("v1.0.0-{}", "a".repeat(58));
    assert_eq!(name.len(), 65);
    r.tag(&approver(), &name, None, None).unwrap();
    r.close().unwrap();
    let mut c = agent(&s);
    let v = c
        .call("rdy_site", json!({"scope": format!("tag:{name}")}))
        .ok();
    let url = v["url"].as_str().unwrap();
    assert!(url.ends_with(&format!("/~tag/{name}/")), "{v}");
    assert_eq!(page(url).status, 200, "{v}");
}

/// Fix round 2 (ruling task-2 (1)): the longest version name rdy stores has a site that the
/// server really serves, and a name one byte over is refused where it would be stored.
#[test]
fn sr2_the_longest_version_has_a_site() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    let max = rdy_engine::MAX_VERSION_BYTES;
    let too_long = format!("v1.0.0-{}", "a".repeat(max - 6));
    assert_eq!(too_long.len(), max + 1);
    let err = r.tag(&approver(), &too_long, None, None).unwrap_err();
    assert_eq!(err.code, "E_BAD_INPUT", "{err:?}");
    let name = format!("v1.0.0-{}", "a".repeat(max - 7));
    assert_eq!(name.len(), max);
    r.tag(&approver(), &name, None, None).unwrap();
    r.close().unwrap();
    let mut c = agent(&s);
    let v = c
        .call("rdy_site", json!({"scope": format!("tag:{name}")}))
        .ok();
    let url = v["url"].as_str().unwrap();
    assert!(url.ends_with(&format!("/~tag/{name}/")), "{v}");
    assert_eq!(page(url).status, 200, "{v}");
    assert_eq!(page(&format!("{url}index.html")).status, 200, "{v}");
}

/// Ruling final (3): a stored tag over 256 bytes (only an older release could store one) has no
/// site page. `rdy_site` says so, the guard does not route it, and the versions page lists it as
/// text with no link; a stored tag of 256 bytes keeps its page.
#[test]
fn sr2_a_long_stored_tag_has_no_site_link() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    r.import(&approver(), "import", Path::new(FIXTURE_DIR))
        .unwrap();
    land(&mut r, "import").unwrap();
    let max = format!("v1.0.0-{}", "a".repeat(rdy_engine::MAX_VERSION_BYTES - 7));
    r.tag(&approver(), &max, None, None).unwrap();
    // the base release stored this one through `rdy tag`; the store API writes it the same way
    let long = format!("v2.0.0-{}", "b".repeat(293));
    assert_eq!(long.len(), 300);
    let landing: rdy_model::Hash = r.read().landings().unwrap()[0].0;
    r.store_mut()
        .put_tag(&rdy_model::Tag {
            name: long.clone(),
            landing,
            message: None,
            tagged_by: approver().provenance(),
        })
        .unwrap();
    r.invalidate();
    r.fsck().expect("a stored long tag is not damage");
    r.close().unwrap();

    let mut c = agent(&s);
    let bad = c.call("rdy_site", json!({"scope": format!("tag:{long}")}));
    assert_eq!(bad.code(), Some("E_BAD_INPUT"), "{bad:?}");
    let Reply::ToolError(e) = bad else {
        panic!("expected a tool error")
    };
    assert_eq!(
        e["message"], "this version's name is longer than the site serves (256 bytes)",
        "{e}"
    );
    assert_eq!(
        e["fix"], "open it with rdy show, or read it in the versions list",
        "{e}"
    );
    let canon = c.call("rdy_site", json!({})).ok();
    let root = canon["url"].as_str().unwrap().to_string();
    assert_eq!(page(&format!("{root}~tag/{long}/")).status, 404);
    // the 256-byte tag keeps its page
    let v = c
        .call("rdy_site", json!({"scope": format!("tag:{max}")}))
        .ok();
    assert_eq!(page(v["url"].as_str().unwrap()).status, 200, "{v}");
    // the versions page: the long tag as text, the other with its link
    let menu = page(&format!("{root}~versions/"));
    assert_eq!(menu.status, 200);
    let h = String::from_utf8(menu.body).unwrap();
    assert!(
        h.contains(&format!("<li><span class=\"vmenu-name\">{long}</span>")),
        "{h}"
    );
    assert!(!h.contains(&format!("~tag/{long}")), "{h}");
    assert!(
        h.contains(&format!("<a href=\"/~tag/{max}/\">{max}</a>")),
        "{h}"
    );
}

#[test]
fn sr2_rdy_site_gives_the_changes_link() {
    let s = golden_served_ahead();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({"scope": "view:editing"})).ok();
    let changes = v["changes"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert!(changes.ends_with("/~view/editing/changes.html"), "{v}");
    assert_eq!(
        changes,
        format!("{}changes.html", v["url"].as_str().unwrap()),
        "{v}"
    );
    let canon = c.call("rdy_site", json!({})).ok();
    let changes = canon["changes"]
        .as_str()
        .unwrap_or_else(|| panic!("{canon}"));
    assert!(changes.ends_with("/changes-since.html"), "{canon}");
    assert!(!changes.contains('~'), "{canon}");
    assert!(canon.get("lan_changes").is_none(), "{canon}");
}

/// Final review I4: `changes` (and `lan_changes`) lead only to a summary the site has. A
/// top-level doc that owns `changes.html` (a view, a proposal version) or `changes-since.html`
/// (canon) keeps its page, so the reply gives no summary link and keeps the render's warning.
/// `rdy site` and `rdy_site` give the same reply.
#[test]
fn sr2_no_changes_link_to_a_doc_that_owns_the_page() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "fields": {"name": "changes-since", "title": "Authored since guide", "doc_type": "overview"}},
            {"op": "create", "type": "doc", "fields": {"name": "base", "title": "Base", "doc_type": "overview"}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    write(
        &mut r,
        "later",
        json!([{"op": "create", "type": "doc", "fields": {"name": "later", "title": "Later", "doc_type": "overview"}}]),
    )
    .unwrap();
    land(&mut r, "later").unwrap();
    write(
        &mut r,
        "collision",
        json!([{"op": "create", "type": "doc", "fields": {"name": "changes", "title": "Authored changes guide", "doc_type": "overview"}}]),
    )
    .unwrap();
    let p = r
        .propose(&writer(), "collision", "Add a guide", None)
        .unwrap();
    r.close().unwrap();

    let kept = "the top-level doc named changes has the page changes.html: the change summary of this view or proposal is left out. Rename the doc to publish it.";
    let since = "the top-level doc named changes-since has the page changes-since.html: the summary of changes since v1.0.0 is left out. Rename the doc to publish it.";
    let mut c = agent(&s);
    for (scope, warning, page_name) in [
        ("view:collision".to_string(), kept, "changes.html"),
        (format!("proposal:{}", p.id), kept, "changes.html"),
        ("canon".to_string(), since, "changes-since.html"),
    ] {
        let v = c.call("rdy_site", json!({ "scope": scope })).ok();
        assert!(v.get("changes").is_none(), "{scope}: {v}");
        assert!(v.get("lan_changes").is_none(), "{scope}: {v}");
        let warnings = v["warnings"].as_array().unwrap_or_else(|| panic!("{v}"));
        assert!(warnings.iter().any(|w| w == warning), "{scope}: {v}");
        // the authored doc keeps its page at that path
        let url = v["url"].as_str().unwrap();
        let h = String::from_utf8(page(&format!("{url}{page_name}")).body).unwrap();
        assert!(h.contains("<h1>Authored "), "{scope}: {h}");
        // the CLI twin gives the same reply, and prints no changes line
        let (ok, cli) = s.json(&["--json", "site", "--scope", &scope]);
        assert!(ok, "{cli}");
        assert_eq!(cli, v, "{scope}");
        let out = s.rdy(&["site", "--scope", &scope]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(!text.contains("changes:"), "{scope}: {text:?}");
    }
}

#[test]
fn sr2_rdy_site_gives_no_since_link_without_landings_since() {
    // the tag stands on canon's head: nothing landed since
    let (s, _, _) = golden_served();
    let mut c = agent(&s);
    let canon = c.call("rdy_site", json!({})).ok();
    assert!(canon.get("changes").is_none(), "{canon}");
    // no version at all
    let plain = Served::new();
    let mut c = agent(&plain);
    let canon = c.call("rdy_site", json!({})).ok();
    assert!(canon.get("changes").is_none(), "{canon}");
}

#[test]
fn sr2_rdy_site_refuses_a_landing_scope() {
    let (s, _, landing) = golden_served();
    let mut c = agent(&s);
    for given in [format!("landing:{landing}"), "inflight".to_string()] {
        let bad = c.call("rdy_site", json!({ "scope": given }));
        assert_eq!(bad.code(), Some("E_BAD_INPUT"), "{given}: {bad:?}");
        let Reply::ToolError(e) = bad else {
            panic!("expected a tool error")
        };
        assert_eq!(
            e["fix"],
            "pass canon, a version as tag:<version>, a view name, view:<name> or proposal:P-x@n",
            "{e}"
        );
    }
}

/// Final review M5 and M1: a view name that needs percent-encoding gets a link that serves; a
/// view named `.` or `..` gets none, since a browser reads that name as a path step.
#[test]
fn ls7_rdy_site_for_any_view_name() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    for view in ["team/a b", ".."] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "doc", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}}]),
        )
        .unwrap();
    }
    r.close().unwrap();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({"scope": "team/a b"})).ok();
    assert_eq!(v["scope"], "view:team/a b", "{v}");
    let url = v["url"].as_str().unwrap();
    assert!(url.ends_with("/~view/team%2Fa%20b/"), "{v}");
    let got = page(url);
    assert_eq!(got.status, 200, "{v}");
    assert!(
        String::from_utf8_lossy(&got.body).contains("Notes"),
        "the view's page"
    );
    let bad = c.call("rdy_site", json!({"scope": ".."}));
    assert_eq!(bad.code(), Some("E_BAD_INPUT"), "{bad:?}");
}

/// The render warning of a hidden glossary doc (D-Y4884, Task 12).
const HIDDEN_GLOSSARY: &str = "the top-level doc named glossary has no page: glossary.html is the site's generated glossary. Rename the doc to publish it.";

/// Ruling final (I1): the live path warns as `rdy render` does. `rdy_site` builds the view first
/// and carries its warning; the daemon log has it once, however often the scope is refreshed or
/// built again with the same warning; `rdy site` prints it as `rdy render` does.
#[test]
fn ls_final_a_hidden_glossary_doc_warns_on_the_live_path() {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    write(&mut r, "g1", json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Our words", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$g", "fields": {"title": "Words"}},
        {"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Hidden"}, "body": "Kept apart."}
    ]))
    .unwrap();
    r.close().unwrap();
    let mut c = agent(&s);
    let v = c.call("rdy_site", json!({"scope": "g1"})).ok();
    assert_eq!(v["warnings"], json!([HIDDEN_GLOSSARY]), "{v}");
    let url = v["url"].as_str().unwrap().to_string();
    // two more refreshes: a page request of the same build, then a commit to the view and
    // rdy_site again (a new build with the same warning)
    assert_eq!(page(&url).status, 200);
    let (ok, made) = s.json(&["--json", "new", "doc", "more", "--view", "g1"]);
    assert!(ok, "{made}");
    let again = c.call("rdy_site", json!({"scope": "g1"})).ok();
    assert_eq!(again["warnings"], json!([HIDDEN_GLOSSARY]), "{again}");
    let log = std::fs::read_to_string(s.run_dir().log_file()).unwrap_or_default();
    assert_eq!(
        log.matches(&format!("site: view:g1: warning: {HIDDEN_GLOSSARY}\n"))
            .count(),
        1,
        "{log}"
    );
    let out = s.rdy(&["site", "--scope", "g1"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // the scope has a diff summary, so `changes:` follows the URL (sr2)
    let changes = again["changes"]
        .as_str()
        .expect("a view has a changes link");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{url}\nchanges: {changes}\nwarning: {HIDDEN_GLOSSARY}\n")
    );
}

#[test]
fn ls7_rdy_site_needs_a_session() {
    let (s, _, _) = golden_served();
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_site", json!({}));
    assert_eq!(r.code(), Some("E_NO_SESSION"), "{r:?}");
}

#[test]
fn ls7_no_daemon_has_no_site() {
    let (s, _, _) = golden_served();
    let mut c = McpClient::start(s.dir.path());
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let r = c.call("rdy_site", json!({}));
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    assert_eq!(e["code"], "E_SITE", "{e}");
    assert_eq!(
        e["message"], "the live site is served by the daemon, and this session runs without one",
        "{e}"
    );
    assert_eq!(
        e["fix"], "drop --no-daemon; for a static site, run rdy render --out <dir>",
        "{e}"
    );
    let _: Value = e;
}

#[test]
fn ls7_lan_mode_gives_the_lan_link_and_the_warning() {
    let log = rdy_qa::daemon::site_log();
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/local.yaml"),
        "serve:\n  site_lan: true\n",
    )
    .unwrap();
    let (_cfg, env) = human_env("human:qa");
    let h = Hosted::start_for_binary(dir, Some(LanOverride::Addr(Ipv4Addr::LOCALHOST)));
    let made = human_command(
        h.dir.path(),
        &env,
        &["new", "doc", "ops", "--view", "editing"],
    )
    .output()
    .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let mut c = McpClient::start_proxy_at(h.dir.path(), &env);
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    let v = c.call("rdy_site", json!({"scope": "editing"})).ok();
    let url = v["url"].as_str().unwrap();
    let lan = v["lan_url"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert!(lan.starts_with("http://127.0.0.1:"), "{v}");
    assert!(lan.ends_with("/~view/editing/"), "{v}");
    assert!(url.ends_with("/~view/editing/"), "{v}");
    let w = &v["warning"];
    assert_eq!(w["code"], "W_SITE_LAN", "{v}");
    assert_eq!(
        w["fix"], "set serve.site_lan to false to keep the site on this machine",
        "{v}"
    );
    // final review M3: the message gives no fix of its own, so an agent relays the fix once
    let root = lan.strip_suffix("~view/editing/").unwrap();
    assert_eq!(
        w["message"],
        format!("anyone on your network can read these docs at {root}"),
        "{v}"
    );
    // the diff summary link has a LAN twin
    let changes = v["changes"].as_str().unwrap_or_else(|| panic!("{v}"));
    let lan_changes = v["lan_changes"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert!(changes.ends_with("/~view/editing/changes.html"), "{v}");
    assert!(lan_changes.ends_with("/~view/editing/changes.html"), "{v}");
    // canon: the LAN link is the bare root
    let canon = c.call("rdy_site", json!({})).ok();
    assert!(
        canon["lan_url"].as_str().unwrap().ends_with('/')
            && !canon["lan_url"].as_str().unwrap().contains('~'),
        "{canon}"
    );
    // the daemon warns once, where the person running it sees it
    assert_eq!(log.lines(), [rdy_qa::daemon::LAN_DOCS_LINE]);
}
