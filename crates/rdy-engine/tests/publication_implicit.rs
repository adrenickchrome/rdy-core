//! pub12 (D-48NYM, D-0VCQ5, D-KDFZ3; rdyhub D-45W02 FW-PUB3; FW-PB9a D-QYCRB): a project with no
//! profile has one implicit profile, everything, named by the exact request value `implicit:org`.
//! It reads no profile block: its id, version and key are null, its title and description are the
//! project's, and its audience is always org. It selects every block of the snapshot, profile
//! blocks included, and then follows the publication rules of a named profile: the reference
//! check, the bounds, the digests and the disclosure comparison.
mod common;

use rdy_engine::publication::{self, Audience, CheckStatus, Resolved};
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------------------------
// helpers

const IMPLICIT: &str = "implicit:org";

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

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    bid(&w.created[name])
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
const ALWAYS_ORG: (&str, &str) = (
    "the implicit profile is always org (FW-PB9a)",
    "name an explicit publication profile whose audience is public",
);

/// Writes and lands a folder `area` with doc `a` (a section, a prose block and a figure on a
/// stored blob) and a top-level doc `b`. Returns the ids by their `$` names and the blob.
fn seed(r: &mut Rdy) -> (BTreeMap<String, BlockId>, Hash) {
    let blob = r.store_mut().put_blob(b"implicit figure bytes").unwrap();
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$area", "fields": {"name": "area"}},
            {"op": "create", "type": "doc", "id": "$a", "parent": "$area", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$a", "fields": {"title": "S"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Words."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$s",
             "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "figure"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}}
        ]),
    );
    land(r, "work");
    let ids = ["$area", "$a", "$s", "$p", "$f", "$b"]
        .iter()
        .map(|n| (n.to_string(), created(&w, n)))
        .collect();
    (ids, blob)
}

