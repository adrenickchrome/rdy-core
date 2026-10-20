//! ha6 (D-KDFZ3, D-F9Z60, D-QYCRB, D-VRW0A, D-WDKWQ): profile and disclosure reads through the
//! adapter, pinned to a captured tag. Selection, the reference check and the comparison are
//! Publications' own, so the expectations here come from calling it directly.

use rdy_adapter::publication::{Audience, DisclosureDiff, PreviousPublication, ProfileSummary};
use rdy_adapter::{BlockId, Hash, RdyError, StoreHandle};
use rdy_engine::publication::{self, IMPLICIT_ORG};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
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
            message: "ha6".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&session(Role::Writer), "work", "ha6", None)
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

/// A closed project with two tags and a later landing that no tag holds.
///
/// v1.0.0: docs `a`, `b`, `admin`, folder `f` holding doc `c`; profile `p1` (key `site`, org,
/// selects a), `p2` (key `press`, public, selects c) and `p3` (key `other`, org, selects a).
/// v2.0.0: `p1` takes key `retired` and selects a and b; `p3` takes key `site`; `p2` becomes org
/// and a new profile `p4` (key `fresh`) exists; doc `d` is added. Then doc `e` lands untagged.
struct Fixture {
    dir: tempfile::TempDir,
    name: String,
    a: BlockId,
    b: BlockId,
    c: BlockId,
    d: BlockId,
    f: BlockId,
    p1: BlockId,
    p2: BlockId,
    p3: BlockId,
    p4: BlockId,
    e: BlockId,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "f"}},
            {"op": "create", "type": "doc", "id": "$c", "parent": "$f", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, b, c, f) = (bid(&w, "$a"), bid(&w, "$b"), bid(&w, "$c"), bid(&w, "$f"));
    let admin = bid(&w, "$admin");
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$p1", "parent": admin.to_string(),
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$p2", "parent": admin.to_string(),
                "fields": {"key": "press", "title": "Press", "audience": "public", "select": [c.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$p3", "parent": admin.to_string(),
                "fields": {"key": "other", "title": "Other", "audience": "org", "select": [a.to_string()]}}
        ]),
    );
    let (p1, p2, p3) = (bid(&w, "$p1"), bid(&w, "$p2"), bid(&w, "$p3"));
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();

    write(
        &mut r,
        json!([{"op": "update", "id": p1.to_string(),
            "fields": {"key": "retired", "select": [a.to_string(), b.to_string()]}}]),
    );
    write(
        &mut r,
        json!([
            {"op": "update", "id": p3.to_string(), "fields": {"key": "site"}},
            {"op": "update", "id": p2.to_string(), "fields": {"audience": "org"}}
        ]),
    );
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$p4", "parent": admin.to_string(),
                "fields": {"key": "fresh", "title": "Fresh", "audience": "org", "select": [b.to_string()]}},
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "D", "doc_type": "page"}}
        ]),
    );
    let (p4, d) = (bid(&w, "$p4"), bid(&w, "$d"));
    land(&mut r);
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();

    let w = write(
        &mut r,
        json!([{"op": "create", "type": "doc", "id": "$e", "fields": {"name": "e", "title": "E", "doc_type": "page"}}]),
    );
    let e = bid(&w, "$e");
    land(&mut r);
    let name = r.read().project_name();
    r.close().unwrap();
    Fixture {
        dir,
        name,
        a,
        b,
        c,
        d,
        f,
        p1,
        p2,
        p3,
        p4,
        e,
    }
}

/// Publications' own resolution of `profile` at the tag `tag`, read straight from the engine.
fn direct(fx: &Fixture, tag: &str, profile: &str) -> publication::Resolved {
    let r = Rdy::open(fx.dir.path()).unwrap();
    let st = r.read();
    let scope = st.resolve_scope(&format!("tag:{tag}")).unwrap();
    let out = publication::resolve(&st, &scope, profile).unwrap();
    r.close().unwrap();
    out
}

fn previous(tag: &str, r: &publication::Resolved) -> PreviousPublication {
    PreviousPublication {
        tag: tag.into(),
        profile_version: r.profile.version,
        selection_identity: r.selection_identity,
    }
}

