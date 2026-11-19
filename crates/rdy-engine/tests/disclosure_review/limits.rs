//! The totals and the bounded rows of the disclosure review (D-RDH1C, D-0C4TB, D-BND1J): totals
//! over the complete comparison, the even share of the 2,000 change rows, the 200-row outer
//! lists of the report, and the byte limits of the report and the adapter review. Each test
//! builds its states in memory over an empty snapshot.

use rdy_engine::publication::review_model::{
    ChangedDocReview, DisclosureReview, DisclosureTotals, Included, OutsideReview, ReviewChange,
    ReviewCounts,
};
use rdy_engine::publication::{
    self, ADAPTER_REVIEW_BYTES, CHANGE_BUDGET, CHANGES_MAX, DisclosureDiff, PublicationReviewSide,
    REPORT_BYTES, REPORT_ROWS, ReadPublicationSource, ReaderReviewSide, ReviewProjection,
    ReviewRemovals, change_shares, compact_len, disclosure_diff, disclosure_review, included,
};
use rdy_engine::{E_LIMIT, Materialized, Rdy, RdyError, ReadState};
use rdy_model::{Block, BlockId, FieldValue, Hash, Link, StateSnapshot};
use serde_json::json;
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

fn link(mut b: Block, kind: &str, target: &str) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: bid(target),
        part: None,
    });
    b
}

fn folder(id: &str, parent: Option<&str>, name: &str, title: &str, order: &str) -> Block {
    with(
        with(blk(id, "folder", parent, order), "name", s(name)),
        "title",
        s(title),
    )
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

fn question(id: &str, parent: &str, order: &str, prompt: &str) -> Block {
    with(
        blk(id, "question", Some(parent), order),
        "prompt",
        s(prompt),
    )
}

fn decision(id: &str, parent: &str, order: &str, statement: &str) -> Block {
    let b = with(
        blk(id, "decision", Some(parent), order),
        "status",
        s("accepted"),
    );
    with(b, "statement", s(statement))
}

fn planned(b: Block) -> Block {
    with(b, "planned", FieldValue::Bool(true))
}

fn hash(n: u16) -> Hash {
    let mut h = [0u8; 32];
    h[..2].copy_from_slice(&n.to_be_bytes());
    h[31] = 9;
    Hash(h)
}

/// A figure with a light image, and a dark one when `dark` is set.
fn figure(
    id: &str,
    parent: &str,
    order: &str,
    title: &str,
    light: u16,
    dark: Option<u16>,
) -> Block {
    let b = with(blk(id, "figure", Some(parent), order), "title", s(title));
    let b = with(b, "alt", s(title));
    let b = with(b, "blob", s(&hash(light).to_string()));
    let b = with(b, "media_type", s("image/png"));
    match dark {
        Some(d) => with(
            with(b, "dark_blob", s(&hash(d).to_string())),
            "dark_media_type",
            s("image/png"),
        ),
        None => b,
    }
}

fn profile(id: &str, select: &[&str], planned: &str) -> Block {
    let b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    let b = with(b, "key", s("site"));
    let b = with(b, "title", s("Profile title"));
    let b = with(b, "audience", s("org"));
    let b = with(b, "planned", s(planned));
    with(
        b,
        "select",
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    )
}

fn admin() -> Block {
    doc("d-ADM1N", None, "admin", "Admin", "zz")
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

/// `b` under `parent` at `order`.
fn moved(mut b: Block, parent: &str, order: &str) -> Block {
    b.parent = Some(bid(parent));
    b.order = order.into();
    b
}

/// Both sides resolved with `profile`, viewed with the comparison's removals, compared and
/// reviewed.
struct Reviewed {
    before: ReaderReviewSide,
    after: ReaderReviewSide,
    diff: DisclosureDiff,
    review: DisclosureReview,
}

fn review(st: &ReadState, before: &[Block], after: &[Block], profile: &str) -> Reviewed {
    let (bm, am) = (synth(before), synth(after));
    let b = publication::resolve_in_for_tests(st, &bm, profile).expect("the baseline");
    let a = publication::resolve_in_for_tests(st, &am, profile).expect("the candidate");
    let source = ReadPublicationSource::with_settings(st, st.pack(), "Project", None);
    let removals = ReviewRemovals::of_sides(&source, &[&b, &a]).expect("the removals");
    let (bv, av) = (
        ReaderReviewSide::new(&b, &removals),
        ReaderReviewSide::new(&a, &removals),
    );
    let diff = disclosure_diff(&b, &a).expect("the comparison");
    let review = disclosure_review(&diff, &bv, &av);
    Reviewed {
        before: bv,
        after: av,
        diff,
        review,
    }
}

fn doc_row<'a>(r: &'a Reviewed, id: &str) -> &'a ChangedDocReview {
    r.review
        .changed_docs
        .iter()
        .find(|d| d.id == bid(id))
        .unwrap_or_else(|| panic!("no row for {id}: {:?}", ids(&r.review.changed_docs)))
}

fn ids(docs: &[ChangedDocReview]) -> Vec<String> {
    docs.iter().map(|d| d.id.to_string()).collect()
}

fn kinds(d: &ChangedDocReview) -> Vec<(String, ReviewChange)> {
    d.changes
        .iter()
        .map(|c| (c.id.to_string(), c.change))
        .collect()
}

fn outside<'a>(r: &'a Reviewed, id: &str) -> &'a OutsideReview {
    r.review
        .changed_outside_docs
        .iter()
        .find(|o| o.id == bid(id))
        .unwrap_or_else(|| panic!("no outside row for {id}"))
}

