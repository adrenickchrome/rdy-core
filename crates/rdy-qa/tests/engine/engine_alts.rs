//! alts1: alternative sets of member views; abandon closes them; member writes carry the link.
use rdy_engine::alt::*;
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn m(label: &str, view: &str) -> AltMemberInput {
    AltMemberInput {
        label: label.into(),
        view: view.into(),
    }
}
/// Canon with one claim; views `a` and `b` (owned by agents a and b) each edit it.
fn two_members(r: &mut Rdy) -> String {
    let (_doc, _s, claim) = seed_doc(r, "seed");
    land(r, "seed").unwrap();
    let v = version_of(r, &Scope::Canon, &claim);
    write_as(
        r,
        &agent("agent:a", Some("a")),
        "a",
        json!([{"op": "update", "id": claim, "base": v, "body": "A"}]),
    )
    .unwrap();
    write_as(
        r,
        &agent("agent:b", Some("b")),
        "b",
        json!([{"op": "update", "id": claim, "base": v, "body": "B"}]),
    )
    .unwrap();
    claim
}

#[test]
fn alts1_create_list_add_remove() {
    let (_d, mut r) = project();
    two_members(&mut r);
    r.view_create(&writer(), "c", None, false).unwrap();
    let o = agent("agent:o", Some("o"));
    let s = r
        .alt_create(&o, "Fee model", None, &[m("flat", "a"), m("tiered", "b")])
        .unwrap();
    assert!(s.id.starts_with("as-") && s.id.len() == 15);
    assert_eq!(s.status, "open");
    assert_eq!(
        s.members
            .iter()
            .map(|x| x.view.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(s.members[0].owner.as_ref().unwrap().actor, "agent:a");
    assert_eq!(s.members[0].changes, 1);
    assert_eq!(s.created_by.actor, "agent:o");
    assert_eq!(r.read().alt_sets(false).unwrap(), vec![s.clone()]);
    let s2 = r.alt_add(&o, &s.id, &m("hybrid", "c")).unwrap();
    assert_eq!(s2.members.len(), 3);
    assert_eq!(s2.prev.as_deref(), Some(s.version.as_str()));
    let e = r
        .alt_add(&agent("agent:x", Some("x")), &s.id, &m("x", "c"))
        .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    let s3 = r.alt_remove(&writer(), &s.id, "hybrid").unwrap();
    assert_eq!(s3.members.len(), 2, "a human may change any set");
    assert_eq!(
        r.read().alt_of_view("a").unwrap().unwrap().0.to_string(),
        s.set
    );
    assert!(r.read().alt_of_view("c").unwrap().is_none());
}

#[test]
fn alts1_membership_rules() {
    let (_d, mut r) = project();
    two_members(&mut r);
    let o = agent("agent:o", Some("o"));
    assert_eq!(
        r.alt_create(&o, "T", None, &[m("x", "a")])
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert_eq!(
        r.alt_create(&o, "T", None, &[m("x", "a"), m("x", "b")])
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    let e = r
        .alt_create(&o, "T", None, &[m("x", "a"), m("y", "nope")])
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER);
    assert!(e.message.contains("nope"), "{}", e.message);
    r.alt_create(&o, "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    r.view_create(&writer(), "c", None, false).unwrap();
    let e = r
        .alt_create(&o, "U", None, &[m("x", "a"), m("y", "c")])
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "a view is in at most one open set");
    r.view_close(&writer(), &["c".into()], "x").unwrap();
    r.view_create(&writer(), "d", None, false).unwrap();
    let e = r
        .alt_create(&o, "U", None, &[m("x", "d"), m("y", "c")])
        .unwrap_err();
    assert_eq!(e.code, E_ALT_MEMBER, "closed member");
    // a doc of its own in view q: seed_doc's "seed" is in canon, so its name would clash
    let w = write(&mut r, "q", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "qdoc", "title": "Q", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
    ])).unwrap();
    let s = w.created["$s"].clone();
    let e = r
        .alt_create(&o, "U", Some(&s), &[m("x", "d"), m("y", "q")])
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "the question must be a question block");
}

#[test]
fn alts1_member_writes_carry_the_alternative_link() {
    let (_d, mut r) = project();
    let claim = two_members(&mut r);
    let o = agent("agent:o", Some("o"));
    let s = r
        .alt_create(&o, "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let a = agent("agent:a", Some("a"));
    let v = version_of(&r, &Scope::View("a".into()), &claim);
    let w = write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "update", "id": claim, "base": v, "body": "A2"}]),
    )
    .unwrap();
    let ch = r
        .read()
        .reader()
        .change(&w.change.parse().unwrap())
        .unwrap();
    let link = ch.links.alternative.unwrap();
    assert_eq!(
        (link.set.to_string(), link.member.as_str()),
        (s.set.clone(), "a")
    );
    // a change written before the view joined has no link
    let first = r.read().included("a")[0];
    assert!(
        r.read()
            .reader()
            .change(&first)
            .unwrap()
            .links
            .alternative
            .is_none()
    );
}