fn assert_err(e: &RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e}"
    );
}

fn by_selector<'a>(rows: &'a [ProfileSummary], id: &BlockId) -> &'a ProfileSummary {
    rows.iter()
        .find(|p| p.selector == id.to_string())
        .unwrap_or_else(|| panic!("no profile {id}"))
}

const PROFILE_MISMATCH: (&str, &str) = (
    "Disclosure comparison requires the same publication profile.",
    "Choose a baseline that contains the candidate profile id.",
);

#[test]
fn ha6_profiles_put_the_org_implicit_profile_first() {
    let fx = fixture();
    let implicit_hash = direct(&fx, "1.0.0", IMPLICIT_ORG).selection_hash;
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let rows = snap.profiles("v1.0.0").unwrap();

    let first = &rows[0];
    assert_eq!(first.selector, "implicit:org");
    assert_eq!(first.name, "implicit:org");
    assert_eq!(first.id, None);
    assert_eq!(first.version, None);
    assert_eq!(first.audience, Audience::Org);
    assert_eq!(first.title, fx.name);
    assert_eq!(first.selection_hash, implicit_hash);
    // every doc and folder of the tag, in id order
    let mut docs = first.docs.clone();
    docs.sort();
    assert_eq!(first.docs, docs);
    assert!(first.docs.contains(&fx.a) && first.docs.contains(&fx.c));
    assert!(!first.docs.contains(&fx.d));
    assert_eq!(first.folders, vec![fx.f.clone()]);

    // then the real profiles by (key, id), each by its full id
    let names: Vec<&str> = rows[1..].iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["other", "press", "site"]);
    let site = by_selector(&rows, &fx.p1);
    assert_eq!(site.id, Some(fx.p1.clone()));
    assert_eq!(site.title, "Site");
    assert_eq!(site.audience, Audience::Org);
    assert_eq!(site.docs, vec![fx.a.clone()]);
    assert!(site.folders.is_empty());
    assert!(site.version.is_some());
    // a public profile keeps its audience; its doc's folder is kept as an ancestor
    let press = by_selector(&rows, &fx.p2);
    assert_eq!(press.audience, Audience::Public);
    assert_eq!(press.docs, vec![fx.c.clone()]);
    assert_eq!(press.folders, vec![fx.f.clone()]);
    assert_eq!(by_selector(&rows, &fx.p3).name, "other");
    handle.close().unwrap();
}

#[test]
fn ha6_public_implicit_is_refused() {
    let fx = fixture();
    let prev = previous("v1.0.0", &direct(&fx, "v1.0.0", IMPLICIT_ORG));
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let msg = "the implicit profile is always org (FW-PB9a)";
    let fix = "name an explicit publication profile whose audience is public";
    let e = snap
        .disclosure("v2.0.0", "implicit:public", None)
        .unwrap_err();
    assert_err(&e, "E_BAD_INPUT", msg, fix);
    // the candidate is read first, so a previous publication changes nothing
    let e = snap
        .disclosure("v2.0.0", "implicit:public", Some(&prev))
        .unwrap_err();
    assert_err(&e, "E_BAD_INPUT", msg, fix);
    handle.close().unwrap();
}

