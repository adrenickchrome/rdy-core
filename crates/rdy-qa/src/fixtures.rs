use rdy_engine::{Rdy, RdyError, Session, WriteRequest, WriteResult};
use rdy_model::{Block, BlockId, Change, ChangeLinks, FieldValue, Op, Role};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// `path` as one YAML single-quoted scalar: a native path (a Windows drive, backslashes, a space
/// or a quote in a directory name) reads back as the same path.
pub fn yaml_path(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

pub fn writer() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}
pub fn approver() -> Session {
    Session::human("human:qa".into(), Role::Approver).unwrap()
}
pub fn reader() -> Session {
    Session::human("human:qa".into(), Role::Reader).unwrap()
}
/// An agent session (writer) for ownership tests.
pub fn agent(actor: &str, run: Option<&str>) -> Session {
    Session::agent(actor.into(), run.map(str::to_string), Role::Writer).unwrap()
}

/// A user config naming `actor` as the human: `<tmp>/rdy/config.yaml`, and the environment
/// (`XDG_CONFIG_HOME`) that points a client at it. Keep the `TempDir` alive.
pub fn human_env(actor: &str) -> (tempfile::TempDir, Vec<(String, String)>) {
    human_env_of(actor, tempfile::tempdir().expect("tempdir"))
}

/// `human_env`, with the config under `parent`.
pub fn human_env_in(
    actor: &str,
    parent: &std::path::Path,
) -> (tempfile::TempDir, Vec<(String, String)>) {
    let dir = tempfile::Builder::new()
        .prefix("rdycfg")
        .tempdir_in(parent)
        .unwrap_or_else(|e| panic!("tempdir in {}: {e}", parent.display()));
    human_env_of(actor, dir)
}

fn human_env_of(actor: &str, dir: tempfile::TempDir) -> (tempfile::TempDir, Vec<(String, String)>) {
    std::fs::create_dir_all(dir.path().join("rdy")).expect("config dir");
    std::fs::write(
        dir.path().join("rdy/config.yaml"),
        format!("actor: {actor}\n"),
    )
    .expect("config");
    // ain: an empty home of its own, so no process reads or starts the real meta project
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home dir");
    let env = person_env_pairs(&home, dir.path());
    (dir, env)
}

/// An agent session (writer) with run `run`.
pub fn as_agent(actor: &str, run: &str) -> Session {
    Session::agent(actor.into(), Some(run.into()), Role::Writer).unwrap()
}

/// The human, as a writer (`writer()`).
pub fn human_writer() -> Session {
    writer()
}

pub fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().expect("tempdir");
    let r = Rdy::init(dir.path(), "rdy-standard").expect("init");
    (dir, r)
}

/// A test's ops, parsed as agents' ops are (`rdy_engine::parse_ops`): a JSON array of ops.
pub fn parse_test_ops(ops: serde_json::Value) -> Result<Vec<rdy_engine::OpInput>, RdyError> {
    let ops: Vec<serde_json::Value> = serde_json::from_value(ops)
        .map_err(|e| RdyError::new(rdy_engine::E_BAD_INPUT, e.to_string(), "fix the test's ops"))?;
    rdy_engine::parse_ops(ops)
}

pub fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> Result<WriteResult, RdyError> {
    let ops = parse_test_ops(ops)?;
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

pub fn write_as(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    ops: serde_json::Value,
) -> Result<WriteResult, RdyError> {
    let ops = parse_test_ops(ops)?;
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

pub fn version_of(r: &Rdy, scope: &rdy_engine::Scope, id: &str) -> String {
    let id = BlockId::parse(id).expect("block id");
    r.read()
        .materialized(scope)
        .expect("scope")
        .version_of(&id)
        .expect("block in scope")
        .to_string()
}

pub fn body_of(r: &Rdy, scope: &rdy_engine::Scope, id: &str) -> String {
    let id = BlockId::parse(id).expect("block id");
    r.read()
        .materialized(scope)
        .expect("scope")
        .get(&id)
        .expect("block in scope")
        .body
        .clone()
}

/// One glossary doc "Seed" with one section and one claim, written into `view` by `writer()`.
pub fn seed_doc(r: &mut Rdy, view: &str) -> (String, String, String) {
    let res = write(r, view, serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "seed", "title": "Seed", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "one"}
    ])).expect("seed");
    (
        res.created["$d"].clone(),
        res.created["$s"].clone(),
        res.created["$c"].clone(),
    )
}

