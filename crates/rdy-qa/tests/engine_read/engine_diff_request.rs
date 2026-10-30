//! dq2: the diff request (D-HZYK0): the one-argument defaults, the scope labels, the root, compact
//! and detail, the reply's frozen keys and the landing read.
use std::collections::BTreeSet;

use rdy_engine::diff::{DiffChange, DiffCounts};
use rdy_engine::diff_request::LabelKind;
use rdy_engine::{
    DiffReply, DiffRequest, E_BAD_INPUT, E_NOT_FOUND, E_UNKNOWN_BLOCK, Rdy, RdyError, ScopeLabel,
};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

fn one(from: &str) -> DiffRequest {
    DiffRequest {
        from: from.into(),
        ..Default::default()
    }
}

fn two(from: &str, to: &str) -> DiffRequest {
    DiffRequest {
        from: from.into(),
        to: Some(to.into()),
        ..Default::default()
    }
}

fn under(from: &str, doc: &str) -> DiffRequest {
    DiffRequest {
        from: from.into(),
        doc: Some(doc.into()),
        ..Default::default()
    }
}

fn label(kind: LabelKind) -> ScopeLabel {
    ScopeLabel {
        kind,
        name: None,
        landing: None,
        version: None,
    }
}

fn landing(id: &str) -> ScopeLabel {
    ScopeLabel {
        landing: Some(id.into()),
        ..label(LabelKind::Landing)
    }
}

fn canon(head: &str) -> ScopeLabel {
    ScopeLabel {
        landing: Some(head.into()),
        ..label(LabelKind::Canon)
    }
}

fn view_label(name: &str) -> ScopeLabel {
    ScopeLabel {
        name: Some(name.into()),
        ..label(LabelKind::View)
    }
}

fn ids(reply: &DiffReply) -> Vec<String> {
    reply.entries.iter().map(|e| e.id.to_string()).collect()
}

fn claim(parent: &str, body: &str) -> Value {
    json!([{"op": "create", "type": "claim", "id": "$c", "parent": parent, "body": body}])
}

/// Writes one claim into `view` and returns its id.
fn add_claim(r: &mut Rdy, view: &str, parent: &str, body: &str) -> String {
    write(r, view, claim(parent, body)).unwrap().created["$c"].clone()
}

fn reply(r: &Rdy, req: &DiffRequest) -> DiffReply {
    r.read().diff_reply(req, None).unwrap()
}

fn refused(r: &Rdy, req: &DiffRequest) -> RdyError {
    r.read().diff_reply(req, None).unwrap_err()
}

/// A landed seed doc: returns the project, the section's id and the landing.
fn seeded() -> (tempfile::TempDir, Rdy, String, String) {
    let (d, mut r) = project();
    let (_, s, _) = seed_doc(&mut r, "seed");
    let l = land(&mut r, "seed").unwrap();
    (d, r, s, l)
}

#[test]
fn dq2_a_view_diffs_from_its_base_landing() {
    let (_d, mut r, s, l1) = seeded();
    // pinned, so that canon's next landing does not rebase it
    r.view_create(&writer(), "v", Some(&format!("landing:{l1}")), true)
        .unwrap();
    let own = add_claim(&mut r, "v", &s, "the view's own work");
    add_claim(&mut r, "other", &s, "canon moves on");
    let l2 = land(&mut r, "other").unwrap();
    assert_ne!(l1, l2);

    let got = reply(&r, &one("v"));
    assert_eq!(got.from, landing(&l1));
    assert_eq!(got.to, view_label("v"));
    assert_eq!(ids(&got), vec![own]);
    assert_eq!(
        got.counts,
        DiffCounts {
            added: 1,
            ..Default::default()
        }
    );
    // `view:` spells the same view
    assert_eq!(reply(&r, &one("view:v")), got);
}

#[test]
fn dq2_a_rebased_view_diffs_from_the_base_it_moved_to() {
    let (_d, mut r, s, _l1) = seeded();
    let own = add_claim(&mut r, "v", &s, "the view's own work");
    add_claim(&mut r, "other", &s, "canon moves on");
    let l2 = land(&mut r, "other").unwrap();
    let got = reply(&r, &one("v"));
    assert_eq!(got.from, landing(&l2));
    assert_eq!(ids(&got), vec![own]);
}

