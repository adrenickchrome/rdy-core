//! reb1 at the engine: landings rebase every live unpinned non-empty view; conflicts with canon
//! are recorded, re-keyed as canon moves, and announced as events.
use rdy_engine::events::{Event, EventKind};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_store::view::ViewOpKind;
use serde_json::json;
use std::sync::Arc;

fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}
fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn view(v: &str) -> Scope {
    Scope::View(v.into())
}
fn drain(rx: &std::sync::mpsc::Receiver<Arc<Event>>) -> Vec<Event> {
    let mut out = vec![];
    while let Ok(e) = rx.try_recv() {
        out.push((*e).clone());
    }
    out
}
fn rebased_in(events: &[Event]) -> Vec<(String, usize)> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ViewRebased {
                view, conflicts, ..
            } => Some((view.clone(), conflicts.len())),
            _ => None,
        })
        .collect()
}

fn two_views() -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    write_as(
        &mut r,
        &as_agent("agent:a", "r1"),
        "a",
        update(&c, &v, "a's"),
    )
    .unwrap();
    write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap();
    (d, r, c, v)
}

fn land_first_then_other_gets_a_canon_conflict(first: &str, second: &str) {
    let (_d, mut r, c, v) = two_views();
    let rx = r.subscribe();
    let l: rdy_model::Hash = land(&mut r, first).unwrap().parse().unwrap();
    let st = r.read();
    assert_eq!(st.view_head(first).unwrap().base, Some(l));
    assert_eq!(
        st.view_head(second).unwrap().base,
        Some(l),
        "{second} was rebased onto the landing"
    );
    assert_eq!(st.included(first).len(), 0);
    assert_eq!(st.included(second).len(), 1);
    let conflicts = st.conflicts(&view(second)).unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    let id = rdy_model::BlockId::parse(&c).unwrap();
    let info = st.conflict_info(&view(second), &id).unwrap().unwrap();
    assert_eq!(
        info.base.as_deref(),
        Some(v.as_str()),
        "the base is the version {second} edited"
    );
    let landed: Vec<&ConflictSide> = info.sides.iter().filter(|s| s.landed.is_some()).collect();
    assert_eq!(landed.len(), 1, "one side is canon's: {:?}", info.sides);
    assert_eq!(landed[0].landed.as_deref(), Some(l.to_string().as_str()));
    assert_eq!(
        body_of(&r, &view(second), &c),
        "one",
        "reads as the base, neither side wins"
    );
    assert_eq!(body_of(&r, &Scope::Canon, &c), format!("{first}'s"));
    let events = drain(&rx);
    let mut got = rebased_in(&events);
    got.sort();
    assert_eq!(
        got,
        vec![
            ("a".to_string(), usize::from(first != "a")),
            ("b".to_string(), usize::from(first != "b"))
        ],
        "events come in view ref order"
    );
    assert_eq!(
        r.replay_events(rdy_store::segment::Location {
            segment: 0,
            offset: 0
        })
        .unwrap()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ViewRebased { .. }))
        .count(),
        3,
        "seed's own rebase plus these two"
    );
    check(&r, &Scope::Canon).unwrap();
    check(&r, &view(second)).unwrap();
}

#[test]
fn reb1_two_views_edit_one_block_land_a_then_b_has_a_canon_conflict() {
    land_first_then_other_gets_a_canon_conflict("a", "b");
}

#[test]
fn reb1_two_views_edit_one_block_land_b_then_a_has_a_canon_conflict() {
    land_first_then_other_gets_a_canon_conflict("b", "a");
}

/// Review Focus 4.
#[test]
fn reb1_a_conflict_on_an_untouched_block_survives_a_rebase_unchanged() {
    let (_d, mut r, c, _v) = two_views();
    let a = as_agent("agent:a", "r1");
    let cb = r.read().included("b")[0];
    // through the store: include is Task 6's, built beside this task
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include { changes: vec![cb] }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    let before = r.read().conflicts(&view("a")).unwrap();
    assert_eq!(before.len(), 1);
    let section = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .find(|b| b.block_type == "section")
        .unwrap()
        .id
        .to_string();
    write_as(
        &mut r,
        &as_agent("agent:c", "r1"),
        "c",
        claim(&section, "elsewhere"),
    )
    .unwrap();
    let rx = r.subscribe();
    let l: rdy_model::Hash = land(&mut r, "c").unwrap().parse().unwrap();
    let after = r.read().conflicts(&view("a")).unwrap();
    assert_eq!(*after, *before, "same id, same sides, same base");
    assert_eq!(r.read().view_head("a").unwrap().base, Some(l));
    let events = drain(&rx);
    assert!(
        rebased_in(&events).contains(&("a".to_string(), 0)),
        "no new conflict recorded for a: {events:?}"
    );
    let _ = c;
}

