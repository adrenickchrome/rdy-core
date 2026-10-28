//! id4 (D-PWB82, D-ME2C0, D-HA7YZ, D-KQ4CA): rdy never joins an actor and a run into one string.
//! A new owner is `{actor, run?}` on every surface (engine, daemon, command line, MCP); a relay
//! flag names the relaying agent's actor alone, and the run always comes from the session; every
//! print shows `<actor> (run <run>)`, or the actor alone. So `agent:a/b` with no run and `agent:a`
//! with run `b` stay two owners, and no input is parsed into an actor and a run.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::{RelayInput, relayed};
use rdy_engine::engine::Local;
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::tensions::{TensionKind, TensionSeverity, tension_label};
use rdy_engine::view_api::{OwnerInput, ViewRequest, run_view};
use rdy_engine::*;
use rdy_model::{ActorRun, FieldValue, Role};
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::intake::{PERSON, batch_project, pending, verdict_on};
use rdy_qa::mcp::McpClient;
use rdy_qa::tensions::{ag, created, one, put, upd};
use rdy_store::Relay;
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::{Value, json};

/// The actor grammar's fix (D-ME2C0).
const ACTOR_FIX: &str =
    "use human:<name>, agent:<name>, agent:<handle>/<name> or ci:<repo_id>/<name>";
/// The run grammar's fix (D-ME2C0).
const RUN_FIX: &str = "use 1 to 64 letters, digits, -, _ or ., with a letter or digit at each end; not none, work, canon, inflight or reaffirm-\u{2026}";
/// The fix of `own` with an owner and none, or with neither.
const OWN_FIX: &str = "pass owner: {actor, run?}, or none: true to leave it unowned";

const DAY_MS: i64 = 86_400_000;

fn owner(actor: &str, run: Option<&str>) -> OwnerInput {
    OwnerInput {
        actor: actor.into(),
        run: run.map(str::to_string),
    }
}

fn req(v: Value) -> ViewRequest {
    serde_json::from_value(v).unwrap()
}

fn claim(parent: &str, body: &str) -> Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

fn refused(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

/// A refusal printed as JSON (the command line's stderr, or an MCP tool error).
fn json_refused(v: &Value, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str(), v["fix"].as_str()),
        (Some(code), Some(message), Some(fix)),
        "{v}"
    );
}

/// The actor refusal of `value`, as the grammar words it.
fn bad_actor(value: &str) -> String {
    format!("the actor {value:?} is not in rdy's actor form")
}

/// The run refusal of `value`, as the grammar words it.
fn bad_run(value: &str) -> String {
    format!("the run {value:?} is not in rdy's run form")
}

/// A project with the seed doc landed and view `a`, owned by `agent:a/r1`; the seed section.
fn views_project() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write_as(
        &mut r,
        &as_agent("agent:a", "r1"),
        "a",
        claim(&section, "a"),
    )
    .unwrap();
    (d, r, section)
}

/// A served project with the seed doc landed and view `a`, owned by `agent:a/r1`.
fn served_views() -> Served {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write_as(
        &mut r,
        &as_agent("agent:a", "r1"),
        "a",
        claim(&section, "a"),
    )
    .unwrap();
    r.close().unwrap();
    s
}

fn owner_of(r: &Rdy, v: &str) -> Option<(String, Option<String>)> {
    r.read()
        .view_head(v)
        .unwrap()
        .owner
        .clone()
        .map(|o| (o.actor, o.run))
}

/// The owner in a reply's JSON: `(actor, run)`, null when absent.
fn owner_json(o: &Value) -> (Value, Value) {
    (o["actor"].clone(), o["run"].clone())
}

/// The end of the project's log, with the daemon stopped.
fn log_end(s: &Served) -> Location {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let end = r.store().log_end();
    r.close().unwrap();
    end
}

/// The kinds of the frames written after `from`, with the daemon stopped.
fn frames_after(s: &Served, from: Location) -> Vec<u8> {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let mut kinds = vec![];
    r.store()
        .for_each_frame_from(from, r.store().log_end(), |_, k, _, _, _| {
            kinds.push(k);
            Ok(())
        })
        .unwrap();
    r.close().unwrap();
    kinds
}

fn args(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|a| a.to_string()).collect()
}

/// The person's words at the command line: `relayed_by` and the quote.
fn cli(by: &str, quote: &str) -> RelayInput {
    RelayInput {
        relayed_by: Some(by.into()),
        evidence: Some(quote.into()),
        question: None,
        session_ref: None,
        kind: None,
    }
}

