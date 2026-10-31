//! ck9 (D-JQD5X, D-WMCFW; n-XFJVW, n-R8PC6; O15): the command line of the authoring checks, and
//! the complete approval loop of a person. `rdy review checks | run | request | list` is the
//! twin of `rdy_review`; `rdy pending` and `rdy answer` carry a verdict's card and its acceptance
//! as `rdy_pending` and `rdy_answer` do; source existence and rule fixes print in the briefing,
//! the source text and the propose report. The CLI builds no card of its own: the engine's card,
//! with its check facts and acceptance options, is the text.
use rdy_engine::Rdy;
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::{land, parse_test_ops, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::mcp::McpClient;
use rdy_qa::packs::QA_PACK_YAML;
use rdy_qa::tensions::{ag, created, j_typo, put_j, try_decide_in, upd};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

/// The reason of every concern and fail these tests write.
const REASON: &str = "it reads otherwise";
/// The citation note of every concern and fail these tests write.
const NOTE: &str = "the meaning moved";
/// The fix of an accepted verdict's issue.
const HELD_FIX: &str = "change the cited blocks, then propose again: rdy_write / rdy edit";

// ---------------------------------------------------------------- shared helpers

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

/// `rdy <args>` as agent `actor` (run `run`); the person when `agent` is `None`.
fn rdy(s: &Served, agent: Option<(&str, &str)>, args: &[&str]) -> Output {
    match agent {
        Some((actor, run)) => s.rdy_as(actor, Some(run), args),
        None => s.rdy(args),
    }
}

/// The JSON of a command that must succeed.
fn cli_json(s: &Served, agent: Option<(&str, &str)>, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = rdy(s, agent, &all);
    assert!(
        out.status.success(),
        "rdy {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// The error JSON of a command that must be refused.
fn cli_refused(s: &Served, agent: Option<(&str, &str)>, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = rdy(s, agent, &all);
    assert!(
        !out.status.success(),
        "rdy {args:?} should be refused: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stderr);
    serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// The text a command prints on success, without its final newline.
fn cli_text(s: &Served, agent: Option<(&str, &str)>, args: &[&str]) -> String {
    let out = rdy(s, agent, args);
    assert!(
        out.status.success(),
        "rdy {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .strip_suffix('\n')
        .unwrap_or_default()
        .to_string()
}

/// `v` with each request id replaced: a request's id carries the time it was written, so two
/// requests for one case differ in it and in nothing else.
fn norm(v: &Value) -> Value {
    match v {
        Value::String(s) if s.starts_with("rq-") => json!("rq-<id>"),
        Value::Array(a) => Value::Array(a.iter().map(norm).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), norm(v))).collect()),
        other => other.clone(),
    }
}

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

/// A served project: the spec doc in canon and agent:a's typo edit of claim C, proposed with a
/// summary. Its version, and the id of C.
struct Versioned {
    s: Served,
    pref: String,
    version: String,
    c: BlockId,
}

fn versioned() -> Versioned {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    put_j(&mut r, "a", upd(&spec.c, "C stands alone!"), j_typo());
    let p = r
        .propose(&ag("a"), "a", "a", Some(TEST_SUMMARY))
        .expect("propose");
    assert!(p.ready, "{:#?}", p.issues);
    r.close().unwrap();
    Versioned {
        s,
        pref: p.r#ref,
        version: p.id,
        c: spec.c,
    }
}

/// An independent `meaning_preservation` request on `pref` from agent:a (run a), claimed and
/// answered `verdict` by agent:r (a concern or a fail cites `cited`) through the MCP tools. The
/// request and the review.
fn review_of(
    a: &mut McpClient,
    rv: &mut McpClient,
    pref: &str,
    verdict: &str,
    cited: &BlockId,
) -> (String, String) {
    let asked = a
        .call(
            "rdy_review",
            json!({"op": "request", "check": "meaning_preservation", "subject": pref}),
        )
        .ok();
    let next = rv
        .call("rdy_review_next", json!({"kind": "meaning_preservation"}))
        .ok();
    assert_eq!(next["request"], asked["request"], "{next}");
    let mut args = json!({"request": next["request"], "case_hash": next["case_hash"],
        "verdict": verdict});
    if verdict != "pass" {
        args["citations"] = json!([{"block": cited.to_string(), "note": NOTE}]);
        args["reason"] = json!(REASON);
    }
    let done = rv.call("rdy_review_submit", args).ok();
    (
        asked["request"].as_str().unwrap().to_string(),
        done["review"].as_str().unwrap().to_string(),
    )
}

/// `text` with each run of spaces as one: a card lines its facts up in columns.
fn squeezed(text: &str) -> String {
    text.split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The approve item of `pref` in a `pending` reply.
fn approve_item(pending: &Value, pref: &str) -> Value {
    pending["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["ref"] == pref && i["kind"] == "approve")
        .cloned()
        .unwrap_or_else(|| panic!("an approve item for {pref}: {pending}"))
}

fn fact_lines(item: &Value) -> Vec<String> {
    item["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            format!(
                "{}: {}",
                f["name"].as_str().unwrap(),
                f["value"].as_str().unwrap()
            )
        })
        .collect()
}

fn option_ids(item: &Value) -> Vec<String> {
    item["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect()
}

/// The `issues` of a refusal, as `message` strings.
fn issue_messages(refusal: &Value) -> Vec<String> {
    refusal["issues"]
        .as_array()
        .unwrap_or_else(|| panic!("a refusal with issues: {refusal}"))
        .iter()
        .map(|i| i["message"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------- the command line

/// O15, D-JQD5X: `checks`, `run`, `request` and `list` print what the MCP tool replies with
/// (`--json` is the engine's reply), the text lines follow the closed grammar, and the relay
/// flags and the third positional value are clap's refusals.
#[test]
fn ck9_cli_checks_match_mcp() {
    let w = world();
    let (p, c) = (w.p.clone(), w.spec.c.to_string());
    let agent = Some(("agent:a", "a"));
    let mut a = client(&w.s, "agent:a", "a", "writer");
    let mut rv = client(&w.s, "agent:r", "r", "reviewer");

    // the catalogue: the same reply, and one line for each check
    let cli = cli_json(&w.s, agent, &["review", "checks"]);
    let mcp = a.call("rdy_review", json!({"op": "checks"})).ok();
    assert_eq!(cli, mcp);
    let lines: Vec<String> = mcp["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            format!(
                "{}  {}",
                c["name"].as_str().unwrap(),
                c["subject"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(lines.len(), 8);
    assert_eq!(
        cli_text(&w.s, agent, &["review", "checks"]),
        lines.join("\n")
    );

    // a run: the same reply but for the request's id; the text names the request, the case
    // hash and the case
    let cli = cli_json(&w.s, agent, &["review", "run", "decision_review", &p]);
    let mcp = a
        .call(
            "rdy_review",
            json!({"op": "run", "check": "decision_review", "subject": p}),
        )
        .ok();
    assert_eq!(norm(&cli), norm(&mcp));
    assert_eq!(cli["case_hash"], mcp["case_hash"]);
    let text = cli_text(&w.s, agent, &["review", "run", "decision_review", &p]);
    let mut lines = text.lines();
    let first = lines.next().unwrap();
    assert!(
        first.starts_with("requested rq-") && first.ends_with(" (decision_review, author-run)"),
        "{text}"
    );
    assert_eq!(
        lines.next().unwrap(),
        format!("case {}", mcp["case_hash"].as_str().unwrap())
    );
    assert_eq!(
        lines.collect::<Vec<_>>().join("\n"),
        serde_json::to_string_pretty(&mcp["case"]).unwrap()
    );

    // a plain independent request, with a reason
    let cli = cli_json(
        &w.s,
        agent,
        &[
            "review",
            "request",
            "decision_review",
            &p,
            "--reason",
            "Check the basis.",
        ],
    );
    let mcp = a
        .call(
            "rdy_review",
            json!({"op": "request", "check": "decision_review", "subject": p,
                   "reason": "Check the basis."}),
        )
        .ok();
    assert_eq!(norm(&cli), norm(&mcp));
    let text = cli_text(&w.s, agent, &["review", "request", "decision_review", &p]);
    assert!(
        text.starts_with("requested rq-")
            && text
                .lines()
                .next()
                .unwrap()
                .ends_with(" (decision_review, independent)"),
        "{text}"
    );

    // a person's request relayed by the agent: the flags are the tool's evidence, question and
    // session_ref; the run comes from the session
    let cli = cli_json(
        &w.s,
        agent,
        &[
            "review",
            "request",
            "decision_review",
            &p,
            "--reason",
            "Check the basis.",
            "--relayed-by",
            "agent:a",
            "--evidence",
            "Yes, ask for a review.",
            "--question",
            "Shall I ask?",
            "--session",
            "chat-9",
        ],
    );
    let mcp = a
        .call(
            "rdy_review",
            json!({"op": "request", "check": "decision_review", "subject": p,
                   "reason": "Check the basis.", "evidence": "Yes, ask for a review.",
                   "question": "Shall I ask?", "session_ref": "chat-9"}),
        )
        .ok();
    assert_eq!(norm(&cli), norm(&mcp));

    // two block ids are one pair: a contradiction case of c and the decision
    let pair = cli_json(&w.s, agent, &["review", "run", "contradiction", &c, &p]);
    let by_tool = a
        .call(
            "rdy_review",
            json!({"op": "run", "check": "contradiction", "subject": [c, p]}),
        )
        .ok();
    assert_eq!(norm(&pair), norm(&by_tool));

    // the list: the same rows, for every filter
    for (cli_args, mcp_args) in [
        (vec!["review", "list"], json!({"op": "list"})),
        (
            vec!["review", "list", "--open"],
            json!({"op": "list", "open": true}),
        ),
        (
            vec!["review", "list", "--subject", &p],
            json!({"op": "list", "subject": p}),
        ),
        (
            vec!["review", "list", "--subject", &c, "--open"],
            json!({"op": "list", "subject": c, "open": true}),
        ),
    ] {
        let cli = cli_json(&w.s, agent, &cli_args);
        let mcp = a.call("rdy_review", mcp_args).ok();
        assert_eq!(cli, mcp, "{cli_args:?}");
    }
    let open = cli_json(&w.s, agent, &["review", "list", "--open"]);
    assert!(open["requests"].as_array().unwrap().len() >= 4, "{open}");

    // a reviewer answers the first independent request; the author answers its own run
    let next = rv
        .call("rdy_review_next", json!({"kind": "decision_review"}))
        .ok();
    let concern = rv
        .call(
            "rdy_review_submit",
            json!({"request": next["request"], "case_hash": next["case_hash"],
                   "verdict": "concern", "citations": [{"block": p, "note": NOTE}],
                   "reason": REASON}),
        )
        .ok();
    let rows = cli_json(&w.s, agent, &["review", "list"]);
    let own = rows["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["mode"] == "author" && r["check"] == "decision_review")
        .unwrap()
        .clone();
    a.call(
        "rdy_review_submit",
        json!({"request": own["request"], "case_hash": own["case_hash"], "verdict": "pass"}),
    )
    .ok();

    // the list text: a line for each request, a line below it for each verdict
    let rows = cli_json(&w.s, agent, &["review", "list"]);
    let mcp = a.call("rdy_review", json!({"op": "list"})).ok();
    assert_eq!(rows, mcp);
    let mut want = vec![];
    for r in rows["requests"].as_array().unwrap() {
        let mode = if r["mode"] == "author" {
            "author-run"
        } else {
            r["mode"].as_str().unwrap()
        };
        want.push(format!(
            "{}  {}  {}  {}",
            r["request"].as_str().unwrap(),
            r["check"].as_str().unwrap(),
            mode,
            r["status"].as_str().unwrap()
        ));
        for v in r["verdicts"].as_array().unwrap() {
            let mut line = format!(
                "  {}  {}  {}",
                v["review"].as_str().unwrap(),
                v["verdict"].as_str().unwrap(),
                v["actor"].as_str().unwrap()
            );
            if let Some(reason) = v["reason"].as_str() {
                line.push_str(&format!(": {reason}"));
            }
            want.push(line);
        }
    }
    assert!(
        want.iter().any(|l| l
            == &format!(
                "  {}  concern  agent:r: {REASON}",
                concern["review"].as_str().unwrap()
            )),
        "{want:#?}"
    );
    assert!(
        want.iter()
            .any(|l| l.ends_with("  decision_review  author-run  answered")),
        "{want:#?}"
    );
    assert!(
        want.iter()
            .any(|l| l.starts_with("  ") && l.ends_with("  pass  agent:a")),
        "a pass has no colon and no reason: {want:#?}"
    );
    assert_eq!(cli_text(&w.s, agent, &["review", "list"]), want.join("\n"));

    // the flags: a relay needs the person's words, and only a request relays
    for args in [
        vec![
            "review",
            "request",
            "decision_review",
            &p,
            "--relayed-by",
            "agent:a",
        ],
        vec![
            "review",
            "request",
            "decision_review",
            &p,
            "--evidence",
            "yes",
        ],
        vec![
            "review",
            "request",
            "decision_review",
            &p,
            "--question",
            "q",
        ],
        vec!["review", "request", "decision_review", &p, "--session", "s"],
        vec![
            "review",
            "run",
            "decision_review",
            &p,
            "--relayed-by",
            "agent:a",
            "--evidence",
            "yes",
        ],
        vec!["review", "run", "decision_review", &p, "--reason", "why"],
        vec!["review", "run", "contradiction", &c, &p, "a-third"],
        vec!["review", "request", "contradiction", &c, &p, "a-third"],
        vec!["review", "checks", "extra"],
    ] {
        let out = rdy(&w.s, agent, &args);
        assert_eq!(out.status.code(), Some(2), "rdy {args:?} is a usage error");
    }
    // no new relay flag names a run
    let help = String::from_utf8_lossy(&rdy(&w.s, agent, &["review", "request", "--help"]).stdout)
        .into_owned();
    for flag in [
        "--relayed-by",
        "--evidence",
        "--question",
        "--session",
        "--reason",
    ] {
        assert!(help.contains(flag), "{help}");
    }
    assert!(
        !help.contains("relayed-run") && !help.contains("--run"),
        "{help}"
    );
    let top = String::from_utf8_lossy(&rdy(&w.s, agent, &["--help"]).stdout).into_owned();
    assert!(
        top.contains("List, run or request authoring checks, and list their requests and verdicts. Checks inform the approval card."),
        "{top}"
    );
    drop((a, rv));
}

// ---------------------------------------------------------------- source and rule text

const RULE_PACK: &str = "name: ck9\nversion: \"1\"\nextends: rdy-standard\nrules:\n  - name: notes-are-claims\n    select: type = note\n    require: type = claim\n    severity: advisory\n    message: \"a note\\u001b[31m is not a claim\"\n    fix: write it as a claim\n";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=qa",
            "-c",
            "user.email=qa",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A checkout with one commit, tagged `v1`.
fn checkout() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    std::fs::write(d.path().join("a.txt"), "a\n").unwrap();
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "--no-verify", "-m", "c"]);
    git(d.path(), &["tag", "v1"]);
    d
}

/// A served project with the rule pack and a linked checkout `app` (tag `v1`): doc `spec` with
/// claim X, which reflects three sources (release v1, release v9, a spec), landed; and the
/// person's view `work` holding a note that breaks the pack's rule, not yet proposed.
struct Sourced {
    s: Served,
    app: tempfile::TempDir,
    claim: String,
    sources: [String; 3],
    note: String,
}

fn sourced() -> Sourced {
    let s = Served::new();
    let app = checkout();
    let dir = s.dir.path();
    rdy_qa::packs::use_pack(
        dir,
        "ck9",
        RULE_PACK,
        "code_repos: [app]\nserve:\n  idle_timeout: 60\n",
    );
    std::fs::write(
        dir.join(".rdy/local.yaml"),
        format!("code_paths:\n  app: {}\n", app.path().display()),
    )
    .unwrap();
    let mut r = Rdy::open(dir).unwrap();
    let w = rdy_qa::fixtures::write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$sec", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$sec", "body": "X holds."},
            {"op": "create", "type": "source", "id": "$s1", "parent": "$sec",
                "fields": {"title": "Release\u{1b}[31m 1", "kind": "release", "locator": "v1"}, "body": "R1."},
            {"op": "create", "type": "source", "id": "$s2", "parent": "$sec",
                "fields": {"title": "Release 9", "kind": "release", "locator": "v9"}, "body": "R9."},
            {"op": "create", "type": "source", "id": "$s3", "parent": "$sec",
                "fields": {"title": "The spec", "kind": "spec", "locator": "spec/v1.md"}, "body": "S."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let id = |k: &str| w.created[k].clone();
    let (claim, sources) = (id("$x"), [id("$s1"), id("$s2"), id("$s3")]);
    rdy_qa::fixtures::write(
        &mut r,
        "linked",
        json!([{"op": "update", "id": claim,
                "links": {"reflects": sources.iter().map(|t| json!({"target": t})).collect::<Vec<_>>()}}]),
    )
    .unwrap();
    land(&mut r, "linked").unwrap();
    let note = rdy_qa::fixtures::write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "note", "id": "$n", "parent": w.created["$sec"],
                "body": "Notes are free text."}]),
    )
    .unwrap();
    r.close().unwrap();
    Sourced {
        s,
        app,
        claim,
        sources,
        note: note.created["$n"].clone(),
    }
}

/// D-JQD5X, O3, O15: a source's line in a briefing ends `; source <state>`, its text says
/// `source: <state>` after its header, a rule warning prints its fix on the next line, and
/// terminal controls in a title or a rule's message become spaces.
#[test]
fn ck9_cli_source_and_rule_text_is_exact() {
    let w = sourced();
    let [s1, s2, s3] = &w.sources;
    // the briefing: found, missing and unchecked, each after its line, in the order of the
    // sources' ids; the title's escape and line separator are spaces
    let brief = cli_text(&w.s, None, &["brief", &w.claim]);
    let mut rows = [
        (s1, "release: Release [31m 1", "found"),
        (s2, "release: Release 9", "missing"),
        (s3, "spec: The spec", "unchecked"),
    ];
    rows.sort();
    let want: String = rows
        .iter()
        .map(|(id, what, state)| format!("  {id} {what} ({}); source {state}\n", w.claim))
        .collect();
    let want = format!("sources:\n{want}");
    assert!(brief.contains(&want), "{brief:?}\nwant {want:?}");
    // the JSON keeps the key the line reads
    let j = cli_json(&w.s, None, &["brief", &w.claim]);
    let states: Vec<(&str, &str)> = j["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["source"].as_str().unwrap(),
                s["existence"].as_str().unwrap(),
            )
        })
        .collect();
    let by_id: Vec<(&str, &str)> = rows
        .iter()
        .map(|(id, _, state)| (id.as_str(), *state))
        .collect();
    assert_eq!(states, by_id);
    // the source text: the state follows the type and version header
    for (id, state) in [(s1, "found"), (s2, "missing"), (s3, "unchecked")] {
        let shown = cli_text(&w.s, None, &["show", id]);
        let mut lines = shown.lines();
        let header = lines.next().unwrap();
        assert!(
            header.starts_with(&format!("{id} source (version ")),
            "{shown}"
        );
        assert_eq!(lines.next().unwrap(), format!("source: {state}"), "{shown}");
    }
    // a block that is no source has no such line
    let claim = cli_text(&w.s, None, &["show", &w.claim]);
    assert!(!claim.contains("\nsource: "), "{claim}");
    // the propose report: the rule warning, then its fix, with the escape as a space
    let note = &w.note;
    let proposed = cli_json(
        &w.s,
        None,
        &["propose", "--title", "A note", "--summary", TEST_SUMMARY],
    );
    let warning = proposed["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["code"] == "W_RULE_CHECK")
        .unwrap_or_else(|| panic!("a rule warning: {proposed}"));
    assert_eq!(
        warning["message"],
        format!("rule notes-are-claims fails on {note}: a note\u{1b}[31m is not a claim")
    );
    assert_eq!(warning["fix"], "write it as a claim");
    let text = cli_text(
        &w.s,
        None,
        &["propose", "--title", "A note", "--summary", TEST_SUMMARY],
    );
    let want = format!(
        "\nwarning W_RULE_CHECK: rule notes-are-claims fails on {note}: a note [31m is not a claim\n  fix: write it as a claim\n"
    );
    assert!(text.contains(&want), "{text:?}\nwant {want:?}");
    // the card the engine renders is shown as it is
    let pending = cli_json(&w.s, None, &["pending"]);
    let item = &pending["items"][0];
    assert_eq!(item["kind"], "approve");
    assert!(
        cli_text(&w.s, None, &["pending"]).contains(item["card"].as_str().unwrap()),
        "the CLI prints the engine's card"
    );
    drop(w.app);
}

/// D-JQD5X, O3, O16: the checkout's state moves what is displayed (the sources' lines and the
/// coverage people read) and nothing else: the rule warnings, the stored assessment's card and
/// the policy verdict are the same under two checkout states.
#[test]
fn ck9_machine_source_fact_never_changes_rule_or_policy_gate() {
    let w = sourced();
    let [s1, s2, _] = &w.sources;
    let existence = |w: &Sourced| -> BTreeMap<String, String> {
        let j = cli_json(&w.s, None, &["brief", &w.claim]);
        j["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["source"].as_str().unwrap().to_string(),
                    s["existence"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let states = |x: [&str; 3]| -> BTreeMap<String, String> {
        w.sources.iter().cloned().zip(x.map(String::from)).collect()
    };
    let not_found = |w: &Sourced| -> Vec<Value> {
        let j = cli_json(&w.s, None, &["coverage"]);
        j["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["code"] == "source_not_found")
            .cloned()
            .collect()
    };
    let propose = |w: &Sourced| -> Value {
        cli_json(
            &w.s,
            None,
            &["propose", "--title", "A note", "--summary", TEST_SUMMARY],
        )
    };
    let card = |w: &Sourced| -> Value {
        let pending = cli_json(&w.s, None, &["pending"]);
        assert_eq!(pending["items"].as_array().unwrap().len(), 1, "{pending}");
        pending["items"][0].clone()
    };
    // state one: v1 is a tag of the checkout, v9 is not
    assert_eq!(existence(&w), states(["found", "missing", "unchecked"]));
    let missing_one = not_found(&w);
    assert_eq!(missing_one.len(), 1, "{missing_one:?}");
    assert!(
        missing_one[0].to_string().contains(s2.as_str()),
        "{missing_one:?}"
    );
    let first = propose(&w);
    let one = card(&w);
    let warned = |p: &Value| -> Vec<Value> {
        p["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["code"] == "W_RULE_CHECK")
            .cloned()
            .collect()
    };
    assert_eq!(warned(&first).len(), 1, "{first}");
    // state two: the tags change places
    git(w.app.path(), &["tag", "-d", "v1"]);
    git(w.app.path(), &["tag", "v9"]);
    assert_eq!(existence(&w), states(["missing", "found", "unchecked"]));
    let missing_two = not_found(&w);
    assert_eq!(missing_two.len(), 1, "{missing_two:?}");
    assert!(
        missing_two[0].to_string().contains(s1.as_str()),
        "{missing_two:?}"
    );
    assert_ne!(missing_one, missing_two, "the coverage people read moved");
    // the stored version's card is the same card, hash and options included
    assert_eq!(
        card(&w),
        one,
        "the stored assessment does not read the checkout"
    );
    // a new version under the new state: the same rule rows, readiness, policy verdict and facts
    let second = propose(&w);
    let two = card(&w);
    assert_ne!(first["ref"], second["ref"]);
    assert_eq!(warned(&first), warned(&second), "the rule rows");
    assert_eq!(first["policy"], second["policy"], "the policy verdict");
    assert_eq!(
        (&first["ready"], &first["issues"]),
        (&second["ready"], &second["issues"]),
        "readiness"
    );
    assert_eq!(one["facts"], two["facts"], "the assessment's facts");
    assert_eq!(option_ids(&one), option_ids(&two));
    assert!(
        fact_lines(&two)
            .iter()
            .any(|l| l.starts_with("Rule check: ")),
        "{:?}",
        fact_lines(&two)
    );
    // the machine fact never reached the card: it names no source state
    for c in [&one, &two] {
        let text = c["card"].as_str().unwrap();
        assert!(
            !text.contains("source_not_found") && !text.contains("source found"),
            "{text}"
        );
    }
    drop(w.app);
}

// ---------------------------------------------------------------- the approval loop

/// O13, O15: the CLI's `pending` and the tool's `rdy_pending` give one card: an independent
/// concern and an author-run pass follow the stored facts, the last of which is Policy; the
/// stored facts do not change; the card hash is the one an answer binds.
#[test]
fn ck9_cli_and_mcp_render_the_same_verdict_card() {
    let v = versioned();
    let mut a = client(&v.s, "agent:a", "a", "writer");
    let mut rv = client(&v.s, "agent:r", "r", "reviewer");
    let before = approve_item(&cli_json(&v.s, None, &["pending"]), &v.pref);
    let stored = fact_lines(&before);
    assert!(
        stored.last().unwrap().starts_with("Policy: "),
        "{stored:#?}"
    );
    assert!(before.get("check_verdicts").is_none());
    // an independent concern, and the author's own run with a pass
    let (_, review) = review_of(&mut a, &mut rv, &v.pref, "concern", &v.c);
    let run = a
        .call(
            "rdy_review",
            json!({"op": "run", "check": "meaning_preservation", "subject": v.pref}),
        )
        .ok();
    a.call(
        "rdy_review_submit",
        json!({"request": run["request"], "case_hash": run["case_hash"], "verdict": "pass"}),
    )
    .ok();
    let cli = approve_item(&cli_json(&v.s, None, &["pending"]), &v.pref);
    let mcp = approve_item(&a.call("rdy_pending", json!({})).ok(), &v.pref);
    assert_eq!(cli, mcp, "one card");
    let lines = fact_lines(&cli);
    let n = stored.len();
    assert_eq!(lines[..n], stored[..], "the stored facts stay as they were");
    let shown = &lines[n..];
    assert_eq!(shown.len(), 2, "{shown:#?}");
    assert!(
        shown.contains(&"meaning preservation: pass (agent:a, author-run)".to_string()),
        "{shown:#?}"
    );
    assert!(
        shown.contains(&format!(
            "meaning preservation: concern (agent:r, independent): {REASON}"
        )),
        "{shown:#?}"
    );
    // the metadata holds both rows, and the acceptance option stands before Reject and Later
    assert_eq!(cli["check_verdicts"].as_array().unwrap().len(), 2);
    assert_eq!(
        option_ids(&cli),
        [
            "approve".to_string(),
            format!("accept_verdict:{review}:{}", v.version),
            "reject".to_string(),
            "later".to_string()
        ]
    );
    // the text is the engine's card, with its verdict lines after Policy
    let text = cli_text(&v.s, None, &["pending"]);
    let card = cli["card"].as_str().unwrap();
    assert!(text.contains(card), "{text}");
    let card = squeezed(card);
    let concern = format!("meaning preservation concern (agent:r, independent): {REASON}");
    let pass = "meaning preservation pass (agent:a, author-run)";
    assert!(card.contains(&concern) && card.contains(pass), "{card}");
    assert!(
        card.contains("Accept meaning preservation concern as blocking"),
        "{card}"
    );
    let (policy, concern_at) = (card.find("Policy ").unwrap(), card.find(&concern).unwrap());
    assert!(
        policy < card.find(pass).unwrap() && policy < concern_at,
        "the verdict lines follow Policy:\n{card}"
    );
    assert!(
        concern_at < card.find("Options").unwrap(),
        "and stand before the options:\n{card}"
    );
    // the hash the person's answer binds is the one both surfaces print
    let hash = cli["card_hash"].as_str().unwrap();
    assert_ne!(hash, before["card_hash"].as_str().unwrap());
    let done = cli_json(&v.s, None, &["answer", &v.pref, "approve", "--card", hash]);
    assert_eq!(done["ref"], v.pref.as_str(), "{done}");
    drop((a, rv));
}

/// A project under an authority policy that lets `agent:a*` land draft edits, and the person
/// `human:qa` as its only approver: doc `notes` with claim `x`, and agent:a1's typo edit of `x`,
/// proposed with a summary.
struct Policed {
    s: Served,
    x: BlockId,
    pref: String,
    version: String,
}

fn policed() -> Policed {
    let s = Served::new();
    let dir = s.dir.path();
    std::fs::write(dir.join(".rdy/packs/rdy-qa.yaml"), QA_PACK_YAML).unwrap();
    std::fs::write(
        dir.join(".rdy.yaml"),
        "pack: rdy-qa\nserve:\n  idle_timeout: 60\n",
    )
    .unwrap();
    let mut r = Rdy::open(dir).unwrap();
    let write_as = |r: &mut Rdy, view: &str, ops: Value, j| {
        r.write(
            &writer(),
            rdy_engine::WriteRequest {
                view: view.into(),
                ops: parse_test_ops(ops).unwrap(),
                justification: j,
                message: String::new(),
            },
        )
        .unwrap_or_else(|e| panic!("write in {view}: {e}"))
    };
    let w = write_as(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."}
        ]),
        None,
    );
    land(&mut r, "seed").unwrap();
    let rules = write_as(
        &mut r,
        "pol",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
        None,
    );
    let d = created(&rules, "$dec");
    write_as(
        &mut r,
        "pol",
        json!([{"op": "create", "type": "authority_policy", "id": "$p", "parent": rules.created["$d"], "fields": {
            "key": "authority-policy",
            "agent_may_accept": [{
                "name": "drafts", "actors": ["agent:a*"], "lifecycle": "draft",
                "justifications": ["implements", "editorial"], "decisions": "none",
                "max_blocks": 20, "max_items": 20
            }],
            "roles": {"approvers": [PERSON]},
            "require": [{"when": "decision", "role": "approvers"}],
            "owners": []
        }}]),
        Some(rdy_model::Justification::Implements(vec![d])),
    );
    land(&mut r, "pol").unwrap();
    let x = created(&w, "$x");
    put_j(&mut r, "a1", upd(&x, "X holds, now."), j_typo());
    let p = r
        .propose(&ag("a1"), "a1", "t", Some(TEST_SUMMARY))
        .expect("propose");
    assert!(p.ready, "{:#?}", p.issues);
    r.close().unwrap();
    Policed {
        s,
        x,
        pref: p.r#ref,
        version: p.id,
    }
}

/// D-JQD5X QD2, D-HAG73: a person accepts a fail on the card through the CLI, relayed; then
/// the tool's policy land and the CLI's approve, and land, all refuse the one issue the
/// acceptance makes. A new version has no hold.
#[test]
fn ck9_person_acceptance_then_policy_land_is_refused() {
    let w = policed();
    let agent = Some(("agent:a1", "a1"));
    let mut a = client(&w.s, "agent:a1", "a1", "writer");
    let mut rv = client(&w.s, "agent:r", "r", "reviewer");
    // the policy would land the version as it stands
    let (_, review) = review_of(&mut a, &mut rv, &w.pref, "fail", &w.x);
    // the person's card, through the CLI
    let pending = cli_json(&w.s, None, &["pending"]);
    let item = approve_item(&pending, &w.pref);
    let id = format!("accept_verdict:{review}:{}", w.version);
    assert!(option_ids(&item).contains(&id), "{:?}", option_ids(&item));
    let hash = item["card_hash"].as_str().unwrap().to_string();
    // the answer is relayed by agent:relay with the person's words and the card hash
    let done = cli_json(
        &w.s,
        Some(("agent:relay", "r1")),
        &[
            "answer",
            &w.pref,
            &id,
            "--card",
            &hash,
            "--relayed-by",
            "agent:relay",
            "--evidence",
            "Yes, block it.",
            "--question",
            "Block this version?",
            "--session",
            "chat-9",
        ],
    );
    assert_eq!(done["act"], "accept_verdict", "{done}");
    assert_eq!(done["option"], id.as_str());
    assert_eq!(
        done["label"],
        "Accept meaning preservation fail as blocking"
    );
    assert_eq!(done["assurance"], "relayed");
    let held = format!(
        "meaning_preservation verdict {review} is accepted as blocking for {}",
        w.pref
    );
    // the tool's policy land, then the CLI's approve and land: the same issue, and no landing
    let head = cli_json(&w.s, None, &["log", "--limit", "1"])[0]["landing"].clone();
    let by_tool = a.call("rdy_land", json!({"proposal": w.pref}));
    assert_eq!(by_tool.code(), Some("E_PROPOSAL_NOT_READY"));
    let refusal = match by_tool {
        rdy_qa::mcp::Reply::ToolError(v) => v,
        other => panic!("refused, not {:?}", other.is_ok()),
    };
    assert_eq!(issue_messages(&refusal), std::slice::from_ref(&held));
    assert_eq!(refusal["issues"][0]["fix"], HELD_FIX);
    for args in [
        vec!["approve", w.pref.as_str()],
        vec!["land", w.pref.as_str()],
    ] {
        let who = if args[0] == "land" { agent } else { None };
        let refused = cli_refused(&w.s, who, &args);
        assert_eq!(refused["code"], "E_PROPOSAL_NOT_READY", "{refused}");
        assert_eq!(
            issue_messages(&refused),
            std::slice::from_ref(&held),
            "{args:?}"
        );
        assert_eq!(refused["issues"][0]["fix"], HELD_FIX);
    }
    // the person's answer to approve is refused too, and the queue lists the version as not ready
    let refused = cli_refused(&w.s, None, &["answer", &w.pref, "approve"]);
    assert_eq!(issue_messages(&refused), std::slice::from_ref(&held));
    let text = cli_text(&w.s, None, &["pending"]);
    assert!(
        text.contains(&format!("not ready: {}: {held} (fix: {HELD_FIX})", w.pref)),
        "{text}"
    );
    assert_eq!(
        cli_json(&w.s, None, &["log", "--limit", "1"])[0]["landing"],
        head,
        "nothing landed"
    );
    // a new version has no hold: the author writes again, proposes, and the policy lands it
    drop((a, rv));
    w.s.stop();
    let pref2 = {
        let mut r = Rdy::open(w.s.dir.path()).unwrap();
        put_j(&mut r, "a1", upd(&w.x, "X holds, fixed."), j_typo());
        let p = r
            .propose(&ag("a1"), "a1", "t", Some(TEST_SUMMARY))
            .expect("propose again");
        r.close().unwrap();
        assert!(p.ready, "{:#?}", p.issues);
        assert_ne!(p.id, w.version);
        p.r#ref
    };
    let pending = cli_json(&w.s, None, &["pending"]);
    approve_item(&pending, &pref2);
    assert!(
        pending["not_ready"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|n| n["ref"] != pref2.as_str()),
        "the new version holds nothing: {pending}"
    );
    let landed = cli_json(&w.s, agent, &["land", &pref2]);
    assert_eq!(landed["assurance"], "policy", "{landed}");
}

/// O13, D-HAG73: a verdict that arrives after the person's card was printed makes the answer
/// refuse the old card with the fresh one, writes nothing, lands nothing and leaves the stored
/// assessment as it was.
#[test]
fn ck9_late_verdict_refuses_the_old_cli_card() {
    let v = versioned();
    let assessment_of = |s: &Served| -> String {
        // the store is open in the daemon only: read the stored assessment's hash through a
        // stopped project
        s.stop();
        let r = Rdy::open(s.dir.path()).unwrap();
        let st = r.read();
        let version: rdy_model::Hash = v.version.parse().unwrap();
        let h = st
            .reader()
            .assessment_of_proposal(&version, st.seq())
            .unwrap()
            .expect("a stored assessment");
        drop(st);
        r.close().unwrap();
        h.to_string()
    };
    let assessment = assessment_of(&v.s);
    let mut a = client(&v.s, "agent:a", "a", "writer");
    let mut rv = client(&v.s, "agent:r", "r", "reviewer");
    let old = approve_item(&cli_json(&v.s, None, &["pending"]), &v.pref);
    let old_hash = old["card_hash"].as_str().unwrap().to_string();
    let head = cli_json(&v.s, None, &["log", "--limit", "1"])[0]["landing"].clone();
    // the late verdict
    review_of(&mut a, &mut rv, &v.pref, "concern", &v.c);
    // the person answers on the old card
    let refused = cli_refused(
        &v.s,
        None,
        &["answer", &v.pref, "approve", "--card", &old_hash],
    );
    assert_eq!(refused["code"], "E_PROPOSAL_MOVED", "{refused}");
    // the corrected card is the item's `ref`, `card` text and `card_hash`
    let fresh = &refused["corrected"];
    assert_eq!(fresh["ref"], v.pref.as_str(), "{refused}");
    assert_ne!(fresh["card_hash"].as_str().unwrap(), old_hash);
    let card = squeezed(fresh["card"].as_str().unwrap());
    assert!(
        card.contains(&format!(
            "meaning preservation concern (agent:r, independent): {REASON}"
        )),
        "{refused}"
    );
    assert!(
        card.contains("Accept meaning preservation concern as blocking"),
        "{refused}"
    );
    // the plain-text refusal prints the fresh card
    let out =
        v.s.rdy(&["answer", &v.pref, "approve", "--card", &old_hash]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("error E_PROPOSAL_MOVED"), "{err}");
    assert!(err.contains(fresh["card"].as_str().unwrap()), "{err}");
    // nothing landed, and the assessment is the one propose stored
    assert_eq!(
        cli_json(&v.s, None, &["log", "--limit", "1"])[0]["landing"],
        head
    );
    assert_eq!(
        assessment_of(&v.s),
        assessment,
        "the stored assessment's bytes"
    );
    // the fresh card's hash lands the version
    let done = cli_json(
        &v.s,
        None,
        &[
            "answer",
            &v.pref,
            "approve",
            "--card",
            fresh["card_hash"].as_str().unwrap(),
        ],
    );
    assert_eq!(done["ref"], v.pref.as_str(), "{done}");
    assert!(done["landing"].is_object(), "{done}");
}
