//! pshow1: a proposal version reads as its title, proposer, changes with block counts, the
//! conflicts and other reasons that block approving it now, and its version chain.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}

fn stable_of(p: &ProposalInfo) -> String {
    p.r#ref.split('@').next().unwrap().to_string()
}

/// A landed seed doc, then two versions of one proposal from view "work": @1 with one claim,
/// @2 with two.
fn two_versions() -> (tempfile::TempDir, Rdy, ProposalInfo, ProposalInfo) {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p1 = r.propose(&writer(), "work", "first", None).unwrap();
    write(&mut r, "work", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "work", "second", None).unwrap();
    (d, r, p1, p2)
}

#[test]
fn pshow1_a_version_shows_its_changes_blocks_and_version_chain() {
    let (_d, r, p1, p2) = two_versions();
    let s = r.read().proposal_show(&p2.r#ref).unwrap();
    assert_eq!(
        (s.r#ref.as_str(), s.version, s.title.as_str()),
        (p2.r#ref.as_str(), 2, "second")
    );
    assert_eq!(
        (s.id.as_str(), s.stable.as_str()),
        (p2.id.as_str(), p1.id.as_str())
    );
    assert_eq!((s.state.as_str(), s.landing.as_deref()), ("open", None));
    assert_eq!((s.changes.len(), s.blocks), (2, 2));
    assert!(
        s.changes
            .iter()
            .all(|c| c.blocks == 1 && c.change.len() == 64),
        "{:?}",
        s.changes
    );
    assert_eq!(s.view, "work");
    let p = r.read().reader().proposal(&p2.id.parse().unwrap()).unwrap();
    assert_eq!(s.proposed_by.actor, p.provenance.actor);
    assert!(s.conflicts.is_empty() && s.blocking.is_empty(), "{s:?}");
    assert_eq!(s.summary, None);
    let chain: Vec<(&str, bool, bool)> = s
        .versions
        .iter()
        .map(|v| (v.r#ref.as_str(), v.latest, v.landed))
        .collect();
    assert_eq!(
        chain,
        vec![
            (p1.r#ref.as_str(), false, false),
            (p2.r#ref.as_str(), true, false)
        ]
    );
    let latest = r.read().proposal_show(&stable_of(&p1)).unwrap();
    assert_eq!(
        (latest.version, latest.id),
        (2, p2.id.clone()),
        "P-<id> is the latest version"
    );
}

#[test]
fn pshow1_an_old_version_is_superseded_and_a_landed_one_names_its_landing() {
    let (_d, mut r, p1, p2) = two_versions();
    let old = r.read().proposal_show(&p1.r#ref).unwrap();
    assert_eq!((old.state.as_str(), old.changes.len()), ("superseded", 1));
    assert_eq!(old.blocking, vec![format!("superseded by {}", p2.r#ref)]);
    assert!(old.conflicts.is_empty());
    let landing = r.land(&approver(), &p2.id).unwrap();
    let s = r.read().proposal_show(&p2.r#ref).unwrap();
    assert_eq!(
        (s.state.as_str(), s.landing.as_deref()),
        ("landed", Some(landing.as_str()))
    );
    assert_eq!(s.blocking, vec![format!("landed in {}", &landing[..12])]);
    // @2 carried @1's change too, so @1 has landed as well, as approving it now would say
    // (E_ALREADY_LANDED)
    assert!(
        s.versions[1].landed && s.versions[0].landed,
        "{:?}",
        s.versions
    );
    let old = r.read().proposal_show(&p1.r#ref).unwrap();
    assert_eq!(
        (old.state.as_str(), old.landing.as_deref()),
        ("landed", Some(landing.as_str()))
    );
    assert_eq!(
        r.land_report(&approver(), &p1.r#ref).unwrap_err().code,
        E_ALREADY_LANDED
    );
}

#[test]
fn pshow1_a_conflicted_version_lists_its_blocking_conflicts() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    let sb = as_agent("agent:b", "r1");
    write_as(&mut r, &sa, "a", update(&c, &v, "a's")).unwrap();
    write_as(&mut r, &sb, "b", update(&c, &v, "b's")).unwrap();
    let pa = r.propose(&sa, "a", "a", None).unwrap();
    r.propose(&sb, "b", "b", None).unwrap();
    r.land_report(&approver(), &pa.r#ref).unwrap();
    let pb2 = r.propose(&sb, "b", "rebased", None).unwrap();
    assert_eq!(
        pb2.blocking.len(),
        1,
        "the fixture makes one blocking conflict"
    );
    let s = r.read().proposal_show(&pb2.r#ref).unwrap();
    assert_eq!(s.state, "open");
    assert_eq!(s.conflicts.len(), 1);
    assert_eq!(s.conflicts[0].block, c);
    assert_eq!(
        s.blocking,
        vec!["1 unresolved conflict(s) on blocks it writes".to_string()]
    );
    assert_eq!(
        s.proposed_by.actor, "agent:b",
        "b's rebased version was proposed by sb"
    );
    assert_eq!(s.proposed_by.run.as_deref(), Some("r1"));
}

#[test]
fn pshow1_refs_are_recognised_and_unknown_ones_surface() {
    for t in [
        "P-abcd",
        "P-abcd@2",
        "P-ABCDEF0123@10",
        &format!("P-{}", "a".repeat(64)),
    ] {
        assert!(is_proposal_ref(t), "{t}");
    }
    for t in [
        "P-abc",
        "P-wxyz",
        "p-abcd",
        "D-W2HXR",
        "P-abcd@",
        "P-abcd@x",
        "",
        "P-",
        &format!("P-{}", "a".repeat(65)),
    ] {
        assert!(!is_proposal_ref(t), "{t}");
    }
    let (_d, r, _p1, p2) = two_versions();
    let st = r.read();
    assert_eq!(st.proposal_show("P-0000@1").unwrap_err().code, E_NOT_FOUND);
    let stable = p2.r#ref.split('@').next().unwrap();
    assert_eq!(
        st.proposal_show(&format!("{stable}@9")).unwrap_err().code,
        E_NOT_FOUND
    );
}

/// Approving a version that carries a decision lands an acceptance change in a final version
/// (`P-x@2`); the version the human approved still reads as landed, as the gate and the log say.
#[test]
fn pshow1_an_approved_version_with_a_decision_reads_landed_like_the_gate_and_the_log() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    r.decide(
        &writer(),
        "work",
        DecisionInput {
            parent: section,
            statement: "Pins record content".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let p1 = r
        .propose(
            &writer(),
            "work",
            "decide",
            Some(rdy_qa::intake::TEST_SUMMARY),
        )
        .unwrap();
    rdy_qa::intake::confirm_before_landing(&mut r, &approver(), &p1);
    let info = r.land_report(&approver(), &p1.r#ref).unwrap();
    assert_eq!(info.approved, p1.r#ref);
    let st = r.read();
    let row = st.log_rows().unwrap().remove(0);
    assert_eq!(row.landing, info.landing);
    assert_eq!(row.approved.as_deref(), Some(p1.r#ref.as_str()));
    assert_ne!(
        row.proposal, p1.id,
        "the fixture lands an acceptance version after the approved one"
    );
    let s = st.proposal_show(&p1.r#ref).unwrap();
    assert_eq!(
        (s.state.as_str(), s.landing.as_deref()),
        ("landed", Some(info.landing.as_str()))
    );
    assert_eq!(
        s.blocking,
        vec![format!("landed in {}", &info.landing[..12])]
    );
    assert!(s.conflicts.is_empty(), "{s:?}");
    let chain: Vec<(&str, bool, bool)> = s
        .versions
        .iter()
        .map(|v| (v.id.as_str(), v.latest, v.landed))
        .collect();
    assert_eq!(
        chain,
        vec![
            (p1.id.as_str(), false, true),
            (row.proposal.as_str(), true, true)
        ]
    );
    let fin = st.proposal_show(&stable_of(&p1)).unwrap();
    assert_eq!(
        (fin.id.as_str(), fin.state.as_str(), fin.landing.as_deref()),
        (row.proposal.as_str(), "landed", Some(info.landing.as_str()))
    );
    drop(st);
    let e = r.land_report(&approver(), &p1.r#ref).unwrap_err();
    assert_eq!(e.code, E_ALREADY_LANDED, "{e}");
}
