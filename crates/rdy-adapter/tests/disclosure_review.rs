//! drv7 (D-RDH1C, D-9BWMV, D-Z8Q4N, D-075QD, D-KDFZ3, D-B2AC3): the adapter's publication check
//! carries the disclosure review. Its `review` member is the engine's one JSON form on captured
//! sides; the report is the same form with its outer lists cut. Hashes, selection identities and
//! doc digests are unchanged by the labels, the caps and the serialization.
//!
//! The projects are built with core calls and closed before an adapter handle opens a store.

use rdy_adapter::publication::{AttachmentVariant, PreviousPublication};
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::publication::{self, ADAPTER_REVIEW_BYTES, IMPLICIT_ORG};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;

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
            message: "drv7".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&session(Role::Writer), "work", "drv7", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = session(Role::Writer);
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
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
}

/// The facts a host records of the publication of `profile` at `tag`.
fn record(r: &Rdy, tag: &str, profile: &str) -> PreviousPublication {
    let st = r.read();
    let scope = st.resolve_scope(&format!("tag:{tag}")).unwrap();
    let res = publication::resolve(&st, &scope, profile).unwrap();
    PreviousPublication {
        tag: tag.into(),
        profile_version: res.profile.version,
        selection_identity: res.selection_identity,
    }
}

/// The engine's report of `profile` at tag `scope`, against tag `against` when given.
fn report(r: &Rdy, profile: &str, scope: &str, against: Option<&str>) -> Value {
    let against = against.map(|t| format!("tag:{t}"));
    publication::check(
        &r.read(),
        profile,
        Some(&format!("tag:{scope}")),
        against.as_deref(),
        None,
    )
    .unwrap()
}

fn members(report: &Value) -> Value {
    json!({
        "references": report["references"],
        "disclosure": report["disclosure"],
        "included": report["included"],
    })
}

/// A project with the org profile `site` over the folder `pubs`.
/// v1.0.0: `pubs` holds the doc `base` (a prose, a figure with a dark image) and the doc `gone`.
/// v2.0.0: `base` has other text, `gone` moved out, and `n` docs are added, each with a figure
/// and a prose that cites the doc `outside`, which no profile selects.
fn project(dir: &Path, n: usize) -> Rdy {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let light = r.store_mut().put_blob(b"drv7 light").unwrap();
    let dark = r.store_mut().put_blob(b"drv7 dark").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$base", "parent": "$pubs", "fields": {"name": "base", "title": "Base", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$bp", "parent": "$base", "body": "Base text."},
            {"op": "create", "type": "figure", "id": "$bf", "parent": "$base", "fields": {"blob": light.to_string(), "dark_blob": dark.to_string(), "dark_media_type": "application/octet-stream", "media_type": "application/octet-stream", "alt": "Base figure"}},
            {"op": "create", "type": "doc", "id": "$gone", "parent": "$pubs", "fields": {"name": "gone", "title": "Gone", "doc_type": "page"}},
            {"op": "create", "type": "prose", "parent": "$gone", "body": "Gone text."},
            {"op": "create", "type": "doc", "id": "$outside", "fields": {"name": "outside", "title": "Outside", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = |n: &str| w.created[n].clone();
    write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "parent": id("$admin"),
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id("$pubs")]}}]),
    );
    land(&mut r);
    tag(&mut r, "v1.0.0");

    let mut ops = vec![
        json!({"op": "update", "id": id("$bp"), "body": "Base text, changed."}),
        json!({"op": "move", "id": id("$gone")}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "doc", "id": format!("$add{i}"), "parent": id("$pubs"),
            "fields": {"name": format!("add{i}"), "title": format!("Added {i}"), "doc_type": "page"}}));
        ops.push(json!({"op": "create", "type": "figure", "parent": format!("$add{i}"),
            "fields": {"blob": light.to_string(), "media_type": "application/octet-stream", "alt": format!("Figure {i}")}}));
        ops.push(
            json!({"op": "create", "type": "prose", "parent": format!("$add{i}"),
            "body": format!("See {{{{{}}}}}.", id("$outside"))}),
        );
    }
    write(&mut r, Value::Array(ops));
    land(&mut r);
    tag(&mut r, "v2.0.0");
    r
}

