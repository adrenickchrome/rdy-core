//! dgm1: the diagram crate's six environment variables are removed at binary startup, so a
//! release render is the same bytes whatever the caller's environment holds (D-E8P8J).
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The six variables the pinned diagram crate reads at run time.
const MMDFLUX_ENVIRONMENT: [&str; 6] = [
    "MMDFLUX_DEBUG_DISABLE_LR_CRISS_CROSS_OVERLAP",
    "MMDFLUX_DEBUG_DISABLE_LR_VERTICAL_OVERLAP",
    "MMDFLUX_SKIP_TITLE_NODES",
    "MMDFLUX_DEBUG_BORDER_NODES",
    "MMDFLUX_DEBUG_PIPELINE",
    "MMDFLUX_DEBUG_LAYOUT",
];

const SEAM_CALL: &str = "rdy_daemon::sys::scrub_mmdflux_environment_at_startup();";
const SEAM_DEF: &str = "pub fn scrub_mmdflux_environment_at_startup()";

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crates directory")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn dgm1_main_scrubs_before_startup() {
    let crates = crates_dir();
    // the first statement of the CLI's main, before arguments, logging, a runtime or a thread
    let main = std::fs::read_to_string(crates.join("rdy-cli/src/main.rs")).unwrap();
    let at = main
        .find("\nfn main() -> ExitCode {\n")
        .expect("the CLI's main");
    let first = main[at..]
        .lines()
        .skip(2)
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("//"))
        .expect("main's first statement");
    assert_eq!(first, SEAM_CALL, "main's first statement");

    // the perf binary's main starts the same way, before it reads its arguments (Task 12)
    let perf = std::fs::read_to_string(crates.join("rdy-qa/src/bin/qa-perf.rs")).unwrap();
    let at = perf
        .find("\nfn main() {\n")
        .expect("the perf binary's main");
    let first = perf[at..]
        .lines()
        .skip(2)
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("//"))
        .expect("the perf main's first statement");
    assert_eq!(first, SEAM_CALL, "the perf main's first statement");

    // the seam removes exactly the six names, and names no other variable of the crate
    let sys = std::fs::read_to_string(crates.join("rdy-daemon/src/sys/mod.rs")).unwrap();
    let def = sys.find(SEAM_DEF).expect("the seam in rdy_daemon::sys");
    let named: Vec<&str> = sys
        .match_indices("\"MMDFLUX_")
        .map(|(i, _)| {
            let rest = &sys[i + 1..];
            &rest[..rest.find('"').expect("the closing quote")]
        })
        .collect();
    assert_eq!(named, MMDFLUX_ENVIRONMENT, "the seam's names, in order");
    assert!(
        sys[def..].contains("remove_var"),
        "the seam removes each name"
    );

    // the seam's production calls are the first statement of a binary's main (the CLI's, and
    // the perf binary's, which perf.rs pins on its own): no renderer, test or harness
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    let mut calls = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let rel = f
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "rdy-qa/tests/render/render_diagram_environment.rs" {
            continue;
        }
        for (i, _) in text.match_indices("scrub_mmdflux_environment_at_startup") {
            let line = text[..i].lines().count();
            calls.push(format!("{rel}:{line}"));
        }
    }
    calls.sort();
    assert_eq!(
        calls.len(),
        3,
        "the definition and two binary calls: {calls:?}"
    );
    assert!(calls[0].starts_with("rdy-cli/src/main.rs:"), "{calls:?}");
    assert!(
        calls[1].starts_with("rdy-daemon/src/sys/mod.rs:"),
        "{calls:?}"
    );
    assert!(
        calls[2].starts_with("rdy-qa/src/bin/qa-perf.rs:"),
        "{calls:?}"
    );
}

/// Every file under `dir`, by its relative path.
fn read_tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// A release `rdy render` child with the six names removed and then `set` added; its output
/// files and its stderr.
fn render(
    project: &Path,
    out: &Path,
    config: &[(String, String)],
    set: &[(&str, &std::ffi::OsStr)],
) -> (BTreeMap<String, Vec<u8>>, Vec<u8>) {
    let mut cmd = Command::new(rdy_bin());
    cmd.args(["--no-daemon", "--project"])
        .arg(project)
        .args(["render", "--out"])
        .arg(out)
        .env("RDY_ACTOR", "human:qa")
        .envs(config.iter().map(|(k, v)| (k, v)));
    for name in MMDFLUX_ENVIRONMENT {
        cmd.env_remove(name);
    }
    for (name, value) in set {
        cmd.env(name, value);
    }
    let o = cmd.output().expect("run rdy render");
    assert!(
        o.status.success(),
        "rdy render with {set:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    (read_tree(out), o.stderr)
}

#[test]
fn dgm1_mmdflux_environment_is_inert() {
    let routing = std::fs::read_to_string(
        crates_dir().join("rdy-diagram/tests/fixtures/diagram/routing.mmd"),
    )
    .unwrap();
    let (dir, mut r) = project();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Routes", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Flow"}},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "routes"},
         "body": routing}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    drop(r);
    let (_cfg, config) = human_env("human:qa");
    let scratch = tempfile::tempdir().unwrap();
    let mut runs = 0;
    let mut next_out = || {
        runs += 1;
        scratch.path().join(format!("site-{runs}"))
    };
    let (want, want_err) = render(dir.path(), &next_out(), &config, &[]);
    assert!(
        want.keys().any(|k| k.ends_with(".html")),
        "the render writes pages: {:?}",
        want.keys().collect::<Vec<_>>()
    );
    let paths = tempfile::tempdir().unwrap();
    let mut fresh = 0;
    let mut fresh_path = || {
        fresh += 1;
        paths.path().join(format!("absent-{fresh}"))
    };
    let one = std::ffi::OsStr::new("1");
    // each name alone, as 1 and as a fresh path; then all six, in each form
    let mut cases: Vec<Vec<(&str, PathBuf)>> = Vec::new();
    for name in MMDFLUX_ENVIRONMENT {
        cases.push(vec![(name, PathBuf::from("1"))]);
        cases.push(vec![(name, fresh_path())]);
    }
    cases.push(
        MMDFLUX_ENVIRONMENT
            .iter()
            .map(|n| (*n, PathBuf::from("1")))
            .collect(),
    );
    cases.push(
        MMDFLUX_ENVIRONMENT
            .iter()
            .map(|n| (*n, fresh_path()))
            .collect(),
    );
    for case in &cases {
        let set: Vec<(&str, &std::ffi::OsStr)> = case
            .iter()
            .map(|(n, v)| {
                let v = if v.as_os_str() == "1" {
                    one
                } else {
                    v.as_os_str()
                };
                (*n, v)
            })
            .collect();
        let (got, err) = render(dir.path(), &next_out(), &config, &set);
        assert!(got == want, "{set:?}: the output files differ");
        assert_eq!(
            String::from_utf8_lossy(&err),
            String::from_utf8_lossy(&want_err),
            "{set:?}: stderr"
        );
        for (_, v) in case {
            if v.as_os_str() != "1" {
                assert!(!v.exists(), "{set:?}: wrote {}", v.display());
            }
        }
    }
    assert_eq!(cases.len(), 14);
}
