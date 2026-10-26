//! ok10 (D-EHC2C, D-VVYJZ, D-KQ4CA): replay is one view operation on every surface. `rdy_view
//! {op: "replay", view?, changes, max_tokens?}` over MCP and `rdy view replay <view> <change>...`
//! at the command line both go through the shared `ViewRequest` dispatcher and return one JSON
//! shape: the replay result and the consequences. `view show` names each change's replay source.
//! A refused replay writes nothing.
use rdy_daemon::{OpContext, dispatch};
use rdy_engine::consequence::CONFLICT_FIX;
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::segment::Location;
use serde_json::{Value, json};

/// The ids a replay test needs.
struct Ids {
    /// The seed claim, which b updates and the divergent views also update.
    claim: String,
    /// b's update of the claim, and b's new claim.
    cx: String,
    cy: String,
    /// The claim's version in canon: the base of `cx`.
    base: String,
    /// The change each divergent view holds (a's own update of the claim), by view.
    own: Vec<(String, String)>,
}

impl Ids {
    fn own_of(&self, view: &str) -> &str {
        &self
            .own
            .iter()
            .find(|(v, _)| v == view)
            .unwrap_or_else(|| panic!("no divergent view {view}"))
            .1
    }
}

/// The views of the conflict test: each holds a's own update of the seed claim.
const DIVERGED: [&str; 3] = ["div-cli", "div-mcp", "div-text"];

fn claim(parent: &str, body: &str) -> Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

/// A served project: the seed doc landed; view `src` with agent b's two changes (an update of the
/// seed claim, and a new claim); agent a's views `plain` (each holding a claim of its own, so a
/// view exists) and `diverged` (each updating the seed claim).
fn served_replay(plain: &[&str], diverged: &[&str]) -> (Served, Ids) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let base = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&BlockId::parse(&c).unwrap())
        .unwrap()
        .to_string();
    let b = as_agent("agent:b", "rb");
    let cx = write_as(
        &mut r,
        &b,
        "src",
        json!([{"op": "update", "id": c, "body": "two"}]),
    )
    .unwrap()
    .change;
    let cy = write_as(&mut r, &b, "src", claim(&section, "extra"))
        .unwrap()
        .change;
    let a = as_agent("agent:a", "r1");
    for v in plain {
        write_as(&mut r, &a, v, claim(&section, &format!("own in {v}"))).unwrap();
    }
    let mut own = vec![];
    for v in diverged {
        let change = write_as(
            &mut r,
            &a,
            v,
            json!([{"op": "update", "id": c, "body": "three"}]),
        )
        .unwrap()
        .change;
        own.push((v.to_string(), change));
    }
    r.close().unwrap();
    (
        s,
        Ids {
            claim: c,
            cx,
            cy,
            base,
            own,
        },
    )
}

const A: &str = "agent:a";
const RUN: &str = "r1";

fn cli(s: &Served, args: &[&str]) -> (bool, Value) {
    s.json_as(A, Some(RUN), args)
}

fn cli_text(s: &Served, args: &[&str]) -> (bool, String) {
    let out = s.rdy_as(A, Some(RUN), args);
    let ok = out.status.success();
    let bytes = if ok { out.stdout } else { out.stderr };
    (ok, String::from_utf8(bytes).unwrap())
}

fn mcp(s: &Served) -> McpClient {
    let mut c = McpClient::start_proxy(s);
    c.call(
        "rdy_session",
        json!({"actor": A, "run": RUN, "role": "writer"}),
    )
    .ok();
    c
}

