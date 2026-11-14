//! The changed-doc rows of the disclosure review (D-RDH1C, D-MCH70, D-0C4TB, D-Z8Q4N): titles,
//! pages, counts, change rows in page order, excerpts and short word diffs, read from two
//! reader views of one store. Each test builds its states in memory over an empty snapshot.

use rdy_engine::publication::review_model::{
    ChangeRow, ChangedDocReview, DiffOp, DiffPiece, ReviewChange, ReviewCounts,
};
use rdy_engine::publication::{
    self, DIFF_MAX, EXCERPT_MAX, PublicationReviewSide, ReadPublicationSource, ReaderReviewSide,
    ReviewRemovals, changed_doc_reviews, disclosure_diff, excerpt, short_diff,
};
use rdy_engine::reader::inline::{CODE_CLOSE, CODE_OPEN};
use rdy_engine::{Materialized, Rdy, ReadState};
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, FieldValue, Hash, Link, StateSnapshot};
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

/// Both sides resolved with `profile`, viewed with the comparison's removals, and compared.
struct Reviewed {
    before: ReaderReviewSide,
    after: ReaderReviewSide,
    changed: Vec<BlockId>,
    docs: Vec<ChangedDocReview>,
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
    let docs = changed_doc_reviews(&diff, &bv, &av);
    Reviewed {
        before: bv,
        after: av,
        changed: diff.changed_docs.iter().map(|d| d.id.clone()).collect(),
        docs,
    }
}

fn doc_row<'a>(r: &'a Reviewed, id: &str) -> &'a ChangedDocReview {
    r.docs
        .iter()
        .find(|d| d.id == bid(id))
        .unwrap_or_else(|| panic!("no row for {id}: {:?}", ids(&r.docs)))
}

fn ids(docs: &[ChangedDocReview]) -> Vec<String> {
    docs.iter().map(|d| d.id.to_string()).collect()
}

fn row<'a>(d: &'a ChangedDocReview, id: &str) -> &'a ChangeRow {
    d.changes
        .iter()
        .find(|c| c.id == bid(id))
        .unwrap_or_else(|| panic!("no change row for {id} in {}: {:?}", d.id, kinds(d)))
}

fn kinds(d: &ChangedDocReview) -> Vec<(String, ReviewChange)> {
    d.changes
        .iter()
        .map(|c| (c.id.to_string(), c.change))
        .collect()
}

fn what(r: &ChangeRow) -> Vec<&str> {
    r.what.iter().map(String::as_str).collect()
}

fn scalars(t: &str) -> usize {
    t.chars().count()
}

fn piece(op: DiffOp, text: &str) -> DiffPiece {
    DiffPiece {
        op,
        text: text.into(),
    }
}

fn pieces_len(p: &[DiffPiece]) -> usize {
    p.iter().map(|x| scalars(&x.text)).sum()
}

fn counts(f: impl FnOnce(&mut ReviewCounts)) -> ReviewCounts {
    let mut c = ReviewCounts::default();
    f(&mut c);
    c
}

/// Every row with a change: `what` is not empty exactly for an update (D-MCH70, D-RDH1C).
fn assert_what_rule(r: &Reviewed) {
    for d in &r.docs {
        for c in &d.changes {
            assert_eq!(
                c.change == ReviewChange::Updated,
                !c.what.is_empty(),
                "{} in {}: {:?} {:?}",
                c.id,
                d.id,
                c.change,
                c.what
            );
        }
    }
}

// ---- tests ----------------------------------------------------------------------------------

