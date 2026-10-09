//! tcar1: the tension set carried across a commit equals a full rebuild; reindex reproduces it.
use proptest::prelude::*;
use rdy_engine::tensions::TensionKind;
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::edit_loop::spec_doc;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(12), ..ProptestConfig::default() })]
    #[test]
    fn tcar1_a_carried_tension_set_equals_a_rebuilt_one(
        steps in proptest::collection::vec((0u8..11, 0u8..3, 0u8..3), 1..12)
    ) {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        let names = ["a", "b", "c"];
        let claims = [s.a.clone(), s.b.clone(), s.c.clone()];
        // FW-TN2: texts that form duplicate groups (two normalize to one) and one that does not
        let texts = ["Fees are charged monthly.", "fees ARE charged  monthly.", "Fees are charged yearly."];
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
                6 => land(&mut r, vn).is_ok(),
                7 => try_put(&mut r, vn, json!([{"op": "create", "type": "claim",
                        "parent": s.section.to_string(), "body": texts[k as usize]}])).is_ok(),
                // perf fix (pre-merge): a created structural block carries
                9 => {
                    let name = ["Fee", "Charge", "fee"][k as usize];
                    try_put(&mut r, vn, json!([
                        {"op": "create", "type": "section", "id": "$n", "parent": s.doc.to_string(), "fields": {"title": format!("New {i}")}},
                        {"op": "create", "type": "claim", "parent": "$n", "body": texts[k as usize]},
                        {"op": "create", "type": "term", "parent": "$n", "fields": {"name": name}, "body": "A term."}
                    ])).is_ok()
                }
                10 => r.new_doc(&ag(vn), vn, &format!("n{i}"), None, None).is_ok(),
                _ => try_put(&mut r, vn, upd(target, texts[k as usize])).is_ok(),
            };
            let st = r.read();
            let set = st.tensions().unwrap();
            let carried: Vec<_> = set.iter().map(|t| t.id).collect();
            let rebuilt: Vec<_> = st.tensions_rebuilt().unwrap().iter().map(|t| t.id).collect();
            prop_assert_eq!(carried, rebuilt, "step {}", i);
            // perf fix (pre-merge): what a proposal of each view lists from the set, the seeded
            // run finds without the set
            for vn in names {
                let inc = st.included(vn);
                let mut from_set: Vec<_> = inc.iter().flat_map(|c| set.involving_change(c)).map(|t| t.id).collect();
                from_set.sort();
                from_set.dedup();
                let seeded: Vec<_> = st.tensions_of_changes_seeded(&inc).unwrap().iter().map(|t| t.id).collect();
                prop_assert_eq!(seeded, from_set, "step {} view {}", i, vn);
            }
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

/// A landed doc at `path` with one section, and (when `claim`) one claim under it: the section
/// and the claim.
fn landed_doc(r: &mut Rdy, path: &str, claim: bool) -> (BlockId, Option<BlockId>) {
    r.new_doc(&writer(), "seed", path, Some("glossary"), Some("Doc"))
        .unwrap();
    let doc = r
        .read()
        .resolve_path(&Scope::View("seed".into()), path)
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

#[test]
fn tcar1_deleting_an_affects_target_carries_the_same_set_as_a_rebuild() {
    // two decisions outside any area are in area payments only through their `affects` target
    // X; deleting X takes the area away, so their overlap goes (the overlap involves only them)
    let (_d, mut r) = qa_project();
    let (_fees, x) = landed_doc(&mut r, "payments/fees", true);
    let x = x.unwrap();
    let (n, _) = landed_doc(&mut r, "notes/misc", false);
    for who in ["a", "b"] {
        r.decide(
            &ag(who),
            who,
            DecisionInput {
                parent: n.to_string(),
                statement: format!("{who} decides"),
                affects: vec![x.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(of_kind(&r, TensionKind::AreaOverlap).len(), 1);
    put(&mut r, "c", json!([{"op": "delete", "id": x.to_string()}]));
    let st = r.read();
    let carried: Vec<_> = st.tensions().unwrap().iter().map(|t| t.id).collect();
    let rebuilt: Vec<_> = st
        .tensions_rebuilt()
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert!(rebuilt.is_empty(), "{rebuilt:?}");
    assert_eq!(carried, rebuilt);
    assert_eq!(st.tensions_carried(), Some(false), "a delete rebuilds");
}

/// Perf fix (pre-merge, daemon_write_single): a new doc or section carries the set, and a
/// commit nobody read the set of is carried when the next one is published, so the read after
/// a run of commits never builds the whole set.
#[test]
fn tcar1_a_created_doc_or_section_carries_and_an_unread_commit_still_carries() {
    use rdy_engine::tensions::set::full_builds;
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.a, "A one."));
    r.read().tensions().unwrap();
    let before = full_builds();
    r.new_doc(&ag("b"), "b", "notes", None, None).unwrap();
    r.read().tensions().unwrap();
    assert_eq!(r.read().tensions_carried(), Some(true), "a new doc carries");
    put(
        &mut r,
        "b",
        json!([{"op": "create", "type": "section", "parent": s.doc.to_string(), "fields": {"title": "More"}}]),
    );
    // nobody reads this state's set: the next commit carries it when it is published
    put(&mut r, "c", upd(&s.a, "A by c."));
    assert_eq!(of_kind(&r, TensionKind::EditCollision).len(), 1);
    assert_eq!(r.read().tensions_carried(), Some(true));
    assert_eq!(full_builds(), before, "no whole set was built");
    let st = r.read();
    let carried: Vec<_> = st.tensions().unwrap().iter().map(|t| t.id).collect();
    let rebuilt: Vec<_> = st
        .tensions_rebuilt()
        .unwrap()
        .iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(carried, rebuilt);
}
