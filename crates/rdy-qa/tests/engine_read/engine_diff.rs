//! sr4: the block diff of two scopes (D-W40KZ, ra-R7PM6): one entry per changed block, in the
//! reading order of the newer scope, with word pieces for each changed body.
use std::collections::BTreeSet;

use rdy_engine::diff::{Diff, DiffChange, DiffCounts, DiffEntry, FieldChange, FromTo};
use rdy_engine::worddiff::{Op, Piece};
use rdy_engine::{Materialized, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::golden::*;
use serde_json::json;

fn view() -> Scope {
    Scope::View(REVIEW_VIEW.into())
}

fn base(r: &Rdy, scope: &Scope) -> Scope {
    rdy_render::review::base_of(&r.read(), scope).expect("a view has a base")
}

fn entry_ids(d: &Diff) -> Vec<BlockId> {
    d.entries.iter().map(|e| e.id.clone()).collect()
}

fn entry<'a>(d: &'a Diff, id: &BlockId) -> &'a DiffEntry {
    d.entries
        .iter()
        .find(|e| &e.id == id)
        .unwrap_or_else(|| panic!("no entry for {id}: {:?}", entry_ids(d)))
}

/// The depth-first reading order of `m`: each block, then its children in order.
fn reading_order(m: &Materialized) -> Vec<BlockId> {
    fn walk(m: &Materialized, parent: Option<&BlockId>, out: &mut Vec<BlockId>) {
        for b in m.children(parent) {
            out.push(b.id.clone());
            walk(m, Some(&b.id), out);
        }
    }
    let mut out = vec![];
    walk(m, None, &mut out);
    out
}

fn children(m: &Materialized, id: &BlockId) -> Vec<BlockId> {
    m.children(Some(id)).iter().map(|b| b.id.clone()).collect()
}

fn piece(op: Op, text: &str) -> Piece {
    Piece {
        op,
        text: text.to_string(),
    }
}

/// The two sides the pieces rebuild: (`Same` + `Del`, `Same` + `Add`).
fn sides(pieces: &[Piece]) -> (String, String) {
    let (mut old, mut new) = (String::new(), String::new());
    for p in pieces {
        match p.op {
            Op::Same => {
                old.push_str(&p.text);
                new.push_str(&p.text);
            }
            Op::Del => old.push_str(&p.text),
            Op::Add => new.push_str(&p.text),
        }
    }
    (old, new)
}

#[test]
fn sr4_a_view_diff_lists_added_removed_and_updated_in_reading_order() {
    let (_d, r, ids) = review_project();
    let st = r.read();
    let view = view();
    let base = rdy_render::review::base_of(&st, &view).unwrap();
    let diff = st.diff(&base, &view).unwrap();
    let to = st.materialized(&view).unwrap();
    let from = st.materialized(&base).unwrap();

    // the view's decision: the one decision under the summary that the base lacks
    let decision: Vec<BlockId> = children(&to, &ids.summary)
        .into_iter()
        .filter(|id| from.get(id).is_none())
        .filter(|id| to.get(id).unwrap().block_type == "decision")
        .collect();
    assert_eq!(decision.len(), 1, "one decision in the view");
    let decision = decision[0].clone();
    let fresh_section = children(&to, &ids.fresh);
    assert_eq!(fresh_section.len(), 1);
    let old_section = children(&from, &ids.old);
    assert_eq!(old_section.len(), 1);
    let old_prose = children(&from, &old_section[0]);
    assert_eq!(old_prose.len(), 1);

    let kept: BTreeSet<BlockId> = [
        ids.lede.clone(),
        ids.edited.clone(),
        ids.added.clone(),
        decision.clone(),
        ids.fresh.clone(),
        fresh_section[0].clone(),
        ids.fresh_prose.clone(),
    ]
    .into_iter()
    .collect();
    let mut want = vec![];
    for id in reading_order(&to) {
        if id == ids.after {
            want.push(ids.gone.clone());
        }
        if kept.contains(&id) {
            want.push(id);
        }
    }
    want.extend([
        ids.old.clone(),
        old_section[0].clone(),
        old_prose[0].clone(),
    ]);
    assert_eq!(entry_ids(&diff), want);

    let change = |id: &BlockId| entry(&diff, id).change;
    assert_eq!(change(&ids.lede), DiffChange::Updated);
    assert_eq!(change(&ids.edited), DiffChange::Updated);
    assert_eq!(change(&ids.gone), DiffChange::Removed);
    for id in [
        &ids.added,
        &decision,
        &ids.fresh,
        &fresh_section[0],
        &ids.fresh_prose,
    ] {
        assert_eq!(change(id), DiffChange::Added, "{id}");
    }
    for id in [&ids.old, &old_section[0], &old_prose[0]] {
        assert_eq!(change(id), DiffChange::Removed, "{id}");
    }
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 5,
            removed: 4,
            updated: 2,
            moved: 0
        }
    );
    // a removed block names the doc that held it in `from`; a doc names itself
    assert_eq!(entry(&diff, &old_prose[0]).doc.as_ref(), Some(&ids.old));
    assert_eq!(entry(&diff, &ids.fresh).doc.as_ref(), Some(&ids.fresh));
    assert_eq!(entry(&diff, &ids.gone).doc.as_ref(), Some(&ids.intro));
}