fn text_of(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{o:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn id4_owner_pair_is_unambiguous_on_every_surface() {
    // the engine: the stored owner is the pair as given, and only that pair owns the view
    let (_d, mut r, section) = views_project();
    let me = human_writer();
    let whole = agent("agent:a/b", None);
    let split = agent("agent:a", Some("b"));
    let whole_r7 = agent("agent:a/b", Some("R7"));
    let commit = |r: &mut Rdy, s: &Session, body: &str| write_as(r, s, "a", claim(&section, body));
    r.view_own(&me, "a", Some(&owner("agent:a/b", None)))
        .unwrap();
    assert_eq!(owner_of(&r, "a"), Some(("agent:a/b".into(), None)));
    let e = commit(&mut r, &split, "split").unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_VIEW_OWNER, "view \"a\" is owned by agent:a/b"),
        "{e}"
    );
    commit(&mut r, &whole, "whole").unwrap();
    r.view_own(&me, "a", Some(&owner("agent:a", Some("b"))))
        .unwrap();
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:a".into(), Some("b".into())))
    );
    let e = commit(&mut r, &whole, "whole again").unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_VIEW_OWNER, "view \"a\" is owned by agent:a (run b)"),
        "{e}"
    );
    commit(&mut r, &split, "split").unwrap();
    r.view_own(&me, "a", Some(&owner("agent:a/b", Some("R7"))))
        .unwrap();
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:a/b".into(), Some("R7".into())))
    );
    for s in [&whole, &split] {
        let e = commit(&mut r, s, "not the owner").unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (E_VIEW_OWNER, "view \"a\" is owned by agent:a/b (run R7)"),
            "{e}"
        );
    }
    commit(&mut r, &whole_r7, "the owner").unwrap();
    r.fsck().expect("fsck");

    // the daemon, through the command line: an actor and --run
    let s = served_views();
    let cases: [(&[&str], (Value, Value)); 3] = [
        (&["agent:a/b"], (json!("agent:a/b"), json!(null))),
        (
            &["agent:a/b", "--run", "R7"],
            (json!("agent:a/b"), json!("R7")),
        ),
        (&["agent:a", "--run", "b"], (json!("agent:a"), json!("b"))),
    ];
    for (extra, want) in &cases {
        let mut a = strings(&["--json", "view", "own", "a"]);
        a.extend(strings(extra));
        let (ok, v) = s.json(&args(&a));
        assert!(ok, "{v}");
        assert_eq!(owner_json(&v["owner"]), *want, "{v}");
        let (ok, v) = s.json(&["--json", "view", "show", "a"]);
        assert!(ok, "{v}");
        assert_eq!(owner_json(&v["head"]["owner"]), *want, "{v}");
    }
    // view a is now agent:a with run b: agent:a/b with no run does not own it
    let (ok, v) = s.json_as("agent:a/b", None, &["--json", "view", "close", "a"]);
    assert!(!ok);
    assert_eq!(
        (v["code"].as_str(), v["message"].as_str()),
        (
            Some("E_VIEW_OWNER"),
            Some("view \"a\" is owned by agent:a (run b)")
        ),
        "{v}"
    );

    // MCP: the owner object, the run omitted, null or given
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "writer"}),
    )
    .ok();
    for (o, want) in [
        (
            json!({"actor": "agent:a/b"}),
            (json!("agent:a/b"), json!(null)),
        ),
        (
            json!({"actor": "agent:a/b", "run": "R7"}),
            (json!("agent:a/b"), json!("R7")),
        ),
        (
            json!({"actor": "agent:a/b", "run": null}),
            (json!("agent:a/b"), json!(null)),
        ),
    ] {
        let v = c
            .call(
                "rdy_view",
                json!({"op": "own", "name": "a", "owner": o, "evidence": "Hand a over"}),
            )
            .ok();
        assert_eq!(owner_json(&v["owner"]), want, "{v}");
        let v = c.call("rdy_view", json!({"op": "show", "name": "a"})).ok();
        assert_eq!(owner_json(&v["head"]["owner"]), want, "{v}");
    }
    drop(c);

    // the command line with no daemon
    s.stop();
    let (ok, v) = s.json(&[
        "--json",
        "--no-daemon",
        "view",
        "own",
        "a",
        "agent:a/b",
        "--run",
        "R9",
    ]);
    assert!(ok, "{v}");
    assert_eq!(
        owner_json(&v["owner"]),
        (json!("agent:a/b"), json!("R9")),
        "{v}"
    );
    let (ok, v) = s.json(&["--json", "--no-daemon", "view", "own", "a", "agent:a/b"]);
    assert!(ok, "{v}");
    assert_eq!(
        owner_json(&v["owner"]),
        (json!("agent:a/b"), json!(null)),
        "{v}"
    );
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    assert_eq!(owner_of(&r, "a"), Some(("agent:a/b".into(), None)));
    r.fsck().expect("fsck");
}

