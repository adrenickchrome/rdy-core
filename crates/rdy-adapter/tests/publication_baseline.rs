//! scc7 (D-CDQF7, D-6WD17): the adapter makes a portable publication baseline at a captured tag
//! and checks a candidate against it. The host's record of the publication the baseline came
//! from binds it: a mismatch is refused before any comparison, and a same-store check keeps its
//! old call and output.
//!
//! The project is built with core calls and closed before an adapter handle opens the store.

use rdy_adapter::publication::{Baseline, BaselineBinding, PreviousChoice, PreviousPublication};
use rdy_adapter::{Hash, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use serde_json::{Value, json};
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
            message: "scc7".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&session(Role::Writer), "work", "scc7", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

/// A project with the org profiles `site` over `pubs` and `other` over `outside`.
/// v1.0.0: `pubs` holds the doc `base`. v2.0.0: `base` has other text and `added` is added.
fn project(dir: &Path) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "folder", "id": "$pubs", "fields": {"name": "pubs"}},
            {"op": "create", "type": "doc", "id": "$base", "parent": "$pubs", "fields": {"name": "base", "title": "Base", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$bp", "parent": "$base", "body": "Base text."},
            {"op": "create", "type": "doc", "id": "$outside", "fields": {"name": "outside", "title": "Outside", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = |n: &str| w.created[n].clone();
    write(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id("$pubs")]}},
            {"op": "create", "type": "publication_profile", "parent": id("$admin"),
             "fields": {"key": "other", "title": "Other", "audience": "org", "select": [id("$outside")]}}
        ]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    write(
        &mut r,
        json!([
            {"op": "update", "id": id("$bp"), "body": "Base text, changed."},
            {"op": "create", "type": "doc", "parent": id("$pubs"), "fields": {"name": "added", "title": "Added", "doc_type": "page"}}
        ]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    r.close().unwrap();
}

fn binding(b: &Baseline) -> BaselineBinding {
    BaselineBinding {
        repo_id: b.repo_id(),
        tag: b.tag().to_string(),
        profile_version: b.profile_version(),
        selection_identity: b.selection_identity(),
    }
}

#[test]
fn scc7_adapter_binding_is_checked_before_compare() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path());
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let baseline = snap.publication_baseline("v1.0.0", "site").unwrap();
    assert_eq!(baseline.tag(), "v1.0.0");
    // the bytes travel: what reads back is the same baseline
    let baseline = Baseline::read(&baseline.to_bytes()).unwrap();
    let good = binding(&baseline);

    // a record of another publication is refused, even where the profile would be refused too
    let wrong = BaselineBinding {
        selection_identity: Hash::of(b"another identity"),
        ..good.clone()
    };
    for profile in ["site", "other"] {
        let e = snap
            .disclosure_against(
                "v2.0.0",
                profile,
                Some(PreviousChoice::Baseline {
                    baseline: &baseline,
                    binding: &wrong,
                }),
            )
            .unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (
                "E_PUBLICATION_BASELINE",
                format!(
                    "The publication baseline does not match the previous publication v1.0.0 of {}.",
                    baseline.repo_id()
                )
                .as_str()
            ),
            "{profile}"
        );
    }
    // with the right record, another profile is refused by the comparison's own text
    let e = snap
        .disclosure_against(
            "v2.0.0",
            "other",
            Some(PreviousChoice::Baseline {
                baseline: &baseline,
                binding: &good,
            }),
        )
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_BAD_INPUT",
            "Disclosure comparison requires the same publication profile."
        )
    );

    // the right record compares: the same rows as the same-store check, and the baseline named
    let check = snap
        .disclosure_against(
            "v2.0.0",
            "site",
            Some(PreviousChoice::Baseline {
                baseline: &baseline,
                binding: &good,
            }),
        )
        .unwrap();
    let rows = snap.profiles("v1.0.0").unwrap();
    let site = rows.iter().find(|p| p.name == "site").unwrap();
    let prev = PreviousPublication {
        tag: "v1.0.0".into(),
        profile_version: site.version,
        selection_identity: site.selection_identity,
    };
    let same = snap.disclosure("v2.0.0", "site", Some(&prev)).unwrap();
    let same_against = snap
        .disclosure_against("v2.0.0", "site", Some(PreviousChoice::SameStore(&prev)))
        .unwrap();
    assert_eq!(same, same_against);
    assert!(same.review["disclosure"].get("baseline").is_none());
    let mut cmp = check.comparison.clone().unwrap();
    let named = cmp.baseline.take().unwrap();
    assert_eq!(named.baseline_hash, baseline.baseline_hash());
    assert_eq!(Some(cmp), same.comparison);
    assert_eq!(
        check.review["disclosure"]["baseline"]["tag"],
        json!("v1.0.0")
    );
    assert_eq!(
        check.review["disclosure"]["redactions"],
        json!({"candidate": [], "baseline": []})
    );
    assert_eq!(check.first_attachments, None);
    handle.close().unwrap();
}
