//! wck6 (D-ZRXY0, D-WV7ZS, D-N1GG5, D-52PJG, D-727FK): module rules at the `change` and
//! `propose` stages. A module rule gives the same rows, codes, caps and readiness item as a
//! filter rule: a fail answer takes the module's text or the rule's own, and a module that
//! cannot judge a block fails it with its cause. At `change` a module judges the changed blocks
//! and the blocks one explicit link away, whatever its depth; at `propose` it judges the whole
//! landing projection. The reach of an assessment holds every block a module input reads, the
//! neighbours the input cap drops and the blocks a derived value reads included, so a landing
//! that changes one stales the assessment. A host fault is the rule read error.
use rdy_engine::check::module_run::module_runs_on_this_thread_for_tests;
use rdy_engine::check::rules::{host_fault_in_next_module_run_for_tests, rule_reach_for_tests};
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::{rdy_session, rdy_write};
use rdy_model::assessment::MAX_ROWS;
use rdy_model::foreign::ForeignParts;
use rdy_model::{Block, BlockId, Hash, Op};
use rdy_qa::fixtures::*;
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items, confirm_before_landing};
use rdy_qa::packs::use_pack;
use rdy_qa::tensions::{ag, j_typo, put_j, upd};
use rdy_schema::{Issue, Severity};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const PASS: &str = include_str!("../../fixtures/wasm/pass.b64");
const FAIL: &str = include_str!("../../fixtures/wasm/fail.b64");
const FAIL_TEXT: &str = include_str!("../../fixtures/wasm/fail-text.b64");
const BAD_OUTPUT: &str = include_str!("../../fixtures/wasm/bad-output.b64");
const TRAP: &str = include_str!("../../fixtures/wasm/trap.b64");
const SPIN: &str = include_str!("../../fixtures/wasm/spin.b64");
const BURN: &str = include_str!("../../fixtures/wasm/burn.b64");
const BIG_OUTPUT: &str = include_str!("../../fixtures/wasm/big-output.b64");
const MARKER: &str = include_str!("../../fixtures/wasm/marker.b64");

/// The pack of these tests: claims, sections and decisions with an `ok` and a `checked` field,
/// then `rules`.
fn pack(rules: &str) -> String {
    format!(
        "\
name: wck6
version: \"1\"
extends: rdy-standard
block_types:
  section:
    prefix: s
    traits: [structural]
    fields:
      title: {{ kind: str, required: true }}
      lifecycle: {{ kind: enum, values: [draft, stable, deprecated] }}
      ok: {{ kind: bool }}
    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, value, entity, flow, example, note, rationale, table, diagram, figure, code, decision, question]
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      ok: {{ kind: bool }}
      checked: {{ kind: bool }}
  decision:
    prefix: D
    traits: [normative]
    fields:
      ok: {{ kind: bool }}
rules:
{rules}"
    )
}

/// One module rule, as the pack file writes it: `extra` holds its `args` and `depth` lines.
fn mrule(name: &str, select: &str, module: &str, extra: &str, severity: &str) -> String {
    format!(
        "  - name: {name}\n    select: {select}\n    module: {}\n{extra}    severity: {severity}\n    message: {name} message\n    fix: {name} fix\n",
        module.trim()
    )
}

fn wck6_project(rules: &str, extra: &str) -> (tempfile::TempDir, Rdy) {
    let (d, r) = project();
    drop(r);
    use_pack(d.path(), "wck6", &pack(rules), extra);
    let r = Rdy::open(d.path()).unwrap();
    (d, r)
}

/// The gate settings that let a version land after another landing.
const LOOSE: &str = "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n";

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn p<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// An MCP session of agent `agent:a` (run `a`, writer) on `e`.
fn mcp(e: &Arc<Local>) -> rdy_mcp::tools::McpState {
    let mut st = rdy_mcp::tools::McpState::new(e.clone());
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:a", "run": "a", "role": "writer"})),
    )
    .unwrap();
    st
}

fn wr(st: &mut rdy_mcp::tools::McpState, view: &str, ops: Value) -> Value {
    rdy_write(st, p(json!({"view": view, "ops": ops}))).unwrap()
}

/// A write of `ops` to view `view` as agent `view`, with the brief stable content needs, and
/// its reply with `on_change`, as a write tool gives it.
fn report(e: &Arc<Local>, view: &str, ops: Value) -> Value {
    let view_name = view.to_string();
    rdy_engine::consequence::write_and_report(&**e, view, None, move |r| {
        rdy_qa::tensions::try_put_j(r, &view_name, ops, j_typo())
    })
    .unwrap()
}

/// The rows of a reply's `on_change.checks`, as they are.
fn check_rows(v: &Value) -> Vec<Value> {
    v["on_change"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("on_change.checks: {v:#}"))
        .clone()
}

fn row_blocks(v: &Value) -> BTreeSet<String> {
    check_rows(v)
        .iter()
        .map(|r| r["block"].as_str().unwrap().to_string())
        .collect()
}

fn created(v: &Value) -> Vec<String> {
    v["created"]
        .as_object()
        .unwrap()
        .values()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

/// A stable doc `spec` with section `ss`, and a draft doc `notes` with section `ds`, landed.
fn stable_and_draft(rules: &str, extra: &str) -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = wck6_project(rules, extra);
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$st", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$st", "fields": {"title": "Rules"}},
            {"op": "create", "type": "doc", "id": "$dr", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ds", "parent": "$dr", "fields": {"title": "Ideas"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let ss = seeded.created["$ss"].clone();
    let ds = seeded.created["$ds"].clone();
    (d, r, ss, ds)
}

fn claim(parent: &str, body: &str, fields: Value) -> Value {
    json!({"op": "create", "type": "claim", "parent": parent, "body": body, "fields": fields})
}

