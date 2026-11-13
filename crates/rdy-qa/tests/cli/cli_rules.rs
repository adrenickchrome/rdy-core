//! rtc6 (FW-MH.rules, D-ZSRBN): `rdy rule add|edit|remove|answer`, the command line twin of
//! `rdy_rule`, on the real `rdy`. It gives the same replies as the tool, takes an agent's relay
//! (`--relayed-by` with `--evidence`), and has no Later. A person's rule goes to the meta project
//! from any project or none; a project's rule stays in its project. A bad form is refused before
//! a daemon starts or a store opens. Every person lives in an isolated home.
use rdy_qa::daemon::short_tempdir;
use rdy_qa::fixtures::output_within;
use rdy_qa::instructions::World;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Output;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(120);

/// What one `rdy` run said.
struct Run {
    code: i32,
    /// The JSON of stdout on success, else of stderr; null when the run printed none.
    json: Value,
    stdout: String,
    stderr: String,
}

/// The identity of an agent session: its actor and run.
type Who = Option<(&'static str, &'static str)>;
const AGENT: Who = Some(("agent:c6", "r1"));

fn parse(out: &Output) -> Run {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let raw = if out.status.success() {
        &stdout
    } else {
        &stderr
    };
    let json = serde_json::from_str(raw.trim())
        .or_else(|_| serde_json::from_str(raw.trim().lines().last().unwrap_or("")))
        .unwrap_or(Value::Null);
    Run {
        code: out.status.code().unwrap_or(-1),
        json,
        stdout,
        stderr,
    }
}

/// `rdy <args>` of `w`, in `cwd`, as `who` (a human when none). `--no-daemon` and `--project` are
/// among the args when a test wants them.
fn rdy(w: &World, cwd: &Path, who: Who, args: &[&str]) -> Run {
    let mut cmd = rdy_qa::fixtures::rdy_command();
    cmd.args(args)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(w.env())
        .current_dir(cwd);
    for name in w.removed_env() {
        cmd.env_remove(name);
    }
    if let Some((actor, run)) = who {
        cmd.env("RDY_ACTOR", actor).env("RDY_RUN", run);
    }
    parse(&output_within(&mut cmd, BUDGET).expect("rdy ran"))
}

/// The words every change carries.
fn words<'a>(by: &'a str, session: &'a str) -> Vec<&'a str> {
    vec![
        "--why",
        "The person asked for it.",
        "--evidence",
        "Please keep this rule.",
        "--relayed-by",
        by,
        "--question",
        "Shall I save this rule?",
        "--session",
        session,
    ]
}

/// `rdy --json --no-daemon rule add ...` in `dir`, as the agent.
fn add(w: &World, dir: &Path, scope: &str, text: &str, session: &str, extra: &[&str]) -> Run {
    let mut args = vec![
        "--json",
        "--no-daemon",
        "rule",
        "add",
        "--scope",
        scope,
        "--text",
        text,
    ];
    args.extend(words("agent:c6", session));
    args.extend_from_slice(extra);
    rdy(w, dir, AGENT, &args)
}

fn ok(r: &Run) -> &Value {
    assert_eq!(r.code, 0, "{} / {}", r.stderr, r.stdout);
    &r.json
}

fn refused<'a>(r: &'a Run, code: &str) -> &'a Value {
    assert_eq!(r.code, 1, "{} / {}", r.stderr, r.stdout);
    assert_eq!(r.json["code"], code, "{}", r.stderr);
    &r.json
}

