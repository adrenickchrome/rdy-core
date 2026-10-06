//! sess2: who is human, what an agent may write into, and where a session writes by default.
use rdy_engine::{
    E_PERMISSION, E_VIEW_OWNER, Rdy, Session, SessionKind, WriteRequest, classify, human_actor,
};
use rdy_model::Role;

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn doc(
    r: &mut Rdy,
    s: &Session,
    view: &str,
    name: &str,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}
    ]))
    .unwrap();
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
}

#[test]
fn sess2_human_actor_comes_from_xdg_then_home_then_user() {
    let xdg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    assert_eq!(human_actor(None, None, Some("nick")), "human:nick");
    assert_eq!(human_actor(None, None, None), "human:unknown");
    assert_eq!(
        human_actor(Some(xdg.path()), Some(home.path()), Some("nick")),
        "human:nick",
        "no file yet"
    );
    std::fs::create_dir_all(home.path().join(".config/rdy")).unwrap();
    std::fs::write(
        home.path().join(".config/rdy/config.yaml"),
        "actor: human:founder\n",
    )
    .unwrap();
    assert_eq!(
        human_actor(None, Some(home.path()), Some("nick")),
        "human:founder"
    );
    std::fs::create_dir_all(xdg.path().join("rdy")).unwrap();
    std::fs::write(xdg.path().join("rdy/config.yaml"), "actor: human:x\n").unwrap();
    assert_eq!(
        human_actor(Some(xdg.path()), Some(home.path()), Some("nick")),
        "human:x",
        "xdg wins"
    );
    std::fs::write(xdg.path().join("rdy/config.yaml"), "actor: [\n").unwrap();
    assert_eq!(
        human_actor(Some(xdg.path()), None, Some("nick")),
        "human:nick",
        "unreadable: the fallback"
    );
    std::fs::write(xdg.path().join("rdy/config.yaml"), "actor: ''\n").unwrap();
    assert_eq!(
        human_actor(Some(xdg.path()), None, Some("nick")),
        "human:nick",
        "empty: the fallback"
    );
}

#[test]
fn sess2_classify_makes_the_human_or_an_agent_and_agents_never_approve() {
    let h = classify("human:n", None, Some("ignored"), Role::Writer).unwrap();
    assert_eq!(
        (h.kind, h.run.as_deref(), h.actor.as_str()),
        (SessionKind::Human, Some("cli"), "human:n")
    );
    let h2 = classify("human:n", Some("human:n"), None, Role::Approver).unwrap();
    assert_eq!((h2.kind, h2.role), (SessionKind::Human, Role::Approver));
    let a = classify("human:n", Some("agent:w"), Some("r1"), Role::Writer).unwrap();
    assert_eq!(
        (a.kind, a.run.as_deref(), a.actor.as_str()),
        (SessionKind::Agent, Some("r1"), "agent:w")
    );
    let e = classify("human:n", Some("agent:w"), None, Role::Approver).unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    assert!(e.fix.contains("RDY_ACTOR"), "{e}");
    // a human-looking name that is not the configured human is an agent
    let o = classify("human:n", Some("human:other"), None, Role::Writer).unwrap();
    assert_eq!(o.kind, SessionKind::Agent);
}

#[test]
fn sess2_default_views_follow_the_session_kind() {
    assert_eq!(
        Session::human("human:n".into(), Role::Writer).default_view(),
        "work"
    );
    assert_eq!(
        Session::agent("agent:w".into(), Some("run:1".into()), Role::Writer)
            .unwrap()
            .default_view(),
        "run-1"
    );
    assert_eq!(
        Session::agent("agent:w".into(), None, Role::Writer)
            .unwrap()
            .default_view(),
        "work-agent-w"
    );
}