#[test]
fn sr4_an_edited_body_carries_word_pieces() {
    let (_d, r, ids) = review_project();
    let view = view();
    let diff = r.read().diff(&base(&r, &view), &view).unwrap();
    let e = entry(&diff, &ids.edited);
    assert_eq!(
        e.body,
        Some(vec![
            piece(Op::Del, "Old"),
            piece(Op::Add, "New"),
            piece(Op::Same, " wording.")
        ])
    );
    assert_eq!(e.change, DiffChange::Updated);
    assert!(e.fields.is_empty(), "{:?}", e.fields);
    assert!(e.from_version.is_some() && e.to_version.is_some());
    assert_ne!(e.from_version, e.to_version);
    // an added block shows whole as added, a removed one whole as removed
    assert_eq!(
        entry(&diff, &ids.added).body,
        Some(vec![piece(Op::Add, "Brand new paragraph.")])
    );
    assert_eq!(
        entry(&diff, &ids.gone).body,
        Some(vec![piece(Op::Del, "Gone soon.")])
    );
}

#[test]
fn sr4_a_moved_block_says_where_it_moved() {
    let (_d, mut r, ids) = review_project();
    write(
        &mut r,
        "mv",
        json!([{"op": "move", "id": ids.kept.to_string(), "parent": ids.keep_summary.to_string()}]),
    )
    .unwrap();
    let view = Scope::View("mv".into());
    let diff = r.read().diff(&base(&r, &view), &view).unwrap();
    assert_eq!(entry_ids(&diff), vec![ids.kept.clone()]);
    let e = entry(&diff, &ids.kept);
    assert_eq!(e.change, DiffChange::Moved);
    let moved = e.moved.as_ref().expect("a moved block says where it moved");
    assert_eq!(moved.from_parent.as_ref(), Some(&ids.summary));
    assert_eq!(moved.to_parent.as_ref(), Some(&ids.keep_summary));
    assert!(!moved.reordered_only);
    assert_eq!(moved.from_path.as_deref(), Some("intro"));
    assert_eq!(moved.to_path.as_deref(), Some("keep"));
    assert_eq!(e.doc.as_ref(), Some(&ids.keep));
    assert_eq!(e.body, None);
    assert_eq!(diff.counts.moved, 1);
    assert_eq!(diff.counts.updated, 0);
}

#[test]
fn sr4_an_order_change_alone_is_reordered_only() {
    let (_d, mut r, ids) = review_project();
    // swap `kept` and `edited`: `kept` moves after `edited` under the same section
    write(
        &mut r,
        "swap",
        json!([{"op": "move", "id": ids.kept.to_string(), "parent": ids.summary.to_string(),
                "after": ids.edited.to_string()}]),
    )
    .unwrap();
    let view = Scope::View("swap".into());
    let st = r.read();
    let diff = st.diff(&base(&r, &view), &view).unwrap();
    assert!(!diff.entries.is_empty(), "the swap is a change");
    for e in &diff.entries {
        assert_eq!(e.change, DiffChange::Moved, "{}", e.id);
        let moved = e.moved.as_ref().expect("a moved entry says where");
        assert!(moved.reordered_only, "{}", e.id);
        assert_eq!(moved.from_parent.as_ref(), Some(&ids.summary));
        assert_eq!(moved.to_parent.as_ref(), Some(&ids.summary));
        assert_eq!(e.body, None);
    }
    // the order key is structure, not a field (task-4 ruling 1): the move says it
    for e in &diff.entries {
        assert!(e.fields.is_empty(), "{}: {:?}", e.id, e.fields);
    }
    assert_eq!(diff.counts.moved, diff.entries.len());
    assert_eq!(diff.counts.updated, 0);
}