#[test]
fn ha6_profiles_are_pinned_to_the_requested_tag() {
    let fx = fixture();
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let v1 = snap.profiles("v1.0.0").unwrap();
    // `tag:<version>` reads: the same tag with or without its `v`
    assert_eq!(snap.profiles("1.0.0").unwrap(), v1);
    let v2 = snap.profiles("v2.0.0").unwrap();
    assert_ne!(v1, v2);

    let names = |rows: &[ProfileSummary]| -> Vec<String> {
        rows[1..].iter().map(|p| p.name.clone()).collect()
    };
    assert_eq!(names(&v1), ["other", "press", "site"]);
    assert_eq!(names(&v2), ["fresh", "press", "retired", "site"]);
    // the profile revision is the one the tag holds
    assert_eq!(by_selector(&v1, &fx.p1).docs, vec![fx.a.clone()]);
    let mut both = vec![fx.a.clone(), fx.b.clone()];
    both.sort();
    assert_eq!(by_selector(&v2, &fx.p1).docs, both);
    assert_eq!(by_selector(&v2, &fx.p2).audience, Audience::Org);
    assert_eq!(by_selector(&v2, &fx.p4).name, "fresh");
    // a doc landed after the tag is in no profile list of it
    for rows in [&v1, &v2] {
        assert!(!rows[0].docs.contains(&fx.e));
    }
    assert!(v2[0].docs.contains(&fx.d) && !v1[0].docs.contains(&fx.d));
    // only a captured tag resolves
    let e = snap.profiles("v9.9.9").unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND");
    handle.close().unwrap();
}

#[test]
fn ha6_disclosure_matches_publications_exactly() {
    let fx = fixture();
    // the expectations, read straight from the engine before the adapter holds the lock
    let id = fx.p1.to_string();
    let old = direct(&fx, "v1.0.0", &id);
    let new = direct(&fx, "v2.0.0", &id);
    let expected: DisclosureDiff = publication::disclosure_diff(&old, &new).unwrap();
    assert!(expected.profile_changed && !expected.added_docs.is_empty());
    let old_implicit = direct(&fx, "v1.0.0", IMPLICIT_ORG);
    let new_implicit = direct(&fx, "v2.0.0", IMPLICIT_ORG);
    let expected_implicit = publication::disclosure_diff(&old_implicit, &new_implicit).unwrap();

    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let check = snap
        .disclosure("v2.0.0", &id, Some(&previous("v1.0.0", &old)))
        .unwrap();
    assert_eq!(check.comparison, Some(expected));
    assert_eq!(check.references, new.references);
    assert_eq!(check.profile.selection_hash, new.selection_hash);
    assert_eq!(check.profile.version, new.profile.version);
    assert_eq!(check.profile.selector, id);
    // a key names the profile as well, and reads the same
    let by_key = snap
        .disclosure("v2.0.0", "retired", Some(&previous("v1.0.0", &old)))
        .unwrap();
    assert_eq!(by_key, check);

    let implicit = snap
        .disclosure(
            "v2.0.0",
            IMPLICIT_ORG,
            Some(&previous("v1.0.0", &old_implicit)),
        )
        .unwrap();
    assert_eq!(implicit.comparison, Some(expected_implicit));
    assert_eq!(implicit.references, new_implicit.references);
    assert_eq!(implicit.profile.selector, "implicit:org");
    handle.close().unwrap();
}

#[test]
fn ha6_previous_identity_mismatch_is_refused() {
    let fx = fixture();
    let id = fx.p1.to_string();
    let old = direct(&fx, "v1.0.0", &id);
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let msg = "previous publication does not match version v1.0.0";
    let fix = "read that version's profile and selection identity again";

    let mut wrong_hash = previous("v1.0.0", &old);
    wrong_hash.selection_identity = Hash::of(b"not the selection");
    let e = snap
        .disclosure("v2.0.0", &id, Some(&wrong_hash))
        .unwrap_err();
    assert_err(&e, "E_BAD_INPUT", msg, fix);

    let mut wrong_version = previous("v1.0.0", &old);
    wrong_version.profile_version = Some(Hash::of(b"not the profile"));
    let e = snap
        .disclosure("v2.0.0", &id, Some(&wrong_version))
        .unwrap_err();
    assert_err(&e, "E_BAD_INPUT", msg, fix);

    let mut no_version = previous("v1.0.0", &old);
    no_version.profile_version = None;
    let e = snap
        .disclosure("v2.0.0", &id, Some(&no_version))
        .unwrap_err();
    assert_err(&e, "E_BAD_INPUT", msg, fix);

    // a record taken at one tag does not describe another
    let e = snap
        .disclosure("v2.0.0", &id, Some(&previous("v2.0.0", &old)))
        .unwrap_err();
    assert_err(
        &e,
        "E_BAD_INPUT",
        "previous publication does not match version v2.0.0",
        fix,
    );
    // a tag the snapshot lacks passes through
    let e = snap
        .disclosure("v2.0.0", &id, Some(&previous("v7.0.0", &old)))
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND");
    handle.close().unwrap();
}