#[test]
fn dq2_a_view_at_canons_head_reads_canon_and_labels_the_landing() {
    let (_d, mut r, s, l1) = seeded();
    let own = add_claim(&mut r, "v", &s, "the view's own work");
    let got = reply(&r, &one("v"));
    assert_eq!(got.from, landing(&l1));
    assert_eq!(ids(&got), vec![own]);
    // the label is the landing, never `canon`, and it reads the same as naming the landing
    assert_eq!(
        reply(&r, &two(&format!("landing:{l1}"), "v")).entries,
        got.entries
    );
}

#[test]
fn dq2_a_view_with_no_base_diffs_from_the_empty_state() {
    let (_d, mut r) = project();
    let (doc, s, c) = seed_doc(&mut r, "v");
    let got = reply(&r, &one("v"));
    assert_eq!(got.from, label(LabelKind::Empty));
    assert_eq!(got.to, view_label("v"));
    let all: BTreeSet<String> = ids(&got).into_iter().collect();
    assert_eq!(all, BTreeSet::from([doc, s, c]));
    assert!(got.entries.iter().all(|e| e.change == DiffChange::Added));
}

#[test]
fn dq2_canon_diffs_from_its_previous_landing() {
    let (_d, mut r, s, l1) = seeded();
    let c = add_claim(&mut r, "two", &s, "the second landing");
    let l2 = land(&mut r, "two").unwrap();
    let got = reply(&r, &one("canon"));
    assert_eq!(got.from, landing(&l1));
    assert_eq!(got.to, canon(&l2));
    assert_eq!(ids(&got), vec![c]);
}

#[test]
fn dq2_canon_with_one_landing_diffs_from_the_empty_state() {
    let (_d, r, _s, l1) = seeded();
    let got = reply(&r, &one("canon"));
    assert_eq!(got.from, label(LabelKind::Empty));
    assert_eq!(got.to, canon(&l1));
    let m = r.read().materialized(&rdy_engine::Scope::Canon).unwrap();
    assert_eq!(got.entries.len(), m.len());
    assert!(got.entries.iter().all(|e| e.change == DiffChange::Added));
    assert_eq!(got.counts.added, m.len());
}

#[test]
fn dq2_canon_with_no_landing_gives_an_empty_diff() {
    let (_d, r) = project();
    let got = reply(&r, &one("canon"));
    assert_eq!(got.from, label(LabelKind::Empty));
    assert_eq!(got.to, label(LabelKind::Canon));
    assert!(got.entries.is_empty());
    assert_eq!(got.counts, DiffCounts::default());
}

#[test]
fn dq2_a_tag_and_a_landing_diff_to_canon() {
    let (_d, mut r, s, l1) = seeded();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    let c = add_claim(&mut r, "two", &s, "after the tag");
    let l2 = land(&mut r, "two").unwrap();

    // the label holds the name as stored, however the request spells it
    for text in ["tag:v1.0.0", "tag:1.0.0"] {
        let got = reply(&r, &one(text));
        assert_eq!(
            got.from,
            ScopeLabel {
                name: Some("v1.0.0".into()),
                landing: Some(l1.clone()),
                ..label(LabelKind::Tag)
            },
            "{text}"
        );
        assert_eq!(got.to, canon(&l2));
        assert_eq!(ids(&got), vec![c.clone()]);
    }
    let got = reply(&r, &one(&format!("landing:{}", &l1[..8])));
    assert_eq!(got.from, landing(&l1));
    assert_eq!(got.to, canon(&l2));
    assert_eq!(ids(&got), vec![c]);
}

/// Proposes `view` and gives its version hash and its ref.
fn propose(r: &mut Rdy, view: &str) -> (String, String) {
    let summary = rdy_qa::intake::summary_for(r, view);
    let p = r.propose(&writer(), view, view, summary).unwrap();
    let pref = r.resolve_proposal(&p.id).unwrap();
    (pref.version.to_string(), pref.to_string())
}

#[test]
fn dq2_a_proposal_diffs_from_its_base() {
    let (_d, mut r, s, l1) = seeded();
    let c = add_claim(&mut r, "p", &s, "the proposal's work");
    let (version, pref) = propose(&mut r, "p");
    add_claim(&mut r, "other", &s, "canon moves on");
    land(&mut r, "other").unwrap();

    let got = reply(&r, &one(&format!("proposal:{pref}")));
    assert_eq!(got.from, landing(&l1));
    assert_eq!(
        got.to,
        ScopeLabel {
            name: Some(pref),
            version: Some(version),
            ..label(LabelKind::Proposal)
        }
    );
    assert_eq!(ids(&got), vec![c]);
}

