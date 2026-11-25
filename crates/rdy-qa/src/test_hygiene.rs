//! The test hygiene check of a focused run (`cargo qa --focus`): it reads the lines that the
//! branch adds to test code and refuses a fixed wall-clock wait and a chosen port.
//!
//! Why: most flaky tests waited a fixed time for background work, or bound a port that another
//! test or process could hold. Alone they passed; on a loaded build machine the work took longer
//! than the wait, or the port was taken, and the test failed. A test waits on a signal (a ready
//! line, a channel, a file, a condition polled under a hang guard) and binds port 0.
//!
//! What it reads: the added lines of `git diff -U0 <base>` (the working tree against the base,
//! so uncommitted lines count) and every line of an untracked file. Test code is a file under
//! `crates/*/tests/`, any file of `crates/rdy-qa/src/` (the crate is test code), and the lines of
//! another `src` file from its first `#[cfg(test)]` on. The base is `RDY_TEST_HYGIENE_BASE`, else
//! the merge base of `HEAD` and `main`.
//!
//! What it refuses on an added line:
//! - `wait`: a sleep (`sleep(`, `sleep_until(`), or a `Duration` literal under 60 s. A deadline
//!   of 60 s or more is a hang guard and passes.
//! - `port`: an address with a port that is not 0 (`127.0.0.1:8080`, `localhost:3000`,
//!   `[::1]:9`, `0.0.0.0:80`), or an `(addr, n)` pair with `n` not 0.
//!
//! The allow comment: `// test-hygiene: allow wait <reason>` (or `allow port <reason>`) on the
//! line or on the line above it. The reason is required: say why the line is a true hang guard
//! or why the port cannot be 0.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// The variable that names the base of the check.
pub const BASE_VAR: &str = "RDY_TEST_HYGIENE_BASE";

/// This file, which the check does not read.
const SELF: &str = "crates/rdy-qa/src/test_hygiene.rs";

/// The shortest deadline that is a hang guard, in milliseconds.
const HANG_GUARD_MS: u128 = 60_000;

/// What a refused line breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    Wait,
    Port,
}

impl Rule {
    fn word(self) -> &'static str {
        match self {
            Rule::Wait => "wait",
            Rule::Port => "port",
        }
    }

    fn why(self) -> &'static str {
        match self {
            Rule::Wait => {
                "a fixed wall-clock wait: wait on a signal, with a hang guard of 60 s or more"
            }
            Rule::Port => "a chosen port: bind port 0 and read the bound address back",
        }
    }
}

/// One refused line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub file: String,
    pub line: usize,
    pub rule: Rule,
    pub text: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}: {}",
            self.file,
            self.line,
            self.rule.word(),
            self.rule.why(),
            self.text.trim()
        )
    }
}

/// One accepted allow comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allowed {
    pub file: String,
    pub line: usize,
    pub rule: Rule,
    pub reason: String,
}

/// The result of the check over a set of added lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub refused: Vec<Refusal>,
    pub allowed: Vec<Allowed>,
    /// The test files read.
    pub files: usize,
}

/// The added lines of one file: its path, and each added line's number and text. `all` is the
/// file's whole text (for the allow comment on the line above, and for the `#[cfg(test)]` line).
#[derive(Clone, Debug, Default)]
pub struct Added {
    pub lines: Vec<(usize, String)>,
    pub all: Vec<String>,
}

/// Whether `path` (relative to the workspace root, `/`-separated) holds test code from line 1.
fn all_test(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["crates", "rdy-qa", "src", ..] => true,
        ["crates", _, "tests", ..] => true,
        _ => false,
    }
}

/// The first line (1-based) of `all` from which a Rust `src` file holds test code, if any.
fn test_from(all: &[String]) -> Option<usize> {
    all.iter()
        .position(|l| l.trim_start().starts_with("#[cfg(test)]"))
        .map(|i| i + 1)
}

