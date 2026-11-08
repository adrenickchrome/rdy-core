//! Unix-only and Windows-only code lives only in the seam modules (FW-XP1 D-1RXEA).
//!
//! The gate's "unix seams" step and `ps11_the_real_tree_has_no_unix_code_outside_the_seams`
//! scan every `.rs` file under `crates/*/src`, `crates/*/tests` and `crates/*/benches`, and
//! every `crates/*/build.rs`, for the tokens in [`TOKENS`] (Unix) and [`WINDOWS_TOKENS`]
//! (Windows). A file that holds a Windows token must be on an exact list: [`WINDOWS_SEAM_FILES`],
//! [`WINDOWS_TEST_FILES`] or, for the tests of one product file, [`WINDOWS_UNIT_TEST_USES`]. No
//! directory is a seam by its name. Task 32 added the Windows half.

use std::path::{Path, PathBuf};

/// The Unix-only tokens (closed list). A line whose trimmed text starts with `//` is skipped.
pub const TOKENS: &[&str] = &[
    "std::os::unix",
    "std::os::fd",
    "std::os::linux",
    "std::os::macos",
    "std::os::{",
    "libc::",
    "tokio::net::unix",
    "tokio::signal::unix",
    "UnixStream",
    "UnixListener",
    "UnixDatagram",
];

/// The seam files: any use is allowed.
pub const SEAM_FILES: &[&str] = &[
    "crates/rdy-store/src/pio.rs",
    "crates/rdy-store/src/pio/unix.rs",
    "crates/rdy-store/src/pio/windows.rs",
    "crates/rdy-store/src/pio/windows_security.rs",
    "crates/rdy-store/src/pio/windows_dir.rs",
    "crates/rdy-store/src/fileid.rs",
    "crates/rdy-store/src/fileid/windows.rs",
    "crates/rdy-daemon/src/sys/unix.rs",
    "crates/rdy-daemon/src/sys/windows.rs",
    "crates/rdy-daemon/src/sys/windows/agent.rs",
    "crates/rdy-daemon/src/sys/windows/identity.rs",
    "crates/rdy-daemon/src/sys/windows/pipe.rs",
    "crates/rdy-daemon/src/sys/windows/run_state.rs",
    "crates/rdy-qa/src/platform.rs",
    "crates/rdy-qa/src/platform/agent_wire.rs",
    "crates/rdy-qa/src/platform/child_fixture.rs",
    "crates/rdy-qa/src/platform/child_fixture_tests.rs",
    "crates/rdy-qa/src/platform/exclusions.rs",
    "crates/rdy-qa/src/platform/unix.rs",
    "crates/rdy-qa/src/platform/windows.rs",
    "crates/rdy-qa/src/platform/windows_accounts.rs",
    "crates/rdy-qa/src/platform/windows_agent.rs",
    "crates/rdy-qa/src/platform/windows_fs.rs",
    "crates/rdy-qa/src/platform/windows_process.rs",
    "crates/rdy-qa/src/platform/windows_release.rs",
    "crates/rdy-qa/src/seams.rs",
];

/// Product files whose `#[cfg(test)]` module uses Unix-only APIs: the path, the exact number of
/// lines with a token, all after the file's first `#[cfg(test)]` line, and why.
pub const UNIT_TEST_USES: &[(&str, usize, &str)] = &[
    (
        "crates/rdy-store/src/fsutil.rs",
        1,
        "a symlink at the temp name",
    ),
    (
        "crates/rdy-engine/src/wc/files.rs",
        3,
        "a symlink in a checkout",
    ),
    (
        "crates/rdy-daemon/src/layout.rs",
        9,
        "modes, owners, error numbers and symlinks of the run dir",
    ),
    (
        "crates/rdy-daemon/src/client.rs",
        1,
        "a read-only parent of the short directory",
    ),
    (
        "crates/rdy-daemon/src/site/addr.rs",
        1,
        "two spellings of one root",
    ),
    ("crates/rdy-daemon/src/socket.rs", 1, "the socket's mode"),
];

/// The Windows-only tokens (closed list). A line whose trimmed text starts with `//` is skipped.
pub const WINDOWS_TOKENS: &[&str] = &[
    "std::os::windows",
    "windows_sys",
    "windows::Win32",
    "winapi::",
    "tokio::net::windows",
    "tokio::signal::windows",
    "extern \"system\"",
    "#[link(",
];

/// The files where a Windows-only token is allowed: the Windows side of each seam, and the seam
/// files that hold a Windows branch beside a Unix one. An exact list, never a directory.
pub const WINDOWS_SEAM_FILES: &[&str] = &[
    "crates/rdy-store/src/pio.rs",
    "crates/rdy-store/src/pio/windows.rs",
    "crates/rdy-store/src/pio/windows_dir.rs",
    "crates/rdy-store/src/pio/windows_durable.rs",
    "crates/rdy-store/src/pio/windows_io.rs",
    "crates/rdy-store/src/pio/windows_security.rs",
    "crates/rdy-store/src/fileid/windows.rs",
    "crates/rdy-daemon/src/sys/mod.rs",
    "crates/rdy-daemon/src/sys/site_socket.rs",
    "crates/rdy-daemon/src/sys/windows.rs",
    "crates/rdy-daemon/src/sys/windows/agent.rs",
    "crates/rdy-daemon/src/sys/windows/identity.rs",
    "crates/rdy-daemon/src/sys/windows/pipe.rs",
    "crates/rdy-daemon/src/sys/windows/process.rs",
    "crates/rdy-daemon/src/sys/windows/run_state.rs",
    "crates/rdy-daemon/src/sys/windows/stop.rs",
    "crates/rdy-qa/src/platform.rs",
    "crates/rdy-qa/src/platform/child_fixture.rs",
    "crates/rdy-qa/src/platform/windows.rs",
    "crates/rdy-qa/src/platform/windows_accounts.rs",
    "crates/rdy-qa/src/platform/windows_agent.rs",
    "crates/rdy-qa/src/platform/windows_fs.rs",
    "crates/rdy-qa/src/platform/windows_process.rs",
    "crates/rdy-qa/src/platform/windows_release.rs",
    "crates/rdy-qa/src/seams.rs",
];