#[test]
fn ha6_implicit_previous_profile_version_is_null() {
    let fx = fixture();
    let old = direct(&fx, "v1.0.0", IMPLICIT_ORG);
    assert_eq!(old.profile.version, None);
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();

    let mut with_version = previous("v1.0.0", &old);
    with_version.profile_version = Some(Hash::of(b"a profile"));
    let e = snap
        .disclosure("v2.0.0", IMPLICIT_ORG, Some(&with_version))
        .unwrap_err();
    assert_err(
        &e,
        "E_BAD_INPUT",
        "previous publication does not match version v1.0.0",
        "read that version's profile and selection identity again",
    );

    let ok = snap
        .disclosure("v2.0.0", IMPLICIT_ORG, Some(&previous("v1.0.0", &old)))
        .unwrap();
    let diff = ok.comparison.unwrap();
    assert_eq!(diff.before_profile_version, None);
    assert_eq!(diff.after_profile_version, None);
    assert!(!diff.profile_changed);
    assert_eq!(diff.audience, Audience::Org);
    handle.close().unwrap();
}

#[test]
fn ha6_missing_previous_is_not_an_empty_diff() {
    let fx = fixture();
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    for profile in [IMPLICIT_ORG.to_string(), fx.p1.to_string(), "site".into()] {
        let check = snap.disclosure("v1.0.0", &profile, None).unwrap();
        assert_eq!(check.comparison, None);
        assert!(!check.profile.docs.is_empty());
        assert_eq!(check.first_attachments, Some(vec![]));
    }
    handle.close().unwrap();
}

#[test]
fn ha6_baseline_uses_the_profile_id_after_key_reassignment() {
    let fx = fixture();
    // at v1 the key `site` named p1; at v2 it names p3, which v1 called `other`
    let id3 = fx.p3.to_string();
    let old3 = direct(&fx, "v1.0.0", &id3);
    assert_eq!(old3.profile.key.as_deref(), Some("other"));
    let new3 = direct(&fx, "v2.0.0", "site");
    assert_eq!(new3.profile.id, Some(fx.p3.clone()));
    let expected = publication::disclosure_diff(&old3, &new3).unwrap();
    let old1 = direct(&fx, "v1.0.0", "site");
    assert_eq!(old1.profile.id, Some(fx.p1.clone()));
    let fresh = direct(&fx, "v2.0.0", &fx.p4.to_string());

    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    // the candidate is named by its key; the baseline is found by its id, not that key
    let check = snap
        .disclosure("v2.0.0", "site", Some(&previous("v1.0.0", &old3)))
        .unwrap();
    assert_eq!(check.comparison, Some(expected));
    assert_eq!(check.profile.id, Some(fx.p3.clone()));
    // the profile the key named at v1 is not a baseline for it
    let e = snap
        .disclosure("v2.0.0", "site", Some(&previous("v1.0.0", &old1)))
        .unwrap_err();
    assert_err(
        &e,
        "E_BAD_INPUT",
        "previous publication does not match version v1.0.0",
        "read that version's profile and selection identity again",
    );

    // a candidate that did not exist at the baseline has no baseline
    let e = snap
        .disclosure("v2.0.0", "fresh", Some(&previous("v1.0.0", &fresh)))
        .unwrap_err();
    assert_err(&e, "E_BAD_INPUT", PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);
    handle.close().unwrap();
}

#[test]
fn ha6_audience_mismatch_passes_through_the_diff_text() {
    let fx = fixture();
    // p2 is public at v1 and org at v2
    let id = fx.p2.to_string();
    let old = direct(&fx, "v1.0.0", &id);
    assert_eq!(
        old.profile.audience,
        rdy_engine::publication::Audience::Public
    );
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let e = snap
        .disclosure("v2.0.0", &id, Some(&previous("v1.0.0", &old)))
        .unwrap_err();
    assert_err(
        &e,
        "E_BAD_INPUT",
        "Disclosure comparison requires the same audience.",
        "Choose a baseline with the same audience as the candidate profile.",
    );
    handle.close().unwrap();
}

