//! The added, removed and included doc summaries of the disclosure review (D-RDH1C, D-0C4TB,
//! D-MCH70, D-TBDA1): titles, pages, top folders, sizes, ledes, outlines, decision titles and
//! figure labels, read from each side's reader view. Each test builds its states in memory over
//! an empty snapshot.

use rdy_engine::publication::review_model::{
    DocSize, DocSummary, FigureEntry, OutlineRow, RemovedDocReview,
};
use rdy_engine::publication::{
    self, AttachmentVariant, DECISION_TITLES_MAX, DisclosureDiff, FIGURES_MAX, OUTLINE_MAX,
    PublicationReviewSide, ReadPublicationSource, ReaderReviewSide, ReviewRemovals,
    disclosure_diff, doc_reviews, included,
};
use rdy_engine::{Materialized, Rdy, ReadState};
use rdy_model::redaction::{CONTENT_REMOVED, RedactedBlockIdentity};
use rdy_model::{Block, BlockId, FieldValue, Hash, StateSnapshot};
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

fn planned(b: Block) -> Block {
    with(b, "planned", FieldValue::Bool(true))
}

fn shipped(b: Block) -> Block {
    with(b, "since", s("1.0.0"))
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

fn hash(n: u16) -> Hash {
    let mut h = [0u8; 32];
    h[..2].copy_from_slice(&n.to_be_bytes());
    h[31] = 7;
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

/// The removed form of `b` under `request`.
fn stub(b: &Block, request: Hash) -> Block {
    Block::redacted(
        b.id.clone(),
        b.block_type.clone(),
        b.parent.clone(),
        b.order.clone(),
        RedactedBlockIdentity {
            version: b.version_hash(),
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

/// Both sides resolved with `profile`, viewed with the comparison's removals, and compared.
fn compared(
    st: &ReadState,
    before: &[Block],
    after: &[Block],
    profile: &str,
) -> (ReaderReviewSide, ReaderReviewSide, DisclosureDiff) {
    let (bm, am) = (synth(before), synth(after));
    let b = publication::resolve_in_for_tests(st, &bm, profile).expect("the baseline");
    let a = publication::resolve_in_for_tests(st, &am, profile).expect("the candidate");
    let source = ReadPublicationSource::with_settings(st, st.pack(), "Project", None);
    let removals = ReviewRemovals::of_sides(&source, &[&b, &a]).expect("the removals");
    let diff = disclosure_diff(&b, &a).expect("the comparison");
    (
        ReaderReviewSide::new(&b, &removals),
        ReaderReviewSide::new(&a, &removals),
        diff,
    )
}

/// The one side of a first publication, viewed with its own removals.
fn first(st: &ReadState, blocks: &[Block], profile: &str) -> ReaderReviewSide {
    first_with(st, blocks, profile, &[])
}

/// [`first`], with each blob of `lost_blobs` removed too, as a removal of its image names it.
fn first_with(
    st: &ReadState,
    blocks: &[Block],
    profile: &str,
    lost_blobs: &[(Hash, Hash)],
) -> ReaderReviewSide {
    let m = synth(blocks);
    let a = publication::resolve_in_for_tests(st, &m, profile).expect("the candidate");
    let source = ReadPublicationSource::with_settings(st, st.pack(), "Project", None);
    let mut removals = ReviewRemovals::of_sides(&source, &[&a]).expect("the removals");
    removals.blobs.extend(lost_blobs.iter().copied());
    ReaderReviewSide::new(&a, &removals)
}

fn ids(rows: &[DocSummary]) -> Vec<String> {
    rows.iter().map(|d| d.id.to_string()).collect()
}

fn removed_ids(rows: &[RemovedDocReview]) -> Vec<String> {
    rows.iter().map(|d| d.id.to_string()).collect()
}

fn summary<'a>(rows: &'a [DocSummary], id: &str) -> &'a DocSummary {
    rows.iter()
        .find(|d| d.id == bid(id))
        .unwrap_or_else(|| panic!("no row for {id}: {:?}", ids(rows)))
}

fn size(sections: u64, blocks: u64, decisions: u64, figures: u64) -> DocSize {
    DocSize {
        sections,
        blocks,
        decisions,
        figures,
    }
}

fn light(label: &str) -> FigureEntry {
    FigureEntry {
        label: label.into(),
        variant: None,
    }
}

fn dark(label: &str) -> FigureEntry {
    FigureEntry {
        label: label.into(),
        variant: Some(AttachmentVariant::Dark),
    }
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// The labels of an outline, in its order (D-EGFYD gives each row more members).
fn labels(v: &[OutlineRow]) -> Vec<&str> {
    v.iter().map(|r| r.label.as_str()).collect()
}

// ---- tests ----------------------------------------------------------------------------------

/// D-RDH1C, D-0C4TB: an added doc names its title, page and top folder, and shows its full size,
/// its lede, its outline in page order, its decision titles and its figure labels. A root doc's
/// folder is null; an empty doc shows nothing but its names.
#[test]
fn drv3_added_docs_show_size_lede_outline_and_top_folder() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        folder("f-00001", None, "top", "Top", "a"),
        folder("f-00002", Some("f-00001"), "sub", "Sub", "a"),
        doc("d-00001", Some("f-00002"), "kept", "Kept", "a"),
        prose("p-00001", "d-00001", "a", "Kept words."),
    ];
    let mut after = before.clone();
    after.extend([
        doc("d-00010", Some("f-00002"), "fresh", "Fresh", "b"),
        prose(
            "p-00010",
            "d-00010",
            "a",
            "Opening words of the `fresh` doc.",
        ),
        section("s-00010", "d-00010", "b", "First part"),
        prose("p-00011", "s-00010", "a", "Inside the first part."),
        section("s-00011", "s-00010", "b", "Deeper part"),
        prose("p-00012", "s-00011", "a", "Deep words."),
        with(
            decision("D-00010", "s-00011", "b", "Fixtures stay small."),
            "title",
            s("Use small fixtures"),
        ),
        figure("x-00010", "s-00010", "c", "A chart", 1, None),
        doc("d-00020", None, "root", "Root", "b"),
        prose("p-00020", "d-00020", "a", "Root words."),
        doc("d-00030", Some("f-00001"), "empty", "Empty", "b"),
    ]);
    let (bv, av, diff) = compared(&st, &before, &after, publication::IMPLICIT_ORG);
    let reviews = doc_reviews(Some((&diff, &bv)), &av);
    assert_eq!(reviews.included, None);
    assert!(reviews.removed_docs.is_empty());
    let added = &reviews.added_docs;
    assert_eq!(
        ids(added),
        vec!["d-00010", "d-00030", "d-00020"],
        "candidate reading order"
    );

    let fresh = summary(added, "d-00010");
    assert_eq!(fresh.title.as_deref(), Some("Fresh"));
    assert_eq!(fresh.page.as_deref(), Some("top/sub/fresh.html"));
    assert_eq!(fresh.folder.as_deref(), Some("Top"), "the top folder");
    assert_eq!(fresh.size, size(2, 6, 1, 1));
    assert_eq!(
        fresh.lede.as_deref(),
        Some("Opening words of the `fresh` doc.")
    );
    assert_eq!(labels(&fresh.outline), vec!["First part", "Deeper part"]);
    assert_eq!(strs(&fresh.decision_titles), vec!["Use small fixtures"]);
    assert_eq!(fresh.figures, vec![light("A chart")]);
    assert_eq!(
        (
            fresh.omitted_outline,
            fresh.omitted_decision_titles,
            fresh.omitted_figures
        ),
        (0, 0, 0)
    );

    let root = summary(added, "d-00020");
    assert_eq!(root.folder, None, "a root doc has no folder");
    assert_eq!(root.page.as_deref(), Some("root.html"));
    assert_eq!(root.size, size(0, 1, 0, 0));
    assert_eq!(root.lede.as_deref(), Some("Root words."));

    let empty = summary(added, "d-00030");
    assert_eq!(empty.title.as_deref(), Some("Empty"));
    assert_eq!(empty.folder.as_deref(), Some("Top"));
    assert_eq!(empty.size, DocSize::default());
    assert_eq!(empty.lede, None);
    assert!(empty.outline.is_empty() && empty.decision_titles.is_empty());
    assert!(empty.figures.is_empty());
}

/// D-RDH1C, D-0C4TB: a removed doc gives the baseline's title, page and size, though the
/// candidate still holds it under a new title outside the selection.
#[test]
fn drv3_removed_docs_use_baseline_titles_pages_and_sizes() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        doc("d-ADM1N", None, "admin", "Admin", "zz"),
        profile("pf-00001", &["d-00001", "d-00002"], "mark"),
        folder("f-00001", None, "guide", "Guide", "a"),
        doc("d-00001", Some("f-00001"), "gone", "Gone", "a"),
        section("s-00001", "d-00001", "a", "Only part"),
        prose("p-00001", "s-00001", "a", "Gone words."),
        decision("D-00001", "s-00001", "b", "Gone decision."),
        figure("x-00001", "s-00001", "c", "Gone chart", 1, Some(2)),
        doc("d-00002", Some("f-00001"), "kept", "Kept", "b"),
        prose("p-00002", "d-00002", "a", "Kept words."),
    ];
    let after: Vec<Block> = before
        .iter()
        .map(|b| match b.id.as_str() {
            "pf-00001" => profile("pf-00001", &["d-00002"], "mark"),
            "d-00001" => with(b.clone(), "title", s("Renamed outside")),
            _ => b.clone(),
        })
        .collect();
    let (bv, av, diff) = compared(&st, &before, &after, "site");
    let reviews = doc_reviews(Some((&diff, &bv)), &av);
    assert!(reviews.added_docs.is_empty());
    assert_eq!(
        reviews.removed_docs,
        vec![RemovedDocReview {
            id: bid("d-00001"),
            title: Some("Gone".into()),
            before_page: Some("guide/gone.html".into()),
            folder: Some("Guide".into()),
            folder_redacted: false,
            position: 0,
            size: size(1, 3, 1, 2),
        }]
    );
}

