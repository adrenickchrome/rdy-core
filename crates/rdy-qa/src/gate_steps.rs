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

/// The run's one clock: the budget of the whole run, the builds included, from one start. A
/// child's deadline is the time left on it ([`Clock::left`]). With no budget, a child has no
/// deadline.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    start: Instant,
    budget: Option<Duration>,
}

impl Clock {
    /// A clock that starts now, with `budget` for the whole run.
    pub fn new(budget: Option<Duration>) -> Self {
        Self::started(Instant::now(), budget)
    }

    /// A clock that started at `start`, with `budget` for the whole run.
    pub fn started(start: Instant, budget: Option<Duration>) -> Self {
        Self { start, budget }
    }

    /// The time left: the budget less the time since the start, never below zero. With no
    /// budget, [`UNBOUNDED`].
    pub fn left(&self) -> Duration {
        self.budget
            .map_or(UNBOUNDED, |b| b.saturating_sub(self.start.elapsed()))
    }

    /// Whether the budget is spent.
    pub fn spent(&self) -> bool {
        self.budget.is_some() && self.left().is_zero()
    }

    /// The budget in whole seconds, when there is one.
    pub fn budget_secs(&self) -> Option<u64> {
        self.budget.map(|b| b.as_secs())
    }
}

/// The time `ps` gives as a process's elapsed time, `[[dd-]hh:]mm:ss`.
pub fn parse_etime(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (days, rest) = match text.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, text),
    };
    let mut secs = 0u64;
    let parts: Vec<&str> = rest.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    for p in parts {
        secs = secs * 60 + p.parse::<u64>().ok()?;
    }
    Some(Duration::from_secs(days * 86_400 + secs))
}