#[test]
fn dq2_a_proposal_with_no_base_diffs_from_the_empty_state() {
    let (_d, mut r) = project();
    let (doc, s, c) = seed_doc(&mut r, "p");
    let (_, pref) = propose(&mut r, "p");
    seed_doc(&mut r, "first");
    land(&mut r, "first").unwrap();

    let got = reply(&r, &one(&format!("proposal:{pref}")));
    assert_eq!(got.from, label(LabelKind::Empty));
    let all: BTreeSet<String> = ids(&got).into_iter().collect();
    assert_eq!(all, BTreeSet::from([doc, s, c]));
    assert!(got.entries.iter().all(|e| e.change == DiffChange::Added));
}

#[test]
fn dq2_inflight_alone_is_refused() {
    let (_d, r) = project();
    let e = refused(&r, &one("inflight"));
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, "inflight needs both sides");
    assert_eq!(
        e.fix,
        "name both scopes: rdy_diff {from, to} / rdy diff <from> <to>"
    );
}

#[test]
fn dq2_two_scopes_diff_as_given() {
    let (_d, mut r, s, _l1) = seeded();
    let own = add_claim(&mut r, "a", &s, "in a");
    let theirs = add_claim(&mut r, "b", &s, "in b");
    let got = reply(&r, &two("a", "inflight"));
    assert_eq!(got.from, view_label("a"));
    assert_eq!(got.to, label(LabelKind::Inflight));
    assert_eq!(ids(&got), vec![theirs]);
    // the other way round the same block is removed: no default turns a side around
    let back = reply(&r, &two("inflight", "a"));
    assert_eq!(back.from, label(LabelKind::Inflight));
    assert_eq!(back.to, view_label("a"));
    assert_eq!(back.counts.removed, 1);
    assert!(!ids(&back).contains(&own));
}

#[test]
fn dq2_each_label_kind_serializes_as_o2() {
    let (_d, mut r, s, l1) = seeded();
    r.tag(&approver(), "v1.0.0", None, None).unwrap();
    add_claim(&mut r, "two", &s, "second");
    let l2 = land(&mut r, "two").unwrap();
    add_claim(&mut r, "p", &s, "proposed");
    let (version, pref) = propose(&mut r, "p");

    let json_of = |l: &ScopeLabel| serde_json::to_value(l).unwrap();
    let canon_side = reply(&r, &one("canon"));
    assert_eq!(
        json_of(&canon_side.to),
        json!({"kind": "canon", "landing": l2})
    );
    assert_eq!(
        json_of(&canon_side.from),
        json!({"kind": "landing", "landing": l1})
    );
    let view_side = reply(&r, &one("p"));
    assert_eq!(json_of(&view_side.to), json!({"kind": "view", "name": "p"}));
    let tag_side = reply(&r, &one("tag:v1.0.0"));
    assert_eq!(
        json_of(&tag_side.from),
        json!({"kind": "tag", "name": "v1.0.0", "landing": l1})
    );
    let proposal_side = reply(&r, &one(&format!("proposal:{pref}")));
    assert_eq!(
        json_of(&proposal_side.to),
        json!({"kind": "proposal", "name": pref, "version": version})
    );
    let inflight_side = reply(&r, &two("canon", "inflight"));
    assert_eq!(json_of(&inflight_side.to), json!({"kind": "inflight"}));

    let (_d2, fresh) = project();
    let empty_side = reply(&fresh, &one("canon"));
    assert_eq!(json_of(&empty_side.from), json!({"kind": "empty"}));
    assert_eq!(json_of(&empty_side.to), json!({"kind": "canon"}));
}