/// The rules an allow comment in `line` permits, with their reasons. An allow with no reason
/// permits nothing.
fn allows(line: &str) -> Vec<(Rule, String)> {
    let mut out = vec![];
    let mut rest = line;
    while let Some(at) = rest.find("test-hygiene: allow ") {
        let after = &rest[at + "test-hygiene: allow ".len()..];
        let (word, reason) = after.split_once(' ').unwrap_or((after, ""));
        let rule = match word {
            "wait" => Some(Rule::Wait),
            "port" => Some(Rule::Port),
            _ => None,
        };
        let reason = reason.trim();
        if let Some(rule) = rule
            && !reason.is_empty()
        {
            out.push((rule, reason.to_string()));
        }
        rest = after;
    }
    out
}

/// `line` without a trailing `//` comment (a `//` inside a string literal is kept).
fn code_of(line: &str) -> &str {
    let mut in_str = false;
    let mut prev = '\0';
    for (i, c) in line.char_indices() {
        match c {
            '"' if prev != '\\' => in_str = !in_str,
            '/' if !in_str && prev == '/' => return &line[..i - 1],
            _ => {}
        }
        prev = if prev == '\\' && c == '\\' { '\0' } else { c };
    }
    line
}

/// The number at the start of `s`, if any.
fn leading_number(s: &str) -> Option<u128> {
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '_')
        .filter(|c| *c != '_')
        .collect();
    digits.parse().ok()
}

/// Whether `code` waits a fixed wall-clock time: a sleep, or a `Duration` literal under the hang
/// guard.
fn is_wait(code: &str) -> bool {
    if code.contains("sleep(") || code.contains("sleep_until(") {
        return true;
    }
    for (unit, ms_per) in [
        ("Duration::from_secs(", 1000.0),
        ("Duration::from_millis(", 1.0),
        ("Duration::from_micros(", 0.001),
        ("Duration::from_nanos(", 0.000_001),
        ("Duration::from_secs_f64(", 1000.0),
        ("Duration::from_secs_f32(", 1000.0),
    ] {
        let mut rest = code;
        while let Some(at) = rest.find(unit) {
            let arg = &rest[at + unit.len()..];
            let value = if unit.contains("_f") {
                let num: String = arg
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                num.parse::<f64>().ok()
            } else {
                leading_number(arg).map(|n| n as f64)
            };
            // a literal under the guard; a name or an expression is not read
            if let Some(v) = value
                && v * ms_per < HANG_GUARD_MS as f64
                && v > 0.0
            {
                return true;
            }
            rest = arg;
        }
    }
    false
}

/// Whether `code` names a chosen port: `host:n` with `n` not 0 for a loopback or any-address
/// host, or an `("host", n)` / `([a, b, c, d], n)` pair with `n` not 0.
fn is_port(code: &str) -> bool {
    for host in ["127.0.0.1:", "localhost:", "[::1]:", "0.0.0.0:", "[::]:"] {
        let mut rest = code;
        while let Some(at) = rest.find(host) {
            let after = &rest[at + host.len()..];
            if let Some(n) = leading_number(after)
                && n != 0
            {
                return true;
            }
            rest = after;
        }
    }
    // a pair: `"127.0.0.1", 8080)` or `[127, 0, 0, 1], 8080)`
    for close in ["\", ", "], "] {
        let mut rest = code;
        while let Some(at) = rest.find(close) {
            let after = &rest[at + close.len()..];
            let n = leading_number(after);
            let tail = after.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_');
            let before = &rest[..at];
            let addr_like = before.ends_with("127.0.0.1")
                || before.ends_with("localhost")
                || before.ends_with("0.0.0.0")
                || before.ends_with("::1")
                || before.ends_with("127, 0, 0, 1")
                || before.ends_with("0, 0, 0, 0");
            if addr_like
                && tail.starts_with(')')
                && let Some(n) = n
                && n != 0
            {
                return true;
            }
            rest = after;
        }
    }
    false
}

