//! insc1: the inflight scope is canon plus the distinct changes of every open view, carried
//! across clean commits; cevt1: the coordination events.
use proptest::prelude::*;
use rdy_engine::*;
use rdy_model::{AltMember, AltSet, AltStatus, BlockId, Intent, IntentStatus};
use rdy_qa::fixtures::*;
use serde_json::json;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}
fn body(r: &Rdy, scope: &Scope, b: &str) -> Option<String> {
    r.read()
        .materialized(scope)
        .unwrap()
        .get(&id(b))
        .map(|x| x.body.clone())
}

#[test]
fn insc1_inflight_holds_every_open_views_changes_once() {
    let (_d, mut r) = project();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &claim);
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "c-AAAAA", "parent": section, "body": "A"}]),
    )
    .unwrap();
    write(
        &mut r,
        "b",
        json!([{"op": "update", "id": claim, "base": v, "body": "B's"}]),
    )
    .unwrap();
    let ca = r.read().included("a")[0].to_string();
    // the same change in two views is one in-flight change
    r.view_include(&writer(), "c", std::slice::from_ref(&ca))
        .unwrap();
    let set = r.read().inflight_set().unwrap();
    assert_eq!(set.changes.len(), 2);
    assert_eq!(
        set.views_of[&ca.parse().unwrap()],
        vec!["a".to_string(), "c".to_string()]
    );
    assert_eq!(body(&r, &Scope::Inflight, "c-AAAAA").as_deref(), Some("A"));
    assert_eq!(body(&r, &Scope::Inflight, &claim).as_deref(), Some("B's"));
    assert_eq!(body(&r, &Scope::Canon, &claim).as_deref(), Some("one"));
    // a closed view leaves the in-flight state
    r.view_close(&writer(), &["b".into()], "done").unwrap();
    assert_eq!(body(&r, &Scope::Inflight, &claim).as_deref(), Some("one"));
    assert_eq!(r.read().inflight_set().unwrap().changes.len(), 1);
}

#[test]
fn insc1_two_views_editing_one_block_read_as_a_conflict_in_flight() {
    let (_d, mut r) = project();
    let (_doc, _section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &claim);
    write(
        &mut r,
        "a",
        json!([{"op": "update", "id": claim, "base": v, "body": "a"}]),
    )
    .unwrap();
    write(
        &mut r,
        "b",
        json!([{"op": "update", "id": claim, "base": v, "body": "b"}]),
    )
    .unwrap();
    let st = r.read();
    assert_eq!(st.conflicts(&Scope::Inflight).unwrap().len(), 1);
    assert_eq!(
        body(&r, &Scope::Inflight, &claim).as_deref(),
        Some("one"),
        "a conflict reads as its base"
    );
}

#[test]
fn insc1_compare_lists_inflight_conflicts_on_either_side() {
    let (_d, mut r) = project();
    let (_doc, _section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &claim);
    for view in ["a", "b"] {
        write(
            &mut r,
            view,
            json!([{"op": "update", "id": claim, "base": v, "body": view}]),
        )
        .unwrap();
    }
    let st = r.read();
    let conflicted = |a: &Scope, b: &Scope| -> Vec<(String, Option<String>)> {
        st.compare(a, b)
            .unwrap()
            .conflicted
            .into_iter()
            .map(|e| (e.id, e.conflict))
            .collect()
    };
    let forward = conflicted(&Scope::Inflight, &Scope::Canon);
    assert_eq!(forward.len(), 1);
    assert_eq!(forward[0].0, claim);
    assert_eq!(
        conflicted(&Scope::Canon, &Scope::Inflight),
        forward,
        "compare lists b's in-flight conflicts too"
    );
    // a view compared with the in-flight state sees the in-flight conflict
    assert_eq!(
        conflicted(&Scope::View("a".into()), &Scope::Inflight),
        forward
    );
}

