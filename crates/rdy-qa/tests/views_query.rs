//! vls1: list and show; cmp1: block-level compare.
use rdy_engine::*;
use rdy_qa::fixtures::*;
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

#[test]
fn vls1_list_shows_owners_bases_counts_conflicts_and_idle_views() {
    let (_d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    let l: rdy_model::Hash = land(&mut r, "seed").unwrap().parse().unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", update(&c, &v, "a's")).unwrap();
    write_as(&mut r, &a, "a", claim(&section, "y")).unwrap();
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap()
    .change;
    r.view_include(&a, "a", &[cb]).unwrap();
    r.view_create(&a, "pin", None, true).unwrap();
    r.store_mut()
        .view_append(
            "b",
            vec![rdy_store::view::ViewOpKind::Close { reason: "x".into() }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    let now = now_ms();
    let st = r.read();
    let list = st.view_list(false, 14, now).unwrap();
    assert_eq!(
        list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "pin", "seed"]
    );
    let va = &list[0];
    assert_eq!(
        (
            va.changes,
            va.blocks,
            va.conflicts,
            va.pinned,
            va.closed,
            va.idle
        ),
        (3, 2, 1, false, false, false)
    );
    assert_eq!(va.owner.as_ref().unwrap().actor, "agent:a");
    assert_eq!(va.base.as_deref(), Some(l.to_string().as_str()));
    let seed = &list[2];
    assert_eq!((seed.changes, seed.blocks, seed.conflicts), (0, 0, 0));
    assert_eq!(
        seed.effective_base.as_deref(),
        Some(l.to_string().as_str()),
        "an emptied view floats"
    );
    let all = st.view_list(true, 14, now).unwrap();
    assert_eq!(
        all.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "pin", "seed"]
    );
    assert!(all[1].closed && all[1].conflicts == 0);
    let idle = st.view_list(true, 0, now + 86_400_000).unwrap();
    assert!(
        idle.iter().filter(|s| !s.closed).all(|s| s.idle),
        "{idle:?}"
    );
    assert!(!idle[1].idle, "a closed view is never idle");
    assert_eq!((r.idle_days(), r.read().idle_days()), (14, 14));
}

#[test]
fn vls1_show_lists_changes_with_their_blocks_the_conflicts_and_the_chain_since_the_checkpoint() {
    let (_d, mut r) = project();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    let ca = write_as(&mut r, &a, "a", update(&c, &v, "a's"))
        .unwrap()
        .change;
    let cy = write_as(&mut r, &a, "a", claim(&section, "y"))
        .unwrap()
        .change;
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap()
    .change;
    r.view_include(&a, "a", std::slice::from_ref(&cb)).unwrap();
    // the undo goes through the store: Rdy::undo is Task 11's, built beside this task
    let include = r.read().view_head("a").unwrap().head;
    r.store_mut()
        .view_append(
            "a",
            vec![rdy_store::view::ViewOpKind::Undo { target: include }],
            &a.provenance(),
        )
        .unwrap();
    r.invalidate();
    let d = r.read().view_show("a").unwrap();
    assert_eq!(d.head.view, "a");
    assert_eq!(
        d.changes.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
        vec![ca.as_str(), cy.as_str()]
    );
    assert_eq!(d.changes[0].blocks, vec![c.clone()]);
    assert_eq!(d.changes[0].actor, "agent:a");
    assert_eq!(d.blocks, 2);
    assert!(d.conflicts.is_empty());
    let kinds: Vec<(&str, bool)> = d.ops.iter().map(|o| (o.kind.as_str(), o.undone)).collect();
    assert_eq!(
        kinds,
        vec![
            ("undo", false),
            ("include", true),
            ("commit", false),
            ("commit", false),
            ("create", false)
        ]
    );
    let e = r.read().view_show("nope").unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
}

#[test]
fn cmp1_compare_lists_added_deleted_changed_moved_and_conflicted_blocks() {
    let (_d, mut r) = project();
    let (doc, section, c) = seed_doc(&mut r, "seed");
    let s2 = write(
        &mut r,
        "seed",
        json!([{"op": "create", "type": "section", "parent": doc, "fields": {"title": "T"}}]),
    )
    .unwrap()
    .created
    .values()
    .next()
    .unwrap()
    .clone();
    let gone = write(&mut r, "seed", claim(&section, "gone"))
        .unwrap()
        .created
        .values()
        .next()
        .unwrap()
        .clone();
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", update(&c, &v, "edited")).unwrap();
    let added = write_as(&mut r, &a, "a", claim(&section, "new"))
        .unwrap()
        .created
        .values()
        .next()
        .unwrap()
        .clone();
    let vg = version_of(&r, &Scope::Canon, &gone);
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "delete", "id": gone, "base": vg}]),
    )
    .unwrap();
    let vs = version_of(&r, &Scope::Canon, &s2);
    // a retitle and a move: `OpInput` moves with its own `move` op (`update` has no `parent`), and
    // a write may change a block once, so they are two writes
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": s2, "base": vs, "fields": {"title": "T2"}}]),
    )
    .unwrap();
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "move", "id": s2, "parent": section}]),
    )
    .unwrap();
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        update(&c, &v, "b's"),
    )
    .unwrap()
    .change;
    r.view_include(&a, "a", &[cb]).unwrap();
    let cmp = r.read().compare(&view("a"), &Scope::Canon).unwrap();
    assert_eq!((cmp.a.as_str(), cmp.b.as_str()), ("a", "canon"));
    assert_eq!(
        cmp.added.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec![added.as_str()]
    );
    assert_eq!(
        cmp.deleted
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec![gone.as_str()]
    );
    let moved = cmp.moved.iter().find(|e| e.id == s2).expect("moved");
    assert_eq!(
        (moved.from_parent.as_deref(), moved.to_parent.as_deref()),
        (Some(doc.as_str()), Some(section.as_str()))
    );
    let changed_s2 = cmp
        .changed
        .iter()
        .find(|e| e.id == s2)
        .expect("retitled too");
    assert!(
        changed_s2.fields.contains(&"title".to_string())
            && !changed_s2.fields.contains(&"body".to_string()),
        "{changed_s2:?}"
    );
    assert!(
        !cmp.changed.iter().any(|e| e.id == c),
        "the conflicted claim reads as canon's version in a: unchanged"
    );
    assert_eq!(
        cmp.conflicted
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec![c.as_str()]
    );
    assert!(cmp.conflicted[0].conflict.is_some());
    let docs: Vec<&CompareEntry> = cmp
        .added
        .iter()
        .chain(&cmp.deleted)
        .chain(&cmp.changed)
        .filter(|e| e.path.is_some())
        .collect();
    assert!(docs.is_empty(), "claims and sections carry no path");
    let same = r.read().compare(&Scope::Canon, &Scope::Canon).unwrap();
    assert!(
        same.added.is_empty()
            && same.deleted.is_empty()
            && same.changed.is_empty()
            && same.moved.is_empty()
            && same.conflicted.is_empty()
    );
    let ab = r.read().compare(&view("a"), &view("b")).unwrap();
    assert_eq!(ab.conflicted.len(), 1);
}

#[test]
fn vls1_idle_days_is_read_from_the_config_on_open_and_survives_every_publish() {
    let (d, r) = project();
    drop(r);
    let cfg = d.path().join(".rdy/config.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, format!("{text}views:\n  idle_days: 3\n")).unwrap();
    let mut r = Rdy::open(d.path()).unwrap();
    assert_eq!(r.idle_days(), 3);
    seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    assert_eq!(r.idle_days(), 3, "a commit and a landing carry it over");
    r.invalidate();
    assert_eq!(r.read().idle_days(), 3);
}
