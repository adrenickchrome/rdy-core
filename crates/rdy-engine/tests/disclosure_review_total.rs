//! The `changes_total` member of the disclosure review's doc rows (D-6T9WJ): each changed, added
//! and removed doc row gives its changed blocks plus its changed decisions, by the rule its
//! counts already follow, also for what a cap leaves out of a list. No selection hash, selection
//! identity or doc digest reads it (D-B2AC3). Each test builds its states in memory over an
//! empty snapshot.

use rdy_engine::publication::review_model::{DisclosureReview, ReviewCounts};
use rdy_engine::publication::{
    self, CHANGES_MAX, DECISIONS_MAX, DisclosureDiff, PublicationReview, ReadPublicationSource,
    ReaderReviewSide, Resolved, ReviewProjection, ReviewRemovals, disclosure_diff,
    disclosure_review, review_json,
};
use rdy_engine::{Materialized, Rdy, ReadState};
use rdy_model::redaction::RedactedBlockIdentity;
use rdy_model::{Block, BlockId, FieldValue, Hash, StateSnapshot};
use serde_json::{Value, json};
use std::collections::BTreeMap;

// ---- fixtures -------------------------------------------------------------------------------

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).expect("an id")
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

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
        foreign: Default::default(),
        read_identity: None,
    }
}

fn with(mut b: Block, k: &str, v: FieldValue) -> Block {
    b.fields.insert(k.into(), v);
    b
}

fn body(mut b: Block, text: &str) -> Block {
    b.body = text.into();
    b
}

fn doc(id: &str, parent: Option<&str>, name: &str, title: &str, order: &str) -> Block {
    let b = with(blk(id, "doc", parent, order), "name", s(name));
    let b = with(b, "title", s(title));
    with(b, "doc_type", s("page"))
}

fn prose(id: &str, parent: &str, order: &str, text: &str) -> Block {
    body(blk(id, "prose", Some(parent), order), text)
}

fn section(id: &str, parent: &str, order: &str, title: &str) -> Block {
    with(blk(id, "section", Some(parent), order), "title", s(title))
}

fn decision(id: &str, parent: &str, order: &str, statement: &str) -> Block {
    let b = with(
        blk(id, "decision", Some(parent), order),
        "status",
        s("accepted"),
    );
    with(b, "statement", s(statement))
}

/// `b` read as its own removed version.
fn removed(b: &Block) -> Block {
    Block::redacted(
        b.id.clone(),
        b.block_type.clone(),
        b.parent.clone(),
        b.order.clone(),
        RedactedBlockIdentity {
            version: b.version_hash(),
            content_hash: b.content_hash(),
            frozen: false,
            request: Hash::of(b"a removal request"),
        },
    )
}

fn synth(blocks: &[Block]) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.iter().cloned().map(|b| (b.id.clone(), b)).collect(),
    )
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().expect("a temporary home");
    let r = Rdy::init(dir.path(), "rdy-standard").expect("a project");
    (dir, r)
}

/// One edit: the block's id and what it becomes (`None` drops it).
type Edit<'a> = (&'a str, &'a dyn Fn(Block) -> Option<Block>);

/// `blocks` with each block `edits` names replaced by the edit's result; an edit that gives
/// `None` drops the block.
fn edit(blocks: &[Block], edits: &[Edit]) -> Vec<Block> {
    blocks
        .iter()
        .filter_map(|b| match edits.iter().find(|(id, _)| b.id == bid(id)) {
            Some((_, f)) => f(b.clone()),
            None => Some(b.clone()),
        })
        .collect()
}

/// Both sides resolved with the implicit profile, compared and reviewed.
struct Reviewed {
    baseline: Resolved,
    candidate: Resolved,
    state: Materialized,
    diff: DisclosureDiff,
    review: DisclosureReview,
}