#[test]
fn id4_owner_input_rejects_old_shapes_without_writes() {
    // the engine's dispatcher: every refusal before a write
    let (dir, r, _section) = views_project();
    let end = r.store().log_end();
    let e = Local::new(r);
    let me = human_writer();
    let own = |o: Value| {
        let mut v = json!({"op": "own", "view": "a"});
        v.as_object_mut()
            .unwrap()
            .extend(o.as_object().unwrap().clone());
        run_view(&e, &me, PERSON, "work", req(v)).unwrap_err()
    };
    let long = format!("agent:{}", "a".repeat(130));
    let clipped = format!("{}\u{2026}", &long[..80]);
    for (o, message, fix) in [
        (
            json!({"owner": {"actor": "Agent:A"}}),
            bad_actor("Agent:A"),
            ACTOR_FIX,
        ),
        (json!({"owner": {"actor": ""}}), bad_actor(""), ACTOR_FIX),
        (
            json!({"owner": {"actor": long}}),
            bad_actor(&clipped),
            ACTOR_FIX,
        ),
        (
            json!({"owner": {"actor": "agent:a/b/c"}}),
            bad_actor("agent:a/b/c"),
            "pass the run with --run",
        ),
        (
            json!({"owner": {"actor": "agent:a/b//c"}}),
            bad_actor("agent:a/b//c"),
            ACTOR_FIX,
        ),
        (
            json!({"owner": {"actor": "agent:a", "run": "none"}}),
            bad_run("none"),
            RUN_FIX,
        ),
        (
            json!({"owner": {"actor": "agent:a", "run": ""}}),
            bad_run(""),
            RUN_FIX,
        ),
        (
            json!({"owner": {"actor": "agent:a", "run": "b/c"}}),
            bad_run("b/c"),
            RUN_FIX,
        ),
        // the actor is checked before the run
        (
            json!({"owner": {"actor": "agent:A", "run": "none"}}),
            bad_actor("agent:A"),
            ACTOR_FIX,
        ),
        (
            json!({"owner": {"actor": "agent:a"}, "none": true}),
            "own takes an owner or none, not both".to_string(),
            OWN_FIX,
        ),
        (json!({}), "own needs an owner or none".to_string(), OWN_FIX),
    ] {
        refused(&own(o), E_BAD_INPUT, &message, fix);
    }

    // the daemon's op: the shared parser's shape errors and the nested unknown key
    let a = agent("agent:relay", Some("r1"));
    let cx = OpContext {
        engine: &e,
        session: &me,
        human: PERSON,
        cwd: dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let ax = OpContext { session: &a, ..cx };
    let err = dispatch(
        &cx,
        "view",
        json!({"op": "own", "name": "a", "owner": {"actor": "agent:a/b", "rnu": "R7"}}),
    )
    .unwrap_err();
    refused(
        &err,
        E_BAD_INPUT,
        "view: unknown field \"owner.rnu\"",
        "did you mean \"run\"? the fields are actor, run",
    );
    let shape = "args for view: {op, name?, from?, base?, pin?, view?, views?, changes?, cascade?, a?, b?, all?, stale?, reason?, owner?: {actor, run?}, none?, target?, block?, landed?, max_tokens?, relayed_by?, evidence?, question?, session_ref?}; op is create, fork, include, exclude, replay, compare, list, show, close, own, resolve or undo; own {name, owner?: {actor, run?}, none?, evidence, question?, session_ref?}";
    for (o, message) in [
        (
            json!("agent:a/b"),
            "view: invalid type: string \"agent:a/b\", expected struct OwnerInput",
        ),
        (json!({"run": "R7"}), "view: missing field `actor`"),
    ] {
        let err = dispatch(&cx, "view", json!({"op": "own", "name": "a", "owner": o})).unwrap_err();
        refused(&err, E_BAD_INPUT, message, shape);
    }
    // a relayed hand-over with a bad owner: the relay passes, the owner is refused
    let err = dispatch(
        &ax,
        "view",
        json!({"op": "own", "name": "a", "owner": {"actor": "agent:a/b/c"},
            "relayed_by": "agent:relay", "evidence": "give it to a/b/c"}),
    )
    .unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            "E_BAD_INPUT",
            bad_actor("agent:a/b/c").as_str(),
            "pass the run with --run"
        ),
        "{err}"
    );
    e.close().unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    assert_eq!(r.store().log_end(), end, "nothing was written");
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:a".into(), Some("r1".into())))
    );
    drop(r);

    // MCP and the command line
    let s = served_views();
    let end = log_end(&s);
    let mut c = McpClient::start_proxy(&s);
    c.call(
        "rdy_session",
        json!({"actor": "agent:m", "run": "m1", "role": "writer"}),
    )
    .ok();
    let mcp_own = |c: &mut McpClient, o: Value| match c.call(
        "rdy_view",
        json!({"op": "own", "name": "a", "owner": o, "evidence": "Hand a over"}),
    ) {
        rdy_qa::mcp::Reply::ToolError(v) => v,
        other => panic!("expected a tool error, got {other:?}"),
    };
    let v = mcp_own(&mut c, json!({"actor": "agent:a/b", "rnu": "R7"}));
    json_refused(
        &v,
        "E_BAD_INPUT",
        "rdy_view: unknown parameter \"owner.rnu\"",
        "did you mean \"run\"? the parameters here are actor, run",
    );
    for (o, message, fix) in [
        (
            json!("agent:a/b"),
            "rdy_view: parameter \"owner\" must be an object, not a string",
            "pass \"owner\" as an object",
        ),
        (
            json!({"run": "R7"}),
            "rdy_view: parameter \"owner.actor\" is required",
            "pass \"owner.actor\" as a string",
        ),
    ] {
        json_refused(&mcp_own(&mut c, o), "E_BAD_INPUT", message, fix);
    }
    let v = mcp_own(&mut c, json!({"actor": "agent:a/b/c"}));
    json_refused(
        &v,
        "E_BAD_INPUT",
        &bad_actor("agent:a/b/c"),
        "pass the run with --run",
    );
    let v = mcp_own(&mut c, json!({"actor": "agent:a", "run": ""}));
    json_refused(&v, "E_BAD_INPUT", &bad_run(""), RUN_FIX);
    drop(c);
    for (extra, message, fix) in [
        (
            &["--run", "R7"][..],
            "--run needs an owner".to_string(),
            "pass an actor before --run, or omit --run",
        ),
        (
            &["--none", "--run", "R7"][..],
            "--run needs an owner".to_string(),
            "pass an actor before --run, or omit --run",
        ),
        (
            &["agent:a", "--none"][..],
            "own takes an owner or none, not both".to_string(),
            OWN_FIX,
        ),
        (&[][..], "own needs an owner or none".to_string(), OWN_FIX),
        (
            &["agent:a/b/c"][..],
            bad_actor("agent:a/b/c"),
            "pass the run with --run",
        ),
        (&["agent:a", "--run", "work"][..], bad_run("work"), RUN_FIX),
    ] {
        for direct in [false, true] {
            let mut a = strings(&["--json"]);
            if direct {
                s.stop();
                a.push("--no-daemon".into());
            }
            a.extend(strings(&["view", "own", "a"]));
            a.extend(strings(extra));
            let (ok, v) = s.json(&args(&a));
            assert!(!ok, "{a:?}: {v}");
            json_refused(&v, "E_BAD_INPUT", &message, fix);
        }
    }
    // no view op and no answer (a session may keep its own refs, which no refusal touches)
    let written = frames_after(&s, end);
    assert!(
        !written
            .iter()
            .any(|k| matches!(*k, kinds::VIEW_OP | kinds::ANSWER | kinds::CHANGE)),
        "{written:?}"
    );
    let r = Rdy::open(s.dir.path()).unwrap();
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:a".into(), Some("r1".into())))
    );
}