fn texts(v: &Value, layer: &str) -> Vec<String> {
    v["instructions"]
        .as_array()
        .unwrap_or_else(|| panic!("instructions in {v}"))
        .iter()
        .filter(|r| r["layer"] == layer)
        .map(|r| r["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Ids, hashes and names differ between worlds: the reply with them masked.
fn masked(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(project) = v.get_mut("project").and_then(Value::as_object_mut) {
        project.remove("name");
    }
    let text = v.to_string();
    let re = regex::Regex::new(r"[0-9a-f]{8,}|\b[A-Za-z]{1,3}-[0-9A-Z]{5}\b|\d{10,}").unwrap();
    let text = re.replace_all(&text, "<x>");
    let text = regex::Regex::new(r"/[^\s\x22]*rdyd[0-9a-z]+")
        .unwrap()
        .replace_all(&text, "<dir>");
    serde_json::from_str(&text).unwrap()
}

/// Lands in the closed store at `dir` a doc with the authority policy `fields`, as the rtc5
/// contract probes do.
fn land_policy(dir: &Path, fields: Value) {
    use rdy_engine::WriteRequest;
    use rdy_model::{BlockId, Justification};
    use rdy_qa::fixtures::{land, parse_test_ops, write, writer};
    let mut r = rdy_engine::Rdy::open(dir).expect("open the project");
    let w = write(
        &mut r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    )
    .expect("write the rules");
    r.write(
        &writer(),
        WriteRequest {
            view: "pol".into(),
            ops: parse_test_ops(json!([
                {"op": "create", "type": "authority_policy", "id": "$p", "parent": w.created["$d"], "fields": fields}
            ]))
            .expect("the policy ops"),
            justification: Some(Justification::Implements(vec![
                BlockId::parse(&w.created["$dec"]).expect("a decision id"),
            ])),
            message: String::new(),
        },
    )
    .expect("write the policy");
    land(&mut r, "pol").expect("land the policy");
}

fn card_policy() -> Value {
    json!({"key": "authority-policy", "agent_may_accept": [], "roles": {}, "require": [],
        "owners": [], "rules_from_chat": "card"})
}

/// The project's name as the replies print it: the folder's.
fn name_of(v: &Value) -> String {
    v["project"]["name"]
        .as_str()
        .expect("a project name")
        .into()
}

#[test]
fn rtc6_cli_add_edit_remove_match_mcp_results() {
    let w = World::new();
    let meta = w.project();
    w.person(meta.path());
    let (a, b) = (w.project(), w.project());

    // a project rule: add, edit, remove, and the exact lines of the text output
    let added = add(
        &w,
        a.path(),
        "project",
        "Say what you changed.",
        "chat-1",
        &[],
    );
    let v = ok(&added).clone();
    assert_eq!(v["scope"], "project", "{v}");
    assert_eq!(v["rule"]["text"], "Say what you changed.", "{v}");
    assert_eq!(v["rule"]["retired"], false, "{v}");
    assert!(v["landing"]["landing"].is_string(), "{v}");
    let id = v["rule"]["id"].as_str().expect("an id").to_string();
    let project = name_of(&v);
    let ruleset = v["rule"]["ruleset"]
        .as_str()
        .expect("a ruleset")
        .to_string();
    // the same words again, in the text form, in a new session
    let mut args = vec![
        "--no-daemon",
        "rule",
        "add",
        "--scope",
        "project",
        "--text",
        "Keep to the point.",
    ];
    args.extend(words("agent:c6", "chat-2"));
    let t = rdy(&w, a.path(), AGENT, &args);
    assert_eq!(t.code, 0, "{}", t.stderr);
    assert_eq!(
        t.stdout.trim(),
        format!("Saved to project {project}: \"Keep to the point.\"")
    );
    // a named ruleset (its handle or its id) and selectors
    let handle = format!("project:{ruleset}");
    let named = add(
        &w,
        a.path(),
        "project",
        "Name the test first.",
        "chat-3",
        &["--ruleset", &handle, "--applies-to", "all"],
    );
    let named = ok(&named);
    assert_eq!(named["rule"]["ruleset"], json!(ruleset), "{named}");
    // edit keeps the id; the text of the edit
    let mut args = vec![
        "--json",
        "--no-daemon",
        "rule",
        "edit",
        &id,
        "--scope",
        "project",
        "--text",
        "Say what you changed and why.",
    ];
    args.extend(words("agent:c6", "chat-4"));
    let edited = rdy(&w, a.path(), AGENT, &args);
    let e = ok(&edited);
    assert_eq!(e["rule"]["id"], json!(id), "{e}");
    assert_eq!(e["rule"]["text"], "Say what you changed and why.", "{e}");
    // remove: the retired rule, the text line
    let mut args = vec!["--no-daemon", "rule", "remove", &id, "--scope", "project"];
    args.extend(words("agent:c6", "chat-5"));
    let r = rdy(&w, a.path(), AGENT, &args);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        r.stdout.trim(),
        format!("Removed rule {id} from project {project}.")
    );
    // an unknown rule: the exact refusal
    let mut args = vec![
        "--json",
        "--no-daemon",
        "rule",
        "remove",
        "ai-00000",
        "--scope",
        "project",
    ];
    args.extend(words("agent:c6", "chat-6"));
    let r = rdy(&w, a.path(), AGENT, &args);
    let e = refused(&r, "E_NOT_FOUND");
    assert_eq!(e["message"], "rule target ai-00000 was not found", "{e}");
    assert_eq!(
        e["fix"], "read the selected project and use an existing instruction or ruleset",
        "{e}"
    );

    // a person's rule from project A reaches project B, and the text names the own rules
    let mut args = vec![
        "--no-daemon",
        "rule",
        "add",
        "--scope",
        "person",
        "--text",
        "Answer in British English.",
    ];
    args.extend(words("agent:c6", "chat-7"));
    let t = rdy(&w, a.path(), AGENT, &args);
    assert_eq!(t.code, 0, "{}", t.stderr);
    assert_eq!(
        t.stdout.trim(),
        "Saved to your own rules: \"Answer in British English.\""
    );
    let seen = rdy(
        &w,
        b.path(),
        None,
        &["--json", "--no-daemon", "instructions"],
    );
    assert!(
        texts(ok(&seen), "person").contains(&"Answer in British English.".to_string()),
        "{}",
        seen.stdout
    );
    assert!(texts(ok(&seen), "project").is_empty(), "{}", seen.stdout);

    // the CLI and the tool give one result: the same request, masked, in two worlds
    let mcp_world = World::new();
    let dir = mcp_world.project();
    let mut c = mcp_world.mcp(dir.path());
    assert!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:c6", "run": "r1", "role": "writer"})
        )
        .is_ok()
    );
    let via_mcp = match c.call(
        "rdy_rule",
        json!({"scope": "project", "op": "add", "text": "Say what you changed.",
            "why": "The person asked for it.", "evidence": "Please keep this rule.",
            "question": "Shall I save this rule?", "session_ref": "chat-1"}),
    ) {
        rdy_qa::mcp::Reply::Ok(v) => v,
        other => panic!("{other:?}"),
    };
    let w2 = World::new();
    let m2 = w2.project();
    w2.person(m2.path());
    let d2 = w2.project();
    let via_cli = add(
        &w2,
        d2.path(),
        "project",
        "Say what you changed.",
        "chat-1",
        &[],
    );
    assert_eq!(masked(ok(&via_cli)), masked(&via_mcp));
}