fn counts(f: impl FnOnce(&mut ReviewCounts)) -> ReviewCounts {
    let mut c = ReviewCounts::default();
    f(&mut c);
    c
}

fn totals(f: impl FnOnce(&mut DisclosureTotals)) -> DisclosureTotals {
    let mut t = DisclosureTotals::default();
    f(&mut t);
    t
}

/// A changed doc `d-{n}` at site place `n`, holding one base prose, and `adds` added proses
/// in page order. The added prose `i` of doc `n` is `p-` and the number `n * 128 + i`.
fn growing_doc(n: usize, adds: usize) -> (Vec<Block>, Vec<Block>) {
    let id = format!("d-{n:05}");
    let base = vec![
        doc(
            &id,
            None,
            &format!("doc{n}"),
            &format!("Doc {n}"),
            &format!("a{n:05}"),
        ),
        prose(&format!("pb-{n:05}"), &id, "a", "Base words."),
    ];
    let mut after = base.clone();
    for i in 0..adds {
        after.push(prose(
            &format!("p-{:05}", n * 128 + i),
            &id,
            &format!("b{i:05}"),
            &format!("Added words {i}."),
        ));
    }
    (base, after)
}

/// The changed docs of `sizes`, each with that many added rows, reviewed with the implicit
/// profile.
fn growing(st: &ReadState, sizes: &[usize]) -> Reviewed {
    let (mut before, mut after) = (vec![], vec![]);
    for (n, adds) in sizes.iter().enumerate() {
        let (b, a) = growing_doc(n, *adds);
        before.extend(b);
        after.extend(a);
    }
    review(st, &before, &after, publication::IMPLICIT_ORG)
}

/// Each changed doc's (kept, omitted) change rows, in row order.
fn kept(r: &Reviewed) -> Vec<(usize, u64)> {
    r.review
        .changed_docs
        .iter()
        .map(|d| (d.changes.len(), d.omitted_changes))
        .collect()
}

/// The first `kept` added rows of doc `n` are the doc's first rows in page order.
fn assert_first_rows(d: &ChangedDocReview, n: usize) {
    for (i, c) in d.changes.iter().enumerate() {
        assert_eq!(
            c.id,
            bid(&format!("p-{:05}", n * 128 + i)),
            "{} row {i}",
            d.id
        );
        assert_eq!(c.change, ReviewChange::Added);
    }
}

// ---- totals ---------------------------------------------------------------------------------

/// D-RDH1C, D-0C4TB: the totals cover every row of the complete comparison, before any list is
/// cut. An ordinary block that moves to another doc counts once, at its new home, though its
/// old doc lists and counts a removed row.
#[test]
fn drv4_totals_cover_every_row_and_count_cross_doc_moves_once() {
    let (_dir, r) = project();
    let st = r.read();
    let mut before = vec![
        doc("d-A0001", None, "from", "From", "a"),
        prose("p-00001", "d-A0001", "a", "Moves."),
        prose("p-00002", "d-A0001", "b", "Edited before."),
        section("s-00001", "d-A0001", "c", "Part"),
        prose("p-00003", "s-00001", "a", "Moves with its section."),
        prose("p-00004", "d-A0001", "d", "Removed."),
        doc("d-B0001", None, "to", "To", "b"),
        prose("p-00010", "d-B0001", "a", "Here."),
        doc("d-C0001", None, "many", "Many", "c"),
        prose("p-C0000", "d-C0001", "a", "Base."),
    ];
    let mut after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(moved(b, "d-B0001", "b"))),
            ("p-00002", &|b| Some(body(b, "Edited after."))),
            ("s-00001", &|b| Some(moved(b, "d-B0001", "c"))),
            ("p-00004", &|_| None),
        ],
    );
    after.push(prose("p-00011", "d-B0001", "d", "Added here."));
    for i in 1..=150 {
        after.push(prose(
            &format!("p-C{i:04}"),
            "d-C0001",
            &format!("b{i:04}"),
            "Many words.",
        ));
    }
    before.push(admin());
    after.push(admin());
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    assert_eq!(
        ids(&rv.review.changed_docs),
        vec!["d-A0001", "d-B0001", "d-C0001"]
    );

    let a = doc_row(&rv, "d-A0001");
    assert_eq!(
        kinds(a),
        vec![
            // the lede first, then the removed blocks
            ("p-00002".into(), ReviewChange::Updated),
            ("p-00001".into(), ReviewChange::Removed),
            ("s-00001".into(), ReviewChange::Removed),
            ("p-00003".into(), ReviewChange::Removed),
            ("p-00004".into(), ReviewChange::Removed),
        ]
    );
    assert_eq!(
        a.counts,
        counts(|c| {
            c.updated = 1;
            c.removed = 4;
            c.sections_removed = 1;
        })
    );
    let b = doc_row(&rv, "d-B0001");
    assert_eq!(
        b.counts,
        counts(|c| {
            c.moved = 3;
            c.added = 1;
        })
    );
    // the counts stay complete when the list is cut to its doc's limit
    let c = doc_row(&rv, "d-C0001");
    assert_eq!(c.counts, counts(|c| c.added = 150));
    assert_eq!((c.changes.len(), c.omitted_changes), (CHANGES_MAX, 50));
    for (i, row) in c.changes.iter().enumerate() {
        assert_eq!(row.id, bid(&format!("p-C{:04}", i + 1)), "page order");
    }

    // every block once: the moved blocks at their new home, the removed one at its old
    assert_eq!(
        rv.review.totals,
        totals(|t| {
            t.docs_changed = 3;
            t.blocks_added = 151;
            t.blocks_removed = 1;
            t.blocks_updated = 1;
            t.blocks_moved = 3;
        })
    );
    // the totals are no sum of the cut lists
    let listed: usize = rv.review.changed_docs.iter().map(|d| d.changes.len()).sum();
    assert_eq!(listed, 5 + 4 + 100);
}

