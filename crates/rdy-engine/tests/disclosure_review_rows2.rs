//! The second set of review-row members (D-5WDC2, D-EGFYD, D-9PG33, D-62KA3, D-YKZNK): the
//! folder of changed and removed doc rows, one reading order of doc rows, the section ids and the
//! old section of change rows, outline rows with their counts, the flags that tell a removed
//! title from a missing one, and the count of dropped decisions. Each test builds its states in
//! memory over an empty snapshot.

use rdy_engine::publication::review_model::{
    ChangeRow, ChangedDocReview, DisclosureReview, DocSummary, OutlineRow, RemovedDocReview,
    ReviewChange,
};
use rdy_engine::publication::{
    self, CHANGES_MAX, DisclosureDiff, OUTLINE_MAX, PublicationReview, ReadPublicationSource,
    ReaderReviewSide, ReferenceLookup, Resolved, ReviewProjection, ReviewRemovals,
    changed_doc_reviews, disclosure_diff, disclosure_review, doc_reviews, dropped_decisions,
    included, reference_labels, reference_review, review_json,
};
use rdy_engine::{Materialized, Rdy, ReadState};
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, FieldValue, Hash, Link, StateSnapshot};
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

fn decision(id: &str, parent: &str, order: &str, statement: &str) -> Block {
    let b = with(
        blk(id, "decision", Some(parent), order),
        "status",
        s("accepted"),
    );
    with(b, "statement", s(statement))
}

fn admin() -> Block {
    doc("d-ADM1N", None, "admin", "Admin", "zz")
}

fn profile(id: &str, select: &[&str]) -> Block {
    let b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    let b = with(b, "key", s("site"));
    let b = with(b, "title", s("Profile title"));
    let b = with(b, "audience", s("org"));
    let b = with(b, "planned", s("mark"));
    with(
        b,
        "select",
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    )
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

/// `b` under `parent` at `order`.
fn moved(mut b: Block, parent: &str, order: &str) -> Block {
    b.parent = Some(bid(parent));
    b.order = order.into();
    b
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

/// Both sides resolved with `profile`, viewed with the comparison's removals, compared and
/// reviewed; with both resolutions, the candidate's state and its reference lookup.
struct Reviewed {
    before: ReaderReviewSide,
    after: ReaderReviewSide,
    baseline: Resolved,
    candidate: Resolved,
    state: Materialized,
    lookup: ReferenceLookup,
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
        lookup: ReferenceLookup::new(&am, &removals),
        baseline: b,
        candidate: a,
        state: am,
        diff,
        review,
    }
}

/// The one side of a first publication, viewed with its own removals.
fn first(st: &ReadState, blocks: &[Block], profile: &str) -> ReaderReviewSide {
    let m = synth(blocks);
    let a = publication::resolve_in_for_tests(st, &m, profile).expect("the candidate");
    let source = ReadPublicationSource::with_settings(st, st.pack(), "Project", None);
    let removals = ReviewRemovals::of_sides(&source, &[&a]).expect("the removals");
    ReaderReviewSide::new(&a, &removals)
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

fn changed<'a>(r: &'a Reviewed, id: &str) -> &'a ChangedDocReview {
    r.review
        .changed_docs
        .iter()
        .find(|d| d.id == bid(id))
        .unwrap_or_else(|| panic!("no changed row for {id}"))
}

fn added<'a>(rows: &'a [DocSummary], id: &str) -> &'a DocSummary {
    rows.iter()
        .find(|d| d.id == bid(id))
        .unwrap_or_else(|| panic!("no added row for {id}"))
}

fn gone<'a>(rows: &'a [RemovedDocReview], id: &str) -> &'a RemovedDocReview {
    rows.iter()
        .find(|d| d.id == bid(id))
        .unwrap_or_else(|| panic!("no removed row for {id}"))
}

fn row<'a>(d: &'a ChangedDocReview, id: &str) -> &'a ChangeRow {
    d.changes
        .iter()
        .find(|c| c.id == bid(id))
        .unwrap_or_else(|| panic!("no change row for {id} in {}", d.id))
}

