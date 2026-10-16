//! Unix-only code lives only in the seam modules (FW-XP1 D-1RXEA).
//!
//! The gate's "unix seams" step and `ps11_the_real_tree_has_no_unix_code_outside_the_seams`
//! scan every `.rs` file under `crates/*/src`, `crates/*/tests` and `crates/*/benches`, and
//! every `crates/*/build.rs`, for the tokens in [`TOKENS`].

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
    "crates/rdy-store/src/fileid.rs",
    "crates/rdy-daemon/src/sys/unix.rs",
    "crates/rdy-qa/src/platform.rs",
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

/// Line 1 of a test or bench file that may use Unix-only APIs.
pub const GATE_LINE: &str = "#![cfg(unix)]";

/// The start of line 2 of a gated test or bench file; at least one more word follows it.
pub const REASON_PREFIX: &str = "// Unix only: ";

/// Every problem under `root` (the workspace), in path then line order; empty when clean.
pub fn seam_problems(root: &Path) -> Vec<String> {
    problems_with(root, SEAM_FILES, UNIT_TEST_USES)
}

/// The line of a problem with the whole file: after every real line.
const FILE: usize = usize::MAX;

/// [`seam_problems`] with the seam files and the unit-test allowlist given.
fn problems_with(root: &Path, seams: &[&str], unit_uses: &[(&str, usize, &str)]) -> Vec<String> {
    // (path, line, message); a file's own problems have line FILE and sort after its lines.
    let mut found: Vec<(String, usize, String)> = Vec::new();
    let files = rust_files(root);
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
        if !files.iter().any(|(p, _)| p == path) {
            found.push((path.to_string(), FILE, count_message(path, 0, *want)));
        }
    }
    found.sort();
    found.into_iter().map(|(_, _, m)| m).collect()
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
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let first = TOKENS
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

/// Every scanned file, as (its path from `root` with `/` separators, its text), in path order:
/// each `.rs` file under `crates/*/src`, `crates/*/tests` and `crates/*/benches`, and each
/// `crates/*/build.rs`.
fn rust_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for krate in sorted_entries(&root.join("crates")) {
        if !krate.is_dir() {
            continue;
        }
        let build = krate.join("build.rs");
        if build.is_file() {
            out.push(build);
        }
        for dir in ["src", "tests", "benches"] {
            walk(&krate.join(dir), &mut out);
        }
    }
    let mut files: Vec<(String, String)> = out
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(root).unwrap_or(&p);
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()).into_owned();
            (name, text)
        })
        .collect();
    files.sort();
    files
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in sorted_entries(dir) {
        if entry.is_dir() {
            walk(&entry, out);
        } else if entry.extension().is_some_and(|e| e == "rs") && entry.is_file() {
            out.push(entry);
        }
    }
}

/// The entries of `dir`, sorted; none when it is not a readable directory.
fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    v.sort();
    v
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

    #[test]
    fn ps11_the_real_tree_has_no_unix_code_outside_the_seams() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert_eq!(seam_problems(&root), Vec::<String>::new());
    }
}
