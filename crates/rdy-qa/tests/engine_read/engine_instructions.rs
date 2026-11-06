//! ain3 (D-8X04F, D-YQPB6, D-4W1ZR, D-A8N0E): one collector reads a captured source's canon in doc
//! order and gives its contribution: the menu of every ruleset, and the whole bodies of the
//! required instructions and the requested rulesets. One merge combines the sources in layer
//! order, resolves the requested load ids and builds the reply that MCP and the CLI serialize.
//! A reply is complete or a refusal; hints never select a rule.
#[allow(clippy::duplicate_mod)]
#[path = "../../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::embedded_guide::RULESETS;
use rdy_engine::instructions::{
    self, BOUNDS, Bounds, Caller, Contribution, InstructionRow, Layer, LoadRequest,
    META_READY_HINT, META_SESSION_HINT, PROJECTLESS_SUFFIX, PersonHost, Reply, create_hint,
    estimate_tokens,
};
use rdy_engine::instructions_config::{NO_HOME_MESSAGE, PersonPath, PersonSource};
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope};
use rdy_model::BlockId;
use rdy_model::redaction::RedactTarget;
use rdy_qa::fixtures::*;
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

const NOT_FOUND_FIX: &str = "call rdy_instructions and load an id from its menu";
const BAD_LOAD: &str = "load must be a list of ruleset ids";
const BAD_LOAD_FIX: &str = "call rdy_instructions and pass ids from its menu";
const AMBIGUOUS_FIX: &str = "use the layer-qualified id from the rdy_instructions menu";
const LIMIT_FIX: &str = "reduce the selected rulesets or the required instruction set, then call rdy_instructions again";
const REASON_FIX: &str = "link the instruction to its reason with decided_by: rdy_write / rdy new instruction --reason <decision>";

/// One source project with a landed decisions doc holding decision `D`.
struct Src {
    dir: tempfile::TempDir,
    r: Rdy,
    decision: String,
    statement: String,
}

fn source(statement: &str) -> Src {
    let (dir, mut r) = project();
    let w = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "decisions", "title": "Decisions", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}
    ]))
    .unwrap();
    let d = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: w.created["$s"].clone(),
                statement: statement.into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "seed").unwrap();
    Src {
        dir,
        r,
        decision: d,
        statement: statement.into(),
    }
}

/// One instruction of a ruleset: its selectors, its body, whether it sits in the doc's section,
/// and its stored `locked` value.
struct Rule<'a> {
    sels: &'a [&'a str],
    body: &'a str,
    in_section: bool,
    locked: Option<bool>,
}

fn rule<'a>(sels: &'a [&'a str], body: &'a str) -> Rule<'a> {
    Rule {
        sels,
        body,
        in_section: false,
        locked: None,
    }
}

fn nested<'a>(sels: &'a [&'a str], body: &'a str) -> Rule<'a> {
    Rule {
        in_section: true,
        ..rule(sels, body)
    }
}

/// Writes doc `name` titled `title` into `view`, with `required` when given, a lead prose block
/// when given, and `rules` in order (the nested ones in one section, placed where the first one
/// is), each with `reason` as its `decided_by`. Returns the doc's id and the instructions' ids.
#[allow(clippy::too_many_arguments)]
fn ruleset(
    r: &mut Rdy,
    view: &str,
    name: &str,
    title: &str,
    required: Option<bool>,
    lead: Option<&str>,
    rules: &[Rule],
    reason: &str,
) -> (String, Vec<String>) {
    let mut fields = json!({"name": name, "title": title, "doc_type": "page"});
    if let Some(req) = required {
        fields["required"] = json!(req);
    }
    let mut ops = vec![json!({"op": "create", "type": "doc", "id": "$d", "fields": fields})];
    if let Some(lead) = lead {
        ops.push(json!({"op": "create", "type": "prose", "parent": "$d", "body": lead}));
    }
    let mut section = false;
    for (i, rl) in rules.iter().enumerate() {
        if rl.in_section && !section {
            ops.push(json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}));
            section = true;
        }
        let mut f = json!({"applies_to": rl.sels});
        if let Some(l) = rl.locked {
            f["locked"] = json!(l);
        }
        ops.push(
            json!({"op": "create", "type": "instruction", "id": format!("$i{i}"),
            "parent": if rl.in_section { "$s" } else { "$d" }, "fields": f, "body": rl.body,
            "links": {"decided_by": [{"target": reason}]}}),
        );
    }
    let w = write(r, view, Value::Array(ops)).unwrap_or_else(|e| panic!("ruleset {name}: {e}"));
    let ids = (0..rules.len())
        .map(|i| w.created[&format!("$i{i}")].clone())
        .collect();
    (w.created["$d"].clone(), ids)
}

fn strings(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn deliver_with(srcs: &[(&Rdy, Layer)], load: &[&str], b: &Bounds) -> Result<Reply, RdyError> {
    let req = LoadRequest::parse_with(&strings(load), b)?;
    let mut cs = vec![];
    for (r, l) in srcs {
        cs.push(instructions::collect_with(&r.read(), l.clone(), &req, b)?);
    }
    instructions::merge_with(cs, &req, b)
}

fn deliver(srcs: &[(&Rdy, Layer)], load: &[&str]) -> Result<Reply, RdyError> {
    deliver_with(srcs, load, &BOUNDS)
}

/// Each row's id, ruleset handle and layer.
fn rows(reply: &Reply) -> Vec<(String, String, String)> {
    reply
        .instructions
        .iter()
        .map(|i| (i.id.clone(), i.ruleset.clone(), i.layer.to_string()))
        .collect()
}

fn row<'a>(reply: &'a Reply, id: &str) -> &'a InstructionRow {
    reply
        .instructions
        .iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

fn menu_ids(reply: &Reply) -> Vec<String> {
    reply.menu.iter().map(|m| m.id.clone()).collect()
}

fn refusal(res: Result<Reply, RdyError>) -> (String, String, String) {
    let e = res.expect_err("the call is refused");
    (e.code.to_string(), e.message, e.fix)
}

fn p(id: &str) -> String {
    format!("project:{id}")
}

fn q(id: &str) -> String {
    format!("person:{id}")
}