#[test]
fn insc1_inflight_is_a_reserved_name_and_a_read_only_scope() {
    let (_d, mut r) = project();
    seed_doc(&mut r, "seed");
    assert_eq!(r.read().resolve_scope("inflight").unwrap(), Scope::Inflight);
    let e = write(
        &mut r,
        "inflight",
        json!([{"op": "create", "type": "doc", "fields": {"title": "X", "doc_type": "glossary"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_RESERVED_VIEW);
    let e = r
        .view_create(&writer(), "inflight", None, false)
        .unwrap_err();
    assert_eq!(e.code, E_RESERVED_VIEW);
    let cov = rdy_engine::brief::Coverage {
        ids: vec![],
        path: Some("seed".into()),
    };
    let e = r.read().brief(&Scope::Inflight, &cov, None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(
        e.message.contains("inflight is an analysis scope"),
        "{}",
        e.message
    );
    let out = tempfile::tempdir().unwrap();
    let e = r.read().export(&Scope::Inflight, out.path()).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    // reads work
    let st = r.read();
    st.tree_full(&Scope::Inflight).unwrap();
    st.suspects(&Scope::Inflight).unwrap();
    assert_eq!(
        st.version_position(&Scope::Inflight).unwrap(),
        st.version_position(&Scope::Canon).unwrap()
    );
}

#[test]
fn insc1_a_clean_commit_carries_the_built_inflight_state() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "c-AAAAA", "parent": section, "body": "A"}]),
    )
    .unwrap();
    r.read().graph(&Scope::Inflight).unwrap();
    write(
        &mut r,
        "a",
        json!([{"op": "create", "type": "claim", "id": "c-BBBBB", "parent": section, "body": "B"}]),
    )
    .unwrap();
    assert_eq!(
        r.read().is_built(&Scope::Inflight),
        (true, true),
        "carried, graph included"
    );
    assert_eq!(body(&r, &Scope::Inflight, "c-BBBBB").as_deref(), Some("B"));
}

#[derive(Clone, Debug)]
enum Step {
    Write { view: usize, claim: usize, body: u8 },
    Include { from: usize, into: usize },
    Close { view: usize },
    Land { view: usize },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (0..3usize, 0..3usize, any::<u8>()).prop_map(|(view, claim, body)| Step::Write { view, claim, body }),
        2 => (0..3usize, 0..3usize).prop_map(|(from, into)| Step::Include { from, into }),
        1 => (0..3usize).prop_map(|view| Step::Close { view }),
        1 => (0..3usize).prop_map(|view| Step::Land { view }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases(24)))]
    #[test]
    fn insc1_a_carried_inflight_state_equals_a_rebuilt_one(steps in prop::collection::vec(step(), 1..14)) {
        let (_d, mut r) = project();
        let (_doc, section, _c) = seed_doc(&mut r, "seed");
        let claims: Vec<String> = (0..3).map(|i| format!("c-{i}AAAA")).collect();
        let ops: Vec<serde_json::Value> = claims
            .iter()
            .map(|c| json!({"op": "create", "type": "claim", "parent": section, "id": c, "body": "base"}))
            .collect();
        write(&mut r, "seed", serde_json::Value::Array(ops)).unwrap();
        land(&mut r, "seed").unwrap();
        let views = ["va", "vb", "vc"];
        for s in steps {
            r.read().graph(&Scope::Inflight).unwrap(); // built, so a commit may carry it
            match s {
                Step::Write { view, claim, body } => {
                    let scope = Scope::View(views[view].into());
                    let base = r.read().materialized(&scope).unwrap().version_of(&id(&claims[claim])).map(|h| h.to_string());
                    if let Some(base) = base {
                        let _ = write(&mut r, views[view], json!([{"op": "update", "id": claims[claim], "base": base, "body": format!("{body}")}]));
                    }
                }
                Step::Include { from, into } => {
                    let changes: Vec<String> = r.read().included(views[from]).iter().map(|h| h.to_string()).collect();
                    if !changes.is_empty() && from != into {
                        let _ = r.view_include(&writer(), views[into], &changes);
                    }
                }
                Step::Close { view } => { let _ = r.view_close(&writer(), &[views[view].into()], "x"); }
                Step::Land { view } => { let _ = land(&mut r, views[view]); }
            }
            let carried = {
                let st = r.read();
                (st.materialized(&Scope::Inflight).unwrap().snapshot_id(), st.conflicts(&Scope::Inflight).unwrap().as_ref().clone(), st.inflight_set().unwrap().as_ref().clone())
            };
            r.invalidate();
            let st = r.read();
            prop_assert_eq!(carried.0, st.materialized(&Scope::Inflight).unwrap().snapshot_id());
            let conflicts = st.conflicts(&Scope::Inflight).unwrap();
            prop_assert_eq!(&carried.1, conflicts.as_ref());
            let set = st.inflight_set().unwrap();
            prop_assert_eq!(&carried.2, set.as_ref());
        }
    }
}

