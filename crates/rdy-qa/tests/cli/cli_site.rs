//! ls7: `rdy site`, the CLI twin of `rdy_site` (D-N5NSN, D-FV6G9, D-KQ4CA).
use rdy_daemon::site::LanOverride;
use rdy_engine::Rdy;
use rdy_qa::daemon::{Hosted, Served, human_command, short_tempdir};
use rdy_qa::fixtures::human_env;
use rdy_qa::http;
use std::net::Ipv4Addr;

fn stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `http://127.0.0.1:<digits>/` and a newline, nothing else.
fn is_the_link(text: &str) -> bool {
    text.strip_prefix("http://127.0.0.1:")
        .and_then(|rest| rest.strip_suffix("/\n"))
        .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

#[test]
fn ls7_rdy_site_prints_the_link() {
    let s = Served::new();
    let text = stdout(&s.rdy(&["site"]));
    assert!(is_the_link(&text), "{text:?}");
    let (ok, v) = s.json(&["--json", "site"]);
    assert!(ok, "{v}");
    assert_eq!(format!("{}\n", v["url"].as_str().unwrap()), text, "{v}");
    assert_eq!(v["scope"], "canon", "{v}");
    assert_eq!(http::get(text.trim_end()).status, 200);
}

#[test]
fn ls7_rdy_site_scope() {
    let s = Served::new();
    // a view exists once a change is written to it
    let (ok, v) = s.json(&["--json", "new", "doc", "ops", "--view", "editing"]);
    assert!(ok, "{v}");
    let text = stdout(&s.rdy(&["site", "--scope", "editing"]));
    let first = text.lines().next().unwrap();
    assert!(
        first.starts_with("http://127.0.0.1:") && first.ends_with("/~view/editing/"),
        "{text:?}"
    );
    // a view has a second line, its change summary (sr2)
    assert_eq!(text.lines().count(), 2, "{text:?}");
}

#[test]
fn sr2_rdy_site_prints_the_changes_line() {
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "new", "doc", "ops", "--view", "editing"]);
    assert!(ok, "{v}");
    let text = stdout(&s.rdy(&["site", "--scope", "view:editing"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    assert!(lines[0].ends_with("/~view/editing/"), "{text:?}");
    let want = format!("changes: {}changes.html", lines[0]);
    assert_eq!(lines[1], want, "{text:?}");
    // JSON carries the field as the reply has it
    let (ok, v) = s.json(&["--json", "site", "--scope", "view:editing"]);
    assert!(ok, "{v}");
    assert_eq!(
        format!("changes: {}", v["changes"].as_str().unwrap()),
        want,
        "{v}"
    );
    // canon of a project with no version: one line, as before
    let canon = stdout(&s.rdy(&["site"]));
    assert_eq!(canon.lines().count(), 1, "{canon:?}");
}

#[test]
fn ls7_rdy_site_in_lan_mode_prints_the_lan_link_and_warns() {
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
    let out = human_command(h.dir.path(), &env, &["site"])
        .output()
        .unwrap();
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    assert!(is_the_link(&format!("{}\n", lines[0])), "{text:?}");
    assert_eq!(
        lines[1], lines[0],
        "the LAN stand-in is 127.0.0.1: {text:?}"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    // final review M3: the fix is printed once, on its own line, as an error's is
    let warning = format!(
        "warning: anyone on your network can read these docs at {}\nfix: set serve.site_lan to false to keep the site on this machine\n",
        lines[1]
    );
    assert_eq!(err, warning.as_str(), "{err:?}");

    let out = human_command(h.dir.path(), &env, &["--json", "site"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["lan_url"], lines[1], "{v}");
    assert_eq!(v["warning"]["code"], "W_SITE_LAN", "{v}");
    // the daemon warns once, where the person running it sees it
    assert_eq!(log.lines(), [rdy_qa::daemon::LAN_DOCS_LINE]);
}

/// Fix round 1: `changes:` is the second line, before the LAN URL.
#[test]
fn sr2_rdy_site_prints_changes_before_the_lan_link() {
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
    let out = human_command(
        h.dir.path(),
        &env,
        &["--json", "new", "doc", "ops", "--view", "editing"],
    )
    .output()
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = human_command(h.dir.path(), &env, &["site", "--scope", "view:editing"])
        .output()
        .unwrap();
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text:?}");
    assert!(lines[0].ends_with("/~view/editing/"), "{text:?}");
    assert_eq!(
        lines[1],
        format!("changes: {}changes.html", lines[0]),
        "{text:?}"
    );
    assert_eq!(
        lines[2], lines[0],
        "the LAN stand-in is 127.0.0.1: {text:?}"
    );
    assert_eq!(log.lines(), [rdy_qa::daemon::LAN_DOCS_LINE]);
}

/// dgm11: each link `rdy site` prints reaches the scope's own themed not-found page for a page
/// it does not have, with the same status and words as the tool's link.
#[test]
fn dgm11_cli_site_links_reach_themed_404() {
    const PAGE: &str =
        "There is no page at this address. It may have moved, or the link may be wrong.";
    let s = Served::new();
    let (ok, v) = s.json(&["--json", "new", "doc", "ops", "--view", "editing"]);
    assert!(ok, "{v}");
    for args in [&["site"][..], &["site", "--scope", "view:editing"][..]] {
        let text = stdout(&s.rdy(args));
        let url = text.lines().next().unwrap();
        assert_eq!(http::get(url).status, 200, "{args:?}");
        let r = http::get(&format!("{url}nosuch/page.html"));
        assert_eq!(r.status, 404, "{args:?}: {}", r.head);
        assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
        let body = String::from_utf8_lossy(&r.body);
        let root = &url[url[7..].find('/').unwrap() + 7..];
        assert!(
            body.contains("<html lang=\"en\" data-rdy-kind=\"not-found\">"),
            "{args:?}: {body}"
        );
        assert!(body.contains("<h1>Page not found</h1>"), "{args:?}");
        assert!(body.contains(&format!("<p>{PAGE}</p>")), "{args:?}");
        assert!(
            body.contains(&format!("href=\"{root}index.html\"")),
            "{args:?}: the home link at {root}"
        );
        assert!(
            !body.contains("/_rdy/live.js") && !body.contains("data-events"),
            "{args:?}"
        );
    }
}
