//! sess2 at the gate: ownership holds under the fixtures, and the human env helper works.
use rdy_engine::{E_VIEW_OWNER, Scope};
use rdy_qa::fixtures::{agent, human_env, project, write, writer};
use rdy_qa::invariants::check;
use serde_json::json;

#[test]
fn sess2_fixture_sessions_are_human_and_agents_are_kept_out_of_work() {
    let (_d, mut r) = project();
    assert!(writer().is_human());
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    )
    .unwrap();
    let a = agent("agent:x", Some("r1"));
    let ops = serde_json::from_value(json!([{"op": "create", "type": "doc", "fields": {"name": "b", "title": "B", "doc_type": "page"}}])).unwrap();
    let e = r
        .write(
            &a,
            rdy_engine::WriteRequest {
                view: "work".into(),
                ops,
                justification: None,
                message: String::new(),
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER);
    check(&r, &Scope::View("work".into())).unwrap();
}

#[test]
fn sess2_human_env_points_xdg_at_a_config_naming_the_actor() {
    let (dir, env) = human_env("human:qa");
    assert_eq!(env.len(), 1);
    assert_eq!(env[0].0, "XDG_CONFIG_HOME");
    let text = std::fs::read_to_string(dir.path().join("rdy/config.yaml")).unwrap();
    assert_eq!(text, "actor: human:qa\n");
    assert_eq!(
        rdy_engine::human_actor(Some(dir.path()), None, Some("nobody")).unwrap(),
        "human:qa"
    );
}