/// Adds an `admin` doc holding two profiles, `orgp` (org, selecting `select`) and `pubp`
/// (public, selecting `select`), and lands it. Returns the admin doc and the two profile ids.
fn two_profiles(r: &mut Rdy, select: &BlockId) -> (BlockId, BlockId, BlockId) {
    let w = write(
        r,
        "admin",
        json!([{"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}]),
    );
    let admin = created(&w, "$admin");
    let w = write(
        r,
        "admin",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$orgp", "parent": admin.to_string(),
             "fields": {"key": "orgp", "title": "Org", "audience": "org", "select": [select.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$pubp", "parent": admin.to_string(),
             "fields": {"key": "pubp", "title": "Public", "audience": "public", "select": [select.to_string()]}}
        ]),
    );
    land(r, "admin");
    (admin, created(&w, "$orgp"), created(&w, "$pubp"))
}

fn put(out: &mut Vec<u8>, part: &str) {
    out.extend_from_slice(part.as_bytes());
    out.push(0);
}

/// The selection hash a profile with no id and no version has over `entries` and the one
/// attachment row of `att`, built here from the brief's byte layout with `-` for both.
fn implicit_hash(m: &Materialized, att: &[(BlockId, Hash, bool)]) -> Hash {
    let mut bytes = b"rdy-publication-selection-1\0".to_vec();
    put(&mut bytes, "-");
    put(&mut bytes, "-");
    for b in m.iter() {
        put(&mut bytes, b.id.as_str());
        put(&mut bytes, &m.version_of(&b.id).unwrap().to_string());
        put(&mut bytes, "-");
    }
    for (block, blob, ok) in att {
        put(&mut bytes, block.as_str());
        put(&mut bytes, &blob.to_string());
        put(&mut bytes, if *ok { "1" } else { "0" });
    }
    Hash::of(&bytes)
}

/// Every field of a resolution that two equal resolutions share.
fn assert_same(a: &Resolved, b: &Resolved) {
    assert_eq!(a.profile, b.profile);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.docs, b.docs);
    assert_eq!(a.attachments, b.attachments);
    assert_eq!(a.references, b.references);
    assert_eq!(a.selection_hash, b.selection_hash);
}

/// The implicit profile of `st`'s canon, checked against every block of the snapshot.
#[track_caller]
fn check_everything(st: &ReadState, f: &BlockId, blob: Hash) -> Resolved {
    let m = st.materialized(&Scope::Canon).unwrap();
    let r = publication::resolve(st, &Scope::Canon, IMPLICIT).unwrap();
    let want: Vec<(BlockId, Hash, Option<Hash>)> = m
        .iter()
        .map(|b| (b.id.clone(), m.version_of(&b.id).unwrap(), None))
        .collect();
    let got: Vec<(BlockId, Hash, Option<Hash>)> = r
        .entries
        .iter()
        .map(|e| (e.id.clone(), e.version, e.redaction))
        .collect();
    assert_eq!(got, want, "entries are every block of the snapshot");
    assert_eq!(r.m.len(), m.len());
    let p = &r.profile;
    assert_eq!((&p.id, &p.version, &p.key), (&None, &None, &None));
    assert_eq!(p.title, st.project_name());
    assert_eq!(p.description, st.project_description());
    assert_eq!(p.audience, Audience::Org);
    assert!(p.trail, "an org implicit profile shows the trail");
    let docs: Vec<BlockId> = m
        .iter()
        .filter(|b| b.block_type == tree::DOC)
        .map(|b| b.id.clone())
        .collect();
    assert_eq!(
        r.docs.iter().map(|d| d.id.clone()).collect::<Vec<_>>(),
        docs
    );
    assert_eq!(
        r.attachments
            .iter()
            .map(|a| (a.block.clone(), a.blob, a.available))
            .collect::<Vec<_>>(),
        vec![(f.clone(), blob, true)]
    );
    assert_eq!(
        r.selection_hash,
        implicit_hash(&m, &[(f.clone(), blob, true)]),
        "the selection hash writes - for the profile id and version"
    );
    assert_eq!(r.references.status, CheckStatus::Clear);
    assert_same(
        &r,
        &publication::resolve(st, &Scope::Canon, IMPLICIT).unwrap(),
    );
    r
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn pub12_implicit_profile_selects_every_block_with_null_identity() {
    // no profile in the project
    let (_dir, mut r) = project();
    let (ids, blob) = seed(&mut r);
    let st = r.read();
    let none = check_everything(&st, &ids["$f"], blob);
    drop(st);

    // two profiles: both profile blocks are in the implicit selection (O12)
    let (admin, orgp, pubp) = two_profiles(&mut r, &ids["$a"]);
    let st = r.read();
    let two = check_everything(&st, &ids["$f"], blob);
    for id in [&admin, &orgp, &pubp] {
        assert!(two.entries.iter().any(|e| &e.id == id), "{id}");
        assert!(two.m.get(id).is_some(), "{id}");
    }
    assert!(!none.entries.iter().any(|e| e.id == orgp));
    assert_ne!(none.selection_hash, two.selection_hash);
    // the named profiles keep their own identity and selection
    let named = publication::resolve(&st, &Scope::Canon, "orgp").unwrap();
    assert_eq!(named.profile.id, Some(orgp.clone()));
    assert!(named.entries.len() < two.entries.len());

    // Task 4's selection bound applies (O14): one block over it is refused, at it resolves
    let n = two.entries.len();
    {
        let _l = publication::override_limits_for_tests(publication::Limits {
            selection: n - 1,
            ..publication::Limits::default()
        });
        let e = publication::resolve(&st, &Scope::Canon, IMPLICIT).unwrap_err();
        assert_err(
            &e,
            E_LIMIT,
            &format!(
                "Publication selection exceeds the limit of {} blocks.",
                n - 1
            ),
            "Reduce the selection or split the source project.",
        );
    }
    {
        let _l = publication::override_limits_for_tests(publication::Limits {
            selection: n,
            ..publication::Limits::default()
        });
        assert!(publication::resolve(&st, &Scope::Canon, IMPLICIT).is_ok());
    }

    // the implicit profile is never public (FW-PB9a): no public implicit selection exists
    let e = publication::resolve(&st, &Scope::Canon, "implicit:public").unwrap_err();
    assert_err(&e, E_BAD_INPUT, ALWAYS_ORG.0, ALWAYS_ORG.1);

    // the scope rule stays Task 4's: a view is refused before any read
    let e = publication::resolve(&st, &Scope::View("work".into()), IMPLICIT).unwrap_err();
    assert_err(
        &e,
        E_BAD_INPUT,
        "A publication profile requires canon, a tag, or a landing.",
        "Use scope canon, tag:<name>, or landing:<hash>.",
    );
}

#[test]
fn pub12_implicit_values_are_exact() {
    let (_dir, mut r) = project();
    seed(&mut r);
    let st = r.read();
    assert!(publication::resolve(&st, &Scope::Canon, "implicit:org").is_ok());

    let e = publication::resolve(&st, &Scope::Canon, "implicit:public").unwrap_err();
    assert_err(&e, E_BAD_INPUT, ALWAYS_ORG.0, ALWAYS_ORG.1);

    for v in ["implicit:", "implicit:all", "implicit:org "] {
        let e = publication::resolve(&st, &Scope::Canon, v).unwrap_err();
        assert_err(
            &e,
            E_BAD_INPUT,
            &format!("Implicit profile \"{v}\" is not known."),
            "Use implicit:org.",
        );
    }
    for v in ["Implicit:org", " implicit:org"] {
        let e = publication::resolve(&st, &Scope::Canon, v).unwrap_err();
        assert_err(
            &e,
            E_NOT_FOUND,
            &format!("Publication profile \"{v}\" was not found."),
            "Use a publication profile id or key from this scope.",
        );
    }
    // no key or id holds a colon, so the value can never name a block
    assert!(rdy_model::key::problem("implicit:org").is_some());
    assert!(BlockId::parse("implicit:org").is_err());

    // the profile-name bound stays Task 4's and applies first
    let long = format!("implicit:{}", "x".repeat(200));
    let e = publication::resolve(&st, &Scope::Canon, &long).unwrap_err();
    assert_err(
        &e,
        E_LIMIT,
        "Publication profile name exceeds the limit of 128 bytes.",
        "Reduce the selection or split the source project.",
    );
}

#[test]
fn pub12_implicit_references_count_missing_targets_only() {
    let (_dir, r) = project();
    let st = r.read();
    let blk = |id: &str, ty: &str, parent: Option<&str>, order: &str, body: &str| Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: order.into(),
        fields: [("name".to_string(), FieldValue::Str(id.to_lowercase()))]
            .into_iter()
            .filter(|_| ty == "doc")
            .collect(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    // `d-ANCH0` refers to `d-0THER`, which is in the source (and so selected), and to
    // `d-M1SS1`, which is in no block of the source
    let blocks = vec![
        blk("d-ANCH0", "doc", None, "a", ""),
        blk(
            "p-ANCH0",
            "prose",
            Some("d-ANCH0"),
            "a",
            "See {{d-0THER}} and {{d-M1SS1}}.",
        ),
        blk("d-0THER", "doc", None, "b", ""),
    ];
    let m = Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    );
    let res = publication::resolve_in_for_tests(&st, &m, IMPLICIT).unwrap();
    let refs = &res.references;
    assert_eq!(
        (refs.missing_count, refs.excluded_count, refs.blocked_count),
        (1, 0, 1)
    );
    assert_eq!(refs.rows.len(), 1);
    assert_eq!(refs.rows[0].target, "d-M1SS1");
    assert_eq!(refs.rows[0].source, bid("p-ANCH0"));
    // the implicit profile is org: the one warning, and output goes ahead
    assert_eq!(refs.status, CheckStatus::Warning);
    let w = res.references.warning().expect("the warning");
    assert_eq!(
        (w.code, w.message.as_str(), w.fix.as_str()),
        (
            W_PUBLICATION_REFERENCE,
            "Org profile has 1 references outside its selection.",
            "Review these references before you share the output.",
        )
    );
    assert!(res.require_output().is_ok());
    // a public implicit profile, which would be blocked, cannot be requested (FW-PB9a)
    let e = publication::resolve_in_for_tests(&st, &m, "implicit:public").unwrap_err();
    assert_err(&e, E_BAD_INPUT, ALWAYS_ORG.0, ALWAYS_ORG.1);
}

#[test]
fn pub12_implicit_diff_uses_the_same_rules() {
    let (_dir, mut r) = project();
    let (ids, _blob) = seed(&mut r);
    let (_admin, _orgp, pubp) = two_profiles(&mut r, &ids["$a"]);
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let w = write(
        &mut r,
        "more",
        json!([{"op": "create", "type": "doc", "id": "$c", "parent": ids["$area"].to_string(),
            "fields": {"name": "c", "title": "C", "doc_type": "page"}}]),
    );
    let c = created(&w, "$c");
    land(&mut r, "more");
    let st = r.read();

    // implicit at two landings: the new doc is added
    let tagged = st.resolve_scope("tag:v1.0.0").unwrap();
    let before = publication::resolve(&st, &tagged, IMPLICIT).unwrap();
    let after = publication::resolve(&st, &Scope::Canon, IMPLICIT).unwrap();
    let d = publication::disclosure_diff(&before, &after).unwrap();
    assert_eq!(
        d.added_docs
            .iter()
            .map(|x| x.id.clone())
            .collect::<Vec<_>>(),
        vec![c.clone()]
    );
    assert!(d.removed_docs.is_empty() && d.changed_docs.is_empty());
    assert_eq!(d.profile_id, None);
    assert_eq!(
        (d.before_profile_version, d.after_profile_version),
        (None, None)
    );
    assert!(!d.profile_changed, "two implicit sides have no revision");
    assert_ne!(d.before_selection_hash, d.after_selection_hash);
    // the check report carries the same comparison, with null identity
    let v = publication::check(&st, IMPLICIT, None, Some("tag:v1.0.0")).unwrap();
    assert_eq!(v["disclosure"]["added_docs"][0]["id"], c.to_string());
    assert_eq!(v["disclosure"]["profile_id"], Value::Null);
    assert_eq!(v["disclosure"]["profile_changed"], false);
    for k in ["id", "version", "key"] {
        assert_eq!(v["profile"][k], Value::Null, "{k}");
    }

    // implicit against a named profile, either way round: the profile-mismatch error
    let named = publication::resolve(&st, &Scope::Canon, "orgp").unwrap();
    let public = publication::resolve(&st, &Scope::Canon, &pubp.to_string()).unwrap();
    for (b, a) in [(&named, &after), (&after, &named), (&public, &after)] {
        let e = publication::disclosure_diff(b, a).unwrap_err();
        assert_err(&e, E_BAD_INPUT, PROFILE_MISMATCH.0, PROFILE_MISMATCH.1);
    }
    // two sides with the same (null) id but different audiences: the audience-mismatch error.
    // The request never makes a public implicit side (FW-PB9a), so the second side is built here.
    let mut other = after.clone();
    other.profile.audience = Audience::Public;
    other.profile.trail = false;
    let e = publication::disclosure_diff(&after, &other).unwrap_err();
    assert_err(&e, E_BAD_INPUT, AUDIENCE_MISMATCH.0, AUDIENCE_MISMATCH.1);
    let e = publication::check(&st, "implicit:public", None, Some("canon")).unwrap_err();
    assert_err(&e, E_BAD_INPUT, ALWAYS_ORG.0, ALWAYS_ORG.1);

    // a self-diff is empty
    let d = publication::disclosure_diff(&after, &after).unwrap();
    assert!(
        d.added_docs.is_empty()
            && d.removed_docs.is_empty()
            && d.changed_docs.is_empty()
            && d.changed_folders.is_empty()
            && d.added_attachments.is_empty()
            && d.removed_attachments.is_empty()
            && d.changed_attachments.is_empty()
            && d.new_references.is_empty()
            && d.new_reference_count == 0
            && !d.profile_changed
    );
    assert_eq!(d.before_selection_hash, d.after_selection_hash);
}

// ---------------------------------------------------------------------------------------------
// fix round 1: the implicit profile checks the tree as a named profile does

/// A block for a state built in memory, outside any store.
fn blk(id: &str, ty: &str, parent: Option<&str>, name: &str) -> Block {
    Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: "a".into(),
        fields: [("name".to_string(), FieldValue::Str(name.into()))].into(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
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

const INVALID_TREE: (&str, &str) = (
    "Publication selection has an invalid document tree.",
    "Run rdy fsck and fix the document tree.",
);

#[test]
fn pub12_implicit_refuses_an_invalid_tree() {
    let (_dir, r) = project();
    let st = r.read();
    // a valid tree resolves: a folder, a doc in it, a section and a decision in the doc
    let valid = vec![
        blk("fd-0KAY1", "folder", None, "ok"),
        blk("d-0KAY2", "doc", Some("fd-0KAY1"), "okdoc"),
        blk("s-0KAY3", "section", Some("d-0KAY2"), "oksec"),
        blk("D-0KAY4", "decision", Some("s-0KAY3"), "okdec"),
    ];
    let res = publication::resolve_in_for_tests(&st, &synth(valid.clone()), IMPLICIT).unwrap();
    assert_eq!(res.entries.len(), 4);
    let cases: Vec<(&str, Vec<Block>)> = vec![
        (
            "orphan doc",
            vec![blk("d-0RPH1", "doc", Some("fd-G0NE1"), "orphan")],
        ),
        (
            "orphan prose",
            vec![blk("p-0RPH2", "prose", Some("d-G0NE2"), "orphan")],
        ),
        (
            "folder loop holding a doc",
            vec![
                blk("fd-R00P1", "folder", Some("fd-R00P2"), "loop1"),
                blk("fd-R00P2", "folder", Some("fd-R00P1"), "loop2"),
                blk("d-R00P3", "doc", Some("fd-R00P1"), "inloop"),
            ],
        ),
        (
            "bare folder loop",
            vec![
                blk("fd-R00P1", "folder", Some("fd-R00P2"), "loop1"),
                blk("fd-R00P2", "folder", Some("fd-R00P1"), "loop2"),
            ],
        ),
        (
            "block loop",
            vec![
                blk("s-R00P4", "section", Some("s-R00P5"), "loop4"),
                blk("s-R00P5", "section", Some("s-R00P4"), "loop5"),
            ],
        ),
        (
            "doc in a doc",
            vec![
                blk("d-0VTER", "doc", None, "outer"),
                blk("d-NEST1", "doc", Some("d-0VTER"), "nested"),
            ],
        ),
        (
            "decision with no home doc",
            vec![
                blk("fd-H0ME1", "folder", None, "homeless"),
                blk("D-N0D0C", "decision", Some("fd-H0ME1"), "decision"),
            ],
        ),
        (
            "top-level decision",
            vec![blk("D-T0P01", "decision", None, "top")],
        ),
    ];
    for (label, extra) in cases {
        let mut blocks = valid.clone();
        blocks.extend(extra);
        let e = publication::resolve_in_for_tests(&st, &synth(blocks), IMPLICIT)
            .err()
            .unwrap_or_else(|| panic!("{label}: an invalid tree resolved"));
        assert_err(&e, E_BAD_INPUT, INVALID_TREE.0, INVALID_TREE.1);
    }
    // the tree is checked before any text is scanned: a doc in a doc with a body past the text
    // bound gives the tree error, not the limit
    let _g = publication::override_limits_for_tests(publication::Limits {
        text_value: 8,
        ..publication::Limits::default()
    });
    let mut nested = blk("d-NEST1", "doc", Some("d-0VTER"), "nested");
    nested.body = "far more than eight bytes".into();
    let e = publication::resolve_in_for_tests(
        &st,
        &synth(vec![blk("d-0VTER", "doc", None, "outer"), nested]),
        IMPLICIT,
    )
    .unwrap_err();
    assert_err(&e, E_BAD_INPUT, INVALID_TREE.0, INVALID_TREE.1);
}

/// Lands a doc `kept` with prose and a named profile `site` selecting it, then stores a landing
/// whose snapshot gives `kept` the absent parent `fd-G0NE1`. Returns that landing and `kept`.
fn orphan_landing(r: &mut Rdy) -> (Hash, BlockId) {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"name": "kept", "title": "ORPHANDOC", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let id = created(&w, "$doc");
    let admin = created(&w, "$admin");
    write(
        r,
        "work",
        json!([
            {"op": "create", "type": "prose", "parent": id.to_string(), "body": "ORPHANWORDS"},
            {"op": "create", "type": "publication_profile", "parent": admin.to_string(),
             "fields": {"key": "site", "title": "Site", "audience": "org", "select": [id.to_string()]}}
        ]),
    );
    land(r, "work");
    let head = r.read().canon_head().unwrap();
    let m = r.read().materialized(&Scope::Canon).unwrap();
    let mut snapshot = m.snapshot();
    let mut orphan = m.get(&id).unwrap().clone();
    orphan.parent = Some(bid("fd-G0NE1"));
    let hash = r.store_mut().put(rdy_store::kinds::BLOCK, &orphan).unwrap();
    snapshot.blocks.insert(id.clone(), hash);
    let state = r
        .store_mut()
        .put(rdy_store::kinds::SNAPSHOT, &snapshot)
        .unwrap();
    let mut landing = r.store().landing(&head).unwrap();
    // a forged record from a read: it no longer has the read's identity
    landing.read_identity = None;
    landing.state = state;
    landing.prev = Some(head);
    let damaged = r
        .store_mut()
        .put(rdy_store::kinds::LANDING, &landing)
        .unwrap();
    r.invalidate();
    (damaged, id)
}

#[test]
fn pub12_implicit_refuses_a_stored_orphan_doc_as_a_named_profile_does() {
    let (_dir, mut r) = project();
    let (damaged, _) = orphan_landing(&mut r);
    let st = r.read();
    let scope = Scope::At(damaged);
    for name in ["site", IMPLICIT] {
        let e = publication::resolve(&st, &scope, name)
            .err()
            .unwrap_or_else(|| panic!("{name}: the orphan doc resolved"));
        assert_err(&e, E_BAD_INPUT, INVALID_TREE.0, INVALID_TREE.1);
    }
    // the canon before the damage still resolves both
    for name in ["site", IMPLICIT] {
        publication::resolve(&st, &Scope::Canon, name).unwrap();
    }
}
