//! vapi1: one request dispatcher serves rdy_view and the ctl `view` op.
use rdy_engine::engine::Local;
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn req(v: Value) -> ViewRequest {
    serde_json::from_value(v).unwrap()
}

#[test]
fn vapi1_every_op_dispatches_and_defaults_to_the_sessions_view() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let cx = write_as(
        &mut r,
        &as_agent("agent:x", "r1"),
        "x",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "x"}]),
    )
    .unwrap()
    .change;
    let e = Local::new(r);
    let a = as_agent("agent:a", "r1");
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "create", "name": "r1"})),
    )
    .unwrap();
    assert_eq!(
        (v["view"].as_str(), v["owner"]["actor"].as_str()),
        (Some("r1"), Some("agent:a"))
    );
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "include", "changes": [cx]})),
    )
    .unwrap();
    assert_eq!(
        (v["view"].as_str(), v["changes"].as_u64()),
        (Some("r1"), Some(1)),
        "the view defaults to the session's"
    );
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "fork", "from": "r1", "name": "r1-copy"})),
    )
    .unwrap();
    assert_eq!(v["changes"], 1);
    let v = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "show"}))).unwrap();
    assert_eq!(v["head"]["view"], "r1");
    let v = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "compare"}))).unwrap();
    assert_eq!(
        (
            v["a"].as_str(),
            v["b"].as_str(),
            v["added"].as_array().unwrap().len()
        ),
        (Some("r1"), Some("canon"), 1)
    );
    let v = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "list"}))).unwrap();
    assert!(
        v["views"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["name"] == "r1-copy"),
        "{v}"
    );
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "exclude", "changes": [cx]})),
    )
    .unwrap();
    assert_eq!(v["changes"], 0);
    let v = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "undo"}))).unwrap();
    assert_eq!(v["kind"], "exclude");
    let v = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "close", "view": "r1-copy", "reason": "dup"})),
    )
    .unwrap();
    assert_eq!(v["closed"][0]["reason"], "dup");
    let err = run_view(
        &e,
        &a,
        "human:qa",
        "r1",
        req(json!({"op": "own", "view": "r1", "none": true})),
    )
    .unwrap_err();
    assert_eq!(err.code, E_PERMISSION, "own is the human's: {err}");
    let v = run_view(
        &e,
        &human_writer(),
        "human:qa",
        "work",
        req(json!({"op": "own", "view": "r1", "owner": "agent:z/r2"})),
    )
    .unwrap();
    assert_eq!(v["owner"], json!({"actor": "agent:z", "run": "r2"}));
    let err = run_view(
        &e,
        &human_writer(),
        "human:qa",
        "work",
        req(json!({"op": "own", "view": "r1", "owner": "agent:z", "none": true})),
    )
    .unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "{err}");
    let err = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "create"}))).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT, "create needs a name: {err}");
    let err = run_view(&e, &a, "human:qa", "r1", req(json!({"op": "nope"}))).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert!(err.fix.contains("create, fork, include"), "{err}");
    e.close().unwrap();
}

#[test]
fn vapi1_unknown_keys_are_refused() {
    assert!(serde_json::from_value::<ViewRequest>(json!({"op": "list", "bogus": 1})).is_err());
}
