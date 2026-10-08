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
