//! pub4 (D-48NYM, D-0VCQ5, D-KDFZ3): a publication profile resolves to one bounded snapshot of
//! the source. It selects whole docs and folders with their ancestor folders' own records, builds
//! the selected state and its induced graph, hashes the selection, lists its attachments, and
//! checks every reference that leaves the selection under the audience's fixed policy.
mod common;
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::publication::{
    self, Audience, CheckStatus, Classification, Limits, ReferenceRow, Resolved,
};
use rdy_engine::*;
use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget, RedactedBlockIdentity};
use rdy_model::*;
use rdy_store::kinds::{self, BlobManifest};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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

fn id_set(ids: &[&BlockId]) -> BTreeSet<BlockId> {
    ids.iter().map(|i| (*i).clone()).collect()
}

fn entry_ids(res: &Resolved) -> Vec<BlockId> {
    res.entries.iter().map(|e| e.id.clone()).collect()
}

/// Everything a resolution yields that two equal resolutions must share.
fn assert_same(a: &Resolved, b: &Resolved) {
    assert_eq!(a.profile, b.profile);
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.docs, b.docs);
    assert_eq!(a.attachments, b.attachments);
    assert_eq!(a.references, b.references);
    assert_eq!(a.selection_hash, b.selection_hash);
    assert_eq!(a.graph, b.graph);
    assert_eq!(a.m.snapshot(), b.m.snapshot());
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

const BAD_SCOPE: (&str, &str) = (
    "A publication profile requires canon, a tag, or a landing.",
    "Use scope canon, tag:<name>, or landing:<hash>.",
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

fn link(mut b: Block, kind: &str, target: &str) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: bid(target),
        part: None,
    });
    b
}