/// D-RDH1C: a first publication lists every selected doc in the added-doc form, in site order,
/// never id order. It has no added or removed rows.
#[test]
fn drv3_first_publication_lists_every_doc_in_site_order() {
    let (_dir, r) = project();
    let st = r.read();
    let blocks = vec![
        folder("f-00009", None, "zed", "Zed", "a"),
        doc("d-00009", Some("f-00009"), "nine", "Nine", "a"),
        prose("p-00009", "d-00009", "a", "Nine words."),
        doc("d-00001", Some("f-00009"), "one", "One", "b"),
        doc("d-00005", None, "five", "Five", "b"),
        section("s-00005", "d-00005", "a", "Five part"),
    ];
    let side = first(&st, &blocks, publication::IMPLICIT_ORG);
    let reviews = doc_reviews(None, &side);
    assert!(reviews.added_docs.is_empty() && reviews.removed_docs.is_empty());
    let inc = reviews.included.expect("a first publication");
    assert_eq!(inc, included(&side));
    assert_eq!(ids(&inc.rows), vec!["d-00009", "d-00001", "d-00005"]);
    assert_eq!(inc.omitted_rows, 0);
    let nine = summary(&inc.rows, "d-00009");
    assert_eq!(nine.title.as_deref(), Some("Nine"));
    assert_eq!(nine.page.as_deref(), Some("zed/nine.html"));
    assert_eq!(nine.folder.as_deref(), Some("Zed"));
    assert_eq!(nine.lede.as_deref(), Some("Nine words."));
    let five = summary(&inc.rows, "d-00005");
    assert_eq!(five.folder, None);
    assert_eq!(labels(&five.outline), vec!["Five part"]);
    assert_eq!(five.size, size(1, 1, 0, 0));
}

