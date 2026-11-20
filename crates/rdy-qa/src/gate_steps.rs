//! The gate's prerequisite steps (the locked dependency fetch, then the release build), its format
//! check (one workspace member at a time) and its structured test discovery: Cargo's artifact
//! record names each test executable, and the gate lists each one directly, with no shell.
//!
//! Each stage is a [`GateStep`]: it names the steps it needs, and a failed step skips them
//! ([`Stages`], [`run_steps`]) unless the gate runs with `--all-stages`. Each child runs under a
//! wall-clock deadline ([`run_stage`]); past it the child's process group gets TERM, then KILL
//! after [`KILL_GRACE`], and the row names the stage.
use crate::platform::ProcessGroup;
use crate::report::{self, Gate};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A gate step's id: the name of its report row.
pub type StepId = String;

/// One stage of the gate: its report row, the steps it needs, and its child's deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateStep {
    pub id: StepId,
    /// The steps this one needs. When one of them failed or was skipped, this one is skipped.
    pub after: Vec<StepId>,
    /// The wall-clock limit of each child this step starts.
    pub deadline: Duration,
}

impl GateStep {
    pub fn new(id: impl Into<StepId>, after: &[&str], deadline: Duration) -> Self {
        Self {
            id: id.into(),
            after: after.iter().map(|s| s.to_string()).collect(),
            deadline,
        }
    }
}

/// The steps that failed or were skipped, each with the failed step at its root.
#[derive(Debug, Default)]
pub struct Stages {
    all_stages: bool,
    broken: BTreeMap<StepId, StepId>,
}

impl Stages {
    /// `all_stages`: run every step whatever failed before it (`--all-stages`).
    pub fn new(all_stages: bool) -> Self {
        Self {
            all_stages,
            broken: BTreeMap::new(),
        }
    }

    /// The failed step that keeps `step` from running, if any. With `--all-stages`, none.
    pub fn blocked(&self, step: &GateStep) -> Option<StepId> {
        if self.all_stages {
            return None;
        }
        step.after
            .iter()
            .find_map(|dep| self.broken.get(dep).cloned())
    }

    /// Records the result of a step that ran.
    pub fn record(&mut self, step: &GateStep, ok: bool) {
        if !ok {
            self.broken.insert(step.id.clone(), step.id.clone());
        }
    }

    /// Records `step` as skipped after the failed step `root`, and returns its report row.
    pub fn skip(&mut self, step: &GateStep, root: StepId) -> Gate {
        eprintln!("==> {}: SKIPPED (after {root} failed)", step.id);
        self.broken.insert(step.id.clone(), root.clone());
        Gate::skipped(&step.id, root)
    }
}

/// Runs `steps` in order through `run` (step in, success and output back), skipping each step
/// whose dependency failed or was skipped (unless `all_stages`). One report row per step; a
/// failed row keeps the tail of its output.
pub fn run_steps(
    steps: &[GateStep],
    all_stages: bool,
    mut run: impl FnMut(&GateStep) -> (bool, String),
) -> Vec<Gate> {
    let mut stages = Stages::new(all_stages);
    steps
        .iter()
        .map(|step| match stages.blocked(step) {
            Some(root) => stages.skip(step, root),
            None => {
                let (ok, out) = run(step);
                stages.record(step, ok);
                Gate::ran(&step.id, ok, fail_tail(ok, &out))
            }
        })
        .collect()
}

/// How long a child past its deadline has, after TERM, before KILL.
pub const KILL_GRACE: Duration = Duration::from_secs(10);

/// The shortest default deadline of a child.
pub const DEADLINE_FLOOR: Duration = Duration::from_secs(600);

/// The kinds of gate child, for their default deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageKind {
    /// `cargo fetch --locked`.
    Fetch,
    /// The release build of `rdy-cli`.
    Build,
    /// One `cargo fmt --check -p <member>` (or `cargo metadata`).
    Fmt,
    /// Workspace clippy.
    Clippy,
    /// The workspace tests outside `rdy-qa`.
    WorkspaceTests,
    /// The build of every `rdy-qa` test binary (`--no-run`).
    Discover,
    /// One `rdy-qa` area binary.
    Area,
    /// The `rdy-qa` unit tests.
    Lib,
    /// The perf tier.
    Perf,
    /// One test binary's `--list`.
    Listing,
    /// The Python CI tests (`tests/ci`).
    Python,
}

/// The median wall time of a whole fast gate run on each platform, in seconds: Linux from 159
/// runs on the 24-thread gate server, macOS from 16 and Windows from 23 native runs (gate
/// records to 2026-11-19). The records hold whole runs only, so a child's median is taken as a
/// share of its platform's whole run ([`STAGE_SHARE_PCT`]).
pub const RUN_MEDIAN_SECS: &[(&str, u64)] = &[("linux", 3290), ("macos", 4152), ("windows", 8767)];

/// The share of a whole run, in percent, taken as one child's median. No child has been timed
/// alone yet, and a deadline that kills a slow but healthy child costs more than one that waits
/// too long for a hung one, so until per-stage times are measured every kind is 50: its deadline
/// (twice the median) is its platform's whole-run median. A hung child is still caught well
/// before the CI job timeout. Measured times replace these values per kind; a kind not listed
/// gets the floor.
pub const STAGE_SHARE_PCT: &[(StageKind, u64)] = &[
    (StageKind::Fetch, 50),
    (StageKind::Build, 50),
    (StageKind::Fmt, 50),
    (StageKind::Clippy, 50),
    (StageKind::WorkspaceTests, 50),
    (StageKind::Discover, 50),
    (StageKind::Area, 50),
    (StageKind::Lib, 50),
    (StageKind::Perf, 50),
    (StageKind::Listing, 50),
    (StageKind::Python, 50),
];

