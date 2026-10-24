//! rdx13 (D-KQ4CA: "The CLI keeps parity with the tools"; D-CC90E: removal lands only with a
//! person's approval; D-6B1W1: the report before the proposal). `rdy redact` sends the shared
//! daemon op `redact` and prints the shared formatter's text; its JSON is the op's reply. It has
//! no force, yes, policy or approval switch. A request is proposed and its removal card is the
//! approval card of the queue. These tests stop before a successful removal landing (Task 15
//! owns that flow).
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::redaction::{RedactInput, RedactResult, report_text};
use rdy_engine::*;
use rdy_model::Role;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use serde_json::{Value, json};

const SECRET: &str = "The launch code is ZEBRA-7731.";
const CLASSES: [&str; 4] = ["secret", "personal", "legal", "retraction"];

const COPIES: &str = "This removes bytes from this store. It cannot erase exports, renders, dumps, clones, published files, working copies, backups, or filesystem snapshots.";
const INVENTORY: &str = "Dumps, clones, exports, renders, and published versions may hold copies. Their locations are unknown to rdy.";
const QUOTES: &str = "Quote matches use complete source strings. Partial quotes may still hold content. Snapshot issue lists and summaries, review citations, tension advice, answer notes and change-plan versions are not erased.";
const REUSE: &str = "This store will refuse these object and chunk hashes on later writes. This can block an identical restore or a file that shares a removed chunk.";
const ROTATE: &str = "Rotate or revoke the secret. Redaction does not undo its exposure.";
const IRREVERSIBLE: &str = "Approved content removal cannot be undone.";
const ROWS_OMITTED: &str = "The row list was omitted. Increase max_tokens to read it.";

/// Runs `f` on the project with the daemon stopped (it holds the store while it runs).
fn with_rdy<T>(s: &Served, f: impl FnOnce(&mut Rdy) -> T) -> T {
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let out = f(&mut r);
    r.close().unwrap();
    out
}

/// Runs the daemon op `op` on the project as `session`, through `dispatch`: the op the daemon
/// serves, and the route `rdy --no-daemon` takes (`Conn::op`, Direct).
fn op_as(
    s: &Served,
    session: &Session,
    op: &str,
    args: Value,
) -> Result<Value, rdy_daemon::ErrorReply> {
    s.stop();
    let e = Local::open(s.dir.path()).unwrap();
    let cx = OpContext {
        engine: &e,
        session,
        human: "human:qa",
        cwd: s.dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let out = dispatch(&cx, op, args).map_err(rdy_daemon::ErrorReply::from);
    e.close().unwrap();
    out
}

/// The CLI's human session: `human:qa`, a writer (`Conn::open` classifies it so).
fn human() -> Session {
    Session::human("human:qa".into(), Role::Writer).unwrap()
}

/// The project's log position, read with the daemon stopped (as text, to compare).
fn seq(s: &Served) -> String {
    with_rdy(s, |r| format!("{:?}", r.read().seq()))
}

/// A served project whose view `work` holds a doc with a prose block holding `SECRET`; returns
/// the prose block's id.
fn secret_served() -> (Served, String) {
    let s = Served::new();
    let id = with_rdy(&s, |r| {
        let w = write(
            r,
            "work",
            json!([
                {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "design", "title": "Design"}},
                {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"title": "Doc", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET}
            ]),
        )
        .unwrap();
        w.created["$p"].clone()
    });
    (s, id)
}

fn out(o: &std::process::Output) -> (bool, String, String) {
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into(),
        String::from_utf8_lossy(&o.stderr).into(),
    )
}

/// The `RedactResult` inside a redact reply (the mutation envelope adds its own keys).
fn result_of(v: &Value) -> RedactResult {
    serde_json::from_value(json!({
        "dry_run": v["dry_run"], "reason_class": v["reason_class"], "reason": v["reason"],
        "change": v["change"], "redaction": v["redaction"], "report": v["report"],
        "warnings": v["warnings"],
    }))
    .unwrap_or_else(|e| panic!("{e}: {v}"))
}

/// `rdy redact` args for `id`: the class, the reason and any extra flags.
fn redact_args<'a>(
    id: &'a str,
    class: &'a str,
    reason: &'a str,
    extra: &[&'a str],
) -> Vec<&'a str> {
    let mut a = vec!["redact", id, "--reason-class", class, "--reason", reason];
    a.extend_from_slice(extra);
    a
}

/// The error a failed `--json` command printed (stderr, one JSON object).
fn error_of(o: &std::process::Output) -> Value {
    let (ok, _, err) = out(o);
    assert!(!ok, "the command succeeded");
    serde_json::from_str(err.trim().lines().last().unwrap_or(""))
        .unwrap_or_else(|e| panic!("{e}: {err}"))
}

