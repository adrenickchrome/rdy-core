//! spv10 (D-CWC91, D-DK03K, D-KQ4CA): the CLI's publication profile links. `rdy site --profile`
//! prints what the ctl op `site_url` returns for the same profile; `rdy serve --profile` reuses
//! a running daemon (and exits 0) or serves in the foreground, printing the profile-only site's
//! link once; `rdy publication check` reads review scopes as candidates only. Every wait is on
//! an event (a line, an exit, a lock or a held callback), never on elapsed time.
use rdy_daemon::site::LanOverride;
use rdy_daemon::{
    ExitReason, Hello, HelloReply, PROTO_VERSION, ProfileSiteReady, Proto, RunDir, ServeOptions,
    serve_with_profile,
};
use rdy_engine::Rdy;
use rdy_qa::daemon::{Hosted, Served, connect_ready, human_command, short_tempdir};
use rdy_qa::fixtures::{human_env, land, write, writer};
use rdy_qa::http;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// This build's version: a ctl client of the binary's daemon says it, or the daemon exits.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The deadline of every wait for a line or an exit (a hang guard, never asserted).
const T: Duration = Duration::from_secs(60);
/// The reference warning line of `guide`'s build: `notes` refers to `secret`.
const GUIDE_REFS: &str = "warning: Org profile has 1 references outside its selection. Review these references before you share the output.";
/// The caveat line of the profile-only site of `Guide book`.
const GUIDE_CAVEAT: &str = "warning: this site shows only the profile \"Guide book\"; anyone with the project still reads everything give the full site's link from rdy site when the person needs everything";

// --- the world ----------------------------------------------------------------------------------