/// The default deadline of a child of `kind` on `os` (`std::env::consts::OS`): twice its median
/// ([`RUN_MEDIAN_SECS`] times [`STAGE_SHARE_PCT`]), never under [`DEADLINE_FLOOR`]. An OS with
/// no record takes the slowest platform's median.
pub fn default_deadline(kind: StageKind, os: &str) -> Duration {
    let run = RUN_MEDIAN_SECS
        .iter()
        .find(|(o, _)| *o == os)
        .or_else(|| RUN_MEDIAN_SECS.iter().max_by_key(|(_, s)| *s))
        .map_or(0, |(_, s)| *s);
    let share = STAGE_SHARE_PCT
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(0, |(_, p)| *p);
    Duration::from_secs(2 * run * share / 100).max(DEADLINE_FLOOR)
}

/// The report line of a child that passed its deadline.
pub fn deadline_line(stage: &str, deadline: Duration) -> String {
    let secs = deadline.as_secs_f64();
    let text = if secs.fract() == 0.0 {
        format!("{}s", deadline.as_secs())
    } else {
        format!("{secs:.1}s")
    };
    format!("deadline {text} passed in stage {stage}")
}

/// The most bytes the gate keeps of one stream of one child. The tail is kept, because a test
/// run ends with its `test result:` lines.
pub const MAX_CHILD_BYTES: usize = 32 * 1024 * 1024;

/// What a child process did. The two streams stay apart, so a line keeps the stream it came from.
#[derive(Clone, Debug, Default)]
pub struct ChildOutput {
    pub ok: bool,
    /// The exit code; none when the process did not start or a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The bytes dropped from the front of the two streams, over `MAX_CHILD_BYTES`.
    pub dropped: usize,
    /// The child passed its deadline and was ended.
    pub timed_out: bool,
}

impl ChildOutput {
    /// `stdout`, then `stderr`: the text the report parser reads.
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The exit status as the report shows it.
    pub fn status_text(&self) -> String {
        match (self.ok, self.code) {
            (_, Some(c)) => format!("exit {c}"),
            (true, None) => "exit 0".into(),
            (false, None) => "no exit code".into(),
        }
    }
}

/// Reads `r` to its end and keeps the last `cap` bytes; returns them and the count dropped. It
/// keeps reading after the cap, so the child never blocks on a full pipe.
pub fn read_tail(mut r: impl Read, cap: usize) -> (Vec<u8>, usize) {
    let mut buf: Vec<u8> = Vec::new();
    let mut dropped = 0;
    let mut chunk = [0u8; 65536];
    loop {
        match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > cap.saturating_mul(2) {
                    let cut = buf.len() - cap;
                    buf.drain(..cut);
                    dropped += cut;
                }
            }
        }
    }
    if buf.len() > cap {
        let cut = buf.len() - cap;
        buf.drain(..cut);
        dropped += cut;
    }
    (buf, dropped)
}

/// Runs `cmd` with both streams captured and bounded, and no deadline. A process that does not
/// start is a failed child whose `stderr` says why.
pub fn run_bounded(cmd: Command) -> ChildOutput {
    run_child(cmd, None, KILL_GRACE)
}

/// Runs `cmd` for the stage `stage` as [`run_bounded`] does, under `deadline`. Past it, the
/// child's process group gets TERM, then KILL after [`KILL_GRACE`]; the result is a failure whose
/// last `stderr` line is [`deadline_line`], which is also printed.
pub fn run_stage(stage: &str, cmd: Command, deadline: Duration) -> ChildOutput {
    let mut out = run_child(cmd, Some(deadline), KILL_GRACE);
    if out.timed_out {
        let line = deadline_line(stage, deadline);
        eprintln!("{line}");
        if !out.stderr.is_empty() && !out.stderr.ends_with('\n') {
            out.stderr.push('\n');
        }
        out.stderr.push_str(&line);
        out.stderr.push('\n');
    }
    out
}

/// What the workers of one child run report.
enum ChildEvent {
    Stdout((Vec<u8>, usize)),
    Stderr((Vec<u8>, usize)),
    Exited(std::io::Result<ExitStatus>),
}

