//! undo1: undo appends an inverse; repeated undos walk back; undo of close rebases; rebases
//! cannot be undone.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_store::view::ViewOpKind;
use serde_json::json;

fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}
fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn view(v: &str) -> Scope {
    Scope::View(v.into())
}
fn head(r: &Rdy, v: &str) -> String {
    r.read().view_head(v).unwrap().head.to_string()
}
fn op_before(r: &Rdy, _v: &str, h: &str) -> String {
    r.read()
        .reader()
        .view_op(&h.parse().unwrap())
        .unwrap()
        .prev
        .unwrap()
        .to_string()
}

#[test]
fn undo1_target_less_undos_walk_back_and_an_undo_of_an_undo_redoes() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "1"))
        .unwrap()
        .change;
    let c2 = write_as(&mut r, &a, "a", claim(&section, "2"))
        .unwrap()
        .change;
    let u1 = r.undo(&a, "a", None, false).unwrap();
    assert_eq!(
        (u1.kind.as_str(), &u1.excluded, u1.effect.as_str()),
        ("commit", &vec![c2.clone()], "excluded 1 change(s)")
    );
    assert_eq!(r.read().included("a").as_ref(), &vec![c1.parse().unwrap()]);
    let u2 = r.undo(&a, "a", None, false).unwrap();
    assert_eq!(
        &u2.excluded,
        &vec![c1.clone()],
        "the second undo walks back, it does not redo"
    );
    assert_eq!(r.read().included("a").len(), 0);
    let e = r.undo(&a, "a", None, false).unwrap_err();
    assert_eq!(e.code, E_NOTHING_TO_UNDO, "{e}");
    let redo = r.undo(&a, "a", Some(&u1.op), false).unwrap();
    assert_eq!(
        (redo.kind.as_str(), &redo.included),
        ("undo", &vec![c2.clone()])
    );
    assert_eq!(r.read().included("a").as_ref(), &vec![c2.parse().unwrap()]);
    let e = r.undo(&a, "a", Some(&u1.op), false).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "already undone: {e}");
    check(&r, &view("a")).unwrap();
    r.fsck().unwrap();
}

#[test]
fn undo1_undo_of_include_and_exclude_and_the_conflicts_a_reinclude_brings_back() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", update(&c, &v, "a's")).unwrap();
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap()
    .change;
    r.view_include(&a, "a", std::slice::from_ref(&cb)).unwrap();
    assert_eq!(r.read().conflicts(&view("a")).unwrap().len(), 1);
    let u = r.undo(&a, "a", None, false).unwrap();
    assert_eq!(
        (u.kind.as_str(), &u.excluded),
        ("include", &vec![cb.clone()])
    );
    assert!(u.conflicts.is_empty());
    let ex = r
        .view_exclude(&a, "a", &[r.read().included("a")[0].to_string()], false)
        .unwrap();
    assert_eq!(ex.changes, 0);
    let u = r.undo(&a, "a", None, false).unwrap();
    assert_eq!((u.kind.as_str(), u.included.len()), ("exclude", 1));
    let u = r
        .undo(
            &a,
            "a",
            Some(&op_before(&r, "a", &op_before(&r, "a", &head(&r, "a")))),
            false,
        )
        .unwrap();
    assert_eq!(
        u.kind, "undo",
        "undoing the first undo re-includes b's change"
    );
    assert_eq!(u.conflicts.len(), 1, "and the conflict is back");
    check(&r, &view("a")).unwrap();
}

#[test]
fn undo1_an_undo_that_would_orphan_dependents_is_blocked_unless_cascaded() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "x")).unwrap();
    let x = c1.created.values().next().unwrap().clone();
    let v1 = version_of(&r, &view("a"), &x);
    let c2 = write_as(&mut r, &a, "a", update(&x, &v1, "x2"))
        .unwrap()
        .change;
    let commit1 = op_before(&r, "a", &head(&r, "a"));
    let e = r.undo(&a, "a", Some(&commit1), false).unwrap_err();
    assert_eq!(e.code, E_UNDO_BLOCKED, "{e}");
    assert_eq!(e.issues.len(), 1);
    assert_eq!(r.read().included("a").len(), 2, "nothing written");
    let u = r.undo(&a, "a", Some(&commit1[..16]), true).unwrap();
    assert_eq!(
        (&u.excluded, &u.cascaded),
        (&vec![c1.change.clone()], &vec![c2.clone()])
    );
    assert_eq!(r.read().included("a").len(), 0);
}

