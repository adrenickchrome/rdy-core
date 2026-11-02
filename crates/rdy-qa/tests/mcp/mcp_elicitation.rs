//! ui10 (D-6M0AF, D-WPE4R, D-DKJS5, D-QVH4N, D-1Z951; TASK-982 O4, O5, O9): `rdy_answer` with
//! `elicit: true` asks the client to put rdy's card to the person. Every test speaks JSON-RPC
//! lines to a real rmcp server over a duplex stream, with the capabilities it declares, so the
//! malformed replies a typed client could not send are sent too. The registry's clock is
//! injected; nothing waits on the wall clock.
use ed25519_dalek::Signer as _;
use rdy_engine::answer::presented_item;
use rdy_engine::engine::{Engine, Local, WriteFn, with_writer_result};
use rdy_engine::pending::{ItemKind, PendingQuery};
use rdy_engine::semantic::lease::LeaseRegistry;
use rdy_engine::signing_crypto::{AgentIdentity, SigningAgent, UnavailableAgent};
use rdy_engine::{
    BriefRegistry, DecisionInput, OpenReport, ProposalInfo, Rdy, RdyError, ReadState, Scope,
    Session, pin_clock,
};
use rdy_mcp::elicitation::{Bound, EXPIRY, MAX_WAITING, Registry};
use rdy_mcp::notify::NotifyHub;
use rdy_mcp::params::parse_params;
use rdy_mcp::server::{RdyServer, serve};
use rdy_mcp::tools::AnswerParams;
use rdy_model::plan::PlanKind;
use rdy_model::{AnswerAct, BlockId, EvidenceKind, Role};
use rdy_qa::edit_loop::{brief_of, spec_doc, write_cited};
use rdy_qa::fixtures::{approver, land, seed_doc, write, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY, batch_project};
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{ag, created, j_typo, put_j, upd};
use serde_json::{Value, json};
use ssh_key::sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

const V_NEW: &str = "2026-07-28";
const V_NOV: &str = "2025-11-25";
const V_JUN: &str = "2025-06-18";
const V_MAR: &str = "2025-03-26";

const MIXED: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "elicit takes only ref, card_hash and brief",
    "pass ref, card_hash, elicit: true and an optional brief, or relay an answer with option and evidence",
);
const GONE: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "this elicited answer is no longer available",
    "read rdy_pending, then ask again with rdy_answer and elicit: true",
);
const INVALID: (&str, &str, &str) = (
    "E_BAD_INPUT",
    "the client returned an invalid elicited answer",
    "ask again with elicit: true, or show the card in chat and relay the person's answer",
);
const UNAVAILABLE: (&str, &str, &str) = (
    "E_ELICITATION_UNAVAILABLE",
    "this client cannot show rdy's card itself",
    "show the card in chat and relay the person's answer: rdy_answer with option and evidence",
);
const NOTE_FIX: &str = "show the card in chat and relay the answer with a note: rdy_answer with option, note and evidence";

// ---------------------------------------------------------------- the protocol client

/// A form capability, as a 2025-11-25 client declares it.
fn form_caps() -> Value {
    json!({"elicitation": {"form": {}}})
}

/// A project, its hub and the signer of its sessions.
struct World {
    dir: tempfile::TempDir,
    engine: Arc<dyn Engine>,
    hub: Arc<NotifyHub>,
    agent: Arc<dyn SigningAgent>,
}

impl World {
    fn new(dir: tempfile::TempDir, e: Local) -> World {
        World {
            dir,
            engine: Arc::new(e),
            hub: NotifyHub::new(),
            agent: Arc::new(UnavailableAgent),
        }
    }

    fn seq(&self) -> rdy_engine::Location {
        self.engine.read().seq()
    }

    /// The card of `r` in the person's complete queue.
    fn card(&self, r: &str) -> rdy_engine::pending::Item {
        presented_item(&self.engine.read(), PERSON, r, None)
            .unwrap_or_else(|e| panic!("the card of {r}: {e:#?}"))
    }

    /// The stored answer of `key`, when one was written.
    fn answer(&self, key: &str) -> Option<rdy_model::Answer> {
        self.engine
            .read()
            .reader()
            .answer_of(key)
            .expect("answer_of")
            .map(|(_, a)| a)
    }

    /// Every snooze the published state holds.
    fn snoozes(&self) -> Vec<rdy_model::Answer> {
        let st = self.engine.read();
        st.reader()
            .answers_with_prefix("pending/")
            .expect("snoozes")
            .into_iter()
            .map(|(_, _, a)| a)
            .collect()
    }

    /// A client on this world's hub, under session id `sid`.
    fn stream(&self, sid: u64) -> DuplexStream {
        let (client, end) = tokio::io::duplex(1 << 20);
        let (r, w) = tokio::io::split(end);
        let server = RdyServer::new(Arc::clone(&self.engine), Some(sid), Arc::clone(&self.hub))
            .with_agent(Arc::clone(&self.agent));
        tokio::spawn(async move {
            let _ = serve(server, r, w).await;
        });
        client
    }
}

/// One JSON-RPC connection. `inline` is the client context each request states (2026-07-28);
/// `claim` is the actor and run each call states.
struct Client {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    next: u64,
    inline: Option<Value>,
    claim: Value,
}

impl Client {
    fn over(stream: DuplexStream, inline: Option<Value>) -> Client {
        let (r, w) = tokio::io::split(stream);
        Client {
            r: BufReader::new(r),
            w,
            next: 1,
            inline,
            claim: json!({"rdy/actor": "agent:relay", "rdy/run": "r1"}),
        }
    }

    /// A 2026-07-28 client: no `initialize`, its context in every request.
    fn inline(w: &World, sid: u64, caps: Value) -> Client {
        Client::over(
            w.stream(sid),
            Some(json!({
                "io.modelcontextprotocol/protocolVersion": V_NEW,
                "io.modelcontextprotocol/clientCapabilities": caps,
                "io.modelcontextprotocol/clientInfo": {"name": "ui10", "version": "0"},
            })),
        )
    }

    /// A client of revision `version`, after its `initialize`.
    async fn legacy(w: &World, sid: u64, version: &str, caps: Value) -> Client {
        let mut c = Client::over(w.stream(sid), None);
        c.send(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": version, "capabilities": caps,
            "clientInfo": {"name": "ui10", "version": "0"}}}),
        )
        .await;
        let init = c.recv().await;
        assert_eq!(init["result"]["protocolVersion"], version, "{init}");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        c
    }

    fn with_actor(mut self, actor: &str, run: Option<&str>) -> Client {
        self.claim = json!({"rdy/actor": actor, "rdy/run": run});
        self
    }

    async fn send(&mut self, v: Value) {
        let line = format!("{v}\n");
        self.w.write_all(line.as_bytes()).await.unwrap();
        self.w.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.r.read_line(&mut line).await.unwrap();
        assert!(n > 0, "the session closed");
        serde_json::from_str(&line).unwrap()
    }

    fn meta(&self) -> Value {
        let mut meta = self.claim.as_object().unwrap().clone();
        if let Some(i) = &self.inline {
            meta.extend(i.as_object().unwrap().clone());
        }
        Value::Object(meta)
    }

    /// Sends one `rdy_answer` call with `extra` beside its arguments. Its request id.
    async fn start(&mut self, args: Value, extra: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let mut params = json!({"name": "rdy_answer", "arguments": args, "_meta": self.meta()});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params}))
            .await;
        id
    }

    /// One `rdy_answer` call's whole `result`.
    async fn answer(&mut self, args: Value, extra: Value) -> Value {
        let id = self.start(args, extra).await;
        let msg = self.recv().await;
        assert_eq!(msg["id"], id, "{msg}");
        msg["result"].clone()
    }

    /// Sends one `rdy_answer` call and returns the `elicitation/create` that comes back.
    async fn asked(&mut self, args: Value) -> (u64, Value) {
        let id = self.start(args, json!({})).await;
        let req = self.recv().await;
        assert_eq!(req["method"], "elicitation/create", "{req}");
        (id, req)
    }

    /// Answers the `elicitation/create` `req` and reads the reply of call `id`.
    async fn respond(&mut self, id: u64, req: &Value, result: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": result}))
            .await;
        let msg = self.recv().await;
        assert_eq!(msg["id"], id, "{msg}");
        msg["result"].clone()
    }

    /// Any other tool's value.
    async fn tool(&mut self, name: &str, args: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let params = json!({"name": name, "arguments": args, "_meta": self.meta()});
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params}))
            .await;
        let msg = self.recv().await;
        assert_eq!(msg["id"], id, "{msg}");
        done(&msg["result"]).unwrap_or_else(|e| panic!("{name}: {e}"))
    }
}

/// A tool result's value, or its refusal.
fn done(result: &Value) -> Result<Value, Value> {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("a tool result: {result}"));
    let v: Value = serde_json::from_str(text).unwrap();
    if result["isError"] == json!(true) {
        Err(v)
    } else {
        Ok(v)
    }
}

