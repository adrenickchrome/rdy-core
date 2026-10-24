//! vinv1: spec §15.3 invariants 1-4 for views, in the checker.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::invariants::{check, check_views};
use rdy_store::view::ViewOpKind;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}
fn rules(r: &Rdy) -> Vec<&'static str> {
    match check_views(r) {
        Ok(()) => vec![],
        Err(v) => v.iter().map(|x| x.rule).collect(),
    }
}

#[test]
fn vinv1_the_checker_catches_a_view_left_behind() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let l1: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let a = as_agent("agent:a", "r1");
    let ca: rdy_model::Hash = write_as(&mut r, &a, "a", claim(&section, "a"))
        .unwrap()
        .change
        .parse()
        .unwrap();
    write(&mut r, "seed", claim(&section, "two")).unwrap();
    land(&mut r, "seed").unwrap();
    assert!(rules(&r).is_empty(), "{:?}", check_views(&r));
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Rebase {
                landing: l1,
                dropped: vec![],
                remaining: vec![ca],
                conflicts: vec![],
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    assert!(rules(&r).contains(&"rebased"), "{:?}", check_views(&r));
    assert!(check(&r, &Scope::Canon).is_err());
}

#[test]
fn vinv1_the_checker_catches_an_open_set() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "x")).unwrap();
    let x = c1.created.values().next().unwrap().clone();
    let v1 = version_of(&r, &Scope::View("a".into()), &x);
    let c2: rdy_model::Hash = write_as(&mut r, &a, "a", update(&x, &v1, "x2"))
        .unwrap()
        .change
        .parse()
        .unwrap();
    let b = as_agent("agent:b", "r1");
    write_as(&mut r, &b, "b", claim(&section, "b")).unwrap();
    r.store_mut()
        .view_append(
            "b",
            vec![ViewOpKind::Include { changes: vec![c2] }],
            &b.provenance(),
        )
        .unwrap();
    r.invalidate();
    assert!(rules(&r).contains(&"closed"), "{:?}", check_views(&r));
}

#[test]
fn vinv1_a_clean_swarm_passes() {
    let (_d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let agents: Vec<Session> = (0..4)
        .map(|i| as_agent(&format!("agent:s{i}"), "r"))
        .collect();
    for round in 0..6 {
        for (i, s) in agents.iter().enumerate() {
            if round > 3 && i == 0 {
                continue;
            } // s0 is closed in round 3
            let v = format!("s{i}");
            write_as(
                &mut r,
                s,
                &v,
                claim(&section, &format!("{v} round {round}")),
            )
            .unwrap();
            if round == 2 && i == 1 {
                let base = version_of(&r, &Scope::View(v.clone()), &c);
                write_as(&mut r, s, &v, update(&c, &base, "s1's edit")).unwrap();
            }
        }
        let from = r.read().included("s0").last().unwrap().to_string();
        r.view_include(&agents[3], "s3", &[from]).unwrap();
        if round == 3 {
            r.undo(&agents[2], "s2", None, false).unwrap();
            r.view_close(&agents[0], &["s0".into()], "done").unwrap();
        }
        let lander = format!("s{}", 1 + round % 3);
        let p = r
            .propose(&agents[1 + round % 3], &lander, "t", Some(TEST_SUMMARY))
            .unwrap();
        let _ = r.land_report(&approver(), &p.r#ref);
        assert!(rules(&r).is_empty(), "round {round}: {:?}", check_views(&r));
    }
    check(&r, &Scope::Canon).unwrap();
    for v in r.read().view_names() {
        check(&r, &Scope::View(v)).unwrap();
    }
}

#[test]
fn vinv1_a_view_that_cannot_be_read_is_a_violation_not_a_pass() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", claim(&section, "a")).unwrap();
    assert!(rules(&r).is_empty(), "{:?}", check_views(&r));
    // an include of a change the store does not hold: the view's merge cannot be built
    let ghost: rdy_model::Hash = "ab".repeat(32).parse().unwrap();
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include {
                changes: vec![ghost],
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    let found = check_views(&r).expect_err("an unreadable view must be reported");
    assert!(
        found
            .iter()
            .any(|x| x.rule == "conflict_sides" && x.detail.contains("\"a\"")),
        "rule 3 must report the failed read, got {found:?}"
    );
}
