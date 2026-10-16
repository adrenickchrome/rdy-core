//! di11 (D-JV6Z8, O10): the inbox entry `pending_added` for a new proposal version and for a
//! verdict that carries advice. It never reads readiness, so a re-derived inbox is the same.
use rdy_engine::inbox::api::INBOX_DOC;
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::{Rdy, Session};
use rdy_model::Role;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::intake::verdict_on;
use rdy_qa::reindex_check::{derived, reindex_reproduces};
use rdy_qa::tensions::{ag, created, put};
use rdy_store::inbox::{InboxCursor, InboxSub};
use serde_json::json;

const WATCHER: &str = "agent:watcher";

/// A subscription of `agent:watcher` that starts at the head of the log.
fn watcher_at_head(r: &Rdy) -> InboxSub {
    InboxSub {
        actor: WATCHER.into(),
        run: None,
        cursor: InboxCursor {
            at: r.read().seq(),
            skip: 0,
        },
        areas: vec![],
        role: "reader".into(),
    }
}

/// The `pending_added` entries `sub` has from its cursor to the end of the log: (ids, line).
fn pending_added(r: &Rdy, sub: &InboxSub) -> Vec<(Vec<String>, String)> {
    r.read()
        .inbox_page(sub, None, usize::MAX)
        .expect("inbox page")
        .entries
        .into_iter()
        .filter(|e| e.event == "pending_added")
        .map(|e| (e.ids, e.line))
        .collect()
}

fn claim(r: &mut Rdy, view: &str, spec: &Spec, body: &str) {
    put(
        r,
        view,
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(), "body": body}]),
    );
}

#[test]
fn di11_a_new_proposal_version_adds_pending_added() {
    let (_dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let sub = watcher_at_head(&r);
    claim(&mut r, "v1", &spec, "Guests read.");
    assert!(
        pending_added(&r, &sub).is_empty(),
        "a write is not a proposal"
    );
    let p1 = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    claim(&mut r, "v1", &spec, "Guests may not write.");
    let p2 = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    assert_eq!(p2.version, 2, "{p2:?}");
    // every session matches it, the proposer's own too
    let own = InboxSub {
        actor: "agent:v1".into(),
        ..sub.clone()
    };
    for s in [&sub, &own] {
        assert_eq!(
            pending_added(&r, s),
            vec![
                (
                    vec![p1.id.clone()],
                    format!("{} waits for an answer; read rdy_pending", p1.r#ref)
                ),
                (
                    vec![p2.id.clone()],
                    format!("{} waits for an answer; read rdy_pending", p2.r#ref)
                ),
            ],
            "{}",
            s.actor
        );
    }
    assert_ne!(p1.r#ref, p2.r#ref);
    assert!(
        INBOX_DOC.contains("pending_added"),
        "rdy_schema names the entry"
    );
}

#[test]
fn di11_an_advised_verdict_adds_pending_added() {
    let (_dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$q",
    );
    let p = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    let sub = watcher_at_head(&r);
    // a verdict with no advice puts nothing in the queue
    let plain = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$p", "parent": spec.section.to_string(),
                    "body": "Guests read the rules."}]),
        ),
        "$p",
    );
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &plain,
        &spec.c,
        "agent:r",
        false,
    );
    assert!(
        pending_added(&r, &sub).is_empty(),
        "no advice: nothing waits"
    );
    // a verdict that advises blocking waits for the person, on the version that holds it
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let entries = pending_added(&r, &sub);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let (ids, line) = &entries[0];
    assert_eq!(ids.len(), 1, "{ids:?}");
    let review: rdy_model::Hash = ids[0].parse().expect("the id is the review");
    assert!(r.read().reader().review(&review).is_ok(), "{ids:?}");
    assert_eq!(
        line,
        &format!("{} waits for an answer; read rdy_pending", p.r#ref)
    );
}