/// Test and bench files that use Windows-only APIs, each with why. They run on Windows only or
/// reach a Windows API from a branch that Windows alone compiles. A new file is not allowed until
/// it is listed here.
pub const WINDOWS_TEST_FILES: &[(&str, &str)] = &[
    (
        "crates/rdy-daemon/tests/common/agent.rs",
        "the fake agent's named pipe",
    ),
    (
        "crates/rdy-daemon/tests/windows_identity.rs",
        "error numbers of a broken pipe",
    ),
    (
        "crates/rdy-daemon/tests/windows_layout.rs",
        "short names, wide strings and links of the run directory",
    ),
    (
        "crates/rdy-daemon/tests/windows_signing.rs",
        "the signing agent's named pipe",
    ),
    (
        "crates/rdy-daemon/tests/windows_stop.rs",
        "tokens, handles and the stop event of a real process",
    ),
    (
        "crates/rdy-engine/tests/modes/mod.rs",
        "links and an exclusive open on Windows",
    ),
    (
        "crates/rdy-qa/tests/release/release_install.rs",
        "an unpaired surrogate in a token",
    ),
    (
        "crates/rdy-qa/tests/wc/wc_windows.rs",
        "the file type of a reparse point",
    ),
    (
        "crates/rdy-store/tests/follower_guard.rs",
        "links and an exclusive open on Windows",
    ),
    (
        "crates/rdy-store/tests/read_only.rs",
        "a file link in a store directory",
    ),
    (
        "crates/rdy-store/tests/store.rs",
        "an exclusive open on Windows",
    ),
    (
        "crates/rdy-store/tests/transition.rs",
        "an exclusive open on Windows",
    ),
    (
        "crates/rdy-store/tests/upgrade.rs",
        "links made with the Windows call",
    ),
    (
        "crates/rdy-store/tests/windows_durability.rs",
        "an exclusive open on Windows",
    ),
];

/// Product files whose `#[cfg(test)]` module uses Windows-only APIs: the path, the exact number of
/// lines with a token, all after the file's first `#[cfg(test)]` line, and why.
pub const WINDOWS_UNIT_TEST_USES: &[(&str, usize, &str)] = &[(
    "crates/rdy-store/src/fsutil.rs",
    1,
    "a file link at the temp name",
)];

/// Line 1 of a test or bench file that may use Unix-only APIs.
pub const GATE_LINE: &str = "#![cfg(unix)]";

/// The start of line 2 of a gated test or bench file; at least one more word follows it.
pub const REASON_PREFIX: &str = "// Unix only: ";

/// Every problem under `root` (the workspace), in path then line order; empty when clean.
pub fn seam_problems(root: &Path) -> Vec<String> {
    all_problems(root, SEAM_FILES, UNIT_TEST_USES, &WindowsRules::REAL)
}

/// The line of a problem with the whole file: after every real line.
const FILE: usize = usize::MAX;

/// A problem before sorting: (path, line, message).
type Problem = (String, usize, String);

/// The Windows half of the scan: the lists of [`WINDOWS_SEAM_FILES`], [`WINDOWS_TEST_FILES`] and
/// [`WINDOWS_UNIT_TEST_USES`].
struct WindowsRules<'a> {
    seams: &'a [&'a str],
    tests: &'a [(&'a str, &'a str)],
    unit_uses: &'a [(&'a str, usize, &'a str)],
}

impl WindowsRules<'static> {
    const REAL: WindowsRules<'static> = WindowsRules {
        seams: WINDOWS_SEAM_FILES,
        tests: WINDOWS_TEST_FILES,
        unit_uses: WINDOWS_UNIT_TEST_USES,
    };
}

#[cfg(test)]
impl WindowsRules<'_> {
    /// No file may use a Windows-only API, and none is listed.
    const NONE: WindowsRules<'static> = WindowsRules {
        seams: &[],
        tests: &[],
        unit_uses: &[],
    };
}

/// The Unix half of the scan with the seam files and the unit-test allowlist given, and no
/// Windows rule listed.
#[cfg(test)]
fn problems_with(root: &Path, seams: &[&str], unit_uses: &[(&str, usize, &str)]) -> Vec<String> {
    all_problems(root, seams, unit_uses, &WindowsRules::NONE)
}