/// D-0C4TB: an added or removed doc or folder counts as one block, and each shown block inside
/// it counts too; a changed folder counts once for its title, name, description or place.
/// Figures count by attachment image, light and dark each, and each figure block counts as a
/// block too.
#[test]
fn drv4_doc_folder_units_and_image_counts_follow_review_chips() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        admin(),
        folder("f-00001", None, "kept", "Kept", "a"),
        doc("d-00001", Some("f-00001"), "pictures", "Pictures", "a"),
        figure("x-00001", "d-00001", "a", "Light only", 1, None),
        figure("x-00002", "d-00001", "b", "Dark changes", 2, Some(3)),
        figure("x-00003", "d-00001", "c", "Both go", 4, Some(5)),
        folder("f-00002", None, "mover", "Mover", "b"),
        doc("d-00002", Some("f-00002"), "rides", "Rides", "a"),
        prose("p-00002", "d-00002", "a", "Rides along."),
        folder("f-00003", None, "gone", "Gone", "c"),
        doc("d-00003", Some("f-00003"), "gonedoc", "Gone doc", "a"),
        prose("p-00030", "d-00003", "a", "Old words."),
        section("s-00030", "d-00003", "b", "Old part"),
        figure("x-00030", "s-00030", "a", "Old chart", 6, Some(7)),
        decision("D-00030", "d-00003", "c", "Old rule."),
    ];
    let mut after = edit(
        &before,
        &[
            ("f-00001", &|b| Some(with(b, "title", s("Kept, renamed")))),
            ("x-00001", &|b| {
                Some(with(
                    with(b, "dark_blob", s(&hash(8).to_string())),
                    "dark_media_type",
                    s("image/png"),
                ))
            }),
            ("x-00002", &|b| {
                Some(with(b, "dark_blob", s(&hash(9).to_string())))
            }),
            ("x-00003", &|_| None),
            ("f-00002", &|b| Some(moved(b, "f-00001", "b"))),
            ("f-00003", &|_| None),
            ("d-00003", &|_| None),
            ("p-00030", &|_| None),
            ("s-00030", &|_| None),
            ("x-00030", &|_| None),
            ("D-00030", &|_| None),
        ],
    );
    after.extend([
        figure("x-00004", "d-00001", "d", "New pair", 10, Some(11)),
        folder("f-00004", None, "new", "New", "d"),
        doc("d-00004", Some("f-00004"), "newdoc", "New doc", "a"),
        prose("p-00040", "d-00004", "a", "New words."),
        section("s-00040", "d-00004", "b", "New part"),
        prose("p-00041", "s-00040", "a", "Inside."),
        figure("x-00040", "s-00040", "b", "New chart", 12, Some(13)),
        decision("D-00040", "d-00004", "c", "New rule."),
    ]);
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);

    let pictures = doc_row(&rv, "d-00001");
    assert_eq!(
        pictures.counts,
        counts(|c| {
            c.added = 1;
            c.removed = 1;
            c.updated = 2;
            c.figures_added = 3;
            c.figures_removed = 2;
            c.figures_changed = 1;
        })
    );
    // the doc whose folder moved changed its path only: its row has nothing a reader sees
    let rides = doc_row(&rv, "d-00002");
    assert!(rides.record_only && rides.changes.is_empty());

    let added = &rv.review.added_docs;
    assert_eq!(added.len(), 1);
    assert_eq!(
        (
            added[0].size.blocks,
            added[0].size.sections,
            added[0].size.figures,
            added[0].size.decisions
        ),
        (4, 1, 2, 1)
    );
    let removed = &rv.review.removed_docs;
    assert_eq!(removed.len(), 1);
    assert_eq!(
        (
            removed[0].size.blocks,
            removed[0].size.sections,
            removed[0].size.figures,
            removed[0].size.decisions
        ),
        (3, 1, 2, 1)
    );

    assert_eq!(
        rv.review.totals,
        totals(|t| {
            t.docs_changed = 2;
            t.docs_added = 1;
            t.docs_removed = 1;
            t.docs_record_only = 1;
            // the new folder, the new doc and its 4 blocks, and the new figure
            t.blocks_added = 1 + (1 + 4) + 1;
            // the gone folder, the gone doc and its 3 blocks, and the gone figure
            t.blocks_removed = 1 + (1 + 3) + 1;
            // the renamed folder and two figures whose images changed
            t.blocks_updated = 3;
            // the folder that took a new place
            t.blocks_moved = 1;
            t.sections_added = 1;
            t.sections_removed = 1;
            t.figures_added = 2 + 3;
            t.figures_removed = 2 + 2;
            t.figures_changed = 1;
            t.decisions_added = 1;
            t.decisions_removed = 1;
        })
    );
}

