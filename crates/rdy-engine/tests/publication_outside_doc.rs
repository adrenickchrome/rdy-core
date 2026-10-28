//! ndh4 (D-P3YD5, D-0VCQ5, D-KDFZ3): a profile publishes a decision with no doc when it selects a
//! block that the decision governs, by any of the four governing routes, with the decision's
//! whole subtree and without its folder. A decision in a selected folder is published with it.
//! The disclosure comparison lists every retained block outside a doc whose version differs.
mod common;

use rdy_engine::publication::{self, Classification, IMPLICIT_ORG, ReferenceRow, Resolved};
use rdy_engine::*;
use rdy_model::redaction::RedactedBlockIdentity;
use rdy_model::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------------------------
// helpers (copied from publication.rs)

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

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

/// The stub of a removed version of `id`.
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

fn entry_ids(res: &Resolved) -> Vec<BlockId> {
    res.entries.iter().map(|e| e.id.clone()).collect()
}

fn version_in(res: &Resolved, id: &str) -> Hash {
    res.entries
        .iter()
        .find(|e| e.id == bid(id))
        .unwrap_or_else(|| panic!("{id} is not retained"))
        .version
}

fn decision(id: &str, parent: Option<&str>, statement: &str) -> Block {
    with_field(
        with_field(blk(id, "decision", parent, "m"), "statement", s(statement)),
        "status",
        s("accepted"),
    )
}

/// The ndh4 blocks: doc `admin` with profile `outside` (org) selecting `select`; doc `guide`
/// (a section and claim `c-C7A1M`, which `claim` builds); `D-R00T1` at the root; folder
/// `fd-AREA1` holding `D-F01D1`; `D-0THR1` at the root governing nothing.
fn blocks(select: &[&str], claim: impl Fn(Block) -> Block) -> Vec<Block> {
    vec![
        named(blk("d-ADM1N", "doc", None, "a"), "admin"),
        profile_block("pf-0VT01", "d-ADM1N", "outside", "org", select),
        named(blk("d-GV1DE", "doc", None, "b"), "guide"),
        with_field(
            blk("s-GV1DE", "section", Some("d-GV1DE"), "a"),
            "title",
            s("Guide section"),
        ),
        claim(with_body(
            blk("c-C7A1M", "claim", Some("s-GV1DE"), "a"),
            "One claim.",
        )),
        decision("D-R00T1", None, "A ruling at the root."),
        named(blk("fd-AREA1", "folder", None, "c"), "area"),
        decision("D-F01D1", Some("fd-AREA1"), "A ruling in a folder."),
        decision("D-0THR1", None, "A ruling that governs nothing."),
    ]
}

fn with_body(mut b: Block, body: &str) -> Block {
    b.body = body.into();
    b
}

