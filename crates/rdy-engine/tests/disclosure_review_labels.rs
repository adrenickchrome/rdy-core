//! The decision rows and the labels of the disclosure review (D-9BWMV, D-0C4TB, D-RDH1C,
//! D-BND1J, D-44NMT, D-7M0YF): decision rows in changed docs and outside docs, folder titles,
//! attachment labels, reference labels and dropped decisions. Each test builds its states in
//! memory over an empty snapshot.

use rdy_engine::publication::review_model::{
    AttachmentLabels, ChangedDocReview, DecisionRow, DisclosureReview, DisclosureTotals,
    FolderReview, OutsideReview, ReviewChange, ReviewCounts,
};
use rdy_engine::publication::{
    self, CheckStatus, DECISIONS_MAX, DisclosureDiff, PublicationReviewSide, ReadPublicationSource,
    ReaderReviewSide, ReferenceLookup, Resolved, ReviewRemovals, disclosure_diff,
    disclosure_review, doc_reviews, dropped_decisions, first_attachments, reference_labels,
    reference_review,
};
use rdy_engine::reader::text;
use rdy_engine::{Materialized, Rdy, ReadState};
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, FieldValue, Hash, Link, StateSnapshot};
use std::collections::{BTreeMap, BTreeSet};

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

fn titled(b: Block, title: &str) -> Block {
    with(b, "title", s(title))
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

/// The removed form of `b` that `version` names.
fn stub_of(b: &Block, version: Hash, request: Hash) -> Block {
    Block::redacted(
        b.id.clone(),
        b.block_type.clone(),
        b.parent.clone(),
        b.order.clone(),
        RedactedBlockIdentity {
            version,
            content_hash: b.content_hash(),
            frozen: false,
            request,
        },
    )
}

/// `b` read as its own removed version.
fn removed(b: &Block) -> Block {
    stub_of(b, b.version_hash(), Hash::of(b"a removal request"))
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

/// A reference row's (source, target, source label, source doc title, target label).
type LabelledRow = (String, String, String, Option<String>, Option<String>);

/// Both sides resolved with `profile`, viewed with the comparison's removals, compared and
/// reviewed; with the candidate's resolution and its reference lookup.
struct Reviewed {
    before: ReaderReviewSide,
    after: ReaderReviewSide,
    candidate: Resolved,
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
        candidate: a,
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

fn outside<'a>(r: &'a Reviewed, id: &str) -> &'a OutsideReview {
    r.review
        .changed_outside_docs
        .iter()
        .find(|o| o.id == bid(id))
        .unwrap_or_else(|| panic!("no outside row for {id}"))
}

/// Each decision row's (id, change, what).
fn decision_kinds(d: &ChangedDocReview) -> Vec<(String, ReviewChange, Vec<&str>)> {
    d.decisions
        .iter()
        .map(|r| {
            (
                r.id.to_string(),
                r.change,
                r.what.iter().map(String::as_str).collect(),
            )
        })
        .collect()
}

fn decision_row<'a>(d: &'a ChangedDocReview, id: &str) -> &'a DecisionRow {
    d.decisions
        .iter()
        .find(|r| r.id == bid(id))
        .unwrap_or_else(|| panic!("no decision row for {id} in {}", d.id))
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

fn status(side: &dyn PublicationReviewSide, id: &str) -> Option<String> {
    side.decision(&bid(id)).and_then(|d| d.status.clone())
}

fn folder_row<'a>(r: &'a Reviewed, id: &str) -> &'a FolderReview {
    let i = r
        .diff
        .changed_folders
        .iter()
        .position(|f| f.id == bid(id))
        .unwrap_or_else(|| panic!("no changed folder {id}"));
    assert_eq!(
        r.review.changed_folders.len(),
        r.diff.changed_folders.len(),
        "one folder review for each changed folder"
    );
    let row = &r.review.changed_folders[i];
    assert_eq!(row.id, bid(id));
    row
}

// ---- decisions ------------------------------------------------------------------------------