#[test]
fn ain3_required_rules_and_the_menu_arrive_together() {
    let mut s = source("Run the tests before every proposal");
    let d = s.decision.clone();
    let r = &mut s.r;
    let (base, base_ids) = ruleset(
        r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        Some("Read these rules before any task."),
        &[
            rule(&["act:write"], "Write small changes."),
            nested(&["type:claim"], "Cite a decision in every claim."),
            nested(&["act:propose"], "Propose with a summary."),
        ],
        &d,
    );
    let (testing, testing_ids) = ruleset(
        r,
        "w",
        "testing",
        "Testing",
        None,
        Some("Use these when you change tests."),
        &[
            rule(&["all"], "Never skip a failing test."),
            rule(&["area:tests"], "Name each test after its pin."),
        ],
        &d,
    );
    // a title that says "Required" proves nothing; only the field does
    let (titled, titled_ids) = ruleset(
        r,
        "w",
        "required-rules",
        "Required rules",
        Some(false),
        Some("Optional despite the title."),
        &[rule(&["act:review"], "Review the whole diff.")],
        &d,
    );
    // a required doc without instructions is no ruleset
    let (empty_required, _) = ruleset(
        r,
        "w",
        "empty",
        "Empty",
        Some(true),
        Some("No rules."),
        &[],
        &d,
    );
    // no lead: when is empty, and no sentence is made up
    let (leadless, leadless_ids) = ruleset(
        r,
        "w",
        "leadless",
        "Leadless",
        None,
        None,
        &[rule(&["act:publish"], "Publish only from canon.")],
        &d,
    );
    land(r, "w").unwrap();

    let reply = deliver(&[(&*r, Layer::Project)], &[]).unwrap();
    // the full required docs, then the individually required rule, in doc order
    assert_eq!(
        rows(&reply),
        vec![
            (base_ids[0].clone(), p(&base), "project".into()),
            (base_ids[1].clone(), p(&base), "project".into()),
            (base_ids[2].clone(), p(&base), "project".into()),
            (testing_ids[0].clone(), p(&testing), "project".into()),
        ]
    );
    let first = serde_json::to_value(&reply.instructions[0]).unwrap();
    assert_eq!(
        first,
        json!({"id": base_ids[0], "ruleset": p(&base), "text": "Write small changes.",
            "layer": "project", "locked": false,
            "reason": [{"id": d, "statement": s.statement}]})
    );
    // the menu: every ruleset in doc order, with its metadata only
    assert_eq!(
        menu_ids(&reply),
        vec![p(&base), p(&testing), p(&titled), p(&leadless)]
    );
    assert!(!menu_ids(&reply).contains(&p(&empty_required)));
    let menu = serde_json::to_value(&reply.menu).unwrap();
    for m in menu.as_array().unwrap() {
        let mut keys: Vec<&str> = m.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["hints", "id", "layer", "name", "required", "tokens", "when"]
        );
    }
    let tokens = |bodies: &[&str]| -> u64 {
        (bodies.iter().map(|b| b.len()).sum::<usize>() as u64).div_ceil(4)
    };
    assert_eq!(
        menu[0],
        json!({"id": p(&base), "name": "Baseline", "when": "Read these rules before any task.",
            "layer": "project", "required": true,
            "tokens": tokens(&["Write small changes.", "Cite a decision in every claim.", "Propose with a summary."]),
            "hints": ["act:write", "type:claim", "act:propose"]})
    );
    assert_eq!(
        menu[1],
        json!({"id": p(&testing), "name": "Testing", "when": "Use these when you change tests.",
            "layer": "project", "required": false,
            "tokens": tokens(&["Never skip a failing test.", "Name each test after its pin."]),
            "hints": ["all", "area:tests"]})
    );
    assert_eq!(menu[2]["name"], "Required rules");
    assert_eq!(menu[2]["required"], false);
    assert_eq!(menu[3]["when"], "");
    assert_eq!(menu[3]["name"], "Leadless");
    // optional bodies stay out of the reply
    let text = String::from_utf8(reply.to_json().unwrap()).unwrap();
    for absent in [
        "Name each test after its pin.",
        "Review the whole diff.",
        "Publish only from canon.",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
    assert!(!text.contains(&titled_ids[0]) && !text.contains(&leadless_ids[0]));
    assert!(!text.contains(&testing_ids[1]));
    // the serialized reply is the owned reply
    let back: Reply = serde_json::from_slice(&reply.to_json().unwrap()).unwrap();
    assert_eq!(back, reply);
    let outer: Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<&str> = outer
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["instructions", "menu"]);
}

#[test]
fn ain3_loading_sets_repeats_required_rules_once() {
    let mut s = source("Keep the rules short");
    let d = s.decision.clone();
    let r = &mut s.r;
    let (base, base_ids) = ruleset(
        r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        Some("Always."),
        &[
            rule(&["all"], "Run the tests."),
            rule(&["act:write"], "Keep it small."),
        ],
        &d,
    );
    let (one, one_ids) = ruleset(
        r,
        "w",
        "one",
        "One",
        None,
        Some("First set."),
        &[
            rule(&["all"], "Say why."),
            rule(&["act:review"], "Read it all."),
        ],
        &d,
    );
    let (two, two_ids) = ruleset(
        r,
        "w",
        "two",
        "Two",
        None,
        Some("Second set."),
        &[nested(&["type:claim"], "Cite a source.")],
        &d,
    );
    let (_three, _) = ruleset(
        r,
        "w",
        "three",
        "Three",
        None,
        Some("Third set."),
        &[rule(&["act:publish"], "Not loaded.")],
        &d,
    );
    land(r, "w").unwrap();

    // several choices, duplicates in both forms, and the required doc again; doc order wins
    let reply = deliver(
        &[(&*r, Layer::Project)],
        &[&two, &one, &one, &p(&one), &base, &p(&base), &two],
    )
    .unwrap();
    assert_eq!(
        rows(&reply),
        vec![
            (base_ids[0].clone(), p(&base), "project".into()),
            (base_ids[1].clone(), p(&base), "project".into()),
            (one_ids[0].clone(), p(&one), "project".into()),
            (one_ids[1].clone(), p(&one), "project".into()),
            (two_ids[0].clone(), p(&two), "project".into()),
        ]
    );
    assert!(
        !String::from_utf8(reply.to_json().unwrap())
            .unwrap()
            .contains("Not loaded.")
    );
    // loading is stateless: a later call without load still returns the required rules
    let again = deliver(&[(&*r, Layer::Project)], &[]).unwrap();
    assert_eq!(
        rows(&again),
        vec![
            (base_ids[0].clone(), p(&base), "project".into()),
            (base_ids[1].clone(), p(&base), "project".into()),
            (one_ids[0].clone(), p(&one), "project".into()),
        ]
    );

    // two sources that repeat the same words both stay visible
    let mut person = source("Person reason");
    let pd = person.decision.clone();
    let (pbase, pids) = ruleset(
        &mut person.r,
        "w",
        "mine",
        "Mine",
        Some(true),
        Some("My rules."),
        &[rule(&["act:write"], "Run the tests.")],
        &pd,
    );
    land(&mut person.r, "w").unwrap();
    let both = deliver(&[(&person.r, Layer::Person), (&*r, Layer::Project)], &[]).unwrap();
    assert_eq!(
        rows(&both),
        vec![
            (base_ids[0].clone(), p(&base), "project".into()),
            (base_ids[1].clone(), p(&base), "project".into()),
            (one_ids[0].clone(), p(&one), "project".into()),
            (pids[0].clone(), q(&pbase), "person".into()),
        ]
    );
    assert_eq!(both.instructions[0].text, "Run the tests.");
    assert_eq!(both.instructions[3].text, "Run the tests.");
    assert_eq!(both.instructions[3].reason[0].id, pd);
}

#[test]
fn ain3_full_bodies_keep_every_paragraph_and_reference() {
    let mut s = source("Keep every word of a rule");
    let d = s.decision.clone();
    let r = &mut s.r;
    // one paragraph of about 1 KiB, with a reference to the reason
    let para = |n: usize| -> String {
        format!(
            "Paragraph {n}: follow {{{{{d}}}}} in every change, and keep its why and how together. {}",
            "The rule stays whole. ".repeat(44)
        )
    };
    let body = |tag: &str, paras: usize| -> String {
        let mut parts: Vec<String> = (0..paras).map(&para).collect();
        parts.push(format!(
            "Why: the reason holds.\n\nHow to apply: read it all.\n\nSENTINEL-{tag}-END"
        ));
        parts.join("\n\n")
    };
    // required text above 4,000 estimated tokens: two rules of 9 KiB
    let req = [body("req-a", 9), body("req-b", 9)];
    let (base, base_ids) = ruleset(
        r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        Some("The baseline."),
        &[
            rule(&["act:write"], &req[0]),
            nested(&["act:propose"], &req[1]),
        ],
        &d,
    );
    // nine optional sets that bring the sample above 39,000 estimated tokens
    let opt: Vec<String> = (0..9).map(|i| body(&format!("opt-{i}"), 17)).collect();
    let mut sets = vec![];
    for (i, b) in opt.iter().enumerate() {
        let (doc, ids) = ruleset(
            r,
            "w",
            &format!("set-{i}"),
            &format!("Set {i}"),
            None,
            Some("A set."),
            &[nested(&["act:review"], b)],
            &d,
        );
        sets.push((doc, ids[0].clone()));
    }
    land(r, "w").unwrap();

    let reply = deliver(&[(&*r, Layer::Project)], &[]).unwrap();
    assert!(reply.menu[0].tokens > 4_000, "{}", reply.menu[0].tokens);
    assert_eq!(row(&reply, &base_ids[0]).text, req[0]);
    assert_eq!(row(&reply, &base_ids[1]).text, req[1]);
    assert!(
        row(&reply, &base_ids[1])
            .text
            .ends_with("SENTINEL-req-b-END")
    );
    assert_eq!(reply.menu[0].id, p(&base));

    let load: Vec<&str> = sets.iter().map(|(doc, _)| doc.as_str()).collect();
    let all = deliver(&[(&*r, Layer::Project)], &load).unwrap();
    let total: u64 = all.menu.iter().map(|m| m.tokens).sum();
    assert!(total > 39_000, "{total}");
    assert_eq!(all.instructions.len(), 11);
    for ((_, id), b) in sets.iter().zip(&opt) {
        let text = &row(&all, id).text;
        assert_eq!(text, b, "every paragraph and reference is kept");
        assert_eq!(text.matches("\n\n").count(), 19);
        assert!(text.contains(&format!("{{{{{d}}}}}")));
        assert!(text.ends_with("-END"));
    }
    // the store holds the same bytes
    for (_, id) in &sets {
        assert_eq!(&body_of(r, &Scope::Canon, id), &row(&all, id).text);
    }
}