/// Seeds the closed project in `dir`. Canon holds a folder `manual` (doc `intro`, prose
/// `INTRO-ONLY-1`; doc `notes`, which refers to `secret`), a top-level `secret` doc
/// (`SECRET-WORDS`) and an `admin` doc holding the org profile `guide` (`Guide book`, the
/// folder). View `draft` adds the public profile `preview` (`Preview book`, the folder), and
/// its proposal is open: `preview` exists in no landing. Returns the ids by `$name` and the
/// proposal's `P-x@n`.
fn seed(dir: &Path) -> (BTreeMap<String, String>, String) {
    let mut r = Rdy::open(dir).unwrap();
    let w = write(
        &mut r,
        "w1",
        json!([
            {"op": "create", "type": "folder", "id": "$manual", "fields": {"name": "manual", "title": "Manual", "description": "Manual folder."}},
            {"op": "create", "type": "doc", "id": "$intro", "parent": "$manual", "fields": {"name": "intro", "title": "Intro page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$intro", "body": "INTRO-ONLY-1"},
            {"op": "create", "type": "doc", "id": "$secret", "fields": {"name": "secret", "title": "Secret page", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$secret", "body": "SECRET-WORDS"},
            {"op": "create", "type": "doc", "id": "$notes", "parent": "$manual", "fields": {"name": "notes", "title": "Notes page", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin page", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    let mut ids: BTreeMap<String, String> = w
        .created
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect();
    land(&mut r, "w1").unwrap();
    let w = write(
        &mut r,
        "admin",
        json!([
            {"op": "create", "type": "prose", "parent": ids["$notes"], "body": format!("See {{{{{}}}}}.", ids["$secret"])},
            {"op": "create", "type": "publication_profile", "id": "$guide", "parent": ids["$admin"],
             "fields": {"key": "guide", "title": "Guide book", "description": "About the guide.",
                "audience": "org", "select": [ids["$manual"]]}}
        ]),
    )
    .unwrap();
    ids.extend(w.created.iter().map(|(k, v)| (k.clone(), v.to_string())));
    land(&mut r, "admin").unwrap();
    let w = write(
        &mut r,
        "draft",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$preview", "parent": ids["$admin"],
             "fields": {"key": "preview", "title": "Preview book", "description": "About the preview.",
                "audience": "public", "select": [ids["$manual"]]}}
        ]),
    )
    .unwrap();
    ids.extend(w.created.iter().map(|(k, v)| (k.clone(), v.to_string())));
    let prop = r.propose(&writer(), "draft", "Draft", None).unwrap().r#ref;
    r.close().unwrap();
    (ids, prop)
}

/// A seeded `Served` project with no daemon running yet.
fn served() -> (Served, BTreeMap<String, String>, String) {
    let s = Served::new();
    let (ids, prop) = seed(s.dir.path());
    (s, ids, prop)
}

/// A fresh short project directory, seeded, with `local` written to `.rdy/local.yaml`.
fn seeded_dir(local: Option<&str>) -> (tempfile::TempDir, BTreeMap<String, String>, String) {
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let (ids, prop) = seed(dir.path());
    if let Some(text) = local {
        std::fs::write(dir.path().join(".rdy/local.yaml"), text).unwrap();
    }
    (dir, ids, prop)
}

/// The hello of a ctl client of this build at `dir`.
fn hello(dir: &Path) -> Hello {
    Hello {
        rdy: PROTO_VERSION,
        version: VERSION.into(),
        proto: Proto::Ctl,
        cwd: dir.into(),
        project: RunDir::of_project(dir).project_id().unwrap(),
    }
}

/// The ctl op `op` on a session of its own, as the human, through the daemon at `dir`.
fn ctl(dir: &Path, op: &str, args: Value) -> Result<Value, rdy_daemon::ErrorReply> {
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut c = connect_ready(&RunDir::of_project(dir), &hello(dir)).await;
        c.ctl("session", json!({"actor": null, "run": null}))
            .await
            .unwrap();
        c.ctl(op, args).await
    })
}

fn ok_ctl(dir: &Path, op: &str, args: Value) -> Value {
    ctl(dir, op, args).unwrap_or_else(|e| panic!("{op}: {e}"))
}

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Stdout whatever the exit.
fn raw_stdout(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The text `rdy serve --profile` prints for a profile-only `site_url` reply.
fn serve_text(v: &Value) -> String {
    let mut t = format!(
        "profile site {}: {}\n",
        v["profile"]["key"].as_str().unwrap(),
        v["url"].as_str().unwrap()
    );
    if let Some(lan) = v["lan_url"].as_str() {
        t.push_str(&format!("{lan}\n"));
    }
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!("warning: {}\n", w.as_str().unwrap()));
    }
    t
}

/// The text `rdy site` prints for a `site_url` reply.
fn site_text(v: &Value) -> String {
    let mut t = format!("{}\n", v["url"].as_str().unwrap());
    if let Some(c) = v["changes"].as_str() {
        t.push_str(&format!("changes: {c}\n"));
    }
    if let Some(lan) = v["lan_url"].as_str() {
        t.push_str(&format!("{lan}\n"));
    }
    for w in v["warnings"].as_array().into_iter().flatten() {
        t.push_str(&format!("warning: {}\n", w.as_str().unwrap()));
    }
    t
}

/// The two stderr lines of a structured LAN warning.
fn lan_lines(v: &Value) -> String {
    format!(
        "warning: {}\nfix: {}\n",
        v["warning"]["message"].as_str().unwrap(),
        v["warning"]["fix"].as_str().unwrap()
    )
}

/// The port of `http://127.0.0.1:<port>/...`.
fn port_of(url: &str) -> u16 {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|r| r.split('/').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("not a loopback link: {url}"))
}

fn closed(url: &str) {
    let port = port_of(url);
    assert!(
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err(),
        "port {port} still accepts"
    );
}

/// Help text with every run of whitespace made one space (clap wraps it).
fn help(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(rdy_qa::fixtures::rdy_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// --- a child that serves ---------------------------------------------------------------------------

/// A spawned `rdy` with its stdout and stderr read line by line on threads of their own (so a
/// full pipe never stalls it), and its exit sent on a channel.
struct Proc {
    pid: u32,
    out: Receiver<String>,
    err: Receiver<String>,
    /// Every stderr line so far, for messages.
    err_log: Arc<Mutex<Vec<String>>>,
    exit: Receiver<ExitStatus>,
}

fn lines_of(
    r: impl Read + Send + 'static,
    log: Option<Arc<Mutex<Vec<String>>>>,
) -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(r).lines() {
            let Ok(line) = line else { break };
            if let Some(l) = &log {
                l.lock().unwrap().push(line.clone());
            }
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

impl Proc {
    fn spawn(mut cmd: Command) -> Proc {
        let mut child: Child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn rdy");
        let pid = child.id();
        let out = lines_of(child.stdout.take().unwrap(), None);
        let err_log = Arc::new(Mutex::new(Vec::new()));
        let err = lines_of(child.stderr.take().unwrap(), Some(Arc::clone(&err_log)));
        let (tx, exit) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait().expect("wait for rdy"));
        });
        Proc {
            pid,
            out,
            err,
            err_log,
            exit,
        }
    }

    fn stderr_now(&self) -> String {
        self.err_log.lock().unwrap().join("\n")
    }

    /// The next stdout line; panics past `T` or at the end of stdout.
    fn line(&self) -> String {
        self.out
            .recv_timeout(T)
            .unwrap_or_else(|e| panic!("no stdout line ({e}); stderr: {}", self.stderr_now()))
    }

    /// The two warning lines after `guide`'s result line: the build's, then the caveat.
    fn tail(&self) {
        assert_eq!(self.line(), GUIDE_REFS);
        assert_eq!(self.line(), GUIDE_CAVEAT);
    }

    /// Waits for a stderr line containing `needle`.
    fn wait_err(&self, needle: &str) {
        loop {
            let l = self.err.recv_timeout(T).unwrap_or_else(|e| {
                panic!(
                    "no stderr line with {needle:?} ({e}): {}",
                    self.stderr_now()
                )
            });
            if l.contains(needle) {
                return;
            }
        }
    }

    fn running(&self) -> bool {
        matches!(
            self.exit.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        )
    }

    /// Waits for the exit, then for the end of stdout: the exit and the rest of stdout.
    fn finish(self) -> (ExitStatus, Vec<String>, String) {
        let status = self
            .exit
            .recv_timeout(T)
            .unwrap_or_else(|e| panic!("rdy did not exit ({e}): {}", self.stderr_now()));
        let rest: Vec<String> = self.out.iter().collect();
        // stderr ends with the process too
        let _: Vec<String> = self.err.iter().collect();
        let err = self.err_log.lock().unwrap().join("\n");
        (status, rest, err)
    }
}

// --- the pins ---------------------------------------------------------------------------------------

/// On one hosted fixture, `rdy site --profile` and the ctl op give one reply: the URL, the
/// profile facts and the warnings, for a key, an id, the implicit profile and a profile that
/// only an open proposal holds. `rdy serve --profile --json` gives the profile-only reply.
#[test]
fn spv10_cli_profile_links_match_ctl() {
    let (dir, ids, prop) = seeded_dir(Some("serve:\n  site_lan: true\n"));
    let (_cfg, env) = human_env("human:qa");
    let h = Hosted::start_for_binary(dir, Some(LanOverride::Addr(Ipv4Addr::LOCALHOST)));
    let p = h.dir.path();
    let proposal = format!("proposal:{prop}");
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (vec!["--profile", "guide"], json!({"profile": "guide"})),
        (
            vec!["--profile", &ids["$guide"]],
            json!({"profile": ids["$guide"]}),
        ),
        (
            vec!["--profile", "implicit:org"],
            json!({"profile": "implicit:org"}),
        ),
        (
            vec!["--profile", "preview", "--scope", &proposal],
            json!({"profile": "preview", "scope": proposal}),
        ),
        (
            vec!["--profile", "guide", "--scope", "view:draft"],
            json!({"profile": "guide", "scope": "view:draft"}),
        ),
    ];
    for (flags, args) in cases {
        let mut cli = vec!["--json", "site"];
        cli.extend(&flags);
        let out = human_command(p, &env, &cli).output().unwrap();
        let v: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        let want = ok_ctl(p, "site_url", args.clone());
        assert_eq!(v, want, "{flags:?}");
        assert!(v["profile"]["key"].is_string(), "{v}");
        assert_eq!(stdout(&out).lines().count(), 1, "one JSON line: {flags:?}");
        // the structured LAN warning keeps its two stderr lines in JSON mode
        assert_eq!(stderr(&out), lan_lines(&v), "{flags:?}");
        // the text keeps the order url, changes, LAN url, warnings
        let mut cli = vec!["site"];
        cli.extend(&flags);
        let out = human_command(p, &env, &cli).output().unwrap();
        assert_eq!(stdout(&out), site_text(&want), "{flags:?}");
        assert_eq!(stderr(&out), lan_lines(&want), "{flags:?}");
    }
    // the proposal-only profile is its proposal's: canon holds no such key
    let e = ctl(p, "site_url", json!({"profile": "preview"})).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    let out = human_command(p, &env, &["site", "--profile", "preview"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).starts_with("error E_NOT_FOUND: "),
        "{}",
        stderr(&out)
    );

    // the profile-only site: `rdy serve --profile` reuses this daemon and prints its reply
    for name in ["guide", ids["$guide"].as_str()] {
        let out = human_command(p, &env, &["--json", "serve", "--profile", name])
            .output()
            .unwrap();
        let v: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        let want = ok_ctl(p, "site_url", json!({"profile": "guide", "only": true}));
        assert_eq!(v, want, "{name}");
        assert_eq!(v["profile_only"], true, "{v}");
        assert_eq!(v["profile"]["key"], "guide", "the resolved key: {v}");
        assert_eq!(stdout(&out).lines().count(), 1);
        assert_eq!(stderr(&out), lan_lines(&v), "{name}");
    }
}