/// The relay's agent in a reply, as a pair.
fn pair(actor: &str, run: Option<&str>) -> ActorRun {
    ActorRun {
        actor: actor.into(),
        run: run.map(str::to_string),
    }
}

/// The three sessions every relayed act is tried with: `agent:relay` with run `chat` (whose
/// joined `agent:relay/chat` is another valid actor), and `agent:relay/chat`, a two-name actor,
/// with no run and with run `R7` (whose joined `agent:relay/chat/R7` is outside the form).
fn relayers() -> (Session, Session, Session) {
    (
        agent("agent:relay", Some("chat")),
        agent("agent:relay/chat", None),
        agent("agent:relay/chat", Some("R7")),
    )
}

/// The refusals of a joined relay flag, each before a write: an unequal actor for the session
/// with run `chat`, the grammar for the two-name session with run `R7`.
/// A relayed act tried as `session` with the relay flag `by`: the relayer it recorded.
type RelayedAct<'a> = dyn FnMut(&Session, &str) -> Result<Option<ActorRun>, RdyError> + 'a;

fn joined_refusals(act: &mut RelayedAct<'_>) {
    let (one, _two, two_r7) = relayers();
    let e = act(&one, "agent:relay/chat").unwrap_err();
    refused(
        &e,
        E_PERMISSION,
        "agent:relay (run chat) relays only as itself, not as agent:relay/chat",
        "pass --relayed-by agent:relay with the person's words as evidence",
    );
    let e = act(&two_r7, "agent:relay/chat/R7").unwrap_err();
    refused(
        &e,
        E_BAD_INPUT,
        &bad_actor("agent:relay/chat/R7"),
        ACTOR_FIX,
    );
}