#[test]
fn undo1_undo_of_close_reopens_and_rebases_onto_canon_recording_conflicts() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    let l1: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", update(&c, &v, "a's")).unwrap();
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Close {
                reason: "pause".into(),
            }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap();
    let l2: rdy_model::Hash = land(&mut r, "b").unwrap().parse().unwrap();
    assert_eq!(
        r.read().view_head("a").unwrap().base,
        Some(l1),
        "closed: not rebased"
    );
    let close = head(&r, "a");
    let u = r.undo(&a, "a", Some(&close), false).unwrap();
    assert!(u.reopened && u.rebased, "{u:?}");
    assert_eq!(u.conflicts.len(), 1);
    let st = r.read();
    let h = st.view_head("a").unwrap();
    assert!(!h.closed);
    assert_eq!(h.base, Some(l2));
    assert_eq!(st.conflicts(&view("a")).unwrap().len(), 1);
    match st.reader().view_op(&h.head).unwrap().op {
        ViewOpKind::Rebase {
            landing, conflicts, ..
        } => {
            assert_eq!(landing, l2);
            assert_eq!(conflicts.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    check(&r, &view("a")).unwrap();
    // a pinned closed view reopens without a rebase
    r.view_create(&a, "pin", Some(&format!("landing:{l1}")), false)
        .unwrap();
    write_as(&mut r, &a, "pin", update(&c, &v, "pin's")).unwrap();
    r.store_mut()
        .view_append(
            "pin",
            vec![ViewOpKind::Close { reason: "x".into() }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    let close = head(&r, "pin");
    let u = r.undo(&a, "pin", Some(&close), false).unwrap();
    assert!(u.reopened && !u.rebased);
    assert_eq!(r.read().view_head("pin").unwrap().base, Some(l1));
}

/// Review Focus 7, plus E_UNDO_LANDING and undo of create.
#[test]
fn undo1_a_rebase_cannot_be_undone_a_foreign_op_is_not_found_and_undo_of_create_closes() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", claim(&section, "a")).unwrap();
    write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        claim(&section, "b"),
    )
    .unwrap();
    let b_head = head(&r, "b");
    let e = r.undo(&a, "a", Some(&b_head), false).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    assert!(e.message.contains("view \"a\""), "{e}");
    land(&mut r, "b").unwrap();
    let rebase = head(&r, "a");
    let e = r.undo(&a, "a", Some(&rebase), false).unwrap_err();
    assert_eq!(e.code, E_UNDO_LANDING, "{e}");
    assert!(e.fix.contains("revok"), "{e}");
    let mut cur = head(&r, "a");
    loop {
        let op = r.read().reader().view_op(&cur.parse().unwrap()).unwrap();
        match op.prev {
            Some(p) => cur = p.to_string(),
            None => break,
        }
    }
    let u = r.undo(&a, "a", Some(&cur), false).unwrap();
    assert!(u.closed && u.kind == "create", "{u:?}");
    assert!(r.read().view_head("a").unwrap().closed);
    let e = write_as(&mut r, &a, "a", claim(&section, "more")).unwrap_err();
    assert_eq!(e.code, E_VIEW_CLOSED, "{e}");
    let e = r.undo(&a, "nope", None, false).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
}

#[test]
fn undo1_undo_never_brings_back_or_removes_landed_work() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", claim(&section, "x")).unwrap();
    let commit = head(&r, "a");
    let p = r.propose(&a, "a", "x").unwrap();
    r.land_report(&approver(), &p.r#ref).unwrap();
    let e = r.undo(&a, "a", Some(&commit), false).unwrap_err();
    assert_eq!(e.code, E_UNDO_LANDING, "{e}");
    assert!(e.fix.contains("revok"), "{e}");
    let e = r.undo(&a, "a", None, false).unwrap_err();
    assert_eq!(
        e.code, E_NOTHING_TO_UNDO,
        "the landed commit is skipped, and a create needs an explicit target: {e}"
    );
    assert_eq!(r.read().included("a").len(), 0, "nothing written");
}

#[test]
fn undo1_close_marks_views_closed_emits_view_closed_and_checks_every_name_first() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", claim(&section, "a")).unwrap();
    write_as(&mut r, &a, "a2", claim(&section, "a2")).unwrap();
    let e = r
        .view_close(&a, &["a".into(), "nope".into()], "done")
        .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    assert!(
        !r.read().view_head("a").unwrap().closed,
        "checks run before any write"
    );
    let rx = r.subscribe();
    let res = r.view_close(&a, &["a".into(), "a2".into()], "").unwrap();
    assert_eq!(
        res.iter()
            .map(|c| (c.view.as_str(), c.reason.as_str()))
            .collect::<Vec<_>>(),
        vec![("a", "closed by hand"), ("a2", "closed by hand")]
    );
    assert!(r.read().view_head("a").unwrap().closed && r.read().view_head("a2").unwrap().closed);
    let mut closed = vec![];
    while let Ok(ev) = rx.try_recv() {
        if let rdy_engine::events::EventKind::ViewClosed { view, reason } = &ev.kind {
            closed.push((view.clone(), reason.clone()));
        }
    }
    assert_eq!(
        closed,
        vec![
            ("a".to_string(), "closed by hand".to_string()),
            ("a2".to_string(), "closed by hand".to_string())
        ]
    );
    let e = r.view_close(&a, &["a".into()], "again").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let u = r.undo(&a, "a", None, false).unwrap();
    assert!(
        u.reopened && u.kind == "close",
        "the close is a's newest own op: {u:?}"
    );
    assert!(!r.read().view_head("a").unwrap().closed);
}

#[test]
fn undo1_an_undo_after_a_redo_takes_the_redone_change_out_again() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "1"))
        .unwrap()
        .change;
    let c2 = write_as(&mut r, &a, "a", claim(&section, "2"))
        .unwrap()
        .change;
    let commit2 = head(&r, "a");
    let u1 = r.undo(&a, "a", None, false).unwrap();
    let redo = r.undo(&a, "a", Some(&u1.op), false).unwrap();
    assert_eq!(r.read().included("a").len(), 2, "c2 is back");
    // an undone undo does not undo its target: u1 is undone by the live redo
    let e = r.undo(&a, "a", Some(&u1.op), false).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains(&redo.op[..12]),
        "names the live redo: {e}"
    );
    // the target-less undo after a redo takes the redone change out, not c1
    let u3 = r.undo(&a, "a", None, false).unwrap();
    assert_eq!(
        (u3.undone.as_str(), u3.kind.as_str(), &u3.excluded),
        (commit2.as_str(), "commit", &vec![c2.clone()]),
        "{u3:?}"
    );
    assert_eq!(r.read().included("a").as_ref(), &vec![c1.parse().unwrap()]);
    let e = r.undo(&a, "a", Some(&commit2), false).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains(&u3.op[..12]) && e.fix.contains(&u3.op[..12]),
        "the refusal names the live undo of c2's commit, not u1: {e}"
    );
    let u4 = r.undo(&a, "a", None, false).unwrap();
    assert_eq!(&u4.excluded, &vec![c1.clone()], "then it walks back to c1");
    check(&r, &view("a")).unwrap();
    r.fsck().unwrap();
}