/// The bounded runner: the child starts in a process group of its own (a Job Object on
/// Windows), so a stop reaches what it started and nothing else. With a deadline, the whole run
/// (exit and both streams) must end by then; past it the group is stopped ([`ProcessGroup::stop`]:
/// TERM, `grace`, KILL) and the workers get `grace` more to finish. A stream whose worker does
/// not finish (a process outside the group holds it) is left empty.
fn run_child(mut cmd: Command, deadline: Option<Duration>, grace: Duration) -> ChildOutput {
    let failed = |e: std::io::Error| ChildOutput {
        stderr: format!("could not run: {e}"),
        ..ChildOutput::default()
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let group = match ProcessGroup::new() {
        Ok(g) => g,
        Err(e) => return failed(e),
    };
    let mut child = match group.spawn(&mut cmd) {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel();
    let workers = [
        {
            let tx = tx.clone();
            std::thread::Builder::new().spawn(move || {
                let _ = tx.send(ChildEvent::Stdout(read_tail(out, MAX_CHILD_BYTES)));
            })
        },
        {
            let tx = tx.clone();
            std::thread::Builder::new().spawn(move || {
                let _ = tx.send(ChildEvent::Stderr(read_tail(err, MAX_CHILD_BYTES)));
            })
        },
        std::thread::Builder::new().spawn(move || {
            let _ = tx.send(ChildEvent::Exited(child.wait()));
        }),
    ];
    let expected = workers.iter().filter(|w| w.is_ok()).count();
    if let Some(Err(e)) = workers.into_iter().find(|w| w.is_err()) {
        let _ = group.kill();
        return failed(e);
    }
    let until = deadline.and_then(|d| Instant::now().checked_add(d));
    let mut stdout = (Vec::new(), 0);
    let mut stderr = (Vec::new(), 0);
    let mut status = None;
    let mut timed_out = false;
    let mut received = 0;
    while received < expected {
        let event = match (until, timed_out) {
            (Some(at), false) => {
                match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(e) => e,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        timed_out = true;
                        let _ = group.stop(grace);
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            (_, true) => match rx.recv_timeout(grace) {
                Ok(e) => e,
                Err(_) => break,
            },
            (None, false) => match rx.recv() {
                Ok(e) => e,
                Err(_) => break,
            },
        };
        received += 1;
        match event {
            ChildEvent::Stdout(o) => stdout = o,
            ChildEvent::Stderr(e) => stderr = e,
            ChildEvent::Exited(s) => status = Some(s),
        }
    }
    let status = status.and_then(Result::ok);
    ChildOutput {
        ok: !timed_out && status.is_some_and(|s| s.success()),
        code: status.and_then(|s| s.code()),
        stdout: String::from_utf8_lossy(&stdout.0).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.0).into_owned(),
        dropped: stdout.1 + stderr.1,
        timed_out,
    }
}

/// The name of the gate's executable after it steps aside: `qa-gate.running-<pid>.exe`.
pub fn aside_name(pid: u32) -> String {
    format!("qa-gate.running-{pid}.exe")
}

/// Whether `name` is an executable that an earlier gate moved aside.
pub fn is_aside_name(name: &str) -> bool {
    name.strip_prefix("qa-gate.running-")
        .and_then(|r| r.strip_suffix(".exe"))
        .is_some_and(|pid| !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()))
}

/// Moves the running gate out of Cargo's way. Windows cannot delete a running executable, and the
/// test builds relink `target/release/qa-gate.exe`, so they would all fail with "Access is
/// denied". It can rename one, so the gate renames itself and Cargo writes a new file. The old
/// files of earlier gates are removed when they are no longer running. Other systems replace a
/// running file without trouble and need none of this.
pub fn step_aside() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().expect("an executable has a directory");
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if is_aside_name(&e.file_name().to_string_lossy()) {
                // fails while that gate still runs; it is removed by a later gate
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    std::fs::rename(&exe, dir.join(aside_name(std::process::id())))
}

/// One test executable that Cargo built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestExe {
    /// The test target's name: the directory of `tests/<name>/main.rs`.
    pub name: String,
    pub path: PathBuf,
}

/// The command that builds every `rdy-qa` test and prints one JSON record per artifact.
pub fn discover_command() -> Command {
    cargo(&[
        "test",
        "-p",
        "rdy-qa",
        "--release",
        "--no-run",
        "--message-format=json",
    ])
}

/// The integration-test executables in Cargo's `--message-format=json` output, by target name.
/// A line that is not a record, a unit-test binary and a library are not listed.
pub fn parse_artifacts(json_lines: &str) -> Vec<TestExe> {
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for line in json_lines.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] != true {
            continue;
        }
        let is_test = v["target"]["kind"]
            .as_array()
            .is_some_and(|k| k.iter().any(|k| k == "test"));
        if let (true, Some(name), Some(path)) = (
            is_test,
            v["target"]["name"].as_str(),
            v["executable"].as_str(),
        ) {
            found.insert(name.to_string(), PathBuf::from(path));
        }
    }
    found
        .into_iter()
        .map(|(name, path)| TestExe { name, path })
        .collect()
}

/// The test executables of a finished discovery, or why there are none: a non-zero exit of Cargo
/// is an error that names its status and its last error lines.
pub fn discovered(out: &ChildOutput) -> Result<Vec<TestExe>, String> {
    if !out.ok {
        return Err(format!(
            "cargo test --no-run exited with {}: {}",
            out.status_text(),
            report::tail(&out.stderr, 5)
        ));
    }
    Ok(parse_artifacts(&out.stdout))
}

/// The command that lists the tests of one executable. The program is the executable itself.
pub fn list_command(exe: &Path, dir: &Path) -> Command {
    let mut c = Command::new(exe);
    c.arg("--list").current_dir(dir);
    c
}

/// The guarded defect ids of every listed executable, and one line for each executable whose
/// listing failed (its name and exit status).
pub fn list_guarded(
    exes: &[TestExe],
    dir: &Path,
    mut run: impl FnMut(Command) -> ChildOutput,
) -> (BTreeMap<String, BTreeSet<String>>, Vec<String>) {
    let mut guarded = BTreeMap::new();
    let mut problems = vec![];
    for exe in exes {
        let out = run(list_command(&exe.path, dir));
        if out.ok {
            report::add_guarded_listing(&mut guarded, &exe.name, &out.stdout);
        } else {
            problems.push(format!(
                "`{}` listing exited with {}: {}",
                exe.name,
                out.status_text(),
                report::tail(&out.combined(), 5)
            ));
        }
    }
    (guarded, problems)
}

pub fn cargo(args: &[&str]) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.args(args);
    c
}