fn review(st: &ReadState, before: &[Block], after: &[Block]) -> Reviewed {
    let (bm, am) = (synth(before), synth(after));
    let b = publication::resolve_in_for_tests(st, &bm, publication::IMPLICIT_ORG).expect("before");
    let a = publication::resolve_in_for_tests(st, &am, publication::IMPLICIT_ORG).expect("after");
    let source = ReadPublicationSource::with_settings(st, st.pack(), "Project", None);
    let removals = ReviewRemovals::of_sides(&source, &[&b, &a]).expect("the removals");
    let (bv, av) = (
        ReaderReviewSide::new(&b, &removals),
        ReaderReviewSide::new(&a, &removals),
    );
    let diff = disclosure_diff(&b, &a).expect("the comparison");
    let review = disclosure_review(&diff, &bv, &av);
    Reviewed {
        baseline: b,
        candidate: a,
        state: am,
        diff,
        review,
    }
}

/// The review JSON of `rv` in `projection`, as chat, the CLI and the adapter write it.
fn json_of(st: &ReadState, rv: &Reviewed, projection: ReviewProjection) -> Value {
    let source = ReadPublicationSource::with_settings(st, st.pack(), "Project", None);
    let pr = PublicationReview::build(
        &source,
        &rv.state,
        &rv.candidate,
        Some((&rv.baseline, &rv.diff)),
    )
    .expect("the review");
    let j = review_json(projection, &rv.candidate.references, Some(&rv.diff), &pr);
    json!({"references": j.references, "disclosure": j.disclosure, "included": j.included})
}

const BOTH: [ReviewProjection; 2] = [ReviewProjection::Report, ReviewProjection::Adapter];

/// The row of `id` in the disclosure's `list`.
fn row<'a>(v: &'a Value, list: &str, id: &str) -> &'a Value {
    v["disclosure"][list]
        .as_array()
        .unwrap_or_else(|| panic!("no list {list}: {v}"))
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no row {id} in {list}"))
}

// ---- tests ----------------------------------------------------------------------------------

/// D-6T9WJ: a changed doc's `changes_total` is its added, removed, updated and moved blocks plus
/// its added, removed and updated decisions. A section and a figure are blocks already counted:
/// their own counts stay out of the sum.
#[test]
fn ct1_changed_row_sums_blocks_and_decisions_without_double_count() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-00001", None, "main", "Main", "a"),
        prose("p-00001", "d-00001", "b", "One."),
        prose("p-00002", "d-00001", "c", "Two."),
        prose("p-00003", "d-00001", "d", "Three."),
        prose("p-00004", "d-00001", "e", "Four."),
        decision("D-00001", "d-00001", "f", "Keep the rows."),
        decision("D-00002", "d-00001", "g", "Drop the rows."),
    ];
    let mut after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(body(b, "One, changed."))),
            ("p-00002", &|_| None),
            ("p-00004", &|mut b| {
                b.order = "a0".into();
                Some(b)
            }),
            ("D-00001", &|b| {
                Some(with(b, "statement", s("Keep every row.")))
            }),
            ("D-00002", &|_| None),
        ],
    );
    after.push(prose("p-00005", "d-00001", "h", "Five."));
    after.push(section("s-00001", "d-00001", "i", "Fresh"));
    after.push(decision("D-00003", "d-00001", "j", "Add a rule."));
    let rv = review(&st, &before, &after);
    let d = &rv.review.changed_docs[0];
    // added: the prose and the section; removed: one; updated: one; moved: one
    assert_eq!(
        d.counts,
        ReviewCounts {
            added: 2,
            removed: 1,
            updated: 1,
            moved: 1,
            sections_added: 1,
            decisions_added: 1,
            decisions_removed: 1,
            decisions_updated: 1,
            ..ReviewCounts::default()
        }
    );
    for p in BOTH {
        let v = json_of(&st, &rv, p);
        // 2 + 1 + 1 + 1 blocks, 3 decisions; the section's own count is not added again
        assert_eq!(row(&v, "changed_docs", "d-00001")["changes_total"], 8);
    }
}

