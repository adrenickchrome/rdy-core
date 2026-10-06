//! DX4 review mode, the model: what a draft changes against the canon it is based on (rv1;
//! FW-RV1, FW-RV2).
use rdy_engine::{Rdy, Scope};
use rdy_model::{BlockId, Hash};
use rdy_qa::golden::*;
use rdy_render::review::{self, DocChange, Mark, Review};
use serde_json::json;

fn rewrite() -> Scope {
    Scope::View(REVIEW_VIEW.into())
}

fn v1(r: &Rdy) -> Hash {
    r.read().versions().unwrap()[0].landing.parse().unwrap()
}

fn of(r: &Rdy) -> Review {
    review::build(&r.read(), &rewrite())
        .unwrap()
        .expect("a draft has a review")
}

/// Each removed line as (before, the ids of its blocks).
fn lines(rv: &Review, doc: &BlockId) -> Vec<(Option<BlockId>, Vec<BlockId>)> {
    rv.removed_in(doc)
        .iter()
        .map(|l| {
            (
                l.before.clone(),
                l.blocks.iter().map(|b| b.id.clone()).collect(),
            )
        })
        .collect()
}

#[test]
fn rv1_canon_and_tags_have_no_base_and_a_draft_reads_against_the_landing_it_is_based_on() {
    let (_d, r, _ids) = review_project();
    let read = r.read();
    let tag = Scope::At(v1(&r));
    assert_eq!(review::base_of(&read, &Scope::Canon), None);
    assert_eq!(review::base_of(&read, &tag), None);
    assert!(review::build(&read, &Scope::Canon).unwrap().is_none());
    assert!(review::build(&read, &tag).unwrap().is_none());
    // the draft was written on v1.0.0's landing and a later landing moved canon on: the base is
    // that landing, or canon itself when the later landing rebased the draft onto canon's head
    match review::base_of(&read, &rewrite()) {
        Some(Scope::At(h)) => assert_eq!(h, v1(&r)),
        Some(Scope::Canon) => assert_eq!(read.effective_base(REVIEW_VIEW), read.canon_head()),
        other => panic!("a draft's base is canon at a landing, not {other:?}"),
    }
}

#[test]
fn rv1_a_draft_marks_edited_and_new_blocks_and_counts_them_per_doc() {
    let (_d, r, ids) = review_project();
    let rv = of(&r);
    assert_eq!(rv.mark(&ids.lede), Some(Mark::Edited));
    assert_eq!(rv.mark(&ids.edited), Some(Mark::Edited));
    assert_eq!(rv.mark(&ids.added), Some(Mark::New));
    for same in [&ids.intro, &ids.summary, &ids.kept, &ids.after] {
        assert_eq!(rv.mark(same), None, "{same}");
    }
    // the decision the draft adds is not on the page: it is not counted (new would be 2)
    let intro = DocChange {
        mark: Mark::Edited,
        edited: 2,
        new: 1,
        removed: 1,
    };
    assert_eq!(rv.doc(&ids.intro), Some(&intro));
    assert_eq!(rv.page_mark(&ids.intro), Some(Mark::Edited));
    assert_eq!(
        intro.counts().as_deref(),
        Some("2 edited \u{b7} 1 new \u{b7} 1 removed")
    );
    assert_eq!((Mark::Edited.word(), Mark::New.word()), ("Edited", "New"));
}

#[test]
fn rv1_a_new_doc_is_new_and_its_blocks_carry_no_mark_of_their_own() {
    let (_d, r, ids) = review_project();
    let rv = of(&r);
    assert_eq!(rv.mark(&ids.fresh), Some(Mark::New));
    assert_eq!(rv.page_mark(&ids.fresh), Some(Mark::New));
    assert_eq!(
        rv.doc(&ids.fresh),
        Some(&DocChange {
            mark: Mark::New,
            edited: 0,
            new: 0,
            removed: 0
        })
    );
    assert_eq!(rv.doc(&ids.fresh).unwrap().counts(), None);
    assert_eq!(rv.mark(&ids.fresh_prose), None);
}