/// TASK-305: a redaction after a publication moves the selection hash of the tag (the mark of an
/// entry is hashed) but keeps its selection identity (ids and versions only, D-075QD), so a later
/// disclosure review against that publication's recorded identity still reads.
#[test]
fn ha6_a_redaction_after_a_publication_keeps_later_disclosure_reviews() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, ap, admin) = (bid(&w, "$a"), bid(&w, "$ap"), bid(&w, "$admin"));
    write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$p", "parent": admin.to_string(),
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}}]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "D", "doc_type": "page"}}]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    // the publication facts, read before the redaction
    let (old, prev) = {
        let st = r.read();
        let scope = st.resolve_scope("tag:v1.0.0").unwrap();
        let old = publication::resolve(&st, &scope, "site").unwrap();
        (old.profile.version, old.selection_identity)
    };

    // the real redaction path, with a human approval
    let s = session(Role::Writer);
    r.redact(
        &s,
        RedactInput {
            targets: vec![ap.to_string()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p = r.propose(&s, "work", "Remove", None).unwrap();
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
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    r.close().unwrap();

    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let previous = PreviousPublication {
        tag: "v1.0.0".into(),
        profile_version: old,
        selection_identity: prev,
    };
    let check = snap.disclosure("v2.0.0", "site", Some(&previous));
    assert!(check.is_ok(), "{}", check.unwrap_err());
    handle.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// hpb8 (D-Z8Q4N, D-075QD, D-KDFZ3, D-VRW0A, D-QYCRB, D-WDKWQ): the one disclosure read binds a
// previous publication by its profile version and selection identity, stays valid across later
// removals, refuses every missing or mismatched baseline, and with no baseline lists every doc
// and attachment of the candidate.

/// Records one removal request of `targets` in `r` and lands it with a person's approval.
fn redact(r: &mut Rdy, targets: &[String]) -> Hash {
    let s = session(Role::Writer);
    let res = r
        .redact_in(
            &s,
            "redact",
            RedactInput {
                targets: targets.to_vec(),
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let request = res.redaction.expect("a request is recorded");
    let p = r.propose(&s, "redact", "Remove", None).unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    request
}

/// What a host records of a publication it made from `row`.
fn record(tag: &str, row: &ProfileSummary) -> PreviousPublication {
    PreviousPublication {
        tag: tag.into(),
        profile_version: row.version,
        selection_identity: row.selection_identity,
    }
}

fn site_row(rows: &[ProfileSummary]) -> ProfileSummary {
    rows.iter()
        .find(|p| p.name == "site")
        .expect("the site profile")
        .clone()
}

#[test]
fn hpb8_binding_from_before_redaction_allows_later_disclosure() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(b"figure bytes").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "The code is ZEBRA-7731."},
            {"op": "create", "type": "figure", "id": "$fa", "parent": "$a", "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, ap, admin) = (bid(&w, "$a"), bid(&w, "$ap"), bid(&w, "$admin"));
    write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "parent": admin.to_string(),
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}}]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    write(
        &mut r,
        json!([{"op": "create", "type": "prose", "parent": a.to_string(), "body": "Later text."}]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    r.close().unwrap();

    // the host's record, read through the adapter before any removal
    let handle = StoreHandle::open(dir.path()).unwrap();
    let then = site_row(&handle.snapshot().unwrap().profiles("v1.0.0").unwrap());
    let previous = record("v1.0.0", &then);
    handle
        .snapshot()
        .unwrap()
        .disclosure("v2.0.0", "site", Some(&previous))
        .unwrap();
    handle.close().unwrap();

    let mut hashes = vec![then.selection_hash];
    for target in [ap.to_string(), format!("blob:{blob}")] {
        let mut r = Rdy::open(dir.path()).unwrap();
        redact(&mut r, std::slice::from_ref(&target));
        r.close().unwrap();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let now = site_row(&snap.profiles("v1.0.0").unwrap());
        // the evidence: the old tag's selection hash moved, its identity did not
        assert!(!hashes.contains(&now.selection_hash), "{target}");
        hashes.push(now.selection_hash);
        assert_eq!(now.selection_identity, then.selection_identity, "{target}");
        let check = snap.disclosure("v2.0.0", "site", Some(&previous));
        let check = check.unwrap_or_else(|e| panic!("{target}: {e}"));
        let diff = check.comparison.expect("a comparison");
        assert_eq!(diff.before_selection_identity, then.selection_identity);
        assert_eq!(diff.before_selection_hash, now.selection_hash);
        assert!(!diff.redactions.is_empty(), "{target}");
        assert_eq!(check.first_attachments, None);
        handle.close().unwrap();
    }
}

#[test]
fn hpb8_first_review_lists_all_docs_and_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let stored = r.store_mut().put_blob(b"stored figure").unwrap();
    // removed after the tag: its rows stay, unavailable
    let missing = r.store_mut().put_blob(b"a removed figure").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "figure", "id": "$fa", "parent": "$a", "fields": {"blob": stored.to_string(), "media_type": "application/octet-stream", "alt": "stored"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "figure", "id": "$fb", "parent": "$b", "fields": {"blob": missing.to_string(), "media_type": "application/octet-stream", "alt": "removed"}},
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "f"}},
            {"op": "create", "type": "doc", "id": "$c", "parent": "$f", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
            {"op": "create", "type": "figure", "id": "$fc", "parent": "$c", "fields": {"blob": stored.to_string(), "media_type": "application/octet-stream", "alt": "again"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, b, c, f) = (bid(&w, "$a"), bid(&w, "$b"), bid(&w, "$c"), bid(&w, "$f"));
    write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "parent": bid(&w, "$admin").to_string(),
            "fields": {"key": "site", "title": "Site", "audience": "org",
                "select": [a.to_string(), b.to_string(), f.to_string()]}}]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    redact(&mut r, &[format!("blob:{missing}")]);
    let want = {
        let st = r.read();
        let scope = st.resolve_scope("tag:v1.0.0").unwrap();
        publication::resolve(&st, &scope, "site").unwrap()
    };
    r.close().unwrap();
    assert_eq!(want.attachments.len(), 3);

    let handle = StoreHandle::open(dir.path()).unwrap();
    let check = handle
        .snapshot()
        .unwrap()
        .disclosure("v1.0.0", "site", None)
        .unwrap();
    assert_eq!(check.comparison, None);
    let mut docs = vec![a.clone(), b.clone(), c.clone()];
    docs.sort();
    assert_eq!(check.profile.docs, docs, "every selected doc");
    assert_eq!(check.profile.folders, vec![f]);
    let first = check
        .first_attachments
        .expect("a first review lists attachments");
    assert_eq!(
        first, want.attachments,
        "the resolution's rows, in its order"
    );
    assert!(
        first
            .windows(2)
            .all(|w| (&w[0].home_doc, &w[0].block, &w[0].blob)
                < (&w[1].home_doc, &w[1].block, &w[1].blob)),
        "sorted by (home doc, block, blob)"
    );
    let rows: std::collections::BTreeMap<BlockId, (BlockId, Hash, bool)> = first
        .iter()
        .map(|x| (x.block.clone(), (x.home_doc.clone(), x.blob, x.available)))
        .collect();
    assert_eq!(
        rows,
        std::collections::BTreeMap::from([
            (bid(&w, "$fa"), (a.clone(), stored, true)),
            (bid(&w, "$fb"), (b.clone(), missing, false)),
            (bid(&w, "$fc"), (c.clone(), stored, true)),
        ])
    );
    assert_eq!(check.profile.selection, want.entries);
    assert_eq!(check.profile.selection_identity, want.selection_identity);
    handle.close().unwrap();
}