#[test]
fn ain3_hints_never_filter_and_tokens_use_utf8_bytes() {
    // the estimate: ceil(bytes / 4)
    for (bytes, est) in [(0u64, 0u64), (1, 1), (3, 1), (4, 1), (5, 2), (8, 2), (9, 3)] {
        assert_eq!(instructions::estimate_tokens(bytes), est, "{bytes}");
    }
    let mut s = source("Hints are hints");
    let d = s.decision.clone();
    let r = &mut s.r;
    // mismatched area, type and act hints: delivered all the same
    let (base, base_ids) = ruleset(
        r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        Some("Always."),
        &[
            rule(&["act:review", "type:claim"], "€€€€€"),
            rule(&["type:claim", "area:nosuch", "act:review"], "a"),
            rule(&["type:no-such-type", "act:redact"], "abcd"),
        ],
        &d,
    );
    // ascii on the four-byte boundary and one past it
    let (four, _) = ruleset(
        r,
        "w",
        "four",
        "Four",
        None,
        None,
        &[rule(&["act:write"], "ab"), rule(&["act:write"], "cd")],
        &d,
    );
    let (five, _) = ruleset(
        r,
        "w",
        "five",
        "Five",
        None,
        None,
        &[rule(&["act:write"], "abcde")],
        &d,
    );
    let (wide, _) = ruleset(
        r,
        "w",
        "wide",
        "Wide",
        None,
        None,
        &[rule(&["act:write"], "é")],
        &d,
    );
    land(r, "w").unwrap();

    let reply = deliver(&[(&*r, Layer::Project)], &[]).unwrap();
    assert_eq!(
        reply
            .instructions
            .iter()
            .map(|i| i.id.clone())
            .collect::<Vec<_>>(),
        base_ids
    );
    let m = |id: &str| reply.menu.iter().find(|m| m.id == p(id)).unwrap().clone();
    // 15 + 1 + 4 bytes: 5 tokens, where six characters would say 2
    assert_eq!(m(&base).tokens, 5);
    assert_eq!(
        m(&base).hints,
        vec![
            "act:review",
            "type:claim",
            "area:nosuch",
            "type:no-such-type",
            "act:redact"
        ]
    );
    assert_eq!(m(&four).tokens, 1);
    assert_eq!(m(&five).tokens, 2);
    assert_eq!(m(&wide).tokens, 1, "a small nonempty rule never says zero");
    assert_eq!(row(&reply, &base_ids[0]).text, "€€€€€");
}

#[test]
fn ain3_layers_remain_visible_and_local_locks_do_not_gain_force() {
    // the reserved order: organization, team sets, cited, project, person
    let order = [
        Layer::Rdy,
        Layer::Organization,
        Layer::Team("beta".into()),
        Layer::Team("alpha".into()),
        Layer::Cited("std".into()),
        Layer::Project,
        Layer::Person,
    ];
    let ranks: Vec<u8> = order.iter().map(Layer::precedence).collect();
    assert_eq!(ranks, vec![0, 1, 2, 2, 3, 4, 5]);
    // team names give display order, not precedence
    assert_eq!(
        Layer::Team("alpha".into()).precedence(),
        Layer::Team("zeta".into()).precedence()
    );
    assert!(Layer::Team("alpha".into()) < Layer::Team("beta".into()));
    // only an organization or team lock wins over a later layer
    assert_eq!(
        order
            .iter()
            .map(Layer::locks_have_force)
            .collect::<Vec<_>>(),
        vec![true, true, true, true, false, false, false]
    );
    for (l, text) in order.iter().zip([
        "rdy",
        "organization",
        "team:beta",
        "team:alpha",
        "cited:std",
        "project",
        "person",
    ]) {
        assert_eq!(l.to_string(), text);
        assert_eq!(serde_json::to_value(l).unwrap(), json!(text));
        assert_eq!(serde_json::from_value::<Layer>(json!(text)).unwrap(), *l);
    }

    let mut proj = source("Project reason");
    let (pr, pd) = (&mut proj.r, proj.decision.clone());
    let (pdoc, pids) = ruleset(
        pr,
        "w",
        "house",
        "House",
        Some(true),
        Some("Project rules."),
        &[Rule {
            locked: Some(true),
            ..rule(&["act:write"], "Use tabs.")
        }],
        &pd,
    );
    land(pr, "w").unwrap();
    let mut person = source("Person reason");
    let (qr, qd) = (&mut person.r, person.decision.clone());
    let (qdoc, qids) = ruleset(
        qr,
        "w",
        "mine",
        "Mine",
        Some(true),
        Some("My rules."),
        &[Rule {
            locked: Some(false),
            ..rule(&["act:write"], "Use spaces.")
        }],
        &qd,
    );
    land(qr, "w").unwrap();

    // person after project, whatever order the host gives; both rules remain
    let reply = deliver(
        &[(&person.r, Layer::Person), (&proj.r, Layer::Project)],
        &[],
    )
    .unwrap();
    assert_eq!(
        rows(&reply),
        vec![
            (pids[0].clone(), p(&pdoc), "project".into()),
            (qids[0].clone(), q(&qdoc), "person".into()),
        ]
    );
    // the local lock is visible and removes nothing
    assert!(reply.instructions[0].locked);
    assert!(!reply.instructions[1].locked);
    assert_eq!(reply.instructions[0].text, "Use tabs.");
    assert_eq!(reply.instructions[1].text, "Use spaces.");
    assert_eq!(
        reply
            .menu
            .iter()
            .map(|m| m.layer.to_string())
            .collect::<Vec<_>>(),
        vec!["project", "person"]
    );
    // two sources of one layer are a host error, never a silent winner
    let req = LoadRequest::parse(&[]).unwrap();
    let e = instructions::merge(
        vec![
            instructions::collect(&proj.r.read(), Layer::Project, &req).unwrap(),
            instructions::collect(&person.r.read(), Layer::Project, &req).unwrap(),
        ],
        &req,
    )
    .unwrap_err();
    assert_eq!(e.code, "E_INTERNAL", "{e}");
}

