//! mg2 (M3 gate, Task 2): the bounded QA client scripts a real elicitation exchange. An eliciting
//! session states a built protocol revision and its form capability, takes the server's form
//! request, gives it the simulated person's act and finishes the original call. The two
//! accepted flows run over the real `rdy mcp` daemon proxy; transport faults use a tiny shell
//! peer. Expiry is an injected clock; nothing waits for ten minutes.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{approver, seed_doc, write, writer};
use rdy_qa::intake::answer_open_items;
use rdy_qa::mcp::{
    CONTINUATION_BOUND, DEADLINE, EXCHANGE_MAX_CALLS, EXCHANGE_MAX_MESSAGES, Exchanged, McpClient,
    McpFailure, PersonAct, Reply, Revision,
};
use rdy_qa::platform;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const V_NEW: &str = "2026-07-28";
const V_NOV: &str = "2025-11-25";

fn form_caps() -> Value {
    json!({"elicitation": {"form": {}}})
}

/// The older revision's in-call flow: `initialize` with 2025-11-25 and the form capability.
fn in_call() -> Revision {
    Revision::Initialize {
        version: V_NOV.into(),
        capabilities: form_caps(),
    }
}

/// The 2026-07-28 flow: no `initialize`, the client context in each request.
fn inline() -> Revision {
    Revision::Inline {
        version: V_NEW.into(),
        capabilities: form_caps(),
    }
}

/// A peer: the native stub child running `script` (the steps of the `script` fixture mode), with
/// `arg` as the file its `record` and `append` steps write to.
fn peer(script: &str, arg: &Path) -> Command {
    crate::mcp_windows::child_fixture::script_command(script, Some(arg))
}

/// A peer's reply to `initialize`.
const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"peer","version":"0"}}}"#;

/// A peer script that answers `initialize`, reads the `initialized` notification, then runs
/// `then`.
fn after_init(then: &str) -> String {
    format!("read\nsay {INIT}\nread\n{then}")
}

/// The server's form request with JSON-RPC id `id`.
fn form_request(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"elicitation/create","params":{{"mode":"form","message":"Approve?","requestedSchema":{{"type":"object","properties":{{"choice":{{"type":"string","enum":["approve","reject"]}}}},"required":["choice"]}}}}}}"#
    )
}

/// A tool result for call `id` whose value is `{"ok": true}`.
fn tool_reply(id: u64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"result":{{"content":[{{"type":"text","text":"{{\"ok\":true}}"}}]}}}}"#
    )
}

fn accept(choice: &str) -> PersonAct {
    PersonAct::Answer(json!({"action": "accept", "content": {"choice": choice}}))
}

/// The kinds of `x` in order: one word each.
fn kinds(x: &[Exchanged]) -> Vec<&'static str> {
    x.iter()
        .map(|e| match e {
            Exchanged::Request(_) => "request",
            Exchanged::ClientNotification(_) => "client-notification",
            Exchanged::Response(_) => "response",
            Exchanged::ServerRequest(_) => "server-request",
            Exchanged::Reply(_) => "reply",
            Exchanged::Notification(_) => "notification",
        })
        .collect()
}

/// The value of an accepted reply.
#[track_caller]
fn value(r: Result<Reply, McpFailure>) -> Value {
    match r {
        Ok(Reply::Ok(v)) => v,
        other => panic!("expected a result, got {other:?}"),
    }
}

// ---------------------------------------------------------------- the served project

/// A served project whose canon holds a seed doc, with views `one` and `two` each proposing one
/// claim (with a summary) for the person `human:qa`, and the refs of the two proposals.
fn served() -> (Served, [String; 2]) {
    let s = Served::new();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).expect("open the served project");
    let (_, section, _) = seed_doc(&mut r, "seed");
    let p = r
        .propose(&writer(), "seed", "Seed", Some("Seeds."))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    let mut refs = Vec::new();
    for view in ["one", "two"] {
        write(
            &mut r,
            view,
            json!([{"op": "create", "type": "claim", "parent": section, "body": format!("A claim of {view}.")}]),
        )
        .unwrap();
        answer_open_items(&mut r, &writer(), view);
        let p = r
            .propose(&writer(), view, view, Some("Adds one claim."))
            .unwrap();
        refs.push(p.r#ref);
    }
    r.close().unwrap();
    (s, [refs[0].clone(), refs[1].clone()])
}