/// How long ago this process started. Under `cargo qa`, Cargo builds `qa-gate` and then runs
/// it in its own process (an exec on Unix), so this is the time since `cargo qa` started, and
/// the run's clock covers that build too. None when `ps` does not say; on Windows, where Cargo
/// starts the gate as a child, the clock starts with the gate.
pub fn process_elapsed() -> Option<Duration> {
    #[cfg(unix)]
    {
        let pid = std::process::id().to_string();
        let out = Command::new("ps")
            .args(["-o", "etime=", "-p", &pid])
            .output()
            .ok()?;
        parse_etime(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The deadline of a child with no clock above it: [`run_stage`] then waits for it to end.
pub const UNBOUNDED: Duration = Duration::MAX;

/// The gate's exit code when the run's budget is spent.
pub const BUDGET_EXIT: i32 = 30;

/// The name of the report row of a spent budget.
pub const BUDGET_ROW: &str = "BUDGET";

/// The failed row of a run whose budget of `secs` seconds was spent in `stage`.
pub fn budget_row(secs: u64, stage: &str) -> Gate {
    Gate::ran(
        BUDGET_ROW,
        false,
        format!("BUDGET: {secs} s spent in stage {stage}"),
    )
}

/// The gate's exit code: [`BUDGET_EXIT`] when the budget is spent, else 0 for a passing report
/// and 1 for a failing one.
pub fn gate_exit(pass: bool, budget_spent: bool) -> i32 {
    if budget_spent {
        BUDGET_EXIT
    } else {
        report::exit_code(pass)
    }
}

/// What a child that the gate does not start returns: the budget is spent.
pub fn not_started(stage: &str) -> ChildOutput {
    ChildOutput {
        stderr: format!("{stage}: not started, the run's budget is spent\n"),
        timed_out: true,
        ..ChildOutput::default()
    }
}

/// The seconds each test binary may take on each platform: `gate-allowances.json` beside the
/// rdy-qa manifest, `{"schema": 1, "platforms": {"<os>": {"<package>/<kind>/<name>": <secs>}}}`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Allowances {
    pub platforms: BTreeMap<String, BTreeMap<String, f64>>,
}

/// How many times its allowance a binary may take before its row fails.
pub const ALLOWANCE_FACTOR: f64 = 2.0;

impl Allowances {
    /// Reads the allowances file text.
    pub fn parse(text: &str) -> Result<Allowances, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("gate-allowances.json: {e}"))?;
        if v["schema"] != 1 {
            return Err("gate-allowances.json: schema is not 1".into());
        }
        let mut platforms = BTreeMap::new();
        for (os, list) in v["platforms"]
            .as_object()
            .ok_or("gate-allowances.json: no platforms")?
        {
            let mut secs = BTreeMap::new();
            for (key, s) in list
                .as_object()
                .ok_or_else(|| format!("gate-allowances.json: {os} is not a map"))?
            {
                let s = s
                    .as_f64()
                    .filter(|s| *s > 0.0)
                    .ok_or_else(|| format!("gate-allowances.json: {os} {key} is not a time"))?;
                secs.insert(key.clone(), s);
            }
            platforms.insert(os.clone(), secs);
        }
        Ok(Allowances { platforms })
    }

    /// The failed rows of the binaries in `times` (key, seconds) that ran more than
    /// [`ALLOWANCE_FACTOR`] times their allowance on `os`, or have none; and a note when `os` has
    /// no list, so nothing was checked.
    pub fn check(&self, os: &str, times: &[(String, f64)]) -> (Vec<Gate>, Option<String>) {
        let Some(list) = self.platforms.get(os) else {
            return (
                vec![],
                Some(format!(
                    "gate-allowances.json has no `{os}` list: the binary times were not checked"
                )),
            );
        };
        let mut rows = vec![];
        for (key, secs) in times {
            match list.get(key) {
                None => rows.push(Gate::ran(
                    &format!("NO ALLOWANCE: {key}"),
                    false,
                    format!("{key} ran {secs:.1} s and has no allowance on {os}"),
                )),
                Some(a) if *secs > ALLOWANCE_FACTOR * a => rows.push(Gate::ran(
                    &format!("OVER ALLOWANCE: {key}"),
                    false,
                    format!(
                        "{key} ran {secs:.1} s, more than {ALLOWANCE_FACTOR} times its allowance of {a:.0} s on {os}"
                    ),
                )),
                Some(_) => {}
            }
        }
        (rows, None)
    }
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

/// The gate's one build ([`report::WORKSPACE_TEST_ARGS`]): it builds every test of the
/// workspace, rdy-qa's included, and prints one JSON record per artifact.
pub fn discover_command() -> Command {
    cargo(&report::WORKSPACE_TEST_ARGS)
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

/// The variables that point a test child's temporary files at `root`: `TMPDIR` (Unix) or `TEMP`
/// and `TMP` (Windows), and [`crate::platform::SHORT_TMP_VAR`], which moves the short root of the
/// daemon and swarm tests too.
pub fn tmp_root_vars(root: &Path) -> Vec<(&'static str, PathBuf)> {
    let names: &[&str] = if cfg!(windows) {
        &["TEMP", "TMP"]
    } else {
        &["TMPDIR"]
    };
    names
        .iter()
        .chain([&crate::platform::SHORT_TMP_VAR])
        .map(|n| (*n, root.to_path_buf()))
        .collect()
}

/// The checked `--tmp-root` of the gate: the directory itself, or why the gate refuses it.
pub fn check_tmp_root(dir: &Path) -> Result<PathBuf, String> {
    if !dir.is_dir() {
        return Err(format!(
            "--tmp-root {}: the directory does not exist",
            dir.display()
        ));
    }
    dir.canonicalize()
        .map_err(|e| format!("--tmp-root {}: {e}", dir.display()))
}

/// The test binaries that keep the real temp directory whatever `--tmp-root` names, as
/// (package, test target): one real-disk pass over the flush path in every run.
pub const REAL_DISK_BINARIES: &[(&str, &str)] = &[("rdy-store", "durability"), ("rdy-qa", "store")];

/// Whether the test target `name` of `package` keeps the real temp directory.
pub fn on_real_disk(package: &str, name: &str) -> bool {
    REAL_DISK_BINARIES
        .iter()
        .any(|(p, n)| *p == package && *n == name)
}

pub fn cargo(args: &[&str]) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.args(args);
    c
}

/// The area binaries that run alone: no other binary runs while one of them runs. The list is
/// empty. It was written for a disk that took about 360 flushes a second. On a RAM disk, with
/// `cli`, `daemon`, `mcp` and `swarm` in the pool of four, every test passed and the rdy-qa
/// areas took 233 s on the Mac, against 455 s with those four alone. Put a binary back on the
/// list only when a run with it in the pool fails or is slower.
pub const SERIAL_AREAS: &[&str] = &[];

/// The Cargo profile of the gate's one build (`[profile.gate]` in the root `Cargo.toml`): the
/// release profile at `opt-level = 1`, with debug assertions and overflow checks on.
pub const GATE_PROFILE: &str = "gate";

/// The gate's one build step: the `rdy` binary, every crate's tests and every rdy-qa binary.
pub const BUILD_STEP: &str = "cargo test --workspace --profile gate --no-run";

/// One test executable of the gate's build: its package, target kind (`lib`, `bin`, `test`) and
/// target name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TestBinary {
    pub package: String,
    pub kind: String,
    pub name: String,
    pub path: PathBuf,
}