#[test]
fn ain3_scope_and_load_identity_follow_the_recorded_rulings() {
    let mut s = source("Canon is the rule");
    let d = s.decision.clone();
    let (base, base_ids) = ruleset(
        &mut s.r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        Some("Always."),
        &[rule(&["act:write"], "canon text")],
        &d,
    );
    let (opt, opt_ids) = ruleset(
        &mut s.r,
        "w",
        "optional",
        "Optional",
        None,
        Some("Sometimes."),
        &[rule(&["act:review"], "optional text")],
        &d,
    );
    land(&mut s.r, "w").unwrap();
    // an unlanded view changes a rule and adds a ruleset: canon only is delivered (D-A8N0E)
    let v = version_of(&s.r, &Scope::Canon, &base_ids[0]);
    write(
        &mut s.r,
        "draft",
        json!([{"op": "update", "id": base_ids[0], "base": v, "body": "view text"}]),
    )
    .unwrap();
    let (unlanded, _) = ruleset(
        &mut s.r,
        "draft",
        "unlanded",
        "Unlanded",
        Some(true),
        Some("Not yet."),
        &[rule(&["all"], "draft rule")],
        &d,
    );
    assert_eq!(
        body_of(&s.r, &Scope::View("draft".into()), &base_ids[0]),
        "view text"
    );
    let reply = deliver(&[(&s.r, Layer::Project)], &[]).unwrap();
    assert_eq!(reply.instructions.len(), 1);
    assert_eq!(reply.instructions[0].text, "canon text");
    assert_eq!(menu_ids(&reply), vec![p(&base), p(&opt)]);
    assert_eq!(
        refusal(deliver(&[(&s.r, Layer::Project)], &[&unlanded])),
        (
            "E_NOT_FOUND".into(),
            format!("ruleset {unlanded} was not found"),
            NOT_FOUND_FIX.into()
        )
    );

    // a clone: two sources share every id
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("clone.rdydump");
    s.r.read().dump(&file).unwrap();
    let clone_dir = scratch.path().join("clone");
    rdy_engine::load::load(&file, &clone_dir).unwrap();
    let clone = Rdy::open(&clone_dir).unwrap();
    // a ruleset only the project holds
    let (only, only_ids) = ruleset(
        &mut s.r,
        "w2",
        "only",
        "Only",
        None,
        Some("Project only."),
        &[rule(&["act:review"], "only text")],
        &d,
    );
    land(&mut s.r, "w2").unwrap();
    let srcs = [(&s.r, Layer::Project), (&clone, Layer::Person)];
    let both = deliver(&srcs, &[]).unwrap();
    assert_eq!(
        menu_ids(&both),
        vec![p(&base), p(&opt), p(&only), q(&base), q(&opt)]
    );
    assert_eq!(
        rows(&both),
        vec![
            (base_ids[0].clone(), p(&base), "project".into()),
            (base_ids[0].clone(), q(&base), "person".into()),
        ]
    );
    // a qualified id loads one layer's set
    let one = deliver(&srcs, &[&q(&opt)]).unwrap();
    assert_eq!(
        rows(&one),
        vec![
            (base_ids[0].clone(), p(&base), "project".into()),
            (base_ids[0].clone(), q(&base), "person".into()),
            (opt_ids[0].clone(), q(&opt), "person".into()),
        ]
    );
    let other = deliver(&srcs, &[&p(&opt)]).unwrap();
    assert_eq!(other.instructions[1].ruleset, p(&opt));
    assert_eq!(other.instructions[1].layer, Layer::Project);
    // a unique bare id loads
    let bare = deliver(&srcs, &[&only]).unwrap();
    assert_eq!(
        rows(&bare),
        vec![
            (base_ids[0].clone(), p(&base), "project".into()),
            (only_ids[0].clone(), p(&only), "project".into()),
            (base_ids[0].clone(), q(&base), "person".into()),
        ]
    );
    // an ambiguous bare id is refused, with no winner
    assert_eq!(
        refusal(deliver(&srcs, &[&only, &opt])),
        (
            "E_BAD_INPUT".into(),
            format!("ruleset {opt} occurs in project and person"),
            AMBIGUOUS_FIX.into()
        )
    );
    // a qualified id of the wrong layer is not found
    assert_eq!(
        refusal(deliver(&srcs, &[&q(&only)])),
        (
            "E_NOT_FOUND".into(),
            format!("ruleset {} was not found", q(&only)),
            NOT_FOUND_FIX.into()
        )
    );
    // malformed load entries
    for bad in [
        "",
        "project:",
        "person:",
        "team:alpha",
        &format!("team:alpha:{opt}"),
        &format!("organization:{opt}"),
        &format!("cited:{opt}"),
        &format!("Project:{opt}"),
        "not an id",
        &format!(" {opt}"),
    ] {
        assert_eq!(
            refusal(deliver(&srcs, &[bad])),
            ("E_BAD_INPUT".into(), BAD_LOAD.into(), BAD_LOAD_FIX.into()),
            "{bad:?}"
        );
    }
    for bad in [
        json!("x"),
        json!(5),
        json!({"id": opt}),
        json!([opt, 5]),
        json!([null]),
    ] {
        let e = LoadRequest::from_json(Some(&bad)).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_BAD_INPUT", BAD_LOAD, BAD_LOAD_FIX),
            "{bad}"
        );
    }
    assert_eq!(
        LoadRequest::from_json(None).unwrap(),
        LoadRequest::parse(&[]).unwrap()
    );
    assert_eq!(
        LoadRequest::from_json(Some(&json!(null))).unwrap(),
        LoadRequest::parse(&[]).unwrap()
    );
    assert_eq!(
        LoadRequest::from_json(Some(&json!([q(&opt)]))).unwrap(),
        LoadRequest::parse(&[q(&opt)]).unwrap()
    );
    clone.close().unwrap();
}

/// Applies one approved removal of every version of `ids` to the closed project at `dir`.
fn redact(dir: &Path, ids: &[&str]) -> (Rdy, String) {
    let f = authorized_batch_fixture(
        &dir.join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            ids.iter()
                .map(|i| RedactTarget::AllVersions(BlockId::parse(i).unwrap()))
                .collect(),
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    (Rdy::open(dir).unwrap(), f.requests[0].request.to_string())
}

#[test]
fn ain3_missing_invalid_and_removed_rules_are_explicit() {
    // an empty source: an empty, successful reply
    let empty = source("Nothing yet");
    let reply = deliver(&[(&empty.r, Layer::Person)], &[]).unwrap();
    assert!(reply.instructions.is_empty() && reply.menu.is_empty());
    assert_eq!(
        String::from_utf8(reply.to_json().unwrap()).unwrap(),
        r#"{"instructions":[],"menu":[]}"#
    );

    let mut s = source("Rules must be visible");
    let d = s.decision.clone();
    let (base, base_ids) = ruleset(
        &mut s.r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        None,
        &[
            rule(&["act:write"], "Keep it."),
            rule(&["act:review"], "Removed later."),
        ],
        &d,
    );
    let (gone, gone_ids) = ruleset(
        &mut s.r,
        "w",
        "gone",
        "Gone",
        None,
        None,
        &[rule(&["act:write"], "Deleted.")],
        &d,
    );
    let (bare_doc, _) = ruleset(
        &mut s.r,
        "w",
        "bare",
        "Bare",
        None,
        Some("No rules."),
        &[],
        &d,
    );
    land(&mut s.r, "w").unwrap();
    let reply = deliver(&[(&s.r, Layer::Person)], &[]).unwrap();
    // an empty lead keeps the required rules
    assert_eq!(reply.menu[0].when, "");
    assert_eq!(reply.instructions.len(), 2);
    assert_eq!(reply.instructions[0].layer, Layer::Person);
    assert_eq!(reply.instructions[0].ruleset, q(&base));

    // deleted: the doc and its rule
    write(
        &mut s.r,
        "del",
        json!([{"op": "delete", "id": gone_ids[0]}, {"op": "delete", "id": gone}]),
    )
    .unwrap();
    land(&mut s.r, "del").unwrap();
    let section =
        s.r.read()
            .block(&Scope::Canon, &BlockId::parse(&d).unwrap())
            .unwrap()
            .parent
            .unwrap()
            .to_string();
    // missing, deleted, not a doc, or a doc with no rules: not found, with the call refused
    for id in [
        gone.clone(),
        q(&gone),
        gone_ids[0].clone(),
        base_ids[0].clone(),
        d.clone(),
        section,
        bare_doc.clone(),
        p(&base),
    ] {
        assert_eq!(
            refusal(deliver(&[(&s.r, Layer::Person)], &[&base, &id])),
            (
                "E_NOT_FOUND".into(),
                format!("ruleset {id} was not found"),
                NOT_FOUND_FIX.into()
            ),
            "{id}"
        );
    }

    // an invalid stored rule in a required doc refuses the call, labelled with its source
    let (_dir, mut bad) = (s.dir, s.r);
    let mut broken = raw(
        "ai-BRKN0",
        "instruction",
        Some(&base),
        "zz",
        &[],
        "No reason.",
    );
    broken.fields.insert(
        "applies_to".into(),
        rdy_model::FieldValue::List(vec![rdy_model::FieldValue::Str("all".into())]),
    );
    raw_commit(&mut bad, "raw", vec![broken]);
    land_with_a_tie(&mut bad, "raw");
    let e = deliver(&[(&bad, Layer::Person)], &[]).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_VALIDATION",
            "person source: instruction ai-BRKN0 requires a decided_by decision",
            REASON_FIX
        )
    );
    assert_eq!(
        e.issues
            .iter()
            .map(|i| (i.code, i.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(
            "E_VALIDATION",
            "instruction ai-BRKN0 requires a decided_by decision"
        )]
    );
    assert_eq!(
        e.issues[0].block.as_ref().map(|b| b.to_string()),
        Some("ai-BRKN0".to_string())
    );
    bad.close().unwrap();

    // removed content: the safe read projection and its notice, never the removed words
    let mut t = source("Removed reasons stay safe");
    let td = t.decision.clone();
    let (tdoc, tids) = ruleset(
        &mut t.r,
        "w",
        "removed",
        "Removed",
        Some(true),
        Some("Some are removed."),
        &[
            rule(&["act:write"], "Secret words."),
            rule(&["act:review"], "Kept words."),
        ],
        &td,
    );
    land(&mut t.r, "w").unwrap();
    t.r.close().unwrap();
    let (tr, request) = redact(t.dir.path(), &[&tids[0], &td]);
    let reply = deliver(&[(&tr, Layer::Person)], &[]).unwrap();
    assert_eq!(
        rows(&reply),
        vec![
            (tids[0].clone(), q(&tdoc), "person".into()),
            (tids[1].clone(), q(&tdoc), "person".into()),
        ]
    );
    let removed = &reply.instructions[0];
    assert_eq!(
        removed.text,
        format!("Content removed.\n\nRedaction: {request}.")
    );
    assert!(removed.reason.is_empty() && !removed.locked);
    let kept = &reply.instructions[1];
    assert_eq!(kept.text, "Kept words.");
    assert_eq!(kept.reason.len(), 1);
    assert_eq!(kept.reason[0].id, td);
    assert_eq!(
        kept.reason[0].statement,
        format!("Content removed.\n\nRedaction: {request}.")
    );
    let json = String::from_utf8(reply.to_json().unwrap()).unwrap();
    assert!(!json.contains("Secret words.") && !json.contains("Removed reasons stay safe"));
    tr.close().unwrap();
}

