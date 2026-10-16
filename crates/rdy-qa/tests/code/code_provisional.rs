//! prov1: code citing a still-proposed decision is provisional; a rejected decision flags its
//! files for rework; how many commits depend on a decision (FW-L2).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::git_repo::*;
use serde_json::json;

fn w() -> Scope {
    Scope::View("w".into())
}

fn linked(r: &Rdy) -> GitRepo {
    let g = GitRepo::new();
    g.link(r.root());
    g
}

#[test]
fn prov1_code_citing_a_proposed_decision_is_provisional_until_it_lands() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let d = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "Sync identifies a device by email".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let p = BlockId::parse(&d.created["$decision"]).unwrap();
    let g = linked(&r);
    g.write("src/p.rs", "p\n");
    g.write("src/q.rs", "q\n");
    g.commit(&format!("feat: p\n\nImplements: {p}"));
    g.write("src/q.rs", "q2\n");
    g.commit(&format!("feat: q\n\nImplements: {p}, {}", s.a));

    let cb = r.read().brief_code(&w(), "src/p.rs", None, None).unwrap();
    let x = &cb.code.implements[0];
    assert_eq!(x.block, p.to_string());
    assert_eq!(x.status.as_deref(), Some("proposed"));
    assert!(x.provisional && !x.rework);
    assert_eq!(x.commits, 1);
    assert_eq!(
        x.dependent_commits,
        Some(2),
        "both commits depend on the decision"
    );
    assert!(cb.code.provisional && !cb.code.rework);

    let canon = r
        .read()
        .brief_code(&Scope::Canon, "src/p.rs", None, None)
        .unwrap();
    assert!(canon.code.implements.is_empty());
    assert_eq!(
        canon.code.unresolved,
        vec![p.to_string()],
        "not in canon yet"
    );

    land(&mut r, "w").unwrap();
    let after = r
        .read()
        .brief_code(&Scope::Canon, "src/p.rs", None, None)
        .unwrap();
    let x = &after.code.implements[0];
    assert_eq!(x.status.as_deref(), Some("live"));
    assert!(!x.provisional && !after.code.provisional);
    assert_eq!(x.dependent_commits, Some(2));
}

#[test]
fn prov1_a_rejected_decision_flags_the_files_that_cite_it_for_rework() {
    use rdy_qa::golden::write_as;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let res = write_as(
        &approver(),
        &mut r,
        "w",
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": s.section.to_string(),
            "fields": {"statement": "Accounts for sync", "status": "rejected",
                       "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                       "options": ["yes", "no"]}}]),
    )
    .unwrap();
    let x = BlockId::parse(&res.created["$x"]).unwrap();
    land(&mut r, "w").unwrap();
    let g = linked(&r);
    g.write("src/r.rs", "r\n");
    g.commit(&format!("feat: accounts\n\nImplements: {x}"));
    let cb = r
        .read()
        .brief_code(&Scope::Canon, "src/r.rs", None, None)
        .unwrap();
    let e = &cb.code.implements[0];
    assert_eq!(e.status.as_deref(), Some("rejected"));
    assert!(e.rework && !e.provisional);
    assert!(cb.code.rework);
    let dep = r.read().code_dependents(&Scope::Canon, &x).unwrap();
    assert_eq!(dep.commits, 1);
    assert_eq!(
        dep.files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src/r.rs"]
    );
}

#[test]
fn prov1_code_dependents_counts_distinct_commits_and_lists_files_at_head() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let g = linked(&r);
    let d = s.decision.to_string();
    g.write("src/x.rs", "x\n");
    g.write("src/y.rs", "y\n");
    g.commit(&format!("feat: x y\n\nImplements: {d}"));
    g.write("src/y.rs", "y2\n");
    g.commit(&format!("feat: y\n\nImplements: {d}, {d}"));
    g.write("src/z.rs", "z\n");
    g.commit(&format!("feat: z\n\nImplements: {d}"));
    g.rm("src/z.rs");
    g.commit("chore: drop z");
    let dep = r
        .read()
        .code_dependents(&Scope::Canon, &s.decision)
        .unwrap();
    assert_eq!(
        dep.commits, 3,
        "every commit that cites D, the deleted file's included"
    );
    let files: Vec<&str> = dep.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, vec!["src/x.rs", "src/y.rs"]);
    assert!(dep.files.iter().all(|f| f.repo == g.configured()));
    let cb = r
        .read()
        .brief_code(&Scope::Canon, "src/y.rs", None, None)
        .unwrap();
    let e = cb.code.implements.iter().find(|e| e.block == d).unwrap();
    assert_eq!(e.commits, 2);
    assert_eq!(e.dependent_commits, Some(3));
    let a = r.read().code_dependents(&Scope::Canon, &s.a).unwrap();
    assert_eq!((a.commits, a.files.len()), (0, 0));
}