#[test]
fn alts1_abandon_closes_every_member_and_compare_shows_each_against_canon() {
    let (_d, mut r) = project();
    two_members(&mut r);
    let o = agent("agent:o", Some("o"));
    let s = r
        .alt_create(&o, "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let cmp = r.read().alt_compare(&s.id).unwrap();
    let v = serde_json::to_value(&cmp).unwrap();
    assert_eq!(v["members"].as_array().unwrap().len(), 2);
    assert_eq!(v["members"][0]["label"], "x");
    assert!(v["members"][0]["compare"].is_object());
    assert_eq!(
        r.alt_abandon(&agent("agent:x", Some("x")), &s.id, "no")
            .unwrap_err()
            .code,
        E_PERMISSION
    );
    assert_eq!(r.alt_abandon(&o, &s.id, "").unwrap_err().code, E_BAD_INPUT);
    let (after, closed) = r.alt_abandon(&o, &s.id, "neither fits").unwrap();
    assert_eq!(closed, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(after.status, "abandoned");
    assert_eq!(after.reason.as_deref(), Some("neither fits"));
    for v in ["a", "b"] {
        assert!(r.read().view_head(v).unwrap().closed);
    }
    assert!(r.read().alt_sets(false).unwrap().is_empty());
    assert_eq!(r.read().alt_sets(true).unwrap().len(), 1);
    assert_eq!(
        r.alt_add(&o, &s.id, &m("z", "seed")).unwrap_err().code,
        E_BAD_INPUT
    );
    // the in-flight state no longer holds the closed members' work
    assert!(r.read().inflight_set().unwrap().changes.is_empty());
}

#[test]
fn alts1_a_remove_that_would_leave_one_member_is_refused() {
    let (_d, mut r) = project();
    two_members(&mut r);
    r.view_create(&writer(), "c", None, false).unwrap();
    let o = agent("agent:o", Some("o"));
    // two members: no remove
    let s = r
        .alt_create(&o, "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let before = r.read().seq();
    let e = r.alt_remove(&o, &s.id, "y").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("one member"), "{}", e.message);
    assert!(e.fix.contains("abandon"), "{}", e.fix);
    assert_eq!(r.read().seq(), before, "a refusal writes nothing");
    assert_eq!(r.read().alt_sets(false).unwrap()[0].members.len(), 2);
    // three members: one remove is allowed, the next is refused
    let s3 = r.alt_add(&o, &s.id, &m("z", "c")).unwrap();
    assert_eq!(s3.members.len(), 3);
    let s2 = r.alt_remove(&o, &s.id, "z").unwrap();
    assert_eq!(
        s2.members
            .iter()
            .map(|x| x.label.as_str())
            .collect::<Vec<_>>(),
        ["x", "y"]
    );
    assert_eq!(r.alt_remove(&o, &s.id, "x").unwrap_err().code, E_BAD_INPUT);
}

#[test]
fn alts1_question_permission_and_lookup_rules() {
    let (_d, mut r) = project();
    let claim = two_members(&mut r);
    let o = agent("agent:o", Some("o"));
    // the role comes first, then the input's shape
    let e = r
        .alt_create(&reader(), "", None, &[m("x", "a")])
        .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    assert_eq!(
        r.alt_create(&o, " ", None, &[m("x", "a"), m("y", "b")])
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert_eq!(
        r.alt_create(&o, "T", None, &[m("", "a"), m("y", "b")])
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert_eq!(
        r.alt_create(&o, "T", None, &[m("x", "a"), m("y", "a")])
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    // a question block in an open view (o has no view of its own: the in-flight state is read)
    let w = write(&mut r, "q", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "qdoc", "title": "Q", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "question", "id": "$q", "parent": "$s", "fields": {"prompt": "Which fee?"}}
    ])).unwrap();
    let e = r
        .alt_create(&o, "T", Some(&claim), &[m("x", "a"), m("y", "b")])
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.message.contains("not a question"), "{}", e.message);
    let e = r
        .alt_create(&o, "T", Some("q-ZZZZZZ"), &[m("x", "a"), m("y", "b")])
        .unwrap_err();
    assert_eq!(e.code, E_UNKNOWN_BLOCK, "{e:?}");
    let s = r
        .alt_create(&o, "T", Some(&w.created["$q"]), &[m("x", "a"), m("y", "b")])
        .unwrap();
    assert_eq!(s.question.as_deref(), Some(w.created["$q"].as_str()));
    // lookup: as-<short>, a 6-hex prefix, the full stable id; anything else is not found
    let rs = r.read();
    for text in [s.id.clone(), s.set[..6].to_string(), s.set.clone()] {
        assert_eq!(
            rs.resolve_alt(&text).unwrap().0.to_string(),
            s.set,
            "{text}"
        );
    }
    for text in ["as-", &s.set[..5], "as-zzzzzz", "nope"] {
        assert_eq!(
            rs.resolve_alt(text).unwrap_err().code,
            E_NOT_FOUND,
            "{text}"
        );
    }
    drop(rs);
    // a reader changes nothing; an unknown member is not found
    assert_eq!(
        r.alt_add(&reader(), &s.id, &m("z", "q")).unwrap_err().code,
        E_PERMISSION
    );
    assert_eq!(
        r.alt_remove(&o, &s.id, "nobody").unwrap_err().code,
        E_NOT_FOUND
    );
    // the creator's actor on another run may change the set; a member by label or view
    let o2 = agent("agent:o", Some("o2"));
    let s2 = r.alt_add(&o2, &s.id, &m("z", "q")).unwrap();
    assert_eq!(s2.created_by.actor, "agent:o");
    assert_eq!(s2.changed_by.run.as_deref(), Some("o2"));
    let s3 = r.alt_remove(&o2, &s.id, "q").unwrap();
    assert_eq!(s3.members.len(), 2);
}

/// Fix round 1: the open-set index (view to set) that member writes read follows every publish
/// that moves a set: create, add, remove, abandon, a landing and a full re-read.
#[test]
fn alts1_the_open_set_index_follows_every_set_change() {
    let (_d, mut r) = project();
    two_members(&mut r);
    r.view_create(&writer(), "c", None, false).unwrap();
    let o = agent("agent:o", Some("o"));
    let members = |r: &Rdy| -> Vec<(String, String)> {
        r.read()
            .coord_heads()
            .alt_members
            .iter()
            .map(|(v, s)| (v.clone(), s.to_string()))
            .collect()
    };
    // sets that were abandoned are in the heads but not in the index
    for i in 0..5 {
        r.view_create(&writer(), &format!("p{i}"), None, false)
            .unwrap();
        r.view_create(&writer(), &format!("q{i}"), None, false)
            .unwrap();
        let s = r
            .alt_create(
                &o,
                "old",
                None,
                &[m("p", &format!("p{i}")), m("q", &format!("q{i}"))],
            )
            .unwrap();
        r.alt_abandon(&o, &s.id, "no").unwrap();
    }
    assert!(members(&r).is_empty());
    assert!(r.read().coord_heads().open_alts.is_empty());
    assert_eq!(r.read().coord_heads().alt_sets.len(), 5);
    let s = r
        .alt_create(&o, "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let set = |views: &[&str]| -> Vec<(String, String)> {
        views
            .iter()
            .map(|v| (v.to_string(), s.set.clone()))
            .collect()
    };
    assert_eq!(members(&r), set(&["a", "b"]));
    let open = r.read().coord_heads().open_alts.clone();
    assert_eq!(open.len(), 1);
    let (version, latest) = open.values().next().unwrap();
    assert_eq!(version.to_string(), s.version);
    assert_eq!(latest.title, "T");
    r.alt_add(&o, &s.id, &m("z", "c")).unwrap();
    assert_eq!(members(&r), set(&["a", "b", "c"]));
    let after_remove = r.alt_remove(&o, &s.id, "z").unwrap();
    assert_eq!(members(&r), set(&["a", "b"]));
    assert_eq!(
        r.read()
            .coord_heads()
            .open_alts
            .values()
            .next()
            .unwrap()
            .0
            .to_string(),
        after_remove.version
    );
    // a full re-read and a landing rebuild the same index
    r.invalidate();
    assert_eq!(members(&r), set(&["a", "b"]));
    let claim = seed_claim(&r);
    let v = version_of(&r, &Scope::Canon, &claim);
    write(
        &mut r,
        "seed",
        json!([{"op": "update", "id": claim, "base": v, "body": "two"}]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    assert_eq!(members(&r), set(&["a", "b"]));
    assert!(r.read().alt_of_view("a").unwrap().is_some());
    r.alt_abandon(&o, &s.id, "neither").unwrap();
    assert!(members(&r).is_empty());
    assert!(r.read().alt_of_view("a").unwrap().is_none());
    assert_eq!(r.read().coord_heads().alt_sets.len(), 6);
}

/// The one claim of canon's seed doc.
fn seed_claim(r: &Rdy) -> String {
    let st = r.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    m.iter()
        .find(|b| b.block_type == "claim")
        .map(|b| b.id.to_string())
        .expect("a claim in canon")
}

/// Fix round 1: an agent never enrols the human's `work` view in a set (create or add), and
/// never abandons a set that holds it (abandon would close `work`). A human may do all three.
#[test]
fn alts1_an_agent_never_enrols_or_closes_the_humans_work_view() {
    let (_d, mut r) = project();
    let claim = two_members(&mut r);
    let v = version_of(&r, &Scope::Canon, &claim);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": claim, "base": v, "body": "H"}]),
    )
    .unwrap();
    let o = agent("agent:o", Some("o"));
    let before = r.read().seq();
    let e = r
        .alt_create(&o, "T", None, &[m("h", "work"), m("b", "b")])
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e:?}");
    assert!(e.message.contains("work"), "{}", e.message);
    assert_eq!(r.read().seq(), before, "a refusal writes nothing");
    let s = r
        .alt_create(&o, "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let before = r.read().seq();
    let e = r.alt_add(&o, &s.id, &m("h", "work")).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e:?}");
    assert_eq!(r.read().seq(), before, "a refusal writes nothing");
    assert!(r.read().alt_of_view("work").unwrap().is_none());
    // a human may enrol work in the agent's set; the agent then may not abandon it
    let s2 = r.alt_add(&writer(), &s.id, &m("h", "work")).unwrap();
    assert_eq!(s2.members.len(), 3);
    let e = r.alt_abandon(&o, &s.id, "no").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e:?}");
    assert!(!r.read().view_head("work").unwrap().closed);
    // the agent may still remove work from its set
    r.alt_remove(&o, &s.id, "h").unwrap();
    assert!(r.read().alt_of_view("work").unwrap().is_none());
    // a human's create that names work is accepted, and the human's writes there carry the link
    let h = r
        .alt_create(&writer(), "H", None, &[m("h", "work"), m("c", "c0")])
        .unwrap_err();
    assert_eq!(h.code, E_ALT_MEMBER, "c0 is not a view yet");
    r.view_create(&writer(), "c0", None, false).unwrap();
    let hs = r
        .alt_create(&writer(), "H", None, &[m("h", "work"), m("c", "c0")])
        .unwrap();
    assert_eq!(hs.members[0].view, "work");
    let v = version_of(&r, &Scope::View("work".into()), &claim);
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": claim, "base": v, "body": "H2"}]),
    )
    .unwrap();
    let ch = r
        .read()
        .reader()
        .change(&w.change.parse().unwrap())
        .unwrap();
    assert_eq!(ch.links.alternative.unwrap().member, "work");
    let (_, closed) = r.alt_abandon(&writer(), &hs.id, "neither").unwrap();
    assert_eq!(closed, vec!["work".to_string(), "c0".to_string()]);
}

