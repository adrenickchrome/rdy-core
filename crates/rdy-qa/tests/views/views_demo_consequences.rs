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

const TRK_PACK: &str = "\
name: trk
version: \"1\"
extends: rdy-standard
block_types:
  decision:
    prefix: D
    traits: [normative]
    fields:
      disposition: { kind: str }
    tracking: [disposition]
";

/// df4 fix round 1 (ruling 1, D-W7KAK): a tracking edit on a live decision D1 whose premise D0
/// canon has superseded keeps the suspect. D1 is frozen, so its kept entry says `reaffirm`, as
/// the supersede's own reply said, and following that fix (supersede D1) succeeds.
#[test]
fn df4_a_tracking_edit_on_a_live_decision_keeps_its_suspect_as_reaffirm() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), TRK_PACK).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let decide = |r: &mut Rdy, statement: &str, depends_on: Vec<String>| {
        let d = r
            .decide(
                &writer(),
                "seed",
                DecisionInput {
                    parent: section.clone(),
                    statement: statement.into(),
                    depends_on,
                    ..Default::default()
                },
            )
            .unwrap();
        d.created["$decision"].clone()
    };
    let d0 = decide(&mut r, "Ship weekly", vec![]);
    let d1 = decide(&mut r, "Release on Fridays", vec![d0.clone()]);
    land(&mut r, "seed").unwrap();
    r.supersede(
        &writer(),
        "s",
        &d0,
        None,
        DecisionInput {
            parent: section.clone(),
            statement: "Ship daily".into(),
            ..Default::default()
        },
    )
    .unwrap();
    land(&mut r, "s").unwrap();
    let e = Local::new(r);
    let v = write_and_report(&e, "reg", None, {
        let d1 = d1.clone();
        move |r| {
            r.write(
                &writer(),
                WriteRequest {
                    view: "reg".into(),
                    ops: serde_json::from_value(
                        json!([{"op": "update", "id": d1, "fields": {"disposition": "M3"}}]),
                    )
                    .unwrap(),
                    justification: Some(rdy_model::Justification::Editorial {
                        class: rdy_model::EditorialClass::Clarify,
                        reason: "re-sliced into M3".into(),
                    }),
                    message: String::new(),
                },
            )
        }
    })
    .unwrap();
    let k = kept(&v, &d1).unwrap_or_else(|| panic!("the write names no kept suspect: {v:#}"));
    assert_eq!(k["because"], json!({"edge": "depends_on", "changed": d0}));
    assert_eq!(k["follow_up"], "reaffirm", "{v:#}");
    assert_eq!(k["fix"], rdy_engine::consequence::REAFFIRM_FIX);
    // the fix's first branch: supersede it
    let s = write_and_report(&e, "reg", None, {
        let d1 = d1.clone();
        move |r| {
            r.supersede(
                &writer(),
                "reg",
                &d1,
                None,
                DecisionInput {
                    parent: section,
                    statement: "Release daily".into(),
                    depends_on: vec![],
                    ..Default::default()
                },
            )
        }
    });
    assert!(s.is_ok(), "following the fix fails: {s:?}");
}

/// df4 fix round 1 (ruling 2, D-SZ66F): a suspect a write leaves in place gets a kept entry
/// whatever link made it suspect. A new decision that refines a superseded one is suspect by
/// `refines`: the write, propose and approval replies each list it.
#[test]
fn df4_a_new_decision_refining_a_superseded_one_is_listed_in_every_reply() {
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
        move |r| {
            r.decide(
                &writer(),
                "w",
                DecisionInput {
                    parent: section,
                    statement: "Keep A short".into(),
                    refines: vec![old],
                    ..Default::default()
                },
            )
        }
    })
    .unwrap();
    let n = v["created"]["$decision"].as_str().unwrap().to_string();
    let k = kept(&v, &n).unwrap_or_else(|| panic!("the write names no kept suspect: {v:#}"));
    assert_eq!(k["because"], json!({"edge": "refines", "changed": old}));
    assert_eq!(k["follow_up"], "update");
    assert_eq!(k["fix"], rdy_engine::consequence::KEPT_REPLACED_FIX);
    let p = propose_and_report(&e, None, |r| r.propose(&writer(), "w", "Keep A short")).unwrap();
    let k = kept(&p, &n).unwrap_or_else(|| panic!("the propose names no kept suspect: {p:#}"));
    assert_eq!(k["because"], json!({"edge": "refines", "changed": old}));
    assert!(p["blocking"].as_array().unwrap().is_empty(), "{p:#}");
    let pref = p["ref"].as_str().unwrap().to_string();
    let l = land_and_report(&e, move |r| {
        r.land_with(&approver(), &pref, LandOptions::default())
    })
    .unwrap();
    let k = kept(&l, &n).unwrap_or_else(|| panic!("the approval names no kept suspect: {l:#}"));
    assert_eq!(k["because"], json!({"edge": "refines", "changed": old}));
}

/// df4 fix round 1 (ruling 3, p-RVR9H): a rewritten block that stays suspect on two targets gets
/// one kept entry per target, each with its own `cq-` id (`suspect {target}`), stable when the
/// consequences are read again.
#[test]
fn df4_kept_entries_of_one_block_have_an_id_per_target() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let (a, c) = (s.a.to_string(), s.c.to_string());
    let res = write(&mut r, "x", json!([
        {"op": "create", "type": "claim", "id": "$x", "parent": s.section.to_string(), "body": format!("X rests on {{{{{a}}}}} and {{{{{c}}}}}.")}
    ]))
    .unwrap();
    let x = rdy_model::BlockId::parse(&res.created["$x"]).unwrap();
    land(&mut r, "x").unwrap();
    let res = write(
        &mut r,
        "w",
        json!([
            {"op": "update", "id": a, "body": "A holds, and more."},
            {"op": "update", "id": c, "body": "C stands alone, and more."},
            {"op": "update", "id": x.to_string(), "body": format!("X rests on {{{{{a}}}}} and {{{{{c}}}}} again.")}
        ]),
    )
    .unwrap();
    let change: rdy_model::Hash = res.change.parse().unwrap();
    let read = |r: &Rdy| {
        r.read()
            .consequences(&Scope::View("w".into()), &change, None)
            .unwrap()
    };
    let first = read(&r);
    let kept: Vec<_> = first
        .items
        .iter()
        .filter(|e| e.block == x.to_string() && e.already_written)
        .collect();
    let changed: Vec<&str> = kept.iter().map(|e| e.because.changed.as_str()).collect();
    let mut want = vec![a.as_str(), c.as_str()];
    want.sort();
    assert_eq!(changed, want, "{first:?}");
    for e in &kept {
        assert_eq!(
            e.id,
            rdy_engine::consequence::consequence_id(
                &change,
                &format!("suspect {}", e.because.changed),
                &x
            )
        );
    }
    assert_ne!(kept[0].id, kept[1].id, "{first:?}");
    assert_eq!(read(&r), first, "recomputed, identical");
}