#[test]
fn sr4_the_diff_matches_compare() {
    let (_d, mut r, ids) = review_project();
    write(
        &mut r,
        REVIEW_VIEW,
        json!([{"op": "move", "id": ids.kept.to_string(), "parent": ids.keep_summary.to_string()}]),
    )
    .unwrap();
    let st = r.read();
    let view = view();
    let base = rdy_render::review::base_of(&st, &view).unwrap();
    let diff = st.diff(&base, &view).unwrap();
    let cmp = st.compare(&view, &base).unwrap();
    let want: BTreeSet<String> = cmp
        .added
        .iter()
        .chain(&cmp.deleted)
        .chain(&cmp.changed)
        .chain(&cmp.moved)
        .map(|e| e.id.clone())
        .collect();
    let got: Vec<String> = diff.entries.iter().map(|e| e.id.to_string()).collect();
    let got_set: BTreeSet<String> = got.iter().cloned().collect();
    assert_eq!(got.len(), got_set.len(), "one entry per id: {got:?}");
    assert_eq!(got_set, want);
    assert_eq!(diff.counts.added, cmp.added.len());
    assert_eq!(diff.counts.removed, cmp.deleted.len());
}

#[test]
fn sr4_diff_since_a_tag_lists_the_landings_changes() {
    let (_d, r, ids) = review_project();
    let st = r.read();
    let tag = st.resolve_scope("tag:v1.0.0").unwrap();
    assert!(matches!(tag, Scope::At(_)));
    let diff = st.diff(&tag, &Scope::Canon).unwrap();
    assert_eq!(entry_ids(&diff), vec![ids.keep_prose.clone()]);
    let e = entry(&diff, &ids.keep_prose);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.doc.as_ref(), Some(&ids.keep));
    let pieces = e.body.as_ref().expect("an edited body has pieces");
    assert_eq!(
        sides(pieces),
        (
            "Nothing changes here.".to_string(),
            "Changed in canon later.".to_string()
        )
    );
}

#[test]
fn sr4_same_scopes_give_an_empty_diff() {
    let (_d, r, _ids) = review_project();
    let diff = r.read().diff(&Scope::Canon, &Scope::Canon).unwrap();
    assert!(diff.entries.is_empty(), "{:?}", entry_ids(&diff));
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 0,
            removed: 0,
            updated: 0,
            moved: 0
        }
    );
}

#[test]
fn sr4_the_diff_is_the_same_every_time() {
    let (_d, r, _ids) = review_project();
    let st = r.read();
    let view = view();
    let base = rdy_render::review::base_of(&st, &view).unwrap();
    let a = st.diff(&base, &view).unwrap();
    let b = st.diff(&base, &view).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

/// `prose` with four custom string fields, three named like structure (task-4 ruling 1).
const FIELDS_PACK: &str = "name: f\nversion: '1'\nextends: rdy-standard\nblock_types:\n  prose:\n    prefix: p\n    traits: [informative]\n    body: required\n    fields:\n      links: { kind: str }\n      order: { kind: str }\n      body: { kind: str }\n      normal: { kind: str }\n";

/// A landed doc whose section holds two proses, `a` then `b`, each with every custom field
/// set to "old", then an entity: (dir, rdy, section, a, b, entity).
fn fields_project() -> (tempfile::TempDir, Rdy, BlockId, BlockId, BlockId, BlockId) {
    let (dir, r) = tiny();
    // the store is locked while a handle is open
    drop(r);
    rdy_qa::packs::use_pack(dir.path(), "f", FIELDS_PACK, "name: Fields\n");
    let mut r = Rdy::open(dir.path()).unwrap();
    let old = json!({"links": "old", "order": "old", "body": "old", "normal": "old"});
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "f", "title": "F", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "prose", "id": "$a", "parent": "$s", "body": "First.", "fields": old},
        {"op": "create", "type": "prose", "id": "$b", "parent": "$s", "body": "Second.", "fields": old},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Thing"}}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| BlockId::parse(&res.created[k]).unwrap();
    let (s, a, b, e) = (id("$s"), id("$a"), id("$b"), id("$e"));
    (dir, r, s, a, b, e)
}