impl TestBinary {
    /// `<package>/<kind>/<name>`: unique in a workspace, and the binary's name in the report.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.package, self.kind, self.name)
    }
}

/// The package name of a Cargo package id: `path+file:///w/crates/rdy-qa#0.58.0` (the name is
/// the last path segment), `path+file:///w/x#rdy-qa@0.58.0`, or the old `rdy-qa 0.58.0 (...)`.
pub fn package_name(id: &str) -> Option<&str> {
    if !id.contains("://") || id.contains(" (") {
        return id.split_whitespace().next();
    }
    let (path, frag) = id.rsplit_once('#')?;
    match frag.split_once('@') {
        Some((name, _)) => Some(name),
        None => path.rsplit('/').next(),
    }
}

/// Every test executable in Cargo's `--message-format=json` output, sorted by key: the test
/// builds of libraries, binaries and test targets. A line that is not a record is skipped.
pub fn parse_test_binaries(json_lines: &str) -> Vec<TestBinary> {
    let mut found: BTreeSet<TestBinary> = BTreeSet::new();
    for line in json_lines.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] != true {
            continue;
        }
        let (Some(id), Some(kind), Some(name), Some(path)) = (
            v["package_id"].as_str(),
            v["target"]["kind"][0].as_str(),
            v["target"]["name"].as_str(),
            v["executable"].as_str(),
        ) else {
            continue;
        };
        let Some(package) = package_name(id) else {
            continue;
        };
        found.insert(TestBinary {
            package: package.into(),
            kind: kind.into(),
            name: name.into(),
            path: PathBuf::from(path),
        });
    }
    found.into_iter().collect()
}

/// The units that a build compiled instead of finding fresh, from Cargo's JSON output, one
/// `<package> <target>` line each.
pub fn stale_units(json_lines: &str) -> Vec<String> {
    json_lines
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .filter(|v| v["reason"] == "compiler-artifact" && v["fresh"] == false)
        .map(|v| {
            format!(
                "{} {}",
                v["package_id"]
                    .as_str()
                    .and_then(package_name)
                    .unwrap_or("?"),
                v["target"]["name"].as_str().unwrap_or("?")
            )
        })
        .collect()
}

/// The rendered compiler errors in Cargo's JSON output, then its last plain lines.
pub fn build_errors(output: &str) -> String {
    let mut errors: Vec<String> = output
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .filter(|v| v["reason"] == "compiler-message" && v["message"]["level"] == "error")
        .filter_map(|v| v["message"]["rendered"].as_str().map(String::from))
        .take(5)
        .collect();
    let plain: String = output
        .lines()
        .filter(|l| !l.trim_start().starts_with('{'))
        .collect::<Vec<_>>()
        .join("\n");
    errors.push(report::tail(&plain, 10));
    errors.join("\n")
}

