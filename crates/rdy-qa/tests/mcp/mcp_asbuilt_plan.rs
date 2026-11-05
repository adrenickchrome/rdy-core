//! ab3 (D-YAY1G, D-KQ4CA): the chat path confirms or defers an as-built plan item with the
//! existing tools (rdy_plan answers, rdy_propose plan_answers), and the fallback `rdy plan`
//! commands do the same. A reasoned confirm, once its version lands, silences the warning on
//! both paths. No new tool, op or flag.
use rdy_engine::Rdy;
use rdy_engine::as_built::{AS_BUILT_FIX_NO_ITEM, AS_BUILT_FIX_OPEN_ITEM};
use rdy_engine::engine::{Engine, Local};
use rdy_mcp::tools::*;
use rdy_model::BlockId;
use rdy_model::plan::{PlanKind, item_hash};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

const ACTOR: &str = "agent:qa";
const RUN: &str = "r1";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Canon: a doc, one section and three accepted decisions that ship 1.0.0, each cited by one
/// claim that is planned on its own: three warnings.
fn seed(r: &mut Rdy) -> BTreeMap<String, String> {
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Plain"}},
            {"op": "create", "type": "decision", "id": "$d1", "fields": {"statement": "Ship one.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$d2", "fields": {"statement": "Ship two.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "decision", "id": "$d3", "fields": {"statement": "Ship three.", "status": "proposed", "built": "1.0.0"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A.", "fields": {"planned": true}, "links": {"decided_by": ["$d1"]}},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B.", "fields": {"planned": true}, "links": {"decided_by": ["$d2"]}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "C.", "fields": {"planned": true}, "links": {"decided_by": ["$d3"]}}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
    w.created.clone()
}

/// The full `pi-` id of the as-built item (cause, subject).
fn item(cause: &str, subject: &str) -> String {
    format!("pi-{}", item_hash(&id(cause), subject, PlanKind::Confirm))
}

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// The as-built rows of a plan reply: (cause, subject, state, answer act).
fn built_rows(v: &Value) -> Vec<(String, String, String, Value)> {
    let mut rows: Vec<_> = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["edge"] == "built")
        .map(|i| {
            assert_eq!(i["kind"], "confirm", "{i}");
            (
                i["cause"].as_str().unwrap().to_string(),
                i["subject"].as_str().unwrap().to_string(),
                i["state"].as_str().unwrap().to_string(),
                i["answer"]["act"].clone(),
            )
        })
        .collect();
    rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    rows
}

fn row(cause: &str, subject: &str, state: &str, act: Value) -> (String, String, String, Value) {
    (cause.into(), subject.into(), state.into(), act)
}

/// The as-built warnings of a reply: (block, fix).
fn warns(v: &Value) -> Vec<(String, String)> {
    let mut w: Vec<_> = v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "W_AS_BUILT_OVERRIDE")
        .map(|w| {
            (
                w["block"].as_str().unwrap().to_string(),
                w["fix"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    w.sort();
    w
}

fn context(st: &mut McpState, at: &str) -> Value {
    rdy_context(st, p(json!({"id": at, "scope": "canon"}))).unwrap()
}

#[test]
fn ab3_chat_can_confirm_or_defer_and_cli_has_parity() {
    // ---- the chat path ----
    let (_dir, mut r) = project();
    let c = seed(&mut r);
    let (d1, d2, a, b) = (&c["$d1"], &c["$d2"], &c["$a"], &c["$b"]);
    let engine = Arc::new(Local::new(r));
    let mut st = McpState::new(engine.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": ACTOR, "run": RUN, "role": "writer", "view": "v"})),
    )
    .unwrap();
    assert_eq!(
        warns(&context(&mut st, a)),
        vec![(a.clone(), AS_BUILT_FIX_NO_ITEM.into())]
    );
    // both decisions ship 1.1.0: the write warns with the open item's fix
    let w = rdy_write(
        &mut st,
        p(json!({
            "ops": [
                {"op": "update", "id": d1, "fields": {"built": "1.1.0"}},
                {"op": "update", "id": d2, "fields": {"built": "1.1.0"}}
            ],
            "editorial": {"class": "clarify", "reason": "the record follows the release"}
        })),
    )
    .unwrap();
    assert_eq!(
        warns(&w),
        vec![
            (a.clone(), AS_BUILT_FIX_OPEN_ITEM.into()),
            (b.clone(), AS_BUILT_FIX_OPEN_ITEM.into())
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    // rdy_plan lists the two items
    let plan = rdy_plan(&mut st, p(json!({}))).unwrap();
    let mut want = vec![
        row(d1, a, "open", Value::Null),
        row(d2, b, "open", Value::Null),
    ];
    want.sort_by(|x, y| (&x.0, &x.1).cmp(&(&y.0, &y.1)));
    assert_eq!(built_rows(&plan), want);
    // a confirm needs a reason
    let e = rdy_plan(
        &mut st,
        p(json!({"answers": [{"item": item(d1, a), "act": "confirm"}]})),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_BAD_INPUT",
            "confirm needs a reason of 1 to 2000 characters"
        )
    );
    // a reasoned defer through rdy_plan
    let plan = rdy_plan(
        &mut st,
        p(json!({"answers": [{"item": item(d2, b), "act": "defer", "reason": "review with the release"}]})),
    )
    .unwrap();
    let deferred = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["edge"] == "built" && i["subject"] == b.as_str())
        .unwrap()
        .clone();
    assert_eq!(deferred["state"], "deferred", "{deferred}");
    assert_eq!(deferred["answer"]["reason"], "review with the release");
    // a reasoned confirm through rdy_propose's plan_answers, in the version's batch
    let prop = rdy_propose(
        &mut st,
        p(json!({
            "title": "ship 1.1.0",
            "summary": "test summary",
            "plan_answers": [{"item": item(d1, a), "act": "confirm", "reason": "a stays planned on purpose"}]
        })),
    )
    .unwrap();
    assert_eq!(prop["plan"]["deferred"], 1, "{prop}");
    assert_eq!(prop["plan"]["open"], 0, "{prop}");
    // still in the view: the confirmed item has no open item, so the fix names none
    let ctx = rdy_context(&mut st, p(json!({"id": a}))).unwrap();
    assert_eq!(warns(&ctx), vec![(a.clone(), AS_BUILT_FIX_NO_ITEM.into())]);
    // land that version: its deferral card and exceptions answered first, as the approver
    let (vid, vref) = (
        prop["id"].as_str().unwrap().to_string(),
        prop["ref"].as_str().unwrap().to_string(),
    );
    engine
        .write(Box::new(move |r| {
            rdy_qa::intake::accept_deferrals(r, &approver(), &vid, &vref);
            rdy_qa::intake::confirm_exceptions(r, &approver(), &vref);
            r.land(&approver(), &vid)?;
            Ok(Value::Null)
        }))
        .unwrap();
    // canon: the confirmed claim is quiet, the deferred one still warns with no item
    assert_eq!(warns(&context(&mut st, a)), vec![]);
    assert_eq!(
        warns(&context(&mut st, b)),
        vec![(b.clone(), AS_BUILT_FIX_NO_ITEM.into())]
    );
    drop(st);
    drop(engine);

    // ---- the fallback path: `rdy plan` confirm and defer, then `rdy show` and rdy_context ----
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let c = seed(&mut r);
    let (d3, cc) = (c["$d3"].clone(), c["$c"].clone());
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: "w".into(),
            ops: rdy_engine::parse_ops(vec![
                json!({"op": "update", "id": d3, "fields": {"built": "1.1.0"}}),
            ])
            .unwrap(),
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "the record follows the release".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    r.close().unwrap();
    let (ok, v) = s.json(&["--json", "plan", "--view", "w"]);
    assert!(ok, "{v}");
    assert_eq!(built_rows(&v), vec![row(&d3, &cc, "open", Value::Null)]);
    let it = item(&d3, &cc);
    let (ok, e) = s.json(&["--json", "plan", "confirm", &it, "--view", "w"]);
    assert!(!ok, "{e}");
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str()),
        (
            Some("E_BAD_INPUT"),
            Some("confirm needs a reason of 1 to 2000 characters")
        )
    );
    let (ok, v) = s.json(&[
        "--json", "plan", "defer", &it, "--reason", "later", "--view", "w",
    ]);
    assert!(ok, "{v}");
    assert_eq!(
        built_rows(&v),
        vec![row(&d3, &cc, "deferred", json!("defer"))]
    );
    let (ok, v) = s.json(&[
        "--json",
        "plan",
        "confirm",
        &it,
        "--reason",
        "c stays planned",
        "--view",
        "w",
    ]);
    assert!(ok, "{v}");
    assert_eq!(
        built_rows(&v),
        vec![row(&d3, &cc, "closed", json!("confirm"))]
    );
    // not landed: canon still warns, the same on both paths
    let shown = s.json(&["--json", "show", &cc]).1;
    assert_eq!(
        warns(&shown),
        vec![(cc.clone(), AS_BUILT_FIX_NO_ITEM.into())]
    );
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    land(&mut r, "w").unwrap();
    r.close().unwrap();
    let shown = s.json(&["--json", "show", &cc]).1;
    assert_eq!(warns(&shown), vec![]);
    let text = String::from_utf8_lossy(&s.rdy(&["show", &cc]).stdout).to_string();
    assert!(!text.contains("W_AS_BUILT_OVERRIDE"), "{text}");
    let mut mcp = McpClient::start_proxy(&s);
    mcp.call(
        "rdy_session",
        json!({"actor": ACTOR, "run": "r2", "role": "reader"}),
    )
    .ok();
    let ctx = mcp
        .call("rdy_context", json!({"id": cc, "scope": "canon"}))
        .ok();
    assert_eq!(warns(&ctx), vec![]);
    assert_eq!(ctx["as_built"], shown["as_built"]);
}