#[test]
fn rdx13_cli_matches_the_shared_request_and_report_contract() {
    let (s, id) = secret_served();
    for class in CLASSES {
        // a dry run: the CLI's JSON is the op's reply, and its text is the shared formatter's
        let mut json_args = vec!["--json"];
        json_args.extend(redact_args(&id, class, "  a leaked code  ", &["--dry-run"]));
        let (ok, cli) = s.json(&json_args);
        assert!(ok, "{class}: {cli}");
        let o = s.rdy(&redact_args(
            &id,
            class,
            "  a leaked code  ",
            &["--dry-run"],
        ));
        let (ok, text, err) = out(&o);
        assert!(ok, "{class}: {err}");
        let op = op_as(
            &s,
            &human(),
            "redact",
            json!({"targets": [id], "reason_class": class, "reason": "  a leaked code  ", "dry_run": true}),
        )
        .unwrap();
        assert_eq!(cli, op, "{class}: the CLI's JSON is the op's reply");
        assert_eq!(op["reason_class"], class);
        assert_eq!(op["reason"], "a leaked code");
        let want = report_text(&result_of(&op), 4000).unwrap();
        assert!(!want.truncated, "{class}");
        assert_eq!(op["text"], want.text.as_str());
        assert_eq!(text, format!("{}\n", want.text), "{class}: the CLI text");
        assert!(text.starts_with("Redaction dry run\n"), "{text}");
        assert!(text.ends_with("\nNo request was recorded.\n"), "{text}");
        for line in [COPIES, INVENTORY, QUOTES, REUSE] {
            assert!(text.contains(line), "{class}: {text}");
        }
        assert_eq!(text.contains(ROTATE), class == "secret", "{class}: {text}");
        assert!(!text.contains("ZEBRA"), "{class}: no source text: {text}");
    }
    // the defaults: not a dry run, the session's view (the human's work), 4000 tokens
    let (ok, dry) = s.json(&[
        "--json",
        "redact",
        &id,
        "--reason-class",
        "personal",
        "--reason",
        "a name",
        "--dry-run",
    ]);
    assert!(ok, "{dry}");
    let before = seq(&s);
    let (ok, rec) = s.json(&[
        "--json",
        "redact",
        &id,
        "--reason-class",
        "personal",
        "--reason",
        "a name",
    ]);
    assert!(ok, "{rec}");
    assert_eq!(rec["dry_run"], false);
    assert!(
        rec["consequences"].is_array(),
        "the mutation envelope: {rec}"
    );
    assert!(rec.get("truncated").is_none(), "{rec}");
    assert_eq!(
        rec["report"], dry["report"],
        "the request binds the dry run's report"
    );
    assert_eq!(rec["warnings"], dry["warnings"]);
    let (change, redaction) = (
        rec["change"].as_str().unwrap().to_string(),
        rec["redaction"].as_str().unwrap().to_string(),
    );
    let result = result_of(&rec);
    let want = report_text(&result, 4000).unwrap();
    assert_eq!(rec["text"], want.text.as_str());
    assert!(
        want.text.ends_with(&format!(
            "\nRecorded redaction {redaction} in change {change}.\n\nPropose the view to ask for approval."
        )),
        "{}",
        want.text
    );
    let ch: rdy_model::Hash = change.parse().unwrap();
    with_rdy(&s, |r| {
        let st = r.read();
        assert_ne!(
            format!("{:?}", st.seq()),
            before,
            "the request was recorded"
        );
        assert!(st.included("work").contains(&ch), "the carrier is in work");
        let req = st
            .reader()
            .redaction_request(&redaction.parse().unwrap())
            .unwrap();
        assert_eq!(req.reason, "a name");
        assert_eq!(req.carrier, ch);
        // nothing is erased: the block still reads its own text
        assert_eq!(body_of(r, &Scope::View("work".into()), &id), SECRET);
    });
    // a recorded request's text is the formatter's at the default budget
    let o = s.rdy(&[
        "redact",
        &id,
        "--reason-class",
        "legal",
        "--reason",
        "an order",
    ]);
    let (ok, text, err) = out(&o);
    assert!(ok, "{err}");
    assert!(text.starts_with("Redaction request\n"), "{text}");
    assert!(
        text.ends_with("\n\nPropose the view to ask for approval.\n"),
        "{text}"
    );
    // the op's refusals reach the CLI unchanged: zero targets, an unknown class, a bad selector
    for (cli_args, op_args) in [
        (
            vec![
                "--json",
                "redact",
                "--reason-class",
                "secret",
                "--reason",
                "x",
            ],
            json!({"targets": [], "reason_class": "secret", "reason": "x"}),
        ),
        (
            vec![
                "--json",
                "redact",
                id.as_str(),
                "--reason-class",
                "other",
                "--reason",
                "x",
            ],
            json!({"targets": [id], "reason_class": "other", "reason": "x"}),
        ),
        (
            vec![
                "--json",
                "redact",
                "p-ZZZZZ",
                "--reason-class",
                "secret",
                "--reason",
                "x",
            ],
            json!({"targets": ["p-ZZZZZ"], "reason_class": "secret", "reason": "x"}),
        ),
        (
            vec![
                "--json",
                "redact",
                id.as_str(),
                "--reason-class",
                "secret",
                "--reason",
                " ",
            ],
            json!({"targets": [id], "reason_class": "secret", "reason": " "}),
        ),
    ] {
        let before = seq(&s);
        let cli = error_of(&s.rdy(&cli_args));
        let op = op_as(&s, &human(), "redact", op_args).unwrap_err();
        assert_eq!(
            (
                cli["code"].as_str(),
                cli["message"].as_str(),
                cli["fix"].as_str()
            ),
            (
                Some(op.code.as_str()),
                Some(op.message.as_str()),
                Some(op.fix.as_str())
            ),
            "{cli_args:?}"
        );
        assert_eq!(seq(&s), before, "{cli_args:?}: a refusal writes nothing");
    }
    let e = op_as(
        &s,
        &human(),
        "redact",
        json!({"targets": [], "reason_class": "secret", "reason": "x"}),
    )
    .unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("E_REDACTION_TARGET", "Cannot redact []: invalid selector.")
    );
    // no switch skips the person: no force, yes, policy or approval flag, on redact or approve
    for flag in ["--force", "--yes", "--policy", "--approve", "--apply"] {
        let o = s.rdy(&redact_args(&id, "secret", "x", &[flag]));
        let (ok, _, err) = out(&o);
        assert!(!ok && err.contains("unexpected argument"), "{flag}: {err}");
        let o = s.rdy(&["approve", "P-0123456789ab@1", flag]);
        let (ok, _, err) = out(&o);
        assert!(!ok && err.contains("unexpected argument"), "{flag}: {err}");
    }
}