/// D-9BWMV: a decision row names its title, the changed parts of its title, statement and
/// derived status, its status change and an excerpt of its reader statement
/// (`reader::text::decision_statement`): a revocation reads its reason, a reference reads as its
/// target's words, a code span keeps its words, and a removed decision reads the notice.
#[test]
fn drv5_decisions_show_title_statement_and_derived_status() {
    let (_dir, r) = project();
    let st = r.read();
    let secret = decision("D-00006", "d-A0001", "f", "Secret plan.");
    let before = vec![
        doc("d-A0001", None, "home", "Home", "a"),
        prose("p-00001", "d-A0001", "a", "Words."),
        body(
            with(
                blk("t-00001", "term", Some("d-A0001"), "b"),
                "name",
                s("Wallet"),
            ),
            "A wallet.",
        ),
        titled(
            decision(
                "D-00001",
                "d-A0001",
                "c",
                "Keep `ledger` rows for {{t-00001}}.",
            ),
            "Keep rows",
        ),
        decision("D-00002", "d-A0001", "d", "The old way."),
        secret.clone(),
        doc("d-B0001", None, "away", "Away", "b"),
        prose("p-00002", "d-B0001", "a", "Away words."),
        decision("D-00005", "d-B0001", "b", "Ship the beta."),
    ];
    let edited_secret = with(secret.clone(), "statement", s("Secret plan, changed."));
    let mut after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(body(b, "Words, edited."))),
            ("D-00001", &|b| {
                Some(titled(
                    with(
                        b,
                        "statement",
                        s("Keep `ledger` rows for {{t-00001}} always."),
                    ),
                    "Keep all rows",
                ))
            }),
            ("D-00006", &|_| Some(removed(&edited_secret))),
        ],
    );
    after.push(link(
        decision("D-00003", "d-B0001", "c", "The new way."),
        "supersedes",
        "D-00002",
    ));
    after.push(link(
        decision(
            "D-00004",
            "d-A0001",
            "e",
            "Revoke D-00005: we no longer ship the beta.",
        ),
        "revokes",
        "D-00005",
    ));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");
    assert_eq!(
        decision_kinds(d),
        vec![
            (
                "D-00001".into(),
                ReviewChange::Updated,
                vec!["title", "statement"]
            ),
            ("D-00002".into(), ReviewChange::Updated, vec!["status"]),
            ("D-00004".into(), ReviewChange::Added, vec![]),
            (
                "D-00006".into(),
                ReviewChange::Updated,
                vec!["statement", "status"]
            ),
        ]
    );
    assert_eq!(d.omitted_decisions, 0);
    assert_eq!(
        d.counts,
        counts(|c| {
            c.updated = 1;
            c.decisions_added = 1;
            c.decisions_updated = 3;
        })
    );

    // the title and the reader statement: a reference reads as its target's words
    let d1 = decision_row(d, "D-00001");
    assert_eq!(d1.title, "Keep all rows");
    assert_eq!(
        d1.excerpt.as_deref(),
        Some("Keep ledger rows for Wallet always.")
    );
    assert_eq!(d1.status_from, d1.status_to);
    assert_eq!(d1.status_to, status(&rv.after, "D-00001"));
    assert!(d1.status_to.is_some());

    // the derived status changes with the superseding decision in another doc
    let d2 = decision_row(d, "D-00002");
    assert_eq!(d2.title, "The old way.");
    assert_eq!(d2.status_from, status(&rv.before, "D-00002"));
    assert_eq!(d2.status_to, status(&rv.after, "D-00002"));
    assert_ne!(d2.status_from, d2.status_to);
    assert!(d2.status_from.is_some() && d2.status_to.is_some());
    assert_eq!(d2.excerpt.as_deref(), Some("The old way."));

    // a revocation reads its reason, without the id it names
    let d4 = decision_row(d, "D-00004");
    assert_eq!(d4.excerpt.as_deref(), Some("We no longer ship the beta."));
    assert!(!d4.title.contains("D-00005"), "{}", d4.title);
    assert_eq!(d4.status_from, None);
    assert_eq!(d4.status_to, status(&rv.after, "D-00004"));

    // a removed version reads the notice and gives no excerpt
    let d6 = decision_row(d, "D-00006");
    assert_eq!(d6.title, CONTENT_REMOVED);
    assert_eq!(d6.excerpt, None);
    assert_eq!(d6.status_to, None);

    // every excerpt is the excerpt of decision_statement's reader text
    let stmt = |id: &str| {
        let b = rv.candidate.m.get(&bid(id)).expect("the decision");
        let revokes = b.links.get("revokes").is_some_and(|l| !l.is_empty());
        text::decision_statement(&rv.lookup, b, revokes)
    };
    assert_eq!(stmt("D-00004"), "We no longer ship the beta.");
    assert_eq!(
        publication::excerpt(&stmt("D-00001")),
        d1.excerpt,
        "the excerpt reads decision_statement"
    );
    assert_eq!(stmt("D-00006"), CONTENT_REMOVED);
    // the doc whose digest changed lists the revoked decision's status change too
    let b = doc_row(&rv, "d-B0001");
    assert_eq!(
        decision_kinds(b),
        vec![
            ("D-00005".into(), ReviewChange::Updated, vec!["status"]),
            ("D-00003".into(), ReviewChange::Added, vec![]),
        ]
    );
}

/// D-0C4TB, D-MCH70: a decision compares only its title, its authored statement and its derived
/// status. A rationale, a pin or another field alone gives no row.
#[test]
fn drv5_decisions_ignore_rationale_pins_and_unrelated_fields() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "why", "Why", "a"),
        prose("p-00001", "d-A0001", "a", "Words."),
        with(
            decision("D-00001", "d-A0001", "b", "Keep it."),
            "rationale",
            s("Because."),
        ),
        body(
            blk("r-00001", "rationale", Some("D-00001"), "a"),
            "The reason.",
        ),
        decision("D-00002", "d-A0001", "c", "Keep that."),
        folder("f-00001", None, "box", "Box", "b"),
        doc("d-B0001", Some("f-00001"), "inner", "Inner", "a"),
        with(
            decision("D-00003", "f-00001", "b", "Outside."),
            "rationale",
            s("Old."),
        ),
    ];
    let pin = Hash([0x44; 32]);
    let after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(body(b, "Words, edited."))),
            ("D-00001", &|b| {
                Some(with(b, "rationale", s("Because, more.")))
            }),
            ("r-00001", &|b| Some(body(b, "The reason, longer."))),
            ("D-00002", &|mut b| {
                b.pins.insert(bid("p-00001"), pin);
                Some(with(b, "note", s("An unrelated field.")))
            }),
            ("D-00003", &|b| Some(with(b, "rationale", s("New.")))),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");
    assert!(d.decisions.is_empty(), "{:?}", decision_kinds(d));
    assert_eq!(d.omitted_decisions, 0);
    assert_eq!(d.counts, counts(|c| c.updated = 1));
    let o = outside(&rv, "D-00003");
    assert_eq!((o.change, o.record_only), (None, true));
    assert_eq!(o.decision, None, "a record-only row has no decision change");
}