fn profile_block(id: &str, parent: &str, key: &str, audience: &str, select: &[&str]) -> Block {
    let mut b = blk(id, "publication_profile", Some(parent), "z");
    b.fields.insert("key".into(), s(key));
    b.fields.insert("title".into(), s("Profile title"));
    b.fields.insert("audience".into(), s(audience));
    b.fields.insert(
        "select".into(),
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    );
    b
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

fn stub(id: &str, ty: &str, parent: Option<&str>, request: Hash) -> Block {
    Block::redacted(
        bid(id),
        ty.into(),
        parent.map(bid),
        "m".into(),
        RedactedBlockIdentity {
            version: Hash::of(format!("version of {id}").as_bytes()),
            content_hash: Hash::of(format!("content of {id}").as_bytes()),
            frozen: false,
            request,
        },
    )
}

/// `v` nested in single-element lists until its innermost value sits `depth` levels down (the
/// top-level field value is level 1).
fn nest(depth: usize) -> FieldValue {
    let mut v = s("{{d-DR0P1}}");
    for _ in 1..depth {
        v = FieldValue::List(vec![v]);
    }
    v
}

/// The bytes of one selection entry row.
fn entry_row(out: &mut Vec<u8>, id: &BlockId, version: &Hash, redaction: Option<Hash>) {
    for part in [
        id.to_string(),
        version.to_string(),
        redaction.map_or("-".into(), |h| h.to_string()),
    ] {
        out.extend_from_slice(part.as_bytes());
        out.push(0);
    }
}

fn attachment_row(out: &mut Vec<u8>, block: &BlockId, blob: &Hash, available: bool) {
    for part in [
        block.to_string(),
        blob.to_string(),
        if available { "1" } else { "0" }.to_string(),
    ] {
        out.extend_from_slice(part.as_bytes());
        out.push(0);
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

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn pub4_selection_is_whole_docs_folders_and_ancestor_records() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$top", "fields": {"name": "top"}},
            {"op": "create", "type": "folder", "id": "$area", "parent": "$top", "fields": {"name": "area"}},
            {"op": "create", "type": "folder", "id": "$inner", "parent": "$area", "fields": {"name": "inner"}},
            {"op": "create", "type": "folder", "id": "$empty", "parent": "$area", "fields": {"name": "empty"}},
            {"op": "create", "type": "doc", "id": "$deep", "parent": "$inner", "fields": {"name": "deep", "title": "Deep", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$deeps", "parent": "$deep", "fields": {"title": "Deep section"}},
            {"op": "create", "type": "prose", "id": "$deepp", "parent": "$deeps", "body": "Deep text."},
            {"op": "create", "type": "doc", "id": "$one", "parent": "$area", "fields": {"name": "one", "title": "One", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ones", "parent": "$one", "fields": {"title": "Rulings"}},
            {"op": "create", "type": "decision", "id": "$x1", "parent": "$ones", "fields": {"statement": "Keep the area.", "status": "proposed"}},
            {"op": "create", "type": "doc", "id": "$sib", "parent": "$top", "fields": {"name": "sibling", "title": "Sibling", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$sibs", "parent": "$sib", "fields": {"title": "Other rulings"}},
            {"op": "create", "type": "decision", "id": "$x2", "parent": "$sibs", "fields": {"statement": "Drop the sibling.", "status": "proposed"}},
            {"op": "create", "type": "doc", "id": "$solo", "parent": "$top", "fields": {"name": "solo", "title": "Solo", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$solop", "parent": "$solo", "body": "Solo text."},
            {"op": "create", "type": "folder", "id": "$other", "fields": {"name": "other"}},
            {"op": "create", "type": "doc", "id": "$o1", "parent": "$other", "fields": {"name": "o1", "title": "O1", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let c = |n: &str| created(&w, n);
    let (top, area, inner, empty) = (c("$top"), c("$area"), c("$inner"), c("$empty"));
    let (deep, deeps, deepp) = (c("$deep"), c("$deeps"), c("$deepp"));
    let (one, ones, x1) = (c("$one"), c("$ones"), c("$x1"));
    let (sib, x2, solo, solop) = (c("$sib"), c("$x2"), c("$solo"), c("$solop"));
    let (other, o1, admin) = (c("$other"), c("$o1"), c("$admin"));
    // author order, unsorted; `one` overlaps `area`
    let p = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin.to_string(),
            "fields": {"key": "pubsite", "title": "Site", "description": "The site.", "audience": "org",
                "select": [solo.to_string(), one.to_string(), area.to_string()]}}]),
    );
    let pf = created(&p, "$pf");
    land(&mut r, "work");
    let st = r.read();
    let source = st.materialized(&Scope::Canon).unwrap();
    let res = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();

    // the profile, read from this snapshot
    assert_eq!(res.profile.id, Some(pf.clone()));
    assert_eq!(res.profile.version, source.version_of(&pf));
    assert_eq!(res.profile.key.as_deref(), Some("pubsite"));
    assert_eq!(res.profile.title, "Site");
    assert_eq!(res.profile.description.as_deref(), Some("The site."));
    assert_eq!(res.profile.audience, Audience::Org);
    assert!(
        res.profile.trail,
        "an org profile shows its trail by default"
    );

    // whole docs and folders, their ancestor folders, nothing else
    let want = id_set(&[
        &top, &area, &inner, &empty, &deep, &deeps, &deepp, &one, &ones, &x1, &solo, &solop,
    ]);
    assert_eq!(entry_ids(&res), want.iter().cloned().collect::<Vec<_>>());
    assert_eq!(res.m.ids().cloned().collect::<BTreeSet<_>>(), want);
    for e in &res.entries {
        assert_eq!(Some(e.version), source.version_of(&e.id), "{}", e.id);
        assert_eq!(e.redaction, None);
        assert_eq!(res.m.version_of(&e.id), Some(e.version));
    }
    for gone in [&sib, &x2, &other, &o1, &admin, &pf] {
        assert!(res.m.get(gone).is_none(), "{gone} is not selected");
    }
    // the ancestor folder keeps its own record, but none of its other children
    assert_eq!(res.m.get(&top), source.get(&top));
    let under_top: BTreeSet<BlockId> = res
        .m
        .children(Some(&top))
        .iter()
        .map(|b| b.id.clone())
        .collect();
    assert_eq!(under_top, id_set(&[&area, &solo]));
    // an empty folder and a nested folder come with their selected folder
    assert!(res.m.children(Some(&empty)).is_empty());
    assert_eq!(res.m.get(&inner), source.get(&inner));
    // a decision comes with its own home doc only
    assert_eq!(res.m.doc_of(&x1).map(|d| d.id.clone()), Some(one.clone()));
    assert!(res.graph.status(&x1).is_some());
    assert!(res.graph.status(&x2).is_none());
    // docs, in id order, with their paths in the selected tree
    let mut docs = vec![
        (deep.clone(), "top/area/inner/deep".to_string()),
        (one.clone(), "top/area/one".to_string()),
        (solo.clone(), "top/solo".to_string()),
    ];
    docs.sort();
    assert_eq!(
        res.docs
            .iter()
            .map(|d| (d.id.clone(), d.path.clone()))
            .collect::<Vec<_>>(),
        docs
    );
    assert!(res.attachments.is_empty());
    assert_eq!(res.references.status, CheckStatus::Clear);
    assert_eq!(res.references.blocked_count, 0);
    assert!(res.references.rows.is_empty());
    assert!(res.require_output().is_ok());

    // deterministic: again, and by id, the same snapshot
    let again = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
    assert_same(&res, &again);
    let by_id = publication::resolve(&st, &Scope::Canon, &pf.to_string()).unwrap();
    assert_same(&res, &by_id);

    // the profile and selector lookups, with their exact texts
    let e = publication::resolve(&st, &Scope::Canon, "nosuch").unwrap_err();
    assert_err(
        &e,
        E_NOT_FOUND,
        "Publication profile \"nosuch\" was not found.",
        "Use a publication profile id or key from this scope.",
    );
    let e = publication::resolve(&st, &Scope::Canon, "pf-ZZZZZ").unwrap_err();
    assert_err(
        &e,
        E_NOT_FOUND,
        "Publication profile \"pf-ZZZZZ\" was not found.",
        "Use a publication profile id or key from this scope.",
    );
    let e = publication::resolve(&st, &Scope::Canon, &solo.to_string()).unwrap_err();
    assert_err(
        &e,
        E_BAD_INPUT,
        &format!("Block {solo} is not a publication profile."),
        "Use a publication profile id or key.",
    );
    let req = Hash::of(b"a request");
    let m = synth(vec![
        named(blk("d-ADM1N", "doc", None, "a"), "admin"),
        named(blk("d-KEEP1", "doc", None, "b"), "keep"),
        with_body(blk("p-KEEP1", "prose", Some("d-KEEP1"), "a"), "Kept."),
        stub("pf-G0NE1", "publication_profile", Some("d-ADM1N"), req),
        profile_block(
            "pf-AB5NT",
            "d-ADM1N",
            "absent",
            "org",
            &["d-KEEP1", "d-ZZZZZ"],
        ),
        profile_block("pf-TYPE1", "d-ADM1N", "wrongtype", "org", &["p-KEEP1"]),
    ]);
    let e = publication::resolve_in_for_tests(&st, &m, "pf-G0NE1").unwrap_err();
    assert_err(
        &e,
        E_BAD_INPUT,
        "Publication profile pf-G0NE1 has been redacted.",
        "Choose a profile whose content is available.",
    );
    let e = publication::resolve_in_for_tests(&st, &m, "absent").unwrap_err();
    assert_err(
        &e,
        E_NOT_FOUND,
        "Publication selector d-ZZZZZ was not found in this scope.",
        "Remove this selector or choose a scope that contains it.",
    );
    let e = publication::resolve_in_for_tests(&st, &m, "wrongtype").unwrap_err();
    assert_err(
        &e,
        E_BAD_INPUT,
        "Publication selector p-KEEP1 must name a doc or folder.",
        "Select the containing doc or folder.",
    );
}

#[test]
fn pub4_tag_uses_its_own_profile_revision() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, ap, b, admin) = (
        created(&w, "$a"),
        created(&w, "$ap"),
        created(&w, "$b"),
        created(&w, "$admin"),
    );
    land(&mut r, "work");
    r.tag(
        &founder(),
        "v0.1.0",
        None,
        Some("before the profile".into()),
    )
    .unwrap();
    let p = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": admin.to_string(),
            "fields": {"key": "pubsite", "title": "Old title", "audience": "public", "select": [a.to_string()]}}]),
    );
    let pf = created(&p, "$pf");
    let l1 = land(&mut r, "work");
    r.tag(&founder(), "v1.0.0", None, Some("old profile".into()))
        .unwrap();
    let old_version = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&pf);
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": pf.to_string(), "fields": {"title": "New title", "select": [b.to_string()], "trail": true}}]),
    );
    land(&mut r, "work");
    let st = r.read();

    let now = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
    assert_eq!(entry_ids(&now), vec![b.clone()]);
    assert_eq!(now.profile.title, "New title");
    assert!(now.profile.trail);
    assert_ne!(now.profile.version, old_version);

    let tag = st.resolve_scope("tag:v1.0.0").unwrap();
    let before = st.materialized(&tag).unwrap().snapshot();
    let then = publication::resolve(&st, &tag, "pubsite").unwrap();
    assert_eq!(
        entry_ids(&then),
        id_set(&[&a, &ap]).into_iter().collect::<Vec<_>>()
    );
    assert_eq!(then.profile.version, old_version);
    assert_eq!(then.profile.title, "Old title");
    assert_eq!(then.profile.audience, Audience::Public);
    assert!(
        !then.profile.trail,
        "a public profile hides its trail by default"
    );
    assert_ne!(then.selection_hash, now.selection_hash);
    // the same landing, named directly, selects the same set
    let landing = st.resolve_scope(&format!("landing:{l1}")).unwrap();
    assert_same(
        &then,
        &publication::resolve(&st, &landing, "pubsite").unwrap(),
    );
    // the tag stays an ordinary read
    assert_eq!(st.materialized(&tag).unwrap().snapshot(), before);
    // a tag from before the profile has none: no fallback to canon
    let early = st.resolve_scope("tag:v0.1.0").unwrap();
    let e = publication::resolve(&st, &early, "pubsite").unwrap_err();
    assert_err(
        &e,
        E_NOT_FOUND,
        "Publication profile \"pubsite\" was not found.",
        "Use a publication profile id or key from this scope.",
    );
    // views, proposals and the in-flight scope are refused before any read
    for scope in [
        Scope::View("work".into()),
        Scope::View("nosuch".into()),
        Scope::Inflight,
        Scope::Proposal(Hash::of(b"a proposal")),
    ] {
        let e = publication::resolve(&st, &scope, "pubsite").unwrap_err();
        assert_err(&e, E_BAD_INPUT, BAD_SCOPE.0, BAD_SCOPE.1);
    }
}

