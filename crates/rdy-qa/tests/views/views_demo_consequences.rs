//! df4: the demo findings on consequences: a suspect a write keeps (F7, D-W7KAK), and a
//! propose with a blocking conflict (F6).
use rdy_engine::consequence::{
    KEPT_CHANGED_FIX, land_and_report, propose_and_report, write_and_report,
};
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn kept<'a>(v: &'a Value, block: &str) -> Option<&'a Value> {
    v["consequences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["block"] == block && c["already_written"] == true && c["kind"] == "suspect")
}

/// The repro of record.py steps 21-23: one write changes a and b (b cites a), a second write
/// rewrites b; b stays suspect, and every reply says so until a repin.
#[test]
fn df4_rewriting_a_suspect_block_names_the_suspect() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r); // spec_doc lands the spec itself
    let (a, b) = (s.a.to_string(), s.b.to_string());
    let e = Local::new(r);
    let w1 = write_and_report(&e, "w", None, {
        let (a, b) = (a.clone(), b.clone());
        move |r| {
            write(
                r,
                "w",
                json!([
                    {"op": "update", "id": a, "body": "A holds, and more."},
                    {"op": "update", "id": b, "body": format!("B builds on {{{{{a}}}}} again.")}
                ]),
            )
        }
    })
    .unwrap();
    let k = kept(&w1, &b).unwrap_or_else(|| panic!("write 1 names no kept suspect: {w1:#}"));
    assert_eq!(k["because"], json!({"edge": "references", "changed": a}));
    assert_eq!(k["follow_up"], "confirm");
    assert_eq!(k["fix"], KEPT_CHANGED_FIX);
    let w2 = write_and_report(&e, "w", None, {
        let (a, b) = (a.clone(), b.clone());
        move |r| {
            write(
                r,
                "w",
                json!([{"op": "update", "id": b, "body": format!("B rests on {{{{{a}}}}}.")}]),
            )
        }
    })
    .unwrap();
    assert!(
        kept(&w2, &b).is_some(),
        "write 2 names no kept suspect: {w2:#}"
    );
    let p = propose_and_report(&e, None, |r| r.propose(&writer(), "w", "A and B")).unwrap();
    assert!(
        kept(&p, &b).is_some(),
        "propose names no kept suspect: {p:#}"
    );
    assert!(
        p["blocking"].as_array().unwrap().is_empty(),
        "a kept suspect never refuses: {p:#}"
    );
    // D-SZ66F: the approval lands it and names the kept suspect again
    let pref = p["ref"].as_str().unwrap().to_string();
    let l = land_and_report(&e, move |r| {
        r.land_with(&approver(), &pref, LandOptions::default())
    })
    .unwrap();
    assert!(
        kept(&l, &b).is_some(),
        "the approval names no kept suspect: {l:#}"
    );
    let w3 = write_and_report(&e, "w", None, {
        let b = b.clone();
        move |r| write(r, "w", json!([{"op": "repin", "id": b}]))
    })
    .unwrap();
    assert!(kept(&w3, &b).is_none(), "{w3:#}");
    let p2 = propose_and_report(&e, None, |r| r.propose(&writer(), "w", "A and B")).unwrap();
    assert!(kept(&p2, &b).is_none(), "{p2:#}");
}

/// The repro case `conflicted-propose-consequences-error`.
#[test]
fn df4_conflicted_propose_consequences_error() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r); // spec_doc lands the spec itself
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A from w."}]),
    )
    .unwrap();
    write(
        &mut r,
        "o",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A from o."}]),
    )
    .unwrap();
    land(&mut r, "o").unwrap();
    let e = Local::new(r);
    let v = propose_and_report(&e, None, |r| r.propose(&writer(), "w", "Change A")).unwrap();
    assert_eq!(v["consequences"], json!([]));
    assert_eq!(
        v["consequences_error"],
        format!(
            "E_CONFLICT_UNRESOLVED: no consequences while {} is in conflict. Write it from the version you want (rdy_context lists the sides), then propose again.",
            s.a
        )
    );
}

/// D-W7KAK covers a block the write creates too: a new claim that cites a decision canon has
/// superseded is suspect when the write makes it, and the reply names it with the replaced-target fix. The
/// write creates only, so this also pins that such a write still reads the graph.
#[test]
fn df4_a_created_block_citing_a_superseded_decision_names_the_suspect() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    r.supersede(
        &writer(),
        "s",
        &s.decision.to_string(),
        None,
        DecisionInput {
            parent: s.section.to_string(),
            statement: "Keep A, and more".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "s").unwrap();
    let (old, section) = (s.decision.to_string(), s.section.to_string());
    let e = Local::new(r);
    let v = write_and_report(&e, "w", None, {
        let old = old.clone();
        move |r| write(r, "w", json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": section, "body": format!("N rests on {{{{{old}}}}}.")}
        ]))
    }).unwrap();
    let n = v["created"]["$n"].as_str().unwrap().to_string();
    let k = kept(&v, &n).unwrap_or_else(|| panic!("the write names no kept suspect: {v:#}"));
    assert_eq!(k["because"], json!({"edge": "references", "changed": old}));
    assert_eq!(k["follow_up"], "update");
    assert_eq!(k["fix"], rdy_engine::consequence::KEPT_REPLACED_FIX);
}
