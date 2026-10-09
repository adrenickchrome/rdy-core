//! tonc1: write responses carry on_change tensions and tension consequences.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use rdy_engine::consequence::{consequence_id, write_and_report};
use rdy_engine::tensions::delta::on_change;
use rdy_engine::tensions::{TensionDelta, tension_delta};
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::{Engine, Local};
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::{Value, json};

fn local() -> (tempfile::TempDir, Local, Spec) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    (d, Local::new(r), s)
}
fn w(e: &Local, who: &str, ops: Value) -> Value {
    let who2 = who.to_string();
    write_and_report(e, who, None, move |r| try_put(r, &who2, ops)).unwrap()
}

#[test]
fn tonc1_a_write_that_opens_a_tension_reports_it_in_on_change_and_as_a_consequence() {
    let (_d, e, s) = local();
    let vb = w(&e, "b", cite(&s.section, &s.a));
    assert_eq!(vb["on_change"], json!({"tensions": [], "resolved": []}));
    let y = vb["created"]["$y"].as_str().unwrap().to_string();
    let va = w(&e, "a", upd(&s.a, "A changed."));
    let opened = va["on_change"]["tensions"].as_array().unwrap();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["kind"], "premise_shift");
    let entry = &va["consequences"][0];
    assert_eq!(entry["kind"], "tension");
    assert_eq!(entry["follow_up"], "resolve");
    assert_eq!(entry["tension"], opened[0]["id"]);
    assert_eq!(entry["block"], y);
    assert_eq!(
        entry["because"],
        json!({"edge": "premise_shift", "changed": s.a.to_string()})
    );
    let change: Hash = va["change"].as_str().unwrap().parse().unwrap();
    let t: Hash = opened[0]["tension"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        entry["id"],
        consequence_id(
            &change,
            &format!("tension {t}"),
            &BlockId::parse(&y).unwrap()
        )
    );
}

#[test]
fn tonc1_a_write_that_resolves_a_tension_lists_it_as_resolved() {
    let (_d, e, s) = local();
    let vb = w(&e, "b", cite(&s.section, &s.a));
    let y = vb["created"]["$y"].as_str().unwrap().to_string();
    let va = w(&e, "a", upd(&s.a, "A changed."));
    let tn = va["on_change"]["tensions"][0]["id"].clone();
    w(&e, "a", upd(&s.c, "C too."));
    let r = e
        .write(Box::new(|r| {
            rdy_qa::fixtures::land(r, "a").map(Value::String)
        }))
        .unwrap();
    assert!(r.is_string());
    let vr = w(&e, "b", json!([{"op": "repin", "id": y}]));
    let resolved = vr["on_change"]["resolved"].as_array().unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["kind"], "premise_shift");
    assert_ne!(
        resolved[0]["id"], tn,
        "the landing re-keyed it; the repin resolves the re-keyed one"
    );
}