#[test]
fn pub4_selected_graph_has_no_excluded_endpoint() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$k", "fields": {"name": "keep", "title": "Keep", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$ks", "parent": "$k", "fields": {"title": "Kept rulings"}},
            {"op": "create", "type": "doc", "id": "$x", "fields": {"name": "drop", "title": "Drop", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$xs", "parent": "$x", "fields": {"title": "Dropped rulings"}},
            {"op": "create", "type": "decision", "id": "$dx2", "parent": "$xs", "fields": {"statement": "The far premise.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$dx1", "parent": "$xs", "fields": {"statement": "The near premise.", "status": "proposed"},
                "links": {"depends_on": [{"target": "$dx2"}]}},
            {"op": "create", "type": "decision", "id": "$dk1", "parent": "$ks", "fields": {"statement": "The kept ruling.", "status": "proposed"},
                "links": {"depends_on": [{"target": "$dx1"}]}},
            {"op": "create", "type": "decision", "id": "$dk2", "parent": "$ks", "fields": {"statement": "The kept refinement.", "status": "proposed"},
                "links": {"depends_on": [{"target": "$dk1"}]}},
            {"op": "create", "type": "decision", "id": "$dx3", "parent": "$xs", "fields": {"statement": "A dropped refinement.", "status": "proposed"},
                "links": {"depends_on": [{"target": "$dk1"}]}},
            {"op": "create", "type": "claim", "id": "$kc", "parent": "$ks", "body": "The kept claim names {{$dx1}} and {{$dk2}}.",
                "links": {"decided_by": [{"target": "$dk1"}, {"target": "$dx1"}]}},
            {"op": "create", "type": "claim", "id": "$xc", "parent": "$xs", "body": "The dropped claim names {{$dk1}}.",
                "links": {"decided_by": [{"target": "$dk2"}]}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let c = |n: &str| created(&w, n);
    let (k, ks, dk1, dk2, kc) = (c("$k"), c("$ks"), c("$dk1"), c("$dk2"), c("$kc"));
    let (x, xs, dx1, dx2, dx3, xc) = (c("$x"), c("$xs"), c("$dx1"), c("$dx2"), c("$dx3"), c("$xc"));
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": c("$admin").to_string(),
            "fields": {"key": "pubsite", "title": "Site", "audience": "org", "select": [k.to_string()]}}]),
    );
    land(&mut r, "work");
    let st = r.read();
    let full = st.graph(&Scope::Canon).unwrap();
    let res = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
    let keep = id_set(&[&k, &ks, &dk1, &dk2, &kc]);
    assert_eq!(res.m.ids().cloned().collect::<BTreeSet<_>>(), keep);
    // the source graph crosses the boundary both ways and transitively
    assert!(full.outgoing(&dk1).iter().any(|e| e.to == dx1));
    assert!(
        full.incoming(&dk1)
            .iter()
            .any(|e| e.from == dx3 || e.from == xc)
    );
    assert!(full.impact(&dx2, 10).iter().any(|(_, id)| *id == dk1));
    assert!(full.impact(&dx2, 10).iter().any(|(_, id)| *id == dk2));
    // the selected graph never does
    for id in &keep {
        for e in res.graph.outgoing(id).iter().chain(res.graph.incoming(id)) {
            assert!(
                keep.contains(&e.from) && keep.contains(&e.to),
                "{id}: {e:?} leaves the selection"
            );
        }
        for (_, hit) in res.graph.impact(id, 10) {
            assert!(keep.contains(&hit), "{id}: impact reaches {hit}");
        }
        for g in res.graph.governing(id) {
            assert!(keep.contains(&g), "{id}: governed by {g}");
        }
    }
    for id in [&x, &xs, &dx1, &dx2, &dx3, &xc] {
        assert!(res.graph.outgoing(id).is_empty() && res.graph.incoming(id).is_empty());
        assert!(res.graph.status(id).is_none());
    }
    // the edges inside the selection stay
    assert!(res.graph.outgoing(&dk2).iter().any(|e| e.to == dk1));
    assert!(res.graph.outgoing(&kc).iter().any(|e| e.to == dk1));
    assert!(res.graph.outgoing(&kc).iter().any(|e| e.to == dk2));
    assert!(
        res.graph
            .incoming(&dk1)
            .iter()
            .all(|e| keep.contains(&e.from))
    );
    // it is the selected blocks' graph, induced on exactly those ids
    let rebuilt =
        rdy_graph::Graph::build_with(res.m.iter(), res.graph.tracking().clone()).induced(&keep);
    assert_eq!(res.graph, rebuilt);
    assert_eq!(**res.graph.tracking(), st.pack().tracking());
    // the boundary-crossing references are reported, never followed
    assert_eq!(res.references.status, CheckStatus::Warning);
    let sources: BTreeSet<BlockId> = res
        .references
        .rows
        .iter()
        .map(|r| r.source.clone())
        .collect();
    assert_eq!(sources, id_set(&[&dk1, &kc]));
    assert!(
        res.references
            .rows
            .iter()
            .all(|r| r.target == dx1.to_string() && r.classification == Classification::Excluded)
    );
}