/// D-6T9WJ: the total covers every change, also those a cap leaves out of the lists: 105 added
/// blocks and 55 changed decisions list 100 and 50 rows and count all of them.
#[test]
fn ct2_total_counts_the_rows_a_cap_leaves_out() {
    let (_dir, r) = project();
    let st = r.read();
    let extra_blocks = CHANGES_MAX + 5;
    let extra_decisions = DECISIONS_MAX + 5;
    let mut before = vec![
        doc("d-00001", None, "big", "Big", "a"),
        prose("p-00000", "d-00001", "a", "Base."),
    ];
    let mut after = before.clone();
    for i in 0..extra_blocks {
        after.push(prose(
            &format!("p-{:05}", i + 1),
            "d-00001",
            &format!("b{i:05}"),
            &format!("Added {i}."),
        ));
    }
    for i in 0..extra_decisions {
        let id = format!("D-{:05}", i + 1);
        before.push(decision(&id, "d-00001", &format!("c{i:05}"), "Old words."));
        after.push(decision(&id, "d-00001", &format!("c{i:05}"), "New words."));
    }
    let rv = review(&st, &before, &after);
    let d = &rv.review.changed_docs[0];
    assert_eq!(d.changes.len(), CHANGES_MAX);
    assert_eq!(d.omitted_changes, 5);
    assert_eq!(d.decisions.len(), DECISIONS_MAX);
    assert_eq!(d.omitted_decisions, 5);
    for p in BOTH {
        let v = json_of(&st, &rv, p);
        let r = row(&v, "changed_docs", "d-00001");
        assert_eq!(r["changes_total"], (extra_blocks + extra_decisions) as u64);
        assert_eq!(r["changes"].as_array().unwrap().len(), CHANGES_MAX);
    }
}

/// D-6T9WJ: an added doc gives 1 for the doc, plus its blocks and its decisions (sections and
/// figures are among its blocks). A removed doc gives the same, read on the baseline's side.
#[test]
fn ct3_added_and_removed_rows_give_one_plus_size() {
    let (_dir, r) = project();
    let st = r.read();
    let gone = vec![
        doc("d-00002", None, "gone", "Gone", "b"),
        section("s-00001", "d-00002", "a", "Part"),
        prose("p-00001", "s-00001", "a", "One."),
        prose("p-00002", "s-00001", "b", "Two."),
        decision("D-00001", "d-00002", "b", "A ruling."),
        decision("D-00002", "d-00002", "c", "Another ruling."),
    ];
    let mut before = vec![doc("d-00001", None, "keep", "Keep", "a")];
    before.extend(gone);
    let mut after = vec![doc("d-00001", None, "keep", "Keep", "a")];
    after.extend([
        doc("d-00003", None, "new", "New", "c"),
        prose("p-00003", "d-00003", "a", "Fresh."),
        decision("D-00003", "d-00003", "b", "A new ruling."),
    ]);
    let rv = review(&st, &before, &after);
    let (added, gone) = (&rv.review.added_docs[0], &rv.review.removed_docs[0]);
    assert_eq!((added.size.blocks, added.size.decisions), (1, 1));
    assert_eq!((gone.size.blocks, gone.size.decisions), (3, 2));
    for p in BOTH {
        let v = json_of(&st, &rv, p);
        assert_eq!(row(&v, "added_docs", "d-00003")["changes_total"], 3);
        assert_eq!(row(&v, "removed_docs", "d-00002")["changes_total"], 6);
    }
}

/// D-6T9WJ: a removed doc reads its size on the baseline's side, whatever the candidate holds
/// under the same id.
#[test]
fn ct4_removed_row_reads_the_baseline_size() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-00001", None, "keep", "Keep", "a"),
        doc("d-00002", None, "gone", "Gone", "b"),
        prose("p-00001", "d-00002", "a", "One."),
        prose("p-00002", "d-00002", "b", "Two."),
        prose("p-00003", "d-00002", "c", "Three."),
    ];
    let after = vec![
        doc("d-00001", None, "keep", "Keep", "a"),
        prose("p-00004", "d-00001", "a", "Elsewhere."),
    ];
    let rv = review(&st, &before, &after);
    assert_eq!(rv.review.removed_docs.len(), 1);
    for p in BOTH {
        let v = json_of(&st, &rv, p);
        assert_eq!(row(&v, "removed_docs", "d-00002")["changes_total"], 4);
    }
}

