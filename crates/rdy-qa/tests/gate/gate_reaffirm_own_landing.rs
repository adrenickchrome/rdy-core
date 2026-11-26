//! r59021 (p-WQB0V, D-CQMA4, D-EJBPD, D-GV465, D-KQ4CA, D-ZY24B): a landing's own edits open no
//! re-affirmation card for the decisions it confirmed. A decision that the landing wrote, or that
//! an earlier landing of the same view wrote before the view changed the block it rests on, is
//! left out of that block's card. The suspect rows stay readable. A change from another view
//! still opens the card, and so does a `one_way` decision whose block a later landing changed.
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, PendingQuery, PlanCardBinding, REAFFIRM_WHY};
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_mcp::tools::{McpState, rdy_pending, rdy_session};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, project, write, writer};
use rdy_qa::intake::PERSON;
use serde_json::json;
use std::sync::Arc;

/// Canon: doc `spec`, section `Rules`, claim `a`.
struct World {
    s: String,
    a: String,
}

fn world(r: &mut Rdy) -> World {
    let w = write(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."}
        ]),
    )
    .unwrap();
    land(r, "seed").unwrap();
    World {
        s: w.created["$s"].clone(),
        a: w.created["$a"].clone(),
    }
}

/// A decision of `statement` in `view` that depends on `on`, not landed.
fn decide_in(
    r: &mut Rdy,
    view: &str,
    section: &str,
    statement: &str,
    on: &[&str],
    reversibility: &str,
) -> String {
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: section.into(),
            statement: statement.into(),
            title: Some(statement.to_string()),
            depends_on: on.iter().map(|s| s.to_string()).collect(),
            reversibility: Some(reversibility.into()),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("decide {statement}: {e}"))
    .created["$decision"]
        .clone()
}

/// A new body for `target` in `view`, not landed.
fn edit(r: &mut Rdy, view: &str, target: &str, body: &str) {
    write(
        r,
        view,
        json!([{"op": "update", "id": target, "body": body}]),
    )
    .unwrap_or_else(|e| panic!("edit {target}: {e}"));
}

fn queue_items(r: &Rdy) -> Vec<Item> {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .expect("pending")
        .items
}

/// The re-affirmation card of `cause`, if the queue holds one.
fn card(r: &Rdy, cause: &str) -> Option<Item> {
    queue_items(r).into_iter().find(|i| {
        matches!(&i.plan_binding, Some(PlanCardBinding::Reaffirm { cause: c, .. }) if c.as_str() == cause)
    })
}

fn members(i: &Item) -> Vec<String> {
    match &i.plan_binding {
        Some(PlanCardBinding::Reaffirm { members, .. }) => {
            members.iter().map(|m| m.id.to_string()).collect()
        }
        other => panic!("not a re-affirmation card: {other:?}"),
    }
}

/// The structural suspect rows of canon on `block`, as the targets they name.
fn suspect_targets(r: &Rdy, block: &str) -> Vec<String> {
    r.read()
        .suspect_details(&Scope::Canon)
        .unwrap()
        .into_iter()
        .filter(|s| s.block.as_str() == block)
        .filter_map(|s| s.target.map(|t| t.to_string()))
        .collect()
}

/// Every re-affirmation card item of an MCP queue read.
fn mcp_reaffirm_refs(st: &mut McpState) -> Vec<String> {
    let q = rdy_pending(
        st,
        serde_json::from_value(json!({"max_tokens": 20000})).unwrap(),
    )
    .unwrap();
    q["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["ref"].as_str())
        .filter(|r| r.starts_with("reaffirm:"))
        .map(str::to_string)
        .collect()
}

fn agent_state(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    st
}

/// The ref of every re-affirmation card in the command line's queue.
fn cli_reaffirm_refs(srv: &Served) -> Vec<String> {
    let (ok, v) = srv.json(&["--json", "pending"]);
    assert!(ok, "{v}");
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["ref"].as_str())
        .filter(|r| r.starts_with("reaffirm:"))
        .map(str::to_string)
        .collect()
}

/// The shape of one proposal that accepts two decisions and edits the block both rest on: the
/// queue has no card for the block, over MCP and the command line, and each decision's suspect
/// row on the block is still readable.
#[test]
fn r59021_own_landing_opens_no_reaffirm_card() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let d1 = decide_in(&mut r, "own", &w.s, "One rule", &[&w.a], "two_way");
    let d2 = decide_in(&mut r, "own", &w.s, "Two rule", &[&w.a], "two_way");
    edit(&mut r, "own", &w.a, "A holds 60 days.");
    land(&mut r, "own").unwrap();
    for d in [&d1, &d2] {
        assert_eq!(
            suspect_targets(&r, d),
            vec![w.a.clone()],
            "the suspect row of {d} on a stays readable"
        );
    }
    assert!(card(&r, &w.a).is_none(), "no card: {:#?}", queue_items(&r));
    let mut st = agent_state(r);
    assert_eq!(mcp_reaffirm_refs(&mut st), Vec::<String>::new());
    drop(st);

    // the command line over a project seeded the same way
    let srv = Served::new();
    srv.stop();
    let mut r = Rdy::open(srv.dir.path()).unwrap();
    let w = world(&mut r);
    decide_in(&mut r, "own", &w.s, "One rule", &[&w.a], "two_way");
    edit(&mut r, "own", &w.a, "A holds 60 days.");
    land(&mut r, "own").unwrap();
    r.close().unwrap();
    assert_eq!(cli_reaffirm_refs(&srv), Vec::<String>::new());
}