#[test]
fn rdx13_reader_dry_run_and_owned_view_rules_are_preserved() {
    let (s, id) = secret_served();
    let args = |dry: bool| json!({"targets": [id], "reason_class": "secret", "reason": "leak", "dry_run": dry});
    let before = seq(&s);
    // a reader's report works; its request is refused (a CLI session is a writer, so the reader
    // takes the CLI's Direct route, `dispatch`)
    let reader = Session::human("human:qa".into(), Role::Reader).unwrap();
    let report = op_as(&s, &reader, "redact", args(true)).unwrap();
    let err = op_as(&s, &reader, "redact", args(false)).unwrap_err();
    assert_eq!(err.code, "E_PERMISSION", "{err:?}");
    // an agent dry-runs through the CLI, in any view; the scan is project-wide: the target lives
    // only in the human's work view, and the agent's report is the reader's
    let (ok, v) = s.json_as(
        "agent:a",
        Some("r1"),
        &[
            "--json",
            "redact",
            &id,
            "--reason-class",
            "secret",
            "--reason",
            "leak",
            "--dry-run",
            "--view",
            "work",
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v, report, "one report, whoever asks and in whichever view");
    // an agent never records in the human's view: the existing ownership error
    let o = s.rdy_as(
        "agent:a",
        Some("r1"),
        &[
            "--json",
            "redact",
            &id,
            "--reason-class",
            "secret",
            "--reason",
            "leak",
            "--view",
            "work",
        ],
    );
    let e = error_of(&o);
    assert_eq!(e["code"], "E_VIEW_OWNER", "{e}");
    assert_eq!(seq(&s), before, "refusals write nothing");
    // the agent's first request creates and claims its own view, as its first commit would
    let (ok, v) = s.json_as(
        "agent:a",
        Some("r1"),
        &[
            "--json",
            "redact",
            &id,
            "--reason-class",
            "secret",
            "--reason",
            "leak",
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(
        v["report"], report["report"],
        "the request binds the same report"
    );
    let ch: rdy_model::Hash = v["change"].as_str().unwrap().parse().unwrap();
    with_rdy(&s, |r| {
        let st = r.read();
        assert!(st.included("r1").contains(&ch));
        let owner = st.view_head("r1").unwrap().owner.clone().unwrap();
        assert_eq!(
            (owner.actor.as_str(), owner.run.as_deref()),
            ("agent:a", Some("r1"))
        );
    });
    // another agent may not record in it: the existing text, unchanged
    let before = seq(&s);
    let o = s.rdy_as(
        "agent:b",
        Some("r2"),
        &[
            "--json",
            "redact",
            &id,
            "--reason-class",
            "secret",
            "--reason",
            "leak",
            "--view",
            "r1",
        ],
    );
    let e = error_of(&o);
    assert_eq!(e["code"], "E_VIEW_OWNER", "{e}");
    assert_eq!(e["message"], "view \"r1\" is owned by agent:a (run r1)");
    assert_eq!(seq(&s), before);
}

#[test]
fn rdx13_direct_approval_displays_the_complete_card() {
    let (s, id) = secret_served();
    // the request, recorded through the shared op, then proposed with a summary
    let (ok, rec) = s.json(&[
        "--json",
        "redact",
        &id,
        "--reason-class",
        "secret",
        "--reason",
        "  a leaked code  ",
    ]);
    assert!(ok, "{rec}");
    let (ok, p) = s.json(&[
        "--json",
        "propose",
        "--title",
        "Remove the code",
        "--summary",
        "Removes the leaked code.",
    ]);
    assert!(ok, "{p}");
    let pref = p["ref"].as_str().unwrap().to_string();
    // the queue's card: one individual approval item bound to the request and its report
    let (ok, q) = s.json(&["--json", "pending"]);
    assert!(ok, "{q}");
    let item = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == pref.as_str())
        .unwrap_or_else(|| panic!("{pref} is not in the queue: {q}"))
        .clone();
    let rep = &rec["report"];
    let len = |k: &str| rep[k].as_array().unwrap().len() as u64;
    assert_eq!(
        item["removal"],
        json!([{
            "request": rec["redaction"], "report": rep["report_hash"],
            "versions": len("versions"), "blobs": len("blobs"), "shared_blobs": len("shared_blobs"),
            "payloads": rep["drop_frames"], "bytes": rep["drop_bytes"],
            "reason_class": "secret", "reason": "a leaked code"
        }]),
        "{item}"
    );
    let card = item["card"].as_str().unwrap();
    for line in [
        "Content removal".to_string(),
        format!("  Request: {}", text(&rec["redaction"])),
        format!("  Report: {}", text(&rep["report_hash"])),
        format!(
            "  Versions: {}; blobs: {}; shared blobs: {}.",
            len("versions"),
            len("blobs"),
            len("shared_blobs")
        ),
        format!(
            "  Payloads: {}; bytes: {}.",
            rep["drop_frames"], rep["drop_bytes"]
        ),
        "  Reason class: secret.".to_string(),
        "  Reason: a leaked code".to_string(),
        format!("  {COPIES}"),
        format!("  {QUOTES}"),
        format!("  {REUSE}"),
        format!("  {ROTATE}"),
    ] {
        assert!(card.contains(&line), "{line}\n---\n{card}");
    }
    assert!(!card.contains("ZEBRA"), "{card}");
    let approve = item["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "approve")
        .unwrap();
    assert_eq!(
        approve["consequences"],
        json!(["Lands the proposal in canon.", IRREVERSIBLE])
    );
    // the person reads the same card as text
    let o = s.rdy(&["pending"]);
    let (ok, t, err) = out(&o);
    assert!(ok, "{err}");
    assert!(t.contains(card), "{t}");
    // the agent-only guards stand unchanged: an agent alone neither approves nor answers the
    // removal card in bulk, and nothing lands
    let landings = with_rdy(&s, |r| r.read().log_page(None, None).unwrap().len());
    let e = error_of(&s.rdy_as("agent:a", Some("r1"), &["--json", "approve", &pref]));
    assert_eq!(e["code"], "E_PERMISSION", "{e}");
    assert_eq!(
        e["message"],
        "an agent approves only by relaying a person's approval"
    );
    let e = error_of(&s.rdy(&["--json", "answer", "--all-recommended", &pref]));
    assert_eq!(e["code"], "E_REDACTION_HUMAN", "{e}");
    assert_eq!(e["message"], "Content removal needs a person's approval.");
    assert_eq!(
        with_rdy(&s, |r| r.read().log_page(None, None).unwrap().len()),
        landings,
        "nothing landed"
    );
}

/// A served project whose prose block holding `SECRET` has `n` versions in view `work`; returns
/// its id.
fn many_versions(n: usize) -> (Served, String) {
    let (s, id) = secret_served();
    with_rdy(&s, |r| {
        for i in 1..n {
            let v = version_of(r, &Scope::View("work".into()), &id);
            write(
                r,
                "work",
                json!([{"op": "update", "id": id, "base": v, "body": format!("{SECRET} ({i})")}]),
            )
            .unwrap();
        }
    });
    (s, id)
}

#[test]
fn rdx13_budgets_never_change_the_manifest() {
    let (s, id) = many_versions(12);
    let dry = |budget: &str| {
        s.json(&[
            "--json",
            "redact",
            &id,
            "--reason-class",
            "legal",
            "--reason",
            "an order",
            "--dry-run",
            "--max-tokens",
            budget,
        ])
    };
    let (ok, full) = dry("20000");
    assert!(ok, "{full}");
    assert!(full.get("truncated").is_none(), "{full}");
    let full_text = full["text"].as_str().unwrap();
    assert!(full_text.contains("block_version: "), "{full_text}");
    // a budget the rows do not fit: the rows give way to one line, nothing else changes
    assert!(
        full_text.len() / 4 > 400,
        "the rows need more than 400 tokens"
    );
    let (ok, short) = dry("400");
    assert!(ok, "{short}");
    assert_eq!(short["truncated"], true, "{short}");
    assert_eq!(
        short["report"], full["report"],
        "the manifest never changes"
    );
    assert_eq!(short["warnings"], full["warnings"]);
    let short_text = short["text"].as_str().unwrap();
    assert!(short_text.len() / 4 <= 400, "{short_text}");
    assert!(!short_text.contains("block_version: "), "{short_text}");
    let warnings: Vec<String> = full["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| format!("{}: {}", text(&w["code"]), text(&w["message"])))
        .collect();
    assert_eq!(
        warnings,
        [
            format!("W_REDACTION_COPIES: {COPIES}"),
            format!("W_REDACTION_INVENTORY: {INVENTORY}"),
            format!("W_REDACTION_QUOTES: {QUOTES}"),
            format!("W_REDACTION_REUSE: {REUSE}"),
        ]
    );
    let lines: Vec<&str> = short_text.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == ROWS_OMITTED)
        .expect("the omitted line");
    assert_eq!(
        &lines[at + 1..],
        [
            "Recorded assessments: not implemented.",
            "Reason (legal): \"an order\"",
            warnings[0].as_str(),
            warnings[1].as_str(),
            warnings[2].as_str(),
            warnings[3].as_str(),
            "No request was recorded.",
        ]
    );
    // final review fix round 2, finding 5 (ruling final): the shortened text adds one mandatory
    // line after its targets, the report hash; every other head line is the full report's
    let report_line = format!("Report: {}.", text(&full["report"]["report_hash"]));
    assert_eq!(lines[2], report_line, "{short_text}");
    let head: Vec<&str> = lines[..at]
        .iter()
        .copied()
        .filter(|l| *l != report_line)
        .collect();
    assert_eq!(
        head[..],
        full_text.lines().take(at - 1).collect::<Vec<_>>()[..],
        "the head is the full report's"
    );
    // the CLI text is the same text
    let o = s.rdy(&[
        "redact",
        &id,
        "--reason-class",
        "legal",
        "--reason",
        "an order",
        "--dry-run",
        "--max-tokens",
        "400",
    ]);
    let (ok, t, err) = out(&o);
    assert!(ok, "{err}");
    assert_eq!(t, format!("{short_text}\n"));
    // a recorded request at that budget binds the full report and ends with its result
    let (ok, rec) = s.json(&[
        "--json",
        "redact",
        &id,
        "--reason-class",
        "legal",
        "--reason",
        "an order",
        "--max-tokens",
        "400",
    ]);
    assert!(ok, "{rec}");
    assert_eq!(rec["truncated"], true, "{rec}");
    assert_eq!(rec["report"], full["report"]);
    let rec_text = rec["text"].as_str().unwrap();
    assert!(rec_text.contains(ROWS_OMITTED), "{rec_text}");
    assert!(
        rec_text.ends_with(&format!(
            "\n{}\nRecorded redaction {} in change {}.\n\nPropose the view to ask for approval.",
            warnings[3],
            text(&rec["redaction"]),
            text(&rec["change"])
        )),
        "{rec_text}"
    );
    // a budget out of range, and one too small for the warnings, refuse before writing
    let before = seq(&s);
    let e = error_of(&s.rdy(&[
        "--json",
        "redact",
        &id,
        "--reason-class",
        "legal",
        "--reason",
        "x",
        "--max-tokens",
        "255",
    ]));
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str()),
        (
            Some("E_BAD_INPUT"),
            Some("max_tokens must be from 256 to 20000.")
        )
    );
    let long = "r".repeat(1500);
    let e = error_of(&s.rdy(&[
        "--json",
        "redact",
        &id,
        "--reason-class",
        "secret",
        "--reason",
        &long,
        "--max-tokens",
        "256",
    ]));
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str(), e["fix"].as_str()),
        (
            Some("E_REDACTION_BUDGET"),
            Some("The output budget is too small for the redaction warning."),
            Some("Increase max_tokens, or propose fewer redaction requests at once.")
        )
    );
    assert_eq!(seq(&s), before, "a refused budget writes nothing");
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// Records one secret request for `id` in `work` through the CLI and proposes `work` (with
/// `summary`, when given); returns the propose reply.
fn propose_removal(s: &Served, id: &str, summary: Option<&str>) -> Value {
    let (ok, rec) = s.json(&[
        "--json",
        "redact",
        id,
        "--reason-class",
        "secret",
        "--reason",
        "a leaked code",
    ]);
    assert!(ok, "{rec}");
    let mut args = vec!["--json", "propose", "--title", "Remove the code"];
    if let Some(summary) = summary {
        args.extend(["--summary", summary]);
    }
    let (ok, p) = s.json(&args);
    assert!(ok, "{p}");
    p
}

