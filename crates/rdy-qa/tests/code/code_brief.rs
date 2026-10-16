#![cfg(unix)]
// Unix only: it makes symlinks. FW-XP1 Phase 4 ports it.
//! cbrief1: rdy_brief on a code path: the blocks the file's commits implement, the blocks whose
//! reflects globs match it, and a briefing over them.
use rdy_engine::code_brief::*;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::git_repo::*;
use serde_json::json;
use std::collections::BTreeSet;

fn linked(r: &Rdy) -> GitRepo {
    let g = GitRepo::new();
    g.link(r.root());
    g
}

fn brief(r: &Rdy, path: &str) -> CodeBriefing {
    r.read()
        .brief_code(&Scope::Canon, path, None, None)
        .unwrap()
}

fn implements(cb: &CodeBriefing) -> Vec<String> {
    cb.code.implements.iter().map(|x| x.block.clone()).collect()
}

fn sorted(ids: &[&BlockId]) -> Vec<String> {
    let mut v: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
    v.sort();
    v
}

fn covered(cb: &CodeBriefing) -> BTreeSet<String> {
    cb.briefing
        .base
        .blocks
        .iter()
        .map(|b| b.id.clone())
        .collect()
}

fn subtree(r: &Rdy, root: &BlockId) -> BTreeSet<String> {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let mut out = BTreeSet::from([root.to_string()]);
    let mut todo = vec![root.clone()];
    while let Some(id) = todo.pop() {
        for c in m.children(Some(&id)) {
            out.insert(c.id.to_string());
            todo.push(c.id.clone());
        }
    }
    out
}

/// Sets `reflects: [glob]` on each block in one write to view `rf`, and lands it (once per test).
fn set_reflects(r: &mut Rdy, pairs: &[(&BlockId, &str)]) {
    let ops: Vec<_> = pairs
        .iter()
        .map(|(id, glob)| json!({"op": "update", "id": id.to_string(), "fields": {"reflects": [glob]}}))
        .collect();
    write(r, "rf", json!(ops)).unwrap();
    land(r, "rf").unwrap();
}

#[test]
fn cbrief1_a_code_path_briefs_the_blocks_its_commits_implement() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let g = linked(&r);
    g.write("src/a.rs", "a\n");
    g.write("src/b.rs", "b\n");
    g.commit(&format!("feat: a\n\nImplements: {}, {}", s.a, s.c));
    g.write("src/b.rs", "b2\n");
    g.commit(&format!("feat: b\n\nImplements: {}", s.b));
    let cb = brief(&r, "src/a.rs");
    assert_eq!(cb.code.path, "src/a.rs");
    assert_eq!(cb.code.repos, vec![g.configured()]);
    assert_eq!(implements(&cb), sorted(&[&s.a, &s.c]));
    for x in &cb.code.implements {
        assert_eq!(x.commits, 1);
        assert_eq!((x.status.clone(), x.dependent_commits), (None, None));
        assert!(!x.provisional && !x.rework);
    }
    assert!(cb.code.unresolved.is_empty() && cb.code.reflects.is_empty());
    assert_eq!(cb.briefing.covers, 2);
    assert_eq!(
        covered(&cb),
        BTreeSet::from([s.a.to_string(), s.c.to_string()])
    );
    assert!(
        cb.briefing
            .governs
            .iter()
            .any(|x| x.decision == s.decision.to_string()),
        "A is decided_by D, as in any briefing"
    );
}

#[test]
fn cbrief1_a_trailer_naming_a_section_covers_every_block_below_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let g = linked(&r);
    g.write("src/rules.rs", "r\n");
    g.commit(&format!("feat: rules\n\nImplements: {}", s.section));
    let cb = brief(&r, "src/rules.rs");
    assert_eq!(implements(&cb), vec![s.section.to_string()]);
    let want = subtree(&r, &s.section);
    assert_eq!(want.len(), 5, "section, A, B, C and the decision");
    assert_eq!(covered(&cb), want);
    assert_eq!(cb.briefing.covers, 5);
}

#[test]
fn cbrief1_keys_resolve_and_values_that_name_nothing_are_unresolved() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    write(
        &mut r,
        "k",
        json!([{"op": "update", "id": s.a.to_string(), "fields": {"key": "REQ-1"}}]),
    )
    .unwrap();
    land(&mut r, "k").unwrap();
    let g = linked(&r);
    g.write("src/a.rs", "a\n");
    g.commit("feat: a\n\nImplements: REQ-1, §4.2, c-77777");
    let cb = brief(&r, "src/a.rs");
    assert_eq!(implements(&cb), vec![s.a.to_string()]);
    assert_eq!(
        cb.code.unresolved,
        vec!["c-77777".to_string(), "§4.2".to_string()]
    );
}

#[test]
fn cbrief1_a_rename_carries_the_anchors_and_the_old_path_has_none() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let g = linked(&r);
    g.write("src/old.rs", &lines(40, "x"));
    g.commit(&format!("feat: x\n\nImplements: {}", s.a));
    g.mv("src/old.rs", "src/new.rs");
    g.write("src/new.rs", &(lines(40, "x") + "more\n"));
    g.commit(&format!("refactor: move\n\nImplements: {}", s.b));
    assert_eq!(implements(&brief(&r, "src/new.rs")), sorted(&[&s.a, &s.b]));
    let old = brief(&r, "src/old.rs");
    assert!(old.code.implements.is_empty());
    assert_eq!(old.briefing.covers, 0);
}