/// The settle-fix shape: the view writes a decision, then edits the block after the decision was
/// written. One landing carries both, or the decision lands first and a later landing of the same
/// view carries the edit: no card in either case, and the suspect row stays.
#[test]
fn r59021_settle_fix_in_the_round_opens_no_card() {
    // one landing: the edit comes after the decision
    let (_d, mut r) = project();
    let w = world(&mut r);
    let d = decide_in(&mut r, "round", &w.s, "Round rule", &[&w.a], "two_way");
    edit(&mut r, "round", &w.a, "A holds 45 days.");
    edit(&mut r, "round", &w.a, "A holds 60 days.");
    land(&mut r, "round").unwrap();
    assert!(card(&r, &w.a).is_none(), "{:#?}", queue_items(&r));
    assert_eq!(suspect_targets(&r, &d), vec![w.a.clone()]);

    // a later landing from the same view
    let (_d, mut r) = project();
    let w = world(&mut r);
    let d = decide_in(&mut r, "round", &w.s, "Round rule", &[&w.a], "two_way");
    land(&mut r, "round").unwrap();
    assert!(card(&r, &w.a).is_none(), "no suspect yet");
    edit(&mut r, "round", &w.a, "A holds 45 days.");
    land(&mut r, "round").unwrap();
    edit(&mut r, "round", &w.a, "A holds 60 days.");
    land(&mut r, "round").unwrap();
    assert!(card(&r, &w.a).is_none(), "{:#?}", queue_items(&r));
    assert_eq!(suspect_targets(&r, &d), vec![w.a.clone()]);
}

/// A later edit of the block from another view opens the card, with its ref, text and options
/// as before: also when the same view's own landing came in between.
#[test]
fn r59021_another_landing_still_opens_the_card() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let d = decide_in(&mut r, "mine", &w.s, "Mine rule", &[&w.a], "two_way");
    land(&mut r, "mine").unwrap();
    edit(&mut r, "other", &w.a, "A holds 60 days.");
    land(&mut r, "other").unwrap();
    let c = card(&r, &w.a).expect("another view changed the block");
    assert_eq!(members(&c), vec![d.clone()]);
    assert!(
        c.r#ref.starts_with(&format!("reaffirm:{}@", w.a)),
        "{}",
        c.r#ref
    );
    assert_eq!(c.why, REAFFIRM_WHY);
    assert_eq!(c.members.len(), 1);
    assert_eq!(
        c.options
            .iter()
            .map(|o| (o.id.as_str(), o.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("o1", rdy_engine::pending::HOLD_LABEL),
            ("o2", rdy_engine::pending::KEEP_LABEL)
        ]
    );

    // the same view's edit first, then another view's: the other view's change opens it
    let (_d, mut r) = project();
    let w = world(&mut r);
    let d = decide_in(&mut r, "mine", &w.s, "Mine rule", &[&w.a], "two_way");
    land(&mut r, "mine").unwrap();
    edit(&mut r, "mine", &w.a, "A holds 45 days.");
    land(&mut r, "mine").unwrap();
    assert!(card(&r, &w.a).is_none(), "its own view's change");
    edit(&mut r, "other", &w.a, "A holds 60 days.");
    land(&mut r, "other").unwrap();
    assert_eq!(members(&card(&r, &w.a).expect("the card")), vec![d]);

    // a cause that two decisions rest on: the one landed with the edit stays off the card
    let (_d, mut r) = project();
    let w = world(&mut r);
    let early = decide_in(&mut r, "early", &w.s, "Early rule", &[&w.a], "two_way");
    land(&mut r, "early").unwrap();
    let own = decide_in(&mut r, "late", &w.s, "Late rule", &[&w.a], "two_way");
    edit(&mut r, "late", &w.a, "A holds 60 days.");
    land(&mut r, "late").unwrap();
    assert_eq!(
        members(&card(&r, &w.a).expect("the early decision's card")),
        vec![early.clone()]
    );
    assert_eq!(suspect_targets(&r, &own), vec![w.a.clone()]);
}

/// A `one_way` decision keeps its card when a later landing of the same view changed its block;
/// the same landing's own edit leaves it out like any other decision.
#[test]
fn r59021_one_way_keeps_its_card_after_a_later_landing() {
    let (_d, mut r) = project();
    let w = world(&mut r);
    let d = decide_in(&mut r, "ow", &w.s, "Never drop A", &[&w.a], "one_way");
    land(&mut r, "ow").unwrap();
    edit(&mut r, "ow", &w.a, "A holds 60 days.");
    land(&mut r, "ow").unwrap();
    let c = card(&r, &w.a).expect("a one_way decision keeps its card");
    assert_eq!(members(&c), vec![d]);

    // the same landing's own edit: no card
    let (_d, mut r) = project();
    let w = world(&mut r);
    decide_in(&mut r, "ow", &w.s, "Never drop A", &[&w.a], "one_way");
    edit(&mut r, "ow", &w.a, "A holds 60 days.");
    land(&mut r, "ow").unwrap();
    assert!(card(&r, &w.a).is_none(), "{:#?}", queue_items(&r));
}