/// The name of the profile directory of the executable `exe`: a test runs from
/// `target/<profile>/deps/`, a binary from `target/<profile>/`.
pub fn profile_dir_of(exe: &Path) -> Option<String> {
    let dir = exe.parent()?;
    let dir = if dir.file_name().is_some_and(|n| n == "deps") {
        dir.parent()?
    } else {
        dir
    };
    Some(dir.file_name()?.to_string_lossy().into_owned())
}

/// The Cargo arguments that select the profile this process was built in: none for `debug`
/// (Cargo's default), `--release`, or `--profile <name>`. A Cargo call of a test then builds in
/// the profile, and the target directory, of the run that started it.
pub fn own_profile_args() -> Vec<String> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| profile_dir_of(&e))
        .unwrap_or_else(|| "debug".into());
    match dir.as_str() {
        "debug" => vec![],
        "release" => vec!["--release".into()],
        other => vec!["--profile".into(), other.into()],
    }
}

/// Names the directory where the probe runner writes one record per test binary.
pub const RUNNER_PROBE_VAR: &str = "RDY_QA_RUNNER_PROBE";

/// The name of the probe runner, built beside the gate.
pub const RUNNER_PROBE_BIN: &str = "qa-env-probe";

/// Cargo's runner variable for the target `triple`: `CARGO_TARGET_<TRIPLE>_RUNNER`.
pub fn runner_var(triple: &str) -> String {
    format!(
        "CARGO_TARGET_{}_RUNNER",
        triple.to_ascii_uppercase().replace(['-', '.'], "_")
    )
}

/// The host triple in `rustc -vV` output.
pub fn host_of(rustc_vv: &str) -> Option<&str> {
    rustc_vv
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::trim)
}

/// The probe runner's record of one test binary: the binary, and [`env_probe_record`]'s
/// working directory and variables.
pub fn runner_record(exe: &str) -> String {
    let mut v: serde_json::Value =
        serde_json::from_str(&env_probe_record()).expect("the env probe record is JSON");
    v["exe"] = exe.into();
    v.to_string()
}

/// The command that runs every test target of the gate's build once through the probe runner
/// `probe`, which writes each binary's record into `dir`. It builds nothing new: the selection is
/// the build's, in the profile this process was built in (the gate profile under `cargo qa`).
pub fn probe_command(probe: &Path, dir: &Path, triple: &str) -> Command {
    let mut args: Vec<String> = ["test", "--workspace"].map(String::from).to_vec();
    args.extend(own_profile_args());
    args.extend(["--locked", "--no-fail-fast", "--lib", "--bins", "--tests"].map(String::from));
    let mut c = cargo(&args.iter().map(String::as_str).collect::<Vec<_>>());
    c.env(runner_var(triple), probe).env(RUNNER_PROBE_VAR, dir);
    c
}

/// The records in `dir`, by test binary, each without the probe's own two variables.
pub fn read_runner_records(dir: &Path, triple: &str) -> Result<BTreeMap<PathBuf, AreaEnv>, String> {
    let mut envs = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("env probe records: {e}"))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("env probe records: {e}"))?.path();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("env probe record {}: {e}", path.display()))?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("env probe record {}: {e}", path.display()))?;
        let exe = v["exe"]
            .as_str()
            .ok_or_else(|| format!("env probe record {} names no binary", path.display()))?;
        let mut env = parse_env_probe(&text)?;
        env.vars.remove(RUNNER_PROBE_VAR);
        env.vars.remove(&runner_var(triple));
        envs.insert(PathBuf::from(exe), env);
    }
    if envs.is_empty() {
        return Err("the env probe recorded no test binary".into());
    }
    Ok(envs)
}