fn governed(b: Block) -> Block {
    link(link(b, "decided_by", "D-R00T1"), "decided_by", "D-F01D1")
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn ndh4_a_governed_decision_with_no_doc_is_published_and_its_folder_is_not() {
    let (_dir, r) = project();
    let st = r.read();
    let m = synth(blocks(&["d-GV1DE"], governed));
    let res = publication::resolve_in_for_tests(&st, &m, "outside").unwrap();
    let ids = entry_ids(&res);
    for kept in ["D-R00T1", "D-F01D1"] {
        assert!(ids.contains(&bid(kept)), "{kept}: {ids:?}");
    }
    for left in ["D-0THR1", "fd-AREA1"] {
        assert!(!ids.contains(&bid(left)), "{left}: {ids:?}");
    }
    let docs: Vec<BlockId> = res.docs.iter().map(|d| d.id.clone()).collect();
    assert_eq!(docs, vec![bid("d-GV1DE")]);
    // the governing links now point into the selection
    assert!(res.references.rows.is_empty(), "{:?}", res.references.rows);
}

#[test]
fn ndh4_a_decision_in_a_selected_folder_is_published_with_it() {
    let (_dir, r) = project();
    let st = r.read();
    let m = synth(vec![
        named(blk("d-ADM1N", "doc", None, "a"), "admin"),
        profile_block("pf-0VT01", "d-ADM1N", "outside", "org", &["fd-AREA1"]),
        named(blk("fd-AREA1", "folder", None, "b"), "area"),
        decision("D-F01D2", Some("fd-AREA1"), "No doc holds this."),
    ]);
    let res = publication::resolve_in_for_tests(&st, &m, "outside").unwrap();
    assert_eq!(entry_ids(&res), vec![bid("D-F01D2"), bid("fd-AREA1")]);
    assert!(res.docs.is_empty());
}

#[test]
fn ndh4_a_governed_decision_keeps_its_subtree_and_an_affects_route_counts() {
    let (_dir, r) = project();
    let st = r.read();
    let mut bs = blocks(&["d-GV1DE"], |c| link(c, "describes", "t-TERM1"));
    bs.extend([
        with_body(
            named(blk("t-TERM1", "term", Some("s-GV1DE"), "b"), "ledger"),
            "A ledger.",
        ),
        named(blk("d-0THER", "doc", None, "d"), "other"),
        with_body(
            named(blk("t-TERM2", "term", Some("d-0THER"), "a"), "wallet"),
            "A wallet.",
        ),
        link(
            link(
                decision("D-R00T2", None, "Ledgers are atomic."),
                "affects",
                "t-TERM1",
            ),
            "affects",
            "t-TERM2",
        ),
        with_body(
            blk("ra-RAT10", "rationale", Some("D-R00T2"), "a"),
            "Because ledgers settle.",
        ),
    ]);
    let res = publication::resolve_in_for_tests(&st, &synth(bs), "outside").unwrap();
    let ids = entry_ids(&res);
    for kept in ["D-R00T2", "ra-RAT10"] {
        assert!(ids.contains(&bid(kept)), "{kept}: {ids:?}");
    }
    for left in ["t-TERM2", "d-0THER", "D-R00T1", "D-F01D1"] {
        assert!(!ids.contains(&bid(left)), "{left}: {ids:?}");
    }
    assert!(
        res.references.rows.contains(&ReferenceRow {
            source: bid("D-R00T2"),
            location: "/links/affects/1".into(),
            target: "t-TERM2".into(),
            classification: Classification::Excluded,
            occurrences: 1,
        }),
        "{:?}",
        res.references.rows
    );
}

#[test]
fn ndh4_the_disclosure_diff_lists_blocks_outside_docs() {
    let (_dir, r) = project();
    let st = r.read();
    let resolve = |bs: Vec<Block>, name: &str| {
        publication::resolve_in_for_tests(&st, &synth(bs), name).unwrap()
    };
    let one = |c: Block| link(c, "decided_by", "D-R00T1");

    // before, the claim has no decided_by; after, it links D-R00T1
    let before = resolve(blocks(&["d-GV1DE"], |c| c), "outside");
    let after = resolve(blocks(&["d-GV1DE"], one), "outside");
    let d = publication::disclosure_diff(&before, &after).unwrap();
    assert_eq!(
        d.changed_outside_docs,
        vec![publication::ChangedOutsideDoc {
            id: bid("D-R00T1"),
            before_version: None,
            after_version: Some(version_in(&after, "D-R00T1")),
            redacted: false,
        }]
    );

    // a new version of D-R00T1 on both sides gives both versions
    let renewed = |mut bs: Vec<Block>| {
        for b in &mut bs {
            if b.id == bid("D-R00T1") {
                b.fields
                    .insert("statement".into(), s("A new ruling at the root."));
            }
        }
        bs
    };
    let again = resolve(renewed(blocks(&["d-GV1DE"], one)), "outside");
    let d = publication::disclosure_diff(&after, &again).unwrap();
    let (v1, v2) = (version_in(&after, "D-R00T1"), version_in(&again, "D-R00T1"));
    assert_ne!(v1, v2);
    assert_eq!(
        d.changed_outside_docs,
        vec![publication::ChangedOutsideDoc {
            id: bid("D-R00T1"),
            before_version: Some(v1),
            after_version: Some(v2),
            redacted: false,
        }]
    );

    // a removed version of D-R00T1 on the after side: a stub governs nothing, so both sides
    // retain it through the implicit profile
    let removed = |mut bs: Vec<Block>| {
        for b in &mut bs {
            if b.id == bid("D-R00T1") {
                *b = stub("D-R00T1", "decision", None, Hash::of(b"a request"));
            }
        }
        bs
    };
    let full = resolve(blocks(&["d-GV1DE"], one), IMPLICIT_ORG);
    let gone = resolve(removed(blocks(&["d-GV1DE"], one)), IMPLICIT_ORG);
    let d = publication::disclosure_diff(&full, &gone).unwrap();
    assert_eq!(
        d.changed_outside_docs,
        vec![publication::ChangedOutsideDoc {
            id: bid("D-R00T1"),
            before_version: Some(version_in(&full, "D-R00T1")),
            after_version: Some(version_in(&gone, "D-R00T1")),
            redacted: true,
        }]
    );

    // the report JSON holds the row, with its omitted count
    report_lists_a_block_outside_docs();
}

// ---------------------------------------------------------------------------------------------
// the report JSON, through a real project

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

/// Through `publication::check` on a real project: a claim that comes to link a decision at the
/// root shows the decision as a row of `changed_outside_docs`.
fn report_lists_a_block_outside_docs() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "S"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One claim."},
            {"op": "create", "type": "decision", "id": "$dec",
             "fields": {"statement": "A ruling at the root.", "status": "proposed"}}
        ]),
    );
    let (c, dec) = (w.created["$c"].clone(), w.created["$dec"].clone());
    let (admin, guide) = (w.created["$admin"].clone(), w.created["$guide"].clone());
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [guide]}}]),
    );
    land(&mut r, "work");
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "links": {"decided_by": [dec]}}]),
    );
    land(&mut r, "work");
    let v = publication::check(&r.read(), "site", None, Some("tag:v1.0.0")).unwrap();
    let version = v["selection"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == json!(dec))
        .expect("the decision is retained")["version"]
        .clone();
    let d = &v["disclosure"];
    assert_eq!(
        d["changed_outside_docs"],
        json!([{"id": dec, "before_version": null, "after_version": version, "redacted": false}]),
        "{d}"
    );
    assert_eq!(d["omitted"]["changed_outside_docs"], json!(0), "{d}");
}