/// D-RDH1C: a changed doc names its candidate title and page, the baseline's title when it
/// differs, and the baseline's page. Rows follow the candidate's reading order, never id order,
/// and a doc with no page keeps its row and its place (D-0C4TB).
#[test]
fn drv2_changed_docs_have_titles_pages_and_before_names() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        folder("f-GD001", None, "guide", "Guide", "b"),
        doc("d-A0001", Some("f-GD001"), "intro", "Intro", "a"),
        prose("p-A0001", "d-A0001", "a", "Hello."),
        doc("d-B0001", Some("f-GD001"), "second", "Second", "b"),
        prose("p-B0001", "d-B0001", "a", "Body one."),
        doc("d-C0001", None, "third", "Third", "a"),
        prose("p-C0001", "d-C0001", "a", "Third body."),
        doc("d-G0001", None, "glossary", "Glossary", "c"),
        prose("p-G0001", "d-G0001", "a", "Terms."),
    ];
    let after = edit(
        &before,
        &[
            ("d-A0001", &|b| Some(with(b, "title", s("Introduction")))),
            ("p-B0001", &|b| Some(body(b, "Body two."))),
            ("d-C0001", &|b| Some(with(b, "name", s("renamed")))),
            ("p-G0001", &|b| Some(body(b, "Terms, edited."))),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    assert_eq!(
        ids(&rv.docs),
        vec!["d-C0001", "d-A0001", "d-B0001", "d-G0001"],
        "candidate reading order"
    );

    let a = doc_row(&rv, "d-A0001");
    assert_eq!(a.title.as_deref(), Some("Introduction"));
    assert_eq!(a.before_title.as_deref(), Some("Intro"));
    assert_eq!(a.page.as_deref(), Some("guide/intro.html"));
    assert_eq!(a.before_page.as_deref(), Some("guide/intro.html"));
    assert_eq!(kinds(a), vec![("d-A0001".into(), ReviewChange::Updated)]);
    let own = row(a, "d-A0001");
    assert_eq!(
        (own.block_type.as_str(), own.label.as_str(), what(own)),
        ("doc", "Introduction", vec!["title"])
    );
    assert_eq!(own.section, None);
    assert_eq!(own.diff, None, "a doc has no text part");
    assert_eq!(a.counts, counts(|c| c.updated = 1));
    assert!(!a.record_only);
    assert!(a.decisions.is_empty());
    assert_eq!((a.omitted_changes, a.omitted_decisions), (0, 0));

    let b = doc_row(&rv, "d-B0001");
    assert_eq!(b.title.as_deref(), Some("Second"));
    assert_eq!(b.before_title, None, "the same title");
    assert_eq!(b.page.as_deref(), Some("guide/second.html"));
    assert_eq!(b.before_page.as_deref(), Some("guide/second.html"));
    assert_eq!(kinds(b), vec![("p-B0001".into(), ReviewChange::Updated)]);
    assert_eq!(row(b, "p-B0001").label, "Body two.");

    let c = doc_row(&rv, "d-C0001");
    assert_eq!(c.title.as_deref(), Some("Third"));
    assert_eq!(c.before_title, None);
    assert_eq!(c.page.as_deref(), Some("renamed.html"));
    assert_eq!(c.before_page.as_deref(), Some("third.html"));
    assert_eq!(what(row(c, "d-C0001")), vec!["field:name"]);

    // the top-level glossary doc has no page on either side, and keeps its row
    let g = doc_row(&rv, "d-G0001");
    assert_eq!(g.title.as_deref(), Some("Glossary"));
    assert_eq!((g.page.as_deref(), g.before_page.as_deref()), (None, None));
    assert_eq!(kinds(g), vec![("p-G0001".into(), ReviewChange::Updated)]);
    assert_what_rule(&rv);
}

/// D-MCH70: each part counts on its own, and `what` lists the changed parts in the closed
/// order: title, text, type, links, place, status, then each custom field by name.
#[test]
fn drv2_counts_and_what_follow_reader_parts() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "parts", "Parts", "a"),
        with(
            prose("p-00001", "d-A0001", "b", "Same words."),
            "title",
            s("Old title"),
        ),
        prose("p-00002", "d-A0001", "c", "Old words here."),
        prose("p-00003", "d-A0001", "d", "Linked."),
        prose("p-00004", "d-A0001", "e", "Typed."),
        link(
            prose("p-00005", "d-A0001", "f", "Governed."),
            "decided_by",
            "D-00001",
        ),
        with(
            prose("p-00006", "d-A0001", "g", "Custom."),
            "everyday",
            s("Old"),
        ),
        link(
            with(
                with(
                    with(prose("p-00007", "d-A0001", "h", "All."), "title", s("T")),
                    "a",
                    s("1"),
                ),
                "b",
                s("1"),
            ),
            "decided_by",
            "D-00001",
        ),
        body(
            with(
                blk("t-00001", "term", Some("d-A0001"), "k"),
                "name",
                s("Term"),
            ),
            "A term.",
        ),
        doc("d-D0001", None, "decisions", "Decisions", "b"),
        with(
            decision("D-00001", "d-D0001", "a", "Ship it."),
            "built",
            s("1.0.0"),
        ),
    ];
    let after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(with(b, "title", s("New title")))),
            ("p-00002", &|b| Some(body(b, "New words here."))),
            ("p-00003", &|b| Some(link(b, "references", "t-00001"))),
            ("p-00004", &|mut b| {
                b.block_type = "quote".into();
                Some(b)
            }),
            ("p-00006", &|b| Some(with(b, "everyday", s("New")))),
            ("p-00007", &|mut b| {
                b.block_type = "quote".into();
                b.order = "a".into();
                let b = with(with(with(b, "title", s("T2")), "a", s("2")), "b", s("2"));
                Some(link(body(b, "All changed."), "references", "t-00001"))
            }),
            ("D-00001", &|b| Some(with(b, "built", s("1.1.0")))),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");
    let w = |id: &str| what(row(d, id));
    assert_eq!(w("p-00001"), vec!["title"]);
    assert_eq!(w("p-00002"), vec!["text"]);
    assert_eq!(w("p-00003"), vec!["links"]);
    assert_eq!(w("p-00004"), vec!["type"]);
    assert_eq!(
        w("p-00005"),
        vec!["status"],
        "the shown status the digest adds"
    );
    assert_eq!(w("p-00006"), vec!["field:everyday"]);
    assert_eq!(
        w("p-00007"),
        vec![
            "title", "text", "type", "links", "place", "status", "field:a", "field:b"
        ]
    );
    // the moved block reads first, in the candidate's page order
    assert_eq!(
        kinds(d).into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
        vec![
            "p-00007", "p-00001", "p-00002", "p-00003", "p-00004", "p-00005", "p-00006"
        ]
    );
    for c in &d.changes {
        assert_eq!(c.change, ReviewChange::Updated, "{}", c.id);
    }
    // only a text change gives a diff; it reads the reader text
    assert!(row(d, "p-00002").diff.is_some());
    for id in ["p-00001", "p-00003", "p-00004", "p-00005", "p-00006"] {
        assert_eq!(row(d, id).diff, None, "{id}");
    }
    assert_eq!(
        row(d, "p-00002").excerpt.as_deref(),
        Some("New words here.")
    );
    assert_eq!(d.counts, counts(|c| c.updated = 7));
    assert!(!d.record_only);
    assert!(rv.docs.iter().all(|x| x.id != bid("t-00001")));
    assert_what_rule(&rv);
}