#[test]
fn cbrief1_reflects_globs_match_a_file_with_no_history() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    set_reflects(&mut r, &[(&s.c, "src/**/*.rs"), (&s.section, "docs/*.md")]);
    let g = linked(&r);
    g.write("README", "r\n");
    g.commit("chore: start");
    let cb = brief(&r, "src/brand_new.rs");
    assert!(cb.code.implements.is_empty());
    assert_eq!(
        cb.code.reflects,
        vec![ReflectsAnchor {
            block: s.c.to_string(),
            glob: "src/**/*.rs".into()
        }]
    );
    assert_eq!(covered(&cb), BTreeSet::from([s.c.to_string()]));
    let docs = brief(&r, "docs/x.md");
    assert_eq!(docs.code.reflects[0].block, s.section.to_string());
    assert_eq!(
        covered(&docs),
        subtree(&r, &s.section),
        "the section and its blocks"
    );
}

#[test]
fn cbrief1_with_no_code_repo_linked_only_reflects_answer() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    set_reflects(&mut r, &[(&s.c, "src/**/*.rs")]);
    let cb = brief(&r, "src/new.rs");
    assert!(cb.code.repos.is_empty());
    assert!(cb.code.implements.is_empty());
    assert_eq!(cb.code.reflects.len(), 1);
    let e = r
        .read()
        .brief_code(&Scope::Canon, "/abs/src/new.rs", None, None)
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.fix.contains("code_repos"), "{}", e.fix);
}

#[test]
fn cbrief1_code_paths_are_repo_relative_and_never_escape() {
    let (_d, mut r) = project();
    let _s = spec_doc(&mut r);
    let g = linked(&r);
    g.write("src/a.rs", "a\n");
    g.commit("feat: a");
    let st = r.read();
    let abs = g.path().join("src/a.rs");
    let path_of = |text: &str, cwd: Option<&std::path::Path>| {
        st.brief_code(&Scope::Canon, text, cwd, None)
            .unwrap()
            .code
            .path
    };
    assert_eq!(path_of(abs.to_str().unwrap(), None), "src/a.rs");
    assert_eq!(path_of("a.rs", Some(&g.path().join("src"))), "src/a.rs");
    assert_eq!(path_of("./src/a.rs", None), "src/a.rs");
    let outside = tempfile::tempdir().unwrap();
    let elsewhere = outside.path().join("x.rs");
    std::fs::write(&elsewhere, "x").unwrap();
    let escaping = format!(
        "{}/nodir/../../etc/x.rs",
        g.path().canonicalize().unwrap().display()
    );
    for bad in [
        escaping.as_str(),
        "",
        "../x.rs",
        "src/../a.rs",
        "src//a.rs",
        "src/./a.rs",
        "src/a.rs/",
        elsewhere.to_str().unwrap(),
    ] {
        let e = st.brief_code(&Scope::Canon, bad, None, None).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{bad:?}");
    }
}

#[test]
fn cbrief1_the_code_briefings_token_is_citable_on_a_stable_write() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let g = linked(&r);
    g.write("src/a.rs", "a\n");
    g.commit(&format!("feat: a\n\nImplements: {}", s.a));
    let cb = brief(&r, "src/a.rs");
    cb.briefing.register(&r.briefs());
    write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A, from the code side."}]),
        Some(&cb.briefing.base.token),
    )
    .expect("the code briefing covers A");
}

#[test]
fn cbrief1_a_local_file_outside_every_repo_falls_back_to_the_repo_relative_path() {
    // Run from the docs project, which has a README.md of its own: the caller meant the repo's.
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    std::fs::write(r.root().join("README.md"), "the project's\n").unwrap();
    let g = linked(&r);
    g.write("README.md", "the app's\n");
    g.commit(&format!("docs: readme\n\nImplements: {}", s.a));
    let cb = r
        .read()
        .brief_code(&Scope::Canon, "README.md", Some(r.root()), None)
        .unwrap();
    assert_eq!(cb.code.path, "README.md");
    assert_eq!(implements(&cb), vec![s.a.to_string()]);
}

#[cfg(unix)]
#[test]
fn cbrief1_a_new_file_in_a_new_folder_under_a_symlinked_prefix_is_inside_the_repo() {
    let (_d, r) = project();
    let tmp = tempfile::tempdir().unwrap();
    let g = GitRepo::at(&tmp.path().join("real"));
    g.link(r.root());
    std::os::unix::fs::symlink(g.path(), tmp.path().join("link")).unwrap();
    let new = tmp.path().join("link/src/newdir/new.rs");
    let cb = r
        .read()
        .brief_code(&Scope::Canon, new.to_str().unwrap(), None, None)
        .unwrap();
    assert_eq!(cb.code.path, "src/newdir/new.rs");
}

#[test]
fn cbrief1_coverage_over_the_limit_is_e_limit() {
    use rdy_engine::brief::MAX_BRIEF_BLOCKS;
    let (_d, mut r) = project();
    let work = Scope::View("work".into());
    // the doc, its section and 4,998 claims: exactly the limit
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "big", "title": "Big", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "All"}}),
    ];
    ops.extend((0..MAX_BRIEF_BLOCKS - 2).map(|i| {
        json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim {i}.")})
    }));
    let w = write(&mut r, "work", json!(ops)).unwrap();
    let section = w.created["$s"].clone();
    let g = linked(&r);
    g.write("src/big.rs", "b\n");
    g.commit("feat: big\n\nImplements: big");
    let st = r.read();
    let at_limit = st.brief_code(&work, "src/big.rs", None, None).unwrap();
    assert_eq!(at_limit.briefing.covers, MAX_BRIEF_BLOCKS);
    drop(st);
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "One more."}]),
    )
    .unwrap();
    let e = r
        .read()
        .brief_code(&work, "src/big.rs", None, None)
        .unwrap_err();
    assert_eq!(e.code, "E_LIMIT", "{e}");
    assert!(
        e.message
            .starts_with("the code briefing covers more than 5000 blocks"),
        "{e}"
    );
}