#[test]
fn di11_pending_added_is_stable_across_reindex() {
    let (dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$q",
    );
    let sub = watcher_at_head(&r);
    r.put_subscription(&sub).expect("subscribe");
    let p = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let before = derived(&r).expect("derived");
    let entries = &before.inboxes[&format!("{WATCHER}/")];
    let added: Vec<_> = entries
        .iter()
        .filter(|e| e["event"] == "pending_added")
        .collect();
    assert_eq!(
        added.len(),
        2,
        "the version and the advised verdict: {entries:?}"
    );
    assert!(
        added
            .iter()
            .all(|e| e["line"]
                == format!("{} waits for an answer; read rdy_pending", p.r#ref).as_str()),
        "{added:?}"
    );
    r.close().expect("close");
    reindex_reproduces(dir.path()).expect("reindex reproduces the inbox");
}

/// A project with an open proposal `P-x@1` whose view holds the claim `$q`, a subscription at the
/// head of the log, and a verdict that advises blocking on `$q` and the spec's section content:
/// the one `pending_added` entry the verdict gave.
fn advised_project() -> (
    tempfile::TempDir,
    Rdy,
    rdy_qa::edit_loop::Spec,
    rdy_engine::ProposalInfo,
    InboxSub,
    (Vec<String>, String),
) {
    let (dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$q",
    );
    let p = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    let sub = watcher_at_head(&r);
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let mut entries = pending_added(&r, &sub);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let first = entries.remove(0);
    assert_eq!(
        first.1,
        format!("{} waits for an answer; read rdy_pending", p.r#ref)
    );
    (dir, r, spec, p, sub, first)
}

#[test]
fn di11_an_advised_entry_stays_after_the_advice_is_accepted() {
    let (_dir, mut r, _spec, p, sub, first) = advised_project();
    let shown = r.read().proposal_show(&p.r#ref).expect("show");
    assert_eq!(shown.advised.len(), 1, "{shown:?}");
    let req = AcceptRequest {
        proposal: p.r#ref.clone(),
        advised_digest: shown.advised_digest.clone().unwrap_or_default(),
        evidence: Some("yes, block on it".into()),
        question: Some("Accept the reviewer's advice to block?".into()),
        session_ref: Some("chat-1".into()),
        ..Default::default()
    };
    let a = r.accept_advice(&ag("relay"), req).expect("accept");
    assert_eq!(a.accepted.len(), 1, "{a:?}");
    // the same cursor reads the same entry: the verdict carried the advice when it was submitted
    assert_eq!(pending_added(&r, &sub), vec![first]);
}

#[test]
fn di11_an_advised_entry_stays_after_the_advice_is_dismissed() {
    let (_dir, mut r, _spec, p, sub, first) = advised_project();
    let shown = r.read().proposal_show(&p.r#ref).expect("show");
    let tension = shown.advised[0].tension.clone();
    let rev = Session::agent("agent:rev".into(), Some("rev".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension, "both stand for now")
        .expect("dismiss");
    assert!(
        r.read().proposal_show(&p.r#ref).unwrap().advised.is_empty(),
        "a dismissed tension is no longer advised"
    );
    assert_eq!(pending_added(&r, &sub), vec![first]);
}

#[test]
fn di11_an_advised_entry_keeps_its_proposal_when_a_replacement_is_published() {
    let (_dir, mut r, spec, p, sub, first) = advised_project();
    // a second claim and a replacement version: the verdict's entry still names the first
    put(
        &mut r,
        "v1",
        json!([{"op": "create", "type": "claim", "parent": spec.section.to_string(), "body": "Guests may not write."}]),
    );
    let p2 = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    assert_eq!(p2.version, 2, "{p2:?}");
    let entries = pending_added(&r, &sub);
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.0 == first.0)
            .collect::<Vec<_>>(),
        vec![&first],
        "{entries:?}"
    );
    assert!(
        first.1.starts_with(&p.r#ref),
        "the entry names the version the verdict met: {first:?}"
    );
}

#[test]
fn di11_an_advised_entry_stays_after_the_advice_is_dismissed_and_the_proposal_lands() {
    let (_dir, mut r, _spec, p, sub, first) = advised_project();
    let shown = r.read().proposal_show(&p.r#ref).expect("show");
    let tension = shown.advised[0].tension.clone();
    let rev = Session::agent("agent:rev".into(), Some("rev".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rev, &tension, "both stand for now")
        .expect("dismiss");
    assert_eq!(pending_added(&r, &sub), vec![first.clone()]);
    // landing the holding proposal changes no record the entry reads: the same cursor, the same entry
    r.land(&approver(), &p.id).expect("land");
    assert_eq!(pending_added(&r, &sub), vec![first]);
}

#[test]
fn di11_an_advised_entry_stays_after_the_holding_proposal_lands() {
    let (_dir, mut r, _spec, p, sub, first) = advised_project();
    r.land(&approver(), &p.id).expect("land");
    assert_eq!(pending_added(&r, &sub), vec![first]);
}

// ---- fix round 3: the holder is read from the records written before the review ----

#[test]
fn di11_advice_on_an_unproposed_claim_and_a_landed_canon_claim_names_no_proposal() {
    // the canon claim was written by the proposal that landed the spec; that proposal never held
    // the pair, so the advice waits on no version and gives no entry
    let (_dir, mut r) = project();
    let spec = spec_doc(&mut r);
    let u = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$u", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec."}]),
        ),
        "$u",
    );
    let sub = watcher_at_head(&r);
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &u,
        &spec.c,
        "agent:r",
        true,
    );
    assert_eq!(pending_added(&r, &sub), vec![]);
}

/// A pack whose claims carry a field `note`, tracked or not.
fn note_pack(tracked: bool) -> String {
    format!(
        "name: trk\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  claim:\n    prefix: c\n    traits: [normative, describes]\n    body: required\n    fields:\n      note: {{ kind: str }}\n{}",
        if tracked {
            "    tracking: [note]\n"
        } else {
            ""
        }
    )
}

/// A project on the `trk` pack (claims carry `note`; tracked or not) with an open proposal whose
/// view holds the claim `$q` (note `n1`), a subscription at the head of the log, and a verdict
/// that advises blocking on `$q` and the spec's claim: the one entry the verdict gave.
fn noted_project(
    tracked: bool,
) -> (
    tempfile::TempDir,
    Rdy,
    rdy_qa::edit_loop::Spec,
    rdy_engine::ProposalInfo,
    InboxSub,
    (Vec<String>, String),
) {
    let (dir, r) = project();
    drop(r);
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), note_pack(tracked)).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec.", "fields": {"note": "n1"}}]),
        ),
        "$q",
    );
    let p = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    let sub = watcher_at_head(&r);
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let mut entries = pending_added(&r, &sub);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let first = entries.remove(0);
    assert_eq!(
        first.1,
        format!("{} waits for an answer; read rdy_pending", p.r#ref)
    );
    (dir, r, spec, p, sub, first)
}

