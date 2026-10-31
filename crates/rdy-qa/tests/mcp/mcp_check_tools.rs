//! ck8 (D-JQD5X, D-HZYK0, D-8QCDH): the MCP and ctl surfaces of the authoring checks. `rdy_review`
//! lists the checks, runs one on the author's own work, requests an independent review, and lists
//! requests and verdicts; `rdy_review_next` and `rdy_review_submit` take the new kinds and
//! verdicts; `rdy_schema` lists every check with its stages. The tools call the engine's
//! `run_review`, so the ctl `review` op takes the same strict input.
use rdy_daemon::{OPS, OpContext, dispatch};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::signing_crypto::UnavailableAgent;
use rdy_engine::{Rdy, RdyError, Session};
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::*;
use rdy_model::Role;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{land, project, write};
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::try_decide_in;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const CHECKS: [&str; 8] = [
    "contradiction",
    "meaning_preservation",
    "implementation_fidelity",
    "decision_review",
    "entity_divergence",
    "prose_drift",
    "diff_contradicts",
    "confirmation",
];

/// A served project: the spec doc in canon and agent:a's proposed decision `p` in view `a`.
struct World {
    s: Served,
    spec: Spec,
    p: String,
}

fn world() -> World {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    let p = try_decide_in(&mut r, "a", &spec.section, "Cache the reads")
        .unwrap()
        .to_string();
    r.close().unwrap();
    World { s, spec, p }
}

/// A session of `actor` (run `run`) with `role`, through the daemon.
fn client(s: &Served, actor: &str, run: &str, role: &str) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    )
    .ok();
    c
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
    k.sort();
    k
}

#[test]
fn ck8_mcp_catalogue_and_schema_agree() {
    let w = world();
    let mut rd = client(&w.s, "agent:rd", "rd", "reader");
    // the catalogue: all eight checks, in order, with subjects and verdicts
    let catalogue = rd.call("rdy_review", json!({"op": "checks"})).ok();
    assert_eq!(keys(&catalogue), ["checks"]);
    let rows = catalogue["checks"].as_array().unwrap();
    let names: Vec<&str> = rows.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, CHECKS);
    for row in rows {
        assert_eq!(keys(row), ["name", "subject", "verdicts"]);
    }
    assert_eq!(
        rows[0]["verdicts"],
        json!(["tension", "no_tension"]),
        "contradiction keeps its verdicts"
    );
    for row in &rows[1..] {
        assert_eq!(row["verdicts"], json!(["pass", "concern", "fail"]));
    }

    // rdy_schema lists every check with its stages: the six deterministic rows, and the eight
    // agent rows with no stage
    let schema = rd.call("rdy_schema", json!({})).ok();
    let checks = &schema["checks"];
    assert_eq!(keys(checks), ["agent", "rules", "stages"]);
    assert_eq!(
        checks["stages"],
        json!([
            {"name": "tension_delta", "stages": ["change"]},
            {"name": "pack_rules", "stages": ["change", "propose"]},
            {"name": "readiness", "stages": ["propose"]},
            {"name": "suspects", "stages": ["land"]},
            {"name": "events", "stages": ["land"]},
            {"name": "rebases", "stages": ["land"]},
        ])
    );
    let agent = checks["agent"].as_array().unwrap();
    assert_eq!(agent.len(), 8);
    for (row, cat) in agent.iter().zip(rows) {
        assert_eq!(keys(row), ["name", "stages", "subject", "verdicts"]);
        assert_eq!(row["stages"], json!([]), "an agent check is in no stage");
        for k in ["name", "subject", "verdicts"] {
            assert_eq!(row[k], cat[k], "{k}");
        }
    }
    // confirmation (FW-X4) appears
    assert_eq!(
        agent[7],
        json!({
            "name": "confirmation",
            "subject": "a decision with confirmation text",
            "stages": [],
            "verdicts": ["pass", "concern", "fail"],
        })
    );
    assert_eq!(
        checks["rules"],
        json!({
            "max": 64,
            "grammar": "filter",
            "severity": ["advisory", "blocking"],
            "default_severity": "advisory",
        })
    );
    // the decision type's field list discovers confirmation from the schema
    let decision = rd
        .call("rdy_schema", json!({"block_type": "decision"}))
        .ok();
    assert_eq!(decision["fields"]["confirmation"]["required"], false);
    assert!(
        decision["fields"]["confirmation"]["kind"].is_string(),
        "{decision}"
    );
    // the old keys stay
    for key in [
        "pack",
        "types",
        "link_types",
        "areas",
        "link_shape",
        "ops",
        "views",
        "intents",
        "alternatives",
        "tensions",
        "reviews",
        "inbox",
        "decisions",
        "entities",
        "limits",
        "errors",
        "doc_types",
        "text",
        "edit_loop",
        "change_plan",
        "brief",
        "consequences",
        "signing",
        "diff",
        "filter",
    ] {
        assert!(!schema[key].is_null(), "rdy_schema lost {key}");
    }
}