#[test]
fn rv1_a_removed_block_is_placed_before_the_next_block_that_stays() {
    let (_d, r, ids) = review_project();
    let rv = of(&r);
    assert_eq!(
        lines(&rv, &ids.intro),
        vec![(Some(ids.after.clone()), vec![ids.gone.clone()])]
    );
    assert_eq!(rv.removed_in(&ids.intro)[0].blocks[0].body, "Gone soon.");
    assert!(rv.removed_in(&ids.keep).is_empty());
}

#[test]
fn rv1_a_removed_doc_is_listed_once_and_its_blocks_are_not() {
    let (_d, r, ids) = review_project();
    let rv = of(&r);
    let pages: Vec<(&BlockId, Option<&str>)> = rv
        .removed_pages()
        .iter()
        .map(|b| (&b.id, b.field_str("title")))
        .collect();
    assert_eq!(pages, vec![(&ids.old, Some("Old guide"))]);
    assert!(rv.removed_in(&ids.old).is_empty());
    assert_eq!(rv.doc(&ids.old), None);
}

#[test]
fn rv1_canon_moving_on_after_the_draft_marks_nothing_in_the_draft() {
    let (_d, r, ids) = review_project();
    let rv = of(&r);
    assert_eq!(rv.mark(&ids.keep_prose), None);
    assert_eq!(rv.doc(&ids.keep), None);
    assert_eq!(rv.page_mark(&ids.keep), None);
}

#[test]
fn rv1_a_removed_block_with_nothing_after_it_shows_at_the_doc_end() {
    let (_d, mut r, ids) = review_project();
    write(
        &mut r,
        REVIEW_VIEW,
        json!([{"op": "delete", "id": ids.after.to_string()}]),
    )
    .unwrap();
    let rv = of(&r);
    assert_eq!(
        lines(&rv, &ids.intro),
        vec![
            (None, vec![ids.gone.clone()]),
            (None, vec![ids.after.clone()])
        ]
    );
    assert_eq!(rv.doc(&ids.intro).unwrap().removed, 2);
}

#[test]
fn rv1_a_removed_section_is_one_line_holding_its_old_blocks() {
    let (_d, mut r, ids) = review_project();
    write(
        &mut r,
        REVIEW_VIEW,
        json!([
            {"op": "delete", "id": ids.keep_prose.to_string()},
            {"op": "delete", "id": ids.keep_summary.to_string()}
        ]),
    )
    .unwrap();
    let rv = of(&r);
    assert_eq!(
        lines(&rv, &ids.keep),
        vec![(None, vec![ids.keep_summary.clone(), ids.keep_prose.clone()])]
    );
    assert_eq!(
        rv.doc(&ids.keep),
        Some(&DocChange {
            mark: Mark::Edited,
            edited: 0,
            new: 0,
            removed: 1
        })
    );
}

#[test]
fn rv1_a_block_moved_to_another_doc_is_edited_there_and_removed_here() {
    let (_d, mut r, ids) = review_project();
    write(
        &mut r,
        REVIEW_VIEW,
        json!([{"op": "move", "id": ids.kept.to_string(), "parent": ids.keep_summary.to_string()}]),
    )
    .unwrap();
    let rv = of(&r);
    assert_eq!(rv.mark(&ids.kept), Some(Mark::Edited));
    assert_eq!(
        rv.doc(&ids.keep),
        Some(&DocChange {
            mark: Mark::Edited,
            edited: 1,
            new: 0,
            removed: 0
        })
    );
    assert_eq!(
        lines(&rv, &ids.intro),
        vec![
            (Some(ids.edited.clone()), vec![ids.kept.clone()]),
            (Some(ids.after.clone()), vec![ids.gone.clone()])
        ]
    );
    assert_eq!(rv.doc(&ids.intro).unwrap().removed, 2);
}

#[test]
fn rv1_counts_name_only_what_changed() {
    let c = |edited, new, removed| DocChange {
        mark: Mark::Edited,
        edited,
        new,
        removed,
    };
    assert_eq!(c(0, 0, 1).counts().as_deref(), Some("1 removed"));
    assert_eq!(c(1, 0, 0).counts().as_deref(), Some("1 edited"));
    assert_eq!(
        c(0, 3, 2).counts().as_deref(),
        Some("3 new \u{b7} 2 removed")
    );
    assert_eq!(c(0, 0, 0).counts(), None);
}