#[test]
fn undo1_after_a_redo_the_redone_op_can_be_undone_by_name() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "1"))
        .unwrap()
        .change;
    let c2 = write_as(&mut r, &a, "a", claim(&section, "2"))
        .unwrap()
        .change;
    let commit2 = head(&r, "a");
    let u1 = r.undo(&a, "a", None, false).unwrap();
    r.undo(&a, "a", Some(&u1.op), false).unwrap();
    let u = r.undo(&a, "a", Some(&commit2), false).unwrap();
    assert_eq!(
        (u.kind.as_str(), &u.excluded),
        ("commit", &vec![c2.clone()])
    );
    assert_eq!(r.read().included("a").as_ref(), &vec![c1.parse().unwrap()]);
    check(&r, &view("a")).unwrap();
    r.fsck().unwrap();
}

#[test]
fn undo1_an_op_is_undone_only_while_its_newest_undo_is_live() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let c1 = write_as(&mut r, &a, "a", claim(&section, "1"))
        .unwrap()
        .change;
    let c2 = write_as(&mut r, &a, "a", claim(&section, "2"))
        .unwrap()
        .change;
    let u1 = r.undo(&a, "a", None, false).unwrap();
    let u2 = r.undo(&a, "a", Some(&u1.op), false).unwrap();
    let u3 = r.undo(&a, "a", None, false).unwrap();
    assert_eq!(&u3.excluded, &vec![c2.clone()], "{u3:?}");
    // undoing the redo now changes nothing (c2 is already out), and it makes u1 live again
    let u4 = r.undo(&a, "a", Some(&u2.op), false).unwrap();
    assert!(u4.excluded.is_empty() && u4.included.is_empty(), "{u4:?}");
    r.undo(&a, "a", Some(&u3.op), false).unwrap();
    assert_eq!(r.read().included("a").len(), 2, "c2 is back");
    // c2's newest undo (u3) is undone, so c2's commit is live although u1 is live too
    let u = r.undo(&a, "a", None, false).unwrap();
    assert_eq!(&u.excluded, &vec![c2.clone()], "{u:?}");
    assert_eq!(r.read().included("a").as_ref(), &vec![c1.parse().unwrap()]);
    check(&r, &view("a")).unwrap();
    r.fsck().unwrap();
}