/// A running daemon is reused: the same process and store owner, the same URL on every call,
/// exit 0, the exact line and caveat; the sites stay readable after the CLI exits.
#[test]
fn spv10_serve_profile_reuses_a_running_daemon() {
    let (s, ids, _) = served();
    let out = s.rdy(&["status"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let pid = s.daemon_pid().expect("the daemon runs");
    let want = ok_ctl(
        s.dir.path(),
        "site_url",
        json!({"profile": "guide", "only": true}),
    );
    let text = serve_text(&want);
    assert!(
        text.starts_with("profile site guide: http://127.0.0.1:"),
        "{text}"
    );
    assert!(text.ends_with(&format!("{GUIDE_CAVEAT}\n")), "{text}");
    for name in ["guide", ids["$guide"].as_str(), "guide"] {
        let out = s.rdy(&["serve", "--profile", name]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert_eq!(stdout(&out), text, "{name}");
        assert!(
            !stderr(&out).contains("rdy serve:"),
            "no daemon started in the command: {}",
            stderr(&out)
        );
        assert_eq!(s.daemon_pid(), Some(pid), "the same daemon owns the store");
    }
    // after the CLI has exited, both sites answer from the same daemon
    let url = want["url"].as_str().unwrap();
    let page = http::get(&format!("{url}manual/intro.html"));
    assert_eq!(page.status, 200);
    assert!(String::from_utf8_lossy(&page.body).contains("INTRO-ONLY-1"));
    assert_eq!(http::get(&format!("{url}secret.html")).status, 404);
    let source = stdout(&s.rdy(&["site"]));
    let source = source.trim_end();
    assert_ne!(source, url);
    assert_eq!(http::get(source).status, 200);
    let secret = http::get(&format!("{source}secret.html"));
    assert_eq!(secret.status, 200);
    assert_eq!(s.daemon_pid(), Some(pid));
}

/// With no daemon, `rdy serve --profile` serves in the foreground: it prints its result once
/// readiness is reached, stays alive until the stop, then exits 0 with both ports closed.
#[test]
fn spv10_serve_profile_starts_in_the_foreground() {
    let (s, _, _) = served();
    assert!(s.daemon_pid().is_none());
    let mut cmd = human_command(
        s.dir.path(),
        &s.env_pairs(),
        &["serve", "--profile", "guide"],
    );
    cmd.stdin(Stdio::null());
    let child = Proc::spawn(cmd);
    let first = child.line();
    let url = first
        .strip_prefix("profile site guide: ")
        .unwrap_or_else(|| panic!("{first}"))
        .to_string();
    child.tail();
    // the result is printed after readiness: a reused call gets the same reply at once
    let want = ok_ctl(
        s.dir.path(),
        "site_url",
        json!({"profile": "guide", "only": true}),
    );
    assert_eq!(want["url"], url.as_str());
    assert_eq!(
        format!("{first}\n{GUIDE_REFS}\n{GUIDE_CAVEAT}\n"),
        serve_text(&want)
    );
    assert!(
        child.running(),
        "the foreground daemon serves until stopped"
    );
    assert_eq!(s.daemon_pid(), Some(child.pid), "the command is the daemon");
    let page = http::get(&format!("{url}manual/intro.html"));
    assert_eq!(page.status, 200);
    assert!(String::from_utf8_lossy(&page.body).contains("INTRO-ONLY-1"));
    let source = stdout(&s.rdy(&["site"])).trim_end().to_string();
    assert_eq!(http::get(&source).status, 200);
    // a second call reuses the foreground daemon
    let again = s.rdy(&["serve", "--profile", "guide"]);
    assert_eq!(stdout(&again), serve_text(&want));
    assert!(child.running());
    // the daemon's logs stay on stderr, apart from the result lines
    child.wait_err(&format!("rdy serve: site: profile guide: {url}"));

    let stop = s.rdy(&["serve", "--stop"]);
    assert_eq!(stdout(&stop), "stopped\n");
    let (status, rest, err) = child.finish();
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(rest.is_empty(), "no other stdout line: {rest:?}");
    assert!(!err.contains("profile site"), "{err}");
    closed(&url);
    closed(&source);
    assert!(s.daemon_pid().is_none());
}

/// A store held by a process that is not a daemon: exit 2, no listener, no daemon.
#[test]
fn spv10_serve_profile_non_daemon_holder_exits_two() {
    let (s, _, _) = served();
    // the handshake: the store is held once the open returns
    let holder = Rdy::open(s.dir.path()).unwrap();
    for args in [
        vec!["serve", "--profile", "guide"],
        vec!["--json", "serve", "--profile", "guide"],
    ] {
        let out = s.rdy(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert_eq!(raw_stdout(&out), "", "no result line");
        assert!(
            stderr(&out).contains("already running: the store at "),
            "{}",
            stderr(&out)
        );
        assert!(!stderr(&out).contains("site: profile"), "{}", stderr(&out));
        assert!(s.daemon_pid().is_none(), "no daemon");
        assert!(!s.run_dir().socket().exists(), "no socket");
    }
    drop(holder);
}

/// A fake daemon at `dir`'s socket that answers every hello `exiting`, until the returned
/// runtime is dropped. The count is the hellos it answered.
fn always_exiting(dir: &Path) -> (tokio::runtime::Runtime, Arc<AtomicUsize>) {
    let run = RunDir::of_project(dir);
    run.ensure().unwrap();
    let rt = Hosted::rt();
    let listener = {
        let _g = rt.enter();
        rdy_daemon::sys::bind(&run.socket()).unwrap()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&count);
    rt.spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        while let Ok((stream, _)) = listener.accept().await {
            let n = Arc::clone(&n);
            tokio::spawn(async move {
                let mut stream = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                if stream.read_line(&mut line).await.is_err() {
                    return;
                }
                let reply = HelloReply::Exiting {
                    version: "0.0.0-old".into(),
                };
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(b'\n');
                let _ = stream.get_mut().write_all(&bytes).await;
                n.fetch_add(1, Ordering::SeqCst);
            });
        }
    });
    (rt, count)
}

/// A scripted ctl peer at a project's socket: it answers the first hello `starting`, says so,
/// and answers `ready` only once released; then it answers `session` and `site_url` (with
/// `site`) and records every request. Alive until `rt` is dropped. The held `ready` waits on an
/// awaited oneshot, never a blocking task: a failed assertion before the release drops the
/// runtime, which cancels the wait instead of hanging on it and hiding the failure.
struct StartingPeer {
    rt: tokio::runtime::Runtime,
    /// The hello the client sent.
    hello: Receiver<Value>,
    /// Sent once the `starting` reply is written, before any `ready` exists.
    starting_sent: Receiver<()>,
    /// Releases the `ready` reply.
    release: tokio::sync::oneshot::Sender<()>,
    /// The client's ctl requests, in order.
    requests: Receiver<Value>,
    /// The connections the peer accepted.
    accepts: Arc<AtomicUsize>,
}

/// One newline-ended JSON line.
fn json_line(v: &impl serde::Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(v).unwrap();
    bytes.push(b'\n');
    bytes
}

fn starting_peer(dir: &Path, site: Value) -> StartingPeer {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let run = RunDir::of_project(dir);
    run.ensure().unwrap();
    let rt = Hosted::rt();
    let listener = {
        let _g = rt.enter();
        rdy_daemon::sys::bind(&run.socket()).unwrap()
    };
    let (hello_tx, hello) = channel();
    let (sent_tx, starting_sent) = channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let (req_tx, requests) = channel();
    let accepts = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&accepts);
    rt.spawn(async move {
        let mut script = Some((hello_tx, sent_tx, released, req_tx, site));
        while let Ok((stream, _)) = listener.accept().await {
            n.fetch_add(1, Ordering::SeqCst);
            // a later connection is counted and closed: the client must not need one
            let Some((hello_tx, sent_tx, released, req_tx, site)) = script.take() else {
                continue;
            };
            tokio::spawn(async move {
                let mut stream = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                hello_tx
                    .send(serde_json::from_str::<Value>(&line).unwrap())
                    .unwrap();
                let starting = HelloReply::Starting {
                    progress: "opening store".into(),
                };
                let w = stream.get_mut();
                w.write_all(&json_line(&starting)).await.unwrap();
                w.flush().await.unwrap();
                sent_tx.send(()).unwrap();
                // a release dropped unsent (a failed test) ends the script
                if released.await.is_err() {
                    return;
                }
                let ready = HelloReply::Ready {
                    version: VERSION.into(),
                    session: rdy_daemon::SessionId(7),
                };
                let w = stream.get_mut();
                w.write_all(&json_line(&ready)).await.unwrap();
                w.flush().await.unwrap();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let ok = match req["op"].as_str() {
                        Some("session") => json!({"session": 7}),
                        Some("site_url") => site.clone(),
                        other => panic!("an unexpected op {other:?}"),
                    };
                    let reply = json_line(&json!({"id": req["id"], "ok": ok}));
                    req_tx.send(req).unwrap();
                    let w = stream.get_mut();
                    w.write_all(&reply).await.unwrap();
                    w.flush().await.unwrap();
                }
            });
        }
    });
    StartingPeer {
        rt,
        hello,
        starting_sent,
        release,
        requests,
        accepts,
    }
}