/// The entry of the review (`first`'s ids) among `sub`'s `pending_added` entries.
fn entry_of(r: &Rdy, sub: &InboxSub, first: &(Vec<String>, String)) -> Vec<(Vec<String>, String)> {
    pending_added(r, sub)
        .into_iter()
        .filter(|e| e.0 == first.0)
        .collect()
}

#[test]
fn di11_a_later_competing_proposal_does_not_rename_the_entry() {
    let (_dir, mut r, spec, _p, sub, first) = noted_project(true);
    // a second view rewrites only the tracked field of the canon claim, so it writes the canon
    // item at the very content the verdict judged, and it is proposed after the verdict
    put(
        &mut r,
        "v2",
        json!([{"op": "update", "id": spec.c.to_string(), "fields": {"note": "other"}}]),
    );
    let p2 = r
        .propose_subset(&ag("v2"), "v2", "Notes", None, None)
        .unwrap();
    assert!(
        pending_added(&r, &sub)
            .iter()
            .any(|e| e.0 == [p2.id.clone()]),
        "the later proposal has its own entry"
    );
    assert_eq!(entry_of(&r, &sub, &first), vec![first]);
}

#[test]
fn di11_a_higher_scoring_replacement_does_not_rename_the_entry() {
    let (_dir, mut r, spec, _p, sub, first) = noted_project(true);
    // the replacement version also writes the canon claim at the judged content: it would hold
    // both items, but it was written after the verdict
    put(
        &mut r,
        "v1",
        json!([{"op": "update", "id": spec.c.to_string(), "fields": {"note": "other"}}]),
    );
    let p2 = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    assert_eq!(p2.version, 2, "{p2:?}");
    assert_eq!(entry_of(&r, &sub, &first), vec![first]);
}