/// A bounded eliciting session over `s`'s proxy, as agent:a1 (run a1, writer).
fn agent(s: &Served, revision: Revision) -> McpClient {
    let mut c = McpClient::try_launch_eliciting(McpClient::proxy(s), revision)
        .unwrap_or_else(|f| panic!("launch the proxy: {f:?}"));
    let r = c
        .try_call(
            "rdy_session",
            json!({"actor": "agent:a1", "run": "a1", "role": "writer"}),
        )
        .expect("rdy_session");
    assert!(r.is_ok(), "{r:?}");
    c
}

/// The approve card of proposal `pref` in the person's queue, read whole.
fn card(c: &mut McpClient, pref: &str) -> Option<Value> {
    let q = value(c.try_call("rdy_pending", json!({})));
    assert!(
        q.get("next").is_none_or(Value::is_null),
        "one page holds the queue: {q}"
    );
    q["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["kind"] == "approve" && i["ref"] == pref)
        .cloned()
}

fn ask(card: &Value) -> Value {
    json!({"ref": card["ref"], "card_hash": card["card_hash"], "elicit": true})
}

/// A person who checks that the form shows `card` word for word, then acts.
fn person(card: &Value, act: PersonAct) -> impl FnMut(&Value) -> PersonAct {
    let text = card["card"].clone();
    let mut act = Some(act);
    move |req: &Value| {
        assert_eq!(req["method"], "elicitation/create", "{req}");
        assert_eq!(
            req["params"]["message"], text,
            "the card word for word: {req}"
        );
        act.take().expect("one form per person")
    }
}

// ---------------------------------------------------------------- mg2 pins

#[test]
fn mg2_client_completes_both_built_elicitation_flows() {
    let (s, [one_ref, two_ref]) = served();

    // 2025-11-25: elicitation/create inside the call; decline writes nothing
    let mut c = agent(&s, in_call());
    assert_eq!(c.initialize_result()["result"]["protocolVersion"], V_NOV);
    let one = card(&mut c, &one_ref).expect("the card of one");
    let mut p = person(&one, PersonAct::Answer(json!({"action": "decline"})));
    let v = value(c.try_call_elicited("rdy_answer", ask(&one), &mut p));
    assert_eq!(v, json!({"state": "declined"}));
    let still = card(&mut c, &one_ref).expect("a declined card stays open");
    assert_eq!(still["card_hash"], one["card_hash"], "{still}");
    // the person's choice finishes the original call, as the person, locally
    let mut p = person(&one, accept("reject"));
    let v = value(c.try_call_elicited("rdy_answer", ask(&one), &mut p));
    assert_eq!(v["act"], "reject_proposal", "{v:#}");
    assert_eq!(v["option"], "reject", "{v:#}");
    assert_eq!(v["assurance"], "local", "{v:#}");
    assert!(v["relayed_by"].is_null(), "{v:#}");
    assert!(
        card(&mut c, &one_ref).is_none(),
        "the answered card left the queue"
    );
    // the form request and its answer sit inside the call, before its one reply
    let x = c.exchange().to_vec();
    assert!(x.len() <= EXCHANGE_MAX_MESSAGES);
    let call = x
        .iter()
        .rposition(
            |e| matches!(e, Exchanged::Request(r) if r["params"]["arguments"]["elicit"] == true),
        )
        .expect("the elicited call");
    assert_eq!(
        kinds(&x[call..call + 4]),
        ["request", "server-request", "response", "reply"]
    );
    let (Exchanged::Request(req), Exchanged::ServerRequest(asked), Exchanged::Response(answer)) =
        (&x[call], &x[call + 1], &x[call + 2])
    else {
        unreachable!()
    };
    assert_eq!(
        answer["id"], asked["id"],
        "the answer names the server's request"
    );
    assert_eq!(answer["result"]["content"]["choice"], "reject");
    let Exchanged::Reply(reply) = &x[call + 3] else {
        unreachable!()
    };
    assert_eq!(reply["id"], req["id"], "the reply ends the original call");
    c.try_finish(DEADLINE).expect("finish");

    // 2026-07-28: input_required, then the bound continuation; cancel writes nothing
    let mut c = agent(&s, inline());
    assert_eq!(c.initialize_result(), &Value::Null, "no initialize");
    let two = card(&mut c, &two_ref).expect("the card of two");
    let mut p = person(&two, PersonAct::Answer(json!({"action": "cancel"})));
    let v = value(c.try_call_elicited("rdy_answer", ask(&two), &mut p));
    assert_eq!(v, json!({"state": "cancelled"}));
    let still = card(&mut c, &two_ref).expect("a cancelled card stays open");
    assert_eq!(still["card_hash"], two["card_hash"], "{still}");
    let mut p = person(&two, accept("approve"));
    let v = value(c.try_call_elicited("rdy_answer", ask(&two), &mut p));
    assert_eq!(v["act"], "approve", "{v:#}");
    assert_eq!(v["assurance"], "local", "{v:#}");
    assert_eq!(v["landing"]["assurance"], "local", "{v:#}");
    assert!(
        card(&mut c, &two_ref).is_none(),
        "the approved card left the queue"
    );
    let x = c.exchange().to_vec();
    assert!(x.len() <= EXCHANGE_MAX_MESSAGES);
    let first = x
        .iter()
        .rposition(|e| matches!(e, Exchanged::Request(r) if r["params"]["arguments"]["elicit"] == true && r["params"].get("requestState").is_none()))
        .expect("the first ask");
    assert_eq!(
        kinds(&x[first..first + 4]),
        ["request", "reply", "request", "reply"]
    );
    let (Exchanged::Request(ask1), Exchanged::Reply(asked), Exchanged::Request(retry)) =
        (&x[first], &x[first + 1], &x[first + 2])
    else {
        unreachable!()
    };
    assert_eq!(
        ask1["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        V_NEW
    );
    assert_eq!(asked["result"]["resultType"], "input_required", "{asked}");
    assert_eq!(
        retry["params"]["requestState"],
        asked["result"]["requestState"]
    );
    assert_eq!(
        retry["params"]["arguments"], ask1["params"]["arguments"],
        "the same call"
    );
    assert_eq!(
        retry["params"]["inputResponses"]["answer"]["content"]["choice"],
        "approve"
    );
    c.try_finish(DEADLINE).expect("finish");
}

#[test]
fn mg2_client_keeps_notifications_and_call_ids_separate() {
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen");
    // the default session's initialize and calls stay byte for byte as before
    let c = McpClient::try_launch_bounded(
        peer(
            r#"read
record
say {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"peer","version":"0"}}}
read
append
drain"#,
            &seen,
        ),
        platform::RELEASE_CHILD_OUTPUT_MAX,
    )
    .expect("a default session");
    assert!(c.exchange().is_empty(), "a default session records nothing");
    c.try_finish(DEADLINE).expect("finish");
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        concat!(
            r#"{"id":1,"jsonrpc":"2.0","method":"initialize","params":{"capabilities":{},"clientInfo":{"name":"rdy-qa","version":"0"},"protocolVersion":"2025-06-18"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n"
        )
    );

    // the server's form request reuses the call's id 2, between two notifications that name
    // it: none of them is the call's reply
    let progress = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":2,"progress":1}}"#;
    let note = r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"id 2"}}"#;
    let script = after_init(&format!(
        "read\nsay {progress}\nsay {}\nread\nrecord\nsay {note}\nsay {}\ndrain",
        form_request(2),
        tool_reply(2)
    ));
    let mut c = McpClient::try_launch_eliciting(peer(&script, &seen), in_call()).expect("launch");
    let mut asked = 0;
    let mut p = |req: &Value| {
        asked += 1;
        assert_eq!(req["id"], 2, "{req}");
        accept("approve")
    };
    let v = value(c.try_call_elicited("rdy_answer", json!({"ref": "r"}), &mut p));
    assert_eq!(v, json!({"ok": true}));
    assert_eq!(asked, 1);
    let methods: Vec<&str> = c
        .notifications()
        .iter()
        .map(|n| n["method"].as_str().unwrap())
        .collect();
    assert_eq!(methods, ["notifications/progress", "notifications/message"]);
    assert_eq!(
        kinds(c.exchange()),
        [
            "request",
            "reply",
            "client-notification",
            "request",
            "notification",
            "server-request",
            "response",
            "notification",
            "reply"
        ]
    );
    let answered: Value =
        serde_json::from_str(std::fs::read_to_string(&seen).unwrap().trim()).unwrap();
    assert_eq!(
        answered,
        json!({"jsonrpc": "2.0", "id": 2, "result": {"action": "accept", "content": {"choice": "approve"}}})
    );
    // the 2025-11-25 initialize states the revision and the form capability
    let Exchanged::Request(init) = &c.exchange()[0] else {
        unreachable!()
    };
    assert_eq!(init["params"]["protocolVersion"], V_NOV);
    assert_eq!(init["params"]["capabilities"], form_caps());
    c.try_finish(DEADLINE).expect("finish");

    // a server request that is not a form, and a form request outside an elicited call, are
    // protocol failures with no reply
    for (what, script, elicited) in [
        (
            "a ping inside an elicited call",
            after_init(
                r#"read
say {"jsonrpc":"2.0","id":5,"method":"ping"}
hang"#,
            ),
            true,
        ),
        (
            "a form request inside a plain call",
            after_init(&format!("read\nsay {}\nhang", form_request(5))),
            false,
        ),
    ] {
        let mut c =
            McpClient::try_launch_eliciting(peer(&script, &seen), in_call()).expect("launch");
        let mut p = |_: &Value| panic!("{what}: the person is never asked");
        let got = if elicited {
            c.try_call_elicited("rdy_answer", json!({}), &mut p)
        } else {
            c.try_call("rdy_answer", json!({}))
        };
        assert_eq!(got.map(|_| ()), Err(McpFailure::Protocol), "{what}");
        assert!(
            !c.exchange()
                .iter()
                .any(|e| matches!(e, Exchanged::Reply(r) if r["id"] == 2)),
            "{what}: no reply"
        );
    }
}