/// D-MCH70: a block counts exactly when its `what` is not empty. A `text` change that the
/// reader text does not show counts with a null diff; pins and rationale alone give no row.
#[test]
fn drv2_what_is_empty_exactly_when_the_review_does_not_count() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-ADM1N", None, "admin", "Admin", "zz"),
        profile("pf-S1TE1", &["d-A0001", "d-R0001"], "mark"),
        with(
            doc("d-A0001", None, "cases", "Cases", "a"),
            "description",
            s("Old description."),
        ),
        body(
            with(
                blk("t-00001", "term", Some("d-A0001"), "a"),
                "name",
                s("Old name"),
            ),
            "Means a thing.",
        ),
        with(
            with(
                blk("tb-00001", "table", Some("d-A0001"), "b"),
                "columns",
                FieldValue::List(vec![s("A")]),
            ),
            "rows",
            FieldValue::List(vec![FieldValue::List(vec![s("1")])]),
        ),
        body(
            with(
                blk("dg-00001", "diagram", Some("d-A0001"), "c"),
                "alt",
                s("Old alt"),
            ),
            "graph A",
        ),
        body(
            with(
                blk("dg-00002", "diagram", Some("d-A0001"), "d"),
                "alt",
                s("Same alt"),
            ),
            "graph A",
        ),
        prose("p-00001", "d-A0001", "e", "See {{D-00009}} here."),
        prose("p-00002", "d-A0001", "f", "Linked."),
        prose("p-00003", "d-A0001", "g", "A word."),
        prose(
            "p-00004",
            "d-A0001",
            "h",
            "Go [there](https://example.org/a).",
        ),
        doc("d-X0001", None, "other", "Other", "c"),
        decision("D-00009", "d-X0001", "a", "Outside the selection."),
        decision("D-00008", "d-X0001", "b", "Also outside."),
        doc("d-R0001", None, "record", "Record", "b"),
        prose("p-R0001", "d-R0001", "a", "Pinned words."),
        with(
            decision("D-R0001", "d-R0001", "b", "Keep it."),
            "rationale",
            s("Because."),
        ),
    ];
    let pin = Hash([0x33; 32]);
    let after = edit(
        &before,
        &[
            ("d-A0001", &|b| {
                Some(with(b, "description", s("New description.")))
            }),
            ("t-00001", &|b| Some(with(b, "name", s("New name")))),
            ("tb-00001", &|b| {
                Some(with(
                    b,
                    "rows",
                    FieldValue::List(vec![FieldValue::List(vec![s("2")])]),
                ))
            }),
            ("dg-00001", &|b| Some(with(b, "alt", s("New alt")))),
            ("dg-00002", &|b| Some(body(b, "graph B"))),
            ("p-00001", &|b| Some(body(b, "See {{D-00008}} here."))),
            ("p-00002", &|b| Some(link(b, "references", "t-00001"))),
            ("p-00003", &|b| Some(body(b, "A **word**."))),
            ("p-00004", &|b| {
                Some(body(b, "Go [there](https://example.org/b)."))
            }),
            ("p-R0001", &|mut b| {
                b.pins.insert(bid("t-00001"), pin);
                Some(b)
            }),
            ("D-R0001", &|b| {
                Some(with(b, "rationale", s("Because, more.")))
            }),
        ],
    );
    let rv = review(&st, &before, &after, "site");
    let d = doc_row(&rv, "d-A0001");
    let case = |id: &str| {
        let c = row(d, id);
        assert_eq!(c.change, ReviewChange::Updated, "{id}");
        (what(c), c.diff.is_some())
    };
    assert_eq!(case("d-A0001"), (vec!["field:description"], false));
    assert_eq!(case("t-00001"), (vec!["text"], true), "a term's name");
    assert_eq!(case("tb-00001"), (vec!["text"], true), "a table's rows");
    assert_eq!(case("dg-00001"), (vec!["text"], true), "a diagram's alt");
    // text that the reader text does not show: a null diff
    assert_eq!(
        case("dg-00002"),
        (vec!["text"], false),
        "a diagram's source"
    );
    assert_eq!(
        case("p-00001"),
        (vec!["text"], false),
        "a reference the profile drops: its words change, its reader text does not"
    );
    assert_eq!(case("p-00003"), (vec!["text"], false), "a Markdown mark");
    assert_eq!(case("p-00004"), (vec!["text"], false), "a link target");
    assert_eq!(case("p-00002"), (vec!["links"], false), "a new link target");
    assert_eq!(row(d, "p-00001").excerpt.as_deref(), Some("See here."));
    assert_eq!(d.counts, counts(|c| c.updated = 9));
    assert!(!d.record_only);

    // pins only and a decision's rationale only: the digest changed, nothing counts
    let rec = doc_row(&rv, "d-R0001");
    assert!(rec.changes.is_empty(), "{:?}", kinds(rec));
    assert_eq!(rec.counts, ReviewCounts::default());
    assert!(rec.record_only);
    assert_what_rule(&rv);
}

/// D-RDH1C: a block that the profile hides on both sides is never counted, labelled or quoted;
/// a block that one side shows reads as added or removed.
#[test]
fn drv2_one_sided_visibility_is_added_or_removed() {
    let (_dir, r) = project();
    let st = r.read();
    let planned = |b: Block| with(b, "planned", FieldValue::Bool(true));
    let before = vec![
        doc("d-ADM1N", None, "admin", "Admin", "zz"),
        profile("pf-S1TE1", &["d-A0001"], "hide"),
        doc("d-A0001", None, "seen", "Seen", "a"),
        planned(prose("p-00001", "d-A0001", "a", "Hidden words one.")),
        planned(prose("p-00002", "d-A0001", "b", "Now shown.")),
        prose("p-00003", "d-A0001", "c", "Now hidden."),
        prose("p-00004", "d-A0001", "d", "Stays."),
    ];
    let after = edit(
        &before,
        &[
            ("p-00001", &|b| Some(body(b, "Hidden words two."))),
            ("p-00002", &|mut b| {
                b.fields.remove("planned");
                Some(b)
            }),
            ("p-00003", &|b| Some(planned(b))),
        ],
    );
    let rv = review(&st, &before, &after, "site");
    let d = doc_row(&rv, "d-A0001");
    assert_eq!(
        kinds(d),
        vec![
            ("p-00002".into(), ReviewChange::Added),
            ("p-00003".into(), ReviewChange::Removed),
        ]
    );
    assert_eq!(row(d, "p-00002").excerpt.as_deref(), Some("Now shown."));
    assert_eq!(row(d, "p-00003").excerpt.as_deref(), Some("Now hidden."));
    assert_eq!(
        d.counts,
        counts(|c| {
            c.added = 1;
            c.removed = 1;
        })
    );
    // the block hidden on both sides: no row, no label, no words
    assert!(rv.before.shown(&bid("p-00001")).is_none());
    assert!(rv.after.shown(&bid("p-00001")).is_none());
    for c in &d.changes {
        let words = format!("{} {:?} {:?}", c.label, c.excerpt, c.diff);
        assert!(!words.contains("Hidden"), "{words}");
    }
    assert_what_rule(&rv);
}