#[test]
fn tonc1_an_include_that_puts_both_sides_in_one_view_resolves_a_shift() {
    let (_d, e, s) = local();
    w(&e, "b", cite(&s.section, &s.a));
    let va = w(&e, "a", upd(&s.a, "A changed."));
    let tn = va["on_change"]["tensions"][0]["id"].clone();
    let v = run_view(
        &e,
        &ag("b"),
        "b",
        ViewRequest {
            op: "include".into(),
            view: Some("b".into()),
            changes: Some(vec![va["change"].as_str().unwrap().to_string()]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(v["on_change"]["resolved"][0]["id"], tn, "{v}");
}

fn w_max(e: &Local, who: &str, max: usize, ops: Value) -> Value {
    let who2 = who.to_string();
    write_and_report(e, who, Some(max), move |r| try_put(r, &who2, ops)).unwrap()
}

/// View a holds term Fee and an edit of A. The returned ops (for view b) cite A (one blocking
/// premise shift) and create four more terms named fee (ten advisory duplicate definitions).
fn crowd(e: &Local, s: &Spec) -> Value {
    w(e, "a", term(&s.section, "Fee"));
    w(e, "a", upd(&s.a, "A changed."));
    let mut ops = cite(&s.section, &s.a).as_array().unwrap().clone();
    for i in 0..4 {
        ops.push(
            json!({"op": "create", "type": "term", "id": format!("$t{i}"),
            "parent": s.section.to_string(), "fields": {"name": "fee"},
            "body": format!("Fee number {i} is a term.")}),
        );
    }
    Value::Array(ops)
}

#[test]
fn tonc1_on_change_honours_max_tokens_keeps_every_blocking_tension_and_counts_advisory_ones_per_kind()
 {
    let (_d, e, s) = local();
    let ops = crowd(&e, &s);
    let full = w(&e, "b", ops);
    assert_eq!(
        full["on_change"]["tensions"].as_array().unwrap().len(),
        11,
        "{full}"
    );
    assert!(
        full["on_change"].get("collapsed").is_none()
            && full["on_change"].get("detail_cut").is_none()
    );

    let (_d2, e2, s2) = local();
    let ops = crowd(&e2, &s2);
    let cut = w_max(&e2, "b", 50, ops);
    let kept = cut["on_change"]["tensions"].as_array().unwrap();
    assert_eq!(kept.len(), 1, "{cut}");
    assert_eq!(kept[0]["severity"], "blocking");
    assert_eq!(kept[0]["kind"], "premise_shift");
    assert_eq!(cut["on_change"]["detail_cut"], true);
    assert_eq!(
        cut["on_change"]["collapsed"],
        json!({"duplicate_definition": 10})
    );
}

/// Lands `pairs` duplicate-definition pairs on canon: standing all-canon tensions no later
/// write touches.
fn standing(e: &Local, s: &Spec, pairs: usize) {
    let mut ops = vec![];
    for i in 0..pairs * 2 {
        ops.push(
            json!({"op": "create", "type": "term", "id": format!("$t{i}"),
            "parent": s.section.to_string(), "fields": {"name": format!("standing {}", i / 2)},
            "body": format!("Standing {i} is a term.")}),
        );
    }
    w(e, "a", Value::Array(ops));
    e.write(Box::new(|r| {
        rdy_qa::fixtures::land(r, "a").map(Value::String)
    }))
    .unwrap();
}

fn ids(v: &[std::sync::Arc<rdy_engine::tensions::Tension>]) -> Vec<Hash> {
    v.iter().map(|t| t.id).collect()
}

fn full_delta(before: &rdy_engine::ReadState, after: &rdy_engine::ReadState) -> TensionDelta {
    let (opened, resolved) = before.tensions().unwrap().diff(&after.tensions().unwrap());
    TensionDelta {
        opened,
        resolved,
        examined: 0,
    }
}

#[test]
fn tonc1_a_write_with_unrelated_standing_canon_tensions_does_not_look_at_them() {
    let mut examined = vec![];
    for pairs in [1, 12] {
        let (_d, e, s) = local();
        standing(&e, &s, pairs);
        let before = e.read();
        let n = before.tensions().unwrap().len();
        assert!(n >= pairs, "{pairs} pairs stand: {n} tensions");
        let v = w(&e, "b", cite(&s.section, &s.a));
        let after = e.read();
        let change: Hash = v["change"].as_str().unwrap().parse().unwrap();
        let d = tension_delta(&before, &after, Some(&[change])).unwrap();
        let f = full_delta(&before, &after);
        assert_eq!(ids(&d.opened), ids(&f.opened));
        assert_eq!(ids(&d.resolved), ids(&f.resolved));
        assert!(d.examined < n, "looked at {} of {n}", d.examined);
        examined.push(d.examined);
    }
    assert_eq!(
        examined[0], examined[1],
        "what a write looks at does not grow with the standing tensions"
    );
}

/// A view op's reply and a write's reply carry what the full diff of the two sets gives.
fn check_step(e: &Local, view: &str, before: &rdy_engine::ReadState, reply: &Value) {
    let after = e.read();
    let full = full_delta(before, &after);
    let want = on_change(before, &after, view, None, &full).unwrap();
    assert_eq!(
        reply["on_change"],
        serde_json::to_value(&want).unwrap(),
        "scoped != full: {reply}"
    );
}

proptest! {
    #![proptest_config(Config { cases: 24, rng_seed: RngSeed::Fixed(11), ..Config::default() })]
    #[test]
    fn tonc1_the_scoped_delta_equals_the_full_diff(
        steps in proptest::collection::vec((0u8..6, 0u8..8), 1..10)
    ) {
        let (_d, e, s) = local();
        standing(&e, &s, 2);
        // view b starts as a fresh view over the landed canon
        let mut known = vec![s.a.clone(), s.c.clone(), s.section.clone()];
        let mut mine: Vec<(&str, Hash)> = vec![];
        for (n, (kind, pick)) in steps.into_iter().enumerate() {
            let view = if pick % 2 == 0 { "a" } else { "b" };
            let other = if view == "a" { "b" } else { "a" };
            let before = e.read();
            let target = known[pick as usize % known.len()].clone();
            let ops = match kind {
                0 => Some(cite(&s.section, &target)),
                1 => Some(upd(&target, &format!("Edit {n}."))),
                2 => Some(term(&s.section, ["standing 0", "Fee", "fee", "cost"][pick as usize % 4])),
                _ => None,
            };
            if let Some(ops) = ops {
                let v2 = view.to_string();
                let Ok(r) = write_and_report(&e, view, None, move |r| try_put(r, &v2, ops)) else {
                    continue;
                };
                if let Some(y) = r["created"].as_object().and_then(|m| m.values().next()).and_then(|y| y.as_str())
                    && let Ok(id) = BlockId::parse(y)
                {
                    known.push(id);
                }
                mine.push((view, r["change"].as_str().unwrap().parse().unwrap()));
                let change: Hash = r["change"].as_str().unwrap().parse().unwrap();
                let after = e.read();
                let d = tension_delta(&before, &after, Some(&[change])).unwrap();
                let f = full_delta(&before, &after);
                prop_assert_eq!(ids(&d.opened), ids(&f.opened));
                prop_assert_eq!(ids(&d.resolved), ids(&f.resolved));
                check_step(&e, view, &before, &r);
            } else {
                // a view op: include another view's change, or exclude one of this view's
                let (op, from) = if kind == 3 { ("include", other) } else { ("exclude", view) };
                let Some((_, c)) = mine.iter().rev().find(|(v, _)| *v == from) else { continue };
                let reply = run_view(&e, &ag(view), view, ViewRequest {
                    op: op.into(),
                    view: Some(view.into()),
                    changes: Some(vec![c.to_string()]),
                    ..Default::default()
                });
                let Ok(reply) = reply else { continue };
                check_step(&e, view, &before, &reply);
            }
        }
    }
}

/// A landed doc at `path` with one section, and (when `claim`) one claim under it: the section
/// and the claim.
fn landed_doc(r: &mut rdy_engine::Rdy, path: &str, claim: bool) -> (BlockId, Option<BlockId>) {
    r.new_doc(&writer(), "seed", path, Some("glossary"), Some("Doc"))
        .unwrap();
    let doc = r
        .read()
        .resolve_path(&rdy_engine::Scope::View("seed".into()), path)
        .unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "section", "id": "$s", "parent": doc.to_string(), "fields": {"title": "S"}}),
    ];
    if claim {
        ops.push(json!({"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "Fees are monthly."}));
    }
    let w = write(r, "seed", json!(ops)).unwrap();
    land(r, "seed").unwrap();
    (created(&w, "$s"), claim.then(|| created(&w, "$x")))
}

/// The QA pack with the spec doc, a landed claim X in area payments and a landed section N in
/// no area; decisions in views a and b under N both affect X, so they overlap in payments only
/// through X (the overlap involves neither X nor any block a delete of X writes).
struct Overlap {
    _d: tempfile::TempDir,
    e: Local,
    s: Spec,
    fees: BlockId,
    x: BlockId,
    n: BlockId,
    overlap: Hash,
}

fn overlap() -> Overlap {
    let (d, mut r) = rdy_qa::packs::qa_project();
    let s = spec_doc(&mut r);
    let (fees, x) = landed_doc(&mut r, "payments/fees", true);
    let x = x.unwrap();
    let (n, _) = landed_doc(&mut r, "notes/misc", false);
    for who in ["a", "b"] {
        r.decide(
            &ag(who),
            who,
            rdy_engine::DecisionInput {
                parent: n.to_string(),
                statement: format!("{who} decides"),
                affects: vec![x.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    let overlap = one(&r, rdy_engine::tensions::TensionKind::AreaOverlap).id;
    Overlap {
        _d: d,
        e: Local::new(r),
        s,
        fees,
        x,
        n,
        overlap,
    }
}

#[test]
fn tonc1_a_delete_that_resolves_a_tension_away_from_its_seeds_reports_it() {
    let o = overlap();
    let before = o.e.read();
    let v = w(&o.e, "a", json!([{"op": "delete", "id": o.x.to_string()}]));
    let after = o.e.read();
    let full = full_delta(&before, &after);
    assert_eq!(
        ids(&full.resolved),
        vec![o.overlap],
        "the delete resolves the overlap"
    );
    let change: Hash = v["change"].as_str().unwrap().parse().unwrap();
    let d = tension_delta(&before, &after, Some(&[change])).unwrap();
    assert_eq!(ids(&d.opened), ids(&full.opened));
    assert_eq!(ids(&d.resolved), ids(&full.resolved));
    let resolved = v["on_change"]["resolved"].as_array().unwrap();
    assert_eq!(resolved.len(), 1, "{v}");
    assert_eq!(resolved[0]["kind"], "area_overlap");
    check_step(&o.e, "a", &before, &v);
}

#[test]
fn tonc1_an_exclude_of_that_delete_reports_the_reopened_tension() {
    let o = overlap();
    let v = w(&o.e, "a", json!([{"op": "delete", "id": o.x.to_string()}]));
    let before = o.e.read();
    let reply = run_view(
        &o.e,
        &ag("a"),
        "a",
        ViewRequest {
            op: "exclude".into(),
            view: Some("a".into()),
            changes: Some(vec![v["change"].as_str().unwrap().to_string()]),
            ..Default::default()
        },
    )
    .unwrap();
    let after = o.e.read();
    let full = full_delta(&before, &after);
    assert_eq!(
        ids(&full.opened),
        vec![o.overlap],
        "the exclude reopens the overlap"
    );
    let opened = reply["on_change"]["tensions"].as_array().unwrap();
    assert_eq!(opened.len(), 1, "{reply}");
    assert_eq!(opened[0]["kind"], "area_overlap");
    check_step(&o.e, "a", &before, &reply);
}

/// One random step of the wide property: ops that carry and ops that do not.
fn wide_ops(
    o: &Overlap,
    kind: u8,
    pick: usize,
    n: usize,
    known: &[BlockId],
    sections: &[BlockId],
) -> Value {
    let target = &known[pick % known.len()];
    let section = &sections[pick % sections.len()];
    let parent = if pick.is_multiple_of(2) {
        &o.s.doc
    } else {
        &o.fees
    };
    let lifecycle = ["draft", "stable", "deprecated"][pick % 3];
    match kind {
        0 => cite(&o.s.section, target),
        1 => upd(target, &format!("Edit {n}.")),
        2 => term(&o.s.section, ["standing 0", "Fee", "fee", "cost"][pick % 4]),
        // a delete (never carries)
        3 => json!([{"op": "delete", "id": target.to_string()}]),
        // a move (never carries)
        4 => json!([{"op": "move", "id": target.to_string(), "parent": section.to_string()}]),
        // structural writes: a section create, a section delete, a doc create
        5 => json!([{"op": "create", "type": "section", "id": "$s",
            "parent": parent.to_string(), "fields": {"title": format!("New {n}")}}]),
        // a section delete: of an empty one this run created, when there is one
        6 => {
            let made = &sections[2.min(sections.len() - 1)..];
            json!([{"op": "delete", "id": made[pick % made.len()].to_string()}])
        }
        7 => json!([{"op": "create", "type": "doc", "id": "$d",
            "fields": {"name": format!("extra{n}"), "title": "Extra", "doc_type": "overview"}}]),
        // a lifecycle change
        _ => json!([{"op": "update", "id": section.to_string(),
            "fields": {"lifecycle": lifecycle}}]),
    }
}

proptest! {
    #![proptest_config(Config { cases: 48, rng_seed: RngSeed::Fixed(12), ..Config::default() })]
    #[test]
    fn tonc1_the_scoped_delta_equals_the_full_diff_for_every_kind_of_step(
        steps in proptest::collection::vec((0u8..13, 0u8..16), 1..10)
    ) {
        let o = overlap();
        let e = &o.e;
        let mut known = vec![o.s.a.clone(), o.s.c.clone(), o.s.section.clone(), o.x.clone(), o.n.clone(), o.s.decision.clone()];
        let mut sections = vec![o.s.section.clone(), o.n.clone()];
        let mut mine: Vec<(&str, Hash)> = vec![];
        for (n, (kind, pick)) in steps.into_iter().enumerate() {
            let pick = pick as usize;
            let view = if pick.is_multiple_of(2) { "a" } else { "b" };
            let other = if view == "a" { "b" } else { "a" };
            let before = e.read();
            let r = match kind {
                0..=8 => {
                    let ops = wide_ops(&o, kind, pick / 2, n, &known, &sections);
                    let v2 = view.to_string();
                    write_and_report(e, view, None, move |r| try_put(r, &v2, ops))
                }
                // a decision with `affects`
                9 => {
                    let (target, parent) = (known[pick % known.len()].to_string(), o.n.to_string());
                    let v2 = view.to_string();
                    write_and_report(e, view, None, move |r| r.decide(&ag(&v2), &v2, rdy_engine::DecisionInput {
                        parent,
                        statement: format!("Decision {n}"),
                        affects: vec![target],
                        ..Default::default()
                    }))
                }
                _ => {
                    // a view op: include another view's change, exclude one of this view's, or undo
                    let req = match kind {
                        10 | 11 => {
                            let (op, from) = if kind == 10 { ("include", other) } else { ("exclude", view) };
                            let Some((_, c)) = mine.iter().rev().find(|(v, _)| *v == from) else { continue };
                            ViewRequest { op: op.into(), view: Some(view.into()),
                                changes: Some(vec![c.to_string()]), ..Default::default() }
                        }
                        _ => ViewRequest { op: "undo".into(), view: Some(view.into()), ..Default::default() },
                    };
                    let Ok(reply) = run_view(e, &ag(view), view, req) else { continue };
                    check_step(e, view, &before, &reply);
                    continue;
                }
            };
            let Ok(r) = r else { continue };
            for y in r["created"].as_object().into_iter().flat_map(|m| m.values()) {
                if let Some(id) = y.as_str().and_then(|y| BlockId::parse(y).ok()) {
                    if kind == 5 { sections.push(id.clone()); }
                    known.push(id);
                }
            }
            let change: Hash = r["change"].as_str().unwrap().parse().unwrap();
            mine.push((view, change));
            let after = e.read();
            let d = tension_delta(&before, &after, Some(&[change])).unwrap();
            let f = full_delta(&before, &after);
            prop_assert_eq!(ids(&d.opened), ids(&f.opened), "step {}", n);
            prop_assert_eq!(ids(&d.resolved), ids(&f.resolved), "step {}", n);
            check_step(e, view, &before, &r);
        }
    }
}

/// A detector that always fails.
struct Failing;
impl rdy_engine::tensions::Detector for Failing {
    fn kind(&self) -> rdy_engine::tensions::TensionKind {
        rdy_engine::tensions::TensionKind::EditCollision
    }
    fn detect(
        &self,
        _cx: &rdy_engine::tensions::DetectCx<'_>,
        _seeds: Option<&std::collections::BTreeSet<BlockId>>,
        _out: &mut Vec<rdy_engine::tensions::Found>,
    ) -> Result<(), rdy_engine::RdyError> {
        Err(rdy_engine::RdyError::new(
            rdy_engine::E_INTERNAL,
            "boom",
            "a test detector",
        ))
    }
}
static FAILING: Failing = Failing;
static FAILING_DETS: [&dyn rdy_engine::tensions::Detector; 1] = [&FAILING];

/// Final review M14: a tension set that cannot be built never fails a write's consequences or
/// a briefing (a briefing gates writes); each says so in its own error field.
#[test]
fn tonc1_a_tension_read_error_never_fails_the_consequences_or_a_briefing() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.set_detectors_for_tests(&FAILING_DETS);
    let e = Local::new(r);
    let v = w(&e, "a", upd(&s.c, "C changed."));
    assert!(v.get("consequences_error").is_none(), "{v:#}");
    assert!(v["consequences"].is_array(), "{v:#}");
    assert!(
        v["consequences_tension_error"]
            .as_str()
            .is_some_and(|m| m.contains("boom")),
        "{v:#}"
    );
    assert!(
        v["on_change_error"]
            .as_str()
            .is_some_and(|m| m.contains("boom")),
        "{v:#}"
    );
    let cov = rdy_engine::brief::Coverage {
        ids: vec![s.a.to_string()],
        path: None,
    };
    let b = e
        .read()
        .brief(&rdy_engine::Scope::Canon, &cov, None)
        .unwrap();
    assert!(
        b.inflight
            .tensions_error
            .as_deref()
            .is_some_and(|m| m.contains("boom")),
        "{:?}",
        b.inflight
    );
}

/// The cite of A (in view b, one blocking premise shift once view a changed A) and `n` terms
/// named fee: n·(n−1)/2 advisory duplicate definitions, all in one change.
fn fees(s: &Spec, n: usize) -> Value {
    let mut ops = cite(&s.section, &s.a).as_array().unwrap().clone();
    for i in 0..n {
        ops.push(
            json!({"op": "create", "type": "term", "id": format!("$f{i}"),
            "parent": s.section.to_string(), "fields": {"name": "fee"},
            "body": format!("Fee number {i} is a term.")}),
        );
    }
    Value::Array(ops)
}

/// Perf fix (mcp_write_9mib): a write that opens thousands of advisory tensions returns
/// within `max_tokens`, keeps its blocking tension, counts the advisory ones per kind, builds
/// no sides for a reply whose bare rows already overflow, serializes each row a bounded number
/// of times, and reads each change's provenance at most once.
#[test]
fn tonc1_a_write_that_opens_thousands_of_advisory_tensions_is_cut_in_linear_time() {
    use rdy_engine::brief::BRIEF_DEFAULT_TOKENS;
    use rdy_engine::tensions::info::info_counts;
    use rdy_engine::tensions::{TensionSeverity, info::TensionInfo};
    use std::collections::BTreeSet;
    let (_d, e, s) = local();
    w(&e, "a", upd(&s.a, "A changed."));
    let before = e.read();
    let reply = w(&e, "b", fees(&s, 100));
    let after = e.read();
    let oc = &reply["on_change"];
    assert!(
        oc.to_string().len() <= BRIEF_DEFAULT_TOKENS * 4,
        "{} bytes",
        oc.to_string().len()
    );
    let kept = oc["tensions"].as_array().unwrap();
    let blocking: Vec<&Value> = kept
        .iter()
        .filter(|t| t["severity"] == "blocking")
        .collect();
    assert_eq!(blocking.len(), 1, "{oc}");
    assert_eq!(blocking[0]["kind"], "premise_shift");
    assert_eq!(oc["detail_cut"], true);
    let collapsed = oc["collapsed"]["duplicate_definition"].as_u64().unwrap();
    assert_eq!(collapsed as usize + kept.len() - 1, 4950, "{oc}");

    let d = full_delta(&before, &after);
    assert_eq!(d.opened.len(), 4951);
    let changes: BTreeSet<Hash> = d.opened.iter().flat_map(|t| change_sides(t)).collect();
    // warms the state's in-flight set, which the sides read
    on_change(&before, &after, "b", Some(10_000_000), &d).unwrap();

    let (f0, (s0, b0)) = (after.reader().frames_read(), info_counts());
    let o = on_change(&before, &after, "b", None, &d).unwrap();
    let (s1, b1) = info_counts();
    assert_eq!(serde_json::to_value(&o).unwrap(), *oc);
    assert_eq!(b1 - b0, 0, "rows the budget cannot hold bare get no sides");
    assert!(
        s1 - s0 <= 2 * (o.tensions.len() as u64 + 2),
        "{} serializations for {} rows kept",
        s1 - s0,
        o.tensions.len()
    );
    assert!(after.reader().frames_read() - f0 <= changes.len() as u64);

    // a budget that holds every row with its sides: each row built and measured once, each
    // change's provenance read once
    let (f0, (s0, b0)) = (after.reader().frames_read(), info_counts());
    let o = on_change(&before, &after, "b", Some(10_000_000), &d).unwrap();
    let (s1, b1) = info_counts();
    assert_eq!(o.tensions.len(), 4951);
    assert!(o.tensions.iter().all(|t: &TensionInfo| !t.sides.is_empty()));
    assert!(!o.detail_cut && o.collapsed.is_empty());
    assert!(
        o.tensions
            .iter()
            .any(|t| t.severity == TensionSeverity::Blocking)
    );
    assert_eq!(b1 - b0, 4951);
    assert!(s1 - s0 <= 2 * 4951, "{} serializations", s1 - s0);
    let frames = after.reader().frames_read() - f0;
    assert!(
        frames <= changes.len() as u64,
        "{frames} frames for {} changes",
        changes.len()
    );
}