#[test]
fn ck8_mcp_run_request_list_and_submit() {
    let w = world();
    let (p, c) = (w.p.clone(), w.spec.c.to_string());
    let mut a = client(&w.s, "agent:a", "a", "writer");
    let mut rv = client(&w.s, "agent:r", "r", "reviewer");
    let mut rd = client(&w.s, "agent:rd", "rd", "reader");

    // a reader lists but writes no request
    let denied = rd.call(
        "rdy_review",
        json!({"op": "run", "check": "decision_review", "subject": p}),
    );
    assert_eq!(denied.code(), Some("E_PERMISSION"));
    assert_eq!(
        rd.call("rdy_review", json!({"op": "list"})).ok()["requests"],
        json!([]),
        "a refused run wrote nothing"
    );

    // an author-run request: the case comes back at once
    let run = a
        .call(
            "rdy_review",
            json!({"op": "run", "check": "decision_review", "subject": p}),
        )
        .ok();
    assert_eq!(
        keys(&run),
        ["case", "case_hash", "omitted", "request", "requests"]
    );
    let run_label = run["request"].as_str().unwrap().to_string();
    assert!(
        run_label.starts_with("rq-") && run_label.len() == 3 + 64,
        "{run_label}"
    );
    assert_eq!(run["case"]["kind"], "decision_review");
    assert_eq!(run["omitted"], 0);
    assert_eq!(
        run["requests"],
        json!([{"request": run["request"], "case_hash": run["case_hash"], "case": run["case"]}])
    );

    // an independent request on the same decision, with a reason
    let asked = a
        .call(
            "rdy_review",
            json!({"op": "request", "check": "decision_review", "subject": p,
                   "reason": "  Check the basis.  "}),
        )
        .ok();
    assert_eq!(
        keys(&asked),
        ["case", "case_hash", "omitted", "request", "requests"]
    );
    let asked_label = asked["request"].as_str().unwrap().to_string();
    assert_ne!(asked_label, run_label);
    // the same case: its hash is the run's
    assert_eq!(asked["case_hash"], run["case_hash"]);

    // the list: exact row keys, both modes, both open
    let all = rd.call("rdy_review", json!({"op": "list"})).ok();
    assert_eq!(keys(&all), ["omitted", "requests"]);
    assert_eq!(all["omitted"], 0);
    let rows = all["requests"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let row_of = |label: &str| rows.iter().find(|r| r["request"] == label).unwrap().clone();
    let (author_row, indep_row) = (row_of(&run_label), row_of(&asked_label));
    for row in [&author_row, &indep_row] {
        assert_eq!(
            keys(row),
            [
                "case_hash",
                "check",
                "mode",
                "reason",
                "request",
                "requested_by",
                "scope",
                "status",
                "subject",
                "verdicts"
            ]
        );
        assert_eq!(row["status"], "open");
        assert_eq!(row["check"], "decision_review");
        assert_eq!(row["verdicts"], json!([]));
        assert_eq!(row["requested_by"], json!({"actor": "agent:a", "run": "a"}));
        assert_eq!(
            row["subject"],
            json!({"blocks": [p], "change": null, "version": null, "code": null})
        );
    }
    assert_eq!(author_row["mode"], "author");
    assert_eq!(author_row["reason"], Value::Null);
    assert_eq!(indep_row["mode"], "independent");
    assert_eq!(indep_row["reason"], "Check the basis.");
    // a subject filter and the open flag
    let by_subject = rd
        .call("rdy_review", json!({"op": "list", "subject": p}))
        .ok();
    assert_eq!(by_subject["requests"].as_array().unwrap().len(), 2);
    let other = rd
        .call("rdy_review", json!({"op": "list", "subject": c}))
        .ok();
    assert_eq!(other["requests"], json!([]));
    let open = rd
        .call("rdy_review", json!({"op": "list", "open": true}))
        .ok();
    assert_eq!(open["requests"].as_array().unwrap().len(), 2);

    // the reviewer claims the independent request, never the author-run one
    let next = rv
        .call("rdy_review_next", json!({"kind": "decision_review"}))
        .ok();
    assert_eq!(
        keys(&next),
        ["case", "case_hash", "kind", "lease_expires_at", "request"]
    );
    assert_eq!(next["request"], asked_label.as_str());
    assert_eq!(next["kind"], "decision_review");
    assert_eq!(
        rv.call("rdy_review_next", json!({"kind": "decision_review"}))
            .ok()["request"],
        Value::Null,
        "the author-run request is nobody else's to claim"
    );
    // an independent concern needs a reason and citations
    let bare = rv.call(
        "rdy_review_submit",
        json!({"request": next["request"], "case_hash": next["case_hash"], "verdict": "concern"}),
    );
    assert_eq!(bare.code(), Some("E_REVIEW_INVALID"));
    let concern = rv
        .call(
            "rdy_review_submit",
            json!({"request": next["request"], "case_hash": next["case_hash"],
                   "verdict": "concern",
                   "citations": [{"block": p, "note": "the basis is thin"}],
                   "reason": "The decision names no measure."}),
        )
        .ok();
    assert_eq!(keys(&concern), ["request", "review", "verdict"]);
    assert_eq!(concern["request"], asked_label.as_str());
    assert_eq!(concern["verdict"], "concern");
    // the author-run request is answered by its own actor only
    let refused = rv.call(
        "rdy_review_submit",
        json!({"request": run_label, "case_hash": run["case_hash"], "verdict": "pass"}),
    );
    assert_eq!(refused.code(), Some("E_PERMISSION"));
    let pass = a
        .call(
            "rdy_review_submit",
            json!({"request": run_label, "case_hash": run["case_hash"], "verdict": "pass"}),
        )
        .ok();
    assert_eq!(keys(&pass), ["request", "review", "verdict"]);
    assert_eq!(pass["verdict"], "pass");

    // the list shows both verdicts, with their exact keys
    let after = rd.call("rdy_review", json!({"op": "list"})).ok();
    let rows = after["requests"].as_array().unwrap();
    let row_of = |label: &str| rows.iter().find(|r| r["request"] == label).unwrap().clone();
    let (author_row, indep_row) = (row_of(&run_label), row_of(&asked_label));
    assert_eq!(author_row["status"], "answered");
    assert_eq!(indep_row["status"], "answered");
    let verdict = &indep_row["verdicts"][0];
    assert_eq!(
        keys(verdict),
        [
            "actor",
            "case_hash",
            "citations",
            "reason",
            "review",
            "run",
            "verdict"
        ]
    );
    assert_eq!(verdict["review"], concern["review"]);
    assert_eq!(verdict["verdict"], "concern");
    assert_eq!(verdict["actor"], "agent:r");
    assert_eq!(verdict["run"], "r");
    assert_eq!(verdict["reason"], "The decision names no measure.");
    assert_eq!(
        verdict["citations"],
        json!([{"block": p, "note": "the basis is thin"}])
    );
    assert_eq!(verdict["case_hash"], run["case_hash"]);
    let own = &author_row["verdicts"][0];
    assert_eq!(own["verdict"], "pass");
    assert_eq!(own["actor"], "agent:a");
    assert_eq!(own["reason"], Value::Null);
    assert_eq!(own["citations"], json!([]));
    let open = rd
        .call("rdy_review", json!({"op": "list", "open": true}))
        .ok();
    assert_eq!(open["requests"], json!([]), "both are answered");
}

/// The three descriptions of O15, the guide paragraph, the instruction and the error rows.
#[test]
fn ck8_tool_and_error_texts_are_exact() {
    let w = world();
    let mut rd = client(&w.s, "agent:rd", "rd", "reader");
    let tools = rd.tools();
    let description = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not listed"))["description"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        description("rdy_review"),
        "List the checks, run one on your work, request an independent review, or list requests and verdicts. A run records an author-run request and returns its case. When you request a review for a person, pass their evidence. Checks inform the approval card; only a person's acceptance can make a check verdict block a version."
    );
    assert_eq!(
        description("rdy_review_next"),
        "Take the next independent review request of kind tension, a named check, or any. The reply gives its case and case_hash. The request is leased to this session under reviews.lease_ttl. Needs the reviewer role. Answer with rdy_review_submit."
    );
    assert_eq!(
        description("rdy_review_submit"),
        "Answer the case you read. A contradiction takes tension or no_tension. Other checks take pass, concern or fail. Concern and fail need citations and a reason. Independent reviews need another actor with the reviewer role. Only the requesting actor answers an author-run check."
    );

    // the guide paragraph is appended, not replaced
    let guide = rdy_engine::semantic::api::REVIEWS_DOC;
    assert!(
        guide.starts_with("rdy_review_next {kind: tension} and rdy_review_submit {request, case_hash, verdict, citations?, severity?, reason?} are the review queue."),
        "{guide}"
    );
    assert!(
        guide.ends_with(" Use rdy_review {op: \"checks\"} to list authoring checks. Use run for your own check, or request for an independent review. A person can accept a concern or fail as blocking on the approval card. A new proposal version does not inherit that acceptance."),
        "{guide}"
    );
    assert!(
        guide.ends_with("Pending requests never block a landing. Use rdy_review {op: \"checks\"} to list authoring checks. Use run for your own check, or request for an independent review. A person can accept a concern or fail as blocking on the approval card. A new proposal version does not inherit that acceptance."),
        "the old text stays directly before the new paragraph: {guide}"
    );
    let schema = rd.call("rdy_schema", json!({})).ok();
    assert_eq!(schema["reviews"], guide);

    // the one new server instruction
    let instruction = "Run an authoring check only when the author or a person asks. Show its verdict on the current approval card before relaying a person's answer.";
    let sentences = rdy_mcp::server::INSTRUCTION_SENTENCES;
    assert_eq!(sentences.iter().filter(|s| **s == instruction).count(), 1);
    assert_eq!(sentences.last(), Some(&instruction));
    assert!(rdy_mcp::server::INSTRUCTIONS.ends_with(instruction));
    assert!(
        !rdy_mcp::server::INSTRUCTIONS.contains("execute a reviewer"),
        "nothing tells an agent to run a reviewer by itself"
    );

    // the three new rows, in the engine's order, directly after E_ALT_CHOSEN
    let rows = rdy_mcp::tools::ERROR_ROWS;
    let at = rows.iter().position(|r| r.0 == "E_ALT_DECISION").unwrap();
    assert_eq!(rows[at - 1].0, "E_ALT_CHOSEN");
    assert_eq!(
        rows[at..at + 3],
        [
            (
                "E_ALT_DECISION",
                "a choice has no unique proposed decision for its considered options",
                "answer the set's question with one decision, or create exactly one decision: rdy_decide / rdy new decision",
            ),
            (
                "E_RULE_CHECK",
                "a blocking pack rule fails on stable content: propose makes it a readiness issue, and a change reports it in on_change without refusing",
                "follow the rule's fix, then propose again",
            ),
            (
                "W_RULE_CHECK",
                "an advisory pack rule fails, or a blocking rule fails outside stable content",
                "follow the rule's fix",
            ),
        ]
    );
    // the E_REVIEW_INVALID cell keeps its text and gains one sentence
    let invalid = rows.iter().find(|r| r.0 == "E_REVIEW_INVALID").unwrap();
    assert!(
        invalid.1.starts_with("rdy_review_submit: the verdict fails its schema (a verdict other than tension or no_tension, a citation on another block, an empty or over-long note, a tension with no citation, more than 20 citations, a severity other than advisory or blocking, advice on a no_tension, advice to block without a reason)"),
        "{invalid:?}"
    );
    assert!(
        invalid.1.ends_with(
            "Check verdicts take pass, concern or fail; concern and fail need a reason and citations."
        ),
        "{invalid:?}"
    );
    assert_eq!(invalid.2, "send the corrected payload");
    // one row per code; rdy_schema reports the rows
    let mut codes: Vec<&str> = rows.iter().map(|r| r.0).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), rows.len(), "one row per code");
    for code in ["E_ALT_DECISION", "E_RULE_CHECK", "W_RULE_CHECK"] {
        assert!(schema["errors"][code]["when"].is_string(), "{code}");
        assert!(schema["errors"][code]["fix"].is_string(), "{code}");
    }
}