/// Final review M2: a scripted peer dropped while it holds `ready` (a test that fails before its
/// release) tears down at once: its wait is cancelled with the runtime, and never holds the
/// teardown that would report the failure.
#[test]
fn spv10_starting_peer_teardown_never_waits_on_its_release() {
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let peer = starting_peer(dir.path(), json!({}));
    let run = RunDir::of_project(dir.path());
    let client_rt = Hosted::rt();
    let attempt = client_rt.block_on(rdy_daemon::client::connect(&run, &hello(dir.path())));
    assert!(
        matches!(attempt, rdy_daemon::client::Attempt::Starting(_)),
        "the peer answers starting"
    );
    peer.hello.recv_timeout(T).expect("the client's hello");
    peer.starting_sent
        .recv_timeout(T)
        .expect("the starting reply is written");
    // drop the peer as an unwinding test would: field by field, the release still unsent
    let (done_tx, done) = channel();
    std::thread::spawn(move || {
        drop(peer);
        done_tx.send(()).unwrap();
    });
    done.recv_timeout(T)
        .expect("the peer's teardown waits on nothing");
    drop(attempt);
}

/// The start races, each behind a barrier: a daemon still starting (a scripted peer that holds
/// `ready`, and a held foreground callback), a concurrent foreground winner, and a daemon
/// exiting to be replaced. Configured timeouts give the existing `E_DAEMON_START` texts.
#[test]
fn spv10_serve_profile_handles_start_and_exit_races() {
    let (_cfg, env) = human_env("human:qa");

    // 1. Exiting, then no daemon: a daemon of another version exits to be replaced, and the
    // command becomes the foreground daemon itself (never a detached one)
    let (dir, _, _) = seeded_dir(None);
    let mut h = Hosted::start_in(dir);
    let p = h.dir.path().to_path_buf();
    let mut cmd = human_command(&p, &env, &["serve", "--profile", "guide"]);
    cmd.stdin(Stdio::null());
    let child = Proc::spawn(cmd);
    // past its connects: the old daemon answered `exiting`, and the command opens the store
    child.wait_err("rdy serve: opening store");
    // the old daemon (this process) releases the store once it is joined
    assert_eq!(
        h.stop(),
        ExitReason::VersionReplaced,
        "the old daemon answered exiting"
    );
    let first = child.line();
    assert!(
        first.starts_with("profile site guide: http://127.0.0.1:"),
        "{first}"
    );
    child.tail();
    let pid = std::fs::read_to_string(RunDir::of_project(&p).pid_file()).unwrap();
    assert_eq!(pid.trim(), child.pid.to_string(), "the command serves");
    let stop = human_command(&p, &env, &["serve", "--stop"])
        .output()
        .unwrap();
    assert_eq!(stdout(&stop), "stopped\n");
    let (status, rest, err) = child.finish();
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(rest.is_empty(), "{rest:?}");
    drop(h);

    // 2. Exiting until the deadline: the configured start timeout, in not_ready's words, and no
    // start of any kind
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/local.yaml"),
        "serve:\n  start_timeout: 1\n",
    )
    .unwrap();
    let (fake, count) = always_exiting(dir.path());
    let out = human_command(dir.path(), &env, &["serve", "--profile", "guide"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out)
            .starts_with("error E_DAEMON_START: the daemon did not become ready within 1 s\n"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("rdy serve:"), "no foreground start");
    assert!(count.load(Ordering::SeqCst) >= 2, "the hello was retried");
    assert!(!RunDir::of_project(dir.path()).pid_file().exists());
    drop(fake);

    // 3a. Starting, waited for: a scripted peer answers the hello `starting` and holds `ready`
    // until the test has seen that reply written. The client cannot have read `ready` from its
    // first reply, so the line it prints comes through `Pending::wait`, then the session and
    // `site_url` of that same connection
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let site = json!({
        "url": "http://127.0.0.1:1/scripted/",
        "lan_url": null,
        "warning": null,
        "profile_only": true,
        "profile": {"key": "guide", "title": "Guide book"},
        "warnings": ["scripted warning"]
    });
    let peer = starting_peer(dir.path(), site.clone());
    let mut cmd = human_command(dir.path(), &env, &["serve", "--profile", "guide"]);
    cmd.stdin(Stdio::null());
    let client = Proc::spawn(cmd);
    let h: Value = peer.hello.recv_timeout(T).expect("the client's hello");
    assert_eq!(h["proto"], "ctl", "{h}");
    assert_eq!(h["version"], VERSION, "{h}");
    peer.starting_sent
        .recv_timeout(T)
        .expect("the starting reply is written");
    assert!(client.running(), "no ready yet: the client waits");
    peer.release.send(()).unwrap();
    assert_eq!(
        client.line(),
        "profile site guide: http://127.0.0.1:1/scripted/"
    );
    assert_eq!(client.line(), "warning: scripted warning");
    let (status, rest, err) = client.finish();
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(rest.is_empty(), "{rest:?}");
    assert!(!err.contains("rdy serve:"), "no start of its own: {err}");
    let session = peer.requests.recv_timeout(T).expect("the session request");
    assert_eq!(session["op"], "session", "{session}");
    assert!(
        session["args"].get("actor").is_some() && session["args"].get("run").is_some(),
        "the CLI caller's classification: {session}"
    );
    let url = peer.requests.recv_timeout(T).expect("the site_url request");
    assert_eq!(url["op"], "site_url", "{url}");
    assert_eq!(url["args"], json!({"profile": "guide", "only": true}));
    assert!(peer.requests.try_recv().is_err(), "no other request");
    assert_eq!(peer.accepts.load(Ordering::SeqCst), 1, "one connection");
    assert!(!RunDir::of_project(dir.path()).pid_file().exists());
    drop(peer.rt);

    // 3b. Starting, past the deadline: a foreground profile start in this process holds its
    // callback, so its daemon answers `starting` until released
    let (dir, _, _) = seeded_dir(None);
    let p = dir.path().to_path_buf();
    let (entered_tx, entered) = channel::<String>();
    let (release, released) = channel::<()>();
    let opts = ServeOptions {
        project: p.clone(),
        version: VERSION.into(),
        detach: false,
        idle_timeout: Some(Duration::from_secs(60)),
        human_actor: Some("human:qa".into()),
        site_lan_override: None,
    };
    let host = std::thread::spawn(move || {
        serve_with_profile(opts, "guide", move |ready: ProfileSiteReady| {
            entered_tx.send(ready.info.url.clone().unwrap()).unwrap();
            released.recv().unwrap();
            Ok(())
        })
    });
    entered.recv_timeout(T).expect("the callback runs");
    // a person whose user config sets a one-second start timeout meets the held start and gives
    // up
    let (short_cfg, short_env) = human_env("human:qa");
    std::fs::write(
        short_cfg.path().join("rdy/config.yaml"),
        "actor: human:qa\nserve:\n  start_timeout: 1\n",
    )
    .unwrap();
    let out = human_command(&p, &short_env, &["serve", "--profile", "guide"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with(
            "error E_DAEMON_START: the daemon did not become ready before the deadline"
        ),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("rdy serve:"), "no start of its own");
    release.send(()).unwrap();
    // this process is the daemon: stop it over its socket (`rdy serve --stop` would wait for
    // this process to exit)
    let run = RunDir::of_project(&p);
    let stopped = Hosted::rt()
        .block_on(rdy_daemon::request_stop(&run, &hello(&p), T))
        .unwrap();
    assert!(stopped.is_some());
    let ran = host.join().unwrap();
    assert!(ran.is_ok(), "{ran:?}");

    // 4. A concurrent winner: two commands meet a held store (both past their first connect),
    // the store is released, one serves and the other reuses it once it is AlreadyRunning
    let (dir, _, _) = seeded_dir(None);
    let p = dir.path().to_path_buf();
    let holder = Rdy::open(&p).unwrap();
    let spawn = || {
        let mut cmd = human_command(&p, &env, &["serve", "--profile", "guide"]);
        cmd.stdin(Stdio::null());
        Proc::spawn(cmd)
    };
    let a = spawn();
    let b = spawn();
    a.wait_err("rdy serve: opening store");
    b.wait_err("rdy serve: opening store");
    drop(holder);
    let (la, lb) = (a.line(), b.line());
    assert_eq!(la, lb, "one profile site");
    assert!(
        la.starts_with("profile site guide: http://127.0.0.1:"),
        "{la}"
    );
    a.tail();
    b.tail();
    // the loser exits 0 after reusing the winner; the winner serves until the stop
    let pid: u32 = std::fs::read_to_string(RunDir::of_project(&p).pid_file())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let (winner, loser) = if pid == a.pid { (a, b) } else { (b, a) };
    assert_eq!(winner.pid, pid);
    let (status, rest, err) = loser.finish();
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(rest.is_empty());
    assert!(err.contains("already running: the store at "), "{err}");
    assert!(winner.running());
    let stop = human_command(&p, &env, &["serve", "--stop"])
        .output()
        .unwrap();
    assert_eq!(stdout(&stop), "stopped\n");
    let (status, _, err) = winner.finish();
    assert_eq!(status.code(), Some(0), "{err}");
}

/// An unknown key: exit 1 with `E_NOT_FOUND` and no profile port. A start of its own is undone
/// (no process, socket or lock left); an already-running daemon is left running.
#[test]
fn spv10_serve_profile_unknown_key_cleans_up_its_own_start() {
    let (s, _, _) = served();
    // the foreground start validates its profile in canon before any port
    for name in ["nope", "preview"] {
        let out = s.rdy(&["serve", "--profile", name]);
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        assert_eq!(raw_stdout(&out), "");
        assert!(
            stderr(&out).contains(&format!(
                "could not open the store: E_NOT_FOUND: Publication profile \"{name}\" was not found."
            )),
            "{}",
            stderr(&out)
        );
        assert!(!stderr(&out).contains("site: profile"), "{}", stderr(&out));
        assert!(s.daemon_pid().is_none());
        assert!(!s.run_dir().socket().exists());
        // the store is free again
        drop(Rdy::open(s.dir.path()).unwrap());
    }
    // a running daemon answers the same refusal and keeps running
    let out = s.rdy(&["status"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let pid = s.daemon_pid().unwrap();
    for json_mode in [false, true] {
        let mut args = vec!["serve", "--profile", "nope"];
        if json_mode {
            args.insert(0, "--json");
        }
        let out = s.rdy(&args);
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        assert_eq!(raw_stdout(&out), "");
        let err = stderr(&out);
        if json_mode {
            let e: Value = serde_json::from_str(err.trim()).unwrap();
            assert_eq!(e["code"], "E_NOT_FOUND", "{e}");
        } else {
            assert!(
                err.starts_with(
                    "error E_NOT_FOUND: Publication profile \"nope\" was not found.\nfix: "
                ),
                "{err}"
            );
        }
        assert_eq!(s.daemon_pid(), Some(pid), "the daemon is left running");
    }
    let source = stdout(&s.rdy(&["site"])).trim_end().to_string();
    assert_eq!(http::get(&source).status, 200);
}

/// Runs `cmd` with no stdin, stdout read in full, and a stderr whose reader closed before the
/// spawn, so each of the child's stderr writes fails. Its exit and stdout.
fn with_closed_stderr(mut cmd: Command) -> (ExitStatus, String) {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(writer))
        .spawn()
        .expect("spawn rdy");
    // drop the parent's copy of the pipe's writer with `cmd`
    drop(cmd);
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output().expect("wait for rdy"));
    });
    let out = rx.recv_timeout(T).expect("rdy exits");
    (out.status, raw_stdout(&out))
}