/// D-RDH1C: an empty previous selection is still a comparison. Every doc is an added row and
/// there is no included list.
#[test]
fn drv3_empty_against_is_not_first_publication() {
    let (_dir, r) = project();
    let st = r.read();
    let after = vec![
        doc("d-00002", None, "two", "Two", "a"),
        prose("p-00002", "d-00002", "a", "Two words."),
        doc("d-00001", None, "one", "One", "b"),
    ];
    let (bv, av, diff) = compared(&st, &[], &after, publication::IMPLICIT_ORG);
    assert!(
        bv.selection().docs.is_empty(),
        "an empty previous selection"
    );
    let reviews = doc_reviews(Some((&diff, &bv)), &av);
    assert_eq!(reviews.included, None, "a comparison has no included list");
    assert!(reviews.removed_docs.is_empty());
    assert_eq!(ids(&reviews.added_docs), vec!["d-00002", "d-00001"]);
    assert_eq!(
        reviews.added_docs[0].lede.as_deref(),
        Some("Two words."),
        "added rows take the added-doc form"
    );
    assert_eq!(reviews.added_docs, included(&av).rows);
}

/// D-RDH1C: an added or included doc lists at most 30 outline entries, 50 decision titles and
/// 50 attachment images, and counts each one it leaves out. An untitled decision is named by its
/// statement, never its id.
#[test]
fn drv3_outline_decision_title_and_figure_caps_count_omissions() {
    assert_eq!(
        (OUTLINE_MAX, DECISION_TITLES_MAX, FIGURES_MAX),
        (30, 50, 50)
    );
    let (_dir, r) = project();
    let st = r.read();
    let mut blocks = vec![];
    // (doc, sections, decisions, figures with both images, figures with a light image only)
    for (d, n, (secs, decs, both, lone)) in [
        ("d-0000A", 'A', (30, 50, 25, 0)),
        ("d-0000B", 'B', (31, 51, 25, 1)),
    ] {
        blocks.push(doc(
            d,
            None,
            &format!("doc{n}"),
            &format!("Doc {n}"),
            &format!("{n}"),
        ));
        for i in 0..secs {
            blocks.push(section(
                &format!("s-{n}{i:04}"),
                d,
                &format!("a{i:04}"),
                &format!("Part {i}"),
            ));
        }
        for i in 0..decs {
            blocks.push(decision(
                &format!("D-{n}{i:04}"),
                d,
                &format!("b{i:04}"),
                &format!("Choice number {i}."),
            ));
        }
        for i in 0..both + lone {
            let dark = (i < both).then_some(1000 + i as u16);
            blocks.push(figure(
                &format!("x-{n}{i:04}"),
                d,
                &format!("c{i:04}"),
                &format!("Chart {i}"),
                i as u16,
                dark,
            ));
        }
    }
    let side = first(&st, &blocks, publication::IMPLICIT_ORG);
    let inc = included(&side);

    let a = summary(&inc.rows, "d-0000A");
    assert_eq!((a.outline.len(), a.omitted_outline), (30, 0));
    assert_eq!(
        (a.decision_titles.len(), a.omitted_decision_titles),
        (50, 0)
    );
    assert_eq!((a.figures.len(), a.omitted_figures), (50, 0));
    assert_eq!(a.size.figures, 50);

    let b = summary(&inc.rows, "d-0000B");
    assert_eq!((b.outline.len(), b.omitted_outline), (30, 1));
    assert_eq!(
        (b.decision_titles.len(), b.omitted_decision_titles),
        (50, 1)
    );
    assert_eq!((b.figures.len(), b.omitted_figures), (50, 1));
    assert_eq!(b.size, size(31, 31 + 26, 51, 51));
    assert_eq!(
        b.size.figures,
        b.figures.len() as u64 + b.omitted_figures,
        "every image is a row or an omission"
    );
    // page order, and the statement for an untitled decision
    assert_eq!(b.outline[0].label, "Part 0");
    assert_eq!(b.outline[29].label, "Part 29");
    assert_eq!(b.decision_titles[0], "Choice number 0.");
    assert_eq!(b.decision_titles[49], "Choice number 49.");
    assert!(b.decision_titles.iter().all(|t| !t.contains("D-B")));
    assert_eq!(b.figures[0], light("Chart 0"));
    assert_eq!(b.figures[1], dark("Chart 0"));
    assert_eq!(b.figures[49], dark("Chart 24"));
}