/// Folder `area` with doc `one` (claim `$c1`, key `DOC-ONE`), and doc `two` at the top level
/// (claim `$c2`), landed; returns the created ids.
fn two_docs() -> (
    tempfile::TempDir,
    Rdy,
    std::collections::BTreeMap<String, String>,
) {
    let (d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "area", "title": "Area"}},
        {"op": "create", "type": "doc", "id": "$one", "parent": "$f", "fields": {"name": "one", "title": "One", "doc_type": "overview", "key": "DOC-ONE"}},
        {"op": "create", "type": "section", "id": "$s1", "parent": "$one", "fields": {"title": "S1"}},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$s1", "body": "First claim."},
        {"op": "create", "type": "doc", "id": "$two", "fields": {"name": "two", "title": "Two", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s2", "parent": "$two", "fields": {"title": "S2"}},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s2", "body": "Second claim."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    (d, r, w.created)
}

#[test]
fn dq2_the_root_takes_an_id_a_key_and_a_path() {
    let (_d, mut r, c) = two_docs();
    write(
        &mut r,
        "edit",
        json!([
            {"op": "update", "id": c["$c1"], "body": "First, edited."},
            {"op": "update", "id": c["$c2"], "body": "Second, edited."}
        ]),
    )
    .unwrap();
    let one_id = BlockId::parse(&c["$one"]).unwrap();
    for doc in [c["$one"].as_str(), "DOC-ONE", "area/one"] {
        let got = reply(&r, &under("edit", doc));
        assert_eq!(got.root.as_ref(), Some(&one_id), "{doc}");
        assert_eq!(ids(&got), vec![c["$c1"].clone()], "{doc}");
        assert_eq!(got.counts.updated, 1, "{doc}");
    }
    // a folder is a root too
    let folder = reply(&r, &under("edit", "area"));
    assert_eq!(folder.root, Some(BlockId::parse(&c["$f"]).unwrap()));
    assert_eq!(ids(&folder), vec![c["$c1"].clone()]);
    // and with no root both changes show
    let whole = reply(&r, &one("edit"));
    assert_eq!(whole.root, None);
    assert_eq!(whole.entries.len(), 2);
}

#[test]
fn dq2_a_root_only_on_the_from_side_lists_its_blocks_as_removed() {
    let (_d, mut r, c) = two_docs();
    write(
        &mut r,
        "edit",
        json!([
            {"op": "delete", "id": c["$c2"]},
            {"op": "delete", "id": c["$s2"]},
            {"op": "delete", "id": c["$two"]}
        ]),
    )
    .unwrap();
    for doc in [c["$two"].as_str(), "two"] {
        let got = reply(&r, &under("edit", doc));
        assert_eq!(got.root, Some(BlockId::parse(&c["$two"]).unwrap()), "{doc}");
        let all: BTreeSet<String> = ids(&got).into_iter().collect();
        assert_eq!(
            all,
            BTreeSet::from([c["$two"].clone(), c["$s2"].clone(), c["$c2"].clone()]),
            "{doc}"
        );
        assert!(got.entries.iter().all(|e| e.change == DiffChange::Removed));
        assert_eq!(got.counts.removed, 3);
    }
}

#[test]
fn dq2_a_root_on_neither_side_is_refused() {
    let (_d, r, _c) = two_docs();
    let fix = "name a doc or folder that one side holds: rdy_search / rdy search";
    let e = refused(&r, &under("canon", "nowhere/at-all"));
    assert_eq!(e.code, E_UNKNOWN_BLOCK);
    assert_eq!(e.message, "nowhere/at-all is on neither side");
    assert_eq!(e.fix, fix);
    // an id that parses and that no side holds: a block of another project
    let (_d2, mut elsewhere) = project();
    let (absent, _, _) = seed_doc(&mut elsewhere, "seed");
    let e = refused(&r, &under("canon", &absent));
    assert_eq!(e.code, E_UNKNOWN_BLOCK);
    assert_eq!(e.message, format!("{absent} is on neither side"));
    assert_eq!(e.fix, fix);
}

#[test]
fn dq2_a_root_that_is_not_a_doc_or_folder_is_refused() {
    let (_d, r, c) = two_docs();
    let e = refused(&r, &under("canon", &c["$c1"]));
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!("{} is a claim, not a doc or a folder", c["$c1"])
    );
    assert_eq!(
        e.fix,
        "name a doc or folder that one side holds: rdy_search / rdy search"
    );
    let e = refused(&r, &under("canon", &c["$s1"]));
    assert_eq!(
        e.message,
        format!("{} is a section, not a doc or a folder", c["$s1"])
    );
}

