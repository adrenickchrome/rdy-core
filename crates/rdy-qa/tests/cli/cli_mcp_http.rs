//! mht7 (D-6M0AF, D-QG54Z, D-5GFFX): the CLI's side of MCP over HTTP. `rdy mcp --http-config`
//! prints the live endpoint and the project's bearer and nothing else; it selects its project as
//! `rdy mcp` does, creates no meta project, never enables the setting and needs the daemon. The
//! status and `rdy site` name the endpoint without the token, and a profile result names the
//! full endpoint, never a profile port. Every URL is read from the command's own output (W5).
use rdy_daemon::site::addr;
use rdy_engine::Rdy;
use rdy_qa::daemon::{Served, human_command, short_tempdir};
use rdy_qa::fixtures::{land, output_within, write};
use rdy_qa::instructions::World;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

const ON: &str = "serve:\n  mcp_http: true\n";
const BUDGET: Duration = Duration::from_secs(120);

fn local(dir: &Path, config: &str) {
    std::fs::write(dir.join(".rdy/local.yaml"), config).unwrap();
}

/// A served project with MCP over HTTP on.
fn on() -> Served {
    let s = Served::new();
    local(s.dir.path(), ON);
    s
}

fn out(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The project's bearer, as its private token file holds it.
fn token(dir: &Path) -> String {
    let text = std::fs::read_to_string(dir.join(".rdy/run/mcp-token")).expect("the token file");
    text.strip_suffix('\n').expect("one LF").to_string()
}

/// `http://127.0.0.1:<port>/~mcp`: its port.
fn port_of(url: &str) -> u16 {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|r| r.strip_suffix("/~mcp"))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("not a loopback MCP endpoint: {url}"))
}

fn error_of(o: &Output) -> Value {
    let text = err(o);
    serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("JSON in {text}"))
}

