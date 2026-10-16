//! ps5 (FW-X5, D-C5955): `code_repos` in `.rdy.yaml` names the linked code repos. Each name's
//! checkout path is under `code_paths` in `.rdy/local.yaml` or the user config, and the local
//! entry wins name by name (F1). An entry of `code_repos` that is not a name is ignored (F2).
//! Every mistake is a `W_CONFIG_UNKNOWN` warning and the project opens; only a wrong YAML type is
//! `E_CONFIG`, when a code briefing reads the keys.
use rdy_engine::code_index::{CodeRepo, code_repos_with};
use rdy_engine::config_levels::*;
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::project;
use rdy_qa::git_repo::*;
use std::path::{Path, PathBuf};

/// Appends `text` to the project's `.rdy/local.yaml`, created when missing.
fn local(project: &Path, text: &str) {
    let f = project.join(LOCAL_FILE);
    let mut t = std::fs::read_to_string(&f).unwrap_or_default();
    t.push_str(text);
    std::fs::write(f, t).unwrap();
}

/// A user config holding `text`; keep the `TempDir` alive.
fn user(text: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("config.yaml");
    std::fs::write(&f, text).unwrap();
    (d, f)
}

/// `p` as a quoted YAML string.
fn q(p: &Path) -> String {
    serde_json::to_string(&p.to_string_lossy()).unwrap()
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

/// The warnings as (message, fix), after checking that each is a `W_CONFIG_UNKNOWN` warning.
fn texts(warnings: &[rdy_schema::Issue]) -> Vec<(String, String)> {
    warnings
        .iter()
        .map(|w| {
            assert_eq!(w.code, "W_CONFIG_UNKNOWN", "{w:?}");
            assert_eq!(w.severity, rdy_schema::Severity::Warning, "{w:?}");
            (w.message.clone(), w.fix.clone())
        })
        .collect()
}

fn pair(message: &str, fix: &str) -> (String, String) {
    (message.to_string(), fix.to_string())
}

fn not_a_name(entry: &str) -> (String, String) {
    pair(
        &format!("code_repos entry {entry} is not a repo name, so rdy ignores it"),
        "name the repo in code_repos and give its path under code_paths in .rdy/local.yaml",
    )
}

fn no_path(name: &str) -> (String, String) {
    pair(
        &format!("code repo {name} has no checkout path, so rdy skips it"),
        &format!(
            "add code_paths: {{{name}: <path to its checkout>}} to .rdy/local.yaml or the user config"
        ),
    )
}

#[test]
fn ps5_a_name_with_a_local_path_links_the_repo() {
    let (d, _r) = project();
    let g = GitRepo::new();
    append_config(d.path(), "code_repos: [app]\n");
    local(d.path(), &format!("code_paths:\n  app: {}\n", q(g.path())));
    let (linked, warnings) = linked_repos(d.path(), None).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        linked,
        vec![LinkedRepo {
            name: "app".to_string(),
            path: g.path().to_path_buf(),
            level: Level::Local,
        }]
    );
    assert_eq!(
        code_repos_with(d.path(), None).unwrap(),
        vec![CodeRepo {
            configured: "app".to_string(),
            root: canon(g.path()),
        }]
    );
}

#[test]
fn ps5_a_relative_local_path_starts_at_the_project_root() {
    let (d, _r) = project();
    let g = GitRepo::at(&d.path().join("checkouts").join("app"));
    append_config(d.path(), "code_repos: [app]\n");
    local(d.path(), "code_paths:\n  app: checkouts/app\n");
    let (linked, warnings) = linked_repos(d.path(), None).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(linked.len(), 1, "{linked:?}");
    assert_eq!(
        linked[0].path,
        d.path().join("checkouts/app"),
        "made absolute"
    );
    let repos = code_repos_with(d.path(), None).unwrap();
    assert_eq!(repos.len(), 1, "{repos:?}");
    assert_eq!(repos[0].configured, "app");
    assert_eq!(repos[0].root, canon(g.path()));
}