#[test]
fn dq2_compact_strips_bodies_and_values_and_detail_keeps_them() {
    let (_d, mut r, c) = two_docs();
    write(
        &mut r,
        "edit",
        json!([
            {"op": "update", "id": c["$c1"], "body": "First, edited."},
            {"op": "update", "id": c["$s2"], "fields": {"title": "S2 again"}}
        ]),
    )
    .unwrap();
    let compact = reply(&r, &one("edit"));
    assert_eq!(compact.entries.len(), 2);
    for e in &compact.entries {
        assert_eq!(e.body, None);
        for f in &e.fields {
            assert_eq!(serde_json::to_value(f).unwrap(), json!({"name": f.name}));
        }
    }
    let section = compact
        .entries
        .iter()
        .find(|e| e.id.to_string() == c["$s2"])
        .unwrap();
    assert_eq!(section.fields.len(), 1);

    let detail = reply(
        &r,
        &DiffRequest {
            from: "edit".into(),
            detail: true,
            ..Default::default()
        },
    );
    let edited = detail
        .entries
        .iter()
        .find(|e| e.id.to_string() == c["$c1"])
        .unwrap();
    assert!(edited.body.as_ref().is_some_and(|b| !b.is_empty()));
    let section = detail
        .entries
        .iter()
        .find(|e| e.id.to_string() == c["$s2"])
        .unwrap();
    assert_eq!(
        serde_json::to_value(&section.fields[0]).unwrap(),
        json!({"name": "title", "from": "S2", "to": "S2 again"})
    );
}

#[test]
fn dq2_the_reply_json_has_the_frozen_keys() {
    let (_d, mut r, c) = two_docs();
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": c["$c1"], "body": "Edited."}]),
    )
    .unwrap();
    let got = serde_json::to_value(reply(&r, &under("edit", "DOC-ONE"))).unwrap();
    let keys: BTreeSet<&str> = got
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["from", "to", "root", "entries", "counts"])
    );
    assert_eq!(
        got["counts"],
        json!({"added": 0, "removed": 0, "updated": 1, "moved": 0})
    );
    assert_eq!(got["root"], json!(c["$one"]));
    // with no root the key is left out, not null
    let plain = serde_json::to_value(reply(&r, &one("edit"))).unwrap();
    assert!(plain.get("root").is_none(), "{plain}");
}

#[test]
fn dq2_landing_show_is_the_row_and_the_diff_from_prev() {
    let (_d, mut r, s, l1) = seeded();
    let c2 = add_claim(&mut r, "two", &s, "second");
    let l2 = land(&mut r, "two").unwrap();
    let c3 = add_claim(&mut r, "three", &s, "third");
    let l3 = land(&mut r, "three").unwrap();
    let read = r.read();
    for (id, prev, want) in [
        (&l1, None, None),
        (&l2, Some(&l1), Some(&c2)),
        (&l3, Some(&l2), Some(&c3)),
    ] {
        let hash: Hash = id.parse().unwrap();
        let show = read.landing_show(&hash, None, None).unwrap();
        assert_eq!(
            serde_json::to_value(&show.landing).unwrap(),
            serde_json::to_value(read.log_row(&hash).unwrap()).unwrap(),
            "{id}"
        );
        assert_eq!(show.diff.to, landing(id));
        assert_eq!(
            show.diff.from,
            prev.map_or(label(LabelKind::Empty), |p| landing(p)),
            "{id}"
        );
        match want {
            Some(c) => assert_eq!(ids(&show.diff), vec![c.clone()]),
            None => assert_eq!(show.diff.counts.added, 3),
        }
        // the change list is compact
        assert!(show.diff.entries.iter().all(|e| e.body.is_none()));
        let v = serde_json::to_value(&show).unwrap();
        let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, BTreeSet::from(["landing", "diff"]));
    }
}

#[test]
fn dq2_an_unknown_view_keeps_the_read_scope_refusal() {
    let (_d, r, _s, _l) = seeded();
    let e = refused(&r, &one("no-such-view"));
    assert_eq!(e.code, E_NOT_FOUND);
    assert_eq!(e.message, "no view \"no-such-view\"");
    // the session's own view exists lazily
    let got = r.read().diff_reply(&one("mine"), Some("mine")).unwrap();
    assert!(got.entries.is_empty());
}