fn old_to_new(name: &str) -> FieldChange {
    FieldChange {
        name: name.to_string(),
        from: Some(json!("old")),
        to: Some(json!("new")),
    }
}

#[test]
fn sr4_custom_fields_named_like_structure_diff_as_fields() {
    let (_d, mut r, _s, a, _b, _e) = fields_project();
    for name in ["links", "order", "body", "normal"] {
        let view = format!("cf{name}");
        write(
            &mut r,
            &view,
            json!([{"op": "update", "id": a.to_string(), "fields": {name: "new"}}]),
        )
        .unwrap();
        let view = Scope::View(view);
        let diff = r.read().diff(&Scope::Canon, &view).unwrap();
        assert_eq!(entry_ids(&diff), vec![a.clone()], "{name}");
        let e = entry(&diff, &a);
        assert_eq!(e.change, DiffChange::Updated, "{name}");
        assert_eq!(e.fields, vec![old_to_new(name)], "{name}");
        assert_eq!(e.moved, None, "{name}");
        assert_eq!(e.body, None, "{name}");
        assert_eq!(e.links, None, "{name}");
        assert_eq!(e.type_change, None, "{name}");
        assert_eq!(
            diff.counts,
            DiffCounts {
                added: 0,
                removed: 0,
                updated: 1,
                moved: 0
            },
            "{name}"
        );
    }
}

#[test]
fn sr4_custom_field_and_real_move_together() {
    let (_d, mut r, s, a, b, thing) = fields_project();
    // `a` gets a new custom `order` and moves after `b`: a field change and a real reorder;
    // `b` gets a new custom `links` and a real link
    write(
        &mut r,
        "both",
        json!([{"op": "update", "id": a.to_string(), "fields": {"order": "new"},
                "move_to": {"parent": s.to_string(), "after": b.to_string()}},
               {"op": "update", "id": b.to_string(), "fields": {"links": "new"},
                "links": {"describes": [{"target": thing.to_string()}]}}]),
    )
    .unwrap();
    let view = Scope::View("both".into());
    let st = r.read();
    let diff = st.diff(&Scope::Canon, &view).unwrap();
    let to = st.materialized(&view).unwrap();
    assert_eq!(
        children(&to, &s),
        vec![b.clone(), a.clone(), thing.clone()],
        "the move landed"
    );
    assert_eq!(entry_ids(&diff), vec![b.clone(), a.clone()]);
    let e = entry(&diff, &a);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.fields, vec![old_to_new("order")]);
    let moved = e.moved.as_ref().expect("the real reorder is reported");
    assert!(moved.reordered_only);
    assert_eq!(moved.from_parent.as_ref(), Some(&s));
    assert_eq!(moved.to_parent.as_ref(), Some(&s));
    assert_eq!(e.links, None);
    // the custom `links` is a field; the real link list is the entry's own `links`
    let e = entry(&diff, &b);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.fields, vec![old_to_new("links")]);
    assert_eq!(e.moved, None);
    let links = e.links.as_ref().expect("the real link change is reported");
    assert_eq!(links.from, json!({}));
    assert!(
        links.to.to_string().contains(&thing.to_string()),
        "{links:?}"
    );
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 0,
            removed: 0,
            updated: 2,
            moved: 0
        }
    );
}

/// The review project with one more view, `retype`, that writes `ops(ids)`; then its diff from
/// its base.
fn retyped(
    ops: impl Fn(&ReviewIds) -> serde_json::Value,
) -> (tempfile::TempDir, Rdy, ReviewIds, Diff) {
    let (d, mut r, ids) = review_project();
    write(&mut r, "retype", ops(&ids)).unwrap();
    let view = Scope::View("retype".into());
    let diff = r.read().diff(&base(&r, &view), &view).unwrap();
    (d, r, ids, diff)
}