#[test]
fn mht7_http_config_prints_only_url_and_authorization() {
    let s = on();
    let text = out(&s.rdy(&["mcp", "--http-config"]));
    let token = token(s.dir.path());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    let url = lines[0].strip_prefix("url: ").expect("the url line");
    port_of(url);
    assert_eq!(lines[1], format!("Authorization: Bearer {token}"));
    assert!(text.ends_with('\n'));
    // the JSON is exactly the client's header block, in this key order
    let json = out(&s.rdy(&["--json", "mcp", "--http-config"]));
    assert_eq!(
        json,
        format!("{{\"url\":\"{url}\",\"headers\":{{\"Authorization\":\"Bearer {token}\"}}}}\n")
    );
    // it is the same endpoint the status and the site name
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    assert_eq!(status["mcp_http"]["url"], url);
    // a flag that is not there is refused by the parser, and plain `rdy mcp` keeps its meaning
    let o = s.rdy(&["mcp", "--http-cofig"]);
    assert!(
        err(&o).contains("--http-config"),
        "the parser suggests the flag"
    );
    // the command wrote nothing else on stderr
    let o = s.rdy(&["mcp", "--http-config"]);
    assert!(
        o.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn mht7_http_config_selects_project_without_creating_meta() {
    let w = World::new();
    let outside = short_tempdir();
    let run = |cwd: &Path, args: &[&str]| -> Output {
        let mut cmd = rdy_qa::fixtures::rdy_command();
        cmd.args(args)
            .env_remove("RDY_ACTOR")
            .env_remove("RDY_RUN")
            .env_remove("RDY_NO_DAEMON")
            .envs(w.env())
            .current_dir(cwd);
        for name in w.removed_env() {
            cmd.env_remove(name);
        }
        output_within(&mut cmd, BUDGET).expect("rdy ran")
    };
    // no project and no meta project: the project refusal, and nothing is created
    let v = error_of(&run(outside.path(), &["--json", "mcp", "--http-config"]));
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
    assert!(!w.default_meta().exists());
    assert!(!w.home.path().join(".rdy").exists());
    assert!(!outside.path().join(".rdy").exists());
    // an explicit project that is none gives the same refusal, and nothing is created
    let v = error_of(&run(
        outside.path(),
        &[
            "--json",
            "--project",
            outside.path().to_str().unwrap(),
            "mcp",
            "--http-config",
        ],
    ));
    assert_eq!(v["code"], "E_NO_PROJECT", "{v}");
    assert!(!w.default_meta().exists());
    // an existing meta project is selected, as `rdy mcp` selects it, and serves its own endpoint
    let meta = w.default_meta();
    w.project_at(&meta);
    local(&meta, ON);
    let o = run(outside.path(), &["--json", "mcp", "--http-config"]);
    let v: Value = serde_json::from_str(out(&o).trim()).expect("JSON");
    port_of(v["url"].as_str().expect("the url"));
    assert_eq!(
        v["headers"]["Authorization"],
        format!("Bearer {}", token(&meta))
    );
    w.stop(&meta);
}

#[test]
fn mht7_http_config_refuses_disabled_and_no_daemon_modes() {
    // the setting is off by default: it is never turned on for the person
    let s = Served::new();
    let o = s.rdy(&["--json", "mcp", "--http-config"]);
    let v = error_of(&o);
    assert_eq!(v["code"], "E_SITE", "{v}");
    assert_eq!(v["message"], "MCP over HTTP is disabled for this project");
    assert_eq!(
        v["fix"],
        "set serve.mcp_http to true in user or local config"
    );
    assert!(o.stdout.is_empty());
    let text = err(&s.rdy(&["mcp", "--http-config"]));
    assert_eq!(
        text,
        "error E_SITE: MCP over HTTP is disabled for this project\nfix: set serve.mcp_http to true in user or local config\n"
    );
    // the refusal turned nothing on
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    assert_eq!(status["mcp_http"]["enabled"], false, "{status}");
    assert_eq!(status["mcp_http"]["url"], Value::Null, "{status}");

    // no daemon mode: the project needs its daemon
    let s = on();
    let v = error_of(&s.rdy(&["--json", "--no-daemon", "mcp", "--http-config"]));
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert_eq!(v["message"], "MCP over HTTP needs the project daemon");
    assert_eq!(v["fix"], "drop --no-daemon and run rdy mcp --http-config");
    assert!(
        s.daemon_pid().is_none(),
        "the refusal started no daemon and opened no store"
    );

    // a malformed token fails closed: the file is never replaced
    let s = on();
    out(&s.rdy(&["mcp", "--http-config"]));
    let path = s.dir.path().join(".rdy/run/mcp-token");
    std::fs::write(&path, "short\n").unwrap();
    let v = error_of(&s.rdy(&["--json", "mcp", "--http-config"]));
    assert_eq!(v["code"], "E_SITE", "{v}");
    assert_eq!(
        v["message"],
        "the project's MCP token is not a valid private token file"
    );
    assert_eq!(
        v["fix"],
        "stop rdy, remove .rdy/run/mcp-token, then run rdy mcp --http-config"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"short\n");
}

#[test]
fn mht7_status_and_site_show_endpoint_without_secret() {
    let s = on();
    // the endpoint is read through the command's own output
    let url = {
        let text = out(&s.rdy(&["mcp", "--http-config"]));
        text.lines()
            .next()
            .unwrap()
            .strip_prefix("url: ")
            .unwrap()
            .to_string()
    };
    let token = token(s.dir.path());
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    assert_eq!(
        status["mcp_http"],
        json!({"enabled": true, "url": url, "active_calls": 0, "listen_streams": 0}),
        "{status}"
    );
    let text = out(&s.rdy(&["status"]));
    assert!(
        text.contains(&format!(
            "\nmcp http: {url}\nmcp http activity: 0 calls, 0 streams\n"
        )),
        "{text}"
    );
    // the daemon line stays where it was, and the mcp lines follow it
    let daemon = text
        .lines()
        .position(|l| l.starts_with("daemon: rdy "))
        .unwrap();
    assert_eq!(
        text.lines().nth(daemon + 1),
        Some(format!("mcp http: {url}").as_str())
    );
    // the site names the endpoint beside its own link; the text of `rdy site` stays one link
    let (ok, site) = s.json(&["--json", "site"]);
    assert!(ok, "{site}");
    assert_eq!(site["mcp_url"], url, "{site}");
    let line = out(&s.rdy(&["site"]));
    assert_eq!(line.lines().count(), 1, "{line:?}");
    // none of it holds the token
    for shown in [
        status.to_string(),
        text,
        site.to_string(),
        line,
        String::from_utf8_lossy(&s.rdy(&["status"]).stderr).into_owned(),
    ] {
        assert!(!shown.contains(&token), "the token reached {shown}");
    }
    assert!(status.get("mcp_token").is_none());

    // a project with the setting off: the status says so, and the site names no endpoint
    let off = Served::new();
    let (ok, status) = off.json(&["--json", "status"]);
    assert!(ok, "{status}");
    assert_eq!(
        status["mcp_http"],
        json!({"enabled": false, "url": null, "active_calls": 0, "listen_streams": 0}),
        "{status}"
    );
    let text = out(&off.rdy(&["status"]));
    assert!(text.contains("\nmcp http: disabled\n"), "{text}");
    assert!(!text.contains("mcp http activity"), "{text}");
    let (ok, site) = off.json(&["--json", "site"]);
    assert!(ok, "{site}");
    assert!(site.get("mcp_url").is_none(), "{site}");

    // without a daemon there is no endpoint to name
    let (ok, status) = off.json(&["--json", "--no-daemon", "status"]);
    assert!(ok, "{status}");
    assert!(status.get("mcp_http").is_none(), "{status}");
    let text = out(&off.rdy(&["--no-daemon", "status"]));
    assert!(!text.contains("mcp http"), "{text}");
}

/// Seeds profile `guide` (the folder `manual`) into canon of the project in `dir`.
fn seed_guide(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$intro", "body": "INTRO-ONLY-1"},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let (manual, admin) = (
        w.created["$manual"].to_string(),
        w.created["$admin"].to_string(),
    );
    land(&mut r, "w1").unwrap();
    write(
        &mut r,
        "admin",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$guide", "parent": admin,
             "fields": {"key": "guide", "title": "Guide book", "description": "About the guide.",
                "audience": "org", "select": [manual]}}
        ]),
    )
    .unwrap();
    land(&mut r, "admin").unwrap();
    r.close().unwrap();
}