fn outline(id: &str, label: &str, blocks: u64, changes: u64) -> OutlineRow {
    OutlineRow {
        id: bid(id),
        label: label.into(),
        blocks,
        changes,
    }
}

/// A change row's (section, section_id, before_section, before_section_id).
fn sections(c: &ChangeRow) -> (Option<&str>, Option<String>, Option<&str>, Option<String>) {
    (
        c.section.as_deref(),
        c.section_id.as_ref().map(ToString::to_string),
        c.before_section.as_deref(),
        c.before_section_id.as_ref().map(ToString::to_string),
    )
}

fn some(s: &str) -> Option<String> {
    Some(s.to_string())
}

// ---- folder (D-5WDC2) -----------------------------------------------------------------------

/// D-5WDC2, D-62KA3: a changed doc row names its top folder on the candidate's side and a
/// removed doc row on the baseline's side, as an added row does. A root doc's folder is null,
/// and no flag is set when no removal removed a title.
#[test]
fn rr2_changed_and_removed_rows_give_folder_from_their_side() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        folder("f-00001", None, "top", "Top", "a"),
        folder("f-00002", Some("f-00001"), "sub", "Sub", "a"),
        doc("d-00001", Some("f-00002"), "moves", "Moves", "a"),
        prose("p-00001", "d-00001", "a", "Moves words."),
        doc("d-00005", Some("f-00002"), "stays", "Stays", "b"),
        prose("p-00005", "d-00005", "a", "Stays words."),
        doc("d-00003", Some("f-00002"), "gone", "Gone", "c"),
        prose("p-00003", "d-00003", "a", "Gone words."),
        folder("f-00003", None, "other", "Other", "b"),
        doc("d-00002", None, "root", "Root", "c"),
        prose("p-00002", "d-00002", "a", "Root words."),
        doc("d-00004", None, "gone-root", "Gone root", "d"),
    ];
    let after = edit(
        &before,
        &[
            ("f-00001", &|b| Some(with(b, "title", s("Top renamed")))),
            ("d-00001", &|b| Some(moved(b, "f-00003", "a"))),
            ("p-00001", &|b| Some(body(b, "Moved words."))),
            ("p-00005", &|b| Some(body(b, "Stays, changed."))),
            ("p-00002", &|b| Some(body(b, "Root, changed."))),
            ("d-00003", &|_| None),
            ("p-00003", &|_| None),
            ("d-00004", &|_| None),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let folder_of = |d: &ChangedDocReview| (d.folder.clone(), d.folder_redacted);
    assert_eq!(folder_of(changed(&rv, "d-00001")), (some("Other"), false));
    assert_eq!(
        folder_of(changed(&rv, "d-00005")),
        (some("Top renamed"), false),
        "the candidate's title"
    );
    assert_eq!(folder_of(changed(&rv, "d-00002")), (None, false));
    let rem = &rv.review.removed_docs;
    let g = gone(rem, "d-00003");
    assert_eq!(
        (g.folder.clone(), g.folder_redacted),
        (some("Top"), false),
        "the baseline's title"
    );
    let g = gone(rem, "d-00004");
    assert_eq!((g.folder.clone(), g.folder_redacted), (None, false));

    // the members enter no hash: both sides keep the hashes their resolution recorded
    assert_eq!(rv.diff.after_selection_hash, rv.candidate.selection_hash);
    assert_eq!(rv.diff.before_selection_hash, rv.baseline.selection_hash);
}

// ---- position (D-EGFYD) ---------------------------------------------------------------------

/// D-EGFYD: every changed, added and removed doc row gives its place in one reading order: the
/// candidate's page order, each removed doc directly after the doc before it in the baseline's
/// order that the candidate still holds, one with no such doc first, and removed docs that share
/// a place in the baseline's order. Docs with no row take no place.
#[test]
fn rr2_positions_follow_one_reading_order_over_all_doc_rows() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-00009", None, "nine", "Nine", "0"),
        doc("d-00001", None, "one", "One", "a"),
        doc("d-00002", None, "two", "Two", "b"),
        doc("d-00003", None, "three", "Three", "c"),
        prose("p-00003", "d-00003", "a", "Three words."),
        doc("d-00004", None, "four", "Four", "d"),
        doc("d-00005", None, "five", "Five", "e"),
        doc("d-00006", None, "six", "Six", "f"),
    ];
    let mut after = edit(
        &before,
        &[
            ("d-00009", &|_| None),
            ("d-00002", &|_| None),
            ("d-00004", &|_| None),
            ("d-00005", &|_| None),
            ("d-00003", &|b| {
                let mut b = b;
                b.order = "g".into();
                Some(b)
            }),
            ("p-00003", &|b| Some(body(b, "Three, changed."))),
        ],
    );
    after.push(doc("d-00007", None, "seven", "Seven", "b5"));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let mut places: Vec<(u64, String)> = vec![];
    places.extend(
        rv.review
            .changed_docs
            .iter()
            .map(|d| (d.position, d.id.to_string())),
    );
    places.extend(
        rv.review
            .added_docs
            .iter()
            .map(|d| (d.position, d.id.to_string())),
    );
    places.extend(
        rv.review
            .removed_docs
            .iter()
            .map(|d| (d.position, d.id.to_string())),
    );
    places.sort();
    // d-00001 and d-00006 are unchanged: they anchor removed docs but take no place
    assert_eq!(
        places,
        vec![
            (0, "d-00009".to_string()),
            (1, "d-00002".to_string()),
            (2, "d-00007".to_string()),
            (3, "d-00003".to_string()),
            (4, "d-00004".to_string()),
            (5, "d-00005".to_string()),
        ]
    );
    // the builders that give each list alone give the same places
    let docs = doc_reviews(Some((&rv.diff, &rv.before)), &rv.after);
    assert_eq!(docs.added_docs, rv.review.added_docs);
    assert_eq!(docs.removed_docs, rv.review.removed_docs);
    let alone = changed_doc_reviews(&rv.diff, &rv.before, &rv.after);
    assert_eq!(alone[0].position, 3);
}

