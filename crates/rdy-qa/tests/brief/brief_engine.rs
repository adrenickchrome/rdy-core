//! brf1: a briefing's coverage, token, sections and cut, and the registry.
use rdy_engine::brief::*;
use rdy_engine::engine::{Engine, Local};
use rdy_engine::*;
use rdy_model::{BlockId, Hash};
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn w() -> Scope {
    Scope::View("w".into())
}

fn subtree(r: &Rdy, scope: &Scope, root: &BlockId) -> BTreeSet<BlockId> {
    let m = r.read().materialized(scope).unwrap();
    let mut out = BTreeSet::from([root.clone()]);
    let mut todo = vec![root.clone()];
    while let Some(id) = todo.pop() {
        for c in m.children(Some(&id)) {
            out.insert(c.id.clone());
            todo.push(c.id.clone());
        }
    }
    out
}

#[test]
fn brf1_the_token_changes_when_and_only_when_a_covered_or_dependency_block_changes() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let t0 = brief_of(&r, &Scope::Canon, &[&s.b]).base.token;
    assert_eq!(
        brief_of(&r, &Scope::Canon, &[&s.b]).base.token,
        t0,
        "deterministic"
    );
    assert_ne!(
        brief_of(&r, &Scope::Canon, &[&s.a, &s.b]).base.token,
        t0,
        "covering A is not the same as depending on it"
    );
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.c.to_string(), "body": "C changed."}]),
    )
    .unwrap();
    assert_eq!(
        brief_of(&r, &w(), &[&s.b]).base.token,
        t0,
        "C is neither covered nor a dependency"
    );
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A changed."}]),
    )
    .unwrap();
    let t1 = brief_of(&r, &w(), &[&s.b]).base.token;
    assert_ne!(t1, t0, "A, a dependency of B, moved");
    let body = format!("B builds on {{{{{}}}}} again.", s.a);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.b.to_string(), "body": body}]),
    )
    .unwrap();
    assert_ne!(brief_of(&r, &w(), &[&s.b]).base.token, t1, "B itself moved");
}

#[test]
fn brf1_a_doc_path_covers_the_doc_and_a_folder_path_every_doc_under_it() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let doc = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![],
                path: Some("spec".into()),
            },
            None,
        )
        .unwrap();
    let want = subtree(&r, &Scope::Canon, &s.doc);
    assert_eq!(doc.covers, want.len());
    assert_eq!(doc.covers, 6, "doc, section, A, B, C and D");
    assert!(want.iter().all(|id| doc.record.covers(id)));
    r.new_folder(&writer(), "w", "area").unwrap();
    r.new_doc(&writer(), "w", "area/inner", None, None).unwrap();
    let folder_id = r.read().resolve_id(&w(), "area").unwrap();
    let folder = r
        .read()
        .brief(
            &w(),
            &Coverage {
                ids: vec![],
                path: Some("area".into()),
            },
            None,
        )
        .unwrap();
    let want = subtree(&r, &w(), &folder_id);
    assert!(want.len() >= 2, "the folder and its doc at least");
    assert_eq!(folder.covers, want.len());
    assert!(want.iter().all(|id| folder.record.covers(id)));
}

#[test]
fn brf1_ids_inside_the_path_subtree_still_cover_the_whole_subtree() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let path_only = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![],
                path: Some("spec".into()),
            },
            None,
        )
        .unwrap();
    for given in [&s.section, &s.doc, &s.a] {
        let both = r
            .read()
            .brief(
                &Scope::Canon,
                &Coverage {
                    ids: vec![given.to_string()],
                    path: Some("spec".into()),
                },
                None,
            )
            .unwrap();
        let want = subtree(&r, &Scope::Canon, &s.doc);
        assert_eq!(both.covers, want.len(), "ids {given} inside the path");
        assert_eq!(both.record.covers, want, "subtree ∪ ids");
        assert!(want.iter().all(|id| both.record.covers(id)));
        assert_eq!(both.base.token, path_only.base.token);
    }
    r.new_folder(&writer(), "w", "area").unwrap();
    r.new_doc(&writer(), "w", "area/inner", None, None).unwrap();
    let outside = r
        .read()
        .brief(
            &w(),
            &Coverage {
                ids: vec![s.section.to_string()],
                path: Some("area".into()),
            },
            None,
        )
        .unwrap();
    let folder_id = r.read().resolve_id(&w(), "area").unwrap();
    let mut want = subtree(&r, &w(), &folder_id);
    want.insert(s.section.clone());
    assert_eq!(
        outside.record.covers, want,
        "an id outside the path is added"
    );
}