/// D-TBDA1: a figure with a dark image gives two rows with its one label, light first; a light
/// row has no `variant` member. Rows follow page order, not id order.
#[test]
fn drv3_light_and_dark_rows_keep_labels_and_variant_shape() {
    let (_dir, r) = project();
    let st = r.read();
    let blocks = vec![
        doc("d-00001", None, "pics", "Pictures", "a"),
        figure("x-00009", "d-00001", "a", "Day and night", 1, Some(2)),
        figure("x-00001", "d-00001", "b", "Only light", 3, None),
    ];
    let side = first(&st, &blocks, publication::IMPLICIT_ORG);
    let inc = included(&side);
    let d = summary(&inc.rows, "d-00001");
    assert_eq!(
        d.figures,
        vec![
            light("Day and night"),
            dark("Day and night"),
            light("Only light")
        ]
    );
    assert_eq!(
        serde_json::to_value(&d.figures).expect("the rows"),
        json!([
            {"label": "Day and night"},
            {"label": "Day and night", "variant": "dark"},
            {"label": "Only light"},
        ])
    );
    assert_eq!(d.size, size(0, 2, 0, 3));
}

/// D-9BWMV, D-Z8Q4N: a removed doc or folder title is null, a removed section or figure reads
/// "Content removed.", and no removed word reaches a summary. A surviving title under a removed
/// folder stays.
#[test]
fn drv3_redacted_doc_folder_and_figure_text_stays_removed() {
    let (_dir, r) = project();
    let st = r.read();
    let request = Hash::of(b"a removal request");
    let secret_folder = folder("f-00001", None, "secret", "Secret folder", "a");
    let secret_doc = doc("d-00001", Some("f-00001"), "hidden", "Secret doc", "a");
    let secret_section = section("s-00001", "d-00001", "b", "Secret section");
    let secret_figure = figure("x-00001", "d-00001", "c", "Secret figure", 1, Some(2));
    let blocks = vec![
        stub(&secret_folder, request),
        stub(&secret_doc, request),
        prose("p-00001", "d-00001", "a", "Plain words."),
        stub(&secret_section, request),
        prose("p-00002", "s-00001", "a", "Under a removed section."),
        stub(&secret_figure, request),
        // a figure whose dark image a removal names: both its rows lose its words
        figure("x-00002", "d-00001", "d", "Secret picture", 3, Some(4)),
        doc("d-00002", Some("f-00001"), "kept", "Kept doc", "b"),
        prose("p-00003", "d-00002", "a", "Kept words."),
    ];
    let side = first_with(
        &st,
        &blocks,
        publication::IMPLICIT_ORG,
        &[(hash(4), request)],
    );
    let inc = included(&side);
    assert_eq!(ids(&inc.rows), vec!["d-00001", "d-00002"]);

    let red = summary(&inc.rows, "d-00001");
    assert_eq!(red.title, None, "a removed doc title is null");
    assert_eq!(red.folder, None, "a removed folder title is null");
    assert_eq!(red.lede.as_deref(), Some("Plain words."));
    assert_eq!(labels(&red.outline), vec![CONTENT_REMOVED]);
    // the removed figure has no image left; the other keeps both rows, each with the notice
    assert_eq!(
        red.figures,
        vec![light(CONTENT_REMOVED), dark(CONTENT_REMOVED)]
    );
    // p-00001, s-00001, p-00002, x-00001 and x-00002
    assert_eq!(red.size, size(1, 5, 0, 2));

    let kept = summary(&inc.rows, "d-00002");
    assert_eq!(kept.title.as_deref(), Some("Kept doc"), "a surviving title");
    assert_eq!(kept.folder, None);

    let words = format!("{inc:?}");
    assert!(!words.contains("Secret"), "{words}");
}

