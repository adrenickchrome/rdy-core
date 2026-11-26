//! r5909 (D-8QV9G, D-KQ4CA, D-TXFHR): through the MCP tools, a refused call names a step that
//! works, and the step works. The refusals come from the shared engine; the parameter and session
//! refusals are the server's. The CLI side is in `cli_refusal_recovery`.
use rdy_engine::engine::Local;
use rdy_engine::{Rdy, RdyError};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::{
    ERROR_ROWS, IdParams, LinksParams, McpState, PlanParams, rdy_answer, rdy_context, rdy_pending,
    rdy_plan, rdy_session, rdy_write,
};
use rdy_qa::edit_loop::{spec_doc, stable_spec};
use rdy_qa::fixtures::project;
use rdy_qa::intake::batch_project;
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Map, Value, json};
use std::sync::Arc;

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn map(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn person_is_qa(dir: &std::path::Path) {
    std::fs::write(dir.join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
}

/// An MCP state on `r` with the agent session `agent:qa`, run `r1`.
fn agent_state(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:qa", "run": "r1", "role": "writer"})),
    )
    .unwrap();
    st
}

fn write(st: &mut McpState, ops: Value) -> Result<Value, RdyError> {
    rdy_write(st, p(json!({"view": "w", "ops": ops})))
}

fn triple(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

/// The refusal a tool's parameters get from the server's parser.
macro_rules! refused {
    ($params:ty, $tool:expr, $args:expr $(,)?) => {
        parse_params::<$params>($tool, map($args)).unwrap_err()
    };
}

/// r5909: through the MCP tools, each recovery text is the engine's or the server's, and the
/// corrected call succeeds. The CLI route of the same categories is pinned in
/// `cli_refusal_recovery`.
#[test]
fn r5909_mcp_and_cli_follow_refusal_fixes() {
    let (dir, mut r) = project();
    person_is_qa(dir.path());
    let spec = spec_doc(&mut r);
    let (a, b, sec) = (
        spec.a.to_string(),
        spec.b.to_string(),
        spec.section.to_string(),
    );
    let mut st = agent_state(r);

    // W-core-1: the act a reaffirm item does not take has no name; the item closes with a write
    let e = refused!(
        PlanParams,
        "rdy_plan",
        json!({"answers": [{"item": "pi-zz", "act": "reaffirm", "reason": "x"}]}),
    );
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        "rdy_plan: the parameters do not parse: unknown variant `reaffirm`, expected one of `confirm`, `defer`, `done`, `dismiss`"
    );
    assert_eq!(
        e.fix,
        "check each parameter against the tool's input schema (tools/list) A reaffirm item closes with an rdy_write reaffirm op."
    );
    // another wrong act keeps the plain fix
    let e = refused!(
        PlanParams,
        "rdy_plan",
        json!({"answers": [{"item": "pi-zz", "act": "approve", "reason": "x"}]})
    );
    assert_eq!(
        e.fix,
        "check each parameter against the tool's input schema (tools/list)"
    );
    // W-core-13: a stale item names the open count; the plan counts the same items
    write(
        &mut st,
        json!([{"op": "update", "id": a, "body": "A holds longer."}]),
    )
    .unwrap();
    let plan = rdy_plan(&mut st, p(json!({"view": "w"}))).unwrap();
    let open = plan["counts"]["open"].as_u64().unwrap();
    assert!(open > 0, "{plan:#}");
    let e = rdy_plan(
        &mut st,
        p(json!({"view": "w", "answers": [{"item": "pi-zz", "act": "defer", "reason": "x"}]})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PLAN_ITEM");
    assert_eq!(
        e.fix,
        format!("{} Open items: {open}.", rdy_engine::plan::PLAN_ITEM_FIX)
    );
    // W-core-1: a confirm item takes confirm or defer; the corrected call answers it
    let confirm = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "confirm")
        .expect("a confirm item")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let e = rdy_plan(
        &mut st,
        p(json!({"view": "w", "answers": [{"item": confirm, "act": "done"}]})),
    )
    .unwrap_err();
    assert_eq!(
        triple(&e),
        (
            "E_BAD_INPUT",
            "done does not answer a confirm item",
            "confirm items take: confirm or defer"
        )
    );
    rdy_plan(
        &mut st,
        p(json!({"view": "w", "answers": [{"item": confirm, "act": "confirm", "reason": "still true"}]})),
    )
    .unwrap();

    // W-core-3: a known wrong name is a hint only; no alias is accepted
    let pair = |tool: &str, field: &str, replacement: &str| {
        let args = json!({ field: "x" });
        let e = match tool {
            "rdy_answer" => refused!(rdy_mcp::tools::AnswerParams, tool, args),
            "rdy_write" => refused!(rdy_mcp::tools::WriteParams, tool, args),
            "rdy_schema" => refused!(rdy_mcp::tools::SchemaParams, tool, args),
            other => panic!("{other}"),
        };
        assert_eq!(e.message, format!("{tool}: unknown parameter \"{field}\""));
        assert!(
            e.fix.starts_with(&format!(
                "did you mean \"{replacement}\"? the parameters here are "
            )),
            "{field}: {}",
            e.fix
        );
    };
    pair("rdy_answer", "proposal", "ref");
    pair("rdy_answer", "card", "ref");
    pair("rdy_answer", "text", "evidence");
    pair("rdy_answer", "answer", "evidence");
    pair("rdy_answer", "words", "evidence");
    pair("rdy_write", "token", "brief");
    pair("rdy_schema", "type", "block_type");
    // an absent replacement is not suggested
    let e = refused!(
        rdy_mcp::tools::SchemaParams,
        "rdy_schema",
        json!({"words": "x"})
    );
    assert!(
        e.fix
            .starts_with("remove \"words\"; the parameters here are "),
        "{}",
        e.fix
    );
    // the op value `edit` has its own fix
    let e = write(&mut st, json!([{"op": "edit", "id": a, "body": "x"}])).unwrap_err();
    assert_eq!(
        e.fix,
        "use op update to edit an existing block; copy the op fields from rdy_schema"
    );
    // and the corrected call: op update
    write(
        &mut st,
        json!([{"op": "update", "id": a, "body": "A holds longest."}]),
    )
    .unwrap();

    // W-core-9: a block id on the links tool asks for a block's relations
    let e = refused!(LinksParams, "rdy_links", json!({"id": a}));
    assert_eq!(e.message, "rdy_links: unknown parameter \"id\"");
    assert_eq!(
        e.fix,
        "rdy_links reads links between projects. To read a block, use rdy_context {id}; use rdy_trace {id} for its decision links, or rdy_impact {id} for its dependents."
    );
    rdy_context(&mut st, p::<IdParams>(json!({"id": a}))).unwrap();

    // W-core-10: a placeholder belongs to its write
    let e = write(
        &mut st,
        json!([{"op": "create", "type": "claim", "parent": sec, "body": "Early {{$later}}."},
               {"op": "create", "type": "claim", "id": "$later", "parent": sec, "body": "Later."}]),
    )
    .unwrap_err();
    assert_eq!(
        triple(&e),
        (
            "E_BAD_INPUT",
            "placeholder $later is used before it is created",
            "create $later earlier in this write, then use it in a later op. A placeholder does not carry into another write; use the returned block id there."
        )
    );
    let made = write(
        &mut st,
        json!([{"op": "create", "type": "claim", "id": "$later", "parent": sec, "body": "Later."}]),
    )
    .unwrap();
    let later = made["created"]["$later"].as_str().unwrap().to_string();
    write(
        &mut st,
        json!([{"op": "create", "type": "claim", "parent": sec, "body": format!("Early {{{{{later}}}}}.")}]),
    )
    .unwrap();

    // W-core-11: a edited, so b's pin is stale and the repin works; the second finds it current
    write(&mut st, json!([{"op": "repin", "id": b, "targets": [a]}])).unwrap();
    let e = write(&mut st, json!([{"op": "repin", "id": b, "targets": [a]}])).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.fix,
        "read rdy_suspects and the block's own pins before retrying. Drop this repin from the write. A superseded or revoked target needs a changed citation; a reaffirm does not change the target's content version."
    );

    // W-core-16: null does not clear a field; unset does
    let e = write(
        &mut st,
        json!([{"op": "update", "id": spec.doc.to_string(), "fields": {"doc_type": null}}]),
    )
    .unwrap_err();
    assert!(
        e.issues.iter().any(|i| i.fix
            == "null does not clear a field; list it in the update op's unset: [\"doc_type\"]"),
        "{e:?}"
    );

    // W-core-23: the children go first, in the same write
    let e = write(&mut st, json!([{"op": "delete", "id": later}])).ok();
    assert!(e.is_some(), "a leaf deletes");
    let e = write(&mut st, json!([{"op": "delete", "id": sec}])).unwrap_err();
    assert_eq!(e.code, "E_HAS_CHILDREN");
    assert!(
        e.fix
            .starts_with("delete or move these children before their parent in the same write: "),
        "{}",
        e.fix
    );

    // W-core-25: no session
    let mut bare = McpState::new(Arc::clone(&st.engine));
    let e = rdy_write(&mut bare, p(json!({"view": "w", "ops": []}))).unwrap_err();
    assert_eq!(
        triple(&e),
        (
            "E_NO_SESSION",
            "no session",
            "state rdy/actor and rdy/run in _meta, or call rdy_session with your actor, run and role, then resend the call"
        )
    );
}

/// r5909: the cards of a batch: a wrong option lists the card's own options; an approval waits
/// for the exception cards, named in order; each card's own answer unlocks it.
#[test]
fn r5909_mcp_cards_name_their_options_and_exceptions() {
    let (dir, r, b) = batch_project();
    person_is_qa(dir.path());
    let mut st = agent_state(r);
    let q = rdy_pending(&mut st, p(json!({}))).unwrap();
    let item = |r#ref: &str| {
        q["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["ref"] == r#ref)
            .unwrap_or_else(|| panic!("{ref} in {q}"))
            .clone()
    };
    let words = |extra: Value| {
        let mut v =
            json!({"evidence": "Yes.", "question": "Shall I record it?", "session_ref": "chat-7"});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    // W-core-2: the batch's card
    let batch = item(&b.r#ref);
    let listed: Vec<String> = batch["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            format!(
                "{} ({})",
                o["id"].as_str().unwrap(),
                o["label"].as_str().unwrap()
            )
        })
        .collect();
    let e = rdy_answer(
        &mut st,
        p(words(
            json!({"ref": b.r#ref, "option": "o9", "card_hash": batch["card_hash"]}),
        )),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, format!("o9 is not an option of {}", b.r#ref));
    assert_eq!(
        e.fix,
        format!(
            "this approve card takes: {}. Copy an option id from the current card; put the person's exact words in evidence.",
            listed.join(", ")
        )
    );
    // W-core-14: an approval waits for the exception cards, in order, and records nothing
    let exceptions: Vec<Value> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "confirm" && i["batch"] == b.r#ref)
        .cloned()
        .collect();
    assert_eq!(exceptions.len(), 4);
    let e = rdy_answer(
        &mut st,
        p(words(
            json!({"ref": b.r#ref, "option": "approve", "card_hash": batch["card_hash"]}),
        )),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_EXCEPTION_OPEN", "{e}");
    let refs: Vec<&str> = e.corrected.as_ref().unwrap()["exceptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["ref"].as_str().unwrap())
        .collect();
    assert_eq!(
        e.fix,
        format!(
            "answer these exception cards first, in order: {}. Then relay the approval.",
            refs.join(", ")
        )
    );
    // each card's own answer unlocks the approval
    for c in &exceptions {
        rdy_answer(
            &mut st,
            p(words(
                json!({"ref": c["ref"], "option": "confirm", "card_hash": c["card_hash"]}),
            )),
        )
        .unwrap_or_else(|e| panic!("confirm {}: {e}", c["ref"]));
    }
    let q = rdy_pending(&mut st, p(json!({}))).unwrap();
    let batch = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == b.r#ref)
        .unwrap()
        .clone();
    rdy_answer(
        &mut st,
        p(words(
            json!({"ref": b.r#ref, "option": "approve", "card_hash": batch["card_hash"]}),
        )),
    )
    .unwrap();
    // the rows the schema lists carry the same fixed text
    let row = |code: &str| ERROR_ROWS.iter().find(|r| r.0 == code).unwrap().2;
    assert_eq!(
        row("E_EXCEPTION_OPEN"),
        "answer these exception cards first, in order: <refs>. Then relay the approval."
    );
    assert_eq!(
        row("E_DEFERRAL_OPEN"),
        "answer the deferral card first: <ref>. Then relay the approval."
    );
    assert_eq!(
        row("E_CONFLICT_UNRESOLVED"),
        "read the conflict with rdy_context; update an editable block with base, or take the landed side of a frozen decision with rdy_view resolve and landed true"
    );
}

/// r5909: a block that only an open view holds is named through the read and context tools, and
/// the corrected read, in that view's scope, returns it.
#[test]
fn r5909_mcp_missing_block_names_an_open_view() {
    let (dir, mut r) = project();
    person_is_qa(dir.path());
    let spec = spec_doc(&mut r);
    let mut st = agent_state(r);
    let made = write(
        &mut st,
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(), "body": "Only in w."}]),
    )
    .unwrap();
    let ghost = made["created"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let hint = "this id is in open view w; read with scope view:w";
    let e = rdy_context(&mut st, p(json!({"id": ghost, "scope": "canon"}))).unwrap_err();
    assert_eq!((e.code, e.fix.as_str()), ("E_UNKNOWN_BLOCK", hint), "{e}");
    let e =
        rdy_mcp::tools::rdy_read(&mut st, p(json!({"id": ghost, "scope": "canon"}))).unwrap_err();
    assert_eq!((e.code, e.fix.as_str()), ("E_UNKNOWN_BLOCK", hint), "{e}");
    rdy_context(&mut st, p(json!({"id": ghost, "scope": "view:w"}))).unwrap();
}

/// r5909: a write on stable content with no briefing is `E_BRIEF_REQUIRED` and carries the
/// token; the retry with the token succeeds. The editorial edit of an accepted decision's
/// tracking field is pinned beside it, in `r5909_tracking_edit_keeps_its_brief_code`.
#[test]
fn r5909_missing_brief_keeps_its_code_and_token() {
    let (_dir, mut r) = project();
    let spec = stable_spec(&mut r);
    let mut st = agent_state(r);
    let a = spec.a.to_string();
    let e = write(
        &mut st,
        json!([{"op": "update", "id": a, "body": "A holds, edited."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BRIEF_REQUIRED", "{e}");
    let token = e.briefing.as_ref().unwrap()["base"]["token"]
        .as_str()
        .expect("briefing.base.token")
        .to_string();
    let w = rdy_write(
        &mut st,
        p(json!({"view": "w", "brief": token,
                 "ops": [{"op": "update", "id": a, "body": "A holds, edited."}]})),
    );
    assert!(w.is_ok(), "{w:?}");
}

/// r5909: the tool descriptions say what the engine does: the write description names the
/// placeholder and unset rules, the answer description names the card rules, the links
/// description names the block-read tools.
#[test]
fn r5909_mcp_descriptions_carry_the_recovery_sentences() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let text = |name: &str| -> String {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name}"))["description"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let write = text("rdy_write");
    for s in [
        "To clear an optional or override field, put its name in unset. Null is not a field value.",
        "A $name placeholder belongs to this write. Create it before an op uses it. Across writes, use the returned block id. Give a new key before a later op uses it. A create and an update of that same block do not belong in one write.",
    ] {
        assert!(write.contains(s), "{s}");
    }
    assert!(text("rdy_answer").contains(
        "Read the current card before answering. Copy its option id into option. Put the person's exact words in evidence as one string, and the card ref in ref. Reaffirm and adopt cards take kind card, or no kind. Proposal and question cards may take kind text. Do not use a question's o-id on a proposal card."
    ));
    assert!(text("rdy_links").contains(
        "rdy_links manages links between projects. It does not read a block's relations. For a block use rdy_context, rdy_trace or rdy_impact."
    ));
    // a call with no session on the wire: the server's own refusal
    match c.call("rdy_overview", json!({})) {
        Reply::ToolError(v) => assert_eq!(
            v["fix"],
            "state rdy/actor and rdy/run in _meta, or call rdy_session with your actor, run and role, then resend the call"
        ),
        other => panic!("{other:?}"),
    }
}

/// A pack whose decisions have one tracking field, `disposition`.
const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

/// r5909: an editorial edit of an accepted decision's tracking field, under stable content, with
/// no briefing is `E_BRIEF_REQUIRED` with the token; the retry with it succeeds.
#[test]
fn r5909_tracking_edit_keeps_its_brief_code() {
    let (dir, r) = project();
    drop(r);
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let spec = stable_spec(&mut r);
    let mut st = agent_state(r);
    let decision = spec.decision.to_string();
    let mut edit = |brief: Option<&str>| {
        let mut args = json!({"view": "w",
            "editorial": {"class": "clarify", "reason": "release tracking"},
            "ops": [{"op": "update", "id": decision, "fields": {"disposition": "shipped"}}]});
        if let Some(b) = brief {
            args["brief"] = json!(b);
        }
        rdy_write(&mut st, p(args))
    };
    let e = edit(None).unwrap_err();
    assert_eq!(e.code, "E_BRIEF_REQUIRED", "{e}");
    let token = e.briefing.as_ref().unwrap()["base"]["token"]
        .as_str()
        .expect("briefing.base.token")
        .to_string();
    let done = edit(Some(&token));
    assert!(done.is_ok(), "{done:?}");
}
