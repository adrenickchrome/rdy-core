//! cidx1: the code index over a real git repository: trailers, renames, the HEAD-keyed cache,
//! and the linked-repo configuration.
use rdy_engine::code_index::*;
use rdy_qa::fixtures::project;
use rdy_qa::git_repo::*;
use std::sync::Arc;

fn repo_of(g: &GitRepo) -> CodeRepo {
    CodeRepo {
        configured: g.configured(),
        root: std::fs::canonicalize(g.path()).unwrap(),
    }
}

#[test]
fn cidx1_index_of_reads_trailers_and_follows_a_rename() {
    let g = GitRepo::new();
    g.write("src/old.rs", &lines(40, "old"));
    let c1 = g.commit("feat: old\n\nImplements: c-AAAAA, s-BBBBB");
    g.mv("src/old.rs", "src/new.rs");
    g.write("src/new.rs", &(lines(40, "old") + "one more\n"));
    let c2 = g.commit("refactor: move\n\nImplements: c-CCCCC");
    let idx = index_of(&repo_of(&g)).unwrap();
    assert_eq!(idx.head.as_deref(), Some(c2.as_str()));
    assert_eq!(idx.commits, 2);
    assert!(!idx.files.contains_key("src/old.rs"));
    let f = &idx.files["src/new.rs"];
    assert_eq!(f["c-AAAAA"], vec![c1.clone()]);
    assert_eq!(f["s-BBBBB"], vec![c1]);
    assert_eq!(f["c-CCCCC"], vec![c2]);
}

#[test]
fn cidx1_only_git_trailers_count_and_the_key_ignores_case() {
    let g = GitRepo::new();
    g.write("a.rs", "a\n");
    g.commit("Implements: c-AAAAA");
    g.write("a.rs", "b\n");
    let c2 = g.commit("fix: b\n\nimplements: c-BBBBB\nCo-Authored-By: qa <qa>");
    let idx = index_of(&repo_of(&g)).unwrap();
    let f = &idx.files["a.rs"];
    assert_eq!(
        f.keys().collect::<Vec<_>>(),
        vec!["c-BBBBB"],
        "a subject line is not a trailer"
    );
    assert_eq!(f["c-BBBBB"], vec![c2]);
}

#[test]
fn cidx1_the_index_is_reused_while_head_stays_and_rebuilt_when_it_moves() {
    let g = GitRepo::new();
    g.write("a.rs", "a\n");
    g.commit("feat: a\n\nImplements: c-AAAAA");
    let r = repo_of(&g);
    let first = index_of(&r).unwrap();
    assert!(
        Arc::ptr_eq(&first, &index_of(&r).unwrap()),
        "HEAD did not move: the cached index"
    );
    g.write("a.rs", "b\n");
    let c2 = g.commit("feat: b\n\nImplements: c-BBBBB");
    let second = index_of(&r).unwrap();
    assert_eq!(second.head.as_deref(), Some(c2.as_str()));
    assert!(second.files["a.rs"].contains_key("c-BBBBB"));
    assert_eq!(
        *second,
        build(&r).unwrap(),
        "the cached index equals a fresh build"
    );
}

#[test]
fn cidx1_two_builds_of_one_head_are_identical() {
    let g = GitRepo::new();
    for i in 0..5 {
        g.write(&format!("f{i}.rs"), &lines(10, &i.to_string()));
        g.commit(&format!("c{i}\n\nImplements: c-AAAA{i}"));
    }
    let r = repo_of(&g);
    assert_eq!(build(&r).unwrap(), build(&r).unwrap());
}

#[test]
fn cidx1_a_repo_with_no_commits_has_an_empty_index() {
    let g = GitRepo::new();
    let idx = index_of(&repo_of(&g)).unwrap();
    assert_eq!(*idx, CodeIndex::default());
}

