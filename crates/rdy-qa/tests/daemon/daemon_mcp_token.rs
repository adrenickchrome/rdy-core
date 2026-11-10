//! mht2: the project's MCP bearer (D-6M0AF, D-QG54Z). The daemon makes `.rdy/run/mcp-token` on
//! its first start, under the start lock and before its site binds: 32 random bytes, 52
//! characters of uppercase RFC 4648 base32 and one LF, written durably and private to this
//! account. Later starts read it and keep it. A link, a reparse point, a second name, a file of
//! another account or with other access, and any malformed or oversized file fail closed: the
//! file is never replaced, and MCP over HTTP stays unavailable while the read-only site and the
//! private ctl and stdio channel keep working. No diagnostic holds the bearer.
//!
//! Another account (W3): Windows asks the system's access check (Authz) what the second account
//! (`RDY_QA_SECOND_ACCOUNT`, default `rdyother`) is granted; a missing account is an error, and
//! the hosted runner, which has none, does not pass this test (the native gate runs it). Unix
//! has no second-account fixture, so there the test proves only the mode and the owner of the
//! opened handle: no process of another account tries the read.
use rdy_daemon::mcp_token::{self, McpToken, TOKEN_FILE, TokenError};
use rdy_daemon::serve::{StartPoint, with_start_hook_for_tests};
use rdy_daemon::{RunDir, ServeError, ServeOptions, ServeRun, start};
use rdy_engine::Rdy;
use rdy_qa::daemon::{Hosted, Served, short_tempdir};
use rdy_qa::platform;
use rdy_store::fileid::FileId;
use rdy_store::fsutil::{PrivateWriteStage, with_private_write_probe_for_tests};
use rdy_store::pio;
use serde_json::json;
use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Barrier};

/// The refusal of a token file that is not a valid private token file.
const INVALID: &str = "the project's MCP token is not a valid private token file";

/// A well-formed token file: 52 base32 characters (32 zero bytes) and one LF.
const ZERO_TOKEN: &[u8; 53] = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";

/// A fresh `rdy-standard` project in a short directory.
fn project() -> tempfile::TempDir {
    let dir = short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").expect("init the project"));
    dir
}

/// A foreground start of the project at `dir`, as `Hosted` starts one.
fn opts(dir: &Path) -> ServeOptions {
    ServeOptions {
        project: dir.to_path_buf(),
        version: "t".into(),
        detach: false,
        idle_timeout: Some(std::time::Duration::from_secs(60)),
        human_actor: Some("human:qa".into()),
        site_lan_override: None,
    }
}

/// `.rdy/run/mcp-token` of the project at `dir`.
fn token_path(dir: &Path) -> PathBuf {
    RunDir::of_project(dir).path().join(TOKEN_FILE)
}

/// The temp name the durable private write uses beside the token.
fn temp_path(dir: &Path) -> PathBuf {
    RunDir::of_project(dir)
        .path()
        .join(format!(".{TOKEN_FILE}.tmp"))
}

/// Asserts that `bytes` is a well-formed token file: 52 characters of A-Z and 2-7, then LF.
fn assert_token_form(bytes: &[u8]) {
    assert_eq!(bytes.len(), 53, "52 characters and one LF");
    assert_eq!(bytes[52], b'\n');
    assert!(
        bytes[..52]
            .iter()
            .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(b)),
        "uppercase RFC 4648 base32 without padding"
    );
}

/// The prepared token of a started daemon; panics with the redacted error otherwise.
fn prepared(run: &ServeRun) -> Arc<mcp_token::Prepared> {
    let p = run.mcp_token();
    if let Err(e) = &*p {
        panic!("the daemon has no MCP token: {e}");
    }
    p
}

/// The token of a successful preparation.
fn tok(p: &mcp_token::Prepared) -> &McpToken {
    p.as_ref().unwrap_or_else(|e| panic!("no MCP token: {e}"))
}

/// Whether the prepared token of `run` is the refusal of an invalid token file.
fn refused_invalid(run: &ServeRun) -> bool {
    matches!(&*run.mcp_token(), Err(TokenError::Invalid))
}