/// Fix round 1: with a stderr whose reader has closed, the structured LAN warning cannot be
/// written. A foreground start fails through its callback with an error, not a panic: exit 1,
/// no result line, and the daemon it created is stopped (no pid file, socket or lock left). A
/// reuse and `rdy site` exit 1 the same way and leave the running daemon serving.
#[test]
fn spv10_serve_profile_closed_stderr_fails_and_cleans_up() {
    // an address this machine does not hold: the LAN bind fails, so every listener has its
    // structured warning
    let lan = "serve:\n  site_lan: true\n  site_lan_address: 10.255.255.254\n";
    let (dir, _, _) = seeded_dir(Some(lan));
    let (_cfg, env) = human_env("human:qa");
    let p = dir.path().to_path_buf();
    let run = RunDir::of_project(&p);
    // the control: with a stderr that is read, this start serves and writes its warning there
    let mut cmd = human_command(&p, &env, &["serve", "--profile", "guide"]);
    cmd.stdin(Stdio::null());
    let child = Proc::spawn(cmd);
    let first = child.line();
    assert!(
        first.starts_with("profile site guide: http://127.0.0.1:"),
        "{first}"
    );
    child.tail();
    let stop = human_command(&p, &env, &["serve", "--stop"])
        .output()
        .unwrap();
    assert_eq!(stdout(&stop), "stopped\n");
    let (status, _, err) = child.finish();
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(
        err.lines().any(|l| l
            .starts_with("warning: profile \"Guide book\": could not listen on 10.255.255.254 (")),
        "the profile listener's LAN warning: {err}"
    );
    for args in [
        vec!["serve", "--profile", "guide"],
        vec!["--json", "serve", "--profile", "guide"],
    ] {
        let (status, out) = with_closed_stderr(human_command(&p, &env, &args));
        assert_eq!(status.code(), Some(1), "{args:?}: an error, not a panic");
        assert_eq!(out, "", "{args:?}: no result line");
        assert!(!run.pid_file().exists(), "{args:?}: no daemon");
        assert!(!run.socket().exists(), "{args:?}: no socket");
        // the store is free again
        drop(Rdy::open(&p).unwrap());
    }

    // a running daemon: the reuse and `rdy site` fail on the same write and leave it serving
    let h = Hosted::start_for_binary(dir, None);
    let p = h.dir.path();
    let want = ok_ctl(p, "site_url", json!({"profile": "guide", "only": true}));
    assert!(want["warning"].is_object(), "{want}");
    assert!(
        ok_ctl(p, "site_url", json!({"profile": "guide"}))["warning"].is_object(),
        "rdy site's reply has the warning too"
    );
    for args in [
        vec!["serve", "--profile", "guide"],
        vec!["--json", "serve", "--profile", "guide"],
        vec!["site", "--profile", "guide"],
        vec!["--json", "site", "--profile", "guide"],
    ] {
        let (status, out) = with_closed_stderr(human_command(p, &env, &args));
        assert_eq!(status.code(), Some(1), "{args:?}: an error, not a panic");
        assert_eq!(out, "", "{args:?}: no result line");
    }
    assert_eq!(
        ok_ctl(p, "site_url", json!({"profile": "guide", "only": true})),
        want,
        "the daemon still serves its profile site"
    );
}

