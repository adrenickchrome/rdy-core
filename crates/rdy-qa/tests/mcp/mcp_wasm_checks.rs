//! wck7 (D-ZRXY0, D-WV7ZS, D-QVTJ9): the MCP surfaces of module rules. `rdy_schema` lists both
//! rule tests and the fixed module limits under `checks.rules`; a module rule's rows keep the
//! filter rule's shape in `rdy_write` and `rdy_view` replies, in the `rdy_propose` result and in
//! the card's "Rule check" facts; `rdy_review` stays the eight agent checks, and a module rule
//! gets no request or verdict. Two goldens of an explicit module-rule world pin the schema's
//! rules object and a propose reply with module warnings. Bless with
//! `RDY_QA_BLESS=1 cargo test -p rdy-qa --release --test mcp wck7_`.
use rdy_engine::engine::Local;
use rdy_engine::{Rdy, pin_clock};
use rdy_mcp::tools::*;
use rdy_mcp::view_tools::rdy_view;
use rdy_qa::fixtures::{land, project};
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items};
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{ag, j_typo, put_j};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FAIL: &str = include_str!("../../fixtures/wasm/fail.b64");
const FAIL_TEXT: &str = include_str!("../../fixtures/wasm/fail-text.b64");
const TRAP: &str = include_str!("../../fixtures/wasm/trap.b64");

/// The eight agent checks, in catalogue order.
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

fn p<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/mcp-wasm")
}