/// cf1: a frozen conflict, cres1's. agent:a (run r1) proposes decision d in view a; agent:b
/// (run r1) forks view b from a and edits d's statement; a lands, so the gate accepts d, and the
/// landing rebases b, leaving d conflicted in b between b's edit and the landed acceptance.
/// agent:c (run r1) also forks view c from a and edits d before the landing: its change is a
/// stale edit of d that no other view includes. Returns d's id, its section's and c's change.
pub fn frozen_conflict(r: &mut Rdy) -> (String, String, String) {
    let seeded = write(r, "seed", serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "log", "title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    let s = seeded.created["$s"].clone();
    land(r, "seed").unwrap();
    let (a, b) = (as_agent("agent:a", "r1"), as_agent("agent:b", "r1"));
    let d = write_as(
        r,
        &a,
        "a",
        serde_json::json!([{"op": "create", "type": "decision", "id": "$x", "parent": s,
        "fields": {"statement": "D", "status": "proposed"}}]),
    )
    .unwrap()
    .created["$x"]
        .clone();
    let vd = version_of(r, &rdy_engine::Scope::View("a".into()), &d);
    r.view_fork(&b, "a", "b").unwrap();
    write_as(
        r,
        &b,
        "b",
        serde_json::json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by b"}}]),
    )
    .unwrap();
    let c = as_agent("agent:c", "r1");
    r.view_fork(&c, "a", "c").unwrap();
    let cc = write_as(
        r,
        &c,
        "c",
        serde_json::json!([{"op": "update", "id": d, "base": vd, "fields": {"statement": "D by c"}}]),
    )
    .unwrap()
    .change;
    land(r, "a").unwrap();
    (d, s, cc)
}

/// Answers the open items of `view`'s change plan that readiness gates
/// ([`answer_open_items`](crate::intake::answer_open_items), O23), proposes `view` as a writer
/// with a summary ([`summary_for`](crate::intake::summary_for): readiness needs one, D-ME2HD),
/// accepts its deferrals and confirms each exception as the approver (F2) and lands it as the
/// approver; returns the landing id.
pub fn land(r: &mut Rdy, view: &str) -> Result<String, RdyError> {
    crate::intake::answer_open_items(r, &writer(), view);
    let summary = crate::intake::summary_for(r, view);
    let p = r.propose(&writer(), view, view, summary)?;
    crate::intake::confirm_before_landing(r, &approver(), &p);
    r.land(&approver(), &p.id)
}

/// Lands `view` straight through the store, with no acceptance change: the way a store lands
/// that holds no rule about order keys. A tie in an order key gets into canon this way by hand
/// (an imported tree or an older store holds one the same way).
pub fn land_with_a_tie(r: &mut Rdy, view: &str) {
    let by = approver().provenance();
    let store = r.store_mut();
    let p = store
        .propose(view, view, by.clone())
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    store
        .land_atomic(rdy_store::LandRequest {
            proposal: p,
            accept: None,
            approver: by,
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap_or_else(|e| panic!("landing {view}: {e}"));
    r.invalidate();
}

pub fn raw(
    id: &str,
    ty: &str,
    parent: Option<&str>,
    order: &str,
    fields: &[(&str, &str)],
    body: &str,
) -> Block {
    Block {
        id: BlockId::parse(id).expect("valid id"),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).expect("valid parent id")),
        order: order.into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
            .collect::<BTreeMap<_, _>>(),
        body: body.into(),
        links: Default::default(),
        pins: Default::default(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// Writes blocks straight to the store, bypassing every engine rule, then drops caches.
pub fn raw_commit(r: &mut Rdy, view: &str, blocks: Vec<Block>) {
    let ops = blocks
        .into_iter()
        .map(|block| Op::Create { block })
        .collect();
    r.store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops,
                deps: vec![],
                provenance: writer().provenance(),
                justification: None,
                message: "raw".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .expect("raw commit");
    r.invalidate();
}

pub fn deep() -> bool {
    std::env::var("RDY_QA_DEEP").is_ok_and(|v| v == "1")
}

/// Proptest case count: `fast`, or ten times it in deep mode.
pub fn cases(fast: u32) -> u32 {
    if deep() { fast * 10 } else { fast }
}

/// The file name of the `rdy` binary on this platform: `rdy`, or `rdy.exe` on Windows.
pub fn rdy_exe_name() -> String {
    format!("rdy{}", std::env::consts::EXE_SUFFIX)
}

/// The `rdy` binary beside the executable `exe` of a build profile.
fn rdy_bin_in(exe: &std::path::Path) -> PathBuf {
    let dir = exe.parent().expect("the binary's directory");
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir.parent().expect("target dir").join(rdy_exe_name())
    } else {
        dir.join(rdy_exe_name())
    }
}

/// The `rdy` binary: `$RDY_BIN`, else the `rdy` of the profile this binary was built in: a test
/// runs from `target/<profile>/deps/`, a qa binary (`qa-perf`) from `target/<profile>/`.
pub fn rdy_bin() -> PathBuf {
    if let Ok(p) = std::env::var("RDY_BIN") {
        return PathBuf::from(p);
    }
    let bin = rdy_bin_in(&std::env::current_exe().expect("current exe"));
    assert!(
        bin.exists(),
        "{} not found: run `cargo build --release -p rdy-cli` first (cargo qa does this) or set RDY_BIN",
        bin.display()
    );
    bin
}

/// An empty home for every `rdy` this process starts with no home of its own: it holds no
/// `.rdy/meta` and no `.config`, so no test reads or starts the real person layer or user
/// config. It lives as long as the process.
pub fn empty_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("rdyhome")
            .tempdir()
            .expect("an empty home")
    })
    .path()
}