#[test]
fn hpb8_comparison_detects_new_and_moved_folder_children() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "f"}},
            {"op": "create", "type": "doc", "id": "$c", "parent": "$f", "fields": {"name": "c", "title": "C", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "g"}},
            {"op": "create", "type": "doc", "id": "$m", "parent": "$g", "fields": {"name": "m", "title": "M", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (f, m) = (bid(&w, "$f"), bid(&w, "$m"));
    write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "parent": bid(&w, "$admin").to_string(),
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [f.to_string()]}}]),
    );
    land(&mut r);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    // a new doc and a new folder inside f, and m moved in from g
    let w2 = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$n", "parent": f.to_string(), "fields": {"name": "n", "title": "N", "doc_type": "page"}},
            {"op": "create", "type": "folder", "id": "$h", "parent": f.to_string(), "fields": {"name": "h"}},
            {"op": "move", "id": m.to_string(), "parent": f.to_string()}
        ]),
    );
    let (n, h) = (bid(&w2, "$n"), bid(&w2, "$h"));
    land(&mut r);
    r.tag(&session(Role::Approver), "v2.0.0", None, None)
        .unwrap();
    r.close().unwrap();

    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let previous = record("v1.0.0", &site_row(&snap.profiles("v1.0.0").unwrap()));
    let check = snap.disclosure("v2.0.0", "site", Some(&previous)).unwrap();
    assert_eq!(
        check.first_attachments, None,
        "a comparison is not a first review"
    );
    let diff = check.comparison.expect("a comparison");
    let mut added: Vec<(BlockId, String)> = diff
        .added_docs
        .iter()
        .map(|d| (d.id.clone(), d.path.clone()))
        .collect();
    added.sort();
    let mut want = vec![
        (m.clone(), "f/m".to_string()),
        (n.clone(), "f/n".to_string()),
    ];
    want.sort();
    assert_eq!(added, want);
    assert!(diff.added_docs.iter().all(|d| !d.redacted));
    assert_eq!(diff.changed_folders.len(), 1);
    let row = &diff.changed_folders[0];
    assert_eq!(row.id, h);
    assert_eq!(
        (row.before_path.as_deref(), row.after_path.as_deref()),
        (None, Some("f/h"))
    );
    assert!(!row.redacted);
    assert!(diff.redactions.is_empty());
    assert_ne!(
        diff.before_selection_identity,
        diff.after_selection_identity
    );
    handle.close().unwrap();
}