#[test]
fn cidx1_code_repos_reads_the_project_config() {
    let (d, _r) = project();
    assert!(code_repos(d.path()).unwrap().is_empty(), "no key: no repos");
    let inner = GitRepo::at(&d.path().join("app"));
    let outer = GitRepo::new();
    let outer_text = outer.configured();
    link_repos(d.path(), &["app", outer_text.as_str(), "app"]);
    let repos = code_repos(d.path()).unwrap();
    assert_eq!(repos.len(), 2, "a repeated repo counts once: {repos:?}");
    assert_eq!(repos[0].configured, "app");
    assert_eq!(repos[0].root, std::fs::canonicalize(inner.path()).unwrap());
    assert_eq!(repos[1].configured, outer_text);
    assert_eq!(repos[1].root, std::fs::canonicalize(outer.path()).unwrap());
}

#[test]
fn cidx1_a_bad_code_repos_entry_is_e_config() {
    for (yaml, needle) in [
        ("code_repos: app\n", "list"),
        ("code_repos: [1]\n", "list"),
        ("code_repos: [\"\"]\n", "list"),
        ("code_repos: [missing]\n", "no such directory"),
    ] {
        let (d, _r) = project();
        append_config(d.path(), yaml);
        let e = code_repos(d.path()).unwrap_err();
        assert_eq!(e.code, "E_CONFIG", "{yaml}");
        assert!(e.message.contains(needle), "{yaml}: {}", e.message);
    }
}

#[test]
fn cidx1_a_linked_path_that_is_not_a_repository_top_is_e_config() {
    // Temporary directories sit outside any git repository, so `plain` is not inside one.
    let (d, _r) = project();
    std::fs::create_dir_all(d.path().join("plain")).unwrap();
    let g = GitRepo::at(&d.path().join("app"));
    std::fs::create_dir_all(g.path().join("sub")).unwrap();
    link_repos(d.path(), &["plain", "app/sub"]);
    let repos = code_repos(d.path()).unwrap();
    let e = index_of(&repos[0]).unwrap_err();
    assert_eq!(e.code, "E_CONFIG");
    assert!(e.message.contains("not a git repository"), "{}", e.message);
    let e = index_of(&repos[1]).unwrap_err();
    assert_eq!(e.code, "E_CONFIG");
    assert!(e.message.contains("top folder"), "{}", e.message);
}

/// Missing objects in the partial clone at `dir`, as `git rev-list --missing=print` lists them.
fn missing_objects(dir: &std::path::Path) -> usize {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-list", "--objects", "--all", "--missing=print"])
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('?'))
        .count()
}

/// git's `(major, minor)` version, from `git --version`.
fn git_version() -> (u32, u32) {
    let out = std::process::Command::new("git")
        .arg("--version")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().nth(2).unwrap_or("0.0");
    let mut it = v.split('.').map(|n| n.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

#[test]
fn cidx1_a_partial_clone_is_read_without_fetching() {
    if git_version() < (2, 45) {
        // GIT_NO_LAZY_FETCH arrived in git 2.45; older gits ignore it and fetch.
        eprintln!(
            "skipped: git {:?} predates GIT_NO_LAZY_FETCH",
            git_version()
        );
        return;
    }
    let g = GitRepo::new();
    g.run(&["config", "uploadpack.allowFilter", "true"]);
    g.write("src/old.rs", &lines(40, "old"));
    g.commit("feat: old\n\nImplements: c-AAAAA");
    g.mv("src/old.rs", "src/new.rs");
    g.write("src/new.rs", &(lines(40, "old") + "one more\n"));
    g.commit("refactor: move\n\nImplements: c-CCCCC");
    let parent = tempfile::tempdir().unwrap();
    let clone = parent.path().join("clone");
    let out = std::process::Command::new("git")
        .args(["clone", "-q", "--filter=blob:none"])
        .arg(format!("file://{}", g.path().display()))
        .arg(&clone)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let before = missing_objects(&clone);
    assert!(before > 0, "the old blob is not in the blobless clone");
    let r = CodeRepo {
        configured: clone.to_string_lossy().into_owned(),
        root: std::fs::canonicalize(&clone).unwrap(),
    };
    let e = build(&r).unwrap_err();
    assert_eq!(e.code, "E_IO");
    assert!(e.fix.contains("full clone"), "{}", e.fix);
    assert_eq!(
        missing_objects(&clone),
        before,
        "building the index fetched into the linked repository"
    );
}
