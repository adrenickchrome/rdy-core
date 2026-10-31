//! sg7: golden MCP replies for every M2 tool (spec §15.2). One scripted session runs in-process
//! on a project with seeded ids and a pinned clock, so every id, hash and time in a reply is
//! fixed. Each reply is compared with `tests/golden/mcp/<tool>--<case>.json`. Bless with
//! `RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test mcp sg7_`.
use rdy_engine::engine::Local;
use rdy_engine::{RdyError, Scope, pin_clock};
use rdy_mcp::alt_tools::rdy_alternatives;
use rdy_mcp::intent_tools::rdy_intent;
use rdy_mcp::tools::*;
use rdy_mcp::view_tools::{rdy_log, rdy_view};
use rdy_qa::edit_loop::stable_spec;
use rdy_qa::fixtures::{land, write};
use rdy_qa::inbox::{landed_claim, on};
use rdy_qa::mcp::McpClient;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::upd;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The two lists a child `rdy mcp` serves with no ids or times.
const STATIC_LISTS: [&str; 2] = ["resources--list", "resources--templates"];

/// Every M2 tool the goldens must name.
const M2_TOOLS: [&str; 15] = [
    "rdy_session",
    "rdy_brief",
    "rdy_write",
    "rdy_view",
    "rdy_intent",
    "rdy_alternatives",
    "rdy_propose",
    "rdy_tensions",
    "rdy_resolve",
    "rdy_log",
    "rdy_coverage",
    "rdy_inbox",
    "rdy_review_next",
    "rdy_review_submit",
    "rdy_review",
];

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/mcp")
}

fn pretty(v: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(v).unwrap())
}