/// The refusal of `result`, which must be `want`.
#[track_caller]
fn refused_with(result: &Value, want: (&str, &str, &str)) {
    let e = done(result).expect_err("a refusal");
    assert_eq!(
        (
            e["code"].as_str().unwrap(),
            e["message"].as_str().unwrap(),
            e["fix"].as_str().unwrap()
        ),
        want,
        "{e}"
    );
}

fn ask(item: &rdy_engine::pending::Item) -> Value {
    json!({"ref": item.r#ref, "card_hash": item.card_hash, "elicit": true})
}

fn choice(id: &str) -> Value {
    json!({"action": "accept", "content": {"choice": id}})
}

fn responses(entry: Value) -> Value {
    json!({"inputResponses": {"answer": entry}})
}

/// The retry of a 2026-07-28 card: `requestState` and the one response.
fn retry(handle: &str, entry: Value) -> Value {
    json!({"requestState": handle, "inputResponses": {"answer": entry}})
}

/// Asks for a 2026-07-28 card and returns its handle.
async fn handle_of(c: &mut Client, args: Value) -> String {
    let r = c.answer(args, json!({})).await;
    assert_eq!(r["resultType"], "input_required", "{r}");
    r["requestState"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------- worlds

fn person_is_qa(dir: &Path) {
    std::fs::write(dir.join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
}

fn on<T: Send + 'static>(
    e: &dyn Engine,
    f: impl FnOnce(&mut Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<T, RdyError> {
    with_writer_result(e, f)
}

/// A project whose canon holds a seed doc, and its section.
fn simple() -> (World, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    person_is_qa(dir.path());
    (World::new(dir, Local::new(r)), section)
}

/// A claim in view `view` under `section`, proposed with a summary.
fn proposal(e: &dyn Engine, section: &str, view: &str) -> ProposalInfo {
    let (section, view) = (section.to_string(), view.to_string());
    on(e, move |r| {
        write(
            r,
            &view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("A claim of {view}.")}]),
        )?;
        rdy_qa::intake::answer_open_items(r, &writer(), &view);
        r.propose(&writer(), &view, &view, Some("Adds one claim."))
    })
    .expect("proposal")
}

/// One approval card: its world and item.
fn approval() -> (World, rdy_engine::pending::Item) {
    let (w, section) = simple();
    let p = proposal(&*w.engine, &section, "one");
    let item = w.card(&p.r#ref);
    (w, item)
}

/// `batch_project`, with its approve card first and four confirm cards.
fn batch() -> World {
    let (dir, r, _) = batch_project();
    person_is_qa(dir.path());
    World::new(dir, Local::new(r))
}

fn pi(cause: &str, subject: &str, kind: PlanKind) -> String {
    rdy_model::plan::item_display(&rdy_model::plan::item_hash(
        &BlockId::parse(cause).unwrap(),
        subject,
        kind,
    ))
}

/// Canon: claims a, b and q, where b and q build on a; view w changes a and defers both confirm
/// items in its propose.
fn deferred() -> (World, ProposalInfo) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
            {"op": "create", "type": "claim", "id": "$q", "parent": "$s", "body": "Q builds on {{$a}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let get = |k: &str| w.created[k].clone();
    let (a, b, q) = (get("$a"), get("$b"), get("$q"));
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
    )
    .unwrap();
    let defer = |item: String, reason: &str| rdy_engine::plan::PlanAnswerInput {
        item,
        act: rdy_model::plan::PlanAct::Defer,
        reason: Some(reason.into()),
    };
    let answers = [
        defer(pi(&a, &b, PlanKind::Confirm), "b waits for the review"),
        defer(pi(&a, &q, PlanKind::Confirm), "q waits for the rewrite"),
    ];
    let p = r
        .propose_plan(
            &writer(),
            rdy_engine::plan::ProposePlanRequest {
                summary: Some(TEST_SUMMARY),
                plan_answers: &answers,
                ..rdy_engine::plan::ProposePlanRequest::new("w", "w")
            },
        )
        .unwrap();
    person_is_qa(dir.path());
    (World::new(dir, Local::new(r)), p)
}

/// A draft doc with claim x, agent:a1's typo edit of x proposed, and a `fail` meaning verdict on
/// it: the world, the version and the review.
fn with_verdict() -> (World, ProposalInfo, String) {
    use rdy_engine::check::api::{ReviewRequest, run_review};
    use rdy_engine::semantic::api::run_review_submit;
    use rdy_engine::semantic::submit::{CitationRequest, ReviewSubmitRequest};
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let made = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let x = created(&made, "$x");
    put_j(&mut r, "a1", upd(&x, "X holds, now."), j_typo());
    let p = r
        .propose(&ag("a1"), "a1", "a1", Some(TEST_SUMMARY))
        .unwrap();
    person_is_qa(dir.path());
    let e = Local::new(r);
    let q = run_review(
        &e,
        &ag("a1"),
        PERSON,
        true,
        None,
        ReviewRequest::from_value(
            json!({"op": "request", "check": "meaning_preservation", "subject": p.r#ref}),
        )
        .unwrap(),
    )
    .expect("request");
    let reviewer = Session::agent("agent:r".into(), Some("r".into()), Role::Reviewer).unwrap();
    let v = run_review_submit(
        &e,
        &reviewer,
        ReviewSubmitRequest {
            request: q["request"].as_str().unwrap().into(),
            case_hash: q["case_hash"].as_str().unwrap().into(),
            verdict: "fail".into(),
            citations: vec![CitationRequest {
                block: x.to_string(),
                note: "the meaning moved".into(),
            }],
            severity: None,
            reason: Some("it reads otherwise".into()),
        },
    )
    .expect("submit");
    let review = v["review"].as_str().unwrap().to_string();
    (World::new(dir, e), p, review)
}

/// A stable spec doc and an open question under its section: the world, the item ref and the
/// section the decision goes under.
fn stable_question() -> (World, String, BlockId) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let spec = spec_doc(&mut r);
    let t = brief_of(&r, &Scope::Canon, &[&spec.doc]).base.token;
    write_cited(
        &mut r,
        "st",
        json!([{"op": "update", "id": spec.doc.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&t),
    )
    .expect("make the doc stable");
    land(&mut r, "st").expect("land the stable doc");
    let t = brief_of(&r, &Scope::Canon, &[&spec.section]).base.token;
    let w = write_cited(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q",
                "parent": spec.section.to_string(), "fields": {
            "prompt": "Who approves refunds?",
            "title": "Who approves refunds?",
            "why": "Support waits on this",
            "options": ["Finance", "Support lead", "Either"],
            "recommended": "Finance",
            "recommend_reason": "Finance owns the ledger"
        }}]),
        Some(&t),
    )
    .unwrap();
    land(&mut r, "qs").unwrap();
    r.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    let q = created(&w, "$q");
    let v = rdy_qa::fixtures::version_of(&r, &Scope::Canon, &q.to_string());
    person_is_qa(dir.path());
    (
        World::new(dir, Local::new(r)),
        format!("{q}@{}", &v[..12]),
        spec.section.clone(),
    )
}

/// The token of a current briefing of `id` in canon, registered in the engine's own registry.
fn current_token(w: &World, id: &BlockId) -> String {
    let b = w
        .engine
        .read()
        .brief_ids(
            &Scope::Canon,
            &std::collections::BTreeSet::from([id.clone()]),
            None,
        )
        .unwrap();
    b.register(&w.engine.briefs());
    b.base.token.clone()
}

// ---------------------------------------------------------------- ui10 pins

#[tokio::test(flavor = "multi_thread")]
async fn ui10_elicit_schema_keeps_every_relay_key() {
    // every key an agent passed before still parses, with elicit true, false or left out
    let old = json!({
        "ref": "r", "option": "o", "card_hash": "h", "note": "n", "brief": "b", "evidence": "e",
        "question": "q", "session_ref": "s", "kind": "card",
        "all_recommended": [{"ref": "r", "card_hash": "h"}],
    });
    for elicit in [None, Some(false), Some(true)] {
        let mut args = old.as_object().unwrap().clone();
        if let Some(e) = elicit {
            args.insert("elicit".into(), json!(e));
        }
        let p = parse_params::<AnswerParams>("rdy_answer", args).expect("every key parses");
        assert_eq!(p.elicit, elicit);
        assert_eq!(p.evidence.as_deref(), Some("e"));
    }
    // a close key keeps its nearest-key guidance
    let args = json!({"elict": true}).as_object().unwrap().clone();
    let e = parse_params::<AnswerParams>("rdy_answer", args).unwrap_err();
    assert_eq!(e.message, "rdy_answer: unknown parameter \"elict\"");
    assert!(e.fix.starts_with("did you mean \"elicit\"?"), "{}", e.fix);

    let (w, item) = approval();
    let before = w.seq();
    let mut c = Client::inline(&w, 1, form_caps());
    // each forbidden key, supplied, whatever its value: null, empty or set
    for (key, values) in [
        ("option", vec![json!(null), json!(""), json!("approve")]),
        ("evidence", vec![json!(null), json!(""), json!("yes")]),
        ("note", vec![json!(null), json!(""), json!("n")]),
        ("question", vec![json!(null), json!(""), json!("q")]),
        ("session_ref", vec![json!(null), json!(""), json!("s")]),
        ("kind", vec![json!(null), json!("card")]),
        ("all_recommended", vec![json!(null), json!([])]),
    ] {
        for v in values {
            let mut args = ask(&item);
            args[key] = v.clone();
            let r = c.answer(args, json!({})).await;
            refused_with(&r, MIXED);
            assert_eq!(r.get("requestState"), None, "{key} {v}");
        }
    }
    // ref and card_hash are required
    for missing in ["ref", "card_hash"] {
        let mut args = ask(&item);
        args.as_object_mut().unwrap().remove(missing);
        refused_with(&c.answer(args, json!({})).await, MIXED);
    }
    // a brief is allowed, and a null brief is no brief
    for brief in [json!("0123"), json!(null)] {
        let mut args = ask(&item);
        args["brief"] = brief;
        let r = c.answer(args, json!({})).await;
        assert_eq!(r["resultType"], "input_required", "{r}");
    }
    // false and left out are the relay path: its evidence stays required, in its own words
    let relay = json!({"ref": item.r#ref, "option": "reject", "card_hash": item.card_hash});
    let want = (
        "E_BAD_INPUT",
        "rdy_answer: parameter \"evidence\" is required",
        "pass \"evidence\" as a string",
    );
    refused_with(&c.answer(relay.clone(), json!({})).await, want);
    let mut off = relay.clone();
    off["elicit"] = json!(false);
    refused_with(&c.answer(off.clone(), json!({})).await, want);
    assert_eq!(w.seq(), before, "nothing is written");
    // and with evidence it relays
    off["evidence"] = json!("no");
    let r = done(&c.answer(off, json!({})).await).unwrap();
    assert_eq!(r["assurance"], "relayed", "{r}");

    // the tool function has no client: valid input is unavailable, mixed input is still mixed
    let mut st = rdy_mcp::tools::McpState::new(Arc::clone(&w.engine));
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:relay", "run": "r1", "role": "writer"}))
            .unwrap(),
    )
    .unwrap();
    let direct = |v: Value| serde_json::from_value::<AnswerParams>(v).unwrap();
    let e = rdy_mcp::tools::rdy_answer(
        &mut st,
        direct(json!({"ref": "r", "card_hash": "h", "elicit": true})),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        UNAVAILABLE,
        "{e:#?}"
    );
    let e = rdy_mcp::tools::rdy_answer(
        &mut st,
        direct(json!({"ref": "r", "card_hash": "h", "elicit": true, "option": "o"})),
    )
    .unwrap_err();
    assert_eq!((e.code, e.message.as_str(), e.fix.as_str()), MIXED);

    // tools/list shows the new key's help and the description's new sentences
    let (listed, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut l = McpClient::start(listed.path());
    let tools = l.tools();
    let tool = tools.iter().find(|t| t["name"] == "rdy_answer").unwrap();
    let d = tool["description"].as_str().unwrap();
    assert!(
        d.ends_with("With elicit: true, pass ref and card_hash, and brief for a question under stable content. The client shows rdy's card and returns the person's choice straight to rdy. If elicitation is unavailable, show the card in chat and relay the answer with option and evidence."),
        "{d}"
    );
    let props = &tool["inputSchema"]["properties"];
    assert_eq!(
        props["elicit"]["description"],
        "Ask the client to show rdy's card. Pass ref and card_hash, and brief when needed, without option or evidence."
    );
    assert_eq!(tool["inputSchema"].get("required"), None, "{tool}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ui10_new_revision_returns_one_bound_form_request() {
    let (w, item) = approval();
    let before = w.seq();
    let mut c = Client::inline(&w, 1, form_caps());
    let r = c.answer(ask(&item), json!({})).await;
    assert_eq!(r["resultType"], "input_required", "{r}");
    let keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["inputRequests", "requestState", "resultType"], "{r}");
    let handle = r["requestState"].as_str().unwrap();
    assert_eq!(handle.len(), 32, "{handle}");
    assert!(
        handle
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    let requests = r["inputRequests"].as_object().unwrap();
    assert_eq!(requests.len(), 1, "{r}");
    let req = &requests["answer"];
    assert_eq!(req["method"], "elicitation/create");
    let params = &req["params"];
    assert_eq!(params["mode"], "form");
    assert_eq!(params["message"], item.card, "the engine's card text");
    let schema = &params["requestedSchema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["choice"]));
    assert_eq!(schema["properties"].as_object().unwrap().len(), 1);
    let want: Vec<Value> = item
        .options
        .iter()
        .map(|o| {
            let title = if o.recommended.is_some() {
                format!("{} (Recommended)", o.label)
            } else {
                o.label.clone()
            };
            json!({"const": o.id, "title": title})
        })
        .collect();
    let choice_schema = &schema["properties"]["choice"];
    assert_eq!(
        choice_schema["oneOf"],
        Value::Array(want),
        "{choice_schema}"
    );
    assert_eq!(choice_schema["type"], "string");
    assert_eq!(choice_schema.get("default"), None, "no default choice");
    assert!(
        item.options
            .iter()
            .filter(|o| o.recommended.is_some())
            .count()
            == 1,
        "the card recommends one option"
    );
    assert_eq!(w.seq(), before, "asking writes nothing");
    assert_eq!(w.hub.elicitation().len(), 1);
    // a stale hash and a bad ref never issue a card
    let stale = json!({"ref": item.r#ref, "card_hash": "0".repeat(64), "elicit": true});
    let e = done(&c.answer(stale, json!({})).await).unwrap_err();
    assert_eq!(e["code"], "E_PROPOSAL_MOVED", "{e}");
    assert_eq!(e["corrected"]["card_hash"], item.card_hash, "{e}");
    assert_eq!(w.hub.elicitation().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn ui10_older_stdio_elicits_inside_the_call() {
    // 2025-11-25: oneOf entries; the choice lands inside the call
    let (w, item) = approval();
    let mut c = Client::legacy(&w, 1, V_NOV, form_caps()).await;
    let (id, req) = c.asked(ask(&item)).await;
    assert_eq!(req["params"]["mode"], "form", "{req}");
    assert_eq!(req["params"]["message"], item.card);
    let select = &req["params"]["requestedSchema"]["properties"]["choice"];
    assert_eq!(select["oneOf"][0]["const"], item.options[0].id, "{select}");
    assert_eq!(select.get("default"), None);
    assert_eq!(
        w.hub.elicitation().len(),
        1,
        "the card waits under a handle"
    );
    let r = c.respond(id, &req, choice("reject")).await;
    let v = done(&r).unwrap();
    assert_eq!(v["act"], "reject_proposal", "{v}");
    assert_eq!(v["assurance"], "local", "{v}");
    assert_eq!(r.get("resultType"), None, "an older client's shape: {r}");
    assert_eq!(w.hub.elicitation().len(), 0);
    let a = w.answer(&item.r#ref).expect("the answer");
    assert_eq!(a.evidence_kind, Some(EvidenceKind::Elicited));
    assert!(
        a.relayed_by.is_none() && a.relay_evidence.is_none(),
        "{a:#?}"
    );

    // 2025-06-18: enum and enumNames, in the same order; its elicitation has no modes
    let (w, item) = approval();
    let mut c = Client::legacy(&w, 1, V_JUN, json!({"elicitation": {}})).await;
    let (id, req) = c.asked(ask(&item)).await;
    let select = &req["params"]["requestedSchema"]["properties"]["choice"];
    let ids: Vec<&str> = item.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(select["enum"], json!(ids), "{select}");
    let titles: Vec<String> = item
        .options
        .iter()
        .map(|o| {
            if o.recommended.is_some() {
                format!("{} (Recommended)", o.label)
            } else {
                o.label.clone()
            }
        })
        .collect();
    assert_eq!(select["enumNames"], json!(titles), "{select}");
    assert_eq!(select.get("oneOf"), None);
    let r = c.respond(id, &req, json!({"action": "decline"})).await;
    assert_eq!(done(&r).unwrap(), json!({"state": "declined"}));

    // the same card over 2026-07-28 is input_required, not a request
    let (w, item) = approval();
    let mut c = Client::inline(&w, 1, form_caps());
    let r = c.answer(ask(&item), json!({})).await;
    assert_eq!(r["resultType"], "input_required", "{r}");

    // no capability, a URL-only client and an earlier revision never get a request
    let (w, item) = approval();
    for (version, caps) in [
        (V_NOV, json!({})),
        (V_NOV, json!({"elicitation": {"url": {}}})),
        (V_JUN, json!({})),
        (V_MAR, form_caps()),
    ] {
        let mut c = Client::legacy(&w, 1, version, caps.clone()).await;
        refused_with(&c.answer(ask(&item), json!({})).await, UNAVAILABLE);
    }
    for caps in [json!({}), json!({"elicitation": {"url": {}}})] {
        let mut c = Client::inline(&w, 2, caps);
        refused_with(&c.answer(ask(&item), json!({})).await, UNAVAILABLE);
    }
    assert_eq!(w.hub.elicitation().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn ui10_continuations_are_owned_expiring_and_single_use() {
    let base = Instant::now();
    let at = Arc::new(Mutex::new(base));
    let (mut w, item) = approval();
    let clock = Arc::clone(&at);
    w.hub = NotifyHub::with_elicitation(Registry::with_clock(Arc::new(move || {
        *clock.lock().unwrap()
    })));
    let before = w.seq();
    let mut c = Client::inline(&w, 1, form_caps());
    let reject = choice("reject");

    // an unknown handle, and responses with no handle
    let r = c
        .answer(ask(&item), retry(&"0".repeat(32), reject.clone()))
        .await;
    refused_with(&r, GONE);
    let r = c.answer(ask(&item), responses(reject.clone())).await;
    refused_with(&r, GONE);

    // at 600 seconds a card is gone; at 599 it still waits
    let h = handle_of(&mut c, ask(&item)).await;
    *at.lock().unwrap() = base + EXPIRY;
    refused_with(&c.answer(ask(&item), retry(&h, reject.clone())).await, GONE);
    assert_eq!(w.seq(), before);
    let start = base + EXPIRY;
    // the in-call flow's reply is held to the same clock
    let mut old = Client::legacy(&w, 11, V_NOV, form_caps()).await;
    let (id, req) = old.asked(ask(&item)).await;
    *at.lock().unwrap() = start + EXPIRY;
    refused_with(&old.respond(id, &req, reject.clone()).await, GONE);
    assert_eq!(w.seq(), before);
    let start = start + EXPIRY;
    let h = handle_of(&mut c, ask(&item)).await;
    *at.lock().unwrap() = start + EXPIRY - Duration::from_secs(1);

    // a tampered handle or changed arguments are refused, and the card stays
    let mut tampered = h.clone();
    tampered.replace_range(0..1, if h.starts_with('0') { "1" } else { "0" });
    refused_with(
        &c.answer(ask(&item), retry(&tampered, reject.clone())).await,
        GONE,
    );
    let other_hash = json!({"ref": item.r#ref, "card_hash": "0".repeat(64), "elicit": true});
    refused_with(&c.answer(other_hash, retry(&h, reject.clone())).await, GONE);
    let mut added = ask(&item);
    added["brief"] = json!("a-token");
    refused_with(&c.answer(added, retry(&h, reject.clone())).await, GONE);
    let relay = json!({"ref": item.r#ref, "card_hash": item.card_hash});
    refused_with(&c.answer(relay, retry(&h, reject.clone())).await, GONE);
    // another actor, and another run of the same actor, do not consume it
    let mut other = Client::inline(&w, 2, form_caps()).with_actor("agent:other", Some("r1"));
    refused_with(
        &other.answer(ask(&item), retry(&h, reject.clone())).await,
        GONE,
    );
    let mut run2 = Client::inline(&w, 3, form_caps()).with_actor("agent:relay", Some("r2"));
    refused_with(
        &run2.answer(ask(&item), retry(&h, reject.clone())).await,
        GONE,
    );
    assert_eq!(w.hub.elicitation().len(), 1);
    assert_eq!(w.seq(), before, "no unwanted write");

    // a second connection of the same actor and run continues; a retry after it is refused
    let mut same = Client::inline(&w, 4, form_caps());
    let v = done(&same.answer(ask(&item), retry(&h, reject.clone())).await).unwrap();
    assert_eq!(v["act"], "reject_proposal", "{v}");
    let after = w.seq();
    refused_with(&c.answer(ask(&item), retry(&h, reject.clone())).await, GONE);
    assert_eq!(w.seq(), after, "a retry answers nothing twice");

    // a brief token is bound: removed, added or replaced, the card refuses
    let (mut w2, section) = simple();
    w2.hub = NotifyHub::new();
    let p = proposal(&*w2.engine, &section, "two");
    let it = w2.card(&p.r#ref);
    let mut c2 = Client::inline(&w2, 1, form_caps());
    let mut with = ask(&it);
    with["brief"] = json!("token-a");
    let h = handle_of(&mut c2, with.clone()).await;
    refused_with(
        &c2.answer(ask(&it), retry(&h, json!({"action": "decline"})))
            .await,
        GONE,
    );
    let mut swapped = ask(&it);
    swapped["brief"] = json!("token-b");
    refused_with(
        &c2.answer(swapped, retry(&h, json!({"action": "decline"})))
            .await,
        GONE,
    );
    assert_eq!(
        done(
            &c2.answer(with, retry(&h, json!({"action": "decline"})))
                .await
        )
        .unwrap(),
        json!({"state": "declined"})
    );

    // two retries at once from two connections: one wins, the other is refused, one write
    let p = proposal(&*w2.engine, &section, "three");
    let it = w2.card(&p.r#ref);
    let mut a = Client::inline(&w2, 5, form_caps());
    let mut b = Client::inline(&w2, 6, form_caps());
    let h = handle_of(&mut a, ask(&it)).await;
    let answers = w2.seq();
    let (ra, rb) = tokio::join!(
        a.answer(ask(&it), retry(&h, choice("reject"))),
        b.answer(ask(&it), retry(&h, choice("reject"))),
    );
    let wins = [&ra, &rb].iter().filter(|r| done(r).is_ok()).count();
    assert_eq!(wins, 1, "{ra} {rb}");
    let loser = if done(&ra).is_ok() { &rb } else { &ra };
    refused_with(loser, GONE);
    assert_ne!(w2.seq(), answers);

    // a restarted daemon has a new hub: no card survives it
    let p = proposal(&*w2.engine, &section, "four");
    let it = w2.card(&p.r#ref);
    let mut a = Client::inline(&w2, 7, form_caps());
    let h = handle_of(&mut a, ask(&it)).await;
    w2.hub = NotifyHub::new();
    let mut after_restart = Client::inline(&w2, 8, form_caps());
    refused_with(
        &after_restart
            .answer(ask(&it), retry(&h, choice("reject")))
            .await,
        GONE,
    );

    // 1024 cards wait; the next refuses, and an expired one makes room
    let p = proposal(&*w2.engine, &section, "five");
    let it = w2.card(&p.r#ref);
    let mut c3 = Client::inline(&w2, 9, form_caps());
    for n in 0..MAX_WAITING {
        w2.hub
            .elicitation()
            .insert(Bound {
                item: format!("x{n}"),
                card_hash: "0".repeat(64),
                brief: None,
                actor: "agent:relay".into(),
                run: Some("r1".into()),
            })
            .unwrap();
    }
    assert_eq!(w2.hub.elicitation().len(), 1024);
    refused_with(
        &c3.answer(ask(&it), json!({})).await,
        (
            "E_LIMIT",
            "elicitation exceeds 1024 waiting cards",
            "ask again later: a waiting card expires after 10 minutes",
        ),
    );
    let base = Instant::now();
    let at = Arc::new(Mutex::new(base));
    let clock = Arc::clone(&at);
    let full = Registry::with_clock(Arc::new(move || *clock.lock().unwrap()));
    let bound = |n: usize| Bound {
        item: format!("x{n}"),
        card_hash: "0".repeat(64),
        brief: None,
        actor: "agent:relay".into(),
        run: None,
    };
    for n in 0..MAX_WAITING {
        full.insert(bound(n)).unwrap();
    }
    assert_eq!(full.insert(bound(9999)).unwrap_err().code, "E_LIMIT");
    *at.lock().unwrap() = base + EXPIRY;
    full.insert(bound(9999)).expect("expiry made room");
    assert_eq!(full.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn ui10_decline_cancel_and_disconnect_leave_the_item_open() {
    let (w, item) = approval();
    let before = w.seq();
    let open = |w: &World| {
        assert_eq!(w.seq(), before, "nothing is written");
        assert!(w.answer(&item.r#ref).is_none());
        assert_eq!(w.card(&item.r#ref).card_hash, item.card_hash, "still open");
    };
    // the new revision
    let mut c = Client::inline(&w, 1, form_caps());
    for (action, state) in [("decline", "declined"), ("cancel", "cancelled")] {
        for entry in [
            json!({"action": action}),
            json!({"action": action, "content": {}}),
        ] {
            let h = handle_of(&mut c, ask(&item)).await;
            let r = c.answer(ask(&item), retry(&h, entry)).await;
            assert_eq!(done(&r).unwrap(), json!({"state": state}));
            assert_eq!(r["resultType"], "complete", "{r}");
            open(&w);
        }
    }
    // the older revision
    let mut o = Client::legacy(&w, 2, V_NOV, form_caps()).await;
    for (action, state) in [("decline", "declined"), ("cancel", "cancelled")] {
        let (id, req) = o.asked(ask(&item)).await;
        let r = o.respond(id, &req, json!({"action": action})).await;
        assert_eq!(done(&r).unwrap(), json!({"state": state}));
        open(&w);
    }
    assert_eq!(w.hub.elicitation().len(), 0);
    // the older revision's call is cancelled while the card waits: the card is dropped
    let (id, _req) = o.asked(ask(&item)).await;
    assert_eq!(w.hub.elicitation().len(), 1);
    o.send(
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": id, "reason": "the person left"}}),
    )
    .await;
    // the server tells the client to close the form; rmcp drops the cancelled call's own reply
    let note = o.recv().await;
    assert_eq!(note["method"], "notifications/cancelled", "{note}");
    // the connection lives: its next call is answered, and the card is gone
    let v = o.tool("rdy_pending", json!({})).await;
    assert!(v["items"].is_array(), "{v}");
    assert_eq!(w.hub.elicitation().len(), 0);
    open(&w);
    // a closed client: the waiting card goes with it
    let (_id, _req) = o.asked(ask(&item)).await;
    assert_eq!(w.hub.elicitation().len(), 1);
    drop(o);
    // the server's task ends the call when it sees the stream close; the bound only stops a hang
    let start = Instant::now();
    while !w.hub.elicitation().is_empty() {
        assert!(
            start.elapsed() < rdy_qa::mcp::DEADLINE,
            "the card outlived its client"
        );
        tokio::task::yield_now().await;
    }
    open(&w);
}

#[tokio::test(flavor = "multi_thread")]
async fn ui10_choice_validation_never_reaches_the_writer() {
    let (w, item) = approval();
    let before = w.seq();
    let mut c = Client::inline(&w, 1, form_caps());
    let extra_key = json!({"action": "accept", "content": {"choice": "reject"}, "extra": 1});
    let bad_entries = [
        json!("accept"),
        json!({"action": "maybe"}),
        json!({"content": {"choice": "reject"}}),
        json!({"action": "accept"}),
        json!({"action": "accept", "content": {}}),
        json!({"action": "accept", "content": {"choice": 7}}),
        json!({"action": "accept", "content": {"choice": "reject", "note": "x"}}),
        json!({"action": "accept", "content": {"choice": "nope"}}),
        json!({"action": "decline", "content": {"choice": "reject"}}),
        json!({"action": "cancel", "_meta": "x"}),
        extra_key,
    ];
    for entry in bad_entries {
        let h = handle_of(&mut c, ask(&item)).await;
        let r = c.answer(ask(&item), retry(&h, entry.clone())).await;
        refused_with(&r, INVALID);
        // an invalid terminal response consumed the owner's card
        refused_with(
            &c.answer(ask(&item), retry(&h, choice("reject"))).await,
            GONE,
        );
    }
    // a missing entry, and an extra one
    let h = handle_of(&mut c, ask(&item)).await;
    let r = c.answer(ask(&item), json!({"requestState": h})).await;
    refused_with(&r, INVALID);
    let h = handle_of(&mut c, ask(&item)).await;
    let r = c
        .answer(
            ask(&item),
            json!({"requestState": h, "inputResponses": {
                "answer": choice("reject"), "more": choice("reject")}}),
        )
        .await;
    refused_with(&r, INVALID);
    let h = handle_of(&mut c, ask(&item)).await;
    let r = c
        .answer(
            ask(&item),
            json!({"requestState": h, "inputResponses": {"other": choice("reject")}}),
        )
        .await;
    refused_with(&r, INVALID);
    // an overlong ref, one past the bound; at the bound it is the ordinary unknown item
    let long = "x".repeat(1025);
    refused_with(
        &c.answer(
            json!({"ref": long, "card_hash": "0".repeat(64), "elicit": true}),
            json!({}),
        )
        .await,
        (
            "E_LIMIT",
            "the elicited item ref exceeds 1024 bytes",
            "read rdy_pending and use its item ref",
        ),
    );
    let at = "x".repeat(1024);
    let e = done(
        &c.answer(
            json!({"ref": at, "card_hash": "0".repeat(64), "elicit": true}),
            json!({}),
        )
        .await,
    )
    .unwrap_err();
    assert_ne!(e["code"], "E_LIMIT", "{e}");
    assert_eq!(w.seq(), before, "nothing reached the writer");

    // the older revision's malformed replies: an unknown action, content, an unknown choice
    let mut o = Client::legacy(&w, 2, V_NOV, form_caps()).await;
    for result in [
        json!({"action": "maybe"}),
        json!({"action": "accept"}),
        json!({"action": "accept", "content": {"choice": "nope"}}),
        json!({"action": "accept", "content": {"choice": "reject", "x": 1}}),
    ] {
        let (id, req) = o.asked(ask(&item)).await;
        let r = o.respond(id, &req, result.clone()).await;
        refused_with(&r, INVALID);
    }
    // a JSON-RPC error instead of a result
    let (id, req) = o.asked(ask(&item)).await;
    o.send(json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -1, "message": "no"}}))
        .await;
    let msg = o.recv().await;
    assert_eq!(msg["id"], id, "{msg}");
    refused_with(&msg["result"], INVALID);
    assert_eq!(w.seq(), before);
    assert_eq!(w.hub.elicitation().len(), 0);
}

/// Answers `item` with `option` over the new revision.
async fn over_new(w: &World, sid: u64, args: Value, option: &str) -> Result<Value, Value> {
    let mut c = Client::inline(w, sid, form_caps());
    let h = handle_of(&mut c, args.clone()).await;
    done(&c.answer(args, retry(&h, choice(option))).await)
}

/// Answers `item` with `option` over the older revision.
async fn over_old(w: &World, sid: u64, args: Value, option: &str) -> Result<Value, Value> {
    let mut c = Client::legacy(w, sid, V_NOV, form_caps()).await;
    let (id, req) = c.asked(args).await;
    done(&c.respond(id, &req, choice(option)).await)
}

/// `over_new` or `over_old`.
async fn over(new: bool, w: &World, sid: u64, args: Value, option: &str) -> Result<Value, Value> {
    if new {
        over_new(w, sid, args, option).await
    } else {
        over_old(w, sid, args, option).await
    }
}

fn local_reply(v: &Value, act: &str, option: &str) {
    assert_eq!(v["act"], act, "{v:#}");
    assert_eq!(v["option"], option, "{v:#}");
    assert_eq!(v["assurance"], "local", "{v:#}");
    assert!(v["relayed_by"].is_null(), "{v:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ui10_elicited_answers_share_the_whole_engine_gate() {
    for new in [true, false] {
        // approval: a landing as the person, local, elicited
        let (w, item) = approval();
        let v = over(new, &w, 1, ask(&item), "approve").await.unwrap();
        local_reply(&v, "approve", "approve");
        assert!(v["landing"].is_object(), "{v:#}");
        for key in [
            "ref",
            "act",
            "option",
            "label",
            "assurance",
            "answer",
            "landing",
            "relayed_by",
            "next",
        ] {
            assert!(v.get(key).is_some(), "the answer reply keeps {key}: {v:#}");
        }

        // a held approval: the exceptions keep it open; a confirm answers one
        let w = batch();
        let queue = w
            .engine
            .read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
            })
            .unwrap();
        let batch_item = queue
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Approve)
            .unwrap()
            .clone();
        let e = over(new, &w, 1, ask(&batch_item), "approve")
            .await
            .unwrap_err();
        assert_eq!(e["code"], "E_EXCEPTION_OPEN", "{e}");
        let confirm = queue
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Confirm)
            .unwrap()
            .clone();
        let v = over(new, &w, 2, ask(&confirm), "confirm").await.unwrap();
        local_reply(&v, "confirm", "confirm");
        let a = w.answer(&confirm.r#ref).expect("the confirm");
        assert_eq!(a.act, AnswerAct::Confirm);
        assert_eq!(a.evidence_kind, Some(EvidenceKind::Elicited));
        assert!(a.relay_evidence.is_none(), "{a:#?}");
        // elicited Later: a local answer of the caller's run, and its no-run refusal
        let other = queue
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Confirm)
            .nth(1)
            .unwrap()
            .clone();
        let v = over(new, &w, 3, ask(&other), "later").await.unwrap();
        local_reply(&v, "later", "later");
        let snoozes = w.snoozes();
        let a = snoozes
            .iter()
            .find(|a| a.item == other.r#ref)
            .expect("the later");
        assert_eq!(a.person.run.as_deref(), Some("r1"), "{a:#?}");
        assert_eq!(a.evidence_kind, Some(EvidenceKind::Elicited));

        // a caller with no run cannot name the chat session of a Later
        let third = queue
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Confirm)
            .nth(2)
            .unwrap()
            .clone();
        let mut c = client(new, &w, 4)
            .await
            .with_actor("ci:0123456789abcdef0123456789abcdef/nightly", None);
        let waiting = Waiting::start(&mut c, new, ask(&third)).await;
        let r = waiting.answer(&mut c, ask(&third), choice("later")).await;
        let e = r.unwrap_err();
        assert_eq!(
            e["message"], "Later needs a run to name the chat session",
            "{e}"
        );
        assert_eq!(
            e["fix"],
            "ask again from a session with a run, or show the card in chat and relay the answer",
            "{e}"
        );

        // a deferral card: its act
        let (w, p) = deferred();
        let defer = format!("defer:{}", p.r#ref);
        let item = w.card(&defer);
        let v = over(new, &w, 1, ask(&item), "o1").await.unwrap();
        local_reply(&v, "accept_deferrals", "o1");

        // M3d: a verdict accepted as blocking
        let (w, p, review) = with_verdict();
        let item = w.card(&p.r#ref);
        let id = format!("accept_verdict:{review}:{}", p.id);
        let v = over(new, &w, 1, ask(&item), &id).await.unwrap();
        local_reply(&v, "accept_verdict", &id);

        // a stale card, before the form and after it: the corrected card comes through
        let (w, section) = simple();
        let p = proposal(&*w.engine, &section, "one");
        let item = w.card(&p.r#ref);
        let args = ask(&item);
        let mut c = client(new, &w, 1).await;
        let waiting = Waiting::start(&mut c, new, args.clone()).await;
        let moved = repropose(&w, &section, "one", "More.");
        let e = waiting
            .answer(&mut c, args.clone(), choice("approve"))
            .await
            .unwrap_err();
        assert_eq!(e["code"], "E_PROPOSAL_MOVED", "{e}");
        assert_eq!(e["corrected"]["ref"], moved.r#ref, "{e}");
        let before = w.seq();
        let e = over_ask_refused(new, &w, args).await;
        assert_eq!(e["code"], "E_PROPOSAL_MOVED", "{e}");
        assert_eq!(w.seq(), before);

        // a person who holds no role of the policy never sees a form
        let (w, p) = policed();
        std::fs::write(w.dir.path().join(".rdy/local.yaml"), "actor: human:other\n").unwrap();
        let item = presented_item(&w.engine.read(), PERSON, &p.r#ref, None).unwrap();
        let e = over_ask_refused(new, &w, ask(&item)).await;
        assert_eq!(e["code"], "E_PERMISSION", "{e}");

        // a choice that needs a note keeps the engine's words, with the elicited fix
        let (w2, cause) = reaffirm();
        let item = w2.card(&cause);
        let e = over(new, &w2, 1, ask(&item), "o2").await.unwrap_err();
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(e["message"], "Keep them suspect needs a note", "{e}");
        assert_eq!(e["fix"], NOTE_FIX, "{e}");

        // an authored question under stable content: the brief binds through both flows
        let (w, it, section) = stable_question();
        let item = w.card(&it);
        let before = w.seq();
        let e = over(new, &w, 1, ask(&item), "o1").await.unwrap_err();
        assert_eq!(e["code"], "E_BRIEF_REQUIRED", "{e}");
        assert_eq!(w.seq(), before);
        let stale = current_token(&w, &section);
        let id = section.clone();
        on(&*w.engine, move |r| {
            let t = brief_of(r, &Scope::Canon, &[&id]).base.token;
            write_cited(
                r,
                "moved",
                json!([{"op": "update", "id": id.to_string(), "fields": {"title": "Moved"}}]),
                Some(&t),
            )?;
            land(r, "moved").map(|_| ())
        })
        .unwrap();
        let item = w.card(&it);
        let before = w.seq();
        let mut args = ask(&item);
        args["brief"] = json!(stale);
        let e = over(new, &w, 2, args, "o1").await.unwrap_err();
        assert_eq!(e["code"], "E_BRIEF_STALE", "{e}");
        assert_eq!(w.seq(), before, "no silent re-fetch and no write");
        let mut args = ask(&item);
        args["brief"] = json!(current_token(&w, &section));
        let v = over(new, &w, 3, args, "o1").await.unwrap();
        local_reply(&v, "answer_question", "o1");
    }
}

/// A client of the new or the older revision.
async fn client(new: bool, w: &World, sid: u64) -> Client {
    if new {
        Client::inline(w, sid, form_caps())
    } else {
        Client::legacy(w, sid, V_NOV, form_caps()).await
    }
}

/// A form the client holds, on either flow.
enum Waiting {
    Handle(String),
    InCall(u64, Value),
}

impl Waiting {
    async fn start(c: &mut Client, new: bool, args: Value) -> Waiting {
        if new {
            Waiting::Handle(handle_of(c, args).await)
        } else {
            let (id, req) = c.asked(args).await;
            Waiting::InCall(id, req)
        }
    }

    async fn answer(self, c: &mut Client, args: Value, entry: Value) -> Result<Value, Value> {
        done(&match self {
            Waiting::Handle(h) => c.answer(args, retry(&h, entry)).await,
            Waiting::InCall(id, req) => c.respond(id, &req, entry).await,
        })
    }
}

/// Another claim in `view`, and the view's next version.
fn repropose(w: &World, section: &str, view: &str, body: &str) -> ProposalInfo {
    let (section, view, body) = (section.to_string(), view.to_string(), body.to_string());
    on(&*w.engine, move |r| {
        write(
            r,
            &view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": body}]),
        )?;
        rdy_qa::intake::answer_open_items(r, &writer(), &view);
        r.propose(&writer(), &view, &view, Some("Adds more."))
    })
    .expect("the next version")
}

/// The refusal of the ask itself: no form is issued.
async fn over_ask_refused(new: bool, w: &World, args: Value) -> Value {
    let mut c = client(new, w, 9).await;
    done(&c.answer(args, json!({})).await).unwrap_err()
}

/// A project under a policy whose role `approvers` holds only the person, and a version that
/// needs them.
fn policed() -> (World, ProposalInfo) {
    use rdy_engine::WriteRequest;
    use rdy_model::Justification;
    use rdy_qa::fixtures::parse_test_ops;
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/packs/rdy-qa.yaml"),
        rdy_qa::packs::QA_PACK_YAML,
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: rdy-qa\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let w = |r: &mut Rdy, view: &str, ops: Value, j: Option<Justification>| {
        r.write(
            &writer(),
            WriteRequest {
                view: view.into(),
                ops: parse_test_ops(ops).unwrap(),
                justification: j,
                message: String::new(),
            },
        )
        .unwrap()
    };
    let seed = w(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}}
        ]),
        None,
    );
    land(&mut r, "seed").unwrap();
    let s = created(&seed, "$s");
    let rules = w(
        &mut r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        None,
    );
    let d = created(&rules, "$dec");
    w(
        &mut r,
        "pol",
        json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": rules.created["$d"], "fields": {
            "key": "authority-policy",
            "agent_may_accept": [],
            "roles": {"approvers": [PERSON]},
            "require": [{"when": "decision", "role": "approvers"}],
            "owners": []
        }}]),
        Some(Justification::Implements(vec![d])),
    );
    land(&mut r, "pol").unwrap();
    put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "decision", "id": "$dec", "parent": s.to_string(),
                "fields": {"statement": "Ideas stay short", "status": "proposed", "needs_content": false,
                           "reversibility": "two_way", "authority_basis": "precedent"}}]),
        j_typo(),
    );
    let p = r
        .propose(&ag("a1"), "a1", "a1", Some(TEST_SUMMARY))
        .unwrap();
    person_is_qa(dir.path());
    (World::new(dir, Local::new(r)), p)
}

/// Canon: a claim and a decision on it; the claim then changes, so its re-affirmation card stands.
fn reaffirm() -> (World, String) {
    use rdy_engine::pending::PlanCardBinding;
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (s, a) = (w.created["$s"].clone(), w.created["$a"].clone());
    r.decide(
        &writer(),
        "d-one",
        DecisionInput {
            parent: s.clone(),
            statement: "One rule".into(),
            depends_on: vec![a.clone()],
            reversibility: Some("two_way".into()),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "d-one").unwrap();
    write(
        &mut r,
        "x",
        json!([{"op": "update", "id": a, "body": "A holds 60 days."}]),
    )
    .unwrap();
    land(&mut r, "x").unwrap();
    person_is_qa(dir.path());
    let world = World::new(dir, Local::new(r));
    let item = world
        .engine
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| {
            matches!(&i.plan_binding, Some(PlanCardBinding::Reaffirm { cause, .. }) if cause.as_str() == a)
        })
        .expect("the re-affirmation card");
    (world, item.r#ref)
}

// -- an imported proposal over the real transport

/// The project's stdio proxy, as a client that states its 2026-07-28 context in each request.
fn inline_line(id: u64, args: Value, extra: Value) -> String {
    let mut params = json!({"name": "rdy_answer", "arguments": args, "_meta": {
        "rdy/actor": "agent:a1", "rdy/run": "a1",
        "io.modelcontextprotocol/protocolVersion": V_NEW,
        "io.modelcontextprotocol/clientCapabilities": form_caps(),
    }});
    for (k, v) in extra.as_object().unwrap() {
        params[k] = v.clone();
    }
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params}).to_string()
}

#[test]
fn ui10_an_imported_proposal_still_needs_the_person_through_the_proxy() {
    let s = crate::mcp_import::policy_served();
    let t = tempfile::tempdir().unwrap();
    let src = t.path().join("wallets");
    crate::mcp_import::tree(
        &src,
        "wallets",
        "d-WA001",
        "s-WA001",
        &[("D-AC001", "accepted")],
    );
    let mut c = McpClient::start_proxy(&s);
    crate::mcp_import::session(&mut c, "agent:a1", "a1", "writer");
    c.call(
        "rdy_import",
        json!({"dir": src.to_str().unwrap(), "view": "a1"}),
    )
    .ok();
    let pref = c
        .call(
            "rdy_propose",
            json!({"view": "a1", "title": "Wallets", "summary": "Imports the wallets doc and its accepted ruling."}),
        )
        .ok()["ref"]
        .as_str()
        .unwrap()
        .to_string();
    let queue = c.call("rdy_pending", json!({})).ok();
    let card = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == pref.as_str() && i["kind"] == "approve")
        .unwrap()
        .clone();
    let args = json!({"ref": pref, "card_hash": card["card_hash"], "elicit": true});
    // the older flow crosses the proxy both ways: the request out, the person's reply back
    let id = c.next_id();
    let mut old = serde_json::from_str::<Value>(&inline_line(id, args.clone(), json!({}))).unwrap();
    old["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!(V_NOV);
    c.send_raw(&old.to_string());
    let req = c.recv_next(rdy_qa::mcp::DEADLINE);
    assert_eq!(req["method"], "elicitation/create", "{req}");
    assert_eq!(req["params"]["message"], card["card"], "{req}");
    let line = json!({"jsonrpc": "2.0", "id": req["id"], "result": {"action": "decline"}});
    c.send_raw(&line.to_string());
    let reply = c.recv(id, rdy_qa::mcp::DEADLINE);
    assert_eq!(
        done(&reply["result"]).unwrap(),
        json!({"state": "declined"})
    );
    // the card asks through the proxy, and the person's choice lands it, signed by nobody
    let id = c.next_id();
    c.send_raw(&inline_line(id, args.clone(), json!({})));
    let first = c.recv(id, rdy_qa::mcp::DEADLINE);
    assert_eq!(first["result"]["resultType"], "input_required", "{first}");
    assert_eq!(
        first["result"]["inputRequests"]["answer"]["params"]["message"], card["card"],
        "{first}"
    );
    let handle = first["result"]["requestState"].as_str().unwrap();
    let id = c.next_id();
    c.send_raw(&inline_line(id, args, retry(handle, choice("approve"))));
    let second = c.recv(id, rdy_qa::mcp::DEADLINE);
    let v = done(&second["result"]).unwrap();
    local_reply(&v, "approve", "approve");
    assert_eq!(v["landing"]["assurance"], "local", "{v:#}");
}

// -- signing

const PERSON_SEED: [u8; 32] = [7; 32];
const AGENT_SEED: [u8; 32] = [9; 32];
const SK_APP: &[u8] = b"ssh:";

fn ssh_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SoftKey {
    seed: [u8; 32],
    sk: bool,
}

fn person_ed() -> SoftKey {
    SoftKey {
        seed: PERSON_SEED,
        sk: false,
    }
}
fn person_sk() -> SoftKey {
    SoftKey {
        seed: PERSON_SEED,
        sk: true,
    }
}
fn agent_ed() -> SoftKey {
    SoftKey {
        seed: AGENT_SEED,
        sk: false,
    }
}

impl SoftKey {
    fn dalek(&self) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&self.seed)
    }

    fn blob(&self) -> Vec<u8> {
        let public = self.dalek().verifying_key().to_bytes();
        if self.sk {
            [
                ssh_string(b"sk-ssh-ed25519@openssh.com"),
                ssh_string(&public),
                ssh_string(SK_APP),
            ]
            .concat()
        } else {
            [ssh_string(b"ssh-ed25519"), ssh_string(&public)].concat()
        }
    }

    fn fingerprint(&self) -> String {
        rdy_engine::signing_crypto::blob_fingerprint(&self.blob())
    }

    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        if self.sk {
            let flags = 1u8;
            let counter = 42u32;
            let mut m = Sha256::digest(SK_APP).to_vec();
            m.push(flags);
            m.extend(counter.to_be_bytes());
            m.extend(Sha256::digest(payload));
            let raw = self.dalek().sign(&m).to_bytes();
            let mut out = [ssh_string(b"sk-ssh-ed25519@openssh.com"), ssh_string(&raw)].concat();
            out.push(flags);
            out.extend(counter.to_be_bytes());
            out
        } else {
            let raw = self.dalek().sign(payload).to_bytes();
            [ssh_string(b"ssh-ed25519"), ssh_string(&raw)].concat()
        }
    }
}

type Hook = Box<dyn FnMut(usize) + Send>;
/// What a signing hook moves the project with, once the world exists.
type Mover = Arc<Mutex<Option<(Arc<dyn Engine>, String)>>>;

/// An agent holding `keys`, counting its signatures and running `hook` before its first one.
struct FakeAgent {
    keys: Vec<SoftKey>,
    signs: AtomicUsize,
    hook: Mutex<Option<Hook>>,
}

impl FakeAgent {
    fn new(keys: &[SoftKey]) -> Arc<FakeAgent> {
        Arc::new(FakeAgent {
            keys: keys.to_vec(),
            signs: AtomicUsize::new(0),
            hook: Mutex::new(None),
        })
    }

    fn hooked(keys: &[SoftKey], hook: impl FnMut(usize) + Send + 'static) -> Arc<FakeAgent> {
        let a = FakeAgent::new(keys);
        *a.hook.lock().unwrap() = Some(Box::new(hook));
        a
    }

    fn signs(&self) -> usize {
        self.signs.load(Ordering::SeqCst)
    }
}

impl SigningAgent for FakeAgent {
    fn identities(&self) -> Result<Vec<AgentIdentity>, RdyError> {
        Ok(self
            .keys
            .iter()
            .map(|k| AgentIdentity {
                public_key: k.blob(),
            })
            .collect())
    }

    fn sign(&self, public_key: &[u8], payload: &[u8]) -> Result<Vec<u8>, RdyError> {
        let n = self.signs.fetch_add(1, Ordering::SeqCst);
        let hook = self.hook.lock().unwrap().take();
        if let Some(mut h) = hook {
            h(n);
        }
        let key = self
            .keys
            .iter()
            .find(|k| k.blob() == public_key)
            .expect("the fake agent holds the key it was asked for");
        Ok(key.sign(payload))
    }
}

fn local_config(dir: &Path, sign: &str, key: Option<SoftKey>, agent: Option<SoftKey>) {
    let sel = |k: Option<SoftKey>| match k {
        Some(k) => format!("\"{}\"", k.fingerprint()),
        None => "null".into(),
    };
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!(
            "actor: {PERSON}\nsigning:\n  key: {}\n  agent_key: {}\n  sign: {sign}\n  allowed_signers: null\n",
            sel(key),
            sel(agent)
        ),
    )
    .unwrap();
}

/// The one signature check of a reply, which must be the person's approval by the key alone.
fn person_signed(v: &Value) -> Value {
    let checks = v["signatures"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "no relay signature: {v:#}");
    assert_eq!(checks[0]["kind"], "approval", "{v:#}");
    assert_eq!(checks[0]["signed"], "valid", "{v:#}");
    assert!(v["relayed_by"].is_null(), "{v:#}");
    checks[0].clone()
}

fn signed_world(agent: Arc<dyn SigningAgent>) -> (World, String) {
    let (mut w, section) = simple();
    w.agent = agent;
    (w, section)
}

#[tokio::test(flavor = "multi_thread")]
async fn ui10_signing_cancellation_keeps_elicited_local() {
    let n = AtomicUsize::new(0);
    let next = || format!("e{}", n.fetch_add(1, Ordering::SeqCst));
    for new in [true, false] {
        // auto and always: the person's key, through the caller's agent, on both flows
        for mode in ["auto", "always"] {
            let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
            let (w, section) = signed_world(agent.clone());
            local_config(w.dir.path(), mode, Some(person_ed()), Some(agent_ed()));
            let p = proposal(&*w.engine, &section, &next());
            let item = w.card(&p.r#ref);
            let v = over(new, &w, 1, ask(&item), "approve").await.unwrap();
            local_reply(&v, "approve", "approve");
            person_signed(&v);
            assert_eq!(agent.signs(), 1, "{mode}");
        }
        // no person key: auto is unsigned, always refuses and writes nothing
        let agent = FakeAgent::new(&[person_ed(), agent_ed()]);
        let (w, section) = signed_world(agent.clone());
        local_config(w.dir.path(), "auto", None, Some(agent_ed()));
        let p = proposal(&*w.engine, &section, &next());
        let v = over(new, &w, 1, ask(&w.card(&p.r#ref)), "approve")
            .await
            .unwrap();
        assert_eq!(v["signatures"], json!([]), "{v:#}");
        assert_eq!(agent.signs(), 0, "the agent key is never used");
        local_config(w.dir.path(), "always", None, Some(agent_ed()));
        let p = proposal(&*w.engine, &section, &next());
        let item = w.card(&p.r#ref);
        let before = w.seq();
        let e = over(new, &w, 2, ask(&item), "approve").await.unwrap_err();
        assert_eq!(e["code"], "E_SIGNING_KEY_MISSING", "{e}");
        assert_eq!(w.seq(), before);
        assert_eq!(agent.signs(), 0);
        // a FIDO key reports the person's presence
        let agent = FakeAgent::new(&[person_sk()]);
        let (w, section) = signed_world(agent);
        local_config(w.dir.path(), "always", Some(person_sk()), None);
        let p = proposal(&*w.engine, &section, &next());
        let v = over(new, &w, 1, ask(&w.card(&p.r#ref)), "approve")
            .await
            .unwrap();
        assert_eq!(person_signed(&v)["presence"], "present", "{v:#}");
        // the project moves while the agent signs: the act is signed again, and lands once
        let slot: Mover = Arc::default();
        let hook = Arc::clone(&slot);
        let agent = FakeAgent::hooked(&[person_ed()], move |_| {
            let (e, section) = hook.lock().unwrap().take().unwrap();
            on(&*e, move |r| {
                write(
                    r,
                    "elsewhere",
                    json!([{"op": "create", "type": "claim", "parent": section, "body": "moves the log"}]),
                )
                .map(|_| ())
            })
            .unwrap();
        });
        let (w, section) = signed_world(agent.clone());
        *slot.lock().unwrap() = Some((Arc::clone(&w.engine), section.clone()));
        local_config(w.dir.path(), "always", Some(person_ed()), None);
        let p = proposal(&*w.engine, &section, &next());
        let v = over(new, &w, 1, ask(&w.card(&p.r#ref)), "approve")
            .await
            .unwrap();
        person_signed(&v);
        assert_eq!(agent.signs(), 2, "signed again after the move");
    }

    // the call is cancelled while the agent signs: the ask stops, its worker is joined, and
    // always writes nothing
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let entered = Mutex::new(Some(entered_tx));
    let release = Mutex::new(release_rx);
    let agent = FakeAgent::hooked(&[person_ed()], move |_| {
        if let Some(t) = entered.lock().unwrap().take() {
            t.send(()).unwrap();
        }
        release.lock().unwrap().recv().unwrap();
    });
    let (w, section) = signed_world(agent.clone());
    local_config(w.dir.path(), "always", Some(person_ed()), None);
    let p = proposal(&*w.engine, &section, "cancelled");
    let item = w.card(&p.r#ref);
    let before = w.seq();
    let mut c = Client::inline(&w, 1, form_caps());
    let h = handle_of(&mut c, ask(&item)).await;
    let id = c.start(ask(&item), retry(&h, choice("approve"))).await;
    entered_rx.await.unwrap();
    c.send(
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": id, "reason": "stop"}}),
    )
    .await;
    // rmcp reads its lines in order: the reply to this ping says the cancel has been read
    let ping =
        json!({"jsonrpc": "2.0", "id": 900, "method": "ping", "params": {"_meta": c.meta()}});
    c.send(ping).await;
    assert_eq!(c.recv().await["id"], 900);
    // a later call waits for its turn, so its reply says the worker has ended
    c.start(
        json!({"ref": "x", "card_hash": "x", "elicit": true}),
        json!({}),
    )
    .await;
    release_tx.send(()).unwrap();
    let msg = c.recv().await;
    assert_ne!(msg["id"], id, "rmcp drops a cancelled call's reply: {msg}");
    assert_eq!(
        w.seq(),
        before,
        "a cancelled ask under always lands nothing"
    );
    assert!(w.card(&p.r#ref).card_hash == item.card_hash);
}

// -- the goldens

/// An engine whose writes run on a pinned clock and seeded ids, on whichever thread the blocking
/// pool gives them.
struct Pinned(Local);

impl Engine for Pinned {
    fn read(&self) -> Arc<ReadState> {
        self.0.read()
    }
    fn write(&self, f: WriteFn) -> Result<Value, RdyError> {
        rdy_model::seed_ids_for_tests(0x5549_0010);
        let _clock = pin_clock(1_770_000_000_000);
        let out = self.0.write(f);
        rdy_model::unseed_ids_for_tests();
        out
    }
    fn read_serialized(&self, f: rdy_engine::engine::ReadFn) -> Result<Value, RdyError> {
        self.0.read_serialized(f)
    }
    fn republish(&self) -> Result<(), RdyError> {
        self.0.republish()
    }
    fn open_report(&self) -> OpenReport {
        self.0.open_report()
    }
    fn root(&self) -> PathBuf {
        self.0.root()
    }
    fn briefs(&self) -> Arc<BriefRegistry> {
        self.0.briefs()
    }
    fn leases(&self) -> Arc<LeaseRegistry> {
        self.0.leases()
    }
}

struct Unseed;
impl Drop for Unseed {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/mcp")
        .join(format!("{name}.json"))
}

/// The opaque handle is the one value a protocol golden masks.
fn masked(mut v: Value) -> Value {
    if v.get("requestState").is_some() {
        v["requestState"] = json!("<handle>");
    }
    v
}

fn check_golden(name: &str, v: Value) {
    let text = format!("{}\n", serde_json::to_string_pretty(&masked(v)).unwrap());
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::write(golden_path(name), &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(golden_path(name))
        .unwrap_or_else(|_| panic!("{name}: no golden file"));
    assert_eq!(want, text, "{name}: the reply changed from the golden");
}

/// A seeded project: canon and one version of the batch, on this thread's pinned clock.
fn golden_world() -> (World, rdy_engine::pending::Item) {
    rdy_model::seed_ids_for_tests(0x5549_000A);
    let _unseed = Unseed;
    let _clock = pin_clock(1_760_000_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    write(
        &mut r,
        "one",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A claim of one."}]),
    )
    .unwrap();
    rdy_qa::intake::answer_open_items(&mut r, &writer(), "one");
    let p = r
        .propose(&writer(), "one", "one", Some("Adds one claim."))
        .unwrap();
    person_is_qa(dir.path());
    let e = Pinned(Local::new(r));
    let item = presented_item(&e.read(), PERSON, &p.r#ref, None).unwrap();
    (
        World {
            dir,
            engine: Arc::new(e),
            hub: NotifyHub::new(),
            agent: Arc::new(UnavailableAgent),
        },
        item,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn ui10_contract_and_goldens_are_additive() {
    let (w, item) = golden_world();
    let mut c = Client::inline(&w, 1, form_caps());
    let first = c.answer(ask(&item), json!({})).await;
    check_golden("rdy_answer--input-required", first.clone());
    let h = first["requestState"].as_str().unwrap();
    let declined = c
        .answer(ask(&item), retry(h, json!({"action": "decline"})))
        .await;
    check_golden("rdy_answer--declined", declined);
    let h = handle_of(&mut c, ask(&item)).await;
    let cancelled = c
        .answer(ask(&item), retry(&h, json!({"action": "cancel"})))
        .await;
    check_golden("rdy_answer--cancelled", cancelled);
    let h = handle_of(&mut c, ask(&item)).await;
    let elicited = c.answer(ask(&item), retry(&h, choice("reject"))).await;
    check_golden("rdy_answer--elicited", elicited.clone());
    let v = done(&elicited).unwrap();
    for key in [
        "ref",
        "act",
        "option",
        "label",
        "assurance",
        "answer",
        "landing",
        "relayed_by",
        "next",
    ] {
        assert!(v.get(key).is_some(), "every AnswerInfo key stays: {key}");
    }
    let mut none = Client::inline(&w, 2, json!({}));
    let unavailable = none.answer(ask(&item), json!({})).await;
    check_golden("rdy_answer--elicitation-unavailable", unavailable);

    // the code has its row, and the signing topic and the instructions their new words
    let row = rdy_mcp::tools::ERROR_ROWS
        .iter()
        .find(|(c, _, _)| *c == "E_ELICITATION_UNAVAILABLE")
        .expect("the code's row");
    assert_eq!(
        row.1,
        "the MCP client cannot show rdy's card through form elicitation"
    );
    assert_eq!(row.2, UNAVAILABLE.2);
    assert!(rdy_mcp::tools::SIGNING_DOC.ends_with(
        "A UI approval is unsigned; sign it later with rdy_sign or rdy sign. With signing.sign: always, the UI refuses and names rdy approve. An elicited approval signs with the person's key (signing.key) through the caller's SSH agent, with no relay."
    ));
}