/// D-0C4TB: size counts every block the view shows below the doc at any depth, sections and
/// figures included, and neither the doc nor its decisions; figures count by attachment image.
/// A block the profile hides is not counted; a shell counts as shown.
#[test]
fn drv3_size_counts_all_depths_and_each_attachment_image() {
    let (_dir, r) = project();
    let st = r.read();
    let blocks = vec![
        doc("d-ADM1N", None, "admin", "Admin", "zz"),
        profile("pf-00001", &["d-00001", "d-00002"], "hide"),
        doc("d-00001", None, "deep", "Deep", "a"),
        section("s-00001", "d-00001", "a", "One"),
        section("s-00002", "s-00001", "a", "Two"),
        section("s-00003", "s-00002", "a", "Three"),
        prose("p-00001", "s-00003", "a", "Deepest words."),
        figure("x-00001", "s-00003", "b", "Deep chart", 1, Some(2)),
        decision("D-00001", "s-00002", "b", "Deep decision."),
        body(blk("r-00001", "rationale", Some("D-00001"), "a"), "Why."),
        planned(prose("p-00002", "d-00001", "b", "Hidden words.")),
        planned(section("s-00004", "d-00001", "c", "Shell part")),
        // a shipped block keeps its planned container as a shell
        shipped(prose("p-00003", "s-00004", "a", "Shown in a shell.")),
        planned(doc("d-00002", None, "shell", "Shell doc", "b")),
        shipped(prose("p-00004", "d-00002", "a", "Shown words.")),
        planned(prose("p-00005", "d-00002", "b", "Hidden too.")),
    ];
    let side = first(&st, &blocks, "site");
    let inc = included(&side);
    assert_eq!(ids(&inc.rows), vec!["d-00001", "d-00002"]);
    let deep = summary(&inc.rows, "d-00001");
    // s-00001..4, p-00001, x-00001 and p-00003; not the doc, the decision or its rationale
    assert_eq!(deep.size, size(4, 7, 1, 2));
    assert_eq!(
        labels(&deep.outline),
        vec!["One", "Two", "Three", "Shell part"]
    );
    assert_eq!(strs(&deep.decision_titles), vec!["Deep decision."]);
    assert_eq!(deep.figures, vec![light("Deep chart"), dark("Deep chart")]);
    let shell = summary(&inc.rows, "d-00002");
    assert_eq!(shell.title.as_deref(), Some("Shell doc"));
    assert_eq!(shell.size, size(0, 1, 0, 0));
    let words = format!("{inc:?}");
    assert!(
        !words.contains("Hidden") && !words.contains("Why"),
        "{words}"
    );
}