/// Stops a started daemon and waits for it.
fn stop(run: ServeRun) {
    run.handle().stop();
    run.wait().expect("the daemon stops cleanly");
}

/// Whether this account's handle on `path` says it is private to this account.
fn private_to_me(file: &fs::File) -> bool {
    let p = pio::privacy(file).expect("read the handle's privacy");
    p.mine && p.private
}

#[test]
fn mht2_token_is_private_durable_and_stable() {
    let dir = project();
    let run_dir = RunDir::of_project(dir.path());
    let token = token_path(dir.path());
    let temp = temp_path(dir.path());

    // The first start makes the token between the store's open and the endpoint's bind, under the
    // start lock; at `Bound` (before the site binds) it is already the durable file.
    let stages: Rc<RefCell<Vec<(PrivateWriteStage, PathBuf)>>> = Rc::default();
    let at_bound: Rc<RefCell<Option<Vec<u8>>>> = Rc::default();
    let hook = {
        let (token, at_bound) = (token.clone(), Rc::clone(&at_bound));
        move |point: StartPoint| {
            match point {
                StartPoint::Opened => assert!(!token.exists(), "no token before the store opens"),
                StartPoint::Bound => *at_bound.borrow_mut() = Some(fs::read(&token)?),
                _ => {}
            }
            Ok(())
        }
    };
    let probe = {
        let stages = Rc::clone(&stages);
        move |stage: PrivateWriteStage, path: &Path| {
            stages.borrow_mut().push((stage, path.to_path_buf()));
            Ok(())
        }
    };
    let served = with_start_hook_for_tests(hook, || {
        with_private_write_probe_for_tests(probe, || start(opts(dir.path())))
    })
    .unwrap_or_else(|e| panic!("start: {e}"));

    let bytes = fs::read(&token).expect("the token file");
    assert_token_form(&bytes);
    assert_eq!(
        at_bound.borrow().as_deref(),
        Some(&bytes[..]),
        "durable before the bind"
    );
    let token_stages: Vec<PrivateWriteStage> = stages
        .borrow()
        .iter()
        .filter(|(_, p)| *p == temp || *p == token || *p == run_dir.path())
        .map(|(s, _)| *s)
        .collect();
    assert_eq!(
        token_stages,
        [
            PrivateWriteStage::Created,
            PrivateWriteStage::Written,
            PrivateWriteStage::Synced,
            PrivateWriteStage::Renamed,
            PrivateWriteStage::DirSynced,
        ],
        "one durable private write: the temp file is fsynced, renamed, then the directory"
    );
    assert!(!temp.exists(), "no temp file is left");

    // Private to this account, on the handle: the file and its directory.
    assert!(private_to_me(
        &fs::File::open(&token).expect("open the token")
    ));
    assert!(private_to_me(
        &pio::open_dir(run_dir.path()).expect("open the run dir")
    ));

    // The prepared token is the file's, compared on its decoded bytes.
    let p = prepared(&served);
    let t = tok(&p);
    assert!(t.matches(&bytes[..52]));
    assert!(!t.matches(&bytes), "the LF is no part of the bearer");
    assert!(!t.matches(&bytes[..52].to_ascii_lowercase()));
    assert!(!t.matches(&ZERO_TOKEN[..52]));
    assert!(!t.matches(b""));
    let identity = FileId::lstat(&token).expect("the token's identity");
    stop(served);

    // A restart reads the same file, and writes nothing.
    let stages: Rc<RefCell<Vec<(PrivateWriteStage, PathBuf)>>> = Rc::default();
    let probe = {
        let stages = Rc::clone(&stages);
        move |stage: PrivateWriteStage, path: &Path| {
            stages.borrow_mut().push((stage, path.to_path_buf()));
            Ok(())
        }
    };
    let again = with_private_write_probe_for_tests(probe, || start(opts(dir.path())))
        .unwrap_or_else(|e| panic!("restart: {e}"));
    assert!(
        !stages
            .borrow()
            .iter()
            .any(|(_, p)| *p == temp || *p == token),
        "a restart never rewrites the token"
    );
    assert_eq!(fs::read(&token).expect("the token"), bytes);
    assert_eq!(FileId::lstat(&token).expect("identity"), identity);
    let p = prepared(&again);
    assert!(tok(&p).matches(&bytes[..52]));
    stop(again);

    // The library read takes the same file and never makes one.
    let read = mcp_token::read(&run_dir).unwrap_or_else(|e| panic!("read: {e}"));
    assert!(read.matches(&bytes[..52]));
    let empty = project();
    let missing = mcp_token::read(&RunDir::of_project(empty.path()));
    assert!(
        matches!(&missing, Err(TokenError::Io(e)) if e.kind() == io::ErrorKind::NotFound),
        "{missing:?}"
    );
    assert!(!token_path(empty.path()).exists(), "a read makes no token");
}