#[test]
fn brf1_a_briefing_lists_governs_dependents_will_go_stale_and_suspects() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let a = brief_of(&r, &Scope::Canon, &[&s.a]);
    let (sa, sb, sd) = (s.a.to_string(), s.b.to_string(), s.decision.to_string());
    assert!(
        a.governs
            .iter()
            .any(|g| g.decision == sd && g.status == "live" && g.blocks == vec![sa.clone()]),
        "{:?}",
        a.governs
    );
    assert!(
        a.dependents
            .iter()
            .any(|d| d.block == sb && d.kind == "references" && d.on == sa),
        "{:?}",
        a.dependents
    );
    assert!(
        a.will_go_stale.iter().any(|x| x.block == sb
            && x.via == "references"
            && x.on == sa
            && x.follow_up == FollowUp::Confirm
            && x.states == Some(vec![])),
        "{:?}",
        a.will_go_stale
    );
    assert!(a.suspects.is_empty());
    assert!(
        a.record.recorded.contains_key(&s.decision),
        "A depends on D through decided_by"
    );
    let m = r.read().materialized(&Scope::Canon).unwrap();
    assert_eq!(
        a.base.blocks,
        vec![BaseBlock {
            id: sa.clone(),
            version: m.version_of(&s.a).unwrap().to_string()
        }]
    );
    assert_eq!(a.base.token, a.record.token_hex());
    assert_eq!(a.base.depends, 1);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": sa.clone(), "body": "A changed."}]),
    )
    .unwrap();
    let b = brief_of(&r, &w(), &[&s.b]);
    assert!(
        b.suspects
            .iter()
            .any(|x| x.block == s.b && x.target.as_ref() == Some(&s.a) && x.reason == "changed"),
        "{:?}",
        b.suspects
    );
    // rf6 (D-VPATZ): a proposed decision that affects A governs it too, labelled proposed
    let p = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.section.to_string(),
                statement: "A stays short".into(),
                affects: vec![sa.clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let a = brief_of(&r, &w(), &[&s.a]);
    assert!(
        a.governs
            .iter()
            .any(|g| g.decision == p && g.status == "proposed" && g.blocks == vec![sa.clone()]),
        "{:?}",
        a.governs
    );
    assert!(
        a.governs
            .iter()
            .any(|g| g.decision == sd && g.status == "live"),
        "{:?}",
        a.governs
    );
}

#[test]
fn brf1_a_max_tokens_cut_drops_detail_and_never_coverage() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ops: Vec<_> = (0..200)
        .map(|i| {
            json!({"op": "create", "type": "claim", "parent": s.section.to_string(),
                        "body": format!("Claim {i} leans on {{{{{}}}}}.", s.a)})
        })
        .collect();
    write(&mut r, "w", json!(ops)).unwrap();
    let a = r
        .read()
        .brief(
            &w(),
            &Coverage {
                ids: vec![s.a.to_string()],
                path: None,
            },
            Some(800),
        )
        .unwrap();
    assert!(
        a.truncated && a.omitted.get("dependents").copied().unwrap_or(0) > 0,
        "{:?}",
        a.omitted
    );
    assert!(serde_json::to_string(&a).unwrap().len() / 4 <= 800);
    assert!(a.record.covers(&s.a));
    let doc = r
        .read()
        .brief(
            &w(),
            &Coverage {
                ids: vec![],
                path: Some("spec".into()),
            },
            Some(200),
        )
        .unwrap();
    assert_eq!(doc.covers, 206);
    assert!(
        doc.base.blocks.len() < 206 && doc.omitted["base.blocks"] > 0,
        "{:?}",
        doc.omitted
    );
    assert_eq!(
        doc.record.covers.len(),
        206,
        "the token covers every requested block"
    );
    assert_eq!(doc.base.token, doc.record.token_hex());
}

#[test]
fn brf1_the_registry_keeps_the_most_recently_used_briefings() {
    assert_eq!(BRIEF_REGISTRY_CAP, 10_000);
    let rec = |n: u8| {
        Arc::new(BriefRecord {
            token: Hash::of(&[n]),
            covers: BTreeSet::new(),
            recorded: BTreeMap::new(),
            governance: Governance::default(),
        })
    };
    let hex = |n: u8| rec(n).token.to_string();
    let reg = BriefRegistry::new(3);
    for n in 0..3 {
        reg.insert(rec(n));
    }
    assert!(reg.get(&hex(0)).is_some(), "a read refreshes 0");
    reg.insert(rec(3));
    assert!(reg.get(&hex(1)).is_none(), "1 was the least recently used");
    assert!(reg.get(&hex(0)).is_some() && reg.get(&hex(2)).is_some() && reg.get(&hex(3)).is_some());
    assert_eq!(reg.len(), 3);
    assert!(reg.get("not a token").is_none());
    let (_d, r) = project();
    assert_eq!(
        r.briefs().len(),
        0,
        "a fresh open starts with an empty registry"
    );
    let local = Local::new(r);
    assert!(
        Arc::ptr_eq(&local.briefs(), &local.briefs()),
        "one registry per open project"
    );
}

#[test]
fn brf1_unknown_ids_and_empty_coverage_are_refused() {
    let (_d, mut r) = project();
    spec_doc(&mut r);
    let e = r
        .read()
        .brief(&Scope::Canon, &Coverage::default(), None)
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    let ghost = BlockId::new("c").to_string();
    let e = r
        .read()
        .brief(
            &Scope::Canon,
            &Coverage {
                ids: vec![ghost],
                path: None,
            },
            None,
        )
        .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK", "{e}");
}

