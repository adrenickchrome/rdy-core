//! relc4 (D-B2AC3, TASK-1422): the adapter carries the hidden planned lists. A first
//! publication gets `first_hidden_planned`; a later one gets `newly_hidden_planned` in its
//! comparison. Both read the captured tag, the profile's stable id and the host's binding.

use rdy_adapter::publication::{HiddenList, HiddenRow, PreviousPublication};
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

/// The publication tool's own `hidden_planned` and `disclosure.newly_hidden_planned` values for
/// the candidate `tag`, read straight from the engine.
fn tool_values(dir: &std::path::Path, tag: &str, against: Option<&str>) -> (Value, Value) {
    let r = Rdy::open(dir).unwrap();
    let st = r.read();
    let scope = format!("tag:{tag}");
    let against = against.map(|a| format!("tag:{a}"));
    let v = publication::check(&st, "site", Some(&scope), against.as_deref(), None).unwrap();
    drop(st);
    r.close().unwrap();
    (v["hidden_planned"].clone(), v["disclosure"].clone())
}

fn row(doc: &str, title: &str, redacted: bool, hidden_blocks: u64, doc_hidden: bool) -> HiddenRow {
    HiddenRow {
        doc: BlockId::parse(doc).unwrap(),
        title: title.into(),
        redacted,
        hidden_blocks,
        doc_hidden,
    }
}

/// hfp4 (D-B2AC3): the method gives the form the publication tool prints, for an empty list,
/// a list with a fully hidden doc and a partly hidden doc, and the new list of a comparison.
#[test]
fn hfp4_hidden_form_equals_the_publication_tool() {
    let (dir, _) = fixture();
    let (tool_v1, _) = tool_values(dir.path(), "v1.0.0", None);
    let (tool_v2, tool_diff) = tool_values(dir.path(), "v2.0.0", Some("v1.0.0"));
    let (_, v1_identity) = direct(dir.path(), "v1.0.0");
    let (v1, _) = direct(dir.path(), "v1.0.0");
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();

    // the empty list
    assert_eq!(
        HiddenList::default().to_json(),
        json!({"docs": 0, "blocks": 0, "rows": [], "omitted_rows": 0})
    );

    // a full doc (Ghost) beside a partly hidden doc (Guide): the exact form, rows by doc id
    let first = snap.disclosure("v1.0.0", "site", None).unwrap();
    let list = first.first_hidden_planned.as_ref().unwrap();
    let form = list.to_json();
    assert_eq!(form, tool_v1);
    assert_eq!(
        first.first_hidden_planned.as_ref().map(HiddenList::to_json),
        Some(tool_v1)
    );
    assert_eq!(form["docs"], 2);
    assert_eq!(form["blocks"], 2);
    assert_eq!(form["omitted_rows"], 0);
    let rows = form["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (json_row, typed) in rows.iter().zip(&list.rows) {
        let mut keys: Vec<_> = json_row.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["doc", "doc_hidden", "hidden_blocks", "redacted", "title"]
        );
        assert_eq!(json_row["doc"], typed.doc.to_string());
        assert_eq!(json_row["title"], typed.title);
        assert_eq!(json_row["redacted"], false);
        assert_eq!(json_row["hidden_blocks"], typed.hidden_blocks);
        assert_eq!(json_row["doc_hidden"], typed.doc_hidden);
    }
    let by = |t: &str| rows.iter().find(|r| r["title"] == t).unwrap().clone();
    assert_eq!(
        (
            by("Ghost")["doc_hidden"].clone(),
            by("Ghost")["hidden_blocks"].clone()
        ),
        (json!(true), json!(1))
    );
    assert_eq!(
        (
            by("Guide")["doc_hidden"].clone(),
            by("Guide")["hidden_blocks"].clone()
        ),
        (json!(false), json!(1))
    );
    let ids: Vec<_> = rows
        .iter()
        .map(|r| r["doc"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "sorted by doc id");

    // the second tag holds one more hidden block
    let second = snap.disclosure("v2.0.0", "site", None).unwrap();
    assert_eq!(
        second
            .first_hidden_planned
            .as_ref()
            .map(HiddenList::to_json),
        Some(tool_v2)
    );

    // the new list of a comparison is the same form as the review's key
    let previous = PreviousPublication {
        tag: "v1.0.0".into(),
        profile_version: v1.profile.version,
        selection_identity: v1_identity,
    };
    let later = snap.disclosure("v2.0.0", "site", Some(&previous)).unwrap();
    assert_eq!(later.first_hidden_planned, None);
    let diff = later.comparison.as_ref().unwrap();
    let newly = diff.newly_hidden_planned.to_json();
    assert_eq!(newly["docs"], 1);
    assert_eq!(newly["blocks"], 1);
    assert_eq!(newly, later.review["disclosure"]["newly_hidden_planned"]);
    assert_eq!(newly, tool_diff["newly_hidden_planned"]);
    drop(snap);
    handle.close().unwrap();
}