#[test]
fn rtc6_cli_answer_uses_returned_ref_and_hash() {
    let w = World::new();
    let meta = w.project();
    w.person(meta.path());
    let a = w.project();
    land_policy(a.path(), card_policy());

    // the card policy stops at the card; the text form prints the card as it is
    let card = add(
        &w,
        a.path(),
        "project",
        "Keep one idea per change.",
        "chat-1",
        &[],
    );
    let card = ok(&card).clone();
    assert_eq!(card["state"], "card", "{card}");
    let (item, hash) = (
        card["ref"].as_str().expect("a ref").to_string(),
        card["card_hash"].as_str().expect("a hash").to_string(),
    );
    let mut args = vec![
        "--no-daemon",
        "rule",
        "add",
        "--scope",
        "project",
        "--text",
        "Keep one idea per change.",
    ];
    args.extend(words("agent:c6", "chat-1"));
    let again = rdy(&w, a.path(), AGENT, &args);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(
        again.stdout.trim(),
        card["card"]["card"].as_str().expect("a card text").trim(),
        "the card, unchanged"
    );

    // a wrong hash is the normal moved-card refusal, and nothing landed
    let answer = |item: &str, option: &str, hash: &str| {
        rdy(
            &w,
            a.path(),
            AGENT,
            &[
                "--json",
                "--no-daemon",
                "rule",
                "answer",
                item,
                option,
                "--scope",
                "project",
                "--card",
                hash,
                "--evidence",
                "Yes, keep it.",
                "--relayed-by",
                "agent:c6",
                "--question",
                "Shall I land this rule?",
            ],
        )
    };
    let wrong = answer(&item, "approve", &"0".repeat(64));
    assert_eq!(wrong.code, 1, "{} / {}", wrong.stderr, wrong.stdout);
    let code = wrong.json["code"].as_str().unwrap_or_default().to_string();
    assert!(
        ["E_ITEM_MOVED", "E_PROPOSAL_MOVED"].contains(&code.as_str()),
        "{}",
        wrong.stderr
    );
    // the returned ref and hash land it
    let landed = answer(&item, "approve", &hash);
    let landed = ok(&landed).clone();
    assert_eq!(
        landed["rule"]["text"], "Keep one idea per change.",
        "{landed}"
    );
    assert!(landed["landing"]["landing"].is_string(), "{landed}");
    let id = landed["rule"]["id"].as_str().expect("an id").to_string();

    // an edit stops at the card with the confirm card beside it; confirm, then approve
    let mut args = vec![
        "--json",
        "--no-daemon",
        "rule",
        "edit",
        &id,
        "--scope",
        "project",
        "--text",
        "Keep one idea in each change.",
    ];
    args.extend(words("agent:c6", "chat-2"));
    let edit = rdy(&w, a.path(), AGENT, &args);
    let edit = ok(&edit).clone();
    assert_eq!(edit["state"], "card", "{edit}");
    let confirm = edit["cards"][0].clone();
    let (cref, chash) = (
        confirm["ref"].as_str().expect("a ref").to_string(),
        confirm["card_hash"].as_str().expect("a hash").to_string(),
    );
    let step = answer(&cref, "confirm", &chash);
    let step = ok(&step).clone();
    assert_eq!(step["state"], "answered", "{step}");
    let next = step["remaining_cards"][0].clone();
    let t = rdy(
        &w,
        a.path(),
        AGENT,
        &[
            "--no-daemon",
            "rule",
            "answer",
            &cref,
            "confirm",
            "--scope",
            "project",
            "--card",
            &chash,
            "--evidence",
            "Yes.",
            "--relayed-by",
            "agent:c6",
        ],
    );
    // the same answer again is the stored result or the normal refusal; never a second effect
    assert!(t.code == 0 || t.code == 1, "{}", t.stderr);
    let landed = answer(
        next["ref"].as_str().expect("a ref"),
        "approve",
        next["card_hash"].as_str().expect("a hash"),
    );
    assert_eq!(ok(&landed)["rule"]["text"], "Keep one idea in each change.");

    // a rejection in the same destination answers the card and lands nothing
    let rej = add(
        &w,
        a.path(),
        "project",
        "Never mind this one.",
        "chat-3",
        &[],
    );
    let rej = ok(&rej).clone();
    assert_eq!(rej["state"], "card", "{rej}");
    let r = answer(
        rej["ref"].as_str().expect("a ref"),
        "reject",
        rej["card_hash"].as_str().expect("a hash"),
    );
    let r = ok(&r);
    assert_ne!(r["state"], "card", "{r}");
    assert!(r["landing"].is_null(), "{r}");
    let seen = rdy(
        &w,
        a.path(),
        None,
        &["--json", "--no-daemon", "instructions"],
    );
    assert_eq!(
        texts(ok(&seen), "project"),
        ["Keep one idea in each change."],
        "{}",
        seen.stdout
    );
}