/// D-BND1J, D-0C4TB: a row outside docs counts by its parts, with sibling place left out. A
/// version change with no changed part keeps its row with `change` null, an empty `what` and
/// `record_only` true, and counts nothing. Outside decisions count in the decision totals.
#[test]
fn drv4_outside_rows_count_parts_and_ignore_record_only() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-00001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "Stays."),
        question("q-00001", "f-00001", "b", "Old prompt?"),
        question("q-00002", "f-00001", "c", "Reordered?"),
        question("q-00003", "f-00001", "d", "Removed?"),
        decision("D-00001", "f-00001", "e", "Old statement."),
        with(
            decision("D-00003", "f-00001", "f", "Same statement."),
            "rationale",
            s("Because."),
        ),
        body(
            blk("r-00001", "rationale", Some("D-00003"), "a"),
            "Old reason.",
        ),
    ];
    let mut after = edit(
        &before,
        &[
            ("q-00001", &|b| Some(with(b, "prompt", s("New prompt?")))),
            // a new place among its siblings, before the doc: no counted part
            ("q-00002", &|mut b| {
                b.order = "0".into();
                Some(b)
            }),
            ("q-00003", &|_| None),
            ("D-00001", &|b| {
                Some(with(b, "statement", s("New statement.")))
            }),
            ("D-00003", &|b| {
                Some(with(b, "rationale", s("Because, more.")))
            }),
            ("r-00001", &|b| Some(body(b, "New reason."))),
        ],
    );
    after.extend([
        question("q-00004", "f-00001", "g", "Added?"),
        decision("D-00002", "f-00001", "h", "Added statement."),
    ]);
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    assert!(
        rv.review.changed_docs.is_empty(),
        "{:?}",
        ids(&rv.review.changed_docs)
    );

    let rows: Vec<(String, Option<ReviewChange>, Vec<&str>, bool)> = rv
        .review
        .changed_outside_docs
        .iter()
        .map(|o| {
            (
                o.id.to_string(),
                o.change,
                o.what.iter().map(String::as_str).collect(),
                o.record_only,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "D-00001".into(),
                Some(ReviewChange::Updated),
                vec!["statement"],
                false
            ),
            ("D-00002".into(), Some(ReviewChange::Added), vec![], false),
            ("D-00003".into(), None, vec![], true),
            (
                "q-00001".into(),
                Some(ReviewChange::Updated),
                vec!["text"],
                false
            ),
            ("q-00002".into(), None, vec![], true),
            ("q-00003".into(), Some(ReviewChange::Removed), vec![], false),
            ("q-00004".into(), Some(ReviewChange::Added), vec![], false),
            ("r-00001".into(), None, vec![], true),
        ],
        "every version-change row stays, in id order"
    );
    // each row is the diff's row; its type and label read the candidate, or the baseline for a
    // removed block
    assert_eq!(
        rv.review
            .changed_outside_docs
            .iter()
            .map(|o| o.id.clone())
            .collect::<Vec<_>>(),
        rv.diff
            .changed_outside_docs
            .iter()
            .map(|o| o.id.clone())
            .collect::<Vec<_>>()
    );
    let q1 = outside(&rv, "q-00001");
    assert_eq!(q1.block_type, "question");
    assert_eq!(q1.label, rv.after.shown(&bid("q-00001")).unwrap().label);
    let q3 = outside(&rv, "q-00003");
    assert_eq!(q3.label, rv.before.shown(&bid("q-00003")).unwrap().label);
    assert!(rv.after.shown(&bid("q-00003")).is_none());
    assert_eq!(outside(&rv, "D-00001").block_type, "decision");

    assert_eq!(
        rv.review.totals,
        totals(|t| {
            t.blocks_added = 1;
            t.blocks_removed = 1;
            t.blocks_updated = 1;
            t.decisions_added = 1;
            t.decisions_updated = 1;
        })
    );
}