/// D-0C4TB, D-MCH70: first-publication totals sum the rows' sizes, with no doc or folder unit in
/// `blocks`; `docs` and `folders` count the selected docs and folders and equal their totals.
#[test]
fn drv3_first_totals_sum_sizes_and_included_counts_match() {
    let (_dir, r) = project();
    let st = r.read();
    let blocks = vec![
        folder("f-00001", None, "one", "One", "a"),
        folder("f-00002", Some("f-00001"), "two", "Two", "a"),
        doc("d-00001", Some("f-00002"), "a", "A", "a"),
        section("s-00001", "d-00001", "a", "Part"),
        prose("p-00001", "s-00001", "a", "Words."),
        decision("D-00001", "d-00001", "b", "A decision."),
        doc("d-00002", Some("f-00001"), "c", "C", "b"),
        figure("x-00001", "d-00002", "a", "Chart", 1, Some(2)),
        doc("d-00003", None, "b", "B", "b"),
        prose("p-00003", "d-00003", "a", "More words."),
        decision("D-00003", "d-00003", "b", "Another decision."),
    ];
    let side = first(&st, &blocks, publication::IMPLICIT_ORG);
    let inc = included(&side);
    assert_eq!(ids(&inc.rows), vec!["d-00001", "d-00002", "d-00003"]);
    let t = inc.totals;
    assert_eq!((t.docs, t.folders), (3, 2));
    assert_eq!((inc.docs, inc.folders), (t.docs, t.folders));
    assert_eq!((t.sections, t.blocks, t.decisions, t.figures), (1, 4, 2, 2));
    let sum = |f: fn(&DocSize) -> u64| inc.rows.iter().map(|r| f(&r.size)).sum::<u64>();
    assert_eq!(t.sections, sum(|s| s.sections));
    assert_eq!(t.blocks, sum(|s| s.blocks));
    assert_eq!(t.decisions, sum(|s| s.decisions));
    assert_eq!(t.figures, sum(|s| s.figures));
    let v = serde_json::to_value(&inc).expect("the included list");
    assert!(v["docs"].is_u64() && v["folders"].is_u64(), "{v}");
    assert_eq!(v["docs"], v["totals"]["docs"]);
    assert_eq!(v["folders"], v["totals"]["folders"]);
}