fn p<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// One engine, read by the MCP tools and by the ctl `review` op.
struct Both {
    _dir: tempfile::TempDir,
    root: PathBuf,
    eng: Arc<Local>,
    a: McpState,
    rd: McpState,
    p: String,
}

fn mcp_session(eng: &Arc<Local>, actor: &str, run: &str, role: &str) -> McpState {
    let mut st = McpState::new(eng.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": actor, "run": run, "role": role})),
    )
    .unwrap();
    st
}

fn both() -> Both {
    let (dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let decision = try_decide_in(&mut r, "a", &spec.section, "Cache the reads")
        .unwrap()
        .to_string();
    let eng = Arc::new(Local::new(r));
    Both {
        root: dir.path().to_path_buf(),
        a: mcp_session(&eng, "agent:a", "a", "writer"),
        rd: mcp_session(&eng, "agent:rd", "rd", "reader"),
        eng,
        _dir: dir,
        p: decision,
    }
}

/// The tool's reply to `args`, for one session.
fn tool(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_review(st, p(args))
}

/// The ctl op's reply to `args`, for one session.
fn ctl(b: &Both, session: &Session, op: &str, args: Value) -> Result<Value, RdyError> {
    let cx = OpContext {
        engine: &*b.eng,
        session,
        human: "human:qa",
        cwd: &b.root,
        agent: &UnavailableAgent,
    };
    dispatch(&cx, op, args)
}

fn triple(r: Result<Value, RdyError>) -> (String, String, String) {
    let e = r.expect_err("a refusal");
    (e.code.to_string(), e.message, e.fix)
}

fn agent_session(actor: &str, run: &str, role: Role) -> Session {
    Session::agent(actor.into(), Some(run.into()), role).unwrap()
}

#[test]
fn ck8_mcp_and_ctl_use_identical_strict_inputs() {
    let mut b = both();
    let writer = agent_session("agent:a", "a", Role::Writer);
    let reader = agent_session("agent:rd", "rd", Role::Reader);
    let pid = b.p.clone();
    let bad = |message: &str, fix: &str| {
        (
            "E_BAD_INPUT".to_string(),
            message.to_string(),
            fix.to_string(),
        )
    };
    let shape = "subject must be one block, change or P-x@n, two block ids, or {decision, code}";

    // each op refuses the keys of the others, with one refusal on both surfaces
    for (args, want) in [
        (
            json!({"op": "checks", "check": "decision_review"}),
            bad(
                "rdy_review: unknown field \"check\"",
                "remove \"check\"; it takes no fields",
            ),
        ),
        (
            json!({"op": "run", "check": "decision_review", "subject": pid, "reason": "why"}),
            bad(
                "rdy_review: unknown field \"reason\"",
                "remove \"reason\"; the fields are check, subject",
            ),
        ),
        (
            json!({"op": "run", "check": "decision_review", "subject": pid, "evidence": "e"}),
            bad(
                "rdy_review: unknown field \"evidence\"",
                "remove \"evidence\"; the fields are check, subject",
            ),
        ),
        (
            json!({"op": "list", "check": "decision_review"}),
            bad(
                "rdy_review: unknown field \"check\"",
                "remove \"check\"; the fields are subject, open",
            ),
        ),
        (
            json!({"op": "list", "open": true, "question": "q?"}),
            bad(
                "rdy_review: unknown field \"question\"",
                "remove \"question\"; the fields are subject, open",
            ),
        ),
        // the strict code-subject object
        (
            json!({"op": "list", "subject": {"decision": "x", "code": "app@a..b", "repo": "app"}}),
            bad(
                "rdy_review: unknown field \"subject.repo\"",
                "remove \"repo\"; the fields are decision, code",
            ),
        ),
        // a pair holds two ids; any other array or a number is no subject
        (
            json!({"op": "list", "subject": ["one"]}),
            bad(
                &format!("rdy_review: {shape}"),
                "args for rdy_review: {op: \"checks\"} | {op: \"run\", check, subject} | {op: \"request\", check, subject, reason?, evidence?, question?, session_ref?} | {op: \"list\", subject?, open?}",
            ),
        ),
        (
            json!({"op": "list", "subject": ["a", "b", "c"]}),
            bad(
                &format!("rdy_review: {shape}"),
                "args for rdy_review: {op: \"checks\"} | {op: \"run\", check, subject} | {op: \"request\", check, subject, reason?, evidence?, question?, session_ref?} | {op: \"list\", subject?, open?}",
            ),
        ),
        (
            json!({"op": "list", "subject": 5}),
            bad(
                &format!("rdy_review: {shape}"),
                "args for rdy_review: {op: \"checks\"} | {op: \"run\", check, subject} | {op: \"request\", check, subject, reason?, evidence?, question?, session_ref?} | {op: \"list\", subject?, open?}",
            ),
        ),
    ] {
        let from_tool = triple(tool(&mut b.a, args.clone()));
        let from_ctl = triple(ctl(&b, &writer, "review", args.clone()));
        assert_eq!(from_tool, want, "tool {args}");
        assert_eq!(from_ctl, want, "ctl {args}");
    }
    // an op the engine does not know is the engine's refusal on both
    let bogus = json!({"op": "bogus"});
    assert_eq!(
        triple(tool(&mut b.a, bogus.clone())),
        triple(ctl(&b, &writer, "review", bogus))
    );

    // a typo is caught first by each surface's own key list, and both name the nearest key
    let typo = |args: Value| {
        parse_params::<ReviewParams>("rdy_review", args.as_object().unwrap().clone())
            .expect_err("a typo is refused")
    };
    let e = typo(json!({"op": "run", "chek": "decision_review", "subject": pid}));
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, "rdy_review: unknown parameter \"chek\"");
    assert!(e.fix.starts_with("did you mean \"check\"?"), "{e:?}");
    let (code, message, fix) = triple(ctl(
        &b,
        &writer,
        "review",
        json!({"op": "run", "chek": "decision_review", "subject": pid}),
    ));
    assert_eq!(code, "E_BAD_INPUT");
    assert_eq!(message, "rdy_review: unknown field \"chek\"");
    assert!(fix.starts_with("did you mean \"check\"?"), "{fix}");
    let e = typo(json!({"op": "list", "opem": true}));
    assert!(e.fix.starts_with("did you mean \"open\"?"), "{e:?}");
    // the tool has no relay key of its own: the session is the relay
    let e = typo(
        json!({"op": "request", "check": "decision_review", "subject": pid, "relayed_by": "agent:a"}),
    );
    assert_eq!(e.message, "rdy_review: unknown parameter \"relayed_by\"");
    // the ctl op admits relayed_by on request only
    let (code, message, _) = triple(ctl(
        &b,
        &writer,
        "review",
        json!({"op": "run", "check": "decision_review", "subject": pid, "relayed_by": "agent:a"}),
    ));
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("E_BAD_INPUT", "rdy_review: unknown field \"relayed_by\"")
    );
    let (_, message, _) = triple(ctl(
        &b,
        &writer,
        "review",
        json!({"op": "request", "check": "decision_review", "subject": pid, "relayed_by": "agent:a"}),
    ));
    assert!(
        !message.contains("unknown field"),
        "request takes relayed_by, and the relay table refuses half a relay: {message}"
    );
    let relayed = ctl(
        &b,
        &writer,
        "review",
        json!({"op": "request", "check": "decision_review", "subject": pid,
               "relayed_by": "agent:a", "evidence": "please review the cache decision"}),
    )
    .unwrap();
    let listed = tool(&mut b.rd, json!({"op": "list"})).unwrap();
    let row = listed["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request"] == relayed["request"])
        .unwrap();
    assert_eq!(row["requested_by"]["actor"], "human:qa", "{row}");
    assert_eq!(row["mode"], "independent");

    // the queue ops are tools only: the ctl has no review_next and no review_submit
    for op in ["review_next", "review_submit"] {
        let (code, message, fix) = triple(ctl(&b, &writer, op, json!({})));
        assert_eq!(code, "E_BAD_INPUT");
        assert_eq!(message, format!("unknown op \"{op}\""));
        assert!(fix.contains("review") && !fix.contains(op), "{fix}");
        assert!(!OPS.contains(&op), "{op}");
    }
    assert!(OPS.contains(&"review"));
    // the required kind stays required
    let e = parse_params::<ReviewNextParams>("rdy_review_next", serde_json::Map::new())
        .expect_err("kind is required");
    assert_eq!(e.message, "rdy_review_next: parameter \"kind\" is required");
    assert_eq!(e.fix, "pass \"kind\" as a string");
    // the old findings key is refused with the key list, and every submit key stays
    let e = parse_params::<ReviewSubmitParams>(
        "rdy_review_submit",
        json!({"request": "rq-1", "case_hash": "0", "verdict": "pass", "findings": []})
            .as_object()
            .unwrap()
            .clone(),
    )
    .expect_err("findings is no key");
    assert_eq!(
        e.message,
        "rdy_review_submit: unknown parameter \"findings\""
    );
    assert_eq!(
        e.fix,
        "remove \"findings\"; the parameters here are case_hash, citations, reason, request, severity, verdict"
    );

    // a reader lists, and cannot create, on both surfaces
    for args in [json!({"op": "checks"}), json!({"op": "list"})] {
        let from_tool = tool(&mut b.rd, args.clone()).unwrap();
        let from_ctl = ctl(&b, &reader, "review", args).unwrap();
        assert_eq!(from_tool, from_ctl);
    }
    for op in ["run", "request"] {
        let args = json!({"op": op, "check": "decision_review", "subject": pid});
        let want = (
            "E_PERMISSION".to_string(),
            "role Reader cannot write".to_string(),
            "start the session as a writer".to_string(),
        );
        assert_eq!(triple(tool(&mut b.rd, args.clone())), want, "{op}");
        assert_eq!(triple(ctl(&b, &reader, "review", args)), want, "{op}");
    }
}

