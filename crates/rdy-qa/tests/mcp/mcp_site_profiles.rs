//! spv9 (D-CWC91, D-DK03K, D-KQ4CA, D-8QV9G): `rdy_site {profile, only}` and the widened
//! `rdy_publication_check` scopes. The tool takes the profile and the profile-only flag, shares
//! the ctl reply builder, and adds no tool. A profile that only a proposal holds can be checked
//! and opened before anything lands. The site is a read-only view: no tool here approves,
//! publishes or controls access.
use rdy_daemon::site::LanOverride;
use rdy_engine::engine::{Engine, site_reply};
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::tools::ERROR_ROWS;
use rdy_qa::daemon::{Hosted, short_tempdir};
use rdy_qa::fixtures::{approver, human_env, land, write, writer};
use rdy_qa::golden::tiny;
use rdy_qa::http;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The caveat a profile-only site carries, for the profile titled `title`.
fn caveat(title: &str) -> String {
    format!(
        "this site shows only the profile \"{title}\"; anyone with the project still reads everything give the full site's link from rdy site when the person needs everything"
    )
}

/// The tool description of `rdy_site`.
const SITE_DESCRIPTION: &str = "Get the live docs link for the person. Pass profile to show its selection in canon, a tag, a view or a proposal. Pass only with profile to serve canon on a separate port. This changes presentation, not source access. Relay warning and every entry of warnings. Use the tools to read and write.";
/// The tool description of `rdy_publication_check`.
const CHECK_DESCRIPTION: &str = "Check a publication profile's selected docs and references. Scope may be canon, a tag, a landing, a view or a proposal. Pass implicit:org for every doc. Against accepts canon, a tag or a landing with the same profile id and audience. This check does not authorize publication or find the last publication. Relay warnings to the person.";
const PROFILE_HELP: &str =
    "A publication profile id or key in this scope, or implicit:org. Omit it for all docs.";
const ONLY_HELP: &str = "Serve canon through this profile on a separate port. Requires profile and scope canon. This changes presentation, not source access.";
const CHECK_SCOPE_HELP: &str =
    "canon (default), tag:<name>, landing:<hash>, a view name, view:<name>, or proposal:P-x@n.";
/// The number of tools the server lists, which the profile task left as it found it: 47. cp11
/// adds `rdy_plan`, and the count is 48. rf12 adds `rdy_trace`, and the count is 49. ok11 adds
/// `rdy_upgrade`, and the count is 50. ap5 adds `rdy_dispute`, and the count is 51.
const TOOL_COUNT: usize = 51;