/// D-RDH1C: a changed doc lists at most 50 decision rows, apart from the change budget, and
/// counts every decision.
#[test]
fn drv5_decision_cap_accepts_50_and_omits_51() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "fifty", "Fifty", "a"),
        prose("p-00001", "d-A0001", "a", "Words."),
        doc("d-B0001", None, "more", "More", "b"),
        prose("p-00002", "d-B0001", "a", "Words."),
    ];
    let mut after = before.clone();
    for i in 0..DECISIONS_MAX {
        after.push(decision(
            &format!("D-A{i:04}"),
            "d-A0001",
            &format!("b{i:03}"),
            &format!("Choice {i}."),
        ));
    }
    for i in 0..=DECISIONS_MAX {
        after.push(decision(
            &format!("D-B{i:04}"),
            "d-B0001",
            &format!("b{i:03}"),
            &format!("Choice {i}."),
        ));
    }
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let a = doc_row(&rv, "d-A0001");
    assert_eq!((a.decisions.len(), a.omitted_decisions), (50, 0));
    assert_eq!(a.counts.decisions_added, 50);
    let b = doc_row(&rv, "d-B0001");
    assert_eq!((b.decisions.len(), b.omitted_decisions), (50, 1));
    assert_eq!(b.counts.decisions_added, 51, "the counts stay complete");
    assert_eq!(b.decisions[0].id, bid("D-B0000"));
    assert_eq!(b.decisions[49].id, bid("D-B0049"), "page order");
    assert!(b.changes.is_empty(), "decisions take no change rows");
    assert_eq!(rv.review.totals.decisions_added, 101);
}

/// D-BND1J: a row outside docs keeps every version change. A version change with no changed
/// part keeps its row with `change` null, an empty `what` and `record_only` true, and counts
/// nothing. A decision row also gets its decision fields.
#[test]
fn drv5_outside_doc_version_rows_keep_record_only_changes() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-00001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "Stays."),
        question("q-00001", "f-00001", "b", "First?"),
        question("q-00002", "f-00001", "c", "Second?"),
        decision("D-00001", "f-00001", "e", "Old statement."),
        with(
            decision("D-00003", "f-00001", "f", "Same statement."),
            "rationale",
            s("Because."),
        ),
    ];
    let mut after = edit(
        &before,
        &[
            // a new place among the questions only: siblings outside docs have no place part
            ("q-00001", &|mut b| {
                b.order = "d".into();
                Some(b)
            }),
            ("D-00001", &|b| {
                Some(with(b, "statement", s("New statement.")))
            }),
            ("D-00003", &|b| {
                Some(with(b, "rationale", s("Because, more.")))
            }),
        ],
    );
    after.push(titled(
        decision("D-00002", "f-00001", "h", "Added statement."),
        "Added choice",
    ));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    assert!(rv.review.changed_docs.is_empty());

    let q = outside(&rv, "q-00001");
    assert_eq!(q.block_type, "question");
    assert_eq!(q.label, "First?");
    assert_eq!((q.change, q.record_only), (None, true));
    assert!(q.what.is_empty());
    assert_eq!(q.decision, None);

    let d1 = outside(&rv, "D-00001");
    assert_eq!(d1.change, Some(ReviewChange::Updated));
    assert!(!d1.record_only);
    let row = d1.decision.as_ref().expect("decision fields");
    assert_eq!(row.change, ReviewChange::Updated);
    assert_eq!(row.what, vec!["statement"]);
    assert_eq!(row.title, "New statement.");
    assert_eq!(row.excerpt.as_deref(), Some("New statement."));
    assert_eq!(row.status_from, status(&rv.before, "D-00001"));
    assert_eq!(row.status_to, status(&rv.after, "D-00001"));

    let d2 = outside(&rv, "D-00002");
    assert_eq!(d2.change, Some(ReviewChange::Added));
    let row = d2.decision.as_ref().expect("decision fields");
    assert_eq!(row.title, "Added choice");
    assert!(row.what.is_empty());
    assert_eq!(row.status_from, None);
    assert_eq!(row.status_to, status(&rv.after, "D-00002"));

    let d3 = outside(&rv, "D-00003");
    assert_eq!((d3.change, d3.record_only), (None, true));
    assert!(d3.what.is_empty());
    assert_eq!(d3.decision, None);

    // the record-only rows count nothing
    assert_eq!(
        rv.review.totals,
        totals(|t| {
            t.decisions_updated = 1;
            t.decisions_added = 1;
        })
    );
}

