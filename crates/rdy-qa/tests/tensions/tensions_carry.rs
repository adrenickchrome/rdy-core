//! tcar1: the tension set carried across a commit equals a full rebuild; reindex reproduces it.
use proptest::prelude::*;
use rdy_engine::DecisionInput;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;
use serde_json::json;

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(12), ..ProptestConfig::default() })]
    #[test]
    fn tcar1_a_carried_tension_set_equals_a_rebuilt_one(
        steps in proptest::collection::vec((0u8..7, 0u8..3, 0u8..3), 1..12)
    ) {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        let names = ["a", "b", "c"];
        let claims = [s.a.clone(), s.b.clone(), s.c.clone()];
        for (i, (kind, v, k)) in steps.into_iter().enumerate() {
            let vn = names[v as usize];
            let target = &claims[k as usize];
            let _ok = match kind {
                0 => try_put(&mut r, vn, upd(target, &format!("{vn} {i}"))).is_ok(),
                1 => try_put(&mut r, vn, cite(&s.section, target)).is_ok(),
                2 => try_put(&mut r, vn, term(&s.section, ["Fee", "Charge", "fee"][k as usize])).is_ok(),
                3 => r.supersede(&ag(vn), vn, &s.decision.to_string(), None, DecisionInput {
                        parent: s.section.to_string(), statement: format!("s{i}"), ..Default::default()
                    }).is_ok(),
                4 => {
                    let other = names[(v as usize + 1) % 3];
                    let inc = r.read().included(other);
                    match inc.last() {
                        Some(c) => r.view_include(&ag(vn), vn, &[c.to_string()]).is_ok(),
                        None => false,
                    }
                }
                5 => try_put(&mut r, vn, json!([{"op": "repin", "id": target.to_string()}])).is_ok(),
                _ => land(&mut r, vn).is_ok(),
            };
            let st = r.read();
            let carried: Vec<_> = st.tensions().unwrap().iter().map(|t| t.id).collect();
            let rebuilt: Vec<_> = st.tensions_rebuilt().unwrap().iter().map(|t| t.id).collect();
            prop_assert_eq!(carried, rebuilt, "step {}", i);
        }
    }
}

#[test]
fn tcar1_a_commit_carries_the_set_and_a_structural_write_rebuilds_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A one."));
    r.read().tensions().unwrap();
    put(&mut r, "b", upd(&s.a, "A two."));
    assert_eq!(r.read().tensions_carried(), None, "unbuilt until read");
    assert_eq!(
        of_kind(&r, rdy_engine::tensions::TensionKind::EditCollision).len(),
        1
    );
    assert_eq!(r.read().tensions_carried(), Some(true));
    put(
        &mut r,
        "b",
        json!([{"op": "update", "id": s.section.to_string(), "fields": {"title": "Rules 2"}}]),
    );
    r.read().tensions().unwrap();
    assert_eq!(
        r.read().tensions_carried(),
        Some(false),
        "a structural write rebuilds"
    );
}

#[test]
fn tcar1_reindex_reproduces_the_tension_ids() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A by a."));
    put(&mut r, "b", upd(&s.a, "A by b."));
    put(&mut r, "c", cite(&s.section, &s.a));
    let before: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert!(before.len() >= 2);
    r.reindex().unwrap();
    let after: Vec<_> = open(&r).iter().map(|t| t.id).collect();
    assert_eq!(before, after);
}