#[test]
fn ain3_work_and_reply_bounds_refuse_without_partial_rules() {
    // the bounds, as numbers
    assert_eq!(
        (
            BOUNDS.visits,
            BOUNDS.rulesets,
            BOUNDS.load_ids,
            BOUNDS.load_id_bytes,
            BOUNDS.selectors,
            BOUNDS.selector_bytes,
            BOUNDS.reply_bytes
        ),
        (100_000, 10_000, 1_024, 256, 1_024, 256, 16_777_216)
    );
    let limit = |limit: u64, unit: &str, found: u64| {
        (
            "E_LIMIT".to_string(),
            format!("instructions exceed {limit} {unit} (found {found})"),
            LIMIT_FIX.to_string(),
        )
    };
    let parse = |ids: Vec<String>| -> Result<Reply, RdyError> {
        LoadRequest::parse(&ids).map(|_| Reply::default())
    };

    // load ids: 1,024 pass the count, 1,025 do not
    let mut s = source("Bounds hold");
    let d = s.decision.clone();
    let (base, base_ids) = ruleset(
        &mut s.r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        Some("Always."),
        &[rule(&["act:write"], "Say \"why\".\nThen act — é.")],
        &d,
    );
    let (opt, _) = ruleset(
        &mut s.r,
        "w",
        "optional",
        "Optional",
        None,
        Some("Sometimes."),
        &[rule(&["act:review"], "Optional.")],
        &d,
    );
    land(&mut s.r, "w").unwrap();
    let ids: Vec<&str> = std::iter::repeat_n(opt.as_str(), 1_024).collect();
    let ok = deliver(&[(&s.r, Layer::Project)], &ids).unwrap();
    assert_eq!(ok.instructions.len(), 2);
    let ids: Vec<String> = std::iter::repeat_n(opt.clone(), 1_025).collect();
    assert_eq!(refusal(parse(ids)), limit(1_024, "load ids", 1_025));
    let e = LoadRequest::from_json(Some(&json!(vec![opt.clone(); 1_025]))).unwrap_err();
    assert_eq!(e.message, "instructions exceed 1024 load ids (found 1025)");

    // load id bytes: 256 UTF-8 bytes pass the bound (and are no id), 257 do not
    let at = "é".repeat(128);
    assert_eq!(
        refusal(parse(vec![at.clone()])),
        ("E_BAD_INPUT".into(), BAD_LOAD.into(), BAD_LOAD_FIX.into())
    );
    assert_eq!(
        refusal(parse(vec![format!("{at}a")])),
        limit(256, "bytes in one load id", 257)
    );

    // selectors: a stored list of 1,024 passes, 1,025 is refused, even as menu metadata only
    let (_dir, mut r) = (s.dir, s.r);
    let sel = |n: usize, s: &str| {
        rdy_model::FieldValue::List(
            std::iter::repeat_n(rdy_model::FieldValue::Str(s.into()), n).collect(),
        )
    };
    let raw_rule = |id: &str, parent: &str, sels| {
        let mut b = raw(
            id,
            "instruction",
            Some(parent),
            "zz",
            &[],
            "Many selectors.",
        );
        b.fields.insert("applies_to".into(), sels);
        b.links.insert(
            "decided_by".into(),
            vec![rdy_model::Link {
                target: BlockId::parse(&d).unwrap(),
                part: None,
            }],
        );
        b
    };
    raw_commit(
        &mut r,
        "raw1",
        vec![raw_rule("ai-S0001", &base, sel(1_024, "all"))],
    );
    land_with_a_tie(&mut r, "raw1");
    let ok = deliver(&[(&r, Layer::Project)], &[]).unwrap();
    assert_eq!(ok.instructions.len(), 2);
    assert_eq!(ok.menu[0].hints, vec!["act:write", "all"]);
    let long = format!("type:{}", "a".repeat(251));
    assert_eq!(long.len(), 256);
    raw_commit(
        &mut r,
        "raw2",
        vec![raw_rule("ai-S0002", &opt, sel(1, &long))],
    );
    land_with_a_tie(&mut r, "raw2");
    let ok = deliver(&[(&r, Layer::Project)], &[]).unwrap();
    assert_eq!(
        ok.menu[1].hints,
        vec!["act:review".to_string(), long.clone()]
    );
    raw_commit(
        &mut r,
        "raw3",
        vec![raw_rule("ai-S0003", &opt, sel(1_025, "act:review"))],
    );
    land_with_a_tie(&mut r, "raw3");
    assert_eq!(
        refusal(deliver(&[(&r, Layer::Project)], &[])),
        limit(1_024, "selectors in one instruction", 1_025)
    );
    write(&mut r, "fix3", json!([{"op": "delete", "id": "ai-S0003"}])).unwrap();
    land_with_a_tie(&mut r, "fix3");
    raw_commit(
        &mut r,
        "raw4",
        vec![raw_rule("ai-S0004", &opt, sel(1, &format!("{long}a")))],
    );
    land_with_a_tie(&mut r, "raw4");
    assert_eq!(
        refusal(deliver(&[(&r, Layer::Project)], &[])),
        limit(256, "bytes in one selector", 257)
    );
    write(
        &mut r,
        "fix4",
        json!([{"op": "delete", "id": "ai-S0004"}, {"op": "delete", "id": "ai-S0002"}]),
    )
    .unwrap();
    land_with_a_tie(&mut r, "fix4");
    let full = deliver(&[(&r, Layer::Project)], &[&opt]).unwrap();

    // blocks visited and rulesets listed: exact passes, one fewer allowed refuses
    let held = r.read().materialized(&Scope::Canon).unwrap().len();
    let src = [(&r, Layer::Project)];
    let visits = |n: usize| Bounds {
        visits: n,
        ..BOUNDS
    };
    assert_eq!(
        deliver_with(&src, &[], &visits(held)).unwrap(),
        deliver(&src, &[]).unwrap()
    );
    assert_eq!(
        refusal(deliver_with(&src, &[], &visits(held - 1))),
        limit(held as u64 - 1, "blocks in one source", held as u64)
    );
    let rulesets = |n: usize| Bounds {
        rulesets: n,
        ..BOUNDS
    };
    assert!(deliver_with(&src, &[], &rulesets(2)).is_ok());
    assert_eq!(
        refusal(deliver_with(&src, &[], &rulesets(1))),
        limit(1, "rulesets", 2)
    );

    // the reply's JSON bytes, escapes and multibyte text included: exact passes, one past refuses
    let bytes = full.to_json().unwrap().len() as u64;
    assert_eq!(instructions::json_len(&full), bytes);
    let replies = |n: u64| Bounds {
        reply_bytes: n,
        ..BOUNDS
    };
    assert_eq!(deliver_with(&src, &[&opt], &replies(bytes)).unwrap(), full);
    assert_eq!(
        refusal(deliver_with(&src, &[&opt], &replies(bytes - 1))),
        limit(bytes - 1, "bytes of reply JSON", bytes)
    );
    // one source fits, two do not: the merge refuses
    let mut twin = source("Bounds hold");
    let td = twin.decision.clone();
    ruleset(
        &mut twin.r,
        "w",
        "twin",
        "Twin",
        Some(true),
        Some("Twin."),
        &[rule(&["act:write"], "Twin rule.")],
        &td,
    );
    land(&mut twin.r, "w").unwrap();
    let two = [(&r, Layer::Project), (&twin.r, Layer::Person)];
    let both = deliver(&two, &[&opt]).unwrap();
    let total = instructions::json_len(&both);
    assert!(total > bytes);
    assert_eq!(deliver_with(&two, &[&opt], &replies(total)).unwrap(), both);
    assert_eq!(
        refusal(deliver_with(&two, &[&opt], &replies(total - 1))),
        limit(total - 1, "bytes of reply JSON", total)
    );
    // the real bound on a serialized reply, with escapes and multibyte text
    let mut big = Reply::default();
    big.instructions.push(both.instructions[0].clone());
    big.instructions[0].text = String::new();
    let base_len = instructions::json_len(&big);
    let room = BOUNDS.reply_bytes - base_len;
    // each quote is two JSON bytes, each é two UTF-8 bytes; one ASCII byte evens the count
    let quotes = (room / 4) as usize;
    let wide = ((room - 2 * quotes as u64) / 2) as usize;
    let rest = (room - 2 * quotes as u64 - 2 * wide as u64) as usize;
    big.instructions[0].text = format!(
        "{}{}{}",
        "\"".repeat(quotes),
        "é".repeat(wide),
        "a".repeat(rest)
    );
    assert_eq!(instructions::json_len(&big), BOUNDS.reply_bytes);
    assert_eq!(big.to_json().unwrap().len() as u64, BOUNDS.reply_bytes);
    big.instructions[0].text.push('"');
    let e = big.to_json().unwrap_err();
    assert_eq!(
        (e.code, e.message, e.fix.as_str()),
        (
            "E_LIMIT",
            format!(
                "instructions exceed 16777216 bytes of reply JSON (found {})",
                BOUNDS.reply_bytes + 2
            ),
            LIMIT_FIX
        )
    );
    assert_eq!(base_ids.len(), 1);
    r.close().unwrap();
}