/// Asks Cargo for the working directory and environment it gives each test binary of the gate's
/// build: Cargo runs every test target through the probe runner (built beside this gate), which
/// records them. `run` runs the command (the caller sets the working directory).
pub fn probe_envs(
    run: impl FnOnce(Command) -> ChildOutput,
) -> Result<BTreeMap<PathBuf, AreaEnv>, String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let vv = Command::new(rustc)
        .arg("-vV")
        .output()
        .map_err(|e| format!("rustc -vV: {e}"))?;
    let vv = String::from_utf8_lossy(&vv.stdout).into_owned();
    let triple = host_of(&vv).ok_or("rustc -vV names no host")?.to_string();
    let exe = std::env::current_exe().map_err(|e| format!("the gate's own path: {e}"))?;
    // the profile directory: the gate's own, or a test's (`target/<profile>/deps/`)
    let mut dir = exe.parent().ok_or("the gate has no directory")?;
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir = dir.parent().ok_or("the test has no profile directory")?;
    }
    let probe = dir.join(format!(
        "{RUNNER_PROBE_BIN}{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !probe.is_file() {
        return Err(format!("the probe runner {} is missing", probe.display()));
    }
    let dir = tempfile::tempdir().map_err(|e| format!("env probe: {e}"))?;
    let out = run(probe_command(&probe, dir.path(), &triple));
    if !out.ok {
        return Err(format!(
            "the env probe exited with {}: {}",
            out.status_text(),
            report::tail(&out.combined(), 5)
        ));
    }
    read_runner_records(dir.path(), &triple)
}

/// How many test binaries run at once. Each binary already runs its tests on every thread.
pub const BINARY_JOBS: usize = 4;

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

/// The `cargo test` arguments that run the env probe test alone, in the profile this process
/// was built in ([`own_profile_args`]). The selection is `--workspace`, as in the gate's build: a
/// selection of rdy-qa alone resolves other features and builds rdy-qa again.
pub fn env_probe_args() -> Vec<String> {
    let mut args: Vec<String> = ["test", "--workspace"].map(String::from).to_vec();
    args.extend(own_profile_args());
    args.extend(["--test", ENV_PROBE_BINARY, "--", ENV_PROBE_TEST, "--exact"].map(String::from));
    args
}

pub fn fail_tail(ok: bool, out: &str) -> String {
    if ok {
        String::new()
    } else {
        report::tail(out, 20)
    }
}

/// The prerequisite steps: the locked fetch of every target's crates, then the one build, which
/// needs it.
pub fn prerequisite_steps() -> [GateStep; 2] {
    [
        GateStep::new("cargo fetch --locked", &[], Duration::ZERO),
        GateStep::new(BUILD_STEP, &["cargo fetch --locked"], Duration::ZERO),
    ]
}