/// D-0C4TB, D-Y4884: a selected doc that its side's site gives no page (the top-level glossary
/// doc) keeps its row and its place in reading order, with a null page.
#[test]
fn drv3_selected_doc_without_page_keeps_row_and_place() {
    let (_dir, r) = project();
    let st = r.read();
    let base = vec![
        doc("d-00001", None, "first", "First", "a"),
        doc("d-00003", None, "third", "Third", "c"),
    ];
    let mut blocks = base.clone();
    blocks.extend([
        doc("d-00002", None, "glossary", "Glossary", "b"),
        prose("p-00002", "d-00002", "a", "Terms."),
    ]);
    let side = first(&st, &blocks, publication::IMPLICIT_ORG);
    let inc = included(&side);
    assert_eq!(ids(&inc.rows), vec!["d-00001", "d-00002", "d-00003"]);
    let g = summary(&inc.rows, "d-00002");
    assert_eq!(g.page, None, "no page is invented");
    assert_eq!(g.title.as_deref(), Some("Glossary"));
    assert_eq!(g.size, size(0, 1, 0, 0));
    assert_eq!(inc.totals.docs, 3);

    let (bv, av, diff) = compared(&st, &base, &blocks, publication::IMPLICIT_ORG);
    let reviews = doc_reviews(Some((&diff, &bv)), &av);
    assert_eq!(ids(&reviews.added_docs), vec!["d-00002"]);
    assert_eq!(reviews.added_docs[0].page, None);
}

/// D-RDH1C, D-0C4TB: removed doc rows follow the baseline's reading order, never id order, and a
/// removed doc with no page keeps its place with a null page.
#[test]
fn drv3_removed_docs_keep_baseline_reading_order() {
    let (_dir, r) = project();
    let st = r.read();
    let before = vec![
        folder("f-00001", None, "guide", "Guide", "b"),
        doc("d-00003", Some("f-00001"), "three", "Three", "a"),
        doc("d-00001", Some("f-00001"), "one", "One", "b"),
        doc("d-00002", None, "two", "Two", "a"),
        doc("d-00004", None, "glossary", "Glossary", "c"),
        doc("d-00009", None, "stays", "Stays", "d"),
    ];
    let after = vec![doc("d-00009", None, "stays", "Stays", "d")];
    let (bv, av, diff) = compared(&st, &before, &after, publication::IMPLICIT_ORG);
    let reviews = doc_reviews(Some((&diff, &bv)), &av);
    assert_eq!(
        removed_ids(&reviews.removed_docs),
        vec!["d-00002", "d-00003", "d-00001", "d-00004"]
    );
    let pages: Vec<Option<&str>> = reviews
        .removed_docs
        .iter()
        .map(|d| d.before_page.as_deref())
        .collect();
    assert_eq!(
        pages,
        vec![
            Some("two.html"),
            Some("guide/three.html"),
            Some("guide/one.html"),
            None
        ]
    );
    assert_eq!(reviews.removed_docs[1].title.as_deref(), Some("Three"));
}