#[test]
fn mht7_profile_result_never_advertises_profile_mcp() {
    let s = on();
    seed_guide(s.dir.path());
    let text = out(&s.rdy(&["mcp", "--http-config"]));
    let url = text
        .lines()
        .next()
        .unwrap()
        .strip_prefix("url: ")
        .unwrap()
        .to_string();
    let main_port = port_of(&url);
    // a profile of the full site: the endpoint is the full site's
    let (ok, v) = s.json(&["--json", "site", "--profile", "guide"]);
    assert!(ok, "{v}");
    assert_eq!(v["mcp_url"], url, "{v}");
    assert!(v.get("profile_only").is_none(), "{v}");
    // the profile-only site of the running daemon: its own port serves no MCP, and the result
    // names the full endpoint
    let (ok, v) = s.json(&["--json", "serve", "--profile", "guide"]);
    assert!(ok, "{v}");
    assert_eq!(v["profile_only"], true, "{v}");
    assert_eq!(v["mcp_url"], url, "{v}");
    let profile_port: u16 = v["url"]
        .as_str()
        .unwrap()
        .strip_prefix("http://127.0.0.1:")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(profile_port, main_port, "{v}");
    assert!(
        !v.to_string().contains(&format!("{profile_port}/~mcp")),
        "{v}"
    );
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    assert_eq!(status["mcp_http"]["url"], url, "{status}");
    assert!(!status.to_string().contains(&format!("{profile_port}/~mcp")));
    let o = s.rdy(&["serve", "--stop"]);
    assert_eq!(out(&o), "stopped\n");

    // the foreground start prints the same: its profile-only result names the full endpoint
    let mut cmd = human_command(
        s.dir.path(),
        &s.env_pairs(),
        &["--json", "serve", "--profile", "guide"],
    );
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn rdy");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .expect("the result line");
    let v: Value = serde_json::from_str(line.trim()).unwrap_or_else(|_| panic!("JSON in {line}"));
    assert_eq!(v["profile_only"], true, "{v}");
    let foreground = v["mcp_url"].as_str().unwrap_or_else(|| panic!("{v}"));
    port_of(foreground);
    assert_ne!(
        port_of(foreground),
        profile_port_of(&v),
        "the foreground result names the full endpoint: {v}"
    );
    let stop = s.rdy(&["serve", "--stop"]);
    assert_eq!(out(&stop), "stopped\n");
    child.wait().expect("the foreground daemon exits");
}

fn profile_port_of(v: &Value) -> u16 {
    v["url"]
        .as_str()
        .and_then(|u| u.strip_prefix("http://127.0.0.1:"))
        .and_then(|r| r.split('/').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no profile link in {v}"))
}

#[test]
fn mht7_http_config_prints_the_bound_port_under_the_fallback() {
    // a project whose derived candidate ports this test holds: the daemon's site takes an
    // OS-assigned port (D-5GFFX), and the command prints that port, not a derived one. A stable
    // URL needs serve.site_port (D-QG54Z).
    let (s, _held) = loop {
        let s = on();
        let root = s.dir.path().canonicalize().unwrap();
        let held: Result<Vec<_>, _> = addr::candidates(None, &root)
            .iter()
            .map(|p| TcpListener::bind((Ipv4Addr::LOCALHOST, *p)))
            .collect();
        if let Ok(held) = held {
            break (s, held);
        }
    };
    let root = s.dir.path().canonicalize().unwrap();
    let candidates = addr::candidates(None, &root);
    let text = out(&s.rdy(&["mcp", "--http-config"]));
    let url = text.lines().next().unwrap().strip_prefix("url: ").unwrap();
    let port = port_of(url);
    assert!(!candidates.contains(&port), "{port} is OS-assigned");
    // the printed port is the one bound: the site's own link and the status say the same
    let site = out(&s.rdy(&["site"]));
    assert_eq!(site, format!("http://127.0.0.1:{port}/\n"));
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    assert_eq!(status["mcp_http"]["url"], url, "{status}");
    // a second run prints the same endpoint while the daemon lives
    assert_eq!(out(&s.rdy(&["mcp", "--http-config"])), text);
}
