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
    assert!(
        text.starts_with("http://127.0.0.1:") && text.ends_with("/~view/editing/\n"),
        "{text:?}"
    );
    assert_eq!(text.lines().count(), 1, "{text:?}");
}

#[test]
fn ls7_rdy_site_in_lan_mode_prints_the_lan_link_and_warns() {
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
    let warning = format!(
        "warning: anyone on your network can read these docs at {}. Set serve.site_lan to false to keep the site on this machine.\n",
        lines[1]
    );
    assert_eq!(err, warning.as_str(), "{err:?}");

    let out = human_command(h.dir.path(), &env, &["--json", "site"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["lan_url"], lines[1], "{v}");
    assert_eq!(v["warning"]["code"], "W_SITE_LAN", "{v}");
}
