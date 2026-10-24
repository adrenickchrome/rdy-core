//! pshow1: a proposal version reads as its title, proposer, changes with block counts, the
//! conflicts and other reasons that block approving it now, and its version chain.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
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
    let p2 = r
        .propose(&writer(), "work", "second", Some(TEST_SUMMARY))
        .unwrap();
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
    assert_eq!(s.summary.as_deref(), Some(TEST_SUMMARY));
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
    let pa = r.propose(&sa, "a", "a", Some(TEST_SUMMARY)).unwrap();
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

/// A landed seed doc, then view "v" with one claim, proposed once by `writer()` (as
/// `two_versions` writes "work"). Returns the seed's section for a later claim.
fn proposed_in_v() -> (tempfile::TempDir, Rdy, String, ProposalInfo) {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "v", claim(&section, "a")).unwrap();
    let p = r
        .propose(&writer(), "v", "first", Some(TEST_SUMMARY))
        .unwrap();
    (d, r, section, p)
}

fn state_of(r: &Rdy, pref: &str) -> String {
    r.read().proposal_show(pref).unwrap().state
}

fn open_refs(r: &Rdy) -> Vec<String> {
    r.read()
        .open_proposals()
        .unwrap()
        .iter()
        .map(|(pref, _)| pref.to_string())
        .collect()
}