fn propose_as(r: &mut Rdy, view: &str) -> ProposalInfo {
    answer_open_items(r, &ag(view), view);
    r.propose(&ag(view), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"))
}

/// A version's rule warnings, as (block, message, fix).
fn warned(p: &ProposalInfo) -> Vec<(String, String, String)> {
    p.warnings
        .iter()
        .filter(|w| w.code == W_RULE_CHECK)
        .map(|w| {
            (
                w.block
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                w.message.clone(),
                w.fix.clone(),
            )
        })
        .collect()
}

fn rule_issues(p: &ProposalInfo) -> Vec<&Issue> {
    p.issues.iter().filter(|i| i.code == E_RULE_CHECK).collect()
}

fn stored(r: &Rdy, p: &ProposalInfo) -> rdy_model::assessment::Assessment {
    let st = r.read();
    let version: Hash = p.id.parse().unwrap();
    let h = st
        .reader()
        .assessment_of_proposal(&version, st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{version} stores an assessment"));
    st.reader().assessment(&h).unwrap()
}

fn fact_names(facts: &[String]) -> Vec<&str> {
    facts
        .iter()
        .map(|f| f.split_once(": ").map_or(f.as_str(), |(n, _)| n))
        .collect()
}

#[test]
fn wck6_pass_fail_and_default_text_use_existing_rows() {
    let rules = mrule("fail-m", "type = claim", FAIL, "", "advisory")
        + &mrule("pass-m", "type = claim", PASS, "", "advisory")
        + &mrule("text-m", "type = claim", FAIL_TEXT, "", "advisory")
        + &mrule(
            "gate-m",
            "type = claim and fields.checked = true",
            FAIL,
            "",
            "blocking",
        );
    let (_d, mut r, ss, ds) = stable_and_draft(&rules, "");
    // propose: a blocking failure on stable content is an issue of readiness item 6
    let w = put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": ss, "body": "C.", "fields": {"checked": true}}]),
        j_typo(),
    );
    let c = w.created["$c"].clone();
    let p1 = propose_as(&mut r, "a1");
    assert!(!p1.ready);
    let issues = rule_issues(&p1);
    assert_eq!(issues.len(), 1, "{:#?}", p1.issues);
    assert_eq!(issues[0].severity, Severity::Error);
    assert_eq!(
        issues[0].block.as_ref().map(ToString::to_string),
        Some(c.clone())
    );
    assert_eq!(
        issues[0].message,
        format!("rule gate-m fails on {c}: gate-m message")
    );
    assert_eq!(issues[0].fix, "gate-m fix");
    // the fail answers with no text take the rule's; a fail with text takes the module's
    assert_eq!(
        warned(&p1),
        vec![
            (
                c.clone(),
                format!("rule fail-m fails on {c}: fail-m message"),
                "fail-m fix".to_string()
            ),
            (
                c.clone(),
                format!("rule text-m fails on {c}: the module says no"),
                "ask the module".to_string()
            ),
        ]
    );
    // on a draft every failure is a warning, stored on the card before Policy
    let w = put_j(
        &mut r,
        "a2",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": ds, "body": "D.", "fields": {"checked": true}}]),
        j_typo(),
    );
    let dc = w.created["$c"].clone();
    let p2 = propose_as(&mut r, "a2");
    assert!(p2.ready, "{:#?}", p2.issues);
    let got: Vec<String> = warned(&p2).into_iter().map(|(_, m, _)| m).collect();
    assert_eq!(
        got,
        vec![
            format!("rule fail-m fails on {dc}: fail-m message"),
            format!("rule gate-m fails on {dc}: gate-m message"),
            format!("rule text-m fails on {dc}: the module says no"),
        ]
    );
    let facts = stored(&r, &p2).card.facts;
    let n = fact_names(&facts);
    assert_eq!(n.iter().filter(|x| **x == "Rule check").count(), 3);
    let last = n.iter().rposition(|x| *x == "Rule check").unwrap();
    assert_eq!(n[last + 1], "Policy");
    assert_eq!(
        facts[last],
        format!("Rule check: rule text-m fails on {dc}: the module says no; fix: ask the module")
    );
    // change: the same five-key rows, in rule then block order; a pass gives none
    let e = Arc::new(Local::new(r));
    let v = report(
        &e,
        "b",
        json!([
            claim(&ss, "Stable.", json!({"checked": true})),
            claim(&ds, "Draft.", json!({"checked": false}))
        ]),
    );
    let made = created(&v);
    let (mut sc, mut dc) = (String::new(), String::new());
    for m in &made {
        let b = e
            .read()
            .materialized(&Scope::View("b".into()))
            .unwrap()
            .get(&id(m))
            .unwrap()
            .clone();
        if b.parent.as_ref().map(ToString::to_string) == Some(ss.clone()) {
            sc = m.clone();
        } else {
            dc = m.clone();
        }
    }
    let row = |rule: &str, code: &str, block: &str, message: &str, fix: &str| {
        json!({"rule": rule, "code": code, "block": block,
               "message": format!("rule {rule} fails on {block}: {message}"), "fix": fix})
    };
    let mut want = vec![
        row(
            "fail-m",
            "W_RULE_CHECK",
            &sc,
            "fail-m message",
            "fail-m fix",
        ),
        row(
            "fail-m",
            "W_RULE_CHECK",
            &dc,
            "fail-m message",
            "fail-m fix",
        ),
        row(
            "gate-m",
            "E_RULE_CHECK",
            &sc,
            "gate-m message",
            "gate-m fix",
        ),
        row(
            "text-m",
            "W_RULE_CHECK",
            &sc,
            "the module says no",
            "ask the module",
        ),
        row(
            "text-m",
            "W_RULE_CHECK",
            &dc,
            "the module says no",
            "ask the module",
        ),
    ];
    want.sort_by(|a, b| {
        (a["rule"].as_str(), a["block"].as_str()).cmp(&(b["rule"].as_str(), b["block"].as_str()))
    });
    assert_eq!(check_rows(&v), want, "{v:#}");
    assert!(v["on_change"].get("checks_omitted").is_none(), "{v:#}");
}

/// What a cause case expects of a row's message after `could not judge <id>: `.
enum Words {
    Exact(&'static str),
    Prefix(&'static str),
}

fn assert_cause(message: &str, rule: &str, block: &str, words: &Words) {
    let head = format!("rule {rule} could not judge {block}: ");
    let rest = message
        .strip_prefix(&head)
        .unwrap_or_else(|| panic!("{message:?} starts with {head:?}"));
    match words {
        Words::Exact(w) => assert_eq!(rest, *w),
        Words::Prefix(w) => assert!(
            rest.starts_with(w) && rest.len() > w.len(),
            "{rest:?} starts with {w:?}"
        ),
    }
}

#[test]
fn wck6_each_cannot_judge_cause_fails_with_effective_severity() {
    let big = "x".repeat(1 << 20);
    let cases: [(&str, &str, Words); 5] = [
        (TRAP, "C.", Words::Prefix("the module trapped: ")),
        (
            SPIN,
            "C.",
            Words::Exact("the module used all of its 10000000 fuel"),
        ),
        (BIG_OUTPUT, "C.", Words::Exact("the output is over 64 KiB")),
        (
            BAD_OUTPUT,
            "C.",
            Words::Prefix("the output is not a check result: "),
        ),
        (PASS, &big, Words::Exact("the input is over 1 MiB")),
    ];
    let fix = "fix the module of rule m in the pack file, or make the rule advisory";
    for (module, body, words) in &cases {
        let (_d, mut r, ss, ds) =
            stable_and_draft(&mrule("m", "type = claim", module, "", "blocking"), "");
        // propose: an issue on stable content, a warning on a draft
        let w = put_j(
            &mut r,
            "p",
            json!([
                {"op": "create", "type": "claim", "id": "$s", "parent": ss, "body": body},
                {"op": "create", "type": "claim", "id": "$d", "parent": ds, "body": body}
            ]),
            j_typo(),
        );
        let (s, d) = (w.created["$s"].clone(), w.created["$d"].clone());
        let pv = propose_as(&mut r, "p");
        assert!(!pv.ready);
        let issues = rule_issues(&pv);
        assert_eq!(issues.len(), 1, "{:#?}", pv.issues);
        assert_eq!(
            issues[0].block.as_ref().map(ToString::to_string),
            Some(s.clone())
        );
        assert_cause(&issues[0].message, "m", &s, words);
        assert_eq!(issues[0].fix, fix);
        let warns = warned(&pv);
        assert_eq!(warns.len(), 1, "{warns:#?}");
        assert_eq!(warns[0].0, d);
        assert_cause(&warns[0].1, "m", &d, words);
        assert_eq!(warns[0].2, fix);
        // change: the same codes by the after-state's stability
        let e = Arc::new(Local::new(r));
        let v = report(
            &e,
            "c",
            json!([claim(&ss, body, json!({})), claim(&ds, body, json!({}))]),
        );
        let rows = check_rows(&v);
        assert_eq!(rows.len(), 2, "{v:#}");
        let codes: BTreeSet<&str> = rows.iter().map(|r| r["code"].as_str().unwrap()).collect();
        assert_eq!(codes, BTreeSet::from(["E_RULE_CHECK", "W_RULE_CHECK"]));
        for row in &rows {
            assert_cause(
                row["message"].as_str().unwrap(),
                "m",
                row["block"].as_str().unwrap(),
                words,
            );
            assert_eq!(row["fix"], fix);
        }
    }
    // the rule's own budget: 100 blocks each use a block's whole fuel (filling memory, which
    // burns fuel fast), and the next block in id order finds none left
    let (_d, r, _ss, ds) = stable_and_draft(&mrule("m", "type = claim", BURN, "", "blocking"), "");
    let e = Arc::new(Local::new(r));
    let mut st = mcp(&e);
    let ops: Vec<Value> = (0..101)
        .map(|k| claim(&ds, &format!("Claim {k}."), json!({})))
        .collect();
    let v = wr(&mut st, "f", Value::Array(ops));
    let rows = check_rows(&v);
    assert_eq!(rows.len(), 101, "{v:#}");
    let last = created(&v).into_iter().max().unwrap();
    for row in &rows {
        let block = row["block"].as_str().unwrap();
        assert_eq!(row["code"], "W_RULE_CHECK");
        let words = if block == last {
            Words::Exact("the rule used all of its 1000000000 fuel in this run")
        } else {
            Words::Exact("the module used all of its 10000000 fuel")
        };
        assert_cause(row["message"].as_str().unwrap(), "m", block, &words);
    }
}

/// The change-selection world, in one draft doc: claim `a` (it references `r`), claim `b` (it
/// references `a`), claim `z` (it references `b`: two hops from `a`), decision `l` (it affects
/// `a`), all in section `s`.
fn link_world(rules: &str) -> (tempfile::TempDir, Arc<Local>, BTreeMap<String, String>) {
    let (d, mut r) = wck6_project(rules, "");
    let w = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "links", "title": "Links", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$r", "parent": "$s", "body": "R holds."},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A builds on {{$r}}."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
        {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z builds on {{$b}}."},
        {"op": "create", "type": "decision", "id": "$l", "parent": "$s", "fields": {"statement": "L", "status": "proposed", "needs_content": false}, "links": {"affects": [{"target": "$a"}]}}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let ids = w
        .created
        .iter()
        .map(|(k, v)| (k.trim_start_matches('$').to_string(), v.clone()))
        .collect();
    (d, Arc::new(Local::new(r)), ids)
}

#[test]
fn wck6_change_selection_stays_one_explicit_hop() {
    // a module of depth 3 reads three hops, but the change stage still judges one
    let (_d, e, ids) = link_world(&mrule(
        "m",
        "type = claim or type = decision",
        FAIL,
        "    depth: 3\n",
        "advisory",
    ));
    let set =
        |names: &[&str]| -> BTreeSet<String> { names.iter().map(|n| ids[*n].clone()).collect() };
    let mut st = mcp(&e);
    let a = ids["a"].clone();
    let v = wr(
        &mut st,
        "u",
        json!([{"op": "update", "id": a, "body": format!("A builds on {{{{{}}}}}, now.", ids["r"])}]),
    );
    assert_eq!(row_blocks(&v), set(&["a", "r", "b", "l"]), "{v:#}");
    // a delete: the before-state's neighbours, judged in the after-state; the deleted block has
    // no row
    let v = wr(&mut st, "d", json!([{"op": "delete", "id": a}]));
    assert_eq!(row_blocks(&v), set(&["r", "b", "l"]), "{v:#}");
}

#[test]
fn wck6_change_caps_count_every_selected_block() {
    let (_d, r, _ss, ds) = stable_and_draft(&mrule("m", "type = claim", FAIL, "", "advisory"), "");
    let e = Arc::new(Local::new(r));
    let mut st = mcp(&e);
    let ops: Vec<Value> = (0..300)
        .map(|k| claim(&ds, &format!("Claim {k}."), json!({})))
        .collect();
    let runs = module_runs_on_this_thread_for_tests();
    let v = wr(&mut st, "c", Value::Array(ops));
    assert_eq!(
        module_runs_on_this_thread_for_tests() - runs,
        300,
        "every selected block is judged"
    );
    let rows = check_rows(&v);
    assert_eq!(rows.len(), 256);
    assert_eq!(
        v["on_change"]["checks_omitted"],
        json!(44),
        "{:#}",
        v["on_change"]
    );
    let blocks: Vec<&str> = rows.iter().map(|r| r["block"].as_str().unwrap()).collect();
    let mut sorted = blocks.clone();
    sorted.sort_unstable();
    assert_eq!(blocks, sorted, "block order");
    let mut all = created(&v);
    all.sort();
    assert_eq!(
        blocks,
        all[..256].iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn wck6_propose_caps_count_every_selected_block() {
    let (_d, mut r, ss, ds) =
        stable_and_draft(&mrule("m", "type = claim", FAIL, "", "blocking"), "");
    let failing = |r: &mut Rdy, view: &str, parent: &str, n: usize| {
        let ops: Vec<Value> = (0..n)
            .map(|k| claim(parent, &format!("Claim {k}."), json!({})))
            .collect();
        put_j(r, view, Value::Array(ops), j_typo());
    };
    // 256 blocking rows are kept; at 257 one is counted
    failing(&mut r, "b1", &ss, 256);
    let p = propose_as(&mut r, "b1");
    assert_eq!(rule_issues(&p).len(), 256);
    assert_eq!(p.checks_omitted, None);
    failing(&mut r, "b2", &ss, 257);
    let runs = module_runs_on_this_thread_for_tests();
    let p = propose_as(&mut r, "b2");
    assert!(
        module_runs_on_this_thread_for_tests() - runs >= 257,
        "every selected block is judged"
    );
    assert!(!p.ready);
    assert_eq!(rule_issues(&p).len(), 256);
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["checks_omitted"], json!({"blocking": 1, "warnings": 0}));
    // 257 warnings on a draft: 256 kept, one counted, and the version is ready
    failing(&mut r, "b3", &ds, 257);
    let p = propose_as(&mut r, "b3");
    assert!(p.ready, "{:#?}", p.issues);
    assert_eq!(warned(&p).len(), 256);
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["checks_omitted"], json!({"blocking": 0, "warnings": 1}));
    let facts = stored(&r, &p).card.facts;
    let n = fact_names(&facts);
    let at = n.iter().position(|x| *x == "Rule checks omitted").unwrap();
    assert_eq!(facts[at], "Rule checks omitted: 0 blocking, 1 advisory");
    assert_eq!(n[at + 1], "Policy");
}

#[test]
fn wck6_propose_judges_the_net_landing_projection() {
    // the module fails a block whose input holds the mark ~!~
    let (_d, mut r) = wck6_project(
        &mrule("mark", "type = claim", MARKER, "    depth: 0\n", "advisory"),
        "",
    );
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
        {"op": "create", "type": "claim", "id": "$old", "parent": "$s", "body": "Old ~!~."}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let s = seeded.created["$s"].clone();
    let old = seeded.created["$old"].clone();
    let blocks = |p: &ProposalInfo| -> BTreeSet<String> {
        warned(p).into_iter().map(|(b, _, _)| b).collect()
    };
    // a subset: the change named, not the view's other edit
    let mine = write(&mut r, "v", json!([claim(&s, "Mine ~!~.", json!({}))])).unwrap();
    let other = write(&mut r, "v", json!([claim(&s, "Other ~!~.", json!({}))])).unwrap();
    let m = mine.created.values().next().unwrap().clone();
    let o = other.created.values().next().unwrap().clone();
    let pv = r
        .propose_subset(
            &writer(),
            "v",
            "subset",
            Some(std::slice::from_ref(&mine.change)),
            Some(TEST_SUMMARY),
        )
        .unwrap();
    let got = blocks(&pv);
    assert!(got.contains(&m) && got.contains(&old), "{got:?}");
    assert!(!got.contains(&o), "the unselected edit is absent: {got:?}");
    assert_eq!(
        warned(&pv).iter().find(|(b, _, _)| *b == m).unwrap().1,
        format!("rule mark fails on {m}: the input holds the mark")
    );
    // a deleted failure disappears
    write(&mut r, "del", json!([{"op": "delete", "id": old}])).unwrap();
    let pd = r
        .propose(&writer(), "del", "del", Some(TEST_SUMMARY))
        .unwrap();
    assert!(!blocks(&pd).contains(&old), "{:?}", warned(&pd));
    // a later repair in the selected changes clears an earlier failure
    let made = write(&mut r, "fix", json!([claim(&s, "F ~!~.", json!({}))])).unwrap();
    let f = made.created.values().next().unwrap().clone();
    write(&mut r, "fix", upd(&id(&f), "F holds.")).unwrap();
    let pf = r
        .propose(&writer(), "fix", "fix", Some(TEST_SUMMARY))
        .unwrap();
    assert!(!blocks(&pf).contains(&f), "{:?}", warned(&pf));
}

/// The freshness world: one draft doc per block, so that only references join them: `w` (the
/// written claim, it references `r`), `n1` references `w`, `n2` references `n1`, `n3`
/// references `n2`, `n4` references `n3`, and `o`, a claim no reference joins. Every claim but `w` is `ok: true`;
/// a version sets `w` to `ok: false`.
struct World {
    _d: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
}

impl World {
    fn id(&self, n: &str) -> String {
        self.ids[n].clone()
    }
    fn name_of(&self, i: &str) -> String {
        self.ids
            .iter()
            .find(|(_, v)| v.as_str() == i)
            .map_or(i.to_string(), |(k, _)| k.clone())
    }
}

fn world(rules: &str, n1_body: Option<&str>) -> World {
    let (d, mut r) = wck6_project(rules, LOOSE);
    let mut ops = vec![];
    for name in ["w", "r", "n1", "n2", "n3", "n4", "o"] {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$d{name}"), "fields": {"name": format!("doc-{name}"), "title": format!("Doc {name}"), "doc_type": "overview"}}));
        ops.push(json!({"op": "create", "type": "section", "id": format!("$s{name}"), "parent": format!("$d{name}"), "fields": {"title": format!("S {name}")}}));
    }
    let body = |text: &str, to: &str| format!("{text} {{{{${to}}}}}.");
    let n1 = n1_body.map_or_else(|| body("N1 builds on", "w"), |b| format!("{b} {{{{$w}}}}."));
    ops.extend([
        json!({"op": "create", "type": "claim", "id": "$r", "parent": "$sr", "body": "R holds.", "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$w", "parent": "$sw", "body": body("W builds on", "r"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n1", "parent": "$sn1", "body": n1, "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n2", "parent": "$sn2", "body": body("N2 builds on", "n1"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n3", "parent": "$sn3", "body": body("N3 builds on", "n2"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n4", "parent": "$sn4", "body": body("N4 builds on", "n3"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$o", "parent": "$so", "body": "O holds.", "fields": {"ok": true}}),
    ]);
    let w = write(&mut r, "seed", Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    let ids = w
        .created
        .iter()
        .map(|(k, v)| (k.trim_start_matches('$').to_string(), v.clone()))
        .collect();
    World { _d: d, r, ids }
}

/// Proposes view `view`, which sets `w` to `ok: false`; the version.
fn propose_w(w: &mut World, view: &str) -> ProposalInfo {
    let wid = w.id("w");
    write(
        &mut w.r,
        view,
        json!([{"op": "update", "id": wid, "fields": {"ok": false}}]),
    )
    .unwrap();
    let p =
        w.r.propose(&writer(), view, view, Some(TEST_SUMMARY))
            .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    p
}

fn reach_names(w: &World, p: &ProposalInfo) -> BTreeSet<String> {
    stored(&w.r, p)
        .reach
        .iter()
        .map(|i| w.name_of(&i.to_string()))
        .collect()
}

/// Lands an edit of `name`'s block through view `view`.
fn land_edit(w: &mut World, view: &str, name: &str) {
    let target = id(&w.id(name));
    write(&mut w.r, view, upd(&target, &format!("{name} changed."))).unwrap();
    land(&mut w.r, view).unwrap();
}

fn approve(w: &mut World, p: &ProposalInfo) -> Result<LandInfo, RdyError> {
    confirm_before_landing(&mut w.r, &approver(), p);
    w.r.land_report(&approver(), &p.id)
}

/// Asserts that landing `p` refuses because a landing changed `name`'s block.
fn assert_moved_by(w: &mut World, p: &ProposalInfo, name: &str) {
    let e = approve(w, p).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    let changed = w.id(name);
    assert!(
        e.message
            .starts_with(&format!("proposal {} moved: landing ", p.r#ref))
            && e.message
                .ends_with(&format!(" changed {changed} after its assessment")),
        "{}",
        e.message
    );
}

#[test]
fn wck6_depth_and_complete_inputs_bind_freshness() {
    let names =
        |list: &[&str]| -> BTreeSet<String> { list.iter().map(|s| s.to_string()).collect() };
    // a passing module still binds what it read: depth 0 reads the block alone and its derived
    // values (its section and doc, and the blocks that reference it, which its status reads)
    let rules = |depth: u8| {
        mrule(
            "m",
            "type = claim and fields.ok = false",
            PASS,
            &format!("    depth: {depth}\n"),
            "advisory",
        )
    };
    let mut w = world(&rules(0), None);
    let p = propose_w(&mut w, "v");
    let reach = reach_names(&w, &p);
    assert!(
        reach.is_superset(&names(&["w", "sw", "dw", "n1"])),
        "{reach:?}"
    );
    assert!(!reach.contains("r") && !reach.contains("n2"), "{reach:?}");
    // depth 1: the neighbours one hop away are in the input, and the inputs of each block of
    // that expansion are read in full: n1's input holds n2, whose status reads n3; four hops
    // stay outside
    let mut w = world(&rules(1), None);
    let p = propose_w(&mut w, "v");
    let reach = reach_names(&w, &p);
    assert!(
        reach.is_superset(&names(&["w", "r", "n1", "n2", "n3"])),
        "{reach:?}"
    );
    assert!(!reach.contains("n4") && !reach.contains("o"), "{reach:?}");
    // a landing of a block the inputs read stales the assessment
    land_edit(&mut w, "x", "n3");
    assert_moved_by(&mut w, &p, "n3");
    // a landing outside the reach that keeps its edges does not
    let mut w = world(&rules(1), None);
    let p = propose_w(&mut w, "v");
    land_field(&mut w, "x", "n4");
    land_field(&mut w, "y", "o");
    let l = approve(&mut w, &p).unwrap();
    assert_eq!(l.approved, p.r#ref);
    // a landing outside the reach that removes a reference to a block of the reach does: n4's
    // new text drops its reference to n3
    let mut w = world(&rules(1), None);
    let p = propose_w(&mut w, "v");
    land_edit(&mut w, "x", "n4");
    assert_moved_by(&mut w, &p, "n4");
}

/// Lands an edit of `name`'s `checked` field through view `view`: its text, links and place
/// stay.
fn land_field(w: &mut World, view: &str, name: &str) {
    let target = w.id(name);
    write(
        &mut w.r,
        view,
        json!([{"op": "update", "id": target, "fields": {"checked": true}}]),
    )
    .unwrap();
    land(&mut w.r, view).unwrap();
}

/// A blocking module rule of depth 1 that fails a block whose input holds the mark `~!~`; a
/// world with it, and the version that sets `w` to `ok: false`, ready: no input holds the mark
/// yet.
fn marked_world() -> (World, ProposalInfo) {
    let rules = mrule(
        "mark",
        "type = claim and fields.ok = false",
        MARKER,
        "    depth: 1\n",
        "blocking",
    );
    let mut w = world(&rules, None);
    let p = propose_w(&mut w, "v");
    let reach = reach_names(&w, &p);
    assert!(
        reach.contains("w") && reach.contains("sw") && !reach.contains("o"),
        "{reach:?}"
    );
    (w, p)
}

#[test]
fn wck6_new_reference_to_a_judged_block_moves_the_assessment() {
    // a landing gives o, outside the reach, a reference to the judged block w, with the mark in
    // its text: w's input now holds the mark, so the assessment moves
    let (mut w, p) = marked_world();
    let (o, wid) = (w.id("o"), w.id("w"));
    write(
        &mut w.r,
        "x",
        json!([{"op": "update", "id": o, "body": format!("O ~!~ builds on {{{{{wid}}}}}.")}]),
    )
    .unwrap();
    land(&mut w.r, "x").unwrap();
    assert_moved_by(&mut w, &p, "o");
}

#[test]
fn wck6_filter_only_pack_keeps_its_freshness_on_a_new_reference() {
    // the same landing under a pack with filter rules only: no module reads neighbours, so a
    // new reference to the judged block leaves the assessment fresh, as before module rules
    let rules = "  - name: f\n    select: type = claim and fields.ok = false\n    require: type = claim\n    severity: blocking\n    message: f message\n    fix: f fix\n";
    let mut w = world(rules, None);
    let p = propose_w(&mut w, "v");
    let reach = reach_names(&w, &p);
    assert!(reach.contains("w") && !reach.contains("o"), "{reach:?}");
    let (o, wid) = (w.id("o"), w.id("w"));
    write(
        &mut w.r,
        "x",
        json!([{"op": "update", "id": o, "body": format!("O ~!~ builds on {{{{{wid}}}}}.")}]),
    )
    .unwrap();
    land(&mut w.r, "x").unwrap();
    let l = approve(&mut w, &p).unwrap();
    assert_eq!(l.approved, p.r#ref);
}

#[test]
fn wck6_new_link_to_a_judged_block_moves_the_assessment() {
    let (mut w, p) = marked_world();
    let (o, wid) = (w.id("o"), w.id("w"));
    write(
        &mut w.r,
        "x",
        json!([{"op": "update", "id": o, "body": "O ~!~ holds.", "links": {"describes": [{"target": wid}]}}]),
    )
    .unwrap();
    land(&mut w.r, "x").unwrap();
    assert_moved_by(&mut w, &p, "o");
}

#[test]
fn wck6_new_child_of_a_reach_block_moves_the_assessment() {
    // a new claim under w's section, which the reach holds
    let (mut w, p) = marked_world();
    let sw = w.id("sw");
    let made = write(
        &mut w.r,
        "x",
        json!([{"op": "create", "type": "claim", "id": "$k", "parent": sw, "body": "K ~!~ holds."}]),
    )
    .unwrap();
    land(&mut w.r, "x").unwrap();
    let k = made.created["$k"].clone();
    let e = approve(&mut w, &p).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert!(
        e.message
            .ends_with(&format!(" changed {k} after its assessment")),
        "{}",
        e.message
    );
}

#[test]
fn wck6_truncated_neighbours_and_derived_inputs_bind_freshness() {
    // n1's text alone is over the input's 1 MiB, so it drops from w's input: the module does
    // not see its mark and passes, yet the reach holds n1
    let big = format!("~!~ {}", "x".repeat(1 << 20));
    let rules = mrule(
        "mark",
        "type = claim and fields.ok = false",
        MARKER,
        "    depth: 1\n",
        "advisory",
    );
    let mut w = world(&rules, Some(&big));
    let p = propose_w(&mut w, "v");
    assert!(warned(&p).is_empty(), "{:#?}", p.warnings);
    let reach = reach_names(&w, &p);
    assert!(reach.contains("n1"), "{reach:?}");
    land_edit(&mut w, "x", "n1");
    assert_moved_by(&mut w, &p, "n1");
    // the derived values of the judged block read past the input's depth: its doc
    let mut w = world(
        &mrule(
            "m",
            "type = claim and fields.ok = false",
            PASS,
            "    depth: 0\n",
            "advisory",
        ),
        None,
    );
    let p = propose_w(&mut w, "v");
    let reach = reach_names(&w, &p);
    assert!(reach.contains("dw"), "{reach:?}");
    let dw = w.id("dw");
    write(
        &mut w.r,
        "x",
        json!([{"op": "update", "id": dw, "fields": {"title": "Doc w, renamed"}}]),
    )
    .unwrap();
    land(&mut w.r, "x").unwrap();
    assert_moved_by(&mut w, &p, "dw");
}

#[test]
fn wck6_module_reach_at_and_past_existing_limit() {
    // pairs at the project root, so that no block has many neighbours: canon holds claim r<k>,
    // and the version writes claim c<k>, which references r<k>, and `pads` lone claims p<k>. A
    // module of depth 1 reads each r<k>; a depth of 0 does not.
    let rules = |depth: u8| {
        pack(&mrule(
            "m",
            "type = claim",
            PASS,
            &format!("    depth: {depth}\n"),
            "advisory",
        ))
    };
    let resolved = |depth: u8| {
        let mut set = rdy_schema::PackSet::with_builtin();
        set.add_yaml(&rules(depth)).unwrap();
        set.resolve("wck6").unwrap()
    };
    let (_d, mut r) = wck6_project(&mrule("m", "type = claim", PASS, "", "advisory"), "");
    write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "big", "title": "Big", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$w", "parent": "$s", "body": "W holds."}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let st = r.read();
    let canon = st.materialized(&Scope::Canon).unwrap().clone();
    let pref = ProposalRef {
        version: Hash::of(b"v"),
        stable: Hash::of(b"s"),
        n: 1,
    };
    let reach = |pack: &rdy_schema::Pack, pairs: usize, pads: usize| {
        let rows = reach_rows(pairs, pads);
        let before = canon.with(&rows.before);
        let projected = before.with(&rows.ops);
        let (bg, pg) = (
            rdy_graph::Graph::build_from(before.iter()),
            rdy_graph::Graph::build_from(projected.iter()),
        );
        rule_reach_for_tests(
            &pref,
            pack,
            (&before, &bg),
            (&projected, &pg),
            &rows.written,
        )
        .unwrap()
    };
    let deep = resolved(1);
    // each pair adds its two blocks, each pad its one
    let fixed = reach(&deep, 10, 0).unwrap().len() - 20;
    assert_eq!(reach(&deep, 20, 3).unwrap().len(), fixed + 43);
    let r10 = reach(&deep, 10, 0).unwrap();
    assert!(
        (0..10).all(|k| r10.contains(&nth("r", k)) && r10.contains(&nth("c", k))),
        "{r10:?}"
    );
    // at the bound: 100,000 blocks pass; one more is the existing "too large" issue
    let room = MAX_ROWS - fixed;
    let (pairs, pads) = (room / 2, room % 2);
    assert_eq!(reach(&deep, pairs, pads).unwrap().len(), MAX_ROWS);
    let e = reach(&deep, pairs, pads + 1).unwrap_err();
    assert_eq!(e.severity, Severity::Error);
    assert_eq!(e.code, E_VALIDATION);
    assert_eq!(e.message, format!("{pref} is too large to assess"));
    assert_eq!(e.fix, "split the work into smaller proposals");
    // depth 0 reads none of the referenced claims: the same world is well inside the bound
    let shallow = resolved(0);
    let got = reach(&shallow, pairs, pads + 1).unwrap();
    assert!(!got.contains(&nth("r", 0)));
    assert!(got.len() < MAX_ROWS, "{}", got.len());
    drop(st);
}

/// The claim of every large-row input, built field by field: it copies no live block, so its
/// bytes do not depend on the ids or the content a run generated. It has no parent, the
/// eight-digit order `k`, the given body, and no fields, links, pins or foreign part.
fn explicit_claim(bid: BlockId, k: usize, body: String) -> Block {
    Block {
        id: bid,
        block_type: "claim".to_string(),
        parent: None,
        order: format!("{k:08}"),
        fields: BTreeMap::new(),
        body,
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: ForeignParts::default(),
        read_identity: None,
    }
}

/// The large-row input of the module reach: `pairs` canon claims `r<k>` (`before`), and the
/// version's `pairs` claims `c<k>`, each referencing its `r<k>`, and `pads` lone claims `p<k>`
/// (`ops`, whose ids are `written`). All at the project root, so that no block has many
/// neighbours.
struct ReachRows {
    before: Vec<Op>,
    ops: Vec<Op>,
    written: BTreeSet<BlockId>,
}

fn reach_rows(pairs: usize, pads: usize) -> ReachRows {
    let at_root = |bid: BlockId, k: usize, body: String| Op::Create {
        block: explicit_claim(bid, k, body),
    };
    let before = (0..pairs)
        .map(|k| at_root(nth("r", k), k, format!("R{k} holds.")))
        .collect();
    let mut written = BTreeSet::new();
    let mut ops = vec![];
    for k in 0..pairs {
        written.insert(nth("c", k));
        ops.push(at_root(
            nth("c", k),
            k,
            format!("C{k} builds on {{{{{}}}}}.", nth("r", k)),
        ));
    }
    for k in 0..pads {
        written.insert(nth("p", k));
        ops.push(at_root(nth("p", k), k, format!("P{k} holds.")));
    }
    ReachRows {
        before,
        ops,
        written,
    }
}

/// The serialized bytes of the large-row input of `pairs` pairs and `pads` pads.
fn reach_row_bytes(pairs: usize, pads: usize) -> Vec<u8> {
    let rows = reach_rows(pairs, pads);
    serde_json::to_vec(&json!({"before": rows.before, "ops": rows.ops})).unwrap()
}

#[test]
fn r5903_reach_fixture_has_fixed_bytes() {
    // two projects with different generated ids: the large rows are the same bytes beside both
    let live = |name: &str| {
        let (_d, mut r) = wck6_project(&mrule("m", "type = claim", PASS, "", "advisory"), "");
        let made = write(&mut r, "seed", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "Big", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "id": "$w", "parent": "$s", "body": "W holds."}
        ]))
        .unwrap();
        land(&mut r, "seed").unwrap();
        (id(&made.created["$w"]), reach_row_bytes(20, 3))
    };
    let ((first, a), (second, b)) = (live("big-a"), live("big-b"));
    assert_ne!(first, second, "the projects generate different ids");
    assert_eq!(a, b, "the large rows do not depend on the project");
    // the explicit claim, byte for byte
    let one = explicit_claim(nth("c", 7), 7, "C7 holds.".to_string());
    assert_eq!(
        serde_json::to_string(&one).unwrap(),
        r#"{"id":"c-00007","block_type":"claim","parent":null,"order":"00000007","fields":{},"body":"C7 holds.","links":{},"pins":{}}"#
    );
    assert_eq!(one.foreign, ForeignParts::default());
    assert!(one.read_identity.is_none());
    // and the whole input of a fixed shape
    assert_eq!(
        Hash::of(&a).to_string(),
        "8fb3f51d9cf60a133598f740a29560083969dc4a9d7d019f6f8757740508e0a3",
        "the large-row input changed"
    );
}

/// The `n`th valid block id with prefix `p`, in order.
fn nth(p: &str, mut n: usize) -> BlockId {
    let mut body = [b'0'; 5];
    for c in body.iter_mut().rev() {
        *c = rdy_model::CROCKFORD[n % 32];
        n /= 32;
    }
    BlockId::parse(&format!("{p}-{}", std::str::from_utf8(&body).unwrap())).unwrap()
}

#[test]
fn wck6_changed_module_args_or_depth_moves_the_approval() {
    let rules = |extra: &str| mrule("m", "type = claim", PASS, extra, "advisory");
    for (from, to) in [
        (
            "    args:\n      level: one\n",
            "    args:\n      level: two\n",
        ),
        ("    depth: 1\n", "    depth: 2\n"),
    ] {
        let (d, mut r, _ss, ds) = stable_and_draft(&rules(from), LOOSE);
        put_j(&mut r, "a", json!([claim(&ds, "A.", json!({}))]), j_typo());
        let p = propose_as(&mut r, "a");
        assert!(p.ready, "{:#?}", p.issues);
        let next = rules(to);
        use_pack(d.path(), "wck6", &pack(&next), LOOSE);
        assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
        confirm_before_landing(&mut r, &approver(), &p);
        let e = r.land_report(&approver(), &p.id).unwrap_err();
        assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
        assert_eq!(
            e.message,
            format!(
                "proposal {} moved: the pack changed after its assessment",
                p.r#ref
            )
        );
    }
}

#[test]
fn wck6_host_fault_is_checks_error_or_propose_refusal() {
    let (_d, mut r, _ss, ds) =
        stable_and_draft(&mrule("m", "type = claim", PASS, "", "advisory"), "");
    // propose: the rule read error, as every read error of a propose, makes the version not
    // ready with the error as its one issue, and no assessment is stored
    put_j(&mut r, "p", json!([claim(&ds, "P.", json!({}))]), j_typo());
    answer_open_items(&mut r, &ag("p"), "p");
    host_fault_in_next_module_run_for_tests();
    let pv = r.propose(&ag("p"), "p", "p", Some(TEST_SUMMARY)).unwrap();
    assert!(!pv.ready);
    assert_eq!(pv.issues.len(), 1, "{:#?}", pv.issues);
    let issue = &pv.issues[0];
    assert_eq!(issue.severity, Severity::Error);
    assert_eq!(issue.code, E_INTERNAL);
    assert_eq!(issue.block, None);
    assert_eq!(
        issue.message,
        "the rule checks could not be read: check module host fault"
    );
    assert_eq!(issue.fix, "report this as a bug in rdy");
    assert!(pv.assessment.is_none(), "{pv:#?}");
    assert!(warned(&pv).is_empty());
    // the next propose runs the module again and is ready
    let pv = r.propose(&ag("p"), "p", "p", Some(TEST_SUMMARY)).unwrap();
    assert!(pv.ready, "{:#?}", pv.issues);
    // change: no rows and the error, and the write stands
    let e = Arc::new(Local::new(r));
    let mut st = mcp(&e);
    host_fault_in_next_module_run_for_tests();
    let v = wr(&mut st, "c", json!([claim(&ds, "C.", json!({}))]));
    assert_eq!(v["on_change"]["checks"], json!([]), "{v:#}");
    assert_eq!(
        v["on_change"]["checks_error"],
        json!("E_INTERNAL: the rule checks could not be read: check module host fault")
    );
    assert!(v["change"].is_string(), "{v:#}");
    let made = created(&v).pop().unwrap();
    assert!(
        e.read()
            .materialized(&Scope::View("c".into()))
            .unwrap()
            .get(&id(&made))
            .is_some()
    );
}