/// D-0C4TB: the content outside docs takes the part of a doc in the move rule. A block that
/// moves into a doc gets its one counted row there; one that moves out leaves a removed row.
#[test]
fn drv2_outside_moves_have_old_removal_and_one_new_change() {
    let (_dir, r) = project();
    let st = r.read();
    let question = |id: &str, parent: &str, order: &str, prompt: &str| {
        with(
            blk(id, "question", Some(parent), order),
            "prompt",
            s(prompt),
        )
    };
    let before = vec![
        folder("f-0F001", None, "box", "Box", "a"),
        doc("d-A0001", Some("f-0F001"), "home", "Home", "a"),
        prose("p-00001", "d-A0001", "a", "Stays."),
        question("q-00001", "f-0F001", "b", "Moves in?"),
        question("q-00002", "f-0F001", "c", "Moves in, edited?"),
        question("q-00003", "d-A0001", "b", "Moves out?"),
    ];
    let after = edit(
        &before,
        &[
            ("q-00001", &|mut b| {
                b.parent = Some(bid("d-A0001"));
                b.order = "c".into();
                Some(b)
            }),
            ("q-00002", &|mut b| {
                b.parent = Some(bid("d-A0001"));
                b.order = "d".into();
                Some(with(b, "prompt", s("Moved in and edited?")))
            }),
            ("q-00003", &|mut b| {
                b.parent = Some(bid("f-0F001"));
                Some(b)
            }),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    assert_eq!(ids(&rv.docs), vec!["d-A0001"]);
    let d = doc_row(&rv, "d-A0001");
    assert_eq!(
        kinds(d),
        vec![
            ("q-00001".into(), ReviewChange::Moved),
            ("q-00002".into(), ReviewChange::Updated),
            ("q-00003".into(), ReviewChange::Removed),
        ]
    );
    assert!(what(row(d, "q-00001")).is_empty());
    assert_eq!(what(row(d, "q-00002")), vec!["text", "place"]);
    assert_eq!(
        row(d, "q-00002").excerpt.as_deref(),
        Some("Moved in and edited?")
    );
    assert_eq!(row(d, "q-00003").excerpt.as_deref(), Some("Moves out?"));
    assert_eq!(
        d.counts,
        counts(|c| {
            c.moved = 1;
            c.updated = 1;
            c.removed = 1;
        })
    );
    assert_what_rule(&rv);
}

/// D-0C4TB: a cut inside a code span closes the span with a backtick before the "…", and the
/// backticks and the "…" count toward the limit, in an excerpt and in a diff piece.
#[test]
fn drv2_hard_cut_closes_code_span_before_ellipsis() {
    let (_dir, r) = project();
    let st = r.read();
    let lead = "abcd ".repeat(38);
    let long_code = format!("{}ab", "ab ".repeat(119));
    let before = vec![
        doc("d-A0001", None, "cuts", "Cuts", "a"),
        prose("p-00002", "d-A0001", "b", "Intro words here."),
    ];
    let mut after = edit(
        &before,
        &[("p-00002", &|b| {
            Some(body(b, &format!("Intro words here. `{long_code}`")))
        })],
    );
    after.push(prose(
        "p-00001",
        "d-A0001",
        "a",
        &format!("{lead}`ef gh ij kl mn op`"),
    ));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");

    let ex = row(d, "p-00001").excerpt.clone().expect("an excerpt");
    assert_eq!(ex, format!("{lead}`ef gh`\u{2026}"));
    assert_eq!(scalars(&ex), 198);
    assert!(scalars(&ex) <= EXCERPT_MAX);

    let diff = row(d, "p-00002").diff.clone().expect("a diff");
    assert_eq!(
        diff,
        vec![
            piece(DiffOp::Same, "Intro words here."),
            piece(DiffOp::Add, &format!(" `{}ab`\u{2026}", "ab ".repeat(92))),
        ]
    );
    assert_eq!(pieces_len(&diff), 299);
    assert!(pieces_len(&diff) <= DIFF_MAX);
}

/// D-0C4TB: when the first word alone passes the limit, the cut falls inside that word, in an
/// excerpt and in a diff piece.
#[test]
fn drv2_first_word_longer_than_limit_is_cut_inside_word() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "words", "Words", "a"),
        prose("p-00003", "d-A0001", "c", "a"),
    ];
    let mut after = edit(
        &before,
        &[("p-00003", &|b| {
            Some(body(b, &format!("a {}", "z".repeat(400))))
        })],
    );
    after.push(prose("p-00001", "d-A0001", "a", &"x".repeat(250)));
    after.push(prose(
        "p-00002",
        "d-A0001",
        "b",
        &format!("`{}`", "y".repeat(250)),
    ));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");
    let ex = |id: &str| row(d, id).excerpt.clone().expect("an excerpt");
    assert_eq!(ex("p-00001"), format!("{}\u{2026}", "x".repeat(199)));
    assert_eq!(ex("p-00002"), format!("`{}`\u{2026}", "y".repeat(197)));
    for id in ["p-00001", "p-00002"] {
        assert_eq!(scalars(&ex(id)), EXCERPT_MAX, "{id}");
    }
    let diff = row(d, "p-00003").diff.clone().expect("a diff");
    assert_eq!(
        diff,
        vec![
            piece(DiffOp::Same, "a"),
            piece(DiffOp::Add, &format!(" {}\u{2026}", "z".repeat(297))),
        ]
    );
    assert_eq!(pieces_len(&diff), DIFF_MAX);
}