/// A running project: the host, the proxy environment and what the pins need.
struct World {
    h: Hosted,
    env: Vec<(String, String)>,
    _cfg: tempfile::TempDir,
    /// The version-1 ref of proposal `newprof`, which adds profile `fresh`.
    newprof: String,
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> BTreeMap<String, String> {
    write(r, view, ops)
        .unwrap_or_else(|e| panic!("write to {view}: {e}"))
        .created
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect()
}

fn profile_op(key: &str, title: &str, audience: &str, admin: &str, select: &[&str]) -> Value {
    json!({"op": "create", "type": "publication_profile", "id": format!("${key}"),
        "parent": admin,
        "fields": {"key": key, "title": title, "description": format!("About {title}."),
            "audience": audience, "select": select}})
}

/// Canon holds a folder `manual` (doc `intro`, prose `INTRO-ONLY-1`), a top-level `secret` doc
/// (prose `SECRET-WORDS`), a top-level `leak` doc that refers to `secret`, and an `admin` doc
/// that holds the profiles `guide` (org, `Guide book`, the folder) and `open` (public, `Open
/// book`, the leak doc). Tag `v1.0.0` is canon. View `work` edits intro. Proposal `newprof`
/// adds profile `fresh` (org, `Fresh book`, the secret doc), which canon does not hold.
fn world() -> World {
    let (dir, mut r) = tiny();
    let mut ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-ONLY-1"},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$secret", "body": "SECRET-WORDS"},
            {"op": "create", "type": "doc", "id": "$leak", "fields": {"name": "leak", "title": "Leak page", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    let (secret, admin, leak, manual) = (
        ids["$secret"].clone(),
        ids["$admin"].clone(),
        ids["$leak"].clone(),
        ids["$manual"].clone(),
    );
    put(
        &mut r,
        "w1",
        json!([{"op": "create", "type": "prose", "parent": leak,
            "body": format!("See {{{{{secret}}}}}.")}]),
    );
    land(&mut r, "w1").unwrap();
    put(
        &mut r,
        "admin",
        json!([
            profile_op("guide", "Guide book", "org", &admin, &[&manual]),
            profile_op("open", "Open book", "public", &admin, &[&leak])
        ]),
    );
    land(&mut r, "admin").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let ip = ids.remove("$ip").unwrap();
    put(
        &mut r,
        "work",
        json!([{"op": "update", "id": ip, "body": "INTRO-VIEW"}]),
    );
    put(
        &mut r,
        "newprof",
        json!([profile_op("fresh", "Fresh book", "org", &admin, &[&secret])]),
    );
    let newprof = r
        .propose(&writer(), "newprof", "Fresh profile", None)
        .unwrap()
        .r#ref;
    drop(r);
    let (cfg, env) = human_env("human:qa");
    let h = Hosted::start_for_binary(dir, Some(LanOverride::Addr(std::net::Ipv4Addr::LOCALHOST)));
    World {
        h,
        env,
        _cfg: cfg,
        newprof,
    }
}

impl World {
    fn engine(&self) -> Arc<dyn Engine> {
        self.h.run.as_ref().unwrap().shared().engine()
    }

    /// A proxy client with a session of `role`.
    fn client(&self, role: &str) -> McpClient {
        let mut c = McpClient::start_proxy_at(self.h.dir.path(), &self.env);
        c.call(
            "rdy_session",
            json!({"actor": format!("agent:{role}"), "run": role, "role": role}),
        )
        .ok();
        c
    }

    fn landings(&self) -> usize {
        self.engine().read().landings().unwrap().len()
    }
}

fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// What `rdy site` and the `site_url` op give: the one reply builder, called on the daemon.
fn ctl(
    w: &World,
    scope: Option<&str>,
    profile: Option<&str>,
    only: bool,
) -> Result<Value, RdyError> {
    site_reply(&*w.engine(), scope, profile, only)
}

/// A page of the site at `url` (a root, with `rel` after it): its status and body text.
fn get(url: &str, rel: &str) -> (u16, String) {
    let r = http::get(&format!("{url}{rel}"));
    (r.status, String::from_utf8_lossy(&r.body).into_owned())
}

fn all_keys(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => m.iter().for_each(|(k, x)| {
            out.push(k.clone());
            all_keys(x, out);
        }),
        Value::Array(a) => a.iter().for_each(|x| all_keys(x, out)),
        _ => {}
    }
}

#[test]
fn spv9_mcp_profile_route_can_review_an_unlanded_profile() {
    let w = world();
    let mut c = w.client("writer");
    let scope = format!("proposal:{}", w.newprof);
    let landings = w.landings();

    // canon does not hold the profile; the proposal does
    let (code, ..) = refused(c.call("rdy_site", json!({"profile": "fresh"})));
    assert_eq!(code, "E_NOT_FOUND");
    let check = c
        .call(
            "rdy_publication_check",
            json!({"profile": "fresh", "scope": scope}),
        )
        .ok();
    assert_eq!(check["profile"]["key"], "fresh", "{check}");
    assert_eq!(check["scope"]["kind"], "proposal", "{check}");
    assert_eq!(check["selection"]["docs"], 1, "{check}");
    assert_eq!(check["references"]["status"], "clear", "{check}");

    // the live link: the proposal's route, the profile's selected page and only it
    let site = c
        .call("rdy_site", json!({"profile": "fresh", "scope": scope}))
        .ok();
    let url = site["url"].as_str().unwrap().to_string();
    assert!(
        url.ends_with(&format!("~proposal/{}/~profile/fresh/", w.newprof)),
        "{site}"
    );
    assert_eq!(site["scope"], scope.as_str(), "{site}");
    assert_eq!(
        site["profile"],
        json!({"key": "fresh", "title": "Fresh book", "audience": "org"}),
        "{site}"
    );
    assert!(site.get("profile_only").is_none(), "{site}");
    let (status, body) = get(&url, "secret.html");
    assert_eq!(status, 200, "{site}");
    assert!(body.contains("SECRET-WORDS"), "{body}");
    assert_eq!(
        get(&url, "manual/intro.html").0,
        404,
        "intro is not selected"
    );
    // the same reply by ctl
    assert_eq!(
        ctl(&w, Some(&scope), Some("fresh"), false).unwrap(),
        site,
        "ctl and the tool agree"
    );
    // looking at the page made no landing, and the replies hold no approval or access claim
    assert_eq!(w.landings(), landings, "no landing was made");
    let mut keys = vec![];
    all_keys(&site, &mut keys);
    all_keys(&check, &mut keys);
    for k in &keys {
        for word in ["receipt", "approv", "authoriz", "publish", "access"] {
            assert!(!k.contains(word), "{k}");
        }
    }
    // a view reads its own revision of the folder
    let view = c
        .call("rdy_site", json!({"profile": "guide", "scope": "work"}))
        .ok();
    assert_eq!(view["scope"], "view:work", "{view}");
    assert!(
        view["url"]
            .as_str()
            .unwrap()
            .ends_with("~view/work/~profile/guide/"),
        "{view}"
    );
    let (status, body) = get(view["url"].as_str().unwrap(), "manual/intro.html");
    assert_eq!(status, 200);
    assert!(body.contains("INTRO-VIEW"), "{body}");
}

#[test]
fn spv9_mcp_only_starts_reuses_and_warns() {
    let log = rdy_qa::daemon::site_log();
    let w = world();
    let source = w.client("writer").call("rdy_site", json!({})).ok();
    let mut first_url = None;
    for role in ["reader", "writer", "reviewer"] {
        let mut c = w.client(role);
        let only = c
            .call("rdy_site", json!({"profile": "Guide", "only": true}))
            .ok();
        assert_eq!(
            only,
            json!({
                "url": only["url"],
                "scope": "canon",
                "profile": {"key": "guide", "title": "Guide book", "audience": "org"},
                "profile_only": true,
                "warnings": [caveat("Guide book")],
            }),
            "{role}: {only}"
        );
        let url = only["url"].as_str().unwrap().to_string();
        match &first_url {
            None => first_url = Some(url.clone()),
            Some(first) => assert_eq!(&url, first, "{role}: one listener per key"),
        }
        assert_ne!(url, source["url"].as_str().unwrap(), "{role}: its own port");
        // the profile's pages answer there, and nothing else
        assert_eq!(get(&url, "manual/intro.html").0, 200, "{role}");
        assert_eq!(get(&url, "secret.html").0, 404, "{role}");
        // the ctl reply is the same value
        assert_eq!(ctl(&w, None, Some("guide"), true).unwrap(), only, "{role}");
    }
    // the source site stays up beside it, whole
    let again = w.client("writer").call("rdy_site", json!({})).ok();
    assert_eq!(again["url"], source["url"], "{again}");
    assert_eq!(get(again["url"].as_str().unwrap(), "secret.html").0, 200);
    // `only: false` is an ordinary route with no caveat
    let plain = w
        .client("reader")
        .call("rdy_site", json!({"profile": "guide", "only": false}))
        .ok();
    assert!(plain.get("profile_only").is_none() && plain.get("warnings").is_none());
    // the daemon gives the caveat once, when the one listener starts
    assert_eq!(log.lines(), [rdy_qa::daemon::GUIDE_CAVEAT_LINE]);
}

#[test]
fn spv9_mcp_profile_params_are_strict() {
    let w = world();
    let mut c = McpClient::start_proxy_at(w.h.dir.path(), &w.env);
    // a session is required, with plain parameters and with profile-only ones
    for args in [
        json!({"profile": "guide"}),
        json!({"profile": "guide", "only": true}),
    ] {
        let r = c.call("rdy_site", args.clone());
        assert_eq!(r.code(), Some("E_NO_SESSION"), "{args}: {r:?}");
    }
    c.call(
        "rdy_session",
        json!({"actor": "agent:a", "run": "a", "role": "writer"}),
    )
    .ok();
    // a misspelt key names the key it was close to; so does any other unknown key
    let (code, message, fix) = refused(c.call("rdy_site", json!({"profiel": "guide"})));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(message, "rdy_site: unknown parameter \"profiel\"");
    assert!(fix.starts_with("did you mean \"profile\"?"), "{fix}");
    let (code, _, fix) = refused(c.call("rdy_site", json!({"profile": "guide", "onlyy": true})));
    assert_eq!(code, "E_BAD_INPUT");
    assert!(fix.contains("only"), "{fix}");
    let (code, ..) = refused(c.call("rdy_site", json!({"profile": "guide", "view": "work"})));
    assert_eq!(code, "E_BAD_INPUT");
    // only is a boolean
    for bad in [json!("yes"), json!(1), json!("true")] {
        let (code, message, _) =
            refused(c.call("rdy_site", json!({"profile": "guide", "only": bad.clone()})));
        assert_eq!(code, "E_BAD_INPUT", "{bad}");
        assert!(message.starts_with("rdy_site: "), "{message}");
    }
    let (code, ..) = refused(c.call("rdy_site", json!({"profile": 7})));
    assert_eq!(code, "E_BAD_INPUT");
    // only needs a profile and canon; the refusal is the ctl one
    for args in [
        json!({"only": true}),
        json!({"only": true, "scope": "canon"}),
        json!({"profile": "guide", "only": true, "scope": "work"}),
        json!({"profile": "guide", "only": true, "scope": "tag:v1.0.0"}),
    ] {
        let (code, message, fix) = refused(c.call("rdy_site", args.clone()));
        assert_eq!(code, "E_BAD_INPUT", "{args}");
        assert_eq!(
            message, "a profile-only site needs a profile and shows canon only",
            "{args}"
        );
        assert_eq!(
            fix, "pass profile with scope canon, or leave out only",
            "{args}"
        );
    }
    // an unknown profile is not found
    let (code, message, _) = refused(c.call("rdy_site", json!({"profile": "nope"})));
    assert_eq!(code, "E_NOT_FOUND");
    assert_eq!(message, "Publication profile \"nope\" was not found.");
    // scope keeps its spellings
    for scope in ["canon", "work", "view:work", "tag:v1.0.0", "tag:1.0.0"] {
        let v = c
            .call("rdy_site", json!({"profile": "guide", "scope": scope}))
            .ok();
        assert_eq!(v["profile"]["key"], "guide", "{scope}: {v}");
    }
}

#[test]
fn spv9_mcp_checks_review_scopes_but_keeps_output_narrow() {
    let w = world();
    let mut c = w.client("writer");
    let scope = format!("proposal:{}", w.newprof);
    let canon = c
        .call("rdy_publication_check", json!({"profile": "guide"}))
        .ok();
    // a view, a tag and a proposal are candidates; the report has the one closed shape
    let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    for (profile, candidate) in [
        ("guide", "work".to_string()),
        ("guide", "view:work".to_string()),
        ("guide", "tag:v1.0.0".to_string()),
        ("fresh", scope.clone()),
    ] {
        let v = c
            .call(
                "rdy_publication_check",
                json!({"profile": profile, "scope": candidate}),
            )
            .ok();
        assert_eq!(keys(&v), keys(&canon), "{candidate}");
    }
    // a blocked public profile in a view is a successful report, with no warning of its own
    let blocked = c
        .call(
            "rdy_publication_check",
            json!({"profile": "open", "scope": "work"}),
        )
        .ok();
    assert_eq!(blocked["references"]["status"], "blocked", "{blocked}");
    assert_eq!(blocked["warnings"], json!([]), "{blocked}");
    // a baseline is still a stable scope: a view or a proposal is refused
    for against in ["work", "view:work", scope.as_str()] {
        let (code, message, _) = refused(c.call(
            "rdy_publication_check",
            json!({"profile": "guide", "against": against}),
        ));
        assert_eq!(code, "E_BAD_INPUT", "{against}");
        assert_eq!(
            message, "Publication check against must be canon, tag:<name>, or landing:<hash>.",
            "{against}"
        );
    }
    // profiled static output still reads only canon, a tag or a landing
    let scratch = short_tempdir();
    for (tool, extra) in [
        ("rdy_render", json!({})),
        ("rdy_export", json!({"format": "html"})),
    ] {
        for (i, bad) in ["work", "view:work", scope.as_str()]
            .into_iter()
            .enumerate()
        {
            let target = scratch.path().join(format!("{tool}-{i}"));
            let mut args =
                json!({"out": target.to_str().unwrap(), "scope": bad, "profile": "guide"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let (code, ..) = refused(c.call(tool, args));
            assert_eq!(code, "E_BAD_INPUT", "{tool} {bad}");
            assert!(!target.exists(), "{tool} {bad}");
        }
    }
}

#[test]
fn spv9_mcp_schema_and_error_rows_describe_the_same_contract() {
    let w = world();
    let mut c = w.client("reader");
    let tools = c.tools();
    // no tool was added, renamed or removed
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(names.len(), TOOL_COUNT, "{names:?}");
    for kept in [
        "rdy_site",
        "rdy_render",
        "rdy_export",
        "rdy_publication_check",
    ] {
        assert!(names.contains(&kept), "{kept}");
    }
    assert!(!names.iter().any(|n| n.contains("profile")), "{names:?}");
    let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();

    let site = find("rdy_site");
    assert_eq!(site["description"], SITE_DESCRIPTION);
    let props = site["inputSchema"]["properties"].as_object().unwrap();
    let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["only", "profile", "scope"]);
    assert_eq!(props["profile"]["description"], PROFILE_HELP);
    assert_eq!(props["only"]["description"], ONLY_HELP);
    assert!(
        props["scope"]["description"]
            .as_str()
            .unwrap()
            .starts_with("canon (default), a version as tag:<version>"),
        "scope keeps its spellings: {}",
        props["scope"]
    );
    assert!(
        site["inputSchema"]
            .get("required")
            .is_none_or(|r| r == &json!([])),
        "nothing is required"
    );

    let check = find("rdy_publication_check");
    assert_eq!(check["description"], CHECK_DESCRIPTION);
    let props = &check["inputSchema"]["properties"];
    assert_eq!(props["scope"]["description"], CHECK_SCOPE_HELP);
    assert_eq!(
        props["against"]["description"],
        "A baseline to compare with: canon, tag:<name> or landing:<hash>. It must hold the same profile id and audience. Omit it for no comparison."
    );
    // render and export keep their static scope rule
    for tool in ["rdy_render", "rdy_export"] {
        let scope = find(tool)["inputSchema"]["properties"]["scope"]["description"]
            .as_str()
            .unwrap();
        assert!(
            scope.contains("With a profile: canon, tag:<version> or landing:<id>."),
            "{tool}: {scope}"
        );
    }

    // the error rows: W_SITE_PROFILE follows W_SITE_LAN; the two publication rows stay last
    let codes: Vec<&str> = ERROR_ROWS.iter().map(|(c, _, _)| *c).collect();
    let lan = codes.iter().position(|c| *c == "W_SITE_LAN").unwrap();
    assert_eq!(codes[lan + 1], "W_SITE_PROFILE");
    assert_eq!(
        &codes[codes.len() - 2..],
        ["E_PUBLICATION_REFERENCE", "W_PUBLICATION_REFERENCE"]
    );
    let row = |code: &str| ERROR_ROWS.iter().find(|(c, _, _)| *c == code).unwrap();
    assert_eq!(
        *row("W_SITE_PROFILE"),
        (
            "W_SITE_PROFILE",
            "a profile-only site is open, or its profile is no longer in canon",
            "relay the warning; use rdy site for the full source link, or restore the profile"
        )
    );
    assert_eq!(
        *row("E_SITE"),
        (
            "E_SITE",
            "the live site has no daemon, cannot bind its port, has reached its profile-site limit, or is stopping",
            "follow the error's fix"
        )
    );
    assert_eq!(
        *row("W_PUBLICATION_REFERENCE"),
        (
            "W_PUBLICATION_REFERENCE",
            "an org profile has references outside its selection, or a live public profile has blocked references",
            "follow the warning's own fix"
        )
    );
}

/// Final review I1: a canon profile route that generates `changes-since.html` (selected landings
/// since its tag) gives `changes` and `lan_changes` through the real build-facts-to-reply path,
/// for a named profile and for `implicit:org`. A profile with no selected landing since the tag,
/// a tag route and a profile-only site give none.
#[test]
fn spv9_canon_profile_replies_link_the_summary_since_the_tag() {
    let log = rdy_qa::daemon::site_log();
    let (dir, mut r) = tiny();
    let ids = put(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ip", "parent": "$intro", "body": "INTRO-ONLY-1"},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$secret", "body": "SECRET-WORDS"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    );
    land(&mut r, "w1").unwrap();
    put(
        &mut r,
        "admin",
        json!([
            profile_op(
                "guide",
                "Guide book",
                "org",
                &ids["$admin"],
                &[&ids["$manual"]]
            ),
            profile_op(
                "other",
                "Other book",
                "org",
                &ids["$admin"],
                &[&ids["$secret"]]
            )
        ]),
    );
    land(&mut r, "admin").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    // one landing since the tag, in `guide`'s selection only
    put(
        &mut r,
        "w2",
        json!([{"op": "update", "id": ids["$ip"], "body": "INTRO-SINCE"}]),
    );
    land(&mut r, "w2").unwrap();
    drop(r);
    std::fs::write(
        dir.path().join(".rdy/local.yaml"),
        "serve:\n  site_lan: true\n",
    )
    .unwrap();
    let h = Hosted::start_with(dir, Some(LanOverride::Addr(std::net::Ipv4Addr::LOCALHOST)));
    let engine = h.run.as_ref().unwrap().shared().engine();
    let reply = |scope: Option<&str>, profile: Option<&str>, only: bool| {
        site_reply(&*engine, scope, profile, only)
            .unwrap_or_else(|e| panic!("{scope:?} {profile:?}: {e}"))
    };

    // a named profile and the implicit profile on canon: both links, each to a served summary
    for (profile, key) in [("guide", "guide"), ("implicit:org", "implicit%3Aorg")] {
        let v = reply(None, Some(profile), false);
        let url = v["url"].as_str().unwrap();
        let lan = v["lan_url"].as_str().unwrap_or_else(|| panic!("{v}"));
        assert!(url.ends_with(&format!("/~profile/{key}/")), "{v}");
        assert_eq!(
            v["changes"].as_str(),
            Some(format!("{url}changes-since.html").as_str()),
            "{profile}: {v}"
        );
        assert_eq!(
            v["lan_changes"].as_str(),
            Some(format!("{lan}changes-since.html").as_str()),
            "{profile}: {v}"
        );
        let (status, body) = get(url, "changes-since.html");
        assert_eq!(status, 200, "{profile}: {v}");
        assert!(body.contains("v1.0.0"), "{profile}: {body}");
        assert_eq!(get(lan, "changes-since.html").0, 200, "{profile}: {v}");
    }
    // no selected landing since the tag: no summary, no link
    let other = reply(None, Some("other"), false);
    assert!(other.get("changes").is_none(), "{other}");
    assert!(other.get("lan_changes").is_none(), "{other}");
    assert_eq!(
        get(other["url"].as_str().unwrap(), "changes-since.html").0,
        404,
        "{other}"
    );
    // a tag route and the profile-only site have none
    let tag = reply(Some("tag:v1.0.0"), Some("guide"), false);
    assert!(
        tag.get("changes").is_none() && tag.get("lan_changes").is_none(),
        "{tag}"
    );
    let only = reply(None, Some("guide"), true);
    assert!(
        only.get("changes").is_none() && only.get("lan_changes").is_none(),
        "{only}"
    );
    // the daemon warns of the LAN sites and the profile-only site
    use rdy_qa::daemon::{GUIDE_CAVEAT_LINE, GUIDE_LAN_LINE, LAN_DOCS_LINE, sorted_lines};
    assert_eq!(
        sorted_lines(&log.lines()),
        sorted_lines(&[LAN_DOCS_LINE, GUIDE_LAN_LINE, GUIDE_CAVEAT_LINE])
    );
}
