//! sr4: the block diff of two scopes (D-W40KZ, ra-R7PM6): one entry per changed block, in the
//! reading order of the newer scope, with word pieces for each changed body.
use std::collections::BTreeSet;

use rdy_engine::diff::{Diff, DiffChange, DiffCounts, DiffEntry};
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
    let names: Vec<&str> = entry(&diff, &ids.kept)
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(names, vec!["order"]);
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