/// Lands an unrelated view `side` as the approver, so that a later `--strict` approval of any
/// other version is refused before it lands (canon moved since that version was proposed).
fn land_unrelated(s: &Served) {
    with_rdy(s, |r| {
        seed_doc(r, "side");
        land(r, "side").unwrap();
    });
}

/// The number of landings in the log.
fn landings(s: &Served) -> usize {
    with_rdy(s, |r| r.read().log_page(None, None).unwrap().len())
}

/// rdx13 fix round 1 (findings 1 and 2): `rdy approve` resolves the proposal it is given with the
/// shared resolver and shows that version's removal card for every spelling the approval accepts
/// (a full version hash, `P-<id>@01`, `P-<id>@1`, `P-<id>`), before the approval runs: a refused
/// approval still showed the whole card, on stdout as text and in the error as JSON.
#[test]
fn rdx13_every_spelling_shows_the_card_before_the_approval_runs() {
    let (s, id) = secret_served();
    let p = propose_removal(&s, &id, Some("Removes the leaked code."));
    let pref = p["ref"].as_str().unwrap().to_string();
    let hash = p["id"].as_str().unwrap().to_string();
    let stable = pref
        .strip_prefix("P-")
        .and_then(|r| r.split_once('@'))
        .unwrap()
        .0
        .to_string();
    land_unrelated(&s);
    // the card the queue holds for the version now, whole
    let q = op_as(&s, &human(), "pending", json!({"max_tokens": 20000})).unwrap();
    let item = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == pref.as_str())
        .unwrap_or_else(|| panic!("{pref}: {q}"))
        .clone();
    let card = item["card"].as_str().unwrap().to_string();
    assert!(card.contains("Content removal"), "{card}");
    let before = landings(&s);
    for named in [
        hash.clone(),
        format!("P-{stable}@01"),
        format!("P-{stable}@1"),
        format!("P-{stable}"),
        // fix round 2 (finding 1): the resolver reads `@+1` as version 1 (`u32` parsing)
        format!("P-{stable}@+1"),
        format!("P-{}@+1", stable.to_ascii_uppercase()),
    ] {
        let o = s.rdy(&["approve", &named, "--strict"]);
        let (ok, stdout, err) = out(&o);
        assert!(!ok, "{named}: --strict refuses after the unrelated landing");
        assert!(err.contains("error E_PROPOSAL_MOVED"), "{named}: {err}");
        assert_eq!(
            stdout,
            format!("{card}\n\n"),
            "{named}: the whole card, shown before the refusal"
        );
        // fix round 2 (finding 2): with --json the card is shown on stderr before the approval
        // runs (stdout stays one JSON value); the error object is stderr's last line
        let o = s.rdy(&["--json", "approve", &named, "--strict"]);
        let (_, stdout, err) = out(&o);
        assert_eq!(stdout, "", "{named}: --json stdout holds no card");
        assert!(
            err.starts_with(&format!("{card}\n\n")),
            "{named}: the whole card on stderr, before the refusal: {err}"
        );
        let e = error_of(&o);
        assert_eq!(e["code"], "E_PROPOSAL_MOVED", "{named}: {e}");
        assert_eq!(
            e["removal_card"],
            json!({"ref": pref, "card": card, "card_hash": item["card_hash"], "removal": item["removal"]}),
            "{named}"
        );
    }
    assert_eq!(landings(&s), before, "nothing landed");
}