/// D-0C4TB: outside content takes the part of a doc for moves to and from it. A block that moves
/// between a doc and that content gets its one counted row at its new place; the old place gives
/// a removed row, and the totals count the move once. A move into an added doc counts in that
/// doc's size, and a move out of a removed doc leaves that doc's size.
#[test]
fn drv4_doc_outside_moves_count_once_at_the_new_home() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-00001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "Stays."),
        question("q-00003", "d-00001", "b", "Moves out?"),
        question("q-00005", "d-00001", "c", "Moves out, edited?"),
        question("q-00001", "f-00001", "b", "Moves in?"),
        question("q-00002", "f-00001", "c", "Moves in, edited?"),
        question("q-00006", "f-00001", "d", "Moves into a new doc?"),
        doc("d-00003", Some("f-00001"), "old", "Old", "e"),
        question("q-00007", "d-00003", "a", "Leaves a removed doc?"),
        prose("p-00008", "d-00003", "b", "Goes with it."),
    ];
    let mut after = edit(
        &before,
        &[
            ("q-00001", &|b| Some(moved(b, "d-00001", "d"))),
            ("q-00002", &|b| {
                Some(with(
                    moved(b, "d-00001", "e"),
                    "prompt",
                    s("Moved in and edited?"),
                ))
            }),
            ("q-00003", &|b| Some(moved(b, "f-00001", "f"))),
            ("q-00005", &|b| {
                Some(with(
                    moved(b, "f-00001", "g"),
                    "prompt",
                    s("Moved out and edited?"),
                ))
            }),
            ("q-00006", &|b| Some(moved(b, "d-00002", "a"))),
            ("d-00003", &|_| None),
            ("q-00007", &|b| Some(moved(b, "f-00001", "h"))),
            ("p-00008", &|_| None),
        ],
    );
    after.push(doc("d-00002", Some("f-00001"), "new", "New", "i"));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);

    let home = doc_row(&rv, "d-00001");
    assert_eq!(
        kinds(home),
        vec![
            ("q-00001".into(), ReviewChange::Moved),
            ("q-00002".into(), ReviewChange::Updated),
            ("q-00003".into(), ReviewChange::Removed),
            ("q-00005".into(), ReviewChange::Removed),
        ]
    );
    assert_eq!(
        home.counts,
        counts(|c| {
            c.removed = 2;
            c.moved = 1;
            c.updated = 1;
        })
    );
    let row = |id: &str| {
        let o = outside(&rv, id);
        (
            o.change,
            o.what.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    };
    assert_eq!(row("q-00001"), (Some(ReviewChange::Removed), vec![]));
    assert_eq!(row("q-00002"), (Some(ReviewChange::Removed), vec![]));
    assert_eq!(row("q-00003"), (Some(ReviewChange::Moved), vec![]));
    assert_eq!(
        row("q-00005"),
        (Some(ReviewChange::Updated), vec!["text", "place"])
    );
    assert_eq!(row("q-00006"), (Some(ReviewChange::Removed), vec![]));
    assert_eq!(row("q-00007"), (Some(ReviewChange::Moved), vec![]));
    assert_eq!(rv.review.added_docs[0].size.blocks, 1);
    assert_eq!(rv.review.removed_docs[0].size.blocks, 2);

    assert_eq!(
        rv.review.totals,
        totals(|t| {
            t.docs_changed = 1;
            t.docs_added = 1;
            t.docs_removed = 1;
            // the new doc and the block that moved into it
            t.blocks_added = 2;
            // the removed doc and the block that went with it; the one that left counts moved
            t.blocks_removed = 2;
            t.blocks_moved = 3;
            t.blocks_updated = 2;
        })
    );
}

/// D-0C4TB: a decision has no `moved` kind. A move across docs, or between a doc and outside
/// content, counts once in `decisions_removed` and once in `decisions_added`; a move inside one
/// doc counts nothing.
#[test]
fn drv4_decision_moves_count_removed_and_added() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-A0001", Some("f-00001"), "from", "From", "a"),
        prose("p-00001", "d-A0001", "a", "From words."),
        section("s-00001", "d-A0001", "b", "Part"),
        decision("D-00001", "d-A0001", "c", "Moves across docs."),
        decision("D-00002", "d-A0001", "d", "Moves outside."),
        decision("D-00004", "d-A0001", "e", "Moves inside its doc."),
        decision("D-00005", "d-A0001", "f", "Edited before."),
        doc("d-B0001", Some("f-00001"), "to", "To", "b"),
        prose("p-00002", "d-B0001", "a", "To words."),
        decision("D-00003", "f-00001", "c", "Moves in from outside."),
    ];
    let after = edit(
        &before,
        &[
            ("D-00001", &|b| Some(moved(b, "d-B0001", "b"))),
            ("D-00002", &|b| Some(moved(b, "f-00001", "d"))),
            ("D-00003", &|b| Some(moved(b, "d-B0001", "c"))),
            ("D-00004", &|b| Some(moved(b, "s-00001", "a"))),
            ("D-00005", &|b| {
                Some(with(b, "statement", s("Edited after.")))
            }),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);

    let from = doc_row(&rv, "d-A0001");
    assert!(from.changes.is_empty(), "{:?}", kinds(from));
    assert_eq!(
        from.counts,
        counts(|c| {
            c.decisions_removed = 2;
            c.decisions_updated = 1;
        })
    );
    assert!(!from.record_only);
    let to = doc_row(&rv, "d-B0001");
    assert_eq!(to.counts, counts(|c| c.decisions_added = 2));
    let change = |id: &str| outside(&rv, id).change;
    assert_eq!(change("D-00002"), Some(ReviewChange::Added));
    assert_eq!(change("D-00003"), Some(ReviewChange::Removed));

    assert_eq!(
        rv.review.totals,
        totals(|t| {
            t.docs_changed = 2;
            t.decisions_added = 3;
            t.decisions_removed = 3;
            t.decisions_updated = 1;
        })
    );
}

// ---- row limits -----------------------------------------------------------------------------

/// D-RDH1C: a first publication lists every selected doc; the report keeps the first 200 in
/// site order and counts the rest, the adapter keeps every row, and the totals cover all.
#[test]
fn drv4_included_rows_accept_200_and_omit_201() {
    let (_dir, r) = project();
    let st = r.read();
    for n in [200usize, 201] {
        let mut blocks = vec![];
        for i in 0..n {
            let id = format!("d-{i:05}");
            blocks.push(doc(
                &id,
                None,
                &format!("doc{i}"),
                &format!("Doc {i}"),
                &format!("a{i:05}"),
            ));
            blocks.push(prose(&format!("p-{i:05}"), &id, "a", "Words."));
        }
        let m = synth(&blocks);
        let a = publication::resolve_in_for_tests(&st, &m, publication::IMPLICIT_ORG)
            .expect("the candidate");
        let source = ReadPublicationSource::with_settings(&st, st.pack(), "Project", None);
        let removals = ReviewRemovals::of_sides(&source, &[&a]).expect("the removals");
        let side = ReaderReviewSide::new(&a, &removals);
        let all: Included = included(&side);
        assert_eq!(all.rows.len(), n);
        assert_eq!(all.omitted_rows, 0, "the model is complete");

        let report = ReviewProjection::Report.included(&all);
        assert_eq!(report.rows.len(), REPORT_ROWS, "{n}");
        assert_eq!(report.omitted_rows, (n - REPORT_ROWS) as u64, "{n}");
        assert_eq!(
            report.rows[..],
            all.rows[..REPORT_ROWS],
            "{n}: site order kept"
        );
        assert_eq!(
            (report.docs, report.folders, report.totals),
            (all.docs, all.folders, all.totals)
        );
        assert_eq!(report.totals.docs, n as u64);
        assert_eq!(report.totals.blocks, n as u64);

        let adapter = ReviewProjection::Adapter.included(&all);
        assert_eq!(adapter, all, "{n}: the adapter keeps every row");
    }
}