// ---- agi2: the built-in layer (D-GEKKY) ----

const HANDLES: [&str; 12] = [
    "rdy:bootstrap",
    "rdy:write",
    "rdy:after-write",
    "rdy:approvals",
    "rdy:as-built",
    "rdy:read",
    "rdy:publish",
    "rdy:review",
    "rdy:diagrams",
    "rdy:parallel",
    "rdy:project-ops",
    "rdy:troubleshoot",
];

/// A person host with a fixed place. `reads` says whether a store may be opened.
struct Host {
    path: PersonPath,
    reads: bool,
}

impl PersonHost for Host {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        Ok(PersonSource {
            path: self.path.clone(),
            origin: None,
            warnings: vec![],
            config_file: None,
        })
    }

    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        _caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        assert!(self.reads, "this call may not read a store");
        let r = Rdy::open(meta)?;
        let part = instructions::collect(&r.read(), Layer::Person, load);
        r.close()?;
        part
    }
}

fn caller() -> Caller {
    Caller {
        actor: "agent".into(),
        run: None,
    }
}

fn req(ids: &[&str], full: bool) -> LoadRequest {
    LoadRequest::parse(&strings(ids)).unwrap().with_full(full)
}

fn at(meta: &Path, reads: bool) -> Host {
    Host {
        path: PersonPath::Override(meta.to_path_buf()),
        reads,
    }
}

/// A call outside any project.
fn projectless(host: &Host, load: &LoadRequest) -> Result<Reply, RdyError> {
    instructions::deliver_with(None, load, &caller(), Some(host), &BOUNDS)
}

/// The built-in rows `names` deliver, in catalogue order: the ids, with the layer checked.
fn builtin_ids(reply: &Reply) -> Vec<String> {
    reply
        .instructions
        .iter()
        .filter(|i| i.layer == Layer::Rdy)
        .map(|i| i.id.clone())
        .collect()
}

fn catalogue_ids(names: &[&str]) -> Vec<String> {
    RULESETS
        .iter()
        .filter(|s| names.contains(&s.name))
        .flat_map(|s| s.rows.iter().map(|r| r.id.to_string()))
        .collect()
}

/// A closed project that holds a required ruleset `req` and an optional ruleset `opt`.
fn rules_project(
    reason: &str,
    req_body: &str,
    opt_body: &str,
    locked: Option<bool>,
) -> (Src, String, String, Vec<String>, Vec<String>) {
    let mut s = source(reason);
    let d = s.decision.clone();
    let (req_doc, req_ids) = ruleset(
        &mut s.r,
        "w",
        "must",
        "Must",
        Some(true),
        Some("Always."),
        &[Rule {
            locked,
            ..rule(&["act:write"], req_body)
        }],
        &d,
    );
    let (opt_doc, opt_ids) = ruleset(
        &mut s.r,
        "w",
        "maybe",
        "Maybe",
        None,
        Some("Sometimes."),
        &[rule(&["act:write"], opt_body)],
        &d,
    );
    land(&mut s.r, "w").unwrap();
    (s, req_doc, opt_doc, req_ids, opt_ids)
}

fn closed(s: Src) -> tempfile::TempDir {
    s.r.close().unwrap();
    s.dir
}

#[test]
fn agi2_fresh_delivery_has_bootstrap_menu_and_setup_hint() {
    let home = tempfile::tempdir().unwrap();
    let meta = home.path().join("meta");
    let host = at(&meta, false);
    let reply = projectless(&host, &req(&[], false)).unwrap();

    // the bootstrap, in three ordered rows, and nothing else
    let boot = &RULESETS[0];
    assert_eq!(boot.name, "bootstrap");
    assert_eq!(boot.rows.len(), 3);
    assert_eq!(reply.instructions.len(), 3);
    // D-Y18E1: the final row carries the bootstrap's value, on a line after its source bytes; the
    // value hashes the source rows, so the line is outside it
    let source: String = boot.rows.iter().map(|r| r.text).collect();
    let digest: String = Sha256::digest(source.as_bytes())
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect();
    let line = format!(
        "On every call to a tool in this ruleset, pass kungfu: \"kf-{digest}\". Tools: rdy_brief, rdy_context, rdy_overview, rdy_pending, rdy_query, rdy_read, rdy_schema, rdy_search, rdy_site, rdy_suspects, rdy_tensions.\n"
    );
    assert!(boot.rows[2].text.ends_with('\n'));
    let delivered = [
        boot.rows[0].text.to_string(),
        boot.rows[1].text.to_string(),
        format!("{}{line}", boot.rows[2].text),
    ];
    for ((got, want), text) in reply.instructions.iter().zip(boot.rows).zip(&delivered) {
        assert_eq!(got.id, want.id);
        assert_eq!(&got.text, text);
        assert_eq!(got.locked, want.locked);
        assert_eq!(got.ruleset, "rdy:bootstrap");
        assert_eq!(got.layer, Layer::Rdy);
        assert!(got.reason.is_empty());
    }
    assert_eq!(
        reply
            .instructions
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        [
            "rdy:bootstrap:before",
            "rdy:bootstrap:approval",
            "rdy:bootstrap:after"
        ]
    );
    assert_eq!(
        reply
            .instructions
            .iter()
            .map(|i| i.locked)
            .collect::<Vec<_>>(),
        [false, true, false]
    );

    // the menu lists all twelve in order; only the bootstrap is required
    assert_eq!(menu_ids(&reply), HANDLES);
    for (m, set) in reply.menu.iter().zip(RULESETS) {
        assert_eq!(m.name, set.title);
        assert_eq!(m.when, set.when);
        assert_eq!(m.layer, Layer::Rdy);
        assert_eq!(m.required, set.name == "bootstrap");
        assert!(m.hints.is_empty());
        // a menu size counts what the ruleset would deliver, its value's line included
        let source: u64 = set.rows.iter().map(|r| r.text.len() as u64).sum();
        let added = match (rdy_engine::kungfu::owner(set.name), set.rows.last()) {
            (Some(owner), Some(last)) => owner.added_bytes(last.text),
            _ => 0,
        };
        if set.name == "bootstrap" {
            assert_eq!(added, line.len() as u64);
        }
        assert_eq!(m.tokens, estimate_tokens(source + added));
        assert!(m.tokens > 0);
    }

    // the hint names the configured path and says to reconnect
    let want = format!(
        "Run rdy setup to create your own rules project at {}. Tell the person when setup finishes. Reconnect this MCP server after setup.",
        meta.display()
    );
    assert_eq!(reply.hint.as_deref(), Some(want.as_str()));
    assert_eq!(
        want,
        format!("{} {}", create_hint(&meta), PROJECTLESS_SUFFIX)
    );
    assert!(reply.warnings.is_empty());
    // no store was made
    assert!(!meta.exists());
}