/// D-EGFYD: a position counts every doc row, also the rows the report's cut leaves out, so a
/// listed row keeps its place and the adapter's full list counts from 0 with no gap.
#[test]
fn rr2_positions_count_rows_the_report_leaves_out() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![doc("d-Z0000", None, "zero", "Zero", "a")];
    let mut after = before.clone();
    for i in 0..201 {
        after.push(doc(
            &format!("d-A{i:04}"),
            None,
            &format!("a{i}"),
            "Added",
            &format!("b{i:04}"),
        ));
    }
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let full = json_of(&st, &rv, ReviewProjection::Adapter);
    let cut = json_of(&st, &rv, ReviewProjection::Report);
    let places = |v: &Value| -> Vec<u64> {
        v["disclosure"]["added_docs"]
            .as_array()
            .expect("the rows")
            .iter()
            .map(|r| r["position"].as_u64().expect("a position"))
            .collect()
    };
    assert_eq!(places(&full), (0..201).collect::<Vec<u64>>());
    assert_eq!(places(&cut), (0..200).collect::<Vec<u64>>());
    assert_eq!(cut["disclosure"]["omitted"]["added_docs"], 1);
}

/// D-EGFYD, D-RDH1C: a first publication's rows take the added-doc form, so each gives its place
/// in the candidate's reading order and outline rows that count each block as a change.
#[test]
fn rr2_included_rows_take_positions_and_counted_outlines() {
    let (_dir, r) = project();
    let st = r.read();
    let blocks = vec![
        folder("f-00009", None, "zed", "Zed", "a"),
        doc("d-00009", Some("f-00009"), "nine", "Nine", "a"),
        section("s-00009", "d-00009", "a", "Nine part"),
        prose("p-00009", "s-00009", "a", "Nine words."),
        doc("d-00001", None, "one", "One", "b"),
    ];
    let side = first(&st, &blocks, publication::IMPLICIT_ORG);
    let inc = included(&side);
    let places: Vec<(String, u64)> = inc
        .rows
        .iter()
        .map(|d| (d.id.to_string(), d.position))
        .collect();
    assert_eq!(
        places,
        vec![("d-00009".to_string(), 0), ("d-00001".to_string(), 1)]
    );
    assert_eq!(
        inc.rows[0].outline,
        vec![outline("s-00009", "Nine part", 1, 1)]
    );
    assert_eq!(
        (inc.rows[0].folder.as_deref(), inc.rows[0].folder_redacted),
        (Some("Zed"), false)
    );
}