/// hfp4 (D-RDH1C): the review keeps its three keys. A first list rides beside it as a typed
/// value and expands no key; a later check returns no first list.
#[test]
fn hfp4_first_list_does_not_expand_review() {
    let (dir, _) = fixture();
    let (v1, v1_identity) = direct(dir.path(), "v1.0.0");
    let (v2, _) = direct(dir.path(), "v2.0.0");
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let keys = |c: &rdy_adapter::publication::PublicationCheck| {
        let mut k: Vec<_> = c.review.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    let expected = ["disclosure", "included", "references"];

    let first = snap.disclosure("v1.0.0", "site", None).unwrap();
    assert_eq!(keys(&first), expected);
    assert!(first.first_hidden_planned.is_some());
    assert!(first.review["disclosure"].is_null());
    assert!(!first.review.to_string().contains("hidden_planned"));

    let previous = PreviousPublication {
        tag: "v1.0.0".into(),
        profile_version: v1.profile.version,
        selection_identity: v1_identity,
    };
    let later = snap.disclosure("v2.0.0", "site", Some(&previous)).unwrap();
    assert_eq!(keys(&later), expected);
    assert_eq!(
        later.first_hidden_planned.as_ref().map(HiddenList::to_json),
        None
    );
    assert!(later.review["included"].is_null());

    // the hidden lists change no status, hash or identity of the tag
    assert_eq!(v2.selection_hash, v1.selection_hash);
    assert_eq!(v2.selection_identity, v1_identity);
    drop(snap);
    handle.close().unwrap();
}

/// hfp4: the form keeps a redacted flag, a title that a removal replaced, the 200-row cap and
/// the omitted count, whatever the list holds.
#[test]
fn hfp4_form_keeps_redaction_and_caps() {
    let doc = |n: u32| format!("d-Q{n:04}");
    let mut rows = vec![row(&doc(0), "Shown", false, 2, false)];
    rows.push(row(&doc(1), &doc(1), true, 0, true));
    for n in 2..200 {
        rows.push(row(&doc(n), "More", false, 1, false));
    }
    let list = HiddenList {
        docs: 203,
        blocks: 1_000,
        rows,
        omitted_rows: 3,
    };
    let form = list.to_json();
    assert_eq!(
        (form["docs"].clone(), form["blocks"].clone()),
        (json!(203), json!(1_000))
    );
    assert_eq!(form["omitted_rows"], 3);
    let rows = form["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 200);
    assert_eq!(
        rows[0],
        json!({"doc": "d-Q0000", "title": "Shown", "redacted": false, "hidden_blocks": 2, "doc_hidden": false})
    );
    // a removed title shows the doc id, and the flag says so
    assert_eq!(
        rows[1],
        json!({"doc": "d-Q0001", "title": "d-Q0001", "redacted": true, "hidden_blocks": 0, "doc_hidden": true})
    );
    // the form holds exactly its four keys
    let mut keys: Vec<_> = form.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["blocks", "docs", "omitted_rows", "rows"]);
}