#[test]
fn mg2_client_disconnect_leaves_no_answer() {
    let dir = tempfile::tempdir().unwrap();
    let unused = dir.path().join("unused");
    // the server goes away while its form waits: the call fails, with no reply
    let script = after_init(&format!("read\nsay {}\nexit 0", form_request(7)));
    let mut c = McpClient::try_launch_eliciting(peer(&script, &unused), in_call()).expect("launch");
    let mut p = |_: &Value| accept("approve");
    assert_eq!(
        c.try_call_elicited("rdy_answer", json!({}), &mut p)
            .map(|_| ()),
        Err(McpFailure::Io)
    );
    assert!(
        !c.exchange()
            .iter()
            .any(|e| matches!(e, Exchanged::Reply(r) if r["id"] == 2)),
        "no fabricated reply"
    );
    // the person leaves: the session closes, and no answer is sent
    let script = after_init(&format!("read\nsay {}\ndrain", form_request(7)));
    let mut c = McpClient::try_launch_eliciting(peer(&script, &unused), in_call()).expect("launch");
    let mut p = |_: &Value| PersonAct::Leave;
    assert_eq!(
        c.try_call_elicited("rdy_answer", json!({}), &mut p)
            .map(|_| ()),
        Err(McpFailure::Io)
    );
    assert_eq!(
        kinds(c.exchange()),
        [
            "request",
            "reply",
            "client-notification",
            "request",
            "server-request"
        ]
    );

    // over the daemon proxy: the person leaves either flow's form, and both cards stay open
    // with no answer
    let (s, refs) = served();
    for (revision, view) in [(in_call(), &refs[0]), (inline(), &refs[1])] {
        let mut c = agent(&s, revision);
        let shown = card(&mut c, view).expect("the card");
        let mut p = person(&shown, PersonAct::Leave);
        assert_eq!(
            c.try_call_elicited("rdy_answer", ask(&shown), &mut p)
                .map(|_| ()),
            Err(McpFailure::Io),
            "{view}"
        );
        drop(c);
        let mut after = agent(
            &s,
            Revision::Initialize {
                version: "2025-06-18".into(),
                capabilities: json!({}),
            },
        );
        let open = card(&mut after, view).expect("the card stays open");
        assert_eq!(open["card_hash"], shown["card_hash"], "{view}: {open}");
        after.try_finish(DEADLINE).expect("finish");
    }
}