/// rdx13 fix round 1 (finding 1): a version the queue does not list (here it has no summary, so
/// it is not ready) still shows its removal card when a person approves it directly.
#[test]
fn rdx13_a_version_the_queue_omits_still_shows_its_card() {
    let (s, id) = secret_served();
    let p = propose_removal(&s, &id, None);
    let pref = p["ref"].as_str().unwrap().to_string();
    let q = op_as(&s, &human(), "pending", json!({"max_tokens": 20000})).unwrap();
    assert!(
        !q["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["ref"] == pref.as_str()),
        "the queue omits {pref}: {q}"
    );
    land_unrelated(&s);
    let before = landings(&s);
    let o = s.rdy(&["approve", &pref, "--strict"]);
    let (ok, stdout, err) = out(&o);
    assert!(!ok && err.contains("error E_PROPOSAL_MOVED"), "{err}");
    for line in [
        "Content removal".to_string(),
        "  Reason class: secret.".to_string(),
        "  Reason: a leaked code".to_string(),
        format!("  {COPIES}"),
        format!("  {QUOTES}"),
        format!("  {REUSE}"),
        format!("  {ROTATE}"),
        IRREVERSIBLE.to_string(),
    ] {
        assert!(stdout.contains(&line), "{line}\n---\n{stdout}");
    }
    assert!(!stdout.contains("ZEBRA"), "{stdout}");
    let e = error_of(&s.rdy(&["--json", "approve", &pref, "--strict"]));
    assert_eq!(e["removal_card"]["ref"], pref.as_str(), "{e}");
    assert!(e["removal_card"]["removal"].is_array(), "{e}");
    assert_eq!(landings(&s), before, "nothing landed");
}

/// Records one secret request for `id` and 17 more whose reasons are 4096 bytes each, through
/// the op, and proposes `work`: the version's removal card does not fit any budget. Returns the
/// version's ref.
fn propose_oversized_removal(s: &Served, id: &str) -> String {
    let long = "r".repeat(4096);
    let record = |reason: &str| {
        op_as(
            s,
            &human(),
            "redact",
            json!({"targets": [id], "reason_class": "secret", "reason": reason}),
        )
        .unwrap()
    };
    record("a leaked code");
    for _ in 0..17 {
        record(&long);
    }
    let (ok, big) = s.json(&[
        "--json",
        "propose",
        "--title",
        "Remove the code",
        "--summary",
        "Removes the leaked code.",
    ]);
    assert!(ok, "{big}");
    big["ref"].as_str().unwrap().to_string()
}

/// rdx13 fix round 1 (finding 4): an approval renders only its own version's card. A removal
/// proposal elsewhere whose card does not fit any budget blocks neither the queue-free approval
/// of an ordinary proposal nor anything but its own approval, which it refuses before running.
#[test]
fn rdx13_an_oversized_card_elsewhere_never_blocks_an_approval() {
    let (s, id) = secret_served();
    let big = propose_oversized_removal(&s, &id);
    let e = op_as(&s, &human(), "pending", json!({"max_tokens": 20000})).unwrap_err();
    assert_eq!(
        e.code, "E_REDACTION_BUDGET",
        "the queue cannot show the card"
    );
    let ordinary = with_rdy(&s, |r| {
        seed_doc(r, "side");
        r.propose(&writer(), "side", "Seed", Some("Adds the seed doc."))
            .unwrap()
            .r#ref
    });
    let before = landings(&s);
    let (ok, v) = s.json(&["--json", "approve", &ordinary]);
    assert!(ok, "the ordinary approval lands: {v}");
    assert_eq!(v["approved"], ordinary.as_str(), "{v}");
    assert!(v.get("removal_card").is_none(), "{v}");
    assert_eq!(landings(&s), before + 1);
    // the oversized card's own approval is refused before it runs
    let before = seq(&s);
    let e = error_of(&s.rdy(&["--json", "approve", &big]));
    assert_eq!(
        (e["code"].as_str(), e["message"].as_str()),
        (
            Some("E_REDACTION_BUDGET"),
            Some("The output budget is too small for the redaction warning.")
        ),
        "{e}"
    );
    assert_eq!(seq(&s), before, "the refusal wrote nothing");
    // fix round 3 (ruling task-13-round2): the show carries the same error as
    // `removal_card_error {code, message, fix}` and no card; the refusal is that error, fix too
    let (ok, v) = s.json(&["--json", "show", &big]);
    assert!(ok, "{v}");
    let p = &v["proposal"];
    assert!(p.get("removal_card").is_none(), "{v}");
    assert_eq!(
        p["removal_card_error"],
        json!({"code": e["code"], "message": e["message"], "fix": e["fix"]}),
        "{v}"
    );
    assert!(!e["fix"].as_str().unwrap_or("").is_empty(), "{e}");
}

/// rdx13 fix round 2 (finding 3): the removal card that cannot be shown stops the approval
/// before it runs. Here stdout is a pipe whose reader is closed, so writing the card fails: the
/// approval is refused with `E_IO` on stderr, not sent (a `--strict` approval that ran would be
/// refused with `E_PROPOSAL_MOVED` after the unrelated landing), and nothing lands.
#[test]
fn rdx13_a_card_that_cannot_be_shown_stops_the_approval() {
    let (s, id) = secret_served();
    let p = propose_removal(&s, &id, Some("Removes the leaked code."));
    let pref = p["ref"].as_str().unwrap().to_string();
    land_unrelated(&s);
    let before = landings(&s);
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let o = rdy_qa::daemon::human_command(
        s.dir.path(),
        &s.env_pairs(),
        &["approve", &pref, "--strict"],
    )
    .stdout(writer)
    .stderr(std::process::Stdio::piped())
    .output()
    .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.starts_with("error E_IO: "),
        "the card write failed: {err}"
    );
    assert!(
        !err.contains("E_PROPOSAL_MOVED"),
        "the approval never ran: {err}"
    );
    assert_eq!(landings(&s), before, "nothing landed");
}

