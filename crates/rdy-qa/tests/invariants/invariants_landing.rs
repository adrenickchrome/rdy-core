//! sg1: the landing audit re-checks what the gate refuses, from public data.
use rdy_engine::alt::AltMemberInput;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::landing_audit::{Landed, audit, landed_of};
use rdy_qa::tensions::{ag, put};
use serde_json::json;

fn rules(v: &[rdy_qa::invariants::Violation]) -> Vec<&'static str> {
    v.iter().map(|x| x.rule).collect()
}

fn m(label: &str, view: &str) -> AltMemberInput {
    AltMemberInput {
        label: label.into(),
        view: view.into(),
    }
}

#[test]
fn sg1_a_clean_landing_passes_the_audit() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let w = put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "One claim."}]),
    );
    let before = r.read();
    let landed = Landed {
        view: "a".into(),
        changes: vec![w.change.parse().unwrap()],
        alternative: None,
    };
    assert!(audit(&before, &landed).is_empty());
    let l = land(&mut r, "a").unwrap();
    assert_eq!(
        landed_of(&r, &l.parse().unwrap()).unwrap().changes,
        landed.changes
    );
}

#[test]
fn sg1_the_audit_catches_a_landing_the_gate_would_refuse() {
    // two views edit one block from one base: an edit collision, blocking by default
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let base = version_of(&r, &Scope::Canon, &c);
    // both views write before either lands, from the same base
    put(
        &mut r,
        "a",
        json!([{"op": "update", "id": c, "base": base, "body": "A says ten."}]),
    );
    let b = put(
        &mut r,
        "b",
        json!([{"op": "update", "id": c, "base": base, "body": "B says five."}]),
    );
    land(&mut r, "a").unwrap();
    // a's landing rebased b onto a canon that changed the same block: the gate refuses b now;
    // the audit, given b's changes against this held state, agrees
    let before = r.read();
    let landed = Landed {
        view: "b".into(),
        changes: vec![b.change.parse().unwrap()],
        alternative: None,
    };
    assert!(
        land(&mut r, "b").is_err(),
        "precondition: the gate refuses b"
    );
    let v = audit(&before, &landed);
    assert!(!v.is_empty(), "the audit must flag what the gate refuses");
    assert!(
        rules(&v)
            .iter()
            .all(|r| *r == "blocking_landed" || *r == "conflict_landed"),
        "{v:?}"
    );
}

#[test]
fn sg1_an_unchosen_alternative_is_flagged() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let x = put(
        &mut r,
        "x",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "X."}]),
    );
    put(
        &mut r,
        "y",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "Y."}]),
    );
    let set = r
        .alt_create(&ag("x"), "Which", None, &[m("x", "x"), m("y", "y")])
        .unwrap();
    // written into a member view after the set exists: the change links to the set
    let w = put(
        &mut r,
        "x",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "X two."}]),
    );
    let before = r.read();
    let landed = Landed {
        view: "x".into(),
        changes: vec![x.change.parse().unwrap(), w.change.parse().unwrap()],
        alternative: None,
    };
    assert!(rules(&audit(&before, &landed)).contains(&"alt_unchosen"));
    let ok = Landed {
        alternative: Some(set.set.parse().unwrap()),
        ..landed
    };
    assert!(!rules(&audit(&before, &ok)).contains(&"alt_unchosen"));
}

#[test]
fn sg1_check_all_covers_canon_and_every_open_view() {
    // guard: check_all is check on canon plus check on each open view plus the logged rule
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "A."}]),
    );
    rdy_qa::invariants::check_all(&r).unwrap();
}