/// The text a request and a verdict quote.
const SECRET: &str = "ZEBRA-7731";

#[test]
fn ck8_carriers_reach_adapter_dump_and_redaction() {
    // the first product writers: the tools write a request, a verdict and an author-run verdict
    let (_dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let decision = try_decide_in(&mut r, "a", &spec.section, "Cache the reads")
        .unwrap()
        .to_string();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Launch", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let secret_block = w.created["$p"].to_string();
    let eng = Arc::new(Local::new(r));
    let mut a = mcp_session(&eng, "agent:a", "a", "writer");
    let mut rv = mcp_session(&eng, "agent:r", "r", "reviewer");
    let run = tool(
        &mut a,
        json!({"op": "run", "check": "decision_review", "subject": decision}),
    )
    .unwrap();
    let asked = tool(
        &mut a,
        json!({"op": "request", "check": "decision_review", "subject": decision,
               "reason": format!("Does this follow from {SECRET}?")}),
    )
    .unwrap();
    let next = rdy_review_next(&mut rv, p(json!({"kind": "decision_review"}))).unwrap();
    assert_eq!(next["request"], asked["request"]);
    let concern = rdy_review_submit(
        &mut rv,
        p(
            json!({"request": next["request"], "case_hash": next["case_hash"],
                 "verdict": "concern",
                 "citations": [{"block": decision, "note": format!("it quotes {SECRET}")}],
                 "reason": format!("{SECRET} is not a measure.")}),
        ),
    )
    .unwrap();
    rdy_review_submit(
        &mut a,
        p(json!({"request": run["request"], "case_hash": run["case_hash"], "verdict": "pass"})),
    )
    .unwrap();
    let listed = tool(&mut a, json!({"op": "list"})).unwrap();
    assert_eq!(listed["requests"].as_array().unwrap().len(), 2);

    // no landing carries a check: LandingRecord.checks holds redaction requests only
    let landings = eng.read().reader().landings().unwrap();
    assert!(!landings.is_empty());
    for (h, rec) in &landings {
        assert!(rec.checks.is_empty(), "landing {h} carries a check");
    }

    // dump and load: the records come back, request, verdict and metadata alike
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    eng.read().dump(&file).unwrap();
    let to = scratch.path().join("loaded");
    rdy_engine::load::load(&file, &to).unwrap();
    let loaded = Arc::new(Local::open(&to).unwrap());
    let mut again = mcp_session(&loaded, "agent:rd", "rd", "reader");
    assert_eq!(
        tool(&mut again, json!({"op": "list"})).unwrap(),
        listed,
        "a loaded project lists the same requests and verdicts"
    );
    assert!(
        loaded
            .read()
            .reader()
            .landings()
            .unwrap()
            .iter()
            .all(|(_, rec)| rec.checks.is_empty()),
        "a load adds no landing check"
    );

    // the redaction report finds the quote in the request reason, the verdict reason and its
    // citation, and never offers them for removal
    let dry = rdy_redact(
        &mut a,
        p(
            json!({"targets": [secret_block], "reason_class": "secret", "reason": "a leaked code",
                 "dry_run": true}),
        ),
    )
    .unwrap();
    let quotes = dry["report"]["quotes"].as_array().unwrap();
    let request_hash = asked["request"].as_str().unwrap().trim_start_matches("rq-");
    let review_hash = concern["review"].as_str().unwrap();
    for (kind, record) in [("check_request", request_hash), ("review", review_hash)] {
        let rows: Vec<&Value> = quotes
            .iter()
            .filter(|q| q["kind"] == kind && q["record"] == record)
            .collect();
        assert!(!rows.is_empty(), "no {kind} quote of {record}: {quotes:?}");
        for q in rows {
            assert_eq!(q["quote"], SECRET);
            assert_eq!(q["erasable"], false, "{q}");
        }
    }
    let inside = dry["report"]["inside"].as_array().unwrap();
    for (kind, record) in [("check_request", request_hash), ("review", review_hash)] {
        let row = inside
            .iter()
            .find(|i| i["kind"] == kind && i["record"] == record)
            .unwrap_or_else(|| panic!("no {kind} row for {record}: {inside:?}"));
        assert_eq!(row["relation"], "quotes");
        assert_eq!(row["erasable"], false, "{row}");
    }
    // the author-run request quotes nothing
    let run_hash = run["request"].as_str().unwrap().trim_start_matches("rq-");
    assert!(
        !quotes.iter().any(|q| q["record"] == run_hash),
        "the author-run request has no reason"
    );
}