/// D-RDH1C, D-MCH70: pins and a decision's rationale are in no part. The doc's digest changed,
/// so it keeps its row, with no change row, zero counts and `record_only`.
#[test]
fn drv2_pins_and_rationale_are_record_only() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "pins", "Pins", "a"),
        prose("p-00001", "d-A0001", "a", "Pinned words."),
        body(
            with(
                blk("t-00001", "term", Some("d-A0001"), "b"),
                "name",
                s("Term"),
            ),
            "A term.",
        ),
        doc("d-B0001", None, "why", "Why", "b"),
        prose("p-00002", "d-B0001", "a", "Governed words."),
        with(
            decision("D-00001", "d-B0001", "b", "Keep it."),
            "rationale",
            s("Because."),
        ),
        body(
            blk("r-00001", "rationale", Some("D-00001"), "a"),
            "The reason.",
        ),
    ];
    let pin = Hash([0x44; 32]);
    let after = edit(
        &before,
        &[
            ("p-00001", &|mut b| {
                b.pins.insert(bid("t-00001"), pin);
                Some(b)
            }),
            ("D-00001", &|b| {
                Some(with(b, "rationale", s("Because, more.")))
            }),
            ("r-00001", &|b| Some(body(b, "The reason, longer."))),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    assert_eq!(ids(&rv.docs), vec!["d-A0001", "d-B0001"]);
    for d in &rv.docs {
        assert!(d.changes.is_empty(), "{}: {:?}", d.id, kinds(d));
        assert_eq!(d.counts, ReviewCounts::default(), "{}", d.id);
        assert!(d.record_only, "{}", d.id);
        assert_eq!(d.before_title, None);
        assert_eq!(d.page, d.before_page);
    }
}

/// D-RDH1C: a change of a block's shown status counts, because the doc digest covers it. A
/// derived decision status that changes with no new version adds no doc row.
#[test]
fn drv2_shown_status_counts_but_derived_status_adds_no_doc() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "governed", "Governed", "a"),
        link(
            prose("p-00001", "d-A0001", "a", "Governed words."),
            "decided_by",
            "D-00001",
        ),
        doc("d-D0001", None, "built", "Built", "b"),
        with(
            decision("D-00001", "d-D0001", "a", "Ship it."),
            "built",
            s("1.0.0"),
        ),
        doc("d-E0001", None, "old", "Old", "c"),
        decision("D-00002", "d-E0001", "a", "The old way."),
        prose("p-00002", "d-E0001", "b", "Old doc words."),
        doc("d-F0001", None, "new", "New", "d"),
        prose("p-00003", "d-F0001", "a", "New doc words."),
    ];
    let mut after = edit(
        &before,
        &[("D-00001", &|b| Some(with(b, "built", s("1.1.0"))))],
    );
    after.push(link(
        decision("D-00003", "d-F0001", "b", "The new way."),
        "supersedes",
        "D-00002",
    ));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");
    assert_eq!(kinds(d), vec![("p-00001".into(), ReviewChange::Updated)]);
    let c = row(d, "p-00001");
    assert_eq!(what(c), vec!["status"]);
    assert_eq!(c.label, "Governed words.");
    assert_eq!(c.diff, None);
    assert_eq!(d.counts, counts(|c| c.updated = 1));

    // the superseded decision's derived status changed; its doc's digest did not
    let old = bid("D-00002");
    assert_ne!(
        rv.before.decision(&old).and_then(|x| x.status.clone()),
        rv.after.decision(&old).and_then(|x| x.status.clone()),
        "the derived status changed"
    );
    assert!(!rv.changed.contains(&bid("d-E0001")));
    assert!(rv.docs.iter().all(|x| x.id != bid("d-E0001")));
    assert_what_rule(&rv);
}

/// p-9M1DK: movement is decided by the place among the siblings both sides hold. New order keys
/// at the same places (a rebalance) and an added sibling move nothing.
#[test]
fn drv2_shared_sibling_order_ignores_rebalance() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "rebalanced", "Rebalanced", "a"),
        prose("p-00001", "d-A0001", "b", "One."),
        prose("p-00002", "d-A0001", "d", "Two."),
        prose("p-00003", "d-A0001", "f", "Three."),
        doc("d-B0001", None, "reordered", "Reordered", "b"),
        prose("p-00004", "d-B0001", "b", "Four."),
        prose("p-00005", "d-B0001", "d", "Five."),
        prose("p-00006", "d-B0001", "f", "Six."),
        prose("p-00007", "d-B0001", "h", "Seven."),
    ];
    let key = |k: &'static str| {
        move |mut b: Block| {
            b.order = k.into();
            Some(b)
        }
    };
    let (k1, k2, k3, k7) = (key("bb"), key("dd"), key("ff"), key("a"));
    let mut after = edit(
        &before,
        &[
            ("p-00001", &k1),
            ("p-00002", &k2),
            ("p-00003", &k3),
            ("p-00007", &k7),
        ],
    );
    after.push(prose("p-00008", "d-B0001", "c", "Eight."));
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);

    let a = doc_row(&rv, "d-A0001");
    assert!(a.changes.is_empty(), "{:?}", kinds(a));
    assert!(a.record_only);

    let b = doc_row(&rv, "d-B0001");
    assert_eq!(
        kinds(b),
        vec![
            ("p-00007".into(), ReviewChange::Moved),
            ("p-00008".into(), ReviewChange::Added),
        ]
    );
    assert_eq!(
        b.counts,
        counts(|c| {
            c.moved = 1;
            c.added = 1;
        })
    );
    assert_what_rule(&rv);
}

