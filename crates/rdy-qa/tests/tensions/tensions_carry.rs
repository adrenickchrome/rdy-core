//! tcar1: the tension set carried across a commit equals a full rebuild; reindex reproduces it.
use proptest::prelude::*;
use rdy_engine::tensions::TensionKind;
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::{spec_doc, stable_spec};
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(12), ..ProptestConfig::default() })]
    #[test]
    fn tcar1_a_carried_tension_set_equals_a_rebuilt_one(
        steps in proptest::collection::vec((0u8..13, 0u8..3, 0u8..3), 1..12)
    ) {
        let (_d, mut r) = project();
        let s = spec_doc(&mut r);
        // one explicit section id that several views create (a create/create conflict)
        let shared = BlockId::new(s.section.prefix());
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
                // adversarial review of e613de0: a delete then a re-create of the same id, and
                // a section id another view may have created, with a claim under it
                11 => try_put(&mut r, vn, json!([{"op": "delete", "id": target.to_string()}])).is_ok()
                    && r.read().tensions().is_ok()
                    && try_put(&mut r, vn, json!([{"op": "create", "type": "claim", "id": target.to_string(),
                        "parent": s.section.to_string(), "body": texts[k as usize]}])).is_ok(),
                12 => try_put(&mut r, vn, json!([
                        {"op": "create", "type": "section", "id": shared.to_string(), "parent": s.doc.to_string(), "fields": {"title": format!("X {i}")}},
                        {"op": "create", "type": "claim", "parent": shared.to_string(), "body": texts[k as usize]}
                    ])).is_ok(),
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
    // plan M2e2: the write logs its moves, so it carries the set before it returns
    assert_eq!(
        r.read().tensions_carried(),
        Some(true),
        "the write carried the set"
    );
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

/// Tensions by kind and id.
type Kinded = Vec<(TensionKind, Hash)>;

/// The carried set and a rebuild of the current state, by id.
fn carried_and_rebuilt(r: &Rdy) -> (Kinded, Kinded) {
    let st = r.read();
    let carried = st
        .tensions()
        .unwrap()
        .iter()
        .map(|t| (t.kind, t.id))
        .collect();
    let rebuilt = st
        .tensions_rebuilt()
        .unwrap()
        .iter()
        .map(|t| (t.kind, t.id))
        .collect();
    (carried, rebuilt)
}

/// Two decisions in `notes/misc` (views a and b) that are in area payments only through their
/// `affects` target `target`.
fn decide_affecting(r: &mut Rdy, n: &BlockId, target: &BlockId) {
    for who in ["a", "b"] {
        r.decide(
            &ag(who),
            who,
            DecisionInput {
                parent: n.to_string(),
                statement: format!("{who} decides"),
                affects: vec![target.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    }
}

/// Adversarial review of e613de0 (F1): re-creating a deleted section gives the decisions that
/// `affects` it their area back, which no seed covers; it must not carry.
#[test]
fn tcar1_recreating_a_deleted_section_carries_the_same_set_as_a_rebuild() {
    let (_d, mut r) = qa_project();
    let (x, _) = landed_doc(&mut r, "payments/fees", false);
    let (n, _) = landed_doc(&mut r, "notes/misc", false);
    decide_affecting(&mut r, &n, &x);
    assert_eq!(of_kind(&r, TensionKind::AreaOverlap).len(), 1);
    let doc = r
        .read()
        .resolve_path(&Scope::Canon, "payments/fees")
        .unwrap();
    put(&mut r, "c", json!([{"op": "delete", "id": x.to_string()}]));
    r.read().tensions().unwrap();
    put(
        &mut r,
        "c",
        json!([{"op": "create", "type": "section", "id": x.to_string(), "parent": doc.to_string(), "fields": {"title": "S again"}}]),
    );
    let (carried, rebuilt) = carried_and_rebuilt(&r);
    assert_eq!(rebuilt.len(), 1, "{rebuilt:?}");
    assert_eq!(carried, rebuilt);
}

/// Adversarial review of e613de0 (F1, also at 7129477): the same for a re-created claim.
#[test]
fn tcar1_recreating_a_deleted_claim_carries_the_same_set_as_a_rebuild() {
    let (_d, mut r) = qa_project();
    let (fees, x) = landed_doc(&mut r, "payments/fees", true);
    let x = x.unwrap();
    let (n, _) = landed_doc(&mut r, "notes/misc", false);
    decide_affecting(&mut r, &n, &x);
    put(&mut r, "c", json!([{"op": "delete", "id": x.to_string()}]));
    r.read().tensions().unwrap();
    put(
        &mut r,
        "c",
        json!([{"op": "create", "type": "claim", "id": x.to_string(), "parent": fees.to_string(), "body": "Fees are monthly."}]),
    );
    let (carried, rebuilt) = carried_and_rebuilt(&r);
    assert_eq!(rebuilt.len(), 1, "{rebuilt:?}");
    assert_eq!(carried, rebuilt);
}

/// Adversarial review of e613de0 (F2): view b creates a section id that view a created too.
/// The in-flight merge drops it (create/create), so a's decision under it loses the path that
/// put it in area payments; the create must not carry.
#[test]
fn tcar1_a_section_id_another_view_created_carries_the_same_set_as_a_rebuild() {
    let (_d, mut r) = qa_project();
    let (k_sec, _) = landed_doc(&mut r, "payments/fees", false);
    let pay = r
        .read()
        .resolve_path(&Scope::Canon, "payments/fees")
        .unwrap();
    let (_, k2) = landed_doc(&mut r, "notes/misc", true);
    let k2 = k2.unwrap();
    let notes = r.read().resolve_path(&Scope::Canon, "notes/misc").unwrap();
    let xs = BlockId::new(k_sec.prefix()).to_string();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "section", "id": xs, "parent": pay.to_string(), "fields": {"title": "A's X"}}]),
    );
    for (who, parent) in [("a", xs.clone()), ("c", k_sec.to_string())] {
        r.decide(
            &ag(who),
            who,
            DecisionInput {
                parent,
                statement: format!("{who} decides"),
                affects: vec![k2.to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(of_kind(&r, TensionKind::AreaOverlap).len(), 1);
    put(
        &mut r,
        "b",
        json!([{"op": "create", "type": "section", "id": xs, "parent": notes.to_string(), "fields": {"title": "B's X"}}]),
    );
    let (carried, rebuilt) = carried_and_rebuilt(&r);
    assert_eq!(carried, rebuilt);
}

/// Fix round for e613de0 (found by the property below, also at 7129477): view a updates X
/// while view c deletes it; the in-flight merge keeps X, so the decisions that `affects` X get
/// their area back, which no seed covers. A write on a block an in-flight change deletes must
/// not carry.
#[test]
fn tcar1_updating_a_block_another_view_deletes_carries_the_same_set_as_a_rebuild() {
    let (_d, mut r) = qa_project();
    let (_, x) = landed_doc(&mut r, "payments/fees", true);
    let x = x.unwrap();
    let (n, _) = landed_doc(&mut r, "notes/misc", false);
    decide_affecting(&mut r, &n, &x);
    put(&mut r, "c", json!([{"op": "delete", "id": x.to_string()}]));
    r.read().tensions().unwrap();
    put(&mut r, "a", upd(&x, "Fees are weekly."));
    let (carried, rebuilt) = carried_and_rebuilt(&r);
    assert!(
        rebuilt.iter().any(|(k, _)| *k == TensionKind::AreaOverlap),
        "{rebuilt:?}"
    );
    assert_eq!(carried, rebuilt);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(12), ..ProptestConfig::default() })]
    /// Adversarial review of e613de0: deletes, re-creates of a deleted id, a section id that
    /// several views create, decisions that `affects` those ids, in the QA pack's areas; some
    /// states are left unread, so the next commit carries them when it is published.
    #[test]
    fn tcar1_a_carried_set_equals_a_rebuilt_one_across_recreates_and_shared_ids(
        steps in proptest::collection::vec((0u8..8, 0u8..3, 0u8..3), 1..12)
    ) {
        let (_d, mut r) = qa_project();
        let (sx, x) = landed_doc(&mut r, "payments/fees", true);
        let x = x.unwrap();
        let (n, _) = landed_doc(&mut r, "notes/misc", false);
        let pay = r.read().resolve_path(&Scope::Canon, "payments/fees").unwrap();
        let notes = r.read().resolve_path(&Scope::Canon, "notes/misc").unwrap();
        let shared = BlockId::new(sx.prefix());
        // decisions in a and b that are in area payments only through `affects` x
        decide_affecting(&mut r, &n, &x);
        let names = ["a", "b", "c"];
        let targets = [x.clone(), sx.clone(), shared.clone()];
        for (i, (kind, v, k)) in steps.into_iter().enumerate() {
            let vn = names[v as usize];
            let target = &targets[k as usize];
            let _ok = match kind {
                0 => r.decide(&ag(vn), vn, DecisionInput {
                        parent: if k == 2 { shared.to_string() } else { n.to_string() },
                        statement: format!("d{i}"),
                        affects: vec![target.to_string()],
                        ..Default::default()
                    }).is_ok(),
                1 => try_put(&mut r, vn, json!([{"op": "delete", "id": target.to_string()}])).is_ok(),
                // a delete, the set read, and a re-create of the deleted id
                2 => try_put(&mut r, vn, json!([{"op": "delete", "id": x.to_string()}])).is_ok()
                    && r.read().tensions().is_ok()
                    && try_put(&mut r, vn, json!([{"op": "create", "type": "claim", "id": x.to_string(),
                        "parent": sx.to_string(), "body": "Fees are monthly."}])).is_ok(),
                3 => try_put(&mut r, vn, json!([{"op": "create", "type": "section", "id": sx.to_string(),
                        "parent": pay.to_string(), "fields": {"title": format!("S {i}")}}])).is_ok(),
                // one explicit section id that several views create, under either doc, with a
                // decision under it that `affects` x
                4 => try_put(&mut r, vn, json!([
                        {"op": "create", "type": "section", "id": shared.to_string(),
                            "parent": if k == 0 { notes.to_string() } else { pay.to_string() }, "fields": {"title": format!("X {i}")}},
                        {"op": "create", "type": "claim", "parent": shared.to_string(), "body": format!("Under X {i}.")}
                    ])).is_ok()
                    && r.decide(&ag(vn), vn, DecisionInput {
                        parent: shared.to_string(),
                        statement: format!("x{i}"),
                        affects: vec![x.to_string()],
                        ..Default::default()
                    }).is_ok(),
                5 => try_put(&mut r, vn, upd(&x, &format!("{vn} {i}"))).is_ok(),
                6 => land(&mut r, vn).is_ok(),
                _ => r.new_doc(&ag(vn), vn, &format!("payments/n{i}"), None, None).is_ok(),
            };
            // every third state is left unread: the next commit carries it when published
            if i % 3 != 1 {
                let (carried, rebuilt) = carried_and_rebuilt(&r);
                prop_assert_eq!(carried, rebuilt, "step {}", i);
            }
        }
        let (carried, rebuilt) = carried_and_rebuilt(&r);
        prop_assert_eq!(carried, rebuilt, "end");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(12), ..ProptestConfig::default() })]
    /// guard (FW-JU1): across unjustified, implementing and editorial writes, decisions and
    /// edits of them, includes, deletes and landings on stable content, the carried set equals a
    /// rebuild, and each view's seeded run finds what the set lists for its changes.
    #[test]
    fn msw21_a_carried_set_equals_a_rebuilt_one_across_justified_and_unjustified_writes(
        steps in proptest::collection::vec((0u8..8, 0u8..3, 0u8..3), 1..12)
    ) {
        let (_d, mut r) = project();
        let s = stable_spec(&mut r);
        let names = ["a", "b", "c"];
        let claims = [s.a.clone(), s.b.clone(), s.c.clone()];
        let mut decisions: Vec<Option<BlockId>> = vec![None; 3];
        for (i, (kind, v, k)) in steps.into_iter().enumerate() {
            let vn = names[v as usize];
            let target = &claims[k as usize];
            let body = format!("{vn} {i}");
            let _ok = match kind {
                0 => try_put_j(&mut r, vn, upd(target, &body), None).is_ok(),
                1 => try_put_j(&mut r, vn, upd(target, &body), j_implements(&s.decision)).is_ok(),
                2 => try_put_j(&mut r, vn, upd(target, &body), j_typo()).is_ok(),
                3 => match try_decide_in(&mut r, vn, &s.section, &format!("rule {i}")) {
                    Ok(d) => {
                        decisions[v as usize] = Some(d);
                        true
                    }
                    Err(_) => false,
                },
                4 => match decisions[v as usize].clone() {
                    Some(d) => try_put_j(&mut r, vn, upd(&d, &body), None).is_ok(),
                    None => false,
                },
                5 => {
                    let other = names[(v as usize + 1) % 3];
                    let inc = r.read().included(other);
                    match inc.last() {
                        Some(c) => r.view_include(&ag(vn), vn, &[c.to_string()]).is_ok(),
                        None => false,
                    }
                }
                6 => land(&mut r, vn).is_ok(),
                _ => try_put_j(&mut r, vn, json!([{"op": "delete", "id": target.to_string()}]), None).is_ok(),
            };
            let st = r.read();
            let set = st.tensions().unwrap();
            let carried: Vec<_> = set.iter().map(|t| t.id).collect();
            let rebuilt: Vec<_> = st.tensions_rebuilt().unwrap().iter().map(|t| t.id).collect();
            prop_assert_eq!(carried, rebuilt, "step {}", i);
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