/// F1: `code_paths` merges name by name. A local entry wins over the user entry of the same name;
/// a user entry of another name still applies.
#[test]
fn ps5_a_user_config_path_links_the_repo_and_a_local_path_wins() {
    let (d, _r) = project();
    let (one, two, lib) = (GitRepo::new(), GitRepo::new(), GitRepo::new());
    append_config(d.path(), "code_repos: [app, lib]\n");
    let (_u, uf) = user(&format!(
        "code_paths:\n  app: {}\n  lib: {}\n",
        q(one.path()),
        q(lib.path())
    ));
    let (linked, warnings) = linked_repos(d.path(), Some(&uf)).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        linked,
        vec![
            LinkedRepo {
                name: "app".to_string(),
                path: one.path().to_path_buf(),
                level: Level::User,
            },
            LinkedRepo {
                name: "lib".to_string(),
                path: lib.path().to_path_buf(),
                level: Level::User,
            },
        ]
    );
    let roots: Vec<PathBuf> = code_repos_with(d.path(), Some(&uf))
        .unwrap()
        .into_iter()
        .map(|r| r.root)
        .collect();
    assert_eq!(roots, vec![canon(one.path()), canon(lib.path())]);

    local(
        d.path(),
        &format!("code_paths:\n  app: {}\n", q(two.path())),
    );
    let (linked, warnings) = linked_repos(d.path(), Some(&uf)).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        linked,
        vec![
            LinkedRepo {
                name: "app".to_string(),
                path: two.path().to_path_buf(),
                level: Level::Local,
            },
            LinkedRepo {
                name: "lib".to_string(),
                path: lib.path().to_path_buf(),
                level: Level::User,
            },
        ],
        "the local entry wins for app; the user entry still gives lib"
    );
    let repos = code_repos_with(d.path(), Some(&uf)).unwrap();
    assert_eq!(
        repos,
        vec![
            CodeRepo {
                configured: "app".to_string(),
                root: canon(two.path()),
            },
            CodeRepo {
                configured: "lib".to_string(),
                root: canon(lib.path()),
            },
        ]
    );
}

#[test]
fn ps5_a_relative_user_path_warns_and_is_ignored() {
    let (d, _r) = project();
    std::fs::create_dir_all(d.path().join("app")).unwrap();
    append_config(d.path(), "code_repos: [app]\n");
    let (_u, uf) = user("code_paths:\n  app: app\n");
    let (linked, warnings) = linked_repos(d.path(), Some(&uf)).unwrap();
    assert!(linked.is_empty(), "{linked:?}");
    assert_eq!(
        texts(&warnings),
        vec![pair(
            &format!(
                "code_paths.app in {} is not an absolute path, so rdy ignores it",
                uf.display()
            ),
            "give an absolute path there, or set it in .rdy/local.yaml",
        )]
    );
    assert!(code_repos_with(d.path(), Some(&uf)).unwrap().is_empty());
}

#[test]
fn ps5_a_name_with_no_path_warns_and_is_skipped() {
    let (d, _r) = project();
    let g = GitRepo::new();
    append_config(d.path(), "code_repos: [app, lib]\n");
    local(d.path(), &format!("code_paths:\n  lib: {}\n", q(g.path())));
    let (linked, warnings) = linked_repos(d.path(), None).unwrap();
    let names: Vec<&str> = linked.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["lib"]);
    assert_eq!(texts(&warnings), vec![no_path("app")]);
    let repos = code_repos_with(d.path(), None).unwrap();
    assert_eq!(repos.len(), 1, "{repos:?}");
    assert_eq!(repos[0].configured, "lib");
}

/// F2: `code_repos` holds names only. A path there, even one to a real checkout, is ignored.
#[test]
fn ps5_an_absolute_path_in_rdy_yaml_warns_and_is_ignored() {
    let (d, _r) = project();
    let g = GitRepo::new();
    let shown = g.path().to_string_lossy().into_owned();
    append_config(d.path(), &format!("code_repos: [{}]\n", q(g.path())));
    let (linked, warnings) = linked_repos(d.path(), None).unwrap();
    assert!(linked.is_empty(), "{linked:?}");
    assert_eq!(texts(&warnings), vec![not_a_name(&shown)]);
    assert!(code_repos_with(d.path(), None).unwrap().is_empty());
}