#[test]
fn talt1_alternative_compare_lists_each_members_tensions() {
    let (_d, mut r) = project();
    let claim = two_members(&mut r);
    let v = version_of(&r, &Scope::Canon, &claim);
    let o = agent("agent:o", Some("o"));
    let set = r
        .alt_create(&o, "T", None, &[m("x", "a"), m("y", "b")])
        .unwrap();
    let kind = rdy_engine::tensions::TensionKind::EditCollision;
    // members of one set never collide with each other (FW-CH6a)
    let cmp = r.read().alt_compare(&set.id).unwrap();
    for row in &cmp.members {
        assert!(
            row.tensions.iter().all(|t| t.kind != kind),
            "{}: {:?}",
            row.view,
            row.tensions
        );
    }
    // a view outside the set changes the claim in canon: each member's write collides with canon
    write_as(
        &mut r,
        &agent("agent:c", Some("c")),
        "c",
        json!([{"op": "update", "id": claim, "base": v, "body": "C"}]),
    )
    .unwrap();
    land(&mut r, "c").unwrap();
    let cmp = r.read().alt_compare(&set.id).unwrap();
    for row in &cmp.members {
        assert_eq!(row.tensions.len(), 1, "{}: {:?}", row.view, row.tensions);
        let t = &row.tensions[0];
        assert_eq!(t.kind, kind);
        assert!(t.blocks.contains(&claim));
        assert!(
            t.sides.iter().any(|x| x.landing.is_some()),
            "a canon side: {t:?}"
        );
        let changes: Vec<_> = t.sides.iter().filter(|x| x.change.is_some()).collect();
        assert_eq!(changes.len(), 1, "{t:?}");
        assert!(
            changes[0].views.contains(&row.view),
            "the member's own change: {t:?}"
        );
    }
}