/// D-9BWMV, D-0C4TB: a folder row gives the candidate's title and the baseline's when it
/// differs. An attachment row gives its figure's label and its home doc's title from the side
/// that holds it; a changed attachment gives both home doc titles.
#[test]
fn drv5_folder_and_attachment_labels_use_the_correct_side() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        folder("f-00001", None, "box", "Box", "a"),
        folder("f-00004", Some("f-00001"), "inner", "Inner", "a"),
        doc("d-00001", Some("f-00004"), "home", "Home", "a"),
        figure("g-00001", "d-00001", "a", "Chart", 1, None),
        figure("g-00002", "d-00001", "b", "Moving chart", 2, None),
        figure("g-00004", "d-00001", "c", "Old chart", 4, None),
        doc("d-00002", None, "away", "Away", "b"),
        prose("p-00002", "d-00002", "a", "Away words."),
    ];
    let mut after = edit(
        &before,
        &[
            ("f-00001", &|b| {
                Some(with(with(b, "title", s("Crate")), "name", s("crate")))
            }),
            ("d-00001", &|b| Some(with(b, "title", s("Start")))),
            ("g-00001", &|b| {
                Some(with(b, "blob", s(&hash(11).to_string())))
            }),
            ("g-00002", &|b| Some(moved(b, "d-00002", "b"))),
            ("g-00004", &|_| None),
        ],
    );
    after.push(folder("f-00002", None, "new", "New box", "c"));
    after.push(figure("g-00003", "d-00002", "c", "New chart", 3, Some(5)));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);

    let f1 = folder_row(&rv, "f-00001");
    assert_eq!(
        (f1.title.as_deref(), f1.before_title.as_deref()),
        (Some("Crate"), Some("Box"))
    );
    let f2 = folder_row(&rv, "f-00002");
    assert_eq!(
        (f2.title.as_deref(), f2.before_title.as_deref()),
        (Some("New box"), None)
    );
    // a new path alone: the same title, no before title
    let f4 = folder_row(&rv, "f-00004");
    assert_eq!(
        (f4.title.as_deref(), f4.before_title.as_deref()),
        (Some("Inner"), None)
    );

    let labels = |rows: &[AttachmentLabels]| -> Vec<(String, Option<String>)> {
        rows.iter()
            .map(|a| (a.label.clone(), a.home_doc_title.clone()))
            .collect()
    };
    assert_eq!(
        rv.review.added_attachments.len(),
        rv.diff.added_attachments.len()
    );
    assert_eq!(
        labels(&rv.review.added_attachments),
        vec![
            ("New chart".into(), Some("Away".into())),
            ("New chart".into(), Some("Away".into())),
        ],
        "light and dark each"
    );
    assert_eq!(
        labels(&rv.review.removed_attachments),
        vec![("Old chart".into(), Some("Home".into()))],
        "the baseline's labels"
    );
    let changed: Vec<(BlockId, &AttachmentLabels)> = rv
        .diff
        .changed_attachments
        .iter()
        .map(|c| c.block.clone())
        .zip(&rv.review.changed_attachments)
        .collect();
    assert_eq!(changed.len(), 2);
    assert_eq!(rv.review.changed_attachments.len(), 2);
    for (block, a) in changed {
        assert_eq!(a.home_doc_title, None);
        if block == bid("g-00001") {
            assert_eq!(a.label, "Chart");
            assert_eq!(a.before_home_doc_title.as_deref(), Some("Home"));
            assert_eq!(a.after_home_doc_title.as_deref(), Some("Start"));
        } else {
            assert_eq!(block, bid("g-00002"));
            assert_eq!(a.label, "Moving chart");
            assert_eq!(a.before_home_doc_title.as_deref(), Some("Home"));
            assert_eq!(a.after_home_doc_title.as_deref(), Some("Away"));
        }
    }
    for a in rv
        .review
        .added_attachments
        .iter()
        .chain(&rv.review.removed_attachments)
    {
        assert_eq!(
            (&a.before_home_doc_title, &a.after_home_doc_title),
            (&None, &None)
        );
    }

    // a first publication's attachments read the candidate, in the typed list's order
    let first = first_attachments(&rv.after);
    let typed = &rv.after.selection().attachments;
    assert_eq!(first.len(), typed.len());
    let got: Vec<(String, Option<String>)> = labels(&first);
    let want: Vec<(String, Option<String>)> = typed
        .iter()
        .map(|a| {
            let label = match a.block.to_string().as_str() {
                "g-00001" => "Chart",
                "g-00002" => "Moving chart",
                _ => "New chart",
            };
            let home = match a.home_doc.to_string().as_str() {
                "d-00001" => "Start",
                _ => "Away",
            };
            (label.to_string(), Some(home.to_string()))
        })
        .collect();
    assert_eq!(got, want);
}