#[test]
fn pub4_references_count_occurrences_and_all_string_locations() {
    let (_dir, r) = project();
    let st = r.read();
    let mut table = blk("tb-TAB01", "table", Some("d-KEEP1"), "e");
    table.fields.insert(
        "columns".into(),
        FieldValue::List(vec![s("a"), s("{{d-DR0P1}}"), FieldValue::Int(7)]),
    );
    table.fields.insert(
        "rows".into(),
        FieldValue::List(vec![FieldValue::List(vec![
            s("x"),
            FieldValue::Map(BTreeMap::from([
                (
                    "n/te~".to_string(),
                    s("{{d-DR0P1}} {{d-DR0P1|Secret label}}"),
                ),
                ("ok".to_string(), s("{{d-KEEP1}}")),
            ])),
        ])]),
    );
    let mut claim = with_body(
        blk("c-ANCH1", "claim", Some("d-KEEP1"), "a"),
        "Claim on {{d-DR0P1}}.",
    );
    claim = link(claim, "see_also", "d-KEEP1");
    claim = link(claim, "see_also", "d-DR0P1");
    claim = link(claim, "see_also", "d-DR0P1");
    claim = link(claim, "depends_on", "D-M1SS2");
    claim = link(claim, "a/b~c", "d-DR0P1");
    claim.pins.insert(bid("d-DR0P2"), Hash::of(b"pinned"));
    claim.pins.insert(bid("d-DR0P1"), Hash::of(b"pinned too"));
    let m = synth(vec![
        named(blk("d-ADM1N", "doc", None, "a"), "admin"),
        profile_block("pf-PR0F1", "d-ADM1N", "pubsite", "org", &["d-KEEP1"]),
        with_field(
            named(blk("d-KEEP1", "doc", None, "b"), "keep"),
            "key",
            s("keep-key"),
        ),
        with_field(
            with_field(
                named(blk("d-DR0P1", "doc", None, "c"), "drop"),
                "key",
                s("drop-key"),
            ),
            "title",
            s("Dropped title"),
        ),
        named(blk("d-DR0P2", "doc", None, "d"), "drop2"),
        with_body(
            blk("p-DR0P1", "prose", Some("d-DR0P1"), "a"),
            "Unselected {{d-M1SS1}} text never counts.",
        ),
        with_body(
            blk("p-REF01", "prose", Some("d-KEEP1"), "b"),
            "See {{d-DR0P1}} and {{d-DR0P1}} again, {{d-KEEP1|shown}}, \\{{d-DR0P1}} escaped, \
             {{d-M1SS1}}, {{drop-key}}, {{keep-key}}, {{d-DR0P1|@key}}, {{ d-DR0P1 | Secret label }}.",
        ),
        with_field(
            with_body(
                blk("cd-C0DE1", "code", Some("d-KEEP1"), "c"),
                "x = {{d-DR0P1}}",
            ),
            "lang",
            s("{{d-DR0P1}}"),
        ),
        with_field(
            with_body(
                blk("dg-D1AG1", "diagram", Some("d-KEEP1"), "d"),
                "A{{Decide?}} --> B{{d-DR0P1}}",
            ),
            "alt",
            s("{{d-M1SS1}}"),
        ),
        table,
        claim,
        // a profile inside a selected doc: its bare selector strings are no references
        profile_block("pf-1NNER", "d-KEEP1", "inner", "public", &["d-DR0P1"]),
    ]);
    let res = publication::resolve_in_for_tests(&st, &m, "pubsite").unwrap();
    use Classification::{Excluded, Missing};
    let want = vec![
        row("c-ANCH1", "/links/a~1b~0c/0", "d-DR0P1", Excluded, 1),
        row("c-ANCH1", "/links/depends_on/0", "D-M1SS2", Missing, 1),
        row("c-ANCH1", "/links/see_also/1", "d-DR0P1", Excluded, 1),
        row("c-ANCH1", "/links/see_also/2", "d-DR0P1", Excluded, 1),
        row("c-ANCH1", "body", "d-DR0P1", Excluded, 1),
        row("cd-C0DE1", "/fields/lang", "d-DR0P1", Excluded, 1),
        row("dg-D1AG1", "/fields/alt", "d-M1SS1", Missing, 1),
        row("dg-D1AG1", "body", "d-DR0P1", Excluded, 1),
        row("p-REF01", "body", "d-DR0P1", Excluded, 4),
        row("p-REF01", "body", "d-M1SS1", Missing, 1),
        row("p-REF01", "body", "drop-key", Excluded, 1),
        row("tb-TAB01", "/fields/columns/1", "d-DR0P1", Excluded, 1),
        row(
            "tb-TAB01",
            "/fields/rows/0/1/n~1te~0",
            "d-DR0P1",
            Excluded,
            2,
        ),
    ];
    assert_eq!(res.references.rows, want);
    assert_eq!(res.references.excluded_count, 14);
    assert_eq!(res.references.missing_count, 3);
    assert_eq!(res.references.blocked_count, 17);
    assert_eq!(res.references.status, CheckStatus::Warning);
    // the rows hold the source's own tokens, never a target's title, key or the display text
    let shown = format!("{:?}", res.references);
    for leak in ["Dropped title", "Secret label", "shown", "drop2", "d-DR0P2"] {
        assert!(!shown.contains(leak), "{leak} in {shown}");
    }
    // a repeat gives the same rows
    let again = publication::resolve_in_for_tests(&st, &m, "pubsite").unwrap();
    assert_eq!(again.references, res.references);
}