#[test]
fn drv7_adapter_review_equals_the_shared_json_form() {
    let dir = tempfile::tempdir().unwrap();
    let r = project(dir.path(), 3);
    let prev = record(&r, "v1.0.0", "site");
    let against = members(&report(&r, "site", "v2.0.0", Some("v1.0.0")));
    let first = members(&report(&r, "site", "v1.0.0", None));
    let implicit = members(&report(&r, IMPLICIT_ORG, "v2.0.0", None));
    r.close().unwrap();

    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    // below the outer caps, the adapter's review is the report's three members exactly
    let check = snap.disclosure("v2.0.0", "site", Some(&prev)).unwrap();
    assert_eq!(check.review, against);
    assert_eq!(check.review["included"], Value::Null);
    let rows = &check.review["disclosure"];
    assert_eq!(rows["added_docs"].as_array().unwrap().len(), 3);
    assert_eq!(rows["removed_docs"].as_array().unwrap().len(), 1);
    assert_eq!(rows["changed_docs"].as_array().unwrap().len(), 1);
    assert!(rows["totals"].is_object());
    assert_eq!(
        check.review["references"]["rows"].as_array().unwrap().len(),
        3
    );

    let check = snap.disclosure("v1.0.0", "site", None).unwrap();
    assert_eq!(check.review, first);
    assert_eq!(check.review["disclosure"], Value::Null);
    let check = snap.disclosure("v2.0.0", IMPLICIT_ORG, None).unwrap();
    assert_eq!(check.review, implicit);
    handle.close().unwrap();
}

#[test]
fn drv7_adapter_keeps_201_docs_and_reference_rows() {
    let n = 201;
    let dir = tempfile::tempdir().unwrap();
    let r = project(dir.path(), n);
    let prev = record(&r, "v1.0.0", "site");
    let cut = members(&report(&r, "site", "v2.0.0", Some("v1.0.0")));
    r.close().unwrap();

    let handle = StoreHandle::open(dir.path()).unwrap();
    let check = handle
        .snapshot()
        .unwrap()
        .disclosure("v2.0.0", "site", Some(&prev))
        .unwrap();
    let (full, short) = (&check.review, &cut);
    let rows = |v: &Value, a: &str, b: &str| v[a][b].as_array().unwrap().clone();

    // the report keeps 200 rows and counts the rest; the adapter keeps every outer row
    assert_eq!(rows(short, "disclosure", "added_docs").len(), 200);
    assert_eq!(short["disclosure"]["omitted"]["added_docs"], 1);
    assert_eq!(rows(full, "disclosure", "added_docs").len(), n);
    assert_eq!(full["disclosure"]["omitted"]["added_docs"], 0);
    assert_eq!(rows(short, "references", "rows").len(), 200);
    assert_eq!(short["references"]["omitted_rows"], 1);
    assert_eq!(rows(full, "references", "rows").len(), n);
    assert_eq!(full["references"]["omitted_rows"], 0);

    // the report is the adapter's complete review with each outer list cut: the same rows, with
    // the same inner allocation, in the same order, and the same full totals
    for (a, b) in [
        ("disclosure", "added_docs"),
        ("disclosure", "removed_docs"),
        ("disclosure", "changed_docs"),
        ("disclosure", "added_attachments"),
        ("references", "rows"),
    ] {
        let (all, kept) = (rows(full, a, b), rows(short, a, b));
        assert_eq!(kept[..], all[..kept.len()], "{a}/{b}");
    }
    assert_eq!(full["disclosure"]["totals"], short["disclosure"]["totals"]);
    for k in ["status", "excluded", "missing", "total"] {
        assert_eq!(full["references"][k], short["references"][k], "{k}");
    }
    // the typed comparison keeps all of its rows too
    assert_eq!(check.comparison.unwrap().added_docs.len(), n);
    handle.close().unwrap();
}

