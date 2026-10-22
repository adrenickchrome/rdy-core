//! A re-affirmed premise (concepts/suspects-tensions p-QMGB4, D-EJBPD, D-GV465): an accepted,
//! live decision that records the retiring decisions of a target in its `reaffirmed` field is
//! no longer suspect on that target while the set is unchanged. Full builds and graphs carried
//! across writes agree, and every other suspect rule is unchanged (system/graph p-3FWHH).
use rdy_graph::*;
use rdy_model::engine_fields::{self, Against, Reaffirmation};
use rdy_model::*;
use std::collections::{BTreeMap, BTreeSet};

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn b(i: &str, ty: &str) -> Block {
    Block {
        id: id(i),
        block_type: ty.into(),
        parent: Some(id("s-SSSSS")),
        order: "V".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn dec(i: &str, status: &str) -> Block {
    let mut d = b(i, "decision");
    d.fields
        .insert("status".into(), FieldValue::Str(status.into()));
    d.fields.insert(
        "statement".into(),
        FieldValue::Str(format!("statement of {i}")),
    );
    d
}

fn link(block: &mut Block, name: &str, target: &str, part: Option<&str>) {
    block.links.entry(name.into()).or_default().push(Link {
        target: id(target),
        part: part.map(Into::into),
    });
}

/// `block` depends on each of `targets`, pinned to its current content.
fn depends(block: &mut Block, targets: &[&Block]) {
    for t in targets {
        link(block, "depends_on", t.id.as_str(), None);
        block.pins.insert(t.id.clone(), t.content_hash());
    }
}

fn retiring(ids: &[&str]) -> Against {
    let mut v: Vec<BlockId> = ids.iter().map(|s| id(s)).collect();
    v.sort();
    Against::Retiring(v)
}

/// `block` with its `reaffirmed` field set to `acks` (or removed when empty).
fn ack(block: &Block, acks: &[(&str, Against)]) -> Block {
    let mut out = block.clone();
    let map: BTreeMap<BlockId, Reaffirmation> = acks
        .iter()
        .map(|(t, a)| {
            (
                id(t),
                Reaffirmation {
                    against: a.clone(),
                    reason: format!("{t} still holds"),
                },
            )
        })
        .collect();
    if map.is_empty() {
        out.fields.remove(engine_fields::REAFFIRMED);
    } else {
        out.fields.insert(
            engine_fields::REAFFIRMED.into(),
            engine_fields::encode_reaffirmed(&map),
        );
    }
    out
}

fn graph(blocks: &[&Block]) -> Graph {
    let m: BTreeMap<BlockId, &Block> = blocks.iter().map(|b| (b.id.clone(), *b)).collect();
    Graph::build_from(m.into_values())
}

fn pairs(g: &Graph) -> Vec<(String, String, String)> {
    g.suspects()
        .into_iter()
        .map(|s| (s.block.to_string(), s.target.to_string(), s.reason))
        .collect()
}

fn reason(g: &Graph, block: &str, target: &str) -> Option<String> {
    g.suspects()
        .into_iter()
        .find(|s| s.block.as_str() == block && s.target.as_str() == target)
        .map(|s| s.reason)
}

/// perf4: `suspects_of` every candidate with `prev`'s suspects of every other block equals
/// `g`'s suspects.
fn carried_suspects_equal(prev: &Graph, g: &Graph, what: &str) {
    let cand = g.suspect_candidates(prev);
    let mut s: Vec<Suspect> = prev
        .suspects()
        .into_iter()
        .filter(|x| !cand.contains(&x.block))
        .collect();
    for c in &cand {
        s.extend(g.suspects_of(c));
    }
    s.sort_by(|a, b| (&a.block, &a.target).cmp(&(&b.block, &b.target)));
    assert_eq!(s, g.suspects(), "{what}");
}

fn set(ids: &[&str]) -> BTreeSet<BlockId> {
    ids.iter().map(|s| id(s)).collect()
}

/// Two causes retire D-AAAAA (a superseder and a revoker). The source that records both is
/// clear on D-AAAAA alone: its other retired premise, other sources on the same target, and
/// every edge, status and content hash stay as they were.
#[test]
fn cp4_same_retiring_set_clears_only_the_acknowledged_edge() {
    let a = dec("D-AAAAA", "accepted");
    let two = dec("D-TW222", "accepted");
    let mut sup = dec("D-SPRSD", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    link(&mut sup, "supersedes", "D-TW222", None);
    let mut rev = dec("D-REV0K", "accepted");
    link(&mut rev, "revokes", "D-AAAAA", None);
    let mut src = dec("D-SRCEE", "accepted");
    depends(&mut src, &[&a, &two]);
    let mut other = dec("D-0THER", "accepted");
    depends(&mut other, &[&a]);
    let mut claim = b("c-C1A1M", "claim");
    claim.body = "Per {{D-AAAAA}}.".into();
    claim.pins.insert(a.id.clone(), a.content_hash());

    let before = graph(&[&a, &two, &sup, &rev, &src, &other, &claim]);
    assert_eq!(
        before.retiring_decisions(&a.id),
        set(&["D-REV0K", "D-SPRSD"])
    );
    assert_eq!(before.retiring_decisions(&two.id), set(&["D-SPRSD"]));
    assert_eq!(before.retiring_decisions(&src.id), set(&[]));
    assert_eq!(
        reason(&before, "D-SRCEE", "D-AAAAA").as_deref(),
        Some("revoked")
    );

    let acked = ack(&src, &[("D-AAAAA", retiring(&["D-SPRSD", "D-REV0K"]))]);
    assert_eq!(
        acked.content_hash(),
        src.content_hash(),
        "metadata only: the source's content is unchanged"
    );
    let after = graph(&[&a, &two, &sup, &rev, &acked, &other, &claim]);
    assert_eq!(
        pairs(&after),
        vec![
            ("D-0THER".into(), "D-AAAAA".into(), "revoked".into()),
            ("D-SRCEE".into(), "D-TW222".into(), "superseded".into()),
            ("c-C1A1M".into(), "D-AAAAA".into(), "revoked".into()),
        ],
        "only the acknowledged source/target pair clears"
    );
    for i in [&a, &two, &sup, &rev, &src, &other, &claim].map(|b| b.id.clone()) {
        assert_eq!(
            after.outgoing(&i),
            before.outgoing(&i),
            "{i}: no edge moves"
        );
        assert_eq!(
            after.incoming(&i),
            before.incoming(&i),
            "{i}: no edge moves"
        );
        assert_eq!(after.status(&i), before.status(&i), "{i}: no ruling moves");
        assert_eq!(after.impact_via(&i, 5), before.impact_via(&i, 5), "{i}");
        assert_eq!(after.governing(&i), before.governing(&i), "{i}");
        assert_eq!(after.retiring_decisions(&i), before.retiring_decisions(&i));
    }
    assert!(matches!(
        after.status(&a.id),
        Some(DerivedStatus::Revoked { .. })
    ));

    // one of the two causes is not the set
    let half = ack(&src, &[("D-AAAAA", retiring(&["D-SPRSD"]))]);
    let g = graph(&[&a, &two, &sup, &rev, &half, &other, &claim]);
    assert_eq!(reason(&g, "D-SRCEE", "D-AAAAA").as_deref(), Some("revoked"));
    // nor is a superset
    let more = ack(
        &src,
        &[("D-AAAAA", retiring(&["D-SPRSD", "D-REV0K", "D-ZZZZZ"]))],
    );
    let g = graph(&[&a, &two, &sup, &rev, &more, &other, &claim]);
    assert_eq!(reason(&g, "D-SRCEE", "D-AAAAA").as_deref(), Some("revoked"));
    // an acknowledgement for another target clears nothing on this one
    let elsewhere = ack(&src, &[("D-TW222", retiring(&["D-REV0K", "D-SPRSD"]))]);
    let g = graph(&[&a, &two, &sup, &rev, &elsewhere, &other, &claim]);
    assert_eq!(reason(&g, "D-SRCEE", "D-AAAAA").as_deref(), Some("revoked"));
    assert_eq!(
        reason(&g, "D-SRCEE", "D-TW222").as_deref(),
        Some("superseded")
    );
}

/// The recorded set is the union of accepted whole and partial superseders and revokers. A new
/// retiring decision, or one taken away, revives the suspect with the usual reason precedence;
/// a proposed one changes nothing.
#[test]
fn cp4_new_superseder_or_revoker_restores_the_suspect() {
    let a = dec("D-AAAAA", "accepted");
    let mut partial = dec("D-PART1", "accepted");
    link(
        &mut partial,
        "supersedes",
        "D-AAAAA",
        Some("the moves part"),
    );
    let mut sup = dec("D-SPRSD", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let mut src = dec("D-SRCEE", "accepted");
    depends(&mut src, &[&a]);
    let acked = ack(&src, &[("D-AAAAA", retiring(&["D-PART1", "D-SPRSD"]))]);

    let g0 = graph(&[&a, &partial, &sup, &acked]);
    assert_eq!(g0.retiring_decisions(&a.id), set(&["D-PART1", "D-SPRSD"]));
    assert!(matches!(
        g0.status(&a.id),
        Some(DerivedStatus::Superseded { .. })
    ));
    assert_eq!(pairs(&g0), vec![], "the whole set is recorded");
    let whole_only = ack(&src, &[("D-AAAAA", retiring(&["D-SPRSD"]))]);
    assert_eq!(
        reason(
            &graph(&[&a, &partial, &sup, &whole_only]),
            "D-SRCEE",
            "D-AAAAA"
        )
        .as_deref(),
        Some("superseded"),
        "a partial superseder is in the set though the whole one wins the status"
    );

    // a proposed revoker is no retiring decision
    let mut proposed = dec("D-REV0K", "proposed");
    link(&mut proposed, "revokes", "D-AAAAA", None);
    let g1 = g0.with_changes([], [&proposed]);
    assert_eq!(g1, graph(&[&a, &partial, &sup, &acked, &proposed]));
    assert_eq!(pairs(&g1), vec![]);
    carried_suspects_equal(&g0, &g1, "a proposed revoker");

    // accepted, it revives the suspect as revoked
    let mut rev = dec("D-REV0K", "accepted");
    link(&mut rev, "revokes", "D-AAAAA", None);
    let g2 = g1.with_changes([&proposed], [&rev]);
    assert_eq!(g2, graph(&[&a, &partial, &sup, &acked, &rev]));
    assert_eq!(
        pairs(&g2),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "revoked".into())]
    );
    carried_suspects_equal(&g1, &g2, "a revoker accepted");

    // re-affirmed against all three, it clears again
    let acked3 = ack(
        &src,
        &[("D-AAAAA", retiring(&["D-PART1", "D-REV0K", "D-SPRSD"]))],
    );
    let g3 = g2.with_changes([&acked], [&acked3]);
    assert_eq!(pairs(&g3), vec![]);
    carried_suspects_equal(&g2, &g3, "re-affirmed against the revoker too");

    // a second whole superseder revives it; the revoker still wins the reason
    let mut sup2 = dec("D-SPRS2", "accepted");
    link(&mut sup2, "supersedes", "D-AAAAA", None);
    let g4 = g3.with_changes([], [&sup2]);
    assert_eq!(g4, graph(&[&a, &partial, &sup, &rev, &acked3, &sup2]));
    assert_eq!(
        pairs(&g4),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "revoked".into())]
    );
    carried_suspects_equal(&g3, &g4, "a second superseder");

    // without the revoker, a removed retiring decision also breaks equality: superseded
    let g5 = g4.with_changes([&sup2, &rev], []);
    assert_eq!(g5, graph(&[&a, &partial, &sup, &acked3]));
    assert_eq!(
        pairs(&g5),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "superseded".into())]
    );
    carried_suspects_equal(&g4, &g5, "the revoker removed");

    // a removed partial superseder breaks the two-member record too
    let g6 = g0.with_changes([&partial], []);
    assert_eq!(g6, graph(&[&a, &sup, &acked]));
    assert_eq!(
        pairs(&g6),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "superseded".into())]
    );
    carried_suspects_equal(&g0, &g6, "the partial superseder removed");
}

