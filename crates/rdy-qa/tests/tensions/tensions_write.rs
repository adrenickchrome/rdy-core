//! tonc1: write responses carry on_change tensions and tension consequences.
use rdy_engine::consequence::{consequence_id, write_and_report};
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