#[test]
fn drv7_first_review_adds_included_and_keeps_typed_fields() {
    let dir = tempfile::tempdir().unwrap();
    let r = project(dir.path(), 2);
    let prev = record(&r, "v1.0.0", "site");
    r.close().unwrap();

    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let first = snap.disclosure("v1.0.0", "site", None).unwrap();
    let included = first
        .included
        .clone()
        .expect("a first check lists included");
    assert_eq!((included.docs, included.folders), (2, 1));
    assert_eq!(included.rows.len(), 2);
    assert_eq!(
        first.review["included"],
        serde_json::to_value(&included).unwrap()
    );
    let mut keys: Vec<&str> = first
        .review
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, ["disclosure", "included", "references"]);
    assert_eq!(first.review["disclosure"], Value::Null);
    // the old typed fields stay
    assert!(first.comparison.is_none());
    assert!(first.first_attachments.is_some() && first.first_hidden_planned.is_some());
    assert_eq!(first.profile.name, "site");

    // with a previous publication the typed list is absent and the JSON member is null
    let later = snap.disclosure("v2.0.0", "site", Some(&prev)).unwrap();
    assert!(later.included.is_none());
    assert_eq!(later.review["included"], Value::Null);
    assert!(later.review["disclosure"].is_object());
    assert!(later.first_attachments.is_none() && later.first_hidden_planned.is_none());
    handle.close().unwrap();
}

#[test]
fn drv7_first_attachments_keep_old_members_and_add_labels() {
    let dir = tempfile::tempdir().unwrap();
    let r = project(dir.path(), 0);
    let want = {
        let st = r.read();
        let scope = st.resolve_scope("tag:v1.0.0").unwrap();
        publication::resolve(&st, &scope, "site")
            .unwrap()
            .attachments
    };
    r.close().unwrap();
    // the light image and the dark image of one figure are two rows
    assert_eq!(want.len(), 2);

    let handle = StoreHandle::open(dir.path()).unwrap();
    let check = handle
        .snapshot()
        .unwrap()
        .disclosure("v1.0.0", "site", None)
        .unwrap();
    let got = check.first_attachments.unwrap();
    let entries: Vec<_> = got.iter().map(|x| x.entry.clone()).collect();
    assert_eq!(entries, want, "every old member, in the resolution's order");
    let variants: Vec<_> = entries.iter().map(|e| e.variant).collect();
    assert!(variants.contains(&None) && variants.contains(&Some(AttachmentVariant::Dark)));
    for x in &got {
        assert_eq!(x.label, "Base figure");
        assert_eq!(x.home_doc_title.as_deref(), Some("Base"));
    }
    handle.close().unwrap();
}

/// The source project of the captured-tag test: a doc and a canary prose, an org profile,
/// `v1.0.0`; then the removal of the canary and `v2.0.0`.
fn removed_source(dir: &Path) -> (Rdy, Hash, Hash, Vec<u8>) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "parent": w.created["$admin"],
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [w.created["$d"]]}}]),
    );
    land(&mut r);
    let v1 = tag(&mut r, "v1.0.0");
    let before = export(&r, v1);
    remove(&mut r, &w.created["$p"]);
    let v2 = tag(&mut r, "v2.0.0");
    (r, v1, v2, before)
}

fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

fn upload(dir: &Path, name: &str, bytes: &[u8]) -> Vec<File> {
    let p = dir.join(format!("{name}.rdysync"));
    std::fs::write(&p, bytes).unwrap();
    vec![File::open(p).unwrap()]
}

#[test]
fn drv7_adapter_uses_captured_tags_and_refuses_stale_epoch() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, v1, v2, before) = removed_source(&dir.path().join("src"));
    let landing = |t: Hash| r.store().read_tag(&t).unwrap().landing;
    let (l1, l2) = (landing(v1), landing(v2));
    let after = export(&r, v2);
    let repo = r.store().repo_id().unwrap().unwrap();
    let handle = StoreHandle::create_holder(&dir.path().join("holder"), repo).unwrap();
    let files = dir.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    let target = |tag: &str, landing| UploadTarget {
        tag: tag.into(),
        landing,
    };

    handle
        .take(&upload(&files, "v1", &before), target("v1.0.0", l1))
        .unwrap();
    let snap = handle.snapshot().unwrap();
    let prev = {
        let rows = snap.profiles("v1.0.0").unwrap();
        let site = rows.iter().find(|p| p.name == "site").unwrap();
        PreviousPublication {
            tag: "v1.0.0".into(),
            profile_version: site.version,
            selection_identity: site.selection_identity,
        }
    };
    let ok = snap.disclosure("v1.0.0", "site", None).unwrap();
    assert!(ok.review["included"].is_object());

    // the holder takes v2 and its removal: the snapshot reads only its captured tags, and every
    // content read of it is refused before and after the review is built
    handle
        .take(&upload(&files, "v2", &after), target("v2.0.0", l2))
        .unwrap();
    for e in [
        snap.disclosure("v1.0.0", "site", None).unwrap_err(),
        snap.disclosure("v1.0.0", "site", Some(&prev)).unwrap_err(),
        snap.disclosure("v2.0.0", "site", None).unwrap_err(),
    ] {
        assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
    }
    // a snapshot taken after the take reads both tags; the review is whole and holds no canary
    let fresh = handle.snapshot().unwrap();
    let now = fresh.disclosure("v2.0.0", "site", Some(&prev)).unwrap();
    assert!(now.review["disclosure"].is_object());
    assert!(!now.review.to_string().contains("ZEBRA-7731"));
    assert!(fresh.disclosure("v3.0.0", "site", None).is_err());
    handle.close().unwrap();
    r.close().unwrap();
}