/// D-RDH1C: a block that moves to another doc gets one row in its new doc, `moved` or
/// `updated` with `place`; its old doc lists and counts a removed row. Moved descendants move
/// with it, each at both homes.
#[test]
fn drv2_cross_doc_move_has_old_removal_and_one_new_change() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "from", "From", "a"),
        prose("p-00001", "d-A0001", "a", "Moves."),
        prose("p-00002", "d-A0001", "b", "Moves, edited."),
        section("s-00001", "d-A0001", "c", "Part"),
        prose("p-00003", "s-00001", "a", "Child one."),
        prose("p-00004", "s-00001", "b", "Child two."),
        prose("p-00005", "d-A0001", "d", "Stays."),
        doc("d-B0001", None, "to", "To", "b"),
        prose("p-00006", "d-B0001", "a", "Here."),
    ];
    let to_b = |order: &'static str| {
        move |mut b: Block| {
            b.parent = Some(bid("d-B0001"));
            b.order = order.into();
            Some(b)
        }
    };
    let (m1, m3) = (to_b("b"), to_b("d"));
    let m2 = |b: Block| to_b("c")(b).map(|b| body(b, "Moved and edited."));
    let after = edit(
        &before,
        &[("p-00001", &m1), ("p-00002", &m2), ("s-00001", &m3)],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    assert_eq!(ids(&rv.docs), vec!["d-A0001", "d-B0001"]);

    let a = doc_row(&rv, "d-A0001");
    assert_eq!(
        kinds(a),
        vec![
            ("p-00001".into(), ReviewChange::Removed),
            ("p-00002".into(), ReviewChange::Removed),
            ("s-00001".into(), ReviewChange::Removed),
            ("p-00003".into(), ReviewChange::Removed),
            ("p-00004".into(), ReviewChange::Removed),
        ],
        "each removed block, in the baseline's order"
    );
    assert_eq!(row(a, "p-00002").excerpt.as_deref(), Some("Moves, edited."));
    assert_eq!(row(a, "p-00003").section.as_deref(), Some("Part"));
    assert_eq!(
        a.counts,
        counts(|c| {
            c.removed = 5;
            c.sections_removed = 1;
        })
    );

    let b = doc_row(&rv, "d-B0001");
    assert_eq!(
        kinds(b),
        vec![
            ("p-00001".into(), ReviewChange::Moved),
            ("p-00002".into(), ReviewChange::Updated),
            ("s-00001".into(), ReviewChange::Moved),
            ("p-00003".into(), ReviewChange::Moved),
            ("p-00004".into(), ReviewChange::Moved),
        ]
    );
    assert_eq!(what(row(b, "p-00002")), vec!["text", "place"]);
    assert!(row(b, "p-00002").diff.is_some());
    assert_eq!(row(b, "p-00003").section.as_deref(), Some("Part"));
    assert_eq!(
        b.counts,
        counts(|c| {
            c.moved = 4;
            c.updated = 1;
        })
    );
    // one counted row for each moved block, at its new home
    for id in ["p-00001", "p-00002", "s-00001", "p-00003", "p-00004"] {
        let counted = rv
            .docs
            .iter()
            .flat_map(|d| &d.changes)
            .filter(|c| c.id == bid(id) && c.change != ReviewChange::Removed)
            .count();
        assert_eq!(counted, 1, "{id}");
    }
    assert_what_rule(&rv);
}