/// Every problem of both halves, sorted.
fn all_problems(
    root: &Path,
    seams: &[&str],
    unit_uses: &[(&str, usize, &str)],
    windows: &WindowsRules<'_>,
) -> Vec<String> {
    // (path, line, message); a file's own problems have line FILE and sort after its lines.
    let (files, mut found) = rust_files(root);
    // The paths the scan could not read; a file at or under one of them is not "gone".
    let unscanned: Vec<String> = found.iter().map(|(p, _, _)| p.clone()).collect();
    for seam in seams {
        if !root.join(seam).is_file() {
            found.push((
                seam.to_string(),
                FILE,
                format!(
                    "{seam}: is a seam file in crates/rdy-qa/src/seams.rs, but it does not exist"
                ),
            ));
        }
    }
    for (path, text) in &files {
        if seams.contains(&path.as_str()) {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let uses = token_lines(&lines);
        if let Some(&(_, want, _)) = unit_uses.iter().find(|(p, _, _)| *p == path) {
            let tests_from = lines.iter().position(|l| l.trim() == "#[cfg(test)]");
            let mut in_tests = 0;
            for &(line, token) in &uses {
                if tests_from.is_some_and(|t| line > t + 1) {
                    in_tests += 1;
                } else {
                    found.push((
                        path.clone(),
                        line,
                        format!(
                            "{path}:{line}: {token} is Unix-only and outside the file's \
                             #[cfg(test)] module; only its tests may use it"
                        ),
                    ));
                }
            }
            if in_tests != want {
                found.push((path.clone(), FILE, count_message(path, in_tests, want)));
            }
            continue;
        }
        if is_test_or_bench(path) && lines.first() == Some(&GATE_LINE) {
            let reason = lines
                .get(1)
                .and_then(|l| l.strip_prefix(REASON_PREFIX))
                .is_some_and(|rest| !rest.trim().is_empty());
            if !reason {
                found.push((
                    path.clone(),
                    FILE,
                    format!(
                        "{path}: is #![cfg(unix)] with no \"// Unix only: <reason>\" line under it"
                    ),
                ));
            }
            continue;
        }
        for (line, token) in uses {
            found.push((
                path.clone(),
                line,
                format!(
                    "{path}:{line}: {token} is Unix-only; move it into a seam (rdy_store::pio, \
                     rdy_store::fileid, rdy_daemon::sys, rdy_qa::platform), or gate the test \
                     file with #![cfg(unix)] and a \"// Unix only: <reason>\" line (FW-XP1)"
                ),
            ));
        }
    }
    // An allowlisted file that is gone has no lines in its tests.
    for (path, want, _) in unit_uses {
        let blocked = is_unscanned(&unscanned, path);
        if !blocked && !files.iter().any(|(p, _)| p == path) {
            found.push((path.to_string(), FILE, count_message(path, 0, *want)));
        }
    }
    found.extend(windows_problems(&files, &unscanned, root, windows));
    found.sort();
    found.into_iter().map(|(_, _, m)| m).collect()
}

/// Whether `path` is, or is under, one of the paths the scan could not read.
fn is_unscanned(unscanned: &[String], path: &str) -> bool {
    unscanned.iter().any(|u| {
        path == u
            || path
                .strip_prefix(u.as_str())
                .is_some_and(|r| r.starts_with('/'))
    })
}

/// The problems of the Windows half. A file with a [`WINDOWS_TOKENS`] line must be a listed seam,
/// a listed test or bench file, or a listed product file whose tests alone use it. A list entry
/// that is gone, or that no longer uses a Windows token, is a problem too, so a list never
/// outlives its need.
fn windows_problems(
    files: &[(String, String)],
    unscanned: &[String],
    root: &Path,
    rules: &WindowsRules<'_>,
) -> Vec<Problem> {
    let mut found: Vec<Problem> = Vec::new();
    let text_of = |path: &str| files.iter().find(|(p, _)| p == path).map(|(_, t)| t);
    for seam in rules.seams {
        if !root.join(seam).is_file() {
            found.push((
                seam.to_string(),
                FILE,
                format!(
                    "{seam}: is a Windows seam file in crates/rdy-qa/src/seams.rs, but it does not \
                     exist"
                ),
            ));
        }
    }
    for (path, text) in files {
        if rules.seams.contains(&path.as_str()) {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let uses = token_lines_of(&lines, WINDOWS_TOKENS);
        if let Some(&(_, want, _)) = rules.unit_uses.iter().find(|(p, _, _)| *p == path) {
            let tests_from = lines.iter().position(|l| l.trim() == "#[cfg(test)]");
            let mut in_tests = 0;
            for &(line, token) in &uses {
                if tests_from.is_some_and(|t| line > t + 1) {
                    in_tests += 1;
                } else {
                    found.push((
                        path.clone(),
                        line,
                        format!(
                            "{path}:{line}: {token} is Windows-only and outside the file's \
                             #[cfg(test)] module; only its tests may use it"
                        ),
                    ));
                }
            }
            if in_tests != want {
                found.push((
                    path.clone(),
                    FILE,
                    format!(
                        "{path}: has {in_tests} Windows-only lines in its tests; \
                         WINDOWS_UNIT_TEST_USES in crates/rdy-qa/src/seams.rs says {want}"
                    ),
                ));
            }
            continue;
        }
        if is_test_or_bench(path) && rules.tests.iter().any(|(p, _)| *p == path) {
            continue;
        }
        for (line, token) in uses {
            found.push((
                path.clone(),
                line,
                format!(
                    "{path}:{line}: {token} is Windows-only; move it into a seam (rdy_store::pio, \
                     rdy_store::fileid, rdy_daemon::sys, rdy_qa::platform), or list the test file \
                     in WINDOWS_TEST_FILES in crates/rdy-qa/src/seams.rs with its reason (FW-XP1)"
                ),
            ));
        }
    }
    for (path, _) in rules.tests {
        if !is_test_or_bench(path) {
            found.push((
                path.to_string(),
                FILE,
                format!("{path}: is in WINDOWS_TEST_FILES but is not under tests/ or benches/"),
            ));
        } else if let Some(text) = text_of(path) {
            let lines: Vec<&str> = text.lines().collect();
            if token_lines_of(&lines, WINDOWS_TOKENS).is_empty() {
                found.push((
                    path.to_string(),
                    FILE,
                    format!("{path}: is in WINDOWS_TEST_FILES but uses no Windows-only API"),
                ));
            }
        } else if !is_unscanned(unscanned, path) {
            found.push((
                path.to_string(),
                FILE,
                format!("{path}: is in WINDOWS_TEST_FILES, but it does not exist"),
            ));
        }
    }
    for (path, want, _) in rules.unit_uses {
        if !is_unscanned(unscanned, path) && text_of(path).is_none() {
            found.push((
                path.to_string(),
                FILE,
                format!(
                    "{path}: has 0 Windows-only lines in its tests; WINDOWS_UNIT_TEST_USES in \
                     crates/rdy-qa/src/seams.rs says {want}"
                ),
            ));
        }
    }
    found
}

fn count_message(path: &str, n: usize, m: usize) -> String {
    format!(
        "{path}: has {n} Unix-only lines in its tests; UNIT_TEST_USES in crates/rdy-qa/src/seams.rs \
         says {m}"
    )
}

/// The 1-based number and the first token of each line that holds a token, skipping comment
/// lines.
fn token_lines(lines: &[&str]) -> Vec<(usize, &'static str)> {
    token_lines_of(lines, TOKENS)
}

/// [`token_lines`] for any closed list of tokens.
fn token_lines_of(lines: &[&str], tokens: &'static [&'static str]) -> Vec<(usize, &'static str)> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let first = tokens
            .iter()
            .filter_map(|t| line.find(t).map(|at| (at, *t)))
            .min();
        if let Some((_, token)) = first {
            out.push((i + 1, token));
        }
    }
    out
}

/// Whether `path` (`crates/<crate>/...`) is under the crate's `tests/` or `benches/`.
fn is_test_or_bench(path: &str) -> bool {
    matches!(path.split('/').nth(2), Some("tests" | "benches"))
}

/// The files of a scan and the paths it could not scan.
struct Scan<'a> {
    root: &'a Path,
    files: Vec<(String, String)>,
    /// (path, FILE, message), ready to merge with the other problems.
    problems: Vec<Problem>,
}