#[test]
fn drv7_review_preserves_binding_hashes_and_doc_digests() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = project(dir.path(), 2);
    let prev = record(&r, "v1.0.0", "site");
    let (old, new) = {
        let st = r.read();
        let at = |t: &str| {
            let scope = st.resolve_scope(&format!("tag:{t}")).unwrap();
            publication::resolve(&st, &scope, "site").unwrap()
        };
        (at("v1.0.0"), at("v2.0.0"))
    };
    let diff = publication::disclosure_diff(&old, &new).unwrap();
    // a removal after the binding, of the text of a doc the check lists
    let secret = new
        .m
        .iter()
        .find(|b| b.body == "Base text, changed.")
        .unwrap()
        .id
        .to_string();
    remove(&mut r, &secret);
    r.close().unwrap();

    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let check = snap.disclosure("v2.0.0", "site", Some(&prev)).unwrap();
    // the binding survives the later removal, and no recipe moved for the review
    assert_eq!(check.profile.selection_identity, new.selection_identity);
    let first = snap.disclosure("v1.0.0", "site", None).unwrap();
    assert_eq!(first.profile.selection_identity, old.selection_identity);
    assert_eq!(first.profile.version, old.profile.version);
    // the typed comparison and the JSON rows carry the doc digests the engine computes
    let typed = check.comparison.as_ref().unwrap();
    assert_eq!(typed.added_docs, diff.added_docs);
    assert_eq!(typed.removed_docs, diff.removed_docs);
    let mut got: Vec<String> = check.review["disclosure"]["added_docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["digest"].as_str().unwrap().to_string())
        .collect();
    let mut want: Vec<String> = diff
        .added_docs
        .iter()
        .map(|d| d.digest.to_string())
        .collect();
    want.sort();
    got.sort();
    assert_eq!(got, want);
    // a review with no comparison adds no hash of its own: the profile facts are the engine's
    let rows = snap.profiles("v2.0.0").unwrap();
    let site = rows.iter().find(|p| p.name == "site").unwrap();
    assert_eq!(site.selection_identity, check.profile.selection_identity);
    assert_eq!(site.selection_hash, check.profile.selection_hash);
    handle.close().unwrap();
}

#[test]
fn drv7_adapter_limit_never_returns_a_partial_review() {
    assert_eq!(ADAPTER_REVIEW_BYTES, 134_217_728);
    let dir = tempfile::tempdir().unwrap();
    let r = project(dir.path(), 3);
    let prev = record(&r, "v1.0.0", "site");
    r.close().unwrap();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let check = snap.disclosure("v2.0.0", "site", Some(&prev)).unwrap();
    let size = serde_json::to_vec(&check.review).unwrap().len();
    assert!(size < ADAPTER_REVIEW_BYTES);
    // the shared limit check refuses a value over the bound whole, with the report's words
    let e = publication::fit_within(check.review.clone(), size - 1, 0).unwrap_err();
    assert_eq!(e.code, "E_LIMIT");
    assert_eq!(
        e.message,
        format!(
            "Publication report exceeds the limit of {} bytes.",
            size - 1
        )
    );
    assert_eq!(e.fix, "Reduce the selection or compare smaller profiles.");
    assert!(publication::fit_within(check.review, size, 0).is_ok());
    handle.close().unwrap();
}