#[test]
fn pub4_reference_policy_has_exact_text_and_no_target_labels() {
    let (_dir, r) = project();
    let st = r.read();
    let m = synth(vec![
        named(blk("d-ADM1N", "doc", None, "a"), "admin"),
        profile_block("pf-0RG01", "d-ADM1N", "orgsite", "org", &["d-KEEP1"]),
        profile_block("pf-PBC01", "d-ADM1N", "pubsite", "public", &["d-KEEP1"]),
        profile_block("pf-PBC02", "d-ADM1N", "onesite", "public", &["d-0NE01"]),
        profile_block("pf-CR001", "d-ADM1N", "clearsite", "public", &["d-DR0P1"]),
        profile_block("pf-CR002", "d-ADM1N", "clearorg", "org", &["d-DR0P1"]),
        named(blk("d-KEEP1", "doc", None, "b"), "keep"),
        with_body(
            blk("p-KEEP1", "prose", Some("d-KEEP1"), "a"),
            "Names {{d-DR0P1|Hidden label}} and {{d-M1SS1}}.",
        ),
        named(blk("d-0NE01", "doc", None, "c"), "one"),
        with_body(
            blk("p-0NE01", "prose", Some("d-0NE01"), "a"),
            "Names {{d-M1SS1}}.",
        ),
        with_field(
            with_field(
                named(blk("d-DR0P1", "doc", None, "d"), "hidden-name"),
                "title",
                s("Hidden title"),
            ),
            "key",
            s("hidden-key"),
        ),
        with_body(
            blk("p-DR0P1", "prose", Some("d-DR0P1"), "a"),
            "Hidden body.",
        ),
    ]);
    let public = publication::resolve_in_for_tests(&st, &m, "pubsite").unwrap();
    assert_eq!(public.references.status, CheckStatus::Blocked);
    assert_eq!(
        (
            public.references.excluded_count,
            public.references.missing_count,
            public.references.blocked_count
        ),
        (1, 1, 2)
    );
    let e = public.require_output().unwrap_err();
    assert_err(
        &e,
        E_PUBLICATION_REFERENCE,
        "Public profile has 2 references outside its selection.",
        "Include the target docs or remove these references. Run the publication check again.",
    );
    assert!(public.references.warning().is_none());
    // one missing target still blocks, and the noun stays plural
    let one = publication::resolve_in_for_tests(&st, &m, "onesite").unwrap();
    assert_eq!(one.references.status, CheckStatus::Blocked);
    assert_eq!(
        one.require_output().unwrap_err().message,
        "Public profile has 1 references outside its selection."
    );
    let org = publication::resolve_in_for_tests(&st, &m, "orgsite").unwrap();
    assert_eq!(org.references.status, CheckStatus::Warning);
    assert!(org.require_output().is_ok());
    let w = org.references.warning().unwrap();
    assert_eq!(
        (w.code, w.message.as_str(), w.fix.as_str()),
        (
            W_PUBLICATION_REFERENCE,
            "Org profile has 2 references outside its selection.",
            "Review these references before you share the output."
        )
    );
    assert_eq!(org.references.rows, public.references.rows);
    for key in ["clearsite", "clearorg"] {
        let clear = publication::resolve_in_for_tests(&st, &m, key).unwrap();
        assert_eq!(clear.references.status, CheckStatus::Clear, "{key}");
        assert!(clear.require_output().is_ok());
        assert!(clear.references.warning().is_none());
    }
    // no target text in a row, the report, the error or the warning
    let all = format!("{:?} {:?} {e:?} {w:?}", public.references, org.references);
    for leak in [
        "Hidden title",
        "hidden-key",
        "hidden-name",
        "Hidden label",
        "Hidden body",
    ] {
        assert!(!all.contains(leak), "{leak} in {all}");
    }
    assert!(!e.message.contains("d-DR0P1") && !w.message.contains("d-DR0P1"));
}