#[test]
fn sess2_a_first_commit_records_the_sessions_actor_and_run_as_the_views_owner() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer);
    doc(&mut r, &h, "work", "a").unwrap();
    let head = r.read().view_head("work").cloned().unwrap();
    let o = head.owner.expect("owned");
    assert_eq!(
        (o.actor.as_str(), o.run.as_deref()),
        ("human:n", Some("cli"))
    );
    let a = Session::agent("agent:w".into(), Some("r1".into()), Role::Writer).unwrap();
    doc(&mut r, &a, "r1", "b").unwrap();
    let o = r.read().view_head("r1").cloned().unwrap().owner.unwrap();
    assert_eq!(
        (o.actor.as_str(), o.run.as_deref()),
        ("agent:w", Some("r1"))
    );
}

#[test]
fn sess2_an_agent_cannot_commit_into_a_view_another_session_owns() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer);
    doc(&mut r, &h, "work", "a").unwrap();
    let a = Session::agent("agent:w".into(), None, Role::Writer).unwrap();
    let e = doc(&mut r, &a, "work", "b").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER);
    // own1 (M2c Task 15): `work` is the human's default view, refused as such for any agent.
    assert_eq!(e.message, "view \"work\" is the human's default view");
    assert!(e.fix.contains("work-agent-w"), "{e}");
    assert_eq!(r.read().included("work").len(), 1, "nothing was written");
    // the same actor from another run is another session
    let a2 = Session::agent("agent:w".into(), Some("r2".into()), Role::Writer).unwrap();
    doc(&mut r, &a2, "r2", "c").unwrap();
    let e = doc(&mut r, &a, "r2", "d").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER);
    // its own view is fine, twice
    doc(&mut r, &a, "work-agent-w", "e").unwrap();
    doc(&mut r, &a, "work-agent-w", "f").unwrap();
    assert_eq!(r.read().included("work-agent-w").len(), 2);
}

#[test]
fn sess2_a_human_may_write_into_any_view() {
    let (_d, mut r) = project();
    let a = Session::agent("agent:w".into(), Some("r1".into()), Role::Writer).unwrap();
    doc(&mut r, &a, "r1", "a").unwrap();
    let h = Session::human("human:n".into(), Role::Writer);
    doc(&mut r, &h, "r1", "b").unwrap();
    assert_eq!(r.read().included("r1").len(), 2);
    let o = r.read().view_head("r1").cloned().unwrap().owner.unwrap();
    assert_eq!(o.actor, "agent:w", "a human's write does not take the view");
}

#[test]
fn sess2_import_is_refused_to_agents_before_any_file_is_read() {
    let (d, mut r) = project();
    let b = Session::agent("agent:b".into(), None, Role::Writer).unwrap();
    let e = r
        .import(&b, "imp", &d.path().join("does-not-exist"))
        .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
}

#[test]
fn sess2_human_provenance_records_the_cli_run() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer);
    let res = doc(&mut r, &h, "work", "a").unwrap();
    let st = r.read();
    let c = st.reader().change(&res.change.parse().unwrap()).unwrap();
    assert_eq!(c.provenance.run.as_deref(), Some("cli"));
}

/// Final review fix round 1 (minor 10): when the refused view is the agent's own default view,
/// the fix does not send it back there.
#[test]
fn sess2_a_refused_default_view_suggests_a_run_name_or_a_fork_not_itself() {
    let (_d, mut r) = project();
    let h = Session::human("human:n".into(), Role::Writer);
    doc(&mut r, &h, "work-agent-x", "a").unwrap();
    let a = Session::agent("agent:x".into(), None, Role::Writer).unwrap();
    assert_eq!(a.default_view(), "work-agent-x");
    let e = doc(&mut r, &a, "work-agent-x", "b").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER);
    assert!(
        !e.fix.contains("write to your own view"),
        "the fix names the view just refused: {e}"
    );
    assert!(e.fix.contains("RDY_RUN"), "{e}");
    assert!(e.fix.contains("rdy_session"), "{e}");
    assert!(e.fix.contains("fork"), "{e}");
    // with a run, the default view is the run's name: the same fix
    doc(&mut r, &h, "r9", "c").unwrap();
    let a9 = Session::agent("agent:x".into(), Some("r9".into()), Role::Writer).unwrap();
    let e = doc(&mut r, &a9, "r9", "d").unwrap_err();
    assert!(e.fix.contains("RDY_RUN"), "{e}");
}