#[test]
fn agi2_each_builtin_loads_and_full_deduplicates() {
    let home = tempfile::tempdir().unwrap();
    let host = at(&home.path().join("meta"), false);
    let boot = catalogue_ids(&["bootstrap"]);

    for h in HANDLES {
        let name = h.strip_prefix("rdy:").unwrap();
        let want = [boot.clone(), catalogue_ids(&[name])]
            .concat()
            .into_iter()
            .fold(vec![], |mut v, id| {
                if !v.contains(&id) {
                    v.push(id);
                }
                v
            });
        let once = projectless(&host, &req(&[h], false)).unwrap();
        assert_eq!(builtin_ids(&once), want, "{h}");
        // a repeated load adds nothing
        let twice = projectless(&host, &req(&[h, h], false)).unwrap();
        assert_eq!(twice.instructions, once.instructions, "{h}");
        assert_eq!(twice.menu, once.menu, "{h}");
        // the menu stays whole
        assert_eq!(menu_ids(&once), HANDLES);
    }

    // the bootstrap named on its own arrives once
    let b = projectless(&host, &req(&["rdy:bootstrap"], false)).unwrap();
    assert_eq!(builtin_ids(&b), boot);

    // full: every row once, in menu order, with or without explicit loads
    let all: Vec<String> = RULESETS
        .iter()
        .flat_map(|s| s.rows.iter().map(|r| r.id.to_string()))
        .collect();
    assert_eq!(all.len(), 14);
    for load in [vec![], vec!["rdy:bootstrap", "rdy:write"], HANDLES.to_vec()] {
        let r = projectless(&host, &req(&load, true)).unwrap();
        assert_eq!(builtin_ids(&r), all, "{load:?}");
        assert_eq!(r.instructions.len(), 14);
    }

    // full plus a mixed layer load: built-ins once, the named stored ruleset whole
    let (pj, _, p_opt, _, p_opt_ids) =
        rules_project("Project reason", "Project must.", "Project may.", None);
    let (m, _, m_opt, _, m_opt_ids) =
        rules_project("Person reason", "Person must.", "Person may.", None);
    let meta_dir = closed(m);
    let host = at(meta_dir.path(), true);
    let mixed = [
        format!("project:{p_opt}"),
        format!("person:{m_opt}"),
        "rdy:write".into(),
    ];
    let load = req(&mixed.iter().map(String::as_str).collect::<Vec<_>>(), true);
    let st = pj.r.read();
    let r = instructions::deliver_with(
        Some((&st, pj.dir.path())),
        &load,
        &caller(),
        Some(&host),
        &BOUNDS,
    )
    .unwrap();
    assert_eq!(builtin_ids(&r), all);
    let ids: Vec<&str> = r.instructions.iter().map(|i| i.id.as_str()).collect();
    assert!(ids.contains(&p_opt_ids[0].as_str()) && ids.contains(&m_opt_ids[0].as_str()));
    assert_eq!(r.instructions.len(), 14 + 2 + 2);
    assert_eq!(r.menu.len(), 12 + 4);
}

#[test]
fn agi2_full_leaves_other_optional_rules_unloaded() {
    let (pj, p_req, p_opt, p_req_ids, p_opt_ids) =
        rules_project("Project reason", "Project must.", "Project may.", None);
    let (m, m_req, m_opt, m_req_ids, m_opt_ids) =
        rules_project("Person reason", "Person must.", "Person may.", None);
    let meta_dir = closed(m);
    let host = at(meta_dir.path(), true);
    let st = pj.r.read();
    let call = |load: &LoadRequest| {
        instructions::deliver_with(
            Some((&st, pj.dir.path())),
            load,
            &caller(),
            Some(&host),
            &BOUNDS,
        )
        .unwrap()
    };

    let full = call(&req(&[], true));
    let stored: Vec<(String, String)> = full
        .instructions
        .iter()
        .filter(|i| i.layer != Layer::Rdy)
        .map(|i| (i.id.clone(), i.layer.to_string()))
        .collect();
    // the required rules arrive, the optional ones stay on the menu
    assert_eq!(
        stored,
        vec![
            (p_req_ids[0].clone(), "project".to_string()),
            (m_req_ids[0].clone(), "person".to_string()),
        ]
    );
    let menu = menu_ids(&full);
    for h in [p(&p_req), p(&p_opt), q(&m_req), q(&m_opt)] {
        assert!(menu.contains(&h), "{h}");
    }
    assert_eq!(builtin_ids(&full).len(), 14);

    // a named optional rule arrives with full, and only that one
    let named = call(&req(&[&q(&m_opt)], true));
    let ids: Vec<&str> = named.instructions.iter().map(|i| i.id.as_str()).collect();
    assert!(ids.contains(&m_opt_ids[0].as_str()));
    assert!(!ids.contains(&p_opt_ids[0].as_str()));
    assert_eq!(builtin_ids(&named).len(), 14);

    // without full, the built-in optional rulesets stay unloaded
    let plain = call(&req(&[], false));
    assert_eq!(builtin_ids(&plain), catalogue_ids(&["bootstrap"]));
}

#[test]
fn agi2_layers_keep_order_and_the_approval_lock() {
    let (pj, _, _, p_ids, _) = rules_project("Project reason", "Use tabs.", "Unused.", Some(true));
    let (m, _, _, m_ids, _) = rules_project("Person reason", "Use spaces.", "Unused.", Some(false));
    let meta_dir = closed(m);
    let host = at(meta_dir.path(), true);
    let st = pj.r.read();
    let reply = instructions::deliver_with(
        Some((&st, pj.dir.path())),
        &req(&[], false),
        &caller(),
        Some(&host),
        &BOUNDS,
    )
    .unwrap();

    // rdy first, then project, then person; the disagreeing rules both stay
    let order: Vec<(String, String)> = reply
        .instructions
        .iter()
        .map(|i| (i.id.clone(), i.layer.to_string()))
        .collect();
    let mut want: Vec<(String, String)> = catalogue_ids(&["bootstrap"])
        .into_iter()
        .map(|id| (id, "rdy".to_string()))
        .collect();
    want.push((p_ids[0].clone(), "project".into()));
    want.push((m_ids[0].clone(), "person".into()));
    assert_eq!(order, want);
    assert_eq!(
        reply
            .menu
            .iter()
            .map(|m| m.layer.to_string())
            .collect::<Vec<_>>()[..13]
            .iter()
            .filter(|l| *l == "rdy")
            .count(),
        12
    );
    let layers: Vec<String> = reply.menu.iter().map(|m| m.layer.to_string()).collect();
    assert!(layers[..12].iter().all(|l| l == "rdy"));
    assert_eq!(layers[12..], ["project", "project", "person", "person"]);

    // the approval paragraph is the one locked row, and its lock has force
    let locked: Vec<&InstructionRow> = reply
        .instructions
        .iter()
        .filter(|i| i.layer == Layer::Rdy && i.locked)
        .collect();
    assert_eq!(locked.len(), 1);
    assert_eq!(locked[0].id, "rdy:bootstrap:approval");
    assert!(Layer::Rdy.locks_have_force());
    // the project's own lock is shown and has no force; the person's rule is not removed
    let proj = reply
        .instructions
        .iter()
        .find(|i| i.id == p_ids[0])
        .unwrap();
    assert!(proj.locked && !proj.layer.locks_have_force());
    assert_eq!(proj.text, "Use tabs.");
    let pers = reply
        .instructions
        .iter()
        .find(|i| i.id == m_ids[0])
        .unwrap();
    assert_eq!(pers.text, "Use spaces.");

    // a host cannot hand the engine a stored source as the rdy layer
    let e = instructions::collect(&st, Layer::Rdy, &req(&[], false)).unwrap_err();
    assert_eq!(e.code, "E_INTERNAL", "{e}");
    // two rdy sources are a host error
    let b = instructions::builtin(&req(&[], false)).unwrap();
    let e = instructions::merge(vec![b.clone(), b], &req(&[], false)).unwrap_err();
    assert_eq!(e.code, "E_INTERNAL", "{e}");
}

