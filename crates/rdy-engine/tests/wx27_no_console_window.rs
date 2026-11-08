//! Task 27: a person must never see a console window flash when rdy starts git. On Windows a
//! detached daemon gives a console program a new console window, so every production git child
//! of the engine goes through `rdy_store::pio::no_console_window`.
use std::path::{Path, PathBuf};

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

/// The production part of a source file: everything before its first `#[cfg(test)]` line.
fn production(text: &str) -> &str {
    match text.find("\n#[cfg(test)]") {
        Some(i) => &text[..i],
        None => text,
    }
}

#[test]
fn wx27_engine_git_commands_set_no_console_window() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut spawns = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        let prod = production(&text);
        for (n, line) in prod.lines().enumerate() {
            if line.contains("Command::new(\"git\")") || line.contains("Command::new(GIT") {
                spawns.push((f.clone(), n, prod.to_string()));
            }
        }
    }
    assert!(!spawns.is_empty(), "the scan finds the git builder");
    for (f, _, prod) in &spawns {
        // the one builder: it names the helper before it returns the command
        assert!(
            f.ends_with("code_index.rs"),
            "{}: a production git child outside git_command",
            f.display()
        );
        let at = prod.find("pub fn git_command").expect("git_command");
        let body = &prod[at..];
        let end = body.find("\n}\n").unwrap();
        assert!(
            body[..end].contains("pio::no_console_window(&mut c)"),
            "git_command sets no console window"
        );
    }
    assert_eq!(spawns.len(), 1, "one production git builder");
}

#[test]
fn wx27_no_console_window_changes_nothing_that_a_child_sees() {
    let mut c = std::process::Command::new("git");
    rdy_store::pio::no_console_window(&mut c);
    let out = c.arg("--version").output().expect("git runs");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
}