// ---- sections (D-EGFYD, D-9PG33) ------------------------------------------------------------

/// The two docs the section tests compare: `d-00001` with sections One, Two (holding Nested) and
/// Gone section, and `d-00002` with Away.
fn sectioned() -> (Vec<Block>, Vec<Block>) {
    let before = vec![
        doc("d-00001", None, "main", "Main", "a"),
        prose("p-00000", "d-00001", "a", "Lead words."),
        prose("p-00010", "d-00001", "a5", "Top words."),
        section("s-00001", "d-00001", "b", "One"),
        prose("p-00001", "s-00001", "a", "Stays in one."),
        prose("p-00002", "s-00001", "b", "Moves to two."),
        prose("p-00003", "s-00001", "c", "Moves and changes."),
        prose("p-00004", "s-00001", "d", "Removed from one."),
        section("s-00002", "d-00001", "c", "Two"),
        section("s-00003", "s-00002", "a", "Nested"),
        prose("p-00005", "s-00003", "a", "Deep words."),
        prose("p-00006", "s-00002", "b", "Two words."),
        section("s-00004", "d-00001", "d", "Gone section"),
        prose("p-00007", "s-00004", "a", "Gone with its section."),
        doc("d-00002", None, "other", "Other", "b"),
        section("s-00005", "d-00002", "a", "Away"),
        prose("p-00008", "s-00005", "a", "Crosses docs."),
        prose("p-00011", "s-00005", "b", "Stays away."),
    ];
    let mut after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(body(b, "Stays in one, changed."))),
            ("p-00002", &|b| Some(moved(b, "s-00002", "c"))),
            ("p-00003", &|b| {
                Some(body(moved(b, "s-00002", "d"), "Moved and changed."))
            }),
            ("p-00004", &|_| None),
            ("s-00004", &|_| None),
            ("p-00007", &|_| None),
            ("p-00008", &|b| Some(moved(b, "s-00003", "b"))),
            ("p-00010", &|b| Some(moved(b, "s-00002", "e"))),
        ],
    );
    after.push(prose("p-00009", "s-00001", "e", "New in one."));
    (before, after)
}