#[test]
fn rtc6_cli_projectless_and_direct_modes_keep_ownership() {
    // no project and no meta: the two refusals, and no project is made
    let w = World::new();
    let outside = short_tempdir();
    let r = add(&w, outside.path(), "person", "Be brief.", "c-1", &[]);
    let e = refused(&r, "E_NO_PROJECT");
    assert_eq!(e["message"], "the meta project does not exist", "{e}");
    assert_eq!(e["fix"], "run rdy setup", "{e}");
    let r = add(&w, outside.path(), "project", "Be brief.", "c-2", &[]);
    let e = refused(&r, "E_NO_PROJECT");
    assert_eq!(e["message"], "this session has no project", "{e}");
    assert_eq!(
        e["fix"], "use scope person, or run rdy init in a folder and reconnect",
        "{e}"
    );
    assert!(
        !outside.path().join(".rdy").exists(),
        "a project was made outside"
    );

    // with a meta project: a person rule lands from outside, and a project rule still has no project
    let setup = rdy(&w, outside.path(), None, &["--json", "setup"]);
    assert_eq!(ok(&setup)["meta"]["result"], "created");
    let meta = w.default_meta();
    let v = add(&w, outside.path(), "person", "Be brief.", "c-3", &[]);
    let v = ok(&v).clone();
    assert_eq!(v["scope"], "person", "{v}");
    assert_eq!(v["rule"]["text"], "Be brief.", "{v}");
    let r = add(&w, outside.path(), "project", "Be brief.", "c-4", &[]);
    refused(&r, "E_NO_PROJECT");
    assert!(!outside.path().join(".rdy").exists());

    // inside the meta project itself, both scopes are its own
    let own = add(&w, &meta, "person", "Cite the source.", "c-5", &[]);
    assert_eq!(ok(&own)["scope"], "person");
    let own = add(&w, &meta, "project", "Cite the page.", "c-6", &[]);
    assert_eq!(ok(&own)["scope"], "project");
    let seen = rdy(
        &w,
        outside.path(),
        None,
        &["--json", "--no-daemon", "instructions"],
    );
    let mut got = texts(ok(&seen), "project");
    got.extend(texts(ok(&seen), "person"));
    for want in ["Be brief.", "Cite the source.", "Cite the page."] {
        assert!(got.iter().any(|t| t == want), "{want} in {got:?}");
    }

    // a held meta project: a direct run asks its daemon, and the daemon stays the owner
    let a = w.project();
    let held = rdy(&w, &meta, None, &["--json", "status"]);
    assert_eq!(held.code, 0, "{}", held.stderr);
    let pid = w.daemon_pid(&meta).expect("meta's daemon runs");
    let v = add(&w, a.path(), "person", "Prefer short words.", "c-7", &[]);
    assert_eq!(ok(&v)["scope"], "person");
    assert_eq!(w.daemon_pid(&meta), Some(pid), "the daemon changed");
    assert_eq!(w.daemon_pid(a.path()), None, "project A got a daemon");

    // an explicit project: from outside, project scope writes there and person scope to meta
    let b = w.project();
    let mut args = vec![
        "--json",
        "--no-daemon",
        "--project",
        b.path().to_str().unwrap(),
        "rule",
        "add",
        "--scope",
        "project",
        "--text",
        "Check the log.",
    ];
    args.extend(words("agent:c6", "c-8"));
    let v = rdy(&w, outside.path(), AGENT, &args);
    let v = ok(&v).clone();
    assert_eq!(v["scope"], "project", "{v}");
    let mut args = vec![
        "--json",
        "--no-daemon",
        "--project",
        b.path().to_str().unwrap(),
        "rule",
        "add",
        "--scope",
        "person",
        "--text",
        "Be kind.",
    ];
    args.extend(words("agent:c6", "c-9"));
    let v = rdy(&w, outside.path(), AGENT, &args);
    assert_eq!(ok(&v)["scope"], "person");
    let seen = rdy(
        &w,
        b.path(),
        None,
        &["--json", "--no-daemon", "instructions"],
    );
    assert!(texts(ok(&seen), "project").contains(&"Check the log.".to_string()));
    assert!(texts(ok(&seen), "person").contains(&"Be kind.".to_string()));
    assert!(!outside.path().join(".rdy").exists());
}