#[test]
fn mht2_parallel_starts_keep_one_token() {
    const STARTS: usize = 4;
    let dir = project();
    let mut first: Option<Vec<u8>> = None;
    for round in 0..2 {
        let barrier = Arc::new(Barrier::new(STARTS));
        let starts: Vec<_> = (0..STARTS)
            .map(|_| {
                let (path, barrier) = (dir.path().to_path_buf(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    start(opts(&path))
                })
            })
            .collect();
        let mut winners = vec![];
        for s in starts {
            match s.join().expect("a start thread") {
                Ok(run) => winners.push(run),
                Err(ServeError::AlreadyRunning(_)) => {}
                Err(e) => panic!("round {round}: a losing start failed otherwise: {e}"),
            }
        }
        assert_eq!(winners.len(), 1, "round {round}: one start wins");
        let winner = winners.pop().expect("the winner");
        let bytes = fs::read(token_path(dir.path())).expect("the token");
        assert_token_form(&bytes);
        assert!(tok(&prepared(&winner)).matches(&bytes[..52]));
        match &first {
            None => first = Some(bytes),
            Some(was) => assert_eq!(&bytes, was, "round {round}: the token is kept"),
        }
        assert!(!temp_path(dir.path()).exists());
        stop(winner);
    }

    // Real processes: several CLI commands start the daemon at once through the start lock.
    let s = Served::new();
    let token = token_path(s.dir.path());
    let children: Vec<_> = (0..STARTS)
        .map(|_| s.spawn(&["--json", "status"]))
        .collect();
    for c in children {
        let out = c.wait_with_output().expect("a status command");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let bytes = fs::read(&token).expect("the daemon made the token");
    assert_token_form(&bytes);
    s.stop();
    assert!(s.json(&["--json", "status"]).0, "a new daemon starts");
    assert_eq!(
        fs::read(&token).expect("the token"),
        bytes,
        "kept across a stop"
    );
}

#[test]
fn mht2_token_links_and_reparse_points_are_refused() {
    let outside = tempfile::tempdir().expect("a directory outside the project");

    // A second name of a directory at the token's name: a symbolic link on Unix, a junction on
    // Windows. A standard Windows account may be refused the follow itself (error 448, W2).
    let dir = project();
    let run_dir = RunDir::of_project(dir.path());
    run_dir.ensure().expect("the run dir");
    let token = token_path(dir.path());
    let target = outside.path().join("target");
    fs::create_dir(&target).expect("the alias target");
    platform::dir_alias(&target, &token).expect("make the directory alias");
    assert!(
        platform::follow_refused(&token) || fs::metadata(&token).is_ok_and(|m| m.is_dir()),
        "the alias resolves or the system refuses to follow it"
    );
    let alias = FileId::lstat(&token).expect("the alias's identity");
    let served = start(opts(dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    assert!(refused_invalid(&served), "{:?}", served.mcp_token());
    stop(served);
    assert_eq!(
        FileId::lstat(&token).expect("identity"),
        alias,
        "left alone"
    );
    assert_eq!(
        fs::read_dir(&target).expect("list").count(),
        0,
        "nothing reached the target"
    );
    assert!(!temp_path(dir.path()).exists());
    platform::remove_dir_link(&token).expect("remove the alias");

    // A symbolic link to a well-formed private token file elsewhere.
    let dir = project();
    RunDir::of_project(dir.path())
        .ensure()
        .expect("the run dir");
    let token = token_path(dir.path());
    let planted = outside.path().join("planted");
    rdy_store::fsutil::write_private_durable(&planted, ZERO_TOKEN).expect("plant a token");
    platform::symlink(&planted, &token).expect("make the file link");
    let link = FileId::lstat(&token).expect("the link's identity");
    let served = start(opts(dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    assert!(refused_invalid(&served), "{:?}", served.mcp_token());
    stop(served);
    assert_eq!(FileId::lstat(&token).expect("identity"), link, "left alone");
    assert_eq!(fs::read(&planted).expect("planted"), ZERO_TOKEN);

    // A second name of the token file itself: a hard link in the project.
    let dir = project();
    let token = token_path(dir.path());
    let served = start(opts(dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    prepared(&served);
    stop(served);
    let bytes = fs::read(&token).expect("the token");
    platform::hard_link(&token, &dir.path().join("copy")).expect("a second name");
    let served = start(opts(dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    assert!(refused_invalid(&served), "{:?}", served.mcp_token());
    stop(served);
    assert_eq!(
        fs::read(&token).expect("the token"),
        bytes,
        "never replaced"
    );
}

#[test]
fn mht2_native_other_account_cannot_read_token() {
    eprintln!(
        "mht2_native_other_account_cannot_read_token proves: {}",
        platform::ANOTHER_ACCOUNT_PROOF
    );
    // The control first: a directory shared on purpose is readable by the other account. Its
    // parent is private, so no inherited grant decides the answer before the share.
    let root = tempfile::tempdir().expect("tempdir");
    let parent = root.path().join("fixture");
    pio::create_private_dir(&parent).expect("a private parent");
    let shared = parent.join("shared");
    pio::create_private_dir(&shared).expect("the shared directory");
    assert!(
        !platform::another_account_can_read(&shared).expect("check before"),
        "a private directory is closed to the other account"
    );
    platform::share_with_another_account(&shared).expect("share");
    assert!(
        platform::another_account_can_read(&shared).expect("check after"),
        "the fixture grants a deliberate read"
    );

    // The token and its directory are closed to it.
    let dir = project();
    let run_dir = RunDir::of_project(dir.path());
    let token = token_path(dir.path());
    let served = start(opts(dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    let bytes = fs::read(&token).expect("the token");
    for object in [token.as_path(), run_dir.path()] {
        assert!(
            !platform::another_account_can_read(object).expect("check"),
            "the other account reads {}",
            object.display()
        );
    }
    // A guessed bearer is no bearer.
    let p = prepared(&served);
    let t = tok(&p);
    assert!(t.matches(&bytes[..52]));
    let mut guess = bytes[..52].to_vec();
    guess[0] = if guess[0] == b'A' { b'B' } else { b'A' };
    assert!(!t.matches(&guess));
    assert!(!t.matches(&ZERO_TOKEN[..52]) || bytes == ZERO_TOKEN);
    stop(served);
}

#[test]
fn mht2_bad_or_oversized_token_fails_closed() {
    let dir = project();
    let run_dir = RunDir::of_project(dir.path());
    run_dir.ensure().expect("the run dir");
    let token = token_path(dir.path());
    let at = |n: usize, c: u8| {
        let mut v = ZERO_TOKEN.to_vec();
        v[n] = c;
        v
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("no LF", ZERO_TOKEN[..52].to_vec()),
        ("short", [&ZERO_TOKEN[..51], b"\n"].concat()),
        ("one byte over", [&ZERO_TOKEN[..52], b"A\n"].concat()),
        ("two LFs", [&ZERO_TOKEN[..], b"\n"].concat()),
        ("large", vec![b'A'; 1 << 20]),
        ("lowercase", ZERO_TOKEN.to_ascii_lowercase()),
        ("padding", at(51, b'=')),
        ("digit 1", at(10, b'1')),
        ("digit 8", at(10, b'8')),
        ("trailing bits", at(51, b'B')),
        ("CR", at(51, b'\r')),
        ("space for LF", at(52, b' ')),
        ("NUL", at(0, 0)),
        ("non-ASCII", at(5, 0xC3)),
    ];
    for (name, bad) in &cases {
        rdy_store::fsutil::write_private_durable(&token, bad).expect("write the bad file");
        let identity = FileId::lstat(&token).expect("identity");
        match mcp_token::prepare(&run_dir) {
            Err(e @ TokenError::Invalid) => {
                assert_eq!(e.to_string(), INVALID, "{name}");
            }
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(
            &fs::read(&token).expect("the file"),
            bad,
            "{name}: never replaced"
        );
        assert_eq!(FileId::lstat(&token).expect("identity"), identity, "{name}");
        assert!(!temp_path(dir.path()).exists(), "{name}");
        assert!(
            matches!(mcp_token::read(&run_dir), Err(TokenError::Invalid)),
            "{name}: a read refuses it too"
        );
    }

    // The control: the well-formed file is accepted as it is.
    rdy_store::fsutil::write_private_durable(&token, ZERO_TOKEN).expect("write");
    let t = mcp_token::prepare(&run_dir).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.matches(&ZERO_TOKEN[..52]));
    // A well-formed file that another account may read.
    platform::open_to_everyone(&token).expect("open the file to everyone");
    assert!(matches!(
        mcp_token::prepare(&run_dir),
        Err(TokenError::Invalid)
    ));
    assert_eq!(fs::read(&token).expect("the file"), ZERO_TOKEN);
    // A directory at the token's name.
    fs::remove_file(&token).expect("remove");
    fs::create_dir(&token).expect("a directory at the name");
    assert!(matches!(
        mcp_token::prepare(&run_dir),
        Err(TokenError::Invalid)
    ));
    assert!(fs::metadata(&token).expect("still there").is_dir());
    fs::remove_dir(&token).expect("remove the directory");

    // A daemon fails closed on an oversized file and on one that is not private, at every start.
    for (name, bad, open) in [
        ("oversized", [&ZERO_TOKEN[..], b"\n"].concat(), false),
        ("not private", ZERO_TOKEN.to_vec(), true),
    ] {
        rdy_store::fsutil::write_private_durable(&token, &bad).expect("write");
        if open {
            platform::open_to_everyone(&token).expect("open the file to everyone");
        }
        for _ in 0..2 {
            let served = start(opts(dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
            assert!(refused_invalid(&served), "{name}: {:?}", served.mcp_token());
            stop(served);
            assert_eq!(
                fs::read(&token).expect("the file"),
                bad,
                "{name}: never rotated"
            );
        }
        fs::remove_file(&token).expect("remove");
    }
}

#[test]
fn mht2_token_prepare_failure_keeps_mcp_unavailable() {
    let log = rdy_daemon::daemon::capture_log_for_tests(&["mcp http: "]);
    let dir = project();
    let token = token_path(dir.path());
    let temp = temp_path(dir.path());

    // A durable write that fails: the temp file's fsync.
    let probe = {
        let temp = temp.clone();
        move |stage: PrivateWriteStage, path: &Path| {
            if stage == PrivateWriteStage::Synced && path == temp {
                return Err(io::Error::other("injected sync failure"));
            }
            Ok(())
        }
    };
    let served = with_private_write_probe_for_tests(probe, || start(opts(dir.path())))
        .unwrap_or_else(|e| panic!("the daemon starts without its MCP token: {e}"));
    match &*served.mcp_token() {
        Err(TokenError::Io(e)) => assert_eq!(e.to_string(), "injected sync failure"),
        other => panic!("{other:?}"),
    }
    assert!(!token.exists(), "no token file");
    assert!(!temp.exists(), "no temp file");
    assert!(
        log.lines()
            .contains(&"mcp http: unavailable: injected sync failure".to_string()),
        "{:?}",
        log.lines()
    );
    let mut h = Hosted {
        dir,
        run: Some(served),
    };
    assert_site_and_ctl_serve(&h);
    h.stop();

    // The next start makes the token.
    let served = start(opts(h.dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    prepared(&served);
    assert_token_form(&fs::read(&token).expect("the token"));
    stop(served);

    // An invalid file: the same, with the fixed refusal.
    rdy_store::fsutil::write_private_durable(&token, b"not a token\n").expect("write");
    let served = start(opts(h.dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    assert!(refused_invalid(&served));
    assert!(
        log.lines()
            .contains(&format!("mcp http: unavailable: {INVALID}")),
        "{:?}",
        log.lines()
    );
    h.run = Some(served);
    assert_site_and_ctl_serve(&h);
    h.stop();
}

/// The read-only site serves its page, and the private ctl channel answers.
fn assert_site_and_ctl_serve(h: &Hosted) {
    let url = h
        .run
        .as_ref()
        .expect("running")
        .shared()
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    let page = rdy_qa::http::get(&url);
    assert_eq!(page.status, 200, "{}", page.head);
    Hosted::rt().block_on(async {
        let mut c = h.client(None, None).await;
        c.ctl("status", json!({})).await.expect("status");
    });
}

#[test]
fn mht2_no_secret_reaches_diagnostics() {
    // In this process: Debug, the ctl replies and the site's page.
    let dir = project();
    let served = start(opts(dir.path())).unwrap_or_else(|e| panic!("start: {e}"));
    let bytes = fs::read(token_path(dir.path())).expect("the token");
    let secret = String::from_utf8(bytes[..52].to_vec()).expect("text");
    let p = prepared(&served);
    let debug = format!("{:?} {:?}", tok(&p), p);
    assert!(!debug.contains(&secret), "{debug}");
    assert!(debug.contains("redacted"), "{debug}");
    let h = Hosted {
        dir,
        run: Some(served),
    };
    let url = h
        .run
        .as_ref()
        .expect("running")
        .shared()
        .site_info
        .get()
        .and_then(|i| i.url.clone().ok())
        .expect("the site listens");
    let page = rdy_qa::http::get(&url);
    assert!(!String::from_utf8_lossy(&page.body).contains(&secret));
    assert!(!page.head.contains(&secret));
    Hosted::rt().block_on(async {
        for (actor, run) in [(None, None), (Some("agent:w"), Some("r1"))] {
            let mut c = h.client(actor, run).await;
            for (op, args) in [("status", json!({})), ("site_url", json!({}))] {
                let reply = format!("{:?}", c.ctl(op, args).await);
                assert!(!reply.contains(&secret), "{op}: {reply}");
            }
        }
    });
    drop(h);

    // A refused file that holds a bearer and one byte more: no error or Debug holds its bytes.
    let other = project();
    let run_dir = RunDir::of_project(other.path());
    run_dir.ensure().expect("the run dir");
    let held = [secret.as_bytes(), b"A\n"].concat();
    rdy_store::fsutil::write_private_durable(&token_path(other.path()), &held).expect("write");
    let e = mcp_token::prepare(&run_dir).expect_err("refused");
    let text = format!("{e} {e:?}");
    assert!(!text.contains(&secret), "{text}");

    // A daemon a CLI started: its status, its site reply and its whole log.
    let s = Served::new();
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    let token = token_path(s.dir.path());
    let secret =
        String::from_utf8(fs::read(&token).expect("the token")[..52].to_vec()).expect("text");
    let (ok, site) = s.json(&["--json", "site"]);
    assert!(ok, "{site}");
    let (_, status) = s.json(&["--json", "status"]);
    for (what, text) in [("status", status.to_string()), ("site", site.to_string())] {
        assert!(!text.contains(&secret), "{what}: {text}");
    }
    // the next start reads a refused file that holds the bearer, and logs its refusal
    s.stop();
    rdy_store::fsutil::write_private_durable(&token, &[secret.as_bytes(), b"A\n"].concat())
        .expect("write");
    let (ok, status) = s.json(&["--json", "status"]);
    assert!(ok, "{status}");
    s.stop();
    let log = fs::read_to_string(s.run_dir().log_file()).expect("the daemon log");
    assert!(
        log.contains(&format!("mcp http: unavailable: {INVALID}")),
        "{log}"
    );
    assert!(!log.contains(&secret), "{log}");
}