/// The area binaries that run alone: no other area runs while one of them runs. Each one's tests
/// load the machine in a way that slows a neighbour's timing-sensitive tests or is slowed by it:
/// - `cli`: most files start the release `rdy` binary or a daemon, with waits on its endpoint;
/// - `daemon`: nearly every file starts a daemon and times its endpoints, stops and restarts;
/// - `io`: crash, fsync and reindex runs, and follower and migration child processes;
/// - `mcp`: most files start a daemon or the HTTP MCP surface, with the largest payloads;
/// - `release`: runs Cargo itself, packages and installs the release binary;
/// - `store`: crash, reopen and fsync runs over many stores (disk-heavy);
/// - `swarm`: many concurrent agents against one store, with kills and restarts.
pub const SERIAL_AREAS: &[&str] = &["cli", "daemon", "io", "mcp", "release", "store", "swarm"];

/// The area binaries launched directly when [`DIRECT_AREAS_VAR`] is not set: the pilot.
pub const PILOT_AREAS: &[&str] = &["model", "schema"];

/// Names the area binaries the gate launches directly: `all`, `none`, or a comma list. Unset, it
/// is [`PILOT_AREAS`]. Every other area runs through `cargo test`, alone, as before.
pub const DIRECT_AREAS_VAR: &str = "RDY_QA_DIRECT_AREAS";

/// The area binaries of `files` to launch directly, for the value of [`DIRECT_AREAS_VAR`].
pub fn direct_areas(value: Option<&str>, files: &[String]) -> BTreeSet<String> {
    let chosen: Vec<&str> = match value.map(str::trim) {
        None => PILOT_AREAS.to_vec(),
        Some("all") => files.iter().map(String::as_str).collect(),
        Some(v) => v.split(',').map(str::trim).collect(),
    };
    files
        .iter()
        .filter(|f| chosen.contains(&f.as_str()))
        .cloned()
        .collect()
}

/// How many area binaries run at once on a machine with `cpus` threads: a quarter of them, from
/// 1 to 4. Each binary already runs its tests on every thread.
pub fn area_jobs(cpus: usize) -> usize {
    (cpus / 4).clamp(1, 4)
}

/// Runs `run` on every item. The items for which `alone` is false share up to `jobs` workers;
/// after them, each item for which `alone` is true runs by itself, so it never overlaps another.
/// The results come back in the order of `items`.
pub fn run_areas<T: Sync, R: Send>(
    items: &[T],
    alone: impl Fn(&T) -> bool,
    jobs: usize,
    run: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let shared: Vec<usize> = (0..items.len()).filter(|&i| !alone(&items[i])).collect();
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(shared.len()) {
            s.spawn(|| {
                while let Some(&i) = shared.get(next.fetch_add(1, Ordering::SeqCst)) {
                    let r = run(&items[i]);
                    done.lock().unwrap_or_else(|p| p.into_inner()).push((i, r));
                }
            });
        }
    });
    let mut results: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in done.into_inner().unwrap_or_else(|p| p.into_inner()) {
        results[i] = Some(r);
    }
    for (i, item) in items.iter().enumerate() {
        if alone(item) {
            results[i] = Some(run(item));
        }
    }
    results
        .into_iter()
        .map(|r| r.expect("every item ran"))
        .collect()
}

/// The working directory and the whole environment that `cargo test` gives an `rdy-qa` test
/// binary. Cargo gives every test target of a package the same ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AreaEnv {
    pub cwd: PathBuf,
    pub vars: BTreeMap<String, String>,
}

/// One area binary run by [`launch_area`].
#[derive(Clone, Debug)]
pub struct AreaRun {
    pub out: ChildOutput,
    pub elapsed: Duration,
}

/// The command that starts the test binary `bin` with `args`, in `env`'s working directory, with
/// `env`'s variables and no others.
pub fn area_command(bin: &Path, env: &AreaEnv, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args)
        .current_dir(&env.cwd)
        .env_clear()
        .envs(&env.vars);
    c
}

/// Starts the test binary `bin` directly, as `cargo test` would ([`area_command`]), for the stage
/// `stage` under `deadline` ([`run_stage`]).
pub fn launch_area(
    bin: &Path,
    env: &AreaEnv,
    args: &[&str],
    stage: &str,
    deadline: Duration,
) -> AreaRun {
    let start = Instant::now();
    let out = run_stage(stage, area_command(bin, env, args), deadline);
    AreaRun {
        out,
        elapsed: start.elapsed(),
    }
}

/// Names the file the env probe test writes its record to.
pub const ENV_PROBE_VAR: &str = "RDY_QA_ENV_PROBE";
/// The area binary that holds the env probe test.
pub const ENV_PROBE_BINARY: &str = "model";
/// The env probe test's full name in [`ENV_PROBE_BINARY`].
pub const ENV_PROBE_TEST: &str = "env_probe::env_probe_dump";

/// The env probe's record of this process: its working directory and every variable, as JSON.
/// A name or value that is not UTF-8 makes it an error record.
pub fn env_probe_record() -> String {
    let record = (|| {
        let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
        let cwd = cwd
            .to_str()
            .ok_or("the working directory is not UTF-8")?
            .to_string();
        let mut vars = serde_json::Map::new();
        for (k, v) in std::env::vars_os() {
            let (Some(k), Some(v)) = (k.to_str(), v.to_str()) else {
                return Err(format!("the variable {k:?} is not UTF-8"));
            };
            vars.insert(k.to_string(), v.into());
        }
        Ok(serde_json::json!({ "cwd": cwd, "vars": vars }))
    })();
    match record {
        Ok(v) => v.to_string(),
        Err(e) => serde_json::json!({ "error": e }).to_string(),
    }
}