#[test]
fn prov1_a_commit_citing_a_docs_path_does_not_count_toward_a_block_inside_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let g = linked(&r);
    let d = s.decision.to_string();
    g.write("src/doc.rs", "doc\n");
    g.commit("feat: doc\n\nImplements: spec");
    g.write("src/dec.rs", "dec\n");
    g.commit(&format!("feat: dec\n\nImplements: {d}"));
    let st = r.read();
    let dep = st.code_dependents(&Scope::Canon, &s.decision).unwrap();
    assert_eq!(
        dep.commits, 1,
        "the commit citing `spec` names the doc, not D"
    );
    let files: Vec<&str> = dep.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, vec!["src/dec.rs"]);
    let a = st.code_dependents(&Scope::Canon, &s.a).unwrap();
    assert_eq!((a.commits, a.files.len()), (0, 0));
    let cb = st
        .brief_code(&Scope::Canon, "src/dec.rs", None, None)
        .unwrap();
    let e = cb.code.implements.iter().find(|e| e.block == d).unwrap();
    assert_eq!(e.dependent_commits, Some(1));
    let doc = st.code_dependents(&Scope::Canon, &s.doc).unwrap();
    assert_eq!(doc.commits, 1, "the doc itself is still named by its path");
}

#[test]
fn prov1_a_docs_path_counts_its_commits_whatever_its_case_or_trailing_slash() {
    // `Spec` and `spec/` resolve to the doc (paths ignore case and empty segments), so
    // code_dependents counts them as rdy_brief resolves them.
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let g = linked(&r);
    g.write("src/a.rs", "a\n");
    g.commit("feat: a\n\nImplements: Spec");
    g.write("src/b.rs", "b\n");
    g.commit("feat: b\n\nImplements: spec/");
    let st = r.read();
    for f in ["src/a.rs", "src/b.rs"] {
        let cb = st.brief_code(&Scope::Canon, f, None, None).unwrap();
        let ids: Vec<&str> = cb
            .code
            .implements
            .iter()
            .map(|x| x.block.as_str())
            .collect();
        assert_eq!(ids, vec![s.doc.to_string().as_str()], "{f}");
    }
    let doc = st.code_dependents(&Scope::Canon, &s.doc).unwrap();
    assert_eq!(doc.commits, 2);
    let files: Vec<&str> = doc.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, vec!["src/a.rs", "src/b.rs"]);
    let sec = st.code_dependents(&Scope::Canon, &s.section).unwrap();
    assert_eq!((sec.commits, sec.files.len()), (0, 0));
}

#[test]
fn prov1_code_dependents_spans_every_linked_repo() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let (one, two) = (GitRepo::new(), GitRepo::new());
    let (one_text, two_text) = (one.configured(), two.configured());
    link_repos(
        r.root(),
        &[
            (one_text.as_str(), one.path().to_str().unwrap()),
            (two_text.as_str(), two.path().to_str().unwrap()),
        ],
    );
    let d = s.decision.to_string();
    one.write("src/x.rs", "x\n");
    one.commit(&format!("feat: x\n\nImplements: {d}"));
    two.write("lib/y.rs", "y\n");
    two.commit(&format!("feat: y\n\nImplements: {d}"));
    two.write("lib/y.rs", "y2\n");
    two.commit(&format!("fix: y\n\nImplements: {d}"));
    let dep = r
        .read()
        .code_dependents(&Scope::Canon, &s.decision)
        .unwrap();
    assert_eq!(dep.commits, 3, "commits from both repos");
    let mut want = vec![
        (one_text.clone(), "src/x.rs".to_string()),
        (two_text.clone(), "lib/y.rs".to_string()),
    ];
    want.sort();
    let got: Vec<(String, String)> = dep
        .files
        .iter()
        .map(|f| (f.repo.clone(), f.path.clone()))
        .collect();
    assert_eq!(got, want);
    let cb = r
        .read()
        .brief_code(&Scope::Canon, "lib/y.rs", None, None)
        .unwrap();
    assert_eq!(cb.code.repos, vec![one_text, two_text]);
    let e = cb.code.implements.iter().find(|e| e.block == d).unwrap();
    assert_eq!((e.commits, e.dependent_commits), (2, Some(3)));
}