/// D-EGFYD, D-9PG33: each change row names its section by label and id. A moved row, and an
/// updated row with `place`, name the section that held the block on the baseline's side, also
/// from another doc; it is null before the first section and on every other row.
#[test]
fn rr2_change_rows_give_section_ids_and_the_section_a_block_left() {
    let (_dir, r) = project();
    let st = r.read();
    let (before, after) = sectioned();
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = changed(&rv, "d-00001");
    let r1 = row(d, "p-00001");
    assert_eq!(r1.change, ReviewChange::Updated);
    assert_eq!(r1.what, vec!["text"]);
    assert_eq!(sections(r1), (Some("One"), some("s-00001"), None, None));
    let r2 = row(d, "p-00002");
    assert_eq!(r2.change, ReviewChange::Moved);
    assert_eq!(
        sections(r2),
        (Some("Two"), some("s-00002"), Some("One"), some("s-00001"))
    );
    let r3 = row(d, "p-00003");
    assert_eq!(
        (r3.change, r3.what.clone()),
        (
            ReviewChange::Updated,
            vec!["text".to_string(), "place".to_string()]
        )
    );
    assert_eq!(
        sections(r3),
        (Some("Two"), some("s-00002"), Some("One"), some("s-00001"))
    );
    // a removed row reads its section on the baseline's side and left nothing
    let r4 = row(d, "p-00004");
    assert_eq!(r4.change, ReviewChange::Removed);
    assert_eq!(sections(r4), (Some("One"), some("s-00001"), None, None));
    let r7 = row(d, "p-00007");
    assert_eq!(
        sections(r7),
        (Some("Gone section"), some("s-00004"), None, None)
    );
    // a top-level section has no section of its own
    assert_eq!(sections(row(d, "s-00004")), (None, None, None, None));
    // a block from another doc names the section it left there
    let r8 = row(d, "p-00008");
    assert_eq!(r8.change, ReviewChange::Moved);
    assert_eq!(
        sections(r8),
        (
            Some("Nested"),
            some("s-00003"),
            Some("Away"),
            some("s-00005")
        )
    );
    // a block from before the first section left no section
    let r10 = row(d, "p-00010");
    assert_eq!(r10.change, ReviewChange::Moved);
    assert_eq!(sections(r10), (Some("Two"), some("s-00002"), None, None));
    let r9 = row(d, "p-00009");
    assert_eq!(r9.change, ReviewChange::Added);
    assert_eq!(sections(r9), (Some("One"), some("s-00001"), None, None));
    // the old doc lists the block that left as removed, in its old section
    let o = changed(&rv, "d-00002");
    assert_eq!(
        sections(row(o, "p-00008")),
        (Some("Away"), some("s-00005"), None, None)
    );
    // every id is null exactly where its label is
    for d in &rv.review.changed_docs {
        for c in &d.changes {
            assert_eq!(c.section.is_none(), c.section_id.is_none(), "{}", c.id);
            assert_eq!(
                c.before_section.is_none(),
                c.before_section_id.is_none(),
                "{}",
                c.id
            );
            let place = c.change == ReviewChange::Moved || c.what.iter().any(|w| w == "place");
            if !place {
                assert_eq!(c.before_section_id, None, "{}", c.id);
            }
        }
    }
}

/// D-EGFYD, D-9PG33: a changed doc's outline lists the candidate's sections in page order, each
/// with its id, its label, the shown blocks whose nearest section it is (a nested section counts
/// as one block of its parent) and the change rows whose block sits in it, matched by id: a
/// removed block by its baseline section. A block before the first section, and a removed block
/// whose section is gone, count in no row.
#[test]
fn rr2_changed_doc_outline_counts_blocks_and_changes_by_section_id() {
    let (_dir, r) = project();
    let st = r.read();
    let (before, after) = sectioned();
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = changed(&rv, "d-00001");
    assert_eq!(
        d.outline,
        vec![
            outline("s-00001", "One", 2, 3),
            outline("s-00002", "Two", 5, 3),
            outline("s-00003", "Nested", 2, 1),
        ]
    );
    assert_eq!(d.omitted_outline, 0);
    // the old doc's section still holds the row of the block that left it
    let o = changed(&rv, "d-00002");
    assert_eq!(o.outline, vec![outline("s-00005", "Away", 1, 1)]);
}

/// D-EGFYD: an added doc's outline counts each block of a section as a change, because each
/// block is added.
#[test]
fn rr2_added_doc_outline_counts_every_block_as_a_change() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![doc("d-00001", None, "kept", "Kept", "a")];
    let mut after = before.clone();
    after.extend([
        doc("d-00009", None, "new", "New", "b"),
        prose("p-00010", "d-00009", "a", "Lead words."),
        section("s-00011", "d-00009", "b", "Alpha"),
        prose("p-00011", "s-00011", "a", "Alpha words."),
        section("s-00012", "s-00011", "b", "Beta"),
        prose("p-00012", "s-00012", "a", "Beta words."),
        decision("D-00012", "s-00012", "b", "Beta decision."),
    ]);
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let a = added(&rv.review.added_docs, "d-00009");
    assert_eq!(
        a.outline,
        vec![
            outline("s-00011", "Alpha", 2, 2),
            outline("s-00012", "Beta", 1, 1),
        ]
    );
}