#[test]
fn id4_every_relay_command_uses_the_session_actor() {
    let (chat, two, two_r7) = relayers();
    // the table itself: the whole actor, and the session's run
    for (s, want) in [
        (&chat, pair("agent:relay", Some("chat"))),
        (&two, pair("agent:relay/chat", None)),
        (&two_r7, pair("agent:relay/chat", Some("R7"))),
    ] {
        let (who, relay) = relayed(s, PERSON, &cli(&s.actor, "yes"), false).unwrap();
        assert_eq!(who.actor, PERSON);
        assert_eq!(relay.map(|r| r.by), Some(want));
    }

    // view own
    let (_d, mut r, _section) = views_project();
    joined_refusals(&mut |s, by| {
        r.view_own_as(
            s,
            PERSON,
            &cli(by, "give it to z"),
            "a",
            Some(&owner("agent:z", None)),
        )
        .map(|i| i.relayed_by)
    });
    for (s, run) in [(&two, None), (&two_r7, Some("R7"))] {
        let info = r
            .view_own_as(
                s,
                PERSON,
                &cli("agent:relay/chat", "give it to z"),
                "a",
                Some(&owner("agent:z", None)),
            )
            .unwrap();
        assert_eq!(info.relayed_by, Some(pair("agent:relay/chat", run)));
    }

    // view close --stale
    let (_d, mut r, _section) = views_project();
    let later = now_ms() + DAY_MS * 2;
    joined_refusals(&mut |s, by| {
        r.view_close_stale_as(s, PERSON, &cli(by, "close them"), 0, later)
            .map(|c| c.relayed_by)
    });
    let c = r
        .view_close_stale_as(
            &two_r7,
            PERSON,
            &cli("agent:relay/chat", "close them"),
            0,
            later,
        )
        .unwrap();
    assert_eq!(c.relayed_by, Some(pair("agent:relay/chat", Some("R7"))));

    // tag
    let (_d, mut r) = project();
    seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    joined_refusals(&mut |s, by| {
        r.tag_as(s, PERSON, &cli(by, "tag it"), false, "v1.0.0", None, None)
            .map(|t| t.relayed_by)
    });
    let t = r
        .tag_as(
            &two_r7,
            PERSON,
            &cli("agent:relay/chat", "tag it"),
            false,
            "v1.0.0",
            None,
            None,
        )
        .unwrap();
    assert_eq!(t.relayed_by, Some(pair("agent:relay/chat", Some("R7"))));

    // propose (the person's work)
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "mine")).unwrap();
    joined_refusals(&mut |s, by| {
        r.propose_as(
            s,
            PERSON,
            &cli(by, "propose it"),
            false,
            "work",
            "t",
            None,
            Some("s"),
            None,
        )
        .map(|_| None)
    });
    r.propose_as(
        &two_r7,
        PERSON,
        &cli("agent:relay/chat", "propose it"),
        false,
        "work",
        "t",
        None,
        Some("A claim of mine."),
        None,
    )
    .unwrap();
    let (_, a) = r
        .store()
        .reader()
        .answer_of("propose:work")
        .unwrap()
        .expect("the propose answer");
    assert_eq!(a.relayed_by, Some(pair("agent:relay/chat", Some("R7"))));

    // approve, at the daemon: its own texts, the same order
    let (dir, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "x")).unwrap();
    let pref = r
        .propose(&writer(), "work", "t", Some("A claim."))
        .unwrap()
        .r#ref;
    let e = Local::new(r);
    let approve = |s: &Session, by: &str, quote: &str| {
        let cx = OpContext {
            engine: &e,
            session: s,
            human: PERSON,
            cwd: dir.path(),
            agent: &rdy_engine::signing_crypto::UnavailableAgent,
        };
        dispatch(
            &cx,
            "approve",
            json!({"proposal": pref, "relayed_by": by, "evidence": quote}),
        )
    };
    let err = approve(&chat, "agent:relay/chat", "yes, land it").unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            "E_PERMISSION",
            "agent:relay (run chat) relays only as itself, not as agent:relay/chat",
            "pass --relayed-by agent:relay with the person's words in --evidence"
        ),
        "{err}"
    );
    for quote in ["yes, land it", "  "] {
        let err = approve(&two_r7, "agent:relay/chat/R7", quote).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str(), err.fix.as_str()),
            (
                "E_BAD_INPUT",
                bad_actor("agent:relay/chat/R7").as_str(),
                ACTOR_FIX
            ),
            "{err}"
        );
    }
    // an empty quote with the session's own actor keeps its refusal
    let err = approve(&two_r7, "agent:relay/chat", "  ").unwrap_err();
    assert_eq!(err.message, "the evidence quote is empty", "{err}");
    let v = approve(&two_r7, "agent:relay/chat", "yes, land it").unwrap();
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay/chat", "run": "R7"}),
        "{v}"
    );
    e.close().unwrap();

    // a structured relay at the gate: the same grammar refusals, after the earlier rules
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "x")).unwrap();
    let p = r.propose(&writer(), "work", "t", Some("A claim.")).unwrap();
    let relay = |actor: &str, run: Option<&str>, quote: &str| LandOptions {
        relay: Some(Relay {
            by: pair(actor, run),
            evidence: rdy_model::RelayEvidence {
                quoted: quote.into(),
                session_ref: "cli".into(),
                question: None,
            },
        }),
        ..LandOptions::default()
    };
    for (opts, message, fix) in [
        (
            relay("agent:relay/chat/R7", None, "yes"),
            bad_actor("agent:relay/chat/R7"),
            ACTOR_FIX,
        ),
        (relay("", Some("R7"), "yes"), bad_actor(""), ACTOR_FIX),
        (
            relay("agent:relay/chat", Some(""), "yes"),
            bad_run(""),
            RUN_FIX,
        ),
        (
            relay("agent:relay/chat", Some("none"), "yes"),
            bad_run("none"),
            RUN_FIX,
        ),
        // the earlier rules keep their place: the quote, then the actor
        (
            relay("agent:relay/chat/R7", None, " "),
            "the evidence quote is empty".to_string(),
            "quote the human's words with --evidence",
        ),
    ] {
        let e = r.land_with(&approver(), &p.r#ref, opts).unwrap_err();
        refused(&e, E_BAD_INPUT, &message, fix);
    }
    let e = r
        .land_with(
            &approver(),
            p.r#ref.split('@').next().unwrap(),
            relay("agent:relay/chat/R7", None, "yes"),
        )
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_INPUT,
            "a relayed approval names the exact version it relays"
        ),
        "{e}"
    );
    let l = r
        .land_with(
            &approver(),
            &p.r#ref,
            relay("agent:relay/chat", Some("R7"), "yes"),
        )
        .unwrap();
    assert!(!l.landing.is_empty());

    // answer
    let (_d, mut r, b) = batch_project();
    let h = pending(&r)
        .items
        .iter()
        .find(|i| i.r#ref == b.r#ref)
        .expect("the batch")
        .card_hash
        .clone();
    let ask = |by: &str| AnswerRequest {
        r#ref: b.r#ref.clone(),
        option: "later".into(),
        card_hash: Some(h.clone()),
        note: None,
        relay: cli(by, "later, please"),
        brief: None,
    };
    joined_refusals(&mut |s, by| r.answer(s, PERSON, ask(by)).map(|i| i.relayed_by));
    let info = r.answer(&two_r7, PERSON, ask("agent:relay/chat")).unwrap();
    assert_eq!(info.relayed_by, Some(pair("agent:relay/chat", Some("R7"))));

    // semantic acceptance
    let (_d, mut r) = project();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", upd(&sp.c, "C contradicts B."));
    verdict_on(&mut r, &Scope::Inflight, &sp.b, &sp.c, "agent:r", true);
    let p = r.propose(&ag("a"), "a", "a", Some("C changes.")).unwrap();
    let accept = |by: &str| AcceptRequest {
        proposal: p.r#ref.clone(),
        advised_digest: p.advised_digest.clone().unwrap_or_default(),
        tensions: vec![],
        evidence: Some("yes, block on it".into()),
        question: None,
        session_ref: None,
        relayed_by: Some(by.into()),
    };
    joined_refusals(&mut |s, by| r.accept_advice(s, PERSON, accept(by)).map(|_| None));
    r.accept_advice(&two_r7, PERSON, accept("agent:relay/chat"))
        .unwrap();
    let t = one(&r, TensionKind::Semantic);
    assert_eq!(t.severity, TensionSeverity::Blocking);
    let acc = r.read().tension_info(&t).judged.unwrap().accepted.unwrap();
    assert_eq!(acc.relayed_by, Some(pair("agent:relay/chat", Some("R7"))));

    // dismissal of an advised tension: the person wrote no side
    let (_d, mut r) = project();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", upd(&sp.c, "C contradicts B."));
    verdict_on(&mut r, &Scope::Inflight, &sp.b, &sp.c, "agent:r", true);
    let tn = tension_label(&one(&r, TensionKind::Semantic).id);
    let founder = "human:founder";
    joined_refusals(&mut |s, by| {
        r.dismiss_tension_as(s, founder, &tn, "they agree", &cli(by, "dismiss it"))
            .map(|d| d.relayed_by)
    });
    let d = r
        .dismiss_tension_as(
            &two_r7,
            founder,
            &tn,
            "they agree",
            &cli("agent:relay/chat", "dismiss it"),
        )
        .unwrap();
    assert_eq!(d.relayed_by, Some(pair("agent:relay/chat", Some("R7"))));
    assert_eq!(
        (d.status.as_str(), d.answer.is_some()),
        ("dismissed", true),
        "{d:?}"
    );
}