/// `against: deleted` holds only while the target is absent. When the target comes back, its
/// pin and status decide as before.
#[test]
fn cp4_deleted_acknowledgement_ends_when_target_returns() {
    let a = dec("D-AAAAA", "accepted");
    let mut src = dec("D-SRCEE", "accepted");
    depends(&mut src, &[&a]);
    let acked = ack(&src, &[("D-AAAAA", Against::Deleted)]);

    assert_eq!(
        pairs(&graph(&[&src])),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "deleted".into())]
    );
    let g0 = graph(&[&acked]);
    assert_eq!(pairs(&g0), vec![], "the deletion is acknowledged");
    // a retirement record is not a deletion record
    let wrong = ack(&src, &[("D-AAAAA", retiring(&[]))]);
    assert_eq!(
        reason(&graph(&[&wrong]), "D-SRCEE", "D-AAAAA").as_deref(),
        Some("deleted")
    );

    // the target comes back with other content: changed
    let mut edited = a.clone();
    edited
        .fields
        .insert("statement".into(), FieldValue::Str("different".into()));
    let g1 = g0.with_changes([], [&edited]);
    assert_eq!(g1, graph(&[&acked, &edited]));
    assert_eq!(
        pairs(&g1),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "changed".into())]
    );
    carried_suspects_equal(&g0, &g1, "the target returns changed");

    // it comes back unchanged but superseded: superseded
    let mut sup = dec("D-SPRSD", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let g2 = g0.with_changes([], [&a, &sup]);
    assert_eq!(g2, graph(&[&acked, &a, &sup]));
    assert_eq!(
        pairs(&g2),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "superseded".into())]
    );
    carried_suspects_equal(&g0, &g2, "the target returns superseded");

    // and deleted again, the record holds again
    let g3 = g2.with_changes([&a], []);
    assert_eq!(g3, graph(&[&acked, &sup]));
    assert_eq!(pairs(&g3), vec![]);
    carried_suspects_equal(&g2, &g3, "the target deleted again");
}