#[test]
fn rtc6_cli_bad_forms_never_open_a_writer() {
    let w = World::new();
    let meta = w.project();
    w.person(meta.path());
    let a = w.project();
    let quiet = |w: &World| {
        assert_eq!(w.daemon_pid(a.path()), None, "project A got a daemon");
        assert_eq!(w.daemon_pid(meta.path()), None, "meta got a daemon");
    };
    // daemon mode: if any form started a writer, a daemon would run
    let go = |who: Who, args: &[&str]| rdy(&w, a.path(), who, args);

    // argument conflicts and missing words are the parser's usage refusal
    for args in [
        // remove takes no text
        vec![
            "rule",
            "remove",
            "ai-00000",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
        ],
        // answer takes no why
        vec![
            "rule",
            "answer",
            "P-23456@1",
            "approve",
            "--scope",
            "project",
            "--card",
            "00",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
            "--why",
            "w",
        ],
        // no evidence
        vec![
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "w",
            "--relayed-by",
            "agent:c6",
        ],
        // no relayer
        vec![
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "e",
        ],
        // a scope outside the two
        vec![
            "rule",
            "add",
            "--scope",
            "org",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
        ],
        // no scope: there is no default
        vec![
            "rule",
            "add",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
        ],
    ] {
        let r = go(AGENT, &args);
        assert_eq!(r.code, 2, "{args:?}: {}", r.stderr);
    }
    quiet(&w);

    // the engine's checks, before any connection
    let long = "x".repeat(65_537);
    let e = if cfg!(windows) {
        // a Windows command line holds 32 KiB at most, so this text cannot be passed to `rdy`:
        // the same check runs over MCP, in a world of its own
        let w2 = World::new();
        let dir = w2.project();
        let mut c = w2.mcp(dir.path());
        assert!(
            c.call(
                "rdy_session",
                json!({"actor": "agent:c6", "run": "r1", "role": "writer"})
            )
            .is_ok()
        );
        match c.call(
            "rdy_rule",
            json!({"scope": "project", "op": "add", "text": long, "why": "w",
                "evidence": "e", "question": "q", "session_ref": "chat-1"}),
        ) {
            rdy_qa::mcp::Reply::ToolError(v) => v,
            other => panic!("{other:?}"),
        }
    } else {
        let r = go(
            AGENT,
            &[
                "--json",
                "rule",
                "add",
                "--scope",
                "project",
                "--text",
                &long,
                "--why",
                "w",
                "--evidence",
                "e",
                "--relayed-by",
                "agent:c6",
            ],
        );
        refused(&r, "E_LIMIT").clone()
    };
    assert_eq!(e["code"], "E_LIMIT", "{e}");
    assert_eq!(
        e["message"], "rdy_rule: \"text\" exceeds 65536 bytes",
        "{e}"
    );
    let r = go(
        AGENT,
        &[
            "--json",
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "a\nb",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
        ],
    );
    let e = refused(&r, "E_BAD_INPUT");
    assert_eq!(e["message"], "rdy_rule: \"why\" must be one line", "{e}");
    let r = go(
        AGENT,
        &[
            "--json",
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "",
            "--relayed-by",
            "agent:c6",
        ],
    );
    let e = refused(&r, "E_PERMISSION");
    assert_eq!(
        e["message"], "an agent answers only by relaying a person's words",
        "{e}"
    );
    let r = go(
        AGENT,
        &[
            "--json",
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
            "--applies-to",
        ],
    );
    assert_eq!(r.code, 2, "{}", r.stderr);
    let r = go(
        AGENT,
        &[
            "--json",
            "rule",
            "answer",
            "P-23456@1",
            "approve",
            "--scope",
            "project",
            "--card",
            "abc",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
        ],
    );
    let e = refused(&r, "E_BAD_INPUT");
    assert_eq!(
        e["message"], "rdy_rule: \"answer.card_hash\" must be a full lowercase hash",
        "{e}"
    );
    // the CLI has no Later
    let r = go(
        AGENT,
        &[
            "--json",
            "rule",
            "answer",
            "P-23456@1",
            "later",
            "--scope",
            "project",
            "--card",
            &"0".repeat(64),
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
        ],
    );
    let e = refused(&r, "E_BAD_INPUT");
    assert_eq!(e["message"], "the CLI has no Later", "{e}");
    // actor pairing: the relayer is the session's own actor; a person relays nothing
    let r = go(
        AGENT,
        &[
            "--json",
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:other",
        ],
    );
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert!(r.json["code"].is_string(), "{}", r.stderr);
    let r = go(
        None,
        &[
            "--json",
            "rule",
            "add",
            "--scope",
            "project",
            "--text",
            "x",
            "--why",
            "w",
            "--evidence",
            "e",
            "--relayed-by",
            "agent:c6",
        ],
    );
    refused(&r, "E_PERMISSION");
    quiet(&w);
}

