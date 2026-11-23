//! hfp6: a canon move that carries a removal leaves no stale read on the live adapter handle that
//! made it. The handle warms its reads of a tag first. After the move, a new snapshot of the same
//! handle remakes the baseline and the review with the removal applied, with no close and no
//! reopen. A snapshot held from before the move refuses every content read.
//!
//! The source project is built with core calls and closed before an adapter handle opens a
//! store. Every hub write goes through the adapter.

use crate::hub_world::{AT, check, export, land, moved, project, repo_of, session, tag, write_in};
use rdy_adapter::publication::{Baseline, BaselineBinding, PreviousChoice, PublicationCheck};
use rdy_adapter::stream::StreamPurpose;
use rdy_adapter::sync::CanonMove;
use rdy_adapter::{Hash, RdyError, Snapshot, StoreHandle};
use rdy_engine::Rdy;
use rdy_engine::redaction::RedactInput;
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use serde_json::json;

/// The removed doc's title and its prose.
const REMOVED: [&str; 2] = ["Quiet Harbor Plan", "The harbor opens at dawn."];

/// Lands one removal of every version of the blocks `targets` in a landing of its own; returns
/// that landing.
fn remove(r: &mut Rdy, targets: &[String]) -> Hash {
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: targets.to_vec(),
                reason_class: "secret".into(),
                reason: "words that must not ship".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the words", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    r.store().reader().canon_head().unwrap().unwrap()
}

/// A project with the org profile `site` over the folder `pubs`. v1.0.0 holds the doc `base`;
/// v2.0.0 adds the doc `added` with the words of [`REMOVED`]. Returns v1, v2 and the ids of
/// `added` and its prose.
fn published(r: &mut Rdy) -> (Hash, Hash, Vec<String>) {
    let w = write_in(
        r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$base", "parent": "$pubs", "fields": {"name": "base", "title": "Base", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$bp", "parent": "$base", "body": "Base text."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = |n: &str| w.created[n].clone();
    write_in(
        r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id("$pubs")]}}
        ]),
    );
    let v1 = land(r, "work");
    tag(r, "v1.0.0");
    let added = write_in(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$added", "parent": id("$pubs"), "fields": {"name": "added", "title": REMOVED[0], "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$added", "body": REMOVED[1]}
        ]),
    );
    let v2 = land(r, "work");
    tag(r, "v2.0.0");
    let ids = ["$added", "$ap"].map(|n| added.created[n].clone());
    (v1, v2, ids.to_vec())
}

fn binding(b: &Baseline) -> BaselineBinding {
    BaselineBinding {
        repo_id: b.repo_id(),
        tag: b.tag().to_string(),
        profile_version: b.profile_version(),
        selection_identity: b.selection_identity(),
    }
}

/// `s`'s v2.0.0 review of `site` against `previous`, or its first review with none.
fn review(s: &Snapshot, previous: Option<&Baseline>) -> Result<PublicationCheck, RdyError> {
    let b = previous.map(binding);
    let choice = previous
        .zip(b.as_ref())
        .map(|(baseline, binding)| PreviousChoice::Baseline { baseline, binding });
    s.disclosure_against("v2.0.0", "site", choice)
}

/// The words of [`REMOVED`] that `text` shows.
fn shows_removed(text: &str) -> Vec<&'static str> {
    REMOVED.into_iter().filter(|w| text.contains(w)).collect()
}

/// The refusal of a read on a snapshot that predates a removal.
fn predates(e: RdyError) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_REDACTED_STATE",
            "This read state predates content removal.",
            "Read the project again, or run the dump again."
        )
    );
}

/// A hub's handle warms its v2.0.0 baseline and reviews, then moves canon with a pushed removal
/// of v2.0.0's added doc and its prose. A new snapshot of the same handle remakes the baseline
/// with the removal: one redaction and none of the removed words. Its first review and its review
/// against the v1.0.0 baseline show none of them either. A snapshot held from before the move
/// refuses the baseline and the review with `E_REDACTED_STATE`.
#[test]
fn hfp6_a_move_that_carries_a_removal_leaves_no_stale_read_on_the_handle() {
    let _clock = rdy_engine::pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().join("p");
    let mut p = project(&pd);
    let repo = repo_of(&p);
    let (v1, v2, added) = published(&mut p);
    p.close().unwrap();

    let at = |n: &str| dir.path().join(format!("{n}.rdysync"));
    let canon = StreamPurpose::CanonHistory;
    export(&pd, canon, v1, None, &at("e1"));
    export(&pd, canon, v2, Some(v1), &at("e2"));
    let hub = StoreHandle::create_holder(&dir.path().join("hub"), repo).unwrap();
    for (path, mv, expected, to) in [
        (at("e1"), CanonMove::Seed, None, v1),
        (at("e2"), CanonMove::Extend, Some(v1), v2),
    ] {
        let s = hub.snapshot().unwrap();
        let c = check(&s, &[&path]).unwrap();
        assert_eq!(hub.move_canon(&c, mv, expected).unwrap(), moved(to));
    }

    // the handle warms its reads of v2.0.0: the baseline and both reviews show the added doc
    let held = hub.snapshot().unwrap();
    let b1 = held.publication_baseline("v1.0.0", "site").unwrap();
    let b2 = held.publication_baseline("v2.0.0", "site").unwrap();
    assert!(b2.redactions().is_empty());
    assert_eq!(shows_removed(&format!("{b2:?}")), REMOVED);
    for previous in [None, Some(&b1)] {
        let c = review(&held, previous).unwrap();
        let text = serde_json::to_string(&c.review).unwrap();
        assert_eq!(shows_removed(&text), REMOVED, "{text}");
    }

    // the project removes the added doc and its prose; the hub takes the removal on the same handle
    let mut p = Rdy::open(&pd).unwrap();
    let removal = remove(&mut p, &added);
    p.close().unwrap();
    export(&pd, canon, removal, Some(v2), &at("e3"));
    let s = hub.snapshot().unwrap();
    let c = check(&s, &[&at("e3")]).unwrap();
    assert_eq!(
        hub.move_canon(&c, CanonMove::Extend, Some(v2)).unwrap(),
        moved(removal)
    );
    drop((s, c));

    // a new snapshot of the same handle reuses no read built before the removal
    let fresh = hub.snapshot().unwrap();
    let b2 = fresh.publication_baseline("v2.0.0", "site").unwrap();
    assert_eq!(b2.redactions().len(), 1);
    assert!(shows_removed(&format!("{b2:?}")).is_empty(), "{b2:?}");
    for previous in [None, Some(&b1)] {
        let c = review(&fresh, previous).unwrap();
        let text = serde_json::to_string(&c.review).unwrap();
        assert!(shows_removed(&text).is_empty(), "{text}");
    }

    // the snapshot held from before the move refuses every content read
    predates(held.publication_baseline("v2.0.0", "site").unwrap_err());
    for previous in [None, Some(&b1)] {
        predates(review(&held, previous).unwrap_err());
    }

    drop((held, fresh));
    hub.close().unwrap();
}