#[test]
fn id4_text_and_semantic_json_keep_actor_and_run_apart() {
    let s = served_views();
    // the own line
    for (extra, want) in [
        (
            &["agent:a/b", "--run", "R7"][..],
            "view a owner agent:a/b (run R7)",
        ),
        (&["agent:a/b"][..], "view a owner agent:a/b"),
        (&["--none"][..], "view a owner none"),
    ] {
        let mut a = strings(&["view", "own", "a"]);
        a.extend(strings(extra));
        assert_eq!(text_of(&s.rdy(&args(&a))), want);
    }
    // relayed, with and without the session's run
    for (run, want) in [
        (
            Some("R7"),
            "view a owner agent:z, relayed by agent:relay/chat (run R7)",
        ),
        (None, "view a owner agent:z, relayed by agent:relay/chat"),
    ] {
        let o = s.rdy_as(
            "agent:relay/chat",
            run,
            &[
                "view",
                "own",
                "a",
                "agent:z",
                "--relayed-by",
                "agent:relay/chat",
                "--evidence",
                "Give a to z",
            ],
        );
        assert_eq!(text_of(&o), want);
    }
    // the owned-view refusal
    let o = s.rdy(&["view", "own", "a", "agent:a/b", "--run", "R7"]);
    text_of(&o);
    let (ok, v) = s.json_as("agent:a/b", None, &["--json", "view", "close", "a"]);
    assert!(!ok);
    assert_eq!(
        v["message"], "view \"a\" is owned by agent:a/b (run R7)",
        "{v}"
    );
    // the history: the proposer and the relayer of a landing, apart
    let (ok, v) = s.json_as(
        "agent:a/b",
        Some("R7"),
        &[
            "--json",
            "propose",
            "--view",
            "a",
            "--title",
            "t",
            "--summary",
            "A claim.",
        ],
    );
    assert!(ok, "{v}");
    let pref = v["ref"].as_str().expect("ref").to_string();
    let (ok, v) = s.json_as(
        "agent:relay/chat",
        Some("R7"),
        &[
            "--json",
            "approve",
            &pref,
            "--relayed-by",
            "agent:relay/chat",
            "--evidence",
            "yes, land it",
        ],
    );
    assert!(ok, "{v}");
    let log = text_of(&s.rdy(&["log"]));
    let top = log.lines().next().unwrap();
    assert!(top.contains("proposed by agent:a/b (run R7)"), "{log}");
    assert!(
        top.contains("relayed by agent:relay/chat (run R7)"),
        "{log}"
    );
    s.stop();

    // the undo refusal, with and without a run
    let (_d, mut r, _section) = views_project();
    for (o, want) in [
        (
            owner("agent:a/b", Some("R7")),
            "no undoable op by agent:a/b (run R7) on view \"a\"",
        ),
        (
            owner("agent:a/b", None),
            "no undoable op by agent:a/b on view \"a\"",
        ),
    ] {
        r.view_own(&human_writer(), "a", Some(&o)).unwrap();
        let me = agent(&o.actor, o.run.as_deref());
        let e = r.undo(&me, "a", None, false).unwrap_err();
        refused(
            &e,
            E_NOTHING_TO_UNDO,
            want,
            "name an op: rdy undo --view a <op> (rdy view show lists them)",
        );
    }
    // a stored identity from before the grammar prints as it was recorded
    assert_eq!(
        rdy_engine::identity::actor_run_text("agent:Old Name", Some("run/1")),
        "agent:Old Name (run run/1)"
    );
    assert_eq!(
        rdy_engine::identity::actor_run_text("agent:a/b", None),
        "agent:a/b"
    );

    // the attestation of a relayed answer to a question
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let sp = spec_doc(&mut r);
    let w = write(
        &mut r,
        "qs",
        json!([{"op": "create", "type": "question", "id": "$q", "parent": sp.section.to_string(),
        "fields": {
            "prompt": "Who approves refunds?",
            "title": "Who approves refunds?",
            "why": "Support waits on this",
            "options": ["Finance", "Support lead", "Either"],
            "recommended": "Finance",
            "recommend_reason": "Finance owns the ledger"
        }}]),
    )
    .unwrap();
    land(&mut r, "qs").unwrap();
    let q = created(&w, "$q");
    let v = version_of(&r, &Scope::Canon, &q.to_string());
    let item = format!("{q}@{}", &v[..12]);
    let (_one, two, two_r7) = relayers();
    for (s, want) in [
        (
            &two_r7,
            "human:qa (relayed by agent:relay/chat (run R7): \"Finance, please\")",
        ),
        (
            &two,
            "human:qa (relayed by agent:relay/chat: \"Finance, please\")",
        ),
    ] {
        let h = pending(&r)
            .items
            .iter()
            .find(|i| i.r#ref == item)
            .expect("the question")
            .card_hash
            .clone();
        let info = r
            .answer(
                s,
                PERSON,
                AnswerRequest {
                    r#ref: item.clone(),
                    option: "o1".into(),
                    card_hash: Some(h),
                    note: None,
                    relay: cli("agent:relay/chat", "Finance, please"),
                    brief: None,
                },
            )
            .unwrap();
        let (_, a) = r.store().reader().answer_of(&item).unwrap().unwrap();
        assert_eq!(info.relayed_by, a.relayed_by);
        let c = r.store().reader().change(&a.targets[0]).unwrap();
        let id = c.ops[0].block_id().clone();
        let d = r.read().block(&Scope::View(s.default_view()), &id).unwrap();
        assert_eq!(
            d.fields.get("attested_by"),
            Some(&FieldValue::Str(want.into()))
        );
    }

    // the semantic acceptance as JSON: the relayer is a pair; no run is null; no relay omits it
    let (_d, mut r) = project();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", upd(&sp.c, "C contradicts B."));
    verdict_on(&mut r, &Scope::Inflight, &sp.b, &sp.c, "agent:r", true);
    let p = r.propose(&ag("a"), "a", "a", Some("C changes.")).unwrap();
    r.accept_advice(
        &two,
        PERSON,
        AcceptRequest {
            proposal: p.r#ref.clone(),
            advised_digest: p.advised_digest.clone().unwrap_or_default(),
            evidence: Some("yes".into()),
            relayed_by: Some("agent:relay/chat".into()),
            ..AcceptRequest::default()
        },
    )
    .unwrap();
    let t = one(&r, TensionKind::Semantic);
    let acc = r.read().tension_info(&t).judged.unwrap().accepted.unwrap();
    let v = serde_json::to_value(&acc).unwrap();
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay/chat", "run": null}),
        "{v}"
    );
    let (_d, mut r) = project();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", upd(&sp.c, "C contradicts B."));
    verdict_on(&mut r, &Scope::Inflight, &sp.b, &sp.c, "agent:r", true);
    let p = r.propose(&ag("a"), "a", "a", Some("C changes.")).unwrap();
    r.accept_advice(
        &two_r7,
        PERSON,
        AcceptRequest {
            proposal: p.r#ref.clone(),
            advised_digest: p.advised_digest.clone().unwrap_or_default(),
            evidence: Some("yes".into()),
            relayed_by: Some("agent:relay/chat".into()),
            ..AcceptRequest::default()
        },
    )
    .unwrap();
    let t = one(&r, TensionKind::Semantic);
    let acc = r.read().tension_info(&t).judged.unwrap().accepted.unwrap();
    let v = serde_json::to_value(&acc).unwrap();
    assert_eq!(
        v["relayed_by"],
        json!({"actor": "agent:relay/chat", "run": "R7"}),
        "{v}"
    );
    // a person's own acceptance names no relayer
    let (_d, mut r) = project();
    let sp = spec_doc(&mut r);
    put(&mut r, "a", upd(&sp.c, "C contradicts B."));
    verdict_on(&mut r, &Scope::Inflight, &sp.b, &sp.c, "agent:r", true);
    let p = r.propose(&ag("a"), "a", "a", Some("C changes.")).unwrap();
    let pat = Session::human("human:pat".into(), Role::Writer).unwrap();
    r.accept_advice(
        &pat,
        PERSON,
        AcceptRequest {
            proposal: p.r#ref.clone(),
            advised_digest: p.advised_digest.clone().unwrap_or_default(),
            ..AcceptRequest::default()
        },
    )
    .unwrap();
    let t = one(&r, TensionKind::Semantic);
    let acc = r.read().tension_info(&t).judged.unwrap().accepted.unwrap();
    let v = serde_json::to_value(&acc).unwrap();
    assert!(v.get("relayed_by").is_none(), "{v}");
}

