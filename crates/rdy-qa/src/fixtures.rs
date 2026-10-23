use rdy_engine::{Rdy, RdyError, Session, WriteRequest, WriteResult};
use rdy_model::{Block, BlockId, Change, ChangeLinks, FieldValue, Op, Role};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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
    let env = vec![(
        "XDG_CONFIG_HOME".to_string(),
        dir.path().to_string_lossy().into_owned(),
    )];
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

/// Proposes `view` as a writer (with a summary when it writes a decision,
/// [`summary_for`](crate::intake::summary_for)), confirms each exception as the approver (F2)
/// and lands it as the approver; returns the landing id.
pub fn land(r: &mut Rdy, view: &str) -> Result<String, RdyError> {
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

/// The `rdy` binary: `$RDY_BIN`, else the `rdy` of the profile this binary was built in: a test
/// runs from `target/<profile>/deps/`, a qa binary (`qa-perf`) from `target/<profile>/`.
pub fn rdy_bin() -> PathBuf {
    if let Ok(p) = std::env::var("RDY_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("current exe");
    let dir = exe.parent().expect("the binary's directory");
    let bin = if dir.file_name().is_some_and(|n| n == "deps") {
        dir.parent().expect("target dir").join("rdy")
    } else {
        dir.join("rdy")
    };
    assert!(
        bin.exists(),
        "{} not found: run `cargo build --release -p rdy-cli` first (cargo qa does this) or set RDY_BIN",
        bin.display()
    );
    bin
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