#[test]
fn sr4_type_only_change_is_an_update() {
    let (_d, _r, ids, diff) =
        retyped(|ids| json!([{"op": "update", "id": ids.kept.to_string(), "type": "note"}]));
    assert_eq!(entry_ids(&diff), vec![ids.kept.clone()]);
    let e = entry(&diff, &ids.kept);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.block_type, "note");
    assert_eq!(
        e.type_change,
        Some(FromTo {
            from: "prose".to_string(),
            to: "note".to_string()
        })
    );
    assert_eq!(e.moved, None);
    assert!(e.fields.is_empty(), "{:?}", e.fields);
    assert_eq!(e.body, None);
    // its own member, apart from the fields, so a custom field named `type` cannot collide
    let json = serde_json::to_value(e).unwrap();
    assert_eq!(json["type"], json!({"from": "prose", "to": "note"}));
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 0,
            removed: 0,
            updated: 1,
            moved: 0
        }
    );
}

#[test]
fn sr4_type_change_and_reorder() {
    let (_d, _r, ids, diff) = retyped(|ids| {
        json!([{"op": "update", "id": ids.kept.to_string(), "type": "note",
                "move_to": {"parent": ids.summary.to_string(), "after": ids.edited.to_string()}}])
    });
    assert_eq!(entry_ids(&diff), vec![ids.kept.clone()]);
    let e = entry(&diff, &ids.kept);
    assert_eq!(e.change, DiffChange::Updated);
    let moved = e.moved.as_ref().expect("the reorder is reported");
    assert!(moved.reordered_only);
    assert_eq!(moved.from_parent.as_ref(), Some(&ids.summary));
    assert_eq!(moved.to_parent.as_ref(), Some(&ids.summary));
    assert_eq!(
        e.type_change,
        Some(FromTo {
            from: "prose".to_string(),
            to: "note".to_string()
        })
    );
    assert!(e.fields.is_empty(), "{:?}", e.fields);
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 0,
            removed: 0,
            updated: 1,
            moved: 0
        }
    );
}

#[test]
fn sr4_type_change_and_parent_move() {
    let (_d, r, ids, diff) = retyped(|ids| {
        json!([{"op": "update", "id": ids.kept.to_string(), "type": "note",
                "move_to": {"parent": ids.keep_summary.to_string()}}])
    });
    assert_eq!(entry_ids(&diff), vec![ids.kept.clone()]);
    let e = entry(&diff, &ids.kept);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.block_type, "note");
    let moved = e.moved.as_ref().expect("the parent move is reported");
    assert!(!moved.reordered_only);
    assert_eq!(moved.from_parent.as_ref(), Some(&ids.summary));
    assert_eq!(moved.to_parent.as_ref(), Some(&ids.keep_summary));
    assert_eq!(e.doc.as_ref(), Some(&ids.keep));
    assert_eq!(
        e.type_change,
        Some(FromTo {
            from: "prose".to_string(),
            to: "note".to_string()
        })
    );
    assert!(e.fields.is_empty(), "{:?}", e.fields);
    assert_eq!(e.body, None);
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 0,
            removed: 0,
            updated: 1,
            moved: 0
        }
    );
    // the compare reports the type change at its source, as well as the move
    let st = r.read();
    let view = Scope::View("retype".into());
    let cmp = st.compare(&view, &base(&r, &view)).unwrap();
    let changed: Vec<&rdy_engine::CompareEntry> = cmp
        .changed
        .iter()
        .filter(|c| c.id == ids.kept.to_string())
        .collect();
    assert_eq!(changed.len(), 1, "{cmp:?}");
    assert_eq!(changed[0].fields, vec!["type".to_string()]);
    assert_eq!(cmp.moved.len(), 1, "{cmp:?}");
}

/// The fields project with its entity landed first under the section, so the section holds
/// `thing`, `a`, `b` and the prose `b` is last: (dir, rdy, section, a, b, thing).
fn last_project() -> (tempfile::TempDir, Rdy, BlockId, BlockId, BlockId, BlockId) {
    let (d, mut r, s, a, b, thing) = fields_project();
    write(
        &mut r,
        "prep",
        json!([{"op": "move", "id": thing.to_string(), "parent": s.to_string(),
                "before": a.to_string()}]),
    )
    .unwrap();
    land(&mut r, "prep").unwrap();
    let canon = r.read().materialized(&Scope::Canon).unwrap();
    assert_eq!(
        children(&canon, &s),
        vec![thing.clone(), a.clone(), b.clone()],
        "the prep landed"
    );
    (d, r, s, a, b, thing)
}