/// Line discipline: text and JSON, the warnings' channels, one result per call, and the clap
/// conflicts, refused before any connection.
#[test]
fn spv10_profile_cli_json_and_conflicts_are_closed() {
    // the foreground start in JSON: one JSON line, the same object a reuse prints
    let (s, _, _) = served();
    let mut cmd = human_command(
        s.dir.path(),
        &s.env_pairs(),
        &["--json", "serve", "--profile", "guide"],
    );
    cmd.stdin(Stdio::null());
    let child = Proc::spawn(cmd);
    let first: Value = serde_json::from_str(&child.line()).unwrap();
    let want = ok_ctl(
        s.dir.path(),
        "site_url",
        json!({"profile": "guide", "only": true}),
    );
    assert_eq!(first, want);
    let reuse = s.rdy(&["--json", "serve", "--profile", "guide"]);
    assert_eq!(stdout(&reuse), format!("{want}\n"));
    assert_eq!(
        stderr(&reuse),
        "",
        "no LAN warning, no copy of the warnings"
    );
    let stop = s.rdy(&["serve", "--stop"]);
    assert_eq!(stdout(&stop), "stopped\n");
    let (status, rest, err) = child.finish();
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(rest.is_empty(), "one JSON line, no text result: {rest:?}");
    assert!(!err.contains("profile site"), "{err}");
    assert!(
        !err.contains(GUIDE_CAVEAT),
        "the warnings stay inside the JSON"
    );

    // a LAN site: the text lines on stdout, the structured warning on stderr
    let (dir, _, _) = seeded_dir(Some("serve:\n  site_lan: true\n"));
    let (_cfg, env) = human_env("human:qa");
    let h = Hosted::start_for_binary(dir, Some(LanOverride::Addr(Ipv4Addr::LOCALHOST)));
    let p = h.dir.path();
    let want = ok_ctl(p, "site_url", json!({"profile": "guide", "only": true}));
    assert!(want["lan_url"].is_string(), "{want}");
    let out = human_command(p, &env, &["serve", "--profile", "guide"])
        .output()
        .unwrap();
    let text = stdout(&out);
    assert_eq!(text, serve_text(&want));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[1], want["lan_url"].as_str().unwrap());
    assert_eq!(*lines.last().unwrap(), GUIDE_CAVEAT);
    assert!(!text.contains("changes:"), "no changes line");
    assert_eq!(
        stderr(&out),
        format!(
            "warning: anyone on your network can read the profile \"Guide book\" at {}\nfix: set serve.site_lan to false to keep the site on this machine\n",
            want["lan_url"].as_str().unwrap()
        )
    );
    let out = human_command(p, &env, &["--json", "serve", "--profile", "guide"])
        .output()
        .unwrap();
    assert_eq!(stdout(&out), format!("{want}\n"));
    assert_eq!(stderr(&out), lan_lines(&want));
    drop(h);

    // the conflicts are clap's own refusal, before any connection or start
    let fresh = short_tempdir();
    drop(Rdy::init(fresh.path(), "rdy-standard").unwrap());
    let (_cfg, env) = human_env("human:qa");
    for extra in [
        vec!["--stop"],
        vec!["--detach"],
        vec!["--detach", "--spawned"],
        vec!["--spawned"],
    ] {
        let mut args = vec!["serve", "--profile", "guide"];
        args.extend(&extra);
        let out = human_command(fresh.path(), &env, &args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{extra:?}");
        let err = stderr(&out);
        assert!(err.starts_with("error: "), "{err}");
        assert!(
            err.contains("cannot be used with") || err.contains("required"),
            "{extra:?}: {err}"
        );
        assert!(raw_stdout(&out).is_empty());
        assert!(!err.contains("rdy serve:"), "{err}");
        assert!(!RunDir::of_project(fresh.path()).pid_file().exists());
    }
    let out = human_command(
        fresh.path(),
        &env,
        &["serve", "--profile", "guide", "--stop"],
    )
    .output()
    .unwrap();
    assert!(
        stderr(&out).contains("the argument '--profile <PROFILE>' cannot be used with '--stop'"),
        "{}",
        stderr(&out)
    );

    // the help says what each flag does
    let site = help(fresh.path(), &["site", "--help"]);
    assert!(
        site.contains(
            "--profile <PROFILE> Show this publication profile's selection. Accepts an id, a key, or implicit:org."
        ),
        "{site}"
    );
    let serve = help(fresh.path(), &["serve", "--help"]);
    assert!(
        serve.contains(
            "--profile <PROFILE> Serve canon through this profile on a separate port. Starts in the foreground, or reuses the running daemon."
        ),
        "{serve}"
    );
    assert!(!site.contains("--only"), "no only-mode flag: {site}");
}

