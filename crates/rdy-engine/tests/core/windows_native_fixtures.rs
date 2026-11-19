//! Task 12b: the engine test fixtures that build paths, private directories and git children
//! use native forms. The same engine results follow on every host: logical paths stay
//! slash-separated, a git child runs no hook, and a private credential file is written through
//! the platform seam.
use rdy_store::pio;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// The logical form of a relative path: slash-separated on every host.
fn logical(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            &format!(
                "core.hooksPath={}",
                rdy_engine::code_index::empty_hooks_dir().display()
            ),
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git")
}

#[test]
fn wx12b_native_fixture_paths_preserve_engine_results() {
    let dir = tempfile::tempdir().unwrap();
    pio::make_private_dir(dir.path()).unwrap();

    // Logical paths stay slash-separated, whatever the host separator is.
    let nested = dir.path().join(".rdy").join("run");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(logical(dir.path(), &nested), ".rdy/run");

    // A private directory and a private credential file come from the platform seam.
    let rdy = dir.path().join("rdy");
    pio::create_private_dir(&rdy).unwrap();
    let creds = rdy.join("credentials.yaml");
    let tmp = creds.with_extension("seed");
    let mut f = fs::File::create(&tmp).unwrap();
    pio::make_private_file(&f).unwrap();
    f.write_all(b"installs: {}\n").unwrap();
    drop(f);
    fs::rename(&tmp, &creds).unwrap();
    assert_eq!(fs::read(&creds).unwrap(), b"installs: {}\n");

    // A git child with the empty hooks directory runs no hook, so a failing hook cannot refuse
    // the commit.
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    assert!(git(&repo, &["init", "-q"]).status.success());
    let hooks = repo.join(".git").join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(hooks.join("pre-commit"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(&repo, &["add", "a.txt"]).status.success());
    let out = git(&repo, &["commit", "-q", "-m", "a"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
