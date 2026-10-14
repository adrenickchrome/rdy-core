//! ls7: `rdy site`, the CLI twin of `rdy_site` (D-N5NSN, D-FV6G9, D-KQ4CA).
use rdy_qa::daemon::Served;
use rdy_qa::http;

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