/// rdx13 (13.promoted fix round 1; ruling task-13-round1 item 2): `rdy show P-x@n` as text
/// shows a person what approving the version removes. For an open removal proposal it prints
/// the version's removal card whole, the same text `rdy pending` and `rdy approve` print, and the
/// card hash; for a version whose card cannot be read it prints the error row (code, message and
/// fix) that the JSON show carries and a direct approval refuses with.
#[test]
fn rdx13_show_text_prints_the_removal_card_or_its_error() {
    let (s, id) = secret_served();
    let p = propose_removal(&s, &id, Some("Removes the leaked code."));
    let pref = p["ref"].as_str().unwrap().to_string();
    let q = op_as(&s, &human(), "pending", json!({"max_tokens": 20000})).unwrap();
    let item = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == pref.as_str())
        .unwrap_or_else(|| panic!("{pref}: {q}"))
        .clone();
    let card = item["card"].as_str().unwrap().to_string();
    let card_hash = item["card_hash"].as_str().unwrap().to_string();
    let o = s.rdy(&["show", &pref]);
    let (ok, stdout, err) = out(&o);
    assert!(ok, "{err}");
    assert!(
        stdout.contains(&format!(
            "\n{card}\n\ncard hash: {card_hash}\napprove with: rdy approve {pref}\n"
        )),
        "the whole card and its hash, before the approve line\n---\n{stdout}"
    );
    for line in [
        "Content removal".to_string(),
        "  Reason class: secret.".to_string(),
        "  Reason: a leaked code".to_string(),
        format!("  {COPIES}"),
        format!("  {QUOTES}"),
        format!("  {REUSE}"),
        format!("  {ROTATE}"),
        IRREVERSIBLE.to_string(),
    ] {
        assert!(stdout.contains(&line), "{line}\n---\n{stdout}");
    }
    assert!(!stdout.contains("ZEBRA"), "{stdout}");
    assert!(!stdout.contains("removal card: not read"), "{stdout}");

    // a version whose card does not fit any budget: the error row instead of the card
    let (s, id) = secret_served();
    let big = propose_oversized_removal(&s, &id);
    let (ok, v) = s.json(&["--json", "show", &big]);
    assert!(ok, "{v}");
    let e = &v["proposal"]["removal_card_error"];
    assert_eq!(e["code"], "E_REDACTION_BUDGET", "{v}");
    let o = s.rdy(&["show", &big]);
    let (ok, stdout, err) = out(&o);
    assert!(ok, "{err}");
    assert!(
        stdout.contains(&format!(
            "removal card: not read ({}: {}) (fix: {})\n",
            text(&e["code"]),
            text(&e["message"]),
            text(&e["fix"])
        )),
        "{stdout}"
    );
    assert!(!stdout.contains("Content removal"), "{stdout}");
    assert!(!stdout.contains("card hash:"), "{stdout}");
}

