//! hpb8 (D-Z8Q4N, D-075QD, D-3M81W, D-KDFZ3, D-0VCQ5): the disclosure comparison shows a doc or
//! folder whose name a removal took by its full id, matched by the removed version, never by id
//! alone; a newer version that no removal names keeps its name. Ids, digests and selection hashes
//! stay the recorded facts, every row says whether a name was replaced, and the applied removal
//! requests are listed sorted. The report's reference object is one helper's projection.
mod common;

use rdy_engine::publication::{self, ChangedDoc, ChangedFolder, DocEntry, Resolved};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

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

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(
            &session(Role::Writer),
            view,
            "publication",
            Some(common::TEST_SUMMARY),
        )
        .unwrap();
    common::confirm_exceptions(r, &session(Role::Approver), &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

/// Records one removal request of `targets` and lands it with a person's approval: the real
/// path. Returns the request.
fn redact(r: &mut Rdy, targets: &[String]) -> Hash {
    let res = r
        .redact_in(
            &session(Role::Writer),
            "redact",
            RedactInput {
                targets: targets.to_vec(),
                reason_class: "secret".into(),
                reason: "a leaked name".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let request = res.redaction.expect("a request is recorded");
    let p = r
        .propose(
            &session(Role::Writer),
            "redact",
            "Remove",
            Some("test summary"),
        )
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    request
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn resolve(r: &Rdy, scope: &str, profile: &str) -> Resolved {
    let st = r.read();
    let scope = st.resolve_scope(scope).unwrap();
    publication::resolve(&st, &scope, profile).unwrap()
}

fn doc_entry(r: &Resolved, id: &BlockId) -> DocEntry {
    r.docs
        .iter()
        .find(|d| &d.id == id)
        .unwrap_or_else(|| panic!("{id} is a selected doc"))
        .clone()
}

/// The removed text this test writes: no output may hold any of it.
const SENTINELS: [&str; 4] = [
    "SENTINEL-ANCESTOR",
    "SENTINEL-DOCNAME",
    "SENTINEL-FOLDERNAME",
    "SENTINEL-BODY",
];
/// The name of folder `n`'s first version, which only that version's removal takes.
const OLD_NAME: &str = "SENTINEL-OLDNAME";

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb8_comparison_hides_removed_names_by_version() {
    let (_dir, mut r) = project();
    // public-area/{D SENTINEL-DOCNAME, F SENTINEL-FOLDERNAME/{G inner}, N SENTINEL-OLDNAME/{K
    // kept}, Q quiet}; SENTINEL-ANCESTOR/{A visible-a (prose X), R leaving}
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$p", "fields": {"name": "public-area"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$p", "fields": {"name": "SENTINEL-DOCNAME", "title": "Doc", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$f", "parent": "$p", "fields": {"name": "SENTINEL-FOLDERNAME"}},
            {"op": "create", "type": "doc", "id": "$g", "parent": "$f", "fields": {"name": "inner", "title": "Inner", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$n", "parent": "$p", "fields": {"name": OLD_NAME}},
            {"op": "create", "type": "doc", "id": "$k", "parent": "$n", "fields": {"name": "kept", "title": "Kept", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$q", "parent": "$p", "fields": {"name": "quiet", "title": "Quiet", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$t", "fields": {"name": "SENTINEL-ANCESTOR"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$t", "fields": {"name": "visible-a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$a", "body": "SENTINEL-BODY text."},
            {"op": "create", "type": "doc", "id": "$r", "parent": "$t", "fields": {"name": "leaving", "title": "Leaving", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    let (p, d, f, g, n, k, q, t, a, x, rr) = (
        id("$p"),
        id("$d"),
        id("$f"),
        id("$g"),
        id("$n"),
        id("$k"),
        id("$q"),
        id("$t"),
        id("$a"),
        id("$x"),
        id("$r"),
    );
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": id("$admin").to_string(),
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [p.to_string(), t.to_string()]}}]),
    );
    land(&mut r, "work");
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    // the facts a hub recorded before any removal
    let pre = resolve(&r, "tag:v1.0.0", "site");
    let n_v1 = pre.m.version_of(&n).unwrap();
    assert!(pre.docs.iter().all(|e| !e.redacted));

    // v2: N takes a new name; A gains a child; R leaves; `fresh` arrives under T
    let w2 = write(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": n.to_string(), "fields": {"name": "renamed-visible"}},
            {"op": "create", "type": "prose", "parent": a.to_string(), "body": "Added text."},
            {"op": "move", "id": rr.to_string()},
            {"op": "create", "type": "doc", "id": "$fresh", "parent": t.to_string(), "fields": {"name": "fresh", "title": "Fresh", "doc_type": "page"}}
        ]),
    );
    let fresh = BlockId::parse(&w2.created["$fresh"]).unwrap();
    land(&mut r, "work");
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();

    // the removals: whole blocks T, D and F, the prose X, and only N's first version
    let mut requests = vec![
        redact(&mut r, &[t.to_string()]),
        redact(&mut r, &[d.to_string(), f.to_string()]),
        redact(&mut r, &[format!("{n}@{n_v1}")]),
        redact(&mut r, &[x.to_string()]),
    ];
    requests.sort();
    let before = resolve(&r, "tag:v1.0.0", "site");
    let after = resolve(&r, "tag:v2.0.0", "site");

    // the resolutions mark each doc whose own name or an ancestor's was replaced
    let flag = |r: &Resolved, id: &BlockId| doc_entry(r, id).redacted;
    for (id, want) in [
        (&d, true),
        (&g, true),
        (&a, true),
        (&fresh, true),
        (&k, false),
        (&q, false),
    ] {
        assert_eq!(flag(&after, id), want, "after {id}");
    }
    assert!(flag(&before, &k), "N's first version was removed");
    assert_eq!(doc_entry(&before, &k).path, format!("public-area/{n}/kept"));
    assert_eq!(
        doc_entry(&after, &k).path,
        "public-area/renamed-visible/kept"
    );

    // fresh sides
    let dd = publication::disclosure_diff(&before, &after).unwrap();
    assert_eq!(
        dd.redactions, requests,
        "the sorted union of applied requests"
    );
    assert_eq!(
        (dd.before_selection_hash, dd.after_selection_hash),
        (before.selection_hash, after.selection_hash)
    );
    assert_eq!(
        (dd.before_selection_identity, dd.after_selection_identity),
        (before.selection_identity, after.selection_identity)
    );
    let fresh_entry = doc_entry(&after, &fresh);
    assert_eq!(
        dd.added_docs,
        vec![DocEntry {
            id: fresh.clone(),
            path: format!("{t}/fresh"),
            digest: fresh_entry.digest,
            redacted: true,
        }]
    );
    assert_eq!(
        dd.removed_docs,
        vec![DocEntry {
            id: rr.clone(),
            path: format!("{t}/leaving"),
            digest: doc_entry(&before, &rr).digest,
            redacted: true,
        }]
    );
    let changed = |id: &BlockId, before_path: String, after_path: String| ChangedDoc {
        id: id.clone(),
        before_path,
        after_path,
        before_digest: doc_entry(&before, id).digest,
        after_digest: doc_entry(&after, id).digest,
        redacted: true,
    };
    let mut want = vec![
        changed(&a, format!("{t}/visible-a"), format!("{t}/visible-a")),
        changed(
            &k,
            format!("public-area/{n}/kept"),
            "public-area/renamed-visible/kept".into(),
        ),
    ];
    want.sort_by(|x, y| x.id.cmp(&y.id));
    assert_eq!(dd.changed_docs, want);
    // the newer, unremoved version of N keeps its name; the removed one shows its id
    assert_eq!(
        dd.changed_folders,
        vec![ChangedFolder {
            id: n.clone(),
            before_version: Some(n_v1),
            after_version: after.m.version_of(&n),
            before_path: Some(format!("public-area/{n}")),
            after_path: Some("public-area/renamed-visible".into()),
            redacted: true,
        }]
    );
    let shown = format!("{dd:?}");
    for word in SENTINELS.iter().chain([&OLD_NAME]) {
        assert!(!shown.contains(word), "{word} in {shown}");
    }
    assert!(shown.contains("renamed-visible"));

    // the public report: no removed name anywhere, lowercase sorted requests, identities
    let v = publication::check(&r.read(), "site", Some("tag:v2.0.0"), Some("tag:v1.0.0")).unwrap();
    let text = v.to_string();
    for word in SENTINELS.iter().chain([&OLD_NAME]) {
        assert!(!text.contains(word), "{word} in {text}");
    }
    let disc = &v["disclosure"];
    assert_eq!(
        disc["redactions"],
        json!(requests.iter().map(ToString::to_string).collect::<Vec<_>>())
    );
    assert!(
        disc["redactions"].as_array().unwrap().iter().all(|h| h
            .as_str()
            .unwrap()
            .bytes()
            .all(|b| !b.is_ascii_uppercase()))
    );
    assert_eq!(disc["omitted"]["redactions"], 0);
    assert_eq!(
        disc["before_selection_identity"],
        before.selection_identity.to_string()
    );
    assert_eq!(
        disc["after_selection_identity"],
        after.selection_identity.to_string()
    );
    assert_eq!(
        v["selection"]["identity"],
        after.selection_identity.to_string()
    );
    assert_eq!(disc["changed_folders"][0]["redacted"], true);
    assert_eq!(disc["added_docs"][0]["redacted"], true);
    assert_eq!(disc["removed_docs"][0]["path"], format!("{t}/leaving"));

    // a supplied pre-removal side: each name whose version the other side holds removed is
    // replaced too; ids and digests stay what the pre-removal side recorded
    let dp = publication::disclosure_diff(&pre, &after).unwrap();
    assert_eq!(dp.before_selection_hash, pre.selection_hash);
    assert_eq!(dp.before_selection_identity, pre.selection_identity);
    assert_eq!(
        dp.redactions, after.redactions,
        "the requests these resolutions apply"
    );
    let row = |id: &BlockId| {
        dp.changed_docs
            .iter()
            .find(|c| &c.id == id)
            .unwrap_or_else(|| panic!("{id} changed"))
            .clone()
    };
    assert_eq!(row(&d).before_path, format!("public-area/{d}"));
    assert_eq!(row(&d).after_path, format!("public-area/{d}"));
    assert_eq!(row(&d).before_digest, doc_entry(&pre, &d).digest);
    assert!(row(&d).redacted);
    assert_eq!(row(&g).before_path, format!("public-area/{f}/inner"));
    assert!(row(&g).redacted);
    assert_ne!(row(&g).before_digest, row(&g).after_digest);
    assert_eq!(row(&a).before_path, format!("{t}/visible-a"));
    assert_eq!(dp.removed_docs[0].path, format!("{t}/leaving"));
    assert!(dp.removed_docs[0].redacted);
    let folder = dp
        .changed_folders
        .iter()
        .find(|c| c.id == f)
        .expect("F's removal is a folder change");
    assert_eq!(
        (folder.before_path.as_deref(), folder.after_path.as_deref()),
        (
            Some(format!("public-area/{f}").as_str()),
            Some(format!("public-area/{f}").as_str())
        )
    );
    assert!(folder.redacted);
    let quiet: BTreeSet<&BlockId> = dp.changed_docs.iter().map(|c| &c.id).collect();
    assert!(!quiet.contains(&q), "an unaffected doc has no row");
    let shown = format!("{dp:?}");
    for word in SENTINELS {
        assert!(!shown.contains(word), "{word} in {shown}");
    }
}

#[test]
fn hpb8_reference_summary_matches_report_projection() {
    for n in [200usize, 201] {
        let (_dir, mut r) = project();
        let w = write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "doc", "id": "$in", "fields": {"name": "in", "title": "In", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
            ]),
        );
        let (inside, out, admin) = (
            w.created["$in"].clone(),
            w.created["$out"].clone(),
            w.created["$admin"].clone(),
        );
        let mut ops: Vec<Value> = (0..n)
            .map(|i| {
                json!({"op": "create", "type": "prose", "parent": inside,
                    "body": format!("Ref {i} to {{{{{out}}}}}.")})
            })
            .collect();
        ops.push(
            json!({"op": "create", "type": "publication_profile", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [inside]}}),
        );
        write(&mut r, "work", Value::Array(ops));
        land(&mut r, "work");
        let resolved = resolve(&r, "canon", "site");
        let summary = publication::reference_summary(&resolved.references);
        for against in [None, Some("canon")] {
            let v = publication::check(&r.read(), "site", None, against).unwrap();
            assert_eq!(summary, v["references"], "{n} {against:?}");
            // the same keys in the same order, byte for byte
            assert_eq!(summary.to_string(), v["references"].to_string());
        }
        let keys: Vec<&str> = summary
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "excluded",
                "missing",
                "omitted_rows",
                "rows",
                "status",
                "total"
            ]
        );
        let rows = summary["rows"].as_array().unwrap();
        assert_eq!(rows.len(), n.min(200));
        assert_eq!(summary["omitted_rows"], n - n.min(200));
        assert_eq!(summary["total"], n);
        assert_eq!(summary["excluded"], n);
        assert_eq!(summary["status"], "warning");
        for (row, want) in rows.iter().zip(&resolved.references.rows) {
            let row_keys: Vec<&str> = row
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                row_keys,
                [
                    "classification",
                    "location",
                    "occurrences",
                    "source",
                    "target"
                ]
            );
            assert_eq!(row["source"], want.source.to_string());
            assert_eq!(row["location"], want.location);
            assert_eq!(row["target"], want.target);
            assert_eq!(row["classification"], want.classification.as_str());
            assert_eq!(row["occurrences"], want.occurrences);
        }
    }
}
