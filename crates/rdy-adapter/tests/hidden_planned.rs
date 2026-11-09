//! relc4 (D-B2AC3, TASK-1422): the adapter carries the hidden planned lists. A first
//! publication gets `first_hidden_planned`; a later one gets `newly_hidden_planned` in its
//! comparison. Both read the captured tag, the profile's stable id and the host's binding.

use rdy_adapter::publication::{HiddenList, PreviousPublication};
use rdy_adapter::{BlockId, StoreHandle};
use rdy_engine::publication::{self, disclosure_diff};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "relc4".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(
            &session(Role::Writer),
            "work",
            "relc4",
            Some("test summary"),
        )
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

/// v1.0.0: the public profile `site` over `guide` (shown prose, one planned note) and the
/// planned doc `ghost` (one prose). v2.0.0: one more planned note in `guide`. Then a third
/// planned note lands untagged.
fn fixture() -> (tempfile::TempDir, BlockId) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$ghost", "fields": {"name": "ghost", "title": "Ghost", "doc_type": "page", "planned": true}}
        ]),
    );
    let id = |k: &str| w.created[k].clone();
    let (admin, guide, ghost) = (id("$admin"), id("$guide"), id("$ghost"));
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "parent": guide, "body": "Shown words."},
            {"op": "create", "type": "note", "parent": guide, "body": "Planned.", "fields": {"planned": true}},
            {"op": "create", "type": "prose", "parent": ghost, "body": "Ghost words."},
            {"op": "create", "type": "publication_profile", "id": "$p", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "public", "select": [guide, ghost]}}
        ]),
    );
    let profile = BlockId::parse(&w.created["$p"]).unwrap();
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let note = |body: &str| json!([{"op": "create", "type": "note", "parent": guide, "body": body, "fields": {"planned": true}}]);
    write(&mut r, note("Second planned."));
    land(&mut r);
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    write(&mut r, note("Third planned."));
    land(&mut r);
    r.close().unwrap();
    (dir, profile)
}

/// The engine's own resolution and lists for `tag`, read straight from it.
fn direct(dir: &std::path::Path, tag: &str) -> (publication::Resolved, rdy_model::Hash) {
    let r = Rdy::open(dir).unwrap();
    let st = r.read();
    let scope = st.resolve_scope(&format!("tag:{tag}")).unwrap();
    let res = publication::resolve(&st, &scope, "site").unwrap();
    let identity = res.selection_identity;
    drop(st);
    r.close().unwrap();
    (res, identity)
}

#[test]
fn relc4_adapter_first_and_later_publications_use_captured_tags() {
    let (dir, profile) = fixture();
    let (v1, v1_identity) = direct(dir.path(), "v1.0.0");
    let (v2, _) = direct(dir.path(), "v2.0.0");
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();

    // a first publication: the full list of the tag, not of the later landings
    for selector in ["site".to_string(), profile.to_string()] {
        let first = snap.disclosure("v1.0.0", &selector, None).unwrap();
        assert_eq!(first.comparison, None);
        assert_eq!(first.first_attachments, Some(vec![]));
        let list = first.first_hidden_planned.expect("a first list");
        assert_eq!(list, v1.hidden_planned.full());
        assert_eq!((list.docs, list.blocks), (2, 2), "{selector}");
        let by = |t: &str| list.rows.iter().find(|r| r.title == t).unwrap().clone();
        assert!(by("Ghost").doc_hidden && by("Ghost").hidden_blocks == 1);
        assert!(!by("Guide").doc_hidden && by("Guide").hidden_blocks == 1);
        let ids: Vec<_> = list.rows.iter().map(|r| r.doc.clone()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]), "sorted by doc id");
    }
    let v2_first = snap.disclosure("v2.0.0", "site", None).unwrap();
    assert_eq!(v2_first.first_hidden_planned.unwrap().blocks, 3);

    // a later publication: the comparison holds the new list, and no first list rides beside it
    let previous = PreviousPublication {
        tag: "v1.0.0".into(),
        profile_version: v1.profile.version,
        selection_identity: v1_identity,
    };
    let later = snap.disclosure("v2.0.0", "site", Some(&previous)).unwrap();
    assert_eq!(later.first_attachments, None);
    assert_eq!(later.first_hidden_planned, None);
    let diff = later.comparison.expect("a comparison");
    assert_eq!(diff, disclosure_diff(&v1, &v2).unwrap());
    let new = &diff.newly_hidden_planned;
    assert_eq!((new.docs, new.blocks, new.omitted_rows), (1, 1, 0));
    assert_eq!(new.rows[0].title, "Guide");
    assert!(!new.rows[0].doc_hidden);

    // hidden blocks are no entries: the tag's identity and hash did not move with them
    assert_eq!(v2.selection_identity, v1_identity);
    assert_eq!(v2.selection_hash, v1.selection_hash);

    // the host's binding still decides: a wrong identity is refused, never a first publication
    let wrong = PreviousPublication {
        selection_identity: rdy_model::Hash::of(b"another selection"),
        ..previous
    };
    let e = snap.disclosure("v2.0.0", "site", Some(&wrong)).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(
        e.message,
        "previous publication does not match version v1.0.0"
    );
    // a tag no one captured is refused
    assert!(snap.disclosure("v9.9.9", "site", None).is_err());
    assert_eq!(HiddenList::default().rows, vec![]);
    handle.close().unwrap();
}