/// D-EGFYD: a changed doc lists at most 30 outline rows and counts the rest; an outline count
/// covers every change row, also the rows the change cap leaves out.
#[test]
fn rr2_changed_outline_cap_and_counts_cover_cut_rows() {
    assert_eq!((OUTLINE_MAX, CHANGES_MAX), (30, 100));
    let (_dir, r) = project();
    let st = r.read();
    let mut before = vec![doc("d-00001", None, "many", "Many", "a")];
    for i in 0..31 {
        before.push(section(
            &format!("s-{i:05}"),
            "d-00001",
            &format!("a{i:04}"),
            &format!("Part {i}"),
        ));
    }
    let mut after = before.clone();
    for i in 0..120 {
        after.push(prose(
            &format!("p-{i:05}"),
            "s-00000",
            &format!("a{i:04}"),
            &format!("New words {i}."),
        ));
    }
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = changed(&rv, "d-00001");
    assert_eq!((d.changes.len(), d.omitted_changes), (100, 20));
    assert_eq!((d.outline.len(), d.omitted_outline), (30, 1));
    assert_eq!(d.outline[0], outline("s-00000", "Part 0", 120, 120));
    assert_eq!(d.outline[29], outline("s-00029", "Part 29", 0, 0));
}

// ---- redactions (D-62KA3, D-9PG33, D-RDH1C) -------------------------------------------------

/// D-62KA3, D-9PG33, D-RDH1C: a folder title that a removal removed is null with
/// `folder_redacted` true on added, changed, removed and included rows, and the row's
/// `redacted` stays true. A removed section reads "Content removed." in an outline row, in a
/// change row's section and in its old section, and keeps its id. No removed word reaches a row.
#[test]
fn rr2_removed_titles_set_flags_and_never_leak() {
    let (_dir, r) = project();
    let st = r.read();
    let secret_folder = folder("f-00001", None, "secret", "Secret folder", "a");
    let secret_section = section("s-00001", "d-00001", "b", "Secret section");
    let before = vec![
        removed(&secret_folder),
        doc("d-00001", Some("f-00001"), "kept", "Kept doc", "a"),
        prose("p-00001", "d-00001", "a", "Plain words."),
        removed(&secret_section),
        prose("p-00004", "s-00001", "a", "Leaves the removed section."),
        section("s-00002", "d-00001", "c", "Open part"),
        prose("p-00003", "s-00002", "a", "Enters the removed section."),
        doc("d-00002", Some("f-00001"), "gone", "Gone doc", "b"),
        doc("d-00004", None, "root", "Root doc", "b"),
        prose("p-00005", "d-00004", "a", "Root words."),
    ];
    let mut after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(body(b, "Plain words, changed."))),
            ("p-00003", &|b| Some(moved(b, "s-00001", "b"))),
            ("p-00004", &|b| Some(moved(b, "s-00002", "b"))),
            ("d-00002", &|_| None),
            ("p-00005", &|b| Some(body(b, "Root words, changed."))),
        ],
    );
    after.push(doc("d-00003", Some("f-00001"), "new", "New doc", "c"));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);

    let kept = changed(&rv, "d-00001");
    assert_eq!((kept.folder.clone(), kept.folder_redacted), (None, true));
    let kept_diff = rv
        .diff
        .changed_docs
        .iter()
        .find(|d| d.id == bid("d-00001"))
        .expect("its comparison row");
    assert!(kept_diff.redacted, "the row's redacted flag stays true");
    let root = changed(&rv, "d-00004");
    assert_eq!((root.folder.clone(), root.folder_redacted), (None, false));
    let g = gone(&rv.review.removed_docs, "d-00002");
    assert_eq!((g.folder.clone(), g.folder_redacted), (None, true));
    let n = added(&rv.review.added_docs, "d-00003");
    assert_eq!((n.folder.clone(), n.folder_redacted), (None, true));

    assert_eq!(
        kept.outline,
        vec![
            outline("s-00001", CONTENT_REMOVED, 1, 1),
            outline("s-00002", "Open part", 1, 1),
        ]
    );
    assert_eq!(
        sections(row(kept, "p-00003")),
        (
            Some(CONTENT_REMOVED),
            some("s-00001"),
            Some("Open part"),
            some("s-00002")
        )
    );
    assert_eq!(
        sections(row(kept, "p-00004")),
        (
            Some("Open part"),
            some("s-00002"),
            Some(CONTENT_REMOVED),
            some("s-00001")
        )
    );

    let side = first(&st, &after, publication::IMPLICIT_ORG);
    let inc = included(&side);
    for id in ["d-00001", "d-00003"] {
        let row = added(&inc.rows, id);
        assert_eq!(
            (row.folder.clone(), row.folder_redacted),
            (None, true),
            "{id}"
        );
    }
    let root = added(&inc.rows, "d-00004");
    assert_eq!((root.folder.clone(), root.folder_redacted), (None, false));

    let words = format!(
        "{:?} {:?} {}",
        rv.review,
        inc,
        json_of(&st, &rv, ReviewProjection::Adapter)
    );
    assert!(!words.contains("Secret"), "{words}");
}