/// The [`AreaEnv`] in an env probe record, without [`ENV_PROBE_VAR`].
pub fn parse_env_probe(text: &str) -> Result<AreaEnv, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("env probe record: {e}"))?;
    if let Some(e) = v["error"].as_str() {
        return Err(format!("env probe: {e}"));
    }
    let cwd = v["cwd"].as_str().ok_or("env probe record has no cwd")?;
    let mut vars = BTreeMap::new();
    for (k, val) in v["vars"]
        .as_object()
        .ok_or("env probe record has no vars")?
    {
        let val = val.as_str().ok_or("env probe value is not a string")?;
        vars.insert(k.clone(), val.to_string());
    }
    vars.remove(ENV_PROBE_VAR);
    Ok(AreaEnv {
        cwd: PathBuf::from(cwd),
        vars,
    })
}

/// The `cargo test` arguments that run the env probe test alone.
pub const ENV_PROBE_ARGS: [&str; 9] = [
    "test",
    "-p",
    "rdy-qa",
    "--release",
    "--test",
    ENV_PROBE_BINARY,
    "--",
    ENV_PROBE_TEST,
    "--exact",
];

/// Asks Cargo for the environment it gives an `rdy-qa` test binary: runs the env probe test
/// through `run` (the command in, its output back; the caller sets the working directory) and
/// reads its record.
pub fn capture_area_env(run: impl FnOnce(Command) -> ChildOutput) -> Result<AreaEnv, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("env probe: {e}"))?;
    let file = dir.path().join("env.json");
    let mut c = cargo(&ENV_PROBE_ARGS);
    c.env(ENV_PROBE_VAR, &file);
    let out = run(c);
    if !out.ok {
        return Err(format!(
            "env probe exited with {}: {}",
            out.status_text(),
            report::tail(&out.combined(), 5)
        ));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("env probe wrote no record ({e}): no test matched?"))?;
    parse_env_probe(&text)
}

pub fn fail_tail(ok: bool, out: &str) -> String {
    if ok {
        String::new()
    } else {
        report::tail(out, 20)
    }
}

/// The prerequisite steps: the locked fetch of every target's crates, then the release build,
/// which needs it.
pub fn prerequisite_steps(os: &str) -> [GateStep; 2] {
    [
        GateStep::new(
            "cargo fetch --locked",
            &[],
            default_deadline(StageKind::Fetch, os),
        ),
        GateStep::new(
            "cargo build --release -p rdy-cli",
            &["cargo fetch --locked"],
            default_deadline(StageKind::Build, os),
        ),
    ]
}

/// Runs the prerequisite steps ([`prerequisite_steps`]) through `run` (label, command and
/// deadline in, success and output back). A failed step is a failed gate row, and its output goes
/// to stderr; a failed fetch skips the build unless `stages` runs every stage.
pub fn build_steps(
    stages: &mut Stages,
    mut run: impl FnMut(&str, Command, Duration) -> (bool, String),
) -> Vec<Gate> {
    let commands: [(&str, &[&str]); 2] = [
        ("fetch locked dependencies", &["fetch", "--locked"]),
        ("build rdy-cli", &["build", "--release", "-p", "rdy-cli"]),
    ];
    let mut gates = Vec::with_capacity(commands.len());
    for (step, (label, args)) in prerequisite_steps(std::env::consts::OS)
        .iter()
        .zip(commands)
    {
        if let Some(root) = stages.blocked(step) {
            gates.push(stages.skip(step, root));
            continue;
        }
        let (ok, out) = run(label, cargo(args), step.deadline);
        if !ok {
            eprintln!("{out}");
        }
        stages.record(step, ok);
        gates.push(Gate::ran(&step.id, ok, fail_tail(ok, &out)));
    }
    gates
}

/// The gate row of the Python CI tests (`tests/ci`).
pub const PYTHON_CI_GATE: &str = "python ci tests";

/// The `FAIL:` and `ERROR:` lines of `unittest` output, one per failing test, in order.
pub fn python_failures(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim_end)
        .filter(|l| l.starts_with("FAIL: ") || l.starts_with("ERROR: "))
        .map(String::from)
        .collect()
}

/// Runs the Python CI tests (`python3 -m unittest discover -s tests/ci`, from the repo root)
/// through `run` (label, command and deadline in, success and output back). A missing `python3`
/// or PyYAML is a failed row that says what is missing, never a pass. A failed run lists the
/// failing tests. On Windows the stage does not run (the Linux `policy-tests` job runs these
/// tests) and the error is the report line that says so.
pub fn python_ci_step(
    os: &str,
    mut run: impl FnMut(&str, Command, Duration) -> (bool, String),
) -> Result<Gate, String> {
    if os == "windows" {
        return Err(format!(
            "{PYTHON_CI_GATE}: not run on Windows; the Linux policy-tests job runs the Python CI tests"
        ));
    }
    let fail = |detail: String| Ok(Gate::ran(PYTHON_CI_GATE, false, detail));
    let deadline = default_deadline(StageKind::Python, os);
    let mut probe = Command::new("python3");
    probe.args(["-c", "import yaml"]);
    let (ok, out) = run("python3 and PyYAML", probe, deadline);
    if !ok {
        let missing = if out.contains("No module named") {
            "PyYAML is missing (python3 -m pip install -r tests/ci/requirements-release-producers.txt)"
        } else {
            "python3 is missing or does not start"
        };
        eprintln!("{out}");
        return fail(format!(
            "{missing}; the Python CI tests did not run:\n{}",
            report::tail(&out, 5)
        ));
    }
    let mut tests = Command::new("python3");
    tests.args(["-m", "unittest", "discover", "-s", "tests/ci"]);
    // no __pycache__ in the checkout: the report marks a tree with untracked files dirty
    tests.env("PYTHONDONTWRITEBYTECODE", "1");
    let (ok, out) = run("python ci tests", tests, deadline);
    if ok {
        return Ok(Gate::ran(PYTHON_CI_GATE, true, String::new()));
    }
    eprintln!("{out}");
    let failures = python_failures(&out);
    let detail = if failures.is_empty() {
        format!("unittest failed:\n{}", report::tail(&out, 20))
    } else {
        failures.join("\n")
    };
    fail(detail)
}