/// D-RDH1C: change rows follow the review index's "Changes on this page" group: the doc's own
/// row, the lede, then each block, with removed blocks at their review position and one row for
/// each block of a grouped removed line. `section` is the candidate's nearest section, or the
/// baseline's for a removed block; a leading block has none.
#[test]
fn drv2_changes_keep_section_and_removed_page_order() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "ordered", "Doc a", "a"),
        section("s-00001", "d-A0001", "a", "One"),
        prose("p-00001", "s-00001", "a", "Lede words."),
        prose("p-00002", "s-00001", "b", "Removed words."),
        prose("p-00003", "s-00001", "c", "Kept words."),
        section("s-00002", "d-A0001", "b", "Two"),
        prose("p-00004", "s-00002", "a", "Edited words."),
        section("s-00003", "s-00002", "b", "Sub"),
        prose("p-00005", "s-00003", "a", "Sub words."),
        prose("p-00006", "s-00002", "z", "End words."),
        doc("d-B0001", None, "leading", "Leading", "b"),
        prose("p-00007", "d-B0001", "a", "Leading words."),
        section("s-00004", "d-B0001", "b", "Later"),
        prose("p-00008", "s-00004", "a", "Later words."),
    ];
    let gone = |_: Block| -> Option<Block> { None };
    let after = edit(
        &before,
        &[
            ("d-A0001", &|b| Some(with(b, "title", s("Doc A2")))),
            ("s-00001", &|b| Some(with(b, "title", s("One renamed")))),
            ("p-00001", &|b| Some(body(b, "Lede words, edited."))),
            ("p-00002", &gone),
            ("s-00002", &|b| Some(with(b, "title", s("Two renamed")))),
            ("p-00004", &|b| Some(body(b, "Edited words, new."))),
            ("s-00003", &gone),
            ("p-00005", &gone),
            ("p-00006", &gone),
            ("p-00007", &|b| Some(body(b, "Leading words, edited."))),
            ("p-00008", &|b| Some(body(b, "Later words, edited."))),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let a = doc_row(&rv, "d-A0001");
    use ReviewChange::{Removed as R, Updated as U};
    assert_eq!(
        kinds(a),
        vec![
            ("d-A0001".into(), U),
            ("p-00001".into(), U),
            ("s-00001".into(), U),
            ("p-00002".into(), R),
            ("s-00002".into(), U),
            ("p-00004".into(), U),
            ("s-00003".into(), R),
            ("p-00005".into(), R),
            ("p-00006".into(), R),
        ]
    );
    let sec = |id: &str| row(a, id).section.clone();
    assert_eq!(sec("d-A0001"), None);
    assert_eq!(sec("p-00001").as_deref(), Some("One renamed"));
    assert_eq!(sec("s-00001"), None);
    assert_eq!(sec("p-00002").as_deref(), Some("One"), "the baseline's");
    assert_eq!(sec("s-00002"), None);
    assert_eq!(sec("p-00004").as_deref(), Some("Two renamed"));
    assert_eq!(sec("s-00003").as_deref(), Some("Two"));
    assert_eq!(sec("p-00005").as_deref(), Some("Sub"));
    assert_eq!(sec("p-00006").as_deref(), Some("Two"));
    assert_eq!(row(a, "s-00002").label, "Two renamed");
    assert_eq!(row(a, "s-00003").label, "Sub");
    assert_eq!(
        a.counts,
        counts(|c| {
            c.updated = 5;
            c.removed = 4;
            c.sections_removed = 1;
        })
    );

    let b = doc_row(&rv, "d-B0001");
    assert_eq!(kinds(b), vec![("p-00007".into(), U), ("p-00008".into(), U)]);
    assert_eq!(row(b, "p-00007").section, None, "a leading block");
    assert_eq!(row(b, "p-00008").section.as_deref(), Some("Later"));
    assert_what_rule(&rv);
}

/// D-RDH1C, D-0C4TB: an excerpt and a diff are plain text with backticks for code; the diff
/// takes the first changed run with up to eight unchanged words on each side, and both keep
/// their hard limits in Unicode scalar values.
#[test]
fn drv2_excerpt_and_first_diff_run_are_plain_and_bounded() {
    use DiffOp::{Add, Del, Same};
    let coded = |t: &str| format!("{CODE_OPEN}{t}{CODE_CLOSE}");
    // eight context words stay whole; a ninth is left out
    let eight = short_diff(
        "w1 w2 w3 w4 w5 w6 w7 w8 OLD x1 x2 x3 x4 x5 x6 x7 x8",
        "w1 w2 w3 w4 w5 w6 w7 w8 NEW x1 x2 x3 x4 x5 x6 x7 x8",
        "prose",
        "prose",
    );
    assert_eq!(
        eight,
        Some(vec![
            piece(Same, "w1 w2 w3 w4 w5 w6 w7 w8 "),
            piece(Del, "OLD"),
            piece(Add, "NEW"),
            piece(Same, " x1 x2 x3 x4 x5 x6 x7 x8"),
        ])
    );
    let nine = short_diff(
        "w0 w1 w2 w3 w4 w5 w6 w7 w8 OLD x1 x2 x3 x4 x5 x6 x7 x8 x9",
        "w0 w1 w2 w3 w4 w5 w6 w7 w8 NEW x1 x2 x3 x4 x5 x6 x7 x8 x9",
        "prose",
        "prose",
    );
    assert_eq!(
        nine,
        Some(vec![
            piece(Same, "w1 w2 w3 w4 w5 w6 w7 w8 "),
            piece(Del, "OLD"),
            piece(Add, "NEW"),
            piece(Same, " x1 x2 x3 x4 x5 x6 x7 x8"),
        ])
    );
    // two separated changed runs: the first only
    let two = short_diff(
        "a b c ONE d e f g h i j k l TWO m",
        "a b c UNO d e f g h i j k l DOS m",
        "prose",
        "prose",
    );
    assert_eq!(
        two,
        Some(vec![
            piece(Same, "a b c "),
            piece(Del, "ONE"),
            piece(Add, "UNO"),
            piece(Same, " d e f g h i j k"),
        ])
    );
    // a code span keeps its backticks; a word that leaves its span is changed, not same
    let code = short_diff(
        &format!("Run {} now.", coded("cargo test")),
        &format!("Run {} now.", coded("cargo build")),
        "prose",
        "prose",
    );
    assert_eq!(
        code,
        Some(vec![
            piece(Same, "Run `cargo `"),
            piece(Del, "`test`"),
            piece(Add, "`build`"),
            piece(Same, " now."),
        ])
    );
    // a code block diffs by lines, and its whitespace collapses
    let lines = short_diff(
        "fn a() {}\nlet x = 1;\n",
        "fn a() {}\nlet x = 2;\n",
        "code",
        "code",
    );
    assert_eq!(
        lines,
        Some(vec![
            piece(Same, "fn a() {} "),
            piece(Del, "let x = 1; "),
            piece(Add, "let x = 2; "),
        ])
    );
    // a block that turns into code shows each side whole
    assert_eq!(
        short_diff("one two", "one three", "prose", "code"),
        Some(vec![piece(Del, "one two"), piece(Add, "one three")])
    );
    // equal reader text gives no diff
    assert_eq!(short_diff("same", "same", "prose", "prose"), None);

    // 300 and 301 scalars over all pieces
    let at = |x: &str| short_diff("a", &format!("a {x}"), "prose", "prose").expect("a diff");
    let fits = at(&format!("{}b", "bc ".repeat(99)));
    assert_eq!(pieces_len(&fits), DIFF_MAX);
    assert!(!fits.iter().any(|p| p.text.contains('\u{2026}')));
    let over = at(&format!("{}bc", "bc ".repeat(99)));
    assert_eq!(
        over,
        vec![
            piece(Same, "a"),
            piece(Add, &format!(" {}bc\u{2026}", "bc ".repeat(98))),
        ]
    );
    assert!(pieces_len(&over) <= DIFF_MAX);

    // 200 and 201 scalars in an excerpt, with Unicode, an ellipsis and backticks
    let two_hundred = format!("{}éé", "éé ".repeat(66));
    assert_eq!(scalars(&two_hundred), EXCERPT_MAX);
    assert_eq!(excerpt(&two_hundred).as_deref(), Some(two_hundred.as_str()));
    let over = format!("{}ééé", "éé ".repeat(66));
    assert_eq!(
        excerpt(&over),
        Some(format!("{}éé\u{2026}", "éé ".repeat(65)))
    );
    let ticks = format!("{}{}", "q ".repeat(95), coded("code1234"));
    let want = format!("{}`code1234`", "q ".repeat(95));
    assert_eq!(scalars(&want), EXCERPT_MAX);
    assert_eq!(excerpt(&ticks), Some(want));
    let ticks = format!("{}{}", "q ".repeat(95), coded("code12345"));
    assert_eq!(
        excerpt(&ticks),
        Some(format!("{}q\u{2026}", "q ".repeat(94)))
    );
    // whitespace collapses; HTML stays plain text; an empty text has no excerpt
    assert_eq!(
        excerpt("  Use <b> tags\n\n & such.  ").as_deref(),
        Some("Use <b> tags & such.")
    );
    assert_eq!(excerpt(" \n "), None);

    // and through the rows: plain text, code spans in backticks
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-A0001", None, "plain", "Plain", "a"),
        prose(
            "p-00001",
            "d-A0001",
            "a",
            "w0 w1 w2 w3 w4 w5 w6 w7 w8 old x1 x2 x3 x4 x5 x6 x7 x8 x9",
        ),
        prose(
            "p-00002",
            "d-A0001",
            "b",
            "Use `cargo test` with <b> & café.",
        ),
    ];
    let after = edit(
        &before,
        &[
            ("p-00001", &|b| {
                Some(body(
                    b,
                    "w0 w1 w2 w3 w4 w5 w6 w7 w8 new x1 x2 x3 x4 x5 x6 x7 x8 x9",
                ))
            }),
            ("p-00002", &|b| {
                Some(body(b, "Use `cargo build` with <b> & café."))
            }),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);
    let d = doc_row(&rv, "d-A0001");
    assert_eq!(
        row(d, "p-00001").diff,
        Some(vec![
            piece(Same, "w1 w2 w3 w4 w5 w6 w7 w8 "),
            piece(Del, "old"),
            piece(Add, "new"),
            piece(Same, " x1 x2 x3 x4 x5 x6 x7 x8"),
        ])
    );
    let p2 = row(d, "p-00002");
    assert_eq!(
        p2.excerpt.as_deref(),
        Some("Use `cargo build` with <b> & café.")
    );
    assert_eq!(p2.label, "Use `cargo build` with <b> & café.");
    assert_eq!(
        p2.diff,
        Some(vec![
            piece(Same, "Use `cargo `"),
            piece(Del, "`test`"),
            piece(Add, "`build`"),
            piece(Same, " with <b> & café."),
        ])
    );
}

/// D-Z8Q4N, D-RDH1C: the removals of both sides apply by version before any text is read. A
/// removed title is null; a removed section, a removed reference target and a removed block
/// read "Content removed."; an old version removed beside a newer one leaves no diff.
#[test]
fn drv2_redaction_masks_both_versions_before_text_reads() {
    let (_dir, r) = project();
    let st = r.read();
    let request = Hash::of(b"a removal request");
    let secret_doc = doc("d-RED01", None, "secret", "Secret doc", "b");
    let secret_section = section("s-00001", "d-A0001", "b", "Secret section");
    let secret_term = body(
        with(
            blk("t-00001", "term", Some("d-A0001"), "c"),
            "name",
            s("Secret term"),
        ),
        "Secret meaning.",
    );
    let old_version = prose("p-00004", "d-A0001", "e", "Secret old words.");
    let new_kept = prose("p-00005", "d-A0001", "f", "Plain old words.");
    let before = vec![
        doc("d-A0001", None, "home", "Home", "a"),
        prose("p-00001", "d-A0001", "a", "First words."),
        secret_section.clone(),
        prose("p-00002", "s-00001", "a", "Inside words."),
        secret_term.clone(),
        prose("p-00003", "d-A0001", "d", "See {{t-00001}}."),
        // the baseline's version of p-00004 is removed: the baseline holds its stub
        stub_of(&old_version, old_version.version_hash(), request),
        new_kept.clone(),
        secret_doc.clone(),
        prose("p-R0001", "d-RED01", "a", "Under a removed doc."),
    ];
    let new_secret = body(new_kept.clone(), "Secret new words.");
    let after = edit(
        &before,
        &[
            // the candidate holds the same removed versions: both sides read them as stubs
            ("s-00001", &|_| {
                Some(stub_of(
                    &secret_section,
                    secret_section.version_hash(),
                    request,
                ))
            }),
            ("t-00001", &|_| {
                Some(stub_of(&secret_term, secret_term.version_hash(), request))
            }),
            ("d-RED01", &|_| {
                Some(stub_of(&secret_doc, secret_doc.version_hash(), request))
            }),
            ("p-00002", &|b| Some(body(b, "Inside words, edited."))),
            ("p-00003", &|b| Some(body(b, "See {{t-00001}} again."))),
            // a newer version beside the removed old one
            ("p-00004", &|_| {
                Some(prose("p-00004", "d-A0001", "e", "New words."))
            }),
            // the candidate's new version is removed; the baseline's version stays
            ("p-00005", &|_| {
                Some(stub_of(&new_secret, new_secret.version_hash(), request))
            }),
        ],
    );
    let rv = review(&st, &before, &after, publication::IMPLICIT_ORG);

    let red = doc_row(&rv, "d-RED01");
    assert_eq!(red.title, None, "a removed title is null");
    assert_eq!(
        red.before_title, None,
        "both sides read the removed version"
    );
    assert!(red.changes.is_empty(), "{:?}", kinds(red));

    let d = doc_row(&rv, "d-A0001");
    let p2 = row(d, "p-00002");
    assert_eq!(p2.section.as_deref(), Some(CONTENT_REMOVED));
    assert!(!p2.redacted);
    let p3 = row(d, "p-00003");
    assert_eq!(what(p3), vec!["text"]);
    let p4 = row(d, "p-00004");
    assert_eq!(p4.change, ReviewChange::Updated);
    assert_eq!(p4.label, "New words.");
    assert_eq!(p4.excerpt.as_deref(), Some("New words."));
    assert_eq!(p4.diff, None, "the old version has no text");
    assert!(!p4.redacted);
    let p5 = row(d, "p-00005");
    assert_eq!(p5.change, ReviewChange::Updated);
    assert_eq!(p5.label, CONTENT_REMOVED);
    assert_eq!((p5.excerpt.as_deref(), p5.diff.as_ref()), (None, None));
    assert!(p5.redacted);
    assert!(
        rv.docs
            .iter()
            .all(|x| x.id != bid("s-00001") && x.id != bid("t-00001")),
        "removed blocks read the same on both sides"
    );
    // no removed word reaches a title, label, section, excerpt or diff
    for x in &rv.docs {
        let mut words = format!("{:?} {:?}", x.title, x.before_title);
        for c in &x.changes {
            words.push_str(&format!(
                " {} {:?} {:?} {:?}",
                c.label, c.section, c.excerpt, c.diff
            ));
        }
        assert!(!words.contains("Secret"), "{words}");
    }
    assert_what_rule(&rv);
}