// ---- references (D-62KA3, D-YKZNK) ----------------------------------------------------------

/// D-62KA3: a reference row whose source sits in a doc whose title a removal removed gives a
/// null `source_doc_title` with `source_doc_redacted` true. A source outside docs gives null with
/// false, and a source in a titled doc gives its title with false. New reference rows read the
/// same way.
#[test]
fn rr2_reference_rows_tell_a_removed_doc_title_from_outside_docs() {
    let (_dir, r) = project();
    let st = r.read();
    let secret_doc = doc("d-00002", Some("f-00001"), "hidden", "Secret doc", "a");
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        removed(&secret_doc),
        prose("p-00001", "d-00002", "a", "Cites {{p-X0002}}."),
        doc("d-00005", Some("f-00001"), "plain", "Plain", "b"),
        prose("p-00005", "d-00005", "a", "No citation yet."),
        decision("D-00010", "f-00001", "c", "Per {{p-X0002}}."),
        doc("d-00004", None, "elsewhere", "Elsewhere", "b"),
        prose("p-X0002", "d-00004", "a", "Outside words."),
        profile("pf-S1TE1", &["f-00001"]),
    ];
    let after = edit(
        &before,
        &[("p-00005", &|b| Some(body(b, "Cites {{p-X0002}} now.")))],
    );
    let rv = review(&st, &before, &after, "pf-S1TE1");
    let report = &rv.candidate.references;
    let labels = reference_review(report, &rv.lookup);
    let flags: Vec<(String, Option<String>, bool)> = report
        .rows
        .iter()
        .zip(&labels.rows)
        .map(|(row, l)| {
            (
                row.source.to_string(),
                l.source_doc_title.clone(),
                l.source_doc_redacted,
            )
        })
        .collect();
    assert_eq!(
        flags,
        vec![
            ("D-00010".to_string(), None, false),
            ("p-00001".to_string(), None, true),
            ("p-00005".to_string(), some("Plain"), false),
        ]
    );
    let new = reference_labels(&rv.diff.new_references, &rv.lookup);
    assert_eq!(new.len(), 1);
    assert_eq!(
        (new[0].source_doc_title.clone(), new[0].source_doc_redacted),
        (some("Plain"), false)
    );
    let words = format!("{labels:?}");
    assert!(!words.contains("Secret"), "{words}");
}