#[test]
fn cevt1_coordination_frames_become_events() {
    let (_d, mut r) = project();
    let s = writer();
    r.view_create(&s, "a", None, false).unwrap();
    r.view_create(&s, "b", None, false).unwrap();
    let prov = s.provenance();
    let store = r.store_mut();
    let i = store
        .put_intent(&Intent {
            stable: None,
            prev: None,
            actor: "agent:a".into(),
            run: Some("r".into()),
            view: "a".into(),
            area: None,
            blocks: vec![],
            angle: "x".into(),
            expires_at_ms: 1,
            status: IntentStatus::Open,
            at_ms: 1,
        })
        .unwrap();
    let members = vec![
        AltMember {
            label: "a".into(),
            view: "a".into(),
        },
        AltMember {
            label: "b".into(),
            view: "b".into(),
        },
    ];
    let first = store
        .put_alt_set(&AltSet {
            stable: None,
            prev: None,
            title: "t".into(),
            question: None,
            members: members.clone(),
            status: AltStatus::Open,
            provenance: prov.clone(),
        })
        .unwrap();
    let next = AltSet {
        stable: Some(first),
        prev: Some(first),
        title: "t".into(),
        question: None,
        members,
        status: AltStatus::Abandoned {
            reason: "no".into(),
        },
        provenance: prov.clone(),
    };
    let v2 = store.put_alt_set(&next).unwrap();
    let seq = store.last_batch_end();
    r.invalidate();
    // the closes are separate writes here (Task 2 builds the one-batch abandon, in parallel);
    // derivation only looks at the frames it is given, so the batch is assembled by hand
    let reason = "alternative set abandoned: test";
    r.view_close(&s, &["a".into(), "b".into()], reason).unwrap();
    let st = r.read();
    let mut frames = vec![(rdy_store::kinds::ALT_SET, v2)];
    for v in ["a", "b"] {
        frames.push((rdy_store::kinds::VIEW_OP, st.view_head(v).unwrap().head));
    }
    let evs = rdy_engine::events::events_for_batch(st.reader(), seq, &frames).unwrap();
    let kinds: Vec<serde_json::Value> = evs
        .iter()
        .map(|e| serde_json::to_value(&e.kind).unwrap())
        .collect();
    assert_eq!(kinds[0]["event"], "alt_set_changed");
    assert_eq!(kinds[0]["change"], "abandoned");
    assert_eq!(kinds[0]["members"], json!(["a", "b"]));
    for k in &kinds[1..] {
        assert_eq!(k["event"], "view_closed");
        assert_eq!(k["set"], first.to_string());
        assert_eq!(k["reason"], reason);
    }
    // an intent's first version is posted; the event names it
    let ev =
        rdy_engine::events::events_for_batch(st.reader(), seq, &[(rdy_store::kinds::INTENT, i)])
            .unwrap();
    assert_eq!(
        serde_json::to_value(&ev[0].kind).unwrap()["event"],
        "intent_posted"
    );
    assert!(st.coord_heads().intents.contains_key(&i));
    assert!(st.coord_heads().alt_sets.contains_key(&first));
}

/// Two views that write one block alike leave one clean version in flight, but the merge orders
/// a later write on top of it by deps and hash: a change the writer does not depend on may merge
/// after it and conflict. The carry must then not apply the write on top; the state is rebuilt
/// and equals a rebuild. Each round uses a fresh claim; a carry that ignores the merge order
/// differs from the rebuild in about two rounds of three (each round's views are closed after
/// it, so every round starts from a clean in-flight state).
#[test]
fn insc1_a_write_over_a_version_two_views_wrote_alike_carries_exactly() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let claims: Vec<String> = (0..12).map(|i| format!("c-{i:X}EEEE")).collect();
    let ops: Vec<serde_json::Value> = claims
        .iter()
        .map(|c| json!({"op": "create", "type": "claim", "parent": section, "id": c, "body": "base"}))
        .collect();
    write(&mut r, "seed", serde_json::Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    for (i, c) in claims.iter().enumerate() {
        let (a, b) = (format!("a{i}"), format!("b{i}"));
        let v = version_of(&r, &Scope::Canon, c);
        write(
            &mut r,
            &a,
            json!([{"op": "update", "id": c, "base": v, "body": "alike"}]),
        )
        .unwrap();
        write(
            &mut r,
            &b,
            json!([{"op": "update", "id": c, "base": v, "body": "alike"}]),
        )
        .unwrap();
        r.read().graph(&Scope::Inflight).unwrap(); // built, so the next commit may carry it
        assert!(
            r.read().conflicts(&Scope::Inflight).unwrap().is_empty(),
            "alike writes merge cleanly"
        );
        let alike = version_of(&r, &Scope::Inflight, c);
        write(
            &mut r,
            &a,
            json!([{"op": "update", "id": c, "base": alike, "body": format!("next {i}")}]),
        )
        .unwrap();
        let carried = {
            let st = r.read();
            (
                st.materialized(&Scope::Inflight).unwrap().snapshot_id(),
                st.conflicts(&Scope::Inflight).unwrap().as_ref().clone(),
            )
        };
        r.invalidate();
        let st = r.read();
        assert_eq!(
            carried.0,
            st.materialized(&Scope::Inflight).unwrap().snapshot_id(),
            "round {i}"
        );
        assert_eq!(
            &carried.1,
            st.conflicts(&Scope::Inflight).unwrap().as_ref(),
            "round {i}"
        );
        drop(st);
        // the next round starts from a clean in-flight state
        r.view_close(&writer(), &[a, b], "round done").unwrap();
    }
}
