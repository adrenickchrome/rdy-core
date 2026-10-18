//! pub7 (D-KDFZ3, D-0VCQ5, D-WDKWQ): two resolved selections of one profile compare to what the
//! candidate discloses that the baseline did not. New docs and docs moved into a selected folder
//! are added; child, asset, path and redaction edits change a doc's digest; folders, attachments
//! and references outside the selection compare by identity. Core compares two selections it is
//! given; which one was last published is the hub's record.
mod common;
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::publication::{
    self, AttachmentEntry, Audience, ChangedAttachment, ChangedDoc, ChangedFolder, Classification,
    DisclosureDiff, DocEntry, ReferenceRow, Resolved,
};
use rdy_engine::*;
use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget, RedactedBlockIdentity};
use rdy_model::*;
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------------------------
// helpers

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn writer() -> Session {
    session(Role::Writer)
}

fn founder() -> Session {
    session(Role::Approver)
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) -> String {
    let p = r
        .propose(&writer(), view, "publication", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap()
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    bid(&w.created[name])
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

const PROFILE_MISMATCH: (&str, &str) = (
    "Disclosure comparison requires the same publication profile.",
    "Choose a baseline that contains the candidate profile id.",
);
const AUDIENCE_MISMATCH: (&str, &str) = (
    "Disclosure comparison requires the same audience.",
    "Choose a baseline with the same audience as the candidate profile.",
);

/// A block for a state built in memory, outside any store.
fn blk(id: &str, ty: &str, parent: Option<&str>, order: &str) -> Block {
    Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: order.into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn named(mut b: Block, name: &str) -> Block {
    b.fields.insert("name".into(), s(name));
    b
}

fn with_body(mut b: Block, body: &str) -> Block {
    b.body = body.into();
    b
}

fn with_field(mut b: Block, k: &str, v: FieldValue) -> Block {
    b.fields.insert(k.into(), v);
    b
}

fn folder(id: &str, parent: Option<&str>, order: &str, name: &str) -> Block {
    named(blk(id, "folder", parent, order), name)
}

fn doc(id: &str, parent: Option<&str>, order: &str, name: &str) -> Block {
    named(blk(id, "doc", parent, order), name)
}

fn prose(id: &str, parent: &str, order: &str, body: &str) -> Block {
    with_body(blk(id, "prose", Some(parent), order), body)
}

fn figure(id: &str, parent: &str, order: &str, blob: Hash) -> Block {
    with_field(
        with_field(
            with_field(
                blk(id, "figure", Some(parent), order),
                "blob",
                s(&blob.to_string()),
            ),
            "media_type",
            s("image/png"),
        ),
        "alt",
        s("CAPTION-CANARY"),
    )
}

fn profile_block(id: &str, key: &str, audience: &str, select: &[&str]) -> Block {
    let mut b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    b.fields.insert("key".into(), s(key));
    b.fields.insert("title".into(), s("Profile title"));
    b.fields.insert("audience".into(), s(audience));
    b.fields.insert(
        "select".into(),
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    );
    b
}

fn admin() -> Block {
    doc("d-ADM1N", None, "zz", "admin")
}

/// A state of `blocks` alone, built in memory.
fn synth(blocks: Vec<Block>) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

/// `profile` resolved in the in-memory state of `blocks`, with `st`'s pack and store.
fn res(st: &ReadState, blocks: Vec<Block>, profile: &str) -> Resolved {
    publication::resolve_in_for_tests(st, &synth(blocks), profile).unwrap()
}

/// `blocks` with each block of `edits` replacing the one of the same id, or added.
fn edit(blocks: &[Block], edits: Vec<Block>) -> Vec<Block> {
    let mut by_id: BTreeMap<BlockId, Block> =
        blocks.iter().map(|b| (b.id.clone(), b.clone())).collect();
    for b in edits {
        by_id.insert(b.id.clone(), b);
    }
    by_id.into_values().collect()
}

/// `blocks` without the blocks `ids` names.
fn without(blocks: &[Block], ids: &[&str]) -> Vec<Block> {
    blocks
        .iter()
        .filter(|b| !ids.contains(&b.id.as_str()))
        .cloned()
        .collect()
}

fn find<'a>(blocks: &'a [Block], id: &str) -> &'a Block {
    blocks.iter().find(|b| b.id.as_str() == id).unwrap()
}

fn doc_entry(r: &Resolved, id: &str) -> DocEntry {
    r.docs
        .iter()
        .find(|d| d.id.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is a selected doc"))
        .clone()
}

fn att_entry(r: &Resolved, id: &str) -> AttachmentEntry {
    r.attachments
        .iter()
        .find(|a| a.block.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is an attachment"))
        .clone()
}

fn changed_doc(before: &Resolved, after: &Resolved, id: &str) -> ChangedDoc {
    let (b, a) = (doc_entry(before, id), doc_entry(after, id));
    ChangedDoc {
        id: bid(id),
        before_path: b.path,
        after_path: a.path,
        before_digest: b.digest,
        after_digest: a.digest,
    }
}

fn row(source: &str, location: &str, target: &str, c: Classification, n: u64) -> ReferenceRow {
    ReferenceRow {
        source: bid(source),
        location: location.into(),
        target: target.into(),
        classification: c,
        occurrences: n,
    }
}

fn diff(before: &Resolved, after: &Resolved) -> DisclosureDiff {
    publication::disclosure_diff(before, after).unwrap()
}

/// Whether every doc, folder, attachment and reference array of `d` is empty.
fn all_empty(d: &DisclosureDiff) -> bool {
    d.added_docs.is_empty()
        && d.removed_docs.is_empty()
        && d.changed_docs.is_empty()
        && d.changed_folders.is_empty()
        && d.added_attachments.is_empty()
        && d.removed_attachments.is_empty()
        && d.changed_attachments.is_empty()
        && d.new_references.is_empty()
        && d.new_reference_count == 0
}

fn assert_no_change(d: &DisclosureDiff, what: &str) {
    assert!(all_empty(d), "{what}: {d:#?}");
    assert!(!d.profile_changed, "{what}");
    assert_eq!(d.before_selection_hash, d.after_selection_hash, "{what}");
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn pub7_new_and_moved_in_docs_are_added() {
    let (_dir, r) = project();
    let st = r.read();
    let base = vec![
        admin(),
        profile_block("pf-AAAA1", "pubsite", "org", &["fd-AREA1"]),
        folder("fd-AREA1", None, "a", "area"),
        doc("d-KEEP1", Some("fd-AREA1"), "a", "keep"),
        prose("p-KEEP1", "d-KEEP1", "a", "Kept."),
        folder("fd-XTRA1", None, "b", "extra"),
        doc("d-MV001", Some("fd-XTRA1"), "a", "moving"),
        prose("p-MV001", "d-MV001", "a", "Moving text."),
    ];
    let before = res(&st, base.clone(), "pubsite");
    assert!(before.docs.iter().all(|d| d.id != bid("d-MV001")));

    // a new doc in the selected folder, and an existing doc moved into it; same profile
    let mut moved = find(&base, "d-MV001").clone();
    moved.parent = Some(bid("fd-AREA1"));
    moved.order = "b".into();
    let next = edit(
        &base,
        vec![
            moved,
            doc("d-NEW01", Some("fd-AREA1"), "c", "fresh"),
            prose("p-NEW01", "d-NEW01", "a", "Fresh text."),
        ],
    );
    let after = res(&st, next.clone(), "pubsite");
    let d = diff(&before, &after);
    assert_eq!(
        d.added_docs,
        vec![doc_entry(&after, "d-MV001"), doc_entry(&after, "d-NEW01")]
    );
    assert_eq!(d.added_docs[0].path, "area/moving");
    assert_eq!(d.added_docs[1].path, "area/fresh");
    assert!(d.removed_docs.is_empty());
    assert!(d.changed_docs.is_empty(), "the kept doc is unchanged");
    assert!(
        d.changed_folders.is_empty(),
        "a folder's own record is unchanged by its children: {:?}",
        d.changed_folders
    );
    assert!(d.new_references.is_empty());
    assert_eq!(d.new_reference_count, 0);
    assert!(!d.profile_changed);
    assert_eq!(d.profile_id, Some(bid("pf-AAAA1")));
    assert_eq!(d.audience, Audience::Org);
    assert_eq!(
        (d.before_selection_hash, d.after_selection_hash),
        (before.selection_hash, after.selection_hash)
    );
    assert_ne!(d.before_selection_hash, d.after_selection_hash);

    // an overlapping selector (the folder and a doc in it) never duplicates a row
    let overlap = edit(
        &next,
        vec![profile_block(
            "pf-AAAA1",
            "pubsite",
            "org",
            &["d-NEW01", "fd-AREA1", "d-MV001"],
        )],
    );
    let after2 = res(&st, overlap, "pubsite");
    let d2 = diff(&before, &after2);
    assert!(d2.profile_changed);
    assert_eq!(
        d2.added_docs,
        vec![doc_entry(&after2, "d-MV001"), doc_entry(&after2, "d-NEW01")]
    );
    assert!(d2.removed_docs.is_empty() && d2.changed_docs.is_empty());
}

#[test]
fn pub7_child_asset_path_and_redaction_changes_are_visible() {
    let (_dir, r) = project();
    let st = r.read();
    let base = vec![
        admin(),
        profile_block("pf-AAAA1", "pubsite", "public", &["d-PAGE1"]),
        folder("fd-T0P01", None, "a", "top"),
        doc("d-PAGE1", Some("fd-T0P01"), "a", "page"),
        prose("p-TXT01", "d-PAGE1", "a", "Child text."),
        prose("p-TXT02", "d-PAGE1", "b", "Other text."),
        figure("fg-F1G01", "d-PAGE1", "c", Hash::of(b"one")),
    ];
    let before = res(&st, base.clone(), "pubsite");
    let doc_version = before.m.version_of(&bid("d-PAGE1"));

    // a child-only text edit
    let child = res(
        &st,
        edit(&base, vec![prose("p-TXT01", "d-PAGE1", "a", "Edited.")]),
        "pubsite",
    );
    let d = diff(&before, &child);
    assert_eq!(
        d.changed_docs,
        vec![changed_doc(&before, &child, "d-PAGE1")]
    );
    assert_eq!(d.changed_docs[0].before_path, "top/page");
    assert_eq!(d.changed_docs[0].after_path, "top/page");
    assert_ne!(
        d.changed_docs[0].before_digest,
        d.changed_docs[0].after_digest
    );
    assert_eq!(child.m.version_of(&bid("d-PAGE1")), doc_version);
    assert!(d.changed_folders.is_empty() && d.added_docs.is_empty());
    assert!(d.changed_attachments.is_empty());

    // a blob hash change: the doc and the attachment change, nothing is added
    let swapped = res(
        &st,
        edit(
            &base,
            vec![figure("fg-F1G01", "d-PAGE1", "c", Hash::of(b"two"))],
        ),
        "pubsite",
    );
    let d = diff(&before, &swapped);
    assert_eq!(
        d.changed_docs,
        vec![changed_doc(&before, &swapped, "d-PAGE1")]
    );
    assert_eq!(swapped.m.version_of(&bid("d-PAGE1")), doc_version);
    assert_eq!(
        d.changed_attachments,
        vec![ChangedAttachment {
            block: bid("fg-F1G01"),
            before_home_doc: bid("d-PAGE1"),
            after_home_doc: bid("d-PAGE1"),
            before_blob: Hash::of(b"one"),
            after_blob: Hash::of(b"two"),
            before_available: false,
            after_available: false,
        }]
    );
    assert!(d.added_attachments.is_empty() && d.removed_attachments.is_empty());

    // a home-doc path change through an ancestor rename
    let renamed = res(
        &st,
        edit(&base, vec![folder("fd-T0P01", None, "a", "renamed")]),
        "pubsite",
    );
    let d = diff(&before, &renamed);
    assert_eq!(
        d.changed_docs,
        vec![changed_doc(&before, &renamed, "d-PAGE1")]
    );
    assert_eq!(
        (
            d.changed_docs[0].before_path.as_str(),
            d.changed_docs[0].after_path.as_str()
        ),
        ("top/page", "renamed/page")
    );
    assert_eq!(renamed.m.version_of(&bid("d-PAGE1")), doc_version);
    assert_eq!(
        d.changed_folders,
        vec![ChangedFolder {
            id: bid("fd-T0P01"),
            before_version: before.m.version_of(&bid("fd-T0P01")),
            after_version: renamed.m.version_of(&bid("fd-T0P01")),
            before_path: Some("top".into()),
            after_path: Some("renamed".into()),
        }]
    );

    // [RD] a stub's request changes the digest; its removal notice is never hashed
    let identity = |request: &[u8]| RedactedBlockIdentity {
        version: Hash::of(b"version of p-TXT01"),
        content_hash: Hash::of(b"content of p-TXT01"),
        frozen: false,
        request: Hash::of(request),
    };
    let stub = |request: &[u8]| {
        Block::redacted(
            bid("p-TXT01"),
            "prose".into(),
            Some(bid("d-PAGE1")),
            "a".into(),
            identity(request),
        )
    };
    let first = res(&st, edit(&base, vec![stub(b"request one")]), "pubsite");
    let second = res(&st, edit(&base, vec![stub(b"request two")]), "pubsite");
    let d = diff(&first, &second);
    assert_eq!(
        d.changed_docs,
        vec![changed_doc(&first, &second, "d-PAGE1")]
    );
    assert_eq!(
        first.m.version_of(&bid("p-TXT01")),
        second.m.version_of(&bid("p-TXT01")),
        "the stub keeps its recorded version"
    );
    let mut renoticed = stub(b"request one");
    assert_eq!(renoticed.body, CONTENT_REMOVED);
    renoticed.body = "A different notice.".into();
    let other_notice = res(&st, edit(&base, vec![renoticed]), "pubsite");
    assert_no_change(&diff(&first, &other_notice), "the notice is not hashed");
    drop(st);
    drop(r);

    // [RD] in a store: a redaction changes the request and the shared blob's availability
    let (dir, mut r) = project();
    let removed_bytes = vec![7u8; 1 << 20];
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let removed_blob = r.store_mut().put_blob(&removed_bytes).unwrap();
    let shared_blob = r.store_mut().put_blob(&shared_bytes).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$top", "fields": {"name": "top"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$top", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$d", "body": "CANARY body."},
            {"op": "create", "type": "prose", "id": "$p2", "parent": "$d", "body": "Readable sibling."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "fields": {"blob": shared_blob.to_string(), "media_type": "application/octet-stream", "alt": "shared"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (d_id, p1, f) = (created(&w, "$d"), created(&w, "$p1"), created(&w, "$f"));
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": created(&w, "$admin").to_string(),
            "fields": {"key": "pubsite", "title": "Site", "audience": "public", "select": [d_id.to_string()]}}]),
    );
    land(&mut r, "work");
    let before = publication::resolve(&r.read(), &Scope::Canon, "pubsite").unwrap();
    assert!(att_entry(&before, f.as_str()).available);
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![
                RedactTarget::AllVersions(p1.clone()),
                RedactTarget::Blob(removed_blob),
            ],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    assert_eq!(fixture.requests.len(), 1);
    let r = Rdy::open(dir.path()).unwrap();
    let after = publication::resolve(&r.read(), &Scope::Canon, "pubsite").unwrap();
    let d = diff(&before, &after);
    assert_eq!(
        d.changed_docs,
        vec![changed_doc(&before, &after, d_id.as_str())]
    );
    assert_eq!(d.changed_docs[0].before_path, d.changed_docs[0].after_path);
    assert_eq!(
        d.changed_attachments,
        vec![ChangedAttachment {
            block: f.clone(),
            before_home_doc: d_id.clone(),
            after_home_doc: d_id.clone(),
            before_blob: shared_blob,
            after_blob: shared_blob,
            before_available: true,
            after_available: false,
        }]
    );
    assert!(d.added_docs.is_empty() && d.removed_docs.is_empty());
    assert!(d.changed_folders.is_empty() && d.new_references.is_empty());
    assert!(!d.profile_changed);
    // no old content and no notice in any row
    let shown = format!("{d:?}");
    assert!(!shown.contains("CANARY"));
    assert!(!shown.contains(CONTENT_REMOVED));
}

#[test]
fn pub7_profile_and_ancestor_metadata_changes_are_visible() {
    let (_dir, r) = project();
    let st = r.read();
    let base = vec![
        admin(),
        profile_block("pf-AAAA1", "pubsite", "org", &["fd-AREA1"]),
        folder("fd-T0P01", None, "a", "top"),
        folder("fd-AREA1", Some("fd-T0P01"), "a", "area"),
        doc("d-KEEP1", Some("fd-AREA1"), "a", "keep"),
        doc("d-S1B01", Some("fd-T0P01"), "b", "sibling"),
        prose("p-S1B01", "d-S1B01", "a", "Sibling text."),
    ];
    let before = res(&st, base.clone(), "pubsite");
    let profile = find(&base, "pf-AAAA1").clone();

    // title, trail and selector revisions, each resolving the same docs
    let revisions = [
        (
            "a title",
            with_field(profile.clone(), "title", s("New title")),
        ),
        (
            "a trail setting",
            with_field(profile.clone(), "trail", FieldValue::Bool(false)),
        ),
        (
            "a redundant selector",
            with_field(
                profile.clone(),
                "select",
                FieldValue::List(vec![s("fd-AREA1"), s("d-KEEP1")]),
            ),
        ),
        (
            "a description",
            with_field(profile.clone(), "description", s("About.")),
        ),
    ];
    for (what, revised) in revisions {
        let after = res(&st, edit(&base, vec![revised]), "pubsite");
        assert_eq!(after.docs, before.docs, "{what}");
        let d = diff(&before, &after);
        assert!(d.profile_changed, "{what}");
        assert!(all_empty(&d), "{what}: {d:#?}");
        assert_eq!(d.before_profile_version, before.profile.version, "{what}");
        assert_eq!(d.after_profile_version, after.profile.version, "{what}");
        assert_ne!(d.before_profile_version, d.after_profile_version, "{what}");
        assert_ne!(d.before_selection_hash, d.after_selection_hash, "{what}");
    }

    // an ancestor-only edit changes its folder row and the selection, and adds no sibling
    let tagged = with_field(
        folder("fd-T0P01", None, "a", "top"),
        "title",
        s("Top folder"),
    );
    let after = res(&st, edit(&base, vec![tagged]), "pubsite");
    let d = diff(&before, &after);
    assert!(!d.profile_changed);
    assert_eq!(
        d.changed_folders,
        vec![ChangedFolder {
            id: bid("fd-T0P01"),
            before_version: before.m.version_of(&bid("fd-T0P01")),
            after_version: after.m.version_of(&bid("fd-T0P01")),
            before_path: Some("top".into()),
            after_path: Some("top".into()),
        }]
    );
    assert_ne!(
        d.changed_folders[0].before_version,
        d.changed_folders[0].after_version
    );
    assert!(d.added_docs.is_empty() && d.changed_docs.is_empty());
    assert_ne!(d.before_selection_hash, d.after_selection_hash);

    // an ancestor rename changes every selected path under it, still adding no sibling
    let renamed = res(
        &st,
        edit(&base, vec![folder("fd-T0P01", None, "a", "renamed")]),
        "pubsite",
    );
    let d = diff(&before, &renamed);
    let folder_paths: Vec<(BlockId, Option<String>, Option<String>)> = d
        .changed_folders
        .iter()
        .map(|f| (f.id.clone(), f.before_path.clone(), f.after_path.clone()))
        .collect();
    assert_eq!(
        folder_paths,
        vec![
            (
                bid("fd-AREA1"),
                Some("top/area".into()),
                Some("renamed/area".into())
            ),
            (bid("fd-T0P01"), Some("top".into()), Some("renamed".into())),
        ]
    );
    assert_eq!(
        d.changed_folders[0].before_version, d.changed_folders[0].after_version,
        "the selected folder's own record did not change, only its path"
    );
    assert_eq!(
        d.changed_docs,
        vec![changed_doc(&before, &renamed, "d-KEEP1")]
    );
    assert!(d.added_docs.is_empty());

    // a sibling's edits and a new sibling outside the selection change nothing
    let sibling = res(
        &st,
        edit(
            &base,
            vec![
                prose("p-S1B01", "d-S1B01", "a", "Edited sibling."),
                doc("d-S1B02", Some("fd-T0P01"), "c", "another"),
                folder("fd-S1B03", Some("fd-T0P01"), "d", "more"),
            ],
        ),
        "pubsite",
    );
    assert_no_change(&diff(&before, &sibling), "a sibling edit");

    // a new folder inside the selection is a folder addition
    let grown = res(
        &st,
        edit(
            &base,
            vec![folder("fd-NEW01", Some("fd-AREA1"), "b", "new")],
        ),
        "pubsite",
    );
    let d = diff(&before, &grown);
    assert_eq!(
        d.changed_folders,
        vec![ChangedFolder {
            id: bid("fd-NEW01"),
            before_version: None,
            after_version: grown.m.version_of(&bid("fd-NEW01")),
            before_path: None,
            after_path: Some("top/area/new".into()),
        }]
    );
    assert!(d.added_docs.is_empty());
}

#[test]
fn pub7_removed_docs_and_attachments_are_reported() {
    let (_dir, r) = project();
    let st = r.read();
    let base = vec![
        admin(),
        profile_block("pf-AAAA1", "pubsite", "org", &["fd-AREA1"]),
        folder("fd-AREA1", None, "a", "area"),
        doc("d-KEEP1", Some("fd-AREA1"), "a", "keep"),
        figure("fg-GN001", "d-KEEP1", "a", Hash::of(b"gone figure")),
        figure("fg-SWAP1", "d-KEEP1", "b", Hash::of(b"first")),
        figure("fg-HM001", "d-KEEP1", "c", Hash::of(b"homed")),
        figure("fg-SAME1", "d-KEEP1", "d", Hash::of(b"same")),
        doc("d-GN001", Some("fd-AREA1"), "b", "gone"),
        figure("fg-GDC01", "d-GN001", "a", Hash::of(b"in gone doc")),
        doc("d-MV001", Some("fd-AREA1"), "c", "moving"),
        prose("p-MV001", "d-MV001", "a", "Moving text."),
        doc("d-0THR1", Some("fd-AREA1"), "d", "other"),
        folder("fd-SB001", Some("fd-AREA1"), "e", "sub"),
        folder("fd-XTRA1", None, "b", "extra"),
    ];
    let before = res(&st, base.clone(), "pubsite");

    // delete a doc, move one out, delete a figure, swap a blob, rehome a figure, drop a folder
    let mut moved = find(&base, "d-MV001").clone();
    moved.parent = Some(bid("fd-XTRA1"));
    let next = edit(
        &without(&base, &["d-GN001", "fg-GDC01", "fg-GN001", "fd-SB001"]),
        vec![
            moved,
            figure("fg-SWAP1", "d-KEEP1", "b", Hash::of(b"second")),
            figure("fg-HM001", "d-0THR1", "a", Hash::of(b"homed")),
        ],
    );
    let after = res(&st, next, "pubsite");
    let d = diff(&before, &after);
    assert_eq!(
        d.removed_docs,
        vec![doc_entry(&before, "d-GN001"), doc_entry(&before, "d-MV001")]
    );
    assert_eq!(
        d.removed_docs[1].path, "area/moving",
        "the before-side path"
    );
    assert!(d.added_docs.is_empty());
    assert_eq!(
        d.removed_attachments,
        vec![
            att_entry(&before, "fg-GDC01"),
            att_entry(&before, "fg-GN001")
        ]
    );
    assert!(
        d.added_attachments.is_empty(),
        "a replaced hash is a change"
    );
    assert_eq!(
        d.changed_attachments,
        vec![
            ChangedAttachment {
                block: bid("fg-HM001"),
                before_home_doc: bid("d-KEEP1"),
                after_home_doc: bid("d-0THR1"),
                before_blob: Hash::of(b"homed"),
                after_blob: Hash::of(b"homed"),
                before_available: false,
                after_available: false,
            },
            ChangedAttachment {
                block: bid("fg-SWAP1"),
                before_home_doc: bid("d-KEEP1"),
                after_home_doc: bid("d-KEEP1"),
                before_blob: Hash::of(b"first"),
                after_blob: Hash::of(b"second"),
                before_available: false,
                after_available: false,
            },
        ]
    );
    assert_eq!(
        d.changed_docs,
        vec![
            changed_doc(&before, &after, "d-0THR1"),
            changed_doc(&before, &after, "d-KEEP1"),
        ]
    );
    assert_eq!(
        d.changed_folders,
        vec![ChangedFolder {
            id: bid("fd-SB001"),
            before_version: before.m.version_of(&bid("fd-SB001")),
            after_version: None,
            before_path: Some("area/sub".into()),
            after_path: None,
        }]
    );
    assert!(d.new_references.is_empty());
    // only ids, hashes and booleans of attachments: never a caption
    assert!(!format!("{d:?}").contains("CAPTION-CANARY"));

    // the reverse comparison adds what this one removed, once each
    let back = diff(&after, &before);
    assert_eq!(
        back.added_docs,
        vec![doc_entry(&before, "d-GN001"), doc_entry(&before, "d-MV001")]
    );
    assert_eq!(
        back.added_attachments,
        vec![
            att_entry(&before, "fg-GDC01"),
            att_entry(&before, "fg-GN001")
        ]
    );
    assert!(back.removed_docs.is_empty() && back.removed_attachments.is_empty());
}

#[test]
fn pub7_new_excluded_references_use_positive_occurrence_deltas() {
    let (_dir, r) = project();
    let st = r.read();
    let base = vec![
        admin(),
        profile_block("pf-AAAA1", "pubsite", "org", &["fd-AREA1"]),
        folder("fd-AREA1", None, "a", "area"),
        doc("d-KEEP1", Some("fd-AREA1"), "a", "keep"),
        prose("p-A0001", "d-KEEP1", "a", "One {{d-XC001}}."),
        prose(
            "p-B0001",
            "d-KEEP1",
            "b",
            "Two {{d-XC001}} and {{d-XC001}}.",
        ),
        prose("p-C0001", "d-KEEP1", "c", "See {{d-TGT01}}."),
        prose("p-D0001", "d-KEEP1", "d", "Here {{d-XC001}}."),
        prose("p-E0001", "d-KEEP1", "e", "Plain."),
        doc("d-TGT01", Some("fd-AREA1"), "b", "target"),
        folder("fd-XTRA1", None, "b", "extra"),
        doc("d-XC001", Some("fd-XTRA1"), "a", "excluded"),
    ];
    let before = res(&st, base.clone(), "pubsite");
    assert_eq!(
        before.references.rows,
        vec![
            row("p-A0001", "body", "d-XC001", Classification::Excluded, 1),
            row("p-B0001", "body", "d-XC001", Classification::Excluded, 2),
            row("p-D0001", "body", "d-XC001", Classification::Excluded, 1),
        ]
    );

    let mut target = find(&base, "d-TGT01").clone();
    target.parent = Some(bid("fd-XTRA1"));
    let next = edit(
        &base,
        vec![
            // duplicates: one occurrence becomes three
            prose(
                "p-A0001",
                "d-KEEP1",
                "a",
                "One {{d-XC001}}, {{d-XC001}}, {{d-XC001}}.",
            ),
            // a decrease: two become one
            prose("p-B0001", "d-KEEP1", "b", "Two {{d-XC001}}."),
            // the same reference at another location
            with_field(
                prose("p-D0001", "d-KEEP1", "d", "Here."),
                "note",
                s("{{d-XC001}}"),
            ),
            // a new reference to no block at all
            prose("p-E0001", "d-KEEP1", "e", "Plain {{missing-key-token}}."),
            // a selected target leaves the selection
            target,
        ],
    );
    let after = res(&st, next, "pubsite");
    let d = diff(&before, &after);
    assert_eq!(
        d.new_references,
        vec![
            row("p-A0001", "body", "d-XC001", Classification::Excluded, 2),
            row("p-C0001", "body", "d-TGT01", Classification::Excluded, 1),
            row(
                "p-D0001",
                "/fields/note",
                "d-XC001",
                Classification::Excluded,
                1
            ),
            row(
                "p-E0001",
                "body",
                "missing-key-token",
                Classification::Missing,
                1
            ),
        ]
    );
    assert_eq!(d.new_reference_count, 5);
    assert_eq!(d.removed_docs, vec![doc_entry(&before, "d-TGT01")]);

    // the reverse: only increases count, decreases and removals add nothing
    let back = diff(&after, &before);
    assert_eq!(
        back.new_references,
        vec![
            row("p-B0001", "body", "d-XC001", Classification::Excluded, 1),
            row("p-D0001", "body", "d-XC001", Classification::Excluded, 1),
        ]
    );
    assert_eq!(back.new_reference_count, 2);

    // a pure decrease adds no row
    let fewer = res(
        &st,
        edit(&base, vec![prose("p-B0001", "d-KEEP1", "b", "Two.")]),
        "pubsite",
    );
    let d = diff(&before, &fewer);
    assert!(d.new_references.is_empty());
    assert_eq!(d.new_reference_count, 0);
}

#[test]
fn pub7_baseline_requires_same_profile_and_audience() {
    let (_dir, r) = project();
    let st = r.read();
    let base = vec![
        admin(),
        profile_block("pf-AAAA1", "pubsite", "org", &["d-KEEP1"]),
        profile_block("pf-BBBB1", "othersite", "org", &["d-KEEP1"]),
        doc("d-KEEP1", None, "a", "keep"),
    ];
    let site = res(&st, base.clone(), "pubsite");
    let other = res(&st, base.clone(), "othersite");
    let e = publication::disclosure_diff(&site, &other).unwrap_err();
    assert_err(&e, E_BAD_INPUT, PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);

    // the same profile id with another audience
    let public = edit(
        &base,
        vec![profile_block("pf-AAAA1", "pubsite", "public", &["d-KEEP1"])],
    );
    let site_public = res(&st, public, "pubsite");
    let e = publication::disclosure_diff(&site, &site_public).unwrap_err();
    assert_err(&e, E_BAD_INPUT, AUDIENCE_MISMATCH.0, AUDIENCE_MISMATCH.1);
    let e = publication::disclosure_diff(&site_public, &site).unwrap_err();
    assert_err(&e, E_BAD_INPUT, AUDIENCE_MISMATCH.0, AUDIENCE_MISMATCH.1);
    // both differ: the profile mismatch wins
    let e = publication::disclosure_diff(&other, &site_public).unwrap_err();
    assert_err(&e, E_BAD_INPUT, PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);

    // a reassigned key: the candidate's id selects the baseline, not the key it held
    let reassigned = edit(
        &base,
        vec![
            profile_block("pf-AAAA1", "newkey", "org", &["d-KEEP1"]),
            profile_block("pf-BBBB1", "pubsite", "org", &["d-KEEP1"]),
        ],
    );
    let candidate = res(&st, reassigned.clone(), "newkey");
    assert_eq!(candidate.profile.id, Some(bid("pf-AAAA1")));
    let id = candidate.profile.id.clone().unwrap().to_string();
    let baseline = res(&st, base.clone(), &id);
    let d = diff(&baseline, &candidate);
    assert_eq!(d.profile_id, Some(bid("pf-AAAA1")));
    assert!(d.profile_changed, "the key revision is a profile change");
    assert!(all_empty(&d), "{d:#?}");
    // the old key, now held by another profile, names a different profile
    let by_key = res(&st, reassigned, "pubsite");
    let e = publication::disclosure_diff(&site, &by_key).unwrap_err();
    assert_err(&e, E_BAD_INPUT, PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);

    // two implicit profiles (no id) are the same profile; one implicit and one block are not
    let implicit = |r: &Resolved| {
        let mut r = r.clone();
        r.profile.id = None;
        r.profile.version = None;
        r.profile.key = None;
        r
    };
    let d = diff(&implicit(&site), &implicit(&site));
    assert_eq!(d.profile_id, None);
    assert_eq!(
        (d.before_profile_version, d.after_profile_version),
        (None, None)
    );
    assert_no_change(&d, "two implicit profiles");
    let e = publication::disclosure_diff(&implicit(&site), &site).unwrap_err();
    assert_err(&e, E_BAD_INPUT, PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);
    let e = publication::disclosure_diff(&site, &implicit(&site)).unwrap_err();
    assert_err(&e, E_BAD_INPUT, PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);
    let e = publication::disclosure_diff(&implicit(&site), &implicit(&site_public)).unwrap_err();
    assert_err(&e, E_BAD_INPUT, AUDIENCE_MISMATCH.0, AUDIENCE_MISMATCH.1);
}

#[test]
fn pub7_diff_order_is_deterministic_and_self_diff_is_empty() {
    let (_dir, r) = project();
    let st = r.read();
    // ids and names sort in opposite orders, and so do attachments' blocks and home docs
    let base = vec![
        admin(),
        profile_block("pf-AAAA1", "pubsite", "org", &["fd-AREA1"]),
        folder("fd-AREA1", None, "a", "area"),
        folder("fd-ZZZZ1", Some("fd-AREA1"), "a", "alpha-folder"),
        folder("fd-AAAA2", Some("fd-AREA1"), "b", "zed-folder"),
        doc("d-ZZZZ1", Some("fd-AREA1"), "c", "alpha"),
        doc("d-AAAA1", Some("fd-AREA1"), "d", "zed"),
        doc("d-ZZZZ2", Some("fd-AREA1"), "e", "alpha-gone"),
        doc("d-AAAA2", Some("fd-AREA1"), "f", "zed-gone"),
        figure("fg-AAAA1", "d-ZZZZ1", "a", Hash::of(b"a1")),
        figure("fg-ZZZZ1", "d-AAAA1", "a", Hash::of(b"z1")),
        figure("fg-AAAA2", "d-ZZZZ2", "a", Hash::of(b"a2")),
        figure("fg-ZZZZ2", "d-AAAA2", "a", Hash::of(b"z2")),
        prose("p-ZZZZ1", "d-ZZZZ1", "b", "Z {{d-XC001}}."),
        prose("p-AAAA1", "d-AAAA1", "b", "A {{d-XC001}}."),
        doc("d-XC001", None, "b", "excluded"),
    ];
    let before = res(&st, base.clone(), "pubsite");
    let next = edit(
        &without(&base, &["d-ZZZZ2", "d-AAAA2", "fg-AAAA2", "fg-ZZZZ2"]),
        vec![
            folder("fd-ZZZZ1", Some("fd-AREA1"), "a", "alpha-renamed"),
            folder("fd-AAAA2", Some("fd-AREA1"), "b", "zed-renamed"),
            figure("fg-AAAA1", "d-ZZZZ1", "a", Hash::of(b"a1 next")),
            figure("fg-ZZZZ1", "d-AAAA1", "a", Hash::of(b"z1 next")),
            doc("d-ZZZZ3", Some("fd-AREA1"), "g", "alpha-new"),
            doc("d-AAAA3", Some("fd-AREA1"), "h", "zed-new"),
            figure("fg-AAAA3", "d-ZZZZ3", "a", Hash::of(b"a3")),
            figure("fg-ZZZZ3", "d-AAAA3", "a", Hash::of(b"z3")),
            prose("p-ZZZZ1", "d-ZZZZ1", "b", "Z {{d-XC001}} {{d-XC001}}."),
            prose("p-AAAA1", "d-AAAA1", "b", "A {{d-XC001}} {{d-XC001}}."),
            with_field(
                prose("p-AAAA3", "d-AAAA3", "b", "{{d-XC001}}"),
                "note",
                s("{{d-XC001}}"),
            ),
        ],
    );
    let after = res(&st, next.clone(), "pubsite");
    let d = diff(&before, &after);

    fn sorted<T, K: Ord>(rows: &[T], key: impl Fn(&T) -> K) -> bool {
        rows.windows(2).all(|w| key(&w[0]) < key(&w[1]))
    }
    let ids = |v: Vec<&BlockId>| v.into_iter().cloned().collect::<Vec<_>>();
    assert_eq!(
        ids(d.added_docs.iter().map(|x| &x.id).collect()),
        vec![bid("d-AAAA3"), bid("d-ZZZZ3")]
    );
    assert_eq!(
        ids(d.removed_docs.iter().map(|x| &x.id).collect()),
        vec![bid("d-AAAA2"), bid("d-ZZZZ2")]
    );
    assert_eq!(
        ids(d.changed_docs.iter().map(|x| &x.id).collect()),
        vec![bid("d-AAAA1"), bid("d-ZZZZ1")]
    );
    assert_eq!(
        ids(d.changed_folders.iter().map(|x| &x.id).collect()),
        vec![bid("fd-AAAA2"), bid("fd-ZZZZ1")]
    );
    assert_eq!(
        ids(d.added_attachments.iter().map(|x| &x.block).collect()),
        vec![bid("fg-AAAA3"), bid("fg-ZZZZ3")]
    );
    assert_eq!(
        ids(d.removed_attachments.iter().map(|x| &x.block).collect()),
        vec![bid("fg-AAAA2"), bid("fg-ZZZZ2")]
    );
    assert_eq!(
        ids(d.changed_attachments.iter().map(|x| &x.block).collect()),
        vec![bid("fg-AAAA1"), bid("fg-ZZZZ1")]
    );
    // the resolution's own attachment order is by home doc, which differs here
    assert_eq!(
        after
            .attachments
            .iter()
            .map(|a| a.block.clone())
            .collect::<Vec<_>>(),
        vec![
            bid("fg-ZZZZ1"),
            bid("fg-ZZZZ3"),
            bid("fg-AAAA1"),
            bid("fg-AAAA3")
        ]
    );
    assert!(sorted(&d.new_references, |r| (
        r.source.clone(),
        r.location.clone(),
        r.target.clone(),
        r.classification
    )));
    assert_eq!(
        d.new_references,
        vec![
            row("p-AAAA1", "body", "d-XC001", Classification::Excluded, 1),
            row(
                "p-AAAA3",
                "/fields/note",
                "d-XC001",
                Classification::Excluded,
                1
            ),
            row("p-AAAA3", "body", "d-XC001", Classification::Excluded, 1),
            row("p-ZZZZ1", "body", "d-XC001", Classification::Excluded, 1),
        ]
    );
    assert_eq!(d.new_reference_count, 4);

    // deterministic: the same inputs, resolved again, give the same comparison
    let again = diff(
        &res(&st, base.clone(), "pubsite"),
        &res(&st, next, "pubsite"),
    );
    assert_eq!(again, d);

    // a comparison with itself, or with an equal resolution, is empty
    for (what, a, b) in [
        ("before", &before, &before),
        ("after", &after, &after),
        ("again", &before, &res(&st, base, "pubsite")),
    ] {
        let d = diff(a, b);
        assert_no_change(&d, what);
        assert_eq!(d.profile_id, a.profile.id, "{what}");
        assert_eq!(d.before_profile_version, d.after_profile_version, "{what}");
    }
}