#[test]
fn pub4_selection_hash_tracks_children_assets_paths_and_redaction() {
    let (dir, mut r) = project();
    let kept_bytes = b"kept figure bytes".to_vec();
    let kept_blob = r.store_mut().put_blob(&kept_bytes).unwrap();
    let other_blob = r.store_mut().put_blob(b"another figure").unwrap();
    // a blob that shares its only full chunk with a blob a redaction removes
    let removed_bytes = vec![7u8; 1 << 20];
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let removed_blob = r.store_mut().put_blob(&removed_bytes).unwrap();
    let shared_blob = r.store_mut().put_blob(&shared_bytes).unwrap();
    let absent_blob = Hash::of(b"a blob this store never held");
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$top", "fields": {"name": "top"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$top", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "CANARY title"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$s", "body": "CANARY body."},
            {"op": "create", "type": "prose", "id": "$p2", "parent": "$s", "body": "Readable sibling."},
            {"op": "create", "type": "figure", "id": "$f1", "parent": "$d", "fields": {"blob": kept_blob.to_string(), "media_type": "application/octet-stream", "alt": "kept"}},
            {"op": "create", "type": "figure", "id": "$f3", "parent": "$d", "fields": {"blob": shared_blob.to_string(), "media_type": "application/octet-stream", "alt": "shared"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let c = |n: &str| created(&w, n);
    let (top, d, sec, p1, p2) = (c("$top"), c("$d"), c("$s"), c("$p1"), c("$p2"));
    let (f1, f3) = (c("$f1"), c("$f3"));
    let p = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$pf", "parent": c("$admin").to_string(),
            "fields": {"key": "pubsite", "title": "Site", "audience": "public", "select": [d.to_string()]}}]),
    );
    let pf = created(&p, "$pf");
    land(&mut r, "work");

    // the digest vectors, built here from the source's ids and versions
    let digests = |st: &ReadState, path: &str, f3_available: bool| -> (Hash, Hash) {
        let src = st.materialized(&Scope::Canon).unwrap();
        let ver = |id: &BlockId| src.version_of(id).unwrap();
        let red = |id: &BlockId| src.get(id).unwrap().redaction();
        let blob_of = |id: &BlockId| -> Hash {
            src.get(id)
                .unwrap()
                .field_str("blob")
                .unwrap()
                .parse()
                .unwrap()
        };
        let mut atts = vec![
            (f1.clone(), blob_of(&f1), true),
            (f3.clone(), blob_of(&f3), f3_available),
        ];
        atts.sort();
        let mut sel = b"rdy-publication-selection-1\0".to_vec();
        sel.extend_from_slice(format!("{pf}\0{}\0", ver(&pf)).as_bytes());
        let mut doc = b"rdy-publication-doc-1\0".to_vec();
        doc.extend_from_slice(format!("{path}\0").as_bytes());
        let mut all: Vec<&BlockId> = vec![&top, &d, &sec, &p1, &p2, &f1, &f3];
        all.sort();
        for id in all {
            entry_row(&mut sel, id, &ver(id), red(id));
            if *id != top {
                entry_row(&mut doc, id, &ver(id), red(id));
            }
        }
        for (b, h, a) in &atts {
            attachment_row(&mut sel, b, h, *a);
            attachment_row(&mut doc, b, h, *a);
        }
        (Hash::of(&sel), Hash::of(&doc))
    };

    let st = r.read();
    let base = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
    let (sel0, doc0) = digests(&st, "top/plan", true);
    assert_eq!(base.selection_hash, sel0);
    assert_eq!(base.docs.len(), 1);
    assert_eq!(
        (base.docs[0].id.clone(), base.docs[0].path.as_str()),
        (d.clone(), "top/plan")
    );
    assert_eq!(base.docs[0].digest, doc0);
    // attachments, sorted by (home doc, block, blob)
    let mut atts = [
        (f1.clone(), kept_blob, true),
        (f3.clone(), shared_blob, true),
    ];
    atts.sort();
    assert_eq!(
        base.attachments
            .iter()
            .map(|a| (a.home_doc.clone(), a.block.clone(), a.blob, a.available))
            .collect::<Vec<_>>(),
        atts.iter()
            .map(|(b, h, a)| (d.clone(), b.clone(), *h, *a))
            .collect::<Vec<_>>()
    );
    // a figure whose blob the store lacks is an unavailable attachment, not an error (O13)
    let mut absent = blk("fg-ABSNT", "figure", Some(d.as_str()), "z");
    absent
        .fields
        .insert("blob".into(), s(&absent_blob.to_string()));
    let m = st
        .materialized(&Scope::Canon)
        .unwrap()
        .with(&[Op::Create { block: absent }]);
    let with_absent = publication::resolve_in_for_tests(&st, &m, "pubsite").unwrap();
    let row = with_absent
        .attachments
        .iter()
        .find(|a| a.block == bid("fg-ABSNT"))
        .unwrap();
    assert_eq!(
        (row.home_doc.clone(), row.blob, row.available),
        (d.clone(), absent_blob, false)
    );
    assert_eq!(with_absent.attachments.len(), 3);
    drop(st);

    // each edit below changes what the digests cover, never the doc block itself
    let doc_version = |r: &Rdy| r.read().materialized(&Scope::Canon).unwrap().version_of(&d);
    let v0 = doc_version(&r);
    let mut last = (base.selection_hash, base.docs[0].digest);
    let mut step = |r: &mut Rdy, ops: Value, path: &str, what: &str| {
        write(r, "work", ops);
        land(r, "work");
        let st = r.read();
        let res = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
        let (sel, doc) = digests(&st, path, true);
        assert_eq!(
            (res.selection_hash, res.docs[0].digest),
            (sel, doc),
            "{what}"
        );
        assert_ne!(res.selection_hash, last.0, "{what}: selection");
        assert_ne!(res.docs[0].digest, last.1, "{what}: doc");
        assert_eq!(res.docs[0].path, path, "{what}");
        last = (sel, doc);
    };
    step(
        &mut r,
        json!([{"op": "update", "id": p2.to_string(), "body": "Edited sibling."}]),
        "top/plan",
        "a child edit",
    );
    step(
        &mut r,
        json!([{"op": "update", "id": f1.to_string(), "fields": {"blob": other_blob.to_string()}}]),
        "top/plan",
        "an asset change",
    );
    step(
        &mut r,
        json!([{"op": "update", "id": top.to_string(), "fields": {"name": "renamed"}}]),
        "renamed/plan",
        "an ancestor rename",
    );
    assert_eq!(doc_version(&r), v0, "the doc block never changed");
    let before = publication::resolve(&r.read(), &Scope::Canon, "pubsite").unwrap();

    // a redaction keeps every version and changes the digests through the request
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![
                RedactTarget::AllVersions(sec.clone()),
                RedactTarget::AllVersions(p1.clone()),
                RedactTarget::Blob(removed_blob),
            ],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let request = fixture.requests[0].request;
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let after = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
    assert_eq!(entry_ids(&after), entry_ids(&before));
    for (a, b) in after.entries.iter().zip(&before.entries) {
        assert_eq!(a.version, b.version, "{}: the version stays", a.id);
        let removed = a.id == sec || a.id == p1;
        assert_eq!(a.redaction, removed.then_some(request), "{}", a.id);
        assert_eq!(b.redaction, None);
    }
    for id in [&sec, &p1] {
        let stub = after.m.get(id).unwrap();
        assert!(stub.is_redacted());
        assert_eq!(stub.body, CONTENT_REMOVED);
        assert_eq!(
            stub.read_identity,
            st.block(&Scope::Canon, id).unwrap().read_identity
        );
    }
    // the removed section still selects its readable child through the stub's parent link
    assert_eq!(after.m.get(&p2).unwrap().parent, Some(sec.clone()));
    // the figure whose blob lost its chunk is unavailable; the other two are as before
    let avail: BTreeMap<BlockId, bool> = after
        .attachments
        .iter()
        .map(|a| (a.block.clone(), a.available))
        .collect();
    assert_eq!(
        avail,
        BTreeMap::from([(f1.clone(), true), (f3.clone(), false)])
    );
    let (sel, doc) = digests(&st, "renamed/plan", false);
    assert_eq!((after.selection_hash, after.docs[0].digest), (sel, doc));
    assert_ne!(after.selection_hash, before.selection_hash);
    assert_ne!(after.docs[0].digest, before.docs[0].digest);
    // the removed text is in no part of the result
    let shown = format!(
        "{:?} {:?} {:?}",
        after.entries, after.docs, after.references
    );
    assert!(!shown.contains("CANARY"));
    assert!(after.m.iter().all(|b| !format!("{b:?}").contains("CANARY")));
}