impl Scan<'_> {
    /// `p` from `root`, with `/` separators.
    fn name(&self, p: &Path) -> String {
        p.strip_prefix(self.root)
            .unwrap_or(p)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn failed(&mut self, p: &Path, err: &std::io::Error) {
        let name = self.name(p);
        let message = format!("{name}: cannot be scanned: {err}");
        self.problems.push((name, FILE, message));
    }

    fn read(&mut self, p: &Path) {
        match std::fs::read(p) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                self.files.push((self.name(p), text));
            }
            Err(e) => self.failed(p, &e),
        }
    }

    /// The metadata of `p` (following links); `None` and a problem for any error. A path the
    /// scan found in a directory exists, so even `NotFound` (a dangling link) is a problem.
    fn metadata(&mut self, p: &Path) -> Option<std::fs::Metadata> {
        match std::fs::metadata(p) {
            Ok(m) => Some(m),
            Err(e) => {
                self.failed(p, &e);
                None
            }
        }
    }

    /// [`Self::metadata`] for an optional path (`src`, `tests`, `benches`, `build.rs`): `None`
    /// with no problem only when nothing is there at all. A link that resolves to nothing is a
    /// problem.
    fn optional_metadata(&mut self, p: &Path) -> Option<std::fs::Metadata> {
        let absent = |r: std::io::Result<std::fs::Metadata>| {
            r.is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        };
        if absent(std::fs::metadata(p)) && absent(std::fs::symlink_metadata(p)) {
            return None;
        }
        self.metadata(p)
    }

    /// The entries of `dir`, sorted; a problem when it or one of its entries cannot be read.
    fn entries(&mut self, dir: &Path) -> Vec<PathBuf> {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                self.failed(dir, &e);
                return Vec::new();
            }
        };
        let mut v = Vec::new();
        for entry in rd {
            match entry {
                Ok(e) => v.push(e.path()),
                Err(e) => self.failed(dir, &e),
            }
        }
        v.sort();
        v
    }

    /// Every `.rs` file under `dir`. `ancestors` holds the canonical path and the name of each
    /// directory the walk is in; a directory that resolves to one of them is a cycle and a
    /// problem, so the walk always ends.
    fn walk(&mut self, dir: &Path, ancestors: &mut Vec<(PathBuf, String)>) {
        let canonical = match std::fs::canonicalize(dir) {
            Ok(c) => c,
            Err(e) => return self.failed(dir, &e),
        };
        if let Some((_, outer)) = ancestors.iter().find(|(c, _)| *c == canonical) {
            let name = self.name(dir);
            let message = format!(
                "{name}: leads back to {outer}, a directory the scan is already in; remove the link"
            );
            self.problems.push((name, FILE, message));
            return;
        }
        ancestors.push((canonical, self.name(dir)));
        for entry in self.entries(dir) {
            let Some(meta) = self.metadata(&entry) else {
                continue;
            };
            if meta.is_dir() {
                self.walk(&entry, ancestors);
            } else if meta.is_file() && entry.extension().is_some_and(|e| e == "rs") {
                self.read(&entry);
            }
        }
        ancestors.pop();
    }
}

