//! rdx12: `rdy_redact {targets, reason_class, reason, dry_run?, view?, max_tokens?}` gives an
//! agent the redaction dry run and request (D-6B1W1: a report "Before a redaction is proposed";
//! D-KQ4CA: the chat path first). The reply is the engine's complete result, as the daemon's
//! `redact` op gives it, plus the shared text. These flows stop before propose and approve: the
//! end-to-end canary owns the approval flow.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::engine::Local;
use rdy_engine::redaction::{RedactResult, report_text};
use rdy_engine::{Rdy, Session};
use rdy_qa::fixtures::{project, write};
use rdy_qa::golden::{approver, land};
use rdy_qa::mcp::{McpClient, Reply};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// The secret the requests select.
const SECRET: &str = "ZEBRA-7731";

/// A project whose canon holds a doc with a secret prose block, a block that quotes the secret
/// whole, and a bystander; landed and tagged `v1.0.0`. Returns the secret block's id.
fn secret_project() -> (tempfile::TempDir, String) {
    let (dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Launch", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": SECRET},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$d", "body": format!("Never say {SECRET} aloud.")},
            {"op": "create", "type": "prose", "id": "$b", "parent": "$d", "body": "An unrelated paragraph."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let id = w.created["$p"].to_string();
    r.close().unwrap();
    (dir, id)
}

/// The daemon `redact` op's reply to `args`, for the agent session `agent:a/r1`, in process.
fn daemon_reply(dir: &Path, args: Value) -> Value {
    let local = Local::new(Rdy::open(dir).unwrap());
    let s = Session::agent("agent:a".into(), Some("r1".into()), rdy_model::Role::Writer).unwrap();
    let cx = OpContext {
        engine: &local,
        session: &s,
        human: "human:qa",
        cwd: dir,
    };
    let v = dispatch(&cx, "redact", args).unwrap();
    local.close().unwrap();
    v
}

fn session(c: &mut McpClient, actor: &str, run: &str, role: &str) {
    c.call(
        "rdy_session",
        json!({"actor": actor, "run": run, "role": role}),
    )
    .ok();
}

/// The code, message and fix of a tool error.
fn refused(r: Reply) -> (String, String, String) {
    let Reply::ToolError(e) = r else {
        panic!("expected a tool error, got {r:?}")
    };
    let s = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    (s("code"), s("message"), s("fix"))
}

/// Whether any segment file of the project at `dir` holds `needle`.
fn on_disk(dir: &Path, needle: &str) -> bool {
    let segs = dir.join(".rdy/store/segments");
    std::fs::read_dir(segs).unwrap().any(|e| {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    })
}

/// The result inside a reply: the reply's own result keys, without the envelope and the text.
fn result_of(v: &Value) -> RedactResult {
    serde_json::from_value(json!({
        "dry_run": v["dry_run"], "reason_class": v["reason_class"], "reason": v["reason"],
        "change": v["change"], "redaction": v["redaction"], "report": v["report"],
        "warnings": v["warnings"],
    }))
    .unwrap()
}

fn close(mut c: McpClient) {
    c.close_stdin();
    let (code, _) = c.wait_exit(Duration::from_secs(30));
    assert_eq!(code, Some(0), "the server exits cleanly");
}

#[test]
fn rdx12_chat_can_report_record_and_show_the_request() {
    let (dir, id) = secret_project();
    let args = json!({"targets": [id], "reason_class": "secret", "reason": "a leaked code", "dry_run": true});
    let daemon = daemon_reply(dir.path(), args.clone());

    let mut c = McpClient::start(dir.path());
    // the instructions say how: dry run, show, record, propose; a person approves
    let i = c.initialize_result()["result"]["instructions"]
        .as_str()
        .unwrap();
    assert!(
        i.contains("Before proposing content removal, call rdy_redact with dry_run true. Show its report and warnings. Record the request, then use rdy_propose and rdy_pending. Only a person can approve it."),
        "{i}"
    );
    // the tool is listed with its schema
    let tools = c.tools();
    let t = tools
        .iter()
        .find(|t| t["name"] == "rdy_redact")
        .expect("rdy_redact is listed");
    assert_eq!(
        t["inputSchema"]["required"],
        json!(["targets", "reason_class", "reason"])
    );
    session(&mut c, "agent:a", "r1", "writer");

    // the dry run: exactly the daemon op's reply, and nothing written
    let dry = c.call("rdy_redact", args).ok();
    assert_eq!(dry, daemon, "the tool and the daemon op give one reply");
    assert_eq!(dry["dry_run"], true);
    assert!(dry["change"].is_null() && dry["redaction"].is_null());
    assert_eq!(dry["report"]["assessment"], "not_implemented");
    let text = dry["text"].as_str().unwrap();
    assert_eq!(
        text,
        report_text(&result_of(&dry), 4000).unwrap().text,
        "the shared text"
    );
    assert!(text.starts_with("Redaction dry run\n"), "{text}");
    assert!(text.ends_with("\nNo request was recorded."), "{text}");
    for code in [
        "W_REDACTION_COPIES",
        "W_REDACTION_INVENTORY",
        "W_REDACTION_QUOTES",
        "W_REDACTION_REUSE",
        "W_REDACTION_SECRET",
    ] {
        assert!(text.contains(&format!("\n{code}: ")), "{code}: {text}");
    }
    // the quote of the whole secret in another block is a candidate, never a target
    assert!(
        dry["report"]["quotes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|q| q["quote"] == SECRET && q["kind"] == "block_version"),
        "{dry}"
    );
    assert_eq!(dry["report"]["targets"], json!([{"all_versions": id}]));
    let dry_view = c
        .call("rdy_view", json!({"op": "show", "view": "r1"}))
        .code()
        .map(str::to_string);
    assert!(
        dry_view.is_some(),
        "a dry run creates no view: {dry_view:?}"
    );

    // the request: recorded in the session's view, through the mutation envelope
    let rec = c
        .call(
            "rdy_redact",
            json!({"targets": [id], "reason_class": "secret", "reason": "a leaked code"}),
        )
        .ok();
    assert_eq!(rec["dry_run"], false);
    assert_eq!(rec["report"], daemon["report"], "the report the person saw");
    assert_eq!(rec["warnings"], daemon["warnings"]);
    assert!(
        rec["consequences"].is_array(),
        "the mutation envelope: {rec}"
    );
    let change = rec["change"].as_str().unwrap().to_string();
    let redaction = rec["redaction"].as_str().unwrap().to_string();
    assert_eq!((change.len(), redaction.len()), (64, 64));
    let text = rec["text"].as_str().unwrap();
    assert_eq!(text, report_text(&result_of(&rec), 4000).unwrap().text);
    assert!(text.starts_with("Redaction request\n"), "{text}");
    assert!(
        text.ends_with(&format!(
            "Recorded redaction {redaction} in change {change}.\n\nPropose the view to ask for approval."
        )),
        "{text}"
    );

    // the request shows in the view, and the content still reads: nothing is erased
    let shown = c
        .call("rdy_view", json!({"op": "show", "view": "r1"}))
        .ok()
        .to_string();
    assert!(
        shown.contains(&change[..12]),
        "the carrier is in r1: {shown}"
    );
    let read = c
        .call(
            "rdy_read",
            json!({"id": id, "scope": "canon", "format": "markdown"}),
        )
        .ok();
    assert!(
        read["text"].as_str().unwrap().contains(SECRET),
        "the original still reads: {read}"
    );
    close(c);
    assert!(on_disk(dir.path(), SECRET), "the original bytes remain");

    // the stored request binds the report the tool showed
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let req = st
        .reader()
        .redaction_request(&redaction.parse().unwrap())
        .unwrap();
    assert_eq!(req.carrier.to_string(), change);
    assert_eq!(req.reason, "a leaked code");
    assert_eq!(req.actor.actor, "agent:a");
    let mut buf = rdy_model::redaction::record_buffer().unwrap();
    assert_eq!(
        req.manifest.report_hash(&mut buf).unwrap().to_string(),
        daemon["report"]["report_hash"].as_str().unwrap()
    );
    assert!(st.included("r1").contains(&req.carrier));
    drop(st);
    r.close().unwrap();
}

#[test]
fn rdx12_reader_dry_run_and_owned_view_rules_are_preserved() {
    let (dir, id) = secret_project();
    let mut c = McpClient::start(dir.path());
    let dry =
        json!({"targets": [id], "reason_class": "personal", "reason": "a name", "dry_run": true});
    let rec = json!({"targets": [id], "reason_class": "personal", "reason": "a name"});
    // no session: refused, even for a dry run
    let (code, ..) = refused(c.call("rdy_redact", dry.clone()));
    assert_eq!(code, "E_NO_SESSION");

    // a reader may run the dry run, never record
    session(&mut c, "agent:r", "look", "reader");
    let v = c.call("rdy_redact", dry.clone()).ok();
    assert_eq!(v["dry_run"], true);
    let (code, message, _) = refused(c.call("rdy_redact", rec.clone()));
    assert_eq!(code, "E_PERMISSION", "{message}");

    // a writer records in its own view; the first request creates and claims it
    session(&mut c, "agent:a", "r1", "writer");
    let v = c.call("rdy_redact", rec.clone()).ok();
    assert!(v["change"].is_string(), "{v}");
    // never in the human's work view
    let mut w = rec.clone();
    w["view"] = json!("work");
    let (code, ..) = refused(c.call("rdy_redact", w));
    assert_eq!(code, "E_VIEW_OWNER");
    // another agent may not record in r1, by either spelling; the existing ownership text
    session(&mut c, "agent:b", "r2", "writer");
    for view in ["r1", "view:r1"] {
        let mut w = rec.clone();
        w["view"] = json!(view);
        let (code, message, _) = refused(c.call("rdy_redact", w));
        assert_eq!(code, "E_VIEW_OWNER", "{view}");
        assert_eq!(message, "view \"r1\" is owned by agent:a/r1", "{view}");
    }
    // a dry run names a view only for the record: its report scans the whole store
    let mut d = dry.clone();
    d["view"] = json!("r1");
    let v2 = c.call("rdy_redact", d).ok();
    assert_eq!(v2["dry_run"], true);
    // its own view works by either spelling
    let mut w = rec.clone();
    w["view"] = json!("view:r2");
    let v = c.call("rdy_redact", w).ok();
    let ch = v["change"].as_str().unwrap().to_string();
    let shown = c
        .call("rdy_view", json!({"op": "show", "view": "r2"}))
        .ok()
        .to_string();
    assert!(shown.contains(&ch[..12]), "{shown}");
    c.alive();
}

#[test]
fn rdx12_budgets_never_change_the_manifest() {
    let (dir, id) = secret_project();
    let mut c = McpClient::start(dir.path());
    session(&mut c, "agent:a", "r1", "writer");
    let dry = |n: usize| json!({"targets": [id], "reason_class": "secret", "reason": "x", "dry_run": true, "max_tokens": n});
    let full = c.call("rdy_redact", dry(20000)).ok();
    assert!(full.get("truncated").is_none(), "{full}");
    let result = result_of(&full);
    let need = full["text"].as_str().unwrap().len() / 4;
    // every budget gives what the shared presentation gives, and the report never changes
    let mut truncated = 0;
    for n in (256..need).rev().step_by(16).chain([256, need - 1, need]) {
        let want = report_text(&result, n);
        match c.call("rdy_redact", dry(n)) {
            Reply::Ok(v) => {
                let want = want.unwrap();
                assert_eq!(v["text"], want.text, "{n}");
                assert_eq!(v.get("truncated").is_some(), want.truncated, "{n}");
                assert_eq!(v["report"], full["report"], "{n}: the report is complete");
                assert_eq!(v["warnings"], full["warnings"], "{n}");
                let text = v["text"].as_str().unwrap();
                for w in full["warnings"].as_array().unwrap() {
                    let line = format!(
                        "{}: {}",
                        w["code"].as_str().unwrap(),
                        w["message"].as_str().unwrap()
                    );
                    assert!(
                        text.contains(&line),
                        "{n}: a warning is never dropped: {line}"
                    );
                }
                if want.truncated {
                    truncated += 1;
                    assert!(
                        text.contains("The row list was omitted. Increase max_tokens to read it.")
                    );
                    assert!(text.len() / 4 <= n, "{n}: within the budget");
                }
            }
            other => {
                let (code, ..) = refused(other);
                assert_eq!(code, "E_REDACTION_BUDGET", "{n}");
                assert_eq!(want.unwrap_err().code, "E_REDACTION_BUDGET", "{n}");
            }
        }
    }
    assert!(truncated > 0, "some budget shortens the rows");
    // out of range: refused before any report
    for n in [255, 20001] {
        let (code, message, _) = refused(c.call("rdy_redact", dry(n)));
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("E_BAD_INPUT", "max_tokens must be from 256 to 20000.")
        );
    }
    // a request recorded under a short budget binds the same manifest
    let small = (256..need)
        .rev()
        .find(|n| report_text(&result, *n).is_ok_and(|t| t.truncated))
        .unwrap();
    let rec = c
        .call(
            "rdy_redact",
            json!({"targets": [id], "reason_class": "secret", "reason": "x", "max_tokens": small}),
        )
        .ok();
    assert_eq!(rec["truncated"], true, "{rec}");
    assert_eq!(rec["report"], full["report"]);
    assert_eq!(rec["report"]["report_hash"], full["report"]["report_hash"]);
    c.alive();
}
