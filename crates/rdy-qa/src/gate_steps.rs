//! The gate's prerequisite steps (the locked dependency fetch, then the release build) and its
//! structured test discovery: Cargo's artifact record names each test executable, and the gate
//! lists each one directly, with no shell.
use crate::report::{self, Gate};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// Runs `cmd` with both streams captured and bounded. A process that does not start is a failed
/// child whose `stderr` says why.
pub fn run_bounded(mut cmd: Command) -> ChildOutput {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ChildOutput {
                stderr: format!("could not run: {e}"),
                ..ChildOutput::default()
            };
        }
    };
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let (out, err) = std::thread::scope(|sc| {
        let t = sc.spawn(|| read_tail(out, MAX_CHILD_BYTES));
        let e = read_tail(err, MAX_CHILD_BYTES);
        (t.join().unwrap_or_default(), e)
    });
    let status = child.wait();
    ChildOutput {
        ok: status.as_ref().is_ok_and(|s| s.success()),
        code: status.as_ref().ok().and_then(|s| s.code()),
        stdout: String::from_utf8_lossy(&out.0).into_owned(),
        stderr: String::from_utf8_lossy(&err.0).into_owned(),
        dropped: out.1 + err.1,
    }
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

pub fn fail_tail(ok: bool, out: &str) -> String {
    if ok {
        String::new()
    } else {
        report::tail(out, 20)
    }
}

/// Runs the prerequisite steps through `run` (label and command in, success and output back):
/// the locked fetch of every target's crates, then the release build. Both always run; a failed
/// step is a failed gate row, and its output goes to stderr.
pub fn build_steps(mut run: impl FnMut(&str, Command) -> (bool, String)) -> Vec<Gate> {
    let steps: [(&str, &str, &[&str]); 2] = [
        (
            "fetch locked dependencies",
            "cargo fetch --locked",
            &["fetch", "--locked"],
        ),
        (
            "build rdy-cli",
            "cargo build --release -p rdy-cli",
            &["build", "--release", "-p", "rdy-cli"],
        ),
    ];
    let mut gates = Vec::with_capacity(steps.len());
    for (label, name, args) in steps {
        let (ok, out) = run(label, cargo(args));
        if !ok {
            eprintln!("{out}");
        }
        gates.push(Gate {
            name: name.into(),
            pass: ok,
            detail: fail_tail(ok, &out),
        });
    }
    gates
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
        let gates = build_steps(|label, cmd| {
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
        let mut labels: Vec<String> = vec![];
        let gates = build_steps(|label, _| {
            labels.push(label.to_string());
            if label == "fetch locked dependencies" {
                (false, "locked fetch failed".to_string())
            } else {
                (true, String::new())
            }
        });
        assert_eq!(labels, ["fetch locked dependencies", "build rdy-cli"]);
        assert_eq!(gates.len(), 2);
        assert!(!gates[0].pass);
        assert_eq!(gates[0].detail, "locked fetch failed");
        assert!(gates[1].pass);
        assert!(!report::overall(&gates, &[], true));
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
}