#[test]
fn agi2_handles_reasons_and_limits_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let host = at(&home.path().join("meta"), false);

    // malformed handles are bad input, before any delivery
    for bad in [
        "rdy:",
        "rdy:Write",
        "rdy:-write",
        "rdy:write-",
        "rdy:a--b",
        "rdy:write:body",
        "rdy: write",
        "rdy:wr ite",
        "rdy:wrïte",
        &format!("rdy:{}", "a".repeat(65)),
        "write",
        "team:x",
    ] {
        let e = LoadRequest::parse(&strings(&[bad])).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_BAD_INPUT", BAD_LOAD, BAD_LOAD_FIX),
            "{bad}"
        );
    }
    // a valid unknown name is not found, and a bare name never resolves
    for unknown in [
        "rdy:nope",
        "rdy:bootstrap2",
        &format!("rdy:{}", "a".repeat(64)),
    ] {
        assert_eq!(
            refusal(projectless(&host, &req(&[unknown], false))),
            (
                "E_NOT_FOUND".into(),
                format!("ruleset {unknown} was not found"),
                NOT_FOUND_FIX.into()
            ),
            "{unknown}"
        );
    }
    // a bad handle beside full is still refused
    assert!(LoadRequest::parse(&strings(&["rdy:Write"])).is_err());
    assert_eq!(
        refusal(projectless(&host, &req(&["rdy:write", "rdy:nope"], true))).0,
        "E_NOT_FOUND"
    );
    // a stored doc id is no built-in name
    let (_pj, p_req, _, _, _) = rules_project("Project reason", "Must.", "May.", None);
    assert!(LoadRequest::parse(&strings(&[&format!("rdy:{p_req}")])).is_err());

    // a stale reason in a stored rule still refuses the whole call
    let mut bad = source("Stale reasons");
    let d = bad.decision.clone();
    let (base, _) = ruleset(
        &mut bad.r,
        "w",
        "baseline",
        "Baseline",
        Some(true),
        None,
        &[rule(&["act:write"], "Keep.")],
        &d,
    );
    land(&mut bad.r, "w").unwrap();
    let mut broken = raw(
        "ai-BRKN0",
        "instruction",
        Some(&base),
        "zz",
        &[],
        "No reason.",
    );
    broken.fields.insert(
        "applies_to".into(),
        rdy_model::FieldValue::List(vec![rdy_model::FieldValue::Str("all".into())]),
    );
    raw_commit(&mut bad.r, "raw", vec![broken]);
    land_with_a_tie(&mut bad.r, "raw");
    let st_bad = bad.r.read();
    let e = instructions::deliver_with(
        Some((&st_bad, bad.dir.path())),
        &req(&[], true),
        &caller(),
        Some(&host),
        &BOUNDS,
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_VALIDATION",
            "project source: instruction ai-BRKN0 requires a decided_by decision",
            REASON_FIX
        )
    );

    // reduced reply bounds refuse, exactly at the size of the reply, with no partial rows
    for load in [req(&[], false), req(&[], true)] {
        let ok = projectless(&host, &load).unwrap();
        let n = instructions::json_len(&ok);
        let tight = Bounds {
            reply_bytes: n,
            ..BOUNDS
        };
        let r = instructions::deliver_with(None, &load, &caller(), Some(&host), &tight).unwrap();
        assert_eq!(r, ok);
        for less in [n - 1, 100, 0] {
            let small = Bounds {
                reply_bytes: less,
                ..BOUNDS
            };
            let e = instructions::deliver_with(None, &load, &caller(), Some(&host), &small)
                .unwrap_err();
            assert_eq!(e.code, "E_LIMIT", "{less}");
            assert_eq!(e.fix, LIMIT_FIX);
            assert!(
                e.message.starts_with("instructions exceed "),
                "{}",
                e.message
            );
        }
    }
    // the built-in part alone is bounded too
    let tiny = Bounds {
        reply_bytes: 10,
        ..BOUNDS
    };
    let e = instructions::builtin_with(&req(&[], false), &tiny).unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
}

#[test]
fn agi2_meta_and_projectless_hints_follow_connection_state() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("absent");
    let setup = |m: &Path| {
        format!(
            "Run rdy setup to create your own rules project at {}. Tell the person when setup finishes.",
            m.display()
        )
    };

    // missing meta: the projectless hint adds the reconnect line, a project's does not
    let host = at(&missing, false);
    let r = projectless(&host, &req(&[], false)).unwrap();
    assert_eq!(
        r.hint,
        Some(format!(
            "{} Reconnect this MCP server after setup.",
            setup(&missing)
        ))
    );
    let (pj, _, _, _, _) = rules_project("Project reason", "Must.", "May.", None);
    let st = pj.r.read();
    let in_project = |host: &Host| {
        instructions::deliver_with(
            Some((&st, pj.dir.path())),
            &req(&[], false),
            &caller(),
            Some(host),
            &BOUNDS,
        )
    };
    let r = in_project(&host).unwrap();
    assert_eq!(r.hint, Some(setup(&missing)));
    assert_eq!(builtin_ids(&r), catalogue_ids(&["bootstrap"]));

    // a file where the meta project belongs is no project: the same missing hint
    let file = home.path().join("file");
    std::fs::write(&file, "x").unwrap();
    let r = projectless(&at(&file, false), &req(&[], false)).unwrap();
    assert_eq!(
        r.hint,
        Some(format!(
            "{} Reconnect this MCP server after setup.",
            setup(&file)
        ))
    );

    // an existing meta project: a projectless connection is told to reconnect, a project's
    // session has no hint, and the person rules arrive
    let (m, m_req, _, m_req_ids, _) = rules_project("Person reason", "Mine.", "Maybe mine.", None);
    let meta_dir = closed(m);
    let host = at(meta_dir.path(), true);
    let r = projectless(&host, &req(&[], false)).unwrap();
    assert_eq!(
        r.hint.as_deref(),
        Some(
            "Your own rules project is ready. Reconnect this MCP server to use it. For other work, run rdy init in a folder and reconnect."
        )
    );
    assert_eq!(r.hint.as_deref(), Some(META_READY_HINT));
    assert!(r.menu.iter().any(|m| m.id == q(&m_req)));
    assert!(r.instructions.iter().any(|i| i.id == m_req_ids[0]));
    let r = in_project(&host).unwrap();
    assert_eq!(r.hint, None);

    // a session on the meta project itself
    let (s, s_req, _, s_ids, _) = rules_project("Own reason", "Own.", "Own maybe.", None);
    let own = s.dir.path().to_path_buf();
    let st_own = s.r.read();
    let on_meta = Host {
        path: PersonPath::Override(own.clone()),
        reads: false,
    };
    let r = instructions::deliver_with(
        Some((&st_own, &own)),
        &req(&[], false),
        &caller(),
        Some(&on_meta),
        &BOUNDS,
    )
    .unwrap();
    assert_eq!(
        r.hint.as_deref(),
        Some(
            "This session is on your own rules project. Use it for your rules. For other work, run rdy init in a folder and reconnect."
        )
    );
    assert_eq!(r.hint.as_deref(), Some(META_SESSION_HINT));
    assert!(
        r.menu
            .iter()
            .any(|m| m.id == q(&s_req) && m.layer == Layer::Person)
    );
    assert!(
        r.instructions
            .iter()
            .any(|i| i.id == s_ids[0] && i.layer == Layer::Person)
    );
    s.r.close().unwrap_or(());

    // no home and no override: the old message, built-ins unchanged
    let nohome = Host {
        path: PersonPath::Unavailable,
        reads: false,
    };
    let r = projectless(&nohome, &req(&[], false)).unwrap();
    assert_eq!(r.hint.as_deref(), Some(NO_HOME_MESSAGE));
    assert_eq!(builtin_ids(&r), catalogue_ids(&["bootstrap"]));
    let r = in_project(&nohome).unwrap();
    assert_eq!(r.hint.as_deref(), Some(NO_HOME_MESSAGE));

    // a meta path that looks like a project but is broken stays a visible failure
    let broken = home.path().join("broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join(".rdy.yaml"), "not: a project: at all\n").unwrap();
    let bad = at(&broken, true);
    assert!(projectless(&bad, &req(&[], false)).is_err());
}
