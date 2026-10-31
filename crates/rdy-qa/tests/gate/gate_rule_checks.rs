//! ck5 (D-JQD5X, D-HZYK0, D-DSPMW, D-AJ0XQ, D-ME2HD, D-HAG73): pack rules at the `change` and
//! `propose` stages. At `change`, a write or view op reports the rule failures of the blocks it
//! changed and of the blocks one explicit link or pinned reference away, in `on_change.checks`,
//! and never refuses. At `propose`, readiness judges every block of the version's landing
//! projection: a blocking rule on stable content is an `E_RULE_CHECK` issue of item 6; any other
//! failure is a `W_RULE_CHECK` warning, stored on the card before `Policy`. The rules widen the
//! assessment's reach, so a landing that changes what a rule read stales the assessment, and
//! nothing reruns a rule at landing.
use rdy_engine::check::rules::{fail_next_rule_report_for_tests, rule_scans_for_tests};
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::{rdy_session, rdy_write};
use rdy_mcp::view_tools::rdy_view;
use rdy_model::{BlockId, Hash, Op};
use rdy_qa::fixtures::*;
use rdy_qa::intake::{PERSON, TEST_SUMMARY, answer_open_items, confirm_before_landing};
use rdy_qa::tensions::{ag, j_typo, put_j};
use rdy_schema::{Issue, Severity};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The pack every test extends: claims, sections and decisions with an `ok` and a `checked`
/// field, then `rules`.
fn pack(rules: &str) -> String {
    format!(
        "\
name: ck5
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

/// A project of pack `ck5` with `rules`, and `extra` in `.rdy.yaml`.
fn ck5_project(rules: &str, extra: &str) -> (tempfile::TempDir, Rdy) {
    let (d, r) = project();
    drop(r);
    rdy_qa::packs::use_pack(d.path(), "ck5", &pack(rules), extra);
    let r = Rdy::open(d.path()).unwrap();
    (d, r)
}

/// One rule, as the pack file writes it.
fn rule(name: &str, select: &str, require: &str, severity: &str) -> String {
    format!(
        "  - name: {name}\n    select: {select}\n    require: {require}\n    severity: {severity}\n    message: {name} message\n    fix: {name} fix\n"
    )
}

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

/// The (code, block) of each row of a reply's `on_change.checks`, in its order.
fn rows(v: &Value) -> Vec<(String, String)> {
    v["on_change"]["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("on_change.checks: {v:#}"))
        .iter()
        .map(|r| {
            (
                r["code"].as_str().unwrap().to_string(),
                r["block"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn blocks_of(rows: &[(String, String)]) -> BTreeSet<String> {
    rows.iter().map(|(_, b)| b.clone()).collect()
}

/// The canon of the change-stage tests, all in one draft doc with every block `ok: false`
/// except `a`: claim `a` (it references `r`), claim `b` (it references `a`), claim `z` (it
/// references `b`: two hops from `a`), decision `l` (it affects `a`), all in section `s` (which
/// is `a`'s parent), and decision `o` in its own section `t`, which affects `x`, a claim in `t`.
struct LinkWorld {
    _d: tempfile::TempDir,
    e: Arc<Local>,
    ids: BTreeMap<&'static str, String>,
}

impl LinkWorld {
    fn id(&self, name: &str) -> String {
        self.ids[name].clone()
    }
    fn set(&self, names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| self.id(n)).collect()
    }
}

fn link_world() -> LinkWorld {
    let all = rule(
        "ok-blocks",
        "type = claim or type = decision or type = section",
        "fields.ok = true",
        "advisory",
    );
    let (d, mut r) = ck5_project(&all, "");
    let w = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "links", "title": "Links", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S", "ok": false}},
        {"op": "create", "type": "claim", "id": "$r", "parent": "$s", "body": "R holds.", "fields": {"ok": false}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A builds on {{$r}}.", "fields": {"ok": true}},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}.", "fields": {"ok": false}},
        {"op": "create", "type": "claim", "id": "$z", "parent": "$s", "body": "Z builds on {{$b}}.", "fields": {"ok": false}},
        {"op": "create", "type": "decision", "id": "$l", "parent": "$s", "fields": {"statement": "L", "status": "proposed", "needs_content": false, "ok": false}, "links": {"affects": [{"target": "$a"}]}},
        {"op": "create", "type": "section", "id": "$t", "parent": "$d", "fields": {"title": "T", "ok": true}},
        {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds.", "fields": {"ok": false}},
        {"op": "create", "type": "decision", "id": "$o", "parent": "$t", "fields": {"statement": "O", "status": "proposed", "needs_content": false, "ok": true}, "links": {"affects": [{"target": "$x"}]}}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let mut ids = BTreeMap::new();
    for k in ["s", "r", "a", "b", "z", "l", "t", "x", "o"] {
        ids.insert(k, w.created[&format!("${k}")].clone());
    }
    LinkWorld {
        _d: d,
        e: Arc::new(Local::new(r)),
        ids,
    }
}

#[test]
fn ck5_change_checks_changed_and_one_link_away() {
    let w = link_world();
    let mut st = mcp(&w.e);
    // a write of a: its outgoing reference (r), its incoming reference (b) and its incoming
    // link (l) are one link away; its parent s and the two-hop z are not
    let v = wr(
        &mut st,
        "a",
        json!([{"op": "update", "id": w.id("a"), "body": "A builds on {{".to_string() + &w.id("r") + "}}, now."}]),
    );
    let got = rows(&v);
    assert!(got.iter().all(|(c, _)| c == "W_RULE_CHECK"), "{got:?}");
    assert_eq!(blocks_of(&got), w.set(&["r", "b", "l"]), "{v:#}");
    // the rows come in rule then block order, each with its exact words
    let row = &v["on_change"]["checks"][0];
    let first = blocks_of(&got).into_iter().next().unwrap();
    assert_eq!(
        row,
        &json!({"rule": "ok-blocks", "code": "W_RULE_CHECK", "block": first,
                "message": format!("rule ok-blocks fails on {first}: ok-blocks message"),
                "fix": "ok-blocks fix"})
    );
    // an outgoing explicit link: a new decision that affects x reports x (the decision itself
    // passes)
    let v = wr(
        &mut st,
        "o",
        json!([{"op": "create", "type": "decision", "parent": w.id("t"),
                "fields": {"statement": "O2", "status": "proposed", "needs_content": false, "ok": true},
                "links": {"affects": [{"target": w.id("x")}]}}]),
    );
    assert_eq!(blocks_of(&rows(&v)), w.set(&["x"]), "{v:#}");
    // a delete of a: before the delete its neighbours were r, b and l; a itself has no row
    let v = wr(&mut st, "d", json!([{"op": "delete", "id": w.id("a")}]));
    assert_eq!(blocks_of(&rows(&v)), w.set(&["r", "b", "l"]), "{v:#}");
    let deleting = v["change"].as_str().unwrap().to_string();
    // an exclude brings a back: the same reach, before and after
    let v = rdy_view(
        &mut st,
        p(json!({"op": "exclude", "view": "d", "changes": [deleting]})),
    )
    .unwrap();
    assert_eq!(blocks_of(&rows(&v)), w.set(&["r", "b", "l"]), "{v:#}");
    // an include of the delete into another view
    let v = rdy_view(
        &mut st,
        p(json!({"op": "include", "view": "inc", "changes": [deleting]})),
    )
    .unwrap();
    assert_eq!(blocks_of(&rows(&v)), w.set(&["r", "b", "l"]), "{v:#}");
    // an undo of that include
    let v = rdy_view(&mut st, p(json!({"op": "undo", "view": "inc"}))).unwrap();
    assert_eq!(blocks_of(&rows(&v)), w.set(&["r", "b", "l"]), "{v:#}");
    // a view op with no materialized delta: an empty list
    let v = rdy_view(&mut st, p(json!({"op": "create", "name": "empty"}))).unwrap();
    assert!(v.get("on_change").is_none() || rows(&v).is_empty(), "{v:#}");
}

#[test]
fn ck5_change_rules_never_refuse_a_write() {
    let rules = rule(
        "checked-claims",
        "type = claim",
        "fields.checked = true",
        "blocking",
    ) + &rule("ok-claims", "type = claim", "fields.ok = true", "advisory");
    let (_d, mut r) = ck5_project(&rules, "");
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$st", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$ss", "parent": "$st", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$ss", "body": "C holds.", "fields": {"checked": true, "ok": true}},
            {"op": "create", "type": "doc", "id": "$dr", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$ds", "parent": "$dr", "fields": {"title": "Ideas"}}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let c = seeded.created["$c"].clone();
    let ds = seeded.created["$ds"].clone();
    let e = Arc::new(Local::new(r));
    // a blocking failure on stable content: E_RULE_CHECK, and the write is durable
    let v = rdy_engine::consequence::write_and_report(&*e, "a1", None, {
        let c = c.clone();
        move |r| {
            rdy_qa::tensions::try_put_j(
                r,
                "a1",
                json!([{"op": "update", "id": c, "fields": {"checked": false}}]),
                j_typo(),
            )
        }
    })
    .unwrap();
    assert_eq!(
        rows(&v),
        vec![("E_RULE_CHECK".to_string(), c.clone())],
        "{v:#}"
    );
    let stored = e
        .read()
        .materialized(&Scope::View("a1".into()))
        .unwrap()
        .get(&id(&c))
        .unwrap()
        .fields
        .get("checked")
        .cloned();
    assert_eq!(stored, Some(rdy_model::FieldValue::Bool(false)), "durable");
    // a blocking and an advisory failure on a draft: both W_RULE_CHECK
    let mut st = mcp(&e);
    let v = wr(
        &mut st,
        "a",
        json!([{"op": "create", "type": "claim", "parent": ds, "body": "D holds.", "fields": {"checked": false, "ok": false}}]),
    );
    let made = v["created"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let got: Vec<(String, String, String)> = v["on_change"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["rule"].as_str().unwrap().to_string(),
                r["code"].as_str().unwrap().to_string(),
                r["block"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("checked-claims".into(), "W_RULE_CHECK".into(), made.clone()),
            ("ok-claims".into(), "W_RULE_CHECK".into(), made.clone()),
        ],
        "{v:#}"
    );
    // an injected report failure: no rows, the error, and the write and its tension keys stand
    fail_next_rule_report_for_tests();
    let v = wr(
        &mut st,
        "a",
        json!([{"op": "update", "id": made, "body": "D holds, now."}]),
    );
    assert_eq!(v["on_change"]["checks"], json!([]), "{v:#}");
    assert_eq!(
        v["on_change"]["checks_error"],
        json!("E_INTERNAL: the rule checks could not be read (test)")
    );
    assert!(v["on_change"]["tensions"].is_array(), "{v:#}");
    assert!(v["on_change"]["resolved"].is_array(), "{v:#}");
    assert!(v["change"].is_string(), "{v:#}");
    let body = e
        .read()
        .materialized(&Scope::View("a".into()))
        .unwrap()
        .get(&id(&made))
        .unwrap()
        .body
        .clone();
    assert_eq!(body, "D holds, now.");
}

/// A version's rule warnings, as (block, message) pairs.
fn warned(p: &ProposalInfo) -> Vec<(String, String)> {
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
            )
        })
        .collect()
}

fn rule_issues(p: &ProposalInfo) -> Vec<&Issue> {
    p.issues.iter().filter(|i| i.code == E_RULE_CHECK).collect()
}

#[test]
fn ck5_propose_rules_use_the_net_subset_candidate() {
    let all = rule("ok-claims", "type = claim", "fields.ok = true", "advisory");
    let (_d, mut r) = ck5_project(&all, "");
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
        {"op": "create", "type": "claim", "id": "$old", "parent": "$s", "body": "Old.", "fields": {"ok": false}}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let s = seeded.created["$s"].clone();
    let old = seeded.created["$old"].clone();
    // a subset: the change named, not the view's other edit
    let mine = write(&mut r, "v", json!([{"op": "create", "type": "claim", "id": "$m", "parent": s, "body": "Mine.", "fields": {"ok": false}}])).unwrap();
    let other = write(&mut r, "v", json!([{"op": "create", "type": "claim", "id": "$o", "parent": s, "body": "Other.", "fields": {"ok": false}}])).unwrap();
    let m = mine.created["$m"].clone();
    let o = other.created["$o"].clone();
    let pv = r
        .propose_subset(
            &writer(),
            "v",
            "subset",
            Some(std::slice::from_ref(&mine.change)),
            Some(TEST_SUMMARY),
        )
        .unwrap();
    let blocks: BTreeSet<String> = warned(&pv).into_iter().map(|(b, _)| b).collect();
    assert!(blocks.contains(&m) && blocks.contains(&old), "{blocks:?}");
    assert!(
        !blocks.contains(&o),
        "the unselected edit is absent: {blocks:?}"
    );
    // a deleted failure disappears
    write(&mut r, "del", json!([{"op": "delete", "id": old}])).unwrap();
    let pd = r
        .propose(&writer(), "del", "del", Some(TEST_SUMMARY))
        .unwrap();
    assert!(
        !warned(&pd).iter().any(|(b, _)| *b == old),
        "{:?}",
        warned(&pd)
    );
    // a later repair in the selected changes clears an earlier failure
    let made = write(&mut r, "fix", json!([{"op": "create", "type": "claim", "id": "$f", "parent": s, "body": "F.", "fields": {"ok": false}}])).unwrap();
    let f = made.created["$f"].clone();
    write(
        &mut r,
        "fix",
        json!([{"op": "update", "id": f, "fields": {"ok": true}}]),
    )
    .unwrap();
    let pf = r
        .propose(&writer(), "fix", "fix", Some(TEST_SUMMARY))
        .unwrap();
    assert!(
        !warned(&pf).iter().any(|(b, _)| *b == f),
        "{:?}",
        warned(&pf)
    );
}

/// A stable doc `spec` with section `ss`, and a draft doc `notes` with section `ds`, landed.
fn stable_and_draft(rules: &str, extra: &str) -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = ck5_project(rules, extra);
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

/// `n` claims under `parent` in view `view`, each `ok: false`, written as agent `view`.
fn failing_claims(r: &mut Rdy, view: &str, parent: &str, n: usize) {
    let ops: Vec<Value> = (0..n)
        .map(|k| json!({"op": "create", "type": "claim", "parent": parent, "body": format!("Claim {k}."), "fields": {"ok": false}}))
        .collect();
    put_j(r, view, Value::Array(ops), j_typo());
}

fn propose_as(r: &mut Rdy, view: &str) -> ProposalInfo {
    answer_open_items(r, &ag(view), view);
    r.propose(&ag(view), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"))
}

#[test]
fn ck5_rule_severity_uses_effective_stability() {
    let rules = rule("ok-claims", "type = claim", "fields.ok = true", "blocking")
        + &rule(
            "soft-claims",
            "type = claim and has fields.checked",
            "fields.ok = true",
            "advisory",
        );
    let (_d, mut r, ss, ds) = stable_and_draft(&rules, "");
    // blocking on stable content: item 6, not ready, with the exact words
    put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": ss, "body": "C.", "fields": {"ok": false}}]),
        j_typo(),
    );
    let p = propose_as(&mut r, "a1");
    assert!(!p.ready);
    let issues = rule_issues(&p);
    assert_eq!(issues.len(), 1, "{:#?}", p.issues);
    let c = issues[0].block.as_ref().unwrap().to_string();
    assert_eq!(issues[0].severity, Severity::Error);
    assert_eq!(
        issues[0].message,
        format!("rule ok-claims fails on {c}: ok-claims message")
    );
    assert_eq!(issues[0].fix, "ok-claims fix");
    assert_eq!(p.checks_omitted, None);
    // blocking on a draft, and advisory: warnings, and the version is ready
    put_j(
        &mut r,
        "a2",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": ds, "body": "D.", "fields": {"ok": false, "checked": true}}]),
        j_typo(),
    );
    let p = propose_as(&mut r, "a2");
    assert!(p.ready, "{:#?}", p.issues);
    let d = warned(&p)[0].0.clone();
    assert_eq!(
        warned(&p),
        vec![
            (
                d.clone(),
                format!("rule ok-claims fails on {d}: ok-claims message")
            ),
            (
                d.clone(),
                format!("rule soft-claims fails on {d}: soft-claims message")
            ),
        ]
    );
    assert!(
        p.warnings
            .iter()
            .filter(|w| w.code == W_RULE_CHECK)
            .all(|w| w.severity == Severity::Warning)
    );
    // the cut: 256 blocking rows are all kept; at 257 one is counted, and a kept blocking row
    // still makes the version not ready
    let (_d2, mut r, ss, ds) = stable_and_draft(
        &rule("ok-claims", "type = claim", "fields.ok = true", "blocking"),
        "",
    );
    failing_claims(&mut r, "b1", &ss, 256);
    let p = propose_as(&mut r, "b1");
    assert_eq!(rule_issues(&p).len(), 256);
    assert_eq!(p.checks_omitted, None);
    failing_claims(&mut r, "b2", &ss, 257);
    let p = propose_as(&mut r, "b2");
    assert!(!p.ready);
    assert_eq!(rule_issues(&p).len(), 256);
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["checks_omitted"], json!({"blocking": 1, "warnings": 0}));
    // 257 warnings on a draft: 256 kept, one counted, ready, and the card says so before Policy
    failing_claims(&mut r, "b3", &ds, 257);
    let p = propose_as(&mut r, "b3");
    assert!(p.ready, "{:#?}", p.issues);
    assert_eq!(warned(&p).len(), 256);
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["checks_omitted"], json!({"blocking": 0, "warnings": 1}));
    let facts = stored_facts(&r, &p);
    let n = fact_names(&facts);
    assert_eq!(n.iter().filter(|x| **x == "Rule check").count(), 256);
    let at = n.iter().position(|x| *x == "Rule checks omitted").unwrap();
    assert_eq!(facts[at], "Rule checks omitted: 0 blocking, 1 advisory");
    assert_eq!(n[at + 1], "Policy");
    assert_eq!(n.last(), Some(&"Policy"));
}

fn stored_facts(r: &Rdy, p: &ProposalInfo) -> Vec<String> {
    stored(r, p).card.facts
}

fn stored(r: &Rdy, p: &ProposalInfo) -> rdy_model::assessment::Assessment {
    stored_version(r, &p.id.parse().unwrap())
}

fn stored_version(r: &Rdy, version: &Hash) -> rdy_model::assessment::Assessment {
    let st = r.read();
    let h = st
        .reader()
        .assessment_of_proposal(version, st.seq())
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
fn ck5_rules_use_m3f_coverage_suspect_and_tension_inputs() {
    use rdy_engine::filter::{FilterContext, FilterInputs, candidate_for_tests};
    let rules = rule("low-coverage", "type = claim", "coverage > 100", "advisory")
        + &rule("suspects", "type = claim", "suspect = false", "advisory")
        + &rule("tensions", "type = claim", "tension = false", "advisory");
    let (_d, mut r) = ck5_project(&rules, "");
    let seeded = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
        {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "A holds 30 days."},
        {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "B builds on {{$a}}."},
        {"op": "create", "type": "claim", "id": "$e", "parent": "$s", "body": "Every invoice is paid within thirty days."},
        {"op": "create", "type": "claim", "id": "$f", "parent": "$s", "body": "Every invoice is paid within thirty days."},
        {"op": "create", "type": "claim", "id": "$g", "parent": "$s", "body": "Every refund is sent within ten days."},
        {"op": "create", "type": "claim", "id": "$h", "parent": "$s", "body": "Every refund is sent within ten days."}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let a = seeded.created["$a"].clone();
    let [e, f, g, h] = ["$e", "$f", "$g", "$h"].map(|k| id(&seeded.created[k]));
    // M3f's own canon query, independently of readiness: both duplicate pairs are in tension
    let in_tension: BTreeSet<BlockId> = r
        .read()
        .query(&Scope::Canon, "type = claim and tension = true")
        .unwrap()
        .matches
        .iter()
        .map(|m| id(&m.id))
        .collect();
    assert_eq!(
        in_tension,
        BTreeSet::from([e.clone(), f.clone(), g.clone(), h.clone()])
    );
    // a's change makes b suspect in the candidate; h's rewrite ends the g/h duplicate, while
    // e/f's canon tension survives the version untouched
    write(
        &mut r,
        "v",
        json!([{"op": "update", "id": a, "body": "A holds 60 days."},
               {"op": "update", "id": h.to_string(), "body": "Each refund goes out within a week."}]),
    )
    .unwrap();
    let pv = r.propose(&writer(), "v", "v", Some(TEST_SUMMARY)).unwrap();
    let st = r.read();
    let c = rdy_engine::readiness::Candidate::stored(
        &st,
        r.resolve_proposal(&pv.r#ref).unwrap(),
        st.reader().proposal(&pv.id.parse().unwrap()).unwrap(),
    )
    .unwrap();
    let inputs = rdy_engine::readiness::rule_inputs_for_tests(&st, &c).unwrap();
    // the suspects are M3f's: the candidate graph's suspects
    let changes: Vec<Hash> = st
        .reader()
        .proposal(&pv.id.parse().unwrap())
        .unwrap()
        .changes;
    let (m, graph) = candidate_for_tests(&st, &changes).unwrap();
    let want: BTreeSet<BlockId> = graph.suspects().into_iter().map(|s| s.block).collect();
    assert_eq!(inputs.suspects, want);
    assert!(inputs.suspects.contains(&id(&seeded.created["$b"])));
    // the evaluator, given those inputs, gives exactly the propose's warnings
    let ctx = FilterContext::from_parts(FilterInputs {
        pack: st.pack(),
        m: &m,
        graph: &graph,
        areas: st.area_matcher(),
        suspects: &inputs.suspects,
        tensions: &inputs.tensions,
        coverage: &inputs.coverage,
    });
    let compiled = rdy_schema::compile_rules(st.pack()).unwrap();
    let mut want: Vec<(String, String)> = vec![];
    for rl in &compiled {
        for b in m.ids() {
            if ctx.matches(&rl.select, b) && !ctx.matches(&rl.require, b) {
                want.push((
                    b.to_string(),
                    format!(
                        "rule {} fails on {b}: {} message",
                        rl.rule.name, rl.rule.name
                    ),
                ));
            }
        }
    }
    assert_eq!(warned(&pv), want);
    // the tensions are the projected candidate's: canon's e/f tension survives the version and
    // enters the rule inputs; the g/h tension the version ends does not
    assert!(
        inputs.tensions.contains(&e) && inputs.tensions.contains(&f),
        "{:?}",
        inputs.tensions
    );
    assert!(
        !inputs.tensions.contains(&g) && !inputs.tensions.contains(&h),
        "{:?}",
        inputs.tensions
    );
    let calm: BTreeSet<String> = warned(&pv)
        .into_iter()
        .filter(|(_, m)| m.starts_with("rule tensions fails on "))
        .map(|(b, _)| b)
        .collect();
    assert_eq!(calm, BTreeSet::from([e.to_string(), f.to_string()]));
    // the coverage is the intrinsic score of each block: no machine-local source fact is in it
    assert!(!inputs.coverage.is_empty());
    assert!(inputs.coverage.values().all(|s| *s <= 100));
}

#[test]
fn ck5_rules_see_canon_tensions_the_version_leaves() {
    // review fix round 1 (L6/I7): a blocking rule on tension reads the complete projected
    // candidate's tensions, so a canon tension the version neither causes nor ends still fails
    // it once the version makes its blocks stable
    let calm = rule("calm-claims", "type = claim", "tension = false", "blocking");
    let (_d, mut r) = ck5_project(&calm, "");
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Every invoice is paid within thirty days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Every invoice is paid within thirty days."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let doc = seeded.created["$d"].clone();
    let (a, b) = (seeded.created["$a"].clone(), seeded.created["$b"].clone());
    let in_tension: BTreeSet<String> = r
        .read()
        .query(&Scope::Canon, "type = claim and tension = true")
        .unwrap()
        .matches
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(in_tension, BTreeSet::from([a.clone(), b.clone()]));
    let stable = json!({"op": "update", "id": doc, "fields": {"name": "notes", "title": "Notes", "doc_type": "overview", "lifecycle": "stable"}});
    // only the doc changes: both claims fail the blocking rule, item 6, not ready
    put_j(&mut r, "v", json!([stable.clone()]), j_typo());
    let p = propose_as(&mut r, "v");
    assert!(!p.ready, "{:#?}", p.issues);
    let failing: BTreeSet<String> = rule_issues(&p)
        .iter()
        .map(|i| i.block.as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        failing,
        BTreeSet::from([a.clone(), b.clone()]),
        "{:#?}",
        p.issues
    );
    assert_eq!(
        rule_issues(&p)[0].message,
        format!(
            "rule calm-claims fails on {}: calm-claims message",
            failing.iter().next().unwrap()
        )
    );
    // a version that also ends the duplicate leaves no canon tension behind: ready
    put_j(
        &mut r,
        "w",
        json!([stable, {"op": "update", "id": b, "body": "Each invoice settles in a month."}]),
        j_typo(),
    );
    let p = propose_as(&mut r, "w");
    assert!(rule_issues(&p).is_empty(), "{:#?}", p.issues);
    assert!(p.ready, "{:#?}", p.issues);
}

#[test]
fn ck5_rules_see_the_surviving_pair_of_a_rewritten_group() {
    // review fix round 2 (L6/I7): three duplicate claims in canon, one rewritten by the
    // version. The canon group tension involves the rewritten claim, but the landing still
    // leaves the other two duplicated: the candidate's tensions hold that surviving pair, so a
    // blocking rule on tension fails on both of them and not on the rewritten claim.
    let calm = rule("calm-claims", "type = claim", "tension = false", "blocking");
    let (_d, mut r) = ck5_project(&calm, "");
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$s", "body": "Every invoice is paid within thirty days."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$s", "body": "Every invoice is paid within thirty days."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Every invoice is paid within thirty days."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let doc = seeded.created["$d"].clone();
    let [a, b, c] = ["$a", "$b", "$c"].map(|k| seeded.created[k].clone());
    let canon_in_tension = |r: &Rdy| -> BTreeSet<String> {
        r.read()
            .query(&Scope::Canon, "type = claim and tension = true")
            .unwrap()
            .matches
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    assert_eq!(
        canon_in_tension(&r),
        BTreeSet::from([a.clone(), b.clone(), c.clone()])
    );
    let rewrite = json!({"op": "update", "id": c, "body": "Each invoice settles in a month."});
    // the version makes the doc stable and rewrites c: a and b still fail, c does not
    put_j(
        &mut r,
        "v",
        json!([
            {"op": "update", "id": doc, "fields": {"name": "notes", "title": "Notes", "doc_type": "overview", "lifecycle": "stable"}},
            rewrite.clone()
        ]),
        j_typo(),
    );
    let p = propose_as(&mut r, "v");
    assert!(!p.ready, "{:#?}", p.issues);
    let failing: BTreeSet<String> = rule_issues(&p)
        .iter()
        .map(|i| i.block.as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        failing,
        BTreeSet::from([a.clone(), b.clone()]),
        "{:#?}",
        p.issues
    );
    for i in rule_issues(&p) {
        let blk = i.block.as_ref().unwrap();
        assert_eq!(
            i.message,
            format!("rule calm-claims fails on {blk}: calm-claims message")
        );
    }
    // independently of readiness: land the same rewrite on its own (the doc stays draft, so
    // the rule only warns) and M3f's canon query gives exactly the surviving pair
    put_j(&mut r, "w", json!([rewrite]), j_typo());
    land(&mut r, "w").unwrap();
    assert_eq!(canon_in_tension(&r), BTreeSet::from([a, b]));
}

/// The claims M3f's canon query `type = claim and tension = true` gives in `r`'s canon.
fn claims_in_canon_tension(r: &Rdy) -> BTreeSet<String> {
    r.read()
        .query(&Scope::Canon, "type = claim and tension = true")
        .unwrap()
        .matches
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// The blocks of the tension input the pack rules of version `p` read at propose.
fn tension_input(r: &Rdy, p: &ProposalInfo) -> BTreeSet<BlockId> {
    let st = r.read();
    let c = rdy_engine::readiness::Candidate::stored(
        &st,
        r.resolve_proposal(&p.r#ref).unwrap(),
        st.reader().proposal(&p.id.parse().unwrap()).unwrap(),
    )
    .unwrap();
    rdy_engine::readiness::rule_inputs_for_tests(&st, &c)
        .unwrap()
        .tensions
}

/// The blocks of `p`'s `E_RULE_CHECK` issues, each with its exact message.
fn failing_blocks(p: &ProposalInfo, rule: &str) -> BTreeSet<String> {
    rule_issues(p)
        .iter()
        .map(|i| {
            let blk = i.block.as_ref().unwrap().to_string();
            assert_eq!(
                i.message,
                format!("rule {rule} fails on {blk}: {rule} message")
            );
            blk
        })
        .collect()
}

/// The canon of the deprecated-reference pins, in a draft doc: claim `x` in section `old`,
/// which is deprecated, and claim `y` in section `live`, which references `x`. Canon holds the
/// deprecated-reference tension of `y` and `x`. Gives the doc, `x` and `y`.
fn deprecated_reference_world(r: &mut Rdy) -> (String, String, String) {
    let seeded = put_j(
        r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$live", "parent": "$d", "fields": {"title": "Live"}},
            {"op": "create", "type": "section", "id": "$old", "parent": "$d", "fields": {"title": "Old", "lifecycle": "deprecated"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$old", "body": "X was the old way."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$live", "body": "Y builds on {{$x}}."}
        ]),
        j_typo(),
    );
    land(r, "seed").unwrap();
    let [doc, x, y] = ["$d", "$x", "$y"].map(|k| seeded.created[k].clone());
    assert_eq!(
        claims_in_canon_tension(r),
        BTreeSet::from([x.clone(), y.clone()])
    );
    (doc, x, y)
}

/// The op that makes doc `doc` (named `notes`) stable.
fn notes_stable(doc: &str) -> Value {
    json!({"op": "update", "id": doc, "fields": {"name": "notes", "title": "Notes", "doc_type": "overview", "lifecycle": "stable"}})
}

#[test]
fn ck5_candidate_keeps_a_retained_deprecated_reference() {
    // review fix round 3 (L6/I7): canon has live claim y that references deprecated x. A
    // version that edits y's text and keeps the reference leaves that standing condition in
    // the candidate (the in-flight form judges only the references an edit adds), so a
    // blocking rule on tension fails on y once the version makes the doc stable.
    let calm = rule("calm-claims", "type = claim", "tension = false", "blocking");
    let (_d, mut r) = ck5_project(&calm, "");
    let (doc, x, y) = deprecated_reference_world(&mut r);
    let edit = json!({"op": "update", "id": y, "body": format!("Y still builds on {{{{{x}}}}}, with care.")});
    put_j(
        &mut r,
        "v",
        json!([notes_stable(&doc), edit.clone()]),
        j_typo(),
    );
    let p = propose_as(&mut r, "v");
    assert!(!p.ready, "{:#?}", p.issues);
    // y is stable content: item 6; x is deprecated content, so its failure only warns
    assert_eq!(
        failing_blocks(&p, "calm-claims"),
        BTreeSet::from([y.clone()]),
        "{:#?}",
        p.issues
    );
    let input = tension_input(&r, &p);
    assert!(
        input.contains(&id(&y)) && input.contains(&id(&x)),
        "{input:?}"
    );
    // independently of readiness: land the same edit on its own (the doc stays draft, so the
    // rule only warns) and M3f's canon query still gives both claims
    put_j(&mut r, "w", json!([edit]), j_typo());
    land(&mut r, "w").unwrap();
    assert_eq!(claims_in_canon_tension(&r), BTreeSet::from([x, y]));
}

#[test]
fn ck5_candidate_drops_a_removed_deprecated_reference() {
    // review fix round 3 (L6/I7): the same canon, and a version whose edit of y takes the
    // reference out. The condition ends with the landing: the candidate holds no tension on y
    // or x, and the blocking rule passes.
    let calm = rule("calm-claims", "type = claim", "tension = false", "blocking");
    let (_d, mut r) = ck5_project(&calm, "");
    let (doc, x, y) = deprecated_reference_world(&mut r);
    let edit = json!({"op": "update", "id": y, "body": "Y stands alone now."});
    put_j(
        &mut r,
        "v",
        json!([notes_stable(&doc), edit.clone()]),
        j_typo(),
    );
    let p = propose_as(&mut r, "v");
    assert!(rule_issues(&p).is_empty(), "{:#?}", p.issues);
    assert!(p.ready, "{:#?}", p.issues);
    let input = tension_input(&r, &p);
    assert!(
        !input.contains(&id(&y)) && !input.contains(&id(&x)),
        "{input:?}"
    );
    // independently of readiness: land the same edit on its own; canon holds no tension
    put_j(&mut r, "w", json!([edit]), j_typo());
    land(&mut r, "w").unwrap();
    assert_eq!(claims_in_canon_tension(&r), BTreeSet::new());
}

#[test]
fn ck5_candidate_keeps_a_standing_drift_of_an_edited_block() {
    // review fix round 3 (L6/I7), the same gap in restatement drift: canon holds a drift of x
    // from y (an edit of x took out text that y restates). A version that edits x again, away
    // from that text, keeps the drift: only an edit of y resolves it (D-S46DE). The in-flight
    // form judges only the new edit, from x's current text. The candidate holds the drift, so
    // the blocking rule fails on x and y once the version makes the doc stable.
    let calm = rule("calm-claims", "type = claim", "tension = false", "blocking");
    let (_d, mut r) = ck5_project(&calm, "");
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "The retry limit is five seconds for every queued job in the billing system."},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "The retry limit is five seconds for every queued job in the billing system today."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let [doc, x, y] = ["$d", "$x", "$y"].map(|k| seeded.created[k].clone());
    assert_eq!(claims_in_canon_tension(&r), BTreeSet::new());
    put_j(
        &mut r,
        "drift",
        json!([{"op": "update", "id": x, "body": "The retry limit is ten seconds for every queued job in the billing system today."}]),
        j_typo(),
    );
    land(&mut r, "drift").unwrap();
    assert_eq!(
        claims_in_canon_tension(&r),
        BTreeSet::from([x.clone(), y.clone()])
    );
    let edit = json!({"op": "update", "id": x, "body": "The retry limit is ten seconds for every queued job in the billing system today, as agreed."});
    put_j(
        &mut r,
        "v",
        json!([notes_stable(&doc), edit.clone()]),
        j_typo(),
    );
    let p = propose_as(&mut r, "v");
    assert!(!p.ready, "{:#?}", p.issues);
    assert_eq!(
        failing_blocks(&p, "calm-claims"),
        BTreeSet::from([x.clone(), y.clone()]),
        "{:#?}",
        p.issues
    );
    // independently of readiness: land the same edit on its own (the doc stays draft) and
    // M3f's canon query still gives both claims
    put_j(&mut r, "w", json!([edit]), j_typo());
    land(&mut r, "w").unwrap();
    assert_eq!(claims_in_canon_tension(&r), BTreeSet::from([x, y]));
}

#[test]
fn ck5_rule_warning_is_stored_before_policy() {
    let all = rule("ok-claims", "type = claim", "fields.ok = true", "advisory");
    let (_d, mut r, _ss, ds) = stable_and_draft(&all, "");
    put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": ds, "body": "C.", "fields": {"ok": false}}]),
        j_typo(),
    );
    let p = propose_as(&mut r, "a1");
    assert!(p.ready, "{:#?}", p.issues);
    let (c, message) = warned(&p)[0].clone();
    let w = p.warnings.iter().find(|w| w.code == W_RULE_CHECK).unwrap();
    assert_eq!(w.fix, "ok-claims fix");
    let value = format!("{message}; fix: ok-claims fix");
    let facts = stored_facts(&r, &p);
    let n = fact_names(&facts);
    let at = n.iter().position(|x| *x == "Rule check").unwrap();
    assert_eq!(facts[at], format!("Rule check: {value}"));
    assert_eq!(n[at + 1], "Policy");
    assert_eq!(n.last(), Some(&"Policy"));
    // a later read keeps the assessment's warning, though the rule passes on canon now
    put_j(
        &mut r,
        "fix",
        json!([{"op": "create", "type": "claim", "parent": ds, "body": "E.", "fields": {"ok": true}}]),
        j_typo(),
    );
    land(&mut r, "fix").unwrap();
    let item = r
        .read()
        .pending(&rdy_engine::pending::PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.r#ref == p.r#ref)
        .expect("the approve card");
    let shown: Vec<(String, String)> = item
        .facts
        .iter()
        .map(|f| (f.name.to_string(), f.value.clone()))
        .collect();
    assert!(shown.contains(&("Rule check".into(), value)), "{shown:#?}");
    assert_eq!(shown.last().map(|f| f.0.as_str()), Some("Policy"));
    let _ = c;
}

#[test]
fn ck5_existing_on_change_keys_survive() {
    let (_d, mut r) = project();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let e = Arc::new(Local::new(r));
    let mut st = mcp(&e);
    let before = e.read();
    let v = wr(
        &mut st,
        "a",
        json!([{"op": "update", "id": claim, "body": "one, now"},
               {"op": "create", "type": "claim", "parent": section, "body": "two"}]),
    );
    let after = e.read();
    let on = v["on_change"].as_object().unwrap();
    let keys: BTreeSet<&str> = on.keys().map(String::as_str).collect();
    assert!(
        keys.is_superset(&BTreeSet::from(["tensions", "resolved", "checks"])),
        "{v:#}"
    );
    assert_eq!(on["checks"], json!([]));
    let delta = rdy_engine::tensions::tension_delta(&before, &after, None).unwrap();
    let want = rdy_engine::tensions::delta::on_change(&before, &after, "a", None, &delta).unwrap();
    let mut old = serde_json::to_value(&want).unwrap();
    old["checks"] = json!([]);
    assert_eq!(v["on_change"], old);
}

#[test]
fn ck5_rules_see_accepted_status_and_rederived_keys() {
    let rules = rule(
        "live-ok",
        "type = decision and status = live",
        "fields.ok = true",
        "blocking",
    ) + &rule(
        "second-guide",
        "key = guide-2",
        "fields.title = Nope",
        "advisory",
    );
    let (_d, mut r, ss, ds) = stable_and_draft(&rules, "");
    // a newly accepted decision is live in the projection: the blocking rule refuses it
    put_j(
        &mut r,
        "a1",
        json!([{"op": "create", "type": "decision", "parent": ss,
        "fields": {"statement": "Use the rule", "status": "proposed", "needs_content": false, "ok": false}}]),
        j_typo(),
    );
    let p = propose_as(&mut r, "a1");
    assert!(!p.ready);
    let issues = rule_issues(&p);
    assert_eq!(issues.len(), 1, "{:#?}", p.issues);
    assert!(
        issues[0].message.starts_with("rule live-ok fails on D-"),
        "{issues:#?}"
    );
    // ok: the same decision is ready
    put_j(
        &mut r,
        "a2",
        json!([{"op": "create", "type": "decision", "parent": ss,
        "fields": {"statement": "Keep the rule", "status": "proposed", "needs_content": false, "ok": true}}]),
        j_typo(),
    );
    let p = propose_as(&mut r, "a2");
    assert!(p.ready, "{:#?}", p.issues);
    // a key another block took: the rule reads the key the version gets, not the one its
    // author's doc holds
    let (kd, mut kr) = ck5_project("  []\n", "");
    kr.new_doc(
        &writer(),
        "k1",
        "engineering-handbook/onboarding/overview",
        None,
        Some("O"),
    )
    .unwrap();
    let mine = kr
        .new_doc(
            &writer(),
            "k2",
            "product-handbook/onboarding/overview",
            None,
            Some("O"),
        )
        .unwrap();
    land(&mut kr, "k1").unwrap();
    let held = {
        let st = kr.read();
        let m = st.materialized(&Scope::Canon).unwrap();
        let doc = m
            .iter()
            .find(|b| b.block_type == "doc" && b.field_str("title") == Some("O"))
            .unwrap()
            .clone();
        (
            doc.id.to_string(),
            doc.field_str("key").unwrap().to_string(),
        )
    };
    let theirs = kr
        .read()
        .materialized(&Scope::View("k2".into()))
        .unwrap()
        .iter()
        .filter(|b| b.block_type == "doc" && b.field_str("key") == Some(held.1.as_str()))
        .count();
    assert_eq!(theirs, 2, "the author's doc holds the taken key: {mine:?}");
    kr.close().unwrap();
    rdy_qa::packs::use_pack(
        kd.path(),
        "ck5",
        &pack(&rule(
            "same-key",
            &format!("type = doc and key = \"{}\"", held.1),
            "fields.title = Nope",
            "advisory",
        )),
        "",
    );
    let mut kr = Rdy::open(kd.path()).unwrap();
    let p = kr
        .propose(&writer(), "k2", "k2", Some(TEST_SUMMARY))
        .unwrap();
    assert_eq!(p.keys.len(), 1, "{p:#?}");
    let blocks: Vec<String> = warned(&p).into_iter().map(|(b, _)| b).collect();
    assert_eq!(
        blocks,
        vec![held.0.clone()],
        "only canon's doc keeps the key"
    );
    // the acceptance change: one update per block, with status and a new order key together
    let made = write(
        &mut r,
        "o1",
        json!([{"op": "create", "type": "claim", "parent": ds, "body": "First."}]),
    )
    .unwrap();
    let _ = made;
    let dec = write(&mut r, "o2", json!([{"op": "create", "type": "decision", "id": "$d", "parent": ds,
        "fields": {"statement": "Second", "status": "proposed", "needs_content": false, "ok": true}}])).unwrap();
    let d = dec.created["$d"].clone();
    let authored = r
        .read()
        .materialized(&Scope::View("o2".into()))
        .unwrap()
        .get(&id(&d))
        .unwrap()
        .clone();
    land(&mut r, "o1").unwrap();
    let p = r
        .propose(&writer(), "o2", "o2", Some(TEST_SUMMARY))
        .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    confirm_before_landing(&mut r, &approver(), &p);
    let l = r.land_report(&approver(), &p.id).unwrap();
    let st = r.read();
    let rec = st.reader().landing(&l.landing.parse().unwrap()).unwrap();
    let mut ups = vec![];
    let mut bases = BTreeSet::new();
    for c in &rec.changes {
        for op in &st.reader().change(c).unwrap().ops {
            if let Op::Update { base, block } = op {
                assert!(bases.insert(*base), "two updates on one base");
                if block.id == id(&d) {
                    ups.push(block.clone());
                }
            }
        }
    }
    assert_eq!(ups.len(), 1, "{ups:#?}");
    assert_eq!(ups[0].field_str("status"), Some("accepted"));
    assert_ne!(ups[0].order, authored.order, "the placement was re-keyed");
    // the author's own block in the view is unchanged by the projection
    assert_eq!(authored.field_str("status"), Some("proposed"));
}

/// The two-child world of review C1: a stable doc whose section `s` must contain a checked
/// claim; claims `c1` and `c2` are both checked. With `policy`, a policy lets agents `agent:a*`
/// land editorial edits of any lifecycle with no decision.
struct TwoChild {
    _d: tempfile::TempDir,
    r: Rdy,
    s: String,
    c1: String,
    c2: String,
    o: String,
}

fn two_child(policy: bool, with_rules: bool) -> TwoChild {
    let rules = if with_rules {
        rule(
            "checked-child",
            "type = section and fields.ok = true",
            "out contains (fields.checked = true)",
            "blocking",
        )
    } else {
        String::new()
    };
    let extra = "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n";
    let (d, mut r) = if with_rules {
        ck5_project(&rules, extra)
    } else {
        ck5_project("  []\n", extra)
    };
    let seeded = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$st", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$st", "fields": {"title": "Rules", "ok": true}},
            {"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "C1 holds.", "fields": {"checked": true}},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "C2 holds.", "fields": {"checked": true}},
            {"op": "create", "type": "doc", "id": "$od", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$os", "parent": "$od", "fields": {"title": "More"}},
            {"op": "create", "type": "claim", "id": "$o", "parent": "$os", "body": "O holds."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    if policy {
        let w = write(&mut r, "pol", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "overview"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d", "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]))
        .unwrap();
        land(&mut r, "pol").unwrap();
        let doc = w.created["$d"].clone();
        let adopt = id(&w.created["$dec"]);
        r.write(
            &writer(),
            WriteRequest {
                view: "pol2".into(),
                ops: parse_test_ops(
                    json!([{"op": "create", "type": "authority_policy", "parent": doc, "fields": {
                        "key": "authority-policy",
                        "agent_may_accept": [{
                            "name": "edits", "actors": ["agent:a*"], "lifecycle": "any",
                            "justifications": ["implements", "editorial"], "decisions": "none",
                            "max_blocks": 20, "max_items": 20
                        }],
                        "roles": {"approvers": [PERSON]}, "require": [], "owners": []
                    }}]),
                )
                .unwrap(),
                justification: Some(rdy_model::Justification::Implements(vec![adopt])),
                message: String::new(),
            },
        )
        .unwrap();
        land(&mut r, "pol2").unwrap();
    }
    TwoChild {
        s: seeded.created["$s"].clone(),
        c1: seeded.created["$c1"].clone(),
        c2: seeded.created["$c2"].clone(),
        o: seeded.created["$o"].clone(),
        _d: d,
        r,
    }
}

fn uncheck(w: &mut TwoChild, view: &str, claim: &str) -> ProposalInfo {
    put_j(
        &mut w.r,
        view,
        json!([{"op": "update", "id": claim, "fields": {"checked": false}}]),
        j_typo(),
    );
    let p = propose_as(&mut w.r, view);
    assert!(p.ready, "{view}: {:#?}", p.issues);
    p
}

fn reach_of(r: &Rdy, p: &ProposalInfo) -> BTreeSet<String> {
    stored(r, p).reach.iter().map(ToString::to_string).collect()
}

fn land_as(
    w: &mut TwoChild,
    p: &ProposalInfo,
    policy: bool,
    view: &str,
) -> Result<LandInfo, RdyError> {
    if policy {
        w.r.land_by_policy(&ag(view), &p.r#ref)
    } else {
        confirm_before_landing(&mut w.r, &approver(), p);
        w.r.land_report(&approver(), &p.id)
    }
}

#[test]
fn ck5_rule_reach_refuses_the_second_child_landing() {
    for policy in [false, true] {
        let mut w = two_child(policy, true);
        let c1 = w.c1.clone();
        let p1 = uncheck(&mut w, "a1", &c1);
        let c2 = w.c2.clone();
        let p2 = uncheck(&mut w, "a2", &c2);
        for p in [&p1, &p2] {
            let reach = reach_of(&w.r, p);
            assert!(
                reach.contains(&w.c1) && reach.contains(&w.c2) && reach.contains(&w.s),
                "policy {policy}: {reach:?}"
            );
        }
        let l1 = land_as(&mut w, &p1, policy, "a1").unwrap();
        let e = land_as(&mut w, &p2, policy, "a2").unwrap_err();
        assert_eq!(e.code, E_PROPOSAL_MOVED, "policy {policy}: {e:#?}");
        assert_eq!(
            e.message,
            format!(
                "proposal {} moved: landing {} changed {} after its assessment",
                p2.r#ref,
                &l1.landing[..12],
                w.c1
            )
        );
        assert_eq!(
            e.fix,
            "fix each issue and propose again: rdy_propose / rdy propose"
        );
        assert!(e.corrected.is_none());
        let fresh: Vec<&Issue> = e.issues.iter().filter(|i| i.code == E_RULE_CHECK).collect();
        assert_eq!(fresh.len(), 1, "{e:#?}");
        assert_eq!(
            fresh[0].message,
            format!("rule checked-child fails on {}: checked-child message", w.s)
        );
        // canon has no invalid second landing
        let c2 =
            w.r.read()
                .materialized(&Scope::Canon)
                .unwrap()
                .get(&id(&w.c2))
                .unwrap()
                .fields
                .get("checked")
                .cloned();
        assert_eq!(
            c2,
            Some(rdy_model::FieldValue::Bool(true)),
            "policy {policy}"
        );
    }
}

#[test]
fn ck5_rule_reach_allows_an_unrelated_landing() {
    for policy in [false, true] {
        let mut w = two_child(policy, true);
        let c2 = w.c2.clone();
        let p2 = uncheck(&mut w, "a2", &c2);
        let scans = rule_scans_for_tests();
        // a landing elsewhere is outside the widened reach
        put_j(
            &mut w.r,
            "other",
            rdy_qa::tensions::upd(&id(&w.o), "O holds, now."),
            j_typo(),
        );
        let po = propose_as(&mut w.r, "other");
        land_as(&mut w, &po, false, "other").unwrap();
        assert!(!reach_of(&w.r, &p2).contains(&w.o));
        let before = rule_scans_for_tests();
        let l = land_as(&mut w, &p2, policy, "a2").unwrap();
        assert_eq!(
            rule_scans_for_tests(),
            before,
            "no rule decides the landing"
        );
        assert_eq!(l.approved, p2.r#ref);
        let _ = scans;
    }
    // with no rules the reach is the written blocks, plan blocks and tension sides, as before
    let mut w = two_child(false, false);
    let c1 = w.c1.clone();
    let p = uncheck(&mut w, "a1", &c1);
    let reach = reach_of(&w.r, &p);
    assert_eq!(reach, BTreeSet::from([w.c1.clone()]), "{reach:?}");
}

/// The reach rule tests' world: one draft doc per block, so that only links join them: `w`
/// (the written claim, it references `r`), `n1` references `w`, `n2` references `n1`, `n3`
/// references `n2`, `n4` references `n3` (four hops), `l` is a decision that affects `w`.
struct ReachWorld {
    _d: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
}

impl ReachWorld {
    fn id(&self, n: &str) -> String {
        self.ids[n].clone()
    }
}

fn reach_world(rules: &str) -> ReachWorld {
    let (d, mut r) = ck5_project(rules, "");
    let mut ops = vec![];
    for name in ["w", "r", "n1", "n2", "n3", "n4", "l", "st"] {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$d{name}"), "fields": {"name": format!("doc-{name}"), "title": format!("Doc {name}"), "doc_type": "overview"}}));
        ops.push(json!({"op": "create", "type": "section", "id": format!("$s{name}"), "parent": format!("$d{name}"), "fields": {"title": format!("S {name}")}}));
    }
    let body = |text: &str, to: &str| format!("{text} {{{{${to}}}}}.");
    ops.extend([
        json!({"op": "create", "type": "claim", "id": "$r", "parent": "$sr", "body": "R holds.", "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$w", "parent": "$sw", "body": body("W builds on", "r"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n1", "parent": "$sn1", "body": body("N1 builds on", "w"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n2", "parent": "$sn2", "body": body("N2 builds on", "n1"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n3", "parent": "$sn3", "body": body("N3 builds on", "n2"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "claim", "id": "$n4", "parent": "$sn4", "body": body("N4 builds on", "n3"), "fields": {"ok": true}}),
        json!({"op": "create", "type": "decision", "id": "$l", "parent": "$sl", "fields": {"statement": "L", "status": "proposed", "needs_content": false, "ok": true}, "links": {"affects": [{"target": "$w"}]}}),
        json!({"op": "create", "type": "claim", "id": "$kid", "parent": "$sst", "body": "Kid.", "fields": {"ok": true}}),
    ]);
    let w = write(&mut r, "seed", Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    let ids = w
        .created
        .iter()
        .map(|(k, v)| (k.trim_start_matches('$').to_string(), v.clone()))
        .collect();
    ReachWorld { _d: d, r, ids }
}

/// The stored reach of the version of view `view` after `ops`, as names of `w`.
fn reach_names(w: &mut ReachWorld, view: &str, ops: Value) -> BTreeSet<String> {
    write(&mut w.r, view, ops).unwrap();
    let p =
        w.r.propose(&writer(), view, view, Some(TEST_SUMMARY))
            .unwrap();
    assert!(p.ready, "{:#?}", p.issues);
    let reach = reach_of(&w.r, &p);
    let by_id: BTreeMap<&String, &String> = w.ids.iter().map(|(k, v)| (v, k)).collect();
    reach
        .iter()
        .map(|i| by_id.get(i).map_or(i.clone(), |n| (*n).clone()))
        .collect()
}

#[test]
fn ck5_rule_reach_covers_inheritance_links_and_stored_rows() {
    let names =
        |list: &[&str]| -> BTreeSet<String> { list.iter().map(|s| s.to_string()).collect() };
    // depth 0: the written block and its descendants, and the inputs of each (its ancestors for
    // an inherited field)
    let r0 = rule(
        "lifecycle",
        "type = claim",
        "not lifecycle = deprecated",
        "advisory",
    );
    let mut w = reach_world(&r0);
    let wid = w.id("w");
    let got = reach_names(
        &mut w,
        "v",
        json!([{"op": "update", "id": wid, "fields": {"ok": false}}]),
    );
    assert!(got.is_superset(&names(&["w", "sw", "dw"])), "{got:?}");
    assert!(!got.contains("r") && !got.contains("n2"), "{got:?}");
    // depth 1 to 3 along references, in either direction; the fourth hop stays outside
    for (depth, require, inside, outside) in [
        (
            1,
            "out references (fields.ok = true)",
            vec!["w", "r", "n1", "l"],
            vec!["n2"],
        ),
        (
            2,
            "out references (out references (fields.ok = true))",
            vec!["w", "r", "n1", "n2", "l"],
            vec!["n3"],
        ),
        (
            3,
            "out references (out references (out references (fields.ok = true)))",
            vec!["w", "r", "n1", "n2", "n3"],
            vec!["n4"],
        ),
    ] {
        let mut w = reach_world(&rule("deep", "type = claim", require, "advisory"));
        let wid = w.id("w");
        let got = reach_names(
            &mut w,
            "v",
            json!([{"op": "update", "id": wid, "fields": {"ok": false}}]),
        );
        assert!(got.is_superset(&names(&inside)), "depth {depth}: {got:?}");
        for o in outside {
            assert!(!got.contains(o), "depth {depth}: {o} in {got:?}");
        }
    }
    // a removed edge: a version that drops w's reference to r still reaches r
    let mut w = reach_world(&rule(
        "near",
        "type = claim",
        "out references (fields.ok = true)",
        "advisory",
    ));
    let wid = w.id("w");
    let got = reach_names(
        &mut w,
        "v",
        json!([{"op": "update", "id": wid, "body": "W stands alone."}]),
    );
    assert!(got.contains("r"), "{got:?}");
    // a changed ancestor: an edit of a doc reaches its descendants; a deleted section's id stays
    let mut w = reach_world(&r0);
    let dst = w.id("dst");
    let got = reach_names(
        &mut w,
        "v",
        json!([{"op": "update", "id": dst, "fields": {"title": "Doc st, now"}}]),
    );
    assert!(got.is_superset(&names(&["dst", "sst", "kid"])), "{got:?}");
    let mut w = reach_world(&r0);
    let (sst, kid) = (w.id("sst"), w.id("kid"));
    let got = reach_names(
        &mut w,
        "v",
        json!([{"op": "delete", "id": kid}, {"op": "delete", "id": sst}]),
    );
    assert!(got.is_superset(&names(&["sst", "kid", "dst"])), "{got:?}");
    // a stored warning row: its derived inputs are in the reach, though far from the write; the
    // fourth hop gets in through that row
    let rr = rule(
        "n4-ok",
        "type = claim",
        "not out references (fields.ok = false)",
        "advisory",
    );
    let mut w = reach_world(&rr);
    let n3 = w.id("n3");
    write(
        &mut w.r,
        "far",
        json!([{"op": "update", "id": n3, "fields": {"ok": false}}]),
    )
    .unwrap();
    land(&mut w.r, "far").unwrap();
    let wid = w.id("w");
    let got = reach_names(
        &mut w,
        "v",
        json!([{"op": "update", "id": wid, "body": "W builds on nothing."}]),
    );
    assert!(got.contains("n4") && got.contains("n3"), "{got:?}");
    // the stored reach is sorted and distinct
    let open = w.r.read().open_proposals().unwrap();
    let a = stored_version(&w.r, &open[0].0.version);
    let mut sorted = a.reach.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, a.reach);
}