/// `rdy publication check` takes view and proposal candidates against a stable baseline; a
/// blocked check still exits 0; a review baseline and review-scoped static output are refused.
#[test]
fn spv10_cli_check_reviews_without_widening_output() {
    let (s, _, prop) = served();
    let proposal = format!("proposal:{prop}");
    let check = help(s.dir.path(), &["publication", "check", "--help"]);
    assert!(
        check.contains(
            "--scope <SCOPE> canon (default), tag:<name>, landing:<hash>, a view name, view:<name>, or proposal:P-x@n."
        ),
        "{check}"
    );
    assert!(
        check.contains(
            "--against <AGAINST> A baseline to compare with: canon, tag:<name> or landing:<hash>."
        ),
        "{check}"
    );

    // a proposal-only public profile refers outside its selection: blocked, exit 0
    let out = s.rdy(&[
        "publication",
        "check",
        "--profile",
        "preview",
        "--scope",
        &proposal,
    ]);
    let t = stdout(&out);
    assert!(
        t.starts_with("Publication profile preview: blocked.\n"),
        "{t}"
    );
    assert_eq!(t.lines().count(), 3, "{t}");
    let (ok, v) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "preview",
        "--scope",
        &proposal,
    ]);
    assert!(ok, "{v}");
    assert_eq!(v["references"]["status"], "blocked", "{v}");
    assert!(v["disclosure"].is_null(), "{v}");
    // a proposal candidate against a stable baseline
    let (ok, v) = s.json(&[
        "--json",
        "publication",
        "check",
        "--profile",
        "guide",
        "--scope",
        &proposal,
        "--against",
        "canon",
    ]);
    assert!(ok, "{v}");
    assert_eq!(v["references"]["status"], "warning", "{v}");
    assert!(v["disclosure"].is_object(), "{v}");
    let t = stdout(&s.rdy(&[
        "publication",
        "check",
        "--profile",
        "guide",
        "--scope",
        &proposal,
        "--against",
        "canon",
    ]));
    assert!(
        t.contains("\nDisclosure changes: 0 added docs, 0 removed docs, 0 changed docs, "),
        "{t}"
    );

    // a view, by name or as view:<name>, is a candidate too
    for scope in ["draft", "view:draft"] {
        let (ok, v) = s.json(&[
            "--json",
            "publication",
            "check",
            "--profile",
            "guide",
            "--scope",
            scope,
            "--against",
            "canon",
        ]);
        assert!(ok, "{scope}: {v}");
        assert_eq!(v["references"]["status"], "warning", "{v}");
    }

    // the baseline stays stable
    for against in ["view:draft", "draft", proposal.as_str()] {
        let out = s.rdy(&[
            "publication",
            "check",
            "--profile",
            "guide",
            "--scope",
            "view:draft",
            "--against",
            against,
        ]);
        assert_eq!(out.status.code(), Some(1), "{against}");
        assert_eq!(
            stderr(&out),
            "error E_BAD_INPUT: Publication check against must be canon, tag:<name>, or landing:<hash>.\nfix: Use canon, tag:<name>, or landing:<hash>.\n",
            "{against}: {}",
            stderr(&out)
        );
    }

    // static output keeps its stable scopes
    let out_dir = short_tempdir();
    let site_out = out_dir.path().join("site");
    let html_out = out_dir.path().join("doc.html");
    for args in [
        vec![
            "render",
            "--out",
            site_out.to_str().unwrap(),
            "--scope",
            "view:draft",
            "--profile",
            "guide",
        ],
        vec![
            "export",
            "--out",
            html_out.to_str().unwrap(),
            "--format",
            "html",
            "--scope",
            &proposal,
            "--profile",
            "preview",
        ],
    ] {
        let out = s.rdy(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&out).starts_with(
                "error E_BAD_INPUT: A publication profile requires canon, a tag, or a landing.\n"
            ),
            "{args:?}: {}",
            stderr(&out)
        );
    }
    assert!(!site_out.exists() && !html_out.exists(), "nothing written");
}