/// F2: a relative path, or any other text that is not a name, is ignored too.
#[test]
fn ps5_a_relative_path_in_rdy_yaml_warns_and_is_ignored() {
    let (d, _r) = project();
    let g = GitRepo::at(&d.path().join("app"));
    std::fs::create_dir_all(g.path().join("sub")).unwrap();
    append_config(
        d.path(),
        "code_repos: [\"../app\", \"app/sub\", \"my app\", \".app\"]\n",
    );
    let (linked, warnings) = linked_repos(d.path(), None).unwrap();
    assert!(linked.is_empty(), "{linked:?}");
    assert_eq!(
        texts(&warnings),
        vec![
            not_a_name("../app"),
            not_a_name("app/sub"),
            not_a_name("my app"),
            not_a_name(".app"),
        ]
    );
    assert!(code_repos_with(d.path(), None).unwrap().is_empty());
}

#[test]
fn ps5_a_name_listed_twice_warns_and_counts_once() {
    let (d, _r) = project();
    let g = GitRepo::new();
    append_config(d.path(), "code_repos: [app, app, app]\n");
    local(d.path(), &format!("code_paths:\n  app: {}\n", q(g.path())));
    let (linked, warnings) = linked_repos(d.path(), None).unwrap();
    assert_eq!(linked.len(), 1, "{linked:?}");
    assert_eq!(
        texts(&warnings),
        vec![pair(
            "code_repos lists app twice; rdy uses it once",
            "remove the second app from code_repos in .rdy.yaml",
        )],
        "one warning per repeated name"
    );
    assert_eq!(code_repos_with(d.path(), None).unwrap().len(), 1);
}

#[test]
fn ps5_code_paths_for_an_unlisted_name_warns() {
    let (d, _r) = project();
    let g = GitRepo::new();
    append_config(d.path(), "code_repos: [app]\n");
    local(
        d.path(),
        &format!(
            "code_paths:\n  app: {}\n  lib: {}\n",
            q(g.path()),
            q(g.path())
        ),
    );
    let (_u, uf) = user(&format!("code_paths:\n  web: {}\n", q(g.path())));
    let (linked, warnings) = linked_repos(d.path(), Some(&uf)).unwrap();
    let names: Vec<&str> = linked.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["app"]);
    let unlisted = |name: &str| {
        pair(
            &format!("code_paths names {name}, which code_repos does not list, so rdy ignores it"),
            &format!("add {name} to code_repos in .rdy.yaml, or remove it from code_paths"),
        )
    };
    assert_eq!(texts(&warnings), vec![unlisted("lib"), unlisted("web")]);
}

/// guard: the wrong-file warning of the table covers `code_paths` in `.rdy.yaml` (and
/// `code_repos` in a person's file); rdy reads neither there.
#[test]
fn ps5_code_paths_in_rdy_yaml_warns_that_it_belongs_in_a_person_file() {
    let (d, _r) = project();
    let g = GitRepo::new();
    append_config(
        d.path(),
        &format!("code_repos: [app]\ncode_paths:\n  app: {}\n", q(g.path())),
    );
    local(d.path(), "code_repos: [lib]\n");
    let all = texts(&config_warnings(d.path(), None));
    assert!(
        all.contains(&pair(
            "code_paths in .rdy.yaml belongs in .rdy/local.yaml or the user config; rdy ignores it here",
            "move code_paths to .rdy/local.yaml or the user config",
        )),
        "{all:?}"
    );
    assert!(
        all.contains(&pair(
            "code_repos in .rdy/local.yaml belongs in .rdy.yaml; rdy ignores it here",
            "move code_repos to .rdy.yaml",
        )),
        "{all:?}"
    );
    let (linked, warnings) = linked_repos(d.path(), None).unwrap();
    assert!(linked.is_empty(), "{linked:?}");
    assert_eq!(texts(&warnings), vec![no_path("app")]);
}