fn tool_error(r: Reply) -> Value {
    match r {
        Reply::ToolError(v) => v,
        other => panic!("expected an rdy tool error, got {other:?}"),
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `v` with the parts that differ between two equal replays masked: `sources` get their label,
/// every other hash is `<hash>`, a consequence id is `<cq>`, a tension id `<tn>`, another block id
/// `<block>` and `view` is `<view>`.
fn norm(v: &Value, view: &str, sources: &[(&str, &str)]) -> Value {
    match v {
        Value::String(s) => {
            if let Some((_, label)) = sources.iter().find(|(h, _)| h == s) {
                json!(label)
            } else if is_hash(s) {
                json!("<hash>")
            } else if s.starts_with("cq-") {
                json!("<cq>")
            } else if s.starts_with("tn-") {
                json!("<tn>")
            } else if s.len() == 7 && s.starts_with("c-") {
                json!("<block>")
            } else if s == view {
                json!("<view>")
            } else {
                v.clone()
            }
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| norm(x, view, sources)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), norm(x, view, sources)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("not a list: {v}"))
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// The newest op of view `view`, read through the CLI.
fn newest_op(s: &Served, view: &str) -> Value {
    let (ok, v) = cli(s, &["--json", "view", "show", view]);
    assert!(ok, "{v}");
    v["ops"][0].clone()
}

/// The end of the project's log, with the daemon stopped.
fn log_end(s: &Served) -> Location {
    s.stop();
    let r = Rdy::open(s.dir.path()).unwrap();
    let end = r.store().log_end();
    r.close().unwrap();
    end
}

const REPLAY_KEYS: [&str; 10] = [
    "view",
    "head",
    "changes",
    "replayed",
    "added",
    "added_deps",
    "removed",
    "cascaded",
    "conflicts",
    "consequences",
];

#[test]
fn ok10_mcp_and_cli_replay_match() {
    // two equal projects: a replay into a second view would collide with the first
    let (s_cli, ids_cli) = served_replay(&["cli"], &[]);
    let (s_mcp, ids_mcp) = served_replay(&["mcp"], &[]);
    let (ok, from_cli) = cli(
        &s_cli,
        &["--json", "view", "replay", "cli", &ids_cli.cx, &ids_cli.cy],
    );
    assert!(ok, "{from_cli}");
    let mut c = mcp(&s_mcp);
    let from_mcp = c
        .call(
            "rdy_view",
            json!({"op": "replay", "view": "mcp", "changes": [ids_mcp.cx, ids_mcp.cy]}),
        )
        .ok();
    for (reply, view, s, ids) in [
        (&from_cli, "cli", &s_cli, &ids_cli),
        (&from_mcp, "mcp", &s_mcp, &ids_mcp),
    ] {
        for key in REPLAY_KEYS {
            assert!(reply.get(key).is_some(), "{view}: no {key}: {reply}");
        }
        assert_eq!(reply["view"], view);
        // the request reached the engine as the replay of exactly these two sources
        let replayed = reply["replayed"].as_array().unwrap();
        assert_eq!(
            sorted(
                replayed
                    .iter()
                    .map(|r| r["source"].as_str().unwrap().to_string())
                    .collect()
            ),
            sorted(vec![ids.cx.clone(), ids.cy.clone()]),
            "{reply}"
        );
        assert_eq!(
            sorted(strings(&reply["added"])),
            sorted(
                replayed
                    .iter()
                    .map(|r| r["change"].as_str().unwrap().to_string())
                    .collect()
            ),
            "added holds every new change: {reply}"
        );
        assert_eq!(reply["conflicts"], json!([]), "{reply}");
        assert!(reply["consequences"].is_array(), "{reply}");
        // the view's head is the one include op the reply names
        let op = newest_op(s, view);
        assert_eq!(op["id"], reply["head"], "{view}: {op}");
        assert_eq!(op["kind"], "include", "{view}: {op}");
        // and the view holds the replays of both sources, once each, beside the change it had
        let (ok, shown) = cli(s, &["--json", "view", "show", view]);
        assert!(ok, "{shown}");
        let from: Vec<String> = shown["changes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["replayed_from"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            sorted(from),
            sorted(vec![ids.cx.clone(), ids.cy.clone()]),
            "{shown}"
        );
        assert_eq!(shown["head"]["changes"], 3, "{shown}");
    }
    // one shape: the two replies differ only in ids that depend on the project and the time
    let masked = |reply: &Value, view: &str, ids: &Ids| {
        norm(
            reply,
            view,
            &[
                (ids.cx.as_str(), "<cx>"),
                (ids.cy.as_str(), "<cy>"),
                (ids.claim.as_str(), "<claim>"),
            ],
        )
    };
    assert_eq!(
        masked(&from_cli, "cli", &ids_cli),
        masked(&from_mcp, "mcp", &ids_mcp)
    );
}

#[test]
fn ok10_replay_reply_has_consequences_and_conflicts() {
    let (s, ids) = served_replay(&[], &DIVERGED);
    let (ok, reply) = cli(&s, &["--json", "view", "replay", "div-cli", &ids.cx]);
    assert!(ok, "{reply}");
    let new = reply["replayed"][0]["change"].as_str().unwrap().to_string();
    let conflicts = reply["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{reply}");
    let conflict = &conflicts[0];
    // the recorded conflict keeps the source's base and names both sides
    assert_eq!(conflict["block"], ids.claim, "{conflict}");
    assert_eq!(conflict["base"], ids.base, "{conflict}");
    let sides = conflict["sides"].as_array().unwrap();
    assert_eq!(sides.len(), 2, "{conflict}");
    let changes = sorted(
        sides
            .iter()
            .map(|x| x["change"].as_str().unwrap().to_string())
            .collect(),
    );
    assert_eq!(
        changes,
        sorted(vec![ids.own_of("div-cli").to_string(), new.clone()])
    );
    assert!(
        sides
            .iter()
            .all(|x| is_hash(x["version"].as_str().unwrap()))
    );
    // the consequences list it to resolve
    let entry = reply["consequences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "conflict")
        .unwrap_or_else(|| panic!("no conflict consequence: {reply}"));
    assert_eq!(entry["block"], ids.claim);
    assert_eq!(entry["conflict"], conflict["id"]);
    assert_eq!(entry["follow_up"], "resolve");
    assert_eq!(entry["fix"], CONFLICT_FIX);
    // MCP: the same conflict from a later read, and a replay into another view that agrees
    let mut c = mcp(&s);
    let from_mcp = c
        .call(
            "rdy_view",
            json!({"op": "replay", "view": "div-mcp", "changes": [ids.cx]}),
        )
        .ok();
    assert_eq!(from_mcp["conflicts"][0]["base"], ids.base, "{from_mcp}");
    assert_eq!(
        from_mcp["conflicts"][0]["sides"].as_array().unwrap().len(),
        2
    );
    for (view, want) in [("div-cli", &reply), ("div-mcp", &from_mcp)] {
        let (ok, shown) = cli(&s, &["--json", "view", "show", view]);
        assert!(ok, "{shown}");
        assert_eq!(shown["conflicts"], want["conflicts"], "{view}: {shown}");
        let shown_mcp = c.call("rdy_view", json!({"op": "show", "view": view})).ok();
        assert_eq!(shown_mcp["conflicts"], want["conflicts"], "{view}");
    }
    // the command line's text: the heading, the conflict lines, then the consequences
    let (ok, text) = cli_text(&s, &["view", "replay", "div-text", &ids.cx]);
    assert!(ok, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "replayed 1 change(s) into div-text", "{text}");
    assert_eq!(
        lines[1],
        format!("conflict {}: 2 sides", ids.claim),
        "{text}"
    );
    assert_eq!(lines[2], "consequences:", "{text}");
    assert!(
        lines[3].starts_with(&format!("  conflict {}: resolve", ids.claim)),
        "{text}"
    );
    assert!(text.contains(&format!("fix: {CONFLICT_FIX}")), "{text}");
}

#[test]
fn ok10_replay_then_undo_is_one_act() {
    let (s, ids) = served_replay(&["undo-cli", "undo-mcp"], &[]);
    let both = [ids.cx.as_str(), ids.cy.as_str()];
    let mut c = mcp(&s);
    // the command line
    let (ok, replay) = cli(
        &s,
        &["--json", "view", "replay", "undo-cli", &ids.cx, &ids.cy],
    );
    assert!(ok, "{replay}");
    let (ok, undo) = cli(&s, &["--json", "undo", "--view", "undo-cli"]);
    assert!(ok, "{undo}");
    // MCP
    let replay_mcp = c
        .call(
            "rdy_view",
            json!({"op": "replay", "view": "undo-mcp", "changes": both}),
        )
        .ok();
    let undo_mcp = c
        .call("rdy_view", json!({"op": "undo", "view": "undo-mcp"}))
        .ok();
    for (view, replay, undo) in [
        ("undo-cli", &replay, &undo),
        ("undo-mcp", &replay_mcp, &undo_mcp),
    ] {
        // one undo: it targets the replay's op and takes out both new changes
        assert_eq!(undo["kind"], "include", "{view}: {undo}");
        assert_eq!(undo["undone"], replay["head"], "{view}: {undo}");
        assert_eq!(
            sorted(strings(&undo["excluded"])),
            sorted(strings(&replay["added"])),
            "{view}: {undo}"
        );
        let (ok, shown) = cli(&s, &["--json", "view", "show", view]);
        assert!(ok, "{shown}");
        let ids_now: Vec<String> = shown["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect();
        assert!(
            replay["added"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| !ids_now.contains(&a.as_str().unwrap().to_string())),
            "{view}: {shown}"
        );
        assert_eq!(
            shown["head"]["changes"], 1,
            "only the view's own change: {shown}"
        );
        let kinds: Vec<&str> = shown["ops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == "undo").count(),
            1,
            "{view}: one undo op: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == "include").count(),
            1,
            "{view}: one include op, the replay: {kinds:?}"
        );
    }
}

#[test]
fn ok10_replay_rejects_extra_fields_and_limits() {
    let (s, ids) = served_replay(&["cli"], &[]);
    let end = log_end(&s);
    let mut c = mcp(&s);
    // a field replay does not take, over MCP; the first in declaration order is named
    for (extra, field) in [
        (json!({"cascade": true}), "cascade"),
        (json!({"name": "n"}), "name"),
        (
            json!({"cascade": true, "name": "n", "evidence": "w"}),
            "name",
        ),
        (json!({"reason": "r", "target": "t"}), "reason"),
        (
            json!({"relayed_by": "agent:a", "evidence": "w"}),
            "evidence",
        ),
    ] {
        let mut req = json!({"op": "replay", "view": "cli", "changes": [ids.cx]});
        req.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let e = tool_error(c.call("rdy_view", req));
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        assert_eq!(e["message"], format!("replay does not take {field}"), "{e}");
        assert_eq!(
            e["fix"],
            format!("drop {field}, or use the view operation that takes it"),
            "{e}"
        );
    }
    // an unknown key is a did-you-mean first, before any operation rule
    let e = tool_error(c.call(
        "rdy_view",
        json!({"op": "replay", "view": "cli", "chnages": [ids.cx], "cascade": true}),
    ));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(
        e["message"], "rdy_view: unknown parameter \"chnages\"",
        "{e}"
    );
    assert!(
        e["fix"]
            .as_str()
            .unwrap()
            .starts_with("did you mean \"changes\"? the parameters here are "),
        "{e}"
    );
    // the daemon's op for the command line says the same
    let (dir, r) = project();
    let local = Local::new(r);
    let me = as_agent("agent:a", "r1");
    let cx = OpContext {
        engine: &local,
        session: &me,
        human: "human:qa",
        cwd: dir.path(),
        agent: &rdy_engine::signing_crypto::UnavailableAgent,
    };
    let err = dispatch(
        &cx,
        "view",
        json!({"op": "replay", "view": "cli", "chnages": ["x"]}),
    )
    .unwrap_err();
    assert_eq!(err.message, "view: unknown field \"chnages\"", "{err}");
    assert!(
        err.fix
            .starts_with("did you mean \"changes\"? the fields are ")
    );
    // changes is required and not empty, on every surface, with one text
    let (text, fix) = (
        "name at least one change to replay",
        "call rdy_view with op replay and changes, or rdy view replay <view> <change>...",
    );
    for req in [
        json!({"op": "replay", "view": "cli"}),
        json!({"op": "replay", "view": "cli", "changes": []}),
    ] {
        let e = tool_error(c.call("rdy_view", req));
        assert_eq!(
            (e["code"].as_str(), e["message"].as_str(), e["fix"].as_str()),
            (Some("E_BAD_INPUT"), Some(text), Some(fix)),
            "{e}"
        );
    }
    let (ok, out) = cli_text(&s, &["view", "replay", "cli"]);
    assert!(!ok, "the command line requires a change: {out}");
    assert!(out.contains("required"), "{out}");
    // limits: too many changes, and a token budget over the range
    let many: Vec<&str> = vec![ids.cx.as_str(); 1001];
    let e = tool_error(c.call(
        "rdy_view",
        json!({"op": "replay", "view": "cli", "changes": many}),
    ));
    assert_eq!(e["code"], "E_LIMIT", "{e}");
    let mut args = vec!["--json", "view", "replay", "cli"];
    args.extend(many.iter());
    let (ok, e) = cli(&s, &args);
    assert!(!ok, "{e}");
    assert_eq!(e["code"], "E_LIMIT", "{e}");
    let e = tool_error(c.call(
        "rdy_view",
        json!({"op": "replay", "view": "cli", "changes": [ids.cx], "max_tokens": 100_001}),
    ));
    assert_eq!(e["code"], "E_LIMIT", "{e}");
    assert_eq!(
        e["message"], "max_tokens 100001 is over the limit of 100000",
        "{e}"
    );
    // the help is one sentence
    let (ok, help) = cli_text(&s, &["view", "replay", "--help"]);
    assert!(ok, "{help}");
    assert_eq!(
        help.lines().next(),
        Some("Replay selected changes into your open view."),
        "{help}"
    );
    // and nothing was written
    drop(c);
    assert_eq!(log_end(&s), end);
}

#[test]
fn ok10_view_show_names_replay_source() {
    let (s, ids) = served_replay(&["show"], &[]);
    let (ok, replay) = cli(&s, &["--json", "view", "replay", "show", &ids.cx]);
    assert!(ok, "{replay}");
    let new = replay["replayed"][0]["change"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, shown) = cli(&s, &["--json", "view", "show", "show"]);
    assert!(ok, "{shown}");
    let rows = shown["changes"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{shown}");
    for row in rows {
        let from = row
            .get("replayed_from")
            .unwrap_or_else(|| panic!("no replayed_from: {row}"));
        if row["id"] == new {
            assert_eq!(*from, json!(ids.cx), "a full source hash: {row}");
        } else {
            assert_eq!(*from, Value::Null, "{row}");
        }
    }
    // MCP reads the same rows
    let mut c = mcp(&s);
    let shown_mcp = c
        .call("rdy_view", json!({"op": "show", "view": "show"}))
        .ok();
    assert_eq!(shown_mcp["changes"], shown["changes"]);
    // the text: the replay's heading ends with its source; any other change's heading is as before
    let (ok, text) = cli_text(&s, &["view", "show", "show"]);
    assert!(ok, "{text}");
    let heading = |id: &str| {
        text.lines()
            .find(|l| l.starts_with(&format!("  {} ", &id[..12])))
            .unwrap_or_else(|| panic!("no heading for {id}: {text}"))
            .to_string()
    };
    let h = heading(&new);
    assert!(
        h.ends_with(&format!(" replayed from {}", &ids.cx[..12])),
        "{h}"
    );
    let own = rows.iter().find(|r| r["id"] != new).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!heading(&own).contains("replayed from"), "{text}");
    // the stored op kind stays include
    assert_eq!(shown["ops"][0]["kind"], "include", "{shown}");
    assert!(text.contains(" include "), "{text}");
}

#[test]
fn ok10_failed_replay_does_not_publish() {
    let (s, ids) = served_replay(&["cli"], &[]);
    let ghost = "ab".repeat(32);
    let other = |args: &[&str]| s.json_as("agent:c", Some("r1"), args);
    // warm up each session, so the log stops growing for reasons of its own
    let mut c = mcp(&s);
    let (ok, before_cli) = cli(&s, &["--json", "view", "show", "cli"]);
    assert!(ok, "{before_cli}");
    let (ok, before_src) = cli(&s, &["--json", "view", "show", "src"]);
    assert!(ok, "{before_src}");
    assert!(other(&["--json", "view", "show", "cli"]).0);
    c.call("rdy_view", json!({"op": "show", "view": "cli"}))
        .ok();
    drop(c);
    let end = log_end(&s);
    // each refusal, then the log: it ends where it did
    let step = |label: &str, run: &dyn Fn() -> Option<Value>| {
        let refused = run();
        if let Some(e) = refused {
            assert!(e["code"].is_string(), "{label}: {e}");
        }
        assert_eq!(log_end(&s), end, "{label}: a frame was written");
    };
    // one real change and one that is not there: the whole call fails
    step("ghost, command line", &|| {
        let (ok, e) = cli(&s, &["--json", "view", "replay", "cli", &ids.cx, &ghost]);
        assert!(!ok, "{e}");
        Some(e)
    });
    step("ghost, MCP", &|| {
        let mut c = mcp(&s);
        Some(tool_error(c.call(
            "rdy_view",
            json!({"op": "replay", "view": "cli", "changes": [ids.cx, ghost]}),
        )))
    });
    // a view of another actor
    step("foreign view", &|| {
        let (ok, e) = other(&["--json", "view", "replay", "cli", &ids.cx]);
        assert!(!ok, "{e}");
        assert_eq!(e["code"], "E_VIEW_OWNER", "{e}");
        Some(e)
    });
    // a field replay does not take
    step("extra field", &|| {
        let mut c = mcp(&s);
        let e = tool_error(c.call(
            "rdy_view",
            json!({"op": "replay", "view": "cli", "changes": [ids.cx], "cascade": true}),
        ));
        assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
        Some(e)
    });
    // a view that is not there
    step("no view", &|| {
        let (ok, e) = cli(&s, &["--json", "view", "replay", "nope", &ids.cx]);
        assert!(!ok, "{e}");
        Some(e)
    });
    let (_, after_cli) = cli(&s, &["--json", "view", "show", "cli"]);
    let (_, after_src) = cli(&s, &["--json", "view", "show", "src"]);
    assert_eq!(after_cli, before_cli);
    assert_eq!(after_src, before_src);
}