/// Unseeds this thread's ids when the script ends, also when it panics.
struct Unseed;
impl Drop for Unseed {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

/// The replies of the script, in the order they were made: `(name, pretty JSON)`.
#[derive(Default)]
struct Replies(Vec<(String, String)>);

impl Replies {
    fn push(&mut self, name: &str, v: &Value) {
        assert!(
            self.0.iter().all(|(n, _)| n != name),
            "{name} is recorded twice"
        );
        let text = pretty(v);
        // a NUL byte marks text the mention scan masked: it must never reach a reply
        assert!(!text.contains("\\u0000"), "{name} holds a NUL byte");
        self.0.push((name.to_string(), text));
    }
    /// Records the reply of a call that must succeed.
    fn ok(&mut self, name: &str, r: Result<Value, RdyError>) -> Value {
        let v = r.unwrap_or_else(|e| panic!("{name}: {}", error_json(&e)));
        self.push(name, &v);
        v
    }
    /// Records the reply of a call that must be refused, as the server renders the error.
    fn refused(&mut self, name: &str, r: Result<Value, RdyError>) -> Value {
        let e = r.expect_err(name);
        let v = error_json(&e);
        self.push(name, &v);
        v
    }
}

/// An unrecorded call that must succeed.
fn must(what: &str, r: Result<Value, RdyError>) -> Value {
    r.unwrap_or_else(|e| panic!("{what}: {}", error_json(&e)))
}

fn p<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn session(eng: &Arc<Local>, actor: &str, run: &str, role: &str, areas: Option<Value>) -> McpState {
    let mut st = McpState::new(eng.clone());
    let mut params = json!({"actor": actor, "run": run, "role": role});
    if let Some(a) = areas {
        params["areas"] = a;
    }
    must("session", rdy_session(&mut st, p(params)));
    st
}

fn str_of(v: &Value, what: &str) -> String {
    v.as_str()
        .unwrap_or_else(|| panic!("{what} is not a string in {v}"))
        .to_string()
}

/// The text of the QA pack's block B as a review case shows it: its reference is its bare id.
const REF_TEXT_PREFIX: &str = "B builds on ";

/// The case block that holds a reference, if the request has one.
fn ref_block(request: &Value) -> Option<&Value> {
    request["case"]["blocks"].as_array()?.iter().find(|x| {
        x["text"].as_array().is_some_and(|ts| {
            ts.iter()
                .any(|t| str_of_opt(&t["text"]).starts_with(REF_TEXT_PREFIX))
        })
    })
}

fn str_of_opt(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// The case shows the block holding a reference as `rdy_read` shows it: every text of the block
/// is in the compact read's text for the block, and none holds a NUL byte.
fn check_case_text_matches_read(st: &mut McpState, request: &Value) {
    let b = ref_block(request).expect("a block with a reference");
    let id = str_of(&b["id"], "the case block id");
    let read = must(
        "rdy_read of the case block",
        rdy_read(st, p(json!({"id": id, "scope": "canon"}))),
    );
    let shown = str_of(&read["text"], "the read's text");
    for t in b["text"].as_array().expect("the block's texts") {
        let text = str_of(&t["text"], "a case text");
        assert!(
            !text.contains('\0'),
            "the case text holds a NUL byte: {text:?}"
        );
        assert!(
            text.starts_with("B builds on c-") && shown.contains(&text),
            "the case text {text:?} is not what rdy_read shows: {shown:?}"
        );
    }
}

/// Runs the scripted session and returns its replies. The human's name is fixed in the project's
/// local config, so the person's inbox does not carry the user name of the machine.
pub(crate) fn run_session() -> Vec<(String, String)> {
    rdy_model::seed_ids_for_tests(0x4D47_0007);
    let _unseed = Unseed;
    let _clock = pin_clock(1_760_000_000_000);
    let (dir, mut r) = qa_project();
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let spec = stable_spec(&mut r);
    let fee = landed_claim(&mut r, "payments/fees");
    // two terms of one name in canon: a tension whose sides are all canon
    let doc = r
        .read()
        .resolve_path(&Scope::Canon, "payments/fees")
        .unwrap();
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "term", "parent": doc.to_string(),
             "fields": {"name": "Fee"}, "body": "Fee is what a payment costs."},
            {"op": "create", "type": "term", "parent": doc.to_string(),
             "fields": {"name": "Fee"}, "body": "Fee is a flat charge."}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let eng = Arc::new(Local::new(r));
    let mut out = Replies::default();

    // the four sessions: two writers, the reviewer and the relay
    let mut a = McpState::new(eng.clone());
    let v = rdy_session(
        &mut a,
        p(json!({"actor": "agent:a", "run": "a", "role": "writer", "areas": ["payments"]})),
    );
    out.ok("rdy_session--writer-with-areas", v);
    let mut b = session(&eng, "agent:b", "b", "writer", None);
    let mut rv = session(&eng, "agent:r", "r", "reviewer", None);
    let mut chat = session(&eng, "agent:relay", "chat", "reviewer", None);
    // the relay opens the person's inbox before anything happens to them
    must(
        "the person's inbox opens",
        rdy_inbox(&mut chat, p(json!({"person": true}))),
    );

    // a briefing, a write that cites it, and a write that does not
    let brief = out.ok(
        "rdy_brief--doc",
        rdy_brief(&mut a, p(json!({"path": "spec"}))),
    );
    let token = str_of(&brief["base"]["token"], "the briefing token");
    out.ok(
        "rdy_write--stable-with-brief",
        rdy_write(
            &mut a,
            p(json!({"ops": [{"op": "update", "id": spec.a.to_string(), "body": "A holds, by a."}],
            "brief": token, "message": "a edits A"})),
        ),
    );
    out.refused(
        "rdy_write--refused-brief-required",
        rdy_write(
            &mut a,
            p(json!({"ops": [{"op": "update", "id": spec.c.to_string(), "body": "C, by a."}]})),
        ),
    );
    out.ok(
        "rdy_intent--post",
        rdy_intent(
            &mut a,
            p(json!({"op": "post", "area": "payments", "angle": "fees", "ttl": "30m"})),
        ),
    );

    // the person edits the payments claim in their own view, as the alternatives do
    let claim = fee.clone();
    on(&eng, move |r| {
        write(r, "work", upd(&claim, "Fees are waived.")).unwrap();
    });

    // b changes the same block A in its own view, and says C contradicts B
    let bb = must("b brief", rdy_brief(&mut b, p(json!({"path": "spec"}))));
    let b_token = str_of(&bb["base"]["token"], "b's briefing token");
    let wb = must(
        "b writes A and C",
        rdy_write(
            &mut b,
            p(json!({"ops": [
                {"op": "update", "id": spec.a.to_string(), "body": "A holds, by b."},
                {"op": "update", "id": spec.c.to_string(), "body": "C contradicts B."}],
            "brief": b_token, "message": "b edits A and C"})),
        ),
    );
    let b_change = str_of(&wb["change"], "b's change");

    // views: create, fork, include (a conflict on A), show, exclude, compare, undo
    out.ok(
        "rdy_view--create",
        rdy_view(&mut a, p(json!({"op": "create", "name": "alt-x"}))),
    );
    out.ok(
        "rdy_view--fork",
        rdy_view(
            &mut a,
            p(json!({"op": "fork", "from": "a", "name": "a-fork"})),
        ),
    );
    out.ok(
        "rdy_view--include",
        rdy_view(&mut a, p(json!({"op": "include", "changes": [b_change]}))),
    );
    out.ok(
        "rdy_view--show-with-conflict",
        rdy_view(&mut a, p(json!({"op": "show"}))),
    );
    out.ok(
        "rdy_view--exclude",
        rdy_view(&mut a, p(json!({"op": "exclude", "changes": [b_change]}))),
    );
    out.ok(
        "rdy_view--compare",
        rdy_view(&mut a, p(json!({"op": "compare"}))),
    );
    out.ok("rdy_view--undo", rdy_view(&mut a, p(json!({"op": "undo"}))));

    // alternatives: two open views that word one payments claim in two ways
    must(
        "alt-y",
        rdy_view(&mut a, p(json!({"op": "create", "name": "alt-y"}))),
    );
    for (view, body) in [("alt-x", "Fees are 1%."), ("alt-y", "Fees are 2%.")] {
        must(
            "alt write",
            rdy_write(
                &mut a,
                p(
                    json!({"view": view, "ops": [{"op": "update", "id": fee.to_string(), "body": body}]}),
                ),
            ),
        );
    }
    let alt = out.ok(
        "rdy_alternatives--create",
        rdy_alternatives(
            &mut a,
            p(json!({"op": "create", "title": "Fee wording",
            "members": [{"label": "x", "view": "alt-x"}, {"label": "y", "view": "alt-y"}]})),
        ),
    );
    let set = str_of(&alt["set"]["id"], "the set id");
    out.ok(
        "rdy_alternatives--compare",
        rdy_alternatives(&mut a, p(json!({"op": "compare", "set": set}))),
    );

    // the tensions list shows b's view and canon
    let tv = out.ok(
        "rdy_tensions--view",
        rdy_tensions(&mut b, p(json!({"scope": "b"}))),
    );
    out.ok(
        "rdy_tensions--canon",
        rdy_tensions(&mut b, p(json!({"scope": "canon"}))),
    );
    let collision = tv["tensions"]
        .as_array()
        .and_then(|l| l.iter().find(|t| t["kind"] == "edit_collision"))
        .unwrap_or_else(|| panic!("no edit collision in {tv}"));
    out.ok(
        "rdy_resolve--dismiss",
        rdy_resolve(
            &mut rv,
            p(json!({"tension": collision["id"],
            "dismiss": {"reason": "checked: b's wording is the newer one"}})),
        ),
    );

    // the proposal, the reviewer's advice, the proposal again, and the relayed acceptance
    out.ok(
        "rdy_propose--with-consequences",
        rdy_propose(
            &mut b,
            p(json!({"title": "Fix A and C", "summary": "Rewords A and C."})),
        ),
    );
    // the reviewer takes requests until one asks about b's C against the decision (a request is
    // leased to the reviewer once it is taken, so the others stay with it)
    let mut next = None;
    let mut seen = vec![];
    for _ in 0..16 {
        let n = must(
            "review next",
            rdy_review_next(&mut rv, p(json!({"kind": "tension"}))),
        );
        if n["request"].is_null() {
            break;
        }
        let blocks = n["case"]["blocks"].as_array().expect("the case's blocks");
        let about_c = blocks
            .iter()
            .any(|x| x["id"] == spec.c.to_string() && x["text"][0]["text"] == "C contradicts B.");
        let with_decision = blocks.iter().any(|x| x["type"] == "decision");
        if about_c && with_decision {
            next = Some(n);
            break;
        }
        seen.push(n);
    }
    let next = next.expect("a request about C and the decision");
    // a case whose block holds a reference shows it as rdy_read does (the reference's id), never
    // as the NUL bytes the mention scan masks it with
    seen.push(next.clone());
    let with_ref = seen
        .iter()
        .find(|n| ref_block(n).is_some())
        .unwrap_or_else(|| panic!("no review case holds the block with a reference"));
    out.push("rdy_review_next--request-with-reference", with_ref);
    check_case_text_matches_read(&mut a, with_ref);
    out.push("rdy_review_next--request", &next);
    let block = spec.c.to_string();
    out.ok(
        "rdy_review_submit--tension-with-advice",
        rdy_review_submit(
            &mut rv,
            p(
                json!({"request": next["request"], "case_hash": next["case_hash"],
            "verdict": "tension",
            "citations": [{"block": block, "note": "these two cannot both hold"}],
            "severity": "blocking", "reason": "it contradicts the gate rule"}),
            ),
        ),
    );
    let advised = out.ok(
        "rdy_propose--with-advice",
        rdy_propose(
            &mut b,
            p(json!({"title": "Fix A and C", "summary": "Rewords A and C."})),
        ),
    );
    out.ok(
        "rdy_resolve--accept",
        rdy_resolve(
            &mut chat,
            p(json!({"accept": {
                "proposal": advised["ref"], "advised_digest": advised["advised_digest"],
                "evidence": "Yes, let it block.",
                "question": "Accept the reviewer's advice to block?",
                "session_ref": "chat-1"}})),
        ),
    );

    // canon's log, coverage of the spec, and the two inbox pages
    out.ok("rdy_log--page", rdy_log(&mut a, p(json!({"limit": 3}))));
    // the diff of the session's view, the detail of one doc against canon, and a filter (dq7)
    let view = a.view.clone();
    out.ok("rdy_diff--view", rdy_diff(&mut a, p(json!({"from": view}))));
    out.ok(
        "rdy_diff--detail-doc",
        rdy_diff(
            &mut a,
            p(json!({"from": "canon", "to": view, "doc": "spec", "detail": true})),
        ),
    );
    out.ok(
        "rdy_query--filter",
        rdy_query(
            &mut a,
            p(json!({"filter": "type = decision and status = live", "scope": "canon"})),
        ),
    );
    out.ok(
        "rdy_coverage--doc",
        rdy_coverage(&mut a, p(json!({"scope": "canon", "path": "spec"}))),
    );
    out.ok("rdy_inbox--page", rdy_inbox(&mut a, p(json!({}))));
    out.ok(
        "rdy_inbox--person",
        rdy_inbox(&mut chat, p(json!({"person": true}))),
    );

    // ck8 (D-JQD5X): the check surfaces. Agent a records a decision, runs a check on it and
    // asks for an independent review; the reviewer answers it, and the list shows both. These
    // calls come last, so no reply above sees their records, and the decision itself is not a
    // golden.
    let made = must(
        "a records a decision",
        rdy_decide(
            &mut a,
            p(
                json!({"statement": "Cache the reads", "title": "Cache the reads",
                     "why": "A person decides how reads are cached"}),
            ),
        ),
    );
    let decision = str_of(&made["created"]["$decision"], "the decision");
    out.ok(
        "rdy_review--run",
        rdy_review(
            &mut a,
            p(json!({"op": "run", "check": "decision_review", "subject": decision})),
        ),
    );
    out.ok(
        "rdy_review--request",
        rdy_review(
            &mut a,
            p(
                json!({"op": "request", "check": "decision_review", "subject": decision,
                     "reason": "Check the basis."}),
            ),
        ),
    );
    let claimed = must(
        "the reviewer claims the request",
        rdy_review_next(&mut rv, p(json!({"kind": "decision_review"}))),
    );
    out.ok(
        "rdy_review_submit--check",
        rdy_review_submit(
            &mut rv,
            p(
                json!({"request": claimed["request"], "case_hash": claimed["case_hash"],
                     "verdict": "concern",
                     "citations": [{"block": decision, "note": "the basis names no measure"}],
                     "reason": "The decision names no measure."}),
            ),
        ),
    );
    out.ok(
        "rdy_review--list",
        rdy_review(&mut a, p(json!({"op": "list"}))),
    );
    out.0
}

/// The two static lists, from a child `rdy mcp` on an empty project.
fn static_lists() -> Vec<(String, String)> {
    let (dir, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    vec![
        (
            STATIC_LISTS[0].to_string(),
            pretty(&Value::Array(c.list_resources())),
        ),
        (
            STATIC_LISTS[1].to_string(),
            pretty(&Value::Array(c.list_resource_templates())),
        ),
    ]
}

fn blessing() -> bool {
    std::env::var_os("RDY_QA_BLESS").is_some()
}

fn golden_names() -> BTreeSet<String> {
    let Ok(rd) = std::fs::read_dir(golden_dir()) else {
        return BTreeSet::new();
    };
    rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|n| n.strip_suffix(".json").map(str::to_string))
        .collect()
}

/// The first line where `got` differs from `want`, with its number (final review M10, minors
/// item 11).
fn first_difference(want: &str, got: &str) -> String {
    let (mut w, mut g) = (want.lines(), got.lines());
    for n in 1.. {
        match (w.next(), g.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (None, None) => return "  the texts differ only in their line ends".to_string(),
            (a, b) => {
                return format!(
                    "  line {n}: golden {:?}\n  line {n}: reply  {:?}",
                    a.unwrap_or("<end>"),
                    b.unwrap_or("<end>")
                );
            }
        }
    }
    unreachable!("the loop returns")
}

#[test]
fn sg7_a_golden_mismatch_names_the_first_differing_line() {
    assert_eq!(
        first_difference("{\n  \"a\": 1\n}", "{\n  \"a\": 2\n}"),
        "  line 2: golden \"  \\\"a\\\": 1\"\n  line 2: reply  \"  \\\"a\\\": 2\""
    );
    assert!(first_difference("a\nb", "a").contains("line 2: reply  \"<end>\""));
}

/// dq7: the replies of the scripted session whose name starts with `prefix` match their goldens.
pub(crate) fn check_goldens(prefix: &str) {
    if blessing() {
        // the sg7 test is rewriting the goldens at this moment
        return;
    }
    let all = run_session();
    let mut checked = 0;
    for (name, text) in all.iter().filter(|(n, _)| n.starts_with(prefix)) {
        let want = std::fs::read_to_string(golden_dir().join(format!("{name}.json")))
            .unwrap_or_else(|_| panic!("{name}: no golden file"));
        assert_eq!(
            &want,
            text,
            "{name}: the reply changed from the golden\n{}",
            first_difference(&want, text)
        );
        checked += 1;
    }
    assert!(checked > 0, "no reply of the session starts with {prefix}");
}

#[test]
fn sg7_every_m2_tool_reply_matches_its_golden() {
    let mut all = run_session();
    all.extend(static_lists());
    if blessing() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        for old in golden_names() {
            std::fs::remove_file(golden_dir().join(format!("{old}.json"))).unwrap();
        }
        for (name, text) in &all {
            std::fs::write(golden_dir().join(format!("{name}.json")), text).unwrap();
        }
        return;
    }
    let mut bad = vec![];
    for (name, text) in &all {
        match std::fs::read_to_string(golden_dir().join(format!("{name}.json"))) {
            Ok(want) if &want == text => {}
            Ok(want) => bad.push(format!(
                "{name}: the reply changed from the golden\n{}",
                first_difference(&want, text)
            )),
            Err(_) => bad.push(format!("{name}: no golden file")),
        }
    }
    let made: BTreeSet<String> = all.iter().map(|(n, _)| n.clone()).collect();
    for extra in golden_names().difference(&made) {
        bad.push(format!("{extra}: a golden no case makes"));
    }
    assert!(
        bad.is_empty(),
        "{} golden mismatches (bless with RDY_QA_BLESS=1 if intended):\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn sg7_the_golden_set_names_every_m2_tool() {
    if blessing() {
        // the other test is rewriting the goldens at this moment
        return;
    }
    let named: BTreeSet<String> = golden_names()
        .iter()
        .filter_map(|n| n.split_once("--").map(|(tool, _)| tool.to_string()))
        .collect();
    let (dir, r) = rdy_qa::fixtures::project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let listed: BTreeSet<String> = c
        .tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for tool in M2_TOOLS {
        assert!(named.contains(tool), "no golden names {tool}");
        assert!(listed.contains(tool), "tools/list lacks {tool}");
    }
    for name in STATIC_LISTS {
        assert!(
            golden_names().contains(name),
            "no golden for the list {name}"
        );
    }
}