/// D-YKZNK: `references.dropped_decisions` gives `count`, the number of dropped decisions over
/// every row: the report lists 200 and counts the rest, and the count still covers all; the
/// adapter keeps every row.
#[test]
fn rr2_dropped_decisions_count_every_decision_the_selection_cites() {
    let (_dir, r) = project();
    let st = r.read();
    for n in [3usize, 201] {
        let mut blocks = vec![
            admin(),
            doc("d-00001", None, "home", "Home", "a"),
            doc("d-X0001", None, "outside", "Outside", "b"),
            profile("pf-S1TE1", &["d-00001"]),
        ];
        for i in 0..n {
            let id = format!("D-{i:05}");
            blocks.push(link(
                prose(
                    &format!("p-{i:05}"),
                    "d-00001",
                    &format!("a{i:05}"),
                    &format!("See {{{{{id}}}}}."),
                ),
                "references",
                &id,
            ));
            blocks.push(decision(&id, "d-X0001", &format!("a{i:05}"), "Outside."));
        }
        let rv = review(&st, &blocks, &blocks, "pf-S1TE1");
        let report = &rv.candidate.references;
        assert_eq!(report.dropped_decisions.len(), n);
        let dropped = dropped_decisions(&report.dropped_decisions, &rv.lookup);
        assert_eq!((dropped.count, dropped.rows.len()), (n as u64, n));
        let cut = json_of(&st, &rv, ReviewProjection::Report);
        let full = json_of(&st, &rv, ReviewProjection::Adapter);
        for (v, listed) in [(&cut, n.min(200)), (&full, n)] {
            let d = &v["references"]["dropped_decisions"];
            assert_eq!(d["count"], json!(n), "{n}");
            assert_eq!(d["rows"].as_array().expect("rows").len(), listed, "{n}");
            assert_eq!(d["omitted_rows"], json!(n - listed), "{n}");
        }
    }
}

// ---- one JSON form --------------------------------------------------------------------------

/// D-5WDC2, D-EGFYD, D-9PG33, D-62KA3, D-YKZNK, D-RDH1C: the JSON form gives every new member
/// with its model value, in the report and in the adapter's projection alike.
#[test]
fn rr2_json_form_gives_every_new_member() {
    let (_dir, r) = project();
    let st = r.read();
    let (mut before, mut after) = sectioned();
    before.push(folder("f-00001", None, "box", "Box", "c"));
    before.push(doc("d-00003", Some("f-00001"), "gone", "Gone", "a"));
    after.push(folder("f-00001", None, "box", "Box", "c"));
    after.push(doc("d-00006", Some("f-00001"), "new", "New", "b"));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    for projection in [ReviewProjection::Report, ReviewProjection::Adapter] {
        let v = json_of(&st, &rv, projection);
        let d = &v["disclosure"];
        let main = d["changed_docs"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|r| r["id"] == "d-00001")
            .expect("d-00001");
        assert_eq!(main["folder"], Value::Null);
        assert_eq!(main["folder_redacted"], false);
        assert_eq!(main["position"], 0);
        assert_eq!(main["omitted_outline"], 0);
        assert_eq!(
            main["outline"][0],
            json!({"id": "s-00001", "label": "One", "blocks": 2, "changes": 3})
        );
        let r2 = main["changes"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|c| c["id"] == "p-00002")
            .expect("p-00002");
        assert_eq!(r2["section_id"], "s-00002");
        assert_eq!(r2["before_section"], "One");
        assert_eq!(r2["before_section_id"], "s-00001");
        let a = &d["added_docs"][0];
        assert_eq!(
            (
                &a["id"],
                &a["folder"],
                &a["folder_redacted"],
                &a["position"]
            ),
            (&json!("d-00006"), &json!("Box"), &json!(false), &json!(3))
        );
        // the removed doc stands directly after d-00002, the doc before it that the candidate
        // still holds, so before the added doc in the same folder
        let g = &d["removed_docs"][0];
        assert_eq!(
            (
                &g["id"],
                &g["folder"],
                &g["folder_redacted"],
                &g["position"]
            ),
            (&json!("d-00003"), &json!("Box"), &json!(false), &json!(2))
        );
        assert_eq!(v["references"]["dropped_decisions"]["count"], 0);
    }
}