/// Runs the prerequisite steps ([`prerequisite_steps`]) through `run` (label, command and
/// deadline in, success and output back), and returns their rows and the build's output (Cargo's
/// JSON artifact records). A failed step is a failed gate row, and its errors go to stderr; a
/// failed fetch skips the build unless `stages` runs every stage.
pub fn build_steps(
    stages: &mut Stages,
    mut run: impl FnMut(&str, Command, Duration) -> (bool, String),
) -> (Vec<Gate>, String) {
    let [fetch, build] = prerequisite_steps();
    let mut gates = Vec::with_capacity(2);
    let mut build_out = String::new();
    for (step, label, cmd) in [
        (
            &fetch,
            "fetch locked dependencies",
            cargo(&["fetch", "--locked"]),
        ),
        (&build, "build", cargo(&report::WORKSPACE_TEST_ARGS)),
    ] {
        if let Some(root) = stages.blocked(step) {
            gates.push(stages.skip(step, root));
            continue;
        }
        let (ok, out) = run(label, cmd, step.deadline);
        let detail = if ok {
            String::new()
        } else if step.id == BUILD_STEP {
            build_errors(&out)
        } else {
            report::tail(&out, 20)
        };
        if !ok {
            eprintln!("{detail}");
        }
        stages.record(step, ok);
        gates.push(Gate::ran(&step.id, ok, detail));
        if step.id == BUILD_STEP {
            build_out = out;
        }
    }
    (gates, build_out)
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
    let deadline = UNBOUNDED;
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
    let deadline = UNBOUNDED;
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
        let (gates, out) = build_steps(&mut Stages::new(false), |label, cmd, _| {
            seen.push((label.to_string(), args_of(&cmd)));
            (true, format!("{label} output"))
        });
        let want: Vec<(String, Vec<String>)> = vec![
            (
                "fetch locked dependencies".into(),
                vec!["fetch".into(), "--locked".into()],
            ),
            (
                "build".into(),
                report::WORKSPACE_TEST_ARGS.map(String::from).to_vec(),
            ),
        ];
        assert_eq!(seen, want);
        let names: Vec<&str> = gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["cargo fetch --locked", BUILD_STEP]);
        assert!(gates.iter().all(|g| g.pass));
        // the build's output comes back for its artifact records
        assert_eq!(out, "build output");
    }

    #[test]
    fn plz3_fetch_failure_remains_a_failed_gate() {
        // a failed fetch skips the build, which needs it
        let mut labels: Vec<String> = vec![];
        let (gates, _) = build_steps(&mut Stages::new(false), |label, _, _| {
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
        let (gates, _) = build_steps(&mut Stages::new(true), |label, _, _| {
            labels.push(label.to_string());
            if label == "fetch locked dependencies" {
                (false, "locked fetch failed".to_string())
            } else {
                (true, String::new())
            }
        });
        assert_eq!(labels, ["fetch locked dependencies", "build"]);
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
        // No median sets a deadline any more: a child's deadline is the time left on the run's
        // one clock, and a run with no budget has no deadline.
        assert_eq!(Clock::new(None).left(), UNBOUNDED);
        assert!(!Clock::new(None).spent());
        // a child that sleeps past the clock is stopped, and the run exits 30 with the row
        let clock = Clock::new(Some(Duration::from_secs(1)));
        let t = Instant::now();
        let out = run_stage(
            "rdy-qa swarm",
            crate::platform::fixture_command("hang"),
            clock.left(),
        );
        assert!(out.timed_out, "{}", out.combined());
        assert!(!out.ok);
        assert!(t.elapsed() < Duration::from_secs(1) + KILL_GRACE + Duration::from_secs(5));
        assert!(clock.spent());
        assert_eq!(clock.left(), Duration::ZERO);
        let row = budget_row(clock.budget_secs().expect("a budget"), "rdy-qa swarm");
        assert_eq!(row.name, BUDGET_ROW);
        assert!(!row.pass);
        assert_eq!(row.detail, "BUDGET: 1 s spent in stage rdy-qa swarm");
        assert_eq!(gate_exit(false, clock.spent()), 30);
        assert_eq!(gate_exit(true, true), BUDGET_EXIT);
        assert_eq!(gate_exit(true, false), 0);
        assert_eq!(gate_exit(false, false), 1);
        // a child the spent clock does not start is a timed-out failure that says so
        let skipped = not_started("rdy-qa store");
        assert!(skipped.timed_out && !skipped.ok);
        assert!(
            skipped.stderr.contains("budget is spent"),
            "{}",
            skipped.stderr
        );
        assert_eq!(
            deadline_line("rdy-qa swarm", Duration::from_secs(1)),
            "deadline 1s passed in stage rdy-qa swarm"
        );
    }

    #[test]
    fn tgs4_the_clock_covers_the_alias_build() {
        assert_eq!(parse_etime("00:07"), Some(Duration::from_secs(7)));
        assert_eq!(parse_etime(" 01:02 "), Some(Duration::from_secs(62)));
        assert_eq!(parse_etime("02:00:01"), Some(Duration::from_secs(7201)));
        assert_eq!(parse_etime("1-00:00:00"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_etime("7"), None);
        assert_eq!(parse_etime("a:b"), None);
        // this test process started before this line ran
        #[cfg(unix)]
        assert!(process_elapsed().is_some());
    }

    #[test]
    fn tgs3_a_binary_past_twice_its_allowance_fails_its_row() {
        let file = r#"{"schema": 1, "platforms": {"macos": {
            "rdy-qa/test/cli": 100, "rdy-store/test/durability": 50}}}"#;
        let allowances = Allowances::parse(file).expect("parse");
        let times = vec![
            ("rdy-qa/test/cli".to_string(), 210.0),
            ("rdy-store/test/durability".to_string(), 99.0),
            ("rdy-qa/test/new".to_string(), 1.0),
        ];
        let (rows, note) = allowances.check("macos", &times);
        assert_eq!(note, None);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "OVER ALLOWANCE: rdy-qa/test/cli",
                "NO ALLOWANCE: rdy-qa/test/new"
            ]
        );
        assert!(rows.iter().all(|r| !r.pass));
        assert!(rows[0].detail.contains("210.0 s"), "{}", rows[0].detail);
        // a platform with no list checks nothing and says so
        let (rows, note) = allowances.check("linux", &times);
        assert!(rows.is_empty());
        assert!(note.expect("a note").contains("no `linux` list"));
        // a bad file is an error, never an empty list
        assert!(Allowances::parse(r#"{"schema": 2, "platforms": {}}"#).is_err());
        assert!(Allowances::parse(r#"{"schema": 1, "platforms": {"macos": {"a": 0}}}"#).is_err());
        // the file in the crate reads
        let ours = Allowances::parse(include_str!("../gate-allowances.json")).expect("ours");
        assert!(
            ours.platforms
                .values()
                .flat_map(|l| l.values())
                .all(|s| *s > 0.0)
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
    fn tgb1_the_gate_reads_every_test_binary_of_its_one_build() {
        let json = r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/rdy-store#0.58.0","profile":{"test":true},"target":{"kind":["test"],"name":"durability"},"executable":"/t/gate/deps/durability-1","fresh":true}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/rdy-store#0.58.0","profile":{"test":true},"target":{"kind":["lib"],"name":"rdy_store"},"executable":"/t/gate/deps/rdy_store-2","fresh":false}
{"reason":"compiler-artifact","package_id":"path+file:///w/x#rdy-qa@0.58.0","profile":{"test":true},"target":{"kind":["test"],"name":"cli"},"executable":"/t/gate/deps/cli-3","fresh":true}
{"reason":"compiler-artifact","package_id":"path+file:///w/crates/rdy-cli#0.58.0","profile":{"test":false},"target":{"kind":["bin"],"name":"rdy"},"executable":"/t/gate/rdy","fresh":true}
not json
"#;
        let found = parse_test_binaries(json);
        let keys: Vec<String> = found.iter().map(TestBinary::key).collect();
        assert_eq!(
            keys,
            [
                "rdy-qa/test/cli",
                "rdy-store/lib/rdy_store",
                "rdy-store/test/durability"
            ]
        );
        assert_eq!(stale_units(json), ["rdy-store rdy_store"]);
        assert_eq!(
            package_name("rdy-qa 0.58.0 (path+file:///w)"),
            Some("rdy-qa")
        );
        assert_eq!(
            runner_var("x86_64-pc-windows-msvc"),
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER"
        );
        assert_eq!(
            host_of("rustc 1.99.0\nhost: aarch64-apple-darwin\n"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            profile_dir_of(Path::new("/t/gate/deps/cli-3")).as_deref(),
            Some("gate")
        );
        assert_eq!(
            profile_dir_of(Path::new("/t/release/qa-gate")).as_deref(),
            Some("release")
        );
        // the real-disk binaries keep one pass over the flush path
        assert!(on_real_disk("rdy-store", "durability") && on_real_disk("rdy-qa", "store"));
        assert!(!on_real_disk("rdy-qa", "durability"));
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