/// D-RDH1C: one doc lists at most 100 changes and counts the rest in `omitted_changes`; its
/// counts and the totals stay complete.
#[test]
fn drv4_change_cap_accepts_100_and_omits_101() {
    let (_dir, r) = project();
    let st = r.read();
    assert_eq!(CHANGES_MAX, 100);
    for n in [100usize, 101] {
        let rv = growing(&st, &[n]);
        let d = &rv.review.changed_docs[0];
        assert_eq!(kept(&rv), vec![(100, (n - 100) as u64)], "{n}");
        assert_eq!(d.counts, counts(|c| c.added = n as u64), "{n}");
        assert_first_rows(d, 0);
        assert_eq!(rv.review.totals.blocks_added, n as u64);
        assert_eq!(rv.review.totals.docs_changed, 1);
    }
}

/// D-RDH1C: the changed docs share 2,000 change rows. 2,000 fit whole; at 2,001 the even share
/// gives each doc 99, and the spare rows go one at a time to docs with rows left, in site order.
#[test]
fn drv4_global_cap_accepts_2000_and_shares_2001() {
    let (_dir, r) = project();
    let st = r.read();
    assert_eq!(CHANGE_BUDGET, 2000);
    let mut sizes = vec![100usize; 20];
    let rv = growing(&st, &sizes);
    assert_eq!(kept(&rv), vec![(100, 0); 20]);
    assert_eq!(rv.review.totals.blocks_added, 2000);
    for (n, d) in rv.review.changed_docs.iter().enumerate() {
        assert_first_rows(d, n);
    }

    sizes.push(1);
    let rv = growing(&st, &sizes);
    let mut want = vec![(100, 0); 19];
    want.push((99, 1));
    want.push((1, 0));
    assert_eq!(kept(&rv), want);
    let listed: usize = rv.review.changed_docs.iter().map(|d| d.changes.len()).sum();
    assert_eq!(listed, 2000);
    for (n, d) in rv.review.changed_docs.iter().enumerate() {
        assert_eq!(d.counts.added, sizes[n] as u64, "complete counts");
        assert_first_rows(d, n);
    }
    assert_eq!(
        rv.review.totals,
        totals(|t| {
            t.docs_changed = 21;
            t.blocks_added = 2001;
        })
    );
}

/// D-RDH1C: the largest even share `k` that fits, then the spare rows one at a time in site
/// order. A short doc keeps all its rows, and what it leaves goes to the others.
#[test]
fn drv4_even_share_redistributes_short_docs_and_orders_remainders() {
    // everything fits: each doc keeps its rows, up to 100
    assert_eq!(
        change_shares(&[5, 300, 100, 40, 2000]),
        vec![5, 100, 100, 40, 100]
    );
    assert_eq!(change_shares(&[]), Vec::<usize>::new());
    assert_eq!(change_shares(&[100; 20]), vec![100; 20]);
    // one short doc and 25 full ones: k = 79, and 20 spare rows to the first full docs
    let mut rows = vec![5usize];
    rows.extend([100; 25]);
    let mut want = vec![5usize];
    want.extend([80; 20]);
    want.extend([79; 5]);
    assert_eq!(change_shares(&rows), want);
    // short docs between full ones: k = 93, 17 spare rows in site order, skipping short docs
    let mut rows = vec![10usize];
    rows.extend([100; 10]);
    rows.push(20);
    rows.extend([100; 11]);
    let shares = change_shares(&rows);
    let mut want = vec![10usize];
    want.extend([94; 10]);
    want.push(20);
    want.extend([94; 7]);
    want.extend([93; 4]);
    assert_eq!(shares, want);
    assert_eq!(shares.iter().sum::<usize>(), 2000);
    // unequal docs near the share: a doc just over k takes a spare row like a full one
    let mut rows = vec![95usize, 300, 1];
    rows.extend([100; 20]);
    let shares = change_shares(&rows);
    // 1 + 22 * k <= 2000 gives k = 90, with 19 spare rows
    let mut want = vec![91usize, 91, 1];
    want.extend([91; 17]);
    want.extend([90; 3]);
    assert_eq!(shares, want);
    assert_eq!(shares.iter().sum::<usize>(), 2000);
}