#[test]
fn rtc6_cli_contract_lists_rule_and_every_flag() {
    let w = World::new();
    let dir = w.project();
    // the current fixture's tree, which rdy-cli compares with its clap tree
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts/readme-from-rdy-v1.json");
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture")).unwrap();
    let tree = &fixture["cli"]["tree"];
    let top: Vec<&str> = tree["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(top.contains(&"rule"), "{top:?}");
    let rule = tree["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "rule")
        .unwrap();
    assert_eq!(rule["subcommand_required"], true);
    let subs: Vec<&str> = rule["subcommands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(subs, ["add", "edit", "remove", "answer"]);
    let flags = |name: &str| -> Vec<String> {
        let cmd = rule["subcommands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap();
        assert!(cmd["hidden"].is_null(), "{name} is hidden");
        // the global --json, --project and --no-daemon stay with every form
        for g in ["json", "project", "no_daemon"] {
            assert!(
                cmd["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|a| a["id"] == g && a["global"] == true),
                "{name} lacks the global {g}"
            );
        }
        cmd["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["id"] != "help" && a["global"].is_null())
            .map(|a| {
                assert!(a["hidden"].is_null(), "{name}: hidden {a}");
                a["long"]
                    .as_str()
                    .map(|l| format!("--{l}"))
                    .unwrap_or_else(|| format!("<{}>", a["id"].as_str().unwrap()))
            })
            .collect()
    };
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let want = |items: &[&str]| sorted(items.iter().map(|s| s.to_string()).collect());
    assert_eq!(
        sorted(flags("add")),
        want(&[
            "--scope",
            "--text",
            "--ruleset",
            "--applies-to",
            "--why",
            "--evidence",
            "--relayed-by",
            "--question",
            "--session"
        ])
    );
    assert_eq!(
        sorted(flags("edit")),
        want(&[
            "<id>",
            "--scope",
            "--text",
            "--ruleset",
            "--applies-to",
            "--why",
            "--evidence",
            "--relayed-by",
            "--question",
            "--session"
        ])
    );
    assert_eq!(
        sorted(flags("remove")),
        want(&[
            "<id>",
            "--scope",
            "--why",
            "--evidence",
            "--relayed-by",
            "--question",
            "--session"
        ])
    );
    assert_eq!(
        sorted(flags("answer")),
        want(&[
            "<ref>",
            "<option>",
            "--scope",
            "--card",
            "--evidence",
            "--relayed-by",
            "--question"
        ])
    );
    // no command other than rule changes a rule: no top-level name and no flag says so
    for name in top {
        assert!(
            !name.contains("rule") || name == "rule",
            "an alternative rule command: {name}"
        );
    }
    // the real binary lists the same four and takes every flag of the contract
    let h = rdy(&w, dir.path(), None, &["rule", "--help"]);
    assert_eq!(h.code, 0, "{}", h.stderr);
    for sub in ["add", "edit", "remove", "answer"] {
        assert!(h.stdout.contains(sub), "{sub} in {}", h.stdout);
    }
    for (sub, flag) in [
        ("add", "--applies-to"),
        ("add", "--ruleset"),
        ("add", "--session"),
        ("edit", "--relayed-by"),
        ("remove", "--evidence"),
        ("answer", "--card"),
    ] {
        let h = rdy(&w, dir.path(), None, &["rule", sub, "--help"]);
        assert_eq!(h.code, 0, "{}", h.stderr);
        assert!(h.stdout.contains(flag), "{sub} {flag} in {}", h.stdout);
    }
    // the answer form has no Later and no session flag
    let h = rdy(&w, dir.path(), None, &["rule", "answer", "--help"]);
    assert!(!h.stdout.contains("--session"), "{}", h.stdout);
}