/// `b`'s order key in canon and in `view`, and the section's children in `view`.
fn keys_and_children(
    r: &Rdy,
    view: &Scope,
    s: &BlockId,
    b: &BlockId,
) -> (String, String, Vec<BlockId>) {
    let st = r.read();
    let (from, to) = (
        st.materialized(&Scope::Canon).unwrap(),
        st.materialized(view).unwrap(),
    );
    let (old, new) = (from.get(b).unwrap(), to.get(b).unwrap());
    (old.order.clone(), new.order.clone(), children(&to, s))
}

#[test]
fn sr4_appending_the_last_block_to_its_parent_is_no_move() {
    let (_d, mut r, s, a, b, thing) = last_project();
    // `b` is already last: appending it to its own parent gives it a new key, not a new place
    write(
        &mut r,
        "again",
        json!([{"op": "move", "id": b.to_string(), "parent": s.to_string()}]),
    )
    .unwrap();
    let view = Scope::View("again".into());
    let (old_key, new_key, kids) = keys_and_children(&r, &view, &s, &b);
    assert_ne!(old_key, new_key, "the append gives a new order key");
    assert_eq!(
        kids,
        vec![thing.clone(), a.clone(), b.clone()],
        "the place is the same"
    );
    let diff = r.read().diff(&Scope::Canon, &view).unwrap();
    for e in &diff.entries {
        assert_eq!(e.moved, None, "{}: no movement", e.id);
        assert_ne!(e.change, DiffChange::Moved, "{}", e.id);
    }
    assert_eq!(diff.counts.moved, 0);
    // the new key alone is a stored change with nothing to show (rule 1: neither)
    let e = entry(&diff, &b);
    assert_eq!(e.change, DiffChange::Updated);
    assert!(e.fields.is_empty(), "{:?}", e.fields);
    assert_eq!(e.body, None);
    assert_eq!(e.links, None);
    assert_eq!(e.type_change, None);
}

#[test]
fn sr4_appending_the_last_block_with_a_field_change_is_no_move() {
    let (_d, mut r, s, a, b, thing) = last_project();
    write(
        &mut r,
        "againf",
        json!([{"op": "update", "id": b.to_string(), "fields": {"normal": "new"},
                "move_to": {"parent": s.to_string()}}]),
    )
    .unwrap();
    let view = Scope::View("againf".into());
    let (old_key, new_key, kids) = keys_and_children(&r, &view, &s, &b);
    assert_ne!(old_key, new_key, "the append gives a new order key");
    assert_eq!(
        kids,
        vec![thing.clone(), a.clone(), b.clone()],
        "the place is the same"
    );
    let diff = r.read().diff(&Scope::Canon, &view).unwrap();
    assert_eq!(entry_ids(&diff), vec![b.clone()]);
    let e = entry(&diff, &b);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.fields, vec![old_to_new("normal")]);
    assert_eq!(e.moved, None, "no movement");
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 0,
            removed: 0,
            updated: 1,
            moved: 0
        }
    );
}

#[test]
fn sr4_a_block_its_siblings_move_around_does_not_move() {
    let (_d, mut r, s, a, b, thing) = last_project();
    // `a` changes a field and keeps its key; `b` moves to the front, so `a` is now last
    write(
        &mut r,
        "around",
        json!([{"op": "update", "id": a.to_string(), "fields": {"normal": "new"}},
               {"op": "move", "id": b.to_string(), "parent": s.to_string(),
                "before": thing.to_string()}]),
    )
    .unwrap();
    let view = Scope::View("around".into());
    let to = r.read().materialized(&view).unwrap();
    assert_eq!(children(&to, &s), vec![b.clone(), thing.clone(), a.clone()]);
    let diff = r.read().diff(&Scope::Canon, &view).unwrap();
    let e = entry(&diff, &a);
    assert_eq!(e.change, DiffChange::Updated);
    assert_eq!(e.moved, None, "`a` did not move: `b` did");
    let e = entry(&diff, &b);
    assert_eq!(e.change, DiffChange::Moved);
    assert!(e.moved.as_ref().unwrap().reordered_only);
    assert_eq!(
        diff.counts,
        DiffCounts {
            added: 0,
            removed: 0,
            updated: 1,
            moved: 1
        }
    );
}