/// The gate row of the format check. The row keeps the name of the one command it replaced.
pub const FMT_GATE: &str = "cargo fmt --all --check";

/// The names of the workspace members in `cargo metadata --no-deps --format-version 1` output,
/// sorted. Lines that are not the metadata record (a warning on stderr) are skipped.
pub fn workspace_members(out: &str) -> Result<Vec<String>, String> {
    let v = out
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .find(|v| v["workspace_members"].is_array())
        .ok_or_else(|| "cargo metadata printed no workspace record".to_string())?;
    let ids: BTreeSet<&str> = v["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str())
        .collect();
    let mut names: Vec<String> = v["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["id"].as_str().is_some_and(|id| ids.contains(id)))
        .filter_map(|p| p["name"].as_str().map(String::from))
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return Err("cargo metadata listed no workspace members".into());
    }
    Ok(names)
}

/// Checks the format of every workspace member through `run` (label, command and deadline in,
/// success and output back), one `cargo fmt --check -p <member>` each. `cargo fmt --all --check` passes every
/// target root of the workspace to one rustfmt command, and Windows refuses a command line over
/// 32767 characters. Every member is checked; the row fails when the member list cannot be read
/// or any member fails, and its detail names each failing member with the tail of its output.
pub fn fmt_step(mut run: impl FnMut(&str, Command, Duration) -> (bool, String)) -> Gate {
    let fail = |detail: String| Gate::ran(FMT_GATE, false, detail);
    let deadline = default_deadline(StageKind::Fmt, std::env::consts::OS);
    let (ok, out) = run(
        "workspace members",
        cargo(&["metadata", "--no-deps", "--format-version", "1"]),
        deadline,
    );
    if !ok {
        eprintln!("{out}");
        return fail(format!(
            "cargo metadata failed:\n{}",
            report::tail(&out, 20)
        ));
    }
    let members = match workspace_members(&out) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let mut failed = vec![];
    for member in &members {
        let (ok, out) = run(
            &format!("fmt {member}"),
            cargo(&["fmt", "--check", "-p", member]),
            deadline,
        );
        if !ok {
            eprintln!("{out}");
            failed.push(format!(
                "`cargo fmt --check -p {member}` failed:\n{}",
                report::tail(&out, 20)
            ));
        }
    }
    if failed.is_empty() {
        Gate::ran(FMT_GATE, true, String::new())
    } else {
        fail(failed.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(c: &Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn plz3_fetch_precedes_build() {
        let mut seen: Vec<(String, Vec<String>)> = vec![];
        let gates = build_steps(&mut Stages::new(false), |label, cmd, _| {
            seen.push((label.to_string(), args_of(&cmd)));
            (true, String::new())
        });
        let want: Vec<(String, Vec<String>)> = vec![
            (
                "fetch locked dependencies".into(),
                vec!["fetch".into(), "--locked".into()],
            ),
            (
                "build rdy-cli".into(),
                ["build", "--release", "-p", "rdy-cli"]
                    .map(String::from)
                    .to_vec(),
            ),
        ];
        assert_eq!(seen, want);
        let names: Vec<&str> = gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            ["cargo fetch --locked", "cargo build --release -p rdy-cli"]
        );
        assert!(gates.iter().all(|g| g.pass));
    }

    #[test]
    fn plz3_fetch_failure_remains_a_failed_gate() {
        // a failed fetch skips the build, which needs it
        let mut labels: Vec<String> = vec![];
        let gates = build_steps(&mut Stages::new(false), |label, _, _| {
            labels.push(label.to_string());
            (false, "locked fetch failed".to_string())
        });
        assert_eq!(labels, ["fetch locked dependencies"]);
        assert_eq!(gates.len(), 2);
        assert!(!gates[0].pass);
        assert_eq!(gates[0].detail, "locked fetch failed");
        assert!(!gates[1].pass);
        assert_eq!(
            gates[1].state,
            report::RowState::Skipped {
                after: "cargo fetch --locked".into()
            }
        );
        assert!(!report::overall(&gates, &[], true));
        // with every stage, both run
        let mut labels: Vec<String> = vec![];
        let gates = build_steps(&mut Stages::new(true), |label, _, _| {
            labels.push(label.to_string());
            if label == "fetch locked dependencies" {
                (false, "locked fetch failed".to_string())
            } else {
                (true, String::new())
            }
        });
        assert_eq!(labels, ["fetch locked dependencies", "build rdy-cli"]);
        assert!(!gates[0].pass);
        assert!(gates[1].pass);
        assert!(!report::overall(&gates, &[], true));
    }

    #[test]
    fn pci1_python_stage_runs_unittest_from_the_root() {
        let mut seen: Vec<Vec<String>> = vec![];
        let gate = python_ci_step("linux", |_, cmd, _| {
            seen.push(args_of(&cmd));
            (true, String::new())
        })
        .unwrap();
        assert!(gate.pass, "{gate:?}");
        assert_eq!(gate.name, PYTHON_CI_GATE);
        assert_eq!(
            seen.last().unwrap(),
            &["-m", "unittest", "discover", "-s", "tests/ci"].map(String::from)
        );
    }

    #[test]
    fn pci2_failing_python_run_fails_and_names_the_tests() {
        let out = "...F\n======\nFAIL: test_a (test_x.T.test_a)\n---\nERROR: test_b (test_y.U.test_b)\nRan 4 tests\nFAILED (failures=1, errors=1)\n";
        let gate = python_ci_step("linux", |label, _, _| {
            if label == "python ci tests" {
                (false, out.into())
            } else {
                (true, String::new())
            }
        })
        .unwrap();
        assert!(!gate.pass);
        assert_eq!(
            gate.detail,
            "FAIL: test_a (test_x.T.test_a)\nERROR: test_b (test_y.U.test_b)"
        );
        assert!(!report::overall(&[gate], &[], true));
    }

    #[test]
    fn pci3_missing_interpreter_or_pyyaml_fails_not_passes() {
        let mut ran_tests = false;
        let gate = python_ci_step("linux", |label, _, _| {
            ran_tests |= label == "python ci tests";
            (false, "No such file or directory (os error 2)".into())
        })
        .unwrap();
        assert!(!gate.pass && !ran_tests);
        assert!(
            gate.detail.contains("python3 is missing"),
            "{}",
            gate.detail
        );
        let gate = python_ci_step("macos", |_, _, _| {
            (false, "ModuleNotFoundError: No module named 'yaml'".into())
        })
        .unwrap();
        assert!(!gate.pass);
        assert!(gate.detail.contains("PyYAML is missing"), "{}", gate.detail);
    }

    #[test]
    fn pci4_windows_skips_with_a_reason() {
        let skip = python_ci_step("windows", |_, _, _| panic!("must not run")).unwrap_err();
        assert!(skip.contains("not run on Windows"), "{skip}");
    }

    #[test]
    fn tgs2_default_deadlines_are_twice_the_median_with_a_floor() {
        // until stages are timed, every child's deadline is its platform's whole-run median
        let whole = [("linux", 3290), ("macos", 4152), ("windows", 8767)];
        for kind in [
            StageKind::Fetch,
            StageKind::Build,
            StageKind::Fmt,
            StageKind::Clippy,
            StageKind::WorkspaceTests,
            StageKind::Discover,
            StageKind::Area,
            StageKind::Lib,
            StageKind::Perf,
            StageKind::Listing,
            StageKind::Python,
        ] {
            for (os, secs) in whole {
                assert_eq!(
                    default_deadline(kind, os),
                    Duration::from_secs(secs),
                    "{kind:?} on {os}"
                );
            }
        }
        // never under the 10 minute floor
        assert!(DEADLINE_FLOOR == Duration::from_secs(600));
        // an OS with no record takes the slowest platform's
        assert_eq!(
            default_deadline(StageKind::Build, "freebsd"),
            default_deadline(StageKind::Build, "windows")
        );
        assert_eq!(
            deadline_line("rdy-qa swarm", Duration::from_secs(1)),
            "deadline 1s passed in stage rdy-qa swarm"
        );
    }

    const METADATA: &str = r#"{"packages":[{"name":"rdy-store","id":"path+file:///w/crates/rdy-store#0.1.0"},{"name":"serde","id":"registry+https://x#serde@1.0.0"},{"name":"rdy-qa","id":"path+file:///w/crates/rdy-qa#0.1.0"}],"workspace_members":["path+file:///w/crates/rdy-store#0.1.0","path+file:///w/crates/rdy-qa#0.1.0"],"workspace_root":"/w"}"#;

    #[test]
    fn wx34_fmt_checks_one_member_at_a_time() {
        // one `cargo fmt --check -p <member>` per workspace member, so no single rustfmt command
        // carries every target root of the workspace (Windows refuses a command line over 32767
        // characters)
        let mut seen: Vec<(String, Vec<String>)> = vec![];
        let gate = fmt_step(|label, cmd, _| {
            seen.push((label.to_string(), args_of(&cmd)));
            if label == "workspace members" {
                (true, format!("{METADATA}\nwarning: a stray line\n"))
            } else {
                (true, String::new())
            }
        });
        let want: Vec<(String, Vec<String>)> = vec![
            (
                "workspace members".into(),
                ["metadata", "--no-deps", "--format-version", "1"]
                    .map(String::from)
                    .to_vec(),
            ),
            (
                "fmt rdy-qa".into(),
                ["fmt", "--check", "-p", "rdy-qa"]
                    .map(String::from)
                    .to_vec(),
            ),
            (
                "fmt rdy-store".into(),
                ["fmt", "--check", "-p", "rdy-store"]
                    .map(String::from)
                    .to_vec(),
            ),
        ];
        assert_eq!(seen, want);
        assert_eq!(gate.name, "cargo fmt --all --check");
        assert!(gate.pass);
        assert_eq!(gate.detail, "");
    }

    #[test]
    fn wx34_fmt_names_each_failing_member_and_checks_the_rest() {
        let mut labels: Vec<String> = vec![];
        let gate = fmt_step(|label, _, _| {
            labels.push(label.to_string());
            match label {
                "workspace members" => (true, METADATA.to_string()),
                "fmt rdy-qa" => (
                    false,
                    "Diff in /w/crates/rdy-qa/src/lib.rs:1:\n".to_string(),
                ),
                _ => (true, String::new()),
            }
        });
        // a failed member does not stop the others
        assert_eq!(labels, ["workspace members", "fmt rdy-qa", "fmt rdy-store"]);
        assert!(!gate.pass);
        assert!(gate.detail.contains("rdy-qa"), "{}", gate.detail);
        assert!(gate.detail.contains("Diff in"), "{}", gate.detail);
        assert!(!gate.detail.contains("rdy-store"), "{}", gate.detail);
    }

    #[test]
    fn wx34_fmt_fails_when_the_members_cannot_be_listed() {
        let mut labels: Vec<String> = vec![];
        let gate = fmt_step(|label, _, _| {
            labels.push(label.to_string());
            (false, "error: could not find Cargo.toml\n".to_string())
        });
        assert_eq!(labels, ["workspace members"]);
        assert!(!gate.pass);
        assert!(gate.detail.contains("could not find"), "{}", gate.detail);
        // an answer with no members is a failure too: the step must check something
        let gate = fmt_step(|_, _, _| (true, "not json\n".to_string()));
        assert!(!gate.pass);
        let empty = r#"{"packages":[],"workspace_members":[]}"#;
        let gate = fmt_step(|_, _, _| (true, empty.to_string()));
        assert!(!gate.pass);
    }

    const ARTIFACTS: &str = r#"{"reason":"compiler-message","message":{}}
not json at all
{"reason":"compiler-artifact","profile":{"test":true},"target":{"kind":["test"],"name":"engine_read"},"executable":"C:\\work\\target\\release\\deps\\engine_read-1a2b.exe"}
{"reason":"compiler-artifact","profile":{"test":true},"target":{"kind":["lib"],"name":"rdy_qa"},"executable":"C:\\work\\target\\release\\deps\\rdy_qa-9.exe"}
{"reason":"compiler-artifact","profile":{"test":true},"target":{"kind":["bin"],"name":"qa-gate"},"executable":"C:\\work\\target\\release\\deps\\qa_gate-3.exe"}
{"reason":"compiler-artifact","profile":{"test":false},"target":{"kind":["test"],"name":"cli"},"executable":null}
{"reason":"compiler-artifact","profile":{"test":true},"target":{"kind":["test"],"name":"cli"},"executable":"C:\\work\\target\\release\\deps\\cli-7.exe"}
{"reason":"compiler-artifact","profile":{"test":true},"target":{"kind":["test"],"name":"cli"},"executable":"C:\\work\\target\\release\\deps\\cli-7.exe"}
{"reason":"build-finished","success":true}
"#;

    fn exe(name: &str) -> TestExe {
        TestExe {
            name: name.into(),
            path: PathBuf::from(format!("/t/{name}")),
        }
    }

    fn done(ok: bool, code: i32, stdout: &str) -> ChildOutput {
        ChildOutput {
            ok,
            code: Some(code),
            stdout: stdout.into(),
            ..ChildOutput::default()
        }
    }

    #[test]
    fn wx33_gate_lists_tests_without_sh() {
        let c = list_command(Path::new("/t/engine_read-1"), Path::new("/w"));
        assert_eq!(c.get_program(), "/t/engine_read-1");
        assert_eq!(args_of(&c), ["--list"]);
        let d = discover_command();
        assert_ne!(d.get_program(), "sh");
        let args = args_of(&d);
        assert!(args.contains(&"--no-run".to_string()));
        assert!(args.contains(&"--message-format=json".to_string()));
        assert!(!args.contains(&"-c".to_string()));
        // the gate binary starts no shell
        let gate_src = include_str!("bin/qa-gate.rs");
        assert!(!gate_src.contains("Command::new(\"sh\")"));
        assert!(!gate_src.contains("\"-c\","));
        // structured discovery: tests only, by target name, duplicates once
        let found = parse_artifacts(ARTIFACTS);
        let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["cli", "engine_read"]);
        assert!(
            found[1]
                .path
                .to_string_lossy()
                .ends_with("engine_read-1a2b.exe")
        );
        // a listing keeps its binary name, and a failed child is named with its status
        let mut seen = vec![];
        let (guarded, problems) = list_guarded(
            &[exe("engine_read"), exe("daemon"), exe("cli")],
            Path::new("/w"),
            |c| {
                seen.push(c.get_program().to_string_lossy().into_owned());
                match c.get_program().to_str().unwrap() {
                    "/t/daemon" => done(false, 101, ""),
                    _ => done(true, 0, "m::ord1_a: test\r\nm::plain: test\r\n"),
                }
            },
        );
        assert_eq!(seen, ["/t/engine_read", "/t/daemon", "/t/cli"]);
        assert_eq!(guarded["engine"].iter().collect::<Vec<_>>(), ["ord1"]);
        assert_eq!(guarded["cli"].iter().collect::<Vec<_>>(), ["ord1"]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("`daemon`") && problems[0].contains("exit 101"));
        // a non-zero cargo is an error naming its status
        let mut bad = done(false, 101, "");
        bad.stderr = "error: could not compile\n".into();
        let e = discovered(&bad).unwrap_err();
        assert!(
            e.contains("exit 101") && e.contains("could not compile"),
            "{e}"
        );
        assert_eq!(discovered(&done(true, 0, ARTIFACTS)).unwrap().len(), 2);
    }

    #[test]
    fn wx33_the_running_gate_steps_aside_by_a_name_it_can_clean_up() {
        assert_eq!(aside_name(42), "qa-gate.running-42.exe");
        assert!(is_aside_name(&aside_name(42)));
        assert!(!is_aside_name("qa-gate.exe"));
        assert!(!is_aside_name("qa-gate.running-.exe"));
        assert!(!is_aside_name("qa-gate.running-4x.exe"));
        assert!(!is_aside_name("qa-perf.running-42.exe"));
    }
}