/// D-RDH1C: the 2,000-row share covers every changed doc, the docs past the report's 200-row
/// doc list too.
#[test]
fn drv4_global_budget_includes_unlisted_docs() {
    let (_dir, r) = project();
    let st = r.read();
    let rv = growing(&st, &[10; 201]);
    // 201 docs of 10 rows: k = 9, and 191 spare rows to the first 191 docs
    let mut want = vec![(10, 0); 191];
    want.extend([(9, 1); 10]);
    assert_eq!(kept(&rv), want);
    let (listed, omitted) = ReviewProjection::Report.list(&rv.review.changed_docs);
    assert_eq!((listed.len(), omitted), (REPORT_ROWS, 1));
    // the listed docs leave the last doc its share
    assert_eq!(
        listed.iter().map(|d| d.changes.len()).sum::<usize>(),
        2000 - 9
    );
    assert_eq!(
        (listed[199].changes.len(), listed[199].omitted_changes),
        (9, 1)
    );
    let (all, none) = ReviewProjection::Adapter.list(&rv.review.changed_docs);
    assert_eq!((all.len(), none), (201, 0));
    assert_eq!(all[200].changes.len(), 9);
    assert_eq!(rv.review.totals.blocks_added, 2010);
    assert_eq!(rv.review.totals.docs_changed, 201);
}

/// D-RDH1C: each top-level list of the report keeps its first 200 rows and counts the rest;
/// the adapter keeps every outer row.
#[test]
fn drv4_outer_caps_accept_200_and_omit_201() {
    let (_dir, r) = project();
    let st = r.read();
    for n in [200usize, 201] {
        let mut before = vec![
            admin(),
            profile("pf-S1TE1", &["f-00001"], "mark"),
            folder("f-00001", None, "box", "Box", "a"),
            doc("d-X0001", None, "outside", "Outside", "b"),
        ];
        let mut after = before.clone();
        for i in 0..n {
            let o = format!("a{i:05}");
            // a removed doc with a figure
            let dr = format!("dr-{i:05}");
            before.push(doc(&dr, Some("f-00001"), &format!("r{i}"), "Removed", &o));
            before.push(figure(&format!("xr-{i:05}"), &dr, "a", "Gone", 1, None));
            // a changed doc whose figure changes its image
            let dc = format!("dc-{i:05}");
            let changed = doc(&dc, Some("f-00001"), &format!("c{i}"), "Changed", &o);
            before.push(changed.clone());
            after.push(changed);
            before.push(figure(&format!("xc-{i:05}"), &dc, "a", "Chart", 2, None));
            after.push(figure(&format!("xc-{i:05}"), &dc, "a", "Chart", 3, None));
            // an added doc with a figure, a missing reference, a dropped decision and a link
            // outside the selection
            let da = format!("da-{i:05}");
            let dropped = format!("D-{i:05}");
            after.push(doc(&da, Some("f-00001"), &format!("a{i}"), "Added", &o));
            after.push(figure(&format!("xa-{i:05}"), &da, "a", "New", 4, None));
            after.push(link(
                prose(
                    &format!("pa-{i:05}"),
                    &da,
                    "b",
                    &format!("See {{{{{dropped}}}}} and {{{{p-MSSNG}}}}."),
                ),
                "references",
                &dropped,
            ));
            for side in [&mut before, &mut after] {
                side.push(decision(&dropped, "d-X0001", &o, "Outside the selection."));
            }
        }
        let rv = review(&st, &before, &after, "site");
        let refs = &rv.after.selection().references;
        let dropped: Vec<BlockId> = refs.dropped_decisions.iter().cloned().collect();

        fn check<T: PartialEq + std::fmt::Debug>(name: &str, all: &[T], want: usize) {
            assert_eq!(all.len(), want, "{name}: the complete list");
            let (rows, omitted) = ReviewProjection::Report.list(all);
            assert_eq!(rows.len(), want.min(REPORT_ROWS), "{name}");
            assert_eq!(omitted, want.saturating_sub(REPORT_ROWS) as u64, "{name}");
            assert_eq!(rows, &all[..rows.len()], "{name}: the first rows, in order");
            let (rows, omitted) = ReviewProjection::Adapter.list(all);
            assert_eq!(
                (rows.len(), omitted),
                (want, 0),
                "{name}: the adapter keeps all"
            );
        }
        check("changed docs", &rv.review.changed_docs, n);
        check("added docs", &rv.review.added_docs, n);
        check("removed docs", &rv.review.removed_docs, n);
        check("reference rows", &refs.rows, n);
        check("links outside", &refs.links_outside, n);
        check("dropped decisions", &dropped, n);
        check("added attachments", &rv.diff.added_attachments, n);
        check("removed attachments", &rv.diff.removed_attachments, n);
        check("changed attachments", &rv.diff.changed_attachments, n);
        // each new missing reference and each new link outside the selection
        check("new references", &rv.diff.new_references, 2 * n);
        assert_eq!(rv.review.totals.new_references, 2 * n as u64);
        assert_eq!(rv.review.totals.docs_added, n as u64);
        assert_eq!(rv.review.totals.docs_removed, n as u64);
        assert_eq!(rv.review.totals.figures_changed, n as u64);
    }
}

// ---- byte limits ----------------------------------------------------------------------------

/// A one-member object whose compact JSON holds escaped and multi-byte characters and is
/// exactly `size` bytes.
fn sized(size: usize) -> serde_json::Value {
    let make = |pad: usize| json!({"text": format!("\u{1}\"é😀\n{}", "x".repeat(pad))});
    let base = compact_len(&make(0), usize::MAX).expect("a size");
    // {"text":"\u0001\"é😀\nx…"}: 11 bytes of frame, 6 + 2 + 2 + 4 + 2 escaped bytes
    assert_eq!(base, 11 + 16);
    make(size - base)
}