#[test]
fn mg2_client_bounds_an_unfinished_form_exchange() {
    let dir = tempfile::tempdir().unwrap();
    let unused = dir.path().join("unused");
    // a server that asks again after every answer: the exchange stops at its message cap
    let script = after_init(&format!("read\nloopsay {}", form_request(7)));
    let mut c = McpClient::try_launch_eliciting(peer(&script, &unused), in_call()).expect("launch");
    let mut p = |_: &Value| accept("approve");
    assert_eq!(
        c.try_call_elicited("rdy_answer", json!({}), &mut p)
            .map(|_| ()),
        Err(McpFailure::OutputLimit)
    );
    assert_eq!(c.exchange().len(), EXCHANGE_MAX_MESSAGES, "exactly the cap");
    assert!(
        !c.exchange()
            .iter()
            .any(|e| matches!(e, Exchanged::Reply(r) if r["id"] == 2))
    );

    // a server that answers every call with input_required: the retries stop at the call cap
    let script = "askforever";
    let mut c = McpClient::try_launch_eliciting(peer(script, &unused), inline()).expect("launch");
    let mut p = |_: &Value| accept("approve");
    assert_eq!(
        c.try_call_elicited("rdy_answer", json!({}), &mut p)
            .map(|_| ()),
        Err(McpFailure::OutputLimit)
    );
    assert_eq!(c.calls(), EXCHANGE_MAX_CALLS);
    let requests = c
        .exchange()
        .iter()
        .filter(|e| matches!(e, Exchanged::Request(_)))
        .count();
    assert_eq!(requests, EXCHANGE_MAX_CALLS, "no call past the cap is sent");

    // the continuation bound is the server's ten minutes, on an injected clock: a person who
    // answers just inside it continues; one who takes the whole bound sends nothing
    assert_eq!(CONTINUATION_BOUND, rdy_mcp::elicitation::EXPIRY);
    let required = r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"input_required","requestState":"h1","inputRequests":{"answer":{"method":"elicitation/create","params":{"mode":"form","message":"m","requestedSchema":{"type":"object"}}}}}}"#;
    let complete = r#"{"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","content":[{"type":"text","text":"{\"ok\":true}"}]}}"#;
    for (took, want) in [
        (CONTINUATION_BOUND - Duration::from_secs(1), Ok(())),
        (CONTINUATION_BOUND, Err(McpFailure::Deadline)),
    ] {
        let script = format!("read\nsay {required}\nread\nsay {complete}\ndrain");
        let mut c =
            McpClient::try_launch_eliciting(peer(&script, &unused), inline()).expect("launch");
        let base = Instant::now();
        let at = Arc::new(Mutex::new(base));
        let clock = Arc::clone(&at);
        c.set_clock(Arc::new(move || *clock.lock().unwrap()));
        let mut p = |_: &Value| {
            *at.lock().unwrap() = base + took;
            accept("approve")
        };
        let got = c.try_call_elicited("rdy_answer", json!({}), &mut p);
        assert_eq!(got.as_ref().map(|_| ()).map_err(|f| *f), want, "{took:?}");
        let sent = c
            .exchange()
            .iter()
            .filter(|e| matches!(e, Exchanged::Request(_)))
            .count();
        match want {
            Ok(()) => {
                assert_eq!(value(got), json!({"ok": true}));
                assert_eq!(sent, 2, "the retry was sent");
            }
            Err(_) => assert_eq!(sent, 1, "an expired continuation is never sent"),
        }
    }
}