/// The check over `files` (path to added lines).
pub fn check(files: &BTreeMap<String, Added>) -> Outcome {
    let mut out = Outcome::default();
    for (path, added) in files {
        // the check's own file holds the patterns it refuses, as its test data
        if !path.ends_with(".rs") || path == SELF {
            continue;
        }
        let from = if all_test(path) {
            Some(1)
        } else if path.starts_with("crates/") && path.contains("/src/") {
            test_from(&added.all)
        } else {
            None
        };
        let Some(from) = from else { continue };
        let mut read = false;
        for (n, text) in &added.lines {
            if *n < from {
                continue;
            }
            read = true;
            let code = code_of(text);
            let mut found = vec![];
            if is_wait(code) {
                found.push(Rule::Wait);
            }
            if is_port(code) {
                found.push(Rule::Port);
            }
            if found.is_empty() {
                continue;
            }
            let above = n
                .checked_sub(2)
                .and_then(|i| added.all.get(i))
                .map(String::as_str)
                .unwrap_or("");
            let mut granted = allows(text);
            granted.extend(allows(above));
            for rule in found {
                match granted.iter().find(|(r, _)| *r == rule) {
                    Some((_, reason)) => out.allowed.push(Allowed {
                        file: path.clone(),
                        line: *n,
                        rule,
                        reason: reason.clone(),
                    }),
                    None => out.refused.push(Refusal {
                        file: path.clone(),
                        line: *n,
                        rule,
                        text: text.clone(),
                    }),
                }
            }
        }
        if read {
            out.files += 1;
        }
    }
    out
}