/// `rdy --project <dir> --json edit <id>` as the human, with `EDITOR` a shell script whose body
/// is `body` (it edits the file named by `$1`).
fn edit_with_script(s: &Served, id: &str, body: &str) -> std::process::Output {
    let script = s.dir.path().join("ed.sh");
    std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
    rdy_qa::platform::set_mode(&script, 0o755).unwrap();
    std::process::Command::new(rdy_bin())
        .args(["--project"])
        .arg(s.dir.path())
        .args(["--json", "edit", id])
        .env("EDITOR", &script)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(s.env_pairs())
        .output()
        .unwrap()
}

/// Final review fix round 1, finding 3 (the CLI path; this test lands a removal, unlike the
/// others here): after a person's approved removal, `rdy edit` that replaces the notice with
/// fresh safe text writes a normal version; an edit that saves the notice unchanged is refused
/// with `E_REDACTED`.
#[test]
fn rdx13_an_edit_replaces_the_removal_notice_with_fresh_text() {
    let (s, id) = secret_served();
    with_rdy(&s, |r| {
        let base = r
            .propose(&writer(), "work", "Base", Some(TEST_SUMMARY))
            .unwrap();
        r.land(&approver(), &base.id).unwrap();
        let v = version_of(r, &Scope::Canon, &id);
        let res = r
            .redact_in(
                &writer(),
                "redact",
                RedactInput {
                    targets: vec![format!("{id}@{v}")],
                    reason_class: "secret".into(),
                    reason: "a leaked code".into(),
                    dry_run: false,
                },
            )
            .unwrap();
        assert!(res.redaction.is_some());
        let p = r
            .propose(&writer(), "redact", "Remove", Some("Removes the code."))
            .unwrap();
        r.land(&approver(), &p.id).unwrap();
    });
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["body"], rdy_model::redaction::CONTENT_REMOVED, "{v}");

    // saving the notice unchanged: refused
    let o = edit_with_script(&s, &id, "true\n");
    let e = error_of(&o);
    assert_eq!(e["code"], E_REDACTED, "{e}");

    // the notice replaced with fresh safe text: a normal version
    let o = edit_with_script(
        &s,
        &id,
        "sed -i.bak 's/Content removed\\./Fresh safe text./' \"$1\"\n",
    );
    let (ok, stdout, stderr) = out(&o);
    assert!(ok, "{stdout}\n{stderr}");
    let (ok, v) = s.json(&["--json", "show", &id, "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["body"], "Fresh safe text.", "{v}");
    assert!(!stdout.contains(SECRET) && !stderr.contains(SECRET));
}