#[test]
fn pub4_hostile_selection_is_bounded() {
    // the production bounds, exactly
    assert_eq!(
        publication::limits(),
        Limits {
            profile_name: 128,
            selection: 100_000,
            graph: 1_000_000,
            text_value: 1_048_576,
            text: 67_108_864,
            references: 1_000_000,
            attachments: 100_000,
            field_depth: 64,
            location: 1024,
            token: 128,
            path: 4096,
            blob: 67_108_864,
            assets: 268_435_456,
        }
    );
    assert_eq!(publication::limits(), Limits::default());
    let (_dir, r) = project();
    let st = r.read();
    let mut table = blk("tb-TAB01", "table", Some("d-H0ST1"), "c");
    table.fields.insert(
        "rows".into(),
        FieldValue::List(vec![FieldValue::Map(BTreeMap::from([(
            "deep~key".to_string(),
            s("{{d-DR0P1}} {{d-KEEP1}}"),
        )]))]),
    );
    let blocks = vec![
        named(blk("d-ADM1N", "doc", None, "a"), "admin"),
        profile_block("pf-H0ST1", "d-ADM1N", "hostkey1", "org", &["fd-AREA1"]),
        named(blk("fd-T0P01", "folder", None, "b"), "top"),
        named(blk("fd-AREA1", "folder", Some("fd-T0P01"), "a"), "area"),
        named(
            blk("d-H0ST1", "doc", Some("fd-AREA1"), "a"),
            "a-rather-long-doc-name",
        ),
        named(blk("d-KEEP1", "doc", Some("fd-AREA1"), "b"), "keep"),
        with_body(
            blk("p-H0ST1", "prose", Some("d-H0ST1"), "a"),
            "One {{d-DR0P1}}, two {{d-KEEP1}}, three {{missing-key-token}}.",
        ),
        with_body(
            blk("p-H0ST2", "prose", Some("d-H0ST1"), "b"),
            "Plain words only.",
        ),
        table,
        link(
            with_field(
                with_field(
                    with_field(
                        blk("fg-F1G01", "figure", Some("d-H0ST1"), "d"),
                        "blob",
                        s(&Hash::of(b"one").to_string()),
                    ),
                    "media_type",
                    s("image/png"),
                ),
                "alt",
                s("first"),
            ),
            "see_also",
            "d-KEEP1",
        ),
        with_field(
            blk("fg-F1G02", "figure", Some("d-KEEP1"), "a"),
            "blob",
            s(&Hash::of(b"two").to_string()),
        ),
        named(blk("d-DR0P1", "doc", None, "c"), "drop"),
    ];
    let m = synth(blocks.clone());
    let res = publication::resolve_in_for_tests(&st, &m, "hostkey1").unwrap();

    // what the fixture holds, counted independently
    let retained = res.entries.len() as u64;
    assert_eq!(
        retained, 9,
        "admin, the profile and the dropped doc are out"
    );
    let edges: u64 = res
        .m
        .iter()
        .map(|b| rdy_graph::edges_of(b).len() as u64 + u64::from(b.parent.is_some()))
        .sum();
    let mut texts: Vec<u64> = vec![];
    for b in res.m.iter() {
        texts.push(b.body.len() as u64);
        fn walk(v: &FieldValue, out: &mut Vec<u64>) {
            match v {
                FieldValue::Str(x) => out.push(x.len() as u64),
                FieldValue::List(xs) => xs.iter().for_each(|x| walk(x, out)),
                FieldValue::Map(xs) => xs.values().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        b.fields.values().for_each(|v| walk(v, &mut texts));
    }
    let longest_text = *texts.iter().max().unwrap();
    let total_text: u64 = texts.iter().sum();
    // spans: p-H0ST1 3, the table 2; links: the figure 1
    let occurrences = 6u64;
    let attachments = res.attachments.len() as u64;
    assert_eq!(attachments, 2);
    assert!(
        res.attachments.iter().all(|a| !a.available),
        "no blob is in the store"
    );
    let longest_location = res
        .references
        .rows
        .iter()
        .map(|r| r.location.len())
        .max()
        .unwrap() as u64;
    assert_eq!(longest_location, "/fields/rows/0/deep~0key".len() as u64);
    let longest_token = res
        .references
        .rows
        .iter()
        .map(|r| r.target.len())
        .max()
        .unwrap() as u64;
    assert_eq!(longest_token, "missing-key-token".len() as u64);
    let longest_path = "top/area/a-rather-long-doc-name".len() as u64;
    assert_eq!(
        res.docs.iter().map(|d| d.path.len() as u64).max(),
        Some(longest_path)
    );

    // each bound: N passes, N - 1 refuses with its exact text
    type Set = fn(&mut Limits, u64);
    let cases: Vec<(&str, &str, u64, Set)> = vec![
        ("profile name", "bytes", "hostkey1".len() as u64, |l, n| {
            l.profile_name = n as usize
        }),
        ("selection", "blocks", retained, |l, n| {
            l.selection = n as usize
        }),
        ("graph", "edges", edges, |l, n| l.graph = n as usize),
        ("text value", "bytes", longest_text, |l, n| {
            l.text_value = n as usize
        }),
        ("text", "bytes", total_text, |l, n| l.text = n as usize),
        ("references", "occurrences", occurrences, |l, n| {
            l.references = n as usize
        }),
        ("attachments", "rows", attachments, |l, n| {
            l.attachments = n as usize
        }),
        ("reference location", "bytes", longest_location, |l, n| {
            l.location = n as usize
        }),
        ("reference token", "bytes", longest_token, |l, n| {
            l.token = n as usize
        }),
        ("selected path", "bytes", longest_path, |l, n| {
            l.path = n as usize
        }),
        ("field depth", "levels", 3, |l, n| {
            l.field_depth = n as usize
        }),
    ];
    for (part, unit, n, set) in cases {
        let mut l = Limits::default();
        set(&mut l, n);
        {
            let _g = publication::override_limits_for_tests(l);
            let ok = publication::resolve_in_for_tests(&st, &m, "hostkey1")
                .unwrap_or_else(|e| panic!("{part} at {n}: {e}"));
            assert_same(&ok, &res);
        }
        set(&mut l, n - 1);
        let _g = publication::override_limits_for_tests(l);
        let e = publication::resolve_in_for_tests(&st, &m, "hostkey1").unwrap_err();
        assert_limit(&e, part, n - 1, unit);
    }
    // the name bound holds for a name that resolves to nothing too, before any lookup
    let long = "x".repeat(129);
    let e = publication::resolve_in_for_tests(&st, &m, &long).unwrap_err();
    assert_limit(&e, "profile name", 128, "bytes");
    let e = publication::resolve(&st, &Scope::Canon, &long).unwrap_err();
    assert_limit(&e, "profile name", 128, "bytes");
    // the first failing phase wins: the selection bound comes before the graph and text bounds
    {
        let l = Limits {
            selection: 1,
            graph: 1,
            text: 1,
            ..Limits::default()
        };
        let _g = publication::override_limits_for_tests(l);
        let e = publication::resolve_in_for_tests(&st, &m, "hostkey1").unwrap_err();
        assert_limit(&e, "selection", 1, "blocks");
    }

    // the real field depth: 64 levels pass, 65 refuse
    for (depth, ok) in [(64usize, true), (65, false)] {
        let mut deep = blocks.clone();
        deep.push(with_field(
            blk("tb-DEEP1", "table", Some("d-KEEP1"), "z"),
            "rows",
            nest(depth),
        ));
        let got = publication::resolve_in_for_tests(&st, &synth(deep), "hostkey1");
        match (got, ok) {
            (Ok(res), true) => assert!(
                res.references
                    .rows
                    .iter()
                    .any(|r| r.source == bid("tb-DEEP1"))
            ),
            (Err(e), false) => assert_limit(&e, "field depth", 64, "levels"),
            (got, _) => panic!("depth {depth}: {:?}", got.map(|r| r.selection_hash)),
        }
    }

    // invalid trees: a parent loop above a selected doc, a missing parent, a doc under a doc
    let invalid = |extra: Vec<Block>, select: &[&str]| {
        let mut bs = vec![named(blk("d-ADM1N", "doc", None, "a"), "admin")];
        bs.push(profile_block(
            "pf-BAD01", "d-ADM1N", "badtree", "org", select,
        ));
        bs.extend(extra);
        let e = publication::resolve_in_for_tests(&st, &synth(bs), "badtree").unwrap_err();
        assert_err(
            &e,
            E_BAD_INPUT,
            "Publication selection has an invalid document tree.",
            "Run rdy fsck and fix the document tree.",
        );
    };
    invalid(
        vec![
            named(blk("fd-R00P1", "folder", Some("fd-R00P2"), "a"), "loop1"),
            named(blk("fd-R00P2", "folder", Some("fd-R00P1"), "a"), "loop2"),
            named(blk("d-R00P3", "doc", Some("fd-R00P1"), "a"), "inloop"),
        ],
        &["d-R00P3"],
    );
    invalid(
        vec![
            named(blk("fd-R00P1", "folder", Some("fd-R00P2"), "a"), "loop1"),
            named(blk("fd-R00P2", "folder", Some("fd-R00P1"), "a"), "loop2"),
        ],
        &["fd-R00P1"],
    );
    invalid(
        vec![named(
            blk("d-0RPH1", "doc", Some("fd-G0NE1"), "a"),
            "orphan",
        )],
        &["d-0RPH1"],
    );
    invalid(
        vec![
            named(blk("d-0VTER", "doc", None, "a"), "outer"),
            named(blk("d-NEST1", "doc", Some("d-0VTER"), "a"), "nested"),
        ],
        &["d-NEST1"],
    );
}

#[test]
fn pub4_asset_limits_precede_byte_reads() {
    let (dir, mut r) = project();
    let small = r.store_mut().put_blob(b"four").unwrap();
    let large = r.store_mut().put_blob(b"ten bytes!").unwrap();
    let removed_bytes = vec![9u8; 1 << 20];
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let removed_blob = r.store_mut().put_blob(&removed_bytes).unwrap();
    let shared_blob = r.store_mut().put_blob(&shared_bytes).unwrap();
    let fig = |ph: &str, blob: &Hash| {
        json!({"op": "create", "type": "figure", "id": ph, "parent": "$d",
            "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": ph}})
    };
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "assets", "title": "Assets", "doc_type": "page"}},
            fig("$small", &small),
            fig("$again", &small),
            fig("$large", &large),
            fig("$shared", &shared_blob),
            fig("$removed", &removed_blob),
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let d = created(&w, "$d");
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": created(&w, "$admin").to_string(),
            "fields": {"key": "pubsite", "title": "Site", "audience": "org", "select": [d.to_string()]}}]),
    );
    land(&mut r, "work");
    r.close().unwrap();
    authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::Blob(removed_blob)],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let mut r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let res = publication::resolve(&st, &Scope::Canon, "pubsite").unwrap();
    let avail: BTreeMap<BlockId, (Hash, bool)> = res
        .attachments
        .iter()
        .map(|a| (a.block.clone(), (a.blob, a.available)))
        .collect();
    let c = |n: &str| created(&w, n);
    assert_eq!(
        avail,
        BTreeMap::from([
            (c("$small"), (small, true)),
            (c("$again"), (small, true)),
            (c("$large"), (large, true)),
            (c("$shared"), (shared_blob, false)),
            (c("$removed"), (removed_blob, false)),
        ]),
        "a removed manifest or chunk is unavailable, without an error"
    );

    // distinct available blobs, each read once, in hash order; removed ones are skipped
    let reads = publication::blob_reads_for_tests();
    let assets = publication::load_assets(&st, &res).unwrap();
    assert_eq!(publication::blob_reads_for_tests() - reads, 2);
    assert_eq!(
        assets,
        BTreeMap::from([(small, b"four".to_vec()), (large, b"ten bytes!".to_vec())])
    );
    // each bound is checked on the manifests before any byte is read
    for (blob, total, err) in [
        (10u64, 14u64, None),
        (9, 14, Some(("blob", 9u64))),
        (10, 13, Some(("assets", 13))),
    ] {
        let l = Limits {
            blob,
            assets: total,
            ..Limits::default()
        };
        let _g = publication::override_limits_for_tests(l);
        let reads = publication::blob_reads_for_tests();
        let got = publication::load_assets(&st, &res);
        match err {
            None => {
                assert_eq!(got.unwrap(), assets);
                assert_eq!(publication::blob_reads_for_tests() - reads, 2);
            }
            Some((part, n)) => {
                assert_limit(&got.unwrap_err(), part, n, "bytes");
                assert_eq!(
                    publication::blob_reads_for_tests(),
                    reads,
                    "{part}: no byte read"
                );
            }
        }
    }
    drop(st);

    // a manifest that is not its own canonical encoding: the selection reads it, and the
    // existing byte reader refuses it
    let manifest: BlobManifest = r
        .read()
        .reader()
        .get_typed(kinds::BLOB_MANIFEST, &small)
        .unwrap();
    let mut bytes = redaction_fixture::enc(&manifest);
    bytes.push(0);
    let loose = r
        .store_mut()
        .write(vec![(kinds::BLOB_MANIFEST, bytes)])
        .unwrap()[0];
    let damaged = r
        .store_mut()
        .write(vec![(kinds::BLOB_MANIFEST, vec![0xff, 0xff, 0xff])])
        .unwrap()[0];
    let fig_in = |view: &str, blob: &Hash, r: &mut Rdy| {
        write(
            r,
            view,
            json!([{"op": "create", "type": "figure", "parent": d.to_string(),
                "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "odd"}}]),
        );
    };
    fig_in("loose", &loose, &mut r);
    fig_in("damaged", &damaged, &mut r);
    let st = r.read();
    let m = st.materialized(&Scope::View("loose".into())).unwrap();
    let res = publication::resolve_in_for_tests(&st, &m, "pubsite").unwrap();
    assert!(
        res.attachments
            .iter()
            .any(|a| a.blob == loose && a.available)
    );
    let reads = publication::blob_reads_for_tests();
    let e = publication::load_assets(&st, &res).unwrap_err();
    assert_eq!(e.code, E_IO);
    assert!(
        e.message.contains("is not a canonical blob manifest"),
        "{e}"
    );
    assert!(publication::blob_reads_for_tests() > reads);
    // a manifest that does not decode is the store's own error, from the selection itself
    let m = st.materialized(&Scope::View("damaged".into())).unwrap();
    let e = publication::resolve_in_for_tests(&st, &m, "pubsite").unwrap_err();
    assert_eq!(e.code, E_IO, "{e}");
}