/// `v` matches `tests/golden/mcp-wasm/<name>.json`; with `RDY_QA_BLESS` set, `v` is written
/// there instead.
fn golden(name: &str, v: &Value) {
    let text = format!("{}\n", serde_json::to_string_pretty(v).unwrap());
    let file = golden_dir().join(format!("{name}.json"));
    if std::env::var_os("RDY_QA_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_else(|_| panic!("{name}: no golden"));
    assert_eq!(want, text, "{name}: the reply changed from the golden");
}

/// One module rule of claims.
fn mrule(name: &str, module: &str) -> String {
    format!(
        "  - name: {name}\n    select: type = claim\n    module: {}\n    message: {name} message\n    fix: {name} fix\n",
        module.trim()
    )
}

/// The pack of these tests: three module rules of claims (one fails with its own text, one
/// traps, one fails with the rule's text) and a filter rule that every claim fails.
fn pack() -> String {
    format!(
        "name: wck7\nversion: \"1\"\nextends: rdy-standard\nrules:\n{}{}{}  - name: d-filter\n    select: type = claim\n    require: type = note\n    message: d-filter message\n    fix: d-filter fix\n",
        mrule("a-text", FAIL_TEXT),
        mrule("b-traps", TRAP),
        mrule("c-default", FAIL),
    )
}

/// Unseeds this thread's ids when the test ends, also when it panics.
struct Unseed;
impl Drop for Unseed {
    fn drop(&mut self) {
        rdy_model::unseed_ids_for_tests();
    }
}

/// A project of pack `wck7` with a landed draft doc and its section, as a local engine; the
/// section's id. The person is `human:qa`.
fn world() -> (tempfile::TempDir, Arc<Local>, String) {
    let (d, r) = project();
    drop(r);
    use_pack(
        d.path(),
        "wck7",
        &pack(),
        "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n",
    );
    std::fs::write(d.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let s = w.created["$s"].clone();
    (d, Arc::new(Local::new(r)), s)
}

fn session(e: &Arc<Local>, actor: &str, run: &str, role: &str) -> McpState {
    let mut st = McpState::new(e.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": actor, "run": run, "role": role})),
    )
    .unwrap();
    st
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
    k.sort();
    k
}

/// The rows a claim `c` gets from the pack, in rule order, as `on_change.checks` gives them.
fn rows_of(c: &str) -> Vec<Value> {
    let row = |rule: &str, message: String, fix: &str| json!({"rule": rule, "code": "W_RULE_CHECK", "block": c, "message": message, "fix": fix});
    vec![
        row(
            "a-text",
            format!("rule a-text fails on {c}: the module says no"),
            "ask the module",
        ),
        row(
            "b-traps",
            format!(
                "rule b-traps could not judge {c}: the module trapped: wasm `unreachable` instruction executed"
            ),
            "fix the module of rule b-traps in the pack file, or make the rule advisory",
        ),
        row(
            "c-default",
            format!("rule c-default fails on {c}: c-default message"),
            "c-default fix",
        ),
        row(
            "d-filter",
            format!("rule d-filter fails on {c}: d-filter message"),
            "d-filter fix",
        ),
    ]
}

fn checks(v: &Value) -> Vec<Value> {
    v["on_change"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("on_change.checks: {v:#}"))
        .clone()
}

fn created(v: &Value) -> String {
    v["created"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn wck7_schema_lists_module_tests_and_exact_limits() {
    let (_d, e, _s) = world();
    let mut st = session(&e, "agent:rd", "rd", "reader");
    let schema = rdy_schema(&mut st, p(json!({}))).unwrap();
    let checks = &schema["checks"];
    assert_eq!(keys(checks), ["agent", "rules", "stages"]);
    let rules = &checks["rules"];
    // the filter keys stay; the two tests and the module limits are added
    assert_eq!(
        keys(rules),
        [
            "default_severity",
            "grammar",
            "max",
            "module",
            "severity",
            "tests"
        ]
    );
    assert_eq!(rules["tests"], json!(["filter", "module"]));
    assert_eq!(
        rules["module"],
        json!({
            "interface": 1,
            "module_bytes": 1_048_576,
            "pack_bytes": 8_388_608,
            "args_bytes": 8192,
            "depth_min": 0,
            "depth_max": 3,
            "depth_default": 1,
            "memory_bytes": 16_777_216,
            "memories": 1,
            "tables": 1,
            "table_elements": 10_000,
            "call_depth": 1024,
            "value_stack_bytes": 524_288,
            "input_bytes": 1_048_576,
            "output_bytes": 65_536,
            "fuel_per_block": 10_000_000,
            "fuel_per_rule": 1_000_000_000_u64,
        })
    );
    // each value is the limit the pack load enforces
    let m = &rules["module"];
    assert_eq!(m["module_bytes"], rdy_schema::MAX_MODULE_BYTES);
    assert_eq!(m["pack_bytes"], rdy_schema::MAX_PACK_MODULE_BYTES);
    assert_eq!(m["args_bytes"], rdy_schema::MAX_ARGS_BYTES);
    assert_eq!(m["depth_max"], rdy_schema::MAX_DEPTH);
    assert_eq!(m["depth_default"], rdy_schema::DEFAULT_DEPTH);
    assert_eq!(
        serde_json::to_value(rdy_engine::check::rules::MODULE_LIMITS).unwrap(),
        *m
    );
    // the catalogue and the deterministic stage rows are the same as with no module rule
    let (_d2, plain) = project();
    let mut st2 = McpState::new(Arc::new(Local::new(plain)));
    rdy_session(
        &mut st2,
        p(json!({"actor": "agent:rd", "run": "rd", "role": "reader"})),
    )
    .unwrap();
    let other = rdy_schema(&mut st2, p(json!({}))).unwrap();
    assert_eq!(other["checks"], *checks);
    assert_eq!(
        checks["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "pack_rules")
            .unwrap()["stages"],
        json!(["change", "propose"])
    );
    let agent: Vec<&str> = checks["agent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(agent, CHECKS);
    golden("schema--rules", rules);
}

#[test]
fn wck7_write_view_propose_and_card_replies_keep_rule_shapes() {
    rdy_model::seed_ids_for_tests(0x5743_4B37);
    let _unseed = Unseed;
    let _clock = pin_clock(1_760_000_000_000);
    let (_d, e, s) = world();
    let mut st = session(&e, "agent:a", "a", "writer");
    // a write: one claim, four rows of the five keys, in rule order
    let v = rdy_write(
        &mut st,
        p(json!({"view": "w", "ops": [{"op": "create", "type": "claim", "parent": s, "body": "A claim."}]})),
    )
    .unwrap();
    let c = created(&v);
    let rows = checks(&v);
    for row in &rows {
        assert_eq!(
            keys(row),
            ["block", "code", "fix", "message", "rule"],
            "{row}"
        );
    }
    let want = rows_of(&c);
    assert_eq!(rows, want, "{v:#}");
    assert!(v["on_change"].get("checks_omitted").is_none(), "{v:#}");
    // a view include: the same rows
    let change = v["change"].as_str().unwrap().to_string();
    let inc = rdy_view(
        &mut st,
        p(json!({"op": "include", "view": "inc", "changes": [change]})),
    )
    .unwrap();
    assert_eq!(checks(&inc), want, "{inc:#}");
    // propose: each row is a warning, in the readiness warnings' shape, the filter rule's too
    rdy_qa::inbox::on(&e, |r| {
        answer_open_items(r, &ag("w"), "w");
    });
    let proposed = rdy_propose(
        &mut st,
        p(json!({"view": "w", "title": "A claim", "summary": TEST_SUMMARY})),
    )
    .unwrap();
    let warned: Vec<Value> = proposed["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "W_RULE_CHECK")
        .cloned()
        .collect();
    assert_eq!(warned.len(), 4, "{proposed:#}");
    let shape = keys(&warned[3]);
    for (w, row) in warned.iter().zip(&want) {
        assert_eq!(keys(w), shape, "{w}");
        assert_eq!(w["block"], row["block"]);
        assert_eq!(w["message"], row["message"]);
        assert_eq!(w["fix"], row["fix"]);
        assert_eq!(w["severity"], "warning");
    }
    assert!(proposed.get("checks_omitted").is_none(), "{proposed:#}");
    golden("propose--module-warning", &proposed);
    // the card: one "Rule check" fact per row, before Policy; the live Recommendation follows
    // Policy at once
    let mut relay = session(&e, "agent:relay", "r1", "writer");
    let pending = rdy_pending(&mut relay, p(json!({}))).unwrap();
    let item = pending["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == proposed["ref"])
        .unwrap_or_else(|| panic!("the card of {}: {pending:#}", proposed["ref"]))
        .clone();
    let facts: Vec<(String, String)> = item["facts"]
        .as_array()
        .unwrap_or_else(|| panic!("card facts: {item:#}"))
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap().to_string(),
                f["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let names: Vec<&str> = facts.iter().map(|(n, _)| n.as_str()).collect();
    let rule_facts: Vec<&str> = facts
        .iter()
        .filter(|(n, _)| n == "Rule check")
        .map(|(_, v)| v.as_str())
        .collect();
    let want_facts: Vec<String> = want
        .iter()
        .map(|r| {
            format!(
                "{}; fix: {}",
                r["message"].as_str().unwrap(),
                r["fix"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(rule_facts, want_facts, "{names:#?}");
    let last = names.iter().rposition(|n| *n == "Rule check").unwrap();
    assert_eq!(names[last + 1], "Policy", "{names:#?}");
    assert_eq!(names.len(), last + 2, "nothing follows Policy: {names:#?}");
    assert!(
        !names.contains(&"Rule checks omitted"),
        "no row is left out: {names:#?}"
    );
    // the integrated fact order of a card with decisions and a live Recommendation: the related
    // decisions follow Decisions, the rule checks precede Policy, and the Recommendation follows
    // Policy at once
    let (d, r, b) = rdy_qa::intake::batch_project();
    r.close().unwrap();
    use_pack(d.path(), "wck7", &pack(), "");
    std::fs::write(d.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    // the batch proposed again under the module pack: a version whose stored card has the rule
    // checks
    use rdy_qa::intake::{BATCH_SUMMARY, BATCH_TITLE, BATCH_VIEW, propose_reported};
    let mut r = Rdy::open(d.path()).unwrap();
    answer_open_items(&mut r, &rdy_qa::fixtures::writer(), BATCH_VIEW);
    let (r, report) = propose_reported(d.path(), r, BATCH_VIEW, BATCH_TITLE, Some(BATCH_SUMMARY));
    let batch_ref = report["ref"].as_str().unwrap().to_string();
    assert_ne!(batch_ref, b.r#ref, "a new version");
    let e = Arc::new(Local::new(r));
    let mut relay = session(&e, "agent:relay", "r1", "writer");
    let pending = rdy_pending(&mut relay, p(json!({"max_tokens": 20000}))).unwrap();
    let item = pending["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["ref"] == batch_ref)
        .unwrap_or_else(|| panic!("the batch's card: {pending:#}"))
        .clone();
    let facts: Vec<(String, String)> = item["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap().to_string(),
                f["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let names: Vec<&str> = facts.iter().map(|(n, _)| n.as_str()).collect();
    let at = |name: &str| {
        names
            .iter()
            .position(|n| *n == name)
            .unwrap_or_else(|| panic!("{name} in {names:#?}"))
    };
    assert_eq!(at("Related decisions"), at("Decisions") + 1, "{names:#?}");
    let related = &facts[at("Related decisions")].1;
    assert!(
        related.contains(" listed by link, ") && related.contains(" found by text, "),
        "{related}"
    );
    // the batch's claim Q fails each of the four rules (the other claims of the landing
    // projection do too)
    let q = b.advised.to_string();
    let rule_rows: Vec<&String> = facts
        .iter()
        .filter(|(n, v)| n == "Rule check" && v.contains(&format!(" {q}: ")))
        .map(|(_, v)| v)
        .collect();
    let want_q: Vec<String> = rows_of(&q)
        .iter()
        .map(|r| {
            format!(
                "{}; fix: {}",
                r["message"].as_str().unwrap(),
                r["fix"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(rule_rows.len(), want_q.len(), "{names:#?}");
    for (got, want) in rule_rows.iter().zip(&want_q) {
        assert_eq!(*got, want);
    }
    let last = names.iter().rposition(|n| *n == "Rule check").unwrap();
    assert_eq!(names[last + 1], "Policy", "{names:#?}");
    assert_eq!(names[last + 2], "Recommendation", "{names:#?}");
    assert!(
        facts[last + 2].1.starts_with("none: "),
        "{:?}",
        facts[last + 2]
    );
    // D-91DEW: no option is recommended while the cause holds
    assert!(
        item["options"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["recommended"] != json!(true)),
        "{item:#}"
    );
}

#[test]
fn wck7_review_remains_agent_checks_only() {
    let (_d, e, s) = world();
    let mut st = session(&e, "agent:a", "a", "writer");
    let v = rdy_write(
        &mut st,
        p(json!({"view": "w", "ops": [{"op": "create", "type": "claim", "parent": s, "body": "A claim."}]})),
    )
    .unwrap();
    assert_eq!(checks(&v).len(), 4, "{v:#}");
    rdy_qa::inbox::on(&e, |r| {
        answer_open_items(r, &ag("w"), "w");
    });
    let proposed = rdy_propose(
        &mut st,
        p(json!({"view": "w", "title": "A claim", "summary": TEST_SUMMARY})),
    )
    .unwrap();
    // the catalogue holds the eight agent checks and no rule
    let catalogue = rdy_review(&mut st, p(json!({"op": "checks"}))).unwrap();
    let names: Vec<&str> = catalogue["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, CHECKS);
    // a module rule's name is no check to run or request
    for args in [
        json!({"op": "run", "check": "a-text", "subject": proposed["ref"]}),
        json!({"op": "request", "check": "a-text", "subject": proposed["ref"], "reason": "is it right?"}),
    ] {
        let op = args["op"].clone();
        let e = rdy_review(&mut st, p(args)).expect_err("a rule is no agent check");
        assert!(
            e.message.contains("a-text"),
            "{op}: {} {}",
            e.code,
            e.message
        );
    }
    // the requests and verdicts name no rule, and a reviewer gets no rule's request
    let list = rdy_review(&mut st, p(json!({"op": "list"}))).unwrap();
    let text = list.to_string();
    for rule in ["a-text", "b-traps", "c-default", "d-filter"] {
        assert!(!text.contains(rule), "{rule} in {list:#}");
    }
    let mut rv = session(&e, "agent:r", "r", "reviewer");
    let refused = rdy_review_next(&mut rv, p(json!({"kind": "a-text"})))
        .expect_err("a rule is no review kind");
    assert!(refused.message.contains("a-text"), "{refused:#?}");
    for _ in 0..16 {
        let next = rdy_review_next(&mut rv, p(json!({"kind": "any"}))).unwrap();
        if next["request"].is_null() {
            break;
        }
        assert!(
            !next.to_string().contains("rule a-text"),
            "a case holds no rule row: {next:#}"
        );
    }
}
