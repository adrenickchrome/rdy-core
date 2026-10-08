//! ccli1: rdy brief --code prints the code section, then the briefing; --json prints rdy_brief's result.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::*;
use rdy_qa::git_repo::GitRepo;

fn served_with_code() -> (Served, Spec, GitRepo) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    r.close().unwrap();
    let g = GitRepo::new();
    g.link(s.dir.path());
    g.write("src/a.rs", "a\n");
    g.commit(&format!("feat: a\n\nImplements: {}", spec.a));
    (s, spec, g)
}

#[test]
fn ccli1_rdy_brief_code_prints_the_code_section_then_the_briefing() {
    let (s, sp, g) = served_with_code();
    let out = s.rdy(&["brief", "--code", "src/a.rs"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.starts_with(&format!(
            "code src/a.rs (repos: {})\nimplements:\n  {} 1 commit(s)\n",
            g.configured(),
            sp.a
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("rdy briefing for {}", sp.a)),
        "{text}"
    );
    assert!(!text.contains("dropped when you save"), "{text}");
    assert!(!text.lines().any(|l| l.starts_with('#')), "{text}");
}

#[test]
fn ccli1_json_takes_an_absolute_path_inside_the_repo() {
    let (s, sp, g) = served_with_code();
    let abs = g.path().join("src/a.rs");
    let (ok, v) = s.json(&["--json", "brief", "--code", abs.to_str().unwrap()]);
    assert!(ok, "{v}");
    assert_eq!(v["code"]["path"], "src/a.rs", "{v}");
    assert_eq!(v["code"]["implements"][0]["block"], sp.a.to_string(), "{v}");
    assert!(v["base"]["token"].as_str().is_some(), "{v}");
}

#[test]
fn ccli1_a_code_path_that_escapes_is_e_bad_input_and_code_is_required() {
    let (s, _sp, _g) = served_with_code();
    let (ok, v) = s.json(&["--json", "brief", "--code", "../x.rs"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    assert!(
        !s.rdy(&["brief"]).status.success(),
        "--code is required until M2f"
    );
}