/// D-K0SS1: closing a view withdraws its open proposal; the record stays readable.
#[test]
fn vw1_closing_a_view_withdraws_its_latest_version() {
    let (_d, mut r, _section, p) = proposed_in_v();
    r.view_close(&writer(), &["v".into()], "done").unwrap();
    let s = r.read().proposal_show(&p.r#ref).unwrap();
    assert_eq!(s.state, "withdrawn");
    assert_eq!(s.blocking, vec!["withdrawn (view v closed)".to_string()]);
    assert!(s.conflicts.is_empty(), "{s:?}");
    assert_eq!(s.landing, None);
    assert!(r.read().open_proposals().unwrap().is_empty());
    // the record is kept: the version reads by its id too
    let by_id = r.read().proposal_show(&p.id).unwrap();
    assert_eq!(
        (by_id.r#ref.as_str(), by_id.state.as_str()),
        (p.r#ref.as_str(), "withdrawn")
    );
}

/// The op at view `view`'s head.
fn head_op(r: &Rdy, view: &str) -> String {
    r.read().view_head(view).unwrap().head.to_string()
}

/// D-K0SS1: an undo of the close reopens the view, never the version; the next propose writes
/// the next version of the same proposal, and that version is open.
#[test]
fn vw1_an_undo_of_the_close_reopens_the_view_but_not_the_version() {
    let (_d, mut r, section, p1) = proposed_in_v();
    r.view_close(&writer(), &["v".into()], "done").unwrap();
    let close = head_op(&r, "v");
    let u = r.undo(&writer(), "v", Some(&close), false).unwrap();
    assert!(u.reopened, "{u:?}");
    assert!(!r.read().view_head("v").unwrap().closed);
    assert_eq!(state_of(&r, &p1.r#ref), "withdrawn");
    assert!(open_refs(&r).is_empty(), "{:?}", open_refs(&r));
    write(&mut r, "v", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "v", "second", None).unwrap();
    assert!(p2.r#ref.ends_with("@2"), "{}", p2.r#ref);
    assert_eq!(stable_of(&p2), stable_of(&p1));
    assert_eq!(state_of(&r, &p2.r#ref), "open");
    assert_eq!(state_of(&r, &p1.r#ref), "superseded");
    assert_eq!(open_refs(&r), vec![p2.r#ref.clone()]);
}

/// D-K0SS1: an undo of the reopen closes the view again (an `Undo` whose effect is a close), so
/// it withdraws the version proposed in between; undoing that undo reopens the view only.
#[test]
fn vw1_a_close_by_an_undo_of_the_reopen_withdraws_the_version() {
    let (_d, mut r, section, _p1) = proposed_in_v();
    r.view_close(&writer(), &["v".into()], "done").unwrap();
    let close = head_op(&r, "v");
    let reopen = r.undo(&writer(), "v", Some(&close), false).unwrap();
    assert!(reopen.reopened, "{reopen:?}");
    write(&mut r, "v", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "v", "second", None).unwrap();
    assert_eq!(state_of(&r, &p2.r#ref), "open");
    let again = r.undo(&writer(), "v", Some(&reopen.op), false).unwrap();
    assert!(again.closed, "{again:?}");
    assert!(r.read().view_head("v").unwrap().closed);
    assert_eq!(state_of(&r, &p2.r#ref), "withdrawn");
    let open_again = r.undo(&writer(), "v", Some(&again.op), false).unwrap();
    assert!(open_again.reopened, "{open_again:?}");
    assert!(!r.read().view_head("v").unwrap().closed);
    assert_eq!(state_of(&r, &p2.r#ref), "withdrawn");
    assert!(open_refs(&r).is_empty(), "{:?}", open_refs(&r));
}

/// Guard (D-K0SS1 precedence): a landed version stays landed after its view closes.
#[test]
fn vw1_a_landed_version_stays_landed_after_its_view_closes() {
    let (_d, mut r, _section, p) = proposed_in_v();
    r.land(&approver(), &p.id).unwrap();
    r.view_close(&writer(), &["v".into()], "done").unwrap();
    assert_eq!(state_of(&r, &p.r#ref), "landed");
}

/// What `proposal_show` and `open_proposals` say of version `id` in the project at `dir`.
fn read_back(dir: &std::path::Path, id: &rdy_model::Hash) -> (String, Vec<String>) {
    let r = Rdy::open(dir).unwrap();
    let state = r.read().proposal_show(&id.to_string()).unwrap().state;
    let open = open_refs(&r);
    r.close().unwrap();
    (state, open)
}

/// Fix round 1 (D-K0SS1, the log order): the withdrawal boundary is the version's PROPOSAL
/// frame, never the first frame with its bytes. A blob chunk with the bytes of a later version,
/// written before a close and its undo, does not make that version withdrawn: not when it is
/// proposed, not after a reindex, and not after a dump and a load.
#[test]
fn vw1_a_blob_with_a_versions_bytes_before_a_close_does_not_withdraw_it() {
    use rdy_model::{Proposal, Provenance, Role};
    use rdy_store::Store;
    use rdy_store::view::ViewOpKind;
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "v", claim(&section, "a")).unwrap();
    r.close().unwrap();
    let mut s = Store::open(&d.path().join(".rdy/store")).unwrap();
    let by = Provenance {
        actor: "agent:w".into(),
        run: Some("r".into()),
        role: Role::Writer,
        at_ms: 7,
    };
    // the version `Store::propose` writes below, as its bytes
    let p = Proposal {
        stable: None,
        prev: None,
        title: "first".into(),
        view: "v".into(),
        changes: s.included("v").unwrap().as_ref().clone(),
        alternative: None,
        provenance: by.clone(),
        canon_at: s.canon_head().unwrap(),
        summary: None,
    };
    s.put_blob(&postcard::to_allocvec(&p).unwrap()).unwrap();
    let close = s
        .view_append(
            "v",
            vec![ViewOpKind::Close {
                reason: "done".into(),
            }],
            &by,
        )
        .unwrap()
        .head;
    s.view_append("v", vec![ViewOpKind::Undo { target: close }], &by)
        .unwrap();
    let id = s.propose("v", "first", by).unwrap();
    assert_eq!(id, p.id(), "the blob chunk holds the version's bytes");
    drop(s);
    let (state, open) = read_back(d.path(), &id);
    assert_eq!(state, "open", "the close precedes the version");
    assert_eq!(open.len(), 1, "{open:?}");
    let mut r = Rdy::open(d.path()).unwrap();
    r.reindex().unwrap();
    r.close().unwrap();
    assert_eq!(
        read_back(d.path(), &id),
        (state.clone(), open.clone()),
        "reindex"
    );
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let r = Rdy::open(d.path()).unwrap();
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    let to = scratch.path().join("loaded");
    rdy_engine::load::load(&file, &to).unwrap();
    assert_eq!(read_back(&to, &id), (state, open), "dump and load");
}