#[test]
fn reb1_a_cascade_across_five_views_re_keys_the_conflict_at_each_landing() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let views = ["v1", "v2", "v3", "v4", "v5"];
    for name in views {
        write_as(
            &mut r,
            &as_agent(&format!("agent:{name}"), "r"),
            name,
            update(&c, &v, name),
        )
        .unwrap();
    }
    let id = rdy_model::BlockId::parse(&c).unwrap();
    let mut previous_ids: std::collections::BTreeMap<&str, rdy_model::Hash> = Default::default();
    for (i, lander) in views.iter().enumerate() {
        if i > 0 {
            // resolve from the conflict base, which is the version this view edited
            let conflict = r
                .read()
                .conflict_on(&view(lander), &id)
                .unwrap()
                .expect("conflicted before resolving");
            let base = conflict.base.unwrap().to_string();
            write_as(
                &mut r,
                &as_agent(&format!("agent:{lander}"), "r"),
                lander,
                update(&c, &base, &format!("{lander} merged")),
            )
            .unwrap();
            assert!(r.read().conflicts(&view(lander)).unwrap().is_empty());
        }
        let rx = r.subscribe();
        land(&mut r, lander).unwrap();
        let rebased = rebased_in(&drain(&rx));
        let expected: Vec<(String, usize)> = views[i..]
            .iter()
            .map(|v| (v.to_string(), if *v == *lander { 0 } else { 1 }))
            .collect();
        assert_eq!(rebased, expected, "landing {lander}");
        for later in &views[i + 1..] {
            let conflict = r
                .read()
                .conflict_on(&view(later), &id)
                .unwrap()
                .expect(later);
            if let Some(old) = previous_ids.get(later) {
                assert_ne!(
                    conflict.id, *old,
                    "{later}: canon's side moved, so the conflict is new"
                );
            }
            previous_ids.insert(later, conflict.id);
            check(&r, &view(later)).unwrap();
        }
        check(&r, &Scope::Canon).unwrap();
    }
    assert_eq!(body_of(&r, &Scope::Canon, &c), "v5 merged");
    r.fsck().unwrap();
}

#[test]
fn reb1_pinned_closed_and_empty_views_are_not_rebased_and_the_proposing_views_later_write_survives()
{
    let (_d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    let l1: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    r.view_create(&a, "pinned", None, true).unwrap();
    write_as(&mut r, &a, "pinned", update(&c, &v, "pinned's")).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:z", "r1"),
        "closed",
        update(&c, &v, "closed's"),
    )
    .unwrap();
    r.store_mut()
        .view_append(
            "closed",
            vec![ViewOpKind::Close {
                reason: "gone".into(),
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    r.view_create(&a, "empty", None, false).unwrap();
    write_as(&mut r, &a, "p", update(&c, &v, "p's")).unwrap();
    let p = r.propose(&a, "p", "p").unwrap();
    let later = write_as(&mut r, &a, "p", claim(&section, "after proposing"))
        .unwrap()
        .change;
    let rx = r.subscribe();
    let l2: rdy_model::Hash = r.land(&approver(), &p.id).unwrap().parse().unwrap();
    let st = r.read();
    assert_eq!(
        st.view_head("pinned").unwrap().base,
        Some(l1),
        "pinned keeps its base"
    );
    assert!(
        st.conflicts(&view("pinned")).unwrap().is_empty(),
        "no conflict is recorded for a pinned view"
    );
    assert_eq!(
        st.view_head("closed").unwrap().base,
        Some(l1),
        "closed is not rebased"
    );
    assert_ne!(
        st.view_head("empty").unwrap().base,
        Some(l2),
        "no op for an empty view"
    );
    assert_eq!(st.effective_base("empty"), Some(l2), "but it floats");
    assert_eq!(st.view_head("p").unwrap().base, Some(l2));
    assert_eq!(
        st.included("p").as_ref(),
        &vec![later.parse().unwrap()],
        "the write after the proposal stays"
    );
    let events = drain(&rx);
    assert_eq!(rebased_in(&events), vec![("p".to_string(), 0)]);
    assert!(!events.iter().any(|e| matches!(&e.kind, EventKind::ViewClosed { view, reason } if view == "closed" && reason == "gone")), "the close happened before subscribing");
    let all = r
        .replay_events(rdy_store::segment::Location {
            segment: 0,
            offset: 0,
        })
        .unwrap();
    assert!(all.iter().any(|e| matches!(&e.kind, EventKind::ViewClosed { view, reason } if view == "closed" && reason == "gone")), "{all:?}");
    assert!(
        !all.iter()
            .any(|e| matches!(&e.kind, EventKind::ViewOp { kind, .. } if kind == "close"))
    );
}

/// Spec §6.5 "Order keys": a rebased view's base is the canon head, so a landed re-key of its
/// siblings no longer warns; a pinned view keeps the warning (Task 3 pinned `engine.rs`'s ord3 tests).
#[test]
fn reb1_after_a_landed_rekey_a_rebased_view_gets_no_siblings_rekeyed_warning() {
    let (_d, mut r) = project();
    let (_doc, section, first) = seed_doc(&mut r, "seed");
    let second = write(&mut r, "seed", claim(&section, "second"))
        .unwrap()
        .created
        .values()
        .next()
        .unwrap()
        .clone();
    land(&mut r, "seed").unwrap();
    let b = as_agent("agent:b", "r1");
    write_as(&mut r, &b, "vb", json!([{"op": "create", "type": "claim", "parent": section, "after": first, "body": "from b"}])).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:a", "r1"),
        "va",
        json!([{"op": "move", "id": first, "parent": section, "after": second}]),
    )
    .unwrap();
    land(&mut r, "va").unwrap();
    assert_eq!(
        r.read().view_head("vb").unwrap().base,
        r.read().canon_head(),
        "vb was rebased onto va's landing"
    );
    let p = r.propose(&b, "vb", "b").unwrap();
    assert!(
        !p.warnings.iter().any(|i| i.code == "W_SIBLINGS_REKEYED"),
        "{:?}",
        p.warnings
    );
}