/// Doc `big` in view `work`: the doc, a section and `claims` claims; plus doc `other`. Returns
/// `other`'s id.
fn big_doc(r: &mut Rdy, claims: usize) -> BlockId {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "big", "title": "Big", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "All"}}),
        json!({"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}}),
    ];
    ops.extend((0..claims).map(|i| {
        json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("Claim {i}.")})
    }));
    let w = write(r, "work", json!(ops)).expect("the big doc");
    BlockId::parse(&w.created["$o"]).expect("created id")
}

#[test]
fn brf1_coverage_over_the_limit_is_refused_without_walking_the_rest() {
    assert_eq!(MAX_BRIEF_BLOCKS, 5_000);
    let (_d, mut r) = project();
    let work = Scope::View("work".into());
    // the doc, its section and 4,998 claims: exactly the limit
    let other = big_doc(&mut r, MAX_BRIEF_BLOCKS - 2);
    let cov = |ids: Vec<String>| Coverage {
        ids,
        path: Some("big".into()),
    };
    let at_limit = r.read().brief(&work, &cov(vec![]), None).unwrap();
    assert_eq!(at_limit.covers, MAX_BRIEF_BLOCKS);
    let doc = r.read().resolve_id(&work, "big").unwrap();
    let overlap = r
        .read()
        .brief(&work, &cov(vec![doc.to_string()]), None)
        .unwrap();
    assert_eq!(
        overlap.covers, MAX_BRIEF_BLOCKS,
        "an id inside the path is counted once"
    );
    let e = r
        .read()
        .brief(&work, &cov(vec![other.to_string()]), None)
        .unwrap_err();
    assert_eq!(e.code, "E_LIMIT", "{e}");
    assert_eq!(
        e.message,
        format!("more than {MAX_BRIEF_BLOCKS} blocks to brief; the limit is {MAX_BRIEF_BLOCKS}"),
        "the walk stops at the first block over the limit, so the total is not known"
    );
    let section = {
        let m = r.read().materialized(&work).unwrap();
        m.children(Some(&doc))[0].id.clone()
    };
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section.to_string(), "body": "One more."}]),
    )
    .unwrap();
    let e = r.read().brief(&work, &cov(vec![]), None).unwrap_err();
    assert_eq!(e.code, "E_LIMIT", "{e}");
    assert!(e.message.starts_with("more than 5000 blocks"), "{e}");
}

#[test]
fn brf1_the_registry_is_bounded_by_recorded_entries_as_well_as_by_count() {
    assert_eq!(BRIEF_REGISTRY_ENTRIES, 1_000_000);
    // a record of `n` with `size` covered blocks, each also recorded: 2 × size entries
    let rec = |n: u8, size: usize| {
        let covers: BTreeSet<BlockId> = (0..size).map(|_| BlockId::new("c")).collect();
        let recorded = covers.iter().map(|id| (id.clone(), None)).collect();
        Arc::new(BriefRecord {
            token: Hash::of(&[n]),
            covers,
            recorded,
            governance: Governance::default(),
        })
    };
    let hex = |n: u8| Hash::of(&[n]).to_string();
    let reg = BriefRegistry::with_entry_budget(100, 10);
    reg.insert(rec(0, 2));
    reg.insert(rec(1, 2));
    assert_eq!((reg.len(), reg.entries()), (2, 8));
    reg.insert(rec(1, 2));
    assert_eq!(
        (reg.len(), reg.entries()),
        (2, 8),
        "a refreshed token is counted once"
    );
    reg.insert(rec(2, 2));
    assert!(
        reg.get(&hex(0)).is_none(),
        "12 entries are over the budget of 10: the least recently used goes"
    );
    assert!(reg.get(&hex(1)).is_some() && reg.get(&hex(2)).is_some());
    assert_eq!((reg.len(), reg.entries()), (2, 8));
    reg.insert(rec(3, 20));
    assert_eq!(
        (reg.len(), reg.entries()),
        (1, 40),
        "a record over the budget on its own is kept, and only it"
    );
    assert!(reg.get(&hex(3)).is_some());
}

#[test]
fn brf1_a_derived_status_tag_is_its_serde_tag() {
    use rdy_graph::DerivedStatus;
    let by = vec![BlockId::new("d")];
    for s in [
        DerivedStatus::Proposed,
        DerivedStatus::Rejected,
        DerivedStatus::Live,
        DerivedStatus::PartiallySuperseded {
            by: by.clone(),
            parts: vec!["x".into()],
        },
        DerivedStatus::Superseded { by: by.clone() },
        DerivedStatus::Revoked { by },
    ] {
        assert_eq!(
            serde_json::to_value(&s).unwrap()["status"],
            s.tag(),
            "{s:?}"
        );
    }
}