/// Every scanned file, as (its path from `root` with `/` separators, its text), in path order:
/// each `.rs` file under `crates/*/src`, `crates/*/tests` and `crates/*/benches`, and each
/// `crates/*/build.rs`. Also the problems of every path that could not be scanned (unreadable,
/// a link that resolves to nothing, or a directory cycle): an absent `src`, `tests`, `benches`
/// or `build.rs` (nothing there, not even a link) is no problem, but every other failure is, so
/// the scan never passes without reading every file it names.
pub(crate) fn rust_files(root: &Path) -> (Vec<(String, String)>, Vec<Problem>) {
    let mut scan = Scan {
        root,
        files: Vec::new(),
        problems: Vec::new(),
    };
    for krate in scan.entries(&root.join("crates")) {
        if !scan.metadata(&krate).is_some_and(|m| m.is_dir()) {
            continue;
        }
        let build = krate.join("build.rs");
        if scan.optional_metadata(&build).is_some_and(|m| m.is_file()) {
            scan.read(&build);
        }
        for dir in ["src", "tests", "benches"] {
            let dir = krate.join(dir);
            if scan.optional_metadata(&dir).is_some_and(|m| m.is_dir()) {
                scan.walk(&dir, &mut Vec::new());
            }
        }
    }
    scan.files.sort();
    (scan.files, scan.problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = d.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        d
    }

    /// The problems in a temp tree with no seam files and no unit-test allowlist.
    fn bare(files: &[(&str, &str)]) -> Vec<String> {
        problems_with(tree(files).path(), &[], &[])
    }

    fn move_it(path: &str, line: usize, token: &str) -> String {
        format!(
            "{path}:{line}: {token} is Unix-only; move it into a seam (rdy_store::pio, \
             rdy_store::fileid, rdy_daemon::sys, rdy_qa::platform), or gate the test file with \
             #![cfg(unix)] and a \"// Unix only: <reason>\" line (FW-XP1)"
        )
    }

    #[test]
    fn ps11_a_use_in_product_code_is_named_with_its_line() {
        let got = bare(&[
            (
                "crates/rdy-engine/src/lib.rs",
                "pub mod a;\nuse std::os::unix::fs::MetadataExt;\nfn f() {}\n",
            ),
            ("crates/rdy-engine/src/clean.rs", "fn g() {}\n"),
            (
                "crates/rdy-cli/build.rs",
                "fn main() {\n    let _ = libc::getuid;\n}\n",
            ),
        ]);
        assert_eq!(
            got,
            vec![
                move_it("crates/rdy-cli/build.rs", 2, "libc::"),
                move_it("crates/rdy-engine/src/lib.rs", 2, "std::os::unix"),
            ]
        );
    }

    #[test]
    fn ps11_a_gated_test_file_with_a_reason_passes() {
        let got = bare(&[
            (
                "crates/rdy-daemon/tests/socket.rs",
                "#![cfg(unix)]\n// Unix only: it uses Unix sockets. FW-XP1 Phase 4 ports it.\n\
                 use std::os::unix::net::UnixStream;\n",
            ),
            (
                "crates/rdy-store/benches/b.rs",
                "#![cfg(unix)]\n// Unix only: it reads modes.\nuse std::os::unix::fs::MetadataExt;\n",
            ),
        ]);
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn ps11_a_gated_test_file_without_a_reason_is_named() {
        let reasonless = |path: &str| {
            format!("{path}: is #![cfg(unix)] with no \"// Unix only: <reason>\" line under it")
        };
        let got = bare(&[
            (
                "crates/rdy-daemon/tests/a.rs",
                "#![cfg(unix)]\nuse std::os::unix::net::UnixStream;\n",
            ),
            // The prefix with no word after it is no reason.
            (
                "crates/rdy-daemon/tests/b.rs",
                "#![cfg(unix)]\n// Unix only:   \nuse std::os::unix::net::UnixStream;\n",
            ),
            // A gated file with no use still needs its reason.
            ("crates/rdy-daemon/tests/c.rs", "#![cfg(unix)]\nfn f() {}\n"),
        ]);
        assert_eq!(
            got,
            vec![
                reasonless("crates/rdy-daemon/tests/a.rs"),
                reasonless("crates/rdy-daemon/tests/b.rs"),
                reasonless("crates/rdy-daemon/tests/c.rs"),
            ]
        );
    }

    #[test]
    fn ps11_an_ungated_test_file_is_named() {
        let got = bare(&[
            (
                "crates/rdy-qa/tests/io/io.rs",
                "use std::fs;\nuse std::os::unix::fs::symlink;\nfn f() { symlink(\"a\", \"b\").unwrap(); }\n",
            ),
            // The reason line without the gate line is not a gate.
            (
                "crates/rdy-store/benches/b.rs",
                "// Unix only: it reads modes.\nuse std::os::unix::fs::MetadataExt;\n",
            ),
        ]);
        assert_eq!(
            got,
            vec![
                move_it("crates/rdy-qa/tests/io/io.rs", 2, "std::os::unix"),
                move_it("crates/rdy-store/benches/b.rs", 2, "std::os::unix"),
            ]
        );
    }

    #[test]
    fn ps11_a_unit_test_count_that_differs_is_named() {
        let files = [(
            "crates/rdy-daemon/src/layout.rs",
            "fn f() {}\n#[cfg(test)]\nmod tests {\n    use std::os::unix::fs::MetadataExt;\n    \
             fn g() { let _ = libc::EIO; }\n}\n",
        )];
        let d = tree(&files);
        let exact = [("crates/rdy-daemon/src/layout.rs", 2, "modes")];
        assert_eq!(problems_with(d.path(), &[], &exact), Vec::<String>::new());
        let fewer = [("crates/rdy-daemon/src/layout.rs", 1, "modes")];
        assert_eq!(
            problems_with(d.path(), &[], &fewer),
            vec![
                "crates/rdy-daemon/src/layout.rs: has 2 Unix-only lines in its tests; \
                 UNIT_TEST_USES in crates/rdy-qa/src/seams.rs says 1"
                    .to_string()
            ]
        );
        let more = [("crates/rdy-daemon/src/layout.rs", 3, "modes")];
        assert_eq!(
            problems_with(d.path(), &[], &more),
            vec![
                "crates/rdy-daemon/src/layout.rs: has 2 Unix-only lines in its tests; \
                 UNIT_TEST_USES in crates/rdy-qa/src/seams.rs says 3"
                    .to_string()
            ]
        );
    }

    #[test]
    fn ps11_a_use_before_cfg_test_in_an_allowlisted_file_is_named() {
        let files = [(
            "crates/rdy-daemon/src/client.rs",
            "use std::os::unix::fs::PermissionsExt;\nfn f() {}\n#[cfg(test)]\nmod tests {\n    \
             use std::os::unix::fs::DirBuilderExt;\n}\n",
        )];
        let d = tree(&files);
        let allow = [("crates/rdy-daemon/src/client.rs", 1, "a read-only parent")];
        assert_eq!(
            problems_with(d.path(), &[], &allow),
            vec![
                "crates/rdy-daemon/src/client.rs:1: std::os::unix is Unix-only and outside the \
                 file's #[cfg(test)] module; only its tests may use it"
                    .to_string()
            ]
        );
        // A file with no #[cfg(test)] line has every use outside its tests.
        let files = [(
            "crates/rdy-daemon/src/client.rs",
            "fn f() {}\nuse std::os::unix::fs::PermissionsExt;\n",
        )];
        let d = tree(&files);
        assert_eq!(
            problems_with(d.path(), &[], &allow),
            vec![
                "crates/rdy-daemon/src/client.rs:2: std::os::unix is Unix-only and outside the \
                 file's #[cfg(test)] module; only its tests may use it"
                    .to_string(),
                "crates/rdy-daemon/src/client.rs: has 0 Unix-only lines in its tests; \
                 UNIT_TEST_USES in crates/rdy-qa/src/seams.rs says 1"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn ps11_comments_are_not_uses() {
        let got = bare(&[(
            "crates/rdy-engine/src/lib.rs",
            "// std::os::unix is not used here\n    /// libc::EIO in a doc comment\n\
             //! UnixStream in a module doc\n\t// tokio::net::unix\nfn f() {}\n",
        )]);
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn ps11_a_missing_seam_file_is_named() {
        let d = tree(&[(
            "crates/rdy-store/src/pio.rs",
            "use std::os::unix::fs::FileExt;\n",
        )]);
        let seams = [
            "crates/rdy-store/src/pio.rs",
            "crates/rdy-daemon/src/sys/unix.rs",
        ];
        assert_eq!(
            problems_with(d.path(), &seams, &[]),
            vec![
                "crates/rdy-daemon/src/sys/unix.rs: is a seam file in crates/rdy-qa/src/seams.rs, \
                 but it does not exist"
                    .to_string()
            ]
        );
    }

    #[test]
    fn ps11_each_token_is_found() {
        let files: Vec<(String, String)> = TOKENS
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    format!("crates/rdy-engine/src/t{i:02}.rs"),
                    format!("fn f() {{}}\nfn g() {{ let _ = {t}x; }}\n"),
                )
            })
            .collect();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let want: Vec<String> = TOKENS
            .iter()
            .enumerate()
            .map(|(i, t)| move_it(&format!("crates/rdy-engine/src/t{i:02}.rs"), 2, t))
            .collect();
        assert_eq!(bare(&refs), want);
        // A line counts once, and its message names the first token on it.
        let got = bare(&[(
            "crates/rdy-engine/src/two.rs",
            "use tokio::net::unix::UnixStream; use libc::EIO;\n",
        )]);
        assert_eq!(
            got,
            vec![move_it(
                "crates/rdy-engine/src/two.rs",
                1,
                "tokio::net::unix"
            )]
        );
    }

    /// Makes `p` unreadable (mode 000) and runs `f`, then restores the mode. Returns `None` when
    /// the mode does not stop this user (root), so the caller skips its checks.
    #[cfg(unix)]
    fn unreadable<T>(p: &Path, f: impl FnOnce() -> T) -> Option<T> {
        use std::os::unix::fs::PermissionsExt;
        let before = std::fs::metadata(p).unwrap().permissions();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = if p.is_dir() {
            std::fs::read_dir(p).is_err()
        } else {
            std::fs::read(p).is_err()
        };
        let out = blocked.then(f);
        std::fs::set_permissions(p, before).unwrap();
        out
    }

    #[cfg(unix)]
    #[test]
    fn ps11_an_unreadable_source_file_is_named() {
        let path = "crates/rdy-engine/src/reviewer_forbidden.rs";
        let d = tree(&[
            (path, "use std::os::unix::fs::MetadataExt;\n"),
            ("crates/rdy-engine/src/lib.rs", "fn f() {}\n"),
        ]);
        assert_eq!(
            problems_with(d.path(), &[], &[]),
            vec![move_it(path, 1, "std::os::unix")]
        );
        let p = d.path().join(path);
        // An unreadable allowlisted file is named once, not also as "has 0 lines".
        let allow = [(path, 1, "a test")];
        let got = unreadable(&p, || {
            let err = std::fs::read(&p).unwrap_err();
            (
                problems_with(d.path(), &[], &[]),
                problems_with(d.path(), &[], &allow),
                err.to_string(),
            )
        });
        if let Some((got, allowed, err)) = got {
            let want = vec![format!("{path}: cannot be scanned: {err}")];
            assert_eq!(got, want);
            assert_eq!(allowed, want);
        }
    }

    #[cfg(unix)]
    #[test]
    fn ps11_an_unreadable_directory_is_named() {
        let d = tree(&[
            (
                "crates/rdy-engine/src/hidden/forbidden.rs",
                "use std::os::unix::fs::MetadataExt;\n",
            ),
            ("crates/rdy-engine/src/lib.rs", "fn f() {}\n"),
        ]);
        let p = d.path().join("crates/rdy-engine/src/hidden");
        let got = unreadable(&p, || {
            let err = std::fs::read_dir(&p).unwrap_err();
            (problems_with(d.path(), &[], &[]), err.to_string())
        });
        if let Some((got, err)) = got {
            assert_eq!(
                got,
                vec![format!(
                    "crates/rdy-engine/src/hidden: cannot be scanned: {err}"
                )]
            );
        }
    }

    /// The message for a path whose link `std::fs::metadata` cannot follow.
    #[cfg(unix)]
    fn unscannable(root: &Path, path: &str) -> String {
        let err = std::fs::metadata(root.join(path)).unwrap_err();
        format!("{path}: cannot be scanned: {err}")
    }

    #[cfg(unix)]
    #[test]
    fn ps11_a_dangling_link_in_a_scanned_directory_is_named() {
        let d = tree(&[("crates/rdy-engine/src/lib.rs", "fn f() {}\n")]);
        let src = d.path().join("crates/rdy-engine/src");
        // A source file whose target is gone, and a link that may have been a directory.
        std::os::unix::fs::symlink("missing.rs", src.join("gone.rs")).unwrap();
        std::os::unix::fs::symlink("missing", src.join("gone_dir")).unwrap();
        assert_eq!(
            problems_with(d.path(), &[], &[]),
            vec![
                unscannable(d.path(), "crates/rdy-engine/src/gone.rs"),
                unscannable(d.path(), "crates/rdy-engine/src/gone_dir"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn ps11_a_dangling_crate_or_optional_path_link_is_named() {
        let d = tree(&[("crates/rdy-engine/src/lib.rs", "fn f() {}\n")]);
        let krate = d.path().join("crates/rdy-engine");
        // An entry of crates/, and a build.rs and tests/ that exist as links but resolve to
        // nothing: none of them is absent.
        std::os::unix::fs::symlink("missing", d.path().join("crates/rdy-ghost")).unwrap();
        std::os::unix::fs::symlink("missing.rs", krate.join("build.rs")).unwrap();
        std::os::unix::fs::symlink("missing", krate.join("tests")).unwrap();
        assert_eq!(
            problems_with(d.path(), &[], &[]),
            vec![
                unscannable(d.path(), "crates/rdy-engine/build.rs"),
                unscannable(d.path(), "crates/rdy-engine/tests"),
                unscannable(d.path(), "crates/rdy-ghost"),
            ]
        );
    }

    #[test]
    fn ps11_absent_tests_and_benches_directories_are_fine() {
        // A crate with only src/, and a crate with no src/, tests/ or benches/ at all.
        let got = bare(&[
            ("crates/rdy-engine/src/lib.rs", "fn f() {}\n"),
            ("crates/rdy-empty/Cargo.toml", "[package]\n"),
        ]);
        assert_eq!(got, Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn ps11_a_directory_cycle_is_named_and_the_scan_ends() {
        let d = tree(&[("crates/rdy-engine/src/lib.rs", "fn f() {}\n")]);
        let src = d.path().join("crates/rdy-engine/src");
        std::os::unix::fs::symlink(".", src.join("a")).unwrap();
        std::os::unix::fs::symlink(".", src.join("b")).unwrap();
        let root = d.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(problems_with(&root, &[], &[]));
        });
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the scan did not end within 10 s");
        let cycle = |link: &str| {
            format!(
                "crates/rdy-engine/src/{link}: leads back to crates/rdy-engine/src, a directory \
                 the scan is already in; remove the link"
            )
        };
        assert_eq!(got, vec![cycle("a"), cycle("b")]);
    }

    /// `crates/rdy-qa/src/platform/` and the seam list name the same files: a child with no
    /// Unix token yet still registers, so that the scan never has to guess at one.
    #[test]
    fn wx23a_every_platform_child_is_an_exact_seam() {
        const DIR: &str = "crates/rdy-qa/src/platform/";
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut on_disk: Vec<String> = std::fs::read_dir(root.join(DIR))
            .expect("list the platform children")
            .map(|entry| {
                let name = entry.expect("an entry").file_name();
                format!("{DIR}{}", name.to_string_lossy())
            })
            .collect();
        on_disk.sort();
        let mut registered: Vec<String> = SEAM_FILES
            .iter()
            .filter(|seam| seam.starts_with(DIR))
            .map(ToString::to_string)
            .collect();
        registered.sort();
        assert_eq!(registered, on_disk);
        for child in ["unix.rs", "windows.rs", "windows_process.rs"] {
            assert!(on_disk.contains(&format!("{DIR}{child}")), "{child}");
        }
    }

    /// The problems of a temp tree under the Windows lists given and no Unix list.
    fn windows_only(
        files: &[(&str, &str)],
        seams: &[&str],
        tests: &[(&str, &str)],
        unit_uses: &[(&str, usize, &str)],
    ) -> Vec<String> {
        let rules = WindowsRules {
            seams,
            tests,
            unit_uses,
        };
        all_problems(tree(files).path(), &[], &[], &rules)
    }

    fn windows_move_it(path: &str, line: usize, token: &str) -> String {
        format!(
            "{path}:{line}: {token} is Windows-only; move it into a seam (rdy_store::pio, \
             rdy_store::fileid, rdy_daemon::sys, rdy_qa::platform), or list the test file in \
             WINDOWS_TEST_FILES in crates/rdy-qa/src/seams.rs with its reason (FW-XP1)"
        )
    }

    #[test]
    fn wx32_windows_apis_outside_seams_are_named() {
        // product code, a build script and a directory that is only named sys
        let got = windows_only(
            &[
                (
                    "crates/rdy-engine/src/lib.rs",
                    "pub mod a;\nuse std::os::windows::fs::OpenOptionsExt;\nfn f() {}\n",
                ),
                (
                    "crates/rdy-engine/src/sys/mod.rs",
                    "use windows_sys::Win32::Foundation::CloseHandle;\n",
                ),
                (
                    "crates/rdy-cli/build.rs",
                    "fn main() {\n    let _ = winapi::um::fileapi::CreateFileW;\n}\n",
                ),
                (
                    "crates/rdy-daemon/src/lib.rs",
                    "extern \"system\" fn cb() {}\n#[link(name = \"kernel32\")]\nextern {}\n",
                ),
                (
                    "crates/rdy-daemon/src/net.rs",
                    "use tokio::net::windows::named_pipe::ServerOptions;\nuse tokio::signal::windows::ctrl_c;\n",
                ),
                (
                    "crates/rdy-engine/src/win32.rs",
                    "use windows::Win32::Foundation::HANDLE;\n",
                ),
            ],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            got,
            vec![
                windows_move_it("crates/rdy-cli/build.rs", 2, "winapi::"),
                windows_move_it("crates/rdy-daemon/src/lib.rs", 1, "extern \"system\""),
                windows_move_it("crates/rdy-daemon/src/lib.rs", 2, "#[link("),
                windows_move_it("crates/rdy-daemon/src/net.rs", 1, "tokio::net::windows"),
                windows_move_it("crates/rdy-daemon/src/net.rs", 2, "tokio::signal::windows"),
                windows_move_it("crates/rdy-engine/src/lib.rs", 2, "std::os::windows"),
                windows_move_it("crates/rdy-engine/src/sys/mod.rs", 1, "windows_sys"),
                windows_move_it("crates/rdy-engine/src/win32.rs", 1, "windows::Win32"),
            ]
        );
        // an exact seam file may use any of them; its sibling in the same directory may not
        let files = [
            (
                "crates/rdy-daemon/src/sys/windows/pipe.rs",
                "use windows_sys::Win32::Foundation::CloseHandle;\n",
            ),
            (
                "crates/rdy-daemon/src/sys/windows/other.rs",
                "use windows_sys::Win32::Foundation::CloseHandle;\n",
            ),
        ];
        assert_eq!(
            windows_only(
                &files,
                &["crates/rdy-daemon/src/sys/windows/pipe.rs"],
                &[],
                &[]
            ),
            vec![windows_move_it(
                "crates/rdy-daemon/src/sys/windows/other.rs",
                1,
                "windows_sys"
            )]
        );
    }

    #[test]
    fn wx32_windows_guard_skips_comments_urls_and_logical_slashes() {
        let got = windows_only(
            &[(
                "crates/rdy-engine/src/lib.rs",
                "// std::os::windows is not used here\n    /// windows_sys in a doc comment\n\
                 //! winapi::um in a module doc\nfn f() { let _ = \"https://example.test/a/b\"; \
                 let _ = \"docs/a.md\"; let _ = \"C:/x\"; }\n",
            )],
            &[],
            &[],
            &[],
        );
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn wx32_windows_test_files_and_unit_uses_are_exact() {
        const CLEAN: (&str, &str) = ("crates/rdy-engine/src/lib.rs", "fn f() {}\n");
        let used = "use std::os::windows::fs::symlink_file;\n";
        // a listed test file is clean; an unlisted one is named; a stale entry is named
        let got = windows_only(
            &[
                ("crates/rdy-store/tests/a.rs", used),
                ("crates/rdy-store/tests/b.rs", used),
                ("crates/rdy-store/tests/c.rs", "fn f() {}\n"),
            ],
            &[],
            &[
                ("crates/rdy-store/tests/a.rs", "links"),
                ("crates/rdy-store/tests/c.rs", "nothing"),
                ("crates/rdy-store/tests/gone.rs", "gone"),
                ("crates/rdy-store/src/lib.rs", "wrong place"),
            ],
            &[],
        );
        assert_eq!(
            got,
            vec![
                "crates/rdy-store/src/lib.rs: is in WINDOWS_TEST_FILES but is not under tests/ or \
                 benches/"
                    .to_string(),
                windows_move_it("crates/rdy-store/tests/b.rs", 1, "std::os::windows"),
                "crates/rdy-store/tests/c.rs: is in WINDOWS_TEST_FILES but uses no Windows-only API"
                    .to_string(),
                "crates/rdy-store/tests/gone.rs: is in WINDOWS_TEST_FILES, but it does not exist"
                    .to_string(),
            ]
        );
        // a product file may use a Windows API in its tests, exactly as often as listed
        let file = "crates/rdy-store/src/fsutil.rs";
        let text = "fn f() {}\n#[cfg(test)]\nmod tests {\n    use std::os::windows::fs::symlink_file;\n}\n";
        assert_eq!(
            windows_only(&[(file, text)], &[], &[], &[(file, 1, "a link")]),
            Vec::<String>::new()
        );
        assert_eq!(
            windows_only(&[(file, text)], &[], &[], &[(file, 2, "a link")]),
            vec![format!(
                "{file}: has 1 Windows-only lines in its tests; WINDOWS_UNIT_TEST_USES in \
                 crates/rdy-qa/src/seams.rs says 2"
            )]
        );
        // a use before the test module is product code
        let early = "use std::os::windows::fs::symlink_file;\n#[cfg(test)]\nmod tests {}\n";
        assert_eq!(
            windows_only(&[(file, early)], &[], &[], &[(file, 0, "none")]),
            vec![format!(
                "{file}:1: std::os::windows is Windows-only and outside the file's #[cfg(test)] \
                 module; only its tests may use it"
            )]
        );
        // a listed file that is gone
        assert_eq!(
            windows_only(&[CLEAN], &[], &[], &[(file, 1, "a link")]),
            vec![format!(
                "{file}: has 0 Windows-only lines in its tests; WINDOWS_UNIT_TEST_USES in \
                 crates/rdy-qa/src/seams.rs says 1"
            )]
        );
        // a missing Windows seam file
        assert_eq!(
            windows_only(&[CLEAN], &["crates/rdy-store/src/pio/windows.rs"], &[], &[]),
            vec![
                "crates/rdy-store/src/pio/windows.rs: is a Windows seam file in \
                 crates/rdy-qa/src/seams.rs, but it does not exist"
                    .to_string()
            ]
        );
    }

    /// Problems with the compile errors and the gate reasons that a Windows build meets.
    fn phase_four_problems(files: &[(String, String)]) -> Vec<String> {
        const CFG: &str = "#[cfg(not(any(unix, windows)))]";
        let mut out = Vec::new();
        for (path, text) in files {
            if path == "crates/rdy-qa/src/seams.rs" {
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if !t.starts_with("compile_error!(") {
                    continue;
                }
                let before = lines[..i]
                    .iter()
                    .rev()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty() && !l.starts_with("//"));
                if before != Some(CFG) {
                    out.push(format!(
                        "{path}:{}: a compile_error! that a Windows build can reach; gate it with \
                         {CFG}",
                        i + 1
                    ));
                }
                if t.contains("Phase 4") || t.contains(" yet") {
                    out.push(format!(
                        "{path}:{}: a compile_error! that still says the port is not done",
                        i + 1
                    ));
                }
            }
            if is_test_or_bench(path) && lines.first() == Some(&GATE_LINE) {
                let reason = lines.get(1).copied().unwrap_or("");
                if reason.contains("Phase 4") || reason.contains("ports it") {
                    out.push(format!(
                        "{path}:2: a Unix gate that waits for a port; name the mechanism and the \
                         Windows counterpart instead"
                    ));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn wx32_no_phase_four_compile_error_remains() {
        // synthetic: the old form, a reachable one, and a gate that waits for the port
        let bad = vec![
            (
                "crates/rdy-qa/src/platform.rs".to_string(),
                "// Stays until every export exists.\n#[cfg(not(unix))]\ncompile_error!(\"no implementation yet (FW-XP1 Phase 4)\");\n".to_string(),
            ),
            (
                "crates/rdy-store/src/pio.rs".to_string(),
                "#[cfg(not(any(unix, windows)))]\ncompile_error!(\"no implementation yet\");\n".to_string(),
            ),
            (
                "crates/rdy-cli/tests/cli.rs".to_string(),
                "#![cfg(unix)]\n// Unix only: it sets file modes. FW-XP1 Phase 4 ports it.\n".to_string(),
            ),
        ];
        assert_eq!(
            phase_four_problems(&bad),
            vec![
                "crates/rdy-cli/tests/cli.rs:2: a Unix gate that waits for a port; name the \
                 mechanism and the Windows counterpart instead"
                    .to_string(),
                "crates/rdy-qa/src/platform.rs:3: a compile_error! that a Windows build can reach; \
                 gate it with #[cfg(not(any(unix, windows)))]"
                    .to_string(),
                "crates/rdy-qa/src/platform.rs:3: a compile_error! that still says the port is not \
                 done"
                    .to_string(),
                "crates/rdy-store/src/pio.rs:2: a compile_error! that still says the port is not \
                 done"
                    .to_string(),
            ]
        );
        // the real tree
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (files, scan) = rust_files(&root);
        assert_eq!(scan, Vec::<Problem>::new());
        assert_eq!(phase_four_problems(&files), Vec::<String>::new());
        // and the three crates that cannot build for another platform still say so
        for path in [
            "crates/rdy-store/src/pio.rs",
            "crates/rdy-store/src/fileid.rs",
            "crates/rdy-daemon/src/sys/mod.rs",
            "crates/rdy-qa/src/platform.rs",
        ] {
            let text = &files.iter().find(|(p, _)| p == path).expect(path).1;
            assert!(text.contains("\ncompile_error!("), "{path}");
        }
    }

    #[test]
    fn wx32_every_audit_row_has_a_disposition() {
        // every list row has a reason and appears once
        for (name, rows) in [
            (
                "UNIT_TEST_USES",
                UNIT_TEST_USES
                    .iter()
                    .map(|r| (r.0, r.2))
                    .collect::<Vec<_>>(),
            ),
            (
                "WINDOWS_UNIT_TEST_USES",
                WINDOWS_UNIT_TEST_USES.iter().map(|r| (r.0, r.2)).collect(),
            ),
            ("WINDOWS_TEST_FILES", WINDOWS_TEST_FILES.to_vec()),
        ] {
            for (path, reason) in &rows {
                assert!(!reason.trim().is_empty(), "{name}: {path} has no reason");
            }
            let mut paths: Vec<&str> = rows.iter().map(|r| r.0).collect();
            paths.sort_unstable();
            let before = paths.len();
            paths.dedup();
            assert_eq!(paths.len(), before, "{name} lists a file twice");
        }
        for list in [SEAM_FILES, WINDOWS_SEAM_FILES] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), list.len(), "a seam file is listed twice");
        }
        // The counts that the moving tasks explained, against the plan's ledger: fsutil.rs 1,
        // wc/files.rs 3, layout.rs 9, client.rs 1, site/addr.rs 1 and socket.rs 1. No task changed
        // one; a change needs its own explanation.
        let counts: Vec<(&str, usize)> = UNIT_TEST_USES.iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            counts,
            vec![
                ("crates/rdy-store/src/fsutil.rs", 1),
                ("crates/rdy-engine/src/wc/files.rs", 3),
                ("crates/rdy-daemon/src/layout.rs", 9),
                ("crates/rdy-daemon/src/client.rs", 1),
                ("crates/rdy-daemon/src/site/addr.rs", 1),
                ("crates/rdy-daemon/src/socket.rs", 1),
            ]
        );
        // Every file of the real tree with a Unix or a Windows token has a disposition, and no
        // list row is stale: the whole scan comes back clean.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert_eq!(seam_problems(&root), Vec::<String>::new());
        // A seam of one family is not a seam of the other by accident: the Unix-only seam files
        // hold no Windows token outside the Windows seam list, and the reverse.
        let (files, _) = rust_files(&root);
        for (path, text) in &files {
            let lines: Vec<&str> = text.lines().collect();
            if !token_lines_of(&lines, WINDOWS_TOKENS).is_empty() {
                let ok = WINDOWS_SEAM_FILES.contains(&path.as_str())
                    || WINDOWS_TEST_FILES.iter().any(|r| r.0 == path)
                    || WINDOWS_UNIT_TEST_USES.iter().any(|r| r.0 == path);
                assert!(ok, "{path}: a Windows token with no disposition");
            }
            if !token_lines(&lines).is_empty() {
                let ok = SEAM_FILES.contains(&path.as_str())
                    || UNIT_TEST_USES.iter().any(|r| r.0 == path)
                    || (is_test_or_bench(path) && lines.first() == Some(&GATE_LINE));
                assert!(ok, "{path}: a Unix token with no disposition");
            }
        }
    }

    /// A box with `core.autocrlf=true` turned README.md, LICENSE-MIT and deny.toml into CRLF, and
    /// 11 tests that read them by bytes failed. The first rule of `.gitattributes` pins every file
    /// of the repository, so a new root file needs no rule of its own; the later rules only narrow
    /// it (`-text` for what is never converted).
    #[test]
    fn wx33h_gitattributes_pin_every_file_to_lf_first() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join(".gitattributes")).unwrap();
        let first = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'));
        assert_eq!(first, Some("* text=auto eol=lf"));
    }

    /// The text files at the repository's root hold no CR in this checkout, whatever its settings.
    #[test]
    fn wx33h_root_text_files_are_lf_in_this_checkout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut seen = Vec::new();
        for e in std::fs::read_dir(&root).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let text_file = name.starts_with("LICENSE")
                || [".md", ".toml", ".lock"].iter().any(|x| name.ends_with(x));
            if e.path().is_file() && text_file {
                let bytes = std::fs::read(e.path()).unwrap();
                assert!(!bytes.contains(&b'\r'), "{name} holds a CR");
                seen.push(name);
            }
        }
        for name in ["README.md", "LICENSE-MIT", "LICENSE-APACHE", "deny.toml"] {
            assert!(seen.iter().any(|s| s == name), "{name} in {seen:?}");
        }
    }

    #[test]
    fn ps11_the_real_tree_has_no_unix_code_outside_the_seams() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert_eq!(seam_problems(&root), Vec::<String>::new());
    }
}