#[test]
fn di11_an_entry_survives_a_pack_change_that_starts_tracking_a_field() {
    let (dir, mut r, _spec, _p, sub, first) = noted_project(false);
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), note_pack(true)).unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    assert_eq!(entry_of(&r, &sub, &first), vec![first]);
}

#[test]
fn di11_an_entry_survives_a_pack_change_that_stops_tracking_a_field() {
    let (dir, mut r, _spec, _p, sub, first) = noted_project(true);
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), note_pack(false)).unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    assert_eq!(entry_of(&r, &sub, &first), vec![first]);
}

// ---- fix round 4: the holder is read under the tracking in force, not under any omission ----

const WIDE_FIELDS: usize = 13;

/// A pack whose claims carry thirteen fields `f01` to `f13`; `tracked` of them are tracking.
fn wide_pack(tracked: &[&str]) -> String {
    let mut s = String::from(
        "name: wide\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  claim:\n    prefix: c\n    traits: [normative, describes]\n    body: required\n    fields:\n",
    );
    for i in 1..=WIDE_FIELDS {
        s.push_str(&format!("      f{i:02}: {{ kind: str }}\n"));
    }
    if !tracked.is_empty() {
        s.push_str(&format!("    tracking: [{}]\n", tracked.join(", ")));
    }
    s
}

#[test]
fn di11_an_entry_survives_dropping_two_tracking_fields_of_a_wide_block() {
    // thirteen fields, two of them tracking when the verdict was written; the pack then stops
    // tracking both. The content hash under the tracking now differs, and the entry stays.
    let (dir, r) = project();
    drop(r);
    std::fs::write(
        dir.path().join(".rdy/packs/wide.yaml"),
        wide_pack(&["f03", "f07"]),
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: wide\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    let fields: serde_json::Map<String, serde_json::Value> = (1..=WIDE_FIELDS)
        .map(|i| (format!("f{i:02}"), json!(format!("v{i}"))))
        .collect();
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec.", "fields": fields}]),
        ),
        "$q",
    );
    let p = r
        .propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    let sub = watcher_at_head(&r);
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    let first = {
        let mut e = pending_added(&r, &sub);
        assert_eq!(e.len(), 1, "{e:?}");
        e.remove(0)
    };
    assert_eq!(
        first.1,
        format!("{} waits for an answer; read rdy_pending", p.r#ref)
    );
    std::fs::write(dir.path().join(".rdy/packs/wide.yaml"), wide_pack(&[])).unwrap();
    assert!(matches!(
        r.reload_pack().unwrap(),
        rdy_engine::ReloadOutcome::Reloaded
    ));
    assert_eq!(entry_of(&r, &sub, &first), vec![first]);
}

#[test]
fn di11_an_unproposed_removal_of_an_ordinary_field_names_no_proposal() {
    // `note` is never a tracking field. The proposal wrote the claim with `note`; the view then
    // unset it and nobody proposed that. The verdict judges the claim without `note`, which no
    // proposal wrote, so no proposal holds the pair and the verdict gives no entry.
    let (dir, r) = project();
    drop(r);
    std::fs::write(dir.path().join(".rdy/packs/trk.yaml"), note_pack(false)).unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: trk\n").unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let spec = spec_doc(&mut r);
    let q = created(
        &put(
            &mut r,
            "v1",
            json!([{"op": "create", "type": "claim", "id": "$q", "parent": spec.section.to_string(),
                    "body": "Guests read every rule of the spec.", "fields": {"note": "n1"}}]),
        ),
        "$q",
    );
    r.propose_subset(&ag("v1"), "v1", "Guests", None, None)
        .unwrap();
    put(
        &mut r,
        "v1",
        json!([{"op": "update", "id": q.to_string(), "unset": ["note"]}]),
    );
    let sub = watcher_at_head(&r);
    verdict_on(
        &mut r,
        &rdy_engine::Scope::Inflight,
        &q,
        &spec.c,
        "agent:r",
        true,
    );
    assert_eq!(pending_added(&r, &sub), vec![]);
}