/// O10: only a wrong YAML type is `E_CONFIG`, and only a code briefing raises it; the project
/// still opens.
#[test]
fn ps5_a_wrong_type_is_e_config_at_briefing_and_the_project_opens() {
    let list = (
        "code_repos must be a list of repo names (at most 16)",
        "fix code_repos in .rdy.yaml, e.g. code_repos: [app]".to_string(),
    );
    let local_map = (
        "code_paths must map repo names to paths",
        "fix code_paths in .rdy/local.yaml, e.g. code_paths: {app: ../app}".to_string(),
    );
    let seventeen: Vec<String> = (0..17).map(|i| format!("r{i}")).collect();
    let too_many = format!("code_repos: [{}]\n", seventeen.join(", "));
    for (config, local_text, want) in [
        ("code_repos: 7\n", "", &list),
        ("code_repos: app\n", "", &list),
        ("code_repos: [1]\n", "", &list),
        ("code_repos: [\"\"]\n", "", &list),
        (too_many.as_str(), "", &list),
        ("code_repos: [app]\n", "code_paths: 7\n", &local_map),
        ("code_repos: [app]\n", "code_paths: [app]\n", &local_map),
        (
            "code_repos: [app]\n",
            "code_paths:\n  app: [x]\n",
            &local_map,
        ),
        ("code_repos: [app]\n", "code_paths:\n  app: 7\n", &local_map),
        // a null value is not a mapping: only a missing key means no entries
        ("code_repos: [app]\n", "code_paths: null\n", &local_map),
        ("code_repos: [app]\n", "code_paths:\n", &local_map),
    ] {
        let (d, r) = project();
        drop(r);
        append_config(d.path(), config);
        local(d.path(), local_text);
        let case = format!("{config}{local_text}");
        effective(d.path(), None).unwrap_or_else(|e| panic!("{case}: {e}"));
        let r = Rdy::open(d.path()).unwrap_or_else(|e| panic!("{case}: the project opens: {e}"));
        let e = r
            .read()
            .brief_code(&Scope::Canon, "src/a.rs", None, None)
            .unwrap_err();
        assert_eq!(e.code, "E_CONFIG", "{case}: {e}");
        assert_eq!((e.message.as_str(), &e.fix), (want.0, &want.1), "{case}");
    }
    // the user config's wrong type names that file
    let (d, _r) = project();
    append_config(d.path(), "code_repos: [app]\n");
    let (_u, uf) = user("code_paths: [app]\n");
    assert!(effective(d.path(), Some(&uf)).is_ok());
    let e = code_repos_with(d.path(), Some(&uf)).unwrap_err();
    assert_eq!(e.code, "E_CONFIG", "{e}");
    assert_eq!(e.message, "code_paths must map repo names to paths");
    assert_eq!(
        e.fix,
        format!(
            "fix code_paths in {}, e.g. code_paths: {{app: ../app}}",
            uf.display()
        )
    );
    assert!(linked_repos(d.path(), Some(&uf)).is_err());
    // a null in the user config is a wrong type there too
    let (_u, uf) = user("code_paths:\n");
    assert!(effective(d.path(), Some(&uf)).is_ok());
    let e = code_repos_with(d.path(), Some(&uf)).unwrap_err();
    assert_eq!(e.code, "E_CONFIG", "{e}");
    assert_eq!(
        (e.message.as_str(), e.fix),
        (
            "code_paths must map repo names to paths",
            format!(
                "fix code_paths in {}, e.g. code_paths: {{app: ../app}}",
                uf.display()
            )
        )
    );
    // a local null never falls back to the user entry of the same name
    let g = GitRepo::new();
    let (_u, uf) = user(&format!("code_paths:\n  app: {}\n", q(g.path())));
    local(d.path(), "code_paths: null\n");
    assert!(effective(d.path(), Some(&uf)).is_ok());
    let e = code_repos_with(d.path(), Some(&uf)).unwrap_err();
    assert_eq!(e.code, "E_CONFIG", "{e}");
    assert_eq!(
        (e.message.as_str(), e.fix.as_str()),
        (
            "code_paths must map repo names to paths",
            "fix code_paths in .rdy/local.yaml, e.g. code_paths: {app: ../app}"
        )
    );
    assert!(linked_repos(d.path(), Some(&uf)).is_err());
}