/// The account name every child of the harness sees (`USERNAME`), whoever runs the tests.
pub const FIXTURE_USERNAME: &str = "rdyqa";

/// The person variables for a child whose home is `home` and whose config place is `cfg`: the
/// ones that are set, as pairs (`HOME`, `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`, `APPDATA`,
/// `XDG_CONFIG_HOME`), then the fixture account name. Where the host has no drive form of a
/// path, the drive variables are absent here and removed by [`isolate_person`].
pub fn person_env_pairs(home: &std::path::Path, cfg: &std::path::Path) -> Vec<(String, String)> {
    let vars = crate::instructions::person_env(
        rdy_engine::instructions_config::Platform::HOST,
        Some(home),
        cfg,
    );
    let mut pairs: Vec<(String, String)> = crate::instructions::person_pairs(&vars)
        .into_iter()
        .filter_map(|(k, v)| Some((k.to_string(), v?.to_string_lossy().into_owned())))
        .collect();
    pairs.push(("USERNAME".to_string(), FIXTURE_USERNAME.to_string()));
    pairs
}

/// Gives `cmd` an isolated person: every variable that places a home or config is set below
/// `home` and `cfg` or removed, `USERNAME` is [`FIXTURE_USERNAME`], and `SSH_AUTH_SOCK` is
/// removed. A test that needs an agent sets `SSH_AUTH_SOCK` itself, after this call. A child's
/// descendants inherit all of it.
pub fn isolate_person<'a>(
    cmd: &'a mut Command,
    home: &std::path::Path,
    cfg: &std::path::Path,
) -> &'a mut Command {
    let vars = crate::instructions::person_env(
        rdy_engine::instructions_config::Platform::HOST,
        Some(home),
        cfg,
    );
    for (name, value) in crate::instructions::person_pairs(&vars) {
        match value {
            Some(v) => cmd.env(name, v),
            None => cmd.env_remove(name),
        };
    }
    isolate_account(cmd)
}

/// The account part of [`isolate_person`]: the fixed `USERNAME`, and no agent socket.
pub fn isolate_account(cmd: &mut Command) -> &mut Command {
    cmd.env("USERNAME", FIXTURE_USERNAME)
        .env_remove("SSH_AUTH_SOCK")
}

/// `Command::new(rdy_bin())` with the person in the [`empty_home`] ([`isolate_person`]): the way
/// every test starts `rdy`. A later `env` or `envs` of the caller wins.
pub fn rdy_command() -> Command {
    let home = empty_home();
    let mut cmd = Command::new(rdy_bin());
    isolate_person(&mut cmd, home, &home.join(".config"));
    cmd
}

/// Runs `cmd` as `Command::output` does (stdin null, stdout and stderr captured), but kills it
/// at `budget`: a hang guard, not a speed check. A command still running at `budget` is killed,
/// reaped, and returned as an error.
pub fn output_within(cmd: &mut Command, budget: Duration) -> Result<Output, String> {
    output_within_stdin(cmd, None, budget)
}