#[test]
fn id4_refused_owner_or_relay_has_no_partial_answer() {
    let (_d, mut r, _section) = views_project();
    let end = r.store().log_end();
    let (one, _two, two_r7) = relayers();
    let words = |by: &str| cli(by, "give a to b");
    // a relay refusal, a view refusal and an owner refusal: no view op, no answer
    let cases: Vec<(Session, RelayInput, &str, OwnerInput)> = vec![
        (
            one.clone(),
            words("agent:relay/chat"),
            "a",
            owner("agent:b", None),
        ),
        (
            two_r7.clone(),
            words("agent:relay/chat/R7"),
            "a",
            owner("agent:b", None),
        ),
        (
            two_r7.clone(),
            words("agent:relay/chat"),
            "nope",
            owner("agent:b", None),
        ),
        (
            two_r7.clone(),
            words("agent:relay/chat"),
            "a",
            owner("agent:b/c/d", None),
        ),
        (
            two_r7.clone(),
            words("agent:relay/chat"),
            "a",
            owner("agent:b", Some("none")),
        ),
        (
            two_r7.clone(),
            words("agent:relay/chat"),
            "a",
            owner("agent:b", Some("")),
        ),
    ];
    for (s, relay, view, o) in &cases {
        r.view_own_as(s, PERSON, relay, view, Some(o)).unwrap_err();
    }
    // a person's own session names no relay
    let e = r
        .view_own_as(
            &writer(),
            PERSON,
            &words("agent:relay/chat"),
            "a",
            Some(&owner("agent:b", None)),
        )
        .unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    assert_eq!(r.store().log_end(), end, "nothing was written");
    assert!(r.store().reader().answer_of("own:a").unwrap().is_none());
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:a".into(), Some("r1".into())))
    );
    // a successful relayed hand-over: the view op and the answer in one sealed batch
    let info = r
        .view_own_as(
            &two_r7,
            PERSON,
            &words("agent:relay/chat"),
            "a",
            Some(&owner("agent:a/b", Some("R7"))),
        )
        .unwrap();
    let batch: Vec<u8> = r.store().last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        (
            batch.iter().filter(|k| **k == kinds::VIEW_OP).count(),
            batch.iter().filter(|k| **k == kinds::ANSWER).count()
        ),
        (1, 1),
        "{batch:?}"
    );
    let (h, a) = r.store().reader().answer_of("own:a").unwrap().unwrap();
    assert_eq!(info.answer, Some(h.to_string()));
    assert_eq!(a.relayed_by, Some(pair("agent:relay/chat", Some("R7"))));
    assert_eq!(
        owner_of(&r, "a"),
        Some(("agent:a/b".into(), Some("R7".into())))
    );
    r.fsck().expect("fsck");
}