/// D-RDH1C: the report holds at most 8,388,608 bytes with the caller's reserve, and the adapter
/// review 134,217,728, in compact UTF-8 JSON with its escapes. At one byte over, the refusal is
/// the report's own words and no part of the value comes back.
#[test]
fn drv4_report_and_adapter_byte_caps_accept_exact_and_refuse_one_over() {
    assert_eq!(REPORT_BYTES, 8_388_608);
    assert_eq!(ADAPTER_REVIEW_BYTES, 134_217_728);
    let refused = |e: RdyError, max: usize| {
        assert_eq!(e.code, E_LIMIT);
        assert_eq!(
            e.message,
            format!("Publication report exceeds the limit of {max} bytes.")
        );
        assert_eq!(e.fix, "Reduce the selection or compare smaller profiles.");
    };

    // the report, with and without a reserve for what the MCP tool adds
    for reserve in [0usize, r#","warnings":[]"#.len() + 100] {
        let exact = sized(REPORT_BYTES - reserve);
        assert_eq!(
            serde_json::to_vec(&exact).unwrap().len() + reserve,
            REPORT_BYTES,
            "the measured size is the compact serialized size"
        );
        let back = ReviewProjection::Report
            .fit(exact.clone(), reserve)
            .expect("exactly the limit");
        assert_eq!(back, exact);
        let over = sized(REPORT_BYTES - reserve + 1);
        refused(
            ReviewProjection::Report
                .fit(over, reserve)
                .map(|_| ())
                .expect_err("refused"),
            REPORT_BYTES,
        );
    }
    // a reserve larger than the limit leaves no room
    refused(
        ReviewProjection::Report
            .fit(json!({}), REPORT_BYTES + 1)
            .map(|_| ())
            .expect_err("refused"),
        REPORT_BYTES,
    );

    // the adapter review: the bounded size check, the same seam
    let exact = sized(ADAPTER_REVIEW_BYTES);
    assert_eq!(compact_len(&exact, usize::MAX), Some(ADAPTER_REVIEW_BYTES));
    assert_eq!(
        compact_len(&exact, ADAPTER_REVIEW_BYTES - 1),
        None,
        "the check stops at the first byte over"
    );
    let exact = ReviewProjection::Adapter
        .fit(exact, 0)
        .expect("exactly the limit");
    drop(exact);
    let over = sized(ADAPTER_REVIEW_BYTES + 1);
    refused(
        ReviewProjection::Adapter
            .fit(over, 0)
            .map(|_| ())
            .expect_err("refused"),
        ADAPTER_REVIEW_BYTES,
    );
    // the report limit refuses what the adapter keeps
    refused(
        ReviewProjection::Report
            .fit(sized(REPORT_BYTES + 1), 0)
            .map(|_| ())
            .expect_err("refused"),
        REPORT_BYTES,
    );
    assert!(
        ReviewProjection::Adapter
            .fit(sized(REPORT_BYTES + 1), 0)
            .is_ok()
    );
}

/// D-RDH1C: the complete hidden rows hold every affected doc for the adapter; the typed lists
/// keep their 200-row cut, their counts and their order.
#[test]
fn drv4_complete_hidden_rows_do_not_change_existing_typed_lists() {
    let (_dir, r) = project();
    let st = r.read();
    for n in [200usize, 201] {
        let mut before = vec![
            admin(),
            profile("pf-S1TE1", &["f-00001"], "hide"),
            folder("f-00001", None, "box", "Box", "a"),
        ];
        let mut after = before.clone();
        for i in 0..n {
            let id = format!("d-{i:05}");
            let d = doc(
                &id,
                Some("f-00001"),
                &format!("doc{i}"),
                &format!("Doc {i}"),
                &format!("a{i:05}"),
            );
            let shown = prose(&format!("p-{i:05}"), &id, "a", "Shown.");
            before.extend([d.clone(), shown.clone()]);
            after.extend([
                d,
                shown,
                planned(prose(&format!("ph-{i:05}"), &id, "b", "Hidden.")),
            ]);
        }
        let (bm, am) = (synth(&before), synth(&after));
        let b = publication::resolve_in_for_tests(&st, &bm, "site").expect("the baseline");
        let a = publication::resolve_in_for_tests(&st, &am, "site").expect("the candidate");

        let full = a.hidden_planned.full();
        let rows = a.hidden_planned.full_rows();
        assert_eq!((full.docs, full.blocks), (n as u64, n as u64), "{n}");
        assert_eq!(full.rows.len(), REPORT_ROWS.min(n), "{n}");
        assert_eq!(
            full.omitted_rows,
            n.saturating_sub(REPORT_ROWS) as u64,
            "{n}"
        );
        assert_eq!(rows.len(), n, "{n}: every doc");
        assert_eq!(
            rows[..full.rows.len()],
            full.rows[..],
            "{n}: same rows, same order"
        );

        let diff = disclosure_diff(&b, &a).expect("the comparison");
        let newly = a.hidden_planned.newly(&b.hidden_planned);
        assert_eq!(
            diff.newly_hidden_planned, newly,
            "{n}: the typed list is unchanged"
        );
        let newly_rows = a.hidden_planned.newly_rows(&b.hidden_planned);
        assert_eq!(newly_rows.len(), n, "{n}");
        assert_eq!(newly_rows[..newly.rows.len()], newly.rows[..], "{n}");
        assert_eq!(newly.omitted_rows, n.saturating_sub(REPORT_ROWS) as u64);
        // nothing newly hidden against itself
        assert!(a.hidden_planned.newly_rows(&a.hidden_planned).is_empty());
    }
}