#[test]
fn hpb8_renamed_profile_uses_stable_identity() {
    let fx = fixture();
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    // p1 is `site` at v1 and `retired` at v2; the record names the profile by its id
    let then = by_selector(&snap.profiles("v1.0.0").unwrap(), &fx.p1).clone();
    assert_eq!(then.key.as_deref(), Some("site"));
    let previous = record("v1.0.0", &then);
    let by_key = snap
        .disclosure("v2.0.0", "retired", Some(&previous))
        .unwrap();
    let by_id = snap
        .disclosure("v2.0.0", &fx.p1.to_string(), Some(&previous))
        .unwrap();
    assert_eq!(by_key, by_id);
    let diff = by_key.comparison.clone().expect("a comparison");
    assert_eq!(diff.profile_id, Some(fx.p1.clone()));
    assert_eq!(diff.before_selection_identity, then.selection_identity);
    assert_eq!(by_key.profile.key.as_deref(), Some("retired"));
    assert_eq!(by_key.profile.selector, fx.p1.to_string());
    // the key `site` names p3 at v2: p1's record is no baseline for it
    let e = snap
        .disclosure("v2.0.0", "site", Some(&previous))
        .unwrap_err();
    assert_err(
        &e,
        "E_BAD_INPUT",
        "previous publication does not match version v1.0.0",
        "read that version's profile and selection identity again",
    );
    handle.close().unwrap();
}