/// A changed target clears only by a current pin: no `reaffirmed` record, of any shape,
/// clears content drift, even when the target's retirement is acknowledged.
#[test]
fn cp4_changed_target_still_requires_current_pin() {
    let a = dec("D-AAAAA", "accepted");
    let mut src = dec("D-SRCEE", "accepted");
    depends(&mut src, &[&a]);
    let mut edited = a.clone();
    edited
        .fields
        .insert("statement".into(), FieldValue::Str("different".into()));

    for (what, against) in [
        ("none", None),
        ("an empty retirement list (O18)", Some(retiring(&[]))),
        ("a retirement list", Some(retiring(&["D-SPRSD"]))),
        ("deleted", Some(Against::Deleted)),
    ] {
        let s = match &against {
            Some(x) => ack(&src, &[("D-AAAAA", x.clone())]),
            None => src.clone(),
        };
        assert_eq!(
            pairs(&graph(&[&edited, &s])),
            vec![("D-SRCEE".into(), "D-AAAAA".into(), "changed".into())],
            "{what}"
        );
        let mut repinned = s.clone();
        repinned
            .pins
            .insert(edited.id.clone(), edited.content_hash());
        assert_eq!(pairs(&graph(&[&edited, &repinned])), vec![], "{what}");
    }

    // an acknowledged retirement leaves the drift to the pin
    let mut sup = dec("D-SPRSD", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let acked = ack(&src, &[("D-AAAAA", retiring(&["D-SPRSD"]))]);
    assert_eq!(pairs(&graph(&[&a, &sup, &acked])), vec![]);
    assert_eq!(
        pairs(&graph(&[&edited, &sup, &acked])),
        vec![("D-SRCEE".into(), "D-AAAAA".into(), "changed".into())]
    );
    let mut repinned = acked.clone();
    repinned
        .pins
        .insert(edited.id.clone(), edited.content_hash());
    assert_eq!(pairs(&graph(&[&edited, &sup, &repinned])), vec![]);
}

/// Every query of a graph carried across writes that add, update and remove `reaffirmed`
/// records equals a full rebuild's.
#[test]
fn cp4_acknowledgement_carry_matches_full_rebuild() {
    let a = dec("D-AAAAA", "accepted");
    let two = dec("D-TW222", "accepted");
    let mut sup = dec("D-SPRSD", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let mut rev = dec("D-REV0K", "accepted");
    link(&mut rev, "revokes", "D-TW222", None);
    let mut src = dec("D-SRCEE", "accepted");
    depends(&mut src, &[&a, &two]);
    src.body = "Also {{D-AAAAA}}.".into();
    let mut other = dec("D-0THER", "accepted");
    depends(&mut other, &[&a]);
    let mut claim = b("c-C1A1M", "claim");
    link(&mut claim, "decided_by", "D-SRCEE", None);
    let section = b("s-SSSSS", "section");

    let mut state: BTreeMap<BlockId, Block> =
        [&a, &two, &sup, &rev, &src, &other, &claim, &section]
            .into_iter()
            .map(|b| (b.id.clone(), b.clone()))
            .collect();
    let mut g = Graph::build_from(state.values());

    let src_id = src.id.clone();
    let mut step = |old: Vec<Block>, new: Vec<Block>, what: &str, expect: &[(&str, &str)]| {
        let next = g.with_changes(old.iter(), new.iter());
        for o in &old {
            state.remove(&o.id);
        }
        for n in &new {
            state.insert(n.id.clone(), n.clone());
        }
        let full = Graph::build_from(state.values());
        assert_eq!(next, full, "{what}: the carried graph equals the rebuild");
        assert_eq!(next.suspects(), full.suspects(), "{what}");
        let mut ids: BTreeSet<BlockId> = state.keys().cloned().collect();
        ids.extend([&a, &two, &sup, &rev, &src, &other].map(|b| b.id.clone()));
        for i in &ids {
            assert_eq!(next.suspects_of(i), full.suspects_of(i), "{what}: {i}");
            assert_eq!(next.status(i), full.status(i), "{what}: {i}");
            assert_eq!(
                next.retiring_decisions(i),
                full.retiring_decisions(i),
                "{what}: {i}"
            );
            assert_eq!(next.outgoing(i), full.outgoing(i), "{what}: {i}");
            assert_eq!(next.incoming(i), full.incoming(i), "{what}: {i}");
            assert_eq!(next.impact_via(i, 4), full.impact_via(i, 4), "{what}: {i}");
            assert_eq!(next.governing(i), full.governing(i), "{what}: {i}");
            assert_eq!(next.governed_by(i), full.governed_by(i), "{what}: {i}");
            assert_eq!(next.redacted_pins_of(i), full.redacted_pins_of(i));
        }
        let keep: BTreeSet<BlockId> = [&a, &sup, &src].map(|b| b.id.clone()).into();
        assert_eq!(next.induced(&keep), full.induced(&keep), "{what}");
        assert_eq!(
            next.induced(&keep).suspects(),
            full.induced(&keep).suspects(),
            "{what}"
        );
        carried_suspects_equal(&g, &next, what);
        let got: Vec<(String, String)> = next
            .suspects_of(&src_id)
            .into_iter()
            .map(|s| (s.target.to_string(), s.reason))
            .collect();
        let want: Vec<(String, String)> = expect
            .iter()
            .map(|(t, r)| (t.to_string(), r.to_string()))
            .collect();
        assert_eq!(got, want, "{what}: the source's suspects");
        g = next;
    };

    let acked = ack(&src, &[("D-AAAAA", retiring(&["D-SPRSD"]))]);
    step(
        vec![src.clone()],
        vec![acked.clone()],
        "an acknowledgement added",
        &[("D-TW222", "revoked")],
    );
    let both = ack(
        &src,
        &[
            ("D-AAAAA", retiring(&["D-SPRSD"])),
            ("D-TW222", retiring(&["D-REV0K"])),
        ],
    );
    step(
        vec![acked.clone()],
        vec![both.clone()],
        "an acknowledgement updated",
        &[],
    );
    let mut sup2 = dec("D-SPRS2", "accepted");
    link(&mut sup2, "supersedes", "D-AAAAA", None);
    step(
        vec![],
        vec![sup2.clone()],
        "a new superseder",
        &[("D-AAAAA", "superseded")],
    );
    step(vec![sup2], vec![], "the new superseder removed", &[]);
    let other_acked = ack(&other, &[("D-AAAAA", retiring(&["D-SPRSD"]))]);
    step(
        vec![other.clone()],
        vec![other_acked.clone()],
        "a second source",
        &[],
    );
    step(vec![sup.clone()], vec![], "the superseder removed", &[]);
    step(
        vec![a.clone()],
        vec![],
        "the target deleted",
        &[("D-AAAAA", "deleted")],
    );
    let deleted = ack(
        &src,
        &[
            ("D-AAAAA", Against::Deleted),
            ("D-TW222", retiring(&["D-REV0K"])),
        ],
    );
    step(
        vec![both.clone()],
        vec![deleted.clone()],
        "a deletion acknowledged",
        &[],
    );
    step(
        vec![],
        vec![a.clone(), sup.clone()],
        "the target returns",
        &[("D-AAAAA", "superseded")],
    );
    step(
        vec![deleted],
        vec![src.clone()],
        "the acknowledgement removed",
        &[("D-AAAAA", "superseded"), ("D-TW222", "revoked")],
    );
    step(vec![src.clone()], vec![], "the source removed", &[]);
    step(vec![], vec![both], "the source returns acknowledged", &[]);
}

/// A change to only a source's `reaffirmed` record changes no edge, content hash or status, yet
/// the source's suspects can change: it is a suspect candidate. So is an acknowledging source
/// whose own status changes.
#[test]
fn cp4_metadata_change_is_a_suspect_candidate() {
    let a = dec("D-AAAAA", "accepted");
    let mut sup = dec("D-SPRSD", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let mut src = dec("D-SRCEE", "accepted");
    depends(&mut src, &[&a]);
    let bystanders: Vec<Block> = (0..20)
        .map(|i| {
            let mut c = b(&format!("c-BY{:03}", i), "claim");
            c.body = format!("Unrelated {i}.");
            c
        })
        .collect();
    let mut all = vec![&a, &sup, &src];
    all.extend(bystanders.iter());
    let g0 = graph(&all);
    assert!(g0.suspect_candidates(&g0).is_empty(), "no change");

    let acked = ack(&src, &[("D-AAAAA", retiring(&["D-SPRSD"]))]);
    let g1 = g0.with_changes([&src], [&acked]);
    let cand = g1.suspect_candidates(&g0);
    assert!(cand.contains(&src.id), "the acknowledging source: {cand:?}");
    assert!(cand.len() < 5, "{cand:?}");
    assert_eq!(pairs(&g1), vec![]);
    carried_suspects_equal(&g0, &g1, "an acknowledgement added");

    let g2 = g1.with_changes([&acked], [&src]);
    assert!(g2.suspect_candidates(&g1).contains(&src.id));
    carried_suspects_equal(&g1, &g2, "an acknowledgement removed");

    // the source itself superseded: its exemption ends
    let mut retire_src = dec("D-RETSR", "accepted");
    link(&mut retire_src, "supersedes", "D-SRCEE", None);
    let g3 = g1.with_changes([], [&retire_src]);
    assert!(g3.suspect_candidates(&g1).contains(&src.id));
    assert_eq!(
        reason(&g3, "D-SRCEE", "D-AAAAA").as_deref(),
        Some("superseded")
    );
    carried_suspects_equal(&g1, &g3, "the source superseded");

    // the source made proposed: its exemption ends
    let mut proposed = acked.clone();
    proposed
        .fields
        .insert("status".into(), FieldValue::Str("proposed".into()));
    let g4 = g1.with_changes([&acked], [&proposed]);
    assert!(g4.suspect_candidates(&g1).contains(&src.id));
    carried_suspects_equal(&g1, &g4, "the source made proposed");

    // the source's superseder made proposed: the exemption returns
    let mut retire_proposed = retire_src.clone();
    retire_proposed
        .fields
        .insert("status".into(), FieldValue::Str("proposed".into()));
    let g5 = g3.with_changes([&retire_src], [&retire_proposed]);
    assert!(g5.suspect_candidates(&g3).contains(&src.id));
    assert_eq!(pairs(&g5), vec![]);
    carried_suspects_equal(&g3, &g5, "the source's superseder made proposed");
}

/// Only an accepted, live or partly superseded decision uses its record. A proposed, rejected,
/// fully superseded or revoked source stays suspect, and so does a block that is no decision.
#[test]
fn cp4_dead_or_proposed_source_cannot_use_reaffirmed() {
    let a = dec("D-AAAAA", "accepted");
    let mut sup = dec("D-SPRSD", "accepted");
    link(&mut sup, "supersedes", "D-AAAAA", None);
    let records: [(&str, Against); 1] = [("D-AAAAA", retiring(&["D-SPRSD"]))];

    let mut src = dec("D-SRCEE", "accepted");
    depends(&mut src, &[&a]);
    let live = ack(&src, &records);
    assert_eq!(pairs(&graph(&[&a, &sup, &live])), vec![], "live");

    let mut whole = dec("D-RETSR", "accepted");
    link(&mut whole, "supersedes", "D-SRCEE", None);
    let mut revoker = dec("D-RETSR", "accepted");
    link(&mut revoker, "revokes", "D-SRCEE", None);
    let mut partial = dec("D-RETSR", "accepted");
    link(&mut partial, "supersedes", "D-SRCEE", Some("one part"));
    assert_eq!(
        pairs(&graph(&[&a, &sup, &live, &partial])),
        vec![],
        "partly superseded"
    );

    let with_status = |s: &str| {
        let mut x = live.clone();
        x.fields.insert("status".into(), FieldValue::Str(s.into()));
        x
    };
    let proposed = with_status("proposed");
    let rejected = with_status("rejected");
    for (what, blocks) in [
        ("proposed", vec![&a, &sup, &proposed]),
        ("rejected", vec![&a, &sup, &rejected]),
        ("fully superseded", vec![&a, &sup, &live, &whole]),
        ("revoked", vec![&a, &sup, &live, &revoker]),
    ] {
        assert_eq!(
            reason(&graph(&blocks), "D-SRCEE", "D-AAAAA").as_deref(),
            Some("superseded"),
            "{what}"
        );
    }

    // the same holds for a deletion record
    let gone = ack(&src, &[("D-AAAAA", Against::Deleted)]);
    let mut gone_proposed = gone.clone();
    gone_proposed
        .fields
        .insert("status".into(), FieldValue::Str("proposed".into()));
    assert_eq!(pairs(&graph(&[&gone])), vec![]);
    assert_eq!(
        reason(&graph(&[&gone_proposed]), "D-SRCEE", "D-AAAAA").as_deref(),
        Some("deleted")
    );
    assert_eq!(
        reason(&graph(&[&gone, &revoker]), "D-SRCEE", "D-AAAAA").as_deref(),
        Some("deleted")
    );

    // a block that is no decision has no exemption
    let mut claim = b("c-C1A1M", "claim");
    depends(&mut claim, &[&a]);
    let claim = ack(&claim, &records);
    assert_eq!(
        reason(&graph(&[&a, &sup, &claim]), "c-C1A1M", "D-AAAAA").as_deref(),
        Some("superseded")
    );
}

/// task-5 R1 (p-QMGB4): the one predicate write, proposal and landing share. A reference to a
/// deleted target is a kept citation only from an accepted decision whose `reaffirmed` record
/// holds `deleted` for exactly that target.
#[test]
fn cp5_only_an_accepted_decisions_deleted_record_keeps_a_citation() {
    let (t, other) = (id("c-TTTTT"), "c-ZZZZZ");
    let accepted = dec("D-AAAAA", "accepted");
    assert!(keeps_deleted_citation(
        &ack(&accepted, &[("c-TTTTT", Against::Deleted)]),
        &t
    ));
    assert!(keeps_deleted_citation(
        &ack(
            &accepted,
            &[("c-TTTTT", Against::Deleted), (other, retiring(&[]))]
        ),
        &t
    ));
    // no record, a record for another target, or one that is not `deleted`
    assert!(!keeps_deleted_citation(&accepted, &t));
    assert!(!keeps_deleted_citation(
        &ack(&accepted, &[(other, Against::Deleted)]),
        &t
    ));
    assert!(!keeps_deleted_citation(
        &ack(&accepted, &[("c-TTTTT", retiring(&[]))]),
        &t
    ));
    assert!(!keeps_deleted_citation(
        &ack(&accepted, &[("c-TTTTT", retiring(&["D-RRRRR"]))]),
        &t
    ));
    // a block that is no accepted decision
    for status in ["proposed", "rejected"] {
        let d = dec("D-PPPPP", status);
        assert!(
            !keeps_deleted_citation(&ack(&d, &[("c-TTTTT", Against::Deleted)]), &t),
            "{status}"
        );
    }
    let claim = b("c-KKKKK", "claim");
    assert!(!keeps_deleted_citation(
        &ack(&claim, &[("c-TTTTT", Against::Deleted)]),
        &t
    ));
}