/// D-44NMT, D-7M0YF: reference rows, link rows and new reference rows gain their source's
/// label, its doc's title and the target's label, read from the candidate source. The lookup
/// names an excluded target for the authorizer and never widens the reader view; a missing
/// target has no label, and a removed one reads the notice. Status, counts and the dropped
/// citations stay as they are.
#[test]
fn drv5_reference_labels_use_candidate_without_widening_selection() {
    let (_dir, r) = project();
    let st = r.read();
    let gone = prose("p-X0004", "d-00002", "c", "Gone words.");
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-00001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "No references yet."),
        link(
            prose("p-00002", "d-00001", "b", "Linked words."),
            "decided_by",
            "D-X0002",
        ),
        prose("p-00003", "d-00001", "c", "Cites {{D-X0003}}."),
        decision("D-00010", "f-00001", "b", "Per {{p-X0001}}."),
        doc("d-00002", None, "elsewhere", "Elsewhere", "b"),
        prose("p-X0001", "d-00002", "a", "Outside words."),
        titled(
            decision("D-X0002", "d-00002", "b", "Linked choice."),
            "Linked choice",
        ),
        titled(
            decision("D-X0003", "d-00002", "d", "Dropped choice."),
            "Dropped choice",
        ),
        removed(&gone),
        profile("pf-S1TE1", &["f-00001"], "mark"),
    ];
    let after = edit(
        &before,
        &[("p-00001", &|b| {
            Some(body(b, "See {{p-X0001}}, {{p-X0004}} and {{p-M1SS1}}."))
        })],
    );
    let rv = review(&st, &before, &after, "pf-S1TE1");
    let report = &rv.candidate.references;

    // the check itself does not change
    assert_eq!(report.status, CheckStatus::Warning);
    assert_eq!((report.excluded_count, report.missing_count), (3, 1));
    assert_eq!(report.blocked_count, 4);
    assert_eq!(report.links_outside_count, 1);
    assert_eq!(
        report.dropped_decisions,
        BTreeSet::from([bid("D-X0003")]),
        "the dropped citation is no row"
    );

    let review = reference_review(report, &rv.lookup);
    assert_eq!(review.rows.len(), report.rows.len());
    let rows: Vec<LabelledRow> = report
        .rows
        .iter()
        .zip(&review.rows)
        .map(|(row, l)| {
            (
                row.source.to_string(),
                row.target.clone(),
                l.source_label.clone(),
                l.source_doc_title.clone(),
                l.target_label.clone(),
            )
        })
        .collect();
    let p1 = rv.lookup.label(&bid("p-00001")).expect("a label");
    let d10 = rv.lookup.label(&bid("D-00010")).expect("a label");
    assert_eq!(
        rows,
        vec![
            (
                "D-00010".into(),
                "p-X0001".into(),
                d10.clone(),
                None,
                Some("Outside words.".into())
            ),
            (
                "p-00001".into(),
                "p-M1SS1".into(),
                p1.clone(),
                Some("Home".into()),
                None
            ),
            (
                "p-00001".into(),
                "p-X0001".into(),
                p1.clone(),
                Some("Home".into()),
                Some("Outside words.".into())
            ),
            (
                "p-00001".into(),
                "p-X0004".into(),
                p1.clone(),
                Some("Home".into()),
                Some(CONTENT_REMOVED.into())
            ),
        ]
    );
    assert!(!p1.contains("p-X0001") && !p1.is_empty(), "{p1}");
    assert!(!d10.is_empty());

    // the link row
    assert_eq!(review.links_outside.len(), 1);
    let l = &review.links_outside[0];
    assert_eq!(l.source_label, "Linked words.");
    assert_eq!(l.source_doc_title.as_deref(), Some("Home"));
    assert_eq!(l.target_label.as_deref(), Some("Linked choice"));

    // the new reference rows of the comparison read the same way
    let new = reference_labels(&rv.diff.new_references, &rv.lookup);
    let new_targets: Vec<(String, Option<String>)> = rv
        .diff
        .new_references
        .iter()
        .zip(&new)
        .map(|(row, l)| (row.target.clone(), l.target_label.clone()))
        .collect();
    assert_eq!(
        new_targets,
        vec![
            ("p-M1SS1".into(), None),
            ("p-X0001".into(), Some("Outside words.".into())),
            ("p-X0004".into(), Some(CONTENT_REMOVED.into())),
        ]
    );
    assert!(new.iter().all(|l| l.source_label == p1));

    // the dropped decision keeps its title row
    assert_eq!(
        review
            .dropped_decisions
            .rows
            .iter()
            .map(|d| (d.id.to_string(), d.title.clone()))
            .collect::<Vec<_>>(),
        vec![("D-X0003".to_string(), "Dropped choice".to_string())]
    );

    // the reader view stays the selection: no excluded target, and no word of one
    for x in ["p-X0001", "D-X0002", "D-X0003", "p-X0004", "d-00002"] {
        assert!(rv.after.shown(&bid(x)).is_none(), "{x} is outside");
    }
    let p1_text = rv.after.reader_text(&bid("p-00001")).expect("its text");
    assert!(!p1_text.contains("Outside words"), "{p1_text}");
    let p3_text = rv.after.reader_text(&bid("p-00003")).expect("its text");
    assert!(
        !p3_text.contains("Dropped choice") && !p3_text.contains("D-X0003"),
        "a dropped citation reads as nothing: {p3_text}"
    );
    // the change rows read the reader view, never the lookup
    let d = doc_row(&rv, "d-00001");
    for c in &d.changes {
        let words = format!("{} {:?} {:?}", c.label, c.excerpt, c.diff);
        assert!(!words.contains("Outside words"), "{words}");
    }
}