#[test]
fn hpb8_missing_or_mismatched_baseline_never_becomes_first() {
    let fx = fixture();
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let v1 = snap.profiles("v1.0.0").unwrap();
    let id = fx.p1.to_string();
    let good = record("v1.0.0", by_selector(&v1, &fx.p1));
    assert!(
        snap.disclosure("v2.0.0", &id, Some(&good))
            .unwrap()
            .comparison
            .is_some()
    );
    let mismatch = |tag: &str| {
        (
            "E_BAD_INPUT",
            format!("previous publication does not match version {tag}"),
            "read that version's profile and selection identity again",
        )
    };
    let mut cases: Vec<(&str, PreviousPublication, (&str, String, &str))> = vec![];
    let mut wrong = good.clone();
    wrong.selection_identity = Hash::of(b"not the identity");
    cases.push(("identity", wrong, mismatch("v1.0.0")));
    let mut wrong = good.clone();
    wrong.profile_version = Some(Hash::of(b"not the version"));
    cases.push(("version", wrong, mismatch("v1.0.0")));
    let mut wrong = good.clone();
    wrong.profile_version = None;
    cases.push(("no version", wrong, mismatch("v1.0.0")));
    let mut wrong = good.clone();
    wrong.tag = "v2.0.0".into();
    cases.push(("another tag", wrong, mismatch("v2.0.0")));
    for (what, prev, (code, message, fix)) in cases {
        let e = snap.disclosure("v2.0.0", &id, Some(&prev)).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code, message.as_str(), fix),
            "{what}"
        );
    }
    // a tag the snapshot lacks is not found, not a first publication
    let mut gone = good.clone();
    gone.tag = "v7.0.0".into();
    let e = snap.disclosure("v2.0.0", &id, Some(&gone)).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    // the audience changed (p2 is public at v1, org at v2): the comparison's own refusal
    let p2 = record("v1.0.0", by_selector(&v1, &fx.p2));
    let e = snap
        .disclosure("v2.0.0", &fx.p2.to_string(), Some(&p2))
        .unwrap_err();
    assert_err(
        &e,
        "E_BAD_INPUT",
        "Disclosure comparison requires the same audience.",
        "Choose a baseline with the same audience as the candidate profile.",
    );
    // a profile the baseline lacks
    let fresh = record("v1.0.0", by_selector(&v1, &fx.p3));
    let e = snap
        .disclosure("v2.0.0", "fresh", Some(&fresh))
        .unwrap_err();
    assert_err(&e, "E_BAD_INPUT", PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);
    handle.close().unwrap();
}

#[test]
fn hpb8_implicit_review_has_null_identity_and_org() {
    let fx = fixture();
    let want = direct(&fx, "v1.0.0", IMPLICIT_ORG);
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let check = snap.disclosure("v1.0.0", IMPLICIT_ORG, None).unwrap();
    let p = &check.profile;
    assert_eq!(p.selector, "implicit:org");
    assert_eq!(p.name, "implicit:org");
    assert_eq!((&p.id, &p.version, &p.key), (&None, &None, &None));
    assert_eq!(p.audience, Audience::Org);
    assert!(p.trail);
    assert_eq!(p.title, fx.name);
    assert_eq!(p.selection_identity, want.selection_identity);
    assert_eq!(p.selection_hash, want.selection_hash);
    assert_eq!(p.selection, want.entries);
    assert_eq!(p.references, want.references.status);
    let mut docs: Vec<BlockId> = want.docs.iter().map(|d| d.id.clone()).collect();
    docs.sort();
    assert_eq!(p.docs, docs, "every doc of the tag");
    assert!(p.docs.contains(&fx.a) && p.docs.contains(&fx.b) && p.docs.contains(&fx.c));
    assert_eq!(check.comparison, None);
    assert_eq!(check.first_attachments, Some(want.attachments.clone()));
    // the implicit profile's record has a null version and binds by its identity
    let prev = record("v1.0.0", p);
    assert_eq!(prev.profile_version, None);
    let later = snap
        .disclosure("v2.0.0", IMPLICIT_ORG, Some(&prev))
        .unwrap();
    let diff = later.comparison.expect("a comparison");
    assert_eq!(diff.profile_id, None);
    assert_eq!(diff.audience, Audience::Org);
    assert_eq!(diff.before_selection_identity, want.selection_identity);
    assert!(diff.added_docs.iter().any(|d| d.id == fx.d));
    handle.close().unwrap();
}