/// F1 in `rdy config` (ruling, fix round 1): `code_paths` shows one row per entry that applies,
/// each with the level and file that supplied it, sorted by name; `merged` holds the same set.
#[test]
fn ps5_config_lists_each_code_path_with_its_own_level() {
    let s = Served::new();
    let app = GitRepo::at(&s.dir.path().join("checkouts").join("app"));
    app.write("src/a.rs", "a\n");
    app.commit("feat: a");
    let lib = GitRepo::at(&s.dir.path().join("lib"));
    let hidden = GitRepo::new();
    append_config(s.dir.path(), "code_repos: [app, lib]\n");
    local(s.dir.path(), "code_paths:\n  app: checkouts/app\n");
    let uf = s.user_config();
    let mut text = std::fs::read_to_string(&uf).unwrap();
    text.push_str(&format!(
        "code_paths:\n  lib: {}\n  app: {}\n",
        q(lib.path()),
        q(hidden.path())
    ));
    std::fs::write(&uf, text).unwrap();
    let local_file = s.dir.path().join(LOCAL_FILE);

    let (ok, v) = s.json(&["--json", "config"]);
    assert!(ok, "{v}");
    let rows: Vec<&serde_json::Value> = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["key"].as_str().unwrap().starts_with("code_paths"))
        .collect();
    assert_eq!(
        rows,
        vec![
            &serde_json::json!({"key": "code_paths.app", "value": "checkouts/app",
                "level": "local", "file": local_file}),
            &serde_json::json!({"key": "code_paths.lib", "value": lib.path(),
                "level": "user", "file": uf}),
        ],
        "{v}"
    );
    let o = s.rdy(&["config", "--show-origin"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success(), "{out}");
    let want = [
        format!("code_paths.app = checkouts/app  # {}", local_file.display()),
        format!(
            "code_paths.lib = {}  # {}",
            lib.path().display(),
            uf.display()
        ),
    ];
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("code_paths"))
        .collect();
    assert_eq!(lines, want, "{out}");

    let e = effective(s.dir.path(), Some(&uf)).unwrap();
    let merged: serde_yaml_ng::Mapping =
        serde_yaml_ng::from_str(&format!("app: checkouts/app\nlib: {}\n", q(lib.path()))).unwrap();
    assert_eq!(
        e.merged.get("code_paths"),
        Some(&serde_yaml_ng::Value::Mapping(merged)),
        "{:?}",
        e.merged
    );

    let (ok, v) = s.json(&["--json", "brief", "--code", "src/a.rs"]);
    assert!(ok, "{v}");
    assert_eq!(v["code"]["repos"], serde_json::json!(["app", "lib"]), "{v}");
}

/// The closed list of warnings, each from `config_warnings` (and `effective`) in list order.
#[test]
fn ps5_the_warnings_are_config_warnings() {
    let (d, _r) = project();
    let g = GitRepo::new();
    append_config(d.path(), "code_repos: [\"../app\", app, app, lib, web]\n");
    local(
        d.path(),
        &format!(
            "code_paths:\n  app: {}\n  extra: {}\n",
            q(g.path()),
            q(g.path())
        ),
    );
    let (_u, uf) = user("code_paths:\n  lib: rel/lib\n");
    let want = vec![
        not_a_name("../app"),
        pair(
            "code_repos lists app twice; rdy uses it once",
            "remove the second app from code_repos in .rdy.yaml",
        ),
        no_path("web"),
        pair(
            "code_paths names extra, which code_repos does not list, so rdy ignores it",
            "add extra to code_repos in .rdy.yaml, or remove it from code_paths",
        ),
        pair(
            &format!(
                "code_paths.lib in {} is not an absolute path, so rdy ignores it",
                uf.display()
            ),
            "give an absolute path there, or set it in .rdy/local.yaml",
        ),
    ];
    assert_eq!(texts(&config_warnings(d.path(), Some(&uf))), want);
    assert_eq!(
        texts(&effective(d.path(), Some(&uf)).unwrap().warnings),
        want
    );
    let (linked, warnings) = linked_repos(d.path(), Some(&uf)).unwrap();
    assert_eq!(texts(&warnings), want);
    let names: Vec<&str> = linked.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["app"]);
}