/// D-7M0YF, D-9BWMV: each dropped decision has a row with its title, in id order. The rows
/// count as neither excluded nor missing references.
#[test]
fn drv5_dropped_decisions_have_titles_in_id_order() {
    let (_dir, r) = project();
    let st = r.read();
    let blocks = vec![
        admin(),
        doc("d-00001", None, "home", "Home", "a"),
        prose(
            "p-00001",
            "d-00001",
            "a",
            "Cites {{D-X0009}}, {{D-X0001}} and {{D-X0005}}.",
        ),
        doc("d-00002", None, "elsewhere", "Elsewhere", "b"),
        titled(decision("D-X0009", "d-00002", "a", "Zed."), "Zed choice"),
        decision("D-X0001", "d-00002", "b", "Alpha choice."),
        titled(decision("D-X0005", "d-00002", "c", "Mid."), "Mid choice"),
        profile("pf-S1TE1", &["d-00001"], "mark"),
    ];
    let rv = review(&st, &blocks, &blocks, "pf-S1TE1");
    let report = &rv.candidate.references;
    assert_eq!((report.excluded_count, report.missing_count), (0, 0));
    assert!(report.rows.is_empty());
    assert_eq!(report.status, CheckStatus::Clear);
    let dropped = dropped_decisions(&report.dropped_decisions, &rv.lookup);
    assert_eq!(dropped.omitted_rows, 0);
    assert_eq!(
        dropped
            .rows
            .iter()
            .map(|d| (d.id.to_string(), d.title.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("D-X0001".to_string(), "Alpha choice."),
            ("D-X0005".to_string(), "Mid choice"),
            ("D-X0009".to_string(), "Zed choice"),
        ]
    );
    assert_eq!(
        reference_review(report, &rv.lookup).dropped_decisions,
        dropped
    );
}

/// D-9BWMV, D-RDH1C: a removed decision's title reads "Content removed." and it has no
/// excerpt, in a decision row, a decision title list and a dropped decision row. A doc title
/// that a removal removed is null.
#[test]
fn drv5_redacted_decisions_say_content_removed_and_doc_titles_are_null() {
    let (_dir, r) = project();
    let st = r.read();
    let secret_doc = doc("d-RED01", Some("f-00001"), "secret", "Secret doc", "b");
    let secret = decision("D-00001", "d-A0001", "b", "Secret choice.");
    let gone = titled(
        decision("D-X0001", "d-00002", "a", "Secret dropped."),
        "Secret dropped",
    );
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-A0001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-A0001", "a", "Cites {{D-X0001}}."),
        secret.clone(),
        removed(&secret_doc),
        prose("p-R0001", "d-RED01", "a", "Under a removed doc."),
        doc("d-00002", None, "elsewhere", "Elsewhere", "c"),
        removed(&gone),
        profile("pf-S1TE1", &["f-00001"], "mark"),
    ];
    let edited = with(secret.clone(), "statement", s("Secret choice, edited."));
    let mut after = edit(&before, &[("D-00001", &|_| Some(removed(&edited)))]);
    after.push(decision("D-00002", "d-RED01", "b", "Visible choice."));
    let hidden = decision("D-00003", "d-N0001", "a", "Secret new.");
    after.push(doc("d-N0001", Some("f-00001"), "fresh", "Fresh", "d"));
    after.push(removed(&hidden));
    let rv = review(&st, &before, &after, "pf-S1TE1");

    let d = doc_row(&rv, "d-A0001");
    let row = decision_row(d, "D-00001");
    assert_eq!(row.change, ReviewChange::Updated);
    assert_eq!(row.title, CONTENT_REMOVED);
    assert_eq!(row.excerpt, None);

    let red = doc_row(&rv, "d-RED01");
    assert_eq!(
        (red.title.as_ref(), red.before_title.as_ref()),
        (None, None)
    );
    let visible = decision_row(red, "D-00002");
    assert_eq!(visible.title, "Visible choice.");
    assert_eq!(visible.excerpt.as_deref(), Some("Visible choice."));

    let fresh = rv
        .review
        .added_docs
        .iter()
        .find(|x| x.id == bid("d-N0001"))
        .expect("the added doc");
    assert_eq!(fresh.decision_titles, vec![CONTENT_REMOVED.to_string()]);

    let dropped = dropped_decisions(&rv.candidate.references.dropped_decisions, &rv.lookup);
    assert_eq!(dropped.rows.len(), 1);
    assert_eq!(dropped.rows[0].title, CONTENT_REMOVED);

    // no removed word reaches a decision row or a title
    let mut words = String::new();
    for x in &rv.review.changed_docs {
        words.push_str(&format!(
            "{:?} {:?} {:?}",
            x.title, x.before_title, x.decisions
        ));
    }
    words.push_str(&format!("{:?} {:?}", rv.review.added_docs, dropped));
    assert!(!words.contains("Secret"), "{words}");
}