/// The added line numbers and texts of each file in a `git diff -U0` output.
pub fn parse_diff(diff: &str) -> BTreeMap<String, Vec<(usize, String)>> {
    let mut out: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    let mut file: Option<String> = None;
    let mut next = 0usize;
    for line in diff.lines() {
        if let Some(p) = line.strip_prefix("+++ ") {
            file = p.strip_prefix("b/").map(String::from);
            continue;
        }
        if line.starts_with("--- ") {
            continue;
        }
        if let Some(h) = line.strip_prefix("@@ ") {
            // `-a,b +c,d @@`
            next = h
                .split(' ')
                .find_map(|p| p.strip_prefix('+'))
                .and_then(|p| p.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            continue;
        }
        if let Some(text) = line.strip_prefix('+')
            && let Some(f) = &file
        {
            out.entry(f.clone())
                .or_default()
                .push((next, text.to_string()));
            next += 1;
        }
    }
    out
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !o.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// The base of the check in the repository at `root`: `RDY_TEST_HYGIENE_BASE`, else the merge
/// base of `HEAD` and `main`.
pub fn base(root: &Path) -> Result<String, String> {
    if let Ok(b) = std::env::var(BASE_VAR)
        && !b.trim().is_empty()
    {
        return Ok(b.trim().to_string());
    }
    git(root, &["merge-base", "HEAD", "main"]).map(|s| s.trim().to_string())
}

/// The added lines of test code in the repository at `root` against `base`: the diff of the
/// working tree, and every line of each untracked Rust file.
pub fn added_lines(root: &Path, base: &str) -> Result<BTreeMap<String, Added>, String> {
    let diff = git(
        root,
        &[
            "diff",
            "-U0",
            "--no-color",
            "--no-ext-diff",
            base,
            "--",
            "crates",
        ],
    )?;
    let mut lines = parse_diff(&diff);
    let untracked = git(
        root,
        &["ls-files", "--others", "--exclude-standard", "--", "crates"],
    )?;
    for f in untracked.lines().filter(|f| f.ends_with(".rs")) {
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        lines.insert(
            f.to_string(),
            text.lines()
                .enumerate()
                .map(|(i, l)| (i + 1, l.to_string()))
                .collect(),
        );
    }
    Ok(lines
        .into_iter()
        .map(|(f, lines)| {
            let all = std::fs::read_to_string(root.join(&f))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect();
            (f, Added { lines, all })
        })
        .collect())
}

/// Runs the check in the repository at `root` and prints what it found: each allow with its
/// reason, each refusal, and a summary line. Ok(true) when nothing is refused.
pub fn run(root: &Path) -> Result<bool, String> {
    let base = base(root)?;
    let files = added_lines(root, &base)?;
    let out = check(&files);
    for a in &out.allowed {
        eprintln!(
            "test hygiene: {}:{}: allowed {}: {}",
            a.file,
            a.line,
            a.rule.word(),
            a.reason
        );
    }
    for r in &out.refused {
        println!("test hygiene: {r}");
    }
    if out.refused.is_empty() {
        println!(
            "test hygiene: passed ({} changed test files against {})",
            out.files,
            &base[..base.len().min(12)]
        );
        Ok(true)
    } else {
        println!(
            "test hygiene: FAIL: {} refused lines; wait on a signal and bind port 0, or add \
             `// test-hygiene: allow <wait|port> <reason>` for a true hang guard",
            out.refused.len()
        );
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(path: &str, all: &[&str], added: &[usize]) -> BTreeMap<String, Added> {
        let all: Vec<String> = all.iter().map(|s| s.to_string()).collect();
        let lines = added.iter().map(|n| (*n, all[n - 1].clone())).collect();
        BTreeMap::from([(path.to_string(), Added { lines, all })])
    }

    const T: &str = "crates/rdy-qa/tests/daemon/x.rs";

    #[test]
    fn th1_a_new_sleep_is_refused_with_its_file_and_line() {
        let out = check(&one(
            T,
            &[
                "fn a() {",
                "    std::thread::sleep(Duration::from_millis(50));",
                "}",
            ],
            &[2],
        ));
        assert_eq!(out.refused.len(), 1, "{out:?}");
        assert_eq!(out.refused[0].line, 2);
        assert_eq!(out.refused[0].rule, Rule::Wait);
        assert!(
            out.refused[0]
                .to_string()
                .starts_with(&format!("{T}:2: wait: "))
        );
        let out = check(&one(T, &["tokio::time::sleep(d).await;"], &[1]));
        assert_eq!(out.refused.len(), 1, "{out:?}");
    }

    #[test]
    fn th1_a_short_deadline_is_refused_and_a_hang_guard_passes() {
        for short in [
            "let until = Instant::now() + Duration::from_secs(3);",
            "rx.recv_timeout(Duration::from_millis(500)).unwrap();",
            "const T: Duration = Duration::from_secs(10);",
            "let d = Duration::from_secs_f64(2.5);",
        ] {
            assert_eq!(check(&one(T, &[short], &[1])).refused.len(), 1, "{short}");
        }
        for guard in [
            "const GUARD: Duration = Duration::from_secs(60);",
            "rx.recv_timeout(Duration::from_secs(120)).unwrap();",
            "let d = Duration::from_millis(60_000);",
            "let d = Duration::from_secs(WAIT_SECS);",
            "let z = Duration::from_secs(0);",
        ] {
            assert!(check(&one(T, &[guard], &[1])).refused.is_empty(), "{guard}");
        }
    }

    #[test]
    fn th1_a_chosen_port_is_refused_and_port_0_passes() {
        for chosen in [
            "let a = \"127.0.0.1:8080\";",
            "connect(\"localhost:3000\")",
            "\"[::1]:9\".parse().unwrap()",
            "TcpListener::bind((\"127.0.0.1\", 4000))",
            "SocketAddr::from(([127, 0, 0, 1], 5555))",
        ] {
            let out = check(&one(T, &[chosen], &[1]));
            assert_eq!(out.refused.len(), 1, "{chosen}: {out:?}");
            assert_eq!(out.refused[0].rule, Rule::Port);
        }
        for free in [
            "TcpListener::bind(\"127.0.0.1:0\")",
            "TcpListener::bind((\"127.0.0.1\", 0))",
            "format!(\"http://127.0.0.1:{port}/\")",
            "SocketAddr::from(([127, 0, 0, 1], port))",
        ] {
            assert!(check(&one(T, &[free], &[1])).refused.is_empty(), "{free}");
        }
    }

    #[test]
    fn th1_an_allow_needs_a_reason() {
        let with = check(&one(
            T,
            &[
                "// test-hygiene: allow wait the child's exec cannot be observed",
                "std::thread::sleep(Duration::from_millis(20));",
            ],
            &[2],
        ));
        assert!(with.refused.is_empty(), "{with:?}");
        assert_eq!(with.allowed.len(), 1);
        assert_eq!(
            with.allowed[0].reason,
            "the child's exec cannot be observed"
        );
        let same_line = check(&one(
            T,
            &["let p = \"127.0.0.1:1\"; // test-hygiene: allow port a port no one listens on"],
            &[1],
        ));
        assert!(same_line.refused.is_empty(), "{same_line:?}");
        let bare = check(&one(
            T,
            &["// test-hygiene: allow wait", "std::thread::sleep(d);"],
            &[2],
        ));
        assert_eq!(bare.refused.len(), 1, "an allow with no reason: {bare:?}");
        let other_rule = check(&one(
            T,
            &[
                "// test-hygiene: allow port no listener",
                "std::thread::sleep(d);",
            ],
            &[2],
        ));
        assert_eq!(other_rule.refused.len(), 1, "{other_rule:?}");
    }

    #[test]
    fn th1_only_added_test_code_is_read() {
        // product code before the test module, and a comment, are not read
        let src = [
            "fn serve() { std::thread::sleep(Duration::from_millis(5)); }",
            "#[cfg(test)]",
            "mod tests {",
            "    // a sleep(…) in a comment",
            "    fn t() { std::thread::sleep(Duration::from_millis(5)); }",
            "}",
        ];
        let out = check(&one("crates/rdy-daemon/src/x.rs", &src, &[1, 4, 5]));
        assert_eq!(out.refused.len(), 1, "{out:?}");
        assert_eq!(out.refused[0].line, 5);
        // a product file with no test module, and a file that is not Rust, are not read
        assert!(
            check(&one("crates/rdy-daemon/src/y.rs", &src[..1], &[1]))
                .refused
                .is_empty()
        );
        assert!(
            check(&one("crates/rdy-qa/tests/x.py", &["time.sleep(1)"], &[1]))
                .refused
                .is_empty()
        );
        // an unchanged line of a test file is not read
        assert!(
            check(&one(T, &["std::thread::sleep(d);", "let x = 1;"], &[2]))
                .refused
                .is_empty()
        );
    }

    #[test]
    fn th1_the_diff_gives_each_added_line_its_number() {
        let diff = "diff --git a/crates/a/tests/t.rs b/crates/a/tests/t.rs\n\
                    --- a/crates/a/tests/t.rs\n\
                    +++ b/crates/a/tests/t.rs\n\
                    @@ -3,0 +4,2 @@ fn x() {\n\
                    +    one();\n\
                    +    two();\n\
                    @@ -10 +12 @@\n\
                    -old\n\
                    +new\n\
                    diff --git a/crates/b/tests/gone.rs b/crates/b/tests/gone.rs\n\
                    --- a/crates/b/tests/gone.rs\n\
                    +++ /dev/null\n\
                    @@ -1 +0,0 @@\n\
                    -x\n";
        let got = parse_diff(diff);
        assert_eq!(
            got["crates/a/tests/t.rs"],
            vec![
                (4, "    one();".to_string()),
                (5, "    two();".to_string()),
                (12, "new".to_string())
            ]
        );
        assert_eq!(got.len(), 1);
    }
}
