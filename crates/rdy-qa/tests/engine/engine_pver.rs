//! pver1: proposals have versions under one stable id; an approval lands the version it names
//! and is refused when a newer version exists.
// `LandOptions { strict: true, ..Default::default() }` pins the shape Task 16 extends (relay,
// evidence); until then the update syntax is needless.
#![allow(clippy::needless_update)]
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}

fn stable_of(r: &ProposalInfo) -> String {
    r.r#ref.split('@').next().unwrap().to_string()
}

#[test]
fn pver1_a_second_propose_on_an_open_view_makes_a_version_under_the_same_stable_id() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p1 = r.propose(&writer(), "work", "first", None).unwrap();
    assert_eq!((p1.version, p1.changes), (1, 1));
    assert_eq!(p1.stable, p1.id);
    assert_eq!(p1.r#ref, format!("P-{}@1", &p1.id[..12]));
    write(&mut r, "work", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "work", "second", None).unwrap();
    assert_eq!(
        (p2.version, p2.changes, p2.stable.as_str()),
        (2, 2, p1.id.as_str())
    );
    assert_ne!(p2.id, p1.id);
    assert_eq!(p2.r#ref, format!("P-{}@2", &p1.id[..12]));
    assert_eq!(
        r.read()
            .reader()
            .proposal(&p2.id.parse().unwrap())
            .unwrap()
            .title,
        "second",
        "a version may retitle"
    );
    let latest = r.resolve_proposal(&stable_of(&p1)).unwrap();
    assert_eq!((latest.version.to_string(), latest.n), (p2.id.clone(), 2));
    let v1 = r
        .resolve_proposal(&format!("{}@1", stable_of(&p1)))
        .unwrap();
    assert_eq!((v1.version.to_string(), v1.n), (p1.id.clone(), 1));
    assert_eq!(
        r.resolve_proposal(&p1.id).unwrap(),
        v1,
        "a full hash resolves to its own version"
    );
    let e = r
        .resolve_proposal(&format!("{}@3", stable_of(&p1)))
        .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    let e = r.resolve_proposal("P-ab").unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let e = r.resolve_proposal("P-ffffffff").unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
}

#[test]
fn pver1_approving_a_moved_proposal_is_refused_and_the_latest_lands_with_its_version_recorded() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p1 = r.propose(&writer(), "work", "first", None).unwrap();
    write(&mut r, "work", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "work", "second", None).unwrap();
    let landings = r.read().landings().unwrap().len();
    let e = r.land_report(&approver(), &p1.r#ref).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert!(
        e.message.contains(&p2.r#ref) && e.fix.contains(&p2.r#ref),
        "{e}"
    );
    assert_eq!(
        r.read().landings().unwrap().len(),
        landings,
        "nothing landed"
    );
    let info = r.land_report(&approver(), &stable_of(&p1)).unwrap();
    assert_eq!(info.approved, p2.r#ref, "P-x resolves to the latest");
    assert_eq!(
        (info.title.as_str(), info.changes),
        ("second", 2),
        "what the CLI prints before landing"
    );
    let rec = r
        .read()
        .reader()
        .landing(&info.landing.parse().unwrap())
        .unwrap();
    assert_eq!(rec.approved_version, Some(p2.id.parse().unwrap()));
    assert_eq!(rec.changes.len(), 2);
    assert_eq!(info.rebased.len(), 1);
    assert_eq!(
        (
            info.rebased[0].view.as_str(),
            info.rebased[0].dropped,
            info.rebased[0].conflicts
        ),
        ("work", 2, 0)
    );
}

/// Review Focus 8.
#[test]
fn pver1_an_older_version_by_full_hash_is_moved() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p1 = r.propose(&writer(), "work", "first", None).unwrap();
    write(&mut r, "work", claim(&section, "b")).unwrap();
    r.propose(&writer(), "work", "second", None).unwrap();
    let e = r.land_report(&approver(), &p1.id).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
}

/// Review Focus 1.
#[test]
fn pver1_a_landing_carries_the_version_not_the_views_current_set() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p1 = r.propose(&writer(), "work", "first", None).unwrap();
    let later = write(&mut r, "work", claim(&section, "b")).unwrap().change;
    let info = r.land_report(&approver(), &p1.r#ref).unwrap();
    assert_eq!(info.approved, p1.r#ref);
    let st = r.read();
    assert_eq!(
        st.reader()
            .landing(&info.landing.parse().unwrap())
            .unwrap()
            .changes
            .len(),
        1
    );
    assert_eq!(
        st.included("work").as_ref(),
        &vec![later.parse().unwrap()],
        "the later write stays in the view"
    );
    assert_eq!(
        st.materialized(&Scope::Canon).unwrap().len(),
        4,
        "doc, section, seed claim, a"
    );
    assert_eq!(
        st.materialized(&Scope::View("work".into())).unwrap().len(),
        5
    );
}

#[test]
fn pver1_after_a_landing_the_next_propose_starts_a_new_stable_id() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p1 = r.propose(&writer(), "work", "first", None).unwrap();
    r.land_report(&approver(), &p1.r#ref).unwrap();
    write(&mut r, "work", claim(&section, "b")).unwrap();
    let p2 = r.propose(&writer(), "work", "next", None).unwrap();
    assert_ne!(p2.stable, p1.stable);
    assert_eq!(p2.version, 1);
    let e = r.land_report(&approver(), &p1.r#ref).unwrap_err();
    assert_eq!(
        e.code, E_ALREADY_LANDED,
        "a landed version stays landed: {e}"
    );
}

/// FW-HB1: a version proposed before an unrelated landing still lands as approved.
#[test]
fn hb1_an_approval_survives_an_unrelated_landing() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    let l0 = land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p = r.propose(&writer(), "work", "mine", None).unwrap();
    assert_eq!(p.canon_at.as_deref(), Some(l0.as_str()));
    let o = as_agent("agent:o", "r1");
    write_as(&mut r, &o, "other", claim(&section, "unrelated")).unwrap();
    let po = r.propose(&o, "other", "theirs", None).unwrap();
    let lo = r.land_report(&approver(), &po.r#ref).unwrap();
    let info = r.land_report(&approver(), &p.r#ref).unwrap();
    assert_eq!(info.approved, p.r#ref, "the version the approver saw lands");
    let rec = r
        .read()
        .reader()
        .landing(&info.landing.parse().unwrap())
        .unwrap();
    assert_eq!(
        rec.prev,
        Some(lo.landing.parse().unwrap()),
        "on top of the unrelated landing"
    );
    assert_eq!(rec.approved_version, Some(p.id.parse().unwrap()));
}

/// FW-HB1: a landing that touched the version's blocks moves it; the approver is told why.
#[test]
fn hb1_a_landing_on_the_versions_blocks_moves_the_proposal() {
    let (_d, mut r) = project();
    let (_doc, _section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &c);
    let sa = as_agent("agent:a", "r1");
    let sb = as_agent("agent:b", "r1");
    write_as(&mut r, &sa, "a", update(&c, &v, "a's")).unwrap();
    write_as(&mut r, &sb, "b", update(&c, &v, "b's")).unwrap();
    let pa = r.propose(&sa, "a", "a", None).unwrap();
    let pb = r.propose(&sb, "b", "b", None).unwrap();
    let lb = r.land_report(&approver(), &pb.r#ref).unwrap();
    let landings = r.read().landings().unwrap().len();
    let e = r.land_report(&approver(), &pa.r#ref).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert!(
        e.message.contains(&c) && e.message.contains(&lb.landing[..12]),
        "names the block and the landing: {e}"
    );
    assert!(e.fix.contains("propose again"), "{e}");
    assert!(
        !e.fix.contains("retry with base="),
        "never the writer's advice: {e}"
    );
    assert_eq!(e.issues.len(), 1);
    assert_eq!(
        r.read().landings().unwrap().len(),
        landings,
        "nothing landed"
    );
}

#[test]
fn hb1_strict_and_require_up_to_date_refuse_any_moved_base() {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    write(&mut r, "work", claim(&section, "a")).unwrap();
    let p = r.propose(&writer(), "work", "mine", None).unwrap();
    let o = as_agent("agent:o", "r1");
    write_as(&mut r, &o, "other", claim(&section, "unrelated")).unwrap();
    let po = r.propose(&o, "other", "theirs", None).unwrap();
    r.land_report(&approver(), &po.r#ref).unwrap();
    let e = r
        .land_with(
            &approver(),
            &p.r#ref,
            LandOptions {
                strict: true,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert!(
        e.message.contains("1 landing(s)") && e.fix.contains("--strict"),
        "{e}"
    );
    let cfg = d.path().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!("{text}reviews:\n  require_up_to_date: true\n"),
    )
    .unwrap();
    // the config is loaded at open and by a pack reload (msw19)
    assert!(matches!(r.reload_pack().unwrap(), ReloadOutcome::Reloaded));
    let e = r.land_report(&approver(), &p.r#ref).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e}");
    assert!(e.fix.contains("require_up_to_date"), "{e}");
    let p2 = r.propose(&writer(), "work", "mine", None).unwrap();
    assert_eq!(p2.version, 2, "re-proposed on the current canon");
    r.land_report(&approver(), &p2.r#ref).unwrap();
}

/// FW-HB1: a version approved across a landing that re-keyed its siblings warns at the approval.
/// The landing rebased the view onto it, but the version's keys were chosen against its
/// `canon_at`, so the warning is computed against that base, not the view's current one.
#[test]
fn hb1_approving_across_a_sibling_rekey_warns() {
    let (_d, mut r) = project();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "claim", "id": "$f", "parent": "$s", "body": "first"},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$s", "after": "$f", "body": "second"}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (s, first, second) = (
        res.created["$s"].clone(),
        res.created["$f"].clone(),
        res.created["$t"].clone(),
    );
    // vb (unpinned) places a block after `first` and proposes: no re-key yet
    write(
        &mut r,
        "vb",
        json!([{"op": "create", "type": "claim", "parent": s, "after": first, "body": "from b"}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "vb", "b", None).unwrap();
    assert!(
        !p.warnings.iter().any(|i| i.code == "W_SIBLINGS_REKEYED"),
        "{:?}",
        p.warnings
    );
    // va moves `first` after `second` and lands; vb is rebased onto that landing
    write(
        &mut r,
        "va",
        json!([{"op": "move", "id": first, "parent": s, "after": second}]),
    )
    .unwrap();
    land(&mut r, "va").unwrap();
    // the frozen version lands as approved (FW-HB1) and the approver is told its siblings moved
    let info = r.land_report(&approver(), &p.r#ref).unwrap();
    assert_eq!(info.approved, p.r#ref);
    let w = info
        .warnings
        .iter()
        .find(|i| i.code == "W_SIBLINGS_REKEYED")
        .unwrap_or_else(|| panic!("{:?}", info.warnings));
    assert!(w.message.contains(&s), "{w:?}");
}