/// D-6T9WJ: a record-only row, whose digest changed with no counted change, gives 0; a redacted
/// block and a redacted decision count like any other.
#[test]
fn ct5_record_only_gives_zero_and_a_redacted_block_counts() {
    let (_dir, r) = project();
    let st = r.read();
    let pin = Hash([0x44; 32]);
    let secret = prose("p-00003", "d-00002", "b", "A secret.");
    let secret_ruling = decision("D-00001", "d-00002", "c", "A secret ruling.");
    // a removal reads the same on both sides; a block changed first, then removed, still counts
    let edited_secret = body(secret.clone(), "A secret, changed.");
    let edited_ruling = with(
        secret_ruling.clone(),
        "statement",
        s("A secret ruling, changed."),
    );
    let before = vec![
        doc("d-00001", None, "pins", "Pins", "a"),
        prose("p-00001", "d-00001", "a", "Pinned words."),
        prose("p-00002", "d-00001", "b", "Target."),
        doc("d-00002", None, "secrets", "Secrets", "b"),
        prose("p-00004", "d-00002", "a", "Open."),
        secret.clone(),
        secret_ruling.clone(),
    ];
    let after = edit(
        &before,
        &[
            ("p-00001", &|mut b| {
                b.pins.insert(bid("p-00002"), pin);
                Some(b)
            }),
            ("p-00003", &|_| Some(removed(&edited_secret))),
            ("D-00001", &|_| Some(removed(&edited_ruling))),
        ],
    );
    let rv = review(&st, &before, &after);
    let quiet = rv
        .review
        .changed_docs
        .iter()
        .find(|d| d.id == bid("d-00001"))
        .unwrap();
    assert!(quiet.record_only);
    let hidden = rv
        .review
        .changed_docs
        .iter()
        .find(|d| d.id == bid("d-00002"))
        .unwrap();
    assert!(!hidden.record_only);
    assert_eq!(hidden.counts.updated, 1, "{:?}", hidden.counts);
    assert_eq!(hidden.counts.decisions_updated, 1, "{:?}", hidden.counts);
    for p in BOTH {
        let v = json_of(&st, &rv, p);
        let q = row(&v, "changed_docs", "d-00001");
        assert_eq!(
            (q["record_only"].clone(), q["changes_total"].clone()),
            (json!(true), json!(0))
        );
        let h = row(&v, "changed_docs", "d-00002");
        assert_eq!(h["changes_total"], 2);
        assert!(
            h["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["redacted"] == true),
            "{h}"
        );
    }
}

/// D-6T9WJ, D-B2AC3: the member enters no selection hash, selection identity or doc digest. The
/// values below were taken before the member existed.
#[test]
fn ct6_hashes_and_digests_do_not_read_the_member() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-00001", None, "main", "Main", "a"),
        prose("p-00001", "d-00001", "a", "One."),
    ];
    let mut after = before.clone();
    after.push(prose("p-00002", "d-00001", "b", "Two."));
    let rv = review(&st, &before, &after);
    let line = |x: &Resolved| {
        format!(
            "{} {} {}",
            x.selection_hash,
            x.selection_identity,
            x.docs
                .iter()
                .map(|d| format!("{}={}", d.id, d.digest))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    assert_eq!(
        line(&rv.baseline),
        "dc13b4c5bb381365bc8ae6e2685f700fa43734308c1a7c6cf7ab042b6c16ae55 \
         63739af9c2d5dfd856495e8836191f9846677348188180dbfe76222d23bd4b9f \
         d-00001=114f1c21721b1dd3d79ea8873316427f3bae6c23b8211d966741f9fae9bbcde4"
    );
    assert_eq!(
        line(&rv.candidate),
        "5fb124b20c32bbbd0c9e922ca5f09347446bd07b58c0fe87659a7fb1f11dc6e4 \
         2f1720a8165d592c0ac3b0e7b9f818418bceefbc7d8c7fe9c0d9d96e091a1ca0 \
         d-00001=bb6a5d4c0cfd5e778b96f479ea381ae6cc5920fcfb92292e839f404b4673f56b"
    );
}
