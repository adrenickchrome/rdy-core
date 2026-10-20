//! spv1 (D-CWC91): a profile is inspected in a review scope. `resolve_inspection` and the
//! publication check read canon, a tag or landing, a view and a proposal version, each at its own
//! profile revision; the baseline stays stable. `resolve_revision` is empty only for an absent
//! id, `profile_at` reads metadata alone, and the live loader and warning keep the output gate.
mod common;
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::publication::{
    self, CheckStatus, Limits, PublicationWarning, ReadPublicationSource, Resolved,
};
use rdy_engine::*;
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

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

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    r.propose(
        &session(Role::Writer),
        view,
        "inspection",
        Some(common::TEST_SUMMARY),
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str) {
    let p = propose(r, view);
    common::confirm_exceptions(r, &session(Role::Approver), &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn id(w: &WriteResult, name: &str) -> String {
    w.created[name].clone()
}

fn entry_ids(res: &Resolved) -> Vec<String> {
    res.entries.iter().map(|e| e.id.to_string()).collect()
}

fn sorted(ids: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

fn assert_limit(e: &RdyError, part: &str, limit: u64, unit: &str) {
    assert_err(
        e,
        E_LIMIT,
        &format!("Publication {part} exceeds the limit of {limit} {unit}."),
        "Reduce the selection or split the source project.",
    );
}

/// The stable entry's refusal of a review scope, unchanged.
const STABLE_SCOPE: (&str, &str) = (
    "A publication profile requires canon, a tag, or a landing.",
    "Use scope canon, tag:<name>, or landing:<hash>.",
);

/// The inspection-scope refusal (D-CWC91).
const INSPECTION_SCOPE: (&str, &str) = (
    "Publication check scope must be canon, tag:<name>, landing:<hash>, a view, or proposal:P-x@n.",
    "Use canon, tag:<name>, landing:<hash>, view:<name>, or proposal:P-x@n.",
);

/// The baseline refusal, unchanged.
const BASELINE: (&str, &str) = (
    "Publication check against must be canon, tag:<name>, or landing:<hash>.",
    "Use canon, tag:<name>, or landing:<hash>.",
);

const PUBLIC_FIX: &str =
    "Include the target docs or remove these references. Run the publication check again.";
const ORG_FIX: &str = "Review these references before you share the output.";

/// `P-x@n` without its version: the latest-version form.
fn latest_form(r: &str) -> String {
    r.split_once('@').unwrap().0.to_string()
}

/// The source the store reads publications through, with `st`'s captured settings.
fn with_source<T>(st: &ReadState, f: impl FnOnce(&ReadPublicationSource<'_>) -> T) -> T {
    let (title, description) = (st.project_name(), st.project_description());
    f(&ReadPublicationSource::with_settings(
        st,
        st.pack(),
        &title,
        description.as_deref(),
    ))
}

/// Every file of the project but the daemon's run dir and the rebuildable index, by path, with
/// its bytes: the log, refs and files an inspection must not change.
fn fingerprint(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            if rel == ".rdy/run" || rel.starts_with(".rdy/run/") || rel.contains("index.redb") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(rel, bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    assert!(
        out.keys().any(|k| k.starts_with(".rdy/store/segments/")),
        "the log is in the print"
    );
    out
}

/// The longest body or string field value of the selected blocks: the figure's blob hash here.
fn longest_text(res: &Resolved) -> u64 {
    fn walk(v: &FieldValue, out: &mut u64) {
        match v {
            FieldValue::Str(x) => *out = (*out).max(x.len() as u64),
            FieldValue::List(xs) => xs.iter().for_each(|x| walk(x, out)),
            FieldValue::Map(xs) => xs.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = 0;
    for b in res.m.iter() {
        out = out.max(b.body.len() as u64);
        b.fields.values().for_each(|v| walk(v, &mut out));
    }
    out
}

/// UTF-8 bytes of every selected body and string field value together, plus the chosen
/// profile's title and description, which the scan reads though the profile is not selected.
fn total_text(res: &Resolved) -> u64 {
    fn walk(v: &FieldValue, out: &mut u64) {
        match v {
            FieldValue::Str(x) => *out += x.len() as u64,
            FieldValue::List(xs) => xs.iter().for_each(|x| walk(x, out)),
            FieldValue::Map(xs) => xs.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = 0;
    for b in res.m.iter() {
        out += b.body.len() as u64;
        b.fields.values().for_each(|v| walk(v, &mut out));
    }
    out + res.profile.title.len() as u64
        + res
            .profile
            .description
            .as_ref()
            .map_or(0, |d| d.len() as u64)
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn spv1_each_scope_reads_its_profile_revision() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$c", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "D", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "e", "title": "E", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [a, b, c, d, e, admin] = ["$a", "$b", "$c", "$d", "$e", "$admin"].map(|n| id(&w, n));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
            "fields": {"key": "site", "title": "Tagged", "audience": "org", "select": [a]}}]),
    );
    let pf = id(&w, "$pf");
    land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    let update = |title: &str, select: &[&str]| json!([{"op": "update", "id": pf, "fields": {"title": title, "select": select}}]);
    write(&mut r, "work", update("Canon", &[&b]));
    land(&mut r, "work");
    // a view's unlanded revision, and two versions of a proposal from another view
    write(&mut r, "draft", update("Draft", &[&c]));
    write(&mut r, "prop", update("First", &[&d]));
    let p1 = propose(&mut r, "prop");
    write(&mut r, "prop", update("Second", &[&e]));
    let p2 = propose(&mut r, "prop");
    assert_eq!((p1.version, p2.version), (1, 2));

    let st = r.read();
    let at = |text: &str| st.resolve_scope(text).unwrap();
    let cases: Vec<(&str, Scope, &str, &str)> = vec![
        ("canon", Scope::Canon, b.as_str(), "Canon"),
        ("tag:v1.0.0", at("tag:v1.0.0"), a.as_str(), "Tagged"),
        ("draft", Scope::View("draft".into()), c.as_str(), "Draft"),
        (
            "pinned",
            at(&format!("proposal:{}", p1.r#ref)),
            d.as_str(),
            "First",
        ),
        (
            "latest",
            at(&format!("proposal:{}", latest_form(&p2.r#ref))),
            e.as_str(),
            "Second",
        ),
    ];
    let mut hashes = vec![];
    for (name, scope, doc, title) in &cases {
        let m = st.materialized(scope).unwrap();
        let res = publication::resolve_inspection(&st, scope, "site").unwrap();
        assert_eq!(entry_ids(&res), vec![doc.to_string()], "{name}");
        assert_eq!(res.profile.id, Some(bid(&pf)), "{name}");
        assert_eq!(res.profile.title, *title, "{name}");
        assert_eq!(res.profile.version, m.version_of(&bid(&pf)), "{name}");
        // the same revision by id, and through the revision lookup
        let by_id = publication::resolve_inspection(&st, scope, &pf).unwrap();
        assert_eq!(by_id.selection_hash, res.selection_hash, "{name}");
        let rev = publication::resolve_revision(&st, scope, &bid(&pf))
            .unwrap()
            .unwrap();
        assert_eq!(rev.selection_hash, res.selection_hash, "{name}");
        assert_eq!(rev.profile, res.profile, "{name}");
        // the metadata alone is the same revision's
        assert_eq!(
            publication::profile_at(&st, scope, "site").unwrap(),
            res.profile,
            "{name}"
        );
        hashes.push(res.selection_hash);
    }
    let mut distinct = hashes.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        cases.len(),
        "each scope selects its own set"
    );

    // canon and the tag resolve as the stable entry resolves them
    for (_, scope, _, _) in &cases[..2] {
        let stable = publication::resolve(&st, scope, "site").unwrap();
        let inspected = publication::resolve_inspection(&st, scope, "site").unwrap();
        assert_eq!(stable.selection_hash, inspected.selection_hash);
        assert_eq!(stable.profile, inspected.profile);
    }
    // the stable entry still refuses every review scope, before any read
    for (_, scope, _, _) in &cases[2..] {
        let err = publication::resolve(&st, scope, "site").unwrap_err();
        assert_err(&err, E_BAD_INPUT, STABLE_SCOPE.0, STABLE_SCOPE.1);
    }

    // the check reads each candidate at its own revision
    for (text, doc) in [
        ("canon", &b),
        ("tag:v1.0.0", &a),
        ("draft", &c),
        ("view:draft", &c),
        (&*format!("proposal:{}", p1.r#ref), &d),
        (&*format!("proposal:{}", p2.r#ref), &e),
        (&*format!("proposal:{}", latest_form(&p2.r#ref)), &e),
    ] {
        let v = publication::check(&st, "site", Some(text), None).unwrap();
        assert_eq!(
            v["selection"]["entries"][0]["id"],
            doc.as_str(),
            "{text}: {v}"
        );
        assert_eq!(v["selection"]["blocks"], 1, "{text}");
        assert_eq!(v["profile"]["id"], pf.as_str(), "{text}");
    }
}

#[test]
fn spv1_missing_revision_is_empty_only_for_absence() {
    let (dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$lone", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [a, ap, lone, admin] = ["$a", "$ap", "$lone", "$admin"].map(|n| id(&w, n));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a, lone]}}]),
    );
    let pf = id(&w, "$pf");
    land(&mut r, "work");
    // a profile only a view holds
    let w = write(
        &mut r,
        "fresh",
        json!([{"op": "create", "type": "publication_profile", "id": "$p2", "parent": admin,
            "fields": {"key": "fresh", "title": "Fresh", "audience": "org", "select": [a]}}]),
    );
    let p2 = id(&w, "$p2");
    // a view whose selected doc is gone, and one whose selector names a block inside a doc
    write(&mut r, "gone", json!([{"op": "delete", "id": lone}]));
    write(
        &mut r,
        "inner",
        json!([{"op": "update", "id": pf, "fields": {"select": [ap]}}]),
    );

    let st = r.read();
    let fresh = Scope::View("fresh".into());
    // absence alone is None, in canon and in a proposal-free view alike
    assert!(
        publication::resolve_revision(&st, &Scope::Canon, &bid(&p2))
            .unwrap()
            .is_none()
    );
    assert!(
        publication::resolve_revision(&st, &Scope::Canon, &bid("pf-ZZZZZ"))
            .unwrap()
            .is_none()
    );
    let got = publication::resolve_revision(&st, &fresh, &bid(&p2))
        .unwrap()
        .unwrap();
    assert_eq!(got.profile.key.as_deref(), Some("fresh"));
    assert_eq!(entry_ids(&got), sorted(&[&a, &ap]));
    // a block of another type is the existing error, not an absence
    let err = publication::resolve_revision(&st, &Scope::Canon, &bid(&a)).unwrap_err();
    assert_err(
        &err,
        E_BAD_INPUT,
        &format!("Block {a} is not a publication profile."),
        "Use a publication profile id or key.",
    );
    // a missing selected doc and an invalid selector are the selection's errors
    let err =
        publication::resolve_revision(&st, &Scope::View("gone".into()), &bid(&pf)).unwrap_err();
    assert_err(
        &err,
        E_NOT_FOUND,
        &format!("Publication selector {lone} was not found in this scope."),
        "Remove this selector or choose a scope that contains it.",
    );
    let err =
        publication::resolve_revision(&st, &Scope::View("inner".into()), &bid(&pf)).unwrap_err();
    assert_err(
        &err,
        E_BAD_INPUT,
        &format!("Publication selector {ap} must name a doc or folder."),
        "Select the containing doc or folder.",
    );
    drop(st);

    // a removed profile is the existing error, not an absence
    r.close().unwrap();
    authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::AllVersions(bid(&pf))],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let err = publication::resolve_revision(&st, &Scope::Canon, &bid(&pf)).unwrap_err();
    assert_err(
        &err,
        E_BAD_INPUT,
        &format!("Publication profile {pf} has been redacted."),
        "Choose a profile whose content is available.",
    );
    let err = publication::profile_at(&st, &Scope::Canon, &pf).unwrap_err();
    assert_err(
        &err,
        E_BAD_INPUT,
        &format!("Publication profile {pf} has been redacted."),
        "Choose a profile whose content is available.",
    );
}

#[test]
fn spv1_check_candidate_widens_but_baseline_does_not() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$c", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [a, b, c, admin] = ["$a", "$b", "$c", "$admin"].map(|n| id(&w, n));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}]),
    );
    let pf = id(&w, "$pf");
    land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    let select = |s: &[&str]| json!([{"op": "update", "id": pf, "fields": {"select": s}}]);
    write(&mut r, "draft", select(&[&a, &b]));
    write(&mut r, "prop", select(&[&c]));
    let p1 = propose(&mut r, "prop");
    write(&mut r, "prop", select(&[&b, &c]));
    let p2 = propose(&mut r, "prop");

    let st = r.read();
    let head = st.canon_head().unwrap().to_string();
    let tagged = match st.resolve_scope("tag:v1.0.0").unwrap() {
        Scope::At(h) => h.to_string(),
        other => panic!("{other:?}"),
    };
    let check = |scope: Option<&str>| publication::check(&st, "site", scope, None).unwrap();

    // the exact candidate objects
    let canon = json!({"kind": "canon", "landing": head});
    let view = json!({"kind": "view", "landing": null, "view": "draft"});
    let pinned = json!({"kind": "proposal", "landing": null, "proposal": p1.r#ref});
    let latest = json!({"kind": "proposal", "landing": null, "proposal": p2.r#ref});
    for (text, want) in [
        (None, &canon),
        (Some("canon"), &canon),
        (Some("draft"), &view),
        (Some("view:draft"), &view),
        (Some(&*format!("proposal:{}", p1.r#ref)), &pinned),
        (Some(&*format!("proposal:{}", p2.r#ref)), &latest),
        (
            Some(&*format!("proposal:{}", latest_form(&p2.r#ref))),
            &latest,
        ),
    ] {
        let v = check(text);
        assert_eq!(&v["scope"], want, "{text:?}");
        assert!(v["against"].is_null() && v["disclosure"].is_null());
    }
    assert_eq!(
        check(Some("tag:v1.0.0"))["scope"],
        json!({"kind": "landing", "landing": tagged})
    );
    assert_eq!(
        check(Some(&format!("landing:{tagged}")))["scope"],
        json!({"kind": "landing", "landing": tagged})
    );
    // a bare and an explicit view are one report; the two proposal forms are one report
    assert_eq!(check(Some("draft")), check(Some("view:draft")));
    assert_eq!(
        check(Some(&format!("proposal:{}", p2.r#ref))),
        check(Some(&format!("proposal:{}", latest_form(&p2.r#ref))))
    );
    // the scope object's keys, closed
    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&view), ["kind", "landing", "view"]);
    assert_eq!(keys(&latest), ["kind", "landing", "proposal"]);

    // a stable baseline compares a review candidate
    let v = publication::check(&st, "site", Some("draft"), Some("tag:v1.0.0")).unwrap();
    assert_eq!(v["scope"], view);
    assert_eq!(v["against"], json!({"kind": "landing", "landing": tagged}));
    assert_eq!(v["disclosure"]["profile_id"], pf.as_str());
    assert_eq!(v["disclosure"]["added_docs"][0]["id"], b.as_str());
    assert_eq!(v["disclosure"]["added_docs"].as_array().unwrap().len(), 1);
    let v = publication::check(
        &st,
        "site",
        Some(&format!("proposal:{}", p2.r#ref)),
        Some("canon"),
    )
    .unwrap();
    assert_eq!(v["against"], canon);
    assert_eq!(v["disclosure"]["removed_docs"][0]["id"], a.as_str());
    assert_eq!(v["disclosure"]["added_docs"].as_array().unwrap().len(), 2);

    // a review or in-flight baseline keeps the stable refusal
    for bad in [
        "draft".to_string(),
        "view:draft".to_string(),
        "inflight".to_string(),
        format!("proposal:{}", p1.r#ref),
        String::new(),
        "Canon".to_string(),
    ] {
        let err = publication::check(&st, "site", Some("draft"), Some(&bad)).unwrap_err();
        assert_err(&err, E_BAD_INPUT, BASELINE.0, BASELINE.1);
    }
    // the in-flight candidate is refused with the inspection text
    let err = publication::check(&st, "site", Some("inflight"), None).unwrap_err();
    assert_err(&err, E_BAD_INPUT, INSPECTION_SCOPE.0, INSPECTION_SCOPE.1);
}

#[test]
fn spv1_inspection_assets_bypass_only_the_output_gate() {
    let (dir, mut r) = project();
    let bytes = b"figure bytes".to_vec();
    let blob = r.store_mut().put_blob(&bytes).unwrap();
    let removed_bytes = vec![5u8; 1 << 20];
    let removed = r.store_mut().put_blob(&removed_bytes).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "D", "doc_type": "page"}},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d",
             "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "kept"}},
            {"op": "create", "type": "figure", "id": "$g", "parent": "$d",
             "fields": {"blob": removed.to_string(), "media_type": "application/octet-stream", "alt": "removed"}},
            {"op": "create", "type": "doc", "id": "$bare", "fields": {"name": "bare", "title": "Bare", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [d, bare, out, admin] = ["$d", "$bare", "$out", "$admin"].map(|n| id(&w, n));
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": d, "body": format!("See {{{{{out}}}}}.")},
            {"op": "create", "type": "prose", "parent": bare, "body": format!("Also {{{{{out}}}}}.")},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [d]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "plain", "title": "Plain", "audience": "public", "select": [bare]}}
        ]),
    );
    land(&mut r, "work");
    r.close().unwrap();
    authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::Blob(removed)],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let gate = "Public profile has 1 references outside its selection.";

    for view in [Scope::Canon, Scope::View("work".into())] {
        let res = publication::resolve_inspection(&st, &view, "open").unwrap();
        assert_eq!(res.references.status, CheckStatus::Blocked);
        let avail: BTreeMap<Hash, bool> = res
            .attachments
            .iter()
            .map(|a| (a.blob, a.available))
            .collect();
        assert_eq!(
            avail,
            BTreeMap::from([(blob, true), (removed, false)]),
            "a removed blob is unavailable, without an error"
        );
        // the live loader reads the available figure of a blocked selection
        let reads = publication::blob_reads_for_tests();
        let got = with_source(&st, |s| publication::load_inspection_assets(s, &res)).unwrap();
        assert_eq!(got, BTreeMap::from([(blob, bytes.clone())]));
        assert_eq!(publication::blob_reads_for_tests() - reads, 1);
        // both gated loaders refuse before any manifest or byte is read
        let reads = publication::blob_reads_for_tests();
        let err = publication::load_assets(&st, &res).unwrap_err();
        assert_err(&err, E_PUBLICATION_REFERENCE, gate, PUBLIC_FIX);
        let err = with_source(&st, |s| publication::load_source_assets(s, &res)).unwrap_err();
        assert_err(&err, E_PUBLICATION_REFERENCE, gate, PUBLIC_FIX);
        assert_eq!(publication::blob_reads_for_tests(), reads, "no byte read");
    }

    // without figures: the live loader returns nothing, the gated ones still refuse
    let res = publication::resolve_inspection(&st, &Scope::Canon, "plain").unwrap();
    assert!(res.attachments.is_empty());
    let reads = publication::blob_reads_for_tests();
    assert!(
        with_source(&st, |s| publication::load_inspection_assets(s, &res))
            .unwrap()
            .is_empty()
    );
    assert_err(
        &publication::load_assets(&st, &res).unwrap_err(),
        E_PUBLICATION_REFERENCE,
        gate,
        PUBLIC_FIX,
    );
    assert_err(
        &with_source(&st, |s| publication::load_source_assets(s, &res)).unwrap_err(),
        E_PUBLICATION_REFERENCE,
        gate,
        PUBLIC_FIX,
    );
    assert_eq!(publication::blob_reads_for_tests(), reads);

    // a figure whose blob the store lacks is skipped, with no error
    let absent = Hash::of(b"a blob this store never held");
    let mut fig = Block {
        id: bid("fg-ABSNT"),
        block_type: "figure".into(),
        parent: Some(bid(&d)),
        order: "z".into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    fig.fields
        .insert("blob".into(), FieldValue::Str(absent.to_string()));
    let m = st
        .materialized(&Scope::Canon)
        .unwrap()
        .with(&[Op::Create { block: fig }]);
    let res = publication::resolve_in_for_tests(&st, &m, "open").unwrap();
    assert!(
        res.attachments
            .iter()
            .any(|a| a.blob == absent && !a.available)
    );
    let got = with_source(&st, |s| publication::load_inspection_assets(s, &res)).unwrap();
    assert_eq!(got, BTreeMap::from([(blob, bytes.clone())]));

    // the asset bounds hold on the live loader, on the manifests, before any byte is read
    let res = publication::resolve_inspection(&st, &Scope::Canon, "open").unwrap();
    let n = bytes.len() as u64;
    for (limit, total, err) in [
        (n, n, None),
        (n - 1, n, Some(("blob", n - 1))),
        (n, n - 1, Some(("assets", n - 1))),
    ] {
        let _g = publication::override_limits_for_tests(Limits {
            blob: limit,
            assets: total,
            ..Limits::default()
        });
        let reads = publication::blob_reads_for_tests();
        let got = with_source(&st, |s| publication::load_inspection_assets(s, &res));
        match err {
            None => assert_eq!(got.unwrap(), BTreeMap::from([(blob, bytes.clone())])),
            Some((part, at)) => {
                assert_limit(&got.unwrap_err(), part, at, "bytes");
                assert_eq!(publication::blob_reads_for_tests(), reads, "{part}");
            }
        }
    }
}

#[test]
fn spv1_check_empty_and_case_sensitive_candidates_are_closed() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [a, admin] = ["$a", "$admin"].map(|n| id(&w, n));
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}]),
    );
    land(&mut r, "work");
    let st = r.read();
    let check = |s: &str| publication::check(&st, "site", Some(s), None);
    for s in ["", "view:", "inflight"] {
        assert_err(
            &check(s).unwrap_err(),
            E_BAD_INPUT,
            INSPECTION_SCOPE.0,
            INSPECTION_SCOPE.1,
        );
    }
    // the inspection entries refuse the in-flight scope with the same text
    assert_err(
        &publication::resolve_inspection(&st, &Scope::Inflight, "site").unwrap_err(),
        E_BAD_INPUT,
        INSPECTION_SCOPE.0,
        INSPECTION_SCOPE.1,
    );
    assert_err(
        &publication::profile_at(&st, &Scope::Inflight, "site").unwrap_err(),
        E_BAD_INPUT,
        INSPECTION_SCOPE.0,
        INSPECTION_SCOPE.1,
    );
    assert_err(
        &publication::resolve_revision(&st, &Scope::Inflight, &bid(&a)).unwrap_err(),
        E_BAD_INPUT,
        INSPECTION_SCOPE.0,
        INSPECTION_SCOPE.1,
    );
    // a view name is case-sensitive: `Canon` is a view, and none exists yet
    let missing = "list views with rdy status (MCP: rdy_overview), or read canon";
    assert_err(
        &check("Canon").unwrap_err(),
        E_NOT_FOUND,
        "no view \"Canon\"",
        missing,
    );
    assert_err(
        &check("view:nosuch").unwrap_err(),
        E_NOT_FOUND,
        "no view \"nosuch\"",
        missing,
    );
    // the tag, landing and proposal parsers keep their own errors
    for s in [
        "tag:",
        "tag:9.9.9",
        "landing:zz",
        "proposal:",
        "proposal:nope",
    ] {
        let err = check(s).unwrap_err();
        assert_ne!(err.message, INSPECTION_SCOPE.0, "{s}: {err}");
    }
    assert_eq!(check("tag:9.9.9").unwrap_err().message, "no version 9.9.9");
    drop(st);

    // once a view named `Canon` exists, it is that view
    write(
        &mut r,
        "Canon",
        json!([{"op": "create", "type": "doc", "fields": {"name": "z", "title": "Z", "doc_type": "page"}}]),
    );
    let st = r.read();
    let v = publication::check(&st, "site", Some("Canon"), None).unwrap();
    assert_eq!(
        v["scope"],
        json!({"kind": "view", "landing": null, "view": "Canon"})
    );
    assert_eq!(v["selection"]["entries"][0]["id"], a.as_str());
    // and `canon` is still canon
    let v = publication::check(&st, "site", Some("canon"), None).unwrap();
    assert_eq!(v["scope"]["kind"], "canon");
}

#[test]
fn spv1_live_warning_keeps_blocked_policy() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$clean", "fields": {"name": "clean", "title": "Clean", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [a, clean, out, admin] = ["$a", "$clean", "$out", "$admin"].map(|n| id(&w, n));
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{out}}}}} and {{{{{out}}}}}.")},
            {"op": "create", "type": "prose", "parent": clean, "body": "Nothing outside."},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "open", "title": "Open", "audience": "public", "select": [a]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "tidy", "title": "Tidy", "audience": "public", "select": [clean]}}
        ]),
    );
    // the same profiles in a view, unlanded
    let st = r.read();
    let scope = Scope::View("work".into());
    let open = publication::resolve_inspection(&st, &scope, "open").unwrap();
    assert_eq!(open.references.status, CheckStatus::Blocked);
    assert_eq!(
        open.references.live_warning(),
        Some(PublicationWarning {
            code: "W_PUBLICATION_REFERENCE",
            message: "Public profile has 2 references outside its selection.".into(),
            fix: PUBLIC_FIX.into(),
        })
    );
    // the warning changes no status and opens no output
    assert_eq!(open.references.status, CheckStatus::Blocked);
    assert_eq!(open.references.warning(), None);
    assert_err(
        &open.require_output().unwrap_err(),
        E_PUBLICATION_REFERENCE,
        "Public profile has 2 references outside its selection.",
        PUBLIC_FIX,
    );
    assert_err(
        &open.references.require_output().unwrap_err(),
        E_PUBLICATION_REFERENCE,
        "Public profile has 2 references outside its selection.",
        PUBLIC_FIX,
    );
    let org = publication::resolve_inspection(&st, &scope, "site").unwrap();
    assert_eq!(org.references.status, CheckStatus::Warning);
    let warned = Some(PublicationWarning {
        code: "W_PUBLICATION_REFERENCE",
        message: "Org profile has 2 references outside its selection.".into(),
        fix: ORG_FIX.into(),
    });
    assert_eq!(org.references.live_warning(), warned);
    assert_eq!(org.references.warning(), warned);
    assert!(org.require_output().is_ok());
    let clear = publication::resolve_inspection(&st, &scope, "tidy").unwrap();
    assert_eq!(clear.references.status, CheckStatus::Clear);
    assert_eq!(clear.references.live_warning(), None);
    assert!(clear.require_output().is_ok());
    // the check report keeps its own warnings: a blocked result carries none
    let v = publication::check(&st, "open", Some("work"), None).unwrap();
    assert_eq!(v["references"]["status"], "blocked");
    assert_eq!(v["warnings"], json!([]));
    let v = publication::check(&st, "site", Some("work"), None).unwrap();
    assert_eq!(
        v["warnings"],
        json!([{
            "code": "W_PUBLICATION_REFERENCE",
            "message": "Org profile has 2 references outside its selection.",
            "fix": ORG_FIX,
        }])
    );
}

#[test]
fn spv1_inspection_is_bounded_and_read_only() {
    let (dir, mut r) = project();
    let blob = r.store_mut().put_blob(b"bounded figure").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$top", "fields": {"name": "top"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$top", "fields": {"name": "a-long-name", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Plain words."},
            {"op": "create", "type": "figure", "id": "$af", "parent": "$a",
             "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}},
            {"op": "create", "type": "doc", "id": "$out", "fields": {"name": "out", "title": "Out", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [top, a, out, admin] = ["$top", "$a", "$out", "$admin"].map(|n| id(&w, n));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [top]}}]),
    );
    let pf = id(&w, "$pf");
    land(&mut r, "work");
    // a view and a proposal that add references leaving the selection, one of them in a
    // nested table cell so the location and field-depth bounds have a deep value to meet
    write(
        &mut r,
        "draft",
        json!([
            {"op": "create", "type": "prose", "parent": a, "body": format!("See {{{{{out}}}}}.")},
            {"op": "create", "type": "table", "parent": a,
             "fields": {"columns": ["c"], "rows": [[[format!("Cell {{{{{out}}}}}.")]]]}}
        ]),
    );
    let p = propose(&mut r, "draft");
    // a view where the selected folder is gone: metadata still reads, selection does not
    write(
        &mut r,
        "gone",
        json!([{"op": "update", "id": pf, "fields": {"select": ["fd-ZZZZZ"]}}]),
    );
    r.close().unwrap();
    let before = fingerprint(dir.path());
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let seq = st.seq();
    let view = Scope::View("draft".into());
    let prop = st.resolve_scope(&format!("proposal:{}", p.r#ref)).unwrap();

    // a 129-byte selector is refused before any lookup, on every inspection entry
    let long = "x".repeat(129);
    for scope in [&Scope::Canon, &view, &prop] {
        assert_limit(
            &publication::resolve_inspection(&st, scope, &long).unwrap_err(),
            "profile name",
            128,
            "bytes",
        );
        assert_limit(
            &publication::profile_at(&st, scope, &long).unwrap_err(),
            "profile name",
            128,
            "bytes",
        );
    }
    assert_limit(
        &publication::check(&st, &long, Some("draft"), None).unwrap_err(),
        "profile name",
        128,
        "bytes",
    );
    // 128 bytes is a name, not a limit
    let at_bound = "x".repeat(128);
    assert_eq!(
        publication::resolve_inspection(&st, &view, &at_bound)
            .unwrap_err()
            .code,
        E_NOT_FOUND
    );

    // each existing bound holds in a review scope: N passes, N - 1 refuses
    for scope in [&view, &prop] {
        let res = publication::resolve_inspection(&st, scope, "site").unwrap();
        let blocks = res.entries.len() as u64;
        let edges: u64 = res
            .m
            .iter()
            .map(|b| rdy_graph::edges_of(b).len() as u64 + u64::from(b.parent.is_some()))
            .sum();
        let path = res.docs.iter().map(|d| d.path.len()).max().unwrap() as u64;
        assert_eq!(path, "top/a-long-name".len() as u64);
        let token = out.len() as u64;
        // the body reference and the table-cell reference, both leaving the selection
        let occurrences: u64 = res.references.rows.iter().map(|r| r.occurrences).sum();
        assert_eq!(occurrences, 2);
        let location = res
            .references
            .rows
            .iter()
            .map(|r| r.location.len())
            .max()
            .unwrap() as u64;
        assert_eq!(location, "/fields/rows/0/0/0".len() as u64);
        // rows (1), a row (2), a cell (3), the string in it (4)
        let depth = 4u64;
        type Set = fn(&mut Limits, u64);
        let cases: Vec<(&str, &str, u64, Set)> = vec![
            ("selection", "blocks", blocks, |l, n| {
                l.selection = n as usize
            }),
            ("graph", "edges", edges, |l, n| l.graph = n as usize),
            ("references", "occurrences", occurrences, |l, n| {
                l.references = n as usize
            }),
            ("attachments", "rows", 1, |l, n| l.attachments = n as usize),
            ("selected path", "bytes", path, |l, n| l.path = n as usize),
            ("reference token", "bytes", token, |l, n| {
                l.token = n as usize
            }),
            ("text value", "bytes", longest_text(&res), |l, n| {
                l.text_value = n as usize
            }),
            ("text", "bytes", total_text(&res), |l, n| {
                l.text = n as usize
            }),
            ("reference location", "bytes", location, |l, n| {
                l.location = n as usize
            }),
            ("field depth", "levels", depth, |l, n| {
                l.field_depth = n as usize
            }),
        ];
        for (part, unit, n, set) in cases {
            let mut l = Limits::default();
            set(&mut l, n);
            {
                let _g = publication::override_limits_for_tests(l);
                let ok = publication::resolve_inspection(&st, scope, "site")
                    .unwrap_or_else(|e| panic!("{part} at {n}: {e}"));
                assert_eq!(ok.selection_hash, res.selection_hash, "{part}");
            }
            set(&mut l, n - 1);
            let _g = publication::override_limits_for_tests(l);
            let err = publication::resolve_inspection(&st, scope, "site").unwrap_err();
            assert_limit(&err, part, n - 1, unit);
            let err = publication::resolve_revision(&st, scope, &bid(&pf)).unwrap_err();
            assert_limit(&err, part, n - 1, unit);
        }
    }

    // the metadata read expands no selection and reads no blob
    let reads = publication::blob_reads_for_tests();
    let gone = Scope::View("gone".into());
    let meta = publication::profile_at(&st, &gone, "site").unwrap();
    assert_eq!(meta.id, Some(bid(&pf)));
    assert_eq!(meta.key.as_deref(), Some("site"));
    assert_eq!(
        publication::resolve_inspection(&st, &gone, "site")
            .unwrap_err()
            .message,
        "Publication selector fd-ZZZZZ was not found in this scope."
    );
    for scope in [&Scope::Canon, &view, &prop] {
        let meta = publication::profile_at(&st, scope, "site").unwrap();
        let full = publication::resolve_inspection(&st, scope, "site").unwrap();
        assert_eq!(meta, full.profile);
    }
    let implicit = publication::profile_at(&st, &view, publication::IMPLICIT_ORG).unwrap();
    assert_eq!(
        (implicit.id, implicit.key, implicit.title),
        (None, None, st.project_name())
    );
    assert_err(
        &publication::profile_at(&st, &view, &a).unwrap_err(),
        E_BAD_INPUT,
        &format!("Block {a} is not a publication profile."),
        "Use a publication profile id or key.",
    );
    assert_eq!(
        publication::profile_at(&st, &view, "nosuch")
            .unwrap_err()
            .message,
        "Publication profile \"nosuch\" was not found."
    );
    assert_eq!(publication::blob_reads_for_tests(), reads, "no blob read");

    // every inspection, refusals included, leaves the log, refs and files as they were
    for scope in ["canon", "draft", &format!("proposal:{}", p.r#ref)] {
        publication::check(&st, "site", Some(scope), Some("canon")).unwrap();
        let s = st.resolve_scope(scope).unwrap();
        let res = publication::resolve_inspection(&st, &s, "site").unwrap();
        with_source(&st, |src| publication::load_inspection_assets(src, &res)).unwrap();
        publication::resolve_revision(&st, &s, &bid(&pf)).unwrap();
    }
    assert!(publication::check(&st, "site", Some("inflight"), None).is_err());
    assert_eq!(st.seq(), seq);
    drop(st);
    r.close().unwrap();
    assert_eq!(
        fingerprint(dir.path()),
        before,
        "log, refs and files are unchanged"
    );
}

#[test]
fn spv1_profile_key_reassignment_does_not_change_baseline_identity() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$c", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let [a, b, c, admin] = ["$a", "$b", "$c", "$admin"].map(|n| id(&w, n));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a]}}]),
    );
    let p1 = id(&w, "$pf");
    land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    // in review: the old profile gives up its key, and a new profile takes it
    let w = write(
        &mut r,
        "draft",
        json!([
            {"op": "update", "id": p1, "fields": {"key": "retired", "select": [a, b]}},
            {"op": "create", "type": "publication_profile", "id": "$p2", "parent": admin,
             "fields": {"key": "site", "title": "Site two", "audience": "org", "select": [c]}}
        ]),
    );
    let p2 = id(&w, "$p2");
    let p = propose(&mut r, "draft");
    let st = r.read();
    let tag_scope = st.resolve_scope("tag:v1.0.0").unwrap();

    for scope in ["draft".to_string(), format!("proposal:{}", p.r#ref)] {
        // the candidate's id governs the baseline: the retired key finds the old profile
        let v = publication::check(&st, "retired", Some(&scope), Some("tag:v1.0.0")).unwrap();
        assert_eq!(v["profile"]["id"], p1.as_str(), "{scope}");
        assert_eq!(v["profile"]["key"], "retired", "{scope}");
        assert_eq!(v["disclosure"]["profile_id"], p1.as_str(), "{scope}");
        assert_eq!(v["disclosure"]["profile_changed"], true, "{scope}");
        let added: Vec<&str> = v["disclosure"]["added_docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(added, [b.as_str()], "{scope}");
        assert_eq!(
            publication::check(&st, &p1, Some(&scope), Some("tag:v1.0.0")).unwrap(),
            v,
            "{scope}: by id, the same report"
        );
        // the key's new holder has no baseline revision: no fallback by key
        let err = publication::check(&st, "site", Some(&scope), Some("tag:v1.0.0")).unwrap_err();
        assert_err(
            &err,
            E_NOT_FOUND,
            &format!("Publication profile \"{p2}\" was not found."),
            "Use a publication profile id or key from this scope.",
        );
        // in the candidate itself the key names the new profile
        let v = publication::check(&st, "site", Some(&scope), None).unwrap();
        assert_eq!(v["profile"]["id"], p2.as_str(), "{scope}");
    }
    // by revision lookup: the tag holds the old profile under the old key, and no new one
    let then = publication::resolve_revision(&st, &tag_scope, &bid(&p1))
        .unwrap()
        .unwrap();
    assert_eq!(then.profile.key.as_deref(), Some("site"));
    assert_eq!(entry_ids(&then), vec![a.clone()]);
    assert!(
        publication::resolve_revision(&st, &tag_scope, &bid(&p2))
            .unwrap()
            .is_none()
    );
}