#[test]
fn ps5_the_cli_prints_the_warning() {
    let s = Served::new();
    append_config(s.dir.path(), "code_repos: [app]\n");
    let line = "W_CONFIG_UNKNOWN: code repo app has no checkout path, so rdy skips it";
    // `rdy status` carries it in its own output
    let o = s.rdy(&["status"]);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "{out}");
    assert!(out.contains(line), "{out}");
    // another command prints it once on stderr
    let o = s.rdy(&["--json", "tags"]);
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "{err}");
    assert!(
        err.lines().any(|l| l
            == "warning W_CONFIG_UNKNOWN: code repo app has no checkout path, so rdy skips it; \
                add code_paths: {app: <path to its checkout>} to .rdy/local.yaml or the user config"),
        "{err}"
    );
    serde_json::from_slice::<serde_json::Value>(&o.stdout).expect("stdout is still JSON");
}

#[test]
fn ps5_a_code_briefing_names_its_repos() {
    let (d, r) = project();
    let g = GitRepo::at(&d.path().join("checkouts").join("app"));
    assert_eq!(g.configured(), "app", "a folder name that is a repo name");
    assert!(
        GitRepo::new().configured().starts_with("repo-"),
        "a temporary folder's name is not a repo name"
    );
    g.write("src/a.rs", "a\n");
    g.commit("feat: a");
    g.link(r.root());
    let text = std::fs::read_to_string(d.path().join(PROJECT_FILE)).unwrap();
    assert!(text.contains("code_repos:\n  - \"app\"\n"), "{text}");
    let cb = r
        .read()
        .brief_code(&Scope::Canon, "src/a.rs", None, None)
        .unwrap();
    assert_eq!(cb.code.repos, vec!["app".to_string()]);
}

/// `.rdy.yaml` is mandatory (final review, fix round 1): in an engine that is already open, a
/// missing project config is the missing-project error, never a briefing that silently links no
/// repo. Restored, the repo comes back. `.rdy/local.yaml` and the user config stay optional.
#[test]
fn ps5_a_missing_project_config_is_an_error_in_an_open_engine() {
    let (d, r) = project();
    let g = GitRepo::at(&d.path().join("checkouts").join("app"));
    g.write("src/a.rs", "a\n");
    g.commit("feat: a");
    g.link(r.root());
    let brief = || r.read().brief_code(&Scope::Canon, "src/a.rs", None, None);
    assert_eq!(brief().unwrap().code.repos, vec!["app".to_string()]);

    let config = d.path().join(PROJECT_FILE);
    let aside = d.path().join("rdy.yaml.aside");
    std::fs::rename(&config, &aside).unwrap();
    let want = missing_project(d.path());
    let e = brief().expect_err("a missing .rdy.yaml is an error, not a briefing with no repos");
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (want.code, want.message.as_str(), want.fix.as_str())
    );
    assert_eq!(e.code, "E_NO_PROJECT", "{e}");
    let e = linked_repos(d.path(), None).unwrap_err();
    assert_eq!((e.code, e.message), (want.code, want.message));

    std::fs::rename(&aside, &config).unwrap();
    assert_eq!(brief().unwrap().code.repos, vec!["app".to_string()]);

    // the optional files: no local file and a user config path with no file link no repo,
    // with the no-path warning, and no error
    std::fs::remove_file(d.path().join(LOCAL_FILE)).unwrap();
    let u = tempfile::tempdir().unwrap();
    let absent_user = u.path().join("config.yaml");
    let (linked, warnings) = linked_repos(d.path(), Some(&absent_user)).unwrap();
    assert!(linked.is_empty(), "{linked:?}");
    assert_eq!(texts(&warnings), vec![no_path("app")]);
    assert!(
        code_repos_with(d.path(), Some(&absent_user))
            .unwrap()
            .is_empty()
    );
}