/// D-0C4TB: a decision that moves to another doc has a removed row in its old doc and an added
/// row in its new doc, and the totals count both. A move inside one doc gives no row.
#[test]
fn drv5_decision_move_across_docs_has_two_rows_and_both_totals() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "from", "From", "a"),
        section("s-00001", "d-A0001", "a", "Part"),
        prose("p-00001", "d-A0001", "b", "Words."),
        titled(decision("D-00001", "d-A0001", "c", "Moves away."), "Mover"),
        decision("D-00002", "d-A0001", "d", "Moves inside."),
        doc("d-B0001", None, "to", "To", "b"),
        prose("p-00002", "d-B0001", "a", "Other words."),
    ];
    let after = edit(
        &before,
        &[
            ("D-00001", &|b| Some(moved(b, "d-B0001", "b"))),
            ("D-00002", &|b| Some(moved(b, "s-00001", "a"))),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let a = doc_row(&rv, "d-A0001");
    assert_eq!(
        decision_kinds(a),
        vec![("D-00001".into(), ReviewChange::Removed, vec![])]
    );
    let gone = decision_row(a, "D-00001");
    assert_eq!(gone.title, "Mover");
    assert_eq!(gone.status_from, status(&rv.before, "D-00001"));
    assert_eq!(gone.status_to, None);
    assert_eq!(gone.excerpt.as_deref(), Some("Moves away."));
    assert_eq!(a.counts.decisions_removed, 1);
    assert_eq!(a.counts.decisions_added, 0);

    let b = doc_row(&rv, "d-B0001");
    assert_eq!(
        decision_kinds(b),
        vec![("D-00001".into(), ReviewChange::Added, vec![])]
    );
    let came = decision_row(b, "D-00001");
    assert_eq!(came.status_from, None);
    assert_eq!(came.status_to, status(&rv.after, "D-00001"));
    assert_eq!(b.counts.decisions_added, 1);

    assert_eq!(rv.review.totals.decisions_added, 1);
    assert_eq!(rv.review.totals.decisions_removed, 1);
    assert_eq!(rv.review.totals.decisions_updated, 0);
}

/// D-0C4TB: a decision that moves between a doc and the content outside docs has a removed row
/// in its old place and an added row in its new one, and the totals count both.
#[test]
fn drv5_decision_moves_to_and_from_outside_have_two_rows_and_both_totals() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-00001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "Words."),
        decision("D-00001", "d-00001", "b", "Leaves the doc."),
        decision("D-00002", "f-00001", "b", "Enters the doc."),
    ];
    let after = edit(
        &before,
        &[
            ("D-00001", &|b| Some(moved(b, "f-00001", "c"))),
            ("D-00002", &|b| Some(moved(b, "d-00001", "c"))),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-00001");
    assert_eq!(
        decision_kinds(d),
        vec![
            ("D-00002".into(), ReviewChange::Added, vec![]),
            ("D-00001".into(), ReviewChange::Removed, vec![]),
        ]
    );
    let out = outside(&rv, "D-00001");
    assert_eq!(out.change, Some(ReviewChange::Added));
    let row = out.decision.as_ref().expect("decision fields");
    assert_eq!(row.change, ReviewChange::Added);
    assert_eq!((row.status_from.as_ref(), row.what.len()), (None, 0));
    assert_eq!(row.title, "Leaves the doc.");

    let back = outside(&rv, "D-00002");
    assert_eq!(back.change, Some(ReviewChange::Removed));
    let row = back.decision.as_ref().expect("decision fields");
    assert_eq!(row.change, ReviewChange::Removed);
    assert_eq!(row.status_to, None);
    assert_eq!(row.status_from, status(&rv.before, "D-00002"));
    assert_eq!(row.excerpt.as_deref(), Some("Enters the doc."));

    assert_eq!(rv.review.totals.decisions_added, 2);
    assert_eq!(rv.review.totals.decisions_removed, 2);
}

/// D-0C4TB: an untitled decision is named by the label rule over its statement, never by its
/// id: in a decision row, a row outside docs, an added doc's decision titles and a dropped
/// decision row.
#[test]
fn drv5_untitled_decision_uses_statement_label_everywhere() {
    let (_dir, r) = project();
    let st = r.read();
    let long = "Use `small` fixtures for every rule that a task pins in its tests.";
    let want = "Use `small` fixtures for every rule that a\u{2026}";
    let before = vec![
        admin(),
        folder("f-00001", None, "box", "Box", "a"),
        doc("d-00001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "Cites {{D-X0001}}."),
        doc("d-X0000", None, "elsewhere", "Elsewhere", "b"),
        decision("D-X0001", "d-X0000", "a", long),
        profile("pf-S1TE1", &["f-00001"], "mark"),
    ];
    let mut after = before.clone();
    after.extend([
        decision("D-00001", "d-00001", "b", long),
        decision("D-00002", "f-00001", "b", long),
        doc("d-00003", Some("f-00001"), "fresh", "Fresh", "c"),
        decision("D-00003", "d-00003", "a", long),
    ]);
    let rv = review(&st, &before, &after, "pf-S1TE1");
    let d = doc_row(&rv, "d-00001");
    assert_eq!(decision_row(d, "D-00001").title, want);
    let o = outside(&rv, "D-00002");
    assert_eq!(o.decision.as_ref().expect("decision fields").title, want);
    let fresh = rv
        .review
        .added_docs
        .iter()
        .find(|x| x.id == bid("d-00003"))
        .expect("the added doc");
    assert_eq!(fresh.decision_titles, vec![want.to_string()]);
    let first = doc_reviews(None, &rv.after)
        .included
        .expect("a first review");
    let row = first
        .rows
        .iter()
        .find(|x| x.id == bid("d-00003"))
        .expect("an included row");
    assert_eq!(row.decision_titles, vec![want.to_string()]);
    let dropped = dropped_decisions(&rv.candidate.references.dropped_decisions, &rv.lookup);
    assert_eq!(
        dropped
            .rows
            .iter()
            .map(|x| (x.id.to_string(), x.title.as_str()))
            .collect::<Vec<_>>(),
        vec![("D-X0001".to_string(), want)]
    );
}

/// D-0C4TB: a removed folder gives the baseline's title.
#[test]
fn drv5_removed_folder_uses_baseline_title() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        folder("f-00001", None, "keep", "Keep", "a"),
        doc("d-00001", Some("f-00001"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "Words."),
        folder("f-00002", None, "old", "Old box", "b"),
        doc("d-00002", Some("f-00002"), "gone", "Gone", "a"),
        prose("p-00002", "d-00002", "a", "Gone words."),
    ];
    let after = edit(
        &before,
        &[
            ("f-00002", &|_| None),
            ("d-00002", &|_| None),
            ("p-00002", &|_| None),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let f = folder_row(&rv, "f-00002");
    assert_eq!(
        (f.title.as_deref(), f.before_title.as_deref()),
        (Some("Old box"), None)
    );
}

/// D-9BWMV, D-RDH1C: a folder title that a removal removed is null, and the comparison row is
/// marked removed. A folder below it keeps its own title.
#[test]
fn drv5_redacted_changed_folder_title_is_null() {
    let (_dir, r) = project();
    let st = r.read();
    let outer = folder("f-00001", None, "box", "Box", "a");
    let before = vec![
        outer.clone(),
        folder("f-00002", Some("f-00001"), "inner", "Inner", "a"),
        doc("d-00001", Some("f-00002"), "home", "Home", "a"),
        prose("p-00001", "d-00001", "a", "Words."),
    ];
    let edited = with(outer.clone(), "title", s("Secret box"));
    let after = edit(&before, &[("f-00001", &|_| Some(removed(&edited)))]);
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let f = folder_row(&rv, "f-00001");
    assert_eq!(f.title, None, "a removed title is null");
    assert_eq!(f.before_title.as_deref(), Some("Box"));
    let raw = rv
        .diff
        .changed_folders
        .iter()
        .find(|x| x.id == bid("f-00001"))
        .expect("the comparison row");
    assert!(raw.redacted);
    let inner = folder_row(&rv, "f-00002");
    assert_eq!(
        (inner.title.as_deref(), inner.before_title.as_deref()),
        (Some("Inner"), None),
        "a surviving title stays"
    );
    assert!(!format!("{:?}", rv.review.changed_folders).contains("Secret"));
}

/// D-0C4TB: an added or removed decision has an empty `what` and a null status for the side
/// that lacks it. A move inside one doc changes none of the three parts and gives no row.
#[test]
fn drv5_decision_edge_forms_follow_the_ruling() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "home", "Home", "a"),
        section("s-00001", "d-A0001", "a", "First"),
        section("s-00002", "d-A0001", "b", "Second"),
        decision("D-00001", "s-00001", "a", "Gets removed."),
        decision("D-00002", "s-00001", "b", "Moves to the second part."),
        decision("D-00003", "s-00002", "a", "Moves first."),
        decision("D-00005", "s-00002", "b", "Stays last."),
    ];
    let mut after = edit(
        &before,
        &[
            ("D-00001", &|_| None),
            ("D-00002", &|b| Some(moved(b, "s-00002", "c"))),
            ("D-00003", &|b| Some(moved(b, "s-00002", "c0"))),
        ],
    );
    after.push(decision("D-00004", "s-00002", "d", "Gets added."));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");
    assert_eq!(
        decision_kinds(d),
        vec![
            ("D-00004".into(), ReviewChange::Added, vec![]),
            ("D-00001".into(), ReviewChange::Removed, vec![]),
        ]
    );
    let added = decision_row(d, "D-00004");
    assert_eq!(added.status_from, None);
    assert!(added.status_to.is_some());
    assert_eq!(added.excerpt.as_deref(), Some("Gets added."));
    let gone = decision_row(d, "D-00001");
    assert_eq!(gone.status_to, None);
    assert!(gone.status_from.is_some());
    assert_eq!(gone.title, "Gets removed.");
    assert_eq!(
        d.counts,
        counts(|c| {
            c.decisions_added = 1;
            c.decisions_removed = 1;
        })
    );
    // the row of every decision has an empty what unless it is updated
    for r in &d.decisions {
        assert_eq!(
            r.what.is_empty(),
            r.change != ReviewChange::Updated,
            "{}",
            r.id
        );
    }
}