/// `output_within` with `stdin` fed to the command on a thread of its own, so a command that
/// stops reading cannot block the caller: the same `budget` bounds the feed and the run. A
/// command that closes its stdin early is not an error, that is its answer.
pub fn output_within_stdin(
    cmd: &mut Command,
    stdin: Option<&[u8]>,
    budget: Duration,
) -> Result<Output, String> {
    let mut child = cmd
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{cmd:?} did not start: {e}"))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let bytes = bytes.to_vec();
        // left behind on a kill like the drains: the dead child closes the pipe and ends it
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + budget;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                // the drain threads are left behind: a grandchild may still hold the pipes open
                return Err(format!("{cmd:?} still running after {budget:?}"));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{cmd:?} could not be waited on: {e}"));
            }
        }
    };
    Ok(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

/// Reads `pipe` to its end on a thread (nothing when there is no pipe).
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = vec![];
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// The libtest name of `test_fn` in the test file whose `module_path!()` is `module_path`:
/// `<file>::<fn>` inside an area binary, `<fn>` at a crate root.
pub fn libtest_name(module_path: &str, test_fn: &str) -> String {
    match module_path.split_once("::") {
        Some((_crate, rest)) => format!("{rest}::{test_fn}"),
        None => test_fn.to_string(),
    }
}

#[cfg(test)]
mod tname_tests {
    use super::libtest_name;

    #[test]
    fn tname1_a_crate_root_keeps_the_bare_name() {
        assert_eq!(
            libtest_name("store_crash", "store_crash_child"),
            "store_crash_child"
        );
    }

    /// The census reads this process's own tables: a listener it holds is listed, and it is
    /// gone once closed. Where the host reads native process data (Linux, Windows) it starts
    /// no process to do so.
    #[test]
    fn wx25a_listener_census_uses_native_process_data() {
        if cfg!(any(target_os = "linux", windows)) {
            assert!(
                ["proc", "iphlpapi"].contains(&crate::platform::LISTENER_CENSUS),
                "{}",
                crate::platform::LISTENER_CENSUS
            );
        } else {
            // a developer host without kernel tables a test can read
            assert_eq!(crate::platform::LISTENER_CENSUS, "lsof");
        }
        let before = crate::platform::tcp_listen_ports();
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = v4.local_addr().expect("addr").port();
        let v6 = std::net::TcpListener::bind("[::1]:0").ok();
        let port6 = v6.as_ref().map(|l| l.local_addr().expect("addr").port());
        let during = crate::platform::tcp_listen_ports();
        assert!(during.contains(&port), "{port} not in {during:?}");
        if let Some(p) = port6 {
            assert!(during.contains(&p), "{p} not in {during:?}");
        }
        assert!(
            !before.contains(&port),
            "{port} was listed before it was bound"
        );
        // a connection is not a listener
        let client = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let client_port = client.local_addr().expect("addr").port();
        assert!(!crate::platform::tcp_listen_ports().contains(&client_port));
        drop((v4, v6, client));
        let after = crate::platform::tcp_listen_ports();
        assert!(!after.contains(&port), "{port} still in {after:?}");
    }

    #[test]
    fn wx25a_executable_name_carries_the_platform_suffix() {
        assert_eq!(
            super::rdy_exe_name(),
            if cfg!(windows) { "rdy.exe" } else { "rdy" }
        );
        // the sibling of the test binary's directory, by that name
        let name = super::rdy_exe_name();
        assert!(name.starts_with("rdy"));
        assert!(super::rdy_bin_in(std::path::Path::new("t/deps/x")).ends_with(format!("t/{name}")));
        assert!(
            super::rdy_bin_in(std::path::Path::new("t/qa-perf")).ends_with(format!("t/{name}"))
        );
    }
    #[test]
    fn tname1_a_module_path_drops_only_the_crate() {
        assert_eq!(
            libtest_name("store::store_crash", "store_crash_child"),
            "store_crash::store_crash_child"
        );
        assert_eq!(
            libtest_name("store::store_crash::inner", "f"),
            "store_crash::inner::f"
        );
    }

    /// Runs inside a module, as every test in an area binary does: `--exact` with the name
    /// must select this one test and nothing else.
    #[test]
    fn tname1_a_reexec_by_the_name_selects_exactly_this_test() {
        let name = crate::test_name!("tname1_a_reexec_by_the_name_selects_exactly_this_test");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--list", "--exact", &name])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let listed: Vec<&str> = text.lines().filter(|l| l.ends_with(": test")).collect();
        assert_eq!(listed, vec![format!("{name}: test").as_str()], "{text}");
    }
}
